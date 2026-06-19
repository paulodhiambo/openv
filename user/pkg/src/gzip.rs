use alloc::vec::Vec;
use libos::write as libos_write;

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit_pos: u8,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, bit_pos: 0 }
    }

    pub fn read_bit(&mut self) -> Option<u8> {
        if self.pos >= self.data.len() {
            return None;
        }
        let bit = (self.data[self.pos] >> self.bit_pos) & 1;
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.pos += 1;
        }
        Some(bit)
    }

    pub fn read_bits(&mut self, n: u8) -> Option<u16> {
        let mut val = 0u16;
        for i in 0..n {
            let bit = self.read_bit()?;
            val |= (bit as u16) << i;
        }
        Some(val)
    }

    pub fn read_byte(&mut self) -> Option<u8> {
        if self.bit_pos == 0 {
            let b = self.data.get(self.pos).copied()?;
            self.pos += 1;
            Some(b)
        } else {
            self.read_bits(8).map(|v| v as u8)
        }
    }

    pub fn read_u16_le(&mut self) -> Option<u16> {
        let lo = self.read_byte()? as u16;
        let hi = self.read_byte()? as u16;
        Some(lo | (hi << 8))
    }

    pub fn byte_align(&mut self) {
        if self.bit_pos != 0 {
            self.bit_pos = 0;
            self.pos += 1;
        }
    }
}

pub struct HuffTree {
    nodes: Vec<[i16; 2]>,
}

impl HuffTree {
    pub fn build(lengths: &[u8]) -> Option<Self> {
        let max_len = 15usize;
        let mut bl_count = [0usize; 16];
        for &l in lengths {
            if l as usize <= max_len {
                bl_count[l as usize] += 1;
            }
        }
        bl_count[0] = 0;

        let mut next_code = [0u16; 16];
        let mut code = 0u16;
        for bits in 1..=max_len {
            code = (code + bl_count[bits - 1] as u16) << 1;
            next_code[bits] = code;
        }

        let mut nodes = Vec::new();
        nodes.push([-1i16, -1i16]);

        for (symbol, &len) in lengths.iter().enumerate() {
            if len == 0 || len as usize > max_len {
                continue;
            }
            let c = next_code[len as usize];
            next_code[len as usize] += 1;

            let mut idx = 0i16;
            for b in (0..len).rev() {
                let bit = ((c >> b) & 1) as usize;
                if nodes[idx as usize][bit] < 0 {
                    let new_idx = nodes.len() as i16;
                    nodes.push([-1i16, -1i16]);
                    nodes[idx as usize][bit] = new_idx;
                }
                idx = nodes[idx as usize][bit];
            }
            nodes[idx as usize][0] = -(symbol as i16 + 1);
            nodes[idx as usize][1] = -(symbol as i16 + 1);
        }

        Some(HuffTree { nodes })
    }

    pub fn decode(&self, reader: &mut BitReader) -> Option<u16> {
        let mut idx = 0i16;
        loop {
            let bit = reader.read_bit()?;
            let child = self.nodes[idx as usize][bit as usize];
            if child < 0 {
                return Some((-(child + 1)) as u16);
            }
            idx = child;
        }
    }
}

const LEN_BASE: [u16; 29] = [3,4,5,6,7,8,9,10,11,13,15,17,19,23,27,31,35,43,51,59,67,83,99,115,131,163,195,227,258];
const LEN_EXTRA: [u8; 29] = [0,0,0,0,0,0,0,0,1,1,1,1,2,2,2,2,3,3,3,3,4,4,4,4,5,5,5,5,0];
const DIST_BASE: [u16; 30] = [1,2,3,4,5,7,9,13,17,25,33,49,65,97,129,193,257,385,513,769,1025,1537,2049,3073,4097,6145,8193,12289,16385,24577];
const DIST_EXTRA: [u8; 30] = [0,0,0,0,1,1,2,2,3,3,4,4,5,5,6,6,7,7,8,8,9,9,10,10,11,11,12,12,13,13];

// DEFLATE max lookback distance is exactly 32768; ring size must be a power of 2.
const RING_SIZE: usize = 32768;
const OUT_BUF_SIZE: usize = 4096;

trait InflateOut {
    fn push_byte(&mut self, b: u8);
    fn copy_backref(&mut self, dist: usize, len: usize) -> bool;
}

// ── Vec sink (used by gunzip) ──────────────────────────────────────────────

impl InflateOut for Vec<u8> {
    fn push_byte(&mut self, b: u8) {
        self.push(b);
    }
    fn copy_backref(&mut self, dist: usize, len: usize) -> bool {
        if dist == 0 || dist > self.len() {
            return false;
        }
        let copy_start = self.len() - dist;
        for i in 0..len {
            self.push(self[copy_start + i]);
        }
        true
    }
}

// ── Streaming fd sink (used by gunzip_to_fd) ──────────────────────────────
//
// Uses a 32 KB ring buffer matching DEFLATE's maximum back-reference distance
// so we never need to hold the full decompressed output in memory.

pub struct StreamWriter {
    ring: Vec<u8>,
    total: usize,
    fd: i32,
    out_buf: Vec<u8>,
    out_pos: usize,
}

impl StreamWriter {
    pub fn new(fd: i32) -> Self {
        StreamWriter {
            ring: alloc::vec![0u8; RING_SIZE],
            total: 0,
            fd,
            out_buf: alloc::vec![0u8; OUT_BUF_SIZE],
            out_pos: 0,
        }
    }

    fn flush(&mut self) {
        if self.out_pos > 0 {
            libos_write(self.fd as usize, self.out_buf.as_ptr(), self.out_pos);
            self.out_pos = 0;
        }
    }

    pub fn finish(mut self) -> usize {
        self.flush();
        self.total
    }
}

impl InflateOut for StreamWriter {
    fn push_byte(&mut self, b: u8) {
        self.ring[self.total & (RING_SIZE - 1)] = b;
        self.total += 1;
        self.out_buf[self.out_pos] = b;
        self.out_pos += 1;
        if self.out_pos == OUT_BUF_SIZE {
            self.flush();
        }
    }

    fn copy_backref(&mut self, dist: usize, len: usize) -> bool {
        if dist == 0 || dist > self.total {
            return false;
        }
        for _ in 0..len {
            // Read before push_byte so overlap copies work correctly.
            let src = (self.total - dist) & (RING_SIZE - 1);
            let b = self.ring[src];
            self.push_byte(b);
        }
        true
    }
}

// ── Core inflate logic ────────────────────────────────────────────────────

fn decode_block<O: InflateOut>(
    reader: &mut BitReader,
    lit_tree: &HuffTree,
    dist_tree: &HuffTree,
    output: &mut O,
) -> Option<()> {
    loop {
        let sym = lit_tree.decode(reader)? as usize;
        if sym < 256 {
            output.push_byte(sym as u8);
        } else if sym == 256 {
            break;
        } else if sym <= 285 {
            let idx = sym - 257;
            let len = LEN_BASE[idx] as usize + reader.read_bits(LEN_EXTRA[idx])? as usize;

            let dist_sym = dist_tree.decode(reader)? as usize;
            let dist = DIST_BASE[dist_sym] as usize + reader.read_bits(DIST_EXTRA[dist_sym])? as usize;

            if !output.copy_backref(dist, len) {
                return None;
            }
        }
    }
    Some(())
}

fn inflate_to<O: InflateOut>(data: &[u8], output: &mut O) -> Option<()> {
    let mut reader = BitReader::new(data);

    loop {
        let bfinal = reader.read_bit()?;
        let btype = reader.read_bits(2)?;

        match btype {
            0 => {
                reader.byte_align();
                let len = reader.read_u16_le()?;
                let nlen = reader.read_u16_le()?;
                if len != (!nlen) & 0xFFFF {
                    return None;
                }
                for _ in 0..len {
                    output.push_byte(reader.read_byte()?);
                }
            }
            1 => {
                let mut lit_len = [0u8; 288];
                for i in 0..144 { lit_len[i] = 8; }
                for i in 144..256 { lit_len[i] = 9; }
                for i in 256..280 { lit_len[i] = 7; }
                for i in 280..288 { lit_len[i] = 8; }
                let mut dist = [0u8; 32];
                for i in 0..32 { dist[i] = 5; }
                let lit_tree = HuffTree::build(&lit_len)?;
                let dist_tree = HuffTree::build(&dist)?;
                decode_block(&mut reader, &lit_tree, &dist_tree, output)?;
            }
            2 => {
                let hlit = reader.read_bits(5)? as usize + 257;
                let hdist = reader.read_bits(5)? as usize + 1;
                let hclen = reader.read_bits(4)? as usize + 4;

                let cl_order = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl_lengths = [0u8; 19];
                for i in 0..hclen {
                    cl_lengths[cl_order[i]] = reader.read_bits(3)? as u8;
                }

                let cl_tree = HuffTree::build(&cl_lengths)?;
                let mut lengths = Vec::new();
                let total = hlit + hdist;
                while lengths.len() < total {
                    let sym = cl_tree.decode(&mut reader)? as usize;
                    if sym < 16 {
                        lengths.push(sym as u8);
                    } else if sym == 16 {
                        let repeat = reader.read_bits(2)? as usize + 3;
                        let prev = *lengths.last().unwrap_or(&0);
                        for _ in 0..repeat { lengths.push(prev); }
                    } else if sym == 17 {
                        let repeat = reader.read_bits(3)? as usize + 3;
                        lengths.extend(core::iter::repeat_n(0u8, repeat));
                    } else if sym == 18 {
                        let repeat = reader.read_bits(7)? as usize + 11;
                        lengths.extend(core::iter::repeat_n(0u8, repeat));
                    }
                }

                let mut ll_arr = [0u8; 288];
                for (i, &v) in lengths[..hlit].iter().enumerate() {
                    ll_arr[i] = v;
                }
                let mut d_arr = [0u8; 32];
                for (i, &v) in lengths[hlit..].iter().enumerate() {
                    if i < 32 { d_arr[i] = v; }
                }

                let lit_tree = HuffTree::build(&ll_arr)?;
                let dist_tree = HuffTree::build(&d_arr)?;
                decode_block(&mut reader, &lit_tree, &dist_tree, output)?;
            }
            _ => return None,
        }

        if bfinal != 0 { break; }
    }

    Some(())
}

// ── Public API ────────────────────────────────────────────────────────────

fn skip_gzip_header(data: &[u8]) -> Option<usize> {
    if data.len() < 18 { return None; }
    if data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 { return None; }
    let flg = data[3];
    let mut pos = 10;
    if flg & 0x04 != 0 {
        let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]);
        pos += 2 + xlen as usize;
    }
    if flg & 0x08 != 0 { while data[pos] != 0 { pos += 1; } pos += 1; }
    if flg & 0x10 != 0 { while data[pos] != 0 { pos += 1; } pos += 1; }
    if flg & 0x02 != 0 { pos += 2; }
    if data.len() < pos + 8 { return None; }
    Some(pos)
}

/// Decompress gzip data into a Vec. Needs output_size bytes of heap.
pub fn gunzip(data: &[u8]) -> Option<Vec<u8>> {
    let pos = skip_gzip_header(data)?;
    let n = data.len();
    let isize_hint = u32::from_le_bytes([data[n-4], data[n-3], data[n-2], data[n-1]]) as usize;
    let compressed = &data[pos..n - 8];
    let mut output = Vec::with_capacity(isize_hint);
    inflate_to(compressed, &mut output)?;
    Some(output)
}

/// Decompress gzip data streaming directly to an open fd.
/// Peak memory: compressed body + 32 KB ring + 4 KB output buffer.
pub fn gunzip_to_fd(data: &[u8], fd: i32) -> Option<usize> {
    let pos = skip_gzip_header(data)?;
    let compressed = &data[pos..data.len() - 8];
    let mut writer = StreamWriter::new(fd);
    inflate_to(compressed, &mut writer)?;
    Some(writer.finish())
}

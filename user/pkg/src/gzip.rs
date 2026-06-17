use alloc::vec::Vec;

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

            // Insert code (MSB first) into tree
            let mut idx = 0i16;
            for b in (0..len).rev() {
                let bit = ((c >> b) & 1) as usize;
                if nodes[idx as usize][bit] < 0 {
                    // Create new node
                    let new_idx = nodes.len() as i16;
                    nodes.push([-1i16, -1i16]);
                    nodes[idx as usize][bit] = new_idx;
                }
                idx = nodes[idx as usize][bit];
            }
            // Mark leaf by encoding symbol as -(symbol+1)
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

fn decode_block(reader: &mut BitReader, lit_tree: &HuffTree, dist_tree: &HuffTree, output: &mut Vec<u8>) -> Option<()> {
    loop {
        let sym = lit_tree.decode(reader)? as usize;
        if sym < 256 {
            output.push(sym as u8);
        } else if sym == 256 {
            break;
        } else if sym <= 285 {
            let idx = sym - 257;
            let len = LEN_BASE[idx] as usize + reader.read_bits(LEN_EXTRA[idx])? as usize;

            let dist_sym = dist_tree.decode(reader)? as usize;
            let dist = DIST_BASE[dist_sym] as usize + reader.read_bits(DIST_EXTRA[dist_sym])? as usize;

            let copy_start = output.len().wrapping_sub(dist);
            for i in 0..len {
                output.push(output[copy_start + i]);
            }
        }
    }
    Some(())
}

fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut reader = BitReader::new(data);
    let mut output = Vec::new();

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
                    output.push(reader.read_byte()?);
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
                decode_block(&mut reader, &lit_tree, &dist_tree, &mut output)?;
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
                        for _ in 0..repeat { lengths.push(0); }
                    } else if sym == 18 {
                        let repeat = reader.read_bits(7)? as usize + 11;
                        for _ in 0..repeat { lengths.push(0); }
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
                decode_block(&mut reader, &lit_tree, &dist_tree, &mut output)?;
            }
            _ => return None,
        }

        if bfinal != 0 { break; }
    }

    Some(output)
}

pub fn gunzip(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 18 { return None; }
    if data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 { return None; }
    let flg = data[3];
    let mut pos = 10;
    if flg & 0x04 != 0 {
        let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]);
        pos += 2 + xlen as usize;
    }
    if flg & 0x08 != 0 {
        while data[pos] != 0 { pos += 1; }
        pos += 1;
    }
    if flg & 0x10 != 0 {
        while data[pos] != 0 { pos += 1; }
        pos += 1;
    }
    if flg & 0x02 != 0 { pos += 2; }

    if data.len() < pos + 8 { return None; }
    let compressed = &data[pos..data.len() - 8];
    inflate(compressed)
}

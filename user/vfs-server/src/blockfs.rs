use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use libos::ipc::Message;

pub struct VirtioBlkProxy {
    pid: i32,
}

impl VirtioBlkProxy {
    pub fn new(pid: i32) -> Self { Self { pid } }

    pub fn read_block(&self, blk: u32, buf: &mut [u8; 4096]) -> bool {
        let mut msg = Message::new();
        msg.type_ = 100;
        msg.data[0..4].copy_from_slice(&blk.to_le_bytes());
        msg.data[4..12].copy_from_slice(&(buf.as_mut_ptr() as usize).to_le_bytes());
        if libos::msg_sendrec(self.pid, &mut msg) != 0 { return false; }
        msg.type_ == 0
    }

    pub fn write_block(&self, blk: u32, buf: &[u8; 4096]) -> bool {
        let mut msg = Message::new();
        msg.type_ = 101;
        msg.data[0..4].copy_from_slice(&blk.to_le_bytes());
        msg.data[4..12].copy_from_slice(&(buf.as_ptr() as usize).to_le_bytes());
        if libos::msg_sendrec(self.pid, &mut msg) != 0 { return false; }
        msg.type_ == 0
    }
}

// ── On-disk constants ────────────────────────────────────────────────────────

const OFS_MAGIC: u32   = 0x4F564653; // "OVFS"
const OFS_VERSION: u32 = 2;          // v2 adds the write-ahead journal

pub const BLOCK_SIZE: usize = 4096;
const MAX_INODES: usize     = 256;
const INODE_SIZE: usize     = 128;
const INODES_PER_BLOCK: usize     = BLOCK_SIZE / INODE_SIZE;   // 32
const DIRENTRY_SIZE: usize        = 64;
const DIRENTRIES_PER_BLOCK: usize = BLOCK_SIZE / DIRENTRY_SIZE; // 64

// ── Disk layout ──────────────────────────────────────────────────────────────
// Block 0:  superblock
// Block 1:  block-allocation bitmap
// Block 2:  inode bitmap
// Blocks 3–10: inode table  (8 blocks × 32 inodes = 256 inodes)
// Block 11: journal header  (magic, n_entries, committed, target_blk[32])
// Blocks 12–43: journal data slots (32 × one full 4096-byte block each)
// Block 44+: user data

const BLOCK_BITMAP_BLK: u32  = 1;
const INODE_BITMAP_BLK: u32  = 2;
const INODE_TABLE_START: u32 = 3;
const INODE_TABLE_BLOCKS: u32 = 8;

const JOURNAL_HDR_BLK: u32  = 11;
const JOURNAL_DATA_BLK: u32 = 12;
const JOURNAL_SLOTS: usize   = 32;
const JOURNAL_MAGIC: u32     = 0x4F4A4C47; // "OJLG"

const DATA_START: u32 = JOURNAL_DATA_BLK + JOURNAL_SLOTS as u32; // = 44

#[allow(dead_code)]
const ITYPE_FREE: u8 = 0;
pub const ITYPE_FILE: u8 = 1;
pub const ITYPE_DIR: u8  = 2;

pub const ITYPE_FILE_ENTRY: u8 = 1;
pub const ITYPE_DIR_ENTRY: u8  = 2;

pub const ROOT_INODE: u32 = 0;

// ── Raw on-disk structures ─────────��──────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RawInode {
    pub itype: u8,
    pub _pad: [u8; 3],
    pub size: u32,
    pub nlink: u32,
    pub used_blocks: u32,
    pub direct: [u32; 12],
    pub indirect: u32,
    pub _reserved: [u32; 15],
}

const _: () = assert!(core::mem::size_of::<RawInode>() == INODE_SIZE);

#[repr(C)]
#[derive(Clone, Copy)]
struct RawDirEntry {
    inode_num: u32,
    name_len: u8,
    entry_type: u8,
    _pad: [u8; 2],
    name: [u8; 56],
}

const _: () = assert!(core::mem::size_of::<RawDirEntry>() == DIRENTRY_SIZE);

// ── OFS state ────────────────────────────────────────────────────────────────

pub struct OfsState {
    dev:          VirtioBlkProxy,
    total_blocks: u32,
    /// Pending block writes for the current transaction.
    /// All metadata (and data) writes are buffered here until `commit_txn` is called.
    pending:      BTreeMap<u32, [u8; BLOCK_SIZE]>,
}

// ── Low-level I/O helpers ─────────���───────────────────────────────────────────

impl OfsState {
    /// Write `buf` for block `blk` to the pending buffer (journaled path).
    fn write_blk(&mut self, blk: u32, buf: &[u8; BLOCK_SIZE]) -> bool {
        self.pending.insert(blk, *buf);
        true
    }

    /// Read block `blk`, checking the pending (write-back) buffer first.
    fn read_blk(&mut self, blk: u32, buf: &mut [u8; BLOCK_SIZE]) -> bool {
        if let Some(data) = self.pending.get(&blk) {
            *buf = *data;
            return true;
        }
        self.dev.read_block(blk, buf)
    }

    // ── Journal ───────────────────────────────────────────────────────────────

    /// On mount: if a committed-but-not-yet-applied transaction exists in the
    /// journal, re-apply it to the real block locations (crash recovery).
    fn journal_replay(&self) {
        let mut hdr = [0u8; BLOCK_SIZE];
        if !self.dev.read_block(JOURNAL_HDR_BLK, &mut hdr) { return; }

        let magic     = u32_le(&hdr, 0);
        let n         = u32_le(&hdr, 4) as usize;
        let committed = u32_le(&hdr, 8);

        if magic != JOURNAL_MAGIC || committed != 1 || n == 0 || n > JOURNAL_SLOTS {
            return;
        }

        for i in 0..n {
            let target = u32_le(&hdr, 16 + i * 4);
            if target == 0 { continue; }
            let mut data = [0u8; BLOCK_SIZE];
            if self.dev.read_block(JOURNAL_DATA_BLK + i as u32, &mut data) {
                self.dev.write_block(target, &data);
            }
        }

        // Clear the journal so a second crash doesn't double-apply.
        let mut clear = [0u8; BLOCK_SIZE];
        clear[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        self.dev.write_block(JOURNAL_HDR_BLK, &clear);
    }

    /// Flush all pending writes atomically via the write-ahead journal.
    pub fn commit_txn(&mut self) -> bool {
        if self.pending.is_empty() { return true; }

        let n = self.pending.len();
        if n > JOURNAL_SLOTS { return false; }

        let entries: Vec<(u32, [u8; BLOCK_SIZE])> =
            self.pending.iter().map(|(&k, &v)| (k, v)).collect();

        // Phase 1 – write each block snapshot to its journal data slot.
        for (i, (_, data)) in entries.iter().enumerate() {
            if !self.dev.write_block(JOURNAL_DATA_BLK + i as u32, data) {
                return false;
            }
        }

        // Phase 2 – write the journal header with committed = 0 (log phase).
        let mut hdr = [0u8; BLOCK_SIZE];
        hdr[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        hdr[4..8].copy_from_slice(&(n as u32).to_le_bytes());
        hdr[8..12].copy_from_slice(&0u32.to_le_bytes()); // not yet committed
        for (i, (target, _)) in entries.iter().enumerate() {
            let off = 16 + i * 4;
            hdr[off..off + 4].copy_from_slice(&target.to_le_bytes());
        }
        if !self.dev.write_block(JOURNAL_HDR_BLK, &hdr) { return false; }

        // Phase 3 – commit marker: flip committed = 1.
        // After this write, recovery WILL replay the journal on crash.
        hdr[8..12].copy_from_slice(&1u32.to_le_bytes());
        if !self.dev.write_block(JOURNAL_HDR_BLK, &hdr) { return false; }

        // Phase 4 – apply to the real block locations.
        for (target, data) in &entries {
            if !self.dev.write_block(*target, data) {
                // Journal still has commit record; recovery will finish this.
                return false;
            }
        }

        // Phase 5 – clear journal (n_entries = 0 signals a clean slate).
        let mut clear = [0u8; BLOCK_SIZE];
        clear[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        self.dev.write_block(JOURNAL_HDR_BLK, &clear);

        self.pending.clear();
        true
    }

    /// Explicit flush: commit any buffered writes to durable storage.
    pub fn fsync(&mut self) -> bool {
        self.commit_txn()
    }

    // ── Bitmap helpers ────────────────────────────────────────────────────────

    fn bitmap_set(&mut self, bitmap_blk: u32, bit: usize, value: bool) {
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(bitmap_blk, &mut buf) { return; }
        if value { buf[bit / 8] |=  1 << (bit % 8); }
        else      { buf[bit / 8] &= !(1 << (bit % 8)); }
        self.write_blk(bitmap_blk, &buf);
    }

    fn bitmap_alloc(&mut self, bitmap_blk: u32, start: usize, limit: usize) -> Option<usize> {
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(bitmap_blk, &mut buf) { return None; }
        for i in start..limit {
            if buf[i / 8] & (1 << (i % 8)) == 0 {
                buf[i / 8] |= 1 << (i % 8);
                self.write_blk(bitmap_blk, &buf);
                return Some(i);
            }
        }
        None
    }

    // ── Inode I/O ──────────��─────────────────────────────────────────────────

    pub fn read_inode(&mut self, ino: u32) -> Option<RawInode> {
        if ino as usize >= MAX_INODES { return None; }
        let table_blk = INODE_TABLE_START + ino / INODES_PER_BLOCK as u32;
        let offset    = (ino as usize % INODES_PER_BLOCK) * INODE_SIZE;
        let mut buf   = [0u8; BLOCK_SIZE];
        if !self.read_blk(table_blk, &mut buf) { return None; }
        let inode = unsafe { *(buf[offset..].as_ptr() as *const RawInode) };
        Some(inode)
    }

    pub fn write_inode(&mut self, ino: u32, inode: &RawInode) -> bool {
        if ino as usize >= MAX_INODES { return false; }
        let table_blk = INODE_TABLE_START + ino / INODES_PER_BLOCK as u32;
        let offset    = (ino as usize % INODES_PER_BLOCK) * INODE_SIZE;
        let mut buf   = [0u8; BLOCK_SIZE];
        if !self.read_blk(table_blk, &mut buf) { return false; }
        unsafe { *(buf[offset..].as_mut_ptr() as *mut RawInode) = *inode; }
        self.write_blk(table_blk, &buf)
    }

    // ── Block allocation ───────────────────────────────────��──────────────────

    fn alloc_block(&mut self) -> Option<u32> {
        let limit = self.total_blocks as usize;
        self.bitmap_alloc(BLOCK_BITMAP_BLK, DATA_START as usize, limit)
            .map(|b| b as u32)
    }

    fn free_block(&mut self, blk: u32) {
        if blk >= DATA_START {
            self.bitmap_set(BLOCK_BITMAP_BLK, blk as usize, false);
        }
    }

    // ── Inode allocation ──────────────────────────��───────────────────────────

    pub fn alloc_inode(&mut self) -> Option<u32> {
        // Inode 0 is always the root; start from 1.
        self.bitmap_alloc(INODE_BITMAP_BLK, 1, MAX_INODES)
            .map(|i| i as u32)
    }

    pub fn free_inode(&mut self, ino: u32) {
        self.bitmap_set(INODE_BITMAP_BLK, ino as usize, false);
    }

    // ── File block mapping ────────────��───────────────────────────────────────

    fn get_file_block(&mut self, inode: &mut RawInode, ino: u32, idx: u32, alloc: bool)
        -> Option<u32>
    {
        if idx < 12 {
            let blk = inode.direct[idx as usize];
            if blk != 0 { return Some(blk); }
            if !alloc   { return None; }
            let new_blk = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_blk(new_blk, &zeros);
            inode.direct[idx as usize] = new_blk;
            inode.used_blocks += 1;
            self.write_inode(ino, inode);
            return Some(new_blk);
        }

        let indirect_idx = idx - 12;
        if indirect_idx >= (BLOCK_SIZE / 4) as u32 { return None; }

        if inode.indirect == 0 {
            if !alloc { return None; }
            let ib = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_blk(ib, &zeros);
            inode.indirect = ib;
            inode.used_blocks += 1;
            self.write_inode(ino, inode);
        }

        let mut ib_buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(inode.indirect, &mut ib_buf) { return None; }
        let entry_off = indirect_idx as usize * 4;
        let blk = u32::from_le_bytes(ib_buf[entry_off..entry_off + 4].try_into().ok()?);
        if blk != 0 { return Some(blk); }
        if !alloc    { return None; }

        let new_blk = self.alloc_block()?;
        let zeros = [0u8; BLOCK_SIZE];
        self.write_blk(new_blk, &zeros);
        ib_buf[entry_off..entry_off + 4].copy_from_slice(&new_blk.to_le_bytes());
        self.write_blk(inode.indirect, &ib_buf);
        inode.used_blocks += 1;
        self.write_inode(ino, inode);
        Some(new_blk)
    }

    // ── Directory helpers ──���───────────────────────────────���──────────────────

    pub fn dir_lookup_inode(&mut self, dir_ino: u32, name: &str) -> Option<(u32, u8)> {
        let inode = self.read_inode(dir_ino)?;
        if inode.itype != ITYPE_DIR { return None; }
        let name_bytes   = name.as_bytes();
        let total_entries = (inode.size as usize + DIRENTRY_SIZE - 1) / DIRENTRY_SIZE;
        let mut seen = 0;

        'outer: for block_idx in 0..12 {
            if seen >= total_entries { break; }
            let blk = inode.direct[block_idx];
            if blk == 0 { seen += DIRENTRIES_PER_BLOCK; continue; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut buf) { break; }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if seen >= total_entries { break 'outer; }
                let de = unsafe { *(buf[slot * DIRENTRY_SIZE..].as_ptr() as *const RawDirEntry) };
                seen += 1;
                if de.name_len == 0 { continue; }
                let n = de.name_len as usize;
                if n == name_bytes.len() && &de.name[..n] == name_bytes {
                    return Some((de.inode_num, de.entry_type));
                }
            }
        }
        None
    }

    pub fn lookup_path(&mut self, path: &str) -> Option<(u32, u8)> {
        let mut cur_ino  = ROOT_INODE;
        let mut cur_type = ITYPE_DIR_ENTRY;
        let path = path.trim_matches('/');
        if path.is_empty() { return Some((ROOT_INODE, ITYPE_DIR_ENTRY)); }
        for comp in path.split('/') {
            if comp.is_empty() { continue; }
            if cur_type != ITYPE_DIR_ENTRY { return None; }
            let (next_ino, next_type) = self.dir_lookup_inode(cur_ino, comp)?;
            cur_ino  = next_ino;
            cur_type = next_type;
        }
        Some((cur_ino, cur_type))
    }

    pub fn dir_add_entry(&mut self, dir_ino: u32, name: &str, child_ino: u32, etype: u8) -> bool {
        let mut inode = match self.read_inode(dir_ino) { Some(i) => i, None => return false };
        if inode.itype != ITYPE_DIR { return false; }
        let name_bytes = name.as_bytes();
        if name_bytes.len() > 55 { return false; }

        let total_entries = (inode.size as usize + DIRENTRY_SIZE - 1) / DIRENTRY_SIZE;

        // Scan for an empty slot in existing blocks.
        for block_idx in 0..12usize {
            let coverage = block_idx * DIRENTRIES_PER_BLOCK;
            if coverage >= total_entries { break; }
            let blk = inode.direct[block_idx];
            if blk == 0 { continue; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut buf) { continue; }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if coverage + slot >= total_entries { break; }
                let de = unsafe { *(buf[slot * DIRENTRY_SIZE..].as_ptr() as *const RawDirEntry) };
                if de.name_len == 0 {
                    write_direntry(&mut buf, slot, child_ino, name_bytes, etype);
                    self.write_blk(blk, &buf);
                    let slot_pos = (coverage + slot + 1) * DIRENTRY_SIZE;
                    if slot_pos > inode.size as usize {
                        inode.size = slot_pos as u32;
                        self.write_inode(dir_ino, &inode);
                    }
                    return self.commit_txn();
                }
            }
        }

        // Append to end — allocate a new block if needed.
        let next_block_idx = (inode.size as usize + BLOCK_SIZE - 1) / BLOCK_SIZE;
        if next_block_idx >= 12 { return false; }

        let blk = if inode.direct[next_block_idx] != 0 {
            inode.direct[next_block_idx]
        } else {
            match self.alloc_block() {
                Some(b) => { inode.direct[next_block_idx] = b; inode.used_blocks += 1; b }
                None    => return false,
            }
        };
        let mut buf = [0u8; BLOCK_SIZE];
        write_direntry(&mut buf, 0, child_ino, name_bytes, etype);
        self.write_blk(blk, &buf);
        inode.size = ((next_block_idx * DIRENTRIES_PER_BLOCK) + 1) as u32 * DIRENTRY_SIZE as u32;
        self.write_inode(dir_ino, &inode);
        self.commit_txn()
    }

    pub fn dir_readall(&mut self, dir_ino: u32) -> Vec<(alloc::string::String, u32, u8)> {
        let mut result = Vec::new();
        let inode = match self.read_inode(dir_ino) { Some(i) => i, None => return result };
        if inode.itype != ITYPE_DIR { return result; }
        let total_entries = (inode.size as usize + DIRENTRY_SIZE - 1) / DIRENTRY_SIZE;
        let mut seen = 0;
        for block_idx in 0..12 {
            if seen >= total_entries { break; }
            let blk = inode.direct[block_idx];
            if blk == 0 { break; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut buf) { break; }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if seen >= total_entries { break; }
                let de = unsafe { *(buf[slot * DIRENTRY_SIZE..].as_ptr() as *const RawDirEntry) };
                seen += 1;
                if de.name_len == 0 { continue; }
                let n = de.name_len as usize;
                if let Ok(s) = core::str::from_utf8(&de.name[..n]) {
                    result.push((alloc::string::String::from(s), de.inode_num, de.entry_type));
                }
            }
        }
        result
    }

    // ── File read/write ───────────────────────────────────────��───────────────

    pub fn file_read(&mut self, ino: u32, offset: usize, buf: &mut [u8])
        -> Result<usize, &'static str>
    {
        let inode = self.read_inode(ino).ok_or("bad inode")?;
        if inode.itype != ITYPE_FILE { return Err("not a file"); }
        if offset >= inode.size as usize { return Ok(0); }
        let available = inode.size as usize - offset;
        let to_read   = core::cmp::min(available, buf.len());
        let mut done  = 0usize;

        while done < to_read {
            let pos       = offset + done;
            let block_idx = (pos / BLOCK_SIZE) as u32;
            let block_off = pos % BLOCK_SIZE;
            let chunk     = core::cmp::min(to_read - done, BLOCK_SIZE - block_off);
            let mut inode_mut = inode;
            let blk = self.get_file_block(&mut inode_mut, ino, block_idx, false)
                .ok_or("missing file block")?;
            let mut blk_buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut blk_buf) { return Err("block read error"); }
            buf[done..done + chunk].copy_from_slice(&blk_buf[block_off..block_off + chunk]);
            done += chunk;
        }
        Ok(to_read)
    }

    pub fn file_write(&mut self, ino: u32, offset: usize, data: &[u8])
        -> Result<usize, &'static str>
    {
        let mut inode = self.read_inode(ino).ok_or("bad inode")?;
        if inode.itype != ITYPE_FILE { return Err("not a file"); }
        let end  = offset + data.len();
        let mut done = 0usize;

        while done < data.len() {
            let pos       = offset + done;
            let block_idx = (pos / BLOCK_SIZE) as u32;
            let block_off = pos % BLOCK_SIZE;
            let chunk     = core::cmp::min(data.len() - done, BLOCK_SIZE - block_off);
            let blk = self.get_file_block(&mut inode, ino, block_idx, true)
                .ok_or("OOM: no free block")?;
            let mut blk_buf = [0u8; BLOCK_SIZE];
            if block_off != 0 || chunk < BLOCK_SIZE {
                if !self.read_blk(blk, &mut blk_buf) { return Err("block read error"); }
            }
            blk_buf[block_off..block_off + chunk].copy_from_slice(&data[done..done + chunk]);
            if !self.write_blk(blk, &blk_buf) { return Err("block write error"); }
            done += chunk;
        }

        if end > inode.size as usize {
            inode.size = end as u32;
            self.write_inode(ino, &inode);
        }
        self.commit_txn();
        Ok(data.len())
    }

    pub fn file_truncate(&mut self, ino: u32) {
        let mut inode = match self.read_inode(ino) { Some(i) => i, None => return };
        for blk in inode.direct.iter().filter(|&&b| b != 0) {
            self.free_block(*blk);
        }
        if inode.indirect != 0 {
            let mut ib_buf = [0u8; BLOCK_SIZE];
            if self.read_blk(inode.indirect, &mut ib_buf) {
                for i in 0..(BLOCK_SIZE / 4) {
                    let b = u32::from_le_bytes(ib_buf[i * 4..i * 4 + 4].try_into().unwrap_or([0; 4]));
                    if b != 0 { self.free_block(b); }
                }
            }
            self.free_block(inode.indirect);
        }
        inode.direct = [0u32; 12];
        inode.indirect = 0;
        inode.size = 0;
        inode.used_blocks = 0;
        self.write_inode(ino, &inode);
        self.commit_txn();
    }

    pub fn dir_unlink(&mut self, dir_ino: u32, name: &str) -> bool {
        let inode = match self.read_inode(dir_ino) { Some(i) => i, None => return false };
        let name_bytes    = name.as_bytes();
        let total_entries = (inode.size as usize + DIRENTRY_SIZE - 1) / DIRENTRY_SIZE;
        let mut seen = 0;

        for block_idx in 0..12usize {
            if seen >= total_entries { break; }
            let blk = inode.direct[block_idx];
            if blk == 0 { break; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut buf) { break; }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if seen >= total_entries { break; }
                let de = unsafe { *(buf[slot * DIRENTRY_SIZE..].as_ptr() as *const RawDirEntry) };
                seen += 1;
                if de.name_len == 0 { continue; }
                let n = de.name_len as usize;
                if n == name_bytes.len() && &de.name[..n] == name_bytes {
                    let child_ino = de.inode_num;
                    if de.entry_type == ITYPE_FILE_ENTRY {
                        self.file_truncate(child_ino);
                    }
                    self.free_inode(child_ino);
                    let zeros = [0u8; DIRENTRY_SIZE];
                    buf[slot * DIRENTRY_SIZE..slot * DIRENTRY_SIZE + DIRENTRY_SIZE]
                        .copy_from_slice(&zeros);
                    self.write_blk(blk, &buf);
                    self.commit_txn();
                    return true;
                }
            }
        }
        false
    }

    // ── Init / mkfs ───────────────────────────────────────────────────────────

    pub fn new(dev: VirtioBlkProxy) -> Option<Self> {
        let mut buf = [0u8; BLOCK_SIZE];
        if !dev.read_block(0, &mut buf) { return None; }

        let total_blocks = 2048;
        let mut state = Self { dev, total_blocks, pending: BTreeMap::new() };

        // Replay any uncommitted journal transaction left by a prior crash.
        state.journal_replay();

        let magic   = u32_le(&buf, 0);
        let version = u32_le(&buf, 4);
        if magic != OFS_MAGIC || version != OFS_VERSION {
            state.mkfs();
        }
        Some(state)
    }

    pub fn mkfs(&mut self) {
        // Write superblock directly (bypass pending; mkfs is a full-disk format).
        let mut buf = [0u8; BLOCK_SIZE];
        buf[0..4].copy_from_slice(&OFS_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&OFS_VERSION.to_le_bytes());
        buf[8..12].copy_from_slice(&self.total_blocks.to_le_bytes());
        self.dev.write_block(0, &buf);

        // Block bitmap: mark system + journal blocks as used.
        buf.fill(0);
        for i in 0..DATA_START as usize {
            buf[i / 8] |= 1 << (i % 8);
        }
        self.dev.write_block(BLOCK_BITMAP_BLK, &buf);

        // Inode bitmap: all free initially.
        buf.fill(0);
        self.dev.write_block(INODE_BITMAP_BLK, &buf);

        // Inode table: zero out.
        buf.fill(0);
        for b in INODE_TABLE_START..INODE_TABLE_START + INODE_TABLE_BLOCKS {
            self.dev.write_block(b, &buf);
        }

        // Initialize journal header (magic only; no pending transaction).
        buf.fill(0);
        buf[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        self.dev.write_block(JOURNAL_HDR_BLK, &buf);

        // Create root inode (allocated in inode 0).
        self.bitmap_set(INODE_BITMAP_BLK, 0, true);
        let root = RawInode {
            itype: ITYPE_DIR,
            _pad: [0; 3],
            size: 0,
            nlink: 1,
            used_blocks: 0,
            direct: [0u32; 12],
            indirect: 0,
            _reserved: [0u32; 15],
        };
        self.write_inode(ROOT_INODE, &root);
        self.commit_txn();
    }
}

// ── Byte helpers ──────────────────────────────────────────────────────────────

fn u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn write_direntry(buf: &mut [u8; BLOCK_SIZE], slot: usize, ino: u32, name: &[u8], etype: u8) {
    let mut de = RawDirEntry {
        inode_num: ino,
        name_len: name.len() as u8,
        entry_type: etype,
        _pad: [0; 2],
        name: [0; 56],
    };
    de.name[..name.len()].copy_from_slice(name);
    unsafe {
        let dst = buf[slot * DIRENTRY_SIZE..].as_mut_ptr() as *mut RawDirEntry;
        *dst = de;
    }
}

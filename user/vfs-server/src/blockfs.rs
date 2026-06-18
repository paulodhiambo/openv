use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::blockio::{FsError, VirtioBlkProxy};

// ── On-disk constants ────────────────────────────────────────────────────────

const OFS_MAGIC: u32   = 0x4F564653; // "OVFS"
const OFS_VERSION: u32 = 3;          // v3: 1024 inodes, double-indirect, CRC

pub const BLOCK_SIZE: usize = 4096;
const MAX_INODES: usize     = 1024;
const INODE_SIZE: usize     = 128;
const INODES_PER_BLOCK: usize     = BLOCK_SIZE / INODE_SIZE;   // 32
const DIRENTRY_SIZE: usize        = 64;
const DIRENTRIES_PER_BLOCK: usize = BLOCK_SIZE / DIRENTRY_SIZE; // 64

// ── Disk layout (v3) ─────────────────────────────────────────────────────────
// Block 0:   superblock  (magic, version, total_blocks, crc32)
// Block 1:   block bitmap
// Block 2:   inode bitmap
// Blocks 3-34: inode table  (32 blocks × 32 inodes = 1024 inodes)
// Block 35:  journal header
// Blocks 36-67: journal data slots (32 × 4 KB)
// Block 68+: user data

const BLOCK_BITMAP_BLK: u32  = 1;
const INODE_BITMAP_BLK: u32  = 2;
const INODE_TABLE_START: u32 = 3;
const INODE_TABLE_BLOCKS: u32 = 32;

const JOURNAL_HDR_BLK: u32  = 35;
const JOURNAL_DATA_BLK: u32 = 36;
const JOURNAL_SLOTS: usize   = 32;
const JOURNAL_MAGIC: u32     = 0x4F4A4C47; // "OJLG"

const DATA_START: u32 = JOURNAL_DATA_BLK + JOURNAL_SLOTS as u32; // = 68

#[allow(dead_code)]
const ITYPE_FREE: u8 = 0;
pub const ITYPE_FILE: u8 = 1;
pub const ITYPE_DIR: u8  = 2;
pub const ITYPE_SYMLINK: u8 = 3;

pub const ITYPE_FILE_ENTRY: u8 = 1;
pub const ITYPE_DIR_ENTRY: u8  = 2;
pub const ITYPE_SYMLINK_ENTRY: u8 = 3;

pub const ROOT_INODE: u32 = 0;

const INODE_CACHE_SIZE: usize = 32;
const SUPERBLOCK_CRC_OFF: usize = 12;

// ── CRC-32 ───────────────────────────────────────────────────────────────────

fn crc32(buf: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in buf {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 { crc = (crc >> 1) ^ 0xEDB88320; }
            else { crc >>= 1; }
        }
    }
    !crc
}

// ── Raw on-disk structures ───────────────────────────────────────────────────

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
    pub double_indirect: u32,
    pub _reserved: [u32; 14],
}

const _: () = assert!(core::mem::size_of::<RawInode>() == INODE_SIZE);

// DirEntry is not repr(C) — we read/write bytes manually to avoid unsafe casts.
// The on-disk layout is: inode_num(4) | name_len(1) | entry_type(1) | _pad(2) | name(56).

// ── Inode cache ──────────────────────────────────────────────────────────────

struct InodeCache {
    entries: [(u32, Option<RawInode>); INODE_CACHE_SIZE],
    generation: u64,
}

impl InodeCache {
    fn new() -> Self {
        Self { entries: [(0, None); INODE_CACHE_SIZE], generation: 1 }
    }

    fn get(&mut self, ino: u32) -> Option<RawInode> {
        for (key, val) in &self.entries {
            if *key == ino { return *val; }
        }
        None
    }

    fn insert(&mut self, ino: u32, inode: RawInode) {
        self.entries[self.generation as usize % INODE_CACHE_SIZE] = (ino, Some(inode));
        self.generation = self.generation.wrapping_add(1);
    }

    fn invalidate(&mut self, ino: u32) {
        for (key, val) in &mut self.entries {
            if *key == ino { *val = None; return; }
        }
    }
}

// ── OFS state ────────────────────────────────────────────────────────────────

pub struct OfsState {
    dev:          VirtioBlkProxy,
    total_blocks: u32,
    pending:      BTreeMap<u32, [u8; BLOCK_SIZE]>,
    cache:        InodeCache,
}

// ── Low-level I/O helpers ────────────────────────────────────────────────────

impl OfsState {
    fn write_blk(&mut self, blk: u32, buf: &[u8; BLOCK_SIZE]) -> bool {
        self.pending.insert(blk, *buf);
        true
    }

    fn read_blk(&mut self, blk: u32, buf: &mut [u8; BLOCK_SIZE]) -> bool {
        if let Some(data) = self.pending.get(&blk) {
            *buf = *data;
            return true;
        }
        self.dev.read_block(blk, buf)
    }

    // ── Journal ─────────────────────────────────────────────────────────────

    fn journal_replay(&self) {
        let mut hdr = [0u8; BLOCK_SIZE];
        if !self.dev.read_block(JOURNAL_HDR_BLK, &mut hdr) { return; }

        let magic     = u32_le(&hdr, 0);
        let n         = u32_le(&hdr, 4) as usize;
        let committed = u32_le(&hdr, 8);
        let stored_crc = u32_le(&hdr, 12);

        if magic != JOURNAL_MAGIC || committed != 1 || n == 0 || n > JOURNAL_SLOTS {
            return;
        }

        // Verify CRC of header (excluding the CRC field itself).
        let mut crc_buf = [0u8; 12];
        crc_buf[0..4].copy_from_slice(&magic.to_le_bytes());
        crc_buf[4..8].copy_from_slice(&(n as u32).to_le_bytes());
        crc_buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        if crc32(&crc_buf) != stored_crc { return; }

        for i in 0..n {
            let target = u32_le(&hdr, 16 + i * 4);
            if target == 0 { continue; }
            let mut data = [0u8; BLOCK_SIZE];
            if self.dev.read_block(JOURNAL_DATA_BLK + i as u32, &mut data) {
                self.dev.write_block(target, &data);
            }
        }

        let mut clear = [0u8; BLOCK_SIZE];
        clear[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        self.dev.write_block(JOURNAL_HDR_BLK, &clear);
    }

    pub fn commit_txn(&mut self) -> bool {
        if self.pending.is_empty() { return true; }

        let n = self.pending.len();
        if n > JOURNAL_SLOTS { return false; }

        let entries: Vec<(u32, [u8; BLOCK_SIZE])> =
            self.pending.iter().map(|(&k, &v)| (k, v)).collect();

        // Phase 1 – write each block snapshot to its journal data slot.
        // (The loop below does the actual write.)
        for (i, (_, data)) in entries.iter().enumerate() {
            if !self.dev.write_block(JOURNAL_DATA_BLK + i as u32, data) {
                return false;
            }
        }

        // Phase 2 – write journal header (committed = 0).
        let mut hdr = [0u8; BLOCK_SIZE];
        hdr[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        hdr[4..8].copy_from_slice(&(n as u32).to_le_bytes());
        hdr[8..12].copy_from_slice(&0u32.to_le_bytes());
        let mut crc_buf = [0u8; 12];
        crc_buf[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        crc_buf[4..8].copy_from_slice(&(n as u32).to_le_bytes());
        crc_buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        hdr[12..16].copy_from_slice(&crc32(&crc_buf).to_le_bytes());
        for (i, (target, _)) in entries.iter().enumerate() {
            let off = 16 + i * 4;
            hdr[off..off + 4].copy_from_slice(&target.to_le_bytes());
        }
        if !self.dev.write_block(JOURNAL_HDR_BLK, &hdr) { return false; }

        // Phase 3 – commit marker.
        hdr[8..12].copy_from_slice(&1u32.to_le_bytes());
        let mut crc_buf2 = [0u8; 12];
        crc_buf2[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        crc_buf2[4..8].copy_from_slice(&(n as u32).to_le_bytes());
        crc_buf2[8..12].copy_from_slice(&1u32.to_le_bytes());
        hdr[12..16].copy_from_slice(&crc32(&crc_buf2).to_le_bytes());
        if !self.dev.write_block(JOURNAL_HDR_BLK, &hdr) { return false; }

        // Phase 4 – apply to real block locations.
        for (target, data) in &entries {
            if !self.dev.write_block(*target, data) {
                return false;
            }
        }

        // Phase 5 – clear journal.
        let mut clear = [0u8; BLOCK_SIZE];
        clear[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        self.dev.write_block(JOURNAL_HDR_BLK, &clear);

        self.pending.clear();
        true
    }

    pub fn fsync(&mut self) -> bool {
        self.commit_txn()
    }

    // ── Bitmap helpers ───────────────────────────────────────────────────────

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

    // ── Safe byte-level inode serialization ────────────────────────────────

    fn raw_inode_read(buf: &[u8]) -> RawInode {
        RawInode {
            itype: buf[0],
            _pad: [buf[1], buf[2], buf[3]],
            size: u32::from_le_bytes(buf[4..8].try_into().unwrap_or([0; 4])),
            nlink: u32::from_le_bytes(buf[8..12].try_into().unwrap_or([0; 4])),
            used_blocks: u32::from_le_bytes(buf[12..16].try_into().unwrap_or([0; 4])),
            direct: {
                let mut d = [0u32; 12];
                for i in 0..12 {
                    d[i] = u32::from_le_bytes(buf[16 + i * 4..20 + i * 4].try_into().unwrap_or([0; 4]));
                }
                d
            },
            indirect: u32::from_le_bytes(buf[64..68].try_into().unwrap_or([0; 4])),
            double_indirect: u32::from_le_bytes(buf[68..72].try_into().unwrap_or([0; 4])),
            _reserved: {
                let mut r = [0u32; 14];
                for i in 0..14 {
                    r[i] = u32::from_le_bytes(buf[72 + i * 4..76 + i * 4].try_into().unwrap_or([0; 4]));
                }
                r
            },
        }
    }

    fn raw_inode_write(buf: &mut [u8], inode: &RawInode) {
        buf[0] = inode.itype;
        buf[1..4].copy_from_slice(&inode._pad);
        buf[4..8].copy_from_slice(&inode.size.to_le_bytes());
        buf[8..12].copy_from_slice(&inode.nlink.to_le_bytes());
        buf[12..16].copy_from_slice(&inode.used_blocks.to_le_bytes());
        for i in 0..12 {
            buf[16 + i * 4..20 + i * 4].copy_from_slice(&inode.direct[i].to_le_bytes());
        }
        buf[64..68].copy_from_slice(&inode.indirect.to_le_bytes());
        buf[68..72].copy_from_slice(&inode.double_indirect.to_le_bytes());
        for i in 0..14 {
            buf[72 + i * 4..76 + i * 4].copy_from_slice(&inode._reserved[i].to_le_bytes());
        }
    }

    // ── Inode I/O ────────────────────────────────────────────────────────────

    pub fn read_inode(&mut self, ino: u32) -> Option<RawInode> {
        if ino as usize >= MAX_INODES { return None; }

        if let Some(cached) = self.cache.get(ino) {
            return Some(cached);
        }

        let table_blk = INODE_TABLE_START + ino / INODES_PER_BLOCK as u32;
        let offset    = (ino as usize % INODES_PER_BLOCK) * INODE_SIZE;
        let mut buf   = [0u8; BLOCK_SIZE];
        if !self.read_blk(table_blk, &mut buf) { return None; }
        let inode = Self::raw_inode_read(&buf[offset..offset + INODE_SIZE]);
        self.cache.insert(ino, inode);
        Some(inode)
    }

    pub fn write_inode(&mut self, ino: u32, inode: &RawInode) -> bool {
        if ino as usize >= MAX_INODES { return false; }
        self.cache.invalidate(ino);
        let table_blk = INODE_TABLE_START + ino / INODES_PER_BLOCK as u32;
        let offset    = (ino as usize % INODES_PER_BLOCK) * INODE_SIZE;
        let mut buf   = [0u8; BLOCK_SIZE];
        if !self.read_blk(table_blk, &mut buf) { return false; }
        Self::raw_inode_write(&mut buf[offset..offset + INODE_SIZE], inode);
        self.write_blk(table_blk, &buf)
    }

    // ── Block allocation ─────────────────────────────────────────────────────

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

    fn free_indirect_blocks(&mut self, blk: u32, depth: u32) {
        if blk == 0 { return; }
        if depth == 0 {
            self.free_block(blk);
            return;
        }
        let mut ib_buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(blk, &mut ib_buf) { return; }
        let nptrs = BLOCK_SIZE / 4;
        for i in 0..nptrs {
            let b = u32::from_le_bytes(ib_buf[i * 4..i * 4 + 4].try_into().unwrap_or([0; 4]));
            if b != 0 { self.free_indirect_blocks(b, depth - 1); }
        }
        self.free_block(blk);
    }

    // ── Inode allocation ─────────────────────────────────────────────────────

    pub fn alloc_inode(&mut self) -> Option<u32> {
        self.bitmap_alloc(INODE_BITMAP_BLK, 1, MAX_INODES)
            .map(|i| i as u32)
    }

    pub fn free_inode(&mut self, ino: u32) {
        self.cache.invalidate(ino);
        self.bitmap_set(INODE_BITMAP_BLK, ino as usize, false);
    }

    // ── File block mapping ───────────────────────────────────────────────────

    fn get_file_block(&mut self, inode: &mut RawInode, ino: u32, idx: u32, alloc: bool)
        -> Option<u32>
    {
        // Direct blocks
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

        let nptrs = (BLOCK_SIZE / 4) as u32;
        let idx = idx - 12;

        // Single indirect
        if idx < nptrs {
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
            let blk = u32::from_le_bytes(ib_buf[idx as usize * 4..idx as usize * 4 + 4].try_into().ok()?);
            if blk != 0 { return Some(blk); }
            if !alloc    { return None; }
            let new_blk = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_blk(new_blk, &zeros);
            ib_buf[idx as usize * 4..idx as usize * 4 + 4].copy_from_slice(&new_blk.to_le_bytes());
            self.write_blk(inode.indirect, &ib_buf);
            inode.used_blocks += 1;
            self.write_inode(ino, inode);
            return Some(new_blk);
        }

        // Double indirect
        let idx = idx - nptrs;
        let per_indirect = nptrs;
        let outer = idx / per_indirect;
        let inner = idx % per_indirect;

        if outer >= nptrs { return None; }

        if inode.double_indirect == 0 {
            if !alloc { return None; }
            let dib = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_blk(dib, &zeros);
            inode.double_indirect = dib;
            inode.used_blocks += 1;
            self.write_inode(ino, inode);
        }

        let mut dib_buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(inode.double_indirect, &mut dib_buf) { return None; }

        let indirect_blk = u32::from_le_bytes(dib_buf[outer as usize * 4..outer as usize * 4 + 4].try_into().ok()?);

        let indirect_blk = if indirect_blk != 0 {
            indirect_blk
        } else if alloc {
            let ib = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_blk(ib, &zeros);
            dib_buf[outer as usize * 4..outer as usize * 4 + 4].copy_from_slice(&ib.to_le_bytes());
            self.write_blk(inode.double_indirect, &dib_buf);
            inode.used_blocks += 1;
            self.write_inode(ino, inode);
            ib
        } else {
            return None;
        };

        let mut ib_buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(indirect_blk, &mut ib_buf) { return None; }
        let blk = u32::from_le_bytes(ib_buf[inner as usize * 4..inner as usize * 4 + 4].try_into().ok()?);
        if blk != 0 { return Some(blk); }
        if !alloc    { return None; }

        let new_blk = self.alloc_block()?;
        let zeros = [0u8; BLOCK_SIZE];
        self.write_blk(new_blk, &zeros);
        ib_buf[inner as usize * 4..inner as usize * 4 + 4].copy_from_slice(&new_blk.to_le_bytes());
        self.write_blk(indirect_blk, &ib_buf);
        inode.used_blocks += 1;
        self.write_inode(ino, inode);
        Some(new_blk)
    }

    // ── Directory helpers ────────────────────────────────────────────────────

    pub fn dir_lookup_inode(&mut self, dir_ino: u32, name: &str) -> Option<(u32, u8)> {
        let inode = self.read_inode(dir_ino)?;
        if inode.itype != ITYPE_DIR { return None; }
        let name_bytes   = name.as_bytes();
        if name_bytes.len() > 55 { return None; }
        let total_entries = (inode.size as usize).div_ceil(DIRENTRY_SIZE);
        let mut seen = 0;

        // We scan all blocks (direct, indirect, double-indirect) the same way
        // file_read does, but directory blocks only use direct+indirect for now.
        // Since we're limited by inode.size, we can just iterate file blocks.
        let mut block_idx = 0u32;
        loop {
            let pos = block_idx as usize * BLOCK_SIZE;
            if pos >= inode.size as usize { break; }
            let blk = self.get_file_block(&mut inode.clone(), dir_ino, block_idx, false)?;
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut buf) { break; }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if seen >= total_entries { return None; }
                let byte_off = slot * DIRENTRY_SIZE;
                let (de_inode, de_name_len, de_entry_type, _) = dirent_read(&buf, byte_off);
                seen += 1;
                if de_name_len == 0 { continue; }
                if dirent_name_eq(&buf, byte_off, de_name_len as usize, name_bytes) {
                    return Some((de_inode, de_entry_type));
                }
            }
            block_idx += 1;
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

    pub fn dir_link_entry(&mut self, dir_ino: u32, name: &str, child_ino: u32, etype: u8) -> bool {
        let mut child = match self.read_inode(child_ino) {
            Some(i) => i,
            None => return false,
        };
        child.nlink = child.nlink.saturating_add(1);
        self.write_inode(child_ino, &child);
        let ok = self.dir_add_entry(dir_ino, name, child_ino, etype);
        if !ok {
            child.nlink = child.nlink.saturating_sub(1);
            self.write_inode(child_ino, &child);
        }
        ok
    }

    pub fn dir_add_entry(&mut self, dir_ino: u32, name: &str, child_ino: u32, etype: u8) -> bool {
        let mut inode = match self.read_inode(dir_ino) { Some(i) => i, None => return false };
        if inode.itype != ITYPE_DIR { return false; }
        let name_bytes = name.as_bytes();
        if name_bytes.len() > 55 { return false; }

        let total_entries = (inode.size as usize).div_ceil(DIRENTRY_SIZE);

        let mut block_idx = 0u32;
        loop {
            let coverage = block_idx as usize * DIRENTRIES_PER_BLOCK;
            if coverage >= total_entries { break; }
            let blk = self.get_file_block(&mut inode, dir_ino, block_idx, false);
            if blk.is_none() { block_idx += 1; continue; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk.unwrap(), &mut buf) { continue; }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if coverage + slot >= total_entries { break; }
                let byte_off = slot * DIRENTRY_SIZE;
                let (_, name_len, _, _) = dirent_read(&buf, byte_off);
                if name_len == 0 {
                    dirent_write(&mut buf, byte_off, child_ino, name_bytes, etype);
                    self.write_blk(blk.unwrap(), &buf);
                    let slot_pos = (coverage + slot + 1) * DIRENTRY_SIZE;
                    if slot_pos > inode.size as usize {
                        inode.size = slot_pos as u32;
                        self.write_inode(dir_ino, &inode);
                    }
                    // If adding a subdirectory, bump parent's nlink
                    if etype == ITYPE_DIR_ENTRY || etype == 2 {
                        if let Some(mut parent) = self.read_inode(dir_ino) {
                            parent.nlink = parent.nlink.saturating_add(1);
                            self.write_inode(dir_ino, &parent);
                        }
                    }
                    return self.commit_txn();
                }
            }
            block_idx += 1;
        }

        // Append — allocate a new directory block via get_file_block.
        let next_block_idx = (inode.size as usize).div_ceil(BLOCK_SIZE) as u32;
        let blk = self.get_file_block(&mut inode, dir_ino, next_block_idx, true)
            .unwrap_or(0);
        if blk == 0 { return false; }
        let mut buf = [0u8; BLOCK_SIZE];
        dirent_write(&mut buf, 0, child_ino, name_bytes, etype);
        self.write_blk(blk, &buf);
        let new_size = ((next_block_idx as usize * DIRENTRIES_PER_BLOCK) + 1) * DIRENTRY_SIZE;
        if new_size > inode.size as usize {
            inode.size = new_size as u32;
            self.write_inode(dir_ino, &inode);
        }
        // If adding a subdirectory, bump parent's nlink
        if etype == ITYPE_DIR_ENTRY || etype == 2 {
            if let Some(mut parent) = self.read_inode(dir_ino) {
                parent.nlink = parent.nlink.saturating_add(1);
                self.write_inode(dir_ino, &parent);
            }
        }
        self.commit_txn()
    }

    pub fn init_dir(&mut self, ino: u32, parent_ino: u32) -> bool {
        let mut inode = match self.read_inode(ino) { Some(i) => i, None => return false };
        if inode.itype != ITYPE_DIR { return false; }
        // Allocate a block for "." and ".."
        let blk = match self.alloc_block() { Some(b) => b, None => return false };
        let mut buf = [0u8; BLOCK_SIZE];
        dirent_write(&mut buf, 0, ino, b".", ITYPE_DIR_ENTRY);
        dirent_write(&mut buf, DIRENTRY_SIZE, parent_ino, b"..", ITYPE_DIR_ENTRY);
        self.write_blk(blk, &buf);
        inode.direct[0] = blk;
        inode.size = (2 * DIRENTRY_SIZE) as u32;
        inode.nlink = 2;
        inode.used_blocks = 1;
        self.write_inode(ino, &inode);
        self.commit_txn();
        true
    }

    pub fn dir_readall(&mut self, dir_ino: u32) -> Vec<(alloc::string::String, u32, u8)> {
        let mut result = Vec::new();
        let inode = match self.read_inode(dir_ino) { Some(i) => i, None => return result };
        if inode.itype != ITYPE_DIR { return result; }
        let total_entries = (inode.size as usize).div_ceil(DIRENTRY_SIZE);
        let mut seen = 0;

        let mut block_idx = 0u32;
        loop {
            let pos = block_idx as usize * BLOCK_SIZE;
            if pos >= inode.size as usize { break; }
            let blk = self.get_file_block(&mut inode.clone(), dir_ino, block_idx, false);
            if blk.is_none() { break; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk.unwrap(), &mut buf) { break; }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if seen >= total_entries { break; }
                let byte_off = slot * DIRENTRY_SIZE;
                let (de_inode, de_name_len, de_entry_type, name_start) = dirent_read(&buf, byte_off);
                seen += 1;
                if de_name_len == 0 { continue; }
                let name = dirent_name(&buf, byte_off, de_name_len as usize);
                if let Ok(s) = core::str::from_utf8(name) {
                    result.push((alloc::string::String::from(s), de_inode, de_entry_type));
                }
            }
            block_idx += 1;
        }
        result
    }

    // ── File read/write ──────────────────────────────────────────────────────

    pub fn file_read(&mut self, ino: u32, offset: usize, buf: &mut [u8])
        -> Result<usize, FsError>
    {
        let inode = self.read_inode(ino).ok_or(FsError::BadInode)?;
        if inode.itype != ITYPE_FILE && inode.itype != ITYPE_SYMLINK { return Err(FsError::NotFile); }
        if offset >= inode.size as usize { return Ok(0); }
        let available = inode.size as usize - offset;
        let to_read   = core::cmp::min(available, buf.len());
        let mut done  = 0usize;
        let mut inode_mut = inode;

        while done < to_read {
            let pos       = offset + done;
            let block_idx = (pos / BLOCK_SIZE) as u32;
            let block_off = pos % BLOCK_SIZE;
            let chunk     = core::cmp::min(to_read - done, BLOCK_SIZE - block_off);
            let blk = self.get_file_block(&mut inode_mut, ino, block_idx, false)
                .ok_or(FsError::MissingBlock)?;
            let mut blk_buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut blk_buf) { return Err(FsError::BlockRead); }
            buf[done..done + chunk].copy_from_slice(&blk_buf[block_off..block_off + chunk]);
            done += chunk;
        }
        Ok(to_read)
    }

    pub fn file_write(&mut self, ino: u32, offset: usize, data: &[u8])
        -> Result<usize, FsError>
    {
        let mut inode = self.read_inode(ino).ok_or(FsError::BadInode)?;
        if inode.itype != ITYPE_FILE && inode.itype != ITYPE_SYMLINK { return Err(FsError::NotFile); }
        let end  = offset + data.len();
        let mut done = 0usize;

        while done < data.len() {
            let pos       = offset + done;
            let block_idx = (pos / BLOCK_SIZE) as u32;
            let block_off = pos % BLOCK_SIZE;
            let chunk     = core::cmp::min(data.len() - done, BLOCK_SIZE - block_off);
            let blk = self.get_file_block(&mut inode, ino, block_idx, true)
                .ok_or(FsError::NoSpace)?;
            let mut blk_buf = [0u8; BLOCK_SIZE];
            if (block_off != 0 || chunk < BLOCK_SIZE) && !self.read_blk(blk, &mut blk_buf) {
                return Err(FsError::BlockRead);
            }
            blk_buf[block_off..block_off + chunk].copy_from_slice(&data[done..done + chunk]);
            if !self.write_blk(blk, &blk_buf) { return Err(FsError::BlockWrite); }
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
            self.free_indirect_blocks(inode.indirect, 1);
        }
        if inode.double_indirect != 0 {
            self.free_indirect_blocks(inode.double_indirect, 2);
        }
        inode.direct = [0u32; 12];
        inode.indirect = 0;
        inode.double_indirect = 0;
        inode.size = 0;
        inode.used_blocks = 0;
        self.write_inode(ino, &inode);
        self.commit_txn();
    }

    pub fn dir_unlink(&mut self, dir_ino: u32, name: &str) -> bool {
        let inode = match self.read_inode(dir_ino) { Some(i) => i, None => return false };
        let name_bytes    = name.as_bytes();
        let total_entries = (inode.size as usize).div_ceil(DIRENTRY_SIZE);
        let mut seen = 0;

        let mut block_idx = 0u32;
        loop {
            let coverage = block_idx as usize * DIRENTRIES_PER_BLOCK;
            if coverage >= total_entries { break; }
            let blk = self.get_file_block(&mut inode.clone(), dir_ino, block_idx, false);
            if blk.is_none() { break; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk.unwrap(), &mut buf) { break; }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if seen >= total_entries { break; }
                let byte_off = slot * DIRENTRY_SIZE;
                let (de_inode, de_name_len, de_entry_type, _) = dirent_read(&buf, byte_off);
                seen += 1;
                if de_name_len == 0 { continue; }
                if dirent_name_eq(&buf, byte_off, de_name_len as usize, name_bytes) {
                    let child_ino = de_inode;
                    let mut child = self.read_inode(child_ino).unwrap_or_default();
                    let is_dir = de_entry_type == ITYPE_DIR_ENTRY || de_entry_type == 2;
                    if child.nlink > 0 { child.nlink -= 1; }
                    if child.nlink == 0 {
                        self.file_truncate(child_ino);
                        self.free_inode(child_ino);
                    } else {
                        self.write_inode(child_ino, &child);
                    }
                    // If removing a subdirectory, decrement parent's nlink
                    if is_dir {
                        if let Some(mut parent) = self.read_inode(dir_ino) {
                            if parent.nlink > 0 { parent.nlink -= 1; }
                            self.write_inode(dir_ino, &parent);
                        }
                    }
                    dirent_clear(&mut buf, byte_off);
                    self.write_blk(blk.unwrap(), &buf);
                    self.commit_txn();
                    return true;
                }
            }
            block_idx += 1;
        }
        false
    }

    // ── Init / mkfs ──────────────────────────────────────────────────────────

    pub fn new(dev: VirtioBlkProxy) -> Option<Self> {
        let mut buf = [0u8; BLOCK_SIZE];
        if !dev.read_block(0, &mut buf) { return None; }

        let total_blocks = 2048;
        let mut state = Self {
            dev,
            total_blocks,
            pending: BTreeMap::new(),
            cache: InodeCache::new(),
        };

        state.journal_replay();

        let magic   = u32_le(&buf, 0);
        let version = u32_le(&buf, 4);
        let stored_total = u32_le(&buf, 8);
        let stored_crc   = u32_le(&buf, SUPERBLOCK_CRC_OFF);

        // Verify superblock CRC (CRC of magic + version + total_blocks).
        let mut crc_buf = [0u8; 12];
        crc_buf[0..4].copy_from_slice(&magic.to_le_bytes());
        crc_buf[4..8].copy_from_slice(&version.to_le_bytes());
        crc_buf[8..12].copy_from_slice(&stored_total.to_le_bytes());
        let crc_ok = crc32(&crc_buf) == stored_crc;

        if !(magic == OFS_MAGIC && version == OFS_VERSION && crc_ok) {
            state.total_blocks = if stored_total > 68 { stored_total } else { 2048 };
            state.mkfs();
        } else {
            state.total_blocks = stored_total;
        }
        Some(state)
    }

    pub fn mkfs(&mut self) {
        let total = self.total_blocks;

        // Superblock
        let mut buf = [0u8; BLOCK_SIZE];
        buf[0..4].copy_from_slice(&OFS_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&OFS_VERSION.to_le_bytes());
        buf[8..12].copy_from_slice(&total.to_le_bytes());
        let mut crc_buf = [0u8; 12];
        crc_buf[0..4].copy_from_slice(&OFS_MAGIC.to_le_bytes());
        crc_buf[4..8].copy_from_slice(&OFS_VERSION.to_le_bytes());
        crc_buf[8..12].copy_from_slice(&total.to_le_bytes());
        buf[12..16].copy_from_slice(&crc32(&crc_buf).to_le_bytes());
        self.dev.write_block(0, &buf);

        // Block bitmap: mark system + journal blocks used.
        buf.fill(0);
        for i in 0..DATA_START as usize {
            buf[i / 8] |= 1 << (i % 8);
        }
        self.dev.write_block(BLOCK_BITMAP_BLK, &buf);

        // Inode bitmap: all free.
        buf.fill(0);
        self.dev.write_block(INODE_BITMAP_BLK, &buf);

        // Inode table: zero out.
        buf.fill(0);
        for b in INODE_TABLE_START..INODE_TABLE_START + INODE_TABLE_BLOCKS {
            self.dev.write_block(b, &buf);
        }

        // Journal header (magic only).
        buf.fill(0);
        buf[0..4].copy_from_slice(&JOURNAL_MAGIC.to_le_bytes());
        self.dev.write_block(JOURNAL_HDR_BLK, &buf);

        // Root inode.
        self.bitmap_set(INODE_BITMAP_BLK, 0, true);
        let root = RawInode {
            itype: ITYPE_DIR,
            _pad: [0; 3],
            size: 0,
            nlink: 1,
            used_blocks: 0,
            direct: [0u32; 12],
            indirect: 0,
            double_indirect: 0,
            _reserved: [0u32; 14],
        };
        self.write_inode(ROOT_INODE, &root);
        self.commit_txn();
    }
}

// ── Common inode info (for main.rs dispatch) ────────────────────────────────

use crate::blockio::InodeInfo;

impl OfsState {
    pub fn inode_info(&mut self, ino: u32) -> Option<InodeInfo> {
        let raw = self.read_inode(ino)?;
        Some(InodeInfo { itype: raw.itype, size: raw.size, nlink: raw.nlink })
    }

    pub fn create_inode(&mut self, itype: u8) -> Option<u32> {
        let ino = self.alloc_inode()?;
        let inode = RawInode {
            itype,
            _pad: [0; 3],
            size: 0,
            nlink: 1,
            used_blocks: 0,
            direct: [0u32; 12],
            indirect: 0,
            double_indirect: 0,
            _reserved: [0u32; 14],
        };
        self.write_inode(ino, &inode);
        Some(ino)
    }
}

// ── Safe directory entry accessors ──────────────────────────────────────────

/// Read a directory entry from a block buffer at the given byte offset.
fn dirent_read(buf: &[u8; BLOCK_SIZE], byte_off: usize) -> (u32, u8, u8, usize) {
    let inode_num = u32::from_le_bytes(
        buf[byte_off..byte_off + 4].try_into().unwrap_or([0; 4])
    );
    let name_len = buf[byte_off + 4];
    let entry_type = buf[byte_off + 5];
    // _pad at byte_off+6..byte_off+8
    let name_start = byte_off + 8;
    (inode_num, name_len, entry_type, name_start)
}

/// Write a directory entry into a block buffer at the given byte offset.
fn dirent_write(buf: &mut [u8; BLOCK_SIZE], byte_off: usize, ino: u32, name: &[u8], etype: u8) {
    buf[byte_off..byte_off + 4].copy_from_slice(&ino.to_le_bytes());
    buf[byte_off + 4] = name.len() as u8;
    buf[byte_off + 5] = etype;
    // _pad at byte_off+6..byte_off+8 (implicit zeros)
    buf[byte_off + 8..byte_off + 8 + name.len()].copy_from_slice(name);
}

fn dirent_name<'a>(buf: &'a [u8; BLOCK_SIZE], byte_off: usize, name_len: usize) -> &'a [u8] {
    &buf[byte_off + 8..byte_off + 8 + name_len]
}

/// Compare a directory entry name at byte_off with the given bytes.
fn dirent_name_eq(buf: &[u8; BLOCK_SIZE], byte_off: usize, name_len: usize, name: &[u8]) -> bool {
    if name_len != name.len() { return false; }
    let start = byte_off + 8;
    &buf[start..start + name_len] == name
}

/// Zero out a directory entry (mark as unused).
fn dirent_clear(buf: &mut [u8; BLOCK_SIZE], byte_off: usize) {
    for b in &mut buf[byte_off..byte_off + DIRENTRY_SIZE] {
        *b = 0;
    }
}

// ── Byte helpers ─────────────────────────────────────────────────────────────

fn u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}

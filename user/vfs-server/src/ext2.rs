use alloc::vec::Vec;
use alloc::string::String;

use crate::blockio::{FsError, VirtioBlkProxy};

// ── Ext2 on-disk constants ───────────────────────────────────────────────────

pub const BLOCK_SIZE: usize = 4096;
const EXT2_MAGIC: u16 = 0xEF53;
const EXT2_SUPERBLOCK_OFF: usize = 1024;

// Superblock field offsets (within the 1024-byte superblock)
const SB_INODES_COUNT: usize       = 0;
const SB_BLOCKS_COUNT: usize       = 4;
const SB_R_BLOCKS_COUNT: usize     = 8;
const SB_FREE_BLOCKS: usize        = 12;
const SB_FREE_INODES: usize        = 16;
const SB_FIRST_DATA_BLOCK: usize   = 20;
const SB_LOG_BLOCK_SIZE: usize     = 24;
const SB_LOG_FRAG_SIZE: usize      = 28;
const SB_BLOCKS_PER_GROUP: usize   = 32;
const SB_FRAGS_PER_GROUP: usize    = 36;
const SB_INODES_PER_GROUP: usize   = 40;
const SB_MAGIC: usize              = 56;
const SB_REV_LEVEL: usize          = 76;
const SB_FIRST_INO: usize          = 84;
const SB_INODE_SIZE: usize         = 88;
const SB_BLOCK_GROUP_NR: usize     = 92;
const SB_FEATURE_COMPAT: usize     = 92;
const SB_FEATURE_INCOMPAT: usize   = 96;
const SB_FEATURE_RO_COMPAT: usize  = 100;
const SB_UUID: usize               = 104;
const SB_VOLUME_NAME: usize        = 120;
const SB_LAST_MOUNTED: usize       = 136;
const SB_ALGO_BITMAP: usize        = 152;
const SB_PREALLOC_BLOCKS: usize    = 156;
const SB_PREALLOC_DIR_BLOCKS: usize = 157;
const SB_PAD_DP: usize             = 158;

// Inode field offsets (128-byte inode, dynamic size via sb)
const INODE_MODE: usize            = 0;
const INODE_UID: usize             = 2;
const INODE_SIZE: usize            = 4;
const INODE_ATIME: usize           = 8;
const INODE_CTIME: usize           = 12;
const INODE_MTIME: usize           = 16;
const INODE_DTIME: usize           = 20;
const INODE_GID: usize             = 24;
const INODE_LINKS_COUNT: usize     = 26;
const INODE_BLOCKS: usize          = 28; // in 512-byte sectors
const INODE_FLAGS: usize           = 32;
const INODE_OSD1: usize            = 36;
const INODE_BLOCK: usize           = 40; // 15 × 4-byte block pointers
const INODE_GENERATION: usize      = 100;
const INODE_FILE_ACL: usize        = 104;
const INODE_DIR_ACL: usize         = 108;
const INODE_FADDR: usize           = 112;
const INODE_OSD2: usize            = 116;

const INODE_NDIR_BLOCKS: usize = 12;
const EXT2_IND_BLOCK: usize    = 12;
const EXT2_DIND_BLOCK: usize   = 13;
const EXT2_TIND_BLOCK: usize   = 14;

// Directory entry file_type values
const EXT2_FT_UNKNOWN: u8  = 0;
const EXT2_FT_REG_FILE: u8 = 1;
const EXT2_FT_DIR: u8      = 2;
const EXT2_FT_SYMLINK: u8  = 7;

// Inode mode bits
const EXT2_S_IFREG: u16   = 0x8000;
const EXT2_S_IFDIR: u16   = 0x4000;
const EXT2_S_IFLNK: u16   = 0xA000;

// Incompatible features we check for
const EXT2_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
const FAST_SYMLINK_MAX: usize = 60; // bytes that fit in 15 × u32 block pointers

// ── Raw superblock (1024 bytes) ──────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct RawSuperblock {
    data: [u8; 1024],
}

impl RawSuperblock {
    fn read(dev: &VirtioBlkProxy) -> Option<Self> {
        let mut blk = [0u8; BLOCK_SIZE];
        if !dev.read_block(0, &mut blk) { return None; }
        let mut sb = Self { data: [0u8; 1024] };
        sb.data.copy_from_slice(&blk[EXT2_SUPERBLOCK_OFF..EXT2_SUPERBLOCK_OFF + 1024]);
        let magic = u16::from_le_bytes(sb.data[SB_MAGIC..SB_MAGIC + 2].try_into().ok()?);
        if magic != EXT2_MAGIC { return None; }
        Some(sb)
    }

    fn store(&self, dev: &VirtioBlkProxy) -> bool {
        let mut blk = [0u8; BLOCK_SIZE];
        if !dev.read_block(0, &mut blk) { return false; }
        blk[EXT2_SUPERBLOCK_OFF..EXT2_SUPERBLOCK_OFF + 1024].copy_from_slice(&self.data);
        dev.write_block(0, &blk)
    }

    fn u32(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap_or([0; 4]))
    }

    fn set_u32(&mut self, off: usize, val: u32) {
        self.data[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn u16(&self, off: usize) -> u16 {
        u16::from_le_bytes(self.data[off..off + 2].try_into().unwrap_or([0; 2]))
    }

    fn set_u16(&mut self, off: usize, val: u16) {
        self.data[off..off + 2].copy_from_slice(&val.to_le_bytes());
    }
}

// ── Block group descriptor (32 bytes) ────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct RawBgDescriptor {
    data: [u8; 32],
}

impl RawBgDescriptor {
    fn bg_block_bitmap(&self) -> u32 {
        u32::from_le_bytes(self.data[0..4].try_into().unwrap_or([0; 4]))
    }
    fn bg_inode_bitmap(&self) -> u32 {
        u32::from_le_bytes(self.data[4..8].try_into().unwrap_or([0; 4]))
    }
    fn bg_inode_table(&self) -> u32 {
        u32::from_le_bytes(self.data[8..12].try_into().unwrap_or([0; 4]))
    }
    fn bg_free_blocks_count(&self) -> u16 {
        u16::from_le_bytes(self.data[12..14].try_into().unwrap_or([0; 2]))
    }
    fn bg_free_inodes_count(&self) -> u16 {
        u16::from_le_bytes(self.data[14..16].try_into().unwrap_or([0; 2]))
    }
    fn bg_used_dirs_count(&self) -> u16 {
        u16::from_le_bytes(self.data[16..18].try_into().unwrap_or([0; 2]))
    }
    fn set_bg_block_bitmap(&mut self, val: u32) {
        self.data[0..4].copy_from_slice(&val.to_le_bytes());
    }
    fn set_bg_inode_bitmap(&mut self, val: u32) {
        self.data[4..8].copy_from_slice(&val.to_le_bytes());
    }
    fn set_bg_inode_table(&mut self, val: u32) {
        self.data[8..12].copy_from_slice(&val.to_le_bytes());
    }
    fn set_bg_free_blocks_count(&mut self, val: u16) {
        self.data[12..14].copy_from_slice(&val.to_le_bytes());
    }
    fn set_bg_free_inodes_count(&mut self, val: u16) {
        self.data[14..16].copy_from_slice(&val.to_le_bytes());
    }
    fn set_bg_used_dirs_count(&mut self, val: u16) {
        self.data[16..18].copy_from_slice(&val.to_le_bytes());
    }
}

// ── Ext2 state ───────────────────────────────────────────────────────────────

pub struct Ext2State {
    dev: VirtioBlkProxy,
    sb: RawSuperblock,
    bgdt: Vec<RawBgDescriptor>,
    inode_size: usize,
    block_size: usize,
    inodes_per_group: u32,
    blocks_per_group: u32,
    first_data_block: u32,
    num_groups: u32,
    bgdt_blocks: u32,
}

// ── Inode type mapping ───────────────────────────────────────────────────────

pub const ITYPE_FILE_ENTRY: u8 = 1;
pub const ITYPE_DIR_ENTRY: u8  = 2;
pub const ITYPE_SYMLINK_ENTRY: u8 = 3;

fn ext2_ftype_to_entry_type(ft: u8) -> u8 {
    match ft {
        EXT2_FT_REG_FILE => ITYPE_FILE_ENTRY,
        EXT2_FT_DIR => ITYPE_DIR_ENTRY,
        EXT2_FT_SYMLINK => ITYPE_SYMLINK_ENTRY,
        _ => ITYPE_FILE_ENTRY,
    }
}

fn mode_to_entry_type(mode: u16) -> u8 {
    if mode & EXT2_S_IFDIR != 0 { ITYPE_DIR_ENTRY }
    else if mode & EXT2_S_IFLNK != 0 { ITYPE_SYMLINK_ENTRY }
    else { ITYPE_FILE_ENTRY }
}

// ── Block I/O ────────────────────────────────────────────────────────────────

impl Ext2State {
    fn read_block(&self, blk: u32, buf: &mut [u8; BLOCK_SIZE]) -> bool {
        self.dev.read_block(blk, buf)
    }

    fn write_block(&self, blk: u32, buf: &[u8; BLOCK_SIZE]) -> bool {
        self.dev.write_block(blk, buf)
    }

    fn read_blocks(&self, start: u32, buf: &mut [u8]) -> bool {
        let n = buf.len() / BLOCK_SIZE;
        let mut tmp = [0u8; BLOCK_SIZE];
        for i in 0..n {
            let offset = i * BLOCK_SIZE;
            if !self.dev.read_block(start + i as u32, &mut tmp) { return false; }
            buf[offset..offset + BLOCK_SIZE].copy_from_slice(&tmp);
        }
        true
    }

    fn write_blocks(&self, start: u32, buf: &[u8]) -> bool {
        let n = buf.len() / BLOCK_SIZE;
        for i in 0..n {
            let offset = i * BLOCK_SIZE;
            let mut tmp = [0u8; BLOCK_SIZE];
            tmp.copy_from_slice(&buf[offset..offset + BLOCK_SIZE]);
            if !self.dev.write_block(start + i as u32, &tmp) { return false; }
        }
        true
    }

    // ── Superblock helpers ───────────────────────────────────────────────────

    fn sb_u32(&self, off: usize) -> u32 { self.sb.u32(off) }
    fn sb_u16(&self, off: usize) -> u16 { self.sb.u16(off) }

    fn update_sb(&mut self) -> bool {
        self.sb.store(&self.dev)
    }

    fn update_bgdt(&mut self) -> bool {
        let bgdt_size = self.num_groups as usize * 32;
        let mut buf = alloc::vec![0u8; bgdt_size];
        for i in 0..self.num_groups as usize {
            buf[i * 32..(i + 1) * 32].copy_from_slice(&self.bgdt[i].data);
        }
        let bgdt_start = if self.first_data_block == 0 { 1 } else { self.first_data_block + 1 };
        self.write_blocks(bgdt_start, &buf)
    }

    // ── Bitmap helpers ───────────────────────────────────────────────────────

    fn group_for_block(&self, blk: u32) -> u32 {
        blk / self.blocks_per_group
    }

    fn group_for_inode(&self, ino: u32) -> (u32, u32) {
        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;
        (group, index)
    }

    fn bitmap_set(&mut self, bitmap_blk: u32, bit: usize, value: bool) {
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_block(bitmap_blk, &mut buf) { return; }
        if value { buf[bit / 8] |=  1 << (bit % 8); }
        else      { buf[bit / 8] &= !(1 << (bit % 8)); }
        self.write_block(bitmap_blk, &buf);
    }

    fn bitmap_alloc(&mut self, bitmap_blk: u32, start: usize, limit: usize) -> Option<usize> {
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_block(bitmap_blk, &mut buf) { return None; }
        for i in start..limit {
            if buf[i / 8] & (1 << (i % 8)) == 0 {
                buf[i / 8] |= 1 << (i % 8);
                self.write_block(bitmap_blk, &buf);
                return Some(i);
            }
        }
        None
    }

    // ── Block allocation ─────────────────────────────────────────────────────

    fn alloc_block(&mut self) -> Option<u32> {
        let total = self.sb_u32(SB_BLOCKS_COUNT);
        // Try each group's block bitmap
        for g in 0..self.num_groups {
            let start = g * self.blocks_per_group;
            let end = core::cmp::min(total, start + self.blocks_per_group);
            let limit = (end - start) as usize;
            let bgb = self.bgdt[g as usize].bg_block_bitmap();
            let blk_off = self.bitmap_alloc(bgb, 0, limit)?;
            let blk = start + blk_off as u32;
            // Update group and superblock free counts
            let free = self.bgdt[g as usize].bg_free_blocks_count();
            self.bgdt[g as usize].set_bg_free_blocks_count(free - 1);
            self.sb.set_u32(SB_FREE_BLOCKS, self.sb_u32(SB_FREE_BLOCKS) - 1);
            self.update_sb();
            self.update_bgdt();
            return Some(blk);
        }
        None
    }

    fn free_block(&mut self, blk: u32) {
        let g = self.group_for_block(blk);
        let offset = blk % self.blocks_per_group;
        let bgb = self.bgdt[g as usize].bg_block_bitmap();
        self.bitmap_set(bgb, offset as usize, false);
        let free = self.bgdt[g as usize].bg_free_blocks_count();
        self.bgdt[g as usize].set_bg_free_blocks_count(free + 1);
        self.sb.set_u32(SB_FREE_BLOCKS, self.sb_u32(SB_FREE_BLOCKS) + 1);
        self.update_sb();
        self.update_bgdt();
    }

    // ── Inode allocation ─────────────────────────────────────────────────────

    pub fn alloc_inode(&mut self) -> Option<u32> {
        let total = self.sb_u32(SB_INODES_COUNT);
        for g in 0..self.num_groups {
            let free = self.bgdt[g as usize].bg_free_inodes_count();
            if free == 0 { continue; }
            let start = g * self.inodes_per_group;
            let end = core::cmp::min(total, start + self.inodes_per_group);
            let limit = (end - start) as usize;
            let ibg = self.bgdt[g as usize].bg_inode_bitmap();
            let ino_off = self.bitmap_alloc(ibg, 0, limit)?;
            let ino = start + ino_off as u32 + 1;
            self.bgdt[g as usize].set_bg_free_inodes_count(free - 1);
            self.sb.set_u32(SB_FREE_INODES, self.sb_u32(SB_FREE_INODES) - 1);
            self.update_sb();
            self.update_bgdt();
            return Some(ino);
        }
        None
    }

    pub fn free_inode(&mut self, ino: u32) {
        let (g, idx) = self.group_for_inode(ino);
        let ibg = self.bgdt[g as usize].bg_inode_bitmap();
        self.bitmap_set(ibg, idx as usize, false);
        let free = self.bgdt[g as usize].bg_free_inodes_count();
        self.bgdt[g as usize].set_bg_free_inodes_count(free + 1);
        self.sb.set_u32(SB_FREE_INODES, self.sb_u32(SB_FREE_INODES) + 1);
        self.update_sb();
        self.update_bgdt();
    }

    // ── Inode I/O ────────────────────────────────────────────────────────────

    pub fn read_inode(&mut self, ino: u32) -> Option<[u8; 128]> {
        let total = self.sb_u32(SB_INODES_COUNT);
        if ino == 0 || ino > total { return None; }
        let (g, idx) = self.group_for_inode(ino);
        let inode_table = self.bgdt[g as usize].bg_inode_table();
        let inodes_per_block = BLOCK_SIZE / self.inode_size;
        let table_blk = inode_table + idx / inodes_per_block as u32;
        let offset = (idx as usize % inodes_per_block) * self.inode_size;
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_block(table_blk, &mut buf) { return None; }
        let mut inode = [0u8; 128];
        inode.copy_from_slice(&buf[offset..offset + 128]);
        Some(inode)
    }

    pub fn write_inode(&mut self, ino: u32, inode: &[u8; 128]) -> bool {
        let total = self.sb_u32(SB_INODES_COUNT);
        if ino == 0 || ino > total { return false; }
        let (g, idx) = self.group_for_inode(ino);
        let inode_table = self.bgdt[g as usize].bg_inode_table();
        let inodes_per_block = BLOCK_SIZE / self.inode_size;
        let table_blk = inode_table + idx / inodes_per_block as u32;
        let offset = (idx as usize % inodes_per_block) * self.inode_size;
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_block(table_blk, &mut buf) { return false; }
        buf[offset..offset + 128].copy_from_slice(inode);
        self.write_block(table_blk, &buf)
    }

    // ── Block pointer helpers ────────────────────────────────────────────────

    fn inode_block_ptr(inode: &[u8; 128], idx: usize) -> u32 {
        let off = INODE_BLOCK + idx * 4;
        u32::from_le_bytes(inode[off..off + 4].try_into().unwrap_or([0; 4]))
    }

    fn set_inode_block_ptr(inode: &mut [u8; 128], idx: usize, val: u32) {
        let off = INODE_BLOCK + idx * 4;
        inode[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn inode_size(inode: &[u8; 128]) -> u32 {
        u32::from_le_bytes(inode[INODE_SIZE..INODE_SIZE + 4].try_into().unwrap_or([0; 4]))
    }

    fn set_inode_size(inode: &mut [u8; 128], val: u32) {
        inode[INODE_SIZE..INODE_SIZE + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn inode_links(inode: &[u8; 128]) -> u16 {
        u16::from_le_bytes(inode[INODE_LINKS_COUNT..INODE_LINKS_COUNT + 2].try_into().unwrap_or([0; 2]))
    }

    fn set_inode_links(inode: &mut [u8; 128], val: u16) {
        inode[INODE_LINKS_COUNT..INODE_LINKS_COUNT + 2].copy_from_slice(&val.to_le_bytes());
    }

    fn inode_mode(inode: &[u8; 128]) -> u16 {
        u16::from_le_bytes(inode[INODE_MODE..INODE_MODE + 2].try_into().unwrap_or([0; 2]))
    }

    fn inode_blocks_512(inode: &[u8; 128]) -> u32 {
        u32::from_le_bytes(inode[INODE_BLOCKS..INODE_BLOCKS + 4].try_into().unwrap_or([0; 4]))
    }

    fn set_inode_blocks_512(inode: &mut [u8; 128], val: u32) {
        inode[INODE_BLOCKS..INODE_BLOCKS + 4].copy_from_slice(&val.to_le_bytes());
    }

    // ── File block mapping ──────────────────────────────────────────────────

    fn get_block_ptr(&self, block: u32, idx: u32) -> Option<u32> {
        let mut ib_buf = [0u8; BLOCK_SIZE];
        if !self.read_block(block, &mut ib_buf) { return None; }
        let off = idx as usize * 4;
        Some(u32::from_le_bytes(ib_buf[off..off + 4].try_into().ok()?))
    }

    fn set_block_ptr(&self, block: u32, idx: u32, val: u32) -> bool {
        let mut ib_buf = [0u8; BLOCK_SIZE];
        if !self.read_block(block, &mut ib_buf) { return false; }
        let off = idx as usize * 4;
        ib_buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
        self.write_block(block, &ib_buf)
    }

    fn get_file_block(&mut self, inode: &mut [u8; 128], ino: u32, idx: u32, alloc: bool)
        -> Option<u32>
    {
        let nptrs = (BLOCK_SIZE / 4) as u32;

        // Direct blocks
        if idx < INODE_NDIR_BLOCKS as u32 {
            let blk = Self::inode_block_ptr(inode, idx as usize);
            if blk != 0 { return Some(blk); }
            if !alloc { return None; }
            let new_blk = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_block(new_blk, &zeros);
            Self::set_inode_block_ptr(inode, idx as usize, new_blk);
            Self::set_inode_blocks_512(inode, Self::inode_blocks_512(inode) + (BLOCK_SIZE / 512) as u32);
            self.write_inode(ino, inode);
            return Some(new_blk);
        }

        let idx = idx - INODE_NDIR_BLOCKS as u32;

        // Single indirect
        if idx < nptrs {
            let ib = Self::inode_block_ptr(inode, EXT2_IND_BLOCK);
            let ib = if ib != 0 {
                ib
            } else if alloc {
                let new_ib = self.alloc_block()?;
                let zeros = [0u8; BLOCK_SIZE];
                self.write_block(new_ib, &zeros);
                Self::set_inode_block_ptr(inode, EXT2_IND_BLOCK, new_ib);
                Self::set_inode_blocks_512(inode, Self::inode_blocks_512(inode) + (BLOCK_SIZE / 512) as u32);
                self.write_inode(ino, inode);
                new_ib
            } else {
                return None;
            };

            let blk = self.get_block_ptr(ib, idx)?;
            if blk != 0 { return Some(blk); }
            if !alloc { return None; }
            let new_blk = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_block(new_blk, &zeros);
            self.set_block_ptr(ib, idx, new_blk);
            Self::set_inode_blocks_512(inode, Self::inode_blocks_512(inode) + (BLOCK_SIZE / 512) as u32);
            self.write_inode(ino, inode);
            return Some(new_blk);
        }

        // Double indirect
        let idx = idx - nptrs;
        let outer = idx / nptrs;
        let inner = idx % nptrs;
        if outer >= nptrs { return None; }

        let dib = Self::inode_block_ptr(inode, EXT2_DIND_BLOCK);
        let dib = if dib != 0 {
            dib
        } else if alloc {
            let new_dib = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_block(new_dib, &zeros);
            Self::set_inode_block_ptr(inode, EXT2_DIND_BLOCK, new_dib);
            Self::set_inode_blocks_512(inode, Self::inode_blocks_512(inode) + (BLOCK_SIZE / 512) as u32);
            self.write_inode(ino, inode);
            new_dib
        } else {
            return None;
        };

        let ib = self.get_block_ptr(dib, outer)?;
        let ib = if ib != 0 {
            ib
        } else if alloc {
            let new_ib = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_block(new_ib, &zeros);
            self.set_block_ptr(dib, outer, new_ib);
            Self::set_inode_blocks_512(inode, Self::inode_blocks_512(inode) + (BLOCK_SIZE / 512) as u32);
            self.write_inode(ino, inode);
            new_ib
        } else {
            return None;
        };

        let blk = self.get_block_ptr(ib, inner)?;
        if blk != 0 { return Some(blk); }
        if !alloc { return None; }

        let new_blk = self.alloc_block()?;
        let zeros = [0u8; BLOCK_SIZE];
        self.write_block(new_blk, &zeros);
        self.set_block_ptr(ib, inner, new_blk);
        Self::set_inode_blocks_512(inode, Self::inode_blocks_512(inode) + (BLOCK_SIZE / 512) as u32);
        self.write_inode(ino, inode);
        Some(new_blk)
    }

    // ── Directory helpers ───────────────────────────────────────────────────

    pub fn dir_lookup_inode(&mut self, dir_ino: u32, name: &str) -> Option<(u32, u8)> {
        let inode = self.read_inode(dir_ino)?;
        if Self::inode_mode(&inode) & EXT2_S_IFDIR == 0 { return None; }
        let name_bytes = name.as_bytes();
        let size = Self::inode_size(&inode) as usize;
        let mut pos = 0usize;
        // ext2 variable-length directory entries, iterating through blocks
        loop {
            if pos >= size { return None; }
            let block_idx = pos / BLOCK_SIZE;
            let _block_off = pos % BLOCK_SIZE;
            let blk = self.get_file_block(&mut inode.clone(), dir_ino, block_idx as u32, false);
            if blk.is_none() { return None; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_block(blk.unwrap(), &mut buf) { return None; }

            let block_limit = core::cmp::min(BLOCK_SIZE, size - block_idx * BLOCK_SIZE);
            loop {
                let off = pos % BLOCK_SIZE;
                if off + 8 > block_limit || pos >= size { break; }
                let de_inode = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]));
                let rec_len = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap_or([0; 2])) as usize;
                let name_len = buf[off + 6] as usize;
                let file_type = buf[off + 7];

                if rec_len == 0 { break; }
                if de_inode == 0 { pos += rec_len; continue; }

                if name_len == name_bytes.len() && off + 8 + name_len <= BLOCK_SIZE {
                    if &buf[off + 8..off + 8 + name_len] == name_bytes {
                        return Some((de_inode, ext2_ftype_to_entry_type(file_type)));
                    }
                }
                pos += rec_len;
            }
            // Align to next block
            pos = (pos + BLOCK_SIZE - 1) / BLOCK_SIZE * BLOCK_SIZE;
        }
    }

    pub fn lookup_path(&mut self, path: &str) -> Option<(u32, u8)> {
        const EXT2_ROOT_INO: u32 = 2;
        let mut cur_ino = EXT2_ROOT_INO;
        let path = path.trim_matches('/');
        if path.is_empty() {
            let root_inode = self.read_inode(EXT2_ROOT_INO)?;
            return Some((EXT2_ROOT_INO, mode_to_entry_type(Self::inode_mode(&root_inode))));
        }
        for comp in path.split('/') {
            if comp.is_empty() { continue; }
            let (next_ino, _) = self.dir_lookup_inode(cur_ino, comp)?;
            cur_ino = next_ino;
            if cur_ino == 0 { return None; }
            // Walk into directory for next component; for last component return type
        }
        let inode = self.read_inode(cur_ino)?;
        Some((cur_ino, mode_to_entry_type(Self::inode_mode(&inode))))
    }

    pub fn dir_add_entry(&mut self, dir_ino: u32, name: &str, child_ino: u32, etype: u8) -> bool {
        let name_bytes = name.as_bytes();
        if name_bytes.is_empty() || name_bytes.len() > 255 { return false; }

        let file_type = match etype {
            ITYPE_DIR_ENTRY => EXT2_FT_DIR,
            ITYPE_SYMLINK_ENTRY => EXT2_FT_SYMLINK,
            _ => EXT2_FT_REG_FILE,
        };

        let entry_size = (8 + name_bytes.len() + 3) & !3usize;
        if entry_size > BLOCK_SIZE { return false; }

        let mut inode = match self.read_inode(dir_ino) { Some(i) => i, None => return false };
        let dir_size = Self::inode_size(&inode) as usize;

        // Scan existing blocks for a free (inode=0) slot large enough to hold this entry
        let mut pos = 0usize;
        while pos < dir_size {
            let block_idx = pos / BLOCK_SIZE;
            let blk = match self.get_file_block(&mut inode, dir_ino, block_idx as u32, false) {
                Some(b) => b,
                None => return false,
            };

            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_block(blk, &mut buf) { return false; }
            let block_limit = core::cmp::min(BLOCK_SIZE, dir_size - block_idx * BLOCK_SIZE);

            let mut off = 0usize;
            while off + 8 <= block_limit {
                let de_inode = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]));
                let rec_len = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap_or([0; 2])) as usize;
                if rec_len == 0 { break; }

                if de_inode == 0 {
                    // Free entry — check if it fits
                    if rec_len >= entry_size {
                        let remaining = rec_len - entry_size;
                        buf[off..off + 4].copy_from_slice(&child_ino.to_le_bytes());
                        buf[off + 6] = name_bytes.len() as u8;
                        buf[off + 7] = file_type;
                        buf[off + 8..off + 8 + name_bytes.len()].copy_from_slice(name_bytes);
                        if remaining >= 8 {
                            let new_rec_len = entry_size as u16;
                            buf[off + 4..off + 6].copy_from_slice(&new_rec_len.to_le_bytes());
                            let dummy_off = off + entry_size;
                            buf[dummy_off..dummy_off + 4].fill(0);
                            buf[dummy_off + 6] = 0;
                            buf[dummy_off + 7] = 0;
                            let dummy_rec_len = remaining as u16;
                            buf[dummy_off + 4..dummy_off + 6].copy_from_slice(&dummy_rec_len.to_le_bytes());
                        }
                        self.write_block(blk, &buf);
                        // If adding a subdirectory, bump parent's nlink + bgdt dir count
                        if etype == ITYPE_DIR_ENTRY || etype == 2 {
                            let mut parent_inode = self.read_inode(dir_ino).unwrap_or([0u8; 128]);
                            let pn = Self::inode_links(&parent_inode);
                            Self::set_inode_links(&mut parent_inode, pn + 1);
                            self.write_inode(dir_ino, &parent_inode);
                            let (g, _) = self.group_for_inode(dir_ino);
                            let dc = self.bgdt[g as usize].bg_used_dirs_count();
                            self.bgdt[g as usize].set_bg_used_dirs_count(dc + 1);
                            self.update_bgdt();
                        }
                        return true;
                    }
                }

                off += rec_len;
            }

            pos += BLOCK_SIZE;
        }

        // No free slot found — allocate a new directory block
        let next_block_idx = dir_size.div_ceil(BLOCK_SIZE);
        let blk = match self.get_file_block(&mut inode, dir_ino, next_block_idx as u32, true) {
            Some(b) => b,
            None => return false,
        };

        let mut buf = [0u8; BLOCK_SIZE];
        buf[0..4].copy_from_slice(&child_ino.to_le_bytes());
        buf[6] = name_bytes.len() as u8;
        buf[7] = file_type;
        buf[8..8 + name_bytes.len()].copy_from_slice(name_bytes);

        let remaining = BLOCK_SIZE - entry_size;
        if remaining >= 8 {
            let new_rec_len = entry_size as u16;
            buf[4..6].copy_from_slice(&new_rec_len.to_le_bytes());
            let dummy_rec_len = remaining as u16;
            buf[entry_size + 4..entry_size + 6].copy_from_slice(&dummy_rec_len.to_le_bytes());
        } else {
            let new_rec_len = BLOCK_SIZE as u16;
            buf[4..6].copy_from_slice(&new_rec_len.to_le_bytes());
        }

        self.write_block(blk, &buf);

        let new_size = next_block_idx * BLOCK_SIZE + entry_size;
        if new_size > dir_size {
            Self::set_inode_size(&mut inode, new_size as u32);
            self.write_inode(dir_ino, &inode);
        }
        // If adding a subdirectory, bump parent's nlink + bgdt dir count
        if etype == ITYPE_DIR_ENTRY || etype == 2 {
            let mut parent_inode = self.read_inode(dir_ino).unwrap_or([0u8; 128]);
            let pn = Self::inode_links(&parent_inode);
            Self::set_inode_links(&mut parent_inode, pn + 1);
            self.write_inode(dir_ino, &parent_inode);
            let (g, _) = self.group_for_inode(dir_ino);
            let dc = self.bgdt[g as usize].bg_used_dirs_count();
            self.bgdt[g as usize].set_bg_used_dirs_count(dc + 1);
            self.update_bgdt();
        }
        true
    }

    pub fn init_dir(&mut self, ino: u32, parent_ino: u32) -> bool {
        let mut inode = match self.read_inode(ino) { Some(i) => i, None => return false };
        if Self::inode_mode(&inode) & EXT2_S_IFDIR == 0 { return false; }

        let blk = match self.alloc_block() { Some(b) => b, None => return false };
        let mut buf = [0u8; BLOCK_SIZE];

        buf[0..4].copy_from_slice(&ino.to_le_bytes());
        let rec_len_dot: u16 = 12;
        buf[4..6].copy_from_slice(&rec_len_dot.to_le_bytes());
        buf[6] = 1;
        buf[7] = EXT2_FT_DIR;
        buf[8] = b'.';

        buf[12..16].copy_from_slice(&parent_ino.to_le_bytes());
        let rec_len_dotdot = (BLOCK_SIZE - 12) as u16;
        buf[16..18].copy_from_slice(&rec_len_dotdot.to_le_bytes());
        buf[18] = 2;
        buf[19] = EXT2_FT_DIR;
        buf[20] = b'.';
        buf[21] = b'.';

        self.write_block(blk, &buf);

        Self::set_inode_block_ptr(&mut inode, 0, blk);
        Self::set_inode_size(&mut inode, BLOCK_SIZE as u32);
        Self::set_inode_links(&mut inode, 2);
        Self::set_inode_blocks_512(&mut inode, (BLOCK_SIZE / 512) as u32);
        self.write_inode(ino, &inode);
        true
    }

    pub fn dir_link_entry(&mut self, dir_ino: u32, name: &str, child_ino: u32, etype: u8) -> bool {
        let mut child = match self.read_inode(child_ino) { Some(i) => i, None => return false };
        let nlink = Self::inode_links(&child);
        Self::set_inode_links(&mut child, nlink.saturating_add(1));
        self.write_inode(child_ino, &child);
        let ok = self.dir_add_entry(dir_ino, name, child_ino, etype);
        if !ok {
            Self::set_inode_links(&mut child, nlink);
            self.write_inode(child_ino, &child);
        }
        ok
    }

    pub fn dir_readall(&mut self, dir_ino: u32) -> Vec<(String, u32, u8)> {
        let mut result = Vec::new();
        let inode = match self.read_inode(dir_ino) { Some(i) => i, None => return result };
        if Self::inode_mode(&inode) & EXT2_S_IFDIR == 0 { return result; }
        let size = Self::inode_size(&inode) as usize;
        let mut pos = 0usize;

        loop {
            if pos >= size { break; }
            let block_idx = pos / BLOCK_SIZE;
            let blk = self.get_file_block(&mut inode.clone(), dir_ino, block_idx as u32, false);
            if blk.is_none() { break; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_block(blk.unwrap(), &mut buf) { break; }

            loop {
                let off = pos % BLOCK_SIZE;
                if off + 8 > BLOCK_SIZE || pos >= size { break; }
                let de_inode = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]));
                let rec_len = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap_or([0; 2])) as usize;
                let name_len = buf[off + 6] as usize;
                let file_type = buf[off + 7];
                if rec_len == 0 { break; }
                if de_inode != 0 && name_len > 0 && off + 8 + name_len <= BLOCK_SIZE {
                    if let Ok(s) = core::str::from_utf8(&buf[off + 8..off + 8 + name_len]) {
                        result.push((String::from(s), de_inode, ext2_ftype_to_entry_type(file_type)));
                    }
                }
                pos += rec_len;
            }
            pos = (pos + BLOCK_SIZE - 1) / BLOCK_SIZE * BLOCK_SIZE;
        }
        result
    }

    pub fn dir_unlink(&mut self, dir_ino: u32, name: &str) -> bool {
        let inode = match self.read_inode(dir_ino) { Some(i) => i, None => return false };
        if Self::inode_mode(&inode) & EXT2_S_IFDIR == 0 { return false; }
        let name_bytes = name.as_bytes();
        let size = Self::inode_size(&inode) as usize;
        let mut pos = 0usize;

        loop {
            if pos >= size { break; }
            let block_idx = pos / BLOCK_SIZE;
            let blk = self.get_file_block(&mut inode.clone(), dir_ino, block_idx as u32, false);
            if blk.is_none() { break; }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_block(blk.unwrap(), &mut buf) { break; }

            loop {
                let off = pos % BLOCK_SIZE;
                if off + 8 > BLOCK_SIZE || pos >= size { break; }
                let de_inode = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]));
                let rec_len = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap_or([0; 2])) as usize;
                let name_len = buf[off + 6] as usize;
                let file_type = buf[off + 7];
                if rec_len == 0 || de_inode == 0 { pos += rec_len; continue; }
                if name_len == name_bytes.len() && off + 8 + name_len <= BLOCK_SIZE
                    && &buf[off + 8..off + 8 + name_len] == name_bytes
                {
                    let child_ino = de_inode;
                    // Decrement nlink on child
                    let mut child = self.read_inode(child_ino).unwrap_or([0u8; 128]);
                    let nlink = Self::inode_links(&child);
                    if nlink > 0 { Self::set_inode_links(&mut child, nlink - 1); }
                    if nlink <= 1 {
                        self.file_truncate(child_ino);
                        self.free_inode(child_ino);
                    } else {
                        self.write_inode(child_ino, &child);
                    }
                    // If removing a subdirectory, decrement parent's nlink + bgdt dir count
                    if file_type == EXT2_FT_DIR {
                        let mut parent_inode = self.read_inode(dir_ino).unwrap_or([0u8; 128]);
                        let pn = Self::inode_links(&parent_inode);
                        if pn > 0 { Self::set_inode_links(&mut parent_inode, pn - 1); }
                        self.write_inode(dir_ino, &parent_inode);
                        let (g, _) = self.group_for_inode(dir_ino);
                        let dc = self.bgdt[g as usize].bg_used_dirs_count();
                        if dc > 0 { self.bgdt[g as usize].set_bg_used_dirs_count(dc - 1); }
                        self.update_bgdt();
                    }
                    // Mark entry as unused (inode = 0)
                    buf[off..off + 4].copy_from_slice(&[0; 4]);
                    self.write_block(blk.unwrap(), &buf);
                    return true;
                }
                pos += rec_len;
            }
            pos = (pos + BLOCK_SIZE - 1) / BLOCK_SIZE * BLOCK_SIZE;
        }
        false
    }

    // ── File read/write ─────────────────────────────────────────────────────

    pub fn file_read(&mut self, ino: u32, offset: usize, buf: &mut [u8])
        -> Result<usize, FsError>
    {
        let inode = self.read_inode(ino).ok_or(FsError::BadInode)?;
        let mode = Self::inode_mode(&inode);
        let is_reg = (mode & EXT2_S_IFREG) != 0;
        let is_lnk = (mode & EXT2_S_IFLNK) != 0;
        if !is_reg && !is_lnk { return Err(FsError::NotFile); }
        let file_size = Self::inode_size(&inode) as usize;
        if offset >= file_size { return Ok(0); }

        if is_lnk && Self::inode_blocks_512(&inode) == 0 {
            let available = file_size - offset;
            let to_read = core::cmp::min(available, buf.len());
            let inline_start = INODE_BLOCK + offset;
            buf[..to_read].copy_from_slice(&inode[inline_start..inline_start + to_read]);
            return Ok(to_read);
        }

        let to_read = core::cmp::min(file_size - offset, buf.len());
        let mut done = 0usize;
        let mut inode_mut = inode;

        while done < to_read {
            let pos = offset + done;
            let block_idx = (pos / BLOCK_SIZE) as u32;
            let block_off = pos % BLOCK_SIZE;
            let chunk = core::cmp::min(to_read - done, BLOCK_SIZE - block_off);
            let blk = self.get_file_block(&mut inode_mut, ino, block_idx, false)
                .ok_or(FsError::MissingBlock)?;
            let mut blk_buf = [0u8; BLOCK_SIZE];
            if !self.read_block(blk, &mut blk_buf) { return Err(FsError::BlockRead); }
            buf[done..done + chunk].copy_from_slice(&blk_buf[block_off..block_off + chunk]);
            done += chunk;
        }
        Ok(to_read)
    }

    pub fn file_write(&mut self, ino: u32, offset: usize, data: &[u8])
        -> Result<usize, FsError>
    {
        let mut inode = self.read_inode(ino).ok_or(FsError::BadInode)?;
        let mode = Self::inode_mode(&inode);
        let is_reg = (mode & EXT2_S_IFREG) != 0;
        let is_lnk = (mode & EXT2_S_IFLNK) != 0;
        if !is_reg && !is_lnk { return Err(FsError::NotFile); }

        if is_lnk && data.len() <= FAST_SYMLINK_MAX && offset == 0 {
            let inline_start = INODE_BLOCK;
            inode[inline_start..inline_start + data.len()].copy_from_slice(data);
            Self::set_inode_size(&mut inode, data.len() as u32);
            Self::set_inode_blocks_512(&mut inode, 0);
            self.write_inode(ino, &inode);
            return Ok(data.len());
        }

        if is_lnk && Self::inode_blocks_512(&inode) == 0 {
            Self::set_inode_size(&mut inode, 0);
            self.write_inode(ino, &inode);
            return self.file_write(ino, offset, data);
        }

        let end = offset + data.len();
        let mut done = 0usize;

        while done < data.len() {
            let pos = offset + done;
            let block_idx = (pos / BLOCK_SIZE) as u32;
            let block_off = pos % BLOCK_SIZE;
            let chunk = core::cmp::min(data.len() - done, BLOCK_SIZE - block_off);
            let blk = self.get_file_block(&mut inode, ino, block_idx, true)
                .ok_or(FsError::NoSpace)?;
            let mut blk_buf = [0u8; BLOCK_SIZE];
            if (block_off != 0 || chunk < BLOCK_SIZE) && !self.read_block(blk, &mut blk_buf) {
                return Err(FsError::BlockRead);
            }
            blk_buf[block_off..block_off + chunk].copy_from_slice(&data[done..done + chunk]);
            self.write_block(blk, &blk_buf);
            done += chunk;
        }

        if end > Self::inode_size(&inode) as usize {
            Self::set_inode_size(&mut inode, end as u32);
            self.write_inode(ino, &inode);
        }
        Ok(data.len())
    }

    pub fn file_truncate(&mut self, ino: u32) {
        let mut inode = match self.read_inode(ino) { Some(i) => i, None => return };

        // Fast symlink — nothing to free
        if Self::inode_blocks_512(&inode) == 0 {
            Self::set_inode_size(&mut inode, 0);
            self.write_inode(ino, &inode);
            return;
        }

        let nptrs = BLOCK_SIZE / 4;

        // Free direct blocks
        for i in 0..INODE_NDIR_BLOCKS {
            let blk = Self::inode_block_ptr(&inode, i);
            if blk != 0 { self.free_block(blk); }
            Self::set_inode_block_ptr(&mut inode, i, 0);
        }

        // Free single indirect chain
        let ib = Self::inode_block_ptr(&inode, EXT2_IND_BLOCK);
        if ib != 0 {
            for i in 0..nptrs {
                let b = self.get_block_ptr(ib, i as u32).unwrap_or(0);
                if b != 0 { self.free_block(b); }
            }
            self.free_block(ib);
            Self::set_inode_block_ptr(&mut inode, EXT2_IND_BLOCK, 0);
        }

        // Free double indirect chain
        let dib = Self::inode_block_ptr(&inode, EXT2_DIND_BLOCK);
        if dib != 0 {
            for i in 0..nptrs {
                let ib2 = self.get_block_ptr(dib, i as u32).unwrap_or(0);
                if ib2 != 0 {
                    for j in 0..nptrs {
                        let b = self.get_block_ptr(ib2, j as u32).unwrap_or(0);
                        if b != 0 { self.free_block(b); }
                    }
                    self.free_block(ib2);
                }
            }
            self.free_block(dib);
            Self::set_inode_block_ptr(&mut inode, EXT2_DIND_BLOCK, 0);
        }

        Self::set_inode_size(&mut inode, 0);
        Self::set_inode_blocks_512(&mut inode, 0);
        self.write_inode(ino, &inode);
    }

    // ── Init / mkfs ──────────────────────────────────────────────────────────

    pub fn new(dev: VirtioBlkProxy) -> Option<Self> {
        let sb = RawSuperblock::read(&dev)?;
        let block_size_log = sb.u32(SB_LOG_BLOCK_SIZE);
        let block_size = (1024usize) << block_size_log;
        if block_size != BLOCK_SIZE { return None; }

        // Reject filesystems with unsupported features (e.g. ext3 journal, extents)
        let incompat = sb.u32(SB_FEATURE_INCOMPAT);
        let supported = 0x0002; // Only DIRENTRY_FILETYPE is supported
        if incompat & !supported != 0 { return None; }

        let ro_compat = sb.u32(SB_FEATURE_RO_COMPAT);
        if ro_compat != 0 { return None; } // No read-only compat features supported

        let blocks_per_group = sb.u32(SB_BLOCKS_PER_GROUP);
        let inodes_per_group = sb.u32(SB_INODES_PER_GROUP);
        let rev_level = sb.u32(SB_REV_LEVEL);
        let inode_size = if rev_level == 0 { 128usize } else {
            let s = sb.u16(SB_INODE_SIZE) as usize;
            if s < 128 { 128 } else { s }
        };
        let first_data_block = sb.u32(SB_FIRST_DATA_BLOCK);
        let total_blocks = sb.u32(SB_BLOCKS_COUNT);
        let num_groups = (total_blocks + blocks_per_group - 1) / blocks_per_group;

        // Read block group descriptor table
        let bgdt_start = if first_data_block == 0 { 1 } else { first_data_block + 1 };
        let bgdt_size = num_groups as usize * 32;
        let bgdt_blocks = ((bgdt_size + BLOCK_SIZE - 1) / BLOCK_SIZE) as u32;
        let mut bgdt_buf = alloc::vec![0u8; bgdt_blocks as usize * BLOCK_SIZE];
        {
            let mut tmp = [0u8; BLOCK_SIZE];
            for g in 0..bgdt_blocks {
                let off = g as usize * BLOCK_SIZE;
                if !dev.read_block(bgdt_start + g, &mut tmp) { return None; }
                bgdt_buf[off..off + BLOCK_SIZE].copy_from_slice(&tmp);
            }
        }

        let mut bgdt = Vec::new();
        for i in 0..num_groups as usize {
            let mut bg = RawBgDescriptor { data: [0u8; 32] };
            bg.data.copy_from_slice(&bgdt_buf[i * 32..(i + 1) * 32]);
            bgdt.push(bg);
        }

        Some(Self {
            dev,
            sb,
            bgdt,
            inode_size,
            block_size,
            inodes_per_group,
            blocks_per_group,
            first_data_block,
            num_groups,
            bgdt_blocks,
        })
    }

    pub fn mkfs(&mut self, total_blocks: u32) {
        let block_size_log = 2u32; // 4096 bytes
        let blocks_per_group = 8192u32;
        let inodes_per_group = 2048u32;
        let inode_size = 128u16;
        let first_data_block = 0u32; // 4096-byte blocks: superblock at offset 1024 within block 0

        let num_groups = (total_blocks + blocks_per_group - 1) / blocks_per_group;

        // Build superblock
        let mut sb = RawSuperblock { data: [0u8; 1024] };
        sb.set_u32(SB_INODES_COUNT, num_groups * inodes_per_group);
        sb.set_u32(SB_BLOCKS_COUNT, total_blocks);
        sb.set_u32(SB_R_BLOCKS_COUNT, 0);
        sb.set_u32(SB_FREE_BLOCKS, total_blocks);
        sb.set_u32(SB_FREE_INODES, num_groups * inodes_per_group);
        sb.set_u32(SB_FIRST_DATA_BLOCK, first_data_block);
        sb.set_u32(SB_LOG_BLOCK_SIZE, block_size_log);
        sb.set_u32(SB_LOG_FRAG_SIZE, block_size_log);
        sb.set_u32(SB_BLOCKS_PER_GROUP, blocks_per_group);
        sb.set_u32(SB_FRAGS_PER_GROUP, blocks_per_group);
        sb.set_u32(SB_INODES_PER_GROUP, inodes_per_group);
        sb.set_u16(SB_MAGIC, EXT2_MAGIC);
        sb.set_u16(SB_MAGIC + 2, 1); // state: clean
        sb.set_u16(SB_MAGIC + 4, 1); // errors: continue
        sb.set_u32(SB_REV_LEVEL, 1); // dynamic inode size
        sb.set_u16(SB_INODE_SIZE, inode_size);
        sb.set_u32(SB_FIRST_INO, 11); // first non-reserved inode

        // Set INCOMPAT_FILETYPE so mke2fs creates file_type in dirents
        sb.set_u32(SB_FEATURE_INCOMPAT, EXT2_FEATURE_INCOMPAT_FILETYPE);
        sb.set_u32(SB_FEATURE_RO_COMPAT, 0);
        sb.set_u32(SB_FEATURE_COMPAT, 0);

        self.sb = sb;

        // Write superblock to block 0 at offset 1024
        let mut blk0 = [0u8; BLOCK_SIZE];
        self.dev.read_block(0, &mut blk0);
        self.sb.store(&self.dev);

        // Build block group descriptors
        let bgdt_size = num_groups as usize * 32;
        let bgdt_blocks = ((bgdt_size + BLOCK_SIZE - 1) / BLOCK_SIZE) as u32;
        let bgdt_start = if first_data_block == 0 { 1 } else { first_data_block + 1 };

        let mut bgdt = Vec::new();
        let mut next_block = bgdt_start + bgdt_blocks;

        for g in 0..num_groups {
            let mut bg = RawBgDescriptor { data: [0u8; 32] };
            let block_bitmap = next_block; next_block += 1;
            let inode_bitmap = next_block; next_block += 1;
            let inode_table = next_block;
            let inodes_per_group_actual = inodes_per_group;
            let inode_table_blocks = (inodes_per_group_actual as usize * inode_size as usize + BLOCK_SIZE - 1) / BLOCK_SIZE;
            next_block += inode_table_blocks as u32;

            bg.set_bg_block_bitmap(block_bitmap);
            bg.set_bg_inode_bitmap(inode_bitmap);
            bg.set_bg_inode_table(inode_table);

            let blocks_in_group = if g == num_groups - 1 {
                let used = total_blocks % blocks_per_group;
                if used == 0 { blocks_per_group } else { used }
            } else {
                blocks_per_group
            };

            let data_blocks = (blocks_in_group as u32)
                .saturating_sub(next_block - (g * blocks_per_group));
            bg.set_bg_free_blocks_count(data_blocks as u16);
            bg.set_bg_free_inodes_count(inodes_per_group_actual as u16);
            bg.set_bg_used_dirs_count(0);

            // Zero out bitmaps and inode table blocks
            let zeros = [0u8; BLOCK_SIZE];
            for b in block_bitmap..next_block {
                self.dev.write_block(b, &zeros);
            }

            bgdt.push(bg);
        }

        self.bgdt = bgdt;
        self.num_groups = num_groups;
        self.blocks_per_group = blocks_per_group;
        self.inodes_per_group = inodes_per_group;
        self.first_data_block = first_data_block;
        self.inode_size = inode_size as usize;
        self.bgdt_blocks = bgdt_blocks;

        // Write BGDT
        self.update_bgdt();

        // Mark superblock and BGDT blocks as used in block bitmap of group 0
        // Superblock is in block 0 (if first_data_block == 0) or first_data_block
        // BGDT is in blocks bgdt_start..bgdt_start + bgdt_blocks
        // In group 0's bitmap:
        let bg0_bitmap_blk = self.bgdt[0].bg_block_bitmap();
        // Mark superblock block(s) as used
        // With 4096-byte blocks and first_data_block = 0, superblock is in block 0
        if first_data_block == 0 {
            self.bitmap_set(bg0_bitmap_blk, 0, true);
            // Also mark blocks up to bgdt_start + bgdt_blocks - 1
            for b in 1..(bgdt_start + bgdt_blocks) {
                self.bitmap_set(bg0_bitmap_blk, b as usize, true);
            }
        } else {
            for b in first_data_block..(bgdt_start + bgdt_blocks) {
                self.bitmap_set(bg0_bitmap_blk, b as usize, true);
            }
        }

        // Reserve inodes 1-10 (0-indexed bits 0-9)
        {
            let bg0_inode_bitmap = self.bgdt[0].bg_inode_bitmap();
            for i in 0..10 {
                self.bitmap_set(bg0_inode_bitmap, i, true);
            }
        }

        // Update free counts
        let overhead = (bgdt_start + bgdt_blocks) as u32;
        self.sb.set_u32(SB_FREE_BLOCKS, total_blocks - overhead);
        self.bgdt[0].set_bg_free_blocks_count(
            (blocks_per_group - overhead) as u16
        );
        let reserved_inodes = 10u32;
        self.sb.set_u32(SB_FREE_INODES, num_groups * inodes_per_group - reserved_inodes);
        self.bgdt[0].set_bg_free_inodes_count(inodes_per_group as u16 - reserved_inodes as u16);
        self.bgdt[0].set_bg_used_dirs_count(1);

        self.update_sb();
        self.update_bgdt();

        // Create root directory inode #2
        let mut root_inode = [0u8; 128];
        // mode: directory (0x4000) | 0755
        let mode: u16 = EXT2_S_IFDIR | 0o755;
        root_inode[INODE_MODE..INODE_MODE + 2].copy_from_slice(&mode.to_le_bytes());
        root_inode[INODE_UID..INODE_UID + 2].copy_from_slice(&0u16.to_le_bytes());
        root_inode[INODE_GID..INODE_GID + 2].copy_from_slice(&0u16.to_le_bytes());
        Self::set_inode_links(&mut root_inode, 2); // . and ..
        // Timestamps: 0 (epoch)
        root_inode[INODE_SIZE..INODE_SIZE + 4].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes()); // initial size: 1 block

        // Allocate one block for root directory entries
        let root_blk = self.alloc_block().expect("alloc root block");
        let mut root_data = [0u8; BLOCK_SIZE];

        // Create "." entry pointing to self
        root_data[0..4].copy_from_slice(&2u32.to_le_bytes()); // inode 2
        let rec_len_dot: u16 = 12; // 8 + 1 + 3 (aligned) = 12
        root_data[4..6].copy_from_slice(&rec_len_dot.to_le_bytes());
        root_data[6] = 1; // name_len = 1
        root_data[7] = EXT2_FT_DIR;
        root_data[8] = b'.';

        // Create ".." entry pointing to self (root has no parent)
        root_data[12..16].copy_from_slice(&2u32.to_le_bytes()); // inode 2
        let rec_len_dotdot = (BLOCK_SIZE - 12) as u16;
        root_data[16..18].copy_from_slice(&rec_len_dotdot.to_le_bytes());
        root_data[18] = 2; // name_len = 2
        root_data[19] = EXT2_FT_DIR;
        root_data[20] = b'.';
        root_data[21] = b'.';

        self.write_block(root_blk, &root_data);

        // Set root inode block pointer
        Self::set_inode_block_ptr(&mut root_inode, 0, root_blk);
        Self::set_inode_blocks_512(&mut root_inode, (BLOCK_SIZE / 512) as u32);

        self.write_inode(2, &root_inode);
    }
}

// ── Common inode info (for main.rs dispatch) ────────────────────────────────

use crate::blockio::InodeInfo;

impl Ext2State {
    pub fn inode_info(&mut self, ino: u32) -> Option<InodeInfo> {
        let raw = self.read_inode(ino)?;
        let mode = u16::from_le_bytes(raw[INODE_MODE..INODE_MODE + 2].try_into().unwrap_or([0; 2]));
        let itype = if mode & EXT2_S_IFDIR != 0 { 2 }
                    else if mode & EXT2_S_IFLNK != 0 { 3 }
                    else { 1 };
        let size = u32::from_le_bytes(raw[INODE_SIZE..INODE_SIZE + 4].try_into().unwrap_or([0; 4]));
        let nlink = u16::from_le_bytes(raw[INODE_LINKS_COUNT..INODE_LINKS_COUNT + 2].try_into().unwrap_or([0; 2])) as u32;
        Some(InodeInfo { itype, size, nlink })
    }

    pub fn create_inode(&mut self, itype: u8) -> Option<u32> {
        let ino = self.alloc_inode()?;
        let mut raw = [0u8; 128];
        let mode = match itype {
            2 => EXT2_S_IFDIR | 0o755,
            3 => EXT2_S_IFLNK | 0o777,
            _ => EXT2_S_IFREG | 0o644,
        };
        raw[INODE_MODE..INODE_MODE + 2].copy_from_slice(&mode.to_le_bytes());
        raw[INODE_SIZE..INODE_SIZE + 4].copy_from_slice(&0u32.to_le_bytes());
        raw[INODE_LINKS_COUNT..INODE_LINKS_COUNT + 2].copy_from_slice(&1u16.to_le_bytes());
        self.write_inode(ino, &raw);
        Some(ino)
    }
}

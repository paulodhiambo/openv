use crate::block::VirtioBlk;
use crate::vfs::{DirEntry, Stat, VnodeType};
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::sync::Mutex;

// ── On-disk constants ────────────────────────────────────────────────────────

const OFS_MAGIC: u32 = 0x4F564653; // "OVFS"
const OFS_VERSION: u32 = 1;

pub const BLOCK_SIZE: usize = 4096;
const MAX_INODES: usize = 256;
const INODE_SIZE: usize = 128;
const INODES_PER_BLOCK: usize = BLOCK_SIZE / INODE_SIZE; // 32
const DIRENTRY_SIZE: usize = 64;
const DIRENTRIES_PER_BLOCK: usize = BLOCK_SIZE / DIRENTRY_SIZE; // 64

const BLOCK_BITMAP_BLK: u32 = 1;
const INODE_BITMAP_BLK: u32 = 2;
const INODE_TABLE_START: u32 = 3;
const INODE_TABLE_BLOCKS: u32 = 8; // 8 × 32 = 256 inodes
const DATA_START: u32 = INODE_TABLE_START + INODE_TABLE_BLOCKS; // 11

#[allow(dead_code)]
const ITYPE_FREE: u8 = 0;
const ITYPE_FILE: u8 = 1;
const ITYPE_DIR: u8 = 2;

const ITYPE_FILE_ENTRY: u8 = 1;
const ITYPE_DIR_ENTRY: u8 = 2;

const ROOT_INODE: u32 = 0;

// ── Raw on-disk structures ────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInode {
    itype: u8,
    _pad: [u8; 3],
    size: u32,
    nlink: u32,
    used_blocks: u32,
    direct: [u32; 12],
    indirect: u32,
    _reserved: [u32; 15],
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
    dev: &'static VirtioBlk,
    total_blocks: u32,
}

impl OfsState {
    fn read_blk(&self, blk: u32, buf: &mut [u8; BLOCK_SIZE]) -> bool {
        self.dev.read_block(blk, buf)
    }

    fn write_blk(&self, blk: u32, buf: &[u8; BLOCK_SIZE]) -> bool {
        self.dev.write_block(blk, buf)
    }

    // ── Bitmap helpers ────────────────────────────────────────────────────────

    fn bitmap_set(&self, bitmap_blk: u32, bit: usize, value: bool) {
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(bitmap_blk, &mut buf) {
            return;
        }
        if value {
            buf[bit / 8] |= 1 << (bit % 8);
        } else {
            buf[bit / 8] &= !(1 << (bit % 8));
        }
        self.write_blk(bitmap_blk, &buf);
    }

    fn bitmap_alloc(&self, bitmap_blk: u32, start: usize, limit: usize) -> Option<usize> {
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(bitmap_blk, &mut buf) {
            return None;
        }
        for i in start..limit {
            if buf[i / 8] & (1 << (i % 8)) == 0 {
                buf[i / 8] |= 1 << (i % 8);
                self.write_blk(bitmap_blk, &buf);
                return Some(i);
            }
        }
        None
    }

    // ── Inode I/O ────────────────────────────────────────────────────────────

    fn read_inode(&self, ino: u32) -> Option<RawInode> {
        if ino as usize >= MAX_INODES {
            return None;
        }
        let table_blk = INODE_TABLE_START + ino / INODES_PER_BLOCK as u32;
        let offset = (ino as usize % INODES_PER_BLOCK) * INODE_SIZE;
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(table_blk, &mut buf) {
            return None;
        }
        let inode = unsafe { *(buf[offset..].as_ptr() as *const RawInode) };
        Some(inode)
    }

    fn write_inode(&self, ino: u32, inode: &RawInode) -> bool {
        if ino as usize >= MAX_INODES {
            return false;
        }
        let table_blk = INODE_TABLE_START + ino / INODES_PER_BLOCK as u32;
        let offset = (ino as usize % INODES_PER_BLOCK) * INODE_SIZE;
        let mut buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(table_blk, &mut buf) {
            return false;
        }
        unsafe {
            let dst = buf[offset..].as_mut_ptr() as *mut RawInode;
            *dst = *inode;
        }
        self.write_blk(table_blk, &buf)
    }

    // ── Block allocation ──────────────────────────────────────────────────────

    fn alloc_block(&self) -> Option<u32> {
        let limit = self.total_blocks as usize;
        self.bitmap_alloc(BLOCK_BITMAP_BLK, DATA_START as usize, limit)
            .map(|b| b as u32)
    }

    fn free_block(&self, blk: u32) {
        if blk >= DATA_START {
            self.bitmap_set(BLOCK_BITMAP_BLK, blk as usize, false);
        }
    }

    // ── Inode allocation ──────────────────────────────────────────────────────

    fn alloc_inode(&self) -> Option<u32> {
        // Inode 0 is always reserved for root; start allocation from 1.
        self.bitmap_alloc(INODE_BITMAP_BLK, 1, MAX_INODES)
            .map(|i| i as u32)
    }

    fn free_inode(&self, ino: u32) {
        self.bitmap_set(INODE_BITMAP_BLK, ino as usize, false);
    }

    // ── File block mapping ────────────────────────────────────────────────────

    /// Return the disk block number for logical block `idx` of inode `ino`.
    /// Allocates and writes back if `alloc` is true and the mapping is absent.
    fn get_file_block(&self, inode: &mut RawInode, ino: u32, idx: u32, alloc: bool) -> Option<u32> {
        if idx < 12 {
            let blk = inode.direct[idx as usize];
            if blk != 0 {
                return Some(blk);
            }
            if !alloc {
                return None;
            }
            let new_blk = self.alloc_block()?;
            // zero new block
            let zeros = [0u8; BLOCK_SIZE];
            self.write_blk(new_blk, &zeros);
            inode.direct[idx as usize] = new_blk;
            inode.used_blocks += 1;
            self.write_inode(ino, inode);
            return Some(new_blk);
        }

        // Single indirect
        let indirect_idx = idx - 12;
        if indirect_idx >= (BLOCK_SIZE / 4) as u32 {
            return None; // beyond single-indirect range
        }

        if inode.indirect == 0 {
            if !alloc {
                return None;
            }
            let ib = self.alloc_block()?;
            let zeros = [0u8; BLOCK_SIZE];
            self.write_blk(ib, &zeros);
            inode.indirect = ib;
            inode.used_blocks += 1;
            self.write_inode(ino, inode);
        }

        let mut ib_buf = [0u8; BLOCK_SIZE];
        if !self.read_blk(inode.indirect, &mut ib_buf) {
            return None;
        }
        let entry_offset = indirect_idx as usize * 4;
        let blk = u32::from_le_bytes(ib_buf[entry_offset..entry_offset + 4].try_into().ok()?);
        if blk != 0 {
            return Some(blk);
        }
        if !alloc {
            return None;
        }
        let new_blk = self.alloc_block()?;
        let zeros = [0u8; BLOCK_SIZE];
        self.write_blk(new_blk, &zeros);
        ib_buf[entry_offset..entry_offset + 4].copy_from_slice(&new_blk.to_le_bytes());
        self.write_blk(inode.indirect, &ib_buf);
        inode.used_blocks += 1;
        self.write_inode(ino, inode);
        Some(new_blk)
    }

    // ── Directory helpers ─────────────────────────────────────────────────────

    fn dir_lookup_inode(&self, dir_ino: u32, name: &str) -> Option<(u32, u8)> {
        let inode = self.read_inode(dir_ino)?;
        if inode.itype != ITYPE_DIR {
            return None;
        }
        let name_bytes = name.as_bytes();
        let total_entries = (inode.size as usize + DIRENTRY_SIZE - 1) / DIRENTRY_SIZE;
        let mut entry_idx = 0;
        for block_idx in 0..12 {
            if entry_idx >= total_entries {
                break;
            }
            let blk = inode.direct[block_idx];
            if blk == 0 {
                entry_idx += DIRENTRIES_PER_BLOCK;
                continue;
            }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut buf) {
                break;
            }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if entry_idx >= total_entries {
                    break;
                }
                let de = unsafe {
                    *(buf[slot * DIRENTRY_SIZE..].as_ptr() as *const RawDirEntry)
                };
                entry_idx += 1;
                if de.name_len == 0 {
                    continue;
                }
                let n = de.name_len as usize;
                if n == name_bytes.len() && &de.name[..n] == name_bytes {
                    return Some((de.inode_num, de.entry_type));
                }
            }
        }
        None
    }

    fn dir_add_entry(&self, dir_ino: u32, name: &str, child_ino: u32, etype: u8) -> bool {
        let mut inode = match self.read_inode(dir_ino) {
            Some(i) => i,
            None => return false,
        };
        if inode.itype != ITYPE_DIR {
            return false;
        }
        let name_bytes = name.as_bytes();
        if name_bytes.len() > 55 {
            return false;
        }

        let total_entries = (inode.size as usize + DIRENTRY_SIZE - 1) / DIRENTRY_SIZE;
        // Scan existing blocks for an empty slot
        for block_idx in 0..12usize {
            let blk = if block_idx * DIRENTRIES_PER_BLOCK < total_entries {
                inode.direct[block_idx]
            } else {
                0
            };
            if blk == 0 && block_idx * DIRENTRIES_PER_BLOCK >= total_entries {
                // Need a new block
                break;
            }
            if blk == 0 {
                continue;
            }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut buf) {
                continue;
            }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                let de = unsafe {
                    *(buf[slot * DIRENTRY_SIZE..].as_ptr() as *const RawDirEntry)
                };
                if de.name_len == 0 {
                    // Found empty slot
                    let mut new_de = RawDirEntry {
                        inode_num: child_ino,
                        name_len: name_bytes.len() as u8,
                        entry_type: etype,
                        _pad: [0; 2],
                        name: [0; 56],
                    };
                    new_de.name[..name_bytes.len()].copy_from_slice(name_bytes);
                    unsafe {
                        let dst = buf[slot * DIRENTRY_SIZE..].as_mut_ptr() as *mut RawDirEntry;
                        *dst = new_de;
                    }
                    self.write_blk(blk, &buf);
                    let slot_pos = (block_idx * DIRENTRIES_PER_BLOCK + slot + 1) * DIRENTRY_SIZE;
                    if slot_pos > inode.size as usize {
                        inode.size = slot_pos as u32;
                        self.write_inode(dir_ino, &inode);
                    }
                    return true;
                }
            }
        }

        // Append to end — get or allocate next block
        let next_block_idx = (inode.size as usize + BLOCK_SIZE - 1) / BLOCK_SIZE;
        if next_block_idx >= 12 {
            return false; // no indirect support for dirs
        }
        let blk = if inode.direct[next_block_idx] != 0 {
            inode.direct[next_block_idx]
        } else {
            match self.alloc_block() {
                Some(b) => {
                    inode.direct[next_block_idx] = b;
                    inode.used_blocks += 1;
                    b
                }
                None => return false,
            }
        };
        let zeros = [0u8; BLOCK_SIZE];
        let mut buf = zeros;
        // (block was zeroed on alloc)
        let mut new_de = RawDirEntry {
            inode_num: child_ino,
            name_len: name_bytes.len() as u8,
            entry_type: etype,
            _pad: [0; 2],
            name: [0; 56],
        };
        new_de.name[..name_bytes.len()].copy_from_slice(name_bytes);
        unsafe {
            let dst = buf[0..].as_mut_ptr() as *mut RawDirEntry;
            *dst = new_de;
        }
        self.write_blk(blk, &buf);
        inode.size = ((next_block_idx * DIRENTRIES_PER_BLOCK) + 1) as u32 * DIRENTRY_SIZE as u32;
        self.write_inode(dir_ino, &inode);
        true
    }

    fn dir_readall(&self, dir_ino: u32) -> Vec<(alloc::string::String, u32, u8)> {
        let mut result = Vec::new();
        let inode = match self.read_inode(dir_ino) {
            Some(i) => i,
            None => return result,
        };
        if inode.itype != ITYPE_DIR {
            return result;
        }
        let total_entries = (inode.size as usize + DIRENTRY_SIZE - 1) / DIRENTRY_SIZE;
        let mut seen = 0;
        for block_idx in 0..12 {
            if seen >= total_entries {
                break;
            }
            let blk = inode.direct[block_idx];
            if blk == 0 {
                break;
            }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut buf) {
                break;
            }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if seen >= total_entries {
                    break;
                }
                let de = unsafe {
                    *(buf[slot * DIRENTRY_SIZE..].as_ptr() as *const RawDirEntry)
                };
                seen += 1;
                if de.name_len == 0 {
                    continue;
                }
                let n = de.name_len as usize;
                if let Ok(s) = core::str::from_utf8(&de.name[..n]) {
                    result.push((
                        alloc::string::String::from(s),
                        de.inode_num,
                        de.entry_type,
                    ));
                }
            }
        }
        result
    }

    // ── File read/write ───────────────────────────────────────────────────────

    fn file_read(&self, ino: u32, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        let inode = self.read_inode(ino).ok_or("bad inode")?;
        if inode.itype != ITYPE_FILE {
            return Err("not a file");
        }
        if offset >= inode.size as usize {
            return Ok(0);
        }
        let available = inode.size as usize - offset;
        let to_read = core::cmp::min(available, buf.len());
        let mut done = 0usize;

        while done < to_read {
            let pos = offset + done;
            let block_idx = (pos / BLOCK_SIZE) as u32;
            let block_off = pos % BLOCK_SIZE;
            let chunk = core::cmp::min(to_read - done, BLOCK_SIZE - block_off);

            let mut inode_mut = inode;
            let blk = self
                .get_file_block(&mut inode_mut, ino, block_idx, false)
                .ok_or("missing file block")?;
            let mut blk_buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut blk_buf) {
                return Err("block read error");
            }
            buf[done..done + chunk].copy_from_slice(&blk_buf[block_off..block_off + chunk]);
            done += chunk;
        }
        Ok(to_read)
    }

    fn file_write(&self, ino: u32, offset: usize, data: &[u8]) -> Result<usize, &'static str> {
        let mut inode = self.read_inode(ino).ok_or("bad inode")?;
        if inode.itype != ITYPE_FILE {
            return Err("not a file");
        }
        let end = offset + data.len();
        let mut done = 0usize;

        while done < data.len() {
            let pos = offset + done;
            let block_idx = (pos / BLOCK_SIZE) as u32;
            let block_off = pos % BLOCK_SIZE;
            let chunk = core::cmp::min(data.len() - done, BLOCK_SIZE - block_off);

            let blk = self
                .get_file_block(&mut inode, ino, block_idx, true)
                .ok_or("OOM: no free block")?;

            let mut blk_buf = [0u8; BLOCK_SIZE];
            // Read-modify-write for partial blocks
            if block_off != 0 || chunk < BLOCK_SIZE {
                if !self.read_blk(blk, &mut blk_buf) {
                    return Err("block read error");
                }
            }
            blk_buf[block_off..block_off + chunk].copy_from_slice(&data[done..done + chunk]);
            if !self.write_blk(blk, &blk_buf) {
                return Err("block write error");
            }
            done += chunk;
        }

        if end > inode.size as usize {
            inode.size = end as u32;
            self.write_inode(ino, &inode);
        }
        Ok(data.len())
    }

    fn file_truncate(&self, ino: u32) {
        let mut inode = match self.read_inode(ino) {
            Some(i) => i,
            None => return,
        };
        // Free data blocks
        for blk in inode.direct.iter().filter(|&&b| b != 0) {
            self.free_block(*blk);
        }
        if inode.indirect != 0 {
            let mut ib_buf = [0u8; BLOCK_SIZE];
            if self.read_blk(inode.indirect, &mut ib_buf) {
                for i in 0..(BLOCK_SIZE / 4) {
                    let b = u32::from_le_bytes(ib_buf[i * 4..i * 4 + 4].try_into().unwrap_or([0; 4]));
                    if b != 0 {
                        self.free_block(b);
                    }
                }
            }
            self.free_block(inode.indirect);
        }
        inode.direct = [0u32; 12];
        inode.indirect = 0;
        inode.size = 0;
        inode.used_blocks = 0;
        self.write_inode(ino, &inode);
    }

    fn dir_unlink(&self, dir_ino: u32, name: &str) -> bool {
        let inode = match self.read_inode(dir_ino) {
            Some(i) => i,
            None => return false,
        };
        let name_bytes = name.as_bytes();
        let total_entries = (inode.size as usize + DIRENTRY_SIZE - 1) / DIRENTRY_SIZE;
        let mut seen = 0;

        for block_idx in 0..12usize {
            if seen >= total_entries {
                break;
            }
            let blk = inode.direct[block_idx];
            if blk == 0 {
                break;
            }
            let mut buf = [0u8; BLOCK_SIZE];
            if !self.read_blk(blk, &mut buf) {
                break;
            }
            for slot in 0..DIRENTRIES_PER_BLOCK {
                if seen >= total_entries {
                    break;
                }
                let de = unsafe {
                    *(buf[slot * DIRENTRY_SIZE..].as_ptr() as *const RawDirEntry)
                };
                seen += 1;
                if de.name_len == 0 {
                    continue;
                }
                let n = de.name_len as usize;
                if n == name_bytes.len() && &de.name[..n] == name_bytes {
                    // Free child's blocks and inode
                    let child_ino = de.inode_num;
                    if de.entry_type == ITYPE_FILE_ENTRY {
                        self.file_truncate(child_ino);
                    }
                    self.free_inode(child_ino);

                    // Zero out this dir entry
                    let zeros = [0u8; DIRENTRY_SIZE];
                    buf[slot * DIRENTRY_SIZE..slot * DIRENTRY_SIZE + DIRENTRY_SIZE]
                        .copy_from_slice(&zeros);
                    self.write_blk(blk, &buf);
                    return true;
                }
            }
        }
        false
    }
}

// ── mkfs / mount ─────────────────────────────────────────────────────────────

fn read_magic(dev: &'static VirtioBlk) -> u32 {
    let mut buf = [0u8; BLOCK_SIZE];
    if !dev.read_block(0, &mut buf) {
        return 0;
    }
    u32::from_le_bytes(buf[0..4].try_into().unwrap_or([0; 4]))
}

fn mkfs(dev: &'static VirtioBlk, total_blocks: u32) {
    crate::println!("blockfs: formatting new OFS filesystem ({} blocks)", total_blocks);
    let mut buf = [0u8; BLOCK_SIZE];

    // Block 0: superblock
    buf[0..4].copy_from_slice(&OFS_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&OFS_VERSION.to_le_bytes());
    buf[8..12].copy_from_slice(&total_blocks.to_le_bytes());
    dev.write_block(0, &buf);

    // Block 1: block bitmap — mark system blocks 0-10 as used
    buf.iter_mut().for_each(|b| *b = 0);
    for i in 0..DATA_START as usize {
        buf[i / 8] |= 1 << (i % 8);
    }
    dev.write_block(BLOCK_BITMAP_BLK, &buf);

    // Block 2: inode bitmap — all free
    buf.iter_mut().for_each(|b| *b = 0);
    dev.write_block(INODE_BITMAP_BLK, &buf);

    // Blocks 3-10: inode table — all zeroed
    for b in INODE_TABLE_START..INODE_TABLE_START + INODE_TABLE_BLOCKS {
        buf.iter_mut().for_each(|x| *x = 0);
        dev.write_block(b, &buf);
    }

    // Create root inode (inode 0)
    let state = OfsState { dev, total_blocks };

    // Alloc inode 0 in bitmap
    state.bitmap_set(INODE_BITMAP_BLK, 0, true);

    let root_inode = RawInode {
        itype: ITYPE_DIR,
        _pad: [0; 3],
        size: 0,
        nlink: 1,
        used_blocks: 0,
        direct: [0u32; 12],
        indirect: 0,
        _reserved: [0u32; 15],
    };
    state.write_inode(ROOT_INODE, &root_inode);
    crate::println!("blockfs: mkfs complete, root inode created");
}

/// Try to mount OFS. Returns None if no block device is available.
/// If disk is blank, formats it first.
pub fn try_mount() -> Option<Arc<dyn crate::vfs::Vnode>> {
    let dev = crate::block::virtio_blk::get()?;

    // Detect disk size (assume 8 MB = 2048 blocks for now)
    let total_blocks: u32 = 2048;

    if read_magic(dev) != OFS_MAGIC {
        mkfs(dev, total_blocks);
    } else {
        crate::println!("blockfs: found existing OFS filesystem");
    }

    let state = Arc::new(Mutex::new(OfsState { dev, total_blocks }));
    Some(Arc::new(OfsDir {
        state,
        ino: ROOT_INODE,
    }))
}

// ── Vnode implementations ─────────────────────────────────────────────────────

pub struct OfsDir {
    state: Arc<Mutex<OfsState>>,
    ino: u32,
}

pub struct OfsFile {
    state: Arc<Mutex<OfsState>>,
    ino: u32,
}

impl crate::vfs::Vnode for OfsDir {
    fn stat(&self) -> Stat {
        Stat {
            mode: 0o755,
            uid: 0,
            gid: 0,
            size: 0,
            vtype: VnodeType::Directory,
        }
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn crate::vfs::Vnode>, &'static str> {
        let st = self.state.lock();
        let (child_ino, etype) = st.dir_lookup_inode(self.ino, name).ok_or("not found")?;
        let vnode: Arc<dyn crate::vfs::Vnode> = if etype == ITYPE_DIR_ENTRY {
            Arc::new(OfsDir {
                state: self.state.clone(),
                ino: child_ino,
            })
        } else {
            Arc::new(OfsFile {
                state: self.state.clone(),
                ino: child_ino,
            })
        };
        Ok(vnode)
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, &'static str> {
        let st = self.state.lock();
        let entries = st.dir_readall(self.ino);
        Ok(entries
            .into_iter()
            .map(|(name, _ino, etype)| DirEntry {
                name,
                vtype: if etype == ITYPE_DIR_ENTRY {
                    VnodeType::Directory
                } else {
                    VnodeType::File
                },
            })
            .collect())
    }

    fn create(&self, name: &str) -> Result<Arc<dyn crate::vfs::Vnode>, &'static str> {
        let st = self.state.lock();
        // If already exists, truncate it
        if let Some((existing_ino, etype)) = st.dir_lookup_inode(self.ino, name) {
            if etype == ITYPE_FILE_ENTRY {
                st.file_truncate(existing_ino);
                return Ok(Arc::new(OfsFile {
                    state: self.state.clone(),
                    ino: existing_ino,
                }));
            }
            return Err("is a directory");
        }
        let child_ino = st.alloc_inode().ok_or("no free inodes")?;
        let new_inode = RawInode {
            itype: ITYPE_FILE,
            _pad: [0; 3],
            size: 0,
            nlink: 1,
            used_blocks: 0,
            direct: [0u32; 12],
            indirect: 0,
            _reserved: [0u32; 15],
        };
        st.write_inode(child_ino, &new_inode);
        if !st.dir_add_entry(self.ino, name, child_ino, ITYPE_FILE_ENTRY) {
            st.free_inode(child_ino);
            return Err("dir add failed");
        }
        Ok(Arc::new(OfsFile {
            state: self.state.clone(),
            ino: child_ino,
        }))
    }

    fn mkdir(&self, name: &str) -> Result<Arc<dyn crate::vfs::Vnode>, &'static str> {
        let st = self.state.lock();
        if st.dir_lookup_inode(self.ino, name).is_some() {
            return Err("already exists");
        }
        let child_ino = st.alloc_inode().ok_or("no free inodes")?;
        let new_inode = RawInode {
            itype: ITYPE_DIR,
            _pad: [0; 3],
            size: 0,
            nlink: 1,
            used_blocks: 0,
            direct: [0u32; 12],
            indirect: 0,
            _reserved: [0u32; 15],
        };
        st.write_inode(child_ino, &new_inode);
        if !st.dir_add_entry(self.ino, name, child_ino, ITYPE_DIR_ENTRY) {
            st.free_inode(child_ino);
            return Err("dir add failed");
        }
        Ok(Arc::new(OfsDir {
            state: self.state.clone(),
            ino: child_ino,
        }))
    }

    fn unlink(&self, name: &str) -> Result<(), &'static str> {
        let st = self.state.lock();
        if st.dir_unlink(self.ino, name) {
            Ok(())
        } else {
            Err("not found")
        }
    }
}

impl crate::vfs::Vnode for OfsFile {
    fn stat(&self) -> Stat {
        let st = self.state.lock();
        let size = st
            .read_inode(self.ino)
            .map(|i| i.size as usize)
            .unwrap_or(0);
        Stat {
            mode: 0o644,
            uid: 0,
            gid: 0,
            size,
            vtype: VnodeType::File,
        }
    }

    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        let st = self.state.lock();
        st.file_read(self.ino, offset, buf)
    }

    fn write_at(&self, offset: usize, buf: &[u8]) -> Result<usize, &'static str> {
        let st = self.state.lock();
        st.file_write(self.ino, offset, buf)
    }

    fn truncate(&self, size: usize) -> Result<(), &'static str> {
        if size == 0 {
            let st = self.state.lock();
            st.file_truncate(self.ino);
            Ok(())
        } else {
            Err("partial truncate not supported")
        }
    }
}

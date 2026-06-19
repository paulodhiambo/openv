#![no_std]
#![no_main]

extern crate alloc;
extern crate libos;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use vfs_proto::*;

mod blockio;
mod blockfs;
mod ext2;

const ITYPE_FILE_ENTRY: u8 = 1;
const ITYPE_DIR_ENTRY: u8 = 2;
const ITYPE_SYMLINK_ENTRY: u8 = 3;

// ── Block filesystem dispatch ────────────────────────────────────────────────

#[allow(clippy::large_enum_variant)]
enum BlockFs {
    Ofs(blockfs::OfsState),
    Ext2(ext2::Ext2State),
}

impl BlockFs {
    fn dir_lookup_inode_dispatch(&mut self, dir_ino: u32, name: &str) -> Option<(u32, u8)> {
        match self {
            BlockFs::Ofs(s) => s.dir_lookup_inode(dir_ino, name),
            BlockFs::Ext2(s) => s.dir_lookup_inode(dir_ino, name),
        }
    }

    fn root_inode_number(&self) -> u32 {
        match self {
            BlockFs::Ofs(_) => blockfs::ROOT_INODE,
            BlockFs::Ext2(_) => 2,
        }
    }

    fn lookup_path(&mut self, path: &str) -> Option<(u32, u8)> {
        self.lookup_path_depth(path, 8, true)
    }

    #[allow(dead_code)]
    fn lookup_path_nofollow(&mut self, path: &str) -> Option<(u32, u8)> {
        self.lookup_path_depth(path, 8, false)
    }

    fn lookup_path_depth(&mut self, path: &str, depth: u32, follow_last: bool) -> Option<(u32, u8)> {
        let root_ino = self.root_inode_number();
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Some((root_ino, ITYPE_DIR_ENTRY));
        }
        let components: Vec<&str> = trimmed.split('/').filter(|c| !c.is_empty()).collect();
        let n = components.len();
        let mut cur_ino = root_ino;
        let mut cur_type = ITYPE_DIR_ENTRY;

        for (i, comp) in components.iter().enumerate() {
            if cur_type != ITYPE_DIR_ENTRY {
                return None;
            }
            let (next_ino, next_type) = self.dir_lookup_inode_dispatch(cur_ino, comp)?;
            let is_last = i == n - 1;

            if next_type == ITYPE_SYMLINK_ENTRY && (follow_last || !is_last) {
                if depth == 0 {
                    return None;
                }
                let info = self.inode_info(next_ino)?;
                let mut target_buf = alloc::vec![0u8; info.size as usize];
                if self.file_read(next_ino, 0, &mut target_buf).is_err() {
                    return None;
                }
                let target = core::str::from_utf8(&target_buf).ok()?;
                let mut new_path = alloc::string::String::new();
                if target.starts_with('/') {
                    new_path.push_str(target);
                } else {
                    for j in 0..i {
                        new_path.push('/');
                        new_path.push_str(components[j]);
                    }
                    new_path.push('/');
                    new_path.push_str(target);
                }
                for j in (i + 1)..n {
                    if !new_path.ends_with('/') {
                        new_path.push('/');
                    }
                    new_path.push_str(components[j]);
                }
                return self.lookup_path_depth(&new_path, depth - 1, follow_last);
            }

            cur_ino = next_ino;
            cur_type = next_type;
        }
        Some((cur_ino, cur_type))
    }
    fn inode_info(&mut self, ino: u32) -> Option<blockio::InodeInfo> {
        match self {
            BlockFs::Ofs(s) => s.inode_info(ino),
            BlockFs::Ext2(s) => s.inode_info(ino),
        }
    }
    fn create_inode(&mut self, itype: u8) -> Option<u32> {
        match self {
            BlockFs::Ofs(s) => s.create_inode(itype),
            BlockFs::Ext2(s) => s.create_inode(itype),
        }
    }
    fn free_inode(&mut self, ino: u32) {
        match self {
            BlockFs::Ofs(s) => s.free_inode(ino),
            BlockFs::Ext2(s) => s.free_inode(ino),
        }
    }
    fn dir_add_entry(&mut self, dir_ino: u32, name: &str, child_ino: u32, etype: u8) -> bool {
        match self {
            BlockFs::Ofs(s) => s.dir_add_entry(dir_ino, name, child_ino, etype),
            BlockFs::Ext2(s) => s.dir_add_entry(dir_ino, name, child_ino, etype),
        }
    }
    fn dir_unlink(&mut self, dir_ino: u32, name: &str) -> bool {
        match self {
            BlockFs::Ofs(s) => s.dir_unlink(dir_ino, name),
            BlockFs::Ext2(s) => s.dir_unlink(dir_ino, name),
        }
    }
    fn file_read(&mut self, ino: u32, offset: usize, buf: &mut [u8]) -> Result<usize, blockio::FsError> {
        match self {
            BlockFs::Ofs(s) => s.file_read(ino, offset, buf),
            BlockFs::Ext2(s) => s.file_read(ino, offset, buf),
        }
    }
    fn file_write(&mut self, ino: u32, offset: usize, data: &[u8]) -> Result<usize, crate::blockio::FsError> {
        match self {
            BlockFs::Ofs(s) => s.file_write(ino, offset, data),
            BlockFs::Ext2(s) => s.file_write(ino, offset, data),
        }
    }
    fn file_truncate(&mut self, ino: u32) {
        match self {
            BlockFs::Ofs(s) => s.file_truncate(ino),
            BlockFs::Ext2(s) => s.file_truncate(ino),
        }
    }
    fn dir_readall(&mut self, dir_ino: u32) -> alloc::vec::Vec<(alloc::string::String, u32, u8)> {
        match self {
            BlockFs::Ofs(s) => s.dir_readall(dir_ino),
            BlockFs::Ext2(s) => s.dir_readall(dir_ino),
        }
    }
    fn dir_link_entry(&mut self, dir_ino: u32, name: &str, child_ino: u32, etype: u8) -> bool {
        match self {
            BlockFs::Ofs(s) => s.dir_link_entry(dir_ino, name, child_ino, etype),
            BlockFs::Ext2(s) => s.dir_link_entry(dir_ino, name, child_ino, etype),
        }
    }
    fn init_dir(&mut self, ino: u32, parent_ino: u32) -> bool {
        match self {
            BlockFs::Ofs(s) => s.init_dir(ino, parent_ino),
            BlockFs::Ext2(s) => s.init_dir(ino, parent_ino),
        }
    }
    fn fsync(&mut self) -> bool {
        match self {
            BlockFs::Ofs(s) => s.fsync(),
            BlockFs::Ext2(_) => true,
        }
    }
    fn rename(&mut self, old_dir: &str, old_name: &str, new_dir: &str, new_name: &str) -> bool {
        let (dir_ino, _) = match self.lookup_path(old_dir) { Some(r) => r, None => return false };
        let (new_dir_ino, _) = match self.lookup_path(new_dir) { Some(r) => r, None => return false };
        // Use dir_lookup_inode_dispatch so we get the entry itself, not a followed symlink
        let (child_ino, etype) = match self.dir_lookup_inode_dispatch(dir_ino, old_name) {
            Some(r) => r, None => return false
        };

        // Check if target already exists
        if let Some((target_ino, _)) = self.dir_lookup_inode_dispatch(new_dir_ino, new_name) {
            if target_ino == child_ino { return true; }
            if !self.dir_unlink(new_dir_ino, new_name) { return false; }
        }

        if !self.dir_link_entry(new_dir_ino, new_name, child_ino, etype) { return false; }
        if !self.dir_unlink(dir_ino, old_name) { return false; }
        true
    }
}

#[allow(static_mut_refs)]
fn get_blockfs() -> Option<&'static mut BlockFs> {
    static mut STATE: Option<BlockFs> = None;
    unsafe {
        if STATE.is_none() {
            let blk_pid = libos::get_blk_pid();
            if blk_pid > 0 {
                let proxy = blockio::VirtioBlkProxy::new(blk_pid);
                if let Some(ofs) = blockfs::OfsState::new(proxy) {
                    STATE = Some(BlockFs::Ofs(ofs));
                }
            }
            if STATE.is_none() {
                let blk_pid = libos::get_blk_pid();
                if blk_pid > 0 {
                    let proxy = blockio::VirtioBlkProxy::new(blk_pid);
                    if let Some(ext2) = ext2::Ext2State::new(proxy) {
                        STATE = Some(BlockFs::Ext2(ext2));
                    }
                }
            }
        }
        STATE.as_mut()
    }
}

enum FsNode {
    /// Read-only file backed by the initrd TAR; data is never copied into memory.
    /// `tar_offset` = byte offset of the first data byte inside the TAR image.
    TarFile { tar_offset: usize, size: usize },
    /// Writable in-memory file (user-created or written-to files).
    MemFile(Vec<u8>),
    /// Directory: ordered list of child names (no leading slash).
    Dir(Vec<String>),
    /// Symbolic link: target path string.
    Symlink(String),
}

fn normalize_path(path: &str) -> String {
    let is_absolute = path.starts_with('/');
    let mut components: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => continue,
            ".." => {
                if !components.is_empty() {
                    components.pop();
                }
            }
            _ => components.push(comp),
        }
    }
    let mut result = String::new();
    if is_absolute || components.is_empty() {
        result.push('/');
    }
    result.push_str(&components.join("/"));
    result
}

#[derive(Clone)]
struct NodeMeta {
    mode: u32,
    uid: u32,
    gid: u32,
    atime: u64,
    mtime: u64,
}

impl NodeMeta {
    fn default_dir() -> Self {
        Self { mode: 0o40755, uid: 0, gid: 0, atime: 0, mtime: 0 }
    }
    fn default_file() -> Self {
        Self { mode: 0o100644, uid: 0, gid: 0, atime: 0, mtime: 0 }
    }
    fn default_symlink() -> Self {
        Self { mode: 0o120777, uid: 0, gid: 0, atime: 0, mtime: 0 }
    }
}

struct Vfs {
    nodes: BTreeMap<String, FsNode>,
    meta: BTreeMap<String, NodeMeta>,
}

impl Vfs {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(String::from("/"), FsNode::Dir(Vec::new()));
        let mut meta = BTreeMap::new();
        meta.insert(String::from("/"), NodeMeta::default_dir());
        Self { nodes, meta }
    }

    fn exists(&self, path: &str) -> bool {
        if is_virtual_path(path) { return true; }
        if let Some(stripped) = path.strip_prefix("/mnt")
            && let Some(ofs) = get_blockfs()
        {
            let p = if stripped.is_empty() { "/" } else { stripped };
            return ofs.lookup_path(p).is_some();
        }
        let path = &normalize_path(path);
        self.nodes.contains_key(path)
    }

    fn parent_and_name(path: &str) -> Option<(String, String)> {
        let path = path.trim_end_matches('/');
        let slash = path.rfind('/')?;
        let parent = if slash == 0 { "/" } else { &path[..slash] };
        let name = &path[slash + 1..];
        if name.is_empty() { return None; }
        Some((String::from(parent), String::from(name)))
    }

    fn mkdir_all(&mut self, path: &str) {
        if path == "/" || self.exists(path) { return; }
        if let Some(slash) = path.rfind('/') && slash > 0 {
            self.mkdir_all(&path[..slash]);
        }
        if !self.exists(path) { self.mkdir(path); }
    }

    fn mkdir(&mut self, path: &str) -> bool {
        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                if let Some((parent, name)) = Self::parent_and_name(p)
                    && let Some((dir_ino, etype)) = ofs.lookup_path(&parent)
                    && etype == ITYPE_DIR_ENTRY
                {
                    let child_ino = match ofs.create_inode(ITYPE_DIR_ENTRY) {
                        Some(i) => i,
                        None => return false,
                    };
                    if !ofs.init_dir(child_ino, dir_ino) {
                        ofs.free_inode(child_ino);
                        return false;
                    }
                    if !ofs.dir_add_entry(dir_ino, &name, child_ino, ITYPE_DIR_ENTRY) {
                        ofs.file_truncate(child_ino);
                        ofs.free_inode(child_ino);
                        return false;
                    }
                    return true;
                }
            }
            return false;
        }
        let path = &normalize_path(path);
        if self.exists(path) { return false; }
        if let Some((parent, name)) = Self::parent_and_name(path)
            && let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent)
        {
            children.push(name);
            self.nodes.insert(String::from(path), FsNode::Dir(Vec::new()));
            self.meta.entry(String::from(path)).or_insert_with(NodeMeta::default_dir);
            return true;
        }
        false
    }

    fn insert_tar_file(&mut self, path: &str, tar_offset: usize, size: usize) {
        let path = &normalize_path(path);
        if let Some((parent, name)) = Self::parent_and_name(path) {
            let already_exists = self.nodes.contains_key(path);
            if let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent) {
                if !already_exists { children.push(name); }
                self.nodes.insert(String::from(path), FsNode::TarFile { tar_offset, size });
                self.meta.entry(String::from(path)).or_insert_with(NodeMeta::default_file);
            }
        }
    }

    fn create(&mut self, path: &str) -> bool {
        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                if let Some((parent, name)) = Self::parent_and_name(p)
                    && let Some((dir_ino, etype)) = ofs.lookup_path(&parent)
                    && etype == ITYPE_DIR_ENTRY
                {
                    if let Some((child_ino, ctype)) = ofs.lookup_path(p) {
                        if ctype == ITYPE_FILE_ENTRY {
                            ofs.file_truncate(child_ino);
                            return true;
                        }
                        return false;
                    }

                    let child_ino = match ofs.create_inode(ITYPE_FILE_ENTRY) {
                        Some(i) => i,
                        None => return false,
                    };
                    if !ofs.dir_add_entry(dir_ino, &name, child_ino, ITYPE_FILE_ENTRY) {
                        ofs.free_inode(child_ino);
                        return false;
                    }
                    return true;
                }
            }
            return false;
        }
        let path = &normalize_path(path);
        if let Some((parent, name)) = Self::parent_and_name(path) {
            let already_exists = self.nodes.contains_key(path);
            if let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent) {
                if !already_exists { children.push(name); }
                self.nodes.insert(String::from(path), FsNode::MemFile(Vec::new()));
                self.meta.entry(String::from(path)).or_insert_with(NodeMeta::default_file);
                return true;
            }
        }
        false
    }

    fn unlink(&mut self, path: &str) -> bool {
        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                if let Some((parent, name)) = Self::parent_and_name(p)
                    && let Some((dir_ino, etype)) = ofs.lookup_path(&parent)
                    && etype == ITYPE_DIR_ENTRY
                {
                    return ofs.dir_unlink(dir_ino, &name);
                }
            }
            return false;
        }
        let path = &normalize_path(path);
        if !self.nodes.contains_key(path) { return false; }
        if let Some((parent, name)) = Self::parent_and_name(path)
            && let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent)
        {
            children.retain(|c| c != &name);
            self.nodes.remove(path);
            self.meta.remove(path);
            return true;
        }
        false
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Option<usize> {
        if path == "/dev/null" { return Some(0); }
        if path == "/dev/zero" {
            let n = buf.len();
            buf.fill(0);
            return Some(n);
        }
        if path == "/dev/tty" { return Some(0); }

        if let Some(rest) = path.strip_prefix("/proc/") {
            if let Some((pid_str, "status")) = rest.split_once('/')
                && let Ok(pid) = pid_str.parse::<i32>()
            {
                return virtual_proc_read_status(pid, offset, buf);
            }
            return None;
        }

        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                if let Some((ino, etype)) = ofs.lookup_path(p)
                    && etype == ITYPE_FILE_ENTRY
                {
                    match ofs.file_read(ino, offset as usize, buf) {
                        Ok(n) => return Some(n),
                        Err(_) => return None,
                    }
                }
            }
            return None;
        }
        let path = &normalize_path(path);
        match self.nodes.get(path)? {
            FsNode::TarFile { tar_offset, size } => {
                let file_off = offset as usize;
                if file_off >= *size { return Some(0); }
                let avail = size - file_off;
                let take = buf.len().min(avail);
                let n = initrd_read(&mut buf[..take], tar_offset + file_off);
                Some(n)
            }
            FsNode::MemFile(data) => {
                let start = (offset as usize).min(data.len());
                let n = (data.len() - start).min(buf.len());
                buf[..n].copy_from_slice(&data[start..start + n]);
                Some(n)
            }
            FsNode::Dir(_) => None,
            FsNode::Symlink(_) => None,
        }
    }

    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Option<usize> {
        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                if let Some((ino, etype)) = ofs.lookup_path(p)
                    && etype == ITYPE_FILE_ENTRY
                {
                    match ofs.file_write(ino, offset as usize, data) {
                        Ok(n) => return Some(n),
                        Err(_) => return None,
                    }
                }
            }
            return None;
        }
        let path = &normalize_path(path);
        match self.nodes.get_mut(path)? {
            FsNode::MemFile(buf) => {
                let start = offset as usize;
                let end = start + data.len();
                if buf.len() < end { buf.resize(end, 0); }
                buf[start..end].copy_from_slice(data);
                Some(data.len())
            }
            // TAR-backed files are read-only; promote to MemFile on first write.
            FsNode::TarFile { tar_offset, size } => {
                let (tar_offset, size) = (*tar_offset, *size);
                let mut mem: Vec<u8> = Vec::new();
                // Copy existing content into memory first.
                if size > 0 {
                    mem.resize(size, 0);
                    initrd_read(&mut mem, tar_offset);
                }
                let start = offset as usize;
                let end = start + data.len();
                if mem.len() < end { mem.resize(end, 0); }
                mem[start..end].copy_from_slice(data);
                let n = data.len();
                *self.nodes.get_mut(path).unwrap() = FsNode::MemFile(mem);
                Some(n)
            }
            FsNode::Dir(_) => None,
            FsNode::Symlink(_) => None,
        }
    }

    fn readdir(&self, path: &str) -> Option<Vec<String>> {
        if path == "/dev" {
            return Some(alloc::vec![
                String::from("null"),
                String::from("zero"),
                String::from("tty"),
            ]);
        }
        if path == "/proc" {
            return Some(virtual_proc_list());
        }
        if let Some(pid_str) = path.strip_prefix("/proc/")
            && pid_str.parse::<i32>().is_ok()
        {
            return Some(alloc::vec![String::from("status")]);
        }

        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                if let Some((dir_ino, etype)) = ofs.lookup_path(p)
                    && etype == ITYPE_DIR_ENTRY
                {
                    let entries = ofs.dir_readall(dir_ino);
                    let mut children = Vec::new();
                    for (name, _, _) in entries {
                        children.push(name);
                    }
                    return Some(children);
                }
            }
            return None;
        }
        let path = &normalize_path(path);
        match self.nodes.get(path)? {
            FsNode::Dir(children) => Some(children.clone()),
            _ => None,
        }
    }

    fn stat(&self, path: &str) -> Option<(bool, u64)> {
        // Virtual paths
        if path == "/dev" || path == "/proc" { return Some((true, 0)); }
        if path == "/dev/null" || path == "/dev/zero" || path == "/dev/tty" {
            return Some((false, 0));
        }
        if let Some(rest) = path.strip_prefix("/proc/") {
            let (pid_part, sub) = rest.split_once('/').unwrap_or((rest, ""));
            if pid_part.parse::<i32>().is_ok() {
                if sub.is_empty() { return Some((true, 0)); }
                if sub == "status" { return Some((false, 256)); }
            }
            return None;
        }

        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                if let Some((ino, etype)) = ofs.lookup_path(p) {
                    if etype == ITYPE_DIR_ENTRY {
                        return Some((true, 0));
                    } else if let Some(info) = ofs.inode_info(ino) {
                        return Some((false, info.size as u64));
                    }
                }
            }
            return None;
        }
        let path = &normalize_path(path);
        match self.nodes.get(path)? {
            FsNode::TarFile { size, .. } => Some((false, *size as u64)),
            FsNode::MemFile(data)        => Some((false, data.len() as u64)),
            FsNode::Dir(_)               => Some((true, 0)),
            FsNode::Symlink(target)       => Some((false, target.len() as u64)),
        }
    }

    fn chmod(&mut self, path: &str, mode: u32) -> bool {
        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                return ofs.lookup_path(p).is_some();
            }
            return false;
        }
        let path = &normalize_path(path);
        if !self.nodes.contains_key(path) { return false; }
        self.meta.entry(String::from(path)).or_insert_with(||
            if matches!(self.nodes.get(path), Some(FsNode::Dir(_))) { NodeMeta::default_dir() }
            else if matches!(self.nodes.get(path), Some(FsNode::Symlink(_))) { NodeMeta::default_symlink() }
            else { NodeMeta::default_file() }
        ).mode = mode;
        true
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> bool {
        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                return ofs.lookup_path(p).is_some();
            }
            return false;
        }
        let path = &normalize_path(path);
        if !self.nodes.contains_key(path) { return false; }
        let meta = self.meta.entry(String::from(path)).or_insert_with(||
            if matches!(self.nodes.get(path), Some(FsNode::Dir(_))) { NodeMeta::default_dir() }
            else if matches!(self.nodes.get(path), Some(FsNode::Symlink(_))) { NodeMeta::default_symlink() }
            else { NodeMeta::default_file() }
        );
        meta.uid = uid;
        meta.gid = gid;
        true
    }

    fn utimens(&mut self, path: &str, atime: u64, mtime: u64) -> bool {
        if let Some(stripped) = path.strip_prefix("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if stripped.is_empty() { "/" } else { stripped };
                return ofs.lookup_path(p).is_some();
            }
            return false;
        }
        let path = &normalize_path(path);
        if !self.nodes.contains_key(path) { return false; }
        let meta = self.meta.entry(String::from(path)).or_insert_with(||
            if matches!(self.nodes.get(path), Some(FsNode::Dir(_))) { NodeMeta::default_dir() }
            else if matches!(self.nodes.get(path), Some(FsNode::Symlink(_))) { NodeMeta::default_symlink() }
            else { NodeMeta::default_file() }
        );
        meta.atime = atime;
        meta.mtime = mtime;
        true
    }

    fn readlink(&self, path: &str) -> Option<&str> {
        let path = &normalize_path(path);
        if let Some(FsNode::Symlink(target)) = self.nodes.get(path) {
            Some(target.as_str())
        } else {
            None
        }
    }

    fn symlink(&mut self, link_path: &str, target: &str) -> bool {
        let link_path = &normalize_path(link_path);
        if self.exists(link_path) { return false; }
        if let Some((parent, name)) = Self::parent_and_name(link_path)
            && let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent)
        {
            children.push(name);
            self.nodes.insert(String::from(link_path), FsNode::Symlink(String::from(target)));
            self.meta.entry(String::from(link_path)).or_insert_with(NodeMeta::default_symlink);
            true
        } else {
            false
        }
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> bool {
        let old_path = &normalize_path(old_path);
        let new_path = &normalize_path(new_path);
        let node = match self.nodes.get(old_path) {
            Some(n) => n,
            None => return false,
        };
        // Don't link directories
        if matches!(node, FsNode::Dir(_)) { return false; }
        // Clone the node value
        let cloned = match node {
            FsNode::TarFile { tar_offset, size } => FsNode::TarFile { tar_offset: *tar_offset, size: *size },
            FsNode::MemFile(data) => FsNode::MemFile(data.clone()),
            FsNode::Symlink(t) => FsNode::Symlink(t.clone()),
            FsNode::Dir(_) => unreachable!(),
        };
        if let Some((parent, name)) = Self::parent_and_name(new_path) {
            if self.nodes.contains_key(new_path) { return false; }
            match self.nodes.get_mut(&parent) {
                Some(FsNode::Dir(children)) => children.push(name),
                _ => return false,
            }
            self.nodes.insert(String::from(new_path), cloned);
            true
        } else {
            false
        }
    }

    fn truncate(&mut self, path: &str, length: u64) -> bool {
        let path = &normalize_path(path);
        match self.nodes.get_mut(path) {
            Some(FsNode::MemFile(data)) => {
                data.resize(length as usize, 0);
                true
            }
            Some(FsNode::TarFile { .. }) => {
                // Promote TAR file to MemFile and truncate.
                let mut data = Vec::new();
                data.resize(length as usize, 0);
                *self.nodes.get_mut(path).unwrap() = FsNode::MemFile(data);
                true
            }
            _ => false,
        }
    }

}

// ── Virtual /proc and /dev helpers ───────────────────────────────────────────

/// Returns true for paths that are synthesised virtually (not stored in `nodes`).
fn is_virtual_path(path: &str) -> bool {
    matches!(path, "/dev" | "/dev/null" | "/dev/zero" | "/dev/tty" | "/proc")
        || path.starts_with("/proc/")
}

/// Ask the kernel for all running PIDs (syscall 110) and return them as strings.
fn virtual_proc_list() -> Vec<String> {
    let mut buf = [0u32; 512];
    let n = libos::syscall(110, buf.as_mut_ptr() as usize, 512, 0);
    if n == usize::MAX { return Vec::new(); }
    (0..n.min(512)).map(|i| alloc::format!("{}", buf[i])).collect()
}

/// Synthesise /proc/<pid>/status content by calling syscall 111.
fn virtual_proc_read_status(pid: i32, offset: u64, buf: &mut [u8]) -> Option<usize> {
    let mut tmp = [0u8; 512];
    let n = libos::syscall(111, pid as usize, tmp.as_mut_ptr() as usize, tmp.len());
    if n == usize::MAX { return None; }
    let start = (offset as usize).min(n);
    let take = (n - start).min(buf.len());
    buf[..take].copy_from_slice(&tmp[start..start + take]);
    Some(take)
}

// ── TAR scanner ───────────────────────────────────────────────────────────────
//
// Reads the initrd TAR header-by-header using syscall 65 without buffering any
// file data.  Only directory structure and per-file metadata are stored.

fn initrd_read(buf: &mut [u8], offset: usize) -> usize {
    libos::syscall(65, buf.as_mut_ptr() as usize, offset, buf.len())
}

fn octal_to_usize(bytes: &[u8]) -> usize {
    let mut v = 0usize;
    for &b in bytes {
        if b == 0 || b == b' ' { break; }
        if (b'0'..=b'7').contains(&b) { v = v * 8 + (b - b'0') as usize; }
    }
    v
}

/// Canonicalize a TAR entry name to an absolute VFS path, e.g.:
///   `./init`  → `/init`
///   `./proc/` → `/proc`
///   `./`      → `/`
fn tar_name_to_path(name: &str) -> String {
    let name = name.trim_end_matches('/');
    // Strip leading "./" or lone "."
    let name = name.strip_prefix("./").unwrap_or(name);
    let name = if name == "." { "" } else { name };
    if name.is_empty() {
        return String::from("/");
    }
    if name.starts_with('/') {
        return String::from(name);
    }
    let mut path = String::with_capacity(name.len() + 1);
    path.push('/');
    path.push_str(name);
    path
}

fn scan_initrd(vfs: &mut Vfs) {  // receives the root-namespace Vfs (ns_id = 1)
    let mut offset = 0usize;
    let mut header = [0u8; 512];
    let mut consecutive_empty = 0u32;

    loop {
        if initrd_read(&mut header, offset) < 512 { break; }

        // Two consecutive zero blocks = end of archive.
        if header.iter().all(|&b| b == 0) {
            consecutive_empty += 1;
            if consecutive_empty >= 2 { break; }
            offset += 512;
            continue;
        }
        consecutive_empty = 0;

        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = match core::str::from_utf8(&header[..name_end]) {
            Ok(s) => s,
            Err(_) => { offset += 512; continue; }
        };
        if name.is_empty() { offset += 512; continue; }
        // Skip macOS ._ resource-fork metadata files
        let base = name.trim_end_matches('/').rsplit('/').next().unwrap_or(name);
        if base.starts_with("._") {
            let size = octal_to_usize(&header[124..136]);
            let data_blocks = size.div_ceil(512);
            offset += 512 + data_blocks * 512;
            continue;
        }

        let type_flag = header[156];
        let size = octal_to_usize(&header[124..136]);

        let path = tar_name_to_path(name);
        if path == "/" { offset += 512; continue; } // skip root entry itself

        // /proc and /dev are kernel VFS mounts; skip them so libos falls back to
        // the kernel path and sees the live ProcFS / DevFS content.
        if path == "/proc" || path.starts_with("/proc/")
            || path == "/dev" || path.starts_with("/dev/")
        {
            let data_blocks = size.div_ceil(512);
            offset += 512 + data_blocks * 512;
            continue;
        }

        let data_start = offset + 512; // byte offset of first data byte in TAR

        let is_dir = type_flag == b'5'
            || (matches!(type_flag, 0 | b'0') && size == 0 && name.ends_with('/'));

        if is_dir {
            vfs.mkdir_all(&path);
        } else if matches!(type_flag, 0 | b'0') && !name.ends_with('/') {
            if let Some(slash) = path.rfind('/') {
                let parent = if slash == 0 { "/" } else { &path[..slash] };
                vfs.mkdir_all(parent);
            }
            vfs.insert_tar_file(&path, data_start, size);
        }

        let data_blocks = size.div_ceil(512);
        offset += 512 + data_blocks * 512;
    }
}

// ── Open-file table ───────────────────────────────────────────────────────────

struct OpenFile { path: String, offset: u64, ns_id: u32 }

struct OpenTable { files: BTreeMap<u32, OpenFile>, next_fd: u32 }

impl OpenTable {
    fn new() -> Self { Self { files: BTreeMap::new(), next_fd: 1 } }
    fn open(&mut self, path: &str, ns_id: u32) -> u32 {
        let fd = self.next_fd; self.next_fd += 1;
        self.files.insert(fd, OpenFile { path: String::from(path), offset: 0, ns_id });
        fd
    }
    fn get(&self, fd: u32) -> Option<&OpenFile> { self.files.get(&fd) }
    fn get_mut(&mut self, fd: u32) -> Option<&mut OpenFile> { self.files.get_mut(&fd) }
    fn close(&mut self, fd: u32) { self.files.remove(&fd); }
    fn dup(&mut self, fd: u32) -> Option<u32> {
        let file = self.files.get(&fd)?;
        let new_fd = self.next_fd;
        self.next_fd += 1;
        self.files.insert(new_fd, OpenFile { path: file.path.clone(), offset: file.offset, ns_id: file.ns_id });
        Some(new_fd)
    }
}

// ── IPC helpers ───────────────────────────────────────────────────────────────

fn vfs_register() { libos::syscall(63, 0, 0, 0); }

fn reply_ok(client: i32, mut msg: libos::ipc::Message) {
    msg.type_ = REPLY_OK;
    libos::msg_send(client, &msg);
}

fn reply_err(client: i32, mut msg: libos::ipc::Message) {
    msg.type_ = REPLY_ERR;
    libos::msg_send(client, &msg);
}

// ── Server PID helpers ────────────────────────────────────────────────────────

fn get_proc_server_pid() -> i32 {
    let r = libos::syscall(115, 0, 0, 0); // SYS_GET_PROC_SERVER_PID
    if r == usize::MAX { 0 } else { r as i32 }
}

fn get_dev_server_pid() -> i32 {
    let r = libos::syscall(117, 0, 0, 0); // SYS_GET_DEV_SERVER_PID
    if r == usize::MAX { 0 } else { r as i32 }
}

// ── proc-server forwarding ────────────────────────────────────────────────────

const OP_PROC_LIST: i32 = 1;
const OP_PROC_READ: i32 = 2;
const OP_PROC_STAT: i32 = 3;
const OP_DEV_LIST: i32 = 1;
const OP_DEV_READ: i32 = 2;
const OP_DEV_STAT: i32 = 4;
const PROC_REPLY_OK: i32 = 100;

/// Forward a /proc/* read to procfs-server. Returns Some(n) if handled.
fn forward_proc_read(proc_pid: i32, path: &str, offset: u64, client: i32, buf_ptr: usize, len: usize) -> Option<usize> {
    // Parse: /proc/<pid>/status
    let rest = path.strip_prefix("/proc/")?;
    let (pid_str, sub) = rest.split_once('/').unwrap_or((rest, ""));
    let pid: i32 = pid_str.parse().ok()?;
    if sub != "status" && !sub.is_empty() { return None; }

    let cap = len.min(512);
    let mut local = alloc::vec![0u8; cap];

    let mut req = libos::ipc::Message::new();
    req.type_ = OP_PROC_READ;
    req.data[0..4].copy_from_slice(&pid.to_le_bytes());
    req.data[4..8].copy_from_slice(&(offset as u32).to_le_bytes());
    req.data[8..12].copy_from_slice(&(cap as u32).to_le_bytes());
    let lp = local.as_mut_ptr() as u64;
    req.data[12..20].copy_from_slice(&lp.to_le_bytes());

    libos::msg_send(proc_pid, &req);
    let mut reply = libos::ipc::Message::new();
    libos::msg_receive(proc_pid, &mut reply);

    if reply.type_ != PROC_REPLY_OK { return None; }
    let n = u32::from_le_bytes(reply.data[0..4].try_into().unwrap_or([0; 4])) as usize;
    if n == 0 { return Some(0); }

    if libos::datacopy(libos::getpid(), local.as_ptr(), client, buf_ptr as *mut u8, n) < 0 {
        return None;
    }
    Some(n)
}

/// Forward a /proc getdents to procfs-server. Returns Some(n) if handled.
fn forward_proc_list(proc_pid: i32, client: i32, buf_ptr: usize, max_len: usize) -> Option<usize> {
    let cap = max_len.min(2048);
    let mut local = alloc::vec![0u8; cap];

    let mut req = libos::ipc::Message::new();
    req.type_ = OP_PROC_LIST;
    let lp = local.as_mut_ptr() as u64;
    req.data[0..8].copy_from_slice(&lp.to_le_bytes());
    req.data[8..12].copy_from_slice(&(cap as u32).to_le_bytes());

    libos::msg_send(proc_pid, &req);
    let mut reply = libos::ipc::Message::new();
    libos::msg_receive(proc_pid, &mut reply);

    if reply.type_ != PROC_REPLY_OK { return None; }
    let n = u32::from_le_bytes(reply.data[0..4].try_into().unwrap_or([0; 4])) as usize;
    if n == 0 { return Some(0); }

    if libos::datacopy(libos::getpid(), local.as_ptr(), client, buf_ptr as *mut u8, n) < 0 {
        return None;
    }
    Some(n)
}

/// Forward a /proc/* stat to procfs-server. Returns Some((is_dir, size)) if handled.
fn forward_proc_stat(proc_pid: i32, path: &str) -> Option<(bool, u64)> {
    let mut req = libos::ipc::Message::new();
    req.type_ = OP_PROC_STAT;

    if path == "/proc" {
        req.data[0..4].copy_from_slice(&0i32.to_le_bytes()); // pid=0
        req.data[4] = 1; // dir query
    } else {
        let rest = path.strip_prefix("/proc/")?;
        let (pid_str, sub) = rest.split_once('/').unwrap_or((rest, ""));
        let pid: i32 = pid_str.parse().ok()?;
        req.data[0..4].copy_from_slice(&pid.to_le_bytes());
        req.data[4] = if sub.is_empty() { 1 } else { 0 }; // dir vs file
    }

    libos::msg_send(proc_pid, &req);
    let mut reply = libos::ipc::Message::new();
    libos::msg_receive(proc_pid, &mut reply);

    if reply.type_ != PROC_REPLY_OK { return None; }
    let is_dir = reply.data[0] != 0;
    let size = u32::from_le_bytes(reply.data[4..8].try_into().unwrap_or([0; 4])) as u64;
    Some((is_dir, size))
}

// ── dev-server forwarding ─────────────────────────────────────────────────────

fn dev_name_to_id(name: &str) -> Option<u8> {
    match name { "null" => Some(0), "zero" => Some(1), "tty" => Some(2), _ => None }
}

/// Forward a /dev/* read to devfs-server. Returns Some(n) if handled.
fn forward_dev_read(dev_pid: i32, path: &str, offset: u64, client: i32, buf_ptr: usize, len: usize) -> Option<usize> {
    let name = path.strip_prefix("/dev/")?;
    let dev_id = dev_name_to_id(name)?;

    let cap = len.min(4096);
    let mut local = alloc::vec![0u8; cap];

    let mut req = libos::ipc::Message::new();
    req.type_ = OP_DEV_READ;
    req.data[0] = dev_id;
    req.data[4..8].copy_from_slice(&(offset as u32).to_le_bytes());
    req.data[8..12].copy_from_slice(&(cap as u32).to_le_bytes());
    let lp = local.as_mut_ptr() as u64;
    req.data[12..20].copy_from_slice(&lp.to_le_bytes());

    libos::msg_send(dev_pid, &req);
    let mut reply = libos::ipc::Message::new();
    libos::msg_receive(dev_pid, &mut reply);

    if reply.type_ != PROC_REPLY_OK { return None; }
    let n = u32::from_le_bytes(reply.data[0..4].try_into().unwrap_or([0; 4])) as usize;
    if n == 0 { return Some(0); }

    if libos::datacopy(libos::getpid(), local.as_ptr(), client, buf_ptr as *mut u8, n) < 0 {
        return None;
    }
    Some(n)
}

/// Forward a /dev/* stat to devfs-server. Returns Some((is_dir, size)) if handled.
fn forward_dev_stat(dev_pid: i32, path: &str) -> Option<(bool, u64)> {
    let mut req = libos::ipc::Message::new();
    req.type_ = OP_DEV_STAT;

    if path == "/dev" {
        req.data[0] = 0xFF; // dir query
    } else {
        let name = path.strip_prefix("/dev/")?;
        let dev_id = dev_name_to_id(name)?;
        req.data[0] = dev_id;
    }

    libos::msg_send(dev_pid, &req);
    let mut reply = libos::ipc::Message::new();
    libos::msg_receive(dev_pid, &mut reply);

    if reply.type_ != PROC_REPLY_OK { return None; }
    let is_dir = reply.data[0] != 0;
    let size = u32::from_le_bytes(reply.data[4..8].try_into().unwrap_or([0; 4])) as u64;
    Some((is_dir, size))
}

/// Forward a /dev getdents to devfs-server. Returns Some(n) if handled.
fn forward_dev_list(dev_pid: i32, client: i32, buf_ptr: usize, max_len: usize) -> Option<usize> {
    let cap = max_len.min(256);
    let mut local = alloc::vec![0u8; cap];

    let mut req = libos::ipc::Message::new();
    req.type_ = OP_DEV_LIST;
    let lp = local.as_mut_ptr() as u64;
    req.data[0..8].copy_from_slice(&lp.to_le_bytes());
    req.data[8..12].copy_from_slice(&(cap as u32).to_le_bytes());

    libos::msg_send(dev_pid, &req);
    let mut reply = libos::ipc::Message::new();
    libos::msg_receive(dev_pid, &mut reply);

    if reply.type_ != PROC_REPLY_OK { return None; }
    let n = u32::from_le_bytes(reply.data[0..4].try_into().unwrap_or([0; 4])) as usize;
    if n == 0 { return Some(0); }

    if libos::datacopy(libos::getpid(), local.as_ptr(), client, buf_ptr as *mut u8, n) < 0 {
        return None;
    }
    Some(n)
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

fn ns_vfs(namespaces: &mut BTreeMap<u32, Vfs>, ns_id: u32) -> &mut Vfs {
    namespaces.entry(ns_id).or_insert_with(Vfs::new)
}

fn dispatch(namespaces: &mut BTreeMap<u32, Vfs>, open_table: &mut OpenTable, mut msg: libos::ipc::Message) {
    let client = msg.source;
    let op = msg.type_;

    match op {
        OP_OPEN => {
            let (_flags, path_ptr, path_len, ns_id) = unpack_path_req(&msg.data);
            if path_len == 0 || path_len > 1024 { reply_err(client, msg); return; }
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };

            if ns_vfs(namespaces, ns_id).exists(path) {
                let fd = open_table.open(path, ns_id);
                pack_u32_reply(&mut msg.data, fd);
                reply_ok(client, msg);
            } else {
                reply_err(client, msg);
            }
        }
        OP_DUP => {
            let fd = unpack_u32_reply(&msg.data);
            if let Some(new_fd) = open_table.dup(fd) {
                pack_u32_reply(&mut msg.data, new_fd);
                reply_ok(client, msg);
            } else {
                reply_err(client, msg);
            }
        }
        OP_READ => {
            let (fd, offset, buf_ptr, len_req) = unpack_rw_req(&msg.data);
            let (path, read_off, file_ns) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), if offset == u64::MAX { f.offset } else { offset }, f.ns_id),
                None => { reply_err(client, msg); return; }
            };

            // Delegate /proc/* reads to procfs-server if registered.
            if path.starts_with("/proc/") {
                let proc_pid = get_proc_server_pid();
                if proc_pid > 0 {
                    match forward_proc_read(proc_pid, &path, read_off, client, buf_ptr, len_req) {
                        Some(n) => {
                            if let Some(f) = open_table.get_mut(fd) { f.offset = read_off + n as u64; }
                            pack_u32_reply(&mut msg.data, n as u32);
                            reply_ok(client, msg);
                        }
                        None => reply_err(client, msg),
                    }
                    return;
                }
            }

            // Delegate /dev/* reads to devfs-server if registered.
            if path.starts_with("/dev/") {
                let dev_pid = get_dev_server_pid();
                if dev_pid > 0 {
                    match forward_dev_read(dev_pid, &path, read_off, client, buf_ptr, len_req) {
                        Some(n) => {
                            if let Some(f) = open_table.get_mut(fd) { f.offset = read_off + n as u64; }
                            pack_u32_reply(&mut msg.data, n as u32);
                            reply_ok(client, msg);
                        }
                        None => reply_err(client, msg),
                    }
                    return;
                }
            }

            let mut data_buf = alloc::vec![0u8; len_req.min(1024 * 1024)]; // max 1MB per chunk
            let take = data_buf.len();
            match ns_vfs(namespaces, file_ns).read(&path, read_off, &mut data_buf[..take]) {
                Some(n) => {
                    if libos::datacopy(libos::getpid(), data_buf.as_ptr(), client, buf_ptr as *mut u8, n) < 0 {
                        reply_err(client, msg); return;
                    }
                    if let Some(f) = open_table.get_mut(fd) { f.offset = read_off + n as u64; }
                    pack_u32_reply(&mut msg.data, n as u32);
                    reply_ok(client, msg);
                }
                None => reply_err(client, msg),
            }
        }
        OP_WRITE => {
            let (fd, offset, buf_ptr, len_req) = unpack_rw_req(&msg.data);
            let (path, write_off, file_ns) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), if offset == u64::MAX { f.offset } else { offset }, f.ns_id),
                None => { reply_err(client, msg); return; }
            };

            let mut data_buf = alloc::vec![0u8; len_req.min(1024 * 1024)];
            let n = data_buf.len();
            if libos::datacopy(client, buf_ptr as *const u8, libos::getpid(), data_buf.as_mut_ptr(), n) < 0 {
                reply_err(client, msg); return;
            }

            match ns_vfs(namespaces, file_ns).write(&path, write_off, &data_buf) {
                Some(written) => {
                    if let Some(f) = open_table.get_mut(fd) { f.offset = write_off + written as u64; }
                    pack_u32_reply(&mut msg.data, written as u32);
                    reply_ok(client, msg);
                }
                None => reply_err(client, msg),
            }
        }
        OP_CLOSE => {
            let fd = unpack_close_req(&msg.data);
            open_table.close(fd);
            reply_ok(client, msg);
        }
        OP_GETDENTS => {
            let (path_ptr, path_len, buf_ptr, buf_len) = unpack_getdents_req(&msg.data);
            if path_len == 0 || path_len > 1024 { reply_err(client, msg); return; }
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };

            // Delegate /proc getdents to procfs-server if registered.
            if path == "/proc" || path.starts_with("/proc/") {
                let proc_pid = get_proc_server_pid();
                if proc_pid > 0 {
                    match forward_proc_list(proc_pid, client, buf_ptr, buf_len) {
                        Some(n) => { pack_u32_reply(&mut msg.data, n as u32); reply_ok(client, msg); }
                        None => reply_err(client, msg),
                    }
                    return;
                }
            }

            // Delegate /dev getdents to devfs-server if registered.
            if path == "/dev" {
                let dev_pid = get_dev_server_pid();
                if dev_pid > 0 {
                    match forward_dev_list(dev_pid, client, buf_ptr, buf_len) {
                        Some(n) => { pack_u32_reply(&mut msg.data, n as u32); reply_ok(client, msg); }
                        None => reply_err(client, msg),
                    }
                    return;
                }
            }

            match ns_vfs(namespaces, 1).readdir(path) {
                Some(children) => {
                    let mut data_buf = alloc::vec![0u8; buf_len];
                    let mut pos = 0usize;
                    for child in &children {
                        let b = child.as_bytes();
                        if pos + b.len() + 1 > data_buf.len() { break; }
                        data_buf[pos..pos + b.len()].copy_from_slice(b);
                        pos += b.len();
                        data_buf[pos] = 0; pos += 1;
                    }
                    if libos::datacopy(libos::getpid(), data_buf.as_ptr(), client, buf_ptr as *mut u8, pos) < 0 {
                        reply_err(client, msg); return;
                    }
                    pack_u32_reply(&mut msg.data, pos as u32);
                    reply_ok(client, msg);
                }
                None => reply_err(client, msg),
            }
        }
        OP_MKDIR => {
            let (_, path_ptr, path_len, ns_id) = unpack_path_req(&msg.data);
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            if ns_vfs(namespaces, ns_id).mkdir(path) { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_CREATE => {
            let (_, path_ptr, path_len, ns_id) = unpack_path_req(&msg.data);
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            if ns_vfs(namespaces, ns_id).create(path) {
                let fd = open_table.open(path, ns_id);
                pack_u32_reply(&mut msg.data, fd);
                reply_ok(client, msg);
            } else {
                reply_err(client, msg)
            }
        }
        OP_UNLINK => {
            let (_, path_ptr, path_len, ns_id) = unpack_path_req(&msg.data);
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            if ns_vfs(namespaces, ns_id).unlink(path) { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_STAT => {
            let (_, path_ptr, path_len, ns_id) = unpack_path_req(&msg.data);
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };

            // Delegate /proc stat to procfs-server if registered.
            if path == "/proc" || path.starts_with("/proc/") {
                let proc_pid = get_proc_server_pid();
                if proc_pid > 0 {
                    match forward_proc_stat(proc_pid, path) {
                        Some((is_dir, size)) => {
                            pack_stat_reply(&mut msg.data, is_dir, size);
                            reply_ok(client, msg);
                        }
                        None => reply_err(client, msg),
                    }
                    return;
                }
            }

            // Delegate /dev stat to devfs-server if registered.
            if path == "/dev" || path.starts_with("/dev/") {
                let dev_pid = get_dev_server_pid();
                if dev_pid > 0 {
                    match forward_dev_stat(dev_pid, path) {
                        Some((is_dir, size)) => {
                            pack_stat_reply(&mut msg.data, is_dir, size);
                            reply_ok(client, msg);
                        }
                        None => reply_err(client, msg),
                    }
                    return;
                }
            }

            match ns_vfs(namespaces, ns_id).stat(path) {
                Some((is_dir, size)) => {
                    pack_stat_reply(&mut msg.data, is_dir, size);
                    reply_ok(client, msg);
                }
                None => reply_err(client, msg),
            }
        }
        OP_RENAME => {
            let (old_ptr, old_len, new_ptr, new_len) = unpack_rename_req(&msg.data);
            let mut old_buf = alloc::vec![0u8; old_len];
            let mut new_buf = alloc::vec![0u8; new_len];
            if libos::datacopy(client, old_ptr as *const u8, libos::getpid(), old_buf.as_mut_ptr(), old_len) < 0 ||
               libos::datacopy(client, new_ptr as *const u8, libos::getpid(), new_buf.as_mut_ptr(), new_len) < 0 {
                reply_err(client, msg); return;
            }
            let old = match core::str::from_utf8(&old_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            let new = match core::str::from_utf8(&new_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };

            let ok = if old.starts_with("/mnt") || new.starts_with("/mnt") {
                let (old_p, new_p) = match (old.strip_prefix("/mnt"), new.strip_prefix("/mnt")) {
                    (Some(o), Some(n)) => {
                        let o = if o.is_empty() { "/" } else { o };
                        let n = if n.is_empty() { "/" } else { n };
                        (o, n)
                    }
                    _ => { reply_err(client, msg); return; } // cross-mount rename not supported
                };
                let ofs = match get_blockfs() { Some(o) => o, None => { reply_err(client, msg); return; } };
                let (old_parent, old_name) = match Vfs::parent_and_name(old_p) { Some(r) => r, None => { reply_err(client, msg); return; } };
                let (new_parent, new_name) = match Vfs::parent_and_name(new_p) { Some(r) => r, None => { reply_err(client, msg); return; } };
                ofs.rename(&old_parent, &old_name, &new_parent, &new_name)
            } else {
                let vfs = ns_vfs(namespaces, 1);
                if let Some(node) = vfs.nodes.remove(old) {
                    if let Some((op, on)) = Vfs::parent_and_name(old)
                        && let Some(FsNode::Dir(c)) = vfs.nodes.get_mut(&op)
                    {
                        c.retain(|x| x != &on);
                    }
                    if let Some((np, nn)) = Vfs::parent_and_name(new)
                        && let Some(FsNode::Dir(c)) = vfs.nodes.get_mut(&np)
                    {
                        c.push(nn);
                    }
                    vfs.nodes.insert(new.to_string(), node);
                    // Copy metadata if it exists
                    if let Some(meta) = vfs.meta.remove(old) {
                        vfs.meta.insert(new.to_string(), meta);
                    }
                    true
                } else {
                    false
                }
            };
            if ok { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_FSYNC => {
            let fd = unpack_close_req(&msg.data);
            let is_mnt = open_table.get(fd).map(|f| f.path.starts_with("/mnt")).unwrap_or(false);
            if is_mnt {
                let ok = get_blockfs().map(|ofs| ofs.fsync()).unwrap_or(false);
                if ok { reply_ok(client, msg) } else { reply_err(client, msg) }
            } else {
                reply_ok(client, msg) // non-OFS fds have nothing to flush
            }
        }
        OP_READLINK => {
            let (path_ptr, path_len, buf_ptr, buf_len) = unpack_getdents_req(&msg.data);
            if path_len == 0 || path_len > 1024 || buf_len == 0 { reply_err(client, msg); return; }
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };

            let target_str;
            let target_bytes: &[u8] = if let Some(stripped) = path.strip_prefix("/mnt") {
                let p = if stripped.is_empty() { "/" } else { stripped };
                let ofs = match get_blockfs() { Some(o) => o, None => { reply_err(client, msg); return; } };
                let (ino, etype) = match ofs.lookup_path(p) { Some(r) => r, None => { reply_err(client, msg); return; } };
                if etype != ITYPE_SYMLINK_ENTRY { reply_err(client, msg); return; }
                let info = match ofs.inode_info(ino) { Some(i) => i, None => { reply_err(client, msg); return; } };
                let mut content = alloc::vec![0u8; info.size as usize];
                if ofs.file_read(ino, 0, &mut content).is_err() { reply_err(client, msg); return; }
                target_str = content; // keep alive
                &target_str
            } else {
                match ns_vfs(namespaces, 1).readlink(path) {
                    Some(t) => t.as_bytes(),
                    None => { reply_err(client, msg); return; }
                }
            };

            let n = target_bytes.len().min(buf_len);
            let mut data_buf = alloc::vec![0u8; n];
            data_buf[..n].copy_from_slice(&target_bytes[..n]);
            if libos::datacopy(libos::getpid(), data_buf.as_ptr(), client, buf_ptr as *mut u8, n) < 0 {
                reply_err(client, msg); return;
            }
            pack_u32_reply(&mut msg.data, n as u32);
            reply_ok(client, msg);
        }
        OP_SYMLINK => {
            let (p1_ptr, p1_len, p2_ptr, p2_len) = unpack_two_paths_req(&msg.data);
            if p1_len == 0 || p1_len > 1024 || p2_len == 0 || p2_len > 1024 { reply_err(client, msg); return; }
            let mut link_buf = alloc::vec![0u8; p1_len];
            let mut target_buf = alloc::vec![0u8; p2_len];
            if libos::datacopy(client, p1_ptr as *const u8, libos::getpid(), link_buf.as_mut_ptr(), p1_len) < 0 ||
               libos::datacopy(client, p2_ptr as *const u8, libos::getpid(), target_buf.as_mut_ptr(), p2_len) < 0 {
                reply_err(client, msg); return;
            }
            let link = match core::str::from_utf8(&link_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            let target = match core::str::from_utf8(&target_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };

            let ok = if let Some(stripped) = link.strip_prefix("/mnt") {
                let p = if stripped.is_empty() { "/" } else { stripped };
                let ofs = match get_blockfs() { Some(o) => o, None => { reply_err(client, msg); return; } };
                let (parent, name) = match Vfs::parent_and_name(p) { Some(r) => r, None => { reply_err(client, msg); return; } };
                let (dir_ino, etype) = match ofs.lookup_path(&parent) { Some(r) => r, None => { reply_err(client, msg); return; } };
                if etype != ITYPE_DIR_ENTRY { reply_err(client, msg); return; }
                let child_ino = match ofs.create_inode(ITYPE_SYMLINK_ENTRY) { Some(i) => i, None => { reply_err(client, msg); return; } };
                if ofs.file_write(child_ino, 0, target.as_bytes()).is_err()
                    || !ofs.dir_add_entry(dir_ino, &name, child_ino, ITYPE_SYMLINK_ENTRY)
                {
                    ofs.free_inode(child_ino);
                    false
                } else {
                    true
                }
            } else {
                ns_vfs(namespaces, 1).symlink(link, target)
            };
            if ok { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_LINK => {
            let (p1_ptr, p1_len, p2_ptr, p2_len) = unpack_two_paths_req(&msg.data);
            if p1_len == 0 || p1_len > 1024 || p2_len == 0 || p2_len > 1024 { reply_err(client, msg); return; }
            let mut old_buf = alloc::vec![0u8; p1_len];
            let mut new_buf = alloc::vec![0u8; p2_len];
            if libos::datacopy(client, p1_ptr as *const u8, libos::getpid(), old_buf.as_mut_ptr(), p1_len) < 0 ||
               libos::datacopy(client, p2_ptr as *const u8, libos::getpid(), new_buf.as_mut_ptr(), p2_len) < 0 {
                reply_err(client, msg); return;
            }
            let old = match core::str::from_utf8(&old_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            let new = match core::str::from_utf8(&new_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            let ok = if old.starts_with("/mnt") || new.starts_with("/mnt") {
                // Both old and new must be on the same filesystem.
                let (old_p, new_p) = if let (Some(o), Some(n)) = (
                    old.strip_prefix("/mnt"),
                    new.strip_prefix("/mnt"),
                ) {
                    let o = if o.is_empty() { "/" } else { o };
                    let n = if n.is_empty() { "/" } else { n };
                    (o, n)
                } else {
                    reply_err(client, msg); return;
                };
                let ofs = match get_blockfs() { Some(o) => o, None => { reply_err(client, msg); return; } };
                let (old_ino, etype) = match ofs.lookup_path(old_p) { Some(r) => r, None => { reply_err(client, msg); return; } };
                let (parent, name) = match Vfs::parent_and_name(new_p) { Some(r) => r, None => { reply_err(client, msg); return; } };
                let (dir_ino, _) = match ofs.lookup_path(&parent) { Some(r) => r, None => { reply_err(client, msg); return; } };
                ofs.dir_link_entry(dir_ino, &name, old_ino, etype)
            } else {
                ns_vfs(namespaces, 1).link(old, new)
            };
            if ok { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_CHMOD => {
            let (mode, path_ptr, path_len, ns_id) = unpack_path_req(&msg.data);
            if path_len == 0 || path_len > 1024 { reply_err(client, msg); return; }
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            if ns_vfs(namespaces, ns_id).chmod(path, mode) { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_CHOWN => {
            let (owner, path_ptr, path_len, ns_id) = unpack_path_req(&msg.data);
            if path_len == 0 || path_len > 1024 { reply_err(client, msg); return; }
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            if ns_vfs(namespaces, ns_id).chown(path, owner, !0) { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_FTRUNCATE => {
            let (fd, length) = unpack_ftruncate_req(&msg.data);
            let (path, file_ns) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), f.ns_id),
                None => { reply_err(client, msg); return; }
            };
            let ok = if let Some(stripped) = path.strip_prefix("/mnt") {
                let p = if stripped.is_empty() { "/" } else { stripped };
                let ofs = match get_blockfs() { Some(o) => o, None => { reply_err(client, msg); return; } };
                let (ino, etype) = match ofs.lookup_path(p) { Some(r) => r, None => { reply_err(client, msg); return; } };
                if etype != ITYPE_FILE_ENTRY { false } else {
                    ofs.file_truncate(ino);
                    true
                }
            } else {
                ns_vfs(namespaces, file_ns).truncate(&path, length)
            };
            if ok { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_LSEEK => {
            let (fd, offset, whence) = unpack_lseek_req(&msg.data);
            let (file_path, file_off, file_ns) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), f.offset, f.ns_id),
                None => { reply_err(client, msg); return; }
            };
            // We need the file size to compute SEEK_END.
            let file_size = ns_vfs(namespaces, file_ns).stat(&file_path).map(|(_, s)| s).unwrap_or(0);
            let new_offset = match whence {
                0 /* SEEK_SET */ => offset,
                1 /* SEEK_CUR */ => file_off as i64 + offset,
                2 /* SEEK_END */ => file_size as i64 + offset,
                _ => { reply_err(client, msg); return; }
            };
            if new_offset < 0 { reply_err(client, msg); return; }
            if let Some(f) = open_table.get_mut(fd) {
                f.offset = new_offset as u64;
                pack_u64_reply(&mut msg.data, new_offset as u64);
                reply_ok(client, msg);
            } else {
                reply_err(client, msg);
            }
        }
        OP_FPATH => {
            let (fd, buf_ptr, buf_len) = unpack_fpath_req(&msg.data);
            let path = match open_table.get(fd) {
                Some(f) => f.path.as_bytes(),
                None => { reply_err(client, msg); return; }
            };
            let n = path.len().min(buf_len);
            let mut data_buf = alloc::vec![0u8; n];
            data_buf.copy_from_slice(&path[..n]);
            if libos::datacopy(libos::getpid(), data_buf.as_ptr(), client, buf_ptr as *mut u8, n) < 0 {
                reply_err(client, msg); return;
            }
            pack_u32_reply(&mut msg.data, n as u32);
            reply_ok(client, msg);
        }
        OP_FUTIMENS => {
            let (fd, ats, _atns, mts, _mtns) = unpack_futimens_req(&msg.data);
            let (path, file_ns) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), f.ns_id),
                None => { reply_err(client, msg); return; }
            };
            // Convert nsec timestamps to whole-seconds (round down)
            let atime = if ats >= 0 { ats as u64 } else { 0 };
            let mtime = if mts >= 0 { mts as u64 } else { 0 };
            if ns_vfs(namespaces, file_ns).utimens(&path, atime, mtime) { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
         OP_FALLOCATE => {
            let (fd, _offset, _length) = unpack_fallocate_req(&msg.data);
            let (path, file_ns) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), f.ns_id),
                None => { reply_err(client, msg); return; }
            };
            if ns_vfs(namespaces, file_ns).exists(&path) { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_FSTAT => {
            let fd = unpack_fstat_req(&msg.data);
            let (path, file_ns) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), f.ns_id),
                None => { reply_err(client, msg); return; }
            };
            match ns_vfs(namespaces, file_ns).stat(&path) {
                Some((is_dir, size)) => {
                    pack_stat_reply(&mut msg.data, is_dir, size);
                    reply_ok(client, msg);
                }
                None => reply_err(client, msg),
            }
        }
        _ => { reply_err(client, msg); }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: usize, _argv: usize) -> i32 {
    vfs_register();

    // Namespace 1 = root mount namespace (kernel assigns ID 1 to the first MountNs).
    let mut namespaces: BTreeMap<u32, Vfs> = BTreeMap::new();
    let root_vfs = namespaces.entry(1).or_insert_with(Vfs::new);
    scan_initrd(root_vfs);
    root_vfs.mkdir_all("/tmp");
    root_vfs.mkdir_all("/home/guest");
    if let Some(FsNode::Dir(root_children)) = root_vfs.nodes.get_mut("/") {
        if !root_children.iter().any(|n| n == "proc") { root_children.push(String::from("proc")); }
        if !root_children.iter().any(|n| n == "dev")  { root_children.push(String::from("dev")); }
    }

    let mut open_table = OpenTable::new();
    let mut msg = libos::ipc::Message::new();

    loop {
        libos::msg_receive(-1, &mut msg);
        dispatch(&mut namespaces, &mut open_table, msg);
    }
}

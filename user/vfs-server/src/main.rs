#![no_std]
#![no_main]

extern crate alloc;
extern crate libos;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use vfs_proto::*;

mod blockfs;

// ── In-memory filesystem ──────────────────────────────────────────────────────

#[allow(static_mut_refs)]
fn get_blockfs() -> Option<&'static mut blockfs::OfsState> {
    static mut STATE: Option<blockfs::OfsState> = None;
    unsafe {
        if STATE.is_none() {
            // Resolve the block driver PID dynamically — never hard-code it.
            let blk_pid = libos::get_blk_pid();
            if blk_pid > 0 {
                // new() returns None if the driver isn't ready yet; we'll retry next call.
                STATE = blockfs::OfsState::new(blockfs::VirtioBlkProxy::new(blk_pid));
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
}

struct Vfs {
    nodes: BTreeMap<String, FsNode>,
}

impl Vfs {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(String::from("/"), FsNode::Dir(Vec::new()));
        Self { nodes }
    }

    fn exists(&self, path: &str) -> bool {
        if is_virtual_path(path) { return true; }
        if path.starts_with("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if path == "/mnt" { "/" } else { &path[4..] };
                return ofs.lookup_path(p).is_some();
            }
        }
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
        if path.starts_with("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if path == "/mnt" { "/" } else { &path[4..] };
                if let Some((parent, name)) = Self::parent_and_name(p) {
                    if let Some((dir_ino, etype)) = ofs.lookup_path(&parent) {
                        if etype == blockfs::ITYPE_DIR_ENTRY {
                            let child_ino = match ofs.alloc_inode() {
                                Some(i) => i,
                                None => return false,
                            };
                            let new_inode = blockfs::RawInode {
                                itype: blockfs::ITYPE_DIR,
                                _pad: [0; 3],
                                size: 0,
                                nlink: 1,
                                used_blocks: 0,
                                direct: [0u32; 12],
                                indirect: 0,
                                _reserved: [0u32; 15],
                            };
                            ofs.write_inode(child_ino, &new_inode);
                            if !ofs.dir_add_entry(dir_ino, &name, child_ino, blockfs::ITYPE_DIR_ENTRY) {
                                ofs.free_inode(child_ino);
                                return false;
                            }
                            return true;
                        }
                    }
                }
            }
            return false;
        }
        if self.exists(path) { return false; }
        if let Some((parent, name)) = Self::parent_and_name(path)
            && let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent)
        {
            children.push(name);
            self.nodes.insert(String::from(path), FsNode::Dir(Vec::new()));
            return true;
        }
        false
    }

    fn insert_tar_file(&mut self, path: &str, tar_offset: usize, size: usize) {
        if let Some((parent, name)) = Self::parent_and_name(path) {
            let already_exists = self.nodes.contains_key(path);
            if let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent) {
                if !already_exists { children.push(name); }
                self.nodes.insert(String::from(path), FsNode::TarFile { tar_offset, size });
            }
        }
    }

    fn create(&mut self, path: &str) -> bool {
        if path.starts_with("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if path == "/mnt" { "/" } else { &path[4..] };
                if let Some((parent, name)) = Self::parent_and_name(p) {
                    if let Some((dir_ino, etype)) = ofs.lookup_path(&parent) {
                        if etype == blockfs::ITYPE_DIR_ENTRY {
                            // If it exists, truncate it instead of failing
                            if let Some((child_ino, ctype)) = ofs.lookup_path(p) {
                                if ctype == blockfs::ITYPE_FILE_ENTRY {
                                    ofs.file_truncate(child_ino);
                                    return true;
                                }
                                return false; // Is a directory
                            }

                            let child_ino = match ofs.alloc_inode() {
                                Some(i) => i,
                                None => return false,
                            };
                            let new_inode = blockfs::RawInode {
                                itype: blockfs::ITYPE_FILE,
                                _pad: [0; 3],
                                size: 0,
                                nlink: 1,
                                used_blocks: 0,
                                direct: [0u32; 12],
                                indirect: 0,
                                _reserved: [0u32; 15],
                            };
                            ofs.write_inode(child_ino, &new_inode);
                            if !ofs.dir_add_entry(dir_ino, &name, child_ino, blockfs::ITYPE_FILE_ENTRY) {
                                ofs.free_inode(child_ino);
                                return false;
                            }
                            return true;
                        }
                    }
                }
            }
            return false;
        }
        if let Some((parent, name)) = Self::parent_and_name(path) {
            let already_exists = self.nodes.contains_key(path);
            if let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent) {
                if !already_exists { children.push(name); }
                self.nodes.insert(String::from(path), FsNode::MemFile(Vec::new()));
                return true;
            }
        }
        false
    }

    fn unlink(&mut self, path: &str) -> bool {
        if path.starts_with("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if path == "/mnt" { "/" } else { &path[4..] };
                if let Some((parent, name)) = Self::parent_and_name(p) {
                    if let Some((dir_ino, etype)) = ofs.lookup_path(&parent) {
                        if etype == blockfs::ITYPE_DIR_ENTRY {
                            return ofs.dir_unlink(dir_ino, &name);
                        }
                    }
                }
            }
            return false;
        }
        if !self.nodes.contains_key(path) { return false; }
        if let Some((parent, name)) = Self::parent_and_name(path)
            && let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent)
        {
            children.retain(|c| c != &name);
            self.nodes.remove(path);
            return true;
        }
        false
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Option<usize> {
        // Virtual /dev files
        if path == "/dev/null" { return Some(0); } // always EOF
        if path == "/dev/zero" {
            let n = buf.len();
            buf.fill(0);
            return Some(n);
        }
        if path == "/dev/tty" { return Some(0); } // stub

        // Virtual /proc files
        if let Some(rest) = path.strip_prefix("/proc/") {
            if let Some((pid_str, "status")) = rest.split_once('/') {
                if let Ok(pid) = pid_str.parse::<i32>() {
                    return virtual_proc_read_status(pid, offset, buf);
                }
            }
            return None;
        }

        if path.starts_with("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if path == "/mnt" { "/" } else { &path[4..] };
                if let Some((ino, etype)) = ofs.lookup_path(p) {
                    if etype == blockfs::ITYPE_FILE_ENTRY {
                        match ofs.file_read(ino, offset as usize, buf) {
                            Ok(n) => return Some(n),
                            Err(_) => return None,
                        }
                    }
                }
            }
            return None;
        }
        match self.nodes.get(path)? {
            FsNode::TarFile { tar_offset, size } => {
                let file_off = offset as usize;
                if file_off >= *size { return Some(0); }
                let avail = size - file_off;
                let take = buf.len().min(avail);
                // Fetch data on-demand from the initrd TAR (no copy kept in VFS).
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
        }
    }

    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Option<usize> {
        if path.starts_with("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if path == "/mnt" { "/" } else { &path[4..] };
                if let Some((ino, etype)) = ofs.lookup_path(p) {
                    if etype == blockfs::ITYPE_FILE_ENTRY {
                        match ofs.file_write(ino, offset as usize, data) {
                            Ok(n) => return Some(n),
                            Err(_) => return None,
                        }
                    }
                }
            }
            return None;
        }
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
        }
    }

    fn readdir(&self, path: &str) -> Option<Vec<String>> {
        // Virtual directories
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
        if let Some(pid_str) = path.strip_prefix("/proc/") {
            if pid_str.parse::<i32>().is_ok() {
                return Some(alloc::vec![String::from("status")]);
            }
        }

        if path.starts_with("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if path == "/mnt" { "/" } else { &path[4..] };
                if let Some((dir_ino, etype)) = ofs.lookup_path(p) {
                    if etype == blockfs::ITYPE_DIR_ENTRY {
                        let entries = ofs.dir_readall(dir_ino);
                        let mut children = Vec::new();
                        for (name, _, _) in entries {
                            children.push(name);
                        }
                        return Some(children);
                    }
                }
            }
            return None;
        }
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
            let (pid_part, sub) = rest.split_once('/').map(|(a, b)| (a, b)).unwrap_or((rest, ""));
            if pid_part.parse::<i32>().is_ok() {
                if sub.is_empty() { return Some((true, 0)); }
                if sub == "status" { return Some((false, 256)); }
            }
            return None;
        }

        if path.starts_with("/mnt") {
            if let Some(ofs) = get_blockfs() {
                let p = if path == "/mnt" { "/" } else { &path[4..] };
                if let Some((ino, etype)) = ofs.lookup_path(p) {
                    if etype == blockfs::ITYPE_DIR_ENTRY {
                        return Some((true, 0));
                    } else if let Some(inode) = ofs.read_inode(ino) {
                        return Some((false, inode.size as u64));
                    }
                }
            }
            return None;
        }
        match self.nodes.get(path)? {
            FsNode::TarFile { size, .. } => Some((false, *size as u64)),
            FsNode::MemFile(data)        => Some((false, data.len() as u64)),
            FsNode::Dir(_)               => Some((true, 0)),
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

fn scan_initrd(vfs: &mut Vfs) {
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

struct OpenFile { path: String, offset: u64 }

struct OpenTable { files: BTreeMap<u32, OpenFile>, next_fd: u32 }

impl OpenTable {
    fn new() -> Self { Self { files: BTreeMap::new(), next_fd: 1 } }
    fn open(&mut self, path: &str) -> u32 {
        let fd = self.next_fd; self.next_fd += 1;
        self.files.insert(fd, OpenFile { path: String::from(path), offset: 0 });
        fd
    }
    fn get(&self, fd: u32) -> Option<&OpenFile> { self.files.get(&fd) }
    fn get_mut(&mut self, fd: u32) -> Option<&mut OpenFile> { self.files.get_mut(&fd) }
    fn close(&mut self, fd: u32) { self.files.remove(&fd); }
    fn dup(&mut self, fd: u32) -> Option<u32> {
        let file = self.files.get(&fd)?;
        let new_fd = self.next_fd;
        self.next_fd += 1;
        self.files.insert(new_fd, OpenFile { path: file.path.clone(), offset: file.offset });
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

// ── Dispatch ──────────────────────────────────────────────────────────────────

fn dispatch(vfs: &mut Vfs, open_table: &mut OpenTable, mut msg: libos::ipc::Message) {
    let client = msg.source;
    let op = msg.type_;

    match op {
        OP_OPEN => {
            let (_flags, path_ptr, path_len) = unpack_path_req(&msg.data);
            if path_len == 0 || path_len > 1024 { reply_err(client, msg); return; }
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            
            if vfs.exists(path) {
                let fd = open_table.open(path);
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
            let (path, read_off) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), if offset == u64::MAX { f.offset } else { offset }),
                None => { reply_err(client, msg); return; }
            };

            let mut data_buf = alloc::vec![0u8; len_req.min(1024 * 1024)]; // max 1MB per chunk
            let take = data_buf.len();
            match vfs.read(&path, read_off, &mut data_buf[..take]) {
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
            let (path, write_off) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), if offset == u64::MAX { f.offset } else { offset }),
                None => { reply_err(client, msg); return; }
            };

            let mut data_buf = alloc::vec![0u8; len_req.min(1024 * 1024)];
            let n = data_buf.len();
            if libos::datacopy(client, buf_ptr as *const u8, libos::getpid(), data_buf.as_mut_ptr(), n) < 0 {
                reply_err(client, msg); return;
            }

            match vfs.write(&path, write_off, &data_buf) {
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
            
            match vfs.readdir(path) {
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
            let (_, path_ptr, path_len) = unpack_path_req(&msg.data);
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            if vfs.mkdir(path) { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_CREATE => {
            let (_, path_ptr, path_len) = unpack_path_req(&msg.data);
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            if vfs.create(path) {
                let fd = open_table.open(path);
                pack_u32_reply(&mut msg.data, fd);
                reply_ok(client, msg);
            } else {
                reply_err(client, msg)
            }
        }
        OP_UNLINK => {
            let (_, path_ptr, path_len) = unpack_path_req(&msg.data);
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            if vfs.unlink(path) { reply_ok(client, msg) } else { reply_err(client, msg) }
        }
        OP_STAT => {
            let (_, path_ptr, path_len) = unpack_path_req(&msg.data);
            let mut path_buf = alloc::vec![0u8; path_len];
            if libos::datacopy(client, path_ptr as *const u8, libos::getpid(), path_buf.as_mut_ptr(), path_len) < 0 {
                reply_err(client, msg); return;
            }
            let path = match core::str::from_utf8(&path_buf) { Ok(s) => s, Err(_) => { reply_err(client, msg); return; } };
            match vfs.stat(path) {
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
                reply_ok(client, msg);
            } else {
                reply_err(client, msg);
            }
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
        _ => reply_err(client, msg),
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: usize, _argv: usize) -> i32 {
    vfs_register();

    let mut vfs = Vfs::new();
    scan_initrd(&mut vfs);      // O(entries) time, O(metadata) memory
    vfs.mkdir_all("/tmp");
    vfs.mkdir_all("/home/guest");

    // Register /proc and /dev as visible root entries so ls / lists them.
    // Their content is synthesised on demand; the Dir entries only hold the name.
    if let Some(FsNode::Dir(root_children)) = vfs.nodes.get_mut("/") {
        if !root_children.iter().any(|n| n == "proc") { root_children.push(String::from("proc")); }
        if !root_children.iter().any(|n| n == "dev")  { root_children.push(String::from("dev")); }
    }

    let mut open_table = OpenTable::new();
    let mut msg = libos::ipc::Message::new();

    loop {
        libos::msg_receive(-1, &mut msg);
        dispatch(&mut vfs, &mut open_table, msg);
    }
}

#![no_std]
#![no_main]

extern crate alloc;
extern crate libos;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use vfs_proto::*;

// ── In-memory filesystem ──────────────────────────────────────────────────────

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

    fn exists(&self, path: &str) -> bool { self.nodes.contains_key(path) }

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
        if !self.exists(path) { return false; }
        if let Some((parent, name)) = Self::parent_and_name(path)
            && let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent)
        {
            children.retain(|c| c != &name);
        }
        self.nodes.remove(path);
        true
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Option<usize> {
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
        match self.nodes.get(path)? {
            FsNode::Dir(children) => Some(children.clone()),
            _ => None,
        }
    }

    fn stat(&self, path: &str) -> Option<(bool, u64)> {
        match self.nodes.get(path)? {
            FsNode::TarFile { size, .. } => Some((false, *size as u64)),
            FsNode::MemFile(data)        => Some((false, data.len() as u64)),
            FsNode::Dir(_)               => Some((true, 0)),
        }
    }
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
}

// ── IPC helpers ───────────────────────────────────────────────────────────────

fn ipc_send(to: i32, buf: &[u8]) -> i32 {
    libos::syscall(61, to as usize, buf.as_ptr() as usize, buf.len()) as i32
}

fn ipc_recv(buf: &mut [u8], from: &mut i32) -> usize {
    libos::syscall(62, buf.as_mut_ptr() as usize, buf.len(), from as *mut i32 as usize)
}

fn vfs_register() { libos::syscall(63, 0, 0, 0); }

// ── Reply helpers ─────────────────────────────────────────────────────────────

fn reply_ok(client: i32, payload: &[u8]) {
    let mut buf = [0u8; MAX_MSG];
    buf[0] = REPLY_OK;
    let n = payload.len().min(MAX_MSG - 1);
    buf[1..1 + n].copy_from_slice(&payload[..n]);
    ipc_send(client, &buf[..1 + n]);
}

fn reply_err(client: i32) { ipc_send(client, &[REPLY_ERR]); }

// ── Dispatch ──────────────────────────────────────────────────────────────────

fn dispatch(vfs: &mut Vfs, open_table: &mut OpenTable, client: i32, msg: &[u8], len: usize) {
    if len == 0 { reply_err(client); return; }
    let op = msg[0];
    let body = &msg[1..len];

    match op {
        OP_OPEN => {
            if body.len() < 4 { reply_err(client); return; }
            let mut off = 0;
            let _flags = get_u32(body, &mut off);
            let path = match core::str::from_utf8(&body[off..]) { Ok(s) => s, Err(_) => { reply_err(client); return; } };
            if vfs.exists(path) {
                let fd = open_table.open(path);
                let mut p = [0u8; 4];
                put_u32(&mut p, &mut 0, fd);
                reply_ok(client, &p);
            } else {
                reply_err(client);
            }
        }
        OP_READ => {
            if body.len() < 13 { reply_err(client); return; }
            let mut off = 0;
            let fd      = get_u32(body, &mut off);
            let offset  = get_u64(body, &mut off);
            let len_req = get_u32(body, &mut off) as usize;

            let (path, read_off) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), if offset == u64::MAX { f.offset } else { offset }),
                None => { reply_err(client); return; }
            };

            let mut data_buf = [0u8; MAX_MSG - 1];
            let take = len_req.min(data_buf.len());
            match vfs.read(&path, read_off, &mut data_buf[..take]) {
                Some(n) => {
                    if let Some(f) = open_table.get_mut(fd) { f.offset = read_off + n as u64; }
                    reply_ok(client, &data_buf[..n]);
                }
                None => reply_err(client),
            }
        }
        OP_WRITE => {
            if body.len() < 13 { reply_err(client); return; }
            let mut off = 0;
            let fd     = get_u32(body, &mut off);
            let offset = get_u64(body, &mut off);
            let data   = &body[off..];

            let (path, write_off) = match open_table.get(fd) {
                Some(f) => (f.path.clone(), if offset == u64::MAX { f.offset } else { offset }),
                None => { reply_err(client); return; }
            };

            match vfs.write(&path, write_off, data) {
                Some(n) => {
                    if let Some(f) = open_table.get_mut(fd) { f.offset = write_off + n as u64; }
                    let mut p = [0u8; 4];
                    put_u32(&mut p, &mut 0, n as u32);
                    reply_ok(client, &p);
                }
                None => reply_err(client),
            }
        }
        OP_CLOSE => {
            if body.len() < 4 { reply_err(client); return; }
            let mut off = 0;
            let fd = get_u32(body, &mut off);
            open_table.close(fd);
            reply_ok(client, &[]);
        }
        OP_GETDENTS => {
            let path = match core::str::from_utf8(body) { Ok(s) => s, Err(_) => { reply_err(client); return; } };
            match vfs.readdir(path) {
                Some(children) => {
                    let mut buf = [0u8; MAX_MSG - 1];
                    let mut pos = 0usize;
                    for child in &children {
                        let b = child.as_bytes();
                        if pos + b.len() + 1 > buf.len() { break; }
                        buf[pos..pos + b.len()].copy_from_slice(b);
                        pos += b.len();
                        buf[pos] = 0; pos += 1;
                    }
                    reply_ok(client, &buf[..pos]);
                }
                None => reply_err(client),
            }
        }
        OP_MKDIR => {
            let path = match core::str::from_utf8(body) { Ok(s) => s, Err(_) => { reply_err(client); return; } };
            if vfs.mkdir(path) { reply_ok(client, &[]) } else { reply_err(client) }
        }
        OP_CREATE => {
            let path = match core::str::from_utf8(body) { Ok(s) => s, Err(_) => { reply_err(client); return; } };
            if vfs.create(path) {
                let fd = open_table.open(path);
                let mut p = [0u8; 4];
                put_u32(&mut p, &mut 0, fd);
                reply_ok(client, &p);
            } else {
                reply_err(client)
            }
        }
        OP_UNLINK => {
            let path = match core::str::from_utf8(body) { Ok(s) => s, Err(_) => { reply_err(client); return; } };
            if vfs.unlink(path) { reply_ok(client, &[]) } else { reply_err(client) }
        }
        OP_STAT => {
            let path = match core::str::from_utf8(body) { Ok(s) => s, Err(_) => { reply_err(client); return; } };
            match vfs.stat(path) {
                Some((is_dir, size)) => {
                    let mut p = [0u8; 9];
                    p[0] = if is_dir { 1 } else { 0 };
                    let mut off = 1;
                    put_u64(&mut p, &mut off, size);
                    reply_ok(client, &p);
                }
                None => reply_err(client),
            }
        }
        OP_RENAME => {
            if body.len() < 4 { reply_err(client); return; }
            let mut off = 0;
            let old_len = get_u32(body, &mut off) as usize;
            if body.len() < 4 + old_len { reply_err(client); return; }
            let old = match core::str::from_utf8(&body[off..off + old_len]) { Ok(s) => s, Err(_) => { reply_err(client); return; } };
            let new = match core::str::from_utf8(&body[off + old_len..]) { Ok(s) => s, Err(_) => { reply_err(client); return; } };

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
                reply_ok(client, &[]);
            } else {
                reply_err(client);
            }
        }
        _ => reply_err(client),
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

    let mut open_table = OpenTable::new();
    let mut msg_buf = [0u8; MAX_MSG];

    loop {
        let mut from: i32 = 0;
        let len = ipc_recv(&mut msg_buf, &mut from);
        if len == 0 { continue; }
        dispatch(&mut vfs, &mut open_table, from, &msg_buf, len);
    }
}

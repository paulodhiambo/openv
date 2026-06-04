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
    File(Vec<u8>),
    Dir(Vec<String>), // child names
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

    /// Create directory and all ancestors silently.
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

    fn create(&mut self, path: &str) -> bool {
        if let Some((parent, name)) = Self::parent_and_name(path) {
            let already_exists = self.nodes.contains_key(path);
            if let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent) {
                if !already_exists { children.push(name); }
                self.nodes.insert(String::from(path), FsNode::File(Vec::new()));
                return true;
            }
        }
        false
    }

    fn create_with_data(&mut self, path: &str, data: Vec<u8>) -> bool {
        if let Some((parent, name)) = Self::parent_and_name(path) {
            let already_exists = self.nodes.contains_key(path);
            if let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent) {
                if !already_exists { children.push(name); }
                self.nodes.insert(String::from(path), FsNode::File(data));
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
            FsNode::File(data) => {
                let start = (offset as usize).min(data.len());
                let end = (start + buf.len()).min(data.len());
                let n = end - start;
                buf[..n].copy_from_slice(&data[start..end]);
                Some(n)
            }
            FsNode::Dir(_) => None,
        }
    }

    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Option<usize> {
        match self.nodes.get_mut(path)? {
            FsNode::File(buf) => {
                let start = offset as usize;
                let end = start + data.len();
                if buf.len() < end { buf.resize(end, 0); }
                buf[start..end].copy_from_slice(data);
                Some(data.len())
            }
            FsNode::Dir(_) => None,
        }
    }

    fn readdir(&self, path: &str) -> Option<Vec<String>> {
        match self.nodes.get(path)? {
            FsNode::Dir(children) => Some(children.clone()),
            FsNode::File(_) => None,
        }
    }

    fn stat(&self, path: &str) -> Option<(bool, u64)> {
        match self.nodes.get(path)? {
            FsNode::File(data) => Some((false, data.len() as u64)),
            FsNode::Dir(_) => Some((true, 0)),
        }
    }
}

// ── TAR parser ───────────────────────────────────────────────────────────────

fn octal_to_usize(bytes: &[u8]) -> usize {
    let mut v = 0usize;
    for &b in bytes {
        if b == 0 || b == b' ' { break; }
        if (b'0'..=b'7').contains(&b) { v = v * 8 + (b - b'0') as usize; }
    }
    v
}

fn populate_from_tar(vfs: &mut Vfs, tar: &[u8]) {
    let mut offset = 0usize;
    let mut consecutive_empty = 0u32;

    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512];

        // Two consecutive zero blocks = end of archive.
        if header.iter().all(|&b| b == 0) {
            consecutive_empty += 1;
            if consecutive_empty >= 2 { break; }
            offset += 512;
            continue;
        }
        consecutive_empty = 0;

        // Name (null-terminated, up to 100 bytes; UStar prefix at 345..500 ignored).
        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = match core::str::from_utf8(&header[..name_end]) {
            Ok(s) => s,
            Err(_) => { offset += 512; continue; }
        };
        if name.is_empty() { offset += 512; continue; }

        let type_flag = header[156];
        let size = octal_to_usize(&header[124..136]);

        // Canonicalize to absolute path, strip trailing slash.
        let mut path = if name.starts_with('/') {
            String::from(name)
        } else {
            let mut s = String::from("/");
            s.push_str(name);
            s
        };
        if path.len() > 1 && path.ends_with('/') {
            path.pop();
        }

        let is_dir = type_flag == b'5'
            || (type_flag == 0 && size == 0 && name.ends_with('/'));

        if is_dir {
            vfs.mkdir_all(&path);
        } else if type_flag == b'0' || type_flag == 0 || type_flag == b'\0' {
            // Regular file.  Ensure all parents exist first.
            if let Some(slash) = path.rfind('/') {
                let parent = if slash == 0 { "/" } else { &path[..slash] };
                vfs.mkdir_all(parent);
            }
            let data_start = offset + 512;
            let data_end = (data_start + size).min(tar.len());
            let data = tar[data_start..data_end].to_vec();
            vfs.create_with_data(&path, data);
        }

        let data_blocks = size.div_ceil(512);
        offset += 512 + data_blocks * 512;
    }
}

// ── Open-file table ───────────────────────────────────────────────────────────

struct OpenFile {
    path: String,
    offset: u64,
}

struct OpenTable {
    files: BTreeMap<u32, OpenFile>,
    next_fd: u32,
}

impl OpenTable {
    fn new() -> Self { Self { files: BTreeMap::new(), next_fd: 1 } }

    fn open(&mut self, path: &str) -> u32 {
        let fd = self.next_fd;
        self.next_fd += 1;
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

/// Fetch the full initrd TAR from the kernel (syscall 65).
fn fetch_initrd() -> Vec<u8> {
    let mut buf = Vec::new();
    let mut offset = 0usize;
    let mut chunk = [0u8; 4096];
    loop {
        let n = libos::syscall(65, chunk.as_mut_ptr() as usize, offset, chunk.len());
        if n == 0 { break; }
        buf.extend_from_slice(&chunk[..n]);
        offset += n;
    }
    buf
}

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
            let path = match core::str::from_utf8(&body[off..]) {
                Ok(s) => s,
                Err(_) => { reply_err(client); return; }
            };
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
            let fd     = get_u32(body, &mut off);
            let offset = get_u64(body, &mut off);
            let len_req = get_u32(body, &mut off) as usize;

            let (path, read_off) = match open_table.get(fd) {
                Some(f) => (f.path.clone(),
                             if offset == u64::MAX { f.offset } else { offset }),
                None => { reply_err(client); return; }
            };

            let mut data_buf = [0u8; MAX_MSG - 1];
            let take = len_req.min(data_buf.len());
            match vfs.read(&path, read_off, &mut data_buf[..take]) {
                Some(n) => {
                    if let Some(f) = open_table.get_mut(fd) {
                        f.offset = read_off + n as u64;
                    }
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
                Some(f) => (f.path.clone(),
                             if offset == u64::MAX { f.offset } else { offset }),
                None => { reply_err(client); return; }
            };

            match vfs.write(&path, write_off, data) {
                Some(n) => {
                    if let Some(f) = open_table.get_mut(fd) {
                        f.offset = write_off + n as u64;
                    }
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
            let path = match core::str::from_utf8(body) {
                Ok(s) => s,
                Err(_) => { reply_err(client); return; }
            };
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
    // Register as the VFS server before touching the filesystem.
    vfs_register();

    let mut vfs = Vfs::new();

    // Populate from the kernel's initrd TAR (fetch in 4 KB chunks via syscall 65).
    {
        let tar = fetch_initrd();
        if !tar.is_empty() {
            populate_from_tar(&mut vfs, &tar);
        }
    }

    // Ensure standard writable directories exist even if absent from initrd.
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

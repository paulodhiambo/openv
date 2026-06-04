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
        // Root directory always exists.
        nodes.insert(String::from("/"), FsNode::Dir(Vec::new()));
        Self { nodes }
    }

    /// Return `true` if the path exists.
    fn exists(&self, path: &str) -> bool {
        self.nodes.contains_key(path)
    }

    /// Canonical parent path + final component for a given path.
    fn parent_and_name(path: &str) -> Option<(String, String)> {
        let path = path.trim_end_matches('/');
        let slash = path.rfind('/')?;
        let parent = if slash == 0 { "/" } else { &path[..slash] };
        let name = &path[slash + 1..];
        if name.is_empty() { return None; }
        Some((String::from(parent), String::from(name)))
    }

    fn mkdir(&mut self, path: &str) -> bool {
        if self.exists(path) { return false; }
        if let Some((parent, name)) = Self::parent_and_name(path) {
            if let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent) {
                children.push(name);
                self.nodes.insert(String::from(path), FsNode::Dir(Vec::new()));
                return true;
            }
        }
        false
    }

    fn create(&mut self, path: &str) -> bool {
        if let Some((parent, name)) = Self::parent_and_name(path) {
            let already_exists = self.nodes.contains_key(path);
            if let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent) {
                if !already_exists {
                    children.push(name);
                }
                // Insert or truncate.
                self.nodes.insert(String::from(path), FsNode::File(Vec::new()));
                return true;
            }
        }
        false
    }

    fn unlink(&mut self, path: &str) -> bool {
        if !self.exists(path) { return false; }
        if let Some((parent, name)) = Self::parent_and_name(path) {
            if let Some(FsNode::Dir(children)) = self.nodes.get_mut(&parent) {
                children.retain(|c| c != &name);
            }
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

// ── IPC helpers (thin libos wrappers) ────────────────────────────────────────

fn ipc_send(to: i32, buf: &[u8]) -> i32 {
    libos::syscall(61, to as usize, buf.as_ptr() as usize, buf.len()) as i32
}

fn ipc_recv(buf: &mut [u8], from: &mut i32) -> usize {
    libos::syscall(62, buf.as_mut_ptr() as usize, buf.len(), from as *mut i32 as usize)
}

fn vfs_register() {
    libos::syscall(63, 0, 0, 0);
}

// ── Reply builders ────────────────────────────────────────────────────────────

fn reply_ok(client: i32, payload: &[u8]) {
    let mut buf = [0u8; MAX_MSG];
    buf[0] = REPLY_OK;
    let n = payload.len().min(MAX_MSG - 1);
    buf[1..1 + n].copy_from_slice(&payload[..n]);
    ipc_send(client, &buf[..1 + n]);
}

fn reply_err(client: i32) {
    let buf = [REPLY_ERR];
    ipc_send(client, &buf);
}

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
            let path_bytes = &body[off..];
            let path = core::str::from_utf8(path_bytes).unwrap_or("");
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
            let fd   = get_u32(body, &mut off);
            let offset = get_u64(body, &mut off);
            let len_req = get_u32(body, &mut off) as usize;

            // Use stored offset if read was sequential.
            let read_off = if let Some(f) = open_table.get(fd) {
                if offset == u64::MAX { f.offset } else { offset }
            } else {
                reply_err(client); return;
            };

            let path = if let Some(f) = open_table.get(fd) {
                f.path.clone()
            } else {
                reply_err(client); return;
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

            let write_off = if let Some(f) = open_table.get(fd) {
                if offset == u64::MAX { f.offset } else { offset }
            } else {
                reply_err(client); return;
            };

            let path = if let Some(f) = open_table.get(fd) {
                f.path.clone()
            } else {
                reply_err(client); return;
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
            let path = core::str::from_utf8(body).unwrap_or("");
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
            let path = core::str::from_utf8(body).unwrap_or("");
            if vfs.mkdir(path) { reply_ok(client, &[]) } else { reply_err(client) }
        }
        OP_CREATE => {
            let path = core::str::from_utf8(body).unwrap_or("");
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
            let path = core::str::from_utf8(body).unwrap_or("");
            if vfs.unlink(path) { reply_ok(client, &[]) } else { reply_err(client) }
        }
        OP_STAT => {
            let path = core::str::from_utf8(body).unwrap_or("");
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
            let old_bytes = &body[off..off + old_len]; off += old_len;
            let new_bytes = &body[off..];
            let old = core::str::from_utf8(old_bytes).unwrap_or("");
            let new = core::str::from_utf8(new_bytes).unwrap_or("");

            // Move node: insert under new name, remove old.
            if let Some(node) = vfs.nodes.remove(old) {
                if let Some((old_parent, old_name)) = Vfs::parent_and_name(old) {
                    if let Some(FsNode::Dir(c)) = vfs.nodes.get_mut(&old_parent) {
                        c.retain(|x| x != &old_name);
                    }
                }
                if let Some((new_parent, new_name)) = Vfs::parent_and_name(new) {
                    if let Some(FsNode::Dir(c)) = vfs.nodes.get_mut(&new_parent) {
                        c.push(new_name);
                    }
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
    let mut open_table = OpenTable::new();

    // Pre-create standard directories.
    vfs.mkdir("/tmp");
    vfs.mkdir("/home");
    vfs.mkdir("/home/guest");

    let mut msg_buf = [0u8; MAX_MSG];
    loop {
        let mut from: i32 = 0;
        let len = ipc_recv(&mut msg_buf, &mut from);
        if len == 0 { continue; }
        dispatch(&mut vfs, &mut open_table, from, &msg_buf, len);
    }
}

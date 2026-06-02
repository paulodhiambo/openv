pub mod memfs;
pub mod tar;

use alloc::sync::Arc;
use spin::Mutex;
use crate::posix::process::Process;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VnodeType {
    File,
    Directory,
}

pub type AccessMask = u32;
pub const ACCESS_READ: AccessMask = 1 << 2;
pub const ACCESS_WRITE: AccessMask = 1 << 1;
pub const ACCESS_EXEC: AccessMask = 1 << 0;

#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: usize,
    pub vtype: VnodeType,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: alloc::string::String,
    pub vtype: VnodeType,
}

/// Abstract representation of a file or directory
pub trait Vnode: Send + Sync {
    fn stat(&self) -> Stat;

    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, &'static str> {
        Err("Not supported")
    }

    fn write_at(&self, _offset: usize, _buf: &[u8]) -> Result<usize, &'static str> {
        Err("Not writable")
    }

    fn truncate(&self, _size: usize) -> Result<(), &'static str> {
        Err("Not supported")
    }

    fn lookup(&self, _name: &str) -> Result<Arc<dyn Vnode>, &'static str> {
        Err("Not a directory")
    }

    fn readdir(&self) -> Result<alloc::vec::Vec<DirEntry>, &'static str> {
        Err("Not a directory")
    }

    /// Create a regular file child inside a directory (truncates if already exists).
    fn create(&self, _name: &str) -> Result<Arc<dyn Vnode>, &'static str> {
        Err("Not a directory")
    }
}

/// Checks if a process has the requested access to a vnode.
pub fn check_access(proc: &Process, node: &dyn Vnode, requested: AccessMask) -> bool {
    if proc.euid == 0 {
        return true; // Root bypass
    }

    let stat = node.stat();
    let mut mode = stat.mode;

    // Shift mode bits to align with requested (R=4, W=2, X=1)
    let granted = if proc.euid == stat.uid {
        (mode >> 6) & 7
    } else if proc.egid == stat.gid {
        (mode >> 3) & 7
    } else {
        mode & 7
    };

    (granted & requested) == requested
}

/// The global Mount Table mapping root namespace
pub struct MountTable {
    pub root: Option<Arc<dyn Vnode>>,
}

pub static MOUNT_TABLE: Mutex<MountTable> = Mutex::new(MountTable { root: None });

/// Returns (parent_dir_vnode, filename) for the given path.
pub fn lookup_parent(path: &str) -> Result<(Arc<dyn Vnode>, alloc::string::String), &'static str> {
    let root = {
        let mt = MOUNT_TABLE.lock();
        mt.root.as_ref().ok_or("No root filesystem mounted")?.clone()
    };

    let parts: alloc::vec::Vec<&str> = path.split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();

    if parts.is_empty() {
        return Err("Invalid path");
    }

    let filename = alloc::string::String::from(*parts.last().unwrap());
    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        current = current.lookup(part)?;
    }
    Ok((current, filename))
}

/// Traverses a path from the root mount.
pub fn lookup_path(path: &str) -> Result<Arc<dyn Vnode>, &'static str> {
    let mt = MOUNT_TABLE.lock();
    let root = mt.root.as_ref().ok_or("No root filesystem mounted")?.clone();
    
    if path == "/" {
        return Ok(root);
    }
    
    let mut current = root;
    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        current = current.lookup(component)?;
    }
    
    Ok(current)
}

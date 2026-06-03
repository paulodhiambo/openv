pub mod devfs;
pub mod memfs;
pub mod procfs;
pub mod tar;

use crate::posix::process::Process;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use spin::Mutex;

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

/// Abstract representation of a file or directory.
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

    /// Create a subdirectory with the given name.
    fn mkdir(&self, _name: &str) -> Result<Arc<dyn Vnode>, &'static str> {
        Err("Not a directory")
    }

    /// Remove a child entry (file or directory) by name.
    fn unlink(&self, _name: &str) -> Result<(), &'static str> {
        Err("Not supported")
    }

    /// Rename a child entry within this directory.
    fn rename(&self, _old_name: &str, _new_name: &str) -> Result<(), &'static str> {
        Err("Not supported")
    }
}

/// Check whether a process has the requested access to a vnode.
pub fn check_access(proc: &Process, node: &dyn Vnode, requested: AccessMask) -> bool {
    let euid = proc.euid.load(Ordering::Relaxed);
    if euid == 0 {
        return true; // root bypass
    }

    let stat = node.stat();
    let granted = if euid == stat.uid {
        (stat.mode >> 6) & 7
    } else if proc.egid.load(Ordering::Relaxed) == stat.gid {
        (stat.mode >> 3) & 7
    } else {
        stat.mode & 7
    };

    (granted & requested) == requested
}

/// Global mount table: one root FS plus optional prefix-mount points.
pub struct MountTable {
    pub root: Option<Arc<dyn Vnode>>,
    /// Additional mounts as (absolute_path_prefix, vnode_root).
    /// Checked before falling through to root FS; longest prefix wins.
    pub mounts: alloc::vec::Vec<(alloc::string::String, Arc<dyn Vnode>)>,
}

pub static MOUNT_TABLE: Mutex<MountTable> = Mutex::new(MountTable {
    root: None,
    mounts: alloc::vec::Vec::new(),
});

/// Returns (parent_dir_vnode, filename) for the given path.
pub fn lookup_parent(path: &str) -> Result<(Arc<dyn Vnode>, alloc::string::String), &'static str> {
    let root = {
        let mt = MOUNT_TABLE.lock();
        mt.root
            .as_ref()
            .ok_or("No root filesystem mounted")?
            .clone()
    };

    let parts: alloc::vec::Vec<&str> = path
        .split('/')
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

/// Traverse a path, honouring mount points (longest prefix match first).
pub fn lookup_path(path: &str) -> Result<Arc<dyn Vnode>, &'static str> {
    // Collect mounts and root while holding the lock, then release.
    let (root, mounts) = {
        let mt = MOUNT_TABLE.lock();
        let root = mt
            .root
            .as_ref()
            .ok_or("No root filesystem mounted")?
            .clone();
        let mounts = mt.mounts.clone();
        (root, mounts)
    };

    // Check all mount prefixes; pick the longest that is a prefix of `path`.
    let mut best_prefix_len = 0usize;
    let mut best_vnode: Option<Arc<dyn Vnode>> = None;
    let mut best_remainder = path;

    for (prefix, vnode) in &mounts {
        // Normalise: strip trailing slash from prefix for comparison.
        let pfx = prefix.trim_end_matches('/');
        if path == pfx || (path.starts_with(pfx) && path.as_bytes().get(pfx.len()) == Some(&b'/')) {
            if pfx.len() >= best_prefix_len {
                best_prefix_len = pfx.len();
                let remainder = &path[pfx.len()..];
                best_remainder = remainder;
                best_vnode = Some(vnode.clone());
            }
        }
    }

    let (mut current, start_path) = if let Some(vnode) = best_vnode {
        (vnode, best_remainder)
    } else {
        (root, path)
    };

    if start_path == "/" || start_path.is_empty() {
        return Ok(current);
    }

    for component in start_path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        current = current.lookup(component)?;
    }

    Ok(current)
}

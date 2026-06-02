use crate::vfs::{Vnode, VnodeType, Stat, DirEntry};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cmp::min;
use spin::Mutex;

pub struct MemFile {
    data: Mutex<Vec<u8>>,
    uid:  u32,
    gid:  u32,
    mode: u32,
}

impl MemFile {
    pub fn new(data: Vec<u8>, uid: u32, gid: u32, mode: u32) -> Self {
        Self { data: Mutex::new(data), uid, gid, mode }
    }
}

impl Vnode for MemFile {
    fn stat(&self) -> Stat {
        Stat {
            mode:  self.mode,
            uid:   self.uid,
            gid:   self.gid,
            size:  self.data.lock().len(),
            vtype: VnodeType::File,
        }
    }

    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        let data = self.data.lock();
        if offset >= data.len() {
            return Ok(0);
        }
        let available = data.len() - offset;
        let to_read = min(available, buf.len());
        buf[..to_read].copy_from_slice(&data[offset..offset + to_read]);
        Ok(to_read)
    }

    fn write_at(&self, offset: usize, buf: &[u8]) -> Result<usize, &'static str> {
        let mut data = self.data.lock();
        let end = offset + buf.len();
        if end > data.len() {
            data.resize(end, 0);
        }
        data[offset..end].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn truncate(&self, size: usize) -> Result<(), &'static str> {
        self.data.lock().resize(size, 0);
        Ok(())
    }
}

pub struct MemDir {
    children: Mutex<BTreeMap<String, Arc<dyn Vnode>>>,
    uid:  u32,
    gid:  u32,
    mode: u32,
}

impl MemDir {
    pub fn new(uid: u32, gid: u32, mode: u32) -> Self {
        Self {
            children: Mutex::new(BTreeMap::new()),
            uid,
            gid,
            mode,
        }
    }

    pub fn add_child(&self, name: &str, node: Arc<dyn Vnode>) {
        self.children.lock().insert(name.to_string(), node);
    }
}

impl Vnode for MemDir {
    fn stat(&self) -> Stat {
        Stat {
            mode:  self.mode,
            uid:   self.uid,
            gid:   self.gid,
            size:  0,
            vtype: VnodeType::Directory,
        }
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Vnode>, &'static str> {
        self.children
            .lock()
            .get(name)
            .cloned()
            .ok_or("File not found")
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, &'static str> {
        let children = self.children.lock();
        Ok(children.iter().map(|(name, node)| DirEntry {
            name:  name.clone(),
            vtype: node.stat().vtype,
        }).collect())
    }

    /// Create (or truncate) a regular file in this directory, return the vnode.
    fn create(&self, name: &str) -> Result<Arc<dyn Vnode>, &'static str> {
        let mut children = self.children.lock();
        if let Some(existing) = children.get(name) {
            existing.truncate(0)?;
            Ok(existing.clone())
        } else {
            let node: Arc<dyn Vnode> = Arc::new(MemFile::new(Vec::new(), 0, 0, 0o644));
            children.insert(String::from(name), node.clone());
            Ok(node)
        }
    }
}

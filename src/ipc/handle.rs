use crate::ipc::channel::ChannelEndpoint;
use crate::mm::vmo::Vmo;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

pub type Koid = u64;
pub type Handle = u32;

pub struct FileDescription {
    pub vnode: Arc<dyn crate::vfs::Vnode>,
    pub offset: Mutex<usize>,
}

/// Byte-stream pipe read half.  EOF when all write halves are dropped.
#[derive(Clone)]
pub struct PipeReadHalf {
    pub data: Arc<Mutex<VecDeque<u8>>>,
    pub write_open: Weak<()>, // becomes invalid when all PipeWriteHalves are dropped
}

/// Byte-stream pipe write half.  Dropping the last clone signals EOF to the reader.
#[derive(Clone)]
pub struct PipeWriteHalf {
    pub data: Arc<Mutex<VecDeque<u8>>>,
    _sentinel: Arc<()>,
}

/// Create a matched read/write pipe pair backed by a shared byte queue.
pub fn create_pipe() -> (PipeReadHalf, PipeWriteHalf) {
    let data = Arc::new(Mutex::new(VecDeque::new()));
    let sentinel = Arc::new(());
    let read = PipeReadHalf {
        data: data.clone(),
        write_open: Arc::downgrade(&sentinel),
    };
    let write = PipeWriteHalf {
        data,
        _sentinel: sentinel,
    };
    (read, write)
}

#[derive(Clone)]
pub enum KernelObject {
    Vmo(Arc<Mutex<Vmo>>),
    Channel(Arc<ChannelEndpoint>),
    Console,
    File(Arc<FileDescription>),
    PipeRead(PipeReadHalf),
    PipeWrite(PipeWriteHalf),
}

static NEXT_KOID: AtomicU64 = AtomicU64::new(1);

pub fn generate_koid() -> Koid {
    NEXT_KOID.fetch_add(1, Ordering::SeqCst)
}

pub struct HandleTable {
    map: BTreeMap<Handle, KernelObject>,
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    /// Lowest handle number not currently in the table.
    pub fn lowest_free(&self) -> Handle {
        let mut h: Handle = 0;
        while self.map.contains_key(&h) {
            h += 1;
        }
        h
    }

    /// Insert at the lowest free handle and return the assigned number.
    pub fn insert(&mut self, obj: KernelObject) -> Handle {
        let h = self.lowest_free();
        self.map.insert(h, obj);
        h
    }

    /// Insert at a specific handle, replacing any existing entry.
    pub fn insert_at(&mut self, handle: Handle, obj: KernelObject) {
        self.map.insert(handle, obj);
    }

    pub fn iter(&self) -> alloc::collections::btree_map::Iter<'_, Handle, KernelObject> {
        self.map.iter()
    }

    pub fn get(&self, handle: Handle) -> Option<&KernelObject> {
        self.map.get(&handle)
    }

    pub fn remove(&mut self, handle: Handle) -> Option<KernelObject> {
        self.map.remove(&handle)
    }

    /// Close all handles (POSIX: fds are released on exit, not on reap).
    pub fn close_all(&mut self) {
        self.map.clear();
    }

    /// Duplicate `src` to the lowest available handle.  Returns the new handle.
    pub fn dup(&mut self, src: Handle) -> Option<Handle> {
        let obj = self.map.get(&src)?.clone();
        let dst = self.lowest_free();
        self.map.insert(dst, obj);
        Some(dst)
    }

    /// Duplicate `src` to `dst`, closing `dst` first if open.  Returns false if src not found.
    pub fn dup2(&mut self, src: Handle, dst: Handle) -> bool {
        if let Some(obj) = self.map.get(&src).cloned() {
            self.map.insert(dst, obj);
            true
        } else {
            false
        }
    }
}

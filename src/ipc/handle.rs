use crate::ipc::channel::ChannelEndpoint;
use crate::mm::vmo::Vmo;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use crate::sync::Mutex;

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
    /// PID blocked waiting for data (0 = nobody). Shared with the write half.
    pub waiter: Arc<AtomicI32>,
}

/// Byte-stream pipe write half.  Dropping the last clone signals EOF to the reader.
#[derive(Clone)]
pub struct PipeWriteHalf {
    pub data: Arc<Mutex<VecDeque<u8>>>,
    _sentinel: Arc<()>,
    /// PID blocked on the read half waiting for data (0 = nobody).
    pub waiter: Arc<AtomicI32>,
}

/// Create a matched read/write pipe pair backed by a shared byte queue.
pub fn create_pipe() -> (PipeReadHalf, PipeWriteHalf) {
    let data = Arc::new(Mutex::new(VecDeque::new()));
    let sentinel = Arc::new(());
    let waiter = Arc::new(AtomicI32::new(0));
    let read = PipeReadHalf {
        data: data.clone(),
        write_open: Arc::downgrade(&sentinel),
        waiter: waiter.clone(),
    };
    let write = PipeWriteHalf {
        data,
        _sentinel: sentinel,
        waiter,
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
    /// Handles with FD_CLOEXEC set — closed by close_on_exec() when exec is called.
    cloexec: BTreeSet<Handle>,
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            cloexec: BTreeSet::new(),
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
        self.cloexec.remove(&handle);
        self.map.remove(&handle)
    }

    /// Close all handles (POSIX: fds are released on exit, not on reap).
    pub fn close_all(&mut self) {
        self.map.clear();
        self.cloexec.clear();
    }

    /// Set or clear the FD_CLOEXEC flag on a handle.
    pub fn set_cloexec(&mut self, handle: Handle, val: bool) {
        if val {
            self.cloexec.insert(handle);
        } else {
            self.cloexec.remove(&handle);
        }
    }

    /// Return whether the handle has FD_CLOEXEC set.
    pub fn is_cloexec(&self, handle: Handle) -> bool {
        self.cloexec.contains(&handle)
    }

    /// Expose the cloexec set so fork can copy it.
    pub fn cloexec_handles(&self) -> &BTreeSet<Handle> {
        &self.cloexec
    }

    /// Close all handles with FD_CLOEXEC — called when exec replaces the process image.
    pub fn close_on_exec(&mut self) {
        let to_close: alloc::vec::Vec<Handle> = self.cloexec.iter().cloned().collect();
        for h in to_close {
            self.map.remove(&h);
        }
        self.cloexec.clear();
    }

    /// Duplicate `src` to the lowest available handle. Does NOT copy FD_CLOEXEC (POSIX).
    pub fn dup(&mut self, src: Handle) -> Option<Handle> {
        let obj = self.map.get(&src)?.clone();
        let dst = self.lowest_free();
        self.map.insert(dst, obj);
        Some(dst)
    }

    /// Duplicate `src` to `dst`, closing `dst` first if open.  Returns false if src not found.
    /// Does NOT copy FD_CLOEXEC on `dst` (POSIX: dup2 clears it).
    pub fn dup2(&mut self, src: Handle, dst: Handle) -> bool {
        if let Some(obj) = self.map.get(&src).cloned() {
            self.map.insert(dst, obj);
            self.cloexec.remove(&dst);
            true
        } else {
            false
        }
    }
}

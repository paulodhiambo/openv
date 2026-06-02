use alloc::sync::Arc;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use crate::mm::vmo::Vmo;
use crate::ipc::channel::ChannelEndpoint;

/// A globally unique Kernel Object ID.
pub type Koid = u64;

/// A process-local integer referencing a capability.
pub type Handle = u32;

/// A POSIX File Description tracking the vnode and current offset.
pub struct FileDescription {
    pub vnode: Arc<dyn crate::vfs::Vnode>,
    pub offset: Mutex<usize>,
}

/// A generic abstraction for any capability-backed object in the kernel.
#[derive(Clone)]
pub enum KernelObject {
    Vmo(Arc<Mutex<Vmo>>),
    Channel(Arc<ChannelEndpoint>),
    Console,
    File(Arc<FileDescription>),
}

static NEXT_KOID: AtomicU64 = AtomicU64::new(1);

pub fn generate_koid() -> Koid {
    NEXT_KOID.fetch_add(1, Ordering::SeqCst)
}

/// A per-process mapping of Handles to KernelObjects.
pub struct HandleTable {
    next_handle: Handle,
    map: BTreeMap<Handle, KernelObject>,
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            next_handle: 1,
            map: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, obj: KernelObject) -> Handle {
        let h = self.next_handle;
        self.next_handle += 1;
        self.map.insert(h, obj);
        h
    }
    
    pub fn insert_at(&mut self, handle: Handle, obj: KernelObject) {
        if handle >= self.next_handle {
            self.next_handle = handle + 1;
        }
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
}

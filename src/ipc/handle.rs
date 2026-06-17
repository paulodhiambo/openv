//! # Kernel Object Handles
//!
//! This module provides kernel object handles and the [`HandleTable`]
//! data structure. Handles are integer identifiers that processes use
//! to refer to kernel objects (such as VMOs, channels, and pipes).
//!
//! ## Overview
//!
//! In OpenV, processes do not access kernel objects directly via
//! pointers. Instead, they use integer handles, which are indices into
//! a per-process [`HandleTable`]. This provides:
//!
//! - **Isolation**: A process can only access objects that it has a
//!    valid handle to.
//!  - **Capability passing**: Handles can be transferred over IPC
//!    channels, allowing processes to share access to objects.
//!  - **Reference counting**: When a handle is removed, the kernel
//!    decrements the object's reference count. When the count reaches
//!    zero, the object is destroyed.
//!
//! ## Kernel Objects
//!
//! The [`KernelObject`] enum represents the types of objects that
//! can be referred to by a handle:
//!
//! - **Vmo**: A Virtual Memory Object (see [`crate::mm::vmo::Vmo`]).
//!  - **Channel**: A channel endpoint (see [`ChannelEndpoint`]).
//!  - **Tty**: A per-session TTY line discipline (see [`crate::tty::TtyState`]).
//!  - **PipeRead/PipeWrite**: Half of a byte-stream pipe.
//!  - **VfsFile**: A handle to a VFS file (integer ID).
//!
//! ## Handle Table
//!
//! The [`HandleTable`] is a per-process data structure that maps
//! handle numbers to [`KernelObject`]s. It supports:
//!
//! - Insertion at the lowest free handle or at a specific handle.
//!  - Lookup, removal, and iteration.
//!  - `FD_CLOEXEC` flag for close-on-exec semantics.
//!  - `dup` and `dup2` for duplicating handles.
//!
//! [`ChannelEndpoint`]: ../channel/struct.ChannelEndpoint.html
//! [`crate::mm::vmo::Vmo`]: ../../mm/vmo/struct.Vmo.html
//! [`crate::tty::TtyState`]: ../../tty/struct.TtyState.html
//! [`HandleTable`]: struct.HandleTable.html
//! [`KernelObject`]: enum.KernelObject.html

use crate::ipc::channel::ChannelEndpoint;
use crate::mm::vmo::Vmo;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use crate::sync::Mutex;

/// Kernel object ID. A unique 64-bit identifier for a kernel object.
pub type Koid = u64;
/// Handle number. A 32-bit integer that indexes into a [`HandleTable`].
// Handle is a 32‑bit value: high 16 bits = generation, low 16 bits = handle id.
pub type Handle = u32;

/// Encode a handle id and generation into a `Handle` value.
#[inline]
pub fn encode_handle(id: u16, generation: u16) -> Handle {
    ((generation as u32) << 16) | (id as u32)
}

/// Decode a `Handle` into its id and generation components.
#[inline]
fn decode_handle(handle: Handle) -> (u16, u16) {
    ((handle >> 16) as u16, (handle & 0xFFFF) as u16)
}

/// Byte-stream pipe read half. EOF when all write halves are dropped.
///
/// # Fields
///
/// * `data` - Shared byte queue between the read and write halves.
/// * `write_open` - Weak reference to a sentinel that becomes invalid
///   when all write halves are dropped.
/// * `waiter` - PID blocked waiting for data (0 = nobody). Shared with
///   the write half.
///
/// [`HandleTable`]: struct.HandleTable.html
#[derive(Clone)]
pub struct PipeReadHalf {
    /// Shared byte queue between the read and write halves.
    pub data: Arc<Mutex<VecDeque<u8>>>,
    /// Weak reference to a sentinel that becomes invalid when all write halves are dropped.
    pub write_open: Weak<()>,
    /// PID blocked waiting for data (0 = nobody). Shared with the write half.
    pub waiter: Arc<AtomicI32>,
    /// Epoll instances watching this pipe for readability. Shared with the write half.
    pub epoll_waiters: Arc<Mutex<Vec<Weak<EpollInstance>>>>,
}

/// Byte-stream pipe write half. Dropping the last clone signals EOF to the reader.
///
/// # Fields
///
/// * `data` - Shared byte queue between the read and write halves.
/// * `_sentinel` - Strong reference count; when this drops to zero,
///   all write halves have been dropped.
/// * `waiter` - PID blocked on the read half waiting for data (0 = nobody).
#[derive(Clone)]
pub struct PipeWriteHalf {
    /// Shared byte queue between the read and write halves.
    pub data: Arc<Mutex<VecDeque<u8>>>,
    /// Strong reference count; when this drops to zero, all write halves have been dropped.
    _sentinel: Arc<()>,
    /// PID blocked on the read half waiting for data (0 = nobody).
    pub waiter: Arc<AtomicI32>,
    /// Epoll instances watching the paired read half. Shared with the read half.
    pub epoll_waiters: Arc<Mutex<Vec<Weak<EpollInstance>>>>,
}

impl Drop for PipeWriteHalf {
    /// Handles cleanup when a pipe write half is dropped.
    ///
    /// If this is the last write half (i.e., the sentinel's strong count
    /// is 1 before this drop), wakes any reader blocked on the pipe so
    /// it can observe EOF, and wakes epoll waiters so they observe EPOLLHUP.
    fn drop(&mut self) {
        // Arc::strong_count here is the count *before* this drop decrements it.
        // If it equals 1 we are the last write half; the read side will see EOF.
        // Wake any reader that is currently blocked so it can observe the EOF.
        if Arc::strong_count(&self._sentinel) == 1 {
            let waiter = self.waiter.swap(0, Ordering::Relaxed);
            if waiter > 0 {
                crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
            }
            wake_epoll_waiters(&self.epoll_waiters);
        }
    }
}

/// A single entry in an epoll instance's interest list.
pub struct EpollItem {
    /// Interest list events (EPOLLIN, EPOLLOUT, etc.)
    pub events: u32,
    /// User data from epoll_event.data
    pub data: u64,
    /// Last-observed readiness state (for edge-triggered mode).
    pub last_state: u32,
}

/// An epoll instance for I/O event notification.
pub struct EpollInstance {
    /// Map of watched file descriptor -> interest list entry.
    pub items: Mutex<BTreeMap<i32, EpollItem>>,
    /// PID blocked in epoll_wait (0 = none).
    pub waiter: AtomicI32,
    /// Saved original blocked-signals mask (-1 = not saved).
    pub saved_sigmask: AtomicI32,
}

impl Drop for EpollInstance {
    fn drop(&mut self) {
        let waiter = self.waiter.swap(0, Ordering::Relaxed);
        if waiter > 0 {
            crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
        }
    }
}

/// Wake all epoll waiters registered on a kernel object's watcher list.
/// Removes stale Weak references.
pub fn wake_epoll_waiters(waiters: &Mutex<Vec<alloc::sync::Weak<EpollInstance>>>) {
    let mut list = waiters.lock();
    list.retain(|weak| {
        if let Some(ep) = weak.upgrade() {
            let pid = ep.waiter.swap(0, Ordering::Relaxed);
            if pid > 0 {
                crate::posix::process::RUN_QUEUE.lock().push_back(pid);
            }
            true
        } else {
            false
        }
    });
}

/// Creates a matched read/write pipe pair backed by a shared byte queue.
///
/// # Returns
///
/// A tuple `(PipeReadHalf, PipeWriteHalf)` representing the two ends
/// of the pipe.
pub fn create_pipe() -> (PipeReadHalf, PipeWriteHalf) {
    let data = Arc::new(Mutex::new(VecDeque::new()));
    let sentinel = Arc::new(());
    let waiter = Arc::new(AtomicI32::new(0));
    let epoll_waiters = Arc::new(Mutex::new(Vec::new()));
    let read = PipeReadHalf {
        data: data.clone(),
        write_open: Arc::downgrade(&sentinel),
        waiter: waiter.clone(),
        epoll_waiters: epoll_waiters.clone(),
    };
    let write = PipeWriteHalf {
        data,
        _sentinel: sentinel,
        waiter,
        epoll_waiters,
    };
    (read, write)
}

/// Enumeration of all kernel object types that can be referred to by a handle.
#[derive(Clone)]
pub enum KernelObject {
    /// A Virtual Memory Object (VMO).
    Vmo(Arc<Mutex<Vmo>>),
    /// A channel endpoint.
    Channel(Arc<ChannelEndpoint>),
    /// Per-session TTY line discipline. Multiple fds (stdin/stdout/stderr) in
    /// the same session share one Arc so they all see the same buffer/echo state.
    Tty(alloc::sync::Arc<crate::tty::TtyState>),
    /// The read half of a pipe.
    PipeRead(PipeReadHalf),
    /// The write half of a pipe.
    PipeWrite(PipeWriteHalf),
    /// A handle to a VFS file (integer ID).
    VfsFile(u32),
    /// A port capability for async event waiting.
    Port(Arc<crate::ipc::port::Port>),
    /// A job capability for resource tracking.
    Job(Arc<crate::posix::job::Job>),
    /// An epoll instance for I/O event notification.
    EpollInstance(Arc<EpollInstance>),
}

/// Counter for generating unique kernel object IDs.
static NEXT_KOID: AtomicU64 = AtomicU64::new(1);

/// Generates a new unique kernel object ID.
///
/// # Returns
///
/// A unique `u64` kernel object ID. IDs are allocated sequentially
/// starting from 1.
///
/// # Implementation
///
/// Uses [`AtomicU64::fetch_add`] with [`Ordering::SeqCst`] to ensure
/// that KOID allocation is properly ordered with respect to other
/// memory operations.
///
/// [`AtomicU64::fetch_add`]: https://doc.rust-lang.org/core/sync/atomic/struct.AtomicU64.html#method.fetch_add
/// [`Ordering::SeqCst`]: https://doc.rust-lang.org/core/sync/atomic/enum.Ordering.html#variant.SeqCst
pub fn generate_koid() -> Koid {
    NEXT_KOID.fetch_add(1, Ordering::SeqCst)
}
bitflags::bitflags! {
    /// Kernel object access rights.
    pub struct Rights: u32 {
        /// Right to read/receive data or capabilities from the object.
        const READ          = 1 << 0;
        /// Right to write/send data or capabilities to the object.
        const WRITE         = 1 << 1;
        /// Right to duplicate/clone the handle.
        const DUPLICATE     = 1 << 2;
        /// Right to transfer the handle to another process via a channel.
        const TRANSFER      = 1 << 3;
    }
}

/// An entry in a [`HandleTable`] representing a [`KernelObject`] and its [`Rights`].
#[derive(Clone)]
pub struct HandleEntry {
    /// The backing kernel object capability.
    pub object: KernelObject,
    /// The access rights allowed for this handle.
    pub rights: Rights,
    /// Generation counter for use‑after‑close protection.
    pub generation: u16,
}
pub struct HandleTable {
    // Map from raw handle id (u16) to entry.
    map: BTreeMap<u16, HandleEntry>,
    /// Handles with `FD_CLOEXEC` set — closed by `close_on_exec()` when exec is called.
    cloexec: BTreeSet<Handle>,
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}

impl HandleTable {
    /// Creates a new empty [`HandleTable`].
    ///
    /// # Returns
    ///
    /// A new [`HandleTable`] with no handles.
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            cloexec: BTreeSet::new(),
        }
    }

    /// Returns the lowest handle number not currently in the table.
    ///
    /// # Returns
    ///
    /// The lowest free handle number.
    ///
    /// # Panics
    ///
    /// Panics if all 65536 handle slots are occupied.
    pub fn lowest_free(&self) -> u16 {
        (0..=u16::MAX)
            .find(|id| !self.map.contains_key(id))
            .expect("handle table full (65536 handles exhausted)")
    }

    /// Inserts a kernel object at the lowest free handle with all rights.
    ///
    /// # Arguments
    ///
    /// * `obj` - The kernel object to insert.
    ///
    /// # Returns
    ///
    /// The assigned handle number.
    pub fn insert(&mut self, obj: KernelObject) -> Handle {
        let id = self.lowest_free();
        // Increment generation for this id (or start at 1).
        let generation = match self.map.get(&id) {
            Some(entry) => entry.generation.wrapping_add(1),
            None => 1,
        };
        let entry = HandleEntry { object: obj, rights: Rights::all(), generation };
        self.map.insert(id, entry);
        encode_handle(id, generation)
    }

    /// Inserts a kernel object with specific rights at the lowest free handle.
    pub fn insert_with_rights(&mut self, obj: KernelObject, rights: Rights) -> Handle {
        let id = self.lowest_free();
        let generation = match self.map.get(&id) {
            Some(entry) => entry.generation.wrapping_add(1),
            None => 1,
        };
        let entry = HandleEntry { object: obj, rights, generation };
        self.map.insert(id, entry);
        encode_handle(id, generation)
    }

    /// Inserts a kernel object at a specific handle with all rights.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle number to use.
    /// * `obj` - The kernel object to insert.
    ///
    /// Note: does NOT set FD_CLOEXEC. Call `set_cloexec` explicitly if needed.
    pub fn insert_at(&mut self, handle: Handle, obj: KernelObject) {
        let (id, generation) = decode_handle(handle);
        let entry = HandleEntry { object: obj, rights: Rights::all(), generation };
        self.map.insert(id, entry);
    }

    /// Inserts a specific handle entry at a specific handle.
    ///
    /// Note: does NOT set FD_CLOEXEC. Call `set_cloexec` explicitly if needed.
    pub fn insert_entry(&mut self, handle: Handle, entry: HandleEntry) {
        let (id, _) = decode_handle(handle);
        self.map.insert(id, entry);
    }

    /// Returns an iterator over the handle table.
    ///
    /// # Returns
    ///
    /// An iterator yielding `(handle, handle_entry)` pairs.
    pub fn iter(&self) -> alloc::collections::btree_map::Iter<'_, u16, HandleEntry> {
        self.map.iter()
    }

    /// Returns a reference to the kernel object for the given handle.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle number to look up.
    ///
    /// # Returns
    ///
    /// `Some(&KernelObject)` if the handle exists, `None` otherwise.
    pub fn get(&self, handle: Handle) -> Option<&KernelObject> {
        let (id, generation) = decode_handle(handle);
        self.map.get(&id).filter(|e| e.generation == generation).map(|entry| &entry.object)
    }

    /// Returns a reference to the handle entry for the given handle.
    pub fn get_entry(&self, handle: Handle) -> Option<&HandleEntry> {
        let (id, generation) = decode_handle(handle);
        self.map.get(&id).filter(|e| e.generation == generation)
    }

    /// Returns a reference to the kernel object only if it possesses the required rights.
    pub fn get_with_rights(&self, handle: Handle, required: Rights) -> Result<&KernelObject, usize> {
        let (id, generation) = decode_handle(handle);
        let entry = self.map.get(&id).filter(|e| e.generation == generation).ok_or(crate::errno::EBADF)?;
        if !entry.rights.contains(required) {
            return Err(crate::errno::EACCES);
        }
        Ok(&entry.object)
    }

    /// Removes a handle from the table.
    ///
    /// Returns `None` if the handle does not exist or the generation does not match,
    /// preventing stale handles from closing a recycled object at the same slot.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle number to remove.
    ///
    /// # Returns
    ///
    /// `Some(KernelObject)` if the handle existed and the generation matched, `None` otherwise.
    pub fn remove(&mut self, handle: Handle) -> Option<KernelObject> {
        self.cloexec.remove(&handle);
        let (id, generation) = decode_handle(handle);
        if self.map.get(&id).map_or(false, |e| e.generation == generation) {
            self.map.remove(&id).map(|entry| entry.object)
        } else {
            None
        }
    }

    /// Removes a handle entry from the table.
    ///
    /// Returns `None` if the handle does not exist or the generation does not match.
    pub fn remove_entry(&mut self, handle: Handle) -> Option<HandleEntry> {
        self.cloexec.remove(&handle);
        let (id, generation) = decode_handle(handle);
        if self.map.get(&id).map_or(false, |e| e.generation == generation) {
            self.map.remove(&id)
        } else {
            None
        }
    }

    /// Closes all handles (POSIX: fds are released on exit, not on reap).
    pub fn close_all(&mut self) {
        self.map.clear();
        self.cloexec.clear();
    }

    /// Sets or clears the `FD_CLOEXEC` flag on a handle.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle number.
    /// * `val` - `true` to set `FD_CLOEXEC`, `false` to clear it.
    pub fn set_cloexec(&mut self, handle: Handle, val: bool) {
        if val {
            self.cloexec.insert(handle);
        } else {
            self.cloexec.remove(&handle);
        }
    }

    /// Returns whether the handle has `FD_CLOEXEC` set.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle number.
    ///
    /// # Returns
    ///
    /// `true` if `FD_CLOEXEC` is set, `false` otherwise.
    pub fn is_cloexec(&self, handle: Handle) -> bool {
        self.cloexec.contains(&handle)
    }

    /// Exposes the cloexec set so fork can copy it.
    ///
    /// # Returns
    ///
    /// A reference to the set of handles with `FD_CLOEXEC` set.
    pub fn cloexec_handles(&self) -> &BTreeSet<Handle> {
        &self.cloexec
    }

    /// Closes all handles with `FD_CLOEXEC` — called when exec replaces
    /// the process image.
    ///
    /// Validates the generation field so that a stale cloexec entry for a
    /// recycled slot does not close an unrelated object at the same id.
    pub fn close_on_exec(&mut self) {
        let to_close: alloc::vec::Vec<Handle> = self.cloexec.iter().cloned().collect();
        for h in to_close {
            let (id, generation) = decode_handle(h);
            if self.map.get(&id).map_or(false, |e| e.generation == generation) {
                self.map.remove(&id);
            }
        }
        self.cloexec.clear();
    }

    /// Duplicates `src` to the lowest available handle. Does NOT copy
    /// `FD_CLOEXEC` (POSIX).
    ///
    /// # Arguments
    ///
    /// * `src` - The source handle to duplicate.
    ///
    /// # Returns
    ///
    /// `Some(new_handle)` on success, `None` if `src` does not exist.
    pub fn dup(&mut self, src: Handle) -> Option<Handle> {
        let entry = self.get_entry(src)?.clone();
        let new_handle = self.insert(entry.object);
        Some(new_handle)
    }

    /// Duplicates `src` to `dst`, closing `dst` first if open. Returns
    /// `false` if `src` is not found. Does NOT copy `FD_CLOEXEC` on
    /// `dst` (POSIX: `dup2` clears it).
    ///
    /// # Arguments
    ///
    /// * `src` - The source handle to duplicate.
    /// * `dst` - The destination handle.
    ///
    /// # Returns
    ///
    /// `true` on success, `false` if `src` is not found.
    pub fn dup2(&mut self, src: Handle, dst: Handle) -> bool {
        if let Some(entry) = self.get_entry(src).cloned() {
            let (dst_id, _) = decode_handle(dst);
            self.map.insert(dst_id, entry);
            self.cloexec.remove(&dst); // POSIX: dup2 always clears FD_CLOEXEC on dst
            true
        } else {
            false
        }
    }
}

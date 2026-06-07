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
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use crate::sync::Mutex;

/// Kernel object ID. A unique 64-bit identifier for a kernel object.
pub type Koid = u64;
/// Handle number. A 32-bit integer that indexes into a [`HandleTable`].
pub type Handle = u32;

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
}

impl Drop for PipeWriteHalf {
    /// Handles cleanup when a pipe write half is dropped.
    ///
    /// If this is the last write half (i.e., the sentinel's strong count
    /// is 1 before this drop), wakes any reader blocked on the pipe so
    /// it can observe EOF.
    fn drop(&mut self) {
        // Arc::strong_count here is the count *before* this drop decrements it.
        // If it equals 1 we are the last write half; the read side will see EOF.
        // Wake any reader that is currently blocked so it can observe the EOF.
        if Arc::strong_count(&self._sentinel) == 1 {
            let waiter = self.waiter.swap(0, Ordering::Relaxed);
            if waiter > 0 {
                crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
            }
        }
    }
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

/// A per-process table of kernel object handles.
///
/// The [`HandleTable`] maps handle numbers to [`KernelObject`]s. It
/// supports insertion, lookup, removal, and various POSIX-like
/// operations (dup, dup2, close-on-exec).
///
/// # Fields
///
/// * `map` - Map from handle number to kernel object.
/// * `cloexec` - Set of handles with `FD_CLOEXEC` set.
pub struct HandleTable {
    /// Map from handle number to kernel object.
    map: BTreeMap<Handle, KernelObject>,
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
    pub fn lowest_free(&self) -> Handle {
        let mut h: Handle = 0;
        while self.map.contains_key(&h) {
            h += 1;
        }
        h
    }

    /// Inserts a kernel object at the lowest free handle and returns the assigned number.
    ///
    /// # Arguments
    ///
    /// * `obj` - The kernel object to insert.
    ///
    /// # Returns
    ///
    /// The assigned handle number.
    pub fn insert(&mut self, obj: KernelObject) -> Handle {
        let h = self.lowest_free();
        self.map.insert(h, obj);
        h
    }

    /// Inserts a kernel object at a specific handle, replacing any existing entry.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle number to use.
///  * `obj` - The kernel object to insert.
    pub fn insert_at(&mut self, handle: Handle, obj: KernelObject) {
        self.map.insert(handle, obj);
    }

    /// Returns an iterator over the handle table.
    ///
    /// # Returns
    ///
    /// An iterator yielding `(handle, kernel_object)` pairs.
    pub fn iter(&self) -> alloc::collections::btree_map::Iter<'_, Handle, KernelObject> {
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
        self.map.get(&handle)
    }

    /// Removes a handle from the table.
    ///
    /// # Arguments
    ///
    /// * `handle` - The handle number to remove.
    ///
    /// # Returns
    ///
    /// `Some(KernelObject)` if the handle existed, `None` otherwise.
    pub fn remove(&mut self, handle: Handle) -> Option<KernelObject> {
        self.cloexec.remove(&handle);
        self.map.remove(&handle)
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
///  * `val` - `true` to set `FD_CLOEXEC`, `false` to clear it.
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
    pub fn close_on_exec(&mut self) {
        let to_close: alloc::vec::Vec<Handle> = self.cloexec.iter().cloned().collect();
        for h in to_close {
            self.map.remove(&h);
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
        let obj = self.map.get(&src)?.clone();
        let dst = self.lowest_free();
        self.map.insert(dst, obj);
        Some(dst)
    }

    /// Duplicates `src` to `dst`, closing `dst` first if open. Returns
    /// `false` if `src` is not found. Does NOT copy `FD_CLOEXEC` on
    /// `dst` (POSIX: `dup2` clears it).
    ///
    /// # Arguments
    ///
    /// * `src` - The source handle to duplicate.
///  * `dst` - The destination handle.
    ///
    /// # Returns
    ///
    /// `true` on success, `false` if `src` does not exist.
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

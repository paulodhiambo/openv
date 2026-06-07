//! # Kernel Namespace Infrastructure
//!
//! This module provides kernel namespace infrastructure for OpenV, including
//! support for mount namespaces, PID namespaces, and network namespaces.
//! Namespaces are a fundamental isolation primitive in modern operating
//! systems, allowing processes to have independent views of system resources.
//!
//! ## Overview
//!
//! OpenV's namespace infrastructure is inspired by Linux's namespace support,
//! but is simplified for a microkernel architecture. Each [`Process`]
//! holds [`Arc`]s to its current namespaces. When `CLONE_NEW*` is passed
//! to `sys_clone` or `sys_unshare`, a fresh namespace is created so the
//! child has an isolated view of that resource.
//!
//! ## Namespace Types
//!
//! OpenV supports three types of namespaces:
//!
//! - **[`MountNs`]**: Isolates the filesystem tree visible to a process group.
//!   Currently a tagged placeholder; the VFS server handles actual mounts.
//!   The ID is forwarded to the VFS server so it can implement per-namespace
//!   mount tables in a future iteration.
//!
//! - **[`PidNs`]**: Gives processes a local PID space. The kernel always
//!   tracks the global (host) PID; the local PID is an alias visible to
//!   processes within the namespace.
//!
//! - **[`NetNs`]**: Isolates sockets, interfaces, and routing tables.
//!   Currently a tagged placeholder; the net-smoltcp server will gain
//!   per-namespace state in a future iteration.
//!
//! ## Usage
//!
//! The root namespaces are created during kernel boot via [`init`]. Processes
//! inherit namespaces from their parent by default, but can create new
//! namespaces via `CLONE_NEWNS`, `CLONE_NEWPID`, or `CLONE_NEWNET` flags
//! passed to `sys_clone` or `sys_unshare`.
//!
//! [`Process`]: ../posix/process/struct.Process.html
//! [`Arc`]: https://doc.rust-lang.org/alloc/sync/struct.Arc.html

use alloc::sync::Arc;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

// ── Namespace ID allocator ────────────────────────────────────────────────────

/// Global counter for allocating unique namespace IDs.
///
/// This atomic counter is incremented each time a new namespace is created.
/// IDs start at 1 (0 is reserved for "no namespace").
static NEXT_NS_ID: AtomicU32 = AtomicU32::new(1);

/// Allocates a new unique namespace ID.
///
/// # Returns
///
/// A unique `u32` namespace ID. IDs are allocated sequentially starting from 1.
///
/// # Implementation
///
/// Uses [`AtomicU32::fetch_add`] with [`Ordering::Relaxed`] because namespace
/// IDs are only used as unique identifiers and do not need to be ordered
/// with respect to other memory operations.
///
/// [`AtomicU32::fetch_add`]: https://doc.rust-lang.org/core/sync/atomic/struct.AtomicU32.html#method.fetch_add
/// [`Ordering::Relaxed`]: https://doc.rust-lang.org/core/sync/atomic/enum.Ordering.html#variant.Relaxed
fn alloc_ns_id() -> u32 {
    NEXT_NS_ID.fetch_add(1, Ordering::Relaxed)
}

// ── Mount namespace ───────────────────────────────────────────────────────────

/// A mount namespace isolates the filesystem tree visible to a process group.
///
/// Currently this is a tagged placeholder; the VFS server handles actual
/// mounts. The ID is forwarded to the VFS server so it can implement
/// per-namespace mount tables in a future iteration.
///
/// # Fields
///
/// * `id` - A unique namespace ID allocated by [`alloc_ns_id`].
pub struct MountNs {
    /// Unique namespace ID.
    pub id: u32,
}

impl MountNs {
    /// Creates a new mount namespace with a unique ID.
    fn new() -> Arc<Self> {
        Arc::new(Self { id: alloc_ns_id() })
    }

    /// Forks this namespace (copy-on-write — currently a fresh empty namespace).
    ///
    /// In a full implementation, this would create a copy-on-write clone of
    /// the mount table. Currently, it creates a fresh empty namespace.
    ///
    /// # Returns
    ///
    /// An [`Arc`] to the new mount namespace.
    ///
    /// [`Arc`]: https://doc.rust-lang.org/alloc/sync/struct.Arc.html
    pub fn fork(&self) -> Arc<Self> {
        Self::new()
    }
}

// ── PID namespace ─────────────────────────────────────────────────────────────

/// A PID namespace gives processes a local PID space.
///
/// The kernel always tracks the global (host) PID; the local PID is an alias
/// visible to processes within the namespace. This allows processes in
/// different PID namespaces to have the same local PID without conflict.
///
/// # Fields
///
/// * `id` - A unique namespace ID allocated by [`alloc_ns_id`].
/// * `next_pid` - Atomic counter for allocating local PIDs in this namespace.
/// * `parent` - The parent PID namespace, or `None` for the root namespace.
pub struct PidNs {
    /// Unique namespace ID.
    pub id:       u32,
    /// Counter for allocating local PIDs in this namespace.
    pub next_pid: AtomicI32,
    /// Parent PID namespace, or `None` for the root namespace.
    pub parent:   Option<Arc<PidNs>>,
}

impl PidNs {
    /// Creates a new PID namespace with a unique ID.
    ///
    /// # Arguments
    ///
    /// * `parent` - The parent PID namespace, or `None` for the root namespace.
    fn new(parent: Option<Arc<PidNs>>) -> Arc<Self> {
        Arc::new(Self {
            id:       alloc_ns_id(),
            next_pid: AtomicI32::new(1),
            parent,
        })
    }

    /// Allocates the next local PID within this namespace.
    ///
    /// # Returns
    ///
    /// A new local PID. PIDs start at 1 (0 is reserved for the idle process
    /// or "no process").
    ///
    /// # Implementation
    ///
    /// Uses [`AtomicI32::fetch_add`] with [`Ordering::SeqCst`] to ensure
    /// that PID allocation is properly ordered with respect to other
    /// memory operations. This is important because PIDs are used as
    /// unique identifiers and must not be reused while still in use.
    ///
    /// [`AtomicI32::fetch_add`]: https://doc.rust-lang.org/core/sync/atomic/struct.AtomicI32.html#method.fetch_add
    /// [`Ordering::SeqCst`]: https://doc.rust-lang.org/core/sync/atomic/enum.Ordering.html#variant.SeqCst
    pub fn alloc_local_pid(&self) -> i32 {
        self.next_pid.fetch_add(1, Ordering::SeqCst)
    }

    /// Forks this namespace (child becomes root of a new namespace).
    ///
    /// The new namespace has this namespace as its parent, forming a
    /// hierarchical tree of PID namespaces.
    ///
    /// # Returns
    ///
    /// An [`Arc`] to the new PID namespace.
    ///
    /// [`Arc`]: https://doc.rust-lang.org/alloc/sync/struct.Arc.html
    pub fn fork(self: &Arc<Self>) -> Arc<Self> {
        Self::new(Some(self.clone()))
    }
}

// ── Network namespace ─────────────────────────────────────────────────────────

/// A network namespace isolates sockets, interfaces, and routing tables.
///
/// Currently a tagged placeholder; the net-smoltcp server will gain
/// per-namespace state in a future iteration.
///
/// # Fields
///
/// * `id` - A unique namespace ID allocated by [`alloc_ns_id`].
pub struct NetNs {
    /// Unique namespace ID.
    pub id: u32,
}

impl NetNs {
    /// Creates a new network namespace with a unique ID.
    fn new() -> Arc<Self> {
        Arc::new(Self { id: alloc_ns_id() })
    }

    /// Forks this namespace (currently a fresh empty namespace).
    ///
    /// In a full implementation, this would create a copy-on-write clone of
    /// the network state. Currently, it creates a fresh empty namespace.
    ///
    /// # Returns
    ///
    /// An [`Arc`] to the new network namespace.
    ///
    /// [`Arc`]: https://doc.rust-lang.org/alloc/sync/struct.Arc.html
    pub fn fork(&self) -> Arc<Self> {
        Self::new()
    }
}

// ── Root namespaces (process 1 starts here) ───────────────────────────────────

/// Storage for the root mount namespace.
///
/// This is initialized once during kernel boot via [`init`] and never
/// modified thereafter. It is wrapped in a [`Mutex`] to allow safe access
/// from any context.
static ROOT_MNT_NS:  crate::sync::Mutex<Option<Arc<MountNs>>> = crate::sync::Mutex::new(None);
/// Storage for the root PID namespace.
///
/// This is initialized once during kernel boot via [`init`] and never
/// modified thereafter. It is wrapped in a [`Mutex`] to allow safe access
/// from any context.
static ROOT_PID_NS:  crate::sync::Mutex<Option<Arc<PidNs>>>   = crate::sync::Mutex::new(None);
/// Storage for the root network namespace.
///
/// This is initialized once during kernel boot via [`init`] and never
/// modified thereafter. It is wrapped in a [`Mutex`] to allow safe access
/// from any context.
static ROOT_NET_NS:  crate::sync::Mutex<Option<Arc<NetNs>>>   = crate::sync::Mutex::new(None);

/// Called once at boot to create the root namespaces.
///
/// This function initializes the root mount, PID, and network namespaces.
/// It should be called exactly once during kernel boot, typically from
/// [`kmain`]. Subsequent calls are no-ops (the first call wins).
///
/// [`kmain`]: ../fn.kmain.html
pub fn init() {
    *ROOT_MNT_NS.lock() = Some(MountNs::new());
    *ROOT_PID_NS.lock() = Some(PidNs::new(None));
    *ROOT_NET_NS.lock() = Some(NetNs::new());
}

/// Returns the root mount namespace.
///
/// # Panics
///
/// Panics if [`init`] has not been called yet.
///
/// [`init`]: fn.init.html
pub fn root_mnt() -> Arc<MountNs> { ROOT_MNT_NS.lock().clone().unwrap() }
/// Returns the root PID namespace.
///
/// # Panics
///
/// Panics if [`init`] has not been called yet.
///
/// [`init`]: fn.init.html
pub fn root_pid() -> Arc<PidNs>   { ROOT_PID_NS.lock().clone().unwrap() }
/// Returns the root network namespace.
///
/// # Panics
///
/// Panics if [`init`] has not been called yet.
///
/// [`init`]: fn.init.html
pub fn root_net() -> Arc<NetNs>   { ROOT_NET_NS.lock().clone().unwrap() }

// ── Namespace bundle ──────────────────────────────────────────────────────────

/// All namespaces a process belongs to, grouped for easy cloning.
///
/// This struct bundles together the three namespace types that a process
/// belongs to. It is used to pass namespaces around as a single unit and
/// to clone them when forking.
///
/// # Fields
///
/// * `mnt` - The mount namespace.
/// * `pid` - The PID namespace.
/// * `net` - The network namespace.
pub struct NsSet {
    /// Mount namespace.
    pub mnt: Arc<MountNs>,
    /// PID namespace.
    pub pid: Arc<PidNs>,
    /// Network namespace.
    pub net: Arc<NetNs>,
}

impl NsSet {
    /// Creates a new `NsSet` referring to the root namespaces.
    ///
    /// # Returns
    ///
    /// A new `NsSet` with all three namespaces set to the root namespaces.
    ///
    /// # Panics
    ///
    /// Panics if [`init`] has not been called yet.
    ///
    /// [`init`]: fn.init.html
    pub fn root() -> Self {
        Self { mnt: root_mnt(), pid: root_pid(), net: root_net() }
    }

    /// Inherits namespaces from a parent, optionally replacing some.
    ///
    /// # Arguments
    ///
    /// * `parent` - The parent's namespace set to inherit from.
    /// * `clone_flags` - Flags controlling which namespaces to replace.
    ///   Uses `CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWNET`:
    ///   - `CLONE_NEWNS` (0x0002_0000): Create a new mount namespace.
    ///   - `CLONE_NEWPID` (0x2000_0000): Create a new PID namespace.
    ///   - `CLONE_NEWNET` (0x4000_0000): Create a new network namespace.
    ///
    /// # Returns
    ///
    /// A new `NsSet` with the appropriate namespaces inherited or forked.
    pub fn fork_from(parent: &NsSet, clone_flags: u32) -> Self {
        /// Create a new mount namespace.
        const CLONE_NEWNS:  u32 = 0x0002_0000;
        /// Create a new PID namespace.
        const CLONE_NEWPID: u32 = 0x2000_0000;
        /// Create a new network namespace.
        const CLONE_NEWNET: u32 = 0x4000_0000;
        Self {
            mnt: if clone_flags & CLONE_NEWNS  != 0 { parent.mnt.fork() } else { parent.mnt.clone() },
            pid: if clone_flags & CLONE_NEWPID != 0 { parent.pid.fork() } else { parent.pid.clone() },
            net: if clone_flags & CLONE_NEWNET != 0 { parent.net.fork() } else { parent.net.clone() },
        }
    }
}

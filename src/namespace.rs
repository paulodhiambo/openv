/// Kernel namespace infrastructure (mount, PID, network).
///
/// Each Process holds Arcs to its current namespaces.  When CLONE_NEW* is
/// passed to sys_clone or sys_unshare, a fresh namespace is created so the
/// child has an isolated view of that resource.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

// ── Namespace ID allocator ────────────────────────────────────────────────────

static NEXT_NS_ID: AtomicU32 = AtomicU32::new(1);

fn alloc_ns_id() -> u32 {
    NEXT_NS_ID.fetch_add(1, Ordering::Relaxed)
}

// ── Mount namespace ───────────────────────────────────────────────────────────

/// A mount namespace isolates the filesystem tree visible to a process group.
/// Currently this is a tagged placeholder; the VFS server handles actual
/// mounts.  The ID is forwarded to the VFS server so it can implement per-
/// namespace mount tables in a future iteration.
pub struct MountNs {
    pub id: u32,
}

impl MountNs {
    fn new() -> Arc<Self> {
        Arc::new(Self { id: alloc_ns_id() })
    }

    /// Fork this namespace (copy-on-write — currently a fresh empty namespace).
    pub fn fork(&self) -> Arc<Self> {
        Self::new()
    }
}

// ── PID namespace ─────────────────────────────────────────────────────────────

/// A PID namespace gives processes a local PID space.
/// The kernel always tracks the global (host) PID; the local PID is an alias
/// visible to processes within the namespace.
pub struct PidNs {
    pub id:       u32,
    pub next_pid: AtomicI32,
    pub parent:   Option<Arc<PidNs>>,
}

impl PidNs {
    fn new(parent: Option<Arc<PidNs>>) -> Arc<Self> {
        Arc::new(Self {
            id:       alloc_ns_id(),
            next_pid: AtomicI32::new(1),
            parent,
        })
    }

    /// Allocate the next local PID within this namespace.
    pub fn alloc_local_pid(&self) -> i32 {
        self.next_pid.fetch_add(1, Ordering::SeqCst)
    }

    /// Fork this namespace (child becomes root of a new namespace).
    pub fn fork(self: &Arc<Self>) -> Arc<Self> {
        Self::new(Some(self.clone()))
    }
}

// ── Network namespace ─────────────────────────────────────────────────────────

/// A network namespace isolates sockets, interfaces, and routing tables.
/// Currently a tagged placeholder; the net-smoltcp server will gain per-
/// namespace state in a future iteration.
pub struct NetNs {
    pub id: u32,
}

impl NetNs {
    fn new() -> Arc<Self> {
        Arc::new(Self { id: alloc_ns_id() })
    }

    pub fn fork(&self) -> Arc<Self> {
        Self::new()
    }
}

// ── Root namespaces (process 1 starts here) ───────────────────────────────────

static ROOT_MNT_NS:  crate::sync::Mutex<Option<Arc<MountNs>>> = crate::sync::Mutex::new(None);
static ROOT_PID_NS:  crate::sync::Mutex<Option<Arc<PidNs>>>   = crate::sync::Mutex::new(None);
static ROOT_NET_NS:  crate::sync::Mutex<Option<Arc<NetNs>>>   = crate::sync::Mutex::new(None);

/// Called once at boot to create the root namespaces.
pub fn init() {
    *ROOT_MNT_NS.lock() = Some(MountNs::new());
    *ROOT_PID_NS.lock() = Some(PidNs::new(None));
    *ROOT_NET_NS.lock() = Some(NetNs::new());
}

pub fn root_mnt() -> Arc<MountNs> { ROOT_MNT_NS.lock().clone().unwrap() }
pub fn root_pid() -> Arc<PidNs>   { ROOT_PID_NS.lock().clone().unwrap() }
pub fn root_net() -> Arc<NetNs>   { ROOT_NET_NS.lock().clone().unwrap() }

// ── Namespace bundle ──────────────────────────────────────────────────────────

/// All namespaces a process belongs to, grouped for easy cloning.
pub struct NsSet {
    pub mnt: Arc<MountNs>,
    pub pid: Arc<PidNs>,
    pub net: Arc<NetNs>,
}

impl NsSet {
    pub fn root() -> Self {
        Self { mnt: root_mnt(), pid: root_pid(), net: root_net() }
    }

    /// Inherit namespaces from a parent, optionally replacing some.
    /// `clone_flags` uses CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWNET.
    pub fn fork_from(parent: &NsSet, clone_flags: u32) -> Self {
        const CLONE_NEWNS:  u32 = 0x0002_0000;
        const CLONE_NEWPID: u32 = 0x2000_0000;
        const CLONE_NEWNET: u32 = 0x4000_0000;
        Self {
            mnt: if clone_flags & CLONE_NEWNS  != 0 { parent.mnt.fork() } else { parent.mnt.clone() },
            pid: if clone_flags & CLONE_NEWPID != 0 { parent.pid.fork() } else { parent.pid.clone() },
            net: if clone_flags & CLONE_NEWNET != 0 { parent.net.fork() } else { parent.net.clone() },
        }
    }
}

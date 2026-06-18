//! # Process Management
//!
//! This module provides the core process management for OpenV. A
//! [`Process`] represents a single thread of execution with its own
//! page table, file descriptor table, and other resources.
//!
//! ## Overview
//!
//! OpenV's process model is inspired by Unix:
//!
//! - Each process has a unique PID (process ID).
//! - Processes are organized into a parent-child hierarchy.
//!  - Processes have credentials (UID, GID, EUID, EGID) and capabilities.
//!  - Processes can send and receive IPC messages.
//!  - Processes can be in various states ([`ProcState`]).
//!
//! ## Process States
//!
//! A process can be in one of the following states:
//! - [`ProcState::Running`]: Currently scheduled on a HART.
//!  - [`ProcState::Stopped`]: Suspended (e.g., waiting for an event).
//!  - [`ProcState::Zombie`]: Terminated, waiting to be reaped by the parent.
//!
//! ## IPC State
//!
//! A process can be in one of the following IPC states ([`IpcState`]):
//! - [`IpcState::None`]: Not currently in an IPC operation.
//!  - [`IpcState::Sending`]: Sending a message.
//!  - [`IpcState::Receiving`]: Waiting for a message.
//!  - [`IpcState::ReceivingReply`]: Waiting for a reply to a sent message.
//!  - [`IpcState::MessageAvailable`]: A message is available.
//!  - [`IpcState::SendComplete`]: A send has completed.
//!
//! ## Capabilities
//!
//! Processes have a set of capabilities that determine what operations
//! they can perform. The following capabilities are defined:
//! - [`CAP_NONE`]: No capabilities.
//!  - [`CAP_MMIO`]: Access MMIO regions.
//!  - [`CAP_DATACOPY`]: Copy data between user and kernel.
//!  - [`CAP_NET_RAW`]: Use raw network sockets.
//!  - [`CAP_PROCESS`]: Manage other processes.
//!  - [`CAP_INTERRUPT`]: Register interrupt handlers.
//!  - [`CAP_SYS_ADMIN`]: Perform system administration tasks.
//!
//! ## Kernel Stack Overflow Detection
//!
//! Each process has a kernel stack with a canary value at the bottom.
//! The scheduler checks the canary before resuming a process; if it
//! has been overwritten, the kernel panics with a "kernel stack
//! overflow" error.

use crate::ipc::handle::HandleTable;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use crate::sync::Mutex;

/// Reference counts for page tables shared across threads (`CLONE_VM`).
///
/// Entries only exist when refcount > 1; absence means sole owner.
pub static SATP_REFCOUNT: Mutex<BTreeMap<usize, usize>> = Mutex::new(BTreeMap::new());

/// Futex wait table: maps (user virtual address) → list of sleeping PIDs.
pub static FUTEX_TABLE: Mutex<BTreeMap<usize, VecDeque<Pid>>> = Mutex::new(BTreeMap::new());

/// The root Job container for the system.
pub static ROOT_JOB: Mutex<Option<Arc<crate::posix::job::Job>>> = Mutex::new(None);

/// Returns the root Job container, allocating it if not already initialized.
pub fn get_root_job() -> Arc<crate::posix::job::Job> {
    let mut root = ROOT_JOB.lock();
    if root.is_none() {
        *root = Some(Arc::new(crate::posix::job::Job::new(None)));
    }
    root.as_ref().unwrap().clone()
}

/// Marks `root_pa` as shared by one additional thread.
///
/// # Arguments
///
/// * `root_pa` - The physical address of the shared page table root.
pub fn satp_share(root_pa: usize) {
    let mut map = SATP_REFCOUNT.lock();
    let count = map.entry(root_pa).or_insert(1);
    *count += 1;
}

/// Called when a process/thread releases `root_pa`.
///
/// # Arguments
///
/// * `root_pa` - The physical address of the page table root.
///
/// # Returns
///
/// `true` if the caller should free the page table (last owner),
/// `false` otherwise.
pub fn satp_unshare(root_pa: usize) -> bool {
    let mut map = SATP_REFCOUNT.lock();
    if let Some(count) = map.get_mut(&root_pa) {
        *count -= 1;
        let remaining = *count;
        if remaining == 0 {
            map.remove(&root_pa);
            true  // last tracked owner — caller must free the page table
        } else {
            false // other owners remain
        }
    } else {
        // Not in the shared map — this process is the sole (unregistered) owner.
        true
    }
}

/// Process ID type.
pub type Pid = i32;
/// Process group ID type.
pub type Pgid = i32;
/// Session ID type.
pub type Sid = i32;

/// No capabilities.
pub const CAP_NONE: u64      = 0;
/// Access MMIO regions.
pub const CAP_MMIO: u64      = 1 << 0;
/// Copy data between user and kernel.
pub const CAP_DATACOPY: u64  = 1 << 1;
/// Use raw network sockets.
pub const CAP_NET_RAW: u64   = 1 << 2;
/// Manage other processes.
pub const CAP_PROCESS: u64   = 1 << 3;
/// Register interrupt handlers.
pub const CAP_INTERRUPT: u64 = 1 << 4;
/// Perform system administration tasks.
pub const CAP_SYS_ADMIN: u64 = 1 << 5;
/// Driver access: MMIO mapping and IRQ registration.
pub const CAP_DRIVER: u64    = 1 << 6;
/// Configure network interfaces (IP, routes, etc.).
pub const CAP_NET_ADMIN: u64 = 1 << 7;
/// Mount/unmount filesystems.
pub const CAP_MOUNT: u64     = 1 << 8;
/// Send signals to any process regardless of ownership.
pub const CAP_KILL_ANY: u64  = 1 << 9;

/// Real-time priority (highest): 0.
pub const PRIO_REALTIME: i32 = 0;
/// High priority (IPC-intensive servers): 8.
pub const PRIO_HIGH: i32     = 8;
/// Normal user-space priority: 16.
pub const PRIO_NORMAL: i32   = 16;
/// Low background priority: 24.
pub const PRIO_LOW: i32      = 24;
/// Idle-only priority (lowest): 31.
pub const PRIO_IDLE: i32     = 31;

/// The state of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    /// Currently scheduled on a HART.
    Running,
    /// Suspended (e.g., waiting for an event).
    Stopped,
    /// Terminated, waiting to be reaped by the parent. Holds the exit status.
    Zombie(i32),
}

/// The IPC state of a process.
#[derive(Debug, Clone)]
pub enum IpcState {
    /// Not currently in an IPC operation.
    None,
    /// Sending a message to `target`.
    Sending { target: Pid, msg: crate::ipc::msg::Message, reply_msg_ptr: Option<usize> },
    /// Waiting for a message from `source` (which can be ANY, i.e., -1).
    Receiving { source: Pid, msg_ptr: usize },
    /// Waiting for a reply to a sent message.
    ReceivingReply { source: Pid, msg_ptr: usize },
    /// A message is available.
    MessageAvailable { msg: crate::ipc::msg::Message },
    /// A send has completed.
    SendComplete,
}

/// A process in the system.
///
/// # Fields
///
/// See the source code for a complete list of fields. Key fields include:
/// * `pid`: The process ID.
/// * `state`: The current state of the process.
/// * `fds`: The file descriptor table.
/// * `trap_frame`: The saved trap frame for context switching.
/// * `satp_val`: The Sv39 SATP register value for this process.
/// * `cwd`: The current working directory.
pub struct Process {
    /// The process ID.
    pub pid: Pid,
    /// Thread group ID; equals `pid` for the main thread.
    pub tgid: AtomicI32,
    /// Parent process ID.
    pub ppid: AtomicI32,
    /// Process group ID.
    pub pgid: AtomicI32,
    /// Session ID.
    pub sid: AtomicI32,
    /// User ID.
    pub uid: AtomicU32,
    /// Group ID.
    pub gid: AtomicU32,
    /// Effective user ID.
    pub euid: AtomicU32,
    /// Effective group ID.
    pub egid: AtomicU32,
    /// Capability mask.
    pub caps: AtomicU64,
    /// Scheduling priority: 0 (highest / PRIO_REALTIME) … 31 (lowest / PRIO_IDLE).
    pub priority: AtomicI32,
    /// The current state of the process.
    pub state: Mutex<ProcState>,
    /// The file descriptor table.
    pub fds: Mutex<HandleTable>,
    /// Child process IDs.
    pub children: Mutex<Vec<Pid>>,
    /// The saved trap frame for context switching.
    pub trap_frame: Mutex<crate::trap::TrapFrame>,
    /// Sv39 SATP register value for this process. Atomic so `sys_exec` can
    /// update it without going through an unsafe `Arc::as_ptr` cast.
    pub satp_val: AtomicUsize,
    /// Next available virtual address for anonymous mmap allocations.
    pub next_mmap_va: AtomicUsize,
    /// Current program break (top of data segment) for brk/sbrk.
    pub heap_break: AtomicUsize,
    /// Bottom (lowest address) of the heap-allocated kernel stack.
    pub kernel_stack_bottom: usize,
    /// Current working directory — inherited from parent, updated by chdir.
    pub cwd: Mutex<String>,
    /// The Job container for this process.
    pub job: Arc<crate::posix::job::Job>,
    /// The number of physical pages allocated to this process's user space.
    pub allocated_pages: AtomicUsize,
    /// User-space address to clear (write 0) and futex-wake on thread exit (CLONE_CHILD_CLEARTID).
    pub clear_tid_ptr: AtomicUsize,

    // Fields to support synchronous waitpid
    /// Target PID for synchronous waitpid.
    pub wait_target: Mutex<Option<Pid>>,
    /// User-space pointer to store the wait status.
    pub wait_status_ptr: Mutex<Option<usize>>,
    /// Queue of (child_pid, exit_status) results pending reap by this process.
    /// `VecDeque` so multiple children exiting before waitpid fires never lose exits.
    pub wait_result: Mutex<VecDeque<(Pid, i32)>>,
    
    // Asynchronous IPC (to be deprecated in Phase 3)
    /// Asynchronous mailbox.
    pub mailbox: Mutex<VecDeque<(Pid, Vec<u8>)>>,
    /// PID of a process blocked waiting on the mailbox.
    pub mailbox_waiter: AtomicI32,
    
    // Synchronous IPC fields
    /// Current IPC state.
    pub ipc_state: Mutex<IpcState>,
    /// Queue of PIDs waiting to send to this process.
    pub senders: Mutex<VecDeque<Pid>>,
    /// Overflow queue for kernel-injected IRQ messages that arrived while
    /// `ipc_state` was already `MessageAvailable` or mid-IPC.
    pub irq_pending: Mutex<VecDeque<crate::ipc::msg::Message>>,
    
    // Signals
    /// Pending signals.
    pub pending_signals: AtomicU32,
    /// Blocked signals mask.
    pub blocked_signals: AtomicU32,
    /// Signal handler addresses (one per signal).
    pub signal_handlers: Mutex<[usize; 32]>,
    /// Signal restorer addresses (one per signal).
    pub signal_restorers: Mutex<[usize; 32]>,
    /// Signal masks (one per signal).
    pub signal_masks: Mutex<[u32; 32]>,

    // Namespaces
    /// The set of namespaces this process belongs to.
    pub ns: crate::namespace::NsSet,
}

/// Canary value written to `kstack_bottom` on process creation.
///
/// `schedule()` verifies it before resuming a process; a mismatch means the
/// kernel stack grew past its allocation (overflow into the guard region).
pub const KSTACK_CANARY: u64 = 0xDEAD_C0DE_DEAD_C0DE;

/// Counter for generating unique process IDs.
static NEXT_PID: AtomicI32 = AtomicI32::new(1);

/// PID of the current foreground process (`-1` = none). Set by `sys_set_fg_pid`.
pub static FOREGROUND_PID: AtomicI32 = AtomicI32::new(-1);

/// Global process table, mapping PIDs to [`Process`] arcs.
pub static PROCESS_TABLE: Mutex<BTreeMap<Pid, Arc<Process>>> = Mutex::new(BTreeMap::new());
/// Global run queue: BTreeMap keyed by priority (0 = highest), each bucket a FIFO.
pub static RUN_QUEUE: Mutex<BTreeMap<i32, VecDeque<Pid>>> = Mutex::new(BTreeMap::new());

/// Enqueue `pid` into the run queue at an explicit priority level.
///
/// Callers that already hold a reference to the process or hold PROCESS_TABLE
/// must use this variant to avoid a second PROCESS_TABLE lock.
pub fn enqueue_with_prio(pid: Pid, prio: i32) {
    RUN_QUEUE.lock()
        .entry(prio)
        .or_insert_with(VecDeque::new)
        .push_back(pid);
}

/// Enqueue `pid` using the process's stored priority.
///
/// Must NOT be called while PROCESS_TABLE is already locked on this HART.
pub fn enqueue(pid: Pid) {
    let prio = PROCESS_TABLE.lock()
        .get(&pid)
        .map(|p| p.priority.load(Ordering::Relaxed))
        .unwrap_or(PRIO_NORMAL);
    enqueue_with_prio(pid, prio);
}

/// Queue of `(Pid, wakeup_mtime_ticks)` for sleeping processes.
pub static SLEEP_QUEUE: Mutex<alloc::vec::Vec<(Pid, u64)>> = Mutex::new(alloc::vec::Vec::new());

/// Per-HART array of the currently running PID.
static CURRENT_PIDS: [AtomicI32; crate::smp::MAX_HARTS] = [
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
];

/// Generates a new unique process ID.
///
/// # Returns
///
/// A unique `Pid`. PIDs are allocated sequentially starting from 1.
pub fn generate_pid() -> Pid {
    NEXT_PID.fetch_add(1, Ordering::SeqCst)
}

/// Wakes up any sleeping processes whose sleep time has expired.
///
/// This function should be called on each timer tick to handle
/// process sleep timeouts.
pub fn wake_sleepers() {
    let now = riscv::register::time::read() as u64;
    let mut sq = SLEEP_QUEUE.lock();
    if sq.is_empty() { return; }
    
    let mut i = 0;
    while i < sq.len() {
        if now >= sq[i].1 {
            let (pid, _) = sq.swap_remove(i);
            // Transition back to Running before enqueuing so another HART
            // can't observe the process as Stopped after it appears in the queue.
            if let Some(proc) = PROCESS_TABLE.lock().get(&pid).cloned() {
                *proc.state.lock() = ProcState::Running;
                enqueue_with_prio(pid, proc.priority.load(Ordering::Relaxed));
            }
        } else {
            i += 1;
        }
    }
}

/// Returns the PID of the currently running process on this HART.
///
/// # Returns
///
/// The PID of the currently running process on the calling HART.
pub fn current_pid() -> Pid {
    CURRENT_PIDS[crate::smp::current_hartid()].load(Ordering::SeqCst)
}

/// Sets the PID of the currently running process on this HART.
///
/// # Arguments
///
/// * `pid` - The PID to set.
fn set_current_pid(pid: Pid) {
    CURRENT_PIDS[crate::smp::current_hartid()].store(pid, Ordering::SeqCst);
}

/// Looks up the current process in the process table.
///
/// # Returns
///
/// `Some(Arc<Process>)` if the current process exists in the table,
/// `None` if it has been concurrently removed. Callers in syscall
/// handlers should translate `None` to `ESRCH`.
pub fn get_current_proc() -> Option<Arc<Process>> {
    PROCESS_TABLE.lock().get(&current_pid()).cloned()
}

/// Pops the next runnable PID from the run queue and executes it.
///
/// If the run queue is empty, this function waits for an interrupt
/// (`wfi`). This function never returns (`!`).
pub fn schedule() -> ! {
    loop {
        unsafe { riscv::register::sstatus::clear_sie(); }
        let next_pid = {
            let mut rq = RUN_QUEUE.lock();
            // Pop from the lowest-numbered (highest-priority) non-empty bucket.
            let prio = rq.keys().next().copied();
            if let Some(p) = prio {
                let bucket = rq.get_mut(&p).unwrap();
                let pid = bucket.pop_front();
                if bucket.is_empty() { rq.remove(&p); }
                pid
            } else {
                None
            }
        };

        if let Some(pid) = next_pid {
            let process_arc = PROCESS_TABLE.lock().get(&pid).cloned();
            if let Some(proc) = process_arc {
                // If it is a Zombie, discard it from the run queue
                if matches!(*proc.state.lock(), ProcState::Zombie(_)) {
                    continue;
                }

                // Kernel stack overflow check: verify the canary written at
                // kstack_bottom during Process::new() is still intact.
                if proc.kernel_stack_bottom != 0 {
                    let canary = unsafe { *(proc.kernel_stack_bottom as *const u64) };
                    if canary != KSTACK_CANARY {
                        panic!("kernel stack overflow detected for pid {}", pid);
                    }
                }

                *proc.state.lock() = ProcState::Running;
                set_current_pid(pid);

                // Switch address space
                let satp = proc.satp_val.load(Ordering::Relaxed);
                unsafe {
                    riscv::register::satp::write(satp);
                    core::arch::asm!("sfence.vma");
                }

                let tf_ptr = {
                    let tf = proc.trap_frame.lock();
                    &(*tf) as *const crate::trap::TrapFrame as usize
                };

                unsafe {
                    crate::trap::return_to_user(tf_ptr);
                }
            }
        } else {
            set_current_pid(0);
            unsafe {
                riscv::register::sstatus::set_sie();
                core::arch::asm!("wfi");
            }
        }
    }
}

impl Process {
    /// Creates a new process, optionally inheriting state from a parent.
    ///
    /// # Arguments
    ///
    /// * `ppid` - The parent process ID, or 0 if this is the init process.
    ///
    /// # Returns
    ///
    /// `Ok(Arc<Process>)` on success, or an error if:
    /// - The physical memory manager is out of pages (OOM).
    /// - The kernel heap is exhausted.
    ///
    /// Callers in syscall handlers should translate errors to
    /// [`crate::errno::ENOMEM`].
    pub fn new(ppid: Pid) -> Result<Arc<Self>, &'static str> {
        // Enforce a per-system process-count ceiling to prevent unbounded resource
        // exhaustion.  512 concurrent processes is far more than any realistic
        // workload on this platform.
        const MAX_PROCESSES: usize = 512;
        if PROCESS_TABLE.lock().len() >= MAX_PROCESSES {
            return Err("process limit reached");
        }

        let pid = generate_pid();

        let pt_addr = crate::mm::vmm::PageTable::new_process_table()
            .map_err(|_| "OOM: failed to create page table for process")?;
        let satp_val_bits = (8usize << 60) | (pt_addr >> 12); // Sv39

        const KERNEL_STACK_SIZE: usize = 65536;
        // INVARIANT: size=65536 and align=16 are compile-time constants that
        // always satisfy Layout's constraints — this can never fail.
        let kstack_layout =
            core::alloc::Layout::from_size_align(KERNEL_STACK_SIZE, 16)
                .unwrap_or_else(|_| unreachable!("constant Layout params"));
        // SAFETY: kstack_layout is valid (see above). We check for null below.
        let kstack_bottom = unsafe { alloc::alloc::alloc_zeroed(kstack_layout) } as usize;
        if kstack_bottom == 0 {
            // Allocation failed — release the page table we just built.
            let root_pa = (pt_addr >> 12) << 12;
            let _ = crate::mm::vmm::destroy_user_space(root_pa);
            return Err("OOM: failed to allocate kernel stack");
        }

        // Write a canary at the bottom of the kernel stack so overflow is
        // caught by schedule() before the corrupted stack is used again.
        unsafe { *(kstack_bottom as *mut u64) = KSTACK_CANARY; }

        let kernel_sp = kstack_bottom + KERNEL_STACK_SIZE;
        let mut tf = crate::trap::TrapFrame::new();
        tf.kernel_sp = kernel_sp;

        let fds = if ppid != 0 {
            let parent_arc = PROCESS_TABLE.lock().get(&ppid).cloned();
            if let Some(parent) = parent_arc {
                let parent_fds = parent.fds.lock();
                let mut new_fds = HandleTable::new();
                for (h, entry) in parent_fds.iter() {
                    new_fds.insert_entry(crate::ipc::handle::encode_handle(*h, entry.generation), entry.clone());
                }
                // Inherit FD_CLOEXEC flags so exec in the child closes them correctly.
                for h in parent_fds.cloexec_handles() {
                    new_fds.set_cloexec(*h, true);
                }
                new_fds
            } else {
                HandleTable::new()
            }
        } else {
            // Allocate the root session TTY and register it as the active ISR target.
            let tty = crate::tty::TtyState::new();
            *crate::tty::ACTIVE_TTY.lock() = Some(tty.clone());
            let mut fds = HandleTable::new();
            let tty_obj = crate::ipc::handle::KernelObject::Tty(tty);
            fds.insert_at(0, tty_obj.clone());
            fds.insert_at(1, tty_obj.clone());
            fds.insert_at(2, tty_obj);
            fds
        };

        let (uid, gid, euid, egid, caps, prio, cwd, handlers, restorers, masks, blocked, pgid, sid, ns) = if ppid != 0 {
            if let Some(parent) = PROCESS_TABLE.lock().get(&ppid) {
                (
                    parent.uid.load(Ordering::Relaxed),
                    parent.gid.load(Ordering::Relaxed),
                    parent.euid.load(Ordering::Relaxed),
                    parent.egid.load(Ordering::Relaxed),
                    parent.caps.load(Ordering::Relaxed),
                    parent.priority.load(Ordering::Relaxed),
                    parent.cwd.lock().clone(),
                    *parent.signal_handlers.lock(),
                    *parent.signal_restorers.lock(),
                    *parent.signal_masks.lock(),
                    parent.blocked_signals.load(Ordering::Relaxed),
                    parent.pgid.load(Ordering::Relaxed),
                    parent.sid.load(Ordering::Relaxed),
                    crate::namespace::NsSet::fork_from(&parent.ns, 0), // inherit, no new ns
                )
            } else {
                (0, 0, 0, 0, CAP_NONE, PRIO_NORMAL, String::from("/"), [0; 32], [0; 32], [0; 32], 0, pid, pid,
                 crate::namespace::NsSet::root())
            }
        } else {
            (0, 0, 0, 0, CAP_NONE, PRIO_NORMAL, String::from("/"), [0; 32], [0; 32], [0; 32], 0, pid, pid,
             crate::namespace::NsSet::root())
        };

        let job = if ppid != 0 {
            let parent_arc = PROCESS_TABLE.lock().get(&ppid).cloned();
            if let Some(parent) = parent_arc {
                parent.job.clone()
            } else {
                get_root_job()
            }
        } else {
            get_root_job()
        };

        job.processes.lock().push(pid);

        let proc = Arc::new(Process {
            pid,
            tgid: AtomicI32::new(pid), // main thread: tgid == pid; overridden by sys_clone
            ppid: AtomicI32::new(ppid),
            pgid: AtomicI32::new(pgid),
            sid: AtomicI32::new(sid),
            uid: AtomicU32::new(uid),
            gid: AtomicU32::new(gid),
            euid: AtomicU32::new(euid),
            egid: AtomicU32::new(egid),
            caps: AtomicU64::new(caps),
            priority: AtomicI32::new(prio),
            state: Mutex::new(ProcState::Running),
            fds: Mutex::new(fds),
            children: Mutex::new(Vec::new()),
            trap_frame: Mutex::new(tf),
            satp_val: AtomicUsize::new(satp_val_bits),
            next_mmap_va: AtomicUsize::new(0x4_0000_0000), // 16 GB – above 4 GB identity map
            heap_break: AtomicUsize::new(0x2_0000_0000),   // 8 GB initial brk
            kernel_stack_bottom: kstack_bottom,
            cwd: Mutex::new(cwd),
            job,
            allocated_pages: AtomicUsize::new(0),
            clear_tid_ptr: AtomicUsize::new(0),
            wait_target: Mutex::new(None),
            wait_status_ptr: Mutex::new(None),
            wait_result: Mutex::new(VecDeque::new()),
            mailbox: Mutex::new(VecDeque::new()),
            mailbox_waiter: AtomicI32::new(0),
            ipc_state: Mutex::new(IpcState::None),
            senders: Mutex::new(VecDeque::new()),
            irq_pending: Mutex::new(VecDeque::new()),
            pending_signals: AtomicU32::new(0),
            blocked_signals: AtomicU32::new(blocked),
            signal_handlers: Mutex::new(handlers),
            signal_restorers: Mutex::new(restorers),
            signal_masks: Mutex::new(masks),
            ns,
        });

        PROCESS_TABLE.lock().insert(pid, proc.clone());

        if ppid != 0 {
            let parent_arc = PROCESS_TABLE.lock().get(&ppid).cloned();
            if let Some(parent) = parent_arc {
                parent.children.lock().push(pid);
            }
        }

        Ok(proc)
    }
}

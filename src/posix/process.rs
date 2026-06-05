use crate::ipc::handle::HandleTable;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};
use crate::sync::Mutex;

pub type Pid = i32;
pub type Pgid = i32;
pub type Sid = i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    Running,
    Stopped,
    Zombie(i32), // Holds the exit status
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcState {
    None,
    Sending { target: Pid, msg: crate::ipc::msg::Message, reply_msg_ptr: Option<usize> },
    Receiving { source: Pid, msg_ptr: usize }, // source can be ANY (-1)
    ReceivingReply { source: Pid, msg_ptr: usize },
    MessageAvailable { msg: crate::ipc::msg::Message },
    SendComplete,
}

pub struct Process {
    pub pid: Pid,
    pub ppid: AtomicI32,
    pub pgid: AtomicI32,
    pub sid: AtomicI32,
    pub uid: AtomicU32,
    pub gid: AtomicU32,
    pub euid: AtomicU32,
    pub egid: AtomicU32,
    pub state: Mutex<ProcState>,
    pub fds: Mutex<HandleTable>,
    pub children: Mutex<Vec<Pid>>,
    pub trap_frame: Mutex<crate::trap::TrapFrame>,
    /// Sv39 SATP register value for this process.  Atomic so sys_exec can
    /// update it without going through an unsafe Arc::as_ptr cast.
    pub satp_val: AtomicUsize,
    /// Bottom (lowest address) of the heap-allocated kernel stack.
    pub kernel_stack_bottom: usize,
    /// Current working directory — inherited from parent, updated by chdir.
    pub cwd: Mutex<String>,
    // Fields to support synchronous waitpid
    pub wait_target: Mutex<Option<Pid>>,
    pub wait_status_ptr: Mutex<Option<usize>>,
    pub wait_result: Mutex<Option<(Pid, i32)>>,
    
    // Asynchronous IPC (to be deprecated in Phase 3)
    pub mailbox: Mutex<VecDeque<(Pid, Vec<u8>)>>,
    pub mailbox_waiter: AtomicI32,
    
    // Synchronous IPC fields
    pub ipc_state: Mutex<IpcState>,
    pub senders: Mutex<VecDeque<Pid>>,
    
    // Signals
    pub pending_signals: AtomicU32,
    pub blocked_signals: AtomicU32,
    pub signal_handlers: Mutex<[usize; 32]>,
}

static NEXT_PID: AtomicI32 = AtomicI32::new(1);

/// PID of the current foreground process (-1 = none). Set by sys_set_fg_pid.
pub static FOREGROUND_PID: AtomicI32 = AtomicI32::new(-1);

pub static PROCESS_TABLE: Mutex<BTreeMap<Pid, Arc<Process>>> = Mutex::new(BTreeMap::new());
pub static RUN_QUEUE: Mutex<alloc::collections::VecDeque<Pid>> =
    Mutex::new(alloc::collections::VecDeque::new());

/// Queue of (Pid, wakeup_mtime_ticks)
pub static SLEEP_QUEUE: Mutex<alloc::vec::Vec<(Pid, u64)>> = Mutex::new(alloc::vec::Vec::new());

static CURRENT_PIDS: [AtomicI32; crate::smp::MAX_HARTS] = [
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
];

pub fn generate_pid() -> Pid {
    NEXT_PID.fetch_add(1, Ordering::SeqCst)
}

pub fn wake_sleepers() {
    let now = riscv::register::time::read() as u64;
    let mut sq = SLEEP_QUEUE.lock();
    if sq.is_empty() { return; }
    
    let mut i = 0;
    while i < sq.len() {
        if now >= sq[i].1 {
            let (pid, _) = sq.swap_remove(i);
            RUN_QUEUE.lock().push_back(pid);
        } else {
            i += 1;
        }
    }
}

pub fn current_pid() -> Pid {
    CURRENT_PIDS[crate::smp::current_hartid()].load(Ordering::SeqCst)
}

fn set_current_pid(pid: Pid) {
    CURRENT_PIDS[crate::smp::current_hartid()].store(pid, Ordering::SeqCst);
}

/// Look up the current process in the process table.
///
/// Returns `None` if the process has been concurrently removed from the table,
/// which should be treated as `ESRCH` by syscall callers.
///
/// Callers in the trap handler should use the [`get_current_proc_or_esrch!`]
/// macro for ergonomic early-exit on `None`.
pub fn get_current_proc() -> Option<Arc<Process>> {
    PROCESS_TABLE.lock().get(&current_pid()).cloned()
}

/// Pop the next runnable PID from the run queue and execute it.
/// If the run queue is empty, wait for an interrupt (WFI).
pub fn schedule() -> ! {
    loop {
        unsafe { riscv::register::sstatus::clear_sie(); }
        let next_pid = {
            let mut rq = RUN_QUEUE.lock();
            rq.pop_front()
        };

        if let Some(pid) = next_pid {
            let process_arc = PROCESS_TABLE.lock().get(&pid).cloned();
            if let Some(proc) = process_arc {
                // If it is a Zombie, discard it from the run queue
                if matches!(*proc.state.lock(), ProcState::Zombie(_)) {
                    continue;
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
    /// Create a new process, optionally inheriting state from a parent.
    /// Returns `Err` if the physical memory manager is out of pages (OOM)
    /// or if the kernel heap is exhausted. Callers in syscall handlers should
    /// translate this to [`crate::errno::ENOMEM`].
    pub fn new(ppid: Pid) -> Result<Arc<Self>, &'static str> {
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

        let kernel_sp = kstack_bottom + KERNEL_STACK_SIZE;
        let mut tf = crate::trap::TrapFrame::new();
        tf.kernel_sp = kernel_sp;

        let fds = if ppid != 0 {
            let parent_arc = PROCESS_TABLE.lock().get(&ppid).cloned();
            if let Some(parent) = parent_arc {
                let parent_fds = parent.fds.lock();
                let mut new_fds = HandleTable::new();
                for (h, obj) in parent_fds.iter() {
                    new_fds.insert_at(*h, obj.clone());
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
            let mut fds = HandleTable::new();
            fds.insert_at(0, crate::ipc::handle::KernelObject::Console);
            fds.insert_at(1, crate::ipc::handle::KernelObject::Console);
            fds.insert_at(2, crate::ipc::handle::KernelObject::Console);
            fds
        };

        let (uid, gid, euid, egid, cwd) = if ppid != 0 {
            if let Some(parent) = PROCESS_TABLE.lock().get(&ppid) {
                (
                    parent.uid.load(Ordering::Relaxed),
                    parent.gid.load(Ordering::Relaxed),
                    parent.euid.load(Ordering::Relaxed),
                    parent.egid.load(Ordering::Relaxed),
                    parent.cwd.lock().clone(),
                )
            } else {
                (0, 0, 0, 0, String::from("/"))
            }
        } else {
            (0, 0, 0, 0, String::from("/"))
        };

        let proc = Arc::new(Process {
            pid,
            ppid: AtomicI32::new(ppid),
            pgid: AtomicI32::new(pid),
            sid: AtomicI32::new(pid),
            uid: AtomicU32::new(uid),
            gid: AtomicU32::new(gid),
            euid: AtomicU32::new(euid),
            egid: AtomicU32::new(egid),
            state: Mutex::new(ProcState::Running),
            fds: Mutex::new(fds),
            children: Mutex::new(Vec::new()),
            trap_frame: Mutex::new(tf),
            satp_val: AtomicUsize::new(satp_val_bits),
            kernel_stack_bottom: kstack_bottom,
            cwd: Mutex::new(cwd),
            wait_target: Mutex::new(None),
            wait_status_ptr: Mutex::new(None),
            wait_result: Mutex::new(None),
            mailbox: Mutex::new(VecDeque::new()),
            mailbox_waiter: AtomicI32::new(0),
            ipc_state: Mutex::new(IpcState::None),
            senders: Mutex::new(VecDeque::new()),
            pending_signals: AtomicU32::new(0),
            blocked_signals: AtomicU32::new(0),
            signal_handlers: Mutex::new([0; 32]),
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

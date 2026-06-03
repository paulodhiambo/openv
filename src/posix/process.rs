use crate::ipc::handle::HandleTable;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use spin::Mutex;

pub type Pid = i32;
pub type Pgid = i32;
pub type Sid = i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    Running,
    Stopped,
    Zombie(i32), // Holds the exit status
}

pub struct Process {
    pub pid: Pid,
    pub ppid: Pid,
    pub pgid: Pgid,
    pub sid: Sid,
    pub uid: AtomicU32,
    pub gid: AtomicU32,
    pub euid: AtomicU32,
    pub egid: AtomicU32,
    pub state: Mutex<ProcState>,
    pub fds: Mutex<HandleTable>,
    pub children: Mutex<Vec<Pid>>,
    pub trap_frame: Mutex<crate::trap::TrapFrame>,
    pub satp_val: usize,
    /// Bottom (lowest address) of the heap-allocated kernel stack.
    pub kernel_stack_bottom: usize,
    /// Current working directory — inherited from parent, updated by chdir.
    pub cwd: Mutex<String>,
    // Fields to support synchronous waitpid
    pub wait_target: Mutex<Option<Pid>>,
    pub wait_status_ptr: Mutex<Option<usize>>,
    pub wait_result: Mutex<Option<(Pid, i32)>>,
}

static NEXT_PID: AtomicI32 = AtomicI32::new(1);

/// PID of the current foreground process (-1 = none). Set by sys_set_fg_pid.
pub static FOREGROUND_PID: AtomicI32 = AtomicI32::new(-1);

pub static PROCESS_TABLE: Mutex<BTreeMap<Pid, Arc<Process>>> = Mutex::new(BTreeMap::new());
pub static RUN_QUEUE: Mutex<alloc::collections::VecDeque<Pid>> =
    Mutex::new(alloc::collections::VecDeque::new());

static CURRENT_PIDS: [AtomicI32; crate::smp::MAX_HARTS] = [
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
];

pub fn generate_pid() -> Pid {
    NEXT_PID.fetch_add(1, Ordering::SeqCst)
}

pub fn current_hart() -> usize {
    let tp: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) tp, options(nomem, nostack));
    }
    tp.min(crate::smp::MAX_HARTS - 1)
}

pub fn current_pid() -> Pid {
    CURRENT_PIDS[current_hart()].load(Ordering::SeqCst)
}

fn set_current_pid(pid: Pid) {
    CURRENT_PIDS[current_hart()].store(pid, Ordering::SeqCst);
}

pub fn schedule() {
    loop {
        let next_pid = RUN_QUEUE.lock().pop_front();

        if let Some(pid) = next_pid {
            let proc = {
                let table = PROCESS_TABLE.lock();
                table.get(&pid).cloned()
            };

            if let Some(proc) = proc {
                let state = *proc.state.lock();
                if matches!(state, ProcState::Running) {
                    set_current_pid(pid);

                    let tf_ptr = {
                        let mut tf = proc.trap_frame.lock();
                        &mut *tf as *mut _ as usize
                    };

                    let satp_val = proc.satp_val;
                    unsafe {
                        riscv::register::satp::write(satp_val);
                        core::arch::asm!("sfence.vma");
                        crate::trap::return_to_user(tf_ptr);
                    }
                    // UNREACHABLE
                }
                // Non-Running pids are silently skipped; they will be re-queued
                // by wake-up paths (exit, waitpid delivery, etc.).
            }
        } else {
            // Idle: rearm timer (clears STIP) and wait for the next tick.
            crate::timer::set_next_timer();
            unsafe {
                core::arch::asm!("wfi");
            }
        }
    }
}

impl Process {
    pub fn new(ppid: Pid) -> Arc<Self> {
        let pid = generate_pid();

        let pt_addr = crate::mm::vmm::PageTable::new_process_table()
            .unwrap_or_else(|_| panic!("Failed to create page table for process"));
        let satp_val = (8usize << 60) | (pt_addr >> 12); // Sv39

        const KERNEL_STACK_SIZE: usize = 65536; // 64 KB — large enough for debug-mode call depth
        let kstack_layout = core::alloc::Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
        let kstack_bottom = unsafe { alloc::alloc::alloc_zeroed(kstack_layout) } as usize;
        assert!(kstack_bottom != 0, "Failed to alloc kernel stack");

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
            ppid,
            pgid: pid,
            sid: pid,
            uid: AtomicU32::new(uid),
            gid: AtomicU32::new(gid),
            euid: AtomicU32::new(euid),
            egid: AtomicU32::new(egid),
            state: Mutex::new(ProcState::Running),
            fds: Mutex::new(fds),
            children: Mutex::new(Vec::new()),
            trap_frame: Mutex::new(tf),
            satp_val,
            kernel_stack_bottom: kstack_bottom,
            cwd: Mutex::new(cwd),
            wait_target: Mutex::new(None),
            wait_status_ptr: Mutex::new(None),
            wait_result: Mutex::new(None),
        });

        PROCESS_TABLE.lock().insert(pid, proc.clone());

        if ppid != 0 {
            if let Some(parent) = PROCESS_TABLE.lock().get(&ppid) {
                parent.children.lock().push(pid);
            }
        }

        proc
    }
}

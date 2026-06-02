use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use alloc::sync::Arc;
use spin::Mutex;
use core::sync::atomic::{AtomicI32, Ordering};
use crate::ipc::handle::HandleTable;

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
    pub uid: u32,
    pub gid: u32,
    pub euid: u32,
    pub egid: u32,
    pub state: Mutex<ProcState>,
    pub fds: Mutex<HandleTable>,
    pub children: Mutex<Vec<Pid>>,
    pub trap_frame: Mutex<crate::trap::TrapFrame>,
    pub satp_val: usize,
    // Fields to support synchronous waitpid
    pub wait_target: Mutex<Option<Pid>>,
    pub wait_status_ptr: Mutex<Option<usize>>,
    pub wait_result: Mutex<Option<(Pid, i32)>>,
}

static NEXT_PID: AtomicI32 = AtomicI32::new(1);

pub static PROCESS_TABLE: Mutex<BTreeMap<Pid, Arc<Process>>> = Mutex::new(BTreeMap::new());
pub static RUN_QUEUE: Mutex<alloc::collections::VecDeque<Pid>> = Mutex::new(alloc::collections::VecDeque::new());
pub static CURRENT_PID: AtomicI32 = AtomicI32::new(0);

pub fn generate_pid() -> Pid {
    NEXT_PID.fetch_add(1, Ordering::SeqCst)
}

pub fn current_pid() -> Pid {
    CURRENT_PID.load(Ordering::SeqCst)
}

pub fn schedule() -> ! {
    loop {
        let next_pid = {
            let mut queue = RUN_QUEUE.lock();
            queue.pop_front()
        };

        if let Some(pid) = next_pid {
            let proc = {
                let table = PROCESS_TABLE.lock();
                table.get(&pid).cloned()
            };

            if let Some(proc) = proc {
                let state = *proc.state.lock();
                if state == ProcState::Running {
                    CURRENT_PID.store(pid, Ordering::SeqCst);
                    
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
                }
            }
        } else {
            // Idle loop: wait for interrupts when there's no runnable process
            unsafe { core::arch::asm!("wfi"); }
        }
    }
}

impl Process {
    pub fn new(ppid: Pid) -> Arc<Self> {
        crate::println!("Process::new: generating pid");
        let pid = generate_pid();
        
        crate::println!("Process::new: creating page table");
        let mut satp_val = 0;
        if let Ok(pt_addr) = crate::mm::vmm::PageTable::new_process_table() {
            let ppn = pt_addr >> 12;
            satp_val = (8 << 60) | ppn; // Sv39
        } else {
            panic!("Failed to create page table for process");
        }

        crate::println!("Process::new: allocating kernel stack");
        let kernel_stack = crate::mm::pmm::alloc_page().expect("Failed to alloc kernel stack 1");
        crate::mm::pmm::alloc_page().unwrap();
        crate::mm::pmm::alloc_page().unwrap();
        let stack_top_page = crate::mm::pmm::alloc_page().unwrap();
        
        let kernel_sp = stack_top_page + crate::mm::pmm::PAGE_SIZE;
        let mut tf = crate::trap::TrapFrame::new();
        tf.kernel_sp = kernel_sp;

        crate::println!("Process::new: setting up fds");
        let fds = if ppid != 0 {
            if let Some(parent) = PROCESS_TABLE.lock().get(&ppid) {
                // Clone the handle table map
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

        crate::println!("Process::new: setting up identity");
        let (uid, gid, euid, egid) = if ppid != 0 {
            if let Some(parent) = PROCESS_TABLE.lock().get(&ppid) {
                (parent.uid, parent.gid, parent.euid, parent.egid)
            } else {
                (0, 0, 0, 0)
            }
        } else {
            (0, 0, 0, 0)
        };

        crate::println!("Process::new: creating Arc<Process>");
        let proc = Arc::new(Process {
            pid,
            ppid,
            pgid: pid, // By default, new process is its own process group leader
            sid: pid,
            uid,
            gid,
            euid,
            egid,
            state: Mutex::new(ProcState::Running),
            fds: Mutex::new(fds),
            children: Mutex::new(Vec::new()),
            trap_frame: Mutex::new(tf),
            satp_val,
            wait_target: Mutex::new(None),
            wait_status_ptr: Mutex::new(None),
            wait_result: Mutex::new(None),
        });
        
        crate::println!("Process::new: inserting to PROCESS_TABLE");
        PROCESS_TABLE.lock().insert(pid, proc.clone());
        
        crate::println!("Process::new: registering with parent");
        // Register with parent if it exists
        if ppid != 0 {
            if let Some(parent) = PROCESS_TABLE.lock().get(&ppid) {
                parent.children.lock().push(pid);
            }
        }
        
        crate::println!("Process::new: done");
        proc
    }
}

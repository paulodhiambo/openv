use crate::posix::process::{Process, Pid, PROCESS_TABLE};
use crate::println;
use crate::vfs::lookup_path;
use crate::posix::elf::load_elf;
use crate::mm::vmm::{PTE_R, PTE_W, PTE_U};
use crate::mm::pmm::{alloc_page, PAGE_SIZE};
use alloc::vec::Vec;
use alloc::sync::Arc;

pub const USER_STACK_TOP: usize = 0x200000000; // 8GB
pub const USER_STACK_PAGES: usize = 4;

pub fn posix_spawn(path: &str, ppid: Pid) -> Result<Pid, &'static str> {
    let proc = Process::new(ppid);
    crate::println!("posix_spawn: Process {} created", proc.pid);
    
    // Read the binary from VFS
    let vnode = lookup_path(path)?;
    
    // Check execute permission
    let proc_ref = {
        let table = PROCESS_TABLE.lock();
        table.get(&proc.pid).cloned().unwrap()
    };
    if !crate::vfs::check_access(&proc_ref, &*vnode, crate::vfs::ACCESS_EXEC) {
        return Err("Permission denied");
    }

    let stat = vnode.stat();
    
    // Setuid / Setgid support
    {
        // We modify the process we just created
        if stat.mode & 0o4000 != 0 {
            // setuid bit
            unsafe {
                let p_ptr = Arc::as_ptr(&proc) as *mut Process;
                (*p_ptr).euid = stat.uid;
            }
        }
        if stat.mode & 0o2000 != 0 {
            // setgid bit
            unsafe {
                let p_ptr = Arc::as_ptr(&proc) as *mut Process;
                (*p_ptr).egid = stat.gid;
            }
        }
    }

    let size = stat.size;
    crate::println!("posix_spawn: VNode size {}", size);
    if size == 0 {
        return Err("File is empty or not found");
    }
    
    let mut file_data = Vec::new();
    crate::println!("posix_spawn: Resizing Vec to {}", size);
    file_data.resize(size, 0);
    crate::println!("posix_spawn: Reading into Vec");
    vnode.read(0, &mut file_data)?;
    crate::println!("posix_spawn: Read successful");
    
    // Get the page table
    let satp_val = proc.satp_val;
    let ppn = satp_val & 0xFFFFFFFFFFF;
    let pt_pa = ppn << 12;
    let pt = unsafe { &mut *(pt_pa as *mut crate::mm::vmm::PageTable) };
    
    // Load ELF
    crate::println!("posix_spawn: Parsing and Loading ELF");
    let entry_point = load_elf(&file_data, pt)?;
    crate::println!("posix_spawn: ELF Loaded, entry={:#x}", entry_point);
    
    // Allocate and map user stack
    let stack_base = USER_STACK_TOP - (USER_STACK_PAGES * PAGE_SIZE);
    for i in 0..USER_STACK_PAGES {
        let va = stack_base + i * PAGE_SIZE;
        crate::println!("Mapping stack VA: {:#x}", va);
        let pa = alloc_page().ok_or("Failed to alloc user stack")?;
        pt.map_page(va, pa, PTE_R | PTE_W | PTE_U)?;
    }
    
    // Configure TrapFrame
    {
        let mut tf = proc.trap_frame.lock();
        tf.sepc = entry_point;
        tf.regs[2] = USER_STACK_TOP; // SP
        // sstatus: Set SPIE (bit 5) = 1, SPP (bit 8) = 0 (U-mode)
        // Also SUM (bit 18) = 1 to allow kernel to access user memory
        tf.sstatus = (1 << 5) | (1 << 18);
    }
    
    println!("posix_spawn: Created Process {} (parent: {}) executing '{}'", proc.pid, ppid, path);
    crate::posix::process::RUN_QUEUE.lock().push_back(proc.pid);
    Ok(proc.pid)
}

pub fn exit(pid: Pid, status: i32) {
    use crate::posix::process::{PROCESS_TABLE, ProcState};

    // Phase 1: Mark zombie and collect ppid + children list (single lock scope).
    let (ppid, children) = {
        let table = PROCESS_TABLE.lock();
        match table.get(&pid) {
            None => return,
            Some(proc) => {
                *proc.state.lock() = ProcState::Zombie(status);
                println!("Process {} exited with status {}", pid, status);
                let children = proc.children.lock().clone();
                (proc.ppid, children)
            }
        }
    };

    // Phase 2: Orphan reparenting — give all children to init (PID 1).
    if !children.is_empty() {
        let table = PROCESS_TABLE.lock();
        for &child_pid in &children {
            if let Some(child) = table.get(&child_pid) {
                unsafe {
                    let p_ptr = Arc::as_ptr(child) as *mut Process;
                    (*p_ptr).ppid = 1;
                }
            }
        }
        if let Some(init) = table.get(&1) {
            let mut init_children = init.children.lock();
            for &child_pid in &children {
                if !init_children.contains(&child_pid) {
                    init_children.push(child_pid);
                }
            }
        }
    }

    // Phase 3: Wake parent if it is waiting for this child (or any child).
    if ppid != 0 {
        let table = PROCESS_TABLE.lock();
        if let Some(parent) = table.get(&ppid) {
            let delivered = {
                let target = parent.wait_target.lock();
                if target.is_none() || *target == Some(pid) || *target == Some(-1) {
                    *parent.wait_result.lock() = Some((pid, status));
                    true
                } else {
                    false
                }
            };

            *parent.state.lock() = ProcState::Running;
            crate::posix::process::RUN_QUEUE.lock().push_back(parent.pid);

            if delivered {
                crate::println!("Delivered wait_result to parent {} for child {}", parent.pid, pid);
            }
        }
    }
}

// Minimal sys_fork scaffold: create a child process entry and copy trapframe.
// TODO: Implement copy-on-write page table duplication.

pub fn sys_fork() -> Result<Pid, &'static str> {
    let ppid = crate::posix::process::current_pid();
    // Create a new process structure (this currently creates a fresh page table).
    let child = crate::posix::process::Process::new(ppid);

    // Attempt to clone the parent's user address space into the child's page table using COW
    if let Some(parent_proc) = crate::posix::process::PROCESS_TABLE.lock().get(&ppid) {
        let parent_satp = parent_proc.satp_val;
        let child_satp = child.satp_val;
        // extract root physical addresses
        let parent_root_pa = parent_satp & 0xFFFFFFFFFFF;
        let child_root_pa = child_satp & 0xFFFFFFFFFFF;
        match crate::mm::vmm::clone_user_space(parent_root_pa, child_root_pa) {
            Ok(()) => {
                // Successful clone
            }
            Err(e) => {
                crate::println!("sys_fork: clone_user_space failed: {}", e);
                return Err("fork failed: clone failed");
            }
        }

        // Copy trap frame from parent so the child resumes at the same place.
        let parent_tf = parent_proc.trap_frame.lock();
        let mut child_tf = child.trap_frame.lock();
        *child_tf = *parent_tf;
        // In child, fork() returns 0
        child_tf.regs[10] = 0;
    }

    // Enqueue child to run
    crate::posix::process::RUN_QUEUE.lock().push_back(child.pid);

    Ok(child.pid)
}

/// Replace current process image with new ELF (execve-like minimal).
/// arg0: path_ptr, arg1: path_len
pub fn sys_exec(path_ptr: *const u8, path_len: usize) -> Result<(), &'static str> {
    let ppid = crate::posix::process::current_pid();
    let path_bytes = unsafe { core::slice::from_raw_parts(path_ptr, path_len) };
    let path = core::str::from_utf8(path_bytes).map_err(|_| "Invalid UTF-8 path")?;

    // Lookup and read file
    let vnode = crate::vfs::lookup_path(path)?;
    let stat = vnode.stat();
    if stat.size == 0 { return Err("Empty binary"); }
    let mut file_data = alloc::vec::Vec::new();
    file_data.resize(stat.size, 0);
    vnode.read(0, &mut file_data)?;

    // Get current process and its old root
    let proc = crate::posix::process::PROCESS_TABLE.lock().get(&ppid).cloned().unwrap();
    let old_satp = proc.satp_val;
    let old_root_pa = old_satp & 0xFFFFFFFFFFF;

    // Create a fresh page table for the process
    let new_root_pa = crate::mm::vmm::PageTable::new_process_table()?;
    let new_ppn = new_root_pa >> 12;
    let new_satp = (8 << 60) | new_ppn;

    // Destroy old user mappings (free pages / decrement refs)
    crate::mm::vmm::destroy_user_space(old_root_pa)?;

    // Install new page table into process
    unsafe {
        let p_ptr = alloc::sync::Arc::as_ptr(&proc) as *mut crate::posix::process::Process;
        (*p_ptr).satp_val = new_satp;
    }

    // Load ELF into new page table
    let new_pt = unsafe { &mut *(new_root_pa as *mut crate::mm::vmm::PageTable) };
    let entry_point = crate::posix::elf::load_elf(&file_data, new_pt)?;

    // Allocate and map user stack
    let stack_base = USER_STACK_TOP - (USER_STACK_PAGES * PAGE_SIZE);
    for i in 0..USER_STACK_PAGES {
        let va = stack_base + i * PAGE_SIZE;
        let pa = alloc_page().ok_or("Failed to alloc user stack")?;
        new_pt.map_page(va, pa, PTE_R | PTE_W | PTE_U)?;
    }

    // Update trap frame for current process
    {
        let mut tf = proc.trap_frame.lock();
        tf.sepc = entry_point;
        tf.regs[2] = USER_STACK_TOP; // SP
        tf.sstatus = (1 << 5) | (1 << 18);
    }

    Ok(())
}

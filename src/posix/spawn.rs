use crate::posix::process::{Process, Pid, PROCESS_TABLE};
use crate::vfs::lookup_path;
use crate::posix::elf::load_elf;
use crate::mm::vmm::{PTE_R, PTE_W, PTE_U};
use crate::mm::pmm::{alloc_page, PAGE_SIZE};
use core::sync::atomic::Ordering;
use alloc::vec::Vec;
use alloc::sync::Arc;

pub const USER_STACK_TOP: usize = 0x200000000; // 8GB
pub const USER_STACK_PAGES: usize = 4;

pub fn posix_spawn(path: &str, ppid: Pid) -> Result<Pid, &'static str> {
    let proc = Process::new(ppid);
    let pid = proc.pid;

    // Helper: on failure, clean up the partially-initialised process.
    let cleanup = |e: &'static str| -> &'static str {
        cleanup_process(pid);
        e
    };

    // Read the binary from VFS
    let vnode = lookup_path(path).map_err(cleanup)?;

    // Check execute permission
    let proc_ref = {
        let table = PROCESS_TABLE.lock();
        table.get(&pid).cloned().unwrap()
    };
    if !crate::vfs::check_access(&proc_ref, &*vnode, crate::vfs::ACCESS_EXEC) {
        cleanup_process(pid);
        return Err("Permission denied");
    }

    let stat = vnode.stat();

    // Setuid / Setgid support
    {
        if stat.mode & 0o4000 != 0 {
            proc.euid.store(stat.uid, Ordering::Relaxed);
        }
        if stat.mode & 0o2000 != 0 {
            proc.egid.store(stat.gid, Ordering::Relaxed);
        }
    }

    let size = stat.size;
    if size == 0 {
        cleanup_process(pid);
        return Err("File is empty or not found");
    }

    let mut file_data = Vec::new();
    file_data.resize(size, 0);
    vnode.read(0, &mut file_data).map_err(cleanup)?;

    // Get the page table
    let satp_val = proc.satp_val;
    let ppn = satp_val & 0xFFFFFFFFFFF;
    let pt_pa = ppn << 12;
    let pt = unsafe { &mut *(pt_pa as *mut crate::mm::vmm::PageTable) };

    // Load ELF
    let entry_point = load_elf(&file_data, pt).map_err(cleanup)?;

    // Allocate and map user stack
    let stack_base = USER_STACK_TOP - (USER_STACK_PAGES * PAGE_SIZE);
    for i in 0..USER_STACK_PAGES {
        let va = stack_base + i * PAGE_SIZE;
        let pa = alloc_page().ok_or_else(|| cleanup("Failed to alloc user stack"))?;
        pt.map_page(va, pa, PTE_R | PTE_W | PTE_U).map_err(cleanup)?;
    }

    // Configure TrapFrame
    {
        let mut tf = proc.trap_frame.lock();
        tf.sepc = entry_point;
        tf.regs[2] = USER_STACK_TOP;
        tf.sstatus = (1 << 5) | (1 << 18);
    }

    crate::println!("spawn: pid {} executing '{}'", pid, path);
    crate::posix::process::RUN_QUEUE.lock().push_back(pid);
    Ok(pid)
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
                crate::println!("exit: pid {} status {}", pid, status);
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

            let _ = delivered;
        }
    }
}

/// sys_fork — create a child process with copy-on-write address space.
///
/// The child receives a cloned page table where all writable user pages
/// are marked read-only and shared with the parent.  The first write by
/// either process triggers handle_store_page_fault which allocates a
/// private copy (COW).
pub fn sys_fork() -> Result<Pid, &'static str> {
    let ppid = crate::posix::process::current_pid();
    let child = crate::posix::process::Process::new(ppid);

    if let Some(parent_proc) = crate::posix::process::PROCESS_TABLE.lock().get(&ppid) {
        let parent_root_pa = (parent_proc.satp_val & 0xFFFFFFFFFFF) << 12;
        let child_root_pa = (child.satp_val & 0xFFFFFFFFFFF) << 12;

        crate::mm::vmm::clone_user_space(parent_root_pa, child_root_pa)
            .map_err(|e| {
                crate::println!("sys_fork: clone_user_space failed: {}", e);
                "fork failed: clone failed"
            })?;

        // Flush the parent's TLB: we cleared PTE_W on its writable user pages,
        // but stale TLB entries would let writes bypass the read-only PTE and
        // defeat COW isolation.
        unsafe { core::arch::asm!("sfence.vma") };

        // Copy trap frame — child resumes past the ecall with a0 = 0.
        let parent_tf = parent_proc.trap_frame.lock();
        let mut child_tf = child.trap_frame.lock();
        let child_kernel_sp = child_tf.kernel_sp; // preserve child's own kernel stack
        *child_tf = *parent_tf;
        child_tf.kernel_sp = child_kernel_sp;
        child_tf.regs[10] = 0;    // fork() returns 0 in the child
        child_tf.sepc += 4;       // advance past the ecall instruction
    }

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

/// Remove a newly-created but not-yet-running process from the system
/// and free its resources.  Used by posix_spawn when setup fails after
/// Process::new has been called.
fn cleanup_process(pid: Pid) {
    // Remove from the parent's children list first.
    {
        let table = PROCESS_TABLE.lock();
        if let Some(proc) = table.get(&pid) {
            let ppid = proc.ppid;
            if ppid != 0 {
                if let Some(parent) = table.get(&ppid) {
                    parent.children.lock().retain(|&c| c != pid);
                }
            }
        }
    }

    // Free the kernel stack and destroy user page tables.
    let stack_and_satp = {
        let table = PROCESS_TABLE.lock();
        table.get(&pid).map(|p| (p.kernel_stack_bottom, p.satp_val))
    };

    if let Some((kstack_bottom, satp)) = stack_and_satp {
        const KERNEL_STACK_SIZE: usize = 65536;
        if kstack_bottom != 0 {
            unsafe {
                alloc::alloc::dealloc(
                    kstack_bottom as *mut u8,
                    core::alloc::Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap(),
                );
            }
        }
        let root_pa = (satp & 0xFFFFFFFFFFF) << 12;
        let _ = crate::mm::vmm::destroy_user_space(root_pa);
    }

    // Remove from the process table.
    PROCESS_TABLE.lock().remove(&pid);
}

use crate::mm::pmm::{alloc_frame, PAGE_SIZE};
use crate::mm::vmm::{PTE_R, PTE_U, PTE_W};
use crate::posix::elf::load_elf;
use crate::posix::process::{PROCESS_TABLE, Pid, Process};
use crate::vfs::lookup_path;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

pub const USER_STACK_TOP: usize = 0x200000000; // 8GB
pub const USER_STACK_PAGES: usize = 8; // 32 KB — enough for smoltcp + daemon stacks in debug builds

pub fn posix_spawn(path: &str, ppid: Pid) -> Result<Pid, &'static str> {
    let proc = Process::new(ppid)?;
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
        table.get(&pid).cloned().ok_or_else(|| cleanup("internal: process table corrupted"))?
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
    let satp_val = proc.satp_val.load(core::sync::atomic::Ordering::Relaxed);
    let ppn = satp_val & 0xFFFFFFFFFFF;
    let pt_pa = ppn << 12;
    let pt = unsafe { &mut *(pt_pa as *mut crate::mm::vmm::PageTable) };

    // Load ELF
    let entry_point = load_elf(&file_data, pt).map_err(cleanup)?;

    // Allocate and map user stack
    let stack_base = USER_STACK_TOP - (USER_STACK_PAGES * PAGE_SIZE);
    for i in 0..USER_STACK_PAGES {
        let va = stack_base + i * PAGE_SIZE;
        let frame = alloc_frame().ok_or_else(|| cleanup("Failed to alloc user stack"))?;
        pt.map_page(va, frame.pa(), PTE_R | PTE_W | PTE_U)
            .map_err(cleanup)?;
        frame.into_raw(); // ownership transferred to PTE
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

fn try_wake_parent(parent: &Process, child_pid: Pid, child_status: i32) {
    let should_wake = {
        let target = parent.wait_target.lock();
        if target.is_none() || *target == Some(child_pid) || *target == Some(-1i32) {
            *parent.wait_result.lock() = Some((child_pid, child_status));
            true
        } else {
            false
        }
    };

    if should_wake {
        let was_stopped = {
            let mut state = parent.state.lock();
            if matches!(*state, crate::posix::process::ProcState::Stopped) {
                *state = crate::posix::process::ProcState::Running;
                true
            } else {
                false
            }
        };
        if was_stopped {
            crate::posix::process::RUN_QUEUE
                .lock()
                .push_back(parent.pid);
        }
    }
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
                // POSIX: close all fds on exit so pipe-write ends drop immediately,
                // allowing blocked readers to see EOF without waiting for reap.
                proc.fds.lock().close_all();
                crate::println!("exit: pid {} status {}", pid, status);
                let children = proc.children.lock().clone();
                (proc.ppid.load(Ordering::Relaxed), children)
            }
        }
    };

    // Phase 2: Orphan reparenting — give all children to init (PID 1).
    if !children.is_empty() {
        let table = PROCESS_TABLE.lock();
        let mut init_arc = None;
        if let Some(init) = table.get(&1) {
            init_arc = Some(init.clone());
            let mut init_children = init.children.lock();
            for &child_pid in &children {
                if !init_children.contains(&child_pid) {
                    init_children.push(child_pid);
                }
            }
        }

        for &child_pid in &children {
            if let Some(child) = table.get(&child_pid) {
                child.ppid.store(1, Ordering::Relaxed);
                
                // If the child is ALREADY a zombie, we must wake init to reap it!
                if let Some(init) = &init_arc {
                    let c_state = *child.state.lock();
                    if let ProcState::Zombie(c_status) = c_state {
                        try_wake_parent(init, child_pid, c_status);
                    }
                }
            }
        }
    }

    // Phase 3: Wake parent if it is blocked waiting for this child (or any child).
    if ppid != 0 {
        let table = PROCESS_TABLE.lock();
        if let Some(parent) = table.get(&ppid) {
            try_wake_parent(parent, pid, status);
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
    let child = crate::posix::process::Process::new(ppid)?;
    let child_pid = child.pid;

    if let Some(parent_proc) = crate::posix::process::PROCESS_TABLE.lock().get(&ppid) {
        let parent_root_pa = (parent_proc.satp_val.load(core::sync::atomic::Ordering::Relaxed) & 0xFFFFFFFFFFF) << 12;
        let child_root_pa  = (child.satp_val.load(core::sync::atomic::Ordering::Relaxed) & 0xFFFFFFFFFFF) << 12;

        crate::mm::vmm::clone_user_space(parent_root_pa, child_root_pa).map_err(|e| {
            crate::println!("sys_fork: clone_user_space failed: {}", e);
            cleanup_process(child_pid);
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
        child_tf.regs[10] = 0; // fork() returns 0 in the child
        child_tf.sepc += 4; // advance past the ecall instruction
    }

    crate::posix::process::RUN_QUEUE.lock().push_back(child.pid);
    Ok(child.pid)
}

/// Replace current process image with new ELF (execve-like minimal).
/// Returns the entry point VA so the trap handler can install it into sepc.
/// arg0: path_ptr, arg1: path_len
pub fn sys_exec(path_ptr: *const u8, path_len: usize) -> Result<usize, &'static str> {
    let ppid = crate::posix::process::current_pid();
    let path_bytes = unsafe { core::slice::from_raw_parts(path_ptr, path_len) };
    let path = core::str::from_utf8(path_bytes).map_err(|_| "Invalid UTF-8 path")?;

    // Lookup and read file
    let vnode = crate::vfs::lookup_path(path)?;
    let stat = vnode.stat();
    if stat.size == 0 {
        return Err("Empty binary");
    }
    let mut file_data = alloc::vec::Vec::new();
    file_data.resize(stat.size, 0);
    vnode.read(0, &mut file_data)?;

    // Get current process and its old root
    let proc = crate::posix::process::PROCESS_TABLE
        .lock()
        .get(&ppid)
        .cloned()
        .ok_or("exec: current process not found")?;
    let old_satp    = proc.satp_val.load(core::sync::atomic::Ordering::Relaxed);
    let old_root_pa = (old_satp & 0xFFFFFFFFFFF) << 12; // PPN → PA

    // Create a fresh page table for the process
    let new_root_pa = crate::mm::vmm::PageTable::new_process_table()?;
    let new_ppn  = new_root_pa >> 12;
    let new_satp = (8usize << 60) | new_ppn;

    // Load ELF and stack into the NEW page table BEFORE touching the old one.
    // If we destroyed the old space first, PMM could reuse its root page for a
    // new mapping while the CPU still page-walks via the old satp — corruption.
    let new_pt = unsafe { &mut *(new_root_pa as *mut crate::mm::vmm::PageTable) };
    let entry_point = crate::posix::elf::load_elf(&file_data, new_pt)?;

    let stack_base = USER_STACK_TOP - (USER_STACK_PAGES * PAGE_SIZE);
    for i in 0..USER_STACK_PAGES {
        let va = stack_base + i * PAGE_SIZE;
        let frame = alloc_frame().ok_or("Failed to alloc user stack")?;
        new_pt.map_page(va, frame.pa(), PTE_R | PTE_W | PTE_U)?;
        frame.into_raw();
    }

    // Install the new page table into the process struct via AtomicUsize::store
    // (safe — no raw pointer cast through Arc needed), then switch the CPU to it
    // before freeing the old space so no stale satp walk occurs.
    proc.satp_val.store(new_satp, core::sync::atomic::Ordering::Relaxed);
    unsafe {
        riscv::register::satp::write(new_satp);
        core::arch::asm!("sfence.vma");
    }

    // Now safe: CPU uses new PT, old space can be freed.
    crate::mm::vmm::destroy_user_space(old_root_pa)?;

    // Return entry_point — trap handler sets sepc, sp, sstatus (satp already switched above).
    Ok(entry_point)
}

/// Remove a newly-created but not-yet-running process from the system
/// and free its resources.  Used by posix_spawn when setup fails after
/// Process::new has been called.
fn cleanup_process(pid: Pid) {
    // Remove from the parent's children list first.
    {
        let table = PROCESS_TABLE.lock();
        if let Some(proc) = table.get(&pid) {
            let ppid = proc.ppid.load(Ordering::Relaxed);
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
        table.get(&pid).map(|p| (p.kernel_stack_bottom, p.satp_val.load(core::sync::atomic::Ordering::Relaxed)))
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

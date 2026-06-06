use crate::trap::TrapFrame;
use core::sync::atomic::Ordering;

pub fn sys_clone_process(arg0: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_PROCESS == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    }
    let child = match crate::posix::process::Process::new(target_pid) {
        Ok(c) => c,
        Err(e) => {
            crate::println!("sys_clone_process failed: {}", e);
            tf.regs[10] = usize::MAX;
            return;
        }
    };
    let child_pid = child.pid;

    if let Some(target_proc) = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid) {
        let parent_root_pa = (target_proc.satp_val.load(Ordering::Relaxed) & 0xFFFFFFFFFFF) << 12;
        let child_root_pa = (child.satp_val.load(Ordering::Relaxed) & 0xFFFFFFFFFFF) << 12;

        if let Err(e) = crate::mm::vmm::clone_user_space(parent_root_pa, child_root_pa) {
            crate::println!("sys_clone_process: clone_user_space failed: {}", e);
            crate::posix::spawn::cleanup_process(child_pid);
            tf.regs[10] = usize::MAX;
            return;
        }

        unsafe { core::arch::asm!("sfence.vma") };

        let parent_tf = target_proc.trap_frame.lock();
        let mut child_tf = child.trap_frame.lock();
        let child_kernel_sp = child_tf.kernel_sp;
        *child_tf = *parent_tf;
        child_tf.kernel_sp = child_kernel_sp;

        // Copy IpcState so the child expects the reply
        *child.ipc_state.lock() = target_proc.ipc_state.lock().clone();
    }

    // Do NOT push to RUN_QUEUE yet; pm-server's msg_send will push it when it replies.
    
    tf.regs[10] = child_pid as usize;
}

pub fn sys_set_trapframe(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    let tf_ptr = arg1 as *const TrapFrame;
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & (crate::posix::process::CAP_PROCESS | crate::posix::process::CAP_SYS_ADMIN) == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    }

    if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid) {
        let mut target_tf = proc.trap_frame.lock();
        let kernel_sp = target_tf.kernel_sp;
        unsafe {
            *target_tf = *tf_ptr;
        }
        target_tf.kernel_sp = kernel_sp;
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_set_process_state(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    let state = arg1; // 0 = Stopped, 1 = Runnable
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_PROCESS == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    }

    if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid) {
        let mut st = proc.state.lock();
        if state == 1 {
            if *st == crate::posix::process::ProcState::Stopped {
                *st = crate::posix::process::ProcState::Running;
                crate::posix::process::RUN_QUEUE.lock().push_back(target_pid);
            }
        } else {
            *st = crate::posix::process::ProcState::Stopped;
        }
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_destroy_user_space(arg0: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & (crate::posix::process::CAP_PROCESS | crate::posix::process::CAP_SYS_ADMIN) == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    }

    if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid) {
        let satp = proc.satp_val.load(Ordering::Relaxed);
        let root_pa = (satp & 0xFFFFFFFFFFF) << 12;
        
        let new_root_pa = crate::mm::vmm::PageTable::new_process_table().unwrap();
        let new_satp = (8usize << 60) | (new_root_pa >> 12);
        
        proc.satp_val.store(new_satp, Ordering::Relaxed);
        
        // Destroy old
        let _ = crate::mm::vmm::destroy_user_space(root_pa);
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_alloc_user_page(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    let va = arg1;
    let flags = arg2; // mapping flags
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & (crate::posix::process::CAP_PROCESS | crate::posix::process::CAP_SYS_ADMIN) == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    }

    if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid) {
        let satp = proc.satp_val.load(Ordering::Relaxed);
        let root_pa = (satp & 0xFFFFFFFFFFF) << 12;

        let frame = match crate::mm::pmm::alloc_frame() {
            Some(f) => f,
            None => {
                tf.regs[10] = usize::MAX;
                return;
            }
        };

        if unsafe { &mut *(root_pa as *mut crate::mm::vmm::PageTable) }.map_page(va, frame.pa(), flags).is_err() {
            tf.regs[10] = usize::MAX;
            return;
        }
        frame.into_raw();
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_reap_process(arg0: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_PROCESS == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    }

    // Refuse to reap a process that is not already a zombie: reaping a running
    // process would free its kernel stack while another HART may be executing on it.
    {
        let table = crate::posix::process::PROCESS_TABLE.lock();
        match table.get(&target_pid) {
            None => { tf.regs[10] = usize::MAX; return; }
            Some(proc) => {
                if !matches!(*proc.state.lock(), crate::posix::process::ProcState::Zombie(_)) {
                    tf.regs[10] = crate::errno::ESRCH as usize;
                    return;
                }
            }
        }
    }

    let removed_proc = {
        let mut table = crate::posix::process::PROCESS_TABLE.lock();
        table.remove(&target_pid)
    };

    if let Some(proc) = removed_proc {
        let kstack = proc.kernel_stack_bottom;
        let satp = proc.satp_val.load(Ordering::Relaxed);
        let root_pa = (satp & 0xFFFFFFFFFFF) << 12;

        if kstack != 0 {
            const KSZ: usize = 65536;
            unsafe {
                alloc::alloc::dealloc(
                    kstack as *mut u8,
                    core::alloc::Layout::from_size_align(KSZ, 16).unwrap(),
                );
            }
        }
        if root_pa != 0 && crate::posix::process::satp_unshare(root_pa) {
            let _ = crate::mm::vmm::destroy_user_space(root_pa);
        }
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_phys_map(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let va = arg0;
    let pa = arg1;
    let len = arg2;

    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    
    if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_MMIO == 0 {
        tf.regs[10] = crate::errno::EPERM as usize;
        return;
    }

    let root_pa = (proc.satp_val.load(core::sync::atomic::Ordering::Relaxed) & 0xFFFFFFFFFFF) << 12;

    if va % 4096 != 0 || pa % 4096 != 0 || len % 4096 != 0 {
        tf.regs[10] = usize::MAX;
        return;
    }

    let root = unsafe { &mut *(root_pa as *mut crate::mm::vmm::PageTable) };
    let flags = crate::mm::vmm::PTE_R | crate::mm::vmm::PTE_W | crate::mm::vmm::PTE_U;

    for offset in (0..len).step_by(4096) {
        if root.map_page(va + offset, pa + offset, flags).is_err() {
            tf.regs[10] = usize::MAX;
            return;
        }
    }
    
    // Flush TLB for this process
    unsafe { core::arch::asm!("sfence.vma zero, zero") };

    tf.regs[10] = va;
}

pub fn sys_virt_to_phys(arg0: usize, tf: &mut TrapFrame) {
    let va = arg0;
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    let root_pa = (proc.satp_val.load(core::sync::atomic::Ordering::Relaxed) & 0xFFFFFFFFFFF) << 12;

    match crate::mm::vmm::PageTable::walk_page_table(root_pa, va) {
        Ok((pa, _flags)) => tf.regs[10] = pa,
        Err(_) => tf.regs[10] = usize::MAX,
    }
}

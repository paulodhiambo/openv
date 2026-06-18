//! # Process / VM Management Syscalls
//!
//! This module implements low-level process and virtual memory
//! management syscalls (numbers 100–109). These are the primitive
//! syscalls used by the `pm` (process manager) server. Regular
//! POSIX-style process operations live in [`crate::syscall::proc`].
//!
//! ## Capability Gating
//!
//! All syscalls in this module require the calling process to have
//! `CAP_PROCESS` or `CAP_SYS_ADMIN` (depending on the operation). If
//! the caller does not have the required capability, the syscall
//! returns `EPERM`. See [`crate::posix::process`] for the capability
//! bit definitions.

use crate::trap::TrapFrame;
use core::sync::atomic::Ordering;

/// Clones the address space of a target process into a new process.
///
/// Creates a [`Process`] with `target_pid` as the parent and copies
/// the parent's user-space page tables (with copy-on-write) into the
/// child. The child's trap frame is a copy of the parent's so the
/// child can resume at the same instruction. The child's IPC state
/// is also copied (so it can expect a reply if the parent was in a
/// send-receive state).
///
/// # Arguments
///
/// * `arg0` (`a0`) - Target parent PID to clone from.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// The new child's PID is written to `tf.regs[10]`. On failure,
/// `usize::MAX` is written.
///
/// # Capability
///
/// Requires `CAP_PROCESS`.
pub fn sys_clone_process(arg0: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    if let Some(proc) = crate::posix::process::get_current_proc()
        && proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_PROCESS == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
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

    // Leave child Stopped with mailbox_waiter=1 so pm-server's msg_send wakes it.
    // pm-server must send a reply to child_pid after setting its trap frame,
    // which will transition child to Running and push it to RUN_QUEUE.
    {
        let table = crate::posix::process::PROCESS_TABLE.lock();
        if let Some(child_proc) = table.get(&child_pid) {
            *child_proc.state.lock() = crate::posix::process::ProcState::Stopped;
            child_proc.mailbox_waiter.store(1, core::sync::atomic::Ordering::Relaxed);
        }
    }

    tf.regs[10] = child_pid as usize;
}

/// Sets the trap frame of a target process (used to start execution
/// at a specific address with a specific register state).
///
/// Copies the trap frame at `arg1` (a user-space pointer) into the
/// target process's saved trap frame. The target's `kernel_sp` is
/// preserved.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Target PID.
/// * `arg1` (`a1`) - Pointer to a [`TrapFrame`] in user-space.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// 0 on success, `usize::MAX` on failure.
///
/// # Capability
///
/// Requires `CAP_PROCESS` or `CAP_SYS_ADMIN`.
pub fn sys_set_trapframe(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    let tf_ptr = arg1 as *const TrapFrame;
    if let Some(proc) = crate::posix::process::get_current_proc()
        && proc.caps.load(core::sync::atomic::Ordering::Relaxed) & (crate::posix::process::CAP_PROCESS | crate::posix::process::CAP_SYS_ADMIN) == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }
    if !crate::mm::vmm::is_user_pointer_valid(tf, tf_ptr, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
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

/// Sets the run state of a target process.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Target PID.
/// * `arg1` (`a1`) - State: 0 = Stopped, 1 = Runnable.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// 0 on success, `usize::MAX` on failure.
///
/// # Capability
///
/// Requires `CAP_PROCESS`.
pub fn sys_set_process_state(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    let state = arg1; // 0 = Stopped, 1 = Runnable
    if let Some(proc) = crate::posix::process::get_current_proc()
        && proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_PROCESS == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
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

/// Destroys the user-space of a target process and gives it a new
/// empty page table.
///
/// Allocates a new page table via [`crate::mm::vmm::PageTable::new_process_table`]
/// and replaces the target process's `satp`. Then destroys the old
/// user-space.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Target PID.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// 0 on success, `usize::MAX` on failure.
///
/// # Capability
///
/// Requires `CAP_PROCESS` or `CAP_SYS_ADMIN`.
pub fn sys_destroy_user_space(arg0: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    if let Some(proc) = crate::posix::process::get_current_proc()
        && proc.caps.load(core::sync::atomic::Ordering::Relaxed) & (crate::posix::process::CAP_PROCESS | crate::posix::process::CAP_SYS_ADMIN) == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }

    if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid) {
        let satp = proc.satp_val.load(Ordering::Relaxed);
        let root_pa = (satp & 0xFFFFFFFFFFF) << 12;
        
        let new_root_pa = crate::mm::vmm::PageTable::new_process_table().unwrap();
        let new_satp = (8usize << 60) | (new_root_pa >> 12);
        
        proc.satp_val.store(new_satp, Ordering::Relaxed);
        
        // Destroy old
        let _ = crate::mm::vmm::destroy_user_space(root_pa);
        let allocated_bytes = proc.allocated_pages.swap(0, Ordering::SeqCst) * 4096;
        proc.job.dealloc_memory(allocated_bytes);
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

/// Allocates a single user page at the given VA with the given flags.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Target PID.
/// * `arg1` (`a1`) - Virtual address to map. Must be page-aligned.
/// * `arg2` (`a2`) - Mapping flags (PTE bits).
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// 0 on success, `usize::MAX` on failure.
///
/// # Capability
///
/// Requires `CAP_PROCESS` or `CAP_SYS_ADMIN`.
pub fn sys_alloc_user_page(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    let va = arg1;
    let flags = arg2; // mapping flags
    if let Some(proc) = crate::posix::process::get_current_proc()
        && proc.caps.load(core::sync::atomic::Ordering::Relaxed) & (crate::posix::process::CAP_PROCESS | crate::posix::process::CAP_SYS_ADMIN) == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }

    if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid) {
        if !proc.job.check_memory_limit(4096) {
            tf.regs[10] = usize::MAX;
            return;
        }
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
        proc.job.alloc_memory(4096);
        proc.allocated_pages.fetch_add(1, Ordering::SeqCst);
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

/// Reaps a zombie process, freeing its kernel stack and user-space.
///
/// The process must already be in the [`crate::posix::process::ProcState::Zombie`]
/// state. Reaping a running process would free its kernel stack
/// while another HART may be executing on it, which is unsafe.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Target PID to reap.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// 0 on success, `usize::MAX` on failure, `ESRCH` if the target is
/// not a zombie.
///
/// # Capability
///
/// Requires `CAP_PROCESS`.
pub fn sys_reap_process(arg0: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    if let Some(proc) = crate::posix::process::get_current_proc()
        && proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_PROCESS == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }

    // Refuse to reap a process that is not already a zombie: reaping a running
    // process would free its kernel stack while another HART may be executing on it.
    {
        let table = crate::posix::process::PROCESS_TABLE.lock();
        match table.get(&target_pid) {
            None => { tf.regs[10] = usize::MAX; return; }
            Some(proc) => {
                if !matches!(*proc.state.lock(), crate::posix::process::ProcState::Zombie(_)) {
                    tf.regs[10] = crate::errno::ESRCH;
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
        let allocated_bytes = proc.allocated_pages.load(Ordering::SeqCst) * 4096;
        proc.job.dealloc_memory(allocated_bytes);

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

/// Maps a range of physical memory into the current process's user
/// address space.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Virtual address. Must be page-aligned.
/// * `arg1` (`a1`) - Physical address. Must be page-aligned.
/// * `arg2` (`a2`) - Length in bytes. Must be page-aligned.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// The mapped VA on success, `usize::MAX` on failure.
///
/// # Capability
///
/// Requires `CAP_MMIO`.
///
/// # Safety
///
/// This syscall exposes physical memory to user-space. Drivers that
/// need to access MMIO registers use this syscall. Pages are mapped
/// as `R | W | U`.
pub fn sys_phys_map(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let va = arg0;
    let pa = arg1;
    let len = arg2;
    let proc = crate::get_current_proc_or_esrch!(tf);
    
    let caps = proc.caps.load(core::sync::atomic::Ordering::Relaxed);
    if caps & (crate::posix::process::CAP_MMIO | crate::posix::process::CAP_DRIVER) == 0 {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }

    let root_pa = (proc.satp_val.load(core::sync::atomic::Ordering::Relaxed) & 0xFFFFFFFFFFF) << 12;

    if !va.is_multiple_of(4096) || !pa.is_multiple_of(4096) || !len.is_multiple_of(4096) {
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

/// Walks the page table to translate a virtual address to a physical
/// address in the current process.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Virtual address to translate.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// The physical address on success, `usize::MAX` if the VA is not
/// mapped.
pub fn sys_virt_to_phys(arg0: usize, tf: &mut TrapFrame) {
    let va = arg0;
    let proc = crate::get_current_proc_or_esrch!(tf);
    let root_pa = (proc.satp_val.load(core::sync::atomic::Ordering::Relaxed) & 0xFFFFFFFFFFF) << 12;

    match crate::mm::vmm::PageTable::walk_page_table(root_pa, va) {
        Ok((pa, _flags)) => tf.regs[10] = pa,
        Err(_) => tf.regs[10] = usize::MAX,
    }
}

/// Creates a hardware resource capability handle.
///
/// Requires `CAP_SYS_ADMIN`. Returns a handle to a [`ResourceSpec`] that
/// grants access to the specified MMIO range and/or IRQ line.
///
/// # Arguments
/// * `arg0` (`a0`) — Physical base address of the MMIO region (0 if IRQ-only).
/// * `arg1` (`a1`) — Size in bytes of the MMIO region (0 if IRQ-only).
/// * `arg2` (`a2`) — IRQ number, or `u32::MAX` (0xffffffff) if no IRQ.
pub fn sys_create_resource(arg0: usize, arg1: usize, arg2: usize, tf: &mut crate::trap::TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);

    if proc.caps.load(core::sync::atomic::Ordering::Relaxed)
        & crate::posix::process::CAP_SYS_ADMIN
        == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }

    let irq = if arg2 == usize::MAX { None } else { Some(arg2 as u32) };
    let spec = crate::ipc::handle::ResourceSpec { pa: arg0, pa_size: arg1, irq };
    let handle = proc.fds.lock().insert(crate::ipc::handle::KernelObject::Resource(spec));
    tf.regs[10] = handle as usize;
}

/// Grants `CAP_DRIVER` capability to a target process.
///
/// Requires `CAP_SYS_ADMIN`. Allows the target process to call
/// `sys_phys_map` and `sys_irq_register`/`sys_irq_enable`.
///
/// # Arguments
/// * `arg0` (`a0`) — PID of the target process.
pub fn sys_grant_driver_cap(arg0: usize, tf: &mut crate::trap::TrapFrame) {
    let caller = crate::get_current_proc_or_esrch!(tf);

    if caller.caps.load(core::sync::atomic::Ordering::Relaxed)
        & crate::posix::process::CAP_SYS_ADMIN
        == 0
    {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }

    let target_pid = arg0 as i32;
    match crate::posix::process::PROCESS_TABLE.lock().get(&target_pid).cloned() {
        Some(target) => {
            target.caps.fetch_or(
                crate::posix::process::CAP_DRIVER,
                core::sync::atomic::Ordering::SeqCst,
            );
            tf.regs[10] = 0;
        }
        None => tf.regs[10] = crate::errno::ESRCH,
    }
}

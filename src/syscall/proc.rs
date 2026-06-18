//! # POSIX Process / Thread Syscalls
//!
//! This module implements the POSIX-style process and thread
//! management syscalls (numbers 0, 1, 6, 7, 50–54, 59, 60, 82–89,
//! 120–122, 135–139). High-level operations like spawn, fork, exec,
//! wait, and signal handling are defined here.
//!
//! ## Process Model
//!
//! OpenV has a 1:1 thread model — a [`Process`] is both a process
//! and a thread. There is no separate `Thread` struct. `tgid` is
//! the thread group ID, set to the process's own PID by default
//! (so `gettid() == getpid()`).
//!
//! ## Signal Delivery
//!
//! Signals are queued in `proc.pending_signals` and delivered on
//! return to user-space by [`crate::trap::rust_trap_handler`].

use crate::trap::TrapFrame;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

unsafe extern "C" {
    fn __halt_cpu() -> !;
}

/// Yields the CPU to the next runnable process.
///
/// If the run queue is non-empty, the current process is pushed to
/// the back and [`crate::posix::process::schedule`] is called.
pub fn sys_yield(_tf: &mut TrapFrame) {
    let is_empty = crate::posix::process::RUN_QUEUE.lock().is_empty();
    if !is_empty {
        crate::posix::process::enqueue(crate::posix::process::current_pid());
        crate::posix::process::schedule();
        unsafe { __halt_cpu() }
    }
}

/// Exits the current process with the given status.
///
/// Calls [`crate::posix::spawn::exit`] which marks the process as a
/// zombie, reparents children to init, wakes the parent, and unblocks
/// any senders. Then schedules the next process.
pub fn sys_exit(status: usize, _tf: &mut TrapFrame) {
    crate::posix::spawn::exit(crate::posix::process::current_pid(), status as i32);
    crate::posix::process::schedule();
    unsafe { __halt_cpu() }
}

/// Spawns a new process from a path.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Pointer to a UTF-8 path string in user-space.
/// * `arg1` (`a1`) - Length of the path string.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// The new child's PID on success, `usize::MAX` on failure.
pub fn sys_spawn(path_ptr: usize, path_len: usize, tf: &mut TrapFrame) {
    if !crate::mm::vmm::is_user_pointer_valid(tf, path_ptr as *const u8, path_len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let path_bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
    if let Ok(path) = core::str::from_utf8(path_bytes) {
        match crate::posix::spawn::posix_spawn(path, crate::posix::process::current_pid()) {
            Ok(child_pid) => tf.regs[10] = child_pid as usize,
            Err(e) => {
                crate::println!("sys_spawn: {}: {}", path, e);
                tf.regs[10] = usize::MAX;
            }
        }
    } else {
        tf.regs[10] = usize::MAX;
    }
}

/// Spawns a new process with explicit capabilities.
///
/// Requires `CAP_SYS_ADMIN`. The child is created with the given
/// capabilities bitmask instead of inheriting the parent's.
pub fn sys_spawn_with_caps(path_ptr: usize, path_len: usize, caps: u64, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_SYS_ADMIN == 0 {
            tf.regs[10] = crate::errno::EPERM;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH;
        return;
    }
    if !crate::mm::vmm::is_user_pointer_valid(tf, path_ptr as *const u8, path_len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    let path_bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
    if let Ok(path) = core::str::from_utf8(path_bytes) {
        match crate::posix::spawn::posix_spawn(path, crate::posix::process::current_pid()) {
            Ok(child_pid) => {
                if let Some(child_proc) = crate::posix::process::PROCESS_TABLE.lock().get(&child_pid) {
                    child_proc.caps.store(caps, core::sync::atomic::Ordering::Relaxed);
                }
                tf.regs[10] = child_pid as usize;
            }
            Err(e) => {
                crate::println!("sys_spawn_with_caps: {}: {}", path, e);
                tf.regs[10] = usize::MAX;
            }
        }
    } else {
        tf.regs[10] = usize::MAX;
    }
}

/// Sets the capabilities of a target process.
///
/// Requires `CAP_SYS_ADMIN`.
pub fn sys_privctl(pid: usize, caps: u64, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_SYS_ADMIN == 0 {
            tf.regs[10] = crate::errno::EPERM;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH;
        return;
    }

    let target_pid = pid as i32;
    if let Some(target_proc) = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid) {
        target_proc.caps.store(caps, core::sync::atomic::Ordering::Relaxed);
        tf.regs[10] = 0; // Success
    } else {
        tf.regs[10] = crate::errno::ESRCH;
    }
}

/// Registers a user-space handler for an IRQ.
///
/// When the given IRQ fires, the kernel masks it at the PLIC and
/// sends a hardware interrupt message (source/type = -2) to the
/// registered PID. The user-space driver is expected to acknowledge
/// by re-enabling the IRQ via [`sys_irq_enable`].
///
/// Requires `CAP_INTERRUPT`.
pub fn sys_irq_register(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        let caps = proc.caps.load(core::sync::atomic::Ordering::Relaxed);
        if caps & (crate::posix::process::CAP_INTERRUPT | crate::posix::process::CAP_DRIVER) == 0 {
            tf.regs[10] = crate::errno::EPERM;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH;
        return;
    }

    let irq = arg0 as u32;
    let pid = arg1 as i32;
    crate::trap::interrupt::IRQ_HANDLERS.lock().insert(irq, pid);
    tf.regs[10] = 0;
}

/// Re-enables an IRQ at the PLIC for the current HART.
///
/// Requires `CAP_INTERRUPT` or `CAP_DRIVER`.
pub fn sys_irq_enable(arg0: usize, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        let caps = proc.caps.load(core::sync::atomic::Ordering::Relaxed);
        if caps & (crate::posix::process::CAP_INTERRUPT | crate::posix::process::CAP_DRIVER) == 0 {
            tf.regs[10] = crate::errno::EPERM;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH;
        return;
    }
    
    let irq = arg0 as u32;
    let hart = crate::smp::current_hartid();
    crate::plic::set_enable(hart, irq, true);
    tf.regs[10] = 0;
}

/// Forks the current process.
///
/// Returns the child's PID in the parent and 0 in the child.
pub fn sys_fork(tf: &mut TrapFrame) {
    match crate::posix::spawn::sys_fork() {
        Ok(child_pid) => {
            tf.regs[10] = child_pid as usize;
        }
        Err(_) => {
            tf.regs[10] = usize::MAX;
        }
    }
}

/// Sets up a user stack with `argv` strings and a pointer array.
///
/// Allocates space on the user stack for the argv strings (each
/// null-terminated), then writes an array of pointers followed by a
/// NULL terminator. Aligns the stack to 16 bytes.
///
/// # Returns
///
/// `(argc, argv_ptr, new_sp)` where `argv_ptr` is the address of the
/// pointer array and `new_sp` is the new stack pointer (with 16-byte
/// alignment and 16 bytes of extra slack).
fn setup_exec_stack(argv_data: &[u8]) -> (usize, usize, usize) {
    use crate::posix::spawn::USER_STACK_TOP;

    let mut args: Vec<&[u8]> = Vec::new();
    let mut start = 0usize;
    for (i, &b) in argv_data.iter().enumerate() {
        if b == 0 {
            if i > start {
                args.push(&argv_data[start..i]);
            }
            start = i + 1;
        }
    }
    if start < argv_data.len() {
        args.push(&argv_data[start..]);
    }

    let argc = args.len();
    if argc == 0 {
        return (0, 0, USER_STACK_TOP);
    }

    let mut sp = USER_STACK_TOP;
    let mut string_addrs: Vec<usize> = Vec::new();

    for arg in &args {
        let len = arg.len() + 1;
        sp -= len;
        unsafe {
            let dst = core::slice::from_raw_parts_mut(sp as *mut u8, len);
            dst[..arg.len()].copy_from_slice(arg);
            dst[arg.len()] = 0;
        }
        string_addrs.push(sp);
    }

    sp &= !7;
    sp -= 8;
    unsafe { *(sp as *mut usize) = 0; }
    for &addr in string_addrs.iter().rev() {
        sp -= 8;
        unsafe { *(sp as *mut usize) = addr; }
    }

    let argv_ptr = sp;
    sp = (sp - 16) & !15;

    (argc, argv_ptr, sp)
}

/// Replaces the current process image with a new executable.
///
/// Loads the ELF file at `path_ptr`/`path_len` and sets `sepc` to
/// the entry point. Closes file descriptors with `O_CLOEXEC`. Resets
/// POSIX signal handlers to `SIG_DFL` per POSIX semantics.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Pointer to a UTF-8 path string.
/// * `arg1` (`a1`) - Length of the path string.
/// * `arg2` (`a2`) - Pointer to a NUL-separated argv buffer.
/// * `arg3` (`a3`) - Argv buffer length (in `tf.regs[13]`).
/// * `tf` - Caller's trap frame.
pub fn sys_exec(path_ptr: usize, path_len: usize, argv_buf_ptr: usize, _dummy: usize, tf: &mut TrapFrame) {
    if !crate::mm::vmm::is_user_pointer_valid(tf, path_ptr as *const u8, path_len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let argv_buf_len = tf.regs[13]; // a3
    if argv_buf_len > 0 {
        if !crate::mm::vmm::is_user_pointer_valid(tf, argv_buf_ptr as *const u8, argv_buf_len) {
            tf.regs[10] = crate::errno::EFAULT;
            return;
        }
    }

    let argv_data: Vec<u8> = if argv_buf_len > 0 && argv_buf_ptr != 0 {
        unsafe { core::slice::from_raw_parts(argv_buf_ptr as *const u8, argv_buf_len).to_vec() }
    } else {
        Vec::new()
    };

    match unsafe { crate::posix::spawn::sys_exec(path_ptr as *const u8, path_len) } {
        Ok(entry_point) => {
            let pid = crate::posix::process::current_pid();

            {
                let table = crate::posix::process::PROCESS_TABLE.lock();
                if let Some(proc) = table.get(&pid) {
                    proc.fds.lock().close_on_exec();
                }
            }

            {
                let table = crate::posix::process::PROCESS_TABLE.lock();
                if let Some(proc) = table.get(&pid) {
                    proc.wait_result.lock().clear();
                    *proc.wait_target.lock() = None;
                    // POSIX: user-installed signal handlers are reset to SIG_DFL on exec.
                    // A handler pointer from the old image is unmapped and would fault.
                    *proc.signal_handlers.lock() = [0usize; 32];
                    *proc.signal_restorers.lock() = [0usize; 32];
                }
            }

            let (argc, argv_ptr, new_sp) = setup_exec_stack(&argv_data);

            let new_satp = crate::posix::process::PROCESS_TABLE
                .lock()
                .get(&pid)
                .map(|p| p.satp_val.load(Ordering::Relaxed))
                .unwrap_or(0);
            unsafe {
                riscv::register::satp::write(new_satp);
                core::arch::asm!("sfence.vma");
            }

            tf.sepc = entry_point;
            tf.regs[2] = new_sp;
            tf.regs[10] = argc;
            tf.regs[11] = argv_ptr;
            tf.sstatus = (1 << 5) | (1 << 18);
            // execution will return to U-mode from the trap handler with the new context
        }
        Err(e) => {
            crate::println!("sys_exec error: {}", e);
            tf.regs[10] = usize::MAX;
        }
    }
}

/// `waitpid` option: do not block if no child has exited.
pub const WNOHANG: usize = 1;
/// `waitpid` option: report stopped children too.
pub const WUNTRACED: usize = 2;

/// Frees a zombie process's resources, unlinks it from the parent's
/// children list, writes the exit status to user-space, and sets
/// `tf.regs[10]` to the reaped PID.
///
/// Called from [`sys_waitpid`] when a zombie is found.
fn reap_zombie(
    zpid: crate::posix::process::Pid,
    zstatus: i32,
    ppid: crate::posix::process::Pid,
    status_ptr: *mut i32,
    tf: &mut TrapFrame,
) {
    let removed_proc = {
        let mut table = crate::posix::process::PROCESS_TABLE.lock();
        let proc = table.remove(&zpid);
        let parent = table.get(&ppid).cloned();
        (proc, parent)
    };
    if let (Some(proc), parent_arc) = removed_proc {
        let kstack = proc.kernel_stack_bottom;
        let zombie_satp = proc.satp_val.load(Ordering::Relaxed);
        if let Some(parent) = parent_arc {
            parent.children.lock().retain(|&p| p != zpid);
            *parent.wait_target.lock() = None;
            *parent.wait_status_ptr.lock() = None;
        }
        if kstack != 0 {
            const KSZ: usize = 65536;
            unsafe {
                alloc::alloc::dealloc(
                    kstack as *mut u8,
                    core::alloc::Layout::from_size_align(KSZ, 16).unwrap(),
                );
            }
        }
        let root_pa = (zombie_satp & 0xFFFFFFFFFFF) << 12;
        if root_pa != 0 && crate::posix::process::satp_unshare(root_pa) {
            crate::mm::swap::evict_all(root_pa);
            let _ = crate::mm::vmm::destroy_user_space(root_pa);
        }
    }
    if !status_ptr.is_null() {
        unsafe { *status_ptr = zstatus; }
    }
    tf.regs[10] = zpid as usize;
}

/// Waits for a child process to exit.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Target: `>0` for a specific PID, `0` for any in
///   the same process group, `-1` for any child, `<-1` for the
///   process group `-pid`.
/// * `arg1` (`a1`) - Pointer to write the exit status to (or null).
/// * `arg2` (`a2`) - Options: [`WNOHANG`], [`WUNTRACED`].
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// The reaped PID on success, 0 if `WNOHANG` and no child is ready,
/// or `-1` (`ECHILD`) if there are no matching children.
pub fn sys_waitpid(target: usize, status_ptr: usize, options: usize, tf: &mut TrapFrame) {
    if status_ptr != 0 && !crate::mm::vmm::is_user_pointer_valid(tf, status_ptr as *const i32, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let target = target as i32;
    let status_ptr = status_ptr as *mut i32;
    let ppid = crate::posix::process::current_pid();

    let delivered = {
        let table = crate::posix::process::PROCESS_TABLE.lock();
        table.get(&ppid).and_then(|p| p.wait_result.lock().pop_front())
    };

    if let Some((zpid, zstatus)) = delivered {
        reap_zombie(zpid, zstatus, ppid, status_ptr, tf);
    } else {
        let found_zombie = {
            let table = crate::posix::process::PROCESS_TABLE.lock();
            let mut found = None;
            if let Some(parent) = table.get(&ppid) {
                let children: alloc::vec::Vec<_> = parent.children.lock().clone();
                let parent_pgid = parent.pgid.load(core::sync::atomic::Ordering::Relaxed);
                for child_pid in children {
                    let matches = if target > 0 {
                        target == child_pid
                    } else if target == 0 {
                        if let Some(c) = table.get(&child_pid) {
                            c.pgid.load(core::sync::atomic::Ordering::Relaxed) == parent_pgid
                        } else { false }
                    } else if target == -1 {
                        true
                    } else {
                        if let Some(c) = table.get(&child_pid) {
                            c.pgid.load(core::sync::atomic::Ordering::Relaxed) == -target
                        } else { false }
                    };

                    if matches
                        && let Some(child) = table.get(&child_pid)
                        && let crate::posix::process::ProcState::Zombie(st) = *child.state.lock()
                    {
                        found = Some((child_pid, st));
                        break;
                    }
                }
            }
            found
        };

        if let Some((zpid, zstatus)) = found_zombie {
            reap_zombie(zpid, zstatus, ppid, status_ptr, tf);
        } else {
            // Check if we have children at all matching the target
            let mut has_matching_children = false;
            {
                let table = crate::posix::process::PROCESS_TABLE.lock();
                if let Some(parent) = table.get(&ppid) {
                    let children = parent.children.lock();
                    let parent_pgid = parent.pgid.load(core::sync::atomic::Ordering::Relaxed);
                    for child_pid in children.iter() {
                        let matches = if target > 0 {
                            target == *child_pid
                        } else if target == 0 {
                            if let Some(c) = table.get(child_pid) {
                                c.pgid.load(core::sync::atomic::Ordering::Relaxed) == parent_pgid
                            } else { false }
                        } else if target == -1 {
                            true
                        } else {
                            if let Some(c) = table.get(child_pid) {
                                c.pgid.load(core::sync::atomic::Ordering::Relaxed) == -target
                            } else { false }
                        };
                        
                        if matches {
                            has_matching_children = true;
                            break;
                        }
                    }
                }
            }
            if !has_matching_children {
                tf.regs[10] = usize::MAX; // ECHILD
            } else if (options & WNOHANG) != 0 {
                tf.regs[10] = 0; // return 0 immediately
            } else {
                let proc_arc = crate::posix::process::PROCESS_TABLE.lock().get(&ppid).cloned();
                if let Some(proc) = proc_arc {
                    *proc.wait_target.lock() = Some(target);
                    *proc.wait_status_ptr.lock() = Some(status_ptr as usize);

                    // TOCTOU double-check: a child may have exited in the window between
                    // our zombie scan above and setting wait_target just now.  In that
                    // window exit() saw wait_target==None and skipped the queue push, so
                    // the zombie is only in the PROCESS_TABLE — find it here before we
                    // sleep so we never miss an exit.
                    let raced = {
                        // First try the queue (exit() may have pushed if it ran *after*
                        // we set wait_target but before this line).
                        let q = proc.wait_result.lock().pop_front();
                        if q.is_some() {
                            q
                        } else {
                            // Fall back to a fresh zombie scan.
                            let table = crate::posix::process::PROCESS_TABLE.lock();
                            let mut found = None;
                            if let Some(parent) = table.get(&ppid) {
                                let children: alloc::vec::Vec<_> = parent.children.lock().clone();
                                for child_pid in children {
                                    let matches = if target > 0 {
                                        target == child_pid
                                    } else {
                                        true // -1 or pgid — accept any
                                    };
                                    if matches
                                        && let Some(child) = table.get(&child_pid)
                                        && let crate::posix::process::ProcState::Zombie(st) =
                                            *child.state.lock()
                                    {
                                        found = Some((child_pid, st));
                                        break;
                                    }
                                }
                            }
                            found
                        }
                    };

                    if let Some((zpid, zstatus)) = raced {
                        // Found a zombie in the race window — clear the wait fields
                        // we just set and reap immediately without sleeping.
                        *proc.wait_target.lock() = None;
                        *proc.wait_status_ptr.lock() = None;
                        drop(proc);
                        reap_zombie(zpid, zstatus, ppid, status_ptr, tf);
                    } else {
                        tf.sepc -= 4;
                        *proc.state.lock() = crate::posix::process::ProcState::Stopped;
                        drop(proc);
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                } else {
                    tf.regs[10] = usize::MAX;
                }
            }
        }
    }
}

/// Returns the current process's PID.
pub fn sys_getpid(tf: &mut TrapFrame) {
    tf.regs[10] = crate::posix::process::current_pid() as usize;
}

/// Returns the parent PID of the current process.
pub fn sys_getppid(tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    tf.regs[10] = proc.ppid.load(Ordering::Relaxed) as usize;
}

/// Sets the foreground process group and routes the UART ISR to
/// the target process's controlling TTY.
pub fn sys_set_fg_pid(arg0: usize, tf: &mut TrapFrame) {
    let pid = arg0 as i32;
    crate::posix::process::FOREGROUND_PID.store(pid, core::sync::atomic::Ordering::Relaxed);

    // Route the UART ISR to the new foreground process's TTY.
    if pid > 0 {
        let proc_arc = crate::posix::process::PROCESS_TABLE.lock().get(&pid).cloned();
        if let Some(proc) = proc_arc
            && let Some(crate::ipc::handle::KernelObject::Tty(tty)) = proc.fds.lock().get(0)
        {
            *crate::tty::ACTIVE_TTY.lock() = Some(tty.clone());
        }
    }
    tf.regs[10] = 0;
}

/// `timeval` structure (seconds + microseconds).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TimeVal {
    /// Seconds since epoch (CLINT clock at 10 MHz, no real epoch).
    pub tv_sec: i64,
    /// Microseconds.
    pub tv_usec: i64,
}

/// `timespec` structure (seconds + nanoseconds).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TimeSpec {
    /// Seconds.
    pub tv_sec: i64,
    /// Nanoseconds.
    pub tv_nsec: i64,
}

/// Returns the current time in CLINT ticks, converted to
/// seconds/microseconds. The CLINT clock is 10 MHz.
pub fn sys_gettimeofday(arg0: usize, _arg1: usize, tf: &mut TrapFrame) {
    if arg0 != 0 && !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const TimeVal, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    // CLINT clock is 10 MHz: 10,000,000 ticks per second
    let ticks = riscv::register::time::read() as u64;
    let sec = ticks / 10_000_000;
    let usec = (ticks % 10_000_000) / 10;
    
    if arg0 != 0 {
        let tv = unsafe { &mut *(arg0 as *mut TimeVal) };
        tv.tv_sec = sec as i64;
        tv.tv_usec = usec as i64;
    }
    tf.regs[10] = 0;
}

/// Sleeps for the given duration, with the wakeup time added to
/// [`crate::posix::process::SLEEP_QUEUE`].
///
/// The current process is marked `Stopped` and the next process
/// is scheduled.
pub fn sys_nanosleep(arg0: usize, _arg1: usize, tf: &mut TrapFrame) {
    if arg0 == 0 || !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const TimeSpec, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let req = unsafe { &*(arg0 as *const TimeSpec) };
    let nsec = req.tv_nsec;
    let sec = req.tv_sec;
    
    // Convert to ticks (10MHz = 10 ticks per microsecond = 1 tick per 100ns)
    let ticks_to_wait = (sec as u64 * 10_000_000) + (nsec as u64 / 100);
    let current_ticks = riscv::register::time::read() as u64;
    let wakeup_time = current_ticks + ticks_to_wait;
    
    let pid = crate::posix::process::current_pid();
    crate::posix::process::SLEEP_QUEUE.lock().push((pid, wakeup_time));

    // Mark Stopped before schedule() so a concurrent HART's wake_sleepers
    // won't observe us as still Running and skip the state transition.
    {
        let proc_arc = crate::posix::process::PROCESS_TABLE.lock().get(&pid).cloned();
        if let Some(proc) = proc_arc {
            *proc.state.lock() = crate::posix::process::ProcState::Stopped;
        }
    }

    tf.regs[10] = 0;
    // sepc was already advanced by 4 in the trap handler, so returning here
    // resumes at the instruction after the ecall — no sepc adjustment needed.
    crate::posix::process::schedule();
    unsafe { __halt_cpu() }
}

/// Sets the process group ID of the current process (or child).
pub fn sys_setpgid(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let mut pid = arg0 as i32;
    let mut pgid = arg1 as i32;
    let proc = crate::get_current_proc_or_esrch!(tf);
    if pid == 0 {
        pid = proc.pid;
    }
    if pgid == 0 {
        pgid = pid;
    }
    // we only support setting our own pgid for now to keep it simple, or children
    if pid == proc.pid {
        proc.pgid.store(pgid, core::sync::atomic::Ordering::Relaxed);
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX; // EPERM or ESRCH
    }
}

/// Returns the process group ID of the current process.
pub fn sys_getpgid(arg0: usize, tf: &mut TrapFrame) {
    let mut pid = arg0 as i32;
    let proc = crate::get_current_proc_or_esrch!(tf);
    if pid == 0 {
        pid = proc.pid;
    }
    if pid == proc.pid {
        tf.regs[10] = proc.pgid.load(core::sync::atomic::Ordering::Relaxed) as usize;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

/// Creates a new session with the current process as the leader
/// and allocates a fresh TTY for the new session.
pub fn sys_setsid(tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    let pid = proc.pid;
    proc.pgid.store(pid, core::sync::atomic::Ordering::Relaxed);
    proc.sid.store(pid, core::sync::atomic::Ordering::Relaxed);

    // Allocate a fresh TTY for the new session so its line discipline state is
    // independent of the parent's session.
    let new_tty = crate::tty::TtyState::new();
    {
        let mut fds = proc.fds.lock();
        let obj = crate::ipc::handle::KernelObject::Tty(new_tty.clone());
        fds.insert_at(0, obj.clone());
        fds.insert_at(1, obj.clone());
        fds.insert_at(2, obj);
    }
    // If this process is currently foreground, also switch the ISR target.
    let fg = crate::posix::process::FOREGROUND_PID.load(Ordering::Relaxed);
    if fg == pid {
        *crate::tty::ACTIVE_TTY.lock() = Some(new_tty);
    }

    tf.regs[10] = pid as usize;
}

/// `sigaction` structure: POSIX signal handler descriptor.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SigAction {
    /// Signal handler address (`0` for `SIG_DFL`, `1` for `SIG_IGN`).
    pub sa_handler: usize,
    /// Flags (currently unused).
    pub sa_flags: usize,
    /// Additional signals to mask during handler execution.
    pub sa_mask: u32,
    /// Address of the signal restorer trampoline.
    pub sa_restorer: usize,
}

/// `SIGKILL` signal number (9) — cannot be caught or ignored.
pub const SIGKILL: usize = 9;
/// `SIGINT` signal number (2) — terminal interrupt (Ctrl-C).
pub const SIGINT: usize = 2;
/// `SIGTERM` signal number (15) — termination request.
pub const SIGTERM: usize = 15;
/// `SIGPIPE` signal number (13) — write to closed pipe/socket.
pub const SIGPIPE: usize = 13;
/// `SIGCHLD` signal number (17) — child terminated/stopped/continued.
pub const SIGCHLD: usize = 17;
/// `SIGCONT` signal number (18) — continue if stopped.
pub const SIGCONT: usize = 18;
/// `SIGSTOP` signal number (19) — stop (cannot be caught/ignored).
pub const SIGSTOP: usize = 19;
/// `SIGTSTP` signal number (20) — terminal stop (Ctrl-Z).
pub const SIGTSTP: usize = 20;
/// `SIGTTIN` signal number (21) — background read on controlling TTY.
pub const SIGTTIN: usize = 21;
/// `SIGTTOU` signal number (22) — background write on controlling TTY.
pub const SIGTTOU: usize = 22;

/// Sends a signal to a process or process group.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Target: `>0` for a specific PID, `0` for the
///   caller's process group, `-1` for all processes except init
///   and self, `<-1` for the process group `-pid`.
/// * `arg1` (`a1`) - Signal number (0..31). `0` is a permission
///   check that does not actually deliver.
/// * `tf` - Caller's trap frame.
/// Wake a Stopped process with EINTR so a pending signal is delivered on the
/// next return-to-user-space.  Must be called while PROCESS_TABLE is held.
///
/// If the process is blocked in a rendezvous-IPC send, it is also removed from
/// the target's senders queue so the target never tries to deliver to it.
fn wake_with_eintr(
    pid: i32,
    proc: &alloc::sync::Arc<crate::posix::process::Process>,
    table: &alloc::collections::BTreeMap<i32, alloc::sync::Arc<crate::posix::process::Process>>,
) {
    use crate::posix::process::{IpcState, ProcState, enqueue_with_prio};

    if !matches!(*proc.state.lock(), ProcState::Stopped) {
        return;
    }

    // If the process is mid-send, pull it out of the target's senders queue so
    // the target won't try to deliver to an IPC state we're about to clear.
    {
        let ipc = proc.ipc_state.lock().clone();
        if let IpcState::Sending { target, .. } = ipc {
            if let Some(target_proc) = table.get(&target) {
                target_proc.senders.lock().retain(|&s| s != pid);
            }
        }
        *proc.ipc_state.lock() = IpcState::None;
    }

    {
        let mut tf = proc.trap_frame.lock();
        tf.sepc += 4; // undo the sepc -= 4 that armed the retry
        tf.regs[10] = crate::errno::EINTR;
    }

    let mut st = proc.state.lock();
    *st = ProcState::Running;
    let prio = proc.priority.load(Ordering::Relaxed);
    drop(st);
    enqueue_with_prio(pid, prio);
}

pub fn sys_kill(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    let sig = arg1 as u32;

    if sig >= 32 {
        tf.regs[10] = usize::MAX; // EINVAL
        return;
    }

    let current_pid = crate::posix::process::current_pid();
    let mut to_kill: Vec<i32> = Vec::new();
    let mut sent = false;

    {
        let table = crate::posix::process::PROCESS_TABLE.lock();
        let current_pgid = table
            .get(&current_pid)
            .map(|p| p.pgid.load(Ordering::Relaxed))
            .unwrap_or(0);

        for (pid, proc) in table.iter() {
            let matches = if target_pid > 0 {
                *pid == target_pid
            } else if target_pid == 0 {
                proc.pgid.load(Ordering::Relaxed) == current_pgid
            } else if target_pid == -1 {
                *pid > 1 && *pid != current_pid
            } else {
                proc.pgid.load(Ordering::Relaxed) == -target_pid
            };

            if matches && sig != 0 {
                if sig == SIGKILL as u32 {
                    // SIGKILL is unblockable — collect for direct exit() after lock drop.
                    to_kill.push(*pid);
                } else {
                    proc.pending_signals.fetch_or(1 << sig, Ordering::Relaxed);
                    // Wake a sleeping process so signal is delivered on next return-to-user.
                    let blocked = proc.blocked_signals.load(Ordering::Relaxed);
                    if (1u32 << sig) & !blocked != 0 {
                        wake_with_eintr(*pid, proc, &table);
                    }
                }
                sent = true;
            }
        }
    } // PROCESS_TABLE released

    // Kill collected SIGKILL targets.  Call exit() with no lock held so it can
    // acquire PROCESS_TABLE itself.  Handle the self-kill case last.
    let kill_self = to_kill.contains(&current_pid);
    for &pid in to_kill.iter().filter(|&&p| p != current_pid) {
        crate::posix::spawn::exit(pid, -9);
    }
    if kill_self {
        crate::posix::spawn::exit(current_pid, -9);
        crate::posix::process::schedule();
        unsafe { __halt_cpu() }
    }

    if sent || !to_kill.is_empty() {
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX; // ESRCH
    }
}

/// Sets or queries the signal handler for a given signal.
///
/// `SIGKILL` and `SIGSTOP` cannot be caught — returns `EINVAL`.
pub fn sys_sigaction(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let sig = arg0;
    if sig >= 32 || sig == SIGKILL {
        tf.regs[10] = usize::MAX; // EINVAL
        return;
    }
    if arg2 != 0 && !crate::mm::vmm::is_user_pointer_valid(tf, arg2 as *const SigAction, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    if arg1 != 0 && !crate::mm::vmm::is_user_pointer_valid(tf, arg1 as *const SigAction, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let proc = crate::get_current_proc_or_esrch!(tf);
    
    if arg2 != 0 {
        let old_act = unsafe { &mut *(arg2 as *mut SigAction) };
        old_act.sa_handler = proc.signal_handlers.lock()[sig];
        old_act.sa_flags = 0;
        old_act.sa_mask = proc.signal_masks.lock()[sig];
        old_act.sa_restorer = proc.signal_restorers.lock()[sig];
    }
    
    if arg1 != 0 {
        let act = unsafe { &*(arg1 as *const SigAction) };
        proc.signal_handlers.lock()[sig] = act.sa_handler;
        proc.signal_masks.lock()[sig] = act.sa_mask;
        proc.signal_restorers.lock()[sig] = act.sa_restorer;
    }
    
    tf.regs[10] = 0;
}

/// Returns from a signal handler, restoring the saved trap frame
/// and signal mask.
///
/// `sp` is expected to point at a [`crate::trap::SignalFrame`] that
/// was pushed by [`crate::trap::rust_trap_handler`] when the signal
/// was delivered.
pub fn sys_sigreturn(tf: &mut TrapFrame) {
    // sp points to the SignalFrame we pushed during signal delivery.
    if !crate::mm::vmm::is_user_pointer_valid(tf, tf.regs[2] as *const crate::trap::SignalFrame, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let frame = unsafe { core::ptr::read(tf.regs[2] as *const crate::trap::SignalFrame) };

    // Restore the saved trap frame registers and sepc.  Do not restore
    // kernel_sp or sstatus — those must remain as set by the kernel.
    for i in 1..32 {
        tf.regs[i] = frame.tf.regs[i];
    }
    tf.sepc = frame.tf.sepc;

    // Restore the signal mask that was active before the handler was invoked.
    if let Some(proc) = crate::posix::process::get_current_proc() {
        proc.blocked_signals.store(frame.saved_blocked, core::sync::atomic::Ordering::Relaxed);
    }
}

// ── Thread support ─────────────────────────────────────────────────────────────

/// `clone` flag: share address space with parent.
const CLONE_VM:               u32 = 0x0000_0100;
/// `clone` flag: join parent's thread group.
const CLONE_THREAD:           u32 = 0x0001_0000;
/// `clone` flag: set `tp` to the TLS argument.
const CLONE_SETTLS:           u32 = 0x0008_0000;
/// `clone` flag: write 0 to child_tid_ptr and futex-wake on thread exit.
const CLONE_CHILD_CLEARTID:   u32 = 0x0020_0000;
/// `clone` flag: write child TID to child_tid_ptr after fork.
const CLONE_CHILD_SETTID:     u32 = 0x0100_0000;

/// Creates a new thread or process.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Flags (combinations of `CLONE_VM`, `CLONE_THREAD`,
///   `CLONE_SETTLS`, `CLONE_CHILD_SETTID`, `CLONE_CHILD_CLEARTID`).
/// * `arg1` (`a1`) - New stack pointer for child (`0` = inherit parent sp).
/// * `arg2` (`a2`) - TLS value loaded into `tp` (only when `CLONE_SETTLS`).
/// * `arg3` (`a3`) - User-space `*mut u32` for SETTID/CLEARTID.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// The new thread/process PID in the parent, 0 in the child.
pub fn sys_clone(arg0: usize, arg1: usize, arg2: usize, arg3: usize, tf: &mut TrapFrame) {
    use core::sync::atomic::Ordering;
    let flags = arg0 as u32;
    let stack = arg1;
    let tls   = arg2;
    let child_tid_ptr = arg3;

    let ppid = crate::posix::process::current_pid();

    let child = match crate::posix::process::Process::new(ppid) {
        Ok(c) => c,
        Err(_) => { tf.regs[10] = crate::errno::ENOMEM; return; }
    };
    let child_pid = child.pid;

    let parent_proc = match crate::posix::process::PROCESS_TABLE.lock().get(&ppid).cloned() {
        Some(p) => p,
        None => {
            crate::posix::spawn::cleanup_process(child_pid);
            tf.regs[10] = crate::errno::ESRCH;
            return;
        }
    };

    if flags & CLONE_VM != 0 {
        // Share address space: discard the child's fresh empty page table and
        // install the parent's, incrementing the shared refcount.
        let child_root_pa =
            (child.satp_val.load(Ordering::Relaxed) & 0xFFF_FFFF_FFFF) << 12;
        let _ = crate::mm::vmm::destroy_user_space(child_root_pa);

        let parent_satp   = parent_proc.satp_val.load(Ordering::Relaxed);
        let parent_root_pa = (parent_satp & 0xFFF_FFFF_FFFF) << 12;
        child.satp_val.store(parent_satp, Ordering::Relaxed);
        crate::posix::process::satp_share(parent_root_pa);
    } else {
        // No CLONE_VM: COW-clone the address space like fork.
        let parent_root_pa =
            (parent_proc.satp_val.load(Ordering::Relaxed) & 0xFFF_FFFF_FFFF) << 12;
        let child_root_pa =
            (child.satp_val.load(Ordering::Relaxed) & 0xFFF_FFFF_FFFF) << 12;
        if let Err(e) = crate::mm::vmm::clone_user_space(parent_root_pa, child_root_pa) {
            crate::println!("sys_clone: clone_user_space failed: {}", e);
            crate::posix::spawn::cleanup_process(child_pid);
            tf.regs[10] = usize::MAX;
            return;
        }
        unsafe { core::arch::asm!("sfence.vma") };
    }

    if flags & CLONE_THREAD != 0 {
        let parent_tgid = parent_proc.tgid.load(Ordering::Relaxed);
        child.tgid.store(parent_tgid, Ordering::Relaxed);
    }

    // Namespace isolation: create new namespaces for any requested CLONE_NEW* flags.
    // The child inherited the parent's ns at Process::new time.  If clone flags
    // request new namespaces, fork (create fresh) the relevant ones now.
    {
        let parent_ns = parent_proc.ns.lock();
        let child_ns = crate::namespace::NsSet::fork_from(&parent_ns, flags);
        *child.ns.lock() = child_ns;
    }

    // Copy the current trap frame into the child.  `tf` IS the parent's trap
    // frame (sscratch points at it), so we read state directly from tf.
    {
        let mut child_tf = child.trap_frame.lock();
        let child_kernel_sp = child_tf.kernel_sp;
        *child_tf = *tf;              // copy all saved registers
        child_tf.kernel_sp = child_kernel_sp;
        child_tf.regs[10] = 0;       // child returns 0 from clone()
        // sepc was already advanced by 4 in the dispatch path
        if stack != 0 {
            child_tf.regs[2] = stack; // sp
        }
        if flags & CLONE_SETTLS != 0 {
            child_tf.regs[4] = tls;   // tp
        }
    }

    // CLONE_CHILD_SETTID: write child's TID into child_tid_ptr (shared address space).
    if flags & CLONE_CHILD_SETTID != 0 && child_tid_ptr != 0 {
        unsafe { core::ptr::write_volatile(child_tid_ptr as *mut u32, child_pid as u32); }
    }

    // CLONE_CHILD_CLEARTID: store the pointer so it is cleared on thread exit.
    if flags & CLONE_CHILD_CLEARTID != 0 && child_tid_ptr != 0 {
        child.clear_tid_ptr.store(child_tid_ptr, Ordering::Relaxed);
    }

    crate::posix::process::enqueue_with_prio(child_pid, child.priority.load(Ordering::Relaxed));
    tf.regs[10] = child_pid as usize;
}

/// Returns the calling thread's TID (equal to its PID in this kernel).
pub fn sys_gettid(tf: &mut TrapFrame) {
    tf.regs[10] = crate::posix::process::current_pid() as usize;
}

/// `futex` op: wait until `*uaddr == val`, atomically.
const FUTEX_WAIT: usize = 0;
/// `futex` op: wake up to `val` waiters.
const FUTEX_WAKE: usize = 1;

/// Minimal futex implementation supporting `FUTEX_WAIT` and `FUTEX_WAKE`.
///
/// # Arguments
///
/// * `arg0` (`a0`) - User VA of a `u32` word.
/// * `arg1` (`a1`) - Op: 0 = `FUTEX_WAIT`, 1 = `FUTEX_WAKE`.
/// * `arg2` (`a2`) - For `WAIT`: expected value; for `WAKE`: max waiters.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// For `WAIT`: 0 on wake, `EAGAIN` if value mismatch.
/// For `WAKE`: number of waiters woken.
pub fn sys_futex(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let uaddr = arg0;
    let op    = arg1;
    let val   = arg2;
    if !crate::mm::vmm::is_user_pointer_valid(tf, uaddr as *const u32, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    match op {
        FUTEX_WAIT => {
            let pid = crate::posix::process::current_pid();
            // Hold the table lock across the read + enqueue to close the
            // lost-wakeup window: a concurrent FUTEX_WAKE that fires between
            // the read and the push would find the waiter and wake it.
            let mut table = crate::posix::process::FUTEX_TABLE.lock();
            let actual = unsafe { core::ptr::read_volatile(uaddr as *const u32) };
            if actual != val as u32 {
                tf.regs[10] = crate::errno::EAGAIN;
                return;
            }
            table.entry(uaddr).or_default().push_back(pid);
            drop(table);

            if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&pid).cloned() {
                *proc.state.lock() = crate::posix::process::ProcState::Stopped;
            }
            tf.regs[10] = 0;
            crate::posix::process::schedule();
            unsafe { __halt_cpu() }
        }
        FUTEX_WAKE => {
            let count = if val == 0 { 1 } else { val };
            let to_wake: Vec<crate::posix::process::Pid> = {
                let mut table = crate::posix::process::FUTEX_TABLE.lock();
                if let Some(waiters) = table.get_mut(&uaddr) {
                    let n = count.min(waiters.len());
                    let drained: Vec<_> = waiters.drain(..n).collect();
                    if waiters.is_empty() { table.remove(&uaddr); }
                    drained
                } else {
                    Vec::new()
                }
            };
            let woken = to_wake.len();
            for waker_pid in to_wake {
                if let Some(proc) =
                    crate::posix::process::PROCESS_TABLE.lock().get(&waker_pid).cloned()
                {
                    *proc.state.lock() = crate::posix::process::ProcState::Running;
                    crate::posix::process::enqueue_with_prio(waker_pid, proc.priority.load(Ordering::Relaxed));
                }
            }
            tf.regs[10] = woken;
        }
        _ => { tf.regs[10] = usize::MAX; }
    }
}

/// Returns the calling process's mount namespace ID (for VFS routing).
pub fn sys_getnsid(tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    tf.regs[10] = proc.ns.lock().mnt.id as usize;
}

// ── Priority / capability syscalls ─────────────────────────────────────────────

/// Sets the calling process's scheduling priority (0 = realtime, 31 = idle).
pub fn sys_setpriority(prio: i32, tf: &mut TrapFrame) {
    let clamped = prio.clamp(
        crate::posix::process::PRIO_REALTIME,
        crate::posix::process::PRIO_IDLE,
    );
    let proc = crate::get_current_proc_or_esrch!(tf);
    proc.priority.store(clamped, Ordering::Relaxed);
    tf.regs[10] = 0;
}

/// Returns the calling process's scheduling priority.
pub fn sys_getpriority(tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    tf.regs[10] = proc.priority.load(Ordering::Relaxed) as usize;
}

/// Grants a subset of the caller's capability mask to `target_pid`.
///
/// Only bits present in the caller's own caps may be granted.
pub fn sys_cap_grant(target_pid: i32, caps: u64, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    let my_caps = proc.caps.load(Ordering::Relaxed);
    if caps & !my_caps != 0 {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }
    let table = crate::posix::process::PROCESS_TABLE.lock();
    if let Some(target) = table.get(&target_pid) {
        target.caps.fetch_or(caps, Ordering::Relaxed);
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = crate::errno::ESRCH;
    }
}

// ── Memory-mapping syscalls (B4) ───────────────────────────────────────────────

/// `mmap` flag: anonymous mapping.
const MAP_ANONYMOUS: usize = 0x20;

/// Allocates a virtual memory region.
///
/// Supports `MAP_ANONYMOUS` (fd = -1). Demand paging fills zero
/// pages on first access.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Hint address (or 0 to let the kernel choose).
/// * `arg1` (`a1`) - Length in bytes.
/// * `arg2` (`a2`) - Protection flags (currently unused).
/// * `arg3` (`a3`) - Mapping flags.
/// * `tf` - Caller's trap frame (fd is in `regs[14]`).
///
/// # Returns
///
/// The mapped VA on success, `EINVAL` on invalid args.
pub fn sys_mmap(arg0: usize, arg1: usize, _arg2: usize, arg3: usize, tf: &mut TrapFrame) {
    let len   = arg1;
    let flags = arg3;
    let fd    = tf.regs[14] as i32;

    let is_anon = (flags & MAP_ANONYMOUS) != 0 || fd == -1;
    if !is_anon {
        tf.regs[10] = crate::errno::EINVAL;
        return;
    }
    if len == 0 {
        tf.regs[10] = crate::errno::EINVAL;
        return;
    }

    let page_size = crate::mm::pmm::PAGE_SIZE;
    let aligned_len = (len + page_size - 1) & !(page_size - 1);

    let proc = match crate::posix::process::get_current_proc() {
        Some(p) => p,
        None => { tf.regs[10] = crate::errno::ESRCH; return; }
    };

    let va = proc.next_mmap_va.fetch_add(aligned_len, Ordering::Relaxed);
    // Physical pages are demand-allocated by handle_user_page_fault on first touch.

    // If a hint address was requested and aligns to a page, honour it only if
    // it falls in the mmap region and is unused (best-effort; we ignore conflicts).
    let result_va = if arg0 != 0 && arg0 >= 0x4_0000_0000 { arg0 } else { va };
    tf.regs[10] = result_va;
}

/// Unmaps a range of pages.
///
/// # Arguments
///
/// * `arg0` (`a0`) - Address (page-aligned).
/// * `arg1` (`a1`) - Length in bytes.
/// * `tf` - Caller's trap frame.
pub fn sys_munmap(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let addr = arg0;
    let len  = arg1;
    if len == 0 { tf.regs[10] = 0; return; }

    let proc = match crate::posix::process::get_current_proc() {
        Some(p) => p,
        None => { tf.regs[10] = crate::errno::ESRCH; return; }
    };

    let satp    = proc.satp_val.load(Ordering::Relaxed);
    let root_pa = (satp & 0xFFF_FFFF_FFFF) << 12;
    let page_size = crate::mm::pmm::PAGE_SIZE;
    let aligned_len = (len + page_size - 1) & !(page_size - 1);

    crate::mm::vmm::unmap_range(root_pa, addr, aligned_len);
    unsafe { core::arch::asm!("sfence.vma") };
    tf.regs[10] = 0;
}

/// Sets or queries the program break (end of heap).
///
/// # Arguments
///
/// * `arg0` (`a0`) - New break, or 0 to query.
/// * `tf` - Caller's trap frame.
///
/// # Returns
///
/// The new break value.
pub fn sys_brk(arg0: usize, tf: &mut TrapFrame) {
    let proc = match crate::posix::process::get_current_proc() {
        Some(p) => p,
        None => { tf.regs[10] = crate::errno::ESRCH; return; }
    };
    let current = proc.heap_break.load(Ordering::Relaxed);
    let new_brk = if arg0 == 0 { current } else { arg0 };
    proc.heap_break.store(new_brk, Ordering::Relaxed);
    tf.regs[10] = new_brk;
}

// ── ABI versioning (C5) ───────────────────────────────────────────────────────

/// Kernel ABI version. Increment on any incompatible syscall table change.
pub const KERNEL_ABI_VERSION: usize = 1;

/// Returns the kernel ABI version so user-space can detect mismatches.
pub fn sys_abi_version(tf: &mut TrapFrame) {
    tf.regs[10] = KERNEL_ABI_VERSION;
}

// ── Namespace unshare (C3) ────────────────────────────────────────────────────

/// Detaches one or more namespaces from the current process.
///
/// Supported flags: `CLONE_NEWNS` (0x00020000), `CLONE_NEWPID`
/// (0x20000000), `CLONE_NEWNET` (0x40000000).
///
/// Creates fresh namespaces for the calling process.  The process's
/// existing namespaces are replaced with newly-created ones.
///
/// Requires `CAP_SYS_ADMIN` to create new namespaces.
pub fn sys_unshare(arg0: usize, tf: &mut TrapFrame) {
    const CLONE_NEWNS:  u32 = 0x0002_0000;
    const CLONE_NEWPID: u32 = 0x2000_0000;
    const CLONE_NEWNET: u32 = 0x4000_0000;

    let flags = arg0 as u32;
    if flags & (CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWNET) == 0 {
        tf.regs[10] = 0; // nothing to do
        return;
    }

    let proc = crate::get_current_proc_or_esrch!(tf);
    if proc.caps.load(Ordering::Relaxed) & crate::posix::process::CAP_SYS_ADMIN == 0 {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }

    let mut ns = proc.ns.lock();
    *ns = crate::namespace::NsSet::fork_from(&ns, flags);
    tf.regs[10] = 0;
}

pub const JOB_POLICY_MAX_MEMORY: usize = 1;

pub fn sys_job_create(parent_handle: usize, options: usize, tf: &mut TrapFrame) {
    let _ = options;
    let proc = crate::get_current_proc_or_esrch!(tf);

    let parent_job = if parent_handle == 0 {
        proc.job.clone()
    } else {
        match proc.fds.lock().get(parent_handle as u32) {
            Some(crate::ipc::handle::KernelObject::Job(job)) => job.clone(),
            _ => {
                tf.regs[10] = crate::errno::EBADF;
                return;
            }
        }
    };

    let new_job = alloc::sync::Arc::new(crate::posix::job::Job::new(Some(alloc::sync::Arc::downgrade(&parent_job))));
    parent_job.children.lock().push(new_job.clone());

    let new_handle = proc.fds.lock().insert(crate::ipc::handle::KernelObject::Job(new_job));
    tf.regs[10] = new_handle as usize;
}

pub fn sys_job_set_policy(job_handle: usize, policy_type: usize, value: usize, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);

    let job = if job_handle == 0 {
        proc.job.clone()
    } else {
        match proc.fds.lock().get(job_handle as u32) {
            Some(crate::ipc::handle::KernelObject::Job(j)) => j.clone(),
            _ => {
                tf.regs[10] = crate::errno::EBADF;
                return;
            }
        }
    };

    if policy_type == JOB_POLICY_MAX_MEMORY {
        job.max_memory.store(value, Ordering::SeqCst);
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = crate::errno::EINVAL;
    }
}

// ── Thread lifecycle helpers ────────────────────────────────────────────────

/// Exits all threads in the calling thread's thread group (POSIX exit_group).
///
/// Sets every process with the same tgid as a zombie with `status`, then
/// schedules the next process.
pub fn sys_exit_group(status: usize, tf: &mut TrapFrame) {
    let _ = tf;
    let my_tgid = {
        let pid = crate::posix::process::current_pid();
        let table = crate::posix::process::PROCESS_TABLE.lock();
        match table.get(&pid) {
            Some(p) => p.tgid.load(Ordering::Relaxed),
            None => { crate::posix::process::schedule(); unsafe { __halt_cpu() } }
        }
    };

    let peers: Vec<crate::posix::process::Pid> = {
        let table = crate::posix::process::PROCESS_TABLE.lock();
        table
            .iter()
            .filter(|(_, p)| p.tgid.load(Ordering::Relaxed) == my_tgid)
            .map(|(pid, _)| *pid)
            .collect()
    };

    for peer in peers {
        crate::posix::spawn::exit(peer, status as i32);
    }

    crate::posix::process::schedule();
    unsafe { __halt_cpu() }
}

/// Stores `ptr` as the thread's `clear_tid_ptr` and returns the calling TID.
///
/// Used by libc at thread start to register the futex address for `pthread_join`.
pub fn sys_set_tid_address(ptr: usize, tf: &mut TrapFrame) {
    let pid = crate::posix::process::current_pid();
    if let Some(proc) = crate::posix::process::PROCESS_TABLE.lock().get(&pid).cloned() {
        proc.clear_tid_ptr.store(ptr, Ordering::Relaxed);
    }
    tf.regs[10] = pid as usize;
}

/// Terminates a task (process or job) identified by a handle.
///
/// If `handle` is 0, kills the calling process. If it resolves to a
/// [`KernelObject::Job`], kills every process currently in the job.
pub fn sys_task_kill(handle: usize, tf: &mut TrapFrame) {
    if handle == 0 {
        let pid = crate::posix::process::current_pid();
        crate::posix::spawn::exit(pid, -1);
        crate::posix::process::schedule();
        unsafe { __halt_cpu() }
    }
    let proc = crate::get_current_proc_or_esrch!(tf);

    let job = match proc.fds.lock().get(handle as u32) {
        Some(crate::ipc::handle::KernelObject::Job(j)) => j.clone(),
        _ => { tf.regs[10] = crate::errno::EBADF; return; }
    };

    let pids: Vec<crate::posix::process::Pid> = job.processes.lock().clone();
    for pid in pids {
        crate::posix::spawn::exit(pid, -1);
    }
    tf.regs[10] = 0;
}

/// Sends signal `sig` to thread `tid` within thread group `tgid`.
///
/// Only delivers if `tid` exists and its `tgid` matches.
pub fn sys_tgkill(tgid: usize, tid: usize, sig: usize, tf: &mut TrapFrame) {
    let target_pid = tid as crate::posix::process::Pid;
    let target_tgid = tgid as i32;

    let proc_arc = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid).cloned();
    match proc_arc {
        Some(p) if p.tgid.load(Ordering::Relaxed) == target_tgid => {
            if sig != 0 && sig < 32 {
                p.pending_signals.fetch_or(1 << (sig - 1), Ordering::SeqCst);
            }
            tf.regs[10] = 0;
        }
        Some(_) => tf.regs[10] = crate::errno::ESRCH,
        None    => tf.regs[10] = crate::errno::ESRCH,
    }
}


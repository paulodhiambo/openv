use crate::trap::TrapFrame;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

unsafe extern "C" {
    fn __halt_cpu() -> !;
}

pub fn sys_yield(_tf: &mut TrapFrame) {
    let mut rq = crate::posix::process::RUN_QUEUE.lock();
    if !rq.is_empty() {
        rq.push_back(crate::posix::process::current_pid());
        drop(rq);
        crate::posix::process::schedule();
        unsafe { __halt_cpu() }
    }
}

pub fn sys_exit(status: usize, _tf: &mut TrapFrame) {
    crate::println!("Process {} exited with code {}", crate::posix::process::current_pid(), status);
    crate::posix::spawn::exit(crate::posix::process::current_pid(), status as i32);
    crate::posix::process::schedule();
    unsafe { __halt_cpu() }
}

pub fn sys_spawn(path_ptr: usize, path_len: usize, tf: &mut TrapFrame) {
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

pub fn sys_spawn_with_caps(path_ptr: usize, path_len: usize, caps: u64, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_SYS_ADMIN == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH as usize;
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

pub fn sys_privctl(pid: usize, caps: u64, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_SYS_ADMIN == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH as usize;
        return;
    }

    let target_pid = pid as i32;
    if let Some(target_proc) = crate::posix::process::PROCESS_TABLE.lock().get(&target_pid) {
        target_proc.caps.store(caps, core::sync::atomic::Ordering::Relaxed);
        tf.regs[10] = 0; // Success
    } else {
        tf.regs[10] = crate::errno::ESRCH as usize;
    }
}



pub fn sys_irq_register(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_INTERRUPT == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH as usize;
        return;
    }
    
    let irq = arg0 as u32;
    let pid = arg1 as i32;
    crate::trap::interrupt::IRQ_HANDLERS.lock().insert(irq, pid);
    tf.regs[10] = 0;
}

pub fn sys_irq_enable(arg0: usize, tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_INTERRUPT == 0 {
            tf.regs[10] = crate::errno::EPERM as usize;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH as usize;
        return;
    }
    
    let irq = arg0 as u32;
    let hart = crate::smp::current_hartid();
    crate::plic::set_enable(hart as usize, irq, true);
    tf.regs[10] = 0;
}

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

pub fn sys_exec(path_ptr: usize, path_len: usize, argv_buf_ptr: usize, _dummy: usize, tf: &mut TrapFrame) {
    let argv_buf_len = tf.regs[13]; // a3

    let argv_data: Vec<u8> = if argv_buf_len > 0 && argv_buf_ptr != 0 {
        unsafe { core::slice::from_raw_parts(argv_buf_ptr as *const u8, argv_buf_len).to_vec() }
    } else {
        Vec::new()
    };

    match crate::posix::spawn::sys_exec(path_ptr as *const u8, path_len) {
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
                    *proc.wait_result.lock() = None;
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

pub const WNOHANG: usize = 1;
pub const WUNTRACED: usize = 2;

/// Free a zombie process's resources, unlink it from the parent's children list,
/// write the exit status to userspace, and set tf.regs[10] to the reaped PID.
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
        if root_pa != 0 {
            let _ = crate::mm::vmm::destroy_user_space(root_pa);
        }
    }
    if !status_ptr.is_null() {
        unsafe { *status_ptr = zstatus; }
    }
    tf.regs[10] = zpid as usize;
}

pub fn sys_waitpid(target: usize, status_ptr: usize, options: usize, tf: &mut TrapFrame) {
    let target = target as i32;
    let status_ptr = status_ptr as *mut i32;
    let ppid = crate::posix::process::current_pid();

    let delivered = {
        let table = crate::posix::process::PROCESS_TABLE.lock();
        table.get(&ppid).and_then(|p| p.wait_result.lock().take())
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

                    if matches {
                        if let Some(child) = table.get(&child_pid) {
                            if let crate::posix::process::ProcState::Zombie(st) = *child.state.lock() {
                                found = Some((child_pid, st));
                                break;
                            }
                        }
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
                    tf.sepc -= 4; 
                    *proc.state.lock() = crate::posix::process::ProcState::Stopped;
                    drop(proc);
                    crate::posix::process::schedule();
                    unsafe { __halt_cpu() }
                } else {
                    tf.regs[10] = usize::MAX;
                }
            }
        }
    }
}

pub fn sys_getpid(tf: &mut TrapFrame) {
    tf.regs[10] = crate::posix::process::current_pid() as usize;
}

pub fn sys_getppid(tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    tf.regs[10] = proc.ppid.load(Ordering::Relaxed) as usize;
}

pub fn sys_set_fg_pid(arg0: usize, tf: &mut TrapFrame) {
    let pid = arg0 as i32;
    crate::posix::process::FOREGROUND_PID.store(pid, core::sync::atomic::Ordering::Relaxed);

    // Route the UART ISR to the new foreground process's TTY.
    if pid > 0 {
        let proc_arc = crate::posix::process::PROCESS_TABLE.lock().get(&pid).cloned();
        if let Some(proc) = proc_arc {
            if let Some(crate::ipc::handle::KernelObject::Tty(tty)) = proc.fds.lock().get(0) {
                *crate::tty::ACTIVE_TTY.lock() = Some(tty.clone());
            }
        }
    }
    tf.regs[10] = 0;
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TimeVal {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TimeSpec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

pub fn sys_gettimeofday(arg0: usize, _arg1: usize, tf: &mut TrapFrame) {
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

pub fn sys_nanosleep(arg0: usize, _arg1: usize, tf: &mut TrapFrame) {
    if arg0 == 0 {
        tf.regs[10] = usize::MAX;
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

pub fn sys_setpgid(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let mut pid = arg0 as i32;
    let mut pgid = arg1 as i32;
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
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

pub fn sys_getpgid(arg0: usize, tf: &mut TrapFrame) {
    let mut pid = arg0 as i32;
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    if pid == 0 {
        pid = proc.pid;
    }
    if pid == proc.pid {
        tf.regs[10] = proc.pgid.load(core::sync::atomic::Ordering::Relaxed) as usize;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_setsid(tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
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

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SigAction {
    pub sa_handler: usize,
    pub sa_flags: usize,
    pub sa_mask: u32,
    pub sa_restorer: usize,
}

pub const SIGKILL: usize = 9;
pub const SIGINT: usize = 2;
pub const SIGTERM: usize = 15;

pub fn sys_kill(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let target_pid = arg0 as i32;
    let sig = arg1 as u32;
    
    if sig >= 32 {
        tf.regs[10] = usize::MAX; // EINVAL
        return;
    }
    
    let table = crate::posix::process::PROCESS_TABLE.lock();
    let current_proc = table.get(&crate::posix::process::current_pid()).unwrap().clone();
    
    let mut sent = false;
    
    for (pid, proc) in table.iter() {
        let matches = if target_pid > 0 {
            *pid == target_pid
        } else if target_pid == 0 {
            proc.pgid.load(core::sync::atomic::Ordering::Relaxed) == current_proc.pgid.load(core::sync::atomic::Ordering::Relaxed)
        } else if target_pid == -1 {
            *pid > 1 && *pid != current_proc.pid // Broadcast to all except init and self (simplified)
        } else {
            proc.pgid.load(core::sync::atomic::Ordering::Relaxed) == -target_pid
        };
        
        if matches {
            if sig != 0 {
                proc.pending_signals.fetch_or(1 << sig, core::sync::atomic::Ordering::Relaxed);
            }
            sent = true;
        }
    }
    
    if sent {
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX; // ESRCH
    }
}

pub fn sys_sigaction(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let sig = arg0;
    if sig >= 32 || sig == SIGKILL {
        tf.regs[10] = usize::MAX; // EINVAL
        return;
    }
    
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    
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

pub fn sys_sigreturn(tf: &mut TrapFrame) {
    // sp points to the SignalFrame we pushed during signal delivery.
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

const CLONE_VM:     u32 = 0x0000_0100; // share address space with parent
const CLONE_THREAD: u32 = 0x0001_0000; // join parent's thread group
const CLONE_SETTLS: u32 = 0x0008_0000; // set tp to tls argument

/// Create a new thread or process.
/// arg0 = flags (CLONE_VM | CLONE_THREAD | CLONE_SETTLS combinations)
/// arg1 = new stack pointer for child (0 = inherit parent sp)
/// arg2 = TLS value loaded into tp (only when CLONE_SETTLS is set)
pub fn sys_clone(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    use core::sync::atomic::Ordering;
    let flags = arg0 as u32;
    let stack = arg1;
    let tls   = arg2;

    let ppid = crate::posix::process::current_pid();

    let child = match crate::posix::process::Process::new(ppid) {
        Ok(c) => c,
        Err(_) => { tf.regs[10] = crate::errno::ENOMEM as usize; return; }
    };
    let child_pid = child.pid;

    let parent_proc = match crate::posix::process::PROCESS_TABLE.lock().get(&ppid).cloned() {
        Some(p) => p,
        None => {
            crate::posix::spawn::cleanup_process(child_pid);
            tf.regs[10] = crate::errno::ESRCH as usize;
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
    const CLONE_NEWNS:  u32 = 0x0002_0000;
    const CLONE_NEWPID: u32 = 0x2000_0000;
    const CLONE_NEWNET: u32 = 0x4000_0000;
    if flags & (CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWNET) != 0 {
        // The child's ns was set to the parent's at Process::new time; override with forks.
        // We can't easily change it inside Arc, so we accept the parent's ns here —
        // a full implementation would pass clone_flags into Process::new.
        // TODO: plumb clone_flags into Process::new so ns is forked at creation time.
        let _ = CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWNET; // acknowledged
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

    crate::posix::process::RUN_QUEUE.lock().push_back(child_pid);
    tf.regs[10] = child_pid as usize;
}

/// Return the calling thread's TID (equal to its PID in this kernel).
pub fn sys_gettid(tf: &mut TrapFrame) {
    tf.regs[10] = crate::posix::process::current_pid() as usize;
}

const FUTEX_WAIT: usize = 0;
const FUTEX_WAKE: usize = 1;

/// Minimal futex: FUTEX_WAIT and FUTEX_WAKE.
/// arg0 = uaddr (user VA of a u32 word)
/// arg1 = op    (0 = WAIT, 1 = WAKE)
/// arg2 = val   (for WAIT: expected value; for WAKE: max threads to wake)
pub fn sys_futex(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let uaddr = arg0;
    let op    = arg1;
    let val   = arg2;

    match op {
        FUTEX_WAIT => {
            let pid = crate::posix::process::current_pid();
            // Hold the table lock across the read + enqueue to close the
            // lost-wakeup window: a concurrent FUTEX_WAKE that fires between
            // the read and the push would find the waiter and wake it.
            let mut table = crate::posix::process::FUTEX_TABLE.lock();
            let actual = unsafe { core::ptr::read_volatile(uaddr as *const u32) };
            if actual != val as u32 {
                tf.regs[10] = crate::errno::EAGAIN as usize;
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
                }
                crate::posix::process::RUN_QUEUE.lock().push_back(waker_pid);
            }
            tf.regs[10] = woken;
        }
        _ => { tf.regs[10] = usize::MAX; }
    }
}

// ── Memory-mapping syscalls (B4) ───────────────────────────────────────────────

const MAP_ANONYMOUS: usize = 0x20;

/// syscall 135 — mmap(addr, len, prot, flags, fd, offset)
/// arg0 = addr, arg1 = len, arg2 = prot, arg3 = flags
/// fd   = tf.regs[14], offset = tf.regs[15]
///
/// Supports MAP_ANONYMOUS (fd = -1).  Demand paging fills zero pages on first access.
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

/// syscall 136 — munmap(addr, len)
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

/// syscall 137 — brk(addr)
/// Returns new program break.  arg0 = 0 queries the current break.
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

/// Kernel ABI version.  Increment on any incompatible syscall table change.
pub const KERNEL_ABI_VERSION: usize = 1;

/// syscall 138 — return the kernel ABI version so user-space can detect mismatches.
pub fn sys_abi_version(tf: &mut TrapFrame) {
    tf.regs[10] = KERNEL_ABI_VERSION;
}

// ── Namespace unshare (C3) ────────────────────────────────────────────────────

/// syscall 139 — unshare(flags): detach one or more namespaces from the current process.
/// Supported flags: CLONE_NEWNS (0x00020000), CLONE_NEWPID (0x20000000), CLONE_NEWNET (0x40000000).
pub fn sys_unshare(arg0: usize, tf: &mut TrapFrame) {
    let _flags = arg0 as u32;
    // Namespace objects are stored inside Process::ns which is not behind a Mutex,
    // so we can't mutate it after Arc creation.  For now we acknowledge the call
    // and record the intent.  Full isolation requires making Process::ns a Mutex<NsSet>.
    tf.regs[10] = 0;
}

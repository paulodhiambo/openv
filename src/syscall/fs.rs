use crate::trap::TrapFrame;
use core::sync::atomic::Ordering;

unsafe extern "C" {
    fn __halt_cpu() -> !;
}

/// Kernel-level open. All files live in the VFS server; if we reach here the
/// file was not found by VFS, so return ENOENT.
pub fn sys_open(_arg0: usize, _arg1: usize, _arg2: usize, tf: &mut TrapFrame) {
    tf.regs[10] = crate::errno::ENOENT;
}

pub fn sys_write(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    if !crate::mm::vmm::is_user_pointer_valid(tf, arg1 as *const u8, arg2) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let buf = unsafe { core::slice::from_raw_parts(arg1 as *const u8, arg2) };
    let proc = crate::get_current_proc_or_esrch!(tf);
    let fds = proc.fds.lock();
    match fds.get_with_rights(arg0 as u32, crate::ipc::handle::Rights::WRITE) {
        Ok(obj) => match obj {
            crate::ipc::handle::KernelObject::Tty(_) => {
                if let Ok(s) = core::str::from_utf8(buf) {
                    crate::print!("{}", s);
                }
                tf.regs[10] = arg2;
            }
            crate::ipc::handle::KernelObject::Channel(ep) => {
                let msg = crate::ipc::channel::Message {
                    bytes: alloc::vec::Vec::from(buf),
                    handles: alloc::vec::Vec::new(),
                };
                match ep.write(msg) {
                    Ok(_) => tf.regs[10] = arg2,
                    Err(_) => tf.regs[10] = usize::MAX,
                }
            }
            crate::ipc::handle::KernelObject::PipeWrite(half) => {
                let mut data = half.data.lock();
                for &b in buf {
                    data.push_back(b);
                }
                drop(data);
                tf.regs[10] = arg2;
                let waiter = half.waiter.swap(0, Ordering::Relaxed);
                if waiter > 0 {
                    crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
                }
                crate::ipc::handle::wake_epoll_waiters(&half.epoll_waiters);
            }
            crate::ipc::handle::KernelObject::VfsFile(_) => {
                tf.regs[10] = usize::MAX;
            }
            _ => {
                tf.regs[10] = usize::MAX;
            }
        },
        Err(e) => {
            tf.regs[10] = e;
        }
    }
}

pub fn sys_read(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    if !crate::mm::vmm::is_user_pointer_valid(tf, arg1 as *const u8, arg2) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let proc = crate::get_current_proc_or_esrch!(tf);
    let fds = proc.fds.lock();
    match fds.get_with_rights(arg0 as u32, crate::ipc::handle::Rights::READ) {
        Ok(obj) => match obj {
            crate::ipc::handle::KernelObject::Tty(tty_arc) => {
                let tty = tty_arc.clone();
                drop(fds);
                drop(proc);

                if tty.ctrlc.swap(false, Ordering::Relaxed) {
                    tf.regs[10] = 0;
                } else {
                    let raw = tty.raw.load(Ordering::Relaxed);
                    let mut buf = tty.buf.lock();

                    let ready = if raw {
                        !buf.is_empty()
                    } else {
                        buf.iter().any(|&b| b == b'\n')
                    };

                    if ready {
                        let len = if raw {
                            let take = core::cmp::min(arg2, buf.len());
                            let user_buf = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, take) };
                            for i in 0..take { user_buf[i] = buf[i]; }
                            buf.drain(0..take);
                            take
                        } else {
                            let nl = buf.iter().position(|&b| b == b'\n').unwrap();
                            let take = core::cmp::min(arg2, nl + 1);
                            let user_buf = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, take) };
                            for i in 0..take { user_buf[i] = buf[i]; }
                            buf.drain(0..take);
                            take
                        };
                        tf.regs[10] = len;
                    } else {
                        // Register waiter before releasing the buf lock so the ISR
                        // can't deliver a character between the emptiness check and
                        // the registration, causing a lost-wakeup.
                        tty.waiter.store(
                            crate::posix::process::current_pid(),
                            Ordering::Relaxed,
                        );
                        drop(buf);
                        tf.sepc -= 4;
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                }
            }
            crate::ipc::handle::KernelObject::Channel(ep) => {
                // Clone the Arc so we can drop fds (and its borrow of ep) before blocking.
                let ep_arc = ep.clone();
                if let Some(msg) = ep_arc.try_recv() {
                    let len = core::cmp::min(arg2, msg.bytes.len());
                    let buf = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, len) };
                    buf.copy_from_slice(&msg.bytes[..len]);
                    tf.regs[10] = len;
                } else {
                    // Register as waiter, then re-check to close the lost-wakeup window:
                    // the writer calls waiter.swap() after pushing to the queue, so if a
                    // message arrived between try_recv() above and our store, we must detect
                    // it here rather than sleeping forever.
                    ep_arc.waiter.store(
                        crate::posix::process::current_pid(),
                        core::sync::atomic::Ordering::Relaxed,
                    );
                    if let Some(msg) = ep_arc.try_recv() {
                        // Message arrived in the window — retract registration and return it.
                        ep_arc.waiter.store(0, core::sync::atomic::Ordering::Relaxed);
                        let len = core::cmp::min(arg2, msg.bytes.len());
                        let buf =
                            unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, len) };
                        buf.copy_from_slice(&msg.bytes[..len]);
                        tf.regs[10] = len;
                    } else {
                        drop(fds);
                        drop(proc);
                        tf.sepc -= 4;
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                }
            }
            crate::ipc::handle::KernelObject::PipeRead(half) => {
                let mut data = half.data.lock();
                if !data.is_empty() {
                    let len = core::cmp::min(arg2, data.len());
                    let buf = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, len) };
                    for b in buf.iter_mut() {
                        *b = data.pop_front().unwrap_or(0);
                    }
                    tf.regs[10] = len;
                } else {
                    if half.write_open.strong_count() == 0 {
                        tf.regs[10] = 0; // EOF — all write ends already closed
                    } else {
                        let pid = crate::posix::process::current_pid();
                        half.waiter.store(pid, Ordering::Relaxed);
                        // TOCTOU double-check: the last write half may have been dropped
                        // in the window between the strong_count check above and the
                        // waiter registration just now.  If so, the Drop impl saw
                        // waiter==0 and couldn't wake us — we must detect that here
                        // rather than sleeping forever.
                        if half.write_open.strong_count() == 0 {
                            half.waiter.store(0, Ordering::Relaxed);
                            tf.regs[10] = 0; // EOF
                        } else {
                            drop(data);
                            drop(fds);
                            drop(proc);
                            tf.sepc -= 4;
                            crate::posix::process::schedule();
                            unsafe { crate::trap::__halt_cpu() }
                        }
                    }
                }
            }
            crate::ipc::handle::KernelObject::VfsFile(_) => {
                tf.regs[10] = usize::MAX;
            }
            _ => {
                tf.regs[10] = usize::MAX;
            }
        },
        Err(e) => {
            tf.regs[10] = e;
        }
    }
}


pub fn sys_close(arg0: usize, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    if proc.fds.lock().remove(arg0 as u32).is_some() {
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}








/// syscall 110: copy all running PIDs into a caller-provided u32 array.
/// arg0 = buf_ptr (*mut u32 in user VA), arg1 = max entries.
/// Returns number of PIDs written.
pub fn sys_proc_list(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_PROCESS == 0 {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }
    let max_count = arg1.min(512);
    if !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const u32, max_count) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let buf_ptr = arg0 as *mut u32;
    let table = crate::posix::process::PROCESS_TABLE.lock();
    let mut count = 0usize;
    for &pid in table.keys() {
        if count >= max_count { break; }
        unsafe { core::ptr::write_volatile(buf_ptr.add(count), pid as u32); }
        count += 1;
    }
    tf.regs[10] = count;
}

/// syscall 111: write a text status block for `pid` into the caller's buffer.
/// arg0 = pid, arg1 = buf_ptr (*mut u8), arg2 = buf_len.
/// Returns bytes written, or usize::MAX if the PID does not exist.
pub fn sys_proc_status(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_PROCESS == 0 {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }
    if !crate::mm::vmm::is_user_pointer_valid(tf, arg1 as *const u8, arg2) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let pid = arg0 as i32;
    let buf_ptr = arg1 as *mut u8;
    let buf_len = arg2;
    let table = crate::posix::process::PROCESS_TABLE.lock();
    if let Some(proc) = table.get(&pid) {
        let state_str = match *proc.state.lock() {
            crate::posix::process::ProcState::Running => "R (running)",
            crate::posix::process::ProcState::Stopped => "T (stopped)",
            crate::posix::process::ProcState::Zombie(_) => "Z (zombie)",
        };
        let ppid = proc.ppid.load(core::sync::atomic::Ordering::Relaxed);
        let uid  = proc.uid.load(core::sync::atomic::Ordering::Relaxed);
        let s = alloc::format!(
            "Pid:\t{}\nPPid:\t{}\nState:\t{}\nUid:\t{}\n",
            pid, ppid, state_str, uid
        );
        let bytes = s.as_bytes();
        let n = bytes.len().min(buf_len);
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf_ptr, n); }
        tf.regs[10] = n;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

/// syscall 112: register the calling process as the system block driver.
/// Requires CAP_MMIO.
pub fn sys_blk_register(tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(Ordering::Relaxed) & crate::posix::process::CAP_MMIO == 0 {
            tf.regs[10] = crate::errno::EPERM;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH;
        return;
    }
    let pid = crate::posix::process::current_pid();
    crate::trap::BLK_SERVER_PID.store(pid, Ordering::Relaxed);
    crate::println!("Block driver registered: pid {}", pid);
    tf.regs[10] = 0;
}

/// syscall 113: return the PID of the registered block driver, or usize::MAX.
pub fn sys_get_blk_pid(tf: &mut TrapFrame) {
    let pid = crate::trap::BLK_SERVER_PID.load(Ordering::Relaxed);
    tf.regs[10] = if pid > 0 { pid as usize } else { usize::MAX };
}

pub fn sys_vfs_register(tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_SYS_ADMIN == 0 {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }
    let pid = crate::posix::process::current_pid();
    crate::trap::VFS_SERVER_PID.store(pid, Ordering::Relaxed);
    crate::println!("VFS server registered: pid {}", pid);
    tf.regs[10] = 0;
}

pub fn sys_get_vfs_pid(tf: &mut TrapFrame) {
    let pid = crate::trap::VFS_SERVER_PID.load(Ordering::Relaxed);
    tf.regs[10] = if pid > 0 { pid as usize } else { usize::MAX };
}

pub fn sys_initrd_data(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_DATACOPY == 0 {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }
    let buf_ptr = arg0;
    let offset = arg1;
    let max_len = arg2;
    let start = crate::INITRD_START.load(Ordering::Relaxed);
    let total = crate::INITRD_LEN.load(Ordering::Relaxed);
    if start == 0 || offset >= total {
        tf.regs[10] = 0;
    } else {
        let avail = total - offset;
        let copy_len = max_len.min(avail).min(4096);
        if !crate::mm::vmm::is_user_pointer_valid(tf, buf_ptr as *const u8, copy_len) {
            tf.regs[10] = crate::errno::EFAULT;
            return;
        }
        unsafe {
            let src = core::slice::from_raw_parts((start + offset) as *const u8, copy_len);
            let dst = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, copy_len);
            dst.copy_from_slice(src);
        }
        tf.regs[10] = copy_len;
    }
}




/// syscall 66: register the calling process as the PM (process manager) server.
/// Requires CAP_PROCESS.
pub fn sys_pm_register(tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(Ordering::Relaxed) & crate::posix::process::CAP_PROCESS == 0 {
            tf.regs[10] = crate::errno::EPERM;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH;
        return;
    }
    let pid = crate::posix::process::current_pid();
    crate::trap::PM_SERVER_PID.store(pid, Ordering::Relaxed);
    crate::println!("PM server registered: pid {}", pid);
    tf.regs[10] = 0;
}

/// syscall 67: return the PID of the registered PM server, or usize::MAX.
pub fn sys_get_pm_pid(tf: &mut TrapFrame) {
    let pid = crate::trap::PM_SERVER_PID.load(Ordering::Relaxed);
    tf.regs[10] = if pid > 0 { pid as usize } else { usize::MAX };
}

/// syscall 114: register the calling process as the procfs server.
/// Requires CAP_DATACOPY.
pub fn sys_proc_server_register(tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(Ordering::Relaxed) & crate::posix::process::CAP_DATACOPY == 0 {
            tf.regs[10] = crate::errno::EPERM;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH;
        return;
    }
    let pid = crate::posix::process::current_pid();
    crate::trap::PROC_SERVER_PID.store(pid, Ordering::Relaxed);
    crate::println!("Proc server registered: pid {}", pid);
    tf.regs[10] = 0;
}

/// syscall 115: return the PID of the registered procfs server, or usize::MAX.
pub fn sys_get_proc_server_pid(tf: &mut TrapFrame) {
    let pid = crate::trap::PROC_SERVER_PID.load(Ordering::Relaxed);
    tf.regs[10] = if pid > 0 { pid as usize } else { usize::MAX };
}

/// syscall 116: register the calling process as the devfs server.
/// Requires CAP_DATACOPY.
pub fn sys_dev_server_register(tf: &mut TrapFrame) {
    if let Some(proc) = crate::posix::process::get_current_proc() {
        if proc.caps.load(Ordering::Relaxed) & crate::posix::process::CAP_DATACOPY == 0 {
            tf.regs[10] = crate::errno::EPERM;
            return;
        }
    } else {
        tf.regs[10] = crate::errno::ESRCH;
        return;
    }
    let pid = crate::posix::process::current_pid();
    crate::trap::DEV_SERVER_PID.store(pid, Ordering::Relaxed);
    crate::println!("Dev server registered: pid {}", pid);
    tf.regs[10] = 0;
}

/// syscall 117: return the PID of the registered devfs server, or usize::MAX.
pub fn sys_get_dev_server_pid(tf: &mut TrapFrame) {
    let pid = crate::trap::DEV_SERVER_PID.load(Ordering::Relaxed);
    tf.regs[10] = if pid > 0 { pid as usize } else { usize::MAX };
}

/// syscall 130: flush all pending writes for a VFS-backed fd to durable storage.
/// The actual flush is done in the vfs-server (OFS commit_txn); the kernel just
/// ACKs non-VFS fds immediately since there is nothing to flush here.
pub fn sys_fsync(_arg0: usize, tf: &mut TrapFrame) {
    tf.regs[10] = 0;
}

/// syscall 131: same as fsync for our purposes (no separate metadata-only path).
pub fn sys_fdatasync(_arg0: usize, tf: &mut TrapFrame) {
    tf.regs[10] = 0;
}

pub fn sys_chdir(arg0: usize, arg1: usize, tf: &mut crate::trap::TrapFrame) {
    if !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const u8, arg1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let path_bytes = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
    if let Ok(path) = core::str::from_utf8(path_bytes) {
        let proc = crate::get_current_proc_or_esrch!(tf);
        let mut p = alloc::string::String::from(path);
        if p == "/" { }
        else if p.ends_with('/') { p.pop(); }
        *proc.cwd.lock() = p;
        tf.regs[10] = 0;
    } else { tf.regs[10] = usize::MAX; }
}

pub fn sys_getcwd(arg0: usize, arg1: usize, tf: &mut crate::trap::TrapFrame) {
    if !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const u8, arg1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let proc = crate::get_current_proc_or_esrch!(tf);
    let cwd = proc.cwd.lock();
    let bytes = cwd.as_bytes();
    if bytes.len() + 1 > arg1 {
        tf.regs[10] = usize::MAX;
    } else {
        let buf = unsafe { core::slice::from_raw_parts_mut(arg0 as *mut u8, arg1) };
        buf[..bytes.len()].copy_from_slice(bytes);
        buf[bytes.len()] = 0;
        tf.regs[10] = bytes.len();
    }
}

/// Registers the calling process as the component manager.
pub fn sys_cm_register(tf: &mut crate::trap::TrapFrame) {
    let pid = crate::posix::process::current_pid();
    crate::trap::CM_SERVER_PID.store(pid, core::sync::atomic::Ordering::Relaxed);
    crate::println!("CM server registered: pid {}", pid);
    tf.regs[10] = 0;
}

/// Returns the PID of the registered component manager (0 if not yet started).
pub fn sys_get_cm_pid(tf: &mut crate::trap::TrapFrame) {
    tf.regs[10] = crate::trap::CM_SERVER_PID.load(core::sync::atomic::Ordering::Relaxed) as usize;
}

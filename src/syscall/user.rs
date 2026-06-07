use crate::trap::TrapFrame;
use core::sync::atomic::Ordering;

pub fn sys_setuid(arg0: usize, tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    if proc.euid.load(Ordering::Relaxed) == 0 {
        let val = arg0 as u32;
        proc.uid.store(val, Ordering::Relaxed);
        proc.euid.store(val, Ordering::Relaxed);
        tf.regs[10] = 0;
    } else if arg0 as u32 == proc.uid.load(Ordering::Relaxed) {
        proc.euid.store(arg0 as u32, Ordering::Relaxed);
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX - 1; // EPERM
    }
}

pub fn sys_setgid(arg0: usize, tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    if proc.euid.load(Ordering::Relaxed) == 0 {
        let val = arg0 as u32;
        proc.gid.store(val, Ordering::Relaxed);
        proc.egid.store(val, Ordering::Relaxed);
        tf.regs[10] = 0;
    } else if arg0 as u32 == proc.gid.load(Ordering::Relaxed) {
        proc.egid.store(arg0 as u32, Ordering::Relaxed);
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX - 1; // EPERM
    }
}

pub fn sys_getuid(tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    tf.regs[10] = proc.uid.load(Ordering::Relaxed) as usize;
}

pub fn sys_geteuid(tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    tf.regs[10] = proc.euid.load(Ordering::Relaxed) as usize;
}

pub fn sys_getgid(tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    tf.regs[10] = proc.gid.load(Ordering::Relaxed) as usize;
}

pub fn sys_getegid(tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    tf.regs[10] = proc.egid.load(Ordering::Relaxed) as usize;
}

pub fn sys_authenticate(arg0: usize, arg1: usize, arg2: usize, arg3: usize, tf: &mut TrapFrame) {
    let username_bytes = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1) };
    let password_bytes = unsafe { core::slice::from_raw_parts(arg2 as *const u8, arg3) };
    if let Ok(username) = core::str::from_utf8(username_bytes) {
        match crate::posix::user::authenticate(username, password_bytes) {
            Some(entry) => tf.regs[10] = entry.uid as usize,
            None => tf.regs[10] = usize::MAX,
        }
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_can_sudo(arg0: usize, tf: &mut TrapFrame) {
    let uid = if arg0 == 0 {
        crate::get_current_proc_or_esrch!(tf);
        let proc = crate::posix::process::get_current_proc().unwrap();
        proc.uid.load(Ordering::Relaxed)
    } else {
        arg0 as u32
    };
    tf.regs[10] = if crate::posix::user::can_sudo(uid) { 1 } else { 0 };
}

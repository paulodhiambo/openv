use crate::trap::TrapFrame;
use core::sync::atomic::Ordering;

pub fn sys_set_echo(arg0: usize, tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    let fds = proc.fds.lock();
    if let Some(crate::ipc::handle::KernelObject::Tty(tty)) = fds.get(0) {
        tty.echo.store(arg0 != 0, Ordering::Relaxed);
    }
    tf.regs[10] = 0;
}

pub fn sys_set_raw(arg0: usize, tf: &mut TrapFrame) {
    crate::get_current_proc_or_esrch!(tf);
    let proc = crate::posix::process::get_current_proc().unwrap();
    let fds = proc.fds.lock();
    if let Some(crate::ipc::handle::KernelObject::Tty(tty)) = fds.get(0) {
        tty.raw.store(arg0 != 0, Ordering::Relaxed);
    }
    tf.regs[10] = 0;
}

use crate::ipc::handle::{EpollInstance, KernelObject};
use crate::trap::TrapFrame;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

const EPOLLIN: u32 = 0x001;
const EPOLLRDNORM: u32 = 0x040;
const EPOLLOUT: u32 = 0x004;
const EPOLLWRNORM: u32 = 0x100;
const EPOLLHUP: u32 = 0x010;
const EPOLLNVAL: u32 = 0x020;

const EPOLLET: u32 = 1 << 31;
const EPOLL_CLOEXEC: usize = 0x80000;

const EPOLL_CTL_ADD: usize = 1;
const EPOLL_CTL_DEL: usize = 2;
const EPOLL_CTL_MOD: usize = 3;

/// Layout of userspace struct epoll_event (16 bytes on 64-bit).
#[repr(C)]
struct EpollEvent {
    events: u32,
    _pad: u32,
    data: u64,
}

/// Check what events are ready for a given fd.
/// Returns the set of ready events (subset of `interest`).
fn check_fd_readiness(fd: i32, interest: u32) -> u32 {
    let proc = crate::posix::process::get_current_proc().unwrap();
    let fds = proc.fds.lock();

    let obj = match fds.get(fd as u32) {
        Some(o) => o,
        None => {
            if interest & EPOLLNVAL != 0 {
                return EPOLLNVAL;
            }
            return 0;
        }
    };

    match obj {
        KernelObject::PipeRead(half) => {
            let mut events = 0u32;
            let data = half.data.lock();
            if interest & (EPOLLIN | EPOLLRDNORM) != 0 && !data.is_empty() {
                events |= EPOLLIN | EPOLLRDNORM;
            }
            drop(data);
            if half.write_open.strong_count() == 0 {
                events |= EPOLLHUP;
            }
            events
        }
        KernelObject::PipeWrite(_half) => {
            let mut events = 0u32;
            if interest & (EPOLLOUT | EPOLLWRNORM) != 0 {
                events |= EPOLLOUT | EPOLLWRNORM;
            }
            events
        }
        KernelObject::Channel(ep) => {
            let signals = ep.get_signals();
            let mut events = 0u32;
            if interest & (EPOLLIN | EPOLLRDNORM) != 0
                && signals & crate::ipc::port::SIGNAL_READABLE != 0
            {
                events |= EPOLLIN | EPOLLRDNORM;
            }
            if signals & crate::ipc::port::SIGNAL_PEER_CLOSED != 0 {
                events |= EPOLLHUP;
            }
            if interest & (EPOLLOUT | EPOLLWRNORM) != 0
                && signals & crate::ipc::port::SIGNAL_WRITABLE != 0
            {
                events |= EPOLLOUT | EPOLLWRNORM;
            }
            // If this channel endpoint is a listening socket, check for
            // pending accepted connections (sys_accept readiness).
            if let Some(sid) = crate::net::socket::socket_id_for_fd(proc.pid, fd as u32) {
                if crate::net::socket::has_pending_accept(sid) {
                    events |= EPOLLIN | EPOLLRDNORM;
                }
            }
            events
        }
        KernelObject::Tty(tty) => {
            let mut events = 0u32;
            if interest & (EPOLLIN | EPOLLRDNORM) != 0 {
                let buf = tty.buf.lock();
                if !buf.is_empty() || tty.ctrlc.load(Ordering::Relaxed) {
                    events |= EPOLLIN | EPOLLRDNORM;
                }
            }
            if interest & (EPOLLOUT | EPOLLWRNORM) != 0 {
                events |= EPOLLOUT | EPOLLWRNORM;
            }
            events
        }
        KernelObject::VfsFile(_) => {
            let mut events = 0u32;
            if interest & (EPOLLIN | EPOLLRDNORM) != 0 {
                events |= EPOLLIN | EPOLLRDNORM;
            }
            if interest & (EPOLLOUT | EPOLLWRNORM) != 0 {
                events |= EPOLLOUT | EPOLLWRNORM;
            }
            events
        }
        KernelObject::Vmo(_) | KernelObject::Port(_) | KernelObject::Job(_)
        | KernelObject::Resource(_) => {
            if interest & EPOLLNVAL != 0 {
                EPOLLNVAL
            } else {
                0
            }
        }
        KernelObject::EpollInstance(_) => {
            if interest & EPOLLNVAL != 0 {
                EPOLLNVAL
            } else {
                0
            }
        }
    }
}

pub fn sys_epoll_create1(flags: usize, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);

    let instance = Arc::new(EpollInstance {
        items: crate::sync::Mutex::new(alloc::collections::BTreeMap::new()),
        waiter: core::sync::atomic::AtomicI32::new(0),
        saved_sigmask: core::sync::atomic::AtomicI32::new(-1),
    });

    let handle = proc.fds.lock().insert(KernelObject::EpollInstance(instance));

    if flags & EPOLL_CLOEXEC != 0 {
        proc.fds.lock().set_cloexec(handle, true);
    }

    tf.regs[10] = handle as usize;
}

fn register_epoll_with_fd(
    ep: &Arc<EpollInstance>,
    fd: i32,
    fds: &crate::ipc::handle::HandleTable,
) {
    match fds.get(fd as u32) {
        Some(KernelObject::PipeRead(half)) => {
            half.epoll_waiters
                .lock()
                .push(alloc::sync::Arc::downgrade(&ep));
        }
        Some(KernelObject::Channel(ch)) => {
            ch.epoll_waiters
                .lock()
                .push(alloc::sync::Arc::downgrade(&ep));
        }
        Some(KernelObject::Tty(tty)) => {
            tty.epoll_waiters
                .lock()
                .push(alloc::sync::Arc::downgrade(&ep));
        }
        _ => {}
    }
}

fn remove_one_waiter(
    waiters: &crate::sync::Mutex<alloc::vec::Vec<alloc::sync::Weak<EpollInstance>>>,
    ep: &Arc<EpollInstance>,
) {
    let mut list = waiters.lock();
    if let Some(pos) = list.iter().position(|w| {
        w.upgrade().map_or(false, |e| Arc::ptr_eq(&e, ep))
    }) {
        list.remove(pos);
    }
}

fn deregister_epoll_with_fd(
    ep: &Arc<EpollInstance>,
    fd: i32,
    fds: &crate::ipc::handle::HandleTable,
) {
    match fds.get(fd as u32) {
        Some(KernelObject::PipeRead(half)) => {
            remove_one_waiter(&half.epoll_waiters, ep);
        }
        Some(KernelObject::Channel(ch)) => {
            remove_one_waiter(&ch.epoll_waiters, ep);
        }
        Some(KernelObject::Tty(tty)) => {
            remove_one_waiter(&tty.epoll_waiters, ep);
        }
        _ => {}
    }
}

pub fn sys_epoll_ctl(epfd: usize, op: usize, fd: usize, event_ptr: usize, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);

    // Release fds lock immediately after cloning ep. check_fd_readiness also
    // acquires proc.fds, so holding fds across that call deadlocks on the
    // same HART (the Mutex detects recursive lock and panics).
    let ep = {
        let fds = proc.fds.lock();
        match fds.get(epfd as u32) {
            Some(KernelObject::EpollInstance(ep)) => ep.clone(),
            _ => {
                tf.regs[10] = crate::errno::EBADF;
                return;
            }
        }
    };

    match op {
        EPOLL_CTL_ADD => {
            if event_ptr == 0 {
                tf.regs[10] = crate::errno::EFAULT;
                return;
            }
            if !crate::mm::vmm::is_user_pointer_valid(tf, event_ptr as *const u8, 16) {
                tf.regs[10] = crate::errno::EFAULT;
                return;
            }
            let event = unsafe { &*(event_ptr as *const EpollEvent) };
            let events = event.events;
            let data = event.data;

            if ep.items.lock().contains_key(&(fd as i32)) {
                tf.regs[10] = crate::errno::EEXIST;
                return;
            }

            // check_fd_readiness acquires proc.fds internally; must not hold
            // ep.items or proc.fds at this point.
            let current_state = check_fd_readiness(fd as i32, events);
            ep.items.lock().insert(
                fd as i32,
                crate::ipc::handle::EpollItem {
                    events,
                    data,
                    last_state: current_state,
                },
            );
            let fds = proc.fds.lock();
            register_epoll_with_fd(&ep, fd as i32, &fds);
            tf.regs[10] = 0;
        }
        EPOLL_CTL_MOD => {
            if event_ptr == 0 {
                tf.regs[10] = crate::errno::EFAULT;
                return;
            }
            if !crate::mm::vmm::is_user_pointer_valid(tf, event_ptr as *const u8, 16) {
                tf.regs[10] = crate::errno::EFAULT;
                return;
            }
            let event = unsafe { &*(event_ptr as *const EpollEvent) };
            let events = event.events;
            let data = event.data;

            // check_fd_readiness acquires proc.fds; call before locking ep.items.
            let new_state = check_fd_readiness(fd as i32, events);
            let mut items = ep.items.lock();
            match items.get_mut(&(fd as i32)) {
                Some(item) => {
                    item.events = events;
                    item.data = data;
                    item.last_state = new_state;
                    tf.regs[10] = 0;
                }
                None => {
                    tf.regs[10] = crate::errno::ENOENT;
                }
            }
        }
        EPOLL_CTL_DEL => {
            let removed = ep.items.lock().remove(&(fd as i32));
            if removed.is_some() {
                let fds = proc.fds.lock();
                deregister_epoll_with_fd(&ep, fd as i32, &fds);
                tf.regs[10] = 0;
            } else {
                tf.regs[10] = crate::errno::ENOENT;
            }
        }
        _ => {
            tf.regs[10] = crate::errno::EINVAL;
        }
    }
}

/// Read a readiness vector for all watched fds, applying edge-triggered filtering.
///
/// Uses a single mutable pass so that `last_state` is updated atomically with
/// the readiness check, eliminating the TOCTOU window that the previous
/// two-pass design had between collecting events and updating last_state.
fn collect_ready(ep: &EpollInstance, maxevents: usize) -> Vec<(u32, u64)> {
    let mut items = ep.items.lock();
    if items.is_empty() {
        return Vec::new();
    }
    let mut ready: Vec<(u32, u64)> = Vec::new();
    for (&fd, item) in items.iter_mut() {
        if ready.len() >= maxevents {
            break;
        }
        // check_fd_readiness acquires proc.fds; ep.items is held here but
        // proc.fds is not, so there is no deadlock with sys_epoll_ctl (which
        // drops proc.fds before acquiring ep.items after the Bug-1 fix).
        let revents = check_fd_readiness(fd, item.events);
        let events_to_report = if item.events & EPOLLET != 0 {
            let old = item.last_state;
            item.last_state = revents; // update in same pass — no TOCTOU
            if revents == 0 {
                continue;
            }
            let new = revents & !old;
            if new == 0 {
                continue;
            }
            new
        } else {
            if revents == 0 {
                continue;
            }
            revents
        };
        ready.push((events_to_report, item.data));
    }
    ready
}

pub fn sys_epoll_pwait(
    epfd: usize,
    events_ptr: usize,
    maxevents: usize,
    timeout: usize,
    tf: &mut TrapFrame,
) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    let ep = {
        let fds = proc.fds.lock();
        match fds.get(epfd as u32) {
            Some(KernelObject::EpollInstance(ep)) => ep.clone(),
            _ => {
                tf.regs[10] = crate::errno::EBADF;
                return;
            }
        }
    };

    let pid = crate::posix::process::current_pid();

    // ── Detect timer-wakeup restart ──────────────────────────────────────
    // When this syscall blocks (sepc -= 4), ep.waiter is set to our PID.
    // If an event fires, wake_epoll_waiters clears ep.waiter to 0 before
    // adding us to the run queue.  If a timeout fires (wake_sleepers),
    // ep.waiter is never cleared, so it still holds our PID on re-entry.
    // Seeing our own PID here means the timeout expired with no events.
    if ep.waiter.load(Ordering::Acquire) == pid {
        ep.waiter.store(0, Ordering::Relaxed);
        let old = ep.saved_sigmask.swap(-1, Ordering::Relaxed);
        if old != -1 {
            proc.blocked_signals.store(old as u32, Ordering::Relaxed);
        }
        tf.regs[10] = 0; // timeout expired, no events
        return;
    }

    // Validate the output buffer if maxevents > 0.
    if maxevents > 0 {
        let buf_size = maxevents * 16;
        if !crate::mm::vmm::is_user_pointer_valid(tf, events_ptr as *const u8, buf_size) {
            tf.regs[10] = crate::errno::EFAULT;
            return;
        }
    }

    // ── Apply sigmask ────────────────────────────────────────────────────
    let sigmask_ptr = tf.regs[14]; // a4 = sigmask pointer
    if sigmask_ptr != 0 && ep.saved_sigmask.load(Ordering::Relaxed) == -1 {
        if !crate::mm::vmm::is_user_pointer_valid(tf, sigmask_ptr as *const u8, 4) {
            tf.regs[10] = crate::errno::EFAULT;
            return;
        }
        let new_mask = unsafe { *(sigmask_ptr as *const u32) };
        let old_mask = proc.blocked_signals.swap(new_mask, Ordering::Relaxed);
        ep.saved_sigmask.store(old_mask as i32, Ordering::Relaxed);
    }

    // ── Poll all watched fds ──────────────────────────────────────────────
    let ready = collect_ready(&ep, maxevents);

    if !ready.is_empty() || timeout as isize == 0 {
        // Restore saved sigmask before returning.
        let old = ep.saved_sigmask.swap(-1, Ordering::Relaxed);
        if old != -1 {
            proc.blocked_signals.store(old as u32, Ordering::Relaxed);
        }
        if !ready.is_empty() {
            let dst = events_ptr as *mut EpollEvent;
            for (i, (revents, data)) in ready.iter().enumerate() {
                unsafe {
                    dst.add(i).write(EpollEvent {
                        events: *revents,
                        _pad: 0,
                        data: *data,
                    });
                }
            }
            tf.regs[10] = ready.len();
        } else {
            tf.regs[10] = 0;
        }
        return;
    }

    // ── No events ready — block ──────────────────────────────────────────
    // Store our PID as the waiter sentinel. wake_epoll_waiters clears it
    // before waking us (event path); wake_sleepers does not (timeout path).
    // On re-entry we distinguish the two cases via the check at the top.
    ep.waiter.store(pid, Ordering::Release);

    let timeout_isize = timeout as isize;
    if timeout_isize > 0 {
        let ticks_to_wait = (timeout_isize as u64) * 10_000;
        let now = riscv::register::time::read() as u64;
        crate::posix::process::SLEEP_QUEUE
            .lock()
            .push((pid, now + ticks_to_wait));
    } else {
        let ticks_to_wait = 10_000_000u64 * 3600 * 24 * 365 * 10;
        let now = riscv::register::time::read() as u64;
        crate::posix::process::SLEEP_QUEUE
            .lock()
            .push((pid, now + ticks_to_wait));
    }

    {
        let proc_arc = crate::posix::process::PROCESS_TABLE
            .lock()
            .get(&pid)
            .cloned();
        if let Some(p) = proc_arc {
            *p.state.lock() = crate::posix::process::ProcState::Stopped;
        }
    }

    tf.sepc -= 4;
    crate::posix::process::schedule();
    unsafe { crate::trap::__halt_cpu() }
}

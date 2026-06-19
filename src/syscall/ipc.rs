use crate::trap::TrapFrame;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use crate::sync::Mutex;
use core::sync::atomic::Ordering;

unsafe extern "C" {
    pub(crate) fn __halt_cpu() -> !;
}

// ---------------------------------------------------------------------------
// POSIX byte-range file locking (F_SETLK / F_SETLKW / F_GETLK)
// ---------------------------------------------------------------------------

/// Lock type constants (l_type field of struct flock).
pub const F_RDLCK: i16 = 0; // shared/read lock
pub const F_WRLCK: i16 = 1; // exclusive/write lock
pub const F_UNLCK: i16 = 2; // unlock

/// Layout of userspace `struct flock` on 64-bit RISC-V (matches musl/glibc).
#[repr(C)]
struct FlockArg {
    l_type:   i16,
    l_whence: i16,
    _pad0:    i32,  // alignment padding before 8-byte off_t
    l_start:  i64,
    l_len:    i64,
    l_pid:    i32,
    _pad1:    i32,  // trailing padding; sizeof = 32
}

#[derive(Clone)]
struct LockEntry {
    pid:     i32,
    l_type:  i16,
    l_start: i64,
    l_end:   i64, // exclusive end; i64::MAX means "to EOF"
}

/// Global file lock table: file_id → list of active lock entries.
static FILE_LOCKS: Mutex<BTreeMap<u64, Vec<LockEntry>>> = Mutex::new(BTreeMap::new());

/// file_id → list of (pid, l_type, l_start, l_end) processes blocked in F_SETLKW.
#[allow(clippy::type_complexity)]
static LOCK_WAITERS: Mutex<BTreeMap<u64, Vec<(i32, i16, i64, i64)>>> =
    Mutex::new(BTreeMap::new());

/// Derive a stable file identity from a KernelObject for locking purposes.
/// VFS files use the server-assigned fd as the global identifier.  Non-lockable
/// objects return None (caller returns EINVAL).
fn file_lock_id(obj: &crate::ipc::handle::KernelObject) -> Option<u64> {
    match obj {
        crate::ipc::handle::KernelObject::VfsFile(sfd) => Some(*sfd as u64),
        _ => None,
    }
}

/// Compute the absolute (l_start, l_end) pair from the flock fields.
/// We only support SEEK_SET (whence=0); anything else is treated as SEEK_SET
/// because the kernel has no access to the current file-position cursor.
fn resolve_range(l_start: i64, l_len: i64) -> (i64, i64) {
    let start = l_start.max(0);
    let end = if l_len == 0 { i64::MAX } else { start.saturating_add(l_len) };
    (start, end)
}

fn ranges_overlap(s1: i64, e1: i64, s2: i64, e2: i64) -> bool {
    s1 < e2 && s2 < e1
}

/// True if `req` conflicts with `existing` (different pids, overlapping range,
/// incompatible lock types).
fn conflicts(req_pid: i32, req_type: i16, rs: i64, re: i64, e: &LockEntry) -> bool {
    if e.pid == req_pid { return false; }
    if !ranges_overlap(rs, re, e.l_start, e.l_end) { return false; }
    // write vs anything, or read vs write
    matches!((req_type, e.l_type), (F_WRLCK, _) | (F_RDLCK, F_WRLCK))
}

/// Attempt to acquire `(req_type, rs, re)` for `pid` on `file_id`.
/// Returns true on success. Does NOT sleep.
fn try_acquire(file_id: u64, pid: i32, l_type: i16, rs: i64, re: i64) -> bool {
    let mut table = FILE_LOCKS.lock();
    let entries = table.entry(file_id).or_insert_with(Vec::new);
    if entries.iter().any(|e| conflicts(pid, l_type, rs, re, e)) {
        return false;
    }
    // Remove any old locks this pid holds in the range (upgrade / replace).
    entries.retain(|e| e.pid != pid || !ranges_overlap(rs, re, e.l_start, e.l_end));
    entries.push(LockEntry { pid, l_type, l_start: rs, l_end: re });
    true
}

/// Remove all locks held by `pid` that overlap `(rs, re)` on `file_id`.
fn release_range(file_id: u64, pid: i32, rs: i64, re: i64) {
    let mut table = FILE_LOCKS.lock();
    if let Some(entries) = table.get_mut(&file_id) {
        entries.retain(|e| e.pid != pid || !ranges_overlap(rs, re, e.l_start, e.l_end));
        if entries.is_empty() { table.remove(&file_id); }
    }
    // After releasing, try to wake waiters blocked on this file_id.
    drop(table);
    wake_lock_waiters(file_id);
}

/// Remove ALL locks held by `pid` across every file (called on process exit).
pub fn release_all_locks(pid: i32) {
    let mut table = FILE_LOCKS.lock();
    let ids: Vec<u64> = table.keys().cloned().collect();
    for id in &ids {
        if let Some(v) = table.get_mut(id) {
            v.retain(|e| e.pid != pid);
            if v.is_empty() { table.remove(id); }
        }
    }
    drop(table);
    // Wake any waiters that were blocked behind this pid's locks.
    let waiter_ids: Vec<u64> = LOCK_WAITERS.lock().keys().cloned().collect();
    for id in waiter_ids { wake_lock_waiters(id); }
}

/// Remove all locks held by `pid` on `file_id` (called on fd close).
pub fn release_fd_locks(file_id: u64, pid: i32) {
    release_range(file_id, pid, 0, i64::MAX);
}

/// Wake up any F_SETLKW waiters on `file_id` whose lock can now be acquired.
fn wake_lock_waiters(file_id: u64) {
    use crate::posix::process::{PROCESS_TABLE, ProcState, IpcState, enqueue_with_prio};
    let mut waiters_map = LOCK_WAITERS.lock();
    let Some(waiters) = waiters_map.get_mut(&file_id) else { return };
    if waiters.is_empty() { return; }

    let mut still_waiting: Vec<(i32, i16, i64, i64)> = Vec::new();
    for (wpid, wtype, ws, we) in waiters.drain(..) {
        if try_acquire(file_id, wpid, wtype, ws, we) {
            // Lock acquired on behalf of the waiter — wake it with success.
            let table = PROCESS_TABLE.lock();
            if let Some(proc) = table.get(&wpid) {
                proc.trap_frame.lock().regs[10] = 0;
                *proc.ipc_state.lock() = IpcState::None;
                *proc.state.lock() = ProcState::Running;
                let prio = proc.priority.load(Ordering::Relaxed);
                drop(table);
                enqueue_with_prio(wpid, prio);
            }
        } else {
            still_waiting.push((wpid, wtype, ws, we));
        }
    }
    *waiters = still_waiting;
}

pub fn sys_pipe(arg0: usize, tf: &mut TrapFrame) {
    let (rh, wh) = crate::ipc::handle::create_pipe();
    let proc = crate::get_current_proc_or_esrch!(tf);
    let mut fds = proc.fds.lock();
    let h_r = fds.insert(crate::ipc::handle::KernelObject::PipeRead(rh));
    let h_w = fds.insert(crate::ipc::handle::KernelObject::PipeWrite(wh));

    if !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const [u32; 2], 1) {
        tf.regs[10] = usize::MAX;
        return;
    }

    unsafe {
        let arr = arg0 as *mut [u32; 2];
        (*arr)[0] = h_r;
        (*arr)[1] = h_w;
    }
    tf.regs[10] = 0;
}

pub fn sys_dup(arg0: usize, tf: &mut TrapFrame) {
    let old_fd = arg0 as u32;
    let proc = crate::get_current_proc_or_esrch!(tf);
    let mut fds = proc.fds.lock();
    
    match fds.get_entry(old_fd) {
        Some(entry) => {
            if !entry.rights.contains(crate::ipc::handle::Rights::DUPLICATE) {
                tf.regs[10] = crate::errno::EACCES;
                return;
            }
        }
        None => {
            tf.regs[10] = crate::errno::EBADF;
            return;
        }
    }

    if let Some(new_fd) = fds.dup(old_fd) {
        tf.regs[10] = new_fd as usize;
    } else {
        tf.regs[10] = crate::errno::EBADF;
    }
}

pub fn sys_dup2(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let old_fd = arg0 as u32;
    let new_fd = arg1 as u32;
    if old_fd == new_fd {
        tf.regs[10] = new_fd as usize;
        return;
    }
    let proc = crate::get_current_proc_or_esrch!(tf);
    let mut fds = proc.fds.lock();
    
    match fds.get_entry(old_fd) {
        Some(entry) => {
            if !entry.rights.contains(crate::ipc::handle::Rights::DUPLICATE) {
                tf.regs[10] = crate::errno::EACCES;
                return;
            }
        }
        None => {
            tf.regs[10] = crate::errno::EBADF;
            return;
        }
    }

    if fds.dup2(old_fd, new_fd) {
        tf.regs[10] = new_fd as usize;
    } else {
        tf.regs[10] = crate::errno::EBADF;
    }
}


pub fn sys_mailbox_send(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let to_pid = arg0 as i32;
    let ptr = arg1 as *const u8;
    let len = arg2;

    if len > 4096 {
        tf.regs[10] = usize::MAX;
        return;
    }

    // Add bounds checking for ptr
    if !crate::mm::vmm::is_user_pointer_valid(tf, ptr, len) {
        tf.regs[10] = usize::MAX;
        return;
    }

    let mut buf = alloc::vec![0u8; len];
    unsafe {
        core::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), len);
    }

    let sender_pid = crate::posix::process::current_pid();
    let table = crate::posix::process::PROCESS_TABLE.lock();
    if let Some(target) = table.get(&to_pid) {
        target.mailbox.lock().push_back((sender_pid, buf));
        if target.mailbox_waiter.load(core::sync::atomic::Ordering::Relaxed) != 0 {
            target.mailbox_waiter.store(0, core::sync::atomic::Ordering::Relaxed);
            let mut state = target.state.lock();
            if matches!(*state, crate::posix::process::ProcState::Stopped) {
                *state = crate::posix::process::ProcState::Running;
                crate::posix::process::enqueue_with_prio(to_pid, target.priority.load(core::sync::atomic::Ordering::Relaxed));
            }
        }
        tf.regs[10] = 0;
    } else {
        tf.regs[10] = usize::MAX;
    }
}

pub fn sys_mailbox_receive(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    let ptr = arg0 as *mut u8;
    let max_len = arg1;
    let from_ptr = arg2 as *mut i32;
    let proc = crate::get_current_proc_or_esrch!(tf);
    
    let msg = {
        let mut mb = proc.mailbox.lock();
        mb.pop_front()
    };
    
    if let Some((sender, buf)) = msg {
        let copy_len = core::cmp::min(max_len, buf.len());

        // Add bounds checking for ptr
        if !crate::mm::vmm::is_user_pointer_valid(tf, ptr, copy_len) {
            tf.regs[10] = usize::MAX;
            return;
        }

        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, copy_len);
            if !from_ptr.is_null() {
                *from_ptr = sender;
            }
        }
        tf.regs[10] = copy_len as usize;
    } else {
        proc.mailbox_waiter.store(1, core::sync::atomic::Ordering::Relaxed);
        *proc.state.lock() = crate::posix::process::ProcState::Stopped;
        tf.sepc -= 4; // Re-execute syscall when woken up
        crate::posix::process::schedule();
        unsafe { __halt_cpu() }
    }
}


pub fn kernel_send_msg(target_pid: i32, msg: crate::ipc::msg::Message) {
    let table = crate::posix::process::PROCESS_TABLE.lock();
    if let Some(target) = table.get(&target_pid) {
        let mut target_state = target.ipc_state.lock();
        match *target_state {
            crate::posix::process::IpcState::Receiving { source, msg_ptr: _ } if source == msg.source || source == -1 => {
                *target_state = crate::posix::process::IpcState::MessageAvailable { msg };
                crate::posix::process::enqueue_with_prio(target_pid, target.priority.load(core::sync::atomic::Ordering::Relaxed));
            }
            crate::posix::process::IpcState::None => {
                *target_state = crate::posix::process::IpcState::MessageAvailable { msg };
            }
            _ => {
                // Target already has a message or is mid-IPC — queue for later delivery
                // so we don't silently drop rapid back-to-back interrupts.
                target.irq_pending.lock().push_back(msg);
            }
        }
    }
}

pub fn sys_ipc_send(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let to_pid = arg0 as i32;
    let msg_ptr = arg1;
    let sender_pid = crate::posix::process::current_pid();

    let table = crate::posix::process::PROCESS_TABLE.lock();
    let sender = table.get(&sender_pid).unwrap();
    
    // Check if we are resuming from a successful send
    {
        let mut state = sender.ipc_state.lock();
        if let crate::posix::process::IpcState::SendComplete = *state {
            *state = crate::posix::process::IpcState::None;
            tf.regs[10] = 0; // Success
            return;
        }
    }

    // Read message from userspace
    let mut local_msg: crate::ipc::msg::Message = unsafe { core::mem::zeroed() };

    // Add bounds checking for msg_ptr
    if !crate::mm::vmm::is_user_pointer_valid(tf, msg_ptr as *const crate::ipc::msg::Message, 1) {
        tf.regs[10] = usize::MAX;
        return;
    }

    unsafe {
        core::ptr::copy_nonoverlapping(msg_ptr as *const crate::ipc::msg::Message, &mut local_msg, 1);
    }
    local_msg.source = sender_pid;

    if let Some(target) = table.get(&to_pid) {
        let mut target_state = target.ipc_state.lock();
        match *target_state {
            crate::posix::process::IpcState::Receiving { source, msg_ptr: _ } |
            crate::posix::process::IpcState::ReceivingReply { source, msg_ptr: _ } if source == sender_pid || source == -1 => {
                // Target is waiting. Give them the message.
                *target_state = crate::posix::process::IpcState::MessageAvailable { msg: local_msg };
                crate::posix::process::enqueue_with_prio(to_pid, target.priority.load(core::sync::atomic::Ordering::Relaxed));
                tf.regs[10] = 0; // Success immediately, no need to block
            }
            _ => {
                crate::println!("sys_ipc_send blocking! target_pid={}, target_state={:?}", to_pid, *target_state);
                // Target not waiting. Block ourselves.
                *sender.ipc_state.lock() = crate::posix::process::IpcState::Sending {
                    target: to_pid,
                    msg: local_msg,
                    reply_msg_ptr: None,
                };
                target.senders.lock().push_back(sender_pid);
                
                tf.sepc -= 4; // Re-execute when woken up
                drop(target_state);
                drop(table);
                crate::posix::process::schedule();
                unsafe { __halt_cpu() }
            }
        }
    } else {
        crate::println!("sys_ipc_send: target_pid={} not found in table!", to_pid);
        tf.regs[10] = usize::MAX; // ESRCH
    }
}



pub fn sys_ipc_receive(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let from_pid = arg0 as i32;
    let msg_ptr = arg1;
    let receiver_pid = crate::posix::process::current_pid();

    let table = crate::posix::process::PROCESS_TABLE.lock();
    let receiver = table.get(&receiver_pid).unwrap();
    
    // Check if we have a message available (either from a previous wake or deposited by a sender)
    {
        let mut state = receiver.ipc_state.lock();
        if let crate::posix::process::IpcState::MessageAvailable { msg } = *state {
            let sender_pid = msg.source;

            // Add bounds checking for msg_ptr
            if !crate::mm::vmm::is_user_pointer_valid(tf, msg_ptr as *mut crate::ipc::msg::Message, 1) {
                tf.regs[10] = usize::MAX;
                return;
            }

            unsafe {
                core::ptr::copy_nonoverlapping(&msg, msg_ptr as *mut crate::ipc::msg::Message, 1);
            }
            // Promote the next queued IRQ message (if any) so it isn't lost
            let next = receiver.irq_pending.lock().pop_front();
            *state = match next {
                Some(next_msg) => crate::posix::process::IpcState::MessageAvailable { msg: next_msg },
                None => crate::posix::process::IpcState::None,
            };
            tf.regs[10] = sender_pid as usize; // Return sender PID
            return;
        }
    }
    // Also drain irq_pending before we scan senders or block, so IRQs that
    // arrived while we were in a non-receivable state are not missed.
    {
        let mut pending = receiver.irq_pending.lock();
        if let Some(pos) = pending.iter().position(|m| from_pid == -1 || from_pid == m.source) {
            let msg = pending.remove(pos).unwrap();
            let sender_pid = msg.source;

            // Add bounds checking for msg_ptr
            if !crate::mm::vmm::is_user_pointer_valid(tf, msg_ptr as *mut crate::ipc::msg::Message, 1) {
                tf.regs[10] = usize::MAX;
                return;
            }

            unsafe {
                core::ptr::copy_nonoverlapping(&msg, msg_ptr as *mut crate::ipc::msg::Message, 1);
            }
            tf.regs[10] = sender_pid as usize;
            return;
        }
    }
    
    let mut senders = receiver.senders.lock();
    
    // Find a matching sender
    let mut found_idx = None;
    for (i, &sender_pid) in senders.iter().enumerate() {
        if from_pid == -1 || from_pid == sender_pid {
            found_idx = Some(i);
            break;
        }
    }
    
    if let Some(idx) = found_idx {
        // We have a pending sender!
        let sender_pid = senders.remove(idx).unwrap();
        let sender = table.get(&sender_pid).unwrap();
        let mut sender_state = sender.ipc_state.lock();
        
        if let crate::posix::process::IpcState::Sending { msg, reply_msg_ptr, .. } = *sender_state {
            let sender_pid_actual = msg.source; // Should be sender_pid

            // Add bounds checking for msg_ptr
            if !crate::mm::vmm::is_user_pointer_valid(tf, msg_ptr as *mut crate::ipc::msg::Message, 1) {
                tf.regs[10] = usize::MAX;
                return;
            }

            // Copy message to our userspace
            unsafe {
                core::ptr::copy_nonoverlapping(&msg, msg_ptr as *mut crate::ipc::msg::Message, 1);
            }
            
            if let Some(r_ptr) = reply_msg_ptr {
                // Sender was using sendrec, transition them to waiting for reply
                *sender_state = crate::posix::process::IpcState::ReceivingReply {
                    source: receiver_pid,
                    msg_ptr: r_ptr,
                };
                // Do NOT wake them yet, they wait for reply!
            } else {
                // Sender was using send, they are done
                *sender_state = crate::posix::process::IpcState::SendComplete;
                crate::posix::process::enqueue_with_prio(sender_pid, sender.priority.load(core::sync::atomic::Ordering::Relaxed));
            }
            
            tf.regs[10] = sender_pid_actual as usize; // Return sender PID
        } else {
            tf.regs[10] = usize::MAX; // Should not happen
        }
    } else {
        // No pending sender. Block receiver.
        *receiver.ipc_state.lock() = crate::posix::process::IpcState::Receiving {
            source: from_pid,
            msg_ptr,
        };
        tf.sepc -= 4; // Re-execute when woken up
        drop(senders);
        drop(table);
        crate::posix::process::schedule();
        unsafe { __halt_cpu() }
    }
}


pub fn sys_ipc_sendrec(arg0: usize, arg1: usize, tf: &mut TrapFrame) {
    let to_pid = arg0 as i32;
    let msg_ptr = arg1;

    let table = crate::posix::process::PROCESS_TABLE.lock();
    let sender_pid = crate::posix::process::current_pid();
    let sender = table.get(&sender_pid).unwrap();
    
    // Check our current state to see where we are in the transaction
    let mut state = sender.ipc_state.lock();
    
    match *state {
        crate::posix::process::IpcState::MessageAvailable { msg } => {
            // We received the reply! Copy to userspace and finish.
            unsafe {
                core::ptr::copy_nonoverlapping(&msg, msg_ptr as *mut crate::ipc::msg::Message, 1);
            }
            *state = crate::posix::process::IpcState::None;
            tf.regs[10] = 0;
        }
        crate::posix::process::IpcState::ReceivingReply { .. } => {
            // Spurious wakeup, go back to sleep
            tf.sepc -= 4;
            drop(state);
            drop(table);
            crate::posix::process::schedule();
            unsafe { __halt_cpu() }
        }
        crate::posix::process::IpcState::None => {
            // Initial call: we need to send the message
            let mut local_msg: crate::ipc::msg::Message = unsafe { core::mem::zeroed() };
            unsafe {
                core::ptr::copy_nonoverlapping(msg_ptr as *const crate::ipc::msg::Message, &mut local_msg, 1);
            }
            local_msg.source = sender_pid;

            if let Some(target) = table.get(&to_pid) {
                let mut target_state = target.ipc_state.lock();
                match *target_state {
                    crate::posix::process::IpcState::Receiving { source, msg_ptr: _ } if source == sender_pid || source == -1 => {
                        // Target is ready. Deposit message and transition to ReceivingReply.
                        *target_state = crate::posix::process::IpcState::MessageAvailable { msg: local_msg };
                        crate::posix::process::enqueue_with_prio(to_pid, target.priority.load(core::sync::atomic::Ordering::Relaxed));

                        *state = crate::posix::process::IpcState::ReceivingReply {
                            source: to_pid,
                            msg_ptr,
                        };
                        tf.sepc -= 4; // Block to wait for reply
                        drop(target_state);
                        drop(state);
                        drop(table);
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                    _ => {
                        // Target not ready. Block and wait to send.
                        *state = crate::posix::process::IpcState::Sending {
                            target: to_pid,
                            msg: local_msg,
                            reply_msg_ptr: Some(msg_ptr),
                        };
                        target.senders.lock().push_back(sender_pid);
                        
                        tf.sepc -= 4; // Block to wait for send AND reply
                        drop(target_state);
                        drop(state);
                        drop(table);
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                }
            } else {
                tf.regs[10] = usize::MAX; // Target doesn't exist
            }
        }
        _ => {
            // Inconsistent state
            *state = crate::posix::process::IpcState::None;
            tf.regs[10] = usize::MAX;
        }
    }
}

// Standard POSIX fcntl commands
pub const F_DUPFD:  usize = 0;
pub const F_GETFD:  usize = 1;
pub const F_SETFD:  usize = 2;
pub const F_GETFL:  usize = 3;
pub const F_SETFL:  usize = 4;
pub const F_GETLK:  usize = 5;
pub const F_SETLK:  usize = 6;
pub const F_SETLKW: usize = 7;

pub const FD_CLOEXEC: usize = 1;

// ── poll(2) constants ──────────────────────────────────────────────────────────
pub const POLLIN:  i16 = 0x001;
pub const POLLOUT: i16 = 0x004;
pub const POLLERR: i16 = 0x008;
pub const POLLHUP: i16 = 0x010;
pub const POLLNVAL: i16 = 0x020;
// Non-standard convenience alias for `POLLERR | POLLHUP | POLLNVAL`.
pub const POLLNBF: i16 = POLLERR | POLLHUP | POLLNVAL;

#[repr(C)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

// Non-POSIX kernel-private commands (high range to avoid clashing with future POSIX additions)
pub const F_SET_VFS_FD: usize = 1000;
pub const F_GET_VFS_FD: usize = 1001;

// Global map: (pid, kernel_fd) → file status flags (F_GETFL / F_SETFL)
static FD_FLAGS: Mutex<BTreeMap<(i32, u32), usize>> = Mutex::new(BTreeMap::new());

pub fn sys_fcntl(arg0: usize, arg1: usize, arg2: usize, tf: &mut TrapFrame) {
    use crate::ipc::handle::KernelObject;
    use crate::posix::process::{ProcState, IpcState};
    use crate::mm::vmm::is_user_pointer_valid;

    let proc = crate::get_current_proc_or_esrch!(tf);
    let pid  = crate::posix::process::current_pid();
    let fd   = arg0 as u32;

    // All commands except F_SET_VFS_FD need a valid fd.
    if arg1 != F_SET_VFS_FD {
        if proc.fds.lock().get(fd).is_none() {
            tf.regs[10] = crate::errno::EBADF;
            return;
        }
    }

    match arg1 {
        // ── standard fd management ───────────────────────────────────────────
        F_DUPFD => {
            let min_fd = arg2 as u32;
            let mut fds = proc.fds.lock();
            let obj = fds.get(fd).unwrap().clone();
            let mut newfd = min_fd;
            while fds.get(newfd).is_some() { newfd += 1; }
            fds.insert_at(newfd, obj);
            tf.regs[10] = newfd as usize;
        }
        F_GETFD => {
            tf.regs[10] = if proc.fds.lock().is_cloexec(fd) { FD_CLOEXEC } else { 0 };
        }
        F_SETFD => {
            proc.fds.lock().set_cloexec(fd, (arg2 & FD_CLOEXEC) != 0);
            tf.regs[10] = 0;
        }
        F_GETFL => {
            let flags = FD_FLAGS.lock().get(&(pid, fd)).copied().unwrap_or(0);
            tf.regs[10] = flags;
        }
        F_SETFL => {
            // Only O_NONBLOCK, O_APPEND, O_ASYNC are meaningful; store the
            // whole mask so F_GETFL reflects what the caller wrote.
            FD_FLAGS.lock().insert((pid, fd), arg2);
            tf.regs[10] = 0;
        }

        // ── POSIX byte-range locking ─────────────────────────────────────────
        F_GETLK | F_SETLK | F_SETLKW => {
            // Validate the flock pointer.
            if !is_user_pointer_valid(tf, arg2 as *const FlockArg, 1) {
                tf.regs[10] = crate::errno::EFAULT;
                return;
            }
            let fl = unsafe { &*(arg2 as *const FlockArg) };
            let (rs, re) = resolve_range(fl.l_start, fl.l_len);

            // Resolve file_id; only VFS files support POSIX locks.
            let lock_id = {
                let fds = proc.fds.lock();
                match fds.get(fd) {
                    Some(obj) => match file_lock_id(obj) {
                        Some(id) => id,
                        None => { tf.regs[10] = crate::errno::EINVAL; return; }
                    },
                    None => { tf.regs[10] = crate::errno::EBADF; return; }
                }
            };

            match arg1 {
                F_GETLK => {
                    // Report the first conflicting lock, or set l_type=F_UNLCK if none.
                    let table = FILE_LOCKS.lock();
                    let blocking = table.get(&lock_id)
                        .and_then(|v| v.iter().find(|e| conflicts(pid, fl.l_type, rs, re, e)))
                        .cloned();
                    drop(table);
                    let out = unsafe { &mut *(arg2 as *mut FlockArg) };
                    if let Some(b) = blocking {
                        out.l_type   = b.l_type;
                        out.l_whence = 0; // SEEK_SET
                        out.l_start  = b.l_start;
                        out.l_len    = if b.l_end == i64::MAX { 0 } else { b.l_end - b.l_start };
                        out.l_pid    = b.pid;
                    } else {
                        out.l_type = F_UNLCK;
                    }
                    tf.regs[10] = 0;
                }
                F_SETLK => {
                    if fl.l_type == F_UNLCK {
                        release_range(lock_id, pid, rs, re);
                        tf.regs[10] = 0;
                    } else if try_acquire(lock_id, pid, fl.l_type, rs, re) {
                        tf.regs[10] = 0;
                    } else {
                        tf.regs[10] = crate::errno::EAGAIN;
                    }
                }
                F_SETLKW => {
                    if fl.l_type == F_UNLCK {
                        release_range(lock_id, pid, rs, re);
                        tf.regs[10] = 0;
                        return;
                    }
                    if try_acquire(lock_id, pid, fl.l_type, rs, re) {
                        tf.regs[10] = 0;
                        return;
                    }
                    // Lock is not available — block the process.
                    LOCK_WAITERS.lock()
                        .entry(lock_id)
                        .or_insert_with(Vec::new)
                        .push((pid, fl.l_type, rs, re));
                    *proc.ipc_state.lock() = IpcState::LockWaiting {
                        lock_id, l_type: fl.l_type, l_start: rs, l_end: re,
                    };
                    *proc.state.lock() = ProcState::Stopped;
                    drop(proc);
                    crate::posix::process::schedule();
                    // NOTE: regs[10] is set to 0 by wake_lock_waiters when woken.
                }
                _ => unreachable!(),
            }
        }

        // ── kernel-private VFS fd plumbing ──────────────────────────────────
        F_SET_VFS_FD => {
            let server_fd = arg2 as u32;
            let mut fds = proc.fds.lock();
            if fd == u32::MAX {
                let newfd = fds.insert(KernelObject::VfsFile(server_fd));
                tf.regs[10] = newfd as usize;
            } else {
                fds.insert_at(fd, KernelObject::VfsFile(server_fd));
                tf.regs[10] = 0;
            }
        }
        F_GET_VFS_FD => {
            let fds = proc.fds.lock();
            if let Some(KernelObject::VfsFile(server_fd)) = fds.get(fd) {
                tf.regs[10] = *server_fd as usize;
            } else {
                // usize::MAX signals "no VFS fd" — libos checks != usize::MAX
                tf.regs[10] = usize::MAX;
            }
        }
        _ => {
            tf.regs[10] = crate::errno::ENOSYS;
        }
    }
}

pub fn sys_datacopy(
    src_pid: usize,
    src_va: usize,
    dst_pid: usize,
    dst_va: usize,
    len: usize,
    tf: &mut TrapFrame,
) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    if proc.caps.load(core::sync::atomic::Ordering::Relaxed) & crate::posix::process::CAP_DATACOPY == 0 {
        tf.regs[10] = crate::errno::EPERM;
        return;
    }

    let src_pid = src_pid as i32;
    let dst_pid = dst_pid as i32;

    let table = crate::posix::process::PROCESS_TABLE.lock();
    let src_proc = match table.get(&src_pid) {
        Some(p) => p,
        None => {
            tf.regs[10] = usize::MAX; // ESRCH
            return;
        }
    };
    let dst_proc = match table.get(&dst_pid) {
        Some(p) => p,
        None => {
            tf.regs[10] = usize::MAX; // ESRCH
            return;
        }
    };

    let src_root_pa = (src_proc.satp_val.load(core::sync::atomic::Ordering::Acquire) & 0xFFF_FFFF_FFFF) << 12;
    let dst_root_pa = (dst_proc.satp_val.load(core::sync::atomic::Ordering::Acquire) & 0xFFF_FFFF_FFFF) << 12;

    // Drop the lock before we start memory copying to avoid holding it during page faults or long copies
    drop(table);

    let mut copied = 0;
    while copied < len {
        let current_src_va = src_va + copied;
        let current_dst_va = dst_va + copied;

        // Try to resolve source PA
        let src_pa = match crate::mm::vmm::PageTable::walk_page_table(src_root_pa, current_src_va) {
            Ok((pa, flags)) => {
                if flags & crate::mm::vmm::PTE_R == 0 {
                    tf.regs[10] = usize::MAX; // EFAULT
                    return;
                }
                pa
            }
            Err(_) => {
                // Not mapped, try to fault it in
                if crate::mm::vmm::handle_user_page_fault(src_root_pa, current_src_va, 0).is_err() {
                    tf.regs[10] = usize::MAX; // EFAULT
                    return;
                }
                // Retry walk
                match crate::mm::vmm::PageTable::walk_page_table(src_root_pa, current_src_va) {
                    Ok((pa, _)) => pa,
                    Err(_) => {
                        tf.regs[10] = usize::MAX; // EFAULT
                        return;
                    }
                }
            }
        };

        // Try to resolve destination PA
        let dst_pa = match crate::mm::vmm::PageTable::walk_page_table(dst_root_pa, current_dst_va) {
            Ok((pa, flags)) => {
                if flags & crate::mm::vmm::PTE_W == 0 {
                    // Try COW
                    if crate::mm::vmm::handle_store_page_fault(dst_root_pa, current_dst_va).is_err() {
                        tf.regs[10] = usize::MAX; // EFAULT
                        return;
                    }
                    match crate::mm::vmm::PageTable::walk_page_table(dst_root_pa, current_dst_va) {
                        Ok((pa, _)) => pa,
                        Err(_) => {
                            tf.regs[10] = usize::MAX; // EFAULT
                            return;
                        }
                    }
                } else {
                    pa
                }
            }
            Err(_) => {
                // Not mapped, try to fault it in
                if crate::mm::vmm::handle_user_page_fault(dst_root_pa, current_dst_va, 0).is_err() {
                    tf.regs[10] = usize::MAX; // EFAULT
                    return;
                }
                // Retry walk
                match crate::mm::vmm::PageTable::walk_page_table(dst_root_pa, current_dst_va) {
                    Ok((pa, _)) => pa,
                    Err(_) => {
                        tf.regs[10] = usize::MAX; // EFAULT
                        return;
                    }
                }
            }
        };

        // Calculate how many bytes we can copy in this chunk (up to page boundary for both)
        let src_page_offset = current_src_va % crate::mm::pmm::PAGE_SIZE;
        let dst_page_offset = current_dst_va % crate::mm::pmm::PAGE_SIZE;
        let src_rem = crate::mm::pmm::PAGE_SIZE - src_page_offset;
        let dst_rem = crate::mm::pmm::PAGE_SIZE - dst_page_offset;
        let chunk = core::cmp::min(len - copied, core::cmp::min(src_rem, dst_rem));

        unsafe {
            core::ptr::copy_nonoverlapping(src_pa as *const u8, dst_pa as *mut u8, chunk);
        }

        copied += chunk;
    }

    tf.regs[10] = copied;
}

pub fn sys_channel_create(arg0: usize, tf: &mut TrapFrame) {
    let (ep1, ep2) = crate::ipc::channel::ChannelEndpoint::create_pair();
    let proc = crate::get_current_proc_or_esrch!(tf);
    let mut fds = proc.fds.lock();
    let h1 = fds.insert(crate::ipc::handle::KernelObject::Channel(ep1));
    let h2 = fds.insert(crate::ipc::handle::KernelObject::Channel(ep2));

    if !crate::mm::vmm::is_user_pointer_valid(tf, arg0 as *const [u32; 2], 1) {
        fds.remove(h1);
        fds.remove(h2);
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    unsafe {
        let arr = arg0 as *mut [u32; 2];
        (*arr)[0] = h1;
        (*arr)[1] = h2;
    }
    tf.regs[10] = 0;
}

pub fn sys_channel_write(
    fd: usize,
    buf_ptr: usize,
    buf_len: usize,
    handles_ptr: usize,
    handles_len: usize,
    tf: &mut TrapFrame,
) {
    if handles_len > 8 {
        tf.regs[10] = crate::errno::EINVAL;
        return;
    }

    if buf_len > 0 && !crate::mm::vmm::is_user_pointer_valid(tf, buf_ptr as *const u8, buf_len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    if handles_len > 0 && !crate::mm::vmm::is_user_pointer_valid(tf, handles_ptr as *const u32, handles_len) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    let mut bytes = alloc::vec![0u8; buf_len];
    if buf_len > 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(buf_ptr as *const u8, bytes.as_mut_ptr(), buf_len);
        }
    }

    let mut handle_ids = alloc::vec![0u32; handles_len];
    if handles_len > 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(handles_ptr as *const u32, handle_ids.as_mut_ptr(), handles_len);
        }
    }
    let proc = crate::get_current_proc_or_esrch!(tf);

    let mut handles = alloc::vec::Vec::with_capacity(handles_len);
    let ep = {
        let mut fds = proc.fds.lock();

        // 1. Resolve fd to ChannelEndpoint with WRITE right
        let ep = match fds.get_with_rights(fd as u32, crate::ipc::handle::Rights::WRITE) {
            Ok(crate::ipc::handle::KernelObject::Channel(ep)) => ep.clone(),
            _ => {
                tf.regs[10] = crate::errno::EACCES;
                return;
            }
        };

        // 2. Disallow sending the channel's own handle to prevent deadlocks/ownership loop
        for &h in &handle_ids {
            if h == fd as u32 {
                tf.regs[10] = crate::errno::EINVAL;
                return;
            }
        }

        // 2.5 Verify all handles in the write array are unique to prevent duplicate transfers
        for (i, &h) in handle_ids.iter().enumerate() {
            if handle_ids[..i].contains(&h) {
                tf.regs[10] = crate::errno::EINVAL;
                return;
            }
        }

        // 3. Verify all handles exist and have TRANSFER rights
        for &h in &handle_ids {
            match fds.get_entry(h) {
                Some(entry) => {
                    if !entry.rights.contains(crate::ipc::handle::Rights::TRANSFER) {
                        tf.regs[10] = crate::errno::EACCES;
                        return;
                    }
                }
                None => {
                    tf.regs[10] = crate::errno::EBADF;
                    return;
                }
            }
        }

        // 4. Remove handle entries from caller's table
        for &h in &handle_ids {
            if let Some(entry) = fds.remove_entry(h) {
                handles.push(entry);
            }
        }

        ep
    };

    // 5. Write message to endpoint
    let msg = crate::ipc::channel::Message {
        bytes,
        handles,
    };

    match ep.write_fast(msg) {
        Ok(waiter_pid) => {
            tf.regs[10] = 0;
            if let Some(waiter) = waiter_pid {
                let current_pid = crate::posix::process::current_pid();
                crate::posix::process::enqueue(current_pid);
                crate::posix::process::enqueue(waiter);
                crate::posix::process::schedule();
                unsafe { __halt_cpu() }
            }
        }
        Err(_) => {
            tf.regs[10] = usize::MAX; // -1 on error
        }
    }
}

pub fn sys_channel_read(
    fd: usize,
    buf_ptr: usize,
    buf_len: usize,
    handles_ptr: usize,
    handles_len: usize,
    tf: &mut TrapFrame,
) {
    if buf_len > 0 && !crate::mm::vmm::is_user_pointer_valid(tf, buf_ptr as *mut u8, buf_len) {
        tf.regs[10] = crate::errno::EFAULT;
        tf.regs[11] = 0;
        return;
    }

    if handles_len > 0 && !crate::mm::vmm::is_user_pointer_valid(tf, handles_ptr as *mut u32, handles_len) {
        tf.regs[10] = crate::errno::EFAULT;
        tf.regs[11] = 0;
        return;
    }
    let proc = crate::get_current_proc_or_esrch!(tf);

    let ep_arc = {
        let fds = proc.fds.lock();
        match fds.get_with_rights(fd as u32, crate::ipc::handle::Rights::READ) {
            Ok(crate::ipc::handle::KernelObject::Channel(ep)) => ep.clone(),
            _ => {
                tf.regs[10] = crate::errno::EACCES;
                tf.regs[11] = 0;
                return;
            }
        }
    };

    // Attempt to receive a message
    match ep_arc.try_recv_constrained(buf_len, handles_len) {
        Ok(Some(msg)) => {
            copy_msg_to_user(msg, buf_ptr, buf_len, handles_ptr, handles_len, &proc, tf);
        }
        Ok(None) => {
            // If peer is closed and no messages left, return EOF (0 bytes, 0 handles)
            if ep_arc.is_peer_closed() {
                tf.regs[10] = 0;
                tf.regs[11] = 0;
                return;
            }

            // Register as waiter
            ep_arc.waiter.store(
                crate::posix::process::current_pid(),
                core::sync::atomic::Ordering::Relaxed,
            );

            // Lost-wakeup check
            match ep_arc.try_recv_constrained(buf_len, handles_len) {
                Ok(Some(msg)) => {
                    ep_arc.waiter.store(0, core::sync::atomic::Ordering::Relaxed);
                    copy_msg_to_user(msg, buf_ptr, buf_len, handles_ptr, handles_len, &proc, tf);
                }
                Ok(None) => {
                    if ep_arc.is_peer_closed() {
                        ep_arc.waiter.store(0, core::sync::atomic::Ordering::Relaxed);
                        tf.regs[10] = 0;
                        tf.regs[11] = 0;
                    } else {
                        // Block
                        tf.sepc -= 4; // Re-execute syscall
                        crate::posix::process::schedule();
                        unsafe { __halt_cpu() }
                    }
                }
                Err(_) => {
                    ep_arc.waiter.store(0, core::sync::atomic::Ordering::Relaxed);
                    tf.regs[10] = crate::errno::EINVAL;
                    tf.regs[11] = 0;
                }
            }
        }
        Err(_) => {
            tf.regs[10] = crate::errno::EINVAL;
            tf.regs[11] = 0;
        }
    }
}

fn copy_msg_to_user(
    msg: crate::ipc::channel::Message,
    buf_ptr: usize,
    buf_len: usize,
    handles_ptr: usize,
    handles_len: usize,
    proc: &alloc::sync::Arc<crate::posix::process::Process>,
    tf: &mut TrapFrame,
) {
    let copy_bytes = core::cmp::min(buf_len, msg.bytes.len());
    if copy_bytes > 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(msg.bytes.as_ptr(), buf_ptr as *mut u8, copy_bytes);
        }
    }

    let copy_handles = core::cmp::min(handles_len, msg.handles.len());
    let mut minted_handles = alloc::vec![0u32; copy_handles];
    if copy_handles > 0 {
        let mut fds = proc.fds.lock();
        for i in 0..copy_handles {
            let entry = msg.handles[i].clone();
            let new_id = fds.lowest_free();
            let new_h = crate::ipc::handle::encode_handle(new_id, entry.generation.wrapping_add(1));
            fds.insert_entry(new_h, entry);
            minted_handles[i] = new_h;
        }
    }

    if copy_handles > 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(minted_handles.as_ptr(), handles_ptr as *mut u32, copy_handles);
        }
    }

    tf.regs[10] = copy_bytes;
    tf.regs[11] = copy_handles;
}

pub fn sys_port_create(out_handle_ptr: usize, tf: &mut TrapFrame) {
    if !crate::mm::vmm::is_user_pointer_valid(tf, out_handle_ptr as *const u32, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    let port = alloc::sync::Arc::new(crate::ipc::port::Port::new());
    let proc = crate::get_current_proc_or_esrch!(tf);
    let mut fds = proc.fds.lock();
    let h = fds.insert(crate::ipc::handle::KernelObject::Port(port));
    
    unsafe {
        *(out_handle_ptr as *mut u32) = h;
    }
    tf.regs[10] = 0;
}

pub fn sys_port_wait(port_fd: usize, out_packet_ptr: usize, tf: &mut TrapFrame) {
    if !crate::mm::vmm::is_user_pointer_valid(tf, out_packet_ptr as *mut crate::ipc::port::PortPacket, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }
    let proc = crate::get_current_proc_or_esrch!(tf);
    
    let port = {
        let fds = proc.fds.lock();
        match fds.get_with_rights(port_fd as u32, crate::ipc::handle::Rights::READ) {
            Ok(crate::ipc::handle::KernelObject::Port(p)) => p.clone(),
            _ => {
                tf.regs[10] = crate::errno::EACCES;
                return;
            }
        }
    };

    if let Some(packet) = port.try_recv() {
        unsafe {
            *(out_packet_ptr as *mut crate::ipc::port::PortPacket) = packet;
        }
        tf.regs[10] = 0;
    } else {
        // Register as waiter
        port.waiter.store(
            crate::posix::process::current_pid(),
            core::sync::atomic::Ordering::Relaxed,
        );
        // Lost-wakeup check
        if let Some(packet) = port.try_recv() {
            port.waiter.store(0, core::sync::atomic::Ordering::Relaxed);
            unsafe {
                *(out_packet_ptr as *mut crate::ipc::port::PortPacket) = packet;
            }
            tf.regs[10] = 0;
        } else {
            // Block
            tf.sepc -= 4; // Re-execute syscall when woken
            crate::posix::process::schedule();
            unsafe { __halt_cpu() }
        }
    }
}

pub fn sys_port_queue(port_fd: usize, packet_ptr: usize, tf: &mut TrapFrame) {
    if !crate::mm::vmm::is_user_pointer_valid(tf, packet_ptr as *const crate::ipc::port::PortPacket, 1) {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    let mut packet: crate::ipc::port::PortPacket = unsafe { core::mem::zeroed() };
    unsafe {
        core::ptr::copy_nonoverlapping(
            packet_ptr as *const crate::ipc::port::PortPacket,
            &mut packet,
            1,
        );
    }
    let proc = crate::get_current_proc_or_esrch!(tf);
    let port = {
        let fds = proc.fds.lock();
        match fds.get_with_rights(port_fd as u32, crate::ipc::handle::Rights::WRITE) {
            Ok(crate::ipc::handle::KernelObject::Port(p)) => p.clone(),
            _ => {
                tf.regs[10] = crate::errno::EACCES;
                return;
            }
        }
    };

    port.queue_packet(packet);
    tf.regs[10] = 0;
}

pub fn sys_port_bind(
    object_fd: usize,
    port_fd: usize,
    key: u64,
    trigger_signals: u32,
    tf: &mut TrapFrame,
) {
    let proc = crate::get_current_proc_or_esrch!(tf);

    let fds = proc.fds.lock();
    
    let port = match fds.get_with_rights(port_fd as u32, crate::ipc::handle::Rights::WRITE) {
        Ok(crate::ipc::handle::KernelObject::Port(p)) => p.clone(),
        _ => {
            tf.regs[10] = crate::errno::EACCES;
            return;
        }
    };

    let channel = match fds.get_with_rights(object_fd as u32, crate::ipc::handle::Rights::READ) {
        Ok(crate::ipc::handle::KernelObject::Channel(ch)) => ch.clone(),
        _ => {
            tf.regs[10] = crate::errno::EACCES;
            return;
        }
    };

    // Bind observer
    let observer = crate::ipc::port::SignalObserver {
        port: port.clone(),
        key,
        trigger_signals,
    };
    channel.observers.lock().push(observer);

    // Immediate state evaluation
    let current_signals = channel.get_signals();
    let triggered = current_signals & trigger_signals;
    if triggered != 0 {
        port.queue_packet(crate::ipc::port::PortPacket {
            key,
            type_: 1,
            status: 0,
            observed_signals: current_signals,
        });
    }

    tf.regs[10] = 0;
}

pub fn sys_handle_duplicate(handle: usize, rights: usize, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    let mut fds = proc.fds.lock();
    
    let entry = match fds.get_entry(handle as u32) {
        Some(e) => e,
        None => {
            tf.regs[10] = crate::errno::EBADF;
            return;
        }
    };
    
    if !entry.rights.contains(crate::ipc::handle::Rights::DUPLICATE) {
        tf.regs[10] = crate::errno::EACCES;
        return;
    }
    
    let req_rights = crate::ipc::handle::Rights::from_bits_truncate(rights as u32);
    if !entry.rights.contains(req_rights) {
        tf.regs[10] = crate::errno::EACCES;
        return;
    }
    
    let obj_clone = entry.object.clone();
    let new_handle = fds.insert_with_rights(obj_clone, req_rights);
    tf.regs[10] = new_handle as usize;
}

pub fn sys_handle_get_rights(handle: usize, tf: &mut TrapFrame) {
    let proc = crate::get_current_proc_or_esrch!(tf);
    let fds = proc.fds.lock();

    match fds.get_entry(handle as u32) {
        Some(entry) => {
            tf.regs[10] = entry.rights.bits() as usize;
        }
        None => {
            tf.regs[10] = crate::errno::EBADF;
        }
    }
}

/// Waits for any of `signals_mask` to be asserted on a Channel handle.
///
/// Writes the observed signal bitmask to `observed_ptr` on success.
/// Blocks (retrying the syscall) if no signal is currently active.
///
/// # Arguments
/// * `arg0` — Channel handle
/// * `arg1` — Signal mask to wait for (bitfield of SIGNAL_* constants)
/// * `arg2` — Deadline (ignored; future extension)
/// * `arg3` — User-space `*mut u32` for the observed signals output
pub fn sys_object_wait_one(arg0: usize, arg1: usize, _arg2: usize, arg3: usize, tf: &mut TrapFrame) {
    let handle = arg0 as u32;
    let signals_mask = arg1 as u32;
    let observed_ptr = arg3;
    let proc = crate::get_current_proc_or_esrch!(tf);

    if observed_ptr != 0
        && !crate::mm::vmm::is_user_pointer_valid(tf, observed_ptr as *const u32, 1)
    {
        tf.regs[10] = crate::errno::EFAULT;
        return;
    }

    let channel = {
        let fds = proc.fds.lock();
        match fds.get(handle) {
            Some(crate::ipc::handle::KernelObject::Channel(ch)) => ch.clone(),
            _ => {
                tf.regs[10] = crate::errno::EBADF;
                return;
            }
        }
    };

    let current_signals = channel.get_signals();
    if current_signals & signals_mask != 0 {
        if observed_ptr != 0 {
            unsafe { *(observed_ptr as *mut u32) = current_signals; }
        }
        tf.regs[10] = 0;
        return;
    }

    // Register as direct waiter before re-checking to avoid lost wakeup.
    let pid = crate::posix::process::current_pid();
    channel.signal_waiters.lock().push_back((pid, signals_mask));

    let current_signals = channel.get_signals();
    if current_signals & signals_mask != 0 {
        let mut w = channel.signal_waiters.lock();
        if let Some(pos) = w.iter().position(|(p, _)| *p == pid) { w.remove(pos); }
        if observed_ptr != 0 {
            unsafe { *(observed_ptr as *mut u32) = current_signals; }
        }
        tf.regs[10] = 0;
        return;
    }

    tf.sepc -= 4;
    crate::posix::process::schedule();
    unsafe { __halt_cpu() }
}


use crate::ipc::Message;
use core::sync::atomic::{AtomicI32, Ordering};
use openv_syscall::{
    syscall0, syscall3,
    SYS_GET_PM_PID, SYS_MAILBOX_SEND, SYS_MAILBOX_RECV,
};
use pm_proto::*;

static PM_SERVER_PID: AtomicI32 = AtomicI32::new(0);

/// Returns the cached PM server PID, or queries the kernel for it.
pub fn pm_pid() -> Option<i32> {
    let cached = PM_SERVER_PID.load(Ordering::Relaxed);
    if cached > 0 {
        return Some(cached);
    }
    let ret = unsafe { syscall0(SYS_GET_PM_PID) };
    if ret != usize::MAX && ret > 0 {
        let pid = ret as i32;
        PM_SERVER_PID.store(pid, Ordering::Relaxed);
        Some(pid)
    } else {
        None
    }
}

fn pm_send(pm: i32, msg: &Message) {
    unsafe {
        syscall3(
            SYS_MAILBOX_SEND,
            pm as usize,
            msg as *const Message as usize,
            core::mem::size_of::<Message>(),
        )
    };
}

fn pm_recv() -> (i32, Message) {
    let mut msg = Message::new();
    let mut sender = 0i32;
    unsafe {
        syscall3(
            SYS_MAILBOX_RECV,
            &mut msg as *mut Message as usize,
            core::mem::size_of::<Message>(),
            &mut sender as *mut i32 as usize,
        )
    };
    // kernel writes sender to msg.source via from_ptr
    (sender, msg)
}

/// Fork the current process via pm-server IPC.
/// Returns the child PID (>0) in the parent, 0 in the child, or -1 on error.
pub fn fork_via_pm() -> i32 {
    let pm = match pm_pid() {
        Some(p) => p,
        None => return -1,
    };
    let mut req = Message::new();
    req.type_ = OP_FORK;
    pm_send(pm, &req);
    let (_sender, reply) = pm_recv();
    if reply.type_ == REPLY_OK {
        unpack_reply_u32(&reply.data) as i32
    } else {
        -1
    }
}

/// Send exit notification to pm-server and terminate.
pub fn exit_via_pm(status: i32) {
    if let Some(pm) = pm_pid() {
        let mut req = Message::new();
        req.type_ = OP_EXIT;
        pack_exit_req(&mut req.data, status);
        pm_send(pm, &req);
        // pm-server will reap us; we also call sys_exit to terminate
    }
}

/// Wait for a child process via pm-server IPC.
/// Returns (child_pid, exit_status) or (-1, -1) on error.
pub fn waitpid_via_pm(pid: i32, options: u32) -> (i32, i32) {
    let pm = match pm_pid() {
        Some(p) => p,
        None => return (-1, -1),
    };
    let mut req = Message::new();
    req.type_ = OP_WAITPID;
    pack_waitpid_req(&mut req.data, pid, options);
    pm_send(pm, &req);
    let (_sender, reply) = pm_recv();
    if reply.type_ == REPLY_OK {
        let child = unpack_reply_u32(&reply.data) as i32;
        let status = i32::from_le_bytes(reply.data[4..8].try_into().unwrap_or([0; 4]));
        (child, status)
    } else {
        (-1, -1)
    }
}

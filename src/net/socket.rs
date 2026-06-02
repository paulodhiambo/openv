#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use spin::Mutex;
use crate::ipc::channel::ChannelEndpoint;
use crate::posix::process::Pid;

// Simple socket registry for proxying sockets to a userspace network daemon.

static NEXT_SID: Mutex<u32> = Mutex::new(1);
static NEW_SOCKS: Mutex<Vec<(u32, Arc<ChannelEndpoint>)>> = Mutex::new(Vec::new());
// socket_id -> (owner_pid, owner_fd)
static SOCKET_OWNER: Mutex<BTreeMap<u32, (Pid, u32)>> = Mutex::new(BTreeMap::new());
// socket_id -> pids waiting in accept
static PENDING_ACCEPTS: Mutex<BTreeMap<u32, Vec<Pid>>> = Mutex::new(BTreeMap::new());
// socket_id -> created but not yet consumed user fds (u32)
static PENDING_ACCEPTED: Mutex<BTreeMap<u32, Vec<u32>>> = Mutex::new(BTreeMap::new());

pub fn alloc_socket_id() -> u32 {
    let mut guard = NEXT_SID.lock();
    let id = *guard;
    *guard += 1;
    id
}

pub fn push_new_socket(sid: u32, ep: Arc<ChannelEndpoint>) {
    NEW_SOCKS.lock().push((sid, ep));
}

pub fn pop_new_socket() -> Option<(u32, Arc<ChannelEndpoint>)> {
    let mut v = NEW_SOCKS.lock();
    if v.is_empty() { None } else { Some(v.remove(0)) }
}

pub fn register_socket_owner(sid: u32, owner_pid: Pid, owner_fd: u32) {
    SOCKET_OWNER.lock().insert(sid, (owner_pid, owner_fd));
}

pub fn owner_of(sid: u32) -> Option<(Pid, u32)> {
    SOCKET_OWNER.lock().get(&sid).cloned()
}

pub fn socket_id_for_fd(pid: Pid, fd: u32) -> Option<u32> {
    for (sid, (opid, ofd)) in SOCKET_OWNER.lock().iter() {
        if *opid == pid && *ofd == fd {
            return Some(*sid);
        }
    }
    None
}

pub fn add_pending_accept(sid: u32, pid: Pid) {
    let mut m = PENDING_ACCEPTS.lock();
    m.entry(sid).or_insert(Vec::new()).push(pid);
}

pub fn pop_waiting_pid(sid: u32) -> Option<Pid> {
    let mut m = PENDING_ACCEPTS.lock();
    if let Some(vec) = m.get_mut(&sid) {
        if vec.is_empty() { None } else { Some(vec.remove(0)) }
    } else { None }
}

pub fn push_pending_accepted(sid: u32, user_fd: u32) {
    let mut m = PENDING_ACCEPTED.lock();
    m.entry(sid).or_insert(Vec::new()).push(user_fd);
}

pub const OPCODE_ACK: u8 = 0;
pub const OPCODE_BIND: u8 = 1;
pub const OPCODE_LISTEN: u8 = 2;
pub const OPCODE_CONNECT: u8 = 3;
pub const OPCODE_SEND: u8 = 4;
pub const OPCODE_RECV: u8 = 5;

pub fn pop_pending_accepted(sid: u32) -> Option<u32> {
    let mut m = PENDING_ACCEPTED.lock();
    if let Some(vec) = m.get_mut(&sid) {
        if vec.is_empty() { None } else { Some(vec.remove(0)) }
    } else { None }
}

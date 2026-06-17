//! # Socket Registry
//!
//! This module provides a simple socket registry for proxying sockets
//! to a user-space network daemon. The kernel does not implement
//! full socket semantics; instead, it maintains a registry of sockets
//! and forwards socket operations to a user-space server.
//!
//! ## Overview
//!
//! The socket registry tracks:
//!
//! - **Socket IDs**: Unique identifiers for each socket.
//!  - **Socket owners**: The PID and file descriptor that own each socket.
//!  - **Pending accepts**: PIDs waiting in `accept` for incoming connections.
//!  - **Pending accepted**: Accepted connections waiting to be consumed.
//!  - **New sockets**: Sockets that have been created but not yet picked
//!    up by the user-space network daemon.
//!
//! ## Opcodes
//!
//! The following opcodes are used for socket operations:
//!
//! - [`OPCODE_ACK`]: Acknowledgment.
//!  - [`OPCODE_BIND`]: Bind a socket to an address.
//!  - [`OPCODE_LISTEN`]: Listen for incoming connections.
//!  - [`OPCODE_CONNECT`]: Connect to a remote address.
//!  - [`OPCODE_SEND`]: Send data.
//!  - [`OPCODE_RECV`]: Receive data.
//!
//! ## Thread Safety
//!
//! All registry data is protected by [`Mutex`]es, so the registry is
//! safe to use from any context.
//!
//! [`Mutex`]: ../../sync/struct.Mutex.html

use crate::ipc::channel::ChannelEndpoint;
use crate::ipc::handle::KernelObject;
use crate::posix::process::{Pid, PROCESS_TABLE};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::sync::Mutex;

// Simple socket registry for proxying sockets to a userspace network daemon.

/// Counter for generating unique socket IDs.
static NEXT_SID: Mutex<u32> = Mutex::new(1);
/// Queue of new sockets waiting to be picked up by the network daemon.
static NEW_SOCKS: Mutex<Vec<(u32, Arc<ChannelEndpoint>)>> = Mutex::new(Vec::new());
// socket_id -> (owner_pid, owner_fd)
/// Map from socket ID to its owner (PID, file descriptor).
static SOCKET_OWNER: Mutex<BTreeMap<u32, (Pid, u32)>> = Mutex::new(BTreeMap::new());
// socket_id -> pids waiting in accept
/// Map from socket ID to PIDs waiting in `accept`.
static PENDING_ACCEPTS: Mutex<BTreeMap<u32, Vec<Pid>>> = Mutex::new(BTreeMap::new());
// socket_id -> created but not yet consumed user fds (u32)
/// Map from socket ID to accepted user file descriptors.
static PENDING_ACCEPTED: Mutex<BTreeMap<u32, Vec<u32>>> = Mutex::new(BTreeMap::new());

/// Allocates a new unique socket ID.
///
/// # Returns
///
/// A unique `u32` socket ID. IDs are allocated sequentially starting from 1.
pub fn alloc_socket_id() -> u32 {
    let mut guard = NEXT_SID.lock();
    let id = *guard;
    *guard += 1;
    id
}

/// Pushes a new socket onto the new-sockets queue.
///
/// # Arguments
///
/// * `sid` - The socket ID.
/// * `ep` - The channel endpoint for the socket.
pub fn push_new_socket(sid: u32, ep: Arc<ChannelEndpoint>) {
    NEW_SOCKS.lock().push((sid, ep));
}

/// Pops a new socket from the new-sockets queue.
///
/// # Returns
///
/// `Some((sid, ep))` if a new socket is available, `None` otherwise.
pub fn pop_new_socket() -> Option<(u32, Arc<ChannelEndpoint>)> {
    let mut v = NEW_SOCKS.lock();
    if v.is_empty() {
        None
    } else {
        Some(v.remove(0))
    }
}

/// Registers the owner of a socket.
///
/// # Arguments
///
/// * `sid` - The socket ID.
/// * `owner_pid` - The PID of the owner process.
/// * `owner_fd` - The file descriptor in the owner process.
pub fn register_socket_owner(sid: u32, owner_pid: Pid, owner_fd: u32) {
    SOCKET_OWNER.lock().insert(sid, (owner_pid, owner_fd));
}

/// Returns the owner of a socket.
///
/// # Arguments
///
/// * `sid` - The socket ID.
///
/// # Returns
///
/// `Some((pid, fd))` if the socket has an owner, `None` otherwise.
pub fn owner_of(sid: u32) -> Option<(Pid, u32)> {
    SOCKET_OWNER.lock().get(&sid).cloned()
}

/// Returns the socket ID for a given (PID, file descriptor) pair.
///
/// # Arguments
///
/// * `pid` - The PID to search for.
/// * `fd` - The file descriptor to search for.
///
/// # Returns
///
/// `Some(sid)` if a matching socket is found, `None` otherwise.
pub fn socket_id_for_fd(pid: Pid, fd: u32) -> Option<u32> {
    for (sid, (opid, ofd)) in SOCKET_OWNER.lock().iter() {
        if *opid == pid && *ofd == fd {
            return Some(*sid);
        }
    }
    None
}

/// Adds a PID to the pending-accepts queue for a socket.
///
/// # Arguments
///
/// * `sid` - The socket ID.
/// * `pid` - The PID to add.
pub fn add_pending_accept(sid: u32, pid: Pid) {
    let mut m = PENDING_ACCEPTS.lock();
    m.entry(sid).or_default().push(pid);
}

/// Pops a waiting PID from the pending-accepts queue for a socket.
///
/// # Arguments
///
/// * `sid` - The socket ID.
///
/// # Returns
///
/// `Some(pid)` if a PID is waiting, `None` otherwise.
pub fn pop_waiting_pid(sid: u32) -> Option<Pid> {
    let mut m = PENDING_ACCEPTS.lock();
    if let Some(vec) = m.get_mut(&sid) {
        if vec.is_empty() {
            None
        } else {
            Some(vec.remove(0))
        }
    } else {
        None
    }
}

/// Pushes a pending-accepted file descriptor onto the queue for a socket.
///
/// # Arguments
///
/// * `sid` - The socket ID.
/// * `user_fd` - The user-space file descriptor for the accepted connection.
pub fn has_pending_accept(sid: u32) -> bool {
    let m = PENDING_ACCEPTED.lock();
    m.get(&sid).map_or(false, |v| !v.is_empty())
}

pub fn push_pending_accepted(sid: u32, user_fd: u32) {
    let mut m = PENDING_ACCEPTED.lock();
    m.entry(sid).or_default().push(user_fd);
    drop(m);
    // Wake epoll waiters on the listening socket.
    if let Some((owner_pid, owner_fd)) = owner_of(sid) {
        if let Some(proc) = PROCESS_TABLE.lock().get(&owner_pid).cloned() {
            let fds = proc.fds.lock();
            if let Some(KernelObject::Channel(ep)) = fds.get(owner_fd) {
                crate::ipc::handle::wake_epoll_waiters(&ep.epoll_waiters);
            }
        }
    }
}

/// Opcode: Acknowledgment.
pub const OPCODE_ACK: u8 = 0;
/// Opcode: Bind a socket to an address.
pub const OPCODE_BIND: u8 = 1;
/// Opcode: Listen for incoming connections.
pub const OPCODE_LISTEN: u8 = 2;
/// Opcode: Connect to a remote address.
pub const OPCODE_CONNECT: u8 = 3;
/// Opcode: Send data.
pub const OPCODE_SEND: u8 = 4;
/// Opcode: Receive data.
pub const OPCODE_RECV: u8 = 5;

/// Pops a pending-accepted file descriptor from the queue for a socket.
///
/// # Arguments
///
/// * `sid` - The socket ID.
///
/// # Returns
///
/// `Some(user_fd)` if a pending accepted FD is available, `None` otherwise.
pub fn pop_pending_accepted(sid: u32) -> Option<u32> {
    let mut m = PENDING_ACCEPTED.lock();
    if let Some(vec) = m.get_mut(&sid) {
        if vec.is_empty() {
            None
        } else {
            Some(vec.remove(0))
        }
    } else {
        None
    }
}

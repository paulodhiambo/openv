#![no_std]

use crate::syscall as syscall_fn;

/// Send a raw Ethernet frame via kernel net_send syscall (10).
/// Returns number of bytes sent or negative on error.
#[unsafe(no_mangle)]
pub extern "C" fn send_frame(buf: *const u8, len: usize) -> isize {
    // Safety: caller ensures buf is valid for `len` bytes.
    unsafe { syscall_fn(10, buf as usize, len, 0) as isize }
}

/// Receive a raw Ethernet frame via kernel net_recv syscall (11).
/// Fills `buf` up to `len` and returns bytes received, 0 if none, or -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn recv_frame(buf: *mut u8, len: usize) -> isize {
    unsafe { syscall_fn(11, buf as usize, len, 0) as isize }
}

/// Safe Rust wrappers for higher-level use in no_std code.
pub fn send_frame_rs(packet: &[u8]) -> isize {
    unsafe { syscall_fn(10, packet.as_ptr() as usize, packet.len(), 0) as isize }
}

pub fn recv_frame_rs(buf: &mut [u8]) -> isize {
    unsafe { syscall_fn(11, buf.as_mut_ptr() as usize, buf.len(), 0) as isize }
}

// Userland adapter: small shim that uses syscalls to send/receive raw packets

/// Send a raw packet via kernel-provided networking syscall (stubbed)
pub fn send_packet(packet: &[u8]) {
    // syscall number 10 reserved for net_send in this proto
    unsafe {
        super::syscall(10, packet.as_ptr() as usize, packet.len(), 0);
    }
}

/// Receive a raw packet into buffer, returns number of bytes received
pub fn recv_packet(buf: &mut [u8]) -> usize {
    // syscall number 11 reserved for net_recv
    unsafe { super::syscall(11, buf.as_mut_ptr() as usize, buf.len(), 0) }
}

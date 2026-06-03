#![no_std]
#![no_main]

mod device;
mod smoltcp_adapter;

extern crate alloc;
use device::SmolDevice;
use heapless::Vec as HVec;
use libos::{sys_yield, try_recv, write};

mod allocator;
use allocator::init_allocator;

// Opcode constants for kernel socket proxy protocol (kept for future TCP work)
const OPCODE_ACK: u8 = 0;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    unsafe { init_allocator(); }

    write(1, b"net: daemon starting\n".as_ptr(), 21);

    // Kernel socket proxy state (fd list for proxied sockets)
    let mut daemon_fds: HVec<i32, 16> = HVec::new();
    let mut msgbuf = [0u8; 256]; // control messages are small

    // Raw packet buffer — put in a Box so the 1536 B live on the heap, not the stack
    let mut buf = alloc::boxed::Box::new([0u8; 1536]);

    // Raw device for ARP / ICMP handling (calls syscall 11 for recv, 10 for send)
    let mut raw_dev = SmolDevice::new();

    write(1, b"net: ready (10.0.2.15/24)\n".as_ptr(), 26);

    loop {
        // ── 1. Accept new sockets from kernel (non-blocking) ─────────────────
        let next = libos::syscall(41, 0, 0, 0);
        if next != usize::MAX {
            let fd = (next & 0xffff_ffff) as i32;
            let _ = daemon_fds.push(fd);
        }

        // ── 2. Drain any pending socket control messages (non-blocking) ──────
        // syscall 49 (try_recv) returns 0 immediately when no message is pending.
        for &fd in daemon_fds.iter() {
            let r = try_recv(fd as usize, msgbuf.as_mut_ptr(), msgbuf.len());
            if r > 0 {
                // Acknowledge unhandled opcodes to unblock callers
                let ack = [OPCODE_ACK];
                let _ = libos::write(fd as usize, ack.as_ptr(), 1);
            }
        }

        // ── 3. Raw packet handling: ARP replies + ICMP echo replies ──────────
        if let Some(got) = raw_dev.recv(buf.as_mut()) {
            if got >= 14 {
                let ethertype = u16::from_be_bytes([buf[12], buf[13]]);
                match ethertype {
                    0x0806 if got >= 14 + 28 => handle_arp(buf.as_mut(), &mut raw_dev),
                    0x0800 if got >= 14 + 20 => handle_ipv4(buf.as_mut(), got, &mut raw_dev),
                    _ => {}
                }
            }
        }

        sys_yield();
    }
}

fn handle_arp(buf: &mut [u8], dev: &mut SmolDevice) {
    const OUR_IP:  [u8; 4] = [10, 0, 2, 15];
    const OUR_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let a = 14;
    let opcode = u16::from_be_bytes([buf[a + 6], buf[a + 7]]);
    let target_ip = [buf[a + 24], buf[a + 25], buf[a + 26], buf[a + 27]];
    if opcode != 1 || target_ip != OUR_IP {
        return;
    }
    let mut sender_mac = [0u8; 6];
    let mut sender_ip = [0u8; 4];
    sender_mac.copy_from_slice(&buf[a + 8..a + 14]);
    sender_ip.copy_from_slice(&buf[a + 14..a + 18]);
    buf[0..6].copy_from_slice(&sender_mac);
    buf[6..12].copy_from_slice(&OUR_MAC);
    buf[a + 6..a + 8].copy_from_slice(&2u16.to_be_bytes()); // ARP reply
    buf[a + 8..a + 14].copy_from_slice(&OUR_MAC);
    buf[a + 14..a + 18].copy_from_slice(&OUR_IP);
    buf[a + 18..a + 24].copy_from_slice(&sender_mac);
    buf[a + 24..a + 28].copy_from_slice(&sender_ip);
    dev.send(&buf[..14 + 28]);
}

fn handle_ipv4(buf: &mut [u8], got: usize, dev: &mut SmolDevice) {
    let ip = 14;
    let ihl = (buf[ip] & 0x0f) as usize * 4;
    if got < ip + ihl {
        return;
    }
    let proto = buf[ip + 9];
    let ip_total = u16::from_be_bytes([buf[ip + 2], buf[ip + 3]]) as usize;
    if proto != 1 {
        return; // only handle ICMP
    }
    let ic = ip + ihl;
    if got < ic + 8 || buf[ic] != 8 {
        return; // only echo request
    }
    const OUR_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    // Swap Ethernet addresses
    let mut old_src_mac = [0u8; 6];
    old_src_mac.copy_from_slice(&buf[6..12]);
    buf[0..6].copy_from_slice(&old_src_mac);
    buf[6..12].copy_from_slice(&OUR_MAC);
    // Swap IP addresses
    let mut src_ip = [0u8; 4];
    let mut dst_ip = [0u8; 4];
    src_ip.copy_from_slice(&buf[ip + 12..ip + 16]);
    dst_ip.copy_from_slice(&buf[ip + 16..ip + 20]);
    buf[ip + 12..ip + 16].copy_from_slice(&dst_ip);
    buf[ip + 16..ip + 20].copy_from_slice(&src_ip);
    // Reset TTL and recompute IP header checksum
    buf[ip + 8] = 64;
    buf[ip + 10] = 0;
    buf[ip + 11] = 0;
    let ip_ck = inet_checksum(&buf[ip..ip + ihl]);
    buf[ip + 10..ip + 12].copy_from_slice(&ip_ck.to_be_bytes());
    // ICMP echo reply (type=0) with recomputed checksum
    buf[ic] = 0;
    buf[ic + 2] = 0;
    buf[ic + 3] = 0;
    let icmp_len = ip_total.saturating_sub(ihl);
    if ic + icmp_len <= buf.len() {
        let icmp_ck = inet_checksum(&buf[ic..ic + icmp_len]);
        buf[ic + 2..ic + 4].copy_from_slice(&icmp_ck.to_be_bytes());
        dev.send(&buf[..14 + ip_total]);
    }
}

fn inet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum = sum.wrapping_add(u16::from_be_bytes([data[i], data[i + 1]]) as u32);
        i += 2;
    }
    if i < data.len() {
        sum = sum.wrapping_add((data[i] as u32) << 8);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

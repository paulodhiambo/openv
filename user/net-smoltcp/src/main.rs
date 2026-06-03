#![no_std]
#![no_main]

mod device;
mod smoltcp_adapter;

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use device::SmolDevice;
use heapless::Vec as HVec;
use libos::{sys_yield, try_recv, write};
use smoltcp_adapter::SmolPhyDevice;

mod allocator;
use allocator::init_allocator;

use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr};

// Opcodes for the kernel socket proxy protocol
const OPCODE_ACK: u8     = 0;
const OPCODE_BIND: u8    = 1;
const OPCODE_LISTEN: u8  = 2;
const OPCODE_CONNECT: u8 = 3;
const OPCODE_SEND: u8    = 4;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    unsafe { init_allocator(); }

    write(1, b"net: daemon starting\n".as_ptr(), 21);

    // smoltcp physical device (for TCP state machine)
    let mut smol_phy = SmolPhyDevice::new(SmolDevice::new(), 1500);
    let config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).into());
    let mut iface = Interface::new(config, &mut smol_phy, Instant::from_micros(0));
    iface.update_ip_addrs(|addrs| {
        addrs.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24)).unwrap();
    });
    let mut socket_storage: [SocketStorage; 8] = [SocketStorage::EMPTY; 8];
    let mut sockets = SocketSet::new(&mut socket_storage[..]);

    // kernel socket proxy state
    let mut daemon_fds: HVec<i32, 16> = HVec::new();
    let mut fd_to_sid: BTreeMap<i32, u32> = BTreeMap::new();
    let mut sid_to_handle: BTreeMap<u32, smoltcp::iface::SocketHandle> = BTreeMap::new();

    // Separate raw device for ARP/ICMP (independent of smoltcp's receive path)
    let mut raw_dev = SmolDevice::new();
    let mut buf = [0u8; 1536];
    let mut msgbuf = [0u8; 2048];

    write(1, b"net: ready (10.0.2.15/24)\n".as_ptr(), 26);

    loop {
        // ── 1. Accept new sockets from kernel ────────────────────────────────
        let next = libos::syscall(41, 0, 0, 0);
        if next != usize::MAX {
            let sid = (next >> 32) as u32;
            let fd = (next & 0xffff_ffff) as i32;
            let _ = daemon_fds.push(fd);
            fd_to_sid.insert(fd, sid);
        }

        // ── 2. Non-blocking read of socket control messages ──────────────────
        // syscall 49 (try_recv) returns 0 immediately if no message is pending,
        // so this never blocks and we always reach the raw packet loop below.
        for &fd in daemon_fds.iter() {
            let r = try_recv(fd as usize, msgbuf.as_mut_ptr(), msgbuf.len());
            if r <= 0 {
                continue;
            }
            let n = r as usize;
            let opcode = msgbuf[0];
            match opcode {
                OPCODE_BIND => {
                    let ack = [OPCODE_ACK];
                    let _ = libos::write(fd as usize, ack.as_ptr(), 1);
                }
                OPCODE_LISTEN => {
                    let port = if n >= 3 {
                        u16::from_be_bytes([msgbuf[1], msgbuf[2]])
                    } else {
                        1024
                    };
                    let rx: &'static mut [u8] = alloc::boxed::Box::leak(
                        alloc::boxed::Box::new([0u8; 2048])
                    );
                    let tx: &'static mut [u8] = alloc::boxed::Box::leak(
                        alloc::boxed::Box::new([0u8; 2048])
                    );
                    let tcp = TcpSocket::new(TcpSocketBuffer::new(rx), TcpSocketBuffer::new(tx));
                    let handle = sockets.add(tcp);
                    let sock = sockets.get_mut::<TcpSocket>(handle);
                    let _ = sock.listen(port);
                    if let Some(sid) = fd_to_sid.get(&fd) {
                        sid_to_handle.insert(*sid, handle);
                    }
                    let ack = [OPCODE_ACK];
                    let _ = libos::write(fd as usize, ack.as_ptr(), 1);
                }
                OPCODE_CONNECT => {
                    if n >= 7 {
                        let ip = IpAddress::v4(msgbuf[1], msgbuf[2], msgbuf[3], msgbuf[4]);
                        let port = u16::from_be_bytes([msgbuf[5], msgbuf[6]]);
                        let rx: &'static mut [u8] = alloc::boxed::Box::leak(
                            alloc::boxed::Box::new([0u8; 2048])
                        );
                        let tx: &'static mut [u8] = alloc::boxed::Box::leak(
                            alloc::boxed::Box::new([0u8; 2048])
                        );
                        let tcp = TcpSocket::new(TcpSocketBuffer::new(rx), TcpSocketBuffer::new(tx));
                        let handle = sockets.add(tcp);
                        let sock = sockets.get_mut::<TcpSocket>(handle);
                        let _ = sock.connect(iface.context(), (ip, port), 49500);
                        if let Some(sid) = fd_to_sid.get(&fd) {
                            sid_to_handle.insert(*sid, handle);
                        }
                    }
                    let ack = [OPCODE_ACK];
                    let _ = libos::write(fd as usize, ack.as_ptr(), 1);
                }
                OPCODE_SEND => {
                    if n > 1 {
                        if let Some(sid) = fd_to_sid.get(&fd) {
                            if let Some(handle) = sid_to_handle.get(sid) {
                                let sock = sockets.get_mut::<TcpSocket>(*handle);
                                let _ = sock.send_slice(&msgbuf[1..n]);
                            }
                        }
                    }
                    let ack = [OPCODE_ACK];
                    let _ = libos::write(fd as usize, ack.as_ptr(), 1);
                }
                _ => {}
            }
        }

        // ── 3. smoltcp TCP state machine poll ────────────────────────────────
        if !sockets.iter().next().is_none() {
            iface.poll(Instant::from_micros(0), &mut smol_phy, &mut sockets);
        }

        // ── 4. Raw packet handling: ARP replies + ICMP echo replies ──────────
        // Uses a second SmolDevice that calls syscall 11 independently.
        // ARP and ICMP requests not consumed by smoltcp end up here.
        if let Some(got) = raw_dev.recv(&mut buf) {
            if got < 14 { sys_yield(); continue; }
            let ethertype = u16::from_be_bytes([buf[12], buf[13]]);

            if ethertype == 0x0806 && got >= 14 + 28 {
                handle_arp(&mut buf, got, &mut raw_dev);
            } else if ethertype == 0x0800 && got >= 14 + 20 {
                handle_ipv4(&mut buf, got, &mut raw_dev);
            }
        }

        sys_yield();
    }
}

fn handle_arp(buf: &mut [u8], _got: usize, dev: &mut SmolDevice) {
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
    // Build ARP reply in-place
    buf[0..6].copy_from_slice(&sender_mac);
    buf[6..12].copy_from_slice(&OUR_MAC);
    // ethertype 0x0806 is already in buf[12..14]
    buf[a + 6..a + 8].copy_from_slice(&2u16.to_be_bytes()); // reply opcode
    buf[a + 8..a + 14].copy_from_slice(&OUR_MAC);
    buf[a + 14..a + 18].copy_from_slice(&OUR_IP);
    buf[a + 18..a + 24].copy_from_slice(&sender_mac);
    buf[a + 24..a + 28].copy_from_slice(&sender_ip);
    dev.send(&buf[..14 + 28]);
}

fn handle_ipv4(buf: &mut [u8], got: usize, dev: &mut SmolDevice) {
    let ip = 14;
    let ihl = (buf[ip] & 0x0f) as usize * 4;
    if got < ip + ihl { return; }
    let proto = buf[ip + 9];
    let ip_total = u16::from_be_bytes([buf[ip + 2], buf[ip + 3]]) as usize;
    if proto == 1 {
        // ICMP
        let ic = ip + ihl;
        if got < ic + 8 { return; }
        if buf[ic] != 8 { return; } // only echo request
        // Swap Ethernet addresses
        buf.copy_within(0..6, 6); // dst → src offset temporarily (6..12 = old src)
        // Actually: old dst was buf[0..6], old src was buf[6..12]
        // We want new dst = old src, new src = OUR_MAC
        const OUR_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
        let mut old_src = [0u8; 6];
        old_src.copy_from_slice(&buf[6..12]);
        buf[0..6].copy_from_slice(&old_src);
        buf[6..12].copy_from_slice(&OUR_MAC);
        // Swap IP src/dst
        let mut src_ip = [0u8; 4];
        let mut dst_ip = [0u8; 4];
        src_ip.copy_from_slice(&buf[ip + 12..ip + 16]);
        dst_ip.copy_from_slice(&buf[ip + 16..ip + 20]);
        buf[ip + 12..ip + 16].copy_from_slice(&dst_ip);
        buf[ip + 16..ip + 20].copy_from_slice(&src_ip);
        // Reset TTL and recompute IP checksum
        buf[ip + 8] = 64;
        buf[ip + 10] = 0; buf[ip + 11] = 0;
        let ip_ck = inet_checksum(&buf[ip..ip + ihl]);
        buf[ip + 10..ip + 12].copy_from_slice(&ip_ck.to_be_bytes());
        // Echo reply (type=0) and recompute ICMP checksum
        buf[ic] = 0;
        buf[ic + 2] = 0; buf[ic + 3] = 0;
        let icmp_len = ip_total.saturating_sub(ihl);
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

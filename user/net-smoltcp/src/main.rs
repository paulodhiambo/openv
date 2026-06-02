#![no_std]
#![no_main]

mod device;
mod smoltcp_adapter;

extern crate alloc;
use alloc::vec::Vec;
use alloc::vec;
use alloc::collections::BTreeMap;
use alloc::boxed::Box;
use libos::{write, sys_yield};
use libos::net_adapter;
use device::SmolDevice;
use smoltcp_adapter::SmolPhyDevice;
use heapless::Vec as HVec;

// allocator
mod allocator;
use allocator::init_allocator;

use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
use smoltcp::wire::{EthernetAddress, IpCidr, IpAddress, Ipv4Address};
use smoltcp::time::Instant;
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer};
use smoltcp::iface::SocketHandle;

// Local opcode defs matching kernel-side proxy protocol
const OPCODE_ACK: u8 = 0;
const OPCODE_BIND: u8 = 1;
const OPCODE_LISTEN: u8 = 2;
const OPCODE_CONNECT: u8 = 3;
const OPCODE_SEND: u8 = 4;
const OPCODE_RECV: u8 = 5;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Build smoltcp device backed by kernel packet IO
    let mut dev = SmolDevice::new();
    let phy = SmolPhyDevice::new(dev, 1500);

    // NOTE: We don't fully integrate the smoltcp interface here (complex),
    // but we provide the Device adapter for future socket work.

    let _ = write(1, b"net-smoltcp: adapter initialized\n".as_ptr(), 26);

    // Daemon runtime: accept kernel-proxied sockets and handle simple framed messages.
    use heapless::Vec as HVec;
    let mut daemon_fds: HVec<i32, 16> = HVec::new();
    let mut buf = [0u8; 1536];
    let mut raw_dev = SmolDevice::new();

    // Initialize allocator and smoltcp structures
    unsafe { init_allocator(); }

    // create smoltcp iface and sockets
    let mut phy_dev = SmolPhyDevice::new(SmolDevice::new(), 1500);
    let mut config = Config::new(EthernetAddress([0x02,0x00,0x00,0x00,0x00,0x01]).into());
    let mut iface = Interface::new(config, &mut phy_dev, Instant::from_micros(0));
    iface.update_ip_addrs(|addrs| { addrs.push(IpCidr::new(IpAddress::v4(10,0,2,15),24)).unwrap(); });
    let mut socket_storage: [SocketStorage; 8] = [SocketStorage::EMPTY; 8];
    let mut sockets = SocketSet::new(&mut socket_storage[..]);

    let mut fd_to_sid: BTreeMap<i32,u32> = BTreeMap::new();
    let mut sid_to_handle: BTreeMap<u32, SocketHandle> = BTreeMap::new();

    loop {
        // 1) Pull any newly created sockets from kernel (syscall 41). Non-blocking: returns usize::MAX if none.
        let next = libos::syscall(41, 0, 0, 0);
        if next != usize::MAX {
            let sid = (next >> 32) as u32;
            let fd = (next & 0xffff_ffff) as u32;
            let _ = write(1, b"net-smoltcp: daemon accepted new socket\n".as_ptr(), 29);
            daemon_fds.push(fd as i32);
            fd_to_sid.insert(fd as i32, sid);
        }

        // 2) Poll one daemon socket per loop iteration to avoid long blocking
        if !daemon_fds.is_empty() {
            let fd = daemon_fds[0];
            // Try to read a message (this will block if none)
            let mut msgbuf = [0u8; 2048];
            let r = libos::read(fd as usize, msgbuf.as_mut_ptr(), msgbuf.len());
            if r > 0 {
                let n = r as usize;
                let opcode = msgbuf[0];
                match opcode {
                    OPCODE_BIND => {
                        let _ = write(1, b"daemon: bind request\n".as_ptr(), 18);
                        // parse port (first two bytes big-endian)
                        if n >= 3 {
                            let port = u16::from_be_bytes([msgbuf[1], msgbuf[2]]);
                            // store nothing special for now; ACK
                            let ack = [OPCODE_ACK];
                            let _ = libos::write(fd as usize, ack.as_ptr(), ack.len());
                        } else {
                            let ack = [OPCODE_ACK];
                            let _ = libos::write(fd as usize, ack.as_ptr(), ack.len());
                        }
                    }
                    OPCODE_LISTEN => {
                        let _ = write(1, b"daemon: listen request\n".as_ptr(), 20);
                        // parse port from previous bind or payload (if provided)
                        let port = if n >= 3 { u16::from_be_bytes([msgbuf[1], msgbuf[2]]) } else { 1024 };
                        // create tcp socket and listen
                        let rx_box = Box::new([0u8; 2048]);
                        let tx_box = Box::new([0u8; 2048]);
                        let rx_slice: &'static mut [u8] = Box::leak(rx_box);
                        let tx_slice: &'static mut [u8] = Box::leak(tx_box);
                        let rx_buffer = TcpSocketBuffer::new(rx_slice);
                        let tx_buffer = TcpSocketBuffer::new(tx_slice);
                        let tcp = TcpSocket::new(rx_buffer, tx_buffer);
                        let handle = sockets.add(tcp);
                        // set to listen
                        let sock = sockets.get_mut::<TcpSocket>(handle);
                        let _ = sock.listen(port);
                        // map fd->sid and sid->handle: need sid from fd_to_sid mapping
                        if let Some(sid) = fd_to_sid.get(&fd) {
                            sid_to_handle.insert(*sid, handle);
                        }
                        let ack = [OPCODE_ACK];
                        let _ = libos::write(fd as usize, ack.as_ptr(), ack.len());
                    }
                    OPCODE_CONNECT => {
                        let _ = write(1, b"daemon: connect request\n".as_ptr(), 21);
                        if n >= 7 {
                            let ip = IpAddress::v4(msgbuf[1], msgbuf[2], msgbuf[3], msgbuf[4]);
                            let port = u16::from_be_bytes([msgbuf[5], msgbuf[6]]);
                            let rx_box = Box::new([0u8; 2048]);
                            let tx_box = Box::new([0u8; 2048]);
                            let rx_slice: &'static mut [u8] = Box::leak(rx_box);
                            let tx_slice: &'static mut [u8] = Box::leak(tx_box);
                            let rx_buffer = TcpSocketBuffer::new(rx_slice);
                            let tx_buffer = TcpSocketBuffer::new(tx_slice);
                            let tcp = TcpSocket::new(rx_buffer, tx_buffer);
                            let handle = sockets.add(tcp);
                            let sock = sockets.get_mut::<TcpSocket>(handle);
                            let _ = sock.connect(iface.context(), (ip, port), 49500);
                            if let Some(sid) = fd_to_sid.get(&fd) {
                                sid_to_handle.insert(*sid, handle);
                            }
                            let ack = [OPCODE_ACK];
                            let _ = libos::write(fd as usize, ack.as_ptr(), ack.len());
                        } else {
                            let ack = [OPCODE_ACK];
                            let _ = libos::write(fd as usize, ack.as_ptr(), ack.len());
                        }
                    }
                    OPCODE_SEND => {
                        // Forward payload into smoltcp socket
                        let _ = write(1, b"daemon: send payload\n".as_ptr(), 19);
                        if n > 1 {
                            let data = &msgbuf[1..n];
                            if let Some(sid) = fd_to_sid.get(&fd) {
                                if let Some(handle) = sid_to_handle.get(sid) {
                                    let sock = sockets.get_mut::<TcpSocket>(*handle);
                                    let _ = sock.send_slice(data);
                                    // acknowledge
                                    let _ = libos::write(fd as usize, [OPCODE_ACK].as_ptr(), 1);
                                }
                            }
                        } else {
                            let _ = write(1, b"daemon: empty send\n".as_ptr(), 17);
                        }
                    }
                    _ => {
                        let _ = write(1, b"daemon: unknown opcode\n".as_ptr(), 21);
                    }
                }
            }
        }

        // 3) Also poll raw device for ARP/ICMP handling as before
        if let Some(got) = raw_dev.recv(&mut buf) {
            if got > 14 {
                // same packet handling as before
                let ethertype = u16::from_be_bytes([buf[12], buf[13]]);
                if ethertype == 0x0806 && got >= 14 + 28 {
                    let arp_off = 14;
                    let opcode = u16::from_be_bytes([buf[arp_off+6], buf[arp_off+7]]);
                    let mut sender_mac = [0u8;6];
                    let mut sender_ip = [0u8;4];
                    let mut target_ip = [0u8;4];
                    sender_mac.copy_from_slice(&buf[arp_off+8..arp_off+14]);
                    sender_ip.copy_from_slice(&buf[arp_off+14..arp_off+18]);
                    target_ip.copy_from_slice(&buf[arp_off+24..arp_off+28]);
                    let our_ip = [10u8,0,2,15];
                    if opcode == 1 && target_ip == our_ip {
                        let our_mac = [0x02u8,0x00,0x00,0x00,0x00,0x01];
                        buf[0..6].copy_from_slice(&sender_mac);
                        buf[6..12].copy_from_slice(&our_mac);
                        buf[arp_off+6..arp_off+8].copy_from_slice(&2u16.to_be_bytes());
                        buf[arp_off+8..arp_off+14].copy_from_slice(&our_mac);
                        buf[arp_off+14..arp_off+18].copy_from_slice(&target_ip);
                        buf[arp_off+18..arp_off+24].copy_from_slice(&sender_mac);
                        buf[arp_off+24..arp_off+28].copy_from_slice(&sender_ip);
                        raw_dev.send(&buf[..(14+28)]);
                        let _ = write(1, b"arp: replied\n".as_ptr(), 11);
                        continue;
                    }
                }

                if ethertype == 0x0800 {
                    if got < 14 + 20 { continue; }
                    let ip_off = 14;
                    let ihl = (buf[ip_off] & 0x0f) as usize * 4;
                    if got < 14 + ihl { continue; }
                    let proto = buf[ip_off + 9];
                    if proto == 1 {
                        let ip_total_len = u16::from_be_bytes([buf[ip_off+2], buf[ip_off+3]]) as usize;
                        let icmp_off = ip_off + ihl;
                        if got < icmp_off + 8 { continue; }
                        let icmp_type = buf[icmp_off];
                        if icmp_type == 8 {
                            let mut dst_mac = [0u8;6];
                            let mut src_mac = [0u8;6];
                            dst_mac.copy_from_slice(&buf[0..6]);
                            src_mac.copy_from_slice(&buf[6..12]);
                            buf[0..6].copy_from_slice(&src_mac);
                            buf[6..12].copy_from_slice(&dst_mac);
                            let mut src_ip = [0u8;4];
                            let mut dst_ip = [0u8;4];
                            src_ip.copy_from_slice(&buf[ip_off+12..ip_off+16]);
                            dst_ip.copy_from_slice(&buf[ip_off+16..ip_off+20]);
                            buf[ip_off+12..ip_off+16].copy_from_slice(&dst_ip);
                            buf[ip_off+16..ip_off+20].copy_from_slice(&src_ip);
                            buf[ip_off + 8] = 64;
                            buf[ip_off + 10] = 0;
                            buf[ip_off + 11] = 0;
                            let ip_ck = inet_checksum(&buf[ip_off..ip_off+ihl]);
                            buf[ip_off + 10..ip_off + 12].copy_from_slice(&ip_ck.to_be_bytes());
                            buf[icmp_off] = 0;
                            buf[icmp_off+2] = 0;
                            buf[icmp_off+3] = 0;
                            let icmp_len = ip_total_len - ihl;
                            let icmp_ck = inet_checksum(&buf[icmp_off..icmp_off+icmp_len]);
                            buf[icmp_off+2..icmp_off+4].copy_from_slice(&icmp_ck.to_be_bytes());
                            let send_len = 14 + ip_total_len;
                            raw_dev.send(&buf[..send_len]);
                            let _ = write(1, b"icmp: replied\n".as_ptr(), 13);
                        }
                    }
                }
            }
        }
        sys_yield();
    }
}

fn inet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        let v = u16::from_be_bytes([data[i], data[i+1]]) as u32;
        sum = sum.wrapping_add(v);
        i += 2;
    }
    if i < data.len() {
        let v = (data[i] as u32) << 8;
        sum = sum.wrapping_add(v);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#![no_std]
#![no_main]
#![allow(clippy::needless_range_loop)]

mod device;
mod smoltcp_adapter;

extern crate alloc;
use alloc::vec::Vec;
use device::SmolDevice;
use smoltcp_adapter::SmolPhyDevice;
use libos::{gettimeofday, sys_yield, try_recv, TimeVal, ipc_send, ipc_recv, getppid, CAP_NET_RAW};
use driver_abi::{DriverDesc, OP_DRIVER_REGISTER, REPLY_DRIVER_OK};

mod allocator;
use allocator::init_allocator;

fn now_ms() -> i64 {
    let mut tv = TimeVal { tv_sec: 0, tv_usec: 0 };
    gettimeofday(&mut tv as *mut TimeVal, core::ptr::null_mut());
    tv.tv_sec * 1000 + tv.tv_usec / 1000
}

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet, SocketStorage};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address};

const OUR_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const GW_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);

const OPCODE_BIND: u8 = 1;
const OPCODE_LISTEN: u8 = 2;
const OPCODE_CONNECT: u8 = 3;
const OPCODE_SEND: u8 = 4;

struct ProxyEntry {
    daemon_fd: i32,
    sid: u32,
    bound_port: u16,
    mode: ProxyMode,
    tx_queue: Vec<u8>,
}

enum ProxyMode {
    New,
    Listening { handle: SocketHandle },
    Connection { handle: SocketHandle },
}

fn register_with_rs() {
    let rs_pid = getppid();
    let desc = DriverDesc {
        name: alloc::string::String::from("net-smoltcp"),
        version: (0, 1, 0),
        required_caps: CAP_NET_RAW,
        resources: alloc::vec![],
    };
    let mut payload = OP_DRIVER_REGISTER.to_le_bytes().to_vec();
    payload.extend_from_slice(&desc.serialize());
    ipc_send(rs_pid, &payload);
    let mut reply = [0u8; 4];
    let mut _from: i32 = 0;
    let n = ipc_recv(&mut reply, &mut _from);
    if n < 4 || i32::from_le_bytes(reply) != REPLY_DRIVER_OK {
        libos::write(1, b"net: rs-server rejected registration\n".as_ptr(), 37);
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    unsafe { init_allocator(); }
    libos::write(1, b"net: daemon starting\n".as_ptr(), 21);
    register_with_rs();

    let mut phy = SmolPhyDevice::new(SmolDevice::new(), 1500);

    let config = Config::new(EthernetAddress(OUR_MAC).into());
    let mut iface = Interface::new(config, &mut phy, Instant::from_millis(now_ms()));
    iface.update_ip_addrs(|addrs| {
        addrs.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24)).ok();
    });
    iface.routes_mut().add_default_ipv4_route(GW_IP).ok();

    let mut sockets: SocketSet<'static> = SocketSet::new(Vec::<SocketStorage<'static>>::new());
    let mut proxies: Vec<ProxyEntry> = Vec::new();

    // Pre-allocate shared message buffers on the heap to avoid large stack frames
    let mut ctrl_buf: Vec<u8> = alloc::vec![0u8; 1500];
    let mut recv_buf: Vec<u8> = alloc::vec![0u8; 4096];

    libos::write(1, b"net: ready (10.0.2.15/24)\n".as_ptr(), 26);

    loop {
        // Drain all new sockets queued by the kernel
        loop {
            let next = libos::syscall(41, 0, 0, 0);
            if next == usize::MAX {
                break;
            }
            let sid = (next >> 32) as u32;
            let fd = (next & 0xffff_ffff) as i32;
            proxies.push(ProxyEntry {
                daemon_fd: fd,
                sid,
                bound_port: 0,
                mode: ProxyMode::New,
                tx_queue: Vec::new(),
            });
        }

        // Process control / data messages for each known socket
        let n = proxies.len();
        for i in 0..n {
            let r = try_recv(
                proxies[i].daemon_fd as usize,
                ctrl_buf.as_mut_ptr(),
                ctrl_buf.len(),
            );
            if r <= 0 {
                continue;
            }
            let r = r as usize;
            match ctrl_buf[0] {
                OPCODE_BIND => {
                    // Message: [OPCODE_BIND][sockaddr_in: 2 family | 2 port BE | 4 addr | 8 pad]
                    if r >= 5 {
                        let port = u16::from_be_bytes([ctrl_buf[3], ctrl_buf[4]]);
                        proxies[i].bound_port = port;
                    }
                }
                OPCODE_LISTEN => {
                    let port = proxies[i].bound_port;
                    if port > 0 {
                        let handle = make_listen_socket(&mut sockets, port);
                        proxies[i].mode = ProxyMode::Listening { handle };
                    }
                }
                OPCODE_CONNECT => {
                    if r >= 9 {
                        let port = u16::from_be_bytes([ctrl_buf[3], ctrl_buf[4]]);
                        let ip = smoltcp::wire::IpAddress::Ipv4(smoltcp::wire::Ipv4Address::new(
                            ctrl_buf[5], ctrl_buf[6], ctrl_buf[7], ctrl_buf[8]
                        ));
                        static mut EPHEMERAL_PORT: u16 = 49152;
                        let local_port = unsafe {
                            let p = EPHEMERAL_PORT;
                            EPHEMERAL_PORT += 1;
                            if EPHEMERAL_PORT < 49152 { EPHEMERAL_PORT = 49152; }
                            p
                        };
                        let rx = tcp::SocketBuffer::new(alloc::vec![0u8; 32768]);
                        let tx = tcp::SocketBuffer::new(alloc::vec![0u8; 32768]);
                        let mut sock = tcp::Socket::new(rx, tx);
                        sock.connect(iface.context(), (ip, port), local_port).ok();
                        let handle = sockets.add(sock);
                        proxies[i].mode = ProxyMode::Connection { handle };
                    }
                }
                OPCODE_SEND => {
                    if let ProxyMode::Connection { .. } = proxies[i].mode {
                        if r > 1 {
                            proxies[i].tx_queue.extend_from_slice(&ctrl_buf[1..r]);
                        }
                    }
                }
                _ => {}
            }
        }

        // Poll smoltcp: handles ARP replies, ICMP echo replies, TCP handshakes, TX/RX
        iface.poll(Instant::from_millis(now_ms()), &mut phy, &mut sockets);

        // Process TX queues for established connections
        for i in 0..proxies.len() {
            if let ProxyMode::Connection { handle } = proxies[i].mode {
                if !proxies[i].tx_queue.is_empty() {
                    let sock = sockets.get_mut::<tcp::Socket>(handle);
                    if sock.can_send() {
                        let to_send = &proxies[i].tx_queue;
                        if let Ok(sent) = sock.send_slice(to_send) {
                            proxies[i].tx_queue.drain(..sent);
                        }
                    }
                }
            }
        }

        // Detect listening sockets that now have an established connection
        let n = proxies.len();
        let mut new_conns: Vec<ProxyEntry> = Vec::new();
        for i in 0..n {
            if let ProxyMode::Listening { handle } = proxies[i].mode {
                let is_active = {
                    let sock = sockets.get_mut::<tcp::Socket>(handle);
                    sock.is_active()
                };
                if is_active {
                    // Notify kernel: create the accepted fd for the user process
                    let sid = proxies[i].sid;
                    let conn_fd = libos::syscall(42, sid as usize, 0, 0);
                    if conn_fd != usize::MAX {
                        new_conns.push(ProxyEntry {
                            daemon_fd: conn_fd as i32,
                            sid: 0,
                            bound_port: 0,
                            mode: ProxyMode::Connection { handle },
                            tx_queue: Vec::new(),
                        });
                    }
                    // Immediately replace with a new listening socket for the next connection
                    let port = proxies[i].bound_port;
                    let new_handle = make_listen_socket(&mut sockets, port);
                    proxies[i].mode = ProxyMode::Listening { handle: new_handle };
                }
            }
        }
        for entry in new_conns {
            proxies.push(entry);
        }

        // Relay incoming TCP data from connected sockets to their user processes
        for i in 0..proxies.len() {
            if let ProxyMode::Connection { handle } = proxies[i].mode {
                let sock = sockets.get_mut::<tcp::Socket>(handle);
                if sock.can_recv() {
                    if let Ok(n) = sock.recv_slice(&mut recv_buf) {
                        if n > 0 {
                            let fd = proxies[i].daemon_fd as usize;
                            libos::write(fd, recv_buf.as_ptr(), n);
                        }
                    }
                }
            }
        }

        sys_yield();
    }
}

fn make_listen_socket(sockets: &mut SocketSet<'static>, port: u16) -> SocketHandle {
    let rx = tcp::SocketBuffer::new(alloc::vec![0u8; 4096]);
    let tx = tcp::SocketBuffer::new(alloc::vec![0u8; 4096]);
    let mut sock = tcp::Socket::new(rx, tx);
    sock.listen(port).ok();
    sockets.add(sock)
}

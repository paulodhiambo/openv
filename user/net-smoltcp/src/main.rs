#![no_std]
#![no_main]

mod device;
mod smoltcp_adapter;

extern crate alloc;
use alloc::vec::Vec;
use device::SmolDevice;
use smoltcp_adapter::SmolPhyDevice;
use libos::{sys_yield, try_recv};

mod allocator;
use allocator::init_allocator;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet, SocketStorage};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address};

const OUR_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const GW_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);

const OPCODE_BIND: u8 = 1;
const OPCODE_LISTEN: u8 = 2;
const OPCODE_SEND: u8 = 4;

struct ProxyEntry {
    daemon_fd: i32,
    sid: u32,
    bound_port: u16,
    mode: ProxyMode,
}

enum ProxyMode {
    New,
    Listening { handle: SocketHandle },
    Connection { handle: SocketHandle },
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    unsafe { init_allocator(); }
    libos::write(1, b"net: daemon starting\n".as_ptr(), 21);

    let mut phy = SmolPhyDevice::new(SmolDevice::new(), 1500);

    let config = Config::new(EthernetAddress(OUR_MAC).into());
    let mut iface = Interface::new(config, &mut phy, Instant::ZERO);
    iface.update_ip_addrs(|addrs| {
        addrs.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24)).ok();
    });
    iface.routes_mut().add_default_ipv4_route(GW_IP).ok();

    let mut sockets: SocketSet<'static> = SocketSet::new(Vec::<SocketStorage<'static>>::new());
    let mut proxies: Vec<ProxyEntry> = Vec::new();

    // Pre-allocate shared message buffers on the heap to avoid large stack frames
    let mut ctrl_buf: Vec<u8> = alloc::vec![0u8; 1500];
    let mut recv_buf: Vec<u8> = alloc::vec![0u8; 1500];

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
                OPCODE_SEND => {
                    if let ProxyMode::Connection { handle } = proxies[i].mode {
                        let sock = sockets.get_mut::<tcp::Socket>(handle);
                        if sock.can_send() && r > 1 {
                            sock.send_slice(&ctrl_buf[1..r]).ok();
                        }
                    }
                }
                _ => {}
            }
        }

        // Poll smoltcp: handles ARP replies, ICMP echo replies, TCP handshakes, TX/RX
        iface.poll(Instant::ZERO, &mut phy, &mut sockets);

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

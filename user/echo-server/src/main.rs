//! TCP echo server on port 5555.
//!
//! Tests the full network stack end-to-end:
//!   kernel socket API → net-smoltcp daemon → virtio-net → QEMU user-mode net
//!
//! Test from the host:
//!   nc 10.0.2.15 5555
//!   (type anything; it is echoed back)
#![no_std]
#![no_main]

use libos::{accept, bind, listen, socket, send, sys_yield, try_recv, write};

fn wrt(s: &[u8]) {
    write(1, s.as_ptr(), s.len());
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    wrt(b"echo-server: starting on port 5555\n");

    // AF_INET=2, SOCK_STREAM=1
    let fd = socket(2, 1, 0);
    if fd < 0 {
        wrt(b"echo-server: socket() failed\n");
        return 1;
    }

    // sockaddr_in: family (LE u16) | port (BE u16) | addr (4 bytes) | pad (8 bytes)
    const PORT: u16 = 5555;
    let addr: [u8; 16] = [
        2, 0,                                      // AF_INET (LE)
        (PORT >> 8) as u8, (PORT & 0xff) as u8,   // port (BE)
        0, 0, 0, 0,                                // INADDR_ANY
        0, 0, 0, 0, 0, 0, 0, 0,                   // padding
    ];

    if bind(fd, addr.as_ptr(), addr.len()) != 0 {
        wrt(b"echo-server: bind() failed\n");
        return 1;
    }
    if listen(fd, 4) != 0 {
        wrt(b"echo-server: listen() failed\n");
        return 1;
    }

    wrt(b"echo-server: listening - connect with: nc 10.0.2.15 5555\n");

    loop {
        let conn = accept(fd);
        if conn < 0 {
            sys_yield();
            continue;
        }
        wrt(b"echo-server: client connected\n");
        echo_conn(conn as usize);
        wrt(b"echo-server: client disconnected\n");
    }
}

fn echo_conn(fd: usize) {
    let mut buf = [0u8; 512];
    let mut idle_ticks = 0u32;
    loop {
        let n = try_recv(fd, buf.as_mut_ptr(), buf.len());
        if n > 0 {
            idle_ticks = 0;
            send(fd as i32, buf.as_ptr(), n as usize, 0);
        } else {
            idle_ticks += 1;
            // After ~10 000 empty polls, treat as disconnected.
            if idle_ticks > 10_000 {
                break;
            }
            sys_yield();
        }
    }
}

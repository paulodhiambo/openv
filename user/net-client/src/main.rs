#![no_std]
#![no_main]

use libos::{socket, connect, send, recv, write};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let msg1 = b"net-client: starting...\n";
    write(1, msg1.as_ptr(), msg1.len());
    
    let mut addr = [0u8; 16];
    addr[0] = 2; // AF_INET
    addr[1] = 0;
    addr[2] = (8000u16 >> 8) as u8;
    addr[3] = (8000u16 & 0xFF) as u8;
    addr[4] = 10;
    addr[5] = 0;
    addr[6] = 2;
    addr[7] = 2;

    let fd = socket(2, 1, 0); 
    if fd < 0 {
        let m = b"net-client: socket() failed\n";
        write(1, m.as_ptr(), m.len());
        return -1;
    }

    let msg2 = b"net-client: connecting to 10.0.2.2:8000...\n";
    write(1, msg2.as_ptr(), msg2.len());
    
    let res = connect(fd, addr.as_ptr(), 16);
    if res < 0 {
        let m = b"net-client: connect() failed\n";
        write(1, m.as_ptr(), m.len());
        return -1;
    }

    let msg3 = b"net-client: sending data...\n";
    write(1, msg3.as_ptr(), msg3.len());
    
    let payload = b"GET / HTTP/1.0\r\nHost: 10.0.2.2\r\n\r\n";
    let sent = send(fd, payload.as_ptr(), payload.len(), 0);
    if sent < 0 {
        let m = b"net-client: send() failed\n";
        write(1, m.as_ptr(), m.len());
        return -1;
    }

    let msg4 = b"net-client: receiving data...\n";
    write(1, msg4.as_ptr(), msg4.len());
    
    let mut buf = [0u8; 128];
    loop {
        let n = recv(fd, buf.as_mut_ptr(), buf.len(), 0);
        if n > 0 {
            write(1, buf.as_ptr(), n as usize);
        } else if n == 0 {
            break;
        } else {
            break;
        }
    }

    let msg5 = b"net-client: done\n";
    write(1, msg5.as_ptr(), msg5.len());
    0
}

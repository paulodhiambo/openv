#![no_std]
#![no_main]

use libos::{write, exit, getdents};

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    let path = "/";
    let mut buf = [0u8; 1024];
    
    let n = getdents(path.as_ptr(), path.len(), buf.as_mut_ptr(), buf.len());
    if n < 0 {
        let msg = b"ls: failed to read directory\n";
        write(1, msg.as_ptr(), msg.len());
        return 1;
    }
    
    let mut pos = 0;
    while pos < n as usize {
        let start = pos;
        while pos < n as usize && buf[pos] != 0 {
            pos += 1;
        }
        
        if pos > start {
            write(1, &buf[start], pos - start);
            write(1, b"\n".as_ptr(), 1);
        }
        pos += 1; // Skip null
    }
    
    0
}

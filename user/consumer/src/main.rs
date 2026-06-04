#![no_std]
#![no_main]

use libos::{read, write};

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    let handle = 4; // fd 4 from pipe
    let mut buf = [0u8; 128];

    let info = b"Consumer: Waiting for messages...\n";
    write(1, info.as_ptr(), info.len());

    for _ in 0..2 {
        let bytes_read = read(handle, buf.as_mut_ptr(), buf.len());
        if bytes_read > 0 {
            write(1, buf.as_ptr(), bytes_read as usize);
        } else {
            let err = b"Consumer: Read error!\n";
            write(1, err.as_ptr(), err.len());
        }
    }

    let info2 = b"Consumer: Done.\n";
    write(1, info2.as_ptr(), info2.len());

    0
}

#![no_std]
#![no_main]

use libos::{close, open, read, sys_yield, write};

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    let info = b"Producer: Opening /dummy.txt...\n";
    write(1, info.as_ptr(), info.len());

    let path = "/dummy.txt";
    let fd = open(path.as_ptr(), path.len(), 0);

    if fd >= 0 {
        let msg = b"Producer: Successfully opened file. Reading...\n";
        write(1, msg.as_ptr(), msg.len());

        let mut buf = [0u8; 128];
        let bytes_read = read(fd as usize, buf.as_mut_ptr(), buf.len());

        if bytes_read > 0 {
            write(1, buf.as_ptr(), bytes_read as usize);
        } else {
            let err = b"Producer: Read returned 0 or error\n";
            write(1, err.as_ptr(), err.len());
        }

        close(fd);
    } else {
        let err = b"Producer: Failed to open file!\n";
        write(1, err.as_ptr(), err.len());
    }

    // Test the pipe too
    let msg1 = b"Producer: Hello Consumer!\n";
    let msg2 = b"Producer: How are you?\n";
    let handle = 3; // fd 3

    let info = b"Producer: Sending messages...\n";
    write(1, info.as_ptr(), info.len());

    write(handle, msg1.as_ptr(), msg1.len());
    sys_yield();
    write(handle, msg2.as_ptr(), msg2.len());

    let info2 = b"Producer: Done.\n";
    write(1, info2.as_ptr(), info2.len());

    0
}

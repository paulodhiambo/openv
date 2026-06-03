#![no_std]
#![no_main]

use libos::{close, exit, open, read, write};

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    // For now, we don't have argv parsing, so let's just cat a hardcoded file
    // Or we could implement a very simple argv strategy if we updated spawn/posix_spawn
    // Since we don't have argv yet, let's just cat /dummy.txt as a test.
    let path = "/dummy.txt";
    let fd = open(path.as_ptr(), path.len(), 0);
    if fd < 0 {
        let msg = b"cat: failed to open file\n";
        write(1, msg.as_ptr(), msg.len());
        return 1;
    }

    let mut buf = [0u8; 1024];
    loop {
        let n = read(fd as usize, buf.as_mut_ptr(), buf.len());
        if n <= 0 {
            break;
        }
        write(1, buf.as_ptr(), n as usize);
    }

    close(fd);
    0
}

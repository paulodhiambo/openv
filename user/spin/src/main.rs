#![no_std]
#![no_main]

use libos::write;

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    write(1, b"spin: running (Ctrl+C to stop)\n".as_ptr(), 31);
    let mut x: u64 = 0;
    loop {
        // busy spin — no syscalls, exercises timer-based SIGINT
        x = x.wrapping_add(1);
        if x == 0 {
            write(1, b"spin: still running\n".as_ptr(), 20);
        }
    }
}

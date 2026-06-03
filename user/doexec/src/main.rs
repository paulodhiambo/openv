#![no_std]
#![no_main]

use libos::{sys_yield, syscall, write};

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    let msg = b"doexec: about to exec /hello\n";
    write(1, msg.as_ptr(), msg.len());

    let path = b"/hello";
    // syscall 51 = exec(path_ptr, len)
    let ret = syscall(51, path.as_ptr() as usize, path.len(), 0);
    if ret != 0 {
        let err = b"doexec: exec failed\n";
        write(1, err.as_ptr(), err.len());
    }

    // If exec succeeded, this process image is replaced and we never reach here.
    loop {
        sys_yield();
    }
}

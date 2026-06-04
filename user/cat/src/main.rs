#![no_std]
#![no_main]

use libos::{close, open, read, write};

#[unsafe(no_mangle)]
pub extern "C" fn main(argc: i32, argv: *const *const u8) -> i32 {
    if argc <= 1 {
        // No arguments: copy stdin to stdout.
        return copy_fd(0);
    }
    let mut status = 0i32;
    for i in 1..argc as usize {
        let path = unsafe { cstr_slice(*argv.add(i)) };
        let fd = open(path.as_ptr(), path.len(), 0);
        if fd < 0 {
            write(2, b"cat: ".as_ptr(), 5);
            write(2, path.as_ptr(), path.len());
            write(2, b": No such file or directory\n".as_ptr(), 28);
            status = 1;
            continue;
        }
        if copy_fd(fd as usize) != 0 {
            status = 1;
        }
        close(fd);
    }
    status
}

fn copy_fd(fd: usize) -> i32 {
    let mut buf = [0u8; 1024];
    loop {
        let n = read(fd, buf.as_mut_ptr(), buf.len());
        if n <= 0 {
            break;
        }
        write(1, buf.as_ptr(), n as usize);
    }
    0
}

/// Return a byte slice from a null-terminated C string pointer.
unsafe fn cstr_slice(ptr: *const u8) -> &'static [u8] {
    let mut len = 0;
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
        }
        core::slice::from_raw_parts(ptr, len)
    }
}

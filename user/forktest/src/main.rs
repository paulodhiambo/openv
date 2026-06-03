#![no_std]
#![no_main]

use libos::{write, fork, waitpid, exit};

fn w(s: &[u8]) { write(1, s.as_ptr(), s.len()); }

fn wnum(n: i32) {
    if n == 0 { w(b"0"); return; }
    let mut buf = [0u8; 16];
    let mut pos = 15;
    let mut nn = if n < 0 { -n } else { n };
    while nn > 0 { buf[pos] = b'0' + (nn % 10) as u8; nn /= 10; pos -= 1; }
    if n < 0 { buf[pos] = b'-'; pos -= 1; }
    w(&buf[pos + 1..16]);
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    w(b"\n[FT:start]\n");

    let pid = fork();
    if pid < 0 {
        w(b"[FT:fork_FAILED]\n");
        return 1;
    }

    if pid == 0 {
        w(b"[FT:child_ok]\n");
        exit(42);
    }

    w(b"[FT:parent_child="); wnum(pid); w(b"]\n");
    w(b"[FT:parent_waiting]\n");

    let mut status: i32 = 0;
    let reaped = waitpid(pid, &mut status as *mut i32);

    w(b"[FT:parent_reaped="); wnum(reaped); w(b"]\n");
    w(b"[FT:parent_status="); wnum(status); w(b"]\n");
    w(b"[FT:PASSED]\n");
    0
}

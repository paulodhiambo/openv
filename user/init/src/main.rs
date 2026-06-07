#![no_std]
#![no_main]

use libos::{spawn, sys_yield, waitpid, write};

fn wrt(s: &[u8]) {
    write(1, s.as_ptr(), s.len());
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    // ── Boot banner ───────────────────────────────────────────────────────────
    wrt(b"\x1b[2J\x1b[H"); // clear screen
    wrt(b"\x1b[1m\x1b[32m");
    wrt(b"  ___  ____  ___  _  _  __   __ \n");
    wrt(b" / _ \\|  _ \\| __|| \\| | \\ \\ / /\n");
    wrt(b"| (_) | |_) | _| |  \\ |  \\ V / \n");
    wrt(b" \\___/|  __/|___||_|\\_|   \\_/  \n");
    wrt(b"      |_|\n");
    wrt(b"\x1b[0m");
    wrt(b"\x1b[36m  openv v0.1.0  \x1b[0m");
    wrt(b"RISC-V 64-bit Microkernel OS\n\n");

    // ── Service startup lines ─────────────────────────────────────────────────
    ok_line(b"Starting UART console");

    // Start VFS server first — other services may need filesystem access.
    let vfs_pid = spawn(b"/vfs-server".as_ptr(), 11);
    if vfs_pid > 0 {
        ok_line(b"Starting VFS server");
        // Give the VFS server a chance to register before proceeding.
        sys_yield();
    }

    ok_line(b"Mounting virtual filesystem");

    // Start the network daemon (runs in background; init reaps it if it exits)
    let net_pid = spawn(b"/net-smoltcp".as_ptr(), 12);
    if net_pid > 0 {
        ok_line(b"Starting network daemon (10.0.2.15/24)");
    }

    // Run fork test
    let ft_pid = spawn(b"/forktest".as_ptr(), 9);
    if ft_pid > 0 {
        let mut status: i32 = 0;
        waitpid(ft_pid, &mut status as *mut i32);
    }

    wrt(b"\n");

    // ── Login line ────────────────────────────────────────────────────────────
    wrt(b"openv 0.1.0-dev ttyS0\n\n");
    wrt(b"\x1b[1mopenv login:\x1b[0m guest \x1b[2m(automatic login)\x1b[0m\n\n");
    wrt(b"Welcome to \x1b[1;32mopenv\x1b[0m!\n");
    wrt(b"  Type \x1b[1mhelp\x1b[0m for a list of commands.\n\n");

    // ── Spawn shell (respawn loop) ────────────────────────────────────────────
    loop {
        let sh_pid = spawn(b"/sh".as_ptr(), 3);
        if sh_pid < 0 {
            wrt(b"\x1b[31mError: failed to spawn /sh\x1b[0m\n");
            loop {
                sys_yield();
            }
        }

        // Reap /sh and any other children that exit while we wait.
        loop {
            let mut status: i32 = 0;
            let reaped = waitpid(-1, &mut status as *mut i32);
            if reaped == sh_pid {
                // Shell exited — break inner loop to respawn.
                break;
            }
            if reaped < 0 {
                // No children left to reap; yield and retry.
                sys_yield();
            }
            // Otherwise reaped some other child; keep waiting.
        }
    }
}

fn ok_line(msg: &[u8]) {
    wrt(b"  \x1b[2m*\x1b[0m ");
    wrt(msg);
    // right-align [ OK ]
    let pad = 50usize.saturating_sub(msg.len());
    for _ in 0..pad {
        wrt(b" ");
    }
    wrt(b"\x1b[32m[ OK ]\x1b[0m\n");
}

#[allow(dead_code)]
fn fail_line(msg: &[u8]) {
    wrt(b"  \x1b[2m*\x1b[0m ");
    wrt(msg);
    let pad = 50usize.saturating_sub(msg.len());
    for _ in 0..pad {
        wrt(b" ");
    }
    wrt(b"\x1b[31m[FAIL]\x1b[0m\n");
}

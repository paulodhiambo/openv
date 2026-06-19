#![no_std]
#![no_main]

extern crate alloc;

use libos::{spawn, sys_yield, waitpid, write};
use alloc::string::ToString;

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

    // Wait for VFS server to register
    wrt(b"[init] Waiting for VFS server to start...\n");
    loop {
        libos::vfs_connect();
        if libos::vfs_pid().is_some() {
            break;
        }
        sys_yield();
    }

    ok_line(b"Mounting virtual filesystem");

    // Block and network drivers are managed by rs-server; init only waits
    // a few ticks so they finish registering before we need them.
    for _ in 0..200 { sys_yield(); }

    // ── Start component manager ───────────────────────────────────────────────
    let _cm_pid = spawn(b"/component-manager".as_ptr(), b"/component-manager".len());
    ok_line(b"Starting component manager");

    // ── Run verification tests ────────────────────────────────────────────────
    ok_line(b"Running boot-time verification");

    // Run epolltest
    let test_pid = spawn(b"/epolltest".as_ptr(), 10);
    if test_pid >= 0 {
        let mut status: i32 = 0;
        waitpid(test_pid, &mut status as *mut i32, 0);
        if status == 0 {
            ok_line(b"epolltest");
        } else {
            // Show FAIL but continue boot
            wrt(b"  \x1b[31mepolltest: FAILED (exit ");
            wrt(status.to_string().as_bytes());
            wrt(b")\x1b[0m\n");
        }
    } else {
        wrt(b"  \x1b[33mepolltest: not found, skipping\x1b[0m\n");
    }

    // Run pkg (quick self-test — shows usage, confirms binary runs)
    let pkg_pid = spawn(b"/pkg".as_ptr(), 4);
    if pkg_pid >= 0 {
        let mut status: i32 = 0;
        waitpid(pkg_pid, &mut status as *mut i32, 0);
        if status == 1 {
            // pkg returns 1 for usage (no args) — that's expected
            ok_line(b"pkg binary present");
        } else {
            wrt(b"  \x1b[33mpkg: unexpected exit status ");
            wrt(status.to_string().as_bytes());
            wrt(b"\x1b[0m\n");
        }
    } else {
        wrt(b"  \x1b[33mpkg: not found, skipping\x1b[0m\n");
    }

    // ── Login line ────────────────────────────────────────────────────────────
    wrt(b"openv 0.1.0-dev ttyS0\n\n");
    wrt(b"\x1b[1mopenv login:\x1b[0m guest \x1b[2m(automatic login)\x1b[0m\n\n");
    wrt(b"Welcome to \x1b[1;32mopenv\x1b[0m!\n");
    wrt(b"  Type \x1b[1mhelp\x1b[0m for a list of commands.\n\n");

    // ── Spawn shell (respawn loop) ────────────────────────────────────────────
    for _ in 0..50 {
        sys_yield();
    }

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
            let reaped = waitpid(-1, &mut status as *mut i32, 0);
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

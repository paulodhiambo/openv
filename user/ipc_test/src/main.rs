#![no_std]
#![no_main]

extern crate alloc;

use libos::{
    channel_create, channel_read, channel_write, exit, fork, pipe, read, waitpid, write,
    port_create, port_wait, port_queue, port_bind, PortPacket, SIGNAL_READABLE,
    job_set_policy, JOB_POLICY_MAX_MEMORY, RIGHTS_READ, RIGHTS_WRITE,
    handle_duplicate, mmap, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE,
    MAP_FAILED, dup, syscall,
};

fn wrt(s: &[u8]) {
    write(1, s.as_ptr(), s.len());
}

fn test_capability_transfer() {
    wrt(b"ipc_test: starting Zircon-style channel tests\n");

    let mut chan_fds = [0u32; 2];
    if channel_create(&mut chan_fds) != 0 {
        wrt(b"ipc_test: channel_create failed\n");
        exit(1);
    }

    let mut pipe_fds = [0u32; 2];
    if pipe(&mut pipe_fds) != 0 {
        wrt(b"ipc_test: pipe failed\n");
        exit(1);
    }

    let child = fork();
    if child == 0 {
        // Child: Read from channel, receive the pipe write handle, write to it
        wrt(b"ipc_test_child: ready\n");

        let mut rx_buf = [0u8; 128];
        let mut rx_handles = [0u32; 8];
        let mut bytes_read = 0;
        let mut handles_read = 0;

        let res = channel_read(
            chan_fds[1] as usize,
            rx_buf.as_mut_ptr(),
            rx_buf.len(),
            rx_handles.as_mut_ptr(),
            rx_handles.len(),
            &mut bytes_read,
            &mut handles_read,
        );

        if res != 0 {
            wrt(b"ipc_test_child: channel_read failed\n");
            exit(1);
        }

        if bytes_read != 5 || rx_buf[0..5] != *b"hello" {
            wrt(b"ipc_test_child: unexpected bytes read\n");
            exit(1);
        }

        if handles_read != 1 {
            wrt(b"ipc_test_child: expected exactly 1 handle\n");
            exit(1);
        }

        wrt(b"ipc_test_child: received pipe write capability via channel!\n");

        // Write test data to the transferred pipe write handle
        let test_data = b"capability transfer works!";
        let written = write(rx_handles[0] as usize, test_data.as_ptr(), test_data.len());
        if written as usize != test_data.len() {
            wrt(b"ipc_test_child: write to transferred pipe handle failed\n");
            exit(1);
        }

        wrt(b"ipc_test_child: successfully wrote to transferred capability\n");
        exit(0);
    } else if child > 0 {
        // Parent: Write pipe write handle to channel, then read from pipe read handle
        wrt(b"ipc_test_parent: waiting for child to start reading\n");
        for _ in 0..500000 {
            core::hint::spin_loop();
        }

        // Write the pipe write handle (pipe_fds[1]) to the channel
        let res = channel_write(
            chan_fds[0] as usize,
            b"hello".as_ptr(),
            5,
            &pipe_fds[1],
            1,
        );

        if res != 0 {
            wrt(b"ipc_test_parent: channel_write failed\n");
            exit(1);
        }

        wrt(b"ipc_test_parent: sent pipe write handle to child via channel\n");

        // Now read from the pipe read end
        let mut read_buf = [0u8; 128];
        let bytes_from_pipe = read(pipe_fds[0] as usize, read_buf.as_mut_ptr(), read_buf.len());

        if bytes_from_pipe <= 0 {
            wrt(b"ipc_test_parent: failed to read from pipe\n");
            exit(1);
        } else {
            let received = &read_buf[0..bytes_from_pipe as usize];
            if received == b"capability transfer works!" {
                wrt(b"ipc_test: Zircon-style channel test PASS\n");
            } else {
                wrt(b"ipc_test: Zircon-style channel test FAIL (unexpected content)\n");
                exit(1);
            }
        }

        let mut status = 0;
        waitpid(child, &mut status, 0);
    } else {
        wrt(b"ipc_test: fork failed\n");
        exit(1);
    }
}

fn test_handle_rights() {
    wrt(b"ipc_test: starting handle rights tests\n");

    let mut chan_fds = [0u32; 2];
    if channel_create(&mut chan_fds) != 0 {
        wrt(b"ipc_test: channel_create failed\n");
        exit(1);
    }

    // 1. Duplicate chan_fds[0] with only READ right (no WRITE)
    let mut restricted_write_h = 0u32;
    if handle_duplicate(chan_fds[0], RIGHTS_READ, &mut restricted_write_h) != 0 {
        wrt(b"ipc_test: handle_duplicate failed\n");
        exit(1);
    }

    // Attempt to write using the handle lacking WRITE right
    let res = channel_write(restricted_write_h as usize, b"fail".as_ptr(), 4, [0u32; 0].as_ptr(), 0);
    // EACCES is -13
    if res != -13 {
        wrt(b"ipc_test: expected EACCES (-13) when writing with read-only handle\n");
        exit(1);
    }
    wrt(b"ipc_test: rights check for WRITE blocked successfully!\n");

    // 2. Duplicate a channel write handle with no TRANSFER right
    let mut restricted_transfer_h = 0u32;
    // We duplicate chan_fds[0] with WRITE (no TRANSFER)
    if handle_duplicate(chan_fds[0], RIGHTS_WRITE, &mut restricted_transfer_h) != 0 {
        wrt(b"ipc_test: handle_duplicate failed\n");
        exit(1);
    }

    // Attempt to transfer restricted_transfer_h over chan_fds[0] (which has WRITE right).
    // The handle being transferred lacks TRANSFER right.
    let res2 = channel_write(chan_fds[0] as usize, b"fail".as_ptr(), 4, &restricted_transfer_h, 1);
    if res2 != -13 {
        wrt(b"ipc_test: expected EACCES (-13) when transferring handle without TRANSFER right\n");
        exit(1);
    }
    wrt(b"ipc_test: rights check for TRANSFER blocked successfully!\n");

    // 3. Duplicate a channel write handle with no DUPLICATE right
    let mut restricted_dup_h = 0u32;
    if handle_duplicate(chan_fds[0], RIGHTS_WRITE, &mut restricted_dup_h) != 0 {
        wrt(b"ipc_test: handle_duplicate failed\n");
        exit(1);
    }

    // Attempt to call dup on restricted_dup_h. It should fail with EACCES (-13)
    let res3 = dup(restricted_dup_h as i32);
    if res3 != -13 {
        wrt(b"ipc_test: expected EACCES (-13) when calling dup on handle without DUPLICATE right\n");
        exit(1);
    }
    wrt(b"ipc_test: rights check for DUPLICATE blocked successfully!\n");
}


fn test_ports() {
    wrt(b"ipc_test: starting async ports tests\n");

    let mut port_fd = 0u32;
    if port_create(&mut port_fd) != 0 {
        wrt(b"ipc_test: port_create failed\n");
        exit(1);
    }

    let mut chan_fds = [0u32; 2];
    if channel_create(&mut chan_fds) != 0 {
        wrt(b"ipc_test: channel_create failed\n");
        exit(1);
    }

    let key = 0xDEADC0DEu64;
    // Bind chan_fds[0] to port_fd
    if port_bind(chan_fds[0], port_fd, key, SIGNAL_READABLE) != 0 {
        wrt(b"ipc_test: port_bind failed\n");
        exit(1);
    }

    // Write a message to the other end (chan_fds[1]) to trigger SIGNAL_READABLE on chan_fds[0]
    if channel_write(chan_fds[1] as usize, b"ping".as_ptr(), 4, [0u32; 0].as_ptr(), 0) != 0 {
        wrt(b"ipc_test: channel_write failed\n");
        exit(1);
    }

    // Wait on the port
    let mut packet = PortPacket { key: 0, type_: 0, status: 0, observed_signals: 0 };
    if port_wait(port_fd, &mut packet) != 0 {
        wrt(b"ipc_test: port_wait failed\n");
        exit(1);
    }

    if packet.key != key {
        wrt(b"ipc_test: port packet key mismatch\n");
        exit(1);
    }

    if (packet.observed_signals & SIGNAL_READABLE) == 0 {
        wrt(b"ipc_test: port packet observed_signals missing SIGNAL_READABLE\n");
        exit(1);
    }

    wrt(b"ipc_test: port packet received correctly on channel write!\n");

    // Queue a packet directly to the port
    let direct_packet = PortPacket { key: 0x12345u64, type_: 99, status: 0, observed_signals: 0 };
    if port_queue(port_fd, &direct_packet) != 0 {
        wrt(b"ipc_test: port_queue failed\n");
        exit(1);
    }

    // Wait on the port
    let mut packet2 = PortPacket { key: 0, type_: 0, status: 0, observed_signals: 0 };
    if port_wait(port_fd, &mut packet2) != 0 {
        wrt(b"ipc_test: port_wait failed\n");
        exit(1);
    }

    if packet2.key != 0x12345u64 || packet2.type_ != 99 {
        wrt(b"ipc_test: queued port packet mismatch\n");
        exit(1);
    }

    wrt(b"ipc_test: port queue and direct wake works!\n");
}

fn test_job_memory_limits() {
    wrt(b"ipc_test: starting job memory limits tests\n");

    let child = fork();
    if child == 0 {
        // Child: set memory policy on self (its job) to 16 KB (4 pages)
        if job_set_policy(0, JOB_POLICY_MAX_MEMORY, 16384) != 0 {
            wrt(b"ipc_test_child: job_set_policy failed\n");
            exit(1);
        }

        // Allocate 6 pages (24 KB) using mmap
        let va = mmap(0, 24576, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
        if va == MAP_FAILED {
            wrt(b"ipc_test_child: mmap failed\n");
            exit(1);
        }

        // Try writing to all 6 pages to trigger page faults
        let ptr = va as *mut u8;
        for i in 0..6 {
            unsafe {
                core::ptr::write_volatile(ptr.add(i * 4096), 42);
            }
        }

        // If we reach here, the limit check failed to abort the process!
        wrt(b"ipc_test_child: ERROR - wrote to all pages without triggering OOM/segfault\n");
        exit(0);
    } else if child > 0 {
        let mut status = 0;
        waitpid(child, &mut status, 0);
        // Page fault failure causes exit status -11 (SIGSEGV)
        if status == -11 {
            wrt(b"ipc_test: job memory limits enforced successfully! (child terminated with -11)\n");
        } else {
            wrt(b"ipc_test: FAIL - child did not terminate with -11\n");
            exit(1);
        }
    } else {
        wrt(b"ipc_test: fork failed\n");
        exit(1);
    }
}

fn test_invalid_pointers() {
    wrt(b"ipc_test: starting invalid pointer validation tests\n");

    // 1. Test write with a null pointer
    let res1 = syscall(2, 1, 0, 10);
    // EFAULT is usize::MAX - 13
    if res1 != usize::MAX - 13 {
        wrt(b"ipc_test: FAIL - write with NULL did not return EFAULT\n");
        exit(1);
    }

    // 2. Test write with an out-of-bounds pointer (kernel memory)
    let res2 = syscall(2, 1, 0xFFFFFFFF_00000000usize, 10);
    if res2 != usize::MAX - 13 {
        wrt(b"ipc_test: FAIL - write with kernel pointer did not return EFAULT\n");
        exit(1);
    }

    // 3. Test read with a null pointer
    let res3 = syscall(5, 0, 0, 10); // syscall 5 is read
    if res3 != usize::MAX - 13 {
        wrt(b"ipc_test: FAIL - read with NULL did not return EFAULT\n");
        exit(1);
    }

    // 4. Test read with an out-of-bounds pointer
    let res4 = syscall(5, 0, 0xFFFFFFFF_00000000usize, 10);
    if res4 != usize::MAX - 13 {
        wrt(b"ipc_test: FAIL - read with kernel pointer did not return EFAULT\n");
        exit(1);
    }

    wrt(b"ipc_test: invalid pointer validation tests PASS!\n");
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    test_capability_transfer();
    test_handle_rights();
    test_ports();
    test_job_memory_limits();
    test_invalid_pointers();
    wrt(b"ipc_test: ALL TESTS PASS!\n");
    0
}

# 12. User Space

### 12.1 libos — The POSIX Shim

`libos` is a static library linked into every user-space binary. It provides:

**Entry point:**

```rust
// libos/src/start.rs
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    // Initialize user heap (buddy allocator over 2 MiB static BSS array)
    libos_init();
    // Call user main()
    let ret = main();
    // Exit with main's return code
    sys_exit(ret);
}
```

**User heap:**

```rust
static mut HEAP_MEM: [u8; 2 * 1024 * 1024] = [0; 2 * 1024 * 1024];  // 2 MiB BSS

fn libos_init() {
    unsafe {
        USER_HEAP.lock().init(
            HEAP_MEM.as_ptr() as usize,
            HEAP_MEM.len(),
        );
    }
}

#[global_allocator]
static USER_HEAP: buddy_system_allocator::LockedHeap<32> =
    buddy_system_allocator::LockedHeap::empty();
```

**Syscall wrappers:**

```rust
// Generic 3-argument syscall
#[inline(always)]
pub unsafe fn syscall(num: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "ecall",
        in("a7") num,
        inlateout("a0") a0 => ret,
        in("a1") a1,
        in("a2") a2,
        options(nostack)
    );
    ret
}

// 4-argument variant
#[inline(always)]
pub unsafe fn syscall4(num: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "ecall",
        in("a7") num,
        inlateout("a0") a0 => ret,
        in("a1") a1,
        in("a2") a2,
        in("a3") a3,
        options(nostack)
    );
    ret
}
```

All 60+ syscall wrappers (e.g., `sys_fork`, `sys_exec`, `sys_open`) are thin wrappers calling `syscall` or `syscall4` with the appropriate number.

### 12.2 `init` — PID 1

`init` is the first userspace process (PID 1). It:

1. Prints the boot banner (openv version, build info).
2. Becomes the **session leader** (sets its own `sid = pid`).
3. Runs the login prompt loop:
   ```
   loop:
       print "login: "; read username (raw mode for arrow keys)
       print "password: "; set_echo(false); read password; set_echo(true)
       uid = sys_authenticate(username, password)
       if uid >= 0:
           fork() → child: sys_setuid(uid); exec("/bin/sh")
           parent: waitpid(child_pid)
           if child exits: restart login loop (shell respawn)
   ```
4. If PID 1 exits, the kernel panics (no init = broken system).

### 12.3 `sh` — The Shell

The shell provides an interactive command-line environment:

**Features:**

| Feature | Implementation |
|---------|---------------|
| Line editing | Raw mode + manual echo; buffer with cursor position |
| History | `Vec<String>` of past commands; up/down arrow keys cycle |
| Pipelines | `fork()` per stage + `dup2()` to connect stdout→stdin |
| Input redirection `<` | `open(file)` + `dup2(fd, STDIN)` |
| Output redirection `>` | `create(file)` + `dup2(fd, STDOUT)` |
| Append redirection `>>` | `open(file, O_APPEND)` + `dup2(fd, STDOUT)` |
| Background `&` | `fork()` + do not `waitpid()` immediately |
| `cd` builtin | `sys_chdir()` |
| `pwd` builtin | `sys_getcwd()` |
| `exit` builtin | `sys_exit(0)` |
| `help` builtin | Print list of builtins |
| `history` builtin | Print command history |
| `nano` builtin | Full-screen text editor (built into shell binary) |

**Command search:**
1. If path contains `/`: use as literal path.
2. Try `/bin/<command>` directly.
3. Return "command not found".

**Pipeline execution (example: `ls | grep foo`):**

```
parent (sh):
  pipe() → [pipe_r, pipe_w]
  fork() → child1 (ls):
      dup2(pipe_w, STDOUT)
      close(pipe_r), close(pipe_w)
      exec("/bin/ls")
  fork() → child2 (grep):
      dup2(pipe_r, STDIN)
      close(pipe_r), close(pipe_w)
      exec("/bin/grep", ["foo"])
  close(pipe_r), close(pipe_w)
  waitpid(child1); waitpid(child2)
```

### 12.4 Coreutils

| Binary | Description |
|--------|-------------|
| `ls` | Lists directory contents. Reads from `sys_getdents`. |
| `cat` | Reads files/stdin and writes to stdout. Handles pipes. |
| `hello` | Minimal "Hello, World!" — used as a test binary. |
| `producer` | Writes sequential messages to a named pipe (IPC demo). |
| `consumer` | Reads from a named pipe and prints to stdout (IPC demo). |
| `doexec` | Exec wrapper: `exec(argv[1])` — useful for testing exec. |
| `forktest` | Exercises `fork()`/`waitpid()` with multiple children. |

### 12.5 `net-smoltcp`

`net-smoltcp` is the TCP/IP daemon process:

**Architecture:**

```rust
fn main() {
    // Set up smoltcp interface using raw Ethernet I/O syscalls
    let device = KernelDevice::new();  // wraps sys_net_send / sys_net_recv
    let mut iface = smoltcp::iface::Interface::new(config, &mut device);

    // Configure IP address (static or DHCP)
    iface.update_ip_addrs(|addrs| {
        addrs.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24)).ok();
    });

    loop {
        // 1. Poll smoltcp (runs TCP state machine, ARP, timers)
        iface.poll(Instant::now(), &mut device, &mut sockets);

        // 2. Accept new kernel sockets from registry
        while let sid = sys_daemon_next_socket() {
            if sid < 0 { break; }
            register_socket(sid);
        }

        // 3. Process opcode messages from each registered socket
        for sock in &mut kernel_sockets {
            if let Some(msg) = try_recv_from_socket(sock) {
                match msg.opcode {
                    BIND    => smoltcp_bind(sock, msg.addr),
                    LISTEN  => smoltcp_listen(sock),
                    CONNECT => smoltcp_connect(sock, msg.addr),
                    SEND    => smoltcp_send(sock, msg.data),
                }
            }
        }

        // 4. Forward received TCP data to waiting applications
        for sock in &mut kernel_sockets {
            if smoltcp_can_recv(sock) {
                let data = smoltcp_recv(sock);
                sys_write(sock.channel_fd, &data);
            }
        }
    }
}
```

`KernelDevice` implements smoltcp's `Device` trait using `sys_net_send` and `sys_net_recv` for physical I/O. smoltcp handles all ARP, IP fragmentation, TCP handshaking, retransmission, and flow control.

---
[Back to Index](README.md)

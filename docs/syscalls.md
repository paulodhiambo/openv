# openv Syscall Reference

> **Calling convention (RISC-V SBI / ecall)**
> `a7` = syscall number · `a0`–`a3` = arguments · `a0` = return value
>
> User-space calls these through the `libos` wrappers defined in `user/libos/src/lib.rs`.
> A negative return value (interpreted as `i32`) signals an error. The error sentinel is `-1` (`usize::MAX` as `usize`, `usize::MAX as i32 == -1`).

---

## Process & Scheduling

| # | Name | C-like signature | Return | Notes |
|---|------|-----------------|--------|-------|
| 0 | `yield` | `void sys_yield(void)` | — | Re-queues self at back of run-queue, schedules. |
| 1 | `exit` | `noreturn sys_exit(i32 status)` | — | Marks zombie, orphans children to init (PID 1), wakes parent if blocked in `waitpid`. Never returns. |
| 6 | `spawn` | `i32 sys_spawn(const u8 *path, usize len)` | child PID / -1 | `Process::new` + ELF load + user stack + push to `RUN_QUEUE`. Inherits parent's open fds, cwd, credentials. |
| 50 | `fork` | `i32 sys_fork(void)` | child PID in parent / 0 in child / -1 | COW clone of address space. Child's `a0` is set to 0, `sepc` advanced past ecall. Child pushed to `RUN_QUEUE`. |
| 51 | `exec` | `i32 sys_exec(const u8 *path, usize len)` | 0 on success / -1 | Replaces current process image with ELF at `path`. Allocates fresh page table, loads segments, destroys old space. Open fds are preserved. |
| 52 | `waitpid` | `i32 sys_waitpid(i32 pid, i32 *status, i32 opts)` | reaped PID / -1 | `pid == -1` means any child. Stores exit code at `*status`. Blocks if no child has exited yet (process set to `Stopped`, `sepc` re-wound). |
| 53 | `getpid` | `i32 sys_getpid(void)` | PID | `CURRENT_PIDS[tp]` — reads `tp` register for hart index. |
| 54 | `getppid` | `i32 sys_getppid(void)` | parent PID | Reads `proc.ppid` from `PROCESS_TABLE`. |
| 55 | `chdir` | `i32 sys_chdir(const u8 *path, usize len)` | 0 / -1 | VFS lookup; fails if vnode is not a directory. Updates `proc.cwd`. |
| 56 | `getcwd` | `isize sys_getcwd(u8 *buf, usize max)` | bytes copied / -1 | Copies current `proc.cwd` into user buffer. |

---

## File Descriptors

| # | Name | C-like signature | Return | Notes |
|---|------|-----------------|--------|-------|
| 2 | `write` | `isize sys_write(usize fd, const u8 *buf, usize len)` | bytes written / -1 | Dispatches by `KernelObject` variant: Console → UART; File → `vnode.write_at(offset)`; PipeWrite → `VecDeque.push_back`; Channel → enqueue message. |
| 5 | `read` | `isize sys_read(usize fd, u8 *buf, usize max)` | bytes read / -1 | Console: cooked or raw line discipline (polling). File: `vnode.read(offset)`. PipeRead: dequeue or block (re-wind sepc). Channel: `try_recv` or block. |
| 8 | `open` | `i32 sys_open(const u8 *path, usize len, u32 flags)` | fd / -1 | VFS `lookup_path` + `check_access(READ)` + insert `KernelObject::File` at lowest free fd. `FileDescription` carries a `Mutex<usize>` offset. |
| 9 | `close` | `i32 sys_close(usize fd)` | 0 / -1 | Removes entry from `HandleTable`. Drop of `PipeWriteHalf` invalidates write sentinel → readers see EOF. |
| 26 | `create` | `i32 sys_create(const u8 *path, usize len)` | fd / -1 | `lookup_parent` + `vnode.create(name)`. Returns a writable fd at offset 0. Truncates if file exists. |
| 57 | `dup` | `i32 sys_dup(usize oldfd)` | new fd / -1 | Clones `KernelObject`, inserts at lowest free handle. |
| 58 | `dup2` | `i32 sys_dup2(usize oldfd, usize newfd)` | newfd / -1 | Clones `KernelObject`, inserts at `newfd` (closes existing entry at `newfd`). |

---

## Pipes

| # | Name | C-like signature | Return | Notes |
|---|------|-----------------|--------|-------|
| 3 | `pipe` | `i32 sys_pipe(u32 fds[2])` | 0 / -1 | Creates `PipeReadHalf` + `PipeWriteHalf` backed by a shared `Arc<Mutex<VecDeque<u8>>>`. Inserts both into fd table; writes `[read_fd, write_fd]` to user memory. EOF when all `PipeWriteHalf` clones are dropped. |

---

## Directory Operations

| # | Name | C-like signature | Return | Notes |
|---|------|-----------------|--------|-------|
| 12 | `getdents` | `isize sys_getdents(const u8 *path, usize len, u8 *buf, usize max)` | bytes written / -1 | Calls `vnode.readdir()`, serialises entries as consecutive null-terminated name strings. ⚠ Takes a path, not a fd — future versions will accept a directory fd. |
| 27 | `mkdir` | `i32 sys_mkdir(const u8 *path, usize len)` | 0 / -1 | `lookup_parent` + `parent.mkdir(name)`. |
| 28 | `unlink` | `i32 sys_unlink(const u8 *path, usize len)` | 0 / -1 | `lookup_parent` + `parent.unlink(name)`. Fails on non-empty directories. |
| 29 | `rename` | `i32 sys_rename(const u8 *old, usize olen, const u8 *new, usize nlen)` | 0 / -1 | `lookup_parent` both sides. Cross-directory rename is not supported — fails with -1 if parents differ. |

---

## Credentials & Authentication

| # | Name | C-like signature | Return | Notes |
|---|------|-----------------|--------|-------|
| 23 | `setuid` | `i32 sys_setuid(u32 uid)` | 0 / -1 (EPERM) | root (`euid == 0`): sets real + effective UID. Non-root: can only set `euid` back to real UID. |
| 24 | `setgid` | `i32 sys_setgid(u32 gid)` | 0 / -1 (EPERM) | Same semantics as `setuid` for group. |
| 30 | `getuid` | `u32 sys_getuid(void)` | real UID | |
| 31 | `geteuid` | `u32 sys_geteuid(void)` | effective UID | |
| 32 | `getgid` | `u32 sys_getgid(void)` | real GID | |
| 33 | `getegid` | `u32 sys_getegid(void)` | effective GID | |
| 34 | `authenticate` | `u32 sys_authenticate(const u8 *user, usize ulen, const u8 *pass, usize plen)` | UID / -1 | FNV-1a hash comparison against the static user database. Returns matching UID on success. |
| 35 | `can_sudo` | `i32 sys_can_sudo(u32 uid)` | 1 / 0 | 1 if `uid == 0` (root) or `uid` is a member of GID 27 (sudo group). |

---

## Terminal (TTY)

| # | Name | C-like signature | Return | Notes |
|---|------|-----------------|--------|-------|
| 37 | `set_echo` | `i32 sys_set_echo(u32 enabled)` | 0 | `ECHO_ENABLED = enabled != 0`. Affects all processes (global buffer — per-session in future). |
| 38 | `set_raw` | `i32 sys_set_raw(u32 enabled)` | 0 | `RAW_MODE = enabled != 0`. In raw mode, `sys_read(0, …)` returns one character immediately without line buffering. |

---

## Networking

| # | Name | C-like signature | Return | Notes |
|---|------|-----------------|--------|-------|
| 10 | `net_send` | `isize sys_net_send(const u8 *frame, usize len)` | bytes sent / -1 | Sends raw Ethernet frame via the registered `NetDevice` (virtio-mmio or loopback). |
| 11 | `net_recv` | `isize sys_net_recv(u8 *buf, usize max)` | bytes received / 0 | Receives next queued Ethernet frame. Returns 0 if no frame available (non-blocking). |
| 40 | `socket` | `i32 sys_socket(void)` | fd / -1 | Creates a kernel channel pair. User-end fd inserted into caller's fd table. Daemon-end queued in `PENDING_SOCKETS`. |
| 41 | `daemon_next_socket` | `usize sys_daemon_next_socket(void)` | socket ID / 0 | Net daemon only: pops next pending socket ID from `PENDING_SOCKETS`. Returns 0 if queue empty. |
| 42 | `daemon_create_conn` | `i32 sys_daemon_create_conn(usize listen_sid)` | conn socket ID / -1 | Net daemon: creates a new accepted-connection channel pair, delivers one end to the process blocked in `accept`. |
| 43 | `accept` | `i32 sys_accept(usize listen_fd)` | new conn fd / -1 | Blocks until `daemon_create_conn` delivers a connection. |
| 44 | `bind` | `i32 sys_bind(usize fd, const u8 *addr, usize len)` | 0 / -1 | Sends `BIND` opcode message through the socket channel to the net daemon. |
| 45 | `listen` | `i32 sys_listen(usize fd, i32 backlog)` | 0 / -1 | Sends `LISTEN` opcode to net daemon. |
| 46 | `connect` | `i32 sys_connect(usize fd, const u8 *addr, usize len)` | 0 / -1 | Sends `CONNECT` opcode to net daemon. |
| 47 | `sock_send` | `isize sys_sock_send(usize fd, const u8 *buf, usize len)` | bytes sent / -1 | Sends `DATA` opcode + payload message to net daemon. |
| 48 | `sock_recv` | `isize sys_sock_recv(usize fd, u8 *buf, usize max)` | bytes received / -1 | `try_recv` on socket channel; blocks if empty. |

---

## Error Values

All syscalls signal errors by returning `usize::MAX` (which equals `-1i32` when cast, following Unix convention). There is currently no `errno` variable; the caller knows only that an error occurred, not its specific cause.

Future work: introduce named error codes (`ENOENT = usize::MAX`, `EACCES = usize::MAX - 12`, etc.) and an `errno` thread-local in libos.

---

## libos Wrapper Example

```rust
// In user/libos/src/lib.rs
#[unsafe(no_mangle)]
pub extern "C" fn read(fd: usize, buf: *mut u8, len: usize) -> isize {
    syscall(5, fd, buf as usize, len) as isize  // syscall 5 = sys_read
}

#[inline]
pub fn syscall(sys_num: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    let mut ret: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") arg0 => ret,
            in("a1") arg1,
            in("a2") arg2,
            in("a7") sys_num,  // syscall number in a7
            options(nostack)
        );
    }
    ret
}
```

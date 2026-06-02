OpenV Syscall ABI

Overview

This document defines the stable syscall Application Binary Interface (ABI) for the OpenV project. It documents register usage, syscall numbering, return conventions, and common error encodings so kernel and userland (libos) agree.

Calling convention

- Syscall instruction: ecall from user mode.
- Syscall number: placed in register a7 before ecall.
- Arguments: a0..a6 (up to 7 general-purpose args) are used for positional syscall arguments.
  - Common helpers use up to a3 (4 args) but userland may use more if needed.
- Return value: a0 contains the syscall return value on success or an encoded negative error on failure.
- Side effects: On blocking syscalls the kernel may "rewind" sepc (subtract 4) and schedule the process; when resumed the syscall restarts and may return a value.

Return / Error encoding

- On success the natural value (usize or byte count) is returned in a0.
- On error the kernel returns a signed-negative errno encoded as a usize:
  - Generic error / -1 is returned as usize::MAX (all ones).
  - Specific errno values are returned as usize::MAX - ERRNO (for example, EACCES is returned as usize::MAX - 12).
  - When casting the returned usize to a signed integer (isize/i32) in userland, this yields the conventional negative errno (e.g., -13 for EACCES would be usize::MAX - 13 cast to isize).

Guidelines for userland

- Use libos::syscall / syscall4 helpers which place arguments in the correct registers and invoke ecall.
- Interpret error returns by casting the usize a0 into signed types (isize/i32) and checking for negative values.
- For blocking syscalls (read on console, accept, recv on channel), the kernel may restart the syscall; userland wrappers should handle the normal return value semantics.

Syscall Table (current implementation)

Number | Name | Args (a0,a1,a2,a3...) | Return / Notes
-------|------|-----------------------|----------------
0 | sys_yield | - | Yield scheduler, no return value
1 | sys_exit | status (a0) | never returns to caller
2 | sys_write | fd (a0), buf_ptr (a1), len (a2) | bytes written or -errno
3 | sys_pipe | ptr to [u32;2] (a0) | 0 on success, fills pair of fds in user buffer
5 | sys_read | fd (a0), buf_ptr (a1), len (a2) | bytes read or -errno; may block
6 | sys_spawn | path_ptr (a0), path_len (a1) | child pid or -errno
8 | sys_open | path_ptr (a0), path_len (a1), flags (a2) | fd or -errno
9 | sys_close | fd (a0) | 0 or -errno
10 | sys_net_send | ptr (a0), len (a1) | bytes sent or -errno
11 | sys_net_recv | buf_ptr (a0), max_len (a1) | bytes received or -errno; may block
12 | sys_getdents | path_ptr (a0), path_len (a1), buf_ptr (a2) | bytes written or -errno
23 | sys_setuid | uid (a0) | 0 or -EPERM
24 | sys_setgid | gid (a0) | 0 or -EPERM
30 | sys_getuid | - | uid
31 | sys_geteuid | - | euid
32 | sys_getgid | - | gid
33 | sys_getegid | - | egid
34 | sys_authenticate | user_ptr (a0), user_len (a1), pass_ptr (a2), pass_len (a3) | uid or -errno
35 | sys_can_sudo | uid (a0) (0 means caller) | 1 if allowed, 0 otherwise
37 | sys_set_echo | enabled (a0) | 0
40 | sys_socket | domain (a0), type (a1), proto (a2) | user fd (Channel) or -errno
41 | sys_daemon_next_socket | - | packed ((sid<<32)|fd) or -1 when none
42 | sys_daemon_create_conn | listen_sid (a0) | daemon-side fd (net) or -errno
43 | sys_accept | listen_fd (a0) | new user fd or -errno; may block
44 | sys_bind | fd (a0), addr_ptr (a1), addr_len (a2) | 0 or -errno
45 | sys_listen | fd (a0), backlog (a1) | 0 or -errno
46 | sys_connect | fd (a0), addr_ptr (a1), addr_len (a2) | 0 or -errno
47 | sys_sock_send | fd (a0), buf_ptr (a1), len (a2) | bytes sent or -errno
48 | sys_sock_recv | fd (a0), buf_ptr (a1), max_len (a2) | bytes read or -errno; may block
50 | sys_fork | - | create child process (scaffold; returns child pid in parent, 0 in child)

Notes and extensibility

- The table above reflects the current kernel implementation and should be considered the authoritative mapping until changed.
- Adding new syscalls: pick an unused number, add kernel handler, update this document and libos wrappers together.
- If more than 7 args are required, pass a user-space pointer to a struct or use a shared memory buffer.

Examples

- Simple write:
  let n = syscall(2, fd, buf.as_ptr() as usize, buf.len());
  if (n as isize) < 0 { /* handle errno = -(n as isize) */ }

- Using syscall4 for 4-arg calls (authenticate):
  let ret = syscall4(34, username_ptr, username_len, password_ptr, password_len) as isize;
  if ret < 0 { /* error; errno = -ret */ }

Document history

- Created 2026-06-02 by developer conversation edits.


# OpenV Operating System - System Documentation

## 1. Overview
OpenV is a microkernel-based operating system designed for RISC-V. It aims to provide a secure, capability-based foundation with a POSIX-like personality layer to support standard Unix-like utilities.

## 2. Architecture
OpenV employs a microkernel design:
- **Core Kernel**: Handles memory management (Sv39 paging, VMOs), IPC (Channels, Handles), and process scheduling.
- **POSIX Personality**: A userspace-centric layer (`libos`) that translates POSIX-like syscalls into microkernel operations.
- **VFS Layer**: A unified namespace supporting multiple backends (initrd/MemFS initially).

## 3. Security Model
OpenV bridges capability-based security with traditional Unix identities.
- **Identity**: Processes possess `uid`, `gid`, `euid`, `egid`. Inheritance occurs during `spawn`.
- **Enforcement**: Access to files is checked via `check_access` during `open()`, considering `uid`/`gid` and file mode bits (`rwx`).
- **Privilege**: Root (`uid=0`) bypasses all permission checks.
- **Privileged Transition**: Setuid/Setgid bits on binaries allow processes to run with the file owner's privileges.

## 4. Syscall Interface
Syscalls are invoked via `ecall` (RISC-V).

| Syscall | Num | Description |
| :--- | :--- | :--- |
| `sys_yield` | 0 | Voluntarily yield the processor. |
| `sys_exit` | 1 | Terminate the process. |
| `sys_write` | 2 | Write bytes to a file descriptor. |
| `sys_pipe` | 3 | Create a pipe channel pair. |
| `sys_read` | 5 | Read bytes from a file descriptor. |
| `sys_spawn` | 6 | Spawn a new process from ELF binary. |
| `sys_open` | 8 | Open a file with permission checks. |
| `sys_close` | 9 | Close a file descriptor. |
| `sys_getdents` | 12 | Read directory entries. |
| `sys_setuid` | 23 | Manage UID. |
| `sys_setgid` | 24 | Manage GID. |

## 5. Network Stack (Work in Progress)
The network stack is based on **smoltcp** (`no_std` TCP/IP).
- **Driver**: Implements `NetDevice` trait to interface with virtio-mmio.
- **Stack**: `src/net/stack.rs` manages the `smoltcp` interface and polling.

## 6. Development Workflow
- **Build**: `./scripts/build.sh` builds kernel, userspace, and packages initrd. `./scripts/run.sh` boots it in QEMU.
- **Testing**: Golden-transcript testing on serial output is the recommended approach for verification.

# 1. Overview

**openv** is a RISC-V 64-bit microkernel-inspired operating system written entirely in Rust. It targets the QEMU `virt` machine and is compiled for the `riscv64gc-unknown-none-elf` bare-metal target, running on top of the OpenSBI firmware.

### Design Philosophy

openv follows a **pragmatic microkernel** approach:

- **In-kernel for v1:** Memory management (PMM + VMM), inter-process communication (IPC channels, pipes), trap dispatch, and POSIX-flavored syscalls live inside the kernel proper. This simplifies the v1 codebase and avoids costly context switches on every OS call.
- **Future trajectory:** The design is explicitly structured to allow POSIX-layer services (filesystem servers, network daemons, device managers) to be migrated into isolated userspace servers. Existing socket networking already demonstrates this pattern — the kernel provides raw Ethernet I/O, while TCP/IP semantics live in the `net-smoltcp` userspace daemon.
- **Safety over performance (initially):** Global mutexes protect shared state (PMM free-list, process table, VFS mount table). The goal is correctness first; fine-grained locking and lock-free structures are deferred to later revisions.

### Key Parameters

| Property               | Value                               |
|------------------------|-------------------------------------|
| ISA                    | RISC-V RV64GC                       |
| Compilation target     | `riscv64gc-unknown-none-elf`        |
| Firmware               | OpenSBI (M-mode)                    |
| Machine                | QEMU `virt`                         |
| Kernel load address    | `0x80200000`                        |
| VM scheme              | Sv39 (3-level page tables)          |
| Page size              | 4 KiB                               |
| Max RAM tracked        | 1 GiB                               |
| Max HARTs              | 4                                   |
| User load address      | `0x100000000` (4 GiB mark)          |
| User stack top         | `0x200000000` (8 GiB)              |

---
[Back to Index](README.md)

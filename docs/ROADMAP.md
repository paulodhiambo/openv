# openv — Development Roadmap

This document tracks what has been implemented, what is in progress, and what is planned.

## ✅ Phase 1 — Foundation (Complete)

*POSIX-compatible kernel foundation and interactive shell.*

- [x] Stable syscall ABI (`ecall`, `a7` = number, `a0`–`a3` args)
- [x] `fork` with Copy-on-Write memory semantics
- [x] `exec` (replaces process image in-place)
- [x] `waitpid` with zombie reaping
- [x] PID 1 (`init`) — login prompt, shell respawn loop
- [x] libos POSIX shim — 60+ syscall wrappers, 2 MB user heap, `_start`
- [x] Interactive shell (`sh`) — pipelines, redirection, history, builtins, nano editor
- [x] Coreutils: `ls`, `cat`, `hello`, `producer`, `consumer`, `doexec`, `forktest`

## ✅ Phase 2 — Kernel Architecture (Complete)

*Production-grade memory management and multi-process kernel.*

- [x] Preemptive multitasking — 10 ms SBI timer, round-robin FIFO scheduler
- [x] SMP support — up to 4 HARTs, per-hart stacks, TLB shootdown IPI
- [x] Sv39 demand paging — lazy zero-fill page allocation
- [x] Copy-on-Write fork — page-level refcounting, store-fault resolution
- [x] Virtual File System (VFS) — `Vnode` trait + mount table
- [x] MemFS — initrd-backed root filesystem (tar parser)
- [x] ProcFS — `/proc/<pid>/status`
- [x] DevFS — `/dev/null`, `/dev/zero`, `/dev/tty`
- [x] Persistent filesystem (OFS) on virtio-blk → mounted at `/mnt`
- [x] File descriptors, `dup`/`dup2`, pipes with EOF detection
- [x] IPC channels — bidirectional message queues
- [x] Multi-user security — Unix DAC, setuid/setgid bits, sudo group
- [x] UART TTY line discipline — cooked + raw mode, echo, Ctrl-C
- [x] Device driver framework — FDT probe, PLIC claim/complete

## ✅ Phase 3 — Networking (Complete, in userspace)

*TCP/IP networking via userspace smoltcp daemon.*

- [x] Virtio-mmio network driver (legacy virtqueue interface)
- [x] Raw Ethernet send/receive syscalls (`net_send`, `net_recv`)
- [x] Socket registry in kernel (`sys_socket`, `sys_accept`, etc.)
- [x] `net-smoltcp` daemon — full TCP/IP stack running in userspace
- [x] Socket lifecycle: `bind`, `listen`, `connect`, `accept`, `send`, `recv`

## 🚧 Phase 4 — Robustness (In Progress)

*Fix known correctness issues and prepare for multi-user workloads.*

- [x] Split `trap.rs` God File into `syscall/` subsystem modules
- [x] Priority-based scheduler (`BTreeMap<u8, VecDeque<Pid>>`) — replaces FIFO round-robin
- [x] No priority inheritance on spawn — children always start at `PRIO_NORMAL`, preventing PRIO_HIGH servers from starving user processes via driver children
- [x] ELF loader zeros BSS pages — pages previously kept PMM poison (0xcd), corrupting zero-initialised statics in any newly spawned process
- [x] libos `F_GET_VFS_FD` constant fixed (`1001` not `6`/`F_SETLK`) — all `write`/`read`/`close` calls to kernel TTY fds were silently failing
- [x] PLIC UART IRQ 10 enabled in `kmain` — UART external interrupts were never enabled, keyboard input only worked via timer-ISR polling
- [x] `BOOT_HARTID` tracking — UART polling and timer ISR now use the actual boot HART ID instead of hardcoded HART 0
- [x] virtio-blk device and OFS persistent filesystem fully operational — `make run` auto-creates `disk.img`; OFS auto-formats blank disk on first boot
- [ ] Fix data races: `ppid` and `satp_val` via atomic types
- [ ] Fix PMM `static mut` free-list — wrap in `Mutex`
- [ ] Per-session TTY line discipline buffer (not global)
- [ ] Named errno constants for all error returns
- [ ] Full preemptive context switch at timer interrupt (not just re-queue)
- [ ] Signal subsystem — `sigaction`, `sigprocmask`, `kill`, `sigreturn` trampoline
- [ ] Job control — process groups, sessions, `tcsetpgrp`, `SIGTSTP`/`SIGCONT`
- [ ] `sys_fstat` — stat an already-open fd, not just a path
- [ ] `sys_getdents` — accept directory fd instead of path

## 📋 Phase 5 — Extended POSIX (Planned)

*Broader compatibility to run more existing software.*

- [ ] `mmap` / `munmap` — map files and anonymous memory regions
- [ ] `poll` / `select` — multiplexed I/O wait
- [ ] `fcntl` — file descriptor flags and locking
- [ ] `lseek` with `SEEK_SET`/`SEEK_CUR`/`SEEK_END`
- [ ] Symlinks in VFS
- [ ] Shared libraries and a dynamic linker (`ld.so`)
- [ ] Port musl libc subset for broader application compatibility

## 💡 Phase 6 — GUI (Future)

*Graphics and interactive desktop environment.*

- [ ] Virtio-GPU framebuffer driver
- [ ] Virtio-input keyboard/mouse driver
- [ ] Basic compositing window manager
- [ ] `embedded-graphics` integration for drawing primitives

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for how to help, especially the **Known Issues to Fix** table which lists good first contributions.

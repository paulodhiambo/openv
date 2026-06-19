# openv

> A RISC-V 64-bit microkernel OS written in Rust.

[![CI](https://github.com/paulodhiambo/openv/actions/workflows/build.yml/badge.svg)](https://github.com/paulodhiambo/openv/actions/workflows/build.yml)
[![Rust nightly](https://img.shields.io/badge/rust-nightly-orange)](rust-toolchain.toml)
[![Architecture](https://img.shields.io/badge/arch-RISC--V%2064-blue)](#architecture)
[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)

openv is a **RISC-V 64-bit microkernel** written in Rust. It boots under QEMU via OpenSBI, provides an interactive Unix-like environment, and implements a comprehensive POSIX-compatible kernel from scratch.

For detailed technical documentation, see the [Documentation Index](docs/index.md).

- **Preemptive multitasking** — round-robin scheduler, 10 ms SBI timer slices
- **Virtual memory** — Sv39 paging, COW fork, demand paging (zero-fill on access)
- **VFS** — MemFS, ProcFS, DevFS, persistent block filesystem (OFS), mount points
- **POSIX process model** — fork, exec, waitpid, pipes, SIGINT (Ctrl-C)
- **IPC** — capability-style handles, bidirectional channels, Unix pipes
- **Multi-user security** — Unix DAC (rwxr-xr-x), setuid/setgid, sudo group
- **Networking** — virtio-mmio NIC driver + userspace smoltcp TCP/IP stack
- **Interactive shell** — line editing, history, pipelines, I/O redirection
- **SMP** — up to 4 harts, per-hart stacks, TLB shootdown via SBI IPI

---

## Quick Start

```console
# Prerequisites: Rust nightly + QEMU
rustup toolchain install nightly \
  --component rust-src,llvm-tools-preview,rustfmt,clippy \
  --target riscv64gc-unknown-none-elf

brew install qemu          # macOS
# apt install qemu-system-riscv64  # Ubuntu/Debian

# Build userspace, package initrd, build kernel, and launch
make
```

You'll land in a login prompt. Default credentials: **guest / guest** or **root / root**.

```
$ ls /
$ cat /dummy.txt
$ ls /proc
$ ls /dev
$ echo hello world
$ cat /proc/1/status
$ mkdir /tmp && ls /
```

---

## Prerequisites

| Tool | Version | Purpose |
|------|---------|---------|
| `rustup` + nightly | any recent nightly | Build kernel and userspace |
| `rust-src` component | — | `build-std` for `core`/`alloc` |
| `riscv64gc-unknown-none-elf` target | — | Cross-compilation |
| `qemu-system-riscv64` | 7+ | Running the OS |
| `riscv64-unknown-elf-gdb` | any | Optional: GDB debugging |

---

## Build System

### Make targets

| Target | Description |
|--------|-------------|
| `make` / `make all` | Build everything then run in QEMU (default) |
| `make build` | Userspace → initrd → kernel (debug) |
| `make build-release` | Full release build |
| `make build-kernel` | Kernel only |
| `make build-user` | Userspace binaries only |
| `make initrd` | Package initrd from existing bins |
| `make disk` | Create 8 MB `disk.img` for virtio-blk (auto-created by `run`) |
| `make run` | Boot in QEMU; creates `disk.img` if absent |
| `make debug` | Boot with GDB server on `:1234` |
| `make image` | Build `openv.img` disk image |
| `make fmt` | Format all Rust source |
| `make clippy` | Lint kernel |
| `make check` | `cargo check` kernel |
| `make clean` | Remove build artifacts (preserves `disk.img` OFS data) |
| `make clean-all` | Remove everything including `disk.img` |

### Variables

```console
make BINS="init sh" QEMU_MEM=512M QEMU_CPUS=2 run
```

| Variable | Default | Description |
|----------|---------|-------------|
| `BINS` | `init sh ls cat hello producer consumer doexec forktest net-smoltcp spin vfs-server pm-server rs-server virtio-blk-driver …` | Userspace binaries copied into the initrd |
| `QEMU_MEM` | `512M` | QEMU guest memory |
| `QEMU_CPUS` | `4` | Number of RISC-V harts |

### Scripts

| Script | Purpose |
|--------|---------|
| `scripts/build.sh [--release]` | Full build |
| `scripts/run.sh` | Boot in QEMU |
| `scripts/build_image.sh` | Create `openv.img` disk image |

---

## Project Structure

```
openv/
├── Makefile
├── linker.ld                  # Kernel link at 0x80200000
├── rust-toolchain.toml        # Nightly pin
│
├── src/                       ── Kernel ──────────────────────────────
│   ├── boot.s                 # _start: park secondaries, BSS clear, kmain
│   ├── main.rs                # kmain, panic handler, __halt_cpu
│   ├── trap.rs                # Trap vector, ~60 syscalls, page faults, IRQs
│   ├── uart.rs                # NS16550 UART driver, print!/println!
│   ├── timer.rs               # SBI timer, 10 ms tick
│   ├── plic.rs                # PLIC claim/complete
│   ├── smp.rs                 # SMP startup, per-hart stacks, IPI
│   ├── sync.rs                # Deadlock-detecting Mutex wrapper
│   ├── errno.rs               # POSIX errno constants
│   │
│   ├── mm/                    ── Memory Management ────────────────────
│   │   ├── pmm.rs             # Physical allocator: DTB detection, free-list, refcounts
│   │   ├── vmm.rs             # Sv39 page tables, COW fork, demand paging
│   │   ├── heap.rs            # Buddy allocator (16 MB heap)
│   │   └── vmo.rs             # Virtual Memory Object
│   │
│   ├── vfs/                   ── Virtual File System ──────────────────
│   │   ├── mod.rs             # Vnode trait, MountTable, lookup_path
│   │   ├── tar.rs             # UStar parser → MemFS at boot
│   │   ├── memfs.rs           # RoFile (zero-copy), MemFile (writable), MemDir
│   │   ├── blockfs.rs         # Persistent block filesystem (OFS)
│   │   ├── procfs.rs          # /proc/<pid>/status
│   │   └── devfs.rs           # /dev/null  /dev/zero  /dev/tty
│   │
│   ├── posix/                 ── Process Model ────────────────────────
│   │   ├── process.rs         # Process struct, PROCESS_TABLE, scheduler
│   │   ├── spawn.rs           # posix_spawn, exit, sys_fork (COW), sys_exec
│   │   ├── elf.rs             # ELF loader (PT_LOAD segments)
│   │   ├── user.rs            # User/group DB, FNV-1a auth, sudo
│   │   └── wait.rs            # waitpid, zombie reaping
│   │
│   ├── ipc/                   ── Inter-Process Communication ───────────
│   │   ├── channel.rs         # Bidirectional message channels
│   │   └── handle.rs          # HandleTable, KernelObject, pipes, FileDescription
│   │
│   ├── net/                   ── Networking ───────────────────────────
│   │   ├── mod.rs             # NetDevice trait, probe/init
│   │   ├── virtio_mmio.rs     # Virtio-mmio NIC driver (legacy, device-id=1)
│   │   ├── virtio_net.rs      # Loopback fallback device
│   │   ├── socket.rs          # Socket registry, pending accepts
│   │   ├── virtqueue.rs       # Virtio split virtqueue implementation
│   │   └── pktbuf.rs          # Packet buffer pool
│   │
│   ├── block/                 ── Block Devices ────────────────────────
│   │   ├── mod.rs             # Block device registry
│   │   └── virtio_blk.rs      # Virtio-blk driver (device-id=2)
│   │
│   └── drivers/               # FDT probe, interrupt dispatch table
│
└── user/                      ── Userspace ────────────────────────────
    ├── libos/                 # POSIX syscall shim + 2 MB heap + _start
    ├── init/                  # PID 1: login prompt, shell respawn
    ├── sh/                    # Shell: editing, history, pipes, redirection
    ├── ls/                    # Directory listing
    ├── cat/                   # File / stdin concatenation
    ├── hello/                 # Smoke-test binary
    ├── spin/                  # CPU spin test (preemption verification)
    ├── producer/ consumer/    # IPC pipe test programs
    ├── doexec/ forktest/      # exec/fork test programs
    └── net-smoltcp/           # Userspace TCP/IP daemon (smoltcp)
```

---

## Architecture

```
╔══════════════════════════════════════════════════════════════════╗
║  User processes (S-mode, U-bit PTEs)                             ║
║   init │ sh │ ls │ cat │ net-smoltcp │ spin │ ...                ║
║   └── libos (ecall wrappers, 2 MB bump heap, _start)             ║
╠══════════════════════════════════════════════════════════════════╣
║  Kernel trap handler  src/trap.rs                                ║
║   ├─ Syscall dispatch (~60 syscalls, a7 = number)                ║
║   ├─ Demand paging  (Exception 12/13 → alloc zero page)         ║
║   ├─ COW fault      (Exception 15   → copy physical page)       ║
║   ├─ Timer IRQ      (Interrupt 5    → re-queue, rearm SBI)      ║
║   ├─ TLB shootdown  (Interrupt 1    → sfence.vma)               ║
║   └─ External IRQ   (Interrupt 9/11 → PLIC → driver dispatch)   ║
╠══════════════════════════════════════════════════════════════════╣
║  Kernel subsystems                                               ║
║   PMM  ── physical pages, refcounts (free-list + u16 array)      ║
║   VMM  ── Sv39 page tables, clone_user_space, destroy_user_space ║
║   VFS  ── Vnode trait, MemFS/ProcFS/DevFS/OFS, mount table      ║
║   POSIX── Process table, FIFO scheduler, ELF loader, waitpid    ║
║   IPC  ── Channels, HandleTable, pipes, FileDescriptions        ║
║   NET  ── Virtio-mmio NIC driver, socket registry               ║
║   BLK  ── Virtio-blk driver, OFS persistent filesystem          ║
╠══════════════════════════════════════════════════════════════════╣
║  RISC-V hardware (QEMU virt machine)                             ║
║   NS16550 UART │ SBI timer │ PLIC │ Virtio-MMIO NIC/Blk          ║
╚══════════════════════════════════════════════════════════════════╝
```

### Key Design Decisions

**Pragmatic microkernel.** The kernel provides memory management, IPC, and trap dispatch. POSIX semantics (process lifecycle, VFS, scheduling) live in the kernel for v1 pragmatism. A strict Fuchsia-style split — with VFS and process servers in userspace — is the intended v2 direction.

**Non-reentrant spinlocks.** `crate::sync::Mutex` is a deadlock-detecting wrapper around `spin::Mutex` that tracks the owning hart and panics on recursive acquisition. Timer IRQs therefore only *re-queue* the current process — they never call `schedule()` from interrupt context.

**Capability-style handles.** File descriptors are `HandleTable` entries storing `KernelObject` variants (`Console`, `Channel`, `File`, `PipeRead`, `PipeWrite`, `Vmo`). `dup`/`dup2` clone the handle slot. Drop closes and frees the resource. Pipe EOF is detected via a `Weak` reference on the write-end sentinel `Arc<()>`.

**COW fork + demand paging.** `sys_fork` marks all writable user pages in both parent and child as read-only and increments their refcounts. The first write to a shared page traps to `handle_store_page_fault`, which allocates a private copy, copies content, installs a writable PTE, and decrements the old refcount. Unmapped pages are zero-filled on first access.

**UART line discipline.** The kernel drains the full UART FIFO on every timer tick (`poll_uart_into_linedisc`). Reads from the console syscall pull exclusively from `LINE_DISC_BUFFER`, eliminating any race between the timer ISR and `sys_read`. Raw mode (used by the shell for arrow-key history) delivers bytes immediately; cooked mode buffers until `\n`.

**Initrd-based root filesystem.** A UStar tar archive (`test_root.tar`) is passed via the DTB `chosen` node. The kernel parses it into a MemFS tree at boot. `/proc` (ProcFS) and `/dev` (DevFS) are layered on top via the mount table. OFS mounts at `/mnt` when a virtio-blk device is present; the first boot auto-formats a blank disk. `make run` creates `disk.img` automatically if it does not exist.

**Userspace TCP/IP.** The kernel provides only raw Ethernet send/receive via two syscalls. The full TCP/IP stack (smoltcp) runs in the `net-smoltcp` userspace daemon, with connections proxied to user processes via kernel IPC channels.

---

## Syscall Table

| # | Name | Signature | Description |
|---|------|-----------|-------------|
| 0 | `yield` | `()` | Yield CPU to next runnable process |
| 1 | `exit` | `(status: i32) → !` | Terminate process, wake parent |
| 2 | `write` | `(fd, buf, len) → isize` | Write to fd (Console/File/Pipe/Channel) |
| 3 | `pipe` | `(&[u32; 2]) → i32` | Create pipe pair, fill [read_fd, write_fd] |
| 5 | `read` | `(fd, buf, max) → isize` | Read from fd (blocks until data) |
| 6 | `spawn` | `(path, len) → i32` | Spawn child process, return PID |
| 8 | `open` | `(path, len, flags) → i32` | Open file, return fd |
| 9 | `close` | `(fd) → i32` | Close fd |
| 10 | `net_send` | `(buf, len) → isize` | Send raw Ethernet frame |
| 11 | `net_recv` | `(buf, max) → isize` | Receive raw Ethernet frame |
| 12 | `getdents` | `(path, len, buf, max) → isize` | List directory (null-separated names) |
| 23 | `setuid` | `(uid) → i32` | Set user ID |
| 24 | `setgid` | `(gid) → i32` | Set group ID |
| 26 | `create` | `(path, len) → i32` | Create/truncate file, return writable fd |
| 27 | `mkdir` | `(path, len) → i32` | Create directory |
| 28 | `unlink` | `(path, len) → i32` | Remove file |
| 29 | `rename` | `(old, olen, new, nlen) → i32` | Rename file |
| 30 | `getuid` | `() → u32` | Real UID |
| 31 | `geteuid` | `() → u32` | Effective UID |
| 32 | `getgid` | `() → u32` | Real GID |
| 33 | `getegid` | `() → u32` | Effective GID |
| 34 | `authenticate` | `(user, ulen, pass, plen) → u32` | Verify credentials, return UID or -1 |
| 35 | `can_sudo` | `(uid) → i32` | 1 if uid can sudo |
| 37 | `set_echo` | `(enabled) → i32` | Toggle terminal echo |
| 38 | `set_raw` | `(enabled) → i32` | Toggle raw mode (char-by-char) |
| 39 | `set_fg_pid` | `(pid) → i32` | Set foreground process for SIGINT delivery |
| 40 | `socket` | `() → i32` | Create proxied socket fd |
| 41 | `daemon_next_socket` | `() → usize` | Net daemon: pop next socket |
| 42 | `daemon_create_conn` | `(listen_sid) → i32` | Net daemon: create accepted connection |
| 43 | `accept` | `(listen_fd) → i32` | Accept incoming connection (blocks) |
| 44 | `bind` | `(fd, addr, alen) → i32` | Bind socket to address |
| 45 | `listen` | `(fd, backlog) → i32` | Mark socket as listening |
| 46 | `connect` | `(fd, addr, alen) → i32` | Connect to remote address |
| 47 | `sock_send` | `(fd, buf, len) → isize` | Send data on socket |
| 48 | `sock_recv` | `(fd, buf, max) → isize` | Receive data on socket (blocks) |
| 50 | `fork` | `() → i32` | COW fork; 0 in child, child PID in parent |
| 51 | `exec` | `(path, len) → i32` | Replace process image with ELF |
| 52 | `waitpid` | `(pid, status_ptr, opts) → i32` | Wait for child exit, reap zombie |
| 53 | `getpid` | `() → i32` | Current process PID |
| 54 | `getppid` | `() → i32` | Parent process PID |
| 55 | `chdir` | `(path, len) → i32` | Change working directory |
| 56 | `getcwd` | `(buf, max) → isize` | Copy cwd to user buffer |
| 57 | `dup` | `(oldfd) → i32` | Duplicate fd to lowest free slot |
| 58 | `dup2` | `(oldfd, newfd) → i32` | Duplicate fd to specific slot |

---

## Virtual File System

The VFS is built around the `Vnode` trait — every file-system object implements it:

```rust
pub trait Vnode: Send + Sync {
    fn stat(&self) -> Stat;
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str>;
    fn write_at(&self, offset: usize, buf: &[u8]) -> Result<usize, &'static str>;
    fn lookup(&self, name: &str) -> Result<Arc<dyn Vnode>, &'static str>;
    fn readdir(&self) -> Result<Vec<DirEntry>, &'static str>;
    fn create(&self, name: &str) -> Result<Arc<dyn Vnode>, &'static str>;
    fn mkdir(&self, name: &str) -> Result<Arc<dyn Vnode>, &'static str>;
    fn unlink(&self, name: &str) -> Result<(), &'static str>;
    fn rename(&self, old: &str, new: &str) -> Result<(), &'static str>;
    fn truncate(&self, size: usize) -> Result<(), &'static str>;
}
```

### Backends

| Backend | Mount point | Description |
|---------|-------------|-------------|
| **MemFS** | `/` | Root filesystem from initrd TAR. `RoFile` = zero-copy slice into initrd; `MemFile` = writable `Vec<u8>`; `MemDir` = `BTreeMap<String, Arc<dyn Vnode>>` |
| **ProcFS** | `/proc` | Enumerates running PIDs; each exposes `/proc/<pid>/status` |
| **DevFS** | `/dev` | `/dev/null` (discards reads/writes), `/dev/zero` (zero-filled reads), `/dev/tty` (UART alias) |
| **OFS** | `/mnt` | Optional persistent on-disk filesystem on virtio-blk. Formatted automatically if the disk is blank. |

---

## Process Model

```
  kmain
    └─ posix_spawn("/init") ──► PID 1 (init)
                                  ├─ fork() + exec("/sh") ──► PID 2 (shell)
                                  │    ├─ fork() + exec("/ls") ──► PID 3 (ls)
                                  │    └─ fork() + exec("/cat") ──► PID 4 (cat)
                                  └─ waitpid(-1) ◄─── shell exits → respawn
```

**Lifecycle states:** `Running → Stopped (waitpid block) → Zombie(status) → reaped`

**Scheduler:** priority-based `BTreeMap<u8, VecDeque<Pid>>`. `schedule()` pops from the lowest-numbered (highest-priority) non-empty bucket, switches SATP, calls `return_to_user`. Idles with `wfi` when all buckets are empty.

Priority levels: `PRIO_REALTIME=0`, `PRIO_HIGH=8`, `PRIO_NORMAL=16`, `PRIO_LOW=24`, `PRIO_IDLE=31`. Boot servers (pm-server, vfs-server, rs-server, …) run at `PRIO_HIGH`; all user processes — including driver children of `PRIO_HIGH` servers — always start at `PRIO_NORMAL`, preventing priority inheritance from starving foreground processes.

**Preemption:** The SBI timer fires every 10 ms. The ISR drains the UART FIFO (so characters are never dropped regardless of what the current process is doing), then re-queues the current PID into `RUN_QUEUE` at the process's stored priority. The next call to `schedule()` switches to the highest-priority runnable process.

**COW Fork:**

```
parent: [PTE_R|V → page A]    fork()    child: [PTE_R|V → page A]
                               ──────►
Both writable PTEs cleared; page A refcount = 2.
First write in either process → fault → allocate page B, copy A→B, refcount(A)--
```

---

## Security Model

openv uses standard Unix Discretionary Access Control:

- **9-bit mode** (`rwxr-xr-x`) stored per-vnode
- **check_access()** — compares process `euid`/`egid` against vnode `uid`/`gid`/`mode`
- **Root bypass** — `euid == 0` skips all permission checks
- **setuid-bit** — `exec` sets `euid = file.uid` when `mode & 0o4000`
- **sudo group** (GID 27) — members can call `sys_authenticate` and `sys_setuid(0)` to become root
- **Passwords** — FNV-1a 64-bit hashes (demo quality; replace with a proper KDF before network exposure)

Default users:

| Username | UID | GID | Password | Sudo? |
|----------|-----|-----|----------|-------|
| root | 0 | 0 | `root` | yes |
| guest | 1000 | 1000 | `guest` | yes |

---

## TTY / Line Discipline

The UART terminal supports two modes controlled by `sys_set_raw`:

**Cooked mode (default)** — Characters are buffered in `LINE_DISC_BUFFER` until Enter:
- Regular characters → echo + append to buffer
- `BS` / `DEL` → visual erase + pop from buffer
- `Ctrl-C` (`0x03`) → kill foreground process (or deliver EOF to blocked read if no fg process)
- `Enter` (`\r`/`\n`) → echo `\n`, deliver full line to `read()` caller

**Raw mode** — Every character returned immediately without buffering or echo. Used by the shell for arrow-key history navigation. Characters are accumulated in `LINE_DISC_BUFFER` and consumed one-at-a-time by `sys_read`.

---

## Networking

The networking architecture keeps the kernel thin:

```
user process          net-smoltcp daemon         kernel
    │                       │                      │
    │  sys_socket()         │                      │
    ├──────────────────────────────────────────────►│ creates channel pair
    │◄── fd (user end) ─────────────────────────────┤ queues daemon end
    │                       │  sys_daemon_next_socket()
    │                       ├─────────────────────►│
    │                       │◄── daemon_fd ─────────┤
    │                       │                      │
    │  sys_bind(fd, addr)   │                      │
    ├──────────────────────►│ BIND opcode via channel
    │  sys_listen(fd, n)    │  smoltcp.listen(addr)
    ├──────────────────────►│
    │  sys_accept(fd)       │  sys_daemon_create_conn(sid)
    │  [blocks]             ├─────────────────────►│ delivers new fd to user
    │◄── new_conn_fd ────── ◄─ wake user ──────────┤
```

Raw Ethernet frames flow via `sys_net_send` / `sys_net_recv`. smoltcp handles ARP, IP, TCP/UDP entirely in userspace.

---

## Debugging

**GDB:**

```console
make debug
# In another terminal:
riscv64-unknown-elf-gdb target/riscv64gc-unknown-none-elf/debug/openv
(gdb) target remote :1234
(gdb) b rust_trap_handler
(gdb) continue
```

**Raw UART prints** (work before heap/VMM init):

```rust
use crate::uart::Uart;
let mut u = Uart::new();
u.put_char(b'!');
```

**Panic handler** prints file + line via UART and halts all harts.

---

## CI / CD

GitHub Actions runs on every push and pull request to `main`/`master`:

```
lint (kernel) ──┐
                ├─ [both pass] ──► build ──► artifacts
lint (user)  ──┘
                                    └─► docs ──► GitHub Pages
```

**`lint` job** (parallel matrix — kernel + userspace):
- `cargo check` — type errors, missing modules
- `cargo clippy -D warnings` — all warnings are errors (style lints selectively suppressed via `-A`)

**`build` job** (depends on lint):
- Builds debug + release kernel and all userspace binaries
- Packages `test_root.tar` initrd
- Reports binary sizes in the GitHub Step Summary via `llvm-size`
- Uploads artifacts: `openv-kernel-debug`, `openv-kernel-release`, `initrd`

**`docs` job** (depends on build, runs on push and PR):
- Builds rustdocs with `--document-private-items` and the ayu theme
- Deploys to GitHub Pages (on push only)

**Release bundle** (push to `main`/`master` only):
- `openv-<branch>-<sha>/` — kernel + initrd + `BUILD_INFO.txt`, retained 90 days

---

## Current Status

### Working
- Boot on QEMU `virt` (single-hart and multi-hart `-smp N`)
- UART output, `println!`/`print!` macros
- Physical memory management (free-list + per-page u16 refcounts)
- Sv39 page tables, kernel identity map
- Demand paging (zero-fill on first access)
- COW fork + store-fault page copy
- Timer interrupts (10 ms), preemptive re-queuing, idle WFI
- Process creation, ELF loading, exec, fork, waitpid, zombie reaping
- Round-robin scheduler
- Initrd (UStar tar) → MemFS; ProcFS at `/proc`; DevFS at `/dev`
- VFS: create, mkdir, unlink, rename, readdir, mount table
- Optional persistent OFS filesystem on virtio-blk (auto-format on blank disk)
- File descriptors, dup/dup2, pipes with EOF detection
- IPC channels and bidirectional messaging
- UART line discipline: cooked + raw mode, echo, Ctrl-C SIGINT
- Interactive shell: line editing, arrow-key history, pipelines, I/O redirection
- User/group database, password authentication, sudo
- Virtio-net driver + userspace smoltcp TCP/IP (net-smoltcp daemon)
- SMP startup (secondary harts park on `SMP_GO` flag → wake → scheduler)
- setuid/setgid bits, Unix DAC permission checks
- `set_fg_pid` syscall for correct Ctrl-C delivery to foreground child processes

### Planned
- Full signal subsystem (sigaction, sigprocmask, sigreturn trampoline)
- Job control (process groups, foreground/background, tcsetpgrp)
- `mmap` / `munmap`
- Hard preemption (context switch inside the timer ISR, not at next syscall)
- Porting a real C toolchain (musl libc)

---

## Known Limitations

1. **Preemption is soft** — the timer ISR re-queues the current process; the actual context switch happens at the next return from user space (next syscall or next timer boundary). A tight `loop {}` (no syscalls) will not be preempted mid-instruction; use `sys_yield` or the `spin` binary to test.
2. **Single TTY** — `LINE_DISC_BUFFER` is global, not per-session. Multiple concurrent login sessions on separate virtual TTYs are not supported.
3. **No signal delivery** — Ctrl-C delivers either EOF to a blocked `read` or kills the foreground PID directly. There is no `sigaction`/`kill`/`sigprocmask` ABI.
4. **Password hashing** — FNV-1a 64-bit is used for demo purposes. Do not expose the network interface without replacing it with Argon2 or bcrypt.
5. **OFS has no fsync** — writes to the virtio-blk OFS filesystem are immediately issued but there is no explicit flush/sync syscall.

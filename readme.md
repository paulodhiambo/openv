# openv — RISC-V 64-bit Microkernel OS

openv is a **RISC-V 64-bit microkernel** written in Rust, booting in QEMU with an interactive shell, POSIX process model, VFS, IPC, and preemptive multitasking. It targets the `riscv64gc-unknown-none-elf` bare-metal target and runs on QEMU's `virt` machine.

## Quick Start

```console
$ make build
[1/3] Building userspace...
[2/3] Packaging initrd...
[3/3] Building kernel...

$ make run
Booting openv...
```

You'll be dropped into an interactive shell (`sh`). Type `help` for builtins, `ls` to list files, `cat /dummy.txt` to read.

**Prerequisites:**

- Rust nightly with `riscv64gc-unknown-none-elf` target and `rust-src` component (for `build-std`)
- `qemu-system-riscv64` (QEMU 7+)

```console
rustup toolchain install nightly --component rust-src --target riscv64gc-unknown-none-elf
brew install qemu          # macOS
apt install qemu-system    # Debian/Ubuntu
```

---

## Build System

| Command | What it does |
|---------|-------------|
| `make` | `make build` + `make run` |
| `make build` | userspace → initrd → kernel (debug) |
| `make build-release` | release build of everything |
| `make build-kernel` | kernel only |
| `make build-user` | userspace binaries only |
| `make initrd` | package initrd from existing bins |
| `make run` | boot in QEMU |
| `make debug` | boot with GDB server on :1234 (`-s -S`) |
| `make image` | build `openv.img` disk image |
| `make fmt` | format all Rust source |
| `make clippy` | lint kernel |
| `make check` | `cargo check` kernel |
| `make clean` | remove all build artifacts |

**Variables:**

```console
make BINS="init sh" QEMU_MEM=512M QEMU_CPUS=4 run
```

- `BINS` — which userspace binaries to copy into the initrd (default: `init sh ls cat hello producer consumer doexec forktest net-smoltcp`)
- `QEMU_MEM` — guest memory (default: `128M`)
- `QEMU_CPUS` — number of harts (default: `1`)

**Scripts:**

| Script | Purpose |
|--------|---------|
| `scripts/build.sh` | Full build (kernel + userspace + initrd); accepts `--release` |
| `scripts/run.sh` | Boot in QEMU |
| `scripts/build_image.sh` | Create `openv.img` disk image with kernel + initrd at known offsets |

---

## Project Structure

```
openv/
├── Makefile                  # Top-level build targets
├── linker.ld                 # Kernel linker script (load at 0x80200000)
├── rust-toolchain.toml       # Nightly toolchain pin
├── build_initrd.sh           # (removed — use make or scripts/build.sh)
│
├── src/                      # ── Kernel ──
│   ├── boot.s                # Boot assembly: _start, park secondary harts, set stack, clear BSS
│   ├── main.rs               # kmain entry point, panic handler, __halt_cpu
│   ├── trap.rs               # Trap vector (assembly), syscall dispatch, page faults, timer/IRQ handlers
│   ├── smp.rs                # SMP startup, per-hart stacks, IPI send
│   ├── uart.rs               # NS16550 UART driver, print!/println! macros
│   ├── timer.rs              # SBI timer, 10ms interval
│   ├── plic.rs               # Platform-Level Interrupt Controller (claim/complete)
│   │
│   ├── mm/                   # Memory management
│   │   ├── mod.rs            # mm::init — calls pmm → vmm → heap
│   │   ├── pmm.rs            # Physical MM: DTB-based RAM detection, free-list allocator, refcounts
│   │   ├── vmm.rs            # Virtual MM: Sv39 page tables, kernel identity map, COW fork,
│   │   │                     #   demand paging, destroy_user_space
│   │   ├── heap.rs           # Buddy allocator (buddy_system_allocator) fed from PMM
│   │   └── vmo.rs            # Virtual Memory Object (VMO) backed by PMM pages
│   │
│   ├── vfs/                  # Virtual File System
│   │   ├── mod.rs            # Vnode trait, MountTable with prefix mounts, lookup_path, lookup_parent
│   │   ├── tar.rs            # UStar tar parser — builds MemFS from initrd
│   │   ├── memfs.rs          # MemFile (writable), RoFile (read-only from initrd slice), MemDir
│   │   ├── procfs.rs         # /proc — enumerates PIDs, exposes status file
│   │   └── devfs.rs          # /dev/null, /dev/zero, /dev/tty → UART
│   │
│   ├── posix/                # POSIX process model
│   │   ├── process.rs        # Process struct, PROCESS_TABLE, RUN_QUEUE, schedule(), generate_pid
│   │   ├── spawn.rs          # posix_spawn, exit, sys_fork (COW), sys_exec, cleanup_process
│   │   ├── elf.rs            # ELF loader (goblin) — maps PT_LOAD segments with permissions
│   │   ├── user.rs           # User/group database, authentication (FNV-1a hashing), sudo
│   │   └── wait.rs           # poll_waitpid, waitpid_sync with zombie reaping
│   │
│   ├── ipc/                  # Inter-Process Communication
│   │   ├── channel.rs        # Bidirectional channels: create_pair, write, poll_recv, try_recv
│   │   └── handle.rs         # HandleTable, KernelObject enum, FileDescription, pipe halves
│   │
│   ├── net/                  # Networking (virtio-mmio + smoltcp in userspace)
│   │   ├── mod.rs            # NetDevice trait, register_device, init (probe or loopback fallback)
│   │   ├── core.rs           # Ethernet/ARP/IPv4 header parsing, checksum
│   │   ├── stack.rs          # smoltcp NetworkStack integration
│   │   ├── socket.rs         # Socket registry, pending accepts, opcodes
│   │   ├── pktbuf.rs         # 1536-byte packet buffer
│   │   ├── virtqueue.rs      # Legacy virtqueue descriptor ring
│   │   ├── virtio_mmio.rs    # Virtio-mmio driver: MMIO registers, feature negotiation, queue setup
│   │   └── virtio_net.rs     # Loopback net device fallback
│   │
│   └── drivers/
│       └── mod.rs            # Driver trait, FDT-based probe, interrupt dispatch
│
└── user/                     # ── Userspace ──
    ├── linker.ld             # User-space linker script (load at 0x100000000)
    ├── .cargo/config.toml    # User-space build config
    ├── Cargo.toml            # Workspace
    │
    ├── libos/                # libos — Rust-native POSIX-like syscall library
    │   └── src/lib.rs        # _start, heap, 60+ syscall wrappers, errno
    │
    ├── init/                 # init (PID 1): boot banner, fork test, login prompt, shell respawn
    ├── sh/                   # Shell: line editing, history, pipelines, redirection, builtins, nano editor
    ├── ls/                   # ls — list directory
    ├── cat/                  # cat — read file
    ├── hello/                # hello — test exec'd program
    ├── producer/             # producer — pipe test (sender)
    ├── consumer/             # consumer — pipe test (receiver)
    ├── doexec/               # doexec — exec test
    ├── forktest/             # forktest — fork/waitpid test
    └── net-smoltcp/          # net-smoltcp daemon: smoltcp TCP/IP stack in userspace
```

---

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│  User processes: init, sh, ls, cat, net-smoltcp, ...     │
│      ↓ libos syscall wrappers (ecall)                    │
├──────────────────────────────────────────────────────────┤
│  Kernel trap handler  (trap.rs)                          │
│   ├── Syscall dispatch  (~60 syscalls)                   │
│   ├── Page faults (demand paging + COW)                  │
│   ├── Timer interrupt (preemptive re-queue)              │
│   └── External interrupts (PLIC → drivers)               │
├──────────────────────────────────────────────────────────┤
│  Kernel subsystems                                       │
│   ├── PMM       Physical page allocator + refcounting    │
│   ├── VMM       Sv39 page tables, COW fork, demand paging│
│   ├── VFS       Vnode trait, mount table, MemFS/DevFS    │
│   ├── POSIX     Process table, scheduler, ELF loader     │
│   ├── IPC       Channels, handle table, pipes            │
│   └── NET       Virtio-mmio driver, socket registry      │
├──────────────────────────────────────────────────────────┤
│  Hardware (QEMU virt, RISC-V Sv39)                       │
│   UART × PLIC × Timer (SBI) × Virtio-MMIO               │
└──────────────────────────────────────────────────────────┘
```

### Key Design Decisions

**Microkernel in kernel space.** The kernel provides MM, IPC, and trap dispatch. POSIX semantics (process lifecycle, VFS, signals) are in the kernel for v1 pragmatism, not purity. A strictly Fuchsia-style split (VFS server, process server in userspace) is feasible but deferred.

**Non-reentrant spinlocks.** Kernel locks use `spin::Mutex`, which is not reentrant. Timer interrupts therefore only re-queue the current process — they never call `schedule()` from interrupt context to avoid deadlocks on the process table. Preemption happens at syscall boundaries.

**Capability-like handles.** File descriptors are `HandleTable` entries storing `KernelObject` variants (Console, Channel, File, PipeRead/Write, Vmo). `dup`/`dup2` clone the handle. Drop closes the handle and frees the resource.

**COW fork + demand paging.** `sys_fork` clones the address space by clearing `PTE_W` on all writable user pages in both parent and child (shared read-only with refcounts). The first write by either traps to `handle_store_page_fault` which allocates a private copy. Instruction/load faults allocate zero-filled pages on demand.

**Initrd-based root filesystem.** A tar archive (`test_root.tar`) is loaded by OpenSBI and passed to the kernel via the DTB `chosen` node. The kernel parses it into a MemFS tree. `/proc` (ProcFS) and `/dev` (DevFS) are mounted on top.

---

## Subsystems

### Memory Management (`src/mm/`)

| Component | Description |
|-----------|-------------|
| **PMM** | Detects RAM from DTB (`/memory` reg property). Builds a free list avoiding kernel image, FDT, and initrd ranges. `alloc_page`/`free_page` with a `[u16; 262144]` refcount table covering up to 1 GB. |
| **VMM** | Sv39 page tables (3-level, 4 KB pages). Kernel identity maps the first 4 GB via 1 GB superpages. `map_page` auto-allocates intermediate levels. `clone_user_space` copies user page tables COW. `destroy_user_space` walks and decrements refs. |
| **Heap** | `buddy_system_allocator` fed from PMM pages. Handles non-contiguous PMM segments (gaps from FDT/initrd holes). |
| **VMO** | Contiguous virtual memory object backed by physical pages — basic building block for shared memory. |

### Virtual File System (`src/vfs/`)

**`Vnode` trait** — the core abstraction:

```rust
trait Vnode: Send + Sync {
    fn stat(&self) -> Stat;
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize>;
    fn write_at(&self, offset: usize, buf: &[u8]) -> Result<usize>;
    fn create(&self, name: &str) -> Result<Arc<dyn Vnode>>;
    fn mkdir(&self, name: &str) -> Result<Arc<dyn Vnode>>;
    fn lookup(&self, name: &str) -> Result<Arc<dyn Vnode>>;
    fn readdir(&self) -> Result<Vec<DirEntry>>;
    fn unlink(&self, name: &str) -> Result<()>;
    fn rename(&self, old_name: &str, new_name: &str) -> Result<()>;
    fn truncate(&self, size: usize) -> Result<()>;
}
```

**MountTable** with longest-prefix matching — `lookup_path("/proc/self/status")` resolves against `/proc` before falling through to the root MemFS.

**Backends:**
- **MemFS** — `MemFile` (writable, `Vec<u8>`), `RoFile` (read-only view into initrd memory, zero-copy), `MemDir` (`BTreeMap<String, Arc<dyn Vnode>>` with create/mkdir/unlink/rename)
- **ProcFS** — `/proc` enumerates PIDs; each PID has a `status` file with pid/ppid/state/uid
- **DevFS** — `/dev/null`, `/dev/zero`, `/dev/tty` (UART console alias)

### POSIX Processes (`src/posix/`)

**Process struct:**
```rust
struct Process {
    pid, ppid, pgid, sid,                 // Identifiers
    uid, gid, euid, egid,                // Credentials (AtomicU32)
    state: Mutex<ProcState>,             // Running | Stopped | Zombie(i32)
    fds: Mutex<HandleTable>,             // File descriptor table
    children: Mutex<Vec<Pid>>,           // Child list
    trap_frame: Mutex<TrapFrame>,        // Saved registers
    satp_val: usize,                     // Page table root
    kernel_stack_pages: [usize; 4],      // 4 × 4KB kernel stacks
    cwd: Mutex<String>,                  // Current working directory
    wait_target/status_ptr/result,       // waitpid state
}
```

**Scheduler** — simple FIFO round-robin via `RUN_QUEUE: VecDeque<Pid>`. `schedule()` pops the front, checks state is `Running`, switches page tables, and calls `return_to_user`. Idles with WFI when the queue is empty.

**Lifecycle:**
1. `Process::new(ppid)` — allocates page table, 4 kernel stack pages, inherits fds/credentials from parent, inserts into `PROCESS_TABLE`
2. `posix_spawn(path, ppid)` — `Process::new` + ELF load + user stack + trap frame setup → pushes to `RUN_QUEUE`
3. `exit(pid, status)` — marks zombie, orphans children to init, wakes parent
4. `waitpid` — reaps zombie, frees kernel stacks and user pages, removes from table

### IPC (`src/ipc/`)

**Channels** — bidirectional message queues. `create_pair()` returns two `ChannelEndpoint`s. `write(msg)` enqueues; `try_recv()` dequeues. Peer-drop signals EOF. Used for socket proxying between net-smoltcp daemon and user processes.

**HandleTable** — per-process open-file table. `KernelObject` variants:
- `Console` — UART I/O
- `Channel(Endpoint)` — IPC channel
- `File(Arc<FileDescription>)` — VFS file with offset
- `PipeRead(Arc<PipeHalf>)` / `PipeWrite(Arc<PipeHalf>)` — pipe end

**Pipes** — bounded byte buffers (`VecDeque<u8>`) with write-end tracking via `Weak` reference for EOF detection.

### Networking (`src/net/`)

**Architecture:** Kernel provides a virtio-mmio driver and a socket registry. The actual TCP/IP stack runs in userspace (`net-smoltcp` binary) using smoltcp. Users create sockets via `sys_socket` which creates a channel pair — one end goes to the user's fd table, the other is queued for the net daemon.

**Virtio-mmio driver:** Legacy interface with single virtqueue. MMIO register access, feature negotiation (zero features), descriptor ring. Interrupt-driven RX with `RX_QUEUE` of physical page addresses.

---

## Syscalls

| # | Name | Args | Description |
|---|------|------|-------------|
| 0 | `yield` | — | Yield CPU |
| 1 | `exit` | status | Terminate process |
| 2 | `write` | fd, buf, len | Write to fd |
| 3 | `read` | fd, buf, max | Read from fd |
| 5 | `pipe` | &[u32; 2] | Create pipe, return [read_fd, write_fd] |
| 6 | `spawn` | path, len | Spawn child process |
| 8 | `open` | path, len, flags | Open file, return fd |
| 9 | `close` | fd | Close fd |
| 10 | `net_send` | buf, len | Send raw Ethernet frame |
| 11 | `net_recv` | buf, max | Receive raw Ethernet frame |
| 12 | `getdents` | path, len, buf, max | List directory entries |
| 23 | `setuid` | uid | Set user ID |
| 24 | `setgid` | gid | Set group ID |
| 26 | `create` | path, len | Create/truncate file |
| 27 | `mkdir` | path, len | Create directory |
| 28 | `unlink` | path, len | Remove file |
| 29 | `rename` | old, old_len, new, new_len | Rename file |
| 30-33 | `getuid/geteuid/getgid/getegid` | — | Credential queries |
| 34 | `authenticate` | user, user_len, pass, pass_len | Login authentication |
| 35 | `can_sudo` | uid | Sudo privilege check |
| 37 | `set_echo` | enabled | Toggle UART echo |
| 38 | `set_raw` | enabled | Toggle raw mode |
| 40-48 | socket syscalls | — | Socket/bind/listen/connect/send/recv |
| 50 | `fork` | — | COW fork |
| 51 | `exec` | path, len | Replace process image |
| 52 | `waitpid` | target, status_ptr, opts | Wait for child |
| 53 | `getpid` | — | Get PID |
| 54 | `getppid` | — | Get parent PID |
| 55 | `chdir` | path, len | Change directory |
| 56 | `getcwd` | buf, max | Get current directory |
| 57 | `dup` | oldfd | Duplicate fd |
| 58 | `dup2` | oldfd, newfd | Duplicate fd to target |

---

## Current Status

### Working
- Boots in QEMU (`virt` machine, single hart)
- UART output, `println!`/`print!` macros
- Physical memory management (free-list + refcounts)
- Sv39 page tables, kernel identity map (1 GB superpages)
- Demand paging (lazy page allocation on user access)
- COW fork + `handle_store_page_fault`
- Timer interrupts (10 ms, preemptive re-queue)
- Process creation, ELF loading, spawning
- Round-robin scheduler with idle WFI
- Initrd (tar) → MemFS parsing
- VFS with mount table, lookup, create, mkdir, unlink, rename
- ProcFS (/proc/PID/status) and DevFS (/dev/null, /dev/zero, /dev/tty)
- File descriptors, dup/dup2
- IPC channels, bidirectional
- Pipes (PipeRead/PipeWrite with EOF detection)
- UART line discipline (canonical + raw mode, echo, Ctrl-C)
- Shell with line editing, history, pipelines, redirection, builtins
- User/group database, authentication, sudo
- Virtio-net driver (userspace smoltcp stack)
- SMP startup (secondary harts park → spin → go flag)

### In Progress
- Preemptive scheduling from timer interrupt (currently re-queues only — schedule happens at syscall boundaries)
- Page table entry cleanup in `destroy_user_space`

### Not Yet
- Signals (SIGINT via Ctrl-C works ad-hoc in line discipline, no full signal delivery)
- Job control (process groups, foreground/background)
- Block device driver (virtio-blk) for persistent storage
- On-disk filesystem (currently initrd-only, no write persistence across reboot)

---

## Debugging

The kernel uses raw UART debug prints via `crate::uart::write_str`, `write_dec`, `write_hex` and the `crate::raw_print!` macro (bypasses `format_args!`). These work even when the heap or VMM is not initialized.

To examine trap state on a crash, the panic handler prints the message, file, and line via raw UART output.

For GDB debugging:
```console
make debug
# In another terminal:
riscv64-unknown-elf-gdb target/riscv64gc-unknown-none-elf/debug/openv
(gdb) target remote :1234
(gdb) continue
```

---

## CI

GitHub Actions runs on push/PR to `main`/`master`:

- **`quality`** — `cargo fmt --check` (kernel + userspace), `cargo clippy -- -D warnings`
- **`build`** — debug + release build of kernel and all userspace binaries, verifies ELF with `file`/`size`, uploads artifacts

Caching is configured for `~/.cargo/registry`, `~/.cargo/git`, `target/`, and `user/target/`.

---

## Design Notes

**Why a single-address-space kernel identity map?** The kernel identity-maps the first 4 GB of physical address space with 1 GB superpages (`PTE_R | W | X`). This gives the kernel direct access to all RAM, MMIO regions (UART at 0x10000000, virtio at 0x10008000, PLIC at 0xC000000), and the initrd without page table manipulation during boot. It also means every user process inherits this mapping (no `PTE_U`), so trap handling never needs a page table switch.

**Why `build-std`?** The kernel uses `#![no_std]` and `build-std = ["core", "alloc", "compiler_builtins"]` to compile `core` and `alloc` from source. This is required because nightly `riscv64gc-unknown-none-elf` ships a prebuilt `core` whose `Arguments` layout may not match the compiler's — using `build-std` ensures the `format_args!` ABI is consistent.

**Why no dynamic linking?** v1 uses static linking for all userspace binaries. A dynamic linker/loader would require significant infrastructure (ELF dynamic sections, `ld.so`, relocations at load time) with limited benefit for an embedded-style system.

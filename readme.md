## Implementation Strategy — Version 1: Unix-Like CLI System on a Fuchsia-Scaled RISC-V Microkernel (Rust)

## What "Version 1" Means

Version 1 is a **single-machine, command-line operating system** that boots in QEMU and drops you into an interactive shell that behaves like `bash`. Success is concrete and testable: you can type `ls -la | grep foo > out.txt`, hit `Ctrl-C` to kill a runaway process, suspend with `Ctrl-Z`, run a job in the background with `&`, and have it all work the way a Unix user expects.

This reframes the original phase plan. The microkernel/capability foundation (Phases 1–4 of the architecture doc) is still the substrate, but the **Unix personality layer** — VFS, file descriptors, signals, job control, process groups, a TTY line discipline — becomes the headline deliverable rather than an afterthought. The capability model doesn't disappear; it sits *underneath* the POSIX API, which is exactly how a microkernel hosts a Unix environment.

Two things are explicitly **out of scope** for v1: networking (no netstack, no sockets — defer entirely) and graphics (no display, no windowing — serial console only). Cutting these removes enormous surface area and lets v1 ship.

---

## The Core Architectural Decision: Where Does Unix Live?

You have a microkernel with capabilities, channels, and handles. Unix has file descriptors, signals, and a global filesystem namespace. These are different worlds, and how you bridge them is the single most important v1 decision.

The recommended model: a **userspace "Unix server"** (call it `posixd`) that owns Unix semantics, with `libos` translating POSIX calls into either direct kernel syscalls or IPC to `posixd`.

```
┌─────────────────────────────────────────┐
│  shell, coreutils (ls, cat, grep...)     │  ← ordinary processes
│      ↓ POSIX calls via libc/libos        │
├─────────────────────────────────────────┤
│  libos: fd table, errno, POSIX shims     │  ← linked into each process
│      ↓ channels (capabilities)           │
├─────────────────────────────────────────┤
│  posixd: VFS, process table, signals,    │  ← the "Unix kernel," in userspace
│          job control, TTY discipline     │
│      ↓ kernel syscalls                   │
├─────────────────────────────────────────┤
│  microkernel: VMOs, channels, handles,   │  ← from the architecture doc
│               scheduler, page tables     │
└─────────────────────────────────────────┘
```

A **file descriptor is a capability handle plus a Unix-visible integer index.** When a process opens `/etc/motd`, `libos` asks `posixd` (or the relevant filesystem server) over a channel; it gets back a channel handle to the open file; it installs that handle in the per-process fd table at the lowest free integer. `read(fd, ...)` becomes a channel transaction. This is the bridge: Unix's "everything is a file descriptor" maps cleanly onto "everything is a handle," and the kernel's capability enforcement gives you isolation for free.

This is essentially how Fuchsia runs POSIX software (its `fdio` layer), so you're following a proven pattern rather than inventing one.

---

## Revised Phase Plan for v1

Phases 0–3 from the architecture doc are unchanged prerequisites (toolchain, kernel core, memory/VMO, IPC). The version-1 work is concentrated in what were Phases 4 and 6, now expanded and reordered around the Unix goal.

### Prerequisite Phases (condensed from architecture doc)

**P0 Toolchain · P1 Kernel core · P2 Memory + VMO · P3 IPC.** Build these as specified. The only v1-specific adjustment: in **P5 (drivers)**, you need exactly *two* drivers — **virtio-console** (your terminal I/O) and **virtio-block** (your disk for the filesystem). Skip virtio-net entirely. Build both in-kernel initially for debugging, as the architecture doc advises; migrating them to userspace can wait for v2.

The rest of this document details the new v1-centric phases.

---

### V1-Phase A: Process Model & Lifecycle

**Goal:** `fork`-like spawn, `exec`, `wait`, `exit` with correct parent/child semantics.

A pure microkernel doesn't want `fork()` (copying an entire address space is hostile to the capability model and to COW-everything purity). The pragmatic v1 answer: implement **`posix_spawn` semantics natively** and emulate `fork`+`exec` in `libos` for shell compatibility. The shell almost always does `fork` immediately followed by `exec`, so you implement that fused path efficiently and treat raw `fork()` (long-lived forked children that never exec) as a best-effort COW clone.

Deliverables:
- **Process object** in `posixd`: PID, parent PID, process group ID (PGID), session ID (SID), fd table, cwd, umask, exit status, child list.
- **`spawn(path, argv, envp, fd_actions)`** — creates address space (kernel), loads ELF (`goblin`), sets up the initial fd table by transferring handles, starts the main thread. `fd_actions` describes dup/close/redirect so the shell can wire up pipes *before* exec.
- **`waitpid`** — blocks (async `.await` under the hood) until a child changes state; reaps zombies; returns encoded status (exit code, signal, stop/continue).
- **Zombie & orphan handling** — dead process becomes a zombie until reaped; orphans reparent to `init` (PID 1).

```rust
// posixd process table entry
struct Process {
    pid: Pid, ppid: Pid, pgid: Pgid, sid: Sid,
    state: ProcState,          // Running | Stopped | Zombie(ExitStatus)
    fds: FdTable,              // index → (handle, flags like CLOEXEC)
    cwd: VfsPath,
    umask: Mode,
    children: Vec<Pid>,
    pending_signals: SigSet,
    signal_handlers: [SigAction; NSIG],
    process_group: Pgid,
}
```

**Milestone:** Shell spawns `/bin/true` and `/bin/false`, `waitpid`s each, and reports correct exit codes.

---

### V1-Phase B: Virtual Filesystem (VFS)

**Goal:** A unified `/` namespace, mountable backends, the file operations every Unix tool assumes.

The VFS is the heart of "everything is a file." In v1 it lives in `posixd` (or a dedicated `vfsd` if you want cleaner separation — recommend keeping it in `posixd` for v1 to reduce IPC chatter).

Deliverables:
- **VFS core**: path resolution (handling `.`, `..`, symlinks, mount crossings), inode/vnode abstraction, the dentry-style lookup cache.
- **vnode operations trait**:
  ```rust
  trait VfsNode {
      fn lookup(&self, name: &str) -> Result<Arc<dyn VfsNode>>;
      fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize>;
      fn write_at(&self, off: u64, buf: &[u8]) -> Result<usize>;
      fn readdir(&self, off: u64) -> Result<Vec<DirEntry>>;
      fn stat(&self) -> Result<Stat>;
      fn truncate(&self, len: u64) -> Result<()>;
      // create, unlink, rename, symlink, mkdir, rmdir...
  }
  ```
- **Three backends for v1**:
    1. **tmpfs** — VMO-backed, RAM-only. Your `/tmp`, and the root before you mount disk. Easiest; build first.
    2. **A real on-disk filesystem** — recommend **ext2** (simple, well-documented, no journaling complexity) over ext4. Read *and* write. This is what `virtio-block` serves. A pre-built ext2 image baked at build time gives you `/bin`, `/etc`, etc.
    3. **devfs** — synthetic `/dev` exposing `/dev/console`, `/dev/null`, `/dev/zero`, `/dev/tty`. These are the device files the shell and coreutils poke at.
- **Mount table**: `mount(backend, at_path)`; path resolution crosses mount points.
- **Standard syscalls**: `open, close, read, write, lseek, stat, fstat, lstat, getdents, mkdir, rmdir, unlink, rename, symlink, readlink, chdir, getcwd, dup, dup2, fcntl, ioctl, truncate, fsync`.

A practical sequencing tip: get tmpfs + devfs working and mount tmpfs as `/` first. You can build the *entire* shell and most coreutils against a RAM filesystem, deferring the ext2 driver until the rest of userland is proven. Then mount ext2 and the same binaries "just work" against persistent storage.

**Milestone:** `ls /`, `cat /etc/motd`, `mkdir /tmp/x && cd /tmp/x && pwd`, and writing then re-reading a file all behave correctly across tmpfs and ext2.

---

### V1-Phase C: TTY, Line Discipline & the Console

**Goal:** A terminal that feels like a terminal — line editing, echo, control characters, canonical vs raw mode.

This is the layer that makes the shell usable and is frequently underestimated. The TTY subsystem sits between the virtio-console driver and processes.

Deliverables:
- **TTY device** in `posixd` (or a small `ttyd`), exposed at `/dev/console` and `/dev/tty`.
- **Line discipline (canonical mode)**: buffers input by line; handles backspace/erase, `Ctrl-U` (kill line), `Ctrl-W` (erase word); echoes typed characters; delivers a full line on Enter. This is what lets users edit before pressing return.
- **Raw / non-canonical mode**: byte-at-a-time, no echo, no line editing — required by anything doing its own input handling (and by your shell's line editor if you write a fancy one).
- **Control-character → signal mapping**: `Ctrl-C` → `SIGINT`, `Ctrl-Z` → `SIGTSTP`, `Ctrl-\` → `SIGQUIT`, delivered to the **foreground process group** (this is where job control and the TTY meet).
- **`termios`**: `tcgetattr`/`tcsetattr` so programs can flip modes; `TCGETS`/`TCSETS` ioctls.
- **Window size**: stub `TIOCGWINSZ` to a fixed 80×24 for v1 (no resize events over serial).

The control-character path is the crux: when the TTY sees `0x03` (`Ctrl-C`), it must know which process group is in the foreground (set via `tcsetpgrp`) and deliver `SIGINT` to every process in it. Get this right and Ctrl-C "just works"; get it wrong and nothing feels like Unix.

**Milestone:** At an early shell prompt, typing with backspace edits the line; Enter submits it; `Ctrl-C` interrupts a running `sleep`.

---

### V1-Phase D: Signals

**Goal:** POSIX signal delivery, handlers, masking, and the default actions.

Signals are how Unix does asynchronous notification, and the shell depends on them heavily (job control is *built* on signals).

Deliverables:
- **Signal generation**: from `kill(pid, sig)`, from the TTY (Ctrl-C/Z), from the kernel (`SIGSEGV` on a fatal page fault, `SIGFPE`, `SIGPIPE` on writing to a closed pipe), from `alarm` (`SIGALRM`).
- **Delivery & default actions**: terminate, terminate+core (skip core dumps in v1 — just terminate), ignore, stop, continue. Implement the standard defaults per signal.
- **Handlers**: `sigaction`/`signal`; set up a user-space trampoline so a delivered signal runs the handler on the user stack and returns via `sigreturn`. This requires kernel cooperation: on return to U-mode for a process with a pending unblocked signal, the kernel (or `posixd` via a "deliver signal" mechanism) rewrites the user context to enter the handler.
- **Masking**: `sigprocmask`, per-thread pending sets, `SA_RESTART` semantics for interrupted syscalls.
- **Process-group signaling**: `kill(-pgid, sig)` and TTY-sourced signals fan out to a whole group.

Mechanism note: because signals can target a process blocked in an IPC `.await` inside `posixd`, your channel-wait primitive must be cancellable/interruptible. Build interruptibility into the wait from the start — retrofitting it is painful. A blocked `read` that receives `SIGINT` must abandon the wait and return `EINTR`.

**Milestone:** A process installs a `SIGINT` handler and prints a message instead of dying; another process ignores `SIGINT`; `kill -9` terminates regardless.

---

### V1-Phase E: Pipes, Redirection & Job Control

**Goal:** The shell plumbing — `|`, `>`, `<`, `>>`, `2>&1`, `&`, `fg`, `bg`, `Ctrl-Z`.

This phase ties A–D together into shell behavior.

Deliverables:
- **Pipes**: `pipe()` returns two fds backed by an in-kernel (or `posixd`) bounded byte buffer with proper blocking, EOF on all-writers-closed, and `SIGPIPE`/`EPIPE` on all-readers-closed. A pipe is just another channel-backed object exposed as two fds.
- **Redirection**: implemented entirely in the shell using `open` + `dup2` + `close` between fork and exec — no new kernel mechanism needed, which is the beauty of getting fds right in Phase B.
- **Process groups & sessions**: `setsid`, `setpgid`, `getpgid`. The shell creates a new process group per pipeline.
- **Foreground/background**: `tcsetpgrp` hands TTY ownership to a process group; the shell reclaims it when the job stops or finishes.
- **Job control**: `Ctrl-Z` → `SIGTSTP` stops the foreground group; shell's `bg` sends `SIGCONT` and leaves it in the background; `fg` sends `SIGCONT` and reattaches it to the terminal. The shell tracks jobs and reports state changes via `waitpid` with `WUNTRACED`/`WCONTINUED`.

**Milestone:** `cat | grep foo` works as a pipeline; `ls > out.txt` redirects; `sleep 100 &` backgrounds; `Ctrl-Z` then `bg` then `fg` cycle a job through stopped/background/foreground.

---

### V1-Phase F: The Shell & Coreutils

**Goal:** An interactive `bash`-like shell and enough utilities to be genuinely usable.

**Shell strategy — two options:**

The fastest credible path is to **port an existing shell** rather than write one. Options, in order of pragmatism:
- **Port `busybox`'s `ash`** (or `dash`) by providing enough libc. This gives you a POSIX `sh` with job control essentially for free, but requires a fairly complete libc — pushing you toward a real musl/newlib port.
- **Port a Rust shell** like `brush` (a bash-compatible shell written in Rust) or build on `nushell` components — easier to compile against your `libos` since it's Rust-native, no C libc needed.
- **Write your own** minimal shell. Most control, least compatibility. Reasonable for v1 if you keep it to: command parsing, pipelines, redirection, variable expansion, globbing, job control builtins, and a line editor.

Recommendation for v1: **write a focused Rust shell** against `libos`. You avoid the libc-completeness rabbit hole, and a shell that does exactly the Phase E features well beats a half-ported bash. Add `readline`-style editing (history with up/down, `Ctrl-R` search) using the raw TTY mode from Phase C.

**Shell must support:**
- Command execution with `PATH` lookup, pipelines, all redirection forms.
- Variables and environment (`FOO=bar`, `export`, `$FOO`, `${FOO:-default}`), `$?`, `$$`, `$!`.
- Globbing (`*`, `?`, `[...]`), tilde expansion, command substitution `$(...)`.
- Quoting (single, double, escapes), here-documents (`<<EOF`).
- Control flow: `if`/`then`/`else`/`fi`, `for`, `while`, `case`, `&&`, `||`, `;`.
- Builtins: `cd`, `pwd`, `export`, `unset`, `exit`, `jobs`, `fg`, `bg`, `kill`, `echo`, `test`/`[`, `alias`, `source`.

**Coreutils** — the minimum set that makes the system feel real:

The cleanest path is to bring in a Rust coreutils implementation rather than hand-writing each. **`uutils/coreutils`** (the Rust reimplementation of GNU coreutils) is the obvious candidate; many of its tools are `no_std`-hostile but compile against a `std` port. For v1, hand-writing a focused subset against `libos` may again be simpler than a full `std` port. Target: `ls, cat, cp, mv, rm, mkdir, rmdir, ln, pwd, echo, touch, head, tail, wc, grep, sort, uniq, cut, tr, sed (basic), find, chmod, stat, du, df, ps, kill, env, date, sleep, true, false, yes, clear, mount`.

**Milestone (the v1 release gate):** Boot to a shell prompt over serial. Run an interactive session including pipelines, redirection, job control, scripting (`if`/`for`/`while`), and a dozen-plus coreutils, against a persistent ext2 filesystem. Run a shell script from a file. Survive `Ctrl-C` on a hung command without taking down the shell.

---

## libc / libos Strategy

This decision pervades every phase above, so make it early.

**v1 recommendation: a Rust-native `libos`, not a full C libc.** Provide POSIX *semantics* through a Rust crate that all userland (shell, coreutils) links against. This sidesteps porting musl/newlib — a large project in itself — and keeps everything in one language with one allocator.

```rust
// libos surface (selected)
pub fn open(path: &CStr, flags: OFlags, mode: Mode) -> Result<Fd>;
pub fn read(fd: Fd, buf: &mut [u8]) -> Result<usize>;
pub fn write(fd: Fd, buf: &[u8]) -> Result<usize>;
pub fn dup2(old: Fd, new: Fd) -> Result<Fd>;
pub fn pipe() -> Result<(Fd, Fd)>;
pub fn spawn(path: &CStr, argv: &[&CStr], envp: &[&CStr],
             actions: &[FdAction]) -> Result<Pid>;
pub fn waitpid(pid: Pid, opts: WaitOpts) -> Result<(Pid, Status)>;
pub fn sigaction(sig: Signal, act: &SigAction) -> Result<SigAction>;
pub fn tcsetpgrp(fd: Fd, pgid: Pgid) -> Result<()>;
```

The escape hatch: if you decide to port real C programs (busybox, GNU tools) later, you add a thin C ABI shim over `libos` and a `*-unknown-myos` Rust target with `std` support. v1 doesn't need it.

---

## Revised Cross-Cutting Recommendations for v1

| Area | v1 Recommendation |
|------|-------------------|
| **Scope cuts** | No networking, no graphics, no SMP (single-hart first), no dynamic linking, no multi-arch. Defer all to v2+. |
| **fork** | Emulate via fused `spawn`; treat true long-lived `fork` as best-effort COW. Don't let `fork` purity block you. |
| **Filesystem** | tmpfs first (build all userland against it), then ext2 read/write for persistence. Skip journaling. |
| **libc** | Rust-native `libos`; defer full C libc until you must run C programs. |
| **Shell** | Write a focused Rust shell against `libos`; don't get stuck porting bash. |
| **Interruptible IPC** | Build cancellable channel waits from day one — signals (`EINTR`) depend on it. |
| **Drivers** | Only virtio-console + virtio-block; in-kernel is fine for v1. |
| **Concurrency** | Single hart simplifies signals, scheduling, TTY ownership enormously. Add SMP in v2. |
| **Testing** | Golden-transcript tests: feed scripted input to the serial console, diff against expected output. This catches shell/TTY/signal regressions cheaply. |

---

## Critical Path for v1

The dependency chain that gates the release:

**P0–P3 (kernel substrate) → A (processes) → B (VFS) → C (TTY) → D (signals) → E (pipes/jobs) → F (shell).**

Within that, three things are both high-risk and load-bearing; prototype them before committing to the full build:

1. **The fd-as-handle bridge** (decided in the architecture-doc Phase 3/4 work). Everything in B, C, E depends on file descriptors being capability handles with a clean integer-index layer on top. Validate this with a toy `open`/`read`/`dup2` before building the VFS.

2. **Interruptible blocking.** Signals, `Ctrl-C`, and job control all require that a process blocked in `posixd` can be woken and made to return `EINTR`. If your IPC wait can't be cancelled, signals don't work. Settle this in Phase 3's executor design.

3. **Foreground-process-group tracking.** The TTY → signal → process-group path (C + D + E) is where "it feels like Unix" is won or lost. Build a minimal end-to-end slice — TTY delivers `SIGINT` to the foreground group — as early as Phase C, even before the full shell exists, using a stub foreground group.

Get those three right and the rest is a (large but) straightforward matter of filling in syscalls and utilities.

---
# Contributing to openv

Thank you for your interest in contributing! openv is a RISC-V 64-bit microkernel OS written in Rust, and contributions of all kinds are welcome — bug fixes, new syscalls, driver ports, documentation improvements, and tests.

## Table of Contents

1. [Development Environment](#1-development-environment)
2. [Project Layout](#2-project-layout)
3. [Build & Run](#3-build--run)
4. [Coding Standards](#4-coding-standards)
5. [Adding a Syscall](#5-adding-a-syscall)
6. [Adding a VFS Backend](#6-adding-a-vfs-backend)
7. [Adding a Driver](#7-adding-a-driver)
8. [Writing Tests](#8-writing-tests)
9. [Submitting Changes](#9-submitting-changes)
10. [Code Review Checklist](#10-code-review-checklist)
11. [Known Issues to Fix](#11-known-issues-to-fix)

---

## 1. Development Environment

### Required tools

| Tool | How to get |
|------|------------|
| Rust nightly | `rustup toolchain install nightly` |
| `riscv64gc-unknown-none-elf` target | `rustup target add riscv64gc-unknown-none-elf --toolchain nightly` |
| `rust-src` component | `rustup component add rust-src --toolchain nightly` |
| `rustfmt` + `clippy` | `rustup component add rustfmt clippy --toolchain nightly` |
| `qemu-system-riscv64` | `brew install qemu` (macOS) · `apt install qemu-system-riscv64` (Debian/Ubuntu) |
| `riscv64-unknown-elf-gdb` | Optional, for GDB debugging |

### One-liner setup

```console
rustup toolchain install nightly \
  --component rust-src,rustfmt,clippy,llvm-tools-preview \
  --target riscv64gc-unknown-none-elf
```

The `rust-toolchain.toml` in the repo root pins the exact nightly version; `rustup show` will install it automatically.

### Editor setup

For VS Code / RustRover: the `rust-analyzer` extension works well. Point it at the workspace root. Note that the kernel (`src/`) and userspace (`user/`) are separate Cargo workspaces — you may need to open them separately or configure `rust-analyzer.linkedProjects`.

---

## 2. Project Layout

```
openv/
├── src/              Kernel source (Cargo workspace root)
│   ├── boot.s        Assembly entry point
│   ├── main.rs       kmain, panic handler
│   ├── trap.rs       Syscall dispatch, page fault handlers, IRQ handlers
│   ├── mm/           Memory management (PMM, VMM, heap, VMO)
│   ├── vfs/          Virtual file system (Vnode trait + backends)
│   ├── posix/        Process model, ELF loader, credentials
│   ├── ipc/          Channels, handle table, pipes
│   ├── net/          Networking (virtio driver + socket registry)
│   └── drivers/      Device driver framework
├── user/             Userspace (separate Cargo workspace)
│   ├── libos/        POSIX shim: _start, heap, syscall wrappers
│   ├── init/         PID 1 init process
│   ├── sh/           Interactive shell
│   ├── ls/ cat/ …    Coreutils
│   └── net-smoltcp/  Userspace TCP/IP daemon
├── docs/             Documentation
├── scripts/          Build & run helper scripts
├── .github/          CI/CD (GitHub Actions)
├── linker.ld         Kernel linker script
└── Makefile
```

---

## 3. Build & Run

```console
# Full debug build + boot in QEMU
make

# Build only (no QEMU)
make build

# Release build
make build-release

# Run with more memory
make QEMU_MEM=512M run

# GDB session
make debug
# In another terminal:
riscv64-unknown-elf-gdb target/riscv64gc-unknown-none-elf/debug/openv
(gdb) target remote :1234
(gdb) continue
```

### Lint before pushing

```console
# Kernel
cargo fmt --check
cargo clippy -- -D warnings -A clippy::missing_safety_doc

# Userspace
cd user
cargo fmt --check
cargo clippy -- -D warnings -A clippy::missing_safety_doc
```

The CI pipeline enforces `-D warnings` (all warnings are errors). Fix all warnings before submitting.

---

## 4. Coding Standards

### Rust style

- Format with `rustfmt` (run `cargo fmt`).
- All `clippy` warnings must be resolved — the CI treats them as errors.
- Prefer `?` over `unwrap()` in fallible functions. Reserve `unwrap()` for genuinely invariant conditions; document why with a comment.
- Do not use `panic!` in interrupt context or inside a `spin::Mutex` guard — panics in those contexts can deadlock or corrupt state.

### Safety

- Every `unsafe` block must have a `// SAFETY:` comment explaining why the operation is sound.
- Avoid raw pointer casts to non-atomic types across threads. The code review flagged `ppid` and `satp_val` as existing violations — do not add more.
- Do not add `static mut` globals without wrapping them in `Mutex` or making them `Atomic*` types.

### Locks

- Use `spin::Mutex<T>` for all shared mutable state.
- **Never call `schedule()` while holding any `spin::Mutex`.** `schedule()` calls `return_to_user` which is a diverging function — any held guard would never be unlocked.
- Drop all locks before calling `schedule()`, `exit()`, or `__halt_cpu()`.
- The pattern is always:
  ```rust
  let data = { GLOBAL.lock().get(&key).cloned() }; // lock released here
  // use data without holding the lock
  ```

### Error handling

- Kernel functions that can fail return `Result<T, &'static str>`. Use descriptive static strings: `Err("OOM allocating page table")`.
- Syscalls return `usize::MAX` (=-1 as i32) on error. Prefer named constants in the future; for now match the existing pattern.
- Never silently swallow errors in syscall handlers — at minimum `crate::println!` the error before returning `-1`.

### Memory

- Call `alloc_page()` for all physical page allocations. Never directly dereference `0x8xxxx` addresses unless you have established they are valid PMM-allocated pages.
- Call `incr_ref` / `decr_ref` consistently whenever you share or release a physical page between page tables.
- In `destroy_user_space`, always skip indices 0–3 of the root page table (kernel superpages).

---

## 5. Adding a Syscall

Adding a new syscall involves changes in three places:

### Step 1 — Kernel: `src/trap.rs`

Find the `match syscall_num { ... }` block and add a new arm:

```rust
99 => {
    // sys_my_syscall — brief description
    // arg0 = first argument (tf.regs[10] / a0)
    // arg1 = second argument (tf.regs[11] / a1)
    let result = my_module::do_something(arg0, arg1);
    tf.regs[10] = match result {
        Ok(val) => val,
        Err(e) => {
            crate::println!("sys_my_syscall: {}", e);
            usize::MAX
        }
    };
}
```

**Rules:**
- Pick an **unused syscall number**. Check the full table in `docs/syscalls.md`.
- For blocking syscalls (ones that must wait for data): re-wind `sepc` by 4 (`tf.sepc -= 4`), push the process back onto `RUN_QUEUE`, then call `schedule()` followed by `unsafe { __halt_cpu() }`.
- **Always** drop all `Mutex` guards before calling `schedule()`.
- Advance `sepc` by 4 is done automatically at the end of the non-blocking path: the code falls through to `tf.sepc += 4` after the `match` block.

### Step 2 — Userspace: `user/libos/src/lib.rs`

Add a wrapper function:

```rust
/// Brief description of what this syscall does.
/// Returns 0 on success, -1 on error.
pub fn my_syscall(arg0: usize, arg1: usize) -> isize {
    syscall(99, arg0, arg1, 0) as isize
}
```

For 4-argument syscalls use `syscall4`:

```rust
pub fn my_syscall(a: usize, b: usize, c: usize, d: usize) -> isize {
    syscall4(99, a, b, c, d) as isize
}
```

### Step 3 — Documentation: `docs/syscalls.md`

Add a row to the appropriate table section:

```markdown
| 99 | `my_syscall` | `(arg0: usize, arg1: usize) → isize` | 0 / -1 | What it does and any important semantics. |
```

Also update the "Complete Syscall Table" in `docs/architecture.md` §5.

---

## 6. Adding a VFS Backend

A new filesystem backend implements the `Vnode` trait in `src/vfs/`:

```rust
// src/vfs/myfs.rs
use crate::vfs::{DirEntry, Stat, Vnode, VnodeType};

pub struct MyFsFile { /* fields */ }

impl Vnode for MyFsFile {
    fn stat(&self) -> Stat { /* ... */ }

    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        // Implement read logic
    }

    // Only implement methods your FS supports.
    // All trait methods have a default returning Err("Not supported").
}
```

Mount it in `src/main.rs` inside the `kmain` VFS setup block:

```rust
mt.mounts.push((
    alloc::string::String::from("/myfs"),
    Arc::new(myfs::MyFsRoot::new()),
));
```

Add the module to `src/vfs/mod.rs`:

```rust
pub mod myfs;
```

**Tips:**
- Use `Arc<Mutex<InnerState>>` for mutable file state.
- If your filesystem has writable nodes, implement `write_at` and `truncate`.
- If it has directories, implement `lookup`, `readdir`, `create`, `mkdir`, `unlink`.
- Read `src/vfs/memfs.rs` as a reference implementation.

---

## 7. Adding a Driver

Drivers hook into the `Driver` trait in `src/drivers/mod.rs`:

```rust
pub trait Driver: Send + Sync {
    /// Human-readable driver name for logging.
    fn name(&self) -> &'static str;
    /// Called on interrupt. Return true if the IRQ was handled.
    fn handle_irq(&self, irq: usize) -> bool;
}
```

For device discovery:
1. Add a probe function that reads the DTB for your device's `compatible` string.
2. Register with `drivers::register(Arc::new(MyDriver::new(mmio_base)))` during `kmain` or `secondary_kmain`.
3. Enable the PLIC for your IRQ number in `plic.rs`.

For memory-mapped devices:
- Use the kernel's physical identity map — MMIO regions are directly addressable as physical addresses.
- Always use `core::ptr::read_volatile` / `write_volatile` for MMIO registers.

---

## 8. Writing Tests

openv does not yet have a dedicated test harness. Testing strategies in order of preference:

### 8.1 Userspace test programs

Add a new binary under `user/` (e.g. `user/mytest/`):

```toml
# user/Cargo.toml — add to members:
members = [..., "mytest"]
```

```rust
// user/mytest/src/main.rs
#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Test logic using libos syscall wrappers
    let fd = libos::open(b"/tmp/test", libos::O_RDONLY);
    assert!(fd >= 0, "open failed");
    0
}
```

Add `mytest` to `BINS` in `scripts/build.sh` and run `make BINS=mytest run`.

### 8.2 Kernel unit tests (host-side)

For pure logic (no hardware dependencies), extract functions into modules with `#[cfg(test)]` and test on the host:

```rust
// In src/vfs/tar.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_simple_tar() { /* ... */ }
}
```

Run with: `cargo test --target x86_64-unknown-linux-gnu` (not the RISC-V target).

### 8.3 QEMU serial output matching

For integration tests, capture QEMU serial output and check for expected strings:

```bash
timeout 30 qemu-system-riscv64 \
  -machine virt -nographic -serial stdio \
  -kernel target/riscv64gc-unknown-none-elf/debug/openv \
  -initrd test_root.tar \
  2>&1 | grep -q "shell: ready" && echo PASS || echo FAIL
```

---

## 9. Submitting Changes

### Workflow

1. Fork the repository and create a feature branch:
   ```console
   git checkout -b feat/my-feature
   ```

2. Make your changes following the coding standards above.

3. Format and lint:
   ```console
   cargo fmt
   cargo clippy -- -D warnings -A clippy::missing_safety_doc
   cd user && cargo fmt && cargo clippy -- -D warnings -A clippy::missing_safety_doc
   ```

4. Build and smoke-test:
   ```console
   make build
   make run   # boot and confirm the shell appears
   ```

5. Commit with a descriptive message:
   ```console
   git commit -m "trap: add sys_my_syscall (number 99)

   Adds a new syscall to [...]. Includes kernel handler in trap.rs,
   libos wrapper in user/libos/src/lib.rs, and documentation update
   in docs/syscalls.md."
   ```

6. Open a pull request against `main`.

### PR title format

```
<subsystem>: <short imperative description>
```

Examples:
- `trap: add sys_fstat (syscall 59)`
- `vfs: implement memfs directory rename across parents`
- `mm: fix double-free in destroy_user_space`
- `docs: add IPC channel usage examples`

### What CI checks

- `cargo fmt --check` on kernel + userspace
- `cargo clippy -D warnings` on kernel + userspace
- `cargo check` on kernel + userspace
- Debug + release build of kernel
- Build of all userspace binaries
- Upload of kernel + initrd artifacts

All checks must pass before a PR can be merged.

---

## 10. Code Review Checklist

Use this checklist when reviewing or submitting PRs:

**Safety**
- [ ] Every `unsafe` block has a `// SAFETY:` comment
- [ ] No raw pointer mutation of shared `Arc<T>` fields that are not `Atomic*` or `Mutex<T>`
- [ ] No new `static mut` without synchronisation
- [ ] `incr_ref`/`decr_ref` balanced for every page shared between address spaces

**Correctness**
- [ ] No `schedule()` called while holding a `spin::Mutex` guard
- [ ] Blocking syscalls re-wind `sepc -= 4` before yielding
- [ ] Non-blocking syscalls fall through to the `sepc += 4` at the end of the match block
- [ ] `destroy_user_space` skips root indices 0–3 (kernel superpages)
- [ ] Pipe EOF detected correctly via `Weak::upgrade` on write-end sentinel

**Testing**
- [ ] New syscalls tested via a userspace test binary
- [ ] New VFS backends tested with read + write + readdir
- [ ] New drivers tested against QEMU virtual hardware

**Documentation**
- [ ] `docs/syscalls.md` updated (new syscalls)
- [ ] `docs/architecture.md` updated (new subsystems)
- [ ] `CONTRIBUTING.md` updated if development workflow changes

**Style**
- [ ] `cargo fmt` applied
- [ ] `cargo clippy -D warnings` passes
- [ ] No debug `println!` left in hot paths
- [ ] Error messages are descriptive strings, not just numbers

---

## 11. Known Issues to Fix

If you are looking for a good first contribution, these are tracked issues from the code review:

| Priority | Issue | File | Fix |
|----------|-------|------|-----|
| 🔴 High | `ppid` mutation via raw pointer (data race) | `src/posix/spawn.rs:115` | Make `ppid` an `AtomicI32` |
| 🔴 High | `satp_val` mutation via raw pointer (data race) | `src/posix/spawn.rs:250` | Make `satp_val` an `AtomicUsize` |
| 🔴 High | PMM free-list in `static mut` (SMP-unsafe) | `src/mm/pmm.rs` | Wrap `NEXT_FREE_PAGE` in a `Mutex` or `AtomicUsize` |
| 🟡 Med | `trap.rs` God File (1,464 lines) | `src/trap.rs` | Split into `src/syscall/fs.rs`, `proc.rs`, `net.rs`, `tty.rs` |
| 🟡 Med | `LINE_DISC_BUFFER` is global, not per-session | `src/trap.rs` | Move into `Process` struct or a `TtySession` type |
| 🟡 Med | `sys_getdents` takes path instead of fd | `src/trap.rs` | Accept directory fd instead of path |
| 🟡 Med | Error returns are `usize::MAX`, no named errno | `src/trap.rs` | Add `const ENOENT: usize = usize::MAX` etc. |
| 🟢 Low | Duplicate `KERNEL_STACK_SIZE` constant | `src/posix/process.rs`, `src/trap.rs` | Define once, `pub` it |
| 🟢 Low | Unused `alloc_error_handler` feature gate | `src/main.rs:3` | Remove the `#![feature(...)]` line |
| 🟢 Low | Debug boot prints in production path | `src/main.rs:54-78` | Gate with `#[cfg(debug_assertions)]` |
| 🟢 Low | Dead `KernelObject::Vmo` variant | `src/ipc/handle.rs` | Add `sys_vmo_create`/`sys_vmo_map` or remove variant |

See the full review in [`docs/code_review.md`](docs/code_review.md) for details on each.

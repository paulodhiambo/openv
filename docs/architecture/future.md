# 16. Known Limitations and Future Work

### Known Bugs and Data Races

These issues were identified in the project's `code_review.md` analysis:

| Issue | Severity | Description |
|-------|----------|-------------|
| `ppid` data race | Medium | `proc.ppid` is mutated (during reparenting on exit) without holding the process table lock |
| `satp_val` race | Medium | `proc.satp_val` may be read by the scheduler on another HART while being updated |
| PMM free-list unlocked | High | `free_page()` is called in some code paths without holding the PMM lock, risking list corruption |
| `trap.rs` God File | Low | `trap.rs` handles syscall dispatch, fault handling, and IPC — should be split into smaller modules |
| Global TTY buffer | Low | `LINE_DISC_BUFFER` is a single global buffer; multiple concurrent readers would conflict |

### Missing POSIX Features

| Feature | Status | Notes |
|---------|--------|-------|
| Signals | Partial | Only Ctrl-C (`SIGINT`) handled ad-hoc in line discipline; no `kill()`, `sigaction()`, signal masks |
| Job control | Stored only | `pgid`/`sid` fields exist; `SIGTSTP`/`SIGCONT`/`SIGHUP` not implemented |
| `mmap` / `munmap` | Not implemented | VMO infrastructure exists; syscall wrappers not present |
| `select` / `poll` / `epoll` | Not implemented | Blocking I/O only; no readiness multiplexing |
| `fcntl` / `ioctl` | Not implemented | `O_NONBLOCK`, `FD_CLOEXEC` not supported |
| Close-on-exec | Not implemented | All FDs inherited across `exec()` |

### Persistence

The root filesystem is backed by the **initrd TAR archive** loaded at boot. All writes (to `MemFile` nodes) are in-memory only and are **lost on reboot**. There is no block device driver or persistent storage in v1.

### Scheduler Limitations

The current FIFO round-robin scheduler:
- Has no priority levels or nice values.
- Does not implement work-stealing across HARTs.
- Timer preemption re-queues the process but does not immediately context-switch; the actual switch happens at the **next syscall boundary** or when the current quantum exhausts naturally.

### Networking Limitations

| Limitation | Notes |
|------------|-------|
| No DHCP (default) | Static IP `10.0.2.15/24` configured at daemon compile time |
| No UDP sockets | Only TCP via smoltcp; `SOCK_DGRAM` not implemented |
| Raw Ethernet only | Kernel I/O is frame-level; higher protocols only in daemon |

### Cross-directory Rename

`sys_rename` calls `vnode.rename(old, new)` on the parent directory vnode. If `old` and `new` have different parent directories, the operation returns an error. Full cross-directory rename requires an atomic 2-directory operation that is deferred to a future version.

### Planned Future Work

| Item | Priority | Description |
|------|----------|-------------|
| Full POSIX signals | High | `kill()`, `sigaction()`, `sigprocmask()`, signal delivery on trap return |
| Persistent storage | High | Virtio-blk driver + ext2 or FAT32 filesystem |
| Fine-grained locking | Medium | Replace global process table lock with per-process locks |
| `mmap` syscall | Medium | Map VMOs into user address space |
| DHCP client | Medium | Dynamic IP assignment in net-smoltcp |
| UDP sockets | Medium | Add SOCK_DGRAM support to socket registry and daemon |
| ASLR | Low | Randomize user load address using a PRNG seeded from `cycle` CSR |
| Stack canaries | Low | Inject canary values at function entry via compiler plugin |
| Argon2id passwords | Low | Replace FNV-1a with proper password KDF |
| Job control | Low | Enforce `pgid`/`sid`, deliver `SIGHUP`, `SIGTSTP` |
| Microkernel split | Long-term | Move filesystem and device drivers to userspace servers |

---

*End of Architecture Reference — openv v1*

[Back to Index](README.md)

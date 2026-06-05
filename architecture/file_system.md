# Filesystem Subsystem — Implementation Specification

> Status: Draft v0.1 (build spec)
> Target: riscv64
> IPC model: capability handles + bidirectional channels (pipe semantics available)
> Companion to: Networking Subsystem spec v0.2 (shares kernel primitives §1 there)
> Audience: implementers.

---

## 0. Relationship to the Netstack Spec

This subsystem reuses, unchanged, the kernel primitives defined in the Networking spec §1:
handles + rights bitmask, `sys_channel_create/send/recv`, VMOs (`sys_vmo_*`), and the
`recd` supervisor with HEALTH_PING/PONG. Where this document says "as in netstack §X" it
refers to that document. Conventions (§0 there: LE, `#[repr(C)]`, `i64` = `-errno` on error,
`Handle` = `u32`) apply identically.

The design decision that drives this whole spec: **MINIX cannot recover a crashed file
server without losing open file descriptors. We will.** See §8.

---

## 1. Architecture Overview

We take one idea from each system, exactly as we did for networking:

| Source | Borrowed idea | Applied here |
|---|---|---|
| **Redox** | Filesystem is just another scheme; a file descriptor is a capability channel | `fs:` scheme family; `File` == per-resource channel handle |
| **Fuchsia** | Per-client namespace assembled from granted directory handles; content-addressed verified store | client-side namespace table; `blob:` content-addressed volume with Merkle verification |
| **MINIX 3** | Central VFS that forwards to pluggable backend FS servers + reincarnation | `vfsd` dispatcher, pluggable `fsd-*` backends, recoverable via handle replay |

```
  +------------------+      +------------------+
  |  Native app      |      |  POSIX app       |
  |  (libfs)         |      |  (libc)          |
  +--------+---------+      +---------+--------+
           |  open("fs:/etc/x")        |  open("/etc/x")
           |  over namespace handle    |  (libc -> same path resolve)
           v                           v
  +-------------------------------------------------+
  |  vfsd — path walk, mount table, fd lifetime,    |   <- the MINIX VFS role,
  |  open-handle journal (recovery)                 |      kept as a real server
  +----+---------------------+----------------------+
       | forwards resolved   |
       | request to backend  |
       v                     v
  +-----------+   +-----------+   +-----------+
  | fsd-rfs   |   | fsd-blob  |   | fsd-mem   |   ... pluggable backends
  | (CoW gen) |   | (content- |   | (tmpfs)   |
  |           |   |  addressed)|  |           |
  +-----+-----+   +-----+-----+   +-----------+
        |  block I/O over a granted channel
        v
  +-------------------------------------------------+
  |  blkd — block device driver (untrusted)         |
  |  exposes  blk:<dev>  scheme                      |
  +------------------------+------------------------+
                           |  MMIO / DMA / IRQ (capability-gated)
                           v
  +-------------------------------------------------+
  |  microkernel — sched, IPC, capabilities         |
  +-------------------------------------------------+

  Supervised by recd (as in netstack §8): blkd, vfsd, each fsd-* are restartable.
```

Note the asymmetry vs. networking on purpose: we KEEP a central dispatcher (`vfsd`,
the MINIX shape) because path resolution, mount semantics, and fd lifetime genuinely
benefit from one authority — but we expose it through the Redox scheme model and the
Fuchsia per-client namespace, and we make it recoverable, which MINIX did not.

---

## 2. Process & Capability Topology

```
recd
 ├─ spawns blkd@vd0    grants: {mmio(vd0), irq(vd0), dma}
 ├─ spawns vfsd        grants: {channel→blkd? no — vfsd talks to fsd only;
 │                              open-handle journal VMO; channel→each fsd}
 ├─ spawns fsd-rfs     grants: {channel→blkd (blk: data), channel→vfsd (control)}
 ├─ spawns fsd-blob    grants: {channel→blkd, channel→vfsd}
 └─ health channels to all of the above

Application
 └─ receives a NAMESPACE capability handle from its launcher (env handle slot 4).
    It is a channel to vfsd carrying a *namespace id* and rights. The app can only
    resolve paths reachable in that namespace (Fuchsia per-client namespacing).
```

A process whose namespace handle grants only `/etc` (read) and `/tmp` (read-write)
literally cannot name `/home/other`. There is no global root every process shares;
the global tree lives in vfsd and is *projected* per client.

---

## 3. Naming & Namespaces

### 3.1 Scheme names (Redox model)

| Name | Provider | Meaning |
|---|---|---|
| `fs:/abs/path` | vfsd | a file or directory in the client's namespace |
| `blk:vd0` | blkd | raw block device |
| `blob:<hash>` | fsd-blob | a content-addressed immutable blob |

### 3.2 Per-client namespace (Fuchsia model)

A namespace is a set of `(prefix → (backend, subtree-root, rights))` entries held by vfsd
and keyed by `namespace_id`. Path resolution is longest-prefix match against the entries
the client may see.

```rust
#[repr(C)]
struct NsEntry {
    prefix_len: u16,      // bytes of prefix string following
    backend_id: u16,      // which fsd-* serves this subtree
    root_inode: u64,      // subtree root in that backend
    rights: u32,          // ceiling for anything opened under this prefix
    // followed by prefix_len bytes
}
```

Launchers compose a child namespace by selecting a subset of the parent's entries with
rights ≤ the parent's — the same monotonic-narrowing rule as handles (netstack §1.1).

---

## 4. Control Protocol

Reuses `CtrlHeader` (netstack §4.1). New opcodes occupy the `0x02xx` filesystem range so
they never collide with the networking `0x00xx`/`0x01xx` ranges.

| Opcode | Name | Direction | Body |
|---|---|---|---|
| 0x0201 | OPEN | app→vfsd | `FsOpenReq` |
| 0x0202 | OPEN_REPLY | vfsd→app | `FsOpenReply` (+1 transferred file handle) |
| 0x0203 | READAT | app→vfsd/fsd | `ReadAtReq` |
| 0x0204 | WRITEAT | app→vfsd/fsd | `WriteAtReq` |
| 0x0205 | READDIR | app→vfsd | `ReadDirReq` / reply (stream of `DirEnt`) |
| 0x0206 | GETATTR | app→vfsd | `GetAttrReq` / reply (`Attr`) |
| 0x0207 | SETATTR | app→vfsd | `SetAttrReq` |
| 0x0208 | TRUNCATE | app→vfsd | `TruncateReq` |
| 0x0209 | FSYNC | app→vfsd | empty body; reply on durability |
| 0x020A | UNLINKAT | app→vfsd | `UnlinkAtReq` |
| 0x020B | RENAMEAT | app→vfsd | `RenameAtReq` |
| 0x020C | MKDIRAT | app→vfsd | `MkdirAtReq` |
| 0x020D | CLOSE | app→vfsd | `CloseReq` |
| 0x0210 | MOUNT | cfg→vfsd | `MountReq` (RIGHT_MANAGE) |
| 0x0211 | UNMOUNT | cfg→vfsd | `UnmountReq` (RIGHT_MANAGE) |
| 0x0280 | FS_BLOCK_RW | fsd→blkd | `BlockRwReq` (§6) |
| 0x0290 | VFS_RECOVER | vfsd→fsd | `RecoverReq` (§8) |

Note the verbs are **positional** (`READAT`/`WRITEAT` carry an explicit offset), not
seek-then-read. This is the modern Redox direction — offset is in the request, no per-fd
seek state living in the server, which also makes recovery far simpler (§8) and matches
how the underlying block disk interface is inherently offset-based.

### 4.1 Key request/reply structs

```rust
#[repr(C)]
struct FsOpenReq {
    hdr: CtrlHeader,        // opcode=OPEN
    ns_id: u32,             // which namespace (validated against caller handle)
    flags: u32,            // O_RDONLY/O_WRONLY/O_RDWR/O_CREAT/O_EXCL/O_DIRECTORY/O_APPEND
    mode: u32,             // creation mode if O_CREAT
    want_rights: u32,
    path_len: u16,
    _pad: u16,
    // followed by path_len bytes of absolute path within the namespace
}

#[repr(C)]
struct FsOpenReply {
    hdr: CtrlHeader,        // opcode=OPEN_REPLY
    status: i32,
    granted_rights: u32,
    fid: u64,               // server-side file id (also the recovery key, §8)
    kind: u8,               // 0=file 1=dir 2=symlink 3=device
    _pad: [u8;7],
    // on success: one transferred handle = per-file channel
}

#[repr(C)]
struct ReadAtReq {
    hdr: CtrlHeader,        // opcode=READAT
    offset: u64,
    len: u32,               // bytes requested
    _pad: u32,
}
// reply: CtrlHeader + u32 nbytes + that many bytes

#[repr(C)]
struct WriteAtReq {
    hdr: CtrlHeader,        // opcode=WRITEAT
    offset: u64,
    len: u32,
    _pad: u32,
    // followed by len bytes
}
// reply: CtrlHeader + i64 nwritten_or_errno

#[repr(C)]
struct Attr {
    size: u64,
    blocks: u64,
    mtime_ns: u64,
    ctime_ns: u64,
    atime_ns: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u32,
    kind: u8,
    _pad: [u8;7],
}

#[repr(C)]
struct DirEnt {
    inode: u64,
    kind: u8,
    name_len: u8,
    _pad: [u8;6],
    // followed by name_len bytes (no NUL); entries packed, 8-byte aligned
}
```

---

## 5. `vfsd` — The VFS Dispatcher

### 5.1 Role (MINIX VFS, kept and hardened)

`vfsd` owns:
- the mount table (`prefix → backend`),
- per-client namespaces (§3.2),
- path walking (longest-prefix match, then delegate remaining path to the backend),
- the **open-handle journal** (§8) — the piece MINIX lacked.

`vfsd` holds **no file contents** and does **no block I/O**. It is pure routing + fd
lifetime + recovery bookkeeping, which keeps it cheap to restart.

### 5.2 Open flow (concrete)

```
app -> vfsd: OPEN{ns_id, "/etc/hosts", flags=O_RDONLY, want_rights=READ}
vfsd:
  1. validate caller's namespace handle authorizes ns_id
  2. longest-prefix match -> NsEntry{backend=fsd-rfs, root_inode, rights ceiling}
  3. clamp want_rights <= entry.rights
  4. forward resolved request to fsd-rfs over vfsd↔fsd control channel:
       {root_inode, residual="hosts", flags, rights}
  5. fsd-rfs walks residual from root_inode, returns {fid, kind, attr}
  6. vfsd creates a per-file channel pair (a,b); records journal entry (§8.2)
  7. reply OPEN_REPLY{status=0, fid, kind} transferring `b` to app; keep `a`
app: now issues READAT/WRITEAT on its file handle
```

Data path detail: for large reads vfsd may **splice** — instead of relaying bytes through
itself, it can hand the backend the app's channel endpoint so the backend replies directly
to the app (the MINIX "FS copies directly to the user process" optimization). v1 relays
through vfsd for simplicity; splice is a v2 behind the same opcodes.

### 5.3 fd lifetime

A file "fd" in app-land is just the per-file channel handle. Closing it (CLOSE or dropping
the handle → channel EPIPE) tells vfsd to release the backend `fid`. Because offsets are
per-request (§4), vfsd stores no seek cursor — POSIX `lseek` is emulated entirely in libc
(§9) by tracking an offset client-side and supplying it to READAT/WRITEAT.

---

## 6. `blkd` — Block Device Driver & `fsd-*` Backends

### 6.1 blkd (untrusted, like nicd)

- One per device, exposes `blk:<dev>`. Granted mmio/irq/dma at spawn (netstack §5.2 shape).
- Offers fixed-size logical blocks (default 4096 B; the disk interface is block-granular,
  not byte-granular — backends adapt).

```rust
#[repr(C)]
struct BlockRwReq {
    hdr: CtrlHeader,        // opcode=FS_BLOCK_RW
    lba: u64,               // logical block address
    count: u32,             // number of blocks
    write: u32,             // 0=read 1=write
    // write: followed by count*block_size bytes
}
```

v1 copies blocks over the channel; v2 negotiates a shared DMA VMO exactly as nicd does
(netstack §5.4) using a `BlockDesc{offset,len}` over the data channel.

### 6.2 fsd-rfs (general CoW filesystem)

The default read-write backend. Design points adopted from the RedoxFS/ZFS lineage but
sized for a first implementation:

- **Copy-on-write**: never overwrite live blocks; write new, then atomically swap a
  pointer. Gives crash-atomic updates without a separate journal for data.
- **Checksummed blocks**: every block carries a checksum in its parent pointer (ZFS-style),
  so corruption is detected on read.
- **Optional AES encryption** at the block layer when the platform exposes AES acceleration;
  the volume key is unwrapped at mount (bootloader can pre-unwrap the root volume).
- On-disk superblock + tree-of-block-pointers; transactional root pointer ("uberblock")
  updated last.

> Scope control: v1 ships CoW + checksums + a single root pointer with a small ring of
> historical roots for rollback. Snapshots, compression, and multi-disk come later, behind
> the same backend interface.

### 6.3 fsd-blob (content-addressed, Fuchsia model)

- A write-once, read-many volume. The name of a blob **is** the hash of its contents.
- On write, the backend computes a Merkle tree over the data; the root hash is the blob id.
- Reads are verified against the Merkle tree block-by-block; a hash mismatch returns `-EIO`.
- Immutable after seal: no WRITEAT after the blob is finalized; only create-whole or remove.
- Used for executables/packages so contents are verifiable before execution.

```rust
#[repr(C)]
struct BlobSeal {
    hdr: CtrlHeader,
    merkle_root: [u8;32],   // = the blob id; clients can verify independently
    total_len: u64,
}
```

### 6.4 fsd-mem (tmpfs)

RAM-backed, no blkd dependency, for `/tmp` and early boot. Trivial; also the easiest
target to bring up vfsd against before blkd/fsd-rfs exist.

---

## 7. Mounting

```rust
#[repr(C)]
struct MountReq {
    hdr: CtrlHeader,       // opcode=MOUNT, needs RIGHT_MANAGE
    backend_id: u16,
    prefix_len: u16,
    flags: u32,            // ro, noexec, etc.
    // followed by prefix bytes; the backend is assumed already spawned & connected
}
```

Mount is a vfsd table edit: associate a path prefix with an already-running `fsd-*` and a
backend root inode. Unlike MINIX where the FS server is started by the mount machinery,
here `recd` spawns backends and vfsd just wires prefixes to them — which is what makes
backends independently restartable.

---

## 8. Recovery — Solving What MINIX Punted On

**Goal:** a crashed `fsd-*` OR a crashed `vfsd` is restarted by `recd` (netstack §8) without
applications losing their open files.

The reason MINIX couldn't do this: open-file state (which fid maps to which inode, at what
access mode, for which client) lived only in the volatile memory of the crashing server, so
on death VFS could only invalidate the fds. We make that state **reconstructable** via two
mechanisms.

### 8.1 Stateless-where-possible design

Because reads/writes are positional (no server-side seek cursor, §4) and CoW makes the
on-disk image always consistent at the last committed root, a backend's only volatile state
is the *open-file table*: `fid → (inode, mode, rights, owner)`. Nothing about *file
position* or *dirty in-flight seek state* needs recovering.

### 8.2 The open-handle journal (in vfsd)

vfsd records every successful OPEN into a small shared VMO owned by recd:

```rust
#[repr(C)]
struct OpenJournalEnt {
    fid: u64,
    backend_id: u16,
    kind: u8,
    _pad: u8,
    rights: u32,
    inode: u64,            // backend root-relative inode resolved at open time
    ns_id: u32,
    owner_pid: u32,
}
```

Entries are appended on OPEN, marked free on CLOSE. This journal is *metadata only* and
tiny (a few hundred bytes per open file).

### 8.3 Recovering a crashed backend (`fsd-*`)

```
1. recd detects fsd-rfs dead (missed HEALTH_PONG / fault) -> revokes its handles,
   respawns it, re-grants {blkd channel, vfsd channel}.
2. recd signals vfsd: VFS_RECOVER{backend_id}.
3. vfsd scans its open-handle journal for that backend_id and, for each live fid,
   sends the fresh fsd a RecoverReq{fid, inode, mode, rights}.
4. fsd re-mounts the volume (CoW root is consistent on disk), re-opens each inode,
   and rebinds the fid. No app-visible handle changes; the per-file channel between
   app and vfsd never closed.
5. In-flight requests that were lost return -EAGAIN to the app; libc retries them
   transparently for idempotent positional ops (READAT is idempotent; WRITEAT is
   idempotent given the same offset+bytes).
```

```rust
#[repr(C)]
struct RecoverReq {
    hdr: CtrlHeader,       // opcode=VFS_RECOVER target fsd
    fid: u64,
    inode: u64,
    mode: u32,
    rights: u32,
}
```

### 8.4 Recovering a crashed `vfsd`

Harder, because vfsd holds the app-facing channel endpoints. Two tiers:

- **Tier 1 (v1):** vfsd crash invalidates app file handles (apps see EPIPE and re-open).
  This is exactly MINIX's behavior — acceptable as a floor, since vfsd is small and rarely
  the crasher.
- **Tier 2 (later):** the open-handle journal (§8.2) lives in recd's VMO, *not* vfsd's
  heap, so a fresh vfsd reloads it. The unrecoverable piece is the kernel channel endpoints
  to apps. We close that gap by having apps hold their file handle as a **handle to a
  vfsd-owned channel that recd can re-bind**: on vfsd restart, recd transfers the surviving
  app-side channel endpoints to the new vfsd, which reloads the journal and resumes. Gated
  behind a kernel capability (`sys_channel_rebind`) — list it as a dependency, do not
  assume it silently.

### 8.5 Idempotency contract (makes retry safe)

| Op | Idempotent? | Recovery action |
|---|---|---|
| READAT | yes | retry |
| WRITEAT (explicit offset) | yes | retry |
| WRITEAT (O_APPEND) | NO | libc resolves append to an explicit offset *before* sending, making it a positional write; retry-safe |
| MKDIRAT / UNLINKAT / RENAMEAT | no (but detectable) | check post-state; treat "already done" as success |
| FSYNC | yes | retry |

This table is the crux: by forbidding hidden server-side mutable position state and
converting append to a resolved offset in libc, almost everything becomes a retryable
positional op, which is what makes §8.3 recovery transparent.

---

## 9. Application Interface

### 9.1 libfs (native)

```rust
pub struct Namespace(Handle);     // env slot 4
pub struct File(Handle);          // per-file channel
pub struct Dir(Handle);

impl Namespace {
    pub fn from_env() -> Result<Self>;
    pub fn open(&self, path: &str, flags: OpenFlags) -> Result<File>;
    pub fn open_dir(&self, path: &str) -> Result<Dir>;
}
impl File {
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize>;   // READAT
    pub fn write_at(&self, off: u64, buf: &[u8]) -> Result<usize>;      // WRITEAT
    pub fn attr(&self) -> Result<Attr>;
    pub fn fsync(&self) -> Result<()>;
}
```

### 9.2 POSIX shim (libc)

| POSIX | Mapping |
|---|---|
| `open(path, flags)` | OPEN over namespace handle; fd = returned file handle; libc inits cursor=0 |
| `read(fd, buf, n)` | READAT{offset=cursor}; cursor += nread |
| `write(fd, buf, n)` | WRITEAT{offset=cursor}; cursor += nwritten (O_APPEND: cursor=size first) |
| `lseek(fd, off, whence)` | pure libc cursor arithmetic; no server round-trip |
| `pread/pwrite` | direct READAT/WRITEAT, cursor untouched |
| `stat/fstat` | GETATTR → Attr |
| `readdir` | READDIR stream → DirEnt decode |
| `mkdir/unlink/rename` | MKDIRAT/UNLINKAT/RENAMEAT |
| `close` | CLOSE / drop handle |

The shim must never widen access: it can only use the namespace + rights already granted;
exceeding scope returns `EACCES`. (Same rule as netstack §9.2.)

---

## 10. Validation Rules (security-critical)

- vfsd validates every path against the caller's namespace BEFORE delegating; no `..`
  escapes above a namespace entry root (canonicalize and re-check prefix after resolving
  `.`/`..`).
- fsd-* treats vfsd as semi-trusted but still bounds-checks inode numbers and offsets
  against the on-disk volume.
- fsd-blob MUST verify Merkle hashes on read; a mismatch is `-EIO`, never silently served.
- blkd bounds-checks LBA+count against device size; rejects writes to read-only mounts at
  the fsd layer (blkd itself is mount-agnostic).
- CoW commit ordering (fsd-rfs): data blocks + checksums durable BEFORE the root pointer is
  swapped; the root swap is the single atomic commit point.

---

## 11. Error Codes

Extends netstack §11 with FS-specific values (negative `-errno`):

| Val | Name | Meaning here |
|---|---|---|
| 2 | ENOENT | path not found |
| 13 | EACCES | rights/namespace insufficient |
| 17 | EEXIST | O_CREAT|O_EXCL on existing |
| 20 | ENOTDIR | path component not a directory |
| 21 | EISDIR | write to a directory |
| 28 | ENOSPC | volume full |
| 30 | EROFS | write to read-only mount |
| 5 | EIO | block I/O failure / Merkle verification failure |
| 11 | EAGAIN | request lost to a mid-flight backend restart; retry (§8.5) |
| 36 | ENAMETOOLONG | path/name over limit |
| 39 | ENOTEMPTY | rmdir non-empty |

---

## 12. Build Phases (testable exit criteria)

| Phase | Deliverable | Exit criterion |
|---|---|---|
| F0 | fsd-mem (tmpfs) + vfsd skeleton; OPEN/READAT/WRITEAT/CLOSE | create, write, read back a file in RAM through vfsd |
| F1 | blkd (qemu virtio-blk), `blk:` BLOCK_RW v1 copying | read/write raw LBAs verified against host image |
| F2 | fsd-rfs: superblock, CoW tree, checksums, mount under vfsd | survive `kill -9` mid-write with no corruption (last root intact) |
| F3 | libfs + libc POSIX shim incl. lseek-in-libc, readdir | unmodified file-using program (e.g. a text editor) runs |
| F4 | per-client namespaces + mount/unmount + path-escape checks | a `..`-escape attempt is rejected; sandboxed proc can't see other subtrees |
| F5 | recovery: open-handle journal + RecoverReq; backend restart | `kill -9 fsd-rfs` mid-session; open fds keep working after respawn (§8.3) |
| F6 | fsd-blob: Merkle write/verify; serve an executable from it | tampered blob read returns EIO; valid blob executes |
| F7 | AES at block layer; bootloader root-volume unlock; v2 DMA + splice | encrypted root mounts; no per-block copy on the data path |

---

## 13. riscv64 Notes

- Block driver MMIO/IRQ/DMA identical to nicd guidance (netstack §13): `fence` between
  descriptor writes and the doorbell; cache clean-before-write / invalidate-after-read on
  non-coherent SoCs; PLIC IRQ via `sys_irq_wait`/`sys_irq_ack`.
- AES: use the RISC-V scalar cryptography extension (Zkn) if present; detect at fsd-rfs
  start and fall back to a software AES path otherwise. Gate the on-disk "encrypted" flag so
  a volume created with hardware AES still mounts on a software-only build (same algorithm,
  different code path).
- Merkle hashing (fsd-blob): SHA-256; use Zknh hash instructions when available.
- All on-disk integers LE (matches riscv64), so the image is endian-portable by construction.

---

## 14. Open Items

1. **vfsd Tier-2 recovery** depends on a `sys_channel_rebind` kernel primitive (§8.4) — must
   confirm the kernel can transfer a live app-side channel endpoint to a respawned vfsd. If
   not, vfsd recovery stays at Tier 1.
2. **Splice data path** (§5.2): handing the app's channel endpoint to a backend crosses a
   trust boundary — decide whether the backend gets a rights-narrowed dup, not the original.
3. **fsd-rfs historical-root ring depth**: how many old uberblocks to retain for rollback vs
   space cost; default 4, revisit after F2.
4. **Directory rename atomicity across backends** (cross-mount rename): forbid in v1 (return
   EXDEV) rather than implement cross-server transactions.
5. **Blob GC**: content-addressed blobs need reference counting for removal; defer the GC
   design to post-F6.
6. **Append idempotency** (§8.5): resolving O_APPEND to an offset in libc has a TOCTOU window
   if two writers share the fd; document that O_APPEND atomicity across processes is not
   guaranteed in v1, or push append resolution into fsd as a dedicated APPEND op (decide).
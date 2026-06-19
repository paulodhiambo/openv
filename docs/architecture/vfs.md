# 7. Virtual File System

### 7.1 Vnode Trait

All filesystem objects implement the `Vnode` trait:

```rust
pub trait Vnode: Send + Sync {
    /// Read up to `buf.len()` bytes at `offset`. Returns bytes read.
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, VfsError>;

    /// Write `buf` at `offset`. Returns bytes written.
    fn write_at(&self, offset: usize, buf: &[u8]) -> Result<usize, VfsError>;

    /// Return file size in bytes.
    fn size(&self) -> usize;

    /// Return file type and permissions.
    fn stat(&self) -> FileStat;

    /// List directory entries. Returns `VfsError::NotDirectory` for files.
    fn readdir(&self) -> Result<Vec<DirEntry>, VfsError>;

    /// Create a child file with name `name`. Returns new Vnode.
    fn create(&self, name: &str, mode: u32) -> Result<Arc<dyn Vnode>, VfsError>;

    /// Create a child directory with name `name`.
    fn mkdir(&self, name: &str, mode: u32) -> Result<Arc<dyn Vnode>, VfsError>;

    /// Remove child entry `name`.
    fn unlink(&self, name: &str) -> Result<(), VfsError>;

    /// Rename child `old_name` to `new_name` within this directory.
    fn rename(&self, old_name: &str, new_name: &str) -> Result<(), VfsError>;

    /// Lookup child entry `name`. Returns `None` if not found.
    fn lookup(&self, name: &str) -> Option<Arc<dyn Vnode>>;

    /// Return owner UID, GID, and permission mode bits.
    fn owner(&self) -> (u32, u32, u32); // (uid, gid, mode)
}
```

### 7.2 MountTable

The `MountTable` maps path prefixes to mounted filesystem roots:

```rust
pub struct MountTable {
    mounts: Vec<(String, Arc<dyn Vnode>)>, // (prefix, root_vnode)
}
```

**Path resolution algorithm (`vfs::lookup(path)`):**

```
1. Find the longest prefix in MountTable that is a prefix of `path`
2. Let root = that mount's root Vnode
3. Strip the prefix from `path` to get relative `rest`
4. Split `rest` by '/' → components
5. For each component:
     a. If ".." → ascend (handle mount boundaries)
     b. If "." → skip
     c. Otherwise → root = root.lookup(component)?
6. Return final Vnode (or VfsError::NotFound)
```

**Mount configuration at boot:**

| Mount point | Filesystem | Backing data                     |
|-------------|------------|----------------------------------|
| `/`         | TarFS      | initrd TAR archive               |
| `/proc`     | ProcFS     | Live kernel state                |
| `/dev`      | DevFS      | Synthetic devices                |
| `/mnt`      | OFS        | virtio-blk `disk.img` (if present; auto-formatted on blank disk) |

### 7.3 MemFS

MemFS provides in-memory filesystem nodes:

```rust
/// Read-only file: zero-copy slice into the initrd region
pub struct RoFile {
    data: &'static [u8],   // pointer into initrd, no copy
    stat: FileStat,
}

/// Writable file: heap-allocated byte vector
pub struct MemFile {
    data: Mutex<Vec<u8>>,
    stat: FileStat,
}

/// Directory: sorted map from name to child Vnode
pub struct MemDir {
    children: Mutex<BTreeMap<String, Arc<dyn Vnode>>>,
    stat:     FileStat,
}
```

`RoFile` avoids copying initrd data into the heap — reads are zero-copy slices. Writes to `RoFile` return `VfsError::ReadOnly`.

### 7.4 TarFS

TarFS parses the initrd TAR archive at boot using the **UStar format**:

```
UStar header (512 bytes):
  [0..100]   filename
  [100..108] mode (octal ASCII)
  [108..116] uid (octal ASCII)
  [116..124] gid (octal ASCII)
  [124..136] size (octal ASCII)
  [136..148] mtime (octal ASCII)
  [148..156] checksum
  [156]      typeflag ('0'=file, '2'=symlink, '5'=directory)
  [157..257] linkname
  [257..265] magic ("ustar")
  ...
```

**Parsing algorithm:**

```
offset = 0
while offset < initrd_size:
    header = initrd[offset..offset+512]
    if header is all zeros: break  (end-of-archive sentinel)
    parse filename, size, typeflag, mode, uid, gid
    create VFS nodes:
        '5' (directory) → MemDir with parsed permissions
        '0' (file)      → RoFile pointing to initrd[offset+512..offset+512+size]
    install into MemFS tree under '/'
    offset += 512 + round_up(size, 512)
```

All file data in `RoFile` nodes is a zero-copy reference into the initrd memory region, which persists for the kernel's lifetime.

### 7.5 ProcFS

ProcFS exposes kernel state as readable virtual files:

| Path                    | Content                                          |
|-------------------------|--------------------------------------------------|
| `/proc/<pid>/status`    | PID, PPID, state, uid, gid, cwd as text          |

`ProcFS` implements a custom `Vnode` that generates content on-demand from the live `PROCESS_TABLE`. The `/proc` root's `readdir()` enumerates all current PIDs.

### 7.6 DevFS

DevFS provides POSIX-style special device files:

| Path          | Vnode type   | `read_at` behaviour                   | `write_at` behaviour      |
|---------------|--------------|---------------------------------------|---------------------------|
| `/dev/null`   | `NullDev`    | Returns 0 (EOF immediately)           | Discards all data          |
| `/dev/zero`   | `ZeroDev`    | Fills buffer with 0x00 bytes          | Discards all data          |
| `/dev/tty`    | `TtyDev`     | Reads from the line discipline buffer | Writes to UART             |

### 7.7 OFS — Persistent On-Disk Filesystem

OFS ("openv filesystem") is a simple journaled block filesystem backed by a virtio-blk device. It lives in `user/vfs-server/src/blockfs.rs` and runs entirely in userspace inside `vfs-server`.

**Disk layout (4 KiB blocks):**

| Block | Contents |
|-------|----------|
| 0 | Superblock: magic `0x4F564653` ("OVFS"), version, total_blocks, CRC32 |
| 1 | Block allocation bitmap |
| 2 | Inode allocation bitmap |
| 3–34 | Inode table (one inode per 4 KiB block) |
| 35 | Journal header: magic, commit flag, slot count, CRC32 |
| 36–67 | Journal data slots (32 slots × 4 KiB) |
| 68+ | Data blocks |

**Write-ahead journal:** All metadata writes go through a two-phase journal: write data to a journal slot, set the commit flag, then apply to the actual block. On mount, OFS replays any committed-but-not-applied journal entry before normal operation.

**Auto-format:** On first mount, if the superblock magic or CRC does not match, OFS calls `mkfs()` to initialise the layout on the blank disk. No manual `mkfs` step is needed.

**Block device interface:** vfs-server communicates with `virtio-blk-driver` (pid 8) via IPC messages:

| Message type | Direction | Meaning |
|---|---|---|
| `OP_BLOCK_READ = 100` | vfs-server → driver | Read 4 KiB block N |
| `OP_BLOCK_WRITE = 101` | vfs-server → driver | Write 4 KiB block N |
| `type_ = 0` | driver → vfs-server | OK reply |
| `type_ = -1` | driver → vfs-server | Error reply |

**QEMU setup:** OFS requires `disk.img` to be passed as a virtio-blk device. `make run` creates an 8 MB `disk.img` automatically if none exists, and QEMU is always launched with `-device virtio-blk-device,drive=disk0`. The disk persists across kernel rebuilds; `make clean-all` removes it.

### 7.8 File Access Control

Access control uses the classic **Unix 9-bit permission model**:

```
mode bits:  S_ISUID S_ISGID  |  owner(rwx)  group(rwx)  other(rwx)
              [11]    [10]       [8:6]         [5:3]        [2:0]
```

**`check_access(vnode, proc, access_flags)`:**

```rust
pub fn check_access(vnode: &dyn Vnode, proc: &Process, flags: u32) -> bool {
    if proc.euid == 0 { return true; }  // root bypasses all checks

    let (owner_uid, owner_gid, mode) = vnode.owner();
    let rwx = if proc.euid == owner_uid {
        (mode >> 6) & 0o7     // owner bits
    } else if proc.egid == owner_gid {
        (mode >> 3) & 0o7     // group bits
    } else {
        mode & 0o7            // other bits
    };

    (rwx & flags) == flags
}
```

where `flags` is a combination of `R_OK=4`, `W_OK=2`, `X_OK=1`.

---
[Back to Index](README.md)

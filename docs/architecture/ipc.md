# 8. IPC

### 8.1 Channels

Channels are **bidirectional message queues** — the primary kernel IPC primitive:

```rust
pub struct Channel {
    queue: Mutex<VecDeque<Vec<u8>>>,
    waiter: Option<u32>,  // PID blocked on try_recv
}

pub struct ChannelPair {
    pub client: Arc<Channel>,
    pub server: Arc<Channel>,
}
```

**Operations:**

- `write(channel, data)` — pushes a `Vec<u8>` message to the queue; wakes any blocked receiver.
- `try_recv(channel)` — pops a message if available; returns `None` immediately if empty.
- Blocking receive: `try_recv` returns `None` → process sets `state = Blocked`, calls `schedule()`. `write` on the other side calls `wake_process(waiter_pid)` which re-queues the blocked process.

Channels underpin: socket IPC, socket daemon communication, accepted connection delivery.

### 8.2 HandleTable

```rust
pub struct HandleTable {
    entries: Vec<Option<Arc<KernelObject>>>,
}

impl HandleTable {
    /// Insert at lowest available index ≥ 0. Returns the FD.
    pub fn insert(&mut self, obj: Arc<KernelObject>) -> usize { ... }

    /// Insert at exactly `fd`, replacing any existing entry.
    pub fn insert_at(&mut self, fd: usize, obj: Arc<KernelObject>) { ... }

    /// Remove and return entry at `fd`.
    pub fn remove(&mut self, fd: usize) -> Option<Arc<KernelObject>> { ... }

    /// Get reference to entry at `fd`.
    pub fn get(&self, fd: usize) -> Option<&Arc<KernelObject>> { ... }

    /// Close all handles (called on process exit).
    pub fn close_all(&mut self) { self.entries.clear(); }
}
```

**FD assignment:** `insert()` scans `entries` for the first `None` slot (lowest-free). If none exists, extends the vector. This matches POSIX FD number assignment semantics.

**Inheritance on fork:** The child's `HandleTable` is cloned (`Arc` clones, same underlying objects). This means child and parent initially share file descriptions (same offset), as POSIX requires.

**Close on exec:** Not yet implemented in v1 (all FDs are inherited across `exec`).

### 8.3 KernelObject Variants

```rust
pub enum KernelObject {
    /// Console standard I/O
    Console,

    /// A bidirectional channel endpoint
    Channel(Arc<Channel>),

    /// A file (shared offset via Arc<FileDescription>)
    File(Arc<FileDescription>),

    /// Read end of a pipe
    PipeRead(Arc<Mutex<VecDeque<u8>>>, Arc<()>),
    //                                 ^^^^^^^ EOF sentinel

    /// Write end of a pipe
    PipeWrite(Arc<Mutex<VecDeque<u8>>>, Weak<()>),
    //                                  ^^^^^^^ Weak to sentinel; drop = EOF signal

    /// A socket (wraps a Channel endpoint + socket ID)
    Socket { channel: Arc<Channel>, sid: usize },
}
```

### 8.4 Pipes

```rust
// Creating a pipe:
let buf    = Arc::new(Mutex::new(VecDeque::<u8>::new()));
let eof    = Arc::new(());            // strong sentinel
let read   = KernelObject::PipeRead(buf.clone(), eof.clone());
let write  = KernelObject::PipeWrite(buf.clone(), Arc::downgrade(&eof));
```

**EOF detection:**
- When the last `PipeWrite` handle is closed, its `Weak<()>` reference is dropped.
- The `eof` `Arc<()>` has only `PipeRead` holding a strong reference.
- `sys_read` on a `PipeRead` checks: if queue is empty AND `Arc::strong_count(eof) == 1`, return 0 (EOF).

**Backpressure:** Not implemented in v1 — writes to a pipe always succeed regardless of buffer size.

### 8.5 File Descriptions

```rust
pub struct FileDescription {
    pub vnode:  Arc<dyn Vnode>,
    pub offset: Mutex<usize>,    // shared current read/write position
    pub flags:  u32,             // O_RDONLY, O_WRONLY, O_RDWR, O_APPEND
}
```

`FileDescription` is wrapped in `Arc<FileDescription>`. Both `dup()` and `fork()` clone the `Arc`, so multiple FDs (possibly in different processes) share the **same file offset**. This is the POSIX-correct behaviour for `dup()`-ed descriptors.

---
[Back to Index](README.md)

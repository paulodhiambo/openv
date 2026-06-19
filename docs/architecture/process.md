# 6. Process Model

### 6.1 Process Struct

```rust
pub struct Process {
    pub pid:        u32,
    pub ppid:       u32,          // parent PID (mutable — updated on reparent)
    pub pgid:       u32,          // process group ID
    pub sid:        u32,          // session ID
    pub uid:        u32,          // real user ID
    pub gid:        u32,          // real group ID
    pub euid:       u32,          // effective user ID (changed by setuid/exec)
    pub egid:       u32,          // effective group ID
    pub state:      ProcessState, // Running | Runnable | Blocked | Zombie
    pub satp_val:   usize,        // Sv39 satp CSR value (root page table PA)
    pub trap_frame: *mut TrapFrame, // pointer to this process's TrapFrame
    pub kernel_sp:  usize,        // top of kernel stack (for TrapFrame setup)
    pub cwd:        String,       // current working directory
    pub handles:    HandleTable,  // file descriptor table
    pub wait_result: Option<(u32, i32)>, // reaped child: (pid, status)
    pub children:   Vec<u32>,    // PIDs of child processes
    pub exit_code:  i32,         // set on exit, read by waitpid
}

pub enum ProcessState {
    Running,    // Currently executing on a HART
    Runnable,   // In RUN_QUEUE, waiting for CPU
    Blocked,    // Waiting for I/O or child exit
    Zombie,     // Exited but not yet reaped by parent
}
```

### 6.2 Scheduler

openv uses a **priority-based multi-level scheduler** with timer preemption. The run queue is a sorted map from priority level to a per-level FIFO:

```rust
pub static RUN_QUEUE: Mutex<BTreeMap<u8, VecDeque<u32>>> = Mutex::new(BTreeMap::new());
```

`BTreeMap` iteration is ordered by key, so the lowest key (= highest priority) is always popped first.

**Priority levels:**

| Constant | Value | Used by |
|----------|-------|---------|
| `PRIO_REALTIME` | 0 | (reserved) |
| `PRIO_HIGH` | 8 | Boot servers: pm-server, vfs-server, rs-server, procfs-server, devfs-server |
| `PRIO_NORMAL` | 16 | init, shell, all user processes, drivers spawned by servers |
| `PRIO_LOW` | 24 | (available for background tasks) |
| `PRIO_IDLE` | 31 | (reserved) |

**Priority assignment rules:**
- `kmain` spawns boot servers at `PRIO_NORMAL`, then explicitly raises them to `PRIO_HIGH` via `priority.store(PRIO_HIGH, …)` before entering the scheduler.
- All other processes — including children spawned by `PRIO_HIGH` servers — always start at `PRIO_NORMAL` regardless of the parent's priority. This prevents rs-server's driver children (net-smoltcp, virtio-blk-driver) from starving user processes.

**`schedule()` algorithm:**

```rust
pub fn schedule() -> ! {
    loop {
        disable_interrupts();
        let next_pid = {
            let mut rq = RUN_QUEUE.lock();
            // BTreeMap::keys() iterates in ascending order — lowest key = highest priority.
            let prio = rq.keys().next().copied();
            if let Some(p) = prio {
                let bucket = rq.get_mut(&p).unwrap();
                let pid = bucket.pop_front();
                if bucket.is_empty() { rq.remove(&p); }
                pid
            } else { None }
        };

        if let Some(pid) = next_pid {
            // Switch page table, set sscratch, return to user
            unsafe { return_to_user(trap_frame_ptr) }
        } else {
            // No runnable process — idle
            set_current_pid(0);
            enable_interrupts();
            unsafe { core::arch::asm!("wfi"); }
        }
    }
}
```

**`enqueue(pid)`** re-queues a process at its stored priority (`proc.priority.load()`). `enqueue_with_prio(pid, prio)` overrides the stored priority and updates `proc.priority` for future enqueues.

**Timer preemption:** The SBI timer fires every 10 ms. The ISR re-queues the current PID via `enqueue(pid)` (using the process's stored priority) but does **not** call `schedule()` from interrupt context. The reschedule happens at the next return from `return_to_user`.

**WFI idle:** When all priority buckets are empty, the HART executes `wfi` with interrupts enabled. The next timer tick, UART interrupt, or IPC wakeup will re-enter the scheduler.

### 6.3 Process Lifecycle

```
   spawn() / fork()
        │
        ▼
   [Runnable] ─── schedule() picks ──► [Running]
        ▲                                  │
        │    timer interrupt               │  syscall yield / blocking I/O
        │◄── re-queued ───────────────────┤
        │                                  │
        │                              [Blocked]
        │                                  │
        │                     event occurs │
        │◄─── re-queued ──────────────────┘
        │
     exit()
        │
        ▼
   [Zombie] ──── waitpid() from parent ──► process struct freed
```

### 6.4 Fork / Exec / Waitpid Semantics

**`fork()`:**
1. `clone_user_space` creates COW copy of address space.
2. Child `Process` struct is allocated; fields copied from parent.
3. Child's `TrapFrame` is copied from parent's; child's `a0` register set to 0 (fork returns 0 to child); child's `sepc` advanced by 4 (past the `ecall` instruction).
4. Child pushed to `RUN_QUEUE`.
5. Parent returns child PID from `sys_fork`.

**`exec(path)`:**
1. Read ELF file from VFS at `path`.
2. Allocate new root page table.
3. Load all `PT_LOAD` ELF segments into new page table (zero-fill `.bss`).
4. `destroy_user_space(old_root_pa)` — frees old address space.
5. Allocate user stack pages at `USER_STACK_TOP - STACK_SIZE`.
6. Set `satp` to new page table.
7. Return entry point to user (libos `_start` jumps to it after clearing registers).

**`waitpid(target, status_ptr, opts)`:**
1. If `proc.wait_result` is set (a child already exited while we ran), consume it and return immediately.
2. Scan `PROCESS_TABLE` for zombie children with matching `ppid`. If found, reap (copy exit code to `*status_ptr`, free process struct), return PID.
3. Set `proc.state = Blocked`. Call `schedule()`. When woken by a child's `exit()`, repeat from step 1.

### 6.5 Process Groups and Sessions

Each process carries `pgid` (process group ID) and `sid` (session ID) fields. These are set on `fork()` to inherit from the parent and on session-leader creation (`setsid` semantics in `init`).

> **Note:** In v1, `pgid` and `sid` are **stored but not enforced**. Job control signals (`SIGTSTP`, `SIGCONT`, `SIGHUP`) are not fully implemented. Ctrl-C is handled ad-hoc in the line discipline (see §10).

### 6.6 Credentials and setuid

Each process maintains four credential fields:

| Field  | Description                              |
|--------|------------------------------------------|
| `uid`  | Real user ID (who the process "is")      |
| `euid` | Effective user ID (used for access checks) |
| `gid`  | Real group ID                            |
| `egid` | Effective group ID                       |

**`setuid(uid)` semantics:**
- If `euid == 0`: sets both `uid` and `euid` to the supplied value. (Root can become any user.)
- If `euid != 0`: can only set `euid` to the saved real `uid`. (Non-root can drop elevated privilege.)

**setuid-bit on exec:** When `sys_exec` loads an ELF, it checks the file's `mode` for the setuid bit (`S_ISUID`). If set, `proc.euid` is changed to the **file owner's UID** before the new image begins executing. This enables classic setuid-root binaries (e.g., `sudo`).

---
[Back to Index](README.md)

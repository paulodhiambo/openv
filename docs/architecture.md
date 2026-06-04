# openv OS — Architecture Reference

> **Version:** v1 (RISC-V 64 / QEMU Virt)
> **Last Updated:** 2026-06
> **Authors:** openv contributors

---

## Table of Contents

1. [Overview](#1-overview)
2. [Boot Sequence](#2-boot-sequence)
3. [Memory Management](#3-memory-management)
   - 3.1 [Physical Memory Manager (PMM)](#31-physical-memory-manager-pmm)
   - 3.2 [Virtual Memory Manager (VMM)](#32-virtual-memory-manager-vmm)
   - 3.3 [Heap](#33-heap)
   - 3.4 [Virtual Memory Objects (VMO)](#34-virtual-memory-objects-vmo)
4. [Trap Handling](#4-trap-handling)
5. [Syscall Interface](#5-syscall-interface)
6. [Process Model](#6-process-model)
7. [Virtual File System](#7-virtual-file-system)
8. [IPC](#8-ipc)
9. [Networking](#9-networking)
10. [TTY / Line Discipline](#10-tty--line-discipline)
11. [SMP](#11-smp)
12. [User Space](#12-user-space)
13. [Security Model](#13-security-model)
14. [Linker Scripts](#14-linker-scripts)
15. [CI/CD](#15-cicd)
16. [Known Limitations and Future Work](#16-known-limitations-and-future-work)

---

## 1. Overview

**openv** is a RISC-V 64-bit microkernel-inspired operating system written entirely in Rust. It targets the QEMU `virt` machine and is compiled for the `riscv64gc-unknown-none-elf` bare-metal target, running on top of the OpenSBI firmware.

### Design Philosophy

openv follows a **pragmatic microkernel** approach:

- **In-kernel for v1:** Memory management (PMM + VMM), inter-process communication (IPC channels, pipes), trap dispatch, and POSIX-flavored syscalls live inside the kernel proper. This simplifies the v1 codebase and avoids costly context switches on every OS call.
- **Future trajectory:** The design is explicitly structured to allow POSIX-layer services (filesystem servers, network daemons, device managers) to be migrated into isolated userspace servers. Existing socket networking already demonstrates this pattern — the kernel provides raw Ethernet I/O, while TCP/IP semantics live in the `net-smoltcp` userspace daemon.
- **Safety over performance (initially):** Global mutexes protect shared state (PMM free-list, process table, VFS mount table). The goal is correctness first; fine-grained locking and lock-free structures are deferred to later revisions.

### Key Parameters

| Property               | Value                               |
|------------------------|-------------------------------------|
| ISA                    | RISC-V RV64GC                       |
| Compilation target     | `riscv64gc-unknown-none-elf`        |
| Firmware               | OpenSBI (M-mode)                    |
| Machine                | QEMU `virt`                         |
| Kernel load address    | `0x80200000`                        |
| VM scheme              | Sv39 (3-level page tables)          |
| Page size              | 4 KiB                               |
| Max RAM tracked        | 1 GiB                               |
| Max HARTs              | 4                                   |
| User load address      | `0x100000000` (4 GiB mark)          |
| User stack top         | `0x200000000` (8 GiB)              |

---

## 2. Boot Sequence

### High-Level Flow

```
OpenSBI (M-mode)
       │
       │  sets a0 = hartid
       │  sets a1 = DTB physical address
       │  jumps to 0x80200000
       ▼
   _start  (boot.s)
       │
       ├─ disable interrupts
       ├─ park secondary HARTs
       ├─ set up kernel stack
       ├─ clear BSS
       ├─ store hartid in tp
       └─ call kmain()
              │
              ├─ UART init
              ├─ PMM init  (DTB memory scan)
              ├─ trap init (stvec, sscratch)
              ├─ net init  (virtio probe)
              ├─ initrd parse
              ├─ VFS mount (TarFS, ProcFS, DevFS)
              ├─ heap test
              ├─ process table setup
              ├─ spawn init process (PID 1)
              ├─ enable timer interrupt
              └─ schedule()  ← never returns
```

### 2.1 OpenSBI Handoff

OpenSBI runs entirely in M-mode and provides the SBI runtime interface (HSM, timer, IPI extensions). Before jumping to the kernel:

- It places the **hart ID** of the boot hart in `a0`.
- It places the **Flattened Device Tree (FDT/DTB)** physical base address in `a1`.
- It transfers control to `0x80200000` in S-mode.

### 2.2 `_start` — Assembly Entry Point (`boot.s`)

```asm
_start:
    # Disable all S-mode interrupts
    csrw    sie, zero

    # Park secondary HARTs: if hartid != 0, jump to park loop
    mv      s0, a0                  # save hartid
    bnez    s0, secondary_park

    # Set stack pointer to top of BSS-defined kernel stack
    la      sp, _stack_end

    # Zero out the BSS segment
    la      t0, _bss_start
    la      t1, _bss_end
bss_loop:
    sd      zero, 0(t0)
    addi    t0, t0, 8
    blt     t0, t1, bss_loop

    # Store boot hartid in tp (thread pointer, per-HART register)
    mv      tp, a0

    # Jump into Rust: kmain(hartid, dtb_ptr)
    call    kmain

secondary_park:
    # Spin until SMP_GO_FLAG is set by primary HART
    la      t0, SMP_GO_FLAG
1:  lb      t1, 0(t0)
    beqz    t1, 1b

    # Compute per-HART stack: base = SECONDARY_STACKS + hartid * 16384
    la      t0, SECONDARY_STACKS
    li      t1, 16384
    mul     t1, s0, t1
    add     sp, t0, t1
    add     sp, sp, t1              # sp = top of this hart's stack region

    mv      tp, s0
    call    secondary_kmain
```

**Key invariants established in `_start`:**
- `sie = 0` — no interrupts until the kernel is ready.
- BSS is zeroed before any Rust code runs (Rust's ownership model requires initialized memory).
- `tp` holds the current HART's ID for the duration of execution; this is the canonical way to identify the current processor throughout the kernel.
- Only HART 0 proceeds to `kmain`; all others spin on `SMP_GO_FLAG`.

### 2.3 `kmain` — Primary Boot Sequence

```
kmain(hartid: usize, dtb_ptr: *const u8)
  │
  ├── uart::init()
  │     Initialises UART0 at MMIO base 0x10000000 (QEMU virt)
  │
  ├── mm::init(dtb_ptr)
  │     ├─ Parse DTB /memory node for RAM base + size
  │     ├─ Build PMM free-list (exclude kernel, FDT, initrd)
  │     ├─ Set up Sv39 kernel identity map (1GB superpages)
  │     └─ Initialise heap (16 MB from PMM)
  │
  ├── trap::init()
  │     ├─ csrw stvec, trap_vector  (Direct mode)
  │     └─ Set sscratch for HART 0
  │
  ├── net::init()
  │     Probe virtio-mmio bus for network device
  │
  ├── initrd::parse(dtb_ptr)
  │     Locate initrd region from DTB chosen node
  │
  ├── vfs::mount_all()
  │     ├─ Mount TarFS at "/"       (backed by initrd)
  │     ├─ Mount ProcFS at "/proc"
  │     └─ Mount DevFS at "/dev"
  │
  ├── heap_test()           (debug builds only)
  │
  ├── process::init_table()
  │     Allocate PID 0 (idle process)
  │
  ├── process::spawn("/sbin/init")
  │     ELF load → PID 1 pushed to RUN_QUEUE
  │
  ├── timer::enable()
  │     Set first SBI timer call; enable supervisor timer interrupt
  │
  └── sched::schedule()     ← enters scheduler, never returns
```

### 2.4 Secondary HART Wakeup

After the primary HART completes `mm::init()` and `trap::init()`, it atomically sets `SMP_GO_FLAG`. Each secondary HART:

1. Reads `SMP_GO_FLAG` until non-zero.
2. Computes its private 16 KiB stack from `SECONDARY_STACKS[hartid * 16384]`.
3. Calls `secondary_kmain(hartid)`, which:
   - Initialises `sscratch` (TrapFrame pointer) for that HART.
   - Enables supervisor timer interrupt for that HART.
   - Calls `sched::schedule()`.

Each HART independently runs the scheduler and may be assigned any runnable process.

---

## 3. Memory Management

### 3.1 Physical Memory Manager (PMM)

The PMM is responsible for tracking free 4 KiB physical pages and providing allocation/deallocation services to the rest of the kernel.

#### RAM Discovery

During `mm::init()`, the DTB is parsed to find the `/memory` node's `reg` property, which encodes one or more `(base, size)` pairs describing physical RAM. On the QEMU `virt` machine, this is typically a single contiguous region beginning at `0x80000000`.

#### Exclusion Regions

Before building the free-list, the following physical regions are excluded from allocation:

| Region            | Bounds                              | Reason                                      |
|-------------------|-------------------------------------|---------------------------------------------|
| Kernel image      | `0x80200000` – `_stack_end` (page-aligned) | Kernel code, data, BSS, and stack    |
| FDT/DTB           | DTB base – DTB base + DTB size      | Required by secondary boot and for queries  |
| initrd            | initrd base – initrd base + size    | TarFS source data; must remain intact       |

#### Free-List Implementation

The PMM uses an **intrusive singly-linked list** stored entirely within the freed pages themselves:

```
┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│ next_ptr ────┼───►│ next_ptr ────┼───►│   NULL       │
│              │    │              │    │              │
│  (free page) │    │  (free page) │    │  (free page) │
└──────────────┘    └──────────────┘    └──────────────┘
        ▲
   FREE_LIST_HEAD
```

Each free 4 KiB page's **first 8 bytes** hold the physical address of the next free page (or `0` for end-of-list). This requires zero additional metadata storage.

#### Allocation

```rust
pub fn alloc_page() -> Option<usize> {
    let mut list = FREE_LIST.lock();
    let pa = list.head?;
    // Read next pointer from the page itself
    let next = unsafe { *(pa as *const usize) };
    list.head = if next == 0 { None } else { Some(next) };
    // Zero the page before returning
    unsafe { core::ptr::write_bytes(pa as *mut u8, 0, PAGE_SIZE) };
    // Set initial refcount to 1
    PAGE_REF_COUNTS.lock()[(pa - RAM_START) / PAGE_SIZE] = 1;
    Some(pa)
}
```

- Returns the physical address of the allocated page.
- **Zeroes the page** before returning (prevents information leaks between processes).
- Sets the reference count to 1.

#### Deallocation

```rust
pub fn free_page(pa: usize) {
    debug_assert_eq!(PAGE_REF_COUNTS.lock()[(pa - RAM_START) / PAGE_SIZE], 0,
        "free_page called on page with non-zero refcount");
    let mut list = FREE_LIST.lock();
    unsafe { *(pa as *mut usize) = list.head.unwrap_or(0) };
    list.head = Some(pa);
}
```

The caller is responsible for ensuring the refcount reaches zero before calling `free_page`.

#### Reference Counting

```rust
static PAGE_REF_COUNTS: Mutex<[u16; 262144]> = Mutex::new([0; 262144]);
```

- 262144 entries × 2 bytes = 512 KiB static array.
- Covers up to **1 GiB** of RAM (`262144 × 4096 = 1 GiB`).
- Index formula: `(pa - RAM_START) / 4096`.
- `u16` allows up to 65535 simultaneous sharers of a single page (sufficient for fork trees).

```rust
pub fn incr_ref(pa: usize) {
    PAGE_REF_COUNTS.lock()[(pa - RAM_START) / PAGE_SIZE] += 1;
}

pub fn decr_ref(pa: usize) -> u16 {
    let mut counts = PAGE_REF_COUNTS.lock();
    let idx = (pa - RAM_START) / PAGE_SIZE;
    counts[idx] -= 1;
    let remaining = counts[idx];
    if remaining == 0 { free_page(pa); }
    remaining
}
```

`incr_ref` / `decr_ref` are called by the COW fork implementation in `clone_user_space` and `handle_store_page_fault`.

---

### 3.2 Virtual Memory Manager (VMM)

#### Sv39 Address Format

openv uses the RISC-V **Sv39** paging scheme: a three-level radix page table where each virtual address is interpreted as:

```
 63      39 38    30 29    21 20    12 11          0
┌──────────┬────────┬────────┬────────┬─────────────┐
│ (sign-ex)│ VPN[2] │ VPN[1] │ VPN[0] │page offset  │
│  25 bits │  9 bits│  9 bits│  9 bits│   12 bits   │
└──────────┴────────┴────────┴────────┴─────────────┘
```

Each page table level has 512 entries of 8 bytes, fitting exactly in one 4 KiB page.

#### PTE Format

```
 63    54 53     28 27     19 18     10 9 8 7 6 5 4 3 2 1 0
┌────────┬─────────┬─────────┬─────────┬───┬─┬─┬─┬─┬─┬─┬─┐
│  RSVD  │  PPN[2] │  PPN[1] │  PPN[0] │RSW│D│A│G│U│X│W│R│V│
└────────┴─────────┴─────────┴─────────┴───┴─┴─┴─┴─┴─┴─┴─┘
                                                         └─ V = Valid
                                                       └─── R = Readable
                                                     └───── W = Writable
                                                   └─────── X = Executable
                                                 └───────── U = User-accessible
```

Flags used by openv:

| Flag    | Constant   | Meaning                                      |
|---------|------------|----------------------------------------------|
| `V`     | `PTE_V`    | Entry is valid                               |
| `R`     | `PTE_R`    | Readable                                     |
| `W`     | `PTE_W`    | Writable                                     |
| `X`     | `PTE_X`    | Executable                                   |
| `U`     | `PTE_U`    | Accessible from U-mode (user processes)      |
| `D`     | `PTE_D`    | Dirty (hardware-set on write)                |
| `A`     | `PTE_A`    | Accessed (hardware-set on read)              |

#### Kernel Identity Map

The kernel establishes a static identity mapping at boot using **1 GiB superpages** at the root (VPN[2]) level:

```
Root page table (512 entries):
  Index 0  → 1GB superpage: PA 0x00000000 – 0x3FFFFFFF  (R+W+X)
  Index 1  → 1GB superpage: PA 0x40000000 – 0x7FFFFFFF  (R+W+X)
  Index 2  → 1GB superpage: PA 0x80000000 – 0xBFFFFFFF  (R+W+X) ← RAM here
  Index 3  → 1GB superpage: PA 0xC0000000 – 0xFFFFFFFF  (R+W+X) ← MMIO here
  Index 4–511 → reserved for user space
```

- **`PTE_U` is NOT set** on kernel superpages — user processes cannot access kernel memory.
- The identity map covers all MMIO (UART, PLIC, virtio) without special mappings.
- Kernel runs with `satp` pointing to this root page table at all times; it does not switch page tables on kernel entry.

#### `map_page(va, pa, flags)`

Maps a single 4 KiB virtual page to a physical page:

```
1. Extract VPN[2] from va → index into root page table
2. If root[VPN[2]].V == 0:
       Allocate new L1 table from PMM
       Install pointer PTE at root[VPN[2]]
3. Extract VPN[1] → index into L1 table
4. If L1[VPN[1]].V == 0:
       Allocate new L2 table from PMM
       Install pointer PTE at L1[VPN[1]]
5. Extract VPN[0] → index into L2 table
6. Install leaf PTE at L2[VPN[0]]:
       pte = (pa >> 12) << 10 | flags | PTE_V
```

Intermediate tables always have `V=1`, `R=0`, `W=0`, `X=0` (pointer PTEs, not leaf PTEs).

#### Copy-on-Write Fork: `clone_user_space`

`clone_user_space(parent_root_pa, child_root_pa)` performs a shallow COW clone of the parent's address space:

```
For each root index 4–511 (skipping kernel superpages 0–3):
  If root entry is valid:
    Allocate child L1 table; copy L1 structure
    For each L1 entry:
      If valid:
        Allocate child L2 table; copy L2 structure
        For each L2 leaf PTE:
          If valid and PTE_U set:
            ┌─ Clear PTE_W in parent PTE
            ├─ Copy PTE to child (also without PTE_W)
            └─ incr_ref(physical_page)  ← both parent and child share this page
```

After `clone_user_space`:
- Both parent and child have **read-only mappings** to the same physical pages.
- Any write attempt causes a Store Page Fault, triggering COW resolution.
- The parent's TLB must be flushed (`sfence.vma`) to honour the newly-read-only PTEs.

#### COW Resolution: `handle_store_page_fault(va)`

Called from the trap handler when a store page fault occurs on a COW page:

```
1. Walk current process's page table to find PTE for `va`
2. Assert PTE_V set, PTE_W clear, PTE_U set  (it's a COW page)
3. pa_old = PTE → physical address
4. refcount = PAGE_REF_COUNTS[(pa_old - RAM_START) / 4096]

5. If refcount == 1:
       # We are the only owner; just make it writable in-place
       PTE |= PTE_W
6. Else:
       # Must copy: another process still shares this page
       pa_new = alloc_page()
       memcpy(pa_new, pa_old, 4096)
       PTE = (pa_new >> 12) << 10 | PTE_V | PTE_R | PTE_W | PTE_U
       decr_ref(pa_old)   ← decrements old page; frees if it drops to 0

7. sfence.vma  ← flush TLB for this address
```

#### Demand Paging: `handle_user_page_fault(va)`

Load or instruction page faults in user space trigger demand paging:

```
1. Walk page table for `va`
2. If PTE missing or not valid:
       pa = alloc_page()       ← already zeroed by alloc_page
       map_page(va_aligned, pa, PTE_V | PTE_R | PTE_U)
       sfence.vma
3. If PTE valid but not executable:
       Add PTE_X (for instruction fault)
```

#### Address Space Teardown: `destroy_user_space(root_pa)`

Called on process exit or exec:

```
For each root index 4–511:
  For each L1 entry:
    For each L2 leaf PTE:
      If PTE_V and PTE_U:
        decr_ref(leaf_physical_page)   ← frees page if refcount → 0
    free_page(L2_table_pa)
  free_page(L1_table_pa)
```

Kernel superpages (indices 0–3) are **never freed** — they belong to the global kernel map.

---

### 3.3 Heap

The kernel requires dynamic allocation for `Vec`, `String`, `Arc`, `BTreeMap`, and other standard library types. openv uses the `buddy_system_allocator` crate:

```rust
#[global_allocator]
static HEAP: buddy_system_allocator::LockedHeap<32> =
    buddy_system_allocator::LockedHeap::empty();
```

**Initialization (inside `mm::init`):**

```rust
// Allocate 16 MB of contiguous physical pages from PMM
const HEAP_PAGES: usize = 4096;   // 4096 × 4KiB = 16 MiB
let heap_start = alloc_page().expect("heap start");
for _ in 1..HEAP_PAGES {
    let next = alloc_page().expect("heap page");
    // Pages are physically contiguous because PMM was built from
    // a contiguous RAM region; assert adjacency in debug builds
    debug_assert_eq!(next, previous + PAGE_SIZE);
}
unsafe {
    HEAP.lock().init(heap_start, HEAP_PAGES * PAGE_SIZE);
}
```

The buddy allocator operates on the identity-mapped virtual addresses (identical to physical addresses inside the kernel) and supports `alloc`/`dealloc` with `O(log N)` complexity.

---

### 3.4 Virtual Memory Objects (VMO)

```rust
pub struct Vmo {
    /// Physical page addresses backing this object, in order
    pub pages: Vec<usize>,
}
```

A VMO represents a logically contiguous virtual region backed by a list of physical pages. VMOs are the intended primitive for:

- **Shared memory IPC:** A VMO can be mapped into multiple address spaces with different permissions.
- **Memory-mapped files:** A VMO page can be backed by a file block rather than an anonymous page.
- **Large contiguous allocations:** DMA buffers, framebuffers, etc.

In v1, VMOs are allocated but not yet mapped via a dedicated `mmap`-style syscall. They are reserved as the backing store for future shared-memory IPC.

---

## 4. Trap Handling

### 4.1 Trap Vector Setup

During `trap::init()`, the `stvec` CSR is written with the address of `trap_vector` in **Direct mode** (bits [1:0] = 00):

```rust
csrw!(stvec, trap_vector as usize);
```

The `sscratch` CSR is used as a pointer to the current process's `TrapFrame`. On kernel entry, the hardware preserves `sscratch`, allowing the assembly trampoline to locate the trap frame without depending on any other register.

### 4.2 TrapFrame Layout

```rust
#[repr(C)]
pub struct TrapFrame {
    pub kernel_sp: usize,       // offset 0:  saved kernel stack pointer
    pub regs: [usize; 32],      // offset 8:  x0–x31 (x0 always 0)
    pub sepc:    usize,         // offset 264: saved program counter
    pub sstatus: usize,         // offset 272: saved status register
}
```

The `TrapFrame` lives at the **bottom of the kernel stack** for each process. `sscratch` always holds the pointer to the currently-running process's `TrapFrame`.

### 4.3 Assembly Trampoline (`trap_vector`)

```asm
trap_vector:
    # Exchange sp and sscratch:
    #   Before: sp = user stack, sscratch = TrapFrame ptr
    #   After:  sp = TrapFrame ptr, sscratch = user stack
    csrrw   sp, sscratch, sp

    # Save all 32 general-purpose registers into TrapFrame.regs
    sd      x0,   8(sp)       # x0 is always 0 but save for uniformity
    sd      x1,  16(sp)       # ra
    # ... (x2/sp is special — saved via sscratch below)
    sd      x3,  32(sp)       # gp
    # ... all other registers ...
    sd      x31, 256(sp)

    # Recover user sp from sscratch and save it at regs[2]
    csrr    t0, sscratch
    sd      t0, 24(sp)

    # Save sepc and sstatus
    csrr    t0, sepc
    sd      t0, 264(sp)
    csrr    t0, sstatus
    sd      t0, 272(sp)

    # Call Rust trap handler with pointer to TrapFrame
    mv      a0, sp
    call    rust_trap_handler

    # Fall through to return_to_user
```

### 4.4 `rust_trap_handler`

```rust
#[no_mangle]
pub extern "C" fn rust_trap_handler(tf: &mut TrapFrame) {
    let cause = csrr!(scause);
    let is_interrupt = cause >> 63 == 1;
    let code = cause & !(1 << 63);

    match (is_interrupt, code) {
        // ── Exceptions ──────────────────────────────────────────────────
        (false, 8)  => syscall_dispatch(tf),          // U-mode ecall
        (false, 12) => handle_user_page_fault(stval()), // Instruction PF
        (false, 13) => handle_user_page_fault(stval()), // Load PF
        (false, 15) => handle_store_page_fault(stval()), // Store PF (COW)

        // ── Interrupts ──────────────────────────────────────────────────
        (true, 5)   => {                              // Supervisor timer
            rearm_timer();
            enqueue_current_pid();
            // NOTE: do NOT call schedule() here — we hold the timer
            // interrupt with sip.STIP set; re-entering the scheduler
            // from within the interrupt handler risks lock re-entrancy.
        },
        (true, 1)   => {                              // Supervisor software
            // TLB shootdown IPI
            unsafe { core::arch::asm!("sfence.vma"); }
            clear_ssip();
        },
        (true, 9) | (true, 11) => {                  // External interrupt
            let irq = plic::claim();
            driver_dispatch(irq);
            plic::complete(irq);
        },

        _ => panic!("unhandled trap: cause={:#x} stval={:#x}", cause, stval()),
    }
}
```

### 4.5 Return to User (`return_to_user`)

```asm
return_to_user:
    # Restore sepc and sstatus
    ld      t0, 264(sp)
    csrw    sepc, t0
    ld      t0, 272(sp)
    csrw    sstatus, t0

    # Restore user sp into sscratch before restoring all regs
    ld      t0, 24(sp)          # regs[2] = user sp
    csrw    sscratch, t0

    # Restore all registers except sp
    ld      x1,  16(sp)
    ld      x3,  32(sp)
    # ... all registers ...
    ld      x31, 256(sp)

    # Swap sp back: sp = user sp, sscratch = TrapFrame ptr (kernel)
    csrrw   sp, sscratch, sp

    sret                        # return to S-mode user code, restore sstatus
```

After `sret`:
- `pc` ← `sepc` (next instruction after `ecall` / faulting instruction)
- `sstatus.SPP` ← U-mode
- Interrupts re-enabled per `sstatus.SPIE`

---

## 5. Syscall Interface

All syscalls use the RISC-V `ecall` convention:
- `a7` = syscall number
- `a0`–`a3` = up to 4 arguments
- `a0` = return value (negative values indicate errors)

The assembly wrapper in libos:

```rust
#[inline(always)]
pub unsafe fn syscall(num: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "ecall",
        in("a7") num,
        inlateout("a0") a0 => ret,
        in("a1") a1,
        in("a2") a2,
        options(nostack)
    );
    ret
}
```

### Complete Syscall Table

| No. | Name | Signature | Returns | Description |
|-----|------|-----------|---------|-------------|
| 0 | `yield` | `()` | `0` | Yields the CPU. Re-queues the current process at the back of the run queue and calls `schedule()`. |
| 1 | `exit` | `(status: i32)` | *(never)* | Marks process as zombie with exit code `status`. Orphans all children (re-parents to PID 1). Wakes the parent if it is blocked in `waitpid`. Calls `schedule()` and never returns. |
| 2 | `write` | `(fd: usize, buf: *const u8, len: usize)` | `isize` | **Console fd:** UART print. **Channel fd:** enqueue message into channel. **File fd:** write at current offset, advance offset. **PipeWrite fd:** push bytes to internal `VecDeque<u8>`. Returns bytes written or negative error. |
| 3 | `pipe` | `(fds: *mut [u32; 2])` | `0 / -1` | Creates a `PipeRead`/`PipeWrite` pair. Inserts both into the current process's FD table at lowest-free positions. Writes the two FD numbers to the user-supplied pointer. |
| 5 | `read` | `(fd: usize, buf: *mut u8, max: usize)` | `isize` | **Console (cooked):** accumulate until `\n`, return line. **Console (raw):** return single byte immediately. **Channel fd:** `try_recv` or yield and retry. **File fd:** read at current offset, advance. **PipeRead:** dequeue or yield if empty. |
| 6 | `spawn` | `(path: *const u8, len: usize)` | `isize` (pid) | `posix_spawn`-style: creates new `Process`, loads ELF from VFS, allocates user stack, pushes to `RUN_QUEUE`. Returns child PID or negative error. |
| 8 | `open` | `(path: *const u8, len: usize, flags: u32)` | `isize` (fd) | VFS path resolution → access check (`check_access`) → creates `FileDescription` → inserts into `HandleTable`. Returns new FD. |
| 9 | `close` | `(fd: usize)` | `0 / -1` | Removes entry from `HandleTable`. Drops the `Arc<KernelObject>`. If last reference to `PipeWrite` sentinel `Arc<()>`, readers will see EOF. |
| 10 | `net_send` | `(buf: *const u8, len: usize)` | `isize` | Sends a raw Ethernet frame via the `NetDevice` driver. Used by the `net-smoltcp` daemon. |
| 11 | `net_recv` | `(buf: *mut u8, max: usize)` | `isize` | Receives a raw Ethernet frame from the `NetDevice` driver. Blocks until a frame is available. |
| 12 | `getdents` | `(path: *const u8, path_len: usize, buf: *mut u8, max: usize)` | `isize` | Reads directory entries from the VFS path into `buf` as a sequence of null-terminated name strings. |
| 23 | `setuid` | `(uid: u32)` | `0 / -1` | If `euid == 0` (root): sets both `uid` and `euid`. If non-root: can only set `euid` back to saved `ruid`. Mirrors POSIX `setuid(2)`. |
| 24 | `setgid` | `(gid: u32)` | `0 / -1` | Same pattern as `setuid` but for group credentials. |
| 26 | `create` | `(path: *const u8, len: usize)` | `isize` (fd) | `lookup_parent` to find parent directory → `vnode.create(name)` → returns writable FD. Fails if parent not found or permission denied. |
| 27 | `mkdir` | `(path: *const u8, len: usize)` | `0 / -1` | `lookup_parent` + `vnode.mkdir(name)`. Creates an empty directory. |
| 28 | `unlink` | `(path: *const u8, len: usize)` | `0 / -1` | `lookup_parent` + `vnode.unlink(name)`. Removes a file entry; last `Arc` drop frees data. |
| 29 | `rename` | `(old: *const u8, old_len: usize, new: *const u8, new_len: usize)` | `0 / -1` | `lookup_parent` on both paths. Calls `vnode.rename(old_name, new_name)`. **Cross-directory rename is not yet supported.** |
| 30 | `getuid` | `()` | `u32` | Returns `proc.uid` (real user ID). |
| 31 | `geteuid` | `()` | `u32` | Returns `proc.euid` (effective user ID). |
| 32 | `getgid` | `()` | `u32` | Returns `proc.gid` (real group ID). |
| 33 | `getegid` | `()` | `u32` | Returns `proc.egid` (effective group ID). |
| 34 | `authenticate` | `(user: *const u8, user_len: usize, pass: *const u8, pass_len: usize)` | `isize` (uid/-1) | Looks up username in the user database, hashes the supplied password with FNV-1a 64-bit, compares against stored hash. Returns `uid` on success, `-1` on failure. |
| 35 | `can_sudo` | `(uid: u32)` | `1 / 0` | Returns 1 if `uid == 0` (root) or if the user is a member of the sudo group (GID 27). |
| 37 | `set_echo` | `(enable: bool)` | `0` | Sets or clears the `ECHO_ENABLED` global flag in the line discipline. |
| 38 | `set_raw` | `(enable: bool)` | `0` | Toggles `RAW_MODE`. In raw mode, `sys_read` on the console returns single characters immediately without buffering or line editing. |
| 40 | `socket` | `()` | `isize` (fd) | Creates a channel pair representing a socket. Registers the socket with the kernel socket registry. Returns the user-facing FD. |
| 41 | `daemon_next_socket` | `()` | `isize` (socket_id/-1) | Called by the `net-smoltcp` daemon. Pops the next pending socket from the registry queue. Returns the socket ID or `-1` if none. |
| 42 | `daemon_create_conn` | `(listen_sid: usize)` | `isize` | Called by daemon after accepting a TCP connection. Creates a new accepted-connection channel pair and delivers the peer end to the process that owns `listen_sid`. |
| 43 | `accept` | `(listen_fd: usize)` | `isize` (fd) | Blocks until the daemon delivers an accepted connection for this listening socket. Returns the new connected FD. |
| 44 | `bind` | `(fd: usize, addr: *const u8, addr_len: usize)` | `0 / -1` | Sends a `BIND` opcode message to the net daemon via the socket's channel. |
| 45 | `listen` | `(fd: usize, backlog: i32)` | `0 / -1` | Sends a `LISTEN` opcode message to the net daemon. |
| 46 | `connect` | `(fd: usize, addr: *const u8, addr_len: usize)` | `0 / -1` | Sends a `CONNECT` opcode message to the net daemon. |
| 47 | `sock_send` | `(fd: usize, buf: *const u8, len: usize)` | `isize` | Sends a `SEND` opcode with data payload to the net daemon channel. |
| 48 | `sock_recv` | `(fd: usize, buf: *mut u8, max: usize)` | `isize` | Tries to receive from the socket's channel. Yields and retries if no data is available. |
| 50 | `fork` | `()` | `isize` (pid/0) | Full COW fork: `clone_user_space` for address space, copy `TrapFrame` (set child `a0 = 0`, advance child `sepc` by 4), allocate new `Process` struct, push child to `RUN_QUEUE`. Returns child PID to parent, 0 to child. |
| 51 | `exec` | `(path: *const u8, len: usize)` | `isize` (entry/-1) | Reads ELF from VFS. Builds a new page table. Loads ELF segments. Calls `destroy_user_space` on old page table. Sets `satp` to new page table. Returns entry point address (user jumps to it). |
| 52 | `waitpid` | `(target: i32, status: *mut i32, opts: u32)` | `isize` (pid/-1) | Three-step: (1) check if `proc.wait_result` is already set; (2) scan zombie children and reap them; (3) block current process until a child exits. `target == -1` waits for any child. |
| 53 | `getpid` | `()` | `u32` | Returns the current process's PID. |
| 54 | `getppid` | `()` | `u32` | Returns the current process's parent PID. |
| 55 | `chdir` | `(path: *const u8, len: usize)` | `0 / -1` | Resolves path via VFS, verifies it is a directory, updates `proc.cwd`. |
| 56 | `getcwd` | `(buf: *mut u8, max: usize)` | `isize` | Copies `proc.cwd` string to user buffer. Returns length or negative on error. |
| 57 | `dup` | `(oldfd: usize)` | `isize` (newfd) | Clones the `KernelObject` at `oldfd`. Inserts the clone at the lowest available FD in `HandleTable`. Returns new FD. |
| 58 | `dup2` | `(oldfd: usize, newfd: usize)` | `isize` (newfd) | Clones the `KernelObject` at `oldfd`. Closes `newfd` if already open. Inserts clone at exactly `newfd`. Returns `newfd`. |

---

## 6. Process Model

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

openv uses a simple **FIFO round-robin** scheduler with timer preemption:

```
┌─────────────────────────────────────────────────────┐
│                    RUN_QUEUE                         │
│  [ PID 1 ] → [ PID 3 ] → [ PID 5 ] → [ PID 2 ] →  │
└─────────────────────────────────────────────────────┘
         │
    schedule() pops front PID
    sets satp, tp, sscratch
    calls return_to_user(trap_frame)
```

**`schedule()` algorithm:**

```rust
pub fn schedule() -> ! {
    loop {
        let pid = RUN_QUEUE.lock().pop_front();
        match pid {
            Some(pid) => {
                let proc = PROCESS_TABLE.lock().get(pid);
                // Switch page table
                csrw!(satp, proc.satp_val);
                sfence_vma();
                // Point sscratch at this process's TrapFrame
                csrw!(sscratch, proc.trap_frame as usize);
                CURRENT_PIDS[current_hart()].store(pid as i32, Ordering::Relaxed);
                unsafe { return_to_user(proc.trap_frame) }
            }
            None => {
                // No runnable process — wait for interrupt (WFI)
                unsafe { core::arch::asm!("wfi"); }
            }
        }
    }
}
```

**Timer preemption:** The SBI timer fires every N milliseconds (configurable). The timer interrupt handler re-queues the current PID at the back of `RUN_QUEUE` but does **not** call `schedule()` from within the interrupt handler (to avoid lock re-entrancy). Instead, the process is transparently re-queued and will be rescheduled after the next `return_to_user` completes the current quantum.

**WFI idle:** When `RUN_QUEUE` is empty, the primary HART executes `wfi`. Any interrupt (timer, UART RX, network) will wake it. This avoids busy-spinning.

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

## 7. Virtual File System

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

| Mount point | Filesystem | Backing data       |
|-------------|------------|--------------------|
| `/`         | TarFS      | initrd TAR archive |
| `/proc`     | ProcFS     | Live kernel state  |
| `/dev`      | DevFS      | Synthetic devices  |

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

### 7.7 File Access Control

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

## 8. IPC

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

## 9. Networking

### 9.1 Architecture Overview

```
┌──────────────────────────────────────────────────┐
│                  User Space                       │
│                                                   │
│  Application ─► sys_connect/bind/send/recv        │
│       │                                           │
│       ▼  (kernel channels)                        │
│  net-smoltcp daemon                               │
│       │  smoltcp TCP/IP stack                     │
│       │  sys_net_send / sys_net_recv              │
└───────┼──────────────────────────────────────────┘
        │  (raw Ethernet frames)
┌───────┼──────────────────────────────────────────┐
│  Kernel                                           │
│       ▼                                           │
│  virtio-mmio NIC driver                           │
│       │  MMIO registers + descriptor rings        │
│       ▼                                           │
│  QEMU virtio-net device                           │
└──────────────────────────────────────────────────┘
```

The kernel provides **raw Ethernet frame I/O** only. All protocol processing (ARP, IP, TCP, UDP) lives in the `net-smoltcp` userspace daemon. This is the microkernel pattern applied to networking.

### 9.2 Virtio-mmio Driver

The virtio-net device is accessed via the **legacy virtio-mmio interface** (version 1):

**MMIO Register Map (offset from device base):**

| Offset | Register          | Description                         |
|--------|-------------------|-------------------------------------|
| 0x000  | `MagicValue`      | Must read `0x74726976` ("virt")     |
| 0x004  | `Version`         | Must be 1 (legacy)                  |
| 0x008  | `DeviceID`        | 1 = network device                  |
| 0x00C  | `VendorID`        | `0x554D4551` ("QEMU")               |
| 0x010  | `DeviceFeatures`  | Device-supported features bitmask   |
| 0x020  | `DriverFeatures`  | Features driver wishes to negotiate |
| 0x028  | `GuestPageSize`   | Must write 4096                     |
| 0x030  | `QueueSel`        | Select which virtqueue to configure |
| 0x034  | `QueueNumMax`     | Maximum queue size (read-only)      |
| 0x038  | `QueueNum`        | Set queue size                      |
| 0x03C  | `QueueAlign`      | Queue alignment                     |
| 0x040  | `QueuePFN`        | Queue physical page number          |
| 0x050  | `QueueNotify`     | Write queue index to trigger device |
| 0x060  | `InterruptStatus` | Bit 0: used buffer notification     |
| 0x064  | `InterruptACK`    | Write to acknowledge interrupt      |
| 0x070  | `Status`          | Driver status bits                  |

**Initialization sequence:**

```
1. Write Status = 0                    (reset)
2. Write Status |= ACKNOWLEDGE (1)
3. Write Status |= DRIVER (2)
4. Read DeviceFeatures; negotiate subset; write DriverFeatures
5. Write GuestPageSize = 4096
6. Configure virtqueues 0 (RX) and 1 (TX):
   - Write QueueSel = queue_index
   - Read QueueNumMax; write QueueNum = min(QueueNumMax, 256)
   - Allocate descriptor ring, available ring, used ring
   - Write QueuePFN = ring_physical_addr >> 12
7. Write Status |= FEATURES_OK (8)
8. Write Status |= DRIVER_OK (4)
```

**Descriptor ring (split virtqueue):**

```
Descriptor table: array of VirtqDesc
  { addr: u64, len: u32, flags: u16, next: u16 }

Available ring:
  { flags: u16, idx: u16, ring: [u16; N] }

Used ring:
  { flags: u16, idx: u16, ring: [VirtqUsedElem; N] }
    where VirtqUsedElem = { id: u32, len: u32 }
```

**TX path:** Place frame in descriptor → add to available ring → write `QueueNotify = 1` → poll used ring for completion.

**RX path:** Pre-fill descriptor ring with receive buffers → device fills them and adds to used ring → driver reads frames from used ring entries.

### 9.3 `net-smoltcp` Userspace Daemon

The `net-smoltcp` daemon is a userspace process (typically PID 2, spawned by `init`):

```
net-smoltcp startup:
  1. sys_net_recv / sys_net_send → raw Ethernet I/O
  2. smoltcp::iface::Interface created with EthernetInterface
  3. Loop:
       a. Poll smoltcp interface (processes timers, ARP, TCP state machines)
       b. sys_daemon_next_socket() → get newly registered socket IDs
       c. For each pending socket: process BIND/LISTEN/CONNECT opcodes
          from the socket's kernel channel
       d. For each TCP socket with received data: forward via kernel channel
          to the waiting application process
       e. sys_net_recv() → inject new Ethernet frames into smoltcp
       f. sys_net_send() → drain frames queued by smoltcp
```

The daemon uses smoltcp's TCP/IP state machine. Applications communicate with it exclusively through kernel channels — they never see raw Ethernet frames.

### 9.4 Socket Lifecycle

```
Application              Kernel                    net-smoltcp daemon
──────────               ──────                    ──────────────────
sys_socket()
  │                SocketRegistry.register(sid)
  │                Creates ChannelPair
  │                Returns user fd
  │
  │                                          sys_daemon_next_socket()
  │                                            ← pops sid from registry queue
  │
sys_bind(fd, addr)
  │                Sends BIND opcode msg
  │                  to socket's channel
  │                                          Reads BIND from channel
  │                                          Creates smoltcp socket
  │                                          Binds to addr
  │
sys_listen(fd, backlog)
  │                Sends LISTEN opcode
  │                                          Reads LISTEN
  │                                          Sets smoltcp socket to listen mode
  │
[incoming TCP connection]
  │                                          smoltcp accepts connection
  │                                          sys_daemon_create_conn(listen_sid)
  │                  Creates new ChannelPair for conn
  │                  Delivers server end to application (via wake)
  │
sys_accept(fd)
  │ ← blocks      Woken; returns new conn fd
  │
sys_sock_recv(conn_fd)
  │                                          Data arrives → sys_write to channel
  │ ← data                ← channel msg
```

---

## 10. TTY / Line Discipline

The TTY system handles keyboard input and terminal semantics.

### Global State

```rust
static LINE_DISC_BUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static ECHO_ENABLED: AtomicBool = AtomicBool::new(true);
static RAW_MODE:     AtomicBool = AtomicBool::new(false);
```

### Cooked Mode (default)

When `RAW_MODE = false`, `sys_read` on the console:

```
loop:
    ch = uart::try_get_char()   // non-blocking UART RX
    if ch is None: yield CPU and retry

    match ch:
        '\n' | '\r':
            push '\n' to LINE_DISC_BUFFER
            if ECHO_ENABLED: uart::put_char('\n')
            return LINE_DISC_BUFFER contents to caller; clear buffer

        '\x08' | '\x7F':        // Backspace or DEL
            if buffer not empty:
                buffer.pop()
                if ECHO_ENABLED: uart::write("\x08 \x08")  // erase on terminal

        '\x03':                  // Ctrl-C
            kill current foreground process with exit(130)
            clear LINE_DISC_BUFFER

        other:
            push to LINE_DISC_BUFFER
            if ECHO_ENABLED: uart::put_char(ch)
```

### Raw Mode

When `RAW_MODE = true`, `sys_read` polls `uart::try_get_char()` in a loop (yielding between attempts) and returns as soon as one character is received, without echo or buffering.

The shell uses raw mode for:
- Reading arrow keys (escape sequences) for history navigation.
- The built-in `nano` editor.

### Echo Control

`sys_set_echo(false)` is called during password prompts to suppress character echo. The shell restores echo with `sys_set_echo(true)` after reading the password.

---

## 11. SMP

### HART Management

```rust
pub const MAX_HARTS: usize = 4;

/// Set by primary HART after mm::init and trap::init complete.
pub static SMP_GO_FLAG: AtomicBool = AtomicBool::new(false);

/// 16 KiB stack for each secondary HART.
/// Layout: SECONDARY_STACKS[hartid * 16384 .. (hartid+1) * 16384]
#[link_section = ".bss"]
pub static mut SECONDARY_STACKS: [u8; MAX_HARTS * 16384] = [0; MAX_HARTS * 16384];

/// Which PID is currently running on each HART. -1 = idle.
pub static CURRENT_PIDS: [AtomicI32; MAX_HARTS] = [
    AtomicI32::new(-1),
    AtomicI32::new(-1),
    AtomicI32::new(-1),
    AtomicI32::new(-1),
];
```

### Secondary HART Startup

```
Primary HART 0          Secondary HART n (n ∈ {1,2,3})
──────────────          ───────────────────────────────
kmain():
  mm::init()
  trap::init()
  ...
  SMP_GO_FLAG.store(true)
                          [woken by SMP_GO_FLAG]
                          sp = SECONDARY_STACKS + hartid*16384 + 16384
                          tp = hartid
                          secondary_kmain(hartid):
                            trap::init_hart(hartid)   // set sscratch
                            timer::enable_hart()
                            sched::schedule()         // enter scheduler
```

### Current HART Identification

```rust
#[inline(always)]
pub fn current_hart() -> usize {
    let tp: usize;
    unsafe { core::arch::asm!("mv {}, tp", out(reg) tp) };
    tp
}

#[inline(always)]
pub fn current_pid() -> i32 {
    CURRENT_PIDS[current_hart()].load(Ordering::Relaxed)
}
```

The `tp` (thread pointer) register is set once in `_start` (or secondary startup) and never modified thereafter, making it a reliable per-HART identifier without any memory access.

### TLB Shootdown

When a process's page table is modified (e.g., COW resolution, `munmap`) on one HART, other HARTs running the same process must flush their TLBs. The mechanism:

1. The modifying HART sets a "shootdown pending" flag.
2. Sends a **supervisor software interrupt (IPI)** to all other HARTs via SBI HSM or direct CLINT write.
3. Each receiving HART's trap handler sees `scause = Interrupt(1)`:
   ```rust
   (true, 1) => {
       unsafe { core::arch::asm!("sfence.vma"); }  // flush entire TLB
       clear_ssip();  // clear SSIP bit via sip CSR write
   }
   ```
4. The originating HART waits (spin) until all remote HARTs have acknowledged.

> **Note:** Full TLB shootdown with address-range `sfence.vma rd, rs` optimization is not yet implemented — all shootdowns flush the entire TLB.

---

## 12. User Space

### 12.1 libos — The POSIX Shim

`libos` is a static library linked into every user-space binary. It provides:

**Entry point:**

```rust
// libos/src/start.rs
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    // Initialize user heap (buddy allocator over 2 MiB static BSS array)
    libos_init();
    // Call user main()
    let ret = main();
    // Exit with main's return code
    sys_exit(ret);
}
```

**User heap:**

```rust
static mut HEAP_MEM: [u8; 2 * 1024 * 1024] = [0; 2 * 1024 * 1024];  // 2 MiB BSS

fn libos_init() {
    unsafe {
        USER_HEAP.lock().init(
            HEAP_MEM.as_ptr() as usize,
            HEAP_MEM.len(),
        );
    }
}

#[global_allocator]
static USER_HEAP: buddy_system_allocator::LockedHeap<32> =
    buddy_system_allocator::LockedHeap::empty();
```

**Syscall wrappers:**

```rust
// Generic 3-argument syscall
#[inline(always)]
pub unsafe fn syscall(num: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "ecall",
        in("a7") num,
        inlateout("a0") a0 => ret,
        in("a1") a1,
        in("a2") a2,
        options(nostack)
    );
    ret
}

// 4-argument variant
#[inline(always)]
pub unsafe fn syscall4(num: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "ecall",
        in("a7") num,
        inlateout("a0") a0 => ret,
        in("a1") a1,
        in("a2") a2,
        in("a3") a3,
        options(nostack)
    );
    ret
}
```

All 60+ syscall wrappers (e.g., `sys_fork`, `sys_exec`, `sys_open`) are thin wrappers calling `syscall` or `syscall4` with the appropriate number.

### 12.2 `init` — PID 1

`init` is the first userspace process (PID 1). It:

1. Prints the boot banner (openv version, build info).
2. Becomes the **session leader** (sets its own `sid = pid`).
3. Runs the login prompt loop:
   ```
   loop:
       print "login: "; read username (raw mode for arrow keys)
       print "password: "; set_echo(false); read password; set_echo(true)
       uid = sys_authenticate(username, password)
       if uid >= 0:
           fork() → child: sys_setuid(uid); exec("/bin/sh")
           parent: waitpid(child_pid)
           if child exits: restart login loop (shell respawn)
   ```
4. If PID 1 exits, the kernel panics (no init = broken system).

### 12.3 `sh` — The Shell

The shell provides an interactive command-line environment:

**Features:**

| Feature | Implementation |
|---------|---------------|
| Line editing | Raw mode + manual echo; buffer with cursor position |
| History | `Vec<String>` of past commands; up/down arrow keys cycle |
| Pipelines | `fork()` per stage + `dup2()` to connect stdout→stdin |
| Input redirection `<` | `open(file)` + `dup2(fd, STDIN)` |
| Output redirection `>` | `create(file)` + `dup2(fd, STDOUT)` |
| Append redirection `>>` | `open(file, O_APPEND)` + `dup2(fd, STDOUT)` |
| Background `&` | `fork()` + do not `waitpid()` immediately |
| `cd` builtin | `sys_chdir()` |
| `pwd` builtin | `sys_getcwd()` |
| `exit` builtin | `sys_exit(0)` |
| `help` builtin | Print list of builtins |
| `history` builtin | Print command history |
| `nano` builtin | Full-screen text editor (built into shell binary) |

**Command search:**
1. If path contains `/`: use as literal path.
2. Try `/bin/<command>` directly.
3. Return "command not found".

**Pipeline execution (example: `ls | grep foo`):**

```
parent (sh):
  pipe() → [pipe_r, pipe_w]
  fork() → child1 (ls):
      dup2(pipe_w, STDOUT)
      close(pipe_r), close(pipe_w)
      exec("/bin/ls")
  fork() → child2 (grep):
      dup2(pipe_r, STDIN)
      close(pipe_r), close(pipe_w)
      exec("/bin/grep", ["foo"])
  close(pipe_r), close(pipe_w)
  waitpid(child1); waitpid(child2)
```

### 12.4 Coreutils

| Binary | Description |
|--------|-------------|
| `ls` | Lists directory contents. Reads from `sys_getdents`. |
| `cat` | Reads files/stdin and writes to stdout. Handles pipes. |
| `hello` | Minimal "Hello, World!" — used as a test binary. |
| `producer` | Writes sequential messages to a named pipe (IPC demo). |
| `consumer` | Reads from a named pipe and prints to stdout (IPC demo). |
| `doexec` | Exec wrapper: `exec(argv[1])` — useful for testing exec. |
| `forktest` | Exercises `fork()`/`waitpid()` with multiple children. |

### 12.5 `net-smoltcp`

`net-smoltcp` is the TCP/IP daemon process:

**Architecture:**

```rust
fn main() {
    // Set up smoltcp interface using raw Ethernet I/O syscalls
    let device = KernelDevice::new();  // wraps sys_net_send / sys_net_recv
    let mut iface = smoltcp::iface::Interface::new(config, &mut device);

    // Configure IP address (static or DHCP)
    iface.update_ip_addrs(|addrs| {
        addrs.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24)).ok();
    });

    loop {
        // 1. Poll smoltcp (runs TCP state machine, ARP, timers)
        iface.poll(Instant::now(), &mut device, &mut sockets);

        // 2. Accept new kernel sockets from registry
        while let sid = sys_daemon_next_socket() {
            if sid < 0 { break; }
            register_socket(sid);
        }

        // 3. Process opcode messages from each registered socket
        for sock in &mut kernel_sockets {
            if let Some(msg) = try_recv_from_socket(sock) {
                match msg.opcode {
                    BIND    => smoltcp_bind(sock, msg.addr),
                    LISTEN  => smoltcp_listen(sock),
                    CONNECT => smoltcp_connect(sock, msg.addr),
                    SEND    => smoltcp_send(sock, msg.data),
                }
            }
        }

        // 4. Forward received TCP data to waiting applications
        for sock in &mut kernel_sockets {
            if smoltcp_can_recv(sock) {
                let data = smoltcp_recv(sock);
                sys_write(sock.channel_fd, &data);
            }
        }
    }
}
```

`KernelDevice` implements smoltcp's `Device` trait using `sys_net_send` and `sys_net_recv` for physical I/O. smoltcp handles all ARP, IP fragmentation, TCP handshaking, retransmission, and flow control.

---

## 13. Security Model

### Discretionary Access Control (DAC)

openv implements Unix-style **Discretionary Access Control**:

- Every file/directory has an owner `uid`, owner `gid`, and a 9-bit `mode`.
- `check_access(vnode, proc, flags)` compares `proc.euid`/`proc.egid` against the file's ownership to select the correct permission bits.
- **Root (`uid=0`) bypasses all access checks** — this is checked first in `check_access`.

### Credential Propagation

```
Process credentials flow:
  fork():  child inherits uid, gid, euid, egid from parent
  exec():  uid/gid unchanged; euid set to file owner if S_ISUID is set
  setuid(): POSIX semantics (root can set any; non-root restricted)
```

### sudo / Privilege Escalation

```
sudo workflow:
  1. User runs sudo binary (setuid-root)
  2. sudo calls sys_authenticate(username, password) → uid check
  3. sudo calls sys_can_sudo(uid) → checks sudo group membership
  4. If authorised: sys_setuid(0) → euid = 0 (root)
  5. Executes target command as root
```

### Password Hashing

Passwords are stored as **FNV-1a 64-bit hashes**:

```rust
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;  // FNV offset basis
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);  // FNV prime
    }
    hash
}
```

> **⚠️ Security Notice:** FNV-1a is a fast, non-cryptographic hash. It is **not suitable for production password storage**. A production system must use a proper password hashing function such as Argon2id, bcrypt, or scrypt with a random salt. The FNV-1a approach is used in openv v1 as a demo-only placeholder.

### Known Gaps in v1

| Gap | Description |
|-----|-------------|
| No ASLR | User processes always load at fixed addresses (`0x100000000`) |
| No stack canaries | Stack overflows are not detected |
| No SMEP/SMAP | RISC-V does not have these x86 mitigations; `PTE_U` on kernel pages is absent |
| Signals | Only Ctrl-C (`exit(130)`) is handled; no full POSIX signal delivery |
| FNV-1a passwords | Not suitable for production (see above) |
| No seccomp | No syscall filtering per-process |

---

## 14. Linker Scripts

### Kernel Linker Script

The kernel is linked with a custom linker script specifying the physical load layout:

```ld
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS {
    . = 0x80200000;   /* OpenSBI hands off here */

    .text : {
        *(.text.entry)  /* boot.s _start must be first */
        *(.text*)
    }

    .rodata : {
        *(.rodata*)
    }

    .data : {
        *(.data*)
        *(.sdata*)
    }

    .bss (NOLOAD) : {
        _bss_start = .;
        *(.bss*)
        *(.sbss*)
        SECONDARY_STACKS = .;  /* SMP stacks carved from BSS */
        . += MAX_HARTS * 16384;
        . = ALIGN(16);
        _stack_end = . + 65536;  /* Primary HART kernel stack (64 KiB) */
        . = _stack_end;
        _bss_end = .;
    }
}
```

The kernel identity map covers all addresses starting at `0x80000000`, so the kernel runs with virtual addresses equal to physical addresses (no relocation needed).

### User Linker Script

User binaries are linked at the **4 GiB mark**, above the kernel identity map:

```ld
OUTPUT_ARCH(riscv)

SECTIONS {
    . = 0x100000000;   /* 4 GiB — above all kernel identity-mapped pages */

    .text : { *(.text*) }
    .rodata : { *(.rodata*) }
    .data : { *(.data*) *(.sdata*) }
    .bss (NOLOAD) : {
        _bss_start = .;
        *(.bss*)
        *(.sbss*)
        _bss_end = .;
    }
}
```

**User stack:**

The kernel allocates the user stack during `spawn`/`exec`:
- Stack top: `USER_STACK_TOP = 0x200000000` (8 GiB virtual address)
- Stack size: configurable (default 512 KiB)
- `sp` is set to `USER_STACK_TOP - 8` (aligned) before entering user code

This places the user stack in the virtual address range `0x1FFFF8000` – `0x200000000`, well above the user code and data segments.

---

## 15. CI/CD

### GitHub Actions Workflow

The CI pipeline runs on every push and pull request with the following structure:

```
┌─────────────────────────────────────────────────────┐
│                      Push / PR                       │
└──────────────────────┬──────────────────────────────┘
                       │
          ┌────────────┴─────────────┐
          ▼                          ▼
    ┌──────────┐               ┌──────────┐
    │  lint    │               │  lint    │
    │ (kernel) │               │(userspace│
    └────┬─────┘               └────┬─────┘
         │  cargo check              │  cargo check
         │  cargo clippy             │  cargo clippy
         │    -D warnings            │    -D warnings
         └────────────┬─────────────┘
                      │  both must pass
                      ▼
               ┌──────────────┐
               │    build     │
               └──────┬───────┘
                      │
            ┌─────────┴──────────┐
            │                    │
            ▼                    ▼
      Debug kernel          Release kernel
      + initrd TAR          + binary size report
```

### Artifacts

| Artifact | Retention | Description |
|----------|-----------|-------------|
| `kernel-debug` | 90 days | Debug build with full debug info |
| `kernel-release` | 90 days | Release build (optimized) |
| `initrd.tar` | 90 days | User space archive (root filesystem) |

### Release Bundle

On push to `main` or `master`, a versioned release bundle is created:

```
openv-<branch>-<short-sha>.tar.gz
├── kernel          (release binary)
├── initrd.tar      (user filesystem)
└── README.md       (build info)
```

### Binary Size Report

After the release build, a **GitHub Step Summary** is posted with:
- Kernel ELF size (bytes)
- Stripped kernel size
- Section sizes (`text`, `rodata`, `data`, `bss`)
- initrd TAR size

This gives developers a quick view of code size regression between commits.

---

## 16. Known Limitations and Future Work

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

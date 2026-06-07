# 3. Memory Management

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
[Back to Index](README.md)

# 2. Boot Sequence

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
[Back to Index](README.md)

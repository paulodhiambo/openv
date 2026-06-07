# 11. SMP

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
[Back to Index](README.md)

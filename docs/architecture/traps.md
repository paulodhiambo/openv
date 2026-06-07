# 4. Trap Handling

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
[Back to Index](README.md)

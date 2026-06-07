.section .text._start
.global _start

_start:
    # OpenSBI passes:
    # a0 = hartid
    # a1 = dtb_phys_addr
    
    # Disable interrupts
    csrw sie, zero

    # Save a0, a1 to s0, s1 (callee-saved registers)
    mv s0, a0
    mv s1, a1

    # Park all harts except hart 0
    bnez s0, park

    # Set stack pointer to the end of the stack region defined in linker.ld
    la sp, _stack_end

    # Clear the .bss section
    la t0, _bss_start
    la t1, _bss_end
    bgeu t0, t1, 2f
1:
    sd zero, (t0)
    addi t0, t0, 8
    bltu t0, t1, 1b
2:

    # Restore arguments for kmain
    mv a0, s0
    mv a1, s1
    # Set tp = hartid (used by kernel to identify current HART in S-mode)
    mv tp, s0

    # Call the Rust kernel entry point
    call kmain

park:
    # Spin until the primary HART sets SMP_GO_FLAG
    la t0, SMP_GO_FLAG
1:
    lb t1, (t0)
    beqz t1, 1b
    # Compute stack top: HartStack is [4096-guard][16384-stack], total 20480 bytes.
    # sp = &SECONDARY_STACKS[hartid].stack[16384]  (top of usable stack)
    la   t0, SECONDARY_STACKS
    li   t1, 20480           # sizeof(HartStack) = 4096 + 16384
    mul  t2, s0, t1          # hartid * 20480 bytes
    add  t0, t0, t2          # base of this hart's HartStack
    add  sp, t0, t1          # sp = base + 20480 (top of stack field)
    mv   tp, s0              # tp = hartid (S-mode per-hart ID)
    mv   a0, s0              # a0 = hartid
    mv   a1, s1              # a1 = dtb_ptr
    call secondary_kmain
    j park

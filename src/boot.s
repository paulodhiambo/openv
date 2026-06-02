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

    # Call the Rust kernel entry point
    call kmain

park:
    wfi
    j park

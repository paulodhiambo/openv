OUTPUT_ARCH(riscv)
ENTRY(_start)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;

    .text : {
        *(.text._start)
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    .data : {
        *(.data .data.*)
    }

    .bss : ALIGN(16) {
        PROVIDE(_bss_start = .);
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        PROVIDE(_bss_end = .);
    }

    /* Primary HART kernel stack with guard page below it.
     * Stack grows downward toward _stack_start; _stack_guard is a 4KB canary
     * region that is checked on each scheduler switch to detect overflow. */
    . = ALIGN(4096);
    PROVIDE(_stack_guard_start = .);
    . += 0x1000; /* 4KB guard region */
    PROVIDE(_stack_start = .);
    . += 0x8000; /* 32KB stack */
    PROVIDE(_stack_end = .);
}

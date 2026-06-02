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

    .bss : {
        PROVIDE(_bss_start = .);
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        PROVIDE(_bss_end = .);
    }

    /* Stack memory space */
    PROVIDE(_stack_start = .);
    . += 0x8000; /* 32KB stack */
    PROVIDE(_stack_end = .);
}

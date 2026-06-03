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

    /* Stack memory space (start aligned to 16 bytes for RISC-V ABI) */
    . = ALIGN(16);
    PROVIDE(_stack_start = .);
    . += 0x8000; /* 32KB stack */
    PROVIDE(_stack_end = .);
}

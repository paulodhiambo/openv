OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
    . = 0x100000000;
    .text : ALIGN(4096) {
        *(.text._start)
        *(.text .text.*)
    }
    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }
    .data : ALIGN(4096) {
        *(.data .data.*)
    }
    .bss : ALIGN(4096) {
        *(.bss .bss.*)
    }
    /DISCARD/ : {
        *(.eh_frame)
    }
}

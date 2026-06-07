# 14. Linker Scripts

### Kernel Linker Script

The kernel is linked with a custom linker script specifying the physical load layout:

```ld
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS {
    . = 0x80200000;   /* OpenSBI hands off here */

    .text : {
        *(.text.entry)  /* boot.s _start must be first */
        *(.text*)
    }

    .rodata : {
        *(.rodata*)
    }

    .data : {
        *(.data*)
        *(.sdata*)
    }

    .bss (NOLOAD) : {
        _bss_start = .;
        *(.bss*)
        *(.sbss*)
        SECONDARY_STACKS = .;  /* SMP stacks carved from BSS */
        . += MAX_HARTS * 16384;
        . = ALIGN(16);
        _stack_end = . + 65536;  /* Primary HART kernel stack (64 KiB) */
        . = _stack_end;
        _bss_end = .;
    }
}
```

The kernel identity map covers all addresses starting at `0x80000000`, so the kernel runs with virtual addresses equal to physical addresses (no relocation needed).

### User Linker Script

User binaries are linked at the **4 GiB mark**, above the kernel identity map:

```ld
OUTPUT_ARCH(riscv)

SECTIONS {
    . = 0x100000000;   /* 4 GiB — above all kernel identity-mapped pages */

    .text : { *(.text*) }
    .rodata : { *(.rodata*) }
    .data : { *(.data*) *(.sdata*) }
    .bss (NOLOAD) : {
        _bss_start = .;
        *(.bss*)
        *(.sbss*)
        _bss_end = .;
    }
}
```

**User stack:**

The kernel allocates the user stack during `spawn`/`exec`:
- Stack top: `USER_STACK_TOP = 0x200000000` (8 GiB virtual address)
- Stack size: configurable (default 512 KiB)
- `sp` is set to `USER_STACK_TOP - 8` (aligned) before entering user code

This places the user stack in the virtual address range `0x1FFFF8000` – `0x200000000`, well above the user code and data segments.

---
[Back to Index](README.md)

use core::ptr::{read_volatile, write_volatile};

// Default PLIC base used by QEMU 'virt' platform. Adjust if platform differs.
const PLIC_BASE: usize = 0x0c00_0000;
const PLIC_CTX_BASE: usize = PLIC_BASE + 0x200000;
const PLIC_CTX_STRIDE: usize = 0x1000;

/// Claim the interrupt for the given hart context (returns 0 if none)
pub fn claim(hart: usize) -> u32 {
    let addr = PLIC_CTX_BASE + hart * PLIC_CTX_STRIDE;
    unsafe { read_volatile((addr + 0x4) as *const u32) }
}

/// Complete the interrupt (ack) for the given hart and irq id
pub fn complete(hart: usize, irq: u32) {
    let addr = PLIC_CTX_BASE + hart * PLIC_CTX_STRIDE;
    unsafe { write_volatile((addr + 0x4) as *mut u32, irq) }
}

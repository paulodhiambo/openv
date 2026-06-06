use core::ptr::{read_volatile, write_volatile};

// Default PLIC base used by QEMU 'virt' platform. Adjust if platform differs.
const PLIC_BASE: usize = 0x0c00_0000;
const PLIC_CTX_BASE: usize = PLIC_BASE + 0x200000;
const PLIC_CTX_STRIDE: usize = 0x1000;

/// Claim the interrupt for the given hart context (returns 0 if none)
pub fn claim(hart: usize) -> u32 {
    let context = hart * 2 + 1; // S-mode context
    let addr = PLIC_CTX_BASE + context * PLIC_CTX_STRIDE;
    unsafe { read_volatile((addr + 0x4) as *const u32) }
}

/// Complete the interrupt (ack) for the given hart and irq id
pub fn complete(hart: usize, irq: u32) {
    let context = hart * 2 + 1; // S-mode context
    let addr = PLIC_CTX_BASE + context * PLIC_CTX_STRIDE;
    unsafe { write_volatile((addr + 0x4) as *mut u32, irq) }
}

/// Enable or disable a specific interrupt for a given hart in the PLIC.
pub fn set_enable(hart: usize, irq: u32, enable: bool) {
    let context = hart * 2 + 1; // S-mode context
    
    // Set priority to 1 when enabling
    if enable {
        let prio_addr = PLIC_BASE + (irq as usize * 4);
        unsafe { write_volatile(prio_addr as *mut u32, 1); }
        
        // Set threshold to 0 to accept all interrupts
        let thresh_addr = PLIC_CTX_BASE + context * PLIC_CTX_STRIDE;
        unsafe { write_volatile(thresh_addr as *mut u32, 0); }
    }

    let enable_base = PLIC_BASE + 0x2000 + (context * 0x80);
    let word = irq / 32;
    let bit = irq % 32;
    let addr = enable_base + (word as usize * 4);
    unsafe {
        let mut val = read_volatile(addr as *const u32);
        if enable {
            val |= 1 << bit;
        } else {
            val &= !(1 << bit);
        }
        write_volatile(addr as *mut u32, val);
    }
}

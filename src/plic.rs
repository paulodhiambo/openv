//! # Platform-Level Interrupt Controller (PLIC) Driver
//!
//! This module provides a driver for the RISC-V Platform-Level Interrupt
//! Controller (PLIC). The PLIC is responsible for managing external interrupts
//! in RISC-V systems, prioritizing them, and delivering them to the appropriate
//! hart (hardware thread) context.
//!
//! ## RISC-V PLIC Overview
//!
//! The PLIC is a standard component in RISC-V systems that handles external
//! interrupts from devices. It provides:
//!
//! - **Interrupt prioritization**: Each interrupt source can be assigned a
//!   priority level (0 = disabled, higher numbers = higher priority).
//! - **Interrupt enabling**: Each interrupt can be individually enabled or
//!   disabled for each hart context.
//! - **Interrupt claiming**: Harts claim interrupts by reading a claim
//!   register, which returns the highest-priority pending interrupt.
//! - **Interrupt completion**: After servicing an interrupt, the hart writes
//!   the interrupt ID to a completion register to acknowledge it.
//!
//! ## Hart Contexts
//!
//! In RISC-V, each hart can have multiple privilege modes (M-mode, S-mode, etc.).
//! The PLIC treats each privilege mode as a separate "context". For S-mode,
//! the context number is typically `hart * 2 + 1` (M-mode is `hart * 2`).
//!
//! ## Memory Map
//!
//! The PLIC's memory map is:
//!
//! - `PLIC_BASE + 0x000000`: Priority registers (one per interrupt, 4 bytes each)
//! - `PLIC_BASE + 0x200000`: Context registers (one block per context)
//!   - `PLIC_CTX_BASE + context * PLIC_CTX_STRIDE + 0x00`: Threshold register
//!   - `PLIC_CTX_BASE + context * PLIC_CTX_STRIDE + 0x04`: Claim/complete register
//! - `PLIC_BASE + 0x2000 + context * 0x80`: Enable registers for a context
//!
//! ## Platform Support
//!
//! This driver uses the QEMU 'virt' platform's PLIC base address
//! (`0x0c00_0000`). For other platforms, the base address may need to be
//! adjusted.
//!
//! ## Safety
//!
//! This driver uses volatile memory operations to interact with PLIC
//! registers. This is necessary because the PLIC is a memory-mapped device
//! and the compiler must not optimize away or reorder accesses to its
//! registers.

use core::ptr::{read_volatile, write_volatile};

// Default PLIC base used by QEMU 'virt' platform. Adjust if platform differs.
const PLIC_BASE: usize = 0x0c00_0000;
/// Base address of the PLIC context registers.
const PLIC_CTX_BASE: usize = PLIC_BASE + 0x200000;
/// Stride between consecutive PLIC context register blocks.
const PLIC_CTX_STRIDE: usize = 0x1000;

/// Claims the interrupt for the given hart context (returns 0 if none).
///
/// When a hart reads the claim register, the PLIC returns the ID of the
/// highest-priority pending interrupt. If no interrupts are pending, it
/// returns 0. After reading, the interrupt is marked as "in service" and
/// will not be claimed again until it is completed.
///
/// # Arguments
///
/// * `hart` - The hart (hardware thread) ID.
///
/// # Returns
///
/// The ID of the claimed interrupt, or 0 if no interrupts are pending.
///
/// # Safety
///
/// This function performs a volatile memory read from a memory-mapped
/// device register. The caller must ensure that the hart ID is valid and
/// that the PLIC has been initialized.
pub fn claim(hart: usize) -> u32 {
    // S-mode context is hart * 2 + 1 (M-mode would be hart * 2)
    let context = hart * 2 + 1;
    let addr = PLIC_CTX_BASE + context * PLIC_CTX_STRIDE;
    unsafe { read_volatile((addr + 0x4) as *const u32) }
}

/// Completes the interrupt (ack) for the given hart and IRQ ID.
///
/// After servicing an interrupt, the hart must write the interrupt ID to
/// the complete register to acknowledge it. This allows the PLIC to
/// deliver the interrupt again in the future.
///
/// # Arguments
///
/// * `hart` - The hart (hardware thread) ID.
/// * `irq` - The IRQ ID of the interrupt to complete.
///
/// # Safety
///
/// This function performs a volatile memory write to a memory-mapped
/// device register. The caller must ensure that the hart ID and IRQ ID
/// are valid and that the interrupt has been claimed.
pub fn complete(hart: usize, irq: u32) {
    // S-mode context is hart * 2 + 1 (M-mode would be hart * 2)
    let context = hart * 2 + 1;
    let addr = PLIC_CTX_BASE + context * PLIC_CTX_STRIDE;
    unsafe { write_volatile((addr + 0x4) as *mut u32, irq) }
}

/// Enables or disables a specific interrupt for a given hart in the PLIC.
///
/// When enabling, this function:
/// 1. Sets the interrupt's priority to 1 (lowest non-zero priority).
/// 2. Sets the hart's threshold to 0 (accept all interrupts).
/// 3. Sets the appropriate bit in the hart's enable register.
///
/// When disabling, this function:
/// 1. Clears the appropriate bit in the hart's enable register.
///
/// # Arguments
///
/// * `hart` - The hart (hardware thread) ID.
/// * `irq` - The IRQ ID of the interrupt to enable or disable.
/// * `enable` - `true` to enable the interrupt, `false` to disable it.
///
/// # Safety
///
/// This function performs volatile memory reads and writes to memory-mapped
/// device registers. The caller must ensure that the hart ID and IRQ ID
/// are valid and that the PLIC has been initialized.
pub fn set_enable(hart: usize, irq: u32, enable: bool) {
    // S-mode context is hart * 2 + 1 (M-mode would be hart * 2)
    let context = hart * 2 + 1;
    
    // Set priority to 1 when enabling. A priority of 0 would disable the
    // interrupt, so we use 1 as the minimum non-zero priority.
    if enable {
        let prio_addr = PLIC_BASE + (irq as usize * 4);
        unsafe { write_volatile(prio_addr as *mut u32, 1); }
        
        // Set threshold to 0 to accept all interrupts. The threshold is
        // the minimum priority that will be delivered to this context.
        let thresh_addr = PLIC_CTX_BASE + context * PLIC_CTX_STRIDE;
        unsafe { write_volatile(thresh_addr as *mut u32, 0); }
    }

    // Compute the address of the enable register bit for this IRQ.
    // Each enable register is 32 bits wide, and each context has its own
    // block of enable registers at offset 0x2000 from the PLIC base.
    let enable_base = PLIC_BASE + 0x2000 + (context * 0x80);
    let word = irq / 32;
    let bit = irq % 32;
    let addr = enable_base + (word as usize * 4);
    unsafe {
        // Read-modify-write to update only the bit for this IRQ.
        let mut val = read_volatile(addr as *const u32);
        if enable {
            val |= 1 << bit;
        } else {
            val &= !(1 << bit);
        }
        write_volatile(addr as *mut u32, val);
    }
}

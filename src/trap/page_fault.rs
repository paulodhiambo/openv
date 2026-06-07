//! # Page Fault Handling
//!
//! This module handles RISC-V page fault exceptions. The cause numbers are:
//!
//! | Cause | Name                |
//! |-------|---------------------|
//! | 12    | Instruction page fault |
//! | 13    | Load page fault        |
//! | 15    | Store/AMO page fault   |
//!
//! ## Demand Paging
//!
//! For instruction and load faults ([`handle_user_page_fault`]):
//!
//! 1. Read the faulting virtual address from `stval`.
//! 2. Compute the page table root physical address from the current `satp`.
//! 3. Call [`crate::mm::vmm::handle_user_page_fault`] to either:
//!    - Allocate a new zeroed page (anonymous mapping).
//!    - Load a page from a backing file or initrd (mapped file).
//!    - Map a previously swapped-out page from swap.
//! 4. If the fault cannot be resolved, kill the process with `SIGSEGV`
//!    (`exit(-11)`).
//!
//! ## Copy-on-Write (Store Faults)
//!
//! For store faults, we first try [`crate::mm::vmm::handle_store_page_fault`]
//! to handle COW. If the page is not COW, we fall back to
//! [`crate::mm::vmm::handle_user_page_fault`]. This sequence ensures
//! that COW pages in forked processes get their own private copies
//! before any other page-fault handling logic runs.

use crate::trap::TrapFrame;

/// Handles a page fault given its cause, EPC, faulting VA, and trap frame.
///
/// # Arguments
///
/// * `cause` - The exception cause from `scause` (12, 13, or 15).
/// * `_sepc` - The saved program counter (unused).
/// * `stval` - The faulting virtual address (or page table entry for
///   some causes).
/// * `tf` - The trap frame.
///
/// # Returns
///
/// A pointer to the (possibly modified) trap frame.
pub fn handle_page_fault(cause: usize, _sepc: usize, stval: usize, tf: &mut TrapFrame) -> *mut TrapFrame {
    match cause {
        // Instruction page fault — demand page with R+W+X+U so the CPU can re-execute
        12 => {
            use crate::mm::vmm::PTE_X;
            use riscv::register::satp;
            let fault_va = stval;
            let root_pa = (satp::read().bits() as usize & 0xFFFFFFFFFFF) << 12;
            match crate::mm::vmm::handle_user_page_fault(root_pa, fault_va, PTE_X) {
                Ok(()) => tf as *mut _,
                Err(e) => {
                    let pid = crate::posix::process::current_pid();
                    crate::println!("Segfault pid {}: instr {} at va={:#x}", pid, e, fault_va);
                    crate::posix::spawn::exit(pid, -11);
                    crate::posix::process::schedule();
                    unsafe { crate::trap::halt_cpu() }
                }
            }
        }
        // Load page fault — demand page with R+W+U
        13 => {
            use riscv::register::satp;
            let fault_va = stval;
            let root_pa = (satp::read().bits() as usize & 0xFFFFFFFFFFF) << 12;
            match crate::mm::vmm::handle_user_page_fault(root_pa, fault_va, 0) {
                Ok(()) => tf as *mut _,
                Err(e) => {
                    let pid = crate::posix::process::current_pid();
                    crate::println!("Segfault pid {}: load {} at va={:#x}", pid, e, fault_va);
                    crate::posix::spawn::exit(pid, -11);
                    crate::posix::process::schedule();
                    unsafe { crate::trap::halt_cpu() }
                }
            }
        }
        // Store/AMO page fault — copy-on-write, then demand paging (stack growth)
        15 => {
            use riscv::register::satp;
            let fault_va = stval;
            let root_pa = (satp::read().bits() as usize & 0xFFFFFFFFFFF) << 12;
            // Try COW first (fork: shared read-only page → private writable copy).
            match crate::mm::vmm::handle_store_page_fault(root_pa, fault_va) {
                Ok(()) => tf as *mut _,
                Err(_) => {
                    // Not a COW page — try demand paging (stack growth or lazy alloc).
                    match crate::mm::vmm::handle_user_page_fault(root_pa, fault_va, 0) {
                        Ok(()) => tf as *mut _,
                        Err(e) => {
                            let pid = crate::posix::process::current_pid();
                            crate::println!(
                                "Segfault pid {}: store {} at va={:#x}",
                                pid,
                                e,
                                fault_va
                            );
                            crate::posix::spawn::exit(pid, -11);
                            crate::posix::process::schedule();
                            unsafe { crate::trap::halt_cpu() }
                        }
                    }
                }
            }
        }
        _ => unreachable!(),
    }
}

use crate::trap::TrapFrame;

pub fn handle_page_fault(cause: usize, _sepc: usize, stval: usize, tf: &mut TrapFrame) -> *mut TrapFrame {
    match cause {
        // Instruction or load page fault — demand paging
        12 | 13 => {
            use riscv::register::satp;
            let fault_va = stval;
            let root_pa = (satp::read().bits() as usize & 0xFFFFFFFFFFF) << 12;
            match crate::mm::vmm::handle_user_page_fault(root_pa, fault_va) {
                Ok(()) => tf as *mut _,
                Err(e) => {
                    let pid = crate::posix::process::current_pid();
                    crate::println!("Segfault pid {}: {} at va={:#x}", pid, e, fault_va);
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
                    match crate::mm::vmm::handle_user_page_fault(root_pa, fault_va) {
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

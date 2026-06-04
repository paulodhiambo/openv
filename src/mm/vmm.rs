use crate::mm::pmm::alloc_frame;
use crate::println;
use core::arch::asm;
use riscv::register::satp;

pub const PTE_V: usize = 1 << 0;
pub const PTE_R: usize = 1 << 1;
pub const PTE_W: usize = 1 << 2;
pub const PTE_X: usize = 1 << 3;
pub const PTE_U: usize = 1 << 4;

#[repr(C, align(4096))]
pub struct PageTable {
    pub entries: [usize; 512],
}

impl PageTable {
    pub const fn new() -> Self {
        PageTable { entries: [0; 512] }
    }

    pub fn new_process_table() -> Result<usize, &'static str> {
        let root_page = alloc_frame().ok_or("Out of memory")?.into_raw();
        // SAFETY:
        // Preconditions: `root_page` is a newly allocated, 4096-aligned physical address exclusively owned by this call.
        // Postconditions: Returns a mutable reference to the 4096-byte frame cast as a `PageTable`. Mutability is safe as we hold exclusive ownership.
        let root = unsafe { &mut *(root_page as *mut PageTable) };

        // Identity map kernel using 1GB mega-pages (0 to 4GB); NOT PTE_U
        for i in 0..4 {
            let pa = i * 0x40000000;
            let ppn = pa >> 12;
            root.entries[i] = (ppn << 10) | PTE_V | PTE_R | PTE_W | PTE_X;
        }

        Ok(root_page)
    }

    /// Maps a 4KB page at the given virtual address to the given physical address.
    pub fn map_page(&mut self, va: usize, pa: usize, flags: usize) -> Result<(), &'static str> {
        let vpn = [(va >> 12) & 0x1FF, (va >> 21) & 0x1FF, (va >> 30) & 0x1FF];

        let mut pt = self;
        for level in (1..=2).rev() {
            let entry = &mut pt.entries[vpn[level]];
            if *entry & PTE_V == 0 {
                let new_page = alloc_frame().ok_or("Out of memory mapping page")?;
                let ppn = new_page.pa() >> 12;
                *entry = (ppn << 10) | PTE_V;
                new_page.into_raw(); // Ownership transferred to PTE
            }
            let next_pt_pa = (*entry >> 10) << 12;
            // SAFETY:
            // Preconditions: `next_pt_pa` is a physical address derived from a valid PTE we just wrote or verified. It represents a 4096-byte aligned page table frame owned by the process.
            // Postconditions: Updates `pt` to reference the next-level page table. Valid because we hold an exclusive borrow to the overall table via `&mut self`.
            pt = unsafe { &mut *(next_pt_pa as *mut PageTable) };
        }

        let entry = &mut pt.entries[vpn[0]];
        if *entry & PTE_V != 0 {
            return Err("Page already mapped");
        }

        let ppn = pa >> 12;
        *entry = (ppn << 10) | PTE_V | flags;

        Ok(())
    }
}

/// Clone user portions of a parent's page table into a child page table, making pages copy-on-write.
/// parent_root_pa and child_root_pa are physical addresses of the root page tables.
pub fn clone_user_space(parent_root_pa: usize, child_root_pa: usize) -> Result<(), &'static str> {
    use crate::mm::pmm;

    // SAFETY for the entire clone_level function:
    // Preconditions: `parent_pa` and `child_pa` must be valid, 4096-byte aligned physical addresses of page tables owned by the respective processes.
    // The parent's page table must not be concurrently modified (caller holds process lock).
    // Postconditions: Recursively clones all mappings and increments refcounts for COW leaf pages.
    unsafe fn clone_level(
        parent_pa: usize,
        child_pa: usize,
        level: usize,
    ) -> Result<(), &'static str> {
        // SAFETY: Preconditions met by caller (see `clone_level` function documentation).
        let parent_pt = unsafe { &mut *(parent_pa as *mut PageTable) };
        // SAFETY: Preconditions met by caller (see `clone_level` function documentation).
        let child_pt = unsafe { &mut *(child_pa as *mut PageTable) };

        // At root level (2), skip kernel identity entries (indices 0-3).
        // They are 1GB superpages already set up by new_process_table and
        // should not be COW'd or refcounted.
        let range = if level == 2 { 4..512 } else { 0..512 };

        for idx in range {
            let entry = parent_pt.entries[idx];
            if entry == 0 {
                child_pt.entries[idx] = 0;
                continue;
            }

            // If this is a pointer to next-level table (not a leaf)
            let is_leaf = (entry & (PTE_R | PTE_X)) != 0;
            if !is_leaf {
                // Allocate a new page for child's next level
                let new_page = pmm::alloc_frame().ok_or("OOM cloning page table")?;
                let ppn = new_page.pa() >> 12;
                child_pt.entries[idx] = (ppn << 10) | PTE_V;
                let next_parent_pa = (entry >> 10) << 12;
                let next_child_pa = new_page.into_raw(); // transferred

                // Recurse into next level
                // SAFETY: see enclosing function doc-comment.
                unsafe { clone_level(next_parent_pa, next_child_pa, level - 1)? };
            } else {
                // Leaf mapping
                let pa = (entry >> 10) << 12;
                let mut flags = entry & 0x3FF; // low 10 bits

                if (flags & PTE_U) != 0 {
                    // User page: make read-only in both parent and child (COW)
                    if (flags & PTE_W) != 0 {
                        // Clear write in parent
                        let new_parent_entry = (pa >> 12) << 10 | (flags & !PTE_W);
                        parent_pt.entries[idx] = new_parent_entry;
                        flags &= !PTE_W;
                    }
                    // Increment refcount for shared page
                    pmm::incr_ref(pa);
                }

                child_pt.entries[idx] = (pa >> 12) << 10 | (flags & 0x3FF) | PTE_V;
            }
        }
        Ok(())
    }

    // Start recursion at level 2 (root has 3 levels: 2,1,0)
    // SAFETY:
    // Preconditions: `parent_root_pa` and `child_root_pa` are valid, 4096-aligned physical addresses representing root page tables.
    // Postconditions: Starts the recursive clone operation starting at level 2.
    unsafe { clone_level(parent_root_pa, child_root_pa, 2) }
}

/// Handle store page fault at given virtual address in the currently active page table.
/// Perform copy-on-write if the page is a user mapping that is currently read-only.
pub fn handle_store_page_fault(fault_va: usize) -> Result<(), &'static str> {
    use crate::mm::pmm;
    use riscv::register::satp;

    // satp::read() is safe — it merely reads a CSR with no side effects.
    let satp_val = satp::read().bits() as usize;
    let ppn = satp_val & 0xFFFFFFFFFFF;
    let root_pa = ppn << 12;

    // Walk to find the leaf PTE
    let vpn = [
        (fault_va >> 12) & 0x1FF,
        (fault_va >> 21) & 0x1FF,
        (fault_va >> 30) & 0x1FF,
    ];

    let mut pt_pa = root_pa;
    for level in (1..=2).rev() {
        // SAFETY:
        // Preconditions: `pt_pa` is derived from the active `SATP` CSR or a valid intermediate PTE, ensuring it is a valid page table physical address.
        // Postconditions: Mutably borrows the page table to walk its entries.
        let pt = unsafe { &mut *(pt_pa as *mut PageTable) };
        let entry = pt.entries[vpn[level]];
        if entry & PTE_V == 0 {
            return Err("no mapping at level");
        }
        let is_leaf = (entry & (PTE_R | PTE_X)) != 0;
        if is_leaf {
            return Err("huge page (1GB/2MB) not supported for COW");
        }
        pt_pa = ((entry >> 10) << 12) as usize;
    }

    // SAFETY:
    // Preconditions: `pt_pa` is a valid L0 page table physical address reached via a successful page walk from `SATP`.
    // Postconditions: Mutably borrows the leaf page table to alter the PTE.
    let pt = unsafe { &mut *(pt_pa as *mut PageTable) };
    let entry = pt.entries[vpn[0]];
    if entry & PTE_V == 0 {
        return Err("no mapping for faulting VA");
    }

    let pa = (entry >> 10) << 12;
    let flags = entry & 0x3FF;

    // If page is user and not writable, perform COW
    if (flags & PTE_U) != 0 && (flags & PTE_W) == 0 {
        // Allocate a new page and copy contents
        let new_frame = pmm::alloc_frame().ok_or("OOM in COW")?;
        let new_pa = new_frame.pa();
        // SAFETY:
        // Preconditions: `pa` is a valid physical address of an existing initialized user page. `new_pa` is a freshly allocated page. Both are PAGE_SIZE (4096) bytes.
        // Postconditions: Copies exactly 4096 bytes from `pa` to `new_pa` without overlapping.
        unsafe {
            core::ptr::copy_nonoverlapping(pa as *const u8, new_pa as *mut u8, pmm::PAGE_SIZE);
        }

        // Update PTE to point to new page with write enabled
        let new_entry = ((new_pa >> 12) << 10) | (flags | PTE_W) | PTE_V;
        pt.entries[vpn[0]] = new_entry;
        new_frame.into_raw(); // ownership transferred to PTE

        // Decrement refcount of old page; free if zero
        let remaining = pmm::decr_ref(pa);
        if remaining == 0 {
            pmm::free_page(pa);
        }

        // Flush TLB to make the new writable PTE visible to the CPU.
        // SAFETY:
        // Preconditions: Process is in supervisor mode.
        // Postconditions: Executes the `sfence.vma` instruction to flush the TLB, ensuring the new PTE is visible.
        unsafe { core::arch::asm!("sfence.vma") };

        Ok(())
    } else {
        Err("not a COW candidate")
    }
}

static mut ROOT_PAGE_TABLE: usize = 0;

// SAFETY for destroy_level:
// Preconditions: `pt_pa` is a valid physical address of a page table owned by the process. No concurrent access to this page table exists.
// Postconditions: Recursively frees all leaf user pages and intermediate page tables.
unsafe fn destroy_level(pt_pa: usize, level: usize) {
    // SAFETY: Preconditions met by caller (see `destroy_level` function documentation).
    let pt = unsafe { &mut *(pt_pa as *mut PageTable) };
    for idx in 0..512 {
        let entry = pt.entries[idx];
        if entry == 0 {
            continue;
        }
        let is_leaf = (entry & (PTE_R | PTE_X)) != 0;
        if is_leaf {
            let pa = (entry >> 10) << 12;
            let flags = entry & 0x3FF;
            if (flags & PTE_U) != 0 {
                // user page: decrement ref and free if needed
                let remaining = crate::mm::pmm::decr_ref(pa);
                if remaining == 0 {
                    crate::mm::pmm::free_page(pa);
                }
            }
            pt.entries[idx] = 0;
        } else {
            // pointer to next-level page table
            let next_pa = (entry >> 10) << 12;
            if level > 0 {
                // SAFETY:
                // Preconditions: `next_pa` is a valid intermediate page table physical address.
                // Postconditions: Recursively destroys the child table.
                unsafe { destroy_level(next_pa, level - 1) };
            }
            // free the page table page itself
            crate::mm::pmm::free_page(next_pa);
            pt.entries[idx] = 0;
        }
    }
}

pub fn destroy_user_space(root_pa: usize) -> Result<(), &'static str> {
    // Walk levels and free user mappings and page-table pages (but keep kernel identity entries if present)
    // SAFETY: `root_pa` is the physical address of the root page table for a
    // process that is currently exiting.  The process has been removed from the
    // scheduler so no concurrent access is possible.  Kernel identity entries
    // (indices 0-3) are explicitly skipped so the kernel mapping is never freed.
    unsafe {
        // root is level 2
        let root = &mut *(root_pa as *mut PageTable);
        // For entries that correspond to kernel identity mapping (first 4), skip
        for i in 0..512 {
            let entry = root.entries[i];
            if entry == 0 {
                continue;
            }
            // Indices 0-3 at root level are kernel 1GB identity mappings — never free them.
            if i < 4 {
                continue;
            }
            let pa = (entry >> 10) << 12;
            let is_leaf = (entry & (PTE_R | PTE_X)) != 0;
            if is_leaf {
                let flags = entry & 0x3FF;
                if (flags & PTE_U) != 0 {
                    let page_pa = pa;
                    let remaining = crate::mm::pmm::decr_ref(page_pa);
                    if remaining == 0 {
                        crate::mm::pmm::free_page(page_pa);
                    }
                }
                root.entries[i] = 0;
            } else {
                let next_pa = pa;
                destroy_level(next_pa, 1);
                // free the page table page
                crate::mm::pmm::free_page(next_pa);
                root.entries[i] = 0;
            }
        }
        // Free the root page table page itself.
        crate::mm::pmm::free_page(root_pa);
        Ok(())
    }
}

/// Handle an instruction or load page fault in user space by lazily allocating a zero page.
pub fn handle_user_page_fault(fault_va: usize) -> Result<(), &'static str> {
    use crate::mm::pmm::PAGE_SIZE;
    // Reject anything in the upper canonical half (kernel space in Sv39)
    if fault_va >= 0x0000_8000_0000_0000 {
        return Err("kernel-space fault");
    }
    // Reject addresses in the 0-4GB range: those are covered by the kernel
    // identity 1GB superpages at root indices 0-3.  Attempting to map there
    // would cause map_page to misinterpret a 1GB leaf as a page-table pointer.
    if fault_va < 0x1_0000_0000 {
        return Err("address in kernel identity map region");
    }
    let page_va = fault_va & !(PAGE_SIZE - 1);

    // satp::read() is safe — it merely reads a CSR with no side effects.
    let satp_val = riscv::register::satp::read().bits() as usize;
    let ppn = satp_val & 0xFFFFFFFFFFF;
    let root_pa = ppn << 12;

    // 2. Allocate a physical page (zeroed by PMM)
    let frame = crate::mm::pmm::alloc_frame().ok_or("OOM in demand paging")?;

    // 3. Map the new page
    // SAFETY:
    // Preconditions: `root_pa` is derived from the active `SATP` CSR, ensuring it is the valid root page table.
    // Postconditions: Mutably borrows the active root page table.
    let pt = unsafe { &mut *(root_pa as *mut PageTable) };
    match pt.map_page(page_va, frame.pa(), PTE_R | PTE_W | PTE_U) {
        Ok(()) => {
            frame.into_raw(); // Ownership transferred to PTE
            // SAFETY: sfence.vma is a privileged instruction valid in S-mode.
            unsafe { core::arch::asm!("sfence.vma") };
            Ok(())
        }
        Err(_) => {
            // Page was already mapped (e.g., a concurrent fault race).
            // `frame` drops here, automatically freeing the physical page.
            // SAFETY:
            // Preconditions: Process is in supervisor mode.
            // Postconditions: Flushes the TLB so the new mapping or concurrent update is visible.
            unsafe { core::arch::asm!("sfence.vma") };
            Ok(())
        }
    }
}

pub fn init() {
    let root_page = alloc_frame().expect("Failed to allocate root page table").into_raw();
    // SAFETY: `ROOT_PAGE_TABLE` is only written here during single-threaded
    // kernel init before any secondary HARTs are released (SMP_GO_FLAG is
    // not set yet).  After init(), it is only read.
    unsafe {
        ROOT_PAGE_TABLE = root_page;
    }

    // SAFETY:
    // Preconditions: `root_page` is a valid, 4096-aligned physical address just allocated.
    // Postconditions: Modifies the root kernel table to identity-map the first 4GB.
    let root = unsafe { &mut *(root_page as *mut PageTable) };

    // Identity map the first 4GB of physical address space using 1GB mega-pages
    // This covers our UART MMIO (0x1000_0000) and RAM (0x8000_0000)
    for i in 0..4 {
        let pa = i * 0x40000000;
        let ppn = pa >> 12;
        // Set Valid, Read, Write, Execute flags
        root.entries[i] = (ppn << 10) | PTE_V | PTE_R | PTE_W | PTE_X;
    }

    let ppn = root_page >> 12;
    // satp MODE: 8 means Sv39
    let satp_val = (8 << 60) | ppn;

    // SAFETY: satp::write() is a privileged CSR write valid in S-mode.
    // sfence.vma flushes the TLB after enabling paging so stale entries
    // from the identity-map phase are evicted before any user code runs.
    unsafe {
        satp::write(satp_val);
        asm!("sfence.vma");
    }
    println!("VMM initialized. Sv39 paging enabled.");
}

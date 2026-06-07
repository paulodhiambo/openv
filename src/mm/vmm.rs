//! # Virtual Memory Manager (VMM)
//!
//! This module provides the virtual memory manager (VMM) for OpenV.
//! The VMM is responsible for setting up and managing the RISC-V
//! Sv39 page tables, which translate virtual addresses to physical
//! addresses.
//!
//! ## Overview
//!
//! OpenV uses the RISC-V Sv39 paging mode, which supports 39-bit
//! virtual addresses (512 GB virtual address space) organized as
//! a three-level page table hierarchy:
//!
//! - **L2** (root): 512 entries, each pointing to an L1 table
//!  - **L1**: 512 entries, each pointing to an L0 table
//!  - **L0** (leaf): 512 entries, each mapping a 4 KB page
//!
//! Each page table entry (PTE) is 8 bytes and contains:
//! - **PPN** (bits 10-53): Physical page number
//!  - **Flags** (bits 0-7): V (valid), R (read), W (write), X (execute),
//!    U (user), G (global), A (accessed), D (dirty)
//!
//! ## PTE Flags
//!
//! The following flag constants are defined:
//! - [`PTE_V`]: Valid
//!  - [`PTE_R`]: Readable
//!  - [`PTE_W`]: Writable
//!  - [`PTE_X`]: Executable
//!  - [`PTE_U`]: User-accessible
//!
//! ## Kernel Mapping
//!
//! The kernel is identity-mapped in the first 4 GB of physical address
//! space using 1 GB mega-pages (indices 0-3 in the root page table).
//! This covers the UART MMIO at `0x1000_0000` and RAM at `0x8000_0000`.
//!
//! ## User Mapping
//!
//! User processes live above the 4 GB kernel identity map and below the
//! Sv39 upper canonical boundary (`0x0000_8000_0000_0000`). The VMM
//! provides functions for:
//!
//! - **Mapping pages**: [`PageTable::map_page`]
//!  - **Walking page tables**: [`PageTable::walk_page_table`]
//!  - **Cloning user space** (fork): [`clone_user_space`]
//!  - **Destroying user space** (exit): [`destroy_user_space`]
//!  - **Handling page faults**: [`handle_user_page_fault`],
//!    [`handle_store_page_fault`]
//!  - **Unmapping ranges**: [`unmap_range`]
//!
//! ## Copy-on-Write (COW)
//!
//! When a process forks, the user space is cloned using copy-on-write.
//! Both parent and child initially share the same physical pages with
//! the write bit cleared. When either process writes to a COW page, a
//! page fault triggers [`handle_store_page_fault`], which allocates a
//! new page, copies the data, and updates the PTE.
//!
//! ## Demand Paging and Swap
//!
//! User pages are demand-paged. When a process accesses an unmapped
//! page, [`handle_user_page_fault`] is called, which:
//!
//! 1. Checks if the page was previously swapped out (via
//!    [`crate::mm::swap::lookup_swap`]) and restores it.
//! 2. Otherwise, allocates a fresh zeroed page and maps it.
//!
//! ## Safety
//!
//! The VMM uses unsafe operations extensively to manipulate page
//! tables. All unsafe operations are carefully documented with safety
//! preconditions and postconditions.

use crate::mm::pmm::alloc_frame;
use crate::println;
use core::arch::asm;
use riscv::register::satp;

/// PTE Valid flag. If set, the PTE is valid.
pub const PTE_V: usize = 1 << 0;
/// PTE Readable flag. If set, the page can be read.
pub const PTE_R: usize = 1 << 1;
/// PTE Writable flag. If set, the page can be written.
pub const PTE_W: usize = 1 << 2;
/// PTE Executable flag. If set, the page can be executed.
pub const PTE_X: usize = 1 << 3;
/// PTE User-accessible flag. If set, the page is accessible from U-mode.
pub const PTE_U: usize = 1 << 4;

/// A page table with 512 entries.
///
/// This is a 4 KB structure that contains 512 8-byte page table entries.
/// It is aligned to 4 KB to ensure that it can be used as a page table.
#[repr(C, align(4096))]
pub struct PageTable {
    /// The 512 page table entries.
    pub entries: [usize; 512],
}

impl PageTable {
    /// Creates a new empty page table with all entries set to zero.
    pub const fn new() -> Self {
        PageTable { entries: [0; 512] }
    }

    /// Creates a new process page table with kernel identity mapping.
    ///
    /// This function:
    /// 1. Allocates a new root page table.
/// 2. Identity-maps the first 4 GB of physical address space using
    ///    1 GB mega-pages (indices 0-3).
    /// 3. Returns the physical address of the root page table.
    ///
    /// # Returns
    ///
    /// `Ok(root_pa)` with the physical address of the new root page
    /// table, or an error if memory allocation fails.
    ///
    /// # Safety
    ///
    /// The returned page table has kernel identity mappings set up.
    /// User mappings must be added separately via [`PageTable::map_page`].
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
    ///
    /// # Arguments
    ///
    /// * `va` - The virtual address to map. Must be 4 KB-aligned.
    /// * `pa` - The physical address to map to. Must be 4 KB-aligned.
    /// * `flags` - The PTE flags (a combination of [`PTE_V`], [`PTE_R`],
    ///   [`PTE_W`], [`PTE_X`], [`PTE_U`]).
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error if:
    /// - Memory allocation fails while creating intermediate page tables.
    /// - The page is already mapped.
    ///
    /// # Implementation
    ///
    /// This function walks the page table hierarchy from L2 to L0,
    /// allocating intermediate page tables as needed. If a leaf PTE
    /// is already valid, the function returns an error.
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

    /// Walks the page table to find the physical address and flags for a given virtual address.
    ///
    /// # Arguments
    ///
    /// * `root_pa` - The physical address of the root page table.
    /// * `va` - The virtual address to look up.
    ///
    /// # Returns
    ///
    /// `Ok((physical_address, flags))` on success, or an error if:
    /// - The virtual address is unmapped.
    /// - A huge page is encountered (not supported).
    ///
    /// # Safety
    ///
    /// The caller must ensure that `root_pa` is a valid page table
    /// root physical address.
    pub fn walk_page_table(root_pa: usize, va: usize) -> Result<(usize, usize), &'static str> {
        let vpn = [(va >> 12) & 0x1FF, (va >> 21) & 0x1FF, (va >> 30) & 0x1FF];

        let mut pt_pa = root_pa;
        for level in (1..=2).rev() {
            // SAFETY: `pt_pa` is derived from a valid page table root or PTE.
            let pt = unsafe { &*(pt_pa as *const PageTable) };
            let entry = pt.entries[vpn[level]];
            if entry & PTE_V == 0 {
                return Err("unmapped");
            }
            let is_leaf = (entry & (PTE_R | PTE_X)) != 0;
            if is_leaf {
                return Err("huge pages not supported");
            }
            pt_pa = ((entry >> 10) << 12) as usize;
        }

        // SAFETY: `pt_pa` is a valid leaf page table physical address.
        let pt = unsafe { &*(pt_pa as *const PageTable) };
        let entry = pt.entries[vpn[0]];
        if entry & PTE_V == 0 {
            return Err("unmapped");
        }

        let pa = ((entry >> 10) << 12) | (va & 0xFFF);
        let flags = entry & 0x3FF;
        Ok((pa, flags))
    }
}

/// Clones the user portions of a parent's page table into a child page table, making pages copy-on-write.
///
/// `parent_root_pa` and `child_root_pa` are physical addresses of the root page tables.
///
/// # Arguments
///
/// * `parent_root_pa` - The physical address of the parent's root page table.
/// * `child_root_pa` - The physical address of the child's root page table.
///
/// # Returns
///
/// `Ok(())` on success, or an error if memory allocation fails.
///
/// # Safety
///
/// The caller must ensure that:
/// - `parent_root_pa` and `child_root_pa` are valid, 4 KB-aligned
///   physical addresses of page tables owned by the respective processes.
/// - The parent's page table is not concurrently modified (the caller
///   should hold the process lock).
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
                    if crate::mm::pmm::is_managed_page(pa) {
                        // User page: make read-only in both parent and child (COW)
                        if (flags & PTE_W) != 0 {
                            // Clear write in parent
                            let new_parent_entry = (pa >> 12) << 10 | (flags & !PTE_W);
                            parent_pt.entries[idx] = new_parent_entry;
                            flags &= !PTE_W;
                        }
                        // Increment refcount for shared page
                        pmm::incr_ref(pa);
                    } else {
                        // Unmanaged user page (like MMIO mapped via sys_phys_map).
                        // Do not clear PTE_W or increment refcount. Just copy mapping as-is.
                    }
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

/// Handles a store page fault at the given virtual address in the currently active page table.
///
/// Performs copy-on-write if the page is a user mapping that is currently read-only.
///
/// # Arguments
///
/// * `root_pa` - The physical address of the active root page table.
/// * `fault_va` - The faulting virtual address.
///
/// # Returns
///
/// `Ok(())` on success, or an error if:
/// - The faulting address is not a COW candidate.
/// - Memory allocation fails.
/// - The page is an unmanaged physical page (e.g., MMIO).
///
/// # Safety
///
/// The caller must ensure that `root_pa` is a valid page table root
/// physical address (typically read from the active `SATP` CSR).
pub fn handle_store_page_fault(root_pa: usize, fault_va: usize) -> Result<(), &'static str> {
    use crate::mm::pmm;

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
        if !crate::mm::pmm::is_managed_page(pa) {
            return Err("Cannot COW an unmanaged physical page (e.g., MMIO)");
        }
        
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

        // Flush local TLB, then IPI all other HARTs so they also flush.
        // SAFETY: sfence.vma is a privileged instruction valid in S-mode.
        unsafe { core::arch::asm!("sfence.vma") };
        crate::smp::send_ipi_all();

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
                if crate::mm::pmm::is_managed_page(pa) {
                    let remaining = crate::mm::pmm::decr_ref(pa);
                    if remaining == 0 {
                        crate::mm::pmm::free_page(pa);
                    }
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

/// Destroys the user space of a process, freeing all user pages and page table pages.
///
/// The kernel identity mappings (indices 0-3 in the root page table) are
/// preserved. The root page table itself is freed.
///
/// # Arguments
///
/// * `root_pa` - The physical address of the root page table to destroy.
///
/// # Returns
///
/// `Ok(())` on success.
///
/// # Safety
///
/// The caller must ensure that:
/// - `root_pa` is the physical address of the root page table for a
///   process that is currently exiting.
/// - The process has been removed from the scheduler so no concurrent
///   access is possible.
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

/// Handles an instruction or load page fault in user space by lazily allocating a zero page.
///
/// # Arguments
///
/// * `root_pa` - The physical address of the active root page table.
/// * `fault_va` - The faulting virtual address.
///
/// # Returns
///
/// `Ok(())` on success, or an error if:
/// - The faulting address is in kernel space.
/// - The faulting address is in the kernel identity map region.
/// - The page was previously swapped out and swap-in failed.
/// - Memory allocation fails.
///
/// # Safety
///
/// The caller must ensure that `root_pa` is a valid page table root
/// physical address (typically read from the active `SATP` CSR).
pub fn handle_user_page_fault(root_pa: usize, fault_va: usize, extra_flags: usize) -> Result<(), &'static str> {
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

    // Check swap first; if a page was previously evicted, restore it.
    if crate::mm::swap::lookup_swap(root_pa, page_va) {
        return crate::mm::swap::swap_in(root_pa, page_va);
    }

    // Allocate a fresh physical page (zeroed by PMM).
    let frame = crate::mm::pmm::alloc_frame().ok_or("OOM in demand paging")?;

    // Map the new page with base R/W/U permissions plus any caller-supplied flags
    // (e.g. PTE_X for instruction-fetch faults so the CPU can re-execute).
    // SAFETY: `root_pa` is derived from the active SATP CSR.
    let pt = unsafe { &mut *(root_pa as *mut PageTable) };
    let flags = PTE_R | PTE_W | PTE_U | extra_flags;
    match pt.map_page(page_va, frame.pa(), flags) {
        Ok(()) => {
            frame.into_raw(); // Ownership transferred to PTE
            // Flush local TLB then IPI all other HARTs.
            unsafe { core::arch::asm!("sfence.vma") };
            crate::smp::send_ipi_all();
            Ok(())
        }
        Err(_) => {
            // Page was already mapped (e.g., a concurrent fault race).
            // `frame` drops here, automatically freeing the physical page.
            unsafe { core::arch::asm!("sfence.vma") };
            crate::smp::send_ipi_all();
            Ok(())
        }
    }
}

/// Unmaps all 4 KB pages in `[va_start, va_start+len)` from `root_pa`.
///
/// Managed physical pages are freed when their ref-count drops to zero.
///
/// # Arguments
///
/// * `root_pa` - The physical address of the root page table.
/// * `va_start` - The start of the virtual address range to unmap.
/// * `len` - The length of the virtual address range to unmap.
///
/// # Safety
///
/// The caller must ensure that `root_pa` is a valid page table root
/// physical address.
pub fn unmap_range(root_pa: usize, va_start: usize, len: usize) {
    use crate::mm::pmm;
    let page_size = pmm::PAGE_SIZE;
    let end = va_start.saturating_add(len);
    let mut va = va_start & !(page_size - 1);
    while va < end {
        let vpn = [(va >> 12) & 0x1FF, (va >> 21) & 0x1FF, (va >> 30) & 0x1FF];
        // SAFETY: root_pa comes from the active process SATP and is valid.
        let root = unsafe { &mut *(root_pa as *mut PageTable) };
        let l2e = root.entries[vpn[2]];
        if l2e & PTE_V == 0 || (l2e & (PTE_R | PTE_X)) != 0 {
            va = va.saturating_add(1 << 30); // skip whole 1 GB region
            va &= !(page_size - 1);
            continue;
        }
        let l1_pa = (l2e >> 10) << 12;
        let l1 = unsafe { &mut *(l1_pa as *mut PageTable) };
        let l1e = l1.entries[vpn[1]];
        if l1e & PTE_V == 0 || (l1e & (PTE_R | PTE_X)) != 0 {
            va = va.saturating_add(1 << 21); // skip whole 2 MB region
            va &= !(page_size - 1);
            continue;
        }
        let l0_pa = (l1e >> 10) << 12;
        let l0 = unsafe { &mut *(l0_pa as *mut PageTable) };
        let l0e = l0.entries[vpn[0]];
        if l0e & PTE_V != 0 {
            let pa = (l0e >> 10) << 12;
            let flags = l0e & 0x3FF;
            if (flags & PTE_U) != 0 && pmm::is_managed_page(pa) {
                let remaining = pmm::decr_ref(pa);
                if remaining == 0 {
                    pmm::free_page(pa);
                }
            }
            l0.entries[vpn[0]] = 0;
        }
        va += page_size;
    }
    // Send IPI to all harts for TLB shootdown
    crate::smp::send_ipi_all();
    // Flush TLB on this hart
    unsafe { core::arch::asm!("sfence.vma") };
}


/// Validates that `[ptr, ptr + count * size_of::<T>())` lies entirely within
/// the user-space virtual address window and is not null.
///
/// Does NOT verify that the pages are actually mapped; call sites must ensure
/// the faulting path is handled or walk the page table separately.
///
/// # Arguments
///
/// * `_tf` - The trap frame (unused, but kept for API consistency).
/// * `ptr` - The pointer to validate.
/// * `count` - The number of elements of type `T`.
///
/// # Returns
///
/// `true` if the pointer and count describe a valid user-space range,
/// `false` otherwise.
pub fn is_user_pointer_valid<T>(
    _tf: &crate::trap::TrapFrame,
    ptr: *const T,
    count: usize,
) -> bool {
    if ptr.is_null() {
        return false;
    }
    let byte_len = core::mem::size_of::<T>().saturating_mul(count);
    let start = ptr as usize;
    let end = start.saturating_add(byte_len);
    // User processes live above the 4 GB kernel identity map and below the
    // Sv39 upper canonical boundary (0x0000_8000_0000_0000).
    start >= 0x1_0000_0000 && end <= 0x0000_8000_0000_0000
}

/// Initializes the virtual memory manager.
///
/// This function:
/// 1. Allocates a root page table for the kernel.
/// 2. Identity-maps the first 4 GB of physical address space.
/// 3. Enables Sv39 paging by writing the `SATP` CSR.
/// 4. Flushes the TLB.
///
/// This should be called once during kernel boot, after the PMM is
/// initialized.
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

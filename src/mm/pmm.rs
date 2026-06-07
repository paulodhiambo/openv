//! # Physical Memory Manager (PMM)
//!
//! This module provides the physical memory manager (PMM) for OpenV.
//! The PMM is responsible for tracking physical page frames, managing
//! their reference counts, and providing allocation and deallocation
//! services to the rest of the kernel.
//!
//! ## Overview
//!
//! The PMM manages physical memory at page granularity (4 KB pages).
//! It uses:
//!
//! - **Linked free list**: Each free page's first 8 bytes contain a
//!    pointer to the next free page. This allows O(1) allocation and
//!    deallocation.
//!  - **Reference counting**: Each page has a reference count that
//!    tracks how many owners it has. When the count reaches zero,
//!    the page is returned to the free list.
//!  - **Reserved regions**: The FDT (flattened device tree) and
//!    initrd (initial ramdisk) regions are reserved and not added
//!    to the free list.
//!
//! ## Initialization
//!
//! The PMM is initialized by [`init`], which walks the DTB to find
//! the RAM region and reserved regions, then builds the free list
//! in ascending address order. This is a single-threaded operation
//! that runs before secondary HARTs are started.
//!
//! ## Allocation
//!
//! Pages are allocated via [`alloc_frame`] or [`alloc_page`]. The
//! former returns a RAII guard ([`PhysFrame`]) that automatically
//! frees the page when dropped, while the latter returns a raw
//! physical address.
//!
//! ## Reference Counting
//!
//! The PMM supports reference counting via [`incr_ref`] and
//! [`decr_ref`]. This is used for copy-on-write (COW) sharing of
//! physical pages between processes.
//!
//! ## Swap
//!
//! If the physical memory is exhausted, [`alloc_page`] attempts to
//! evict a page to swap via [`crate::mm::swap::try_evict_page`] and
//! retry the allocation.
//!
//! ## Safety
//!
//! The PMM uses volatile memory operations to manipulate the free
//! list and reference counts. All public functions are SMP-safe and
//! can be called from any context.

use crate::println;
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::sync::Mutex;

/// Size of a physical page in bytes. Always 4 KB on RISC-V.
pub const PAGE_SIZE: usize = 4096;

// External symbol marking the end of the kernel's BSS section.
unsafe extern "C" {
    static _stack_end: u8;
}

// Written once at boot (before any HART calls alloc_page); safe to read without a lock
// after that because they never change.
static RAM_START: AtomicUsize = AtomicUsize::new(0);
static RAM_END:   AtomicUsize = AtomicUsize::new(0);

// Linked free list: each free page's first word points to the next free page.
// Protected by a spinlock so alloc_page/free_page are SMP-safe.
static FREE_LIST: Mutex<usize> = Mutex::new(0);

// O(1) refcount table: one u16 per 4KB page, indexed by (pa - ram_start) / PAGE_SIZE.
// 524288 × 4 KB = 2 GB — covers QEMU virt with up to 2 GB of RAM (-m 2G).
// The table itself is 1 MB in BSS (524288 × 2 bytes).
const MAX_PAGES: usize = 524288;
static PAGE_REF_COUNTS: Mutex<[u16; MAX_PAGES]> = Mutex::new([0u16; MAX_PAGES]);

/// Computes the page index for a physical address.
///
/// # Arguments
///
/// * `pa` - The physical address.
///
/// # Returns
///
/// The index into [`PAGE_REF_COUNTS`], or [`MAX_PAGES`] if the
/// address is out of range.
#[inline]
fn page_index(pa: usize) -> usize {
    let base = RAM_START.load(Ordering::Relaxed);
    if pa < base {
        return MAX_PAGES; // sentinel: out of RAM range
    }
    let idx = (pa - base) / PAGE_SIZE;
    if idx < MAX_PAGES { idx } else { MAX_PAGES }
}

/// An RAII guard for an allocated physical page.
///
/// Dropping this struct automatically decrements the refcount and
/// frees the page if the refcount reaches zero.
///
/// # Fields
///
/// * `0` - The physical address of the frame.
#[derive(Debug)]
pub struct PhysFrame(pub usize);

impl PhysFrame {
    /// Consumes the guard and returns the underlying physical address
    /// without freeing it.
    ///
    /// Use this when transferring ownership to a page table or other
    /// structure that will manage the frame's lifetime.
    ///
    /// # Returns
    ///
    /// The physical address of the frame.
    #[inline]
    pub fn into_raw(self) -> usize {
        let pa = self.0;
        core::mem::forget(self);
        pa
    }

    /// Returns the physical address of this frame.
    ///
    /// # Returns
    ///
    /// The physical address of the frame.
    #[inline]
    pub fn pa(&self) -> usize {
        self.0
    }
}

impl Drop for PhysFrame {
    /// Frees the frame when the guard is dropped.
    ///
    /// Decrements the reference count and, if it reaches zero,
    /// returns the page to the free list.
    fn drop(&mut self) {
        let remaining = decr_ref(self.0);
        if remaining == 0 {
            free_page(self.0);
        }
    }
}

/// Initializes the physical memory manager.
///
/// This function:
/// 1. Parses the DTB to find the RAM region.
/// 2. Identifies reserved regions (FDT and initrd).
/// 3. Builds the free list in ascending address order.
/// 4. Stores the RAM start and end addresses.
///
/// # Arguments
///
/// * `dtb_ptr` - The physical address of the device tree blob (DTB).
///
/// # Safety
///
/// The caller must ensure that `dtb_ptr` is a valid DTB address.
/// This function is called once during kernel boot, before any
/// secondary HARTs are started.
pub fn init(dtb_ptr: usize) {
    // SAFETY:
    // Preconditions: `dtb_ptr` must point to a valid flattened device tree structure in memory passed by OpenSBI.
    // Postconditions: Returns a parsed `Fdt` structure that safely wraps the underlying device tree memory.
    let fdt = unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8).unwrap() };

    let memory = fdt.memory();
    let region = memory
        .regions()
        .next()
        .expect("No memory region found in DTB");

    let ram_start = region.starting_address as usize;
    let ram_size  = region.size.unwrap_or(0);
    let ram_end   = ram_start + ram_size;

    RAM_START.store(ram_start, Ordering::Relaxed);
    RAM_END  .store(ram_end,   Ordering::Relaxed);

    // Find reserved regions (FDT and Initrd).
    let fdt_start = dtb_ptr;
    let fdt_end   = fdt_start + fdt.total_size();

    let mut initrd_start = 0usize;
    let mut initrd_end   = 0usize;
    if let Some(chosen) = fdt.find_node("/chosen")
        && let (Some(s), Some(e)) = (
            chosen.property("linux,initrd-start"),
            chosen.property("linux,initrd-end"),
        )
    {
        initrd_start = s.as_usize().unwrap_or(0);
        initrd_end   = e.as_usize().unwrap_or(0);
    }

    // SAFETY:
    // Preconditions: None. `_stack_end` is a linker-provided symbol marking the end of the kernel's .bss section.
    // Postconditions: Calculates the first page-aligned physical address immediately following the kernel image.
    let kernel_end =
        unsafe { (&_stack_end as *const u8 as usize + PAGE_SIZE - 1) & !(PAGE_SIZE - 1) };

    // Build the free list in ascending address order by linking each free page's
    // first word to the next free page.  Ascending order is required by heap.rs,
    // which detects contiguous runs via `prev + PAGE_SIZE == pa`.
    // Single-threaded at boot so no lock needed here.
    let mut head: usize = 0;
    let mut prev: usize = 0;
    let mut curr = kernel_end;
    while curr + PAGE_SIZE <= ram_end {
        let next = curr + PAGE_SIZE;
        let overlaps_fdt    = curr < fdt_end   && next > fdt_start;
        let overlaps_initrd = initrd_start > 0 && curr < initrd_end && next > initrd_start;
        if !overlaps_fdt && !overlaps_initrd {
            if head == 0 {
                head = curr;
            } else {
                // SAFETY:
                // Preconditions: `prev` is a valid, aligned physical address that is within RAM bounds and not reserved. It is exclusively owned by the PMM.
                // Postconditions: Writes the `curr` physical address to the first 8 bytes of `prev`, establishing the linked-list structure.
                unsafe { (prev as *mut usize).write_volatile(curr); }
            }
            prev = curr;
        }
        curr = next;
    }
    if prev != 0 {
        // SAFETY:
        // Preconditions: `prev` is a valid, aligned physical address representing the last free page.
        // Postconditions: Writes 0 (null) to the first 8 bytes of the last page to properly terminate the intrusive free list.
        unsafe { (prev as *mut usize).write_volatile(0); } // terminate the list
    }
    *FREE_LIST.lock() = head;

    println!("PMM initialized.");
    println!("RAM Start: {:#x}", ram_start);
    println!("RAM Size: {} MB", ram_size / 1024 / 1024);
    println!("Kernel End: {:#x}", kernel_end);
    println!("Reserved FDT: {:#x} - {:#x}", fdt_start, fdt_end);
    if initrd_start > 0 {
        println!("Reserved Initrd: {:#x} - {:#x}", initrd_start, initrd_end);
    }
}

/// Allocates a physical page frame.
///
/// # Returns
///
/// `Some(PhysFrame)` on success, or `None` if no pages are available.
/// The returned [`PhysFrame`] will automatically free the page when
/// dropped.
pub fn alloc_frame() -> Option<PhysFrame> {
    let page = {
        let mut head = FREE_LIST.lock();
        if *head == 0 {
            return None;
        }
        let page = *head;
        // SAFETY:
        // Preconditions: `page` is a physical address safely popped from the head of the free list. By PMM invariants, its first 8 bytes contain the address of the next free page (or 0).
        // Postconditions: Reads that address to correctly update the list head.
        *head = unsafe { (page as *const usize).read_volatile() };
        page
    };

    // Zero the page before handing it out — prevents information leaks between
    // processes and ensures page table pages start with all-invalid entries.
    // SAFETY:
    // Preconditions: `page` is a newly allocated 4096-byte frame exclusively owned by this call. It is valid to write zeroes.
    // Postconditions: The entire 4096-byte physical frame is zeroed out, preventing information leaks.
    unsafe { core::ptr::write_bytes(page as *mut u8, 0, PAGE_SIZE); }

    let idx = page_index(page);
    if idx < MAX_PAGES {
        PAGE_REF_COUNTS.lock()[idx] = 1;
    }

    Some(PhysFrame(page))
}

/// Allocates a raw physical page.
///
/// This is a convenience function that calls [`alloc_frame`] and
/// converts the result to a raw physical address via [`PhysFrame::into_raw`].
///
/// If the physical memory is exhausted, this function attempts to
/// evict a page to swap via [`crate::mm::swap::try_evict_page`] and
/// retry the allocation.
///
/// # Returns
///
/// `Some(phys_addr)` on success, or `None` if no pages are available
/// and swap eviction failed.
pub fn alloc_page() -> Option<usize> {
    if let Some(f) = alloc_frame() {
        return Some(f.into_raw());
    }
    // Physical memory exhausted — try to evict a page to swap and retry once.
    if crate::mm::swap::try_evict_page() {
        alloc_frame().map(|f| f.into_raw())
    } else {
        None
    }
}

/// Returns a physical page to the free list.
///
/// This function clears the reference count and adds the page to the
/// head of the free list. It acquires both [`PAGE_REF_COUNTS`] and
/// [`FREE_LIST`] locks together to ensure that there is no window
/// where the refcount is 0 but the page hasn't been returned to the
/// free list yet.
///
/// # Arguments
///
/// * `page` - The physical address of the page to free. Must be
///   4 KB-aligned.
///
/// # Panics
///
/// Panics if `page` is not 4 KB-aligned (in debug builds).
pub fn free_page(page: usize) {
    debug_assert!(page.is_multiple_of(PAGE_SIZE), "free_page: misaligned address {:#x}", page);

    // Acquire both locks together so there is no window where the refcount
    // is 0 but the page hasn't been returned to the free list yet.
    // Lock order: PAGE_REF_COUNTS before FREE_LIST (see src/sync.rs).
    let mut counts = PAGE_REF_COUNTS.lock();
    let mut head   = FREE_LIST.lock();

    let idx = page_index(page);
    if idx < MAX_PAGES {
        counts[idx] = 0;
    }

    // SAFETY: `page` is a valid, 4096-byte-aligned physical frame with no
    // active references.  We write the old head into the frame's first word
    // and install the frame as the new head of the free list.
    unsafe { (page as *mut usize).write_volatile(*head); }
    *head = page;
}

/// Increments the reference count for the given physical page.
///
/// This is used for copy-on-write (COW) sharing of physical pages
/// between processes. If the page is not managed by the PMM, this
/// function does nothing.
///
/// # Arguments
///
/// * `page` - The physical address of the page.
pub fn incr_ref(page: usize) {
    let idx = page_index(page);
    if idx < MAX_PAGES {
        let mut counts = PAGE_REF_COUNTS.lock();
        counts[idx] = counts[idx].saturating_add(1);
    }
}

/// Decrements the reference count and returns the new count.
///
/// If the page is not managed by the PMM, this function returns 0.
///
/// # Arguments
///
/// * `page` - The physical address of the page.
///
/// # Returns
///
/// The new reference count, or 0 if the page is untracked.
pub fn decr_ref(page: usize) -> usize {
    if !is_managed_page(page) {
        return 0;
    }
    let idx = page_index(page);
    if idx < MAX_PAGES {
        let mut counts = PAGE_REF_COUNTS.lock();
        if counts[idx] > 1 {
            counts[idx] -= 1;
            counts[idx] as usize
        } else {
            counts[idx] = 0;
            0
        }
    } else {
        0
    }
}

/// Checks if a physical address is managed by the PMM.
///
/// # Arguments
///
/// * `pa` - The physical address.
///
/// # Returns
///
/// `true` if the address is within the PMM's managed range,
/// `false` otherwise.
pub fn is_managed_page(pa: usize) -> bool {
    let base = RAM_START.load(core::sync::atomic::Ordering::Relaxed);
    pa >= base && pa < (base + MAX_PAGES * PAGE_SIZE)
}

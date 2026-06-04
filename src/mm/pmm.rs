use crate::println;
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::sync::Mutex;

pub const PAGE_SIZE: usize = 4096;

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
const MAX_PAGES: usize = 262144; // covers up to 1 GB of RAM
static PAGE_REF_COUNTS: Mutex<[u16; MAX_PAGES]> = Mutex::new([0u16; MAX_PAGES]);

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
/// Dropping this struct automatically decrements the refcount and frees the page
/// if the refcount reaches zero.
#[derive(Debug)]
pub struct PhysFrame(pub usize);

impl PhysFrame {
    /// Consumes the guard and returns the underlying physical address without freeing it.
    /// Use this when transferring ownership to a page table or other structure.
    #[inline]
    pub fn into_raw(self) -> usize {
        let pa = self.0;
        core::mem::forget(self);
        pa
    }

    /// Returns the physical address of this frame.
    #[inline]
    pub fn pa(&self) -> usize {
        self.0
    }
}

impl Drop for PhysFrame {
    fn drop(&mut self) {
        let remaining = decr_ref(self.0);
        if remaining == 0 {
            free_page(self.0);
        }
    }
}

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
    if let Some(chosen) = fdt.find_node("/chosen") {
        if let (Some(s), Some(e)) = (
            chosen.property("linux,initrd-start"),
            chosen.property("linux,initrd-end"),
        ) {
            initrd_start = s.as_usize().unwrap_or(0);
            initrd_end   = e.as_usize().unwrap_or(0);
        }
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

pub fn alloc_page() -> Option<usize> {
    alloc_frame().map(|f| f.into_raw())
}

pub fn free_page(page: usize) {
    debug_assert!(page % PAGE_SIZE == 0, "free_page: misaligned address {:#x}", page);

    // Reset refcount before returning to the free list so that decr_ref on a
    // previously-freed-then-reallocated page starts from a clean baseline.
    let idx = page_index(page);
    if idx < MAX_PAGES {
        PAGE_REF_COUNTS.lock()[idx] = 0;
    }

    let mut head = FREE_LIST.lock();
    // SAFETY:
    // Preconditions: `page` is a valid, 4096-byte aligned physical frame being freed, and absolutely no active references exist to it.
    // Postconditions: The first 8 bytes of `page` are overwritten with the old list head, and `page` becomes the new head of the free list.
    unsafe { (page as *mut usize).write_volatile(*head); }
    *head = page;
}

/// Increment reference count for the given physical page (used for COW sharing).
pub fn incr_ref(page: usize) {
    let idx = page_index(page);
    if idx < MAX_PAGES {
        let mut counts = PAGE_REF_COUNTS.lock();
        counts[idx] = counts[idx].saturating_add(1);
    }
}

/// Decrement reference count and return the new count.  Returns 0 if untracked.
pub fn decr_ref(page: usize) -> usize {
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

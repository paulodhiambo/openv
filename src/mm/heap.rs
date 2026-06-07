//! # Kernel Heap Allocator
//!
//! This module provides the kernel heap allocator for OpenV. The heap
//! is used for dynamic memory allocation in the kernel (e.g., for
//! [`Vec`], [`String`], and [`Box`]).
//!
//! ## Implementation
//!
//! OpenV uses the [`buddy_system_allocator::LockedHeap`] as its global
//! heap allocator. The buddy allocator is a well-known, efficient
//! allocator that splits memory into power-of-two-sized blocks.
//!
//! ## Initialization
//!
//! The heap is initialized by [`init`], which allocates up to 8192
//! physical pages (32 MB) from the PMM and adds them to the buddy
//! allocator. Pages from [`alloc_page`] may not be contiguous if the
//! PMM free list has gaps (e.g., due to FDT or initrd reservations).
//! To handle this, [`init`] groups consecutive pages into segments
//! and adds each segment separately.
//!
//! ## Safety
//!
//! The heap allocator is marked as `#[global_allocator]`, which makes
//! it the default allocator for all `no_std` heap allocations in the
//! kernel. It is safe to use from any context, but allocations are
//! not interrupt-safe (they may block or spin).
//!
//! [`Vec`]: https://doc.rust-lang.org/alloc/vec/struct.Vec.html
//! [`String`]: https://doc.rust-lang.org/alloc/string/struct.String.html
//! [`Box`]: https://doc.rust-lang.org/alloc/boxed/struct.Box.html
//! [`init`]: fn.init.html
//! [`alloc_page`]: ../pmm/fn.alloc_page.html

use crate::mm::pmm::{PAGE_SIZE, alloc_page};
use buddy_system_allocator::LockedHeap;

/// Global kernel heap allocator.
///
/// This is the default allocator for all `no_std` heap allocations in
/// the kernel. It is based on the buddy allocator with a maximum order
/// of 32 (supporting allocations up to 2^32 bytes).
#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap<32> = LockedHeap::empty();

/// Initializes the kernel heap allocator.
///
/// This function allocates up to 8192 physical pages (32 MB) from the
/// PMM and adds them to the buddy allocator. The pages are grouped
/// into contiguous segments and each segment is added separately.
///
/// # Implementation Details
///
/// The buddy allocator rounds each allocation to the nearest power of 2,
/// so loading a large binary (e.g., 4 MB file → 8 MB buddy block) needs
/// enough head-room above that block for the rest of the kernel.
///
/// Pages from [`alloc_page`] may NOT be contiguous if the PMM free list
/// had gaps (e.g., FDT or initrd reservations). We group consecutive
/// pages into segments and add each segment separately so the buddy
/// allocator never receives memory it does not own.
///
/// # Side Effects
///
/// Prints a message to the console indicating the number of pages and
/// megabytes added to the heap allocator.
///
/// [`alloc_page`]: ../pmm/fn.alloc_page.html
pub fn init() {
    // Hand up to 8192 PMM pages (32 MB) to the buddy allocator.
    // The buddy allocator rounds each allocation to the nearest power of 2,
    // so loading a large binary (e.g. 4 MB file → 8 MB buddy block) needs
    // enough head-room above that block for the rest of the kernel.
    // Pages from alloc_page() may NOT be contiguous if the PMM free list
    // had gaps (e.g. FDT or initrd reservations).  We group consecutive
    // pages into segments and add each segment separately so the buddy
    // allocator never receives memory it does not own.
    let mut n = 0usize;
    let mut prev_pa: Option<usize> = None;
    let mut seg_start = 0;
    let mut total_added = 0usize;
    for _ in 0..8192 {
        if let Some(pa) = alloc_page() {
            match prev_pa {
                None => {
                    seg_start = pa;
                }
                Some(prev) if prev + PAGE_SIZE != pa => {
                    unsafe {
                        HEAP_ALLOCATOR
                            .lock()
                            .add_to_heap(seg_start, prev + PAGE_SIZE);
                    }
                    total_added += (prev - seg_start) + PAGE_SIZE;
                    seg_start = pa;
                }
                _ => {}
            }
            prev_pa = Some(pa);
            n += 1;
        }
    }
    if let Some(end) = prev_pa {
        unsafe {
            HEAP_ALLOCATOR
                .lock()
                .add_to_heap(seg_start, end + PAGE_SIZE);
        }
        total_added += (end - seg_start) + PAGE_SIZE;
    }
    crate::println!(
        "Heap: {} pages ({} MB) added to heap allocator",
        n,
        total_added / 1024 / 1024
    );
}

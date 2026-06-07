//! # Memory Management
//!
//! This module provides memory management for OpenV, including physical
//! memory allocation, virtual memory management, and the kernel heap.
//!
//! ## Overview
//!
//! OpenV's memory management is organized into the following submodules:
//!
//! - [`heap`]: Kernel heap allocator based on a linked list of free blocks.
//!  - [`pmm`]: Physical memory manager (PMM), which tracks physical page
//!    frames and their reference counts.
//!  - [`swap`]: Swap implementation for evicting pages to disk.
//!  - [`vmm`]: Virtual memory manager (VMM), which sets up and manages
//!    page tables.
//!  - [`vmo`]: Virtual Memory Objects (VMOs), which are the kernel's
//!    abstraction for contiguous regions of memory.
//!
//! ## Initialization
//!
//! The [`init`] function initializes all memory management subsystems.
//! It should be called once during kernel boot, typically from [`kmain`].
//!
//! [`heap`]: heap/index.html
//! [`pmm`]: pmm/index.html
//! [`swap`]: swap/index.html
//! [`vmm`]: vmm/index.html
//! [`vmo`]: vmo/index.html
//! [`init`]: fn.init.html
//! [`kmain`]: ../fn.kmain.html

/// Kernel heap allocator.
pub mod heap;
/// Physical memory manager (PMM).
pub mod pmm;
/// Swap implementation for evicting pages to disk.
pub mod swap;
/// Virtual memory manager (VMM).
pub mod vmm;
/// Virtual Memory Objects (VMOs).
pub mod vmo;

/// Initializes all memory management subsystems.
///
/// This function initializes the physical memory manager ([`pmm::init`]),
/// the virtual memory manager ([`vmm::init`]), and the kernel heap
/// ([`heap::init`]). It also runs a self-test of the PMM to verify
/// that reference counting and frame allocation work correctly.
///
/// # Arguments
///
/// * `dtb_ptr` - The physical address of the device tree blob (DTB).
///   This is used by the PMM to discover the physical memory layout.
///
/// [`pmm::init`]: pmm/fn.init.html
/// [`vmm::init`]: vmm/fn.init.html
/// [`heap::init`]: heap/fn.init.html
pub fn init(dtb_ptr: usize) {
    pmm::init(dtb_ptr);
    vmm::init();
    heap::init();
    test_memory_manager();
}

/// Self-test for the physical memory manager.
///
/// This function verifies that:
/// 1. Allocating a frame and then dropping it returns the frame to the
///    free list.
/// 2. The free list is LIFO (last-in, first-out), so the same frame
///    is returned on the next allocation.
/// 3. Reference counting works correctly.
///
/// # Panics
///
/// Panics if any of the self-test checks fail.
fn test_memory_manager() {
    crate::println!("--- Testing PMM RAII ---");
    let pa = {
        let frame = pmm::alloc_frame().expect("OOM in PMM test");
        frame.pa()
        // frame drops here, returning the page to the free list
    };

    // Allocate again. Because the free list pushes to the head (LIFO),
    // we should get the exact same physical address back.
    let frame2 = pmm::alloc_frame().expect("OOM in PMM test");
    if pa != frame2.pa() {
        panic!("PMM RAII leak detected! Expected {:#x}, got {:#x}", pa, frame2.pa());
    }
    
    // Test manual reference counting
    pmm::incr_ref(frame2.pa());
    let remaining = pmm::decr_ref(frame2.pa());
    if remaining != 1 {
        panic!("PMM refcount logic failed!");
    }
    
    crate::println!("PMM RAII and Refcount verified.");
}

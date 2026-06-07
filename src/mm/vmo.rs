//! # Virtual Memory Objects (VMOs)
//!
//! This module provides Virtual Memory Objects (VMOs), which are the
//! kernel's abstraction for contiguous regions of memory. VMOs are
//! used to represent memory that can be shared between processes or
//! mapped into multiple address spaces.
//!
//! ## Overview
//!
//! A [`Vmo`] consists of:
//! - A size in bytes.
//!  - An array of physical page addresses that back the virtual memory.
//!
//! VMOs are reference-counted via the [`HandleTable`] (see
//! [`crate::ipc::handle`]). When a VMO is dropped, its physical pages
//! are returned to the PMM.
//!
//! ## Usage
//!
//! VMOs are typically created via [`Vmo::new`], which allocates the
//! necessary physical pages from the PMM. The resulting VMO can be:
//! - Mapped into a process's address space (via the VMM).
//! - Shared with another process by passing a handle over IPC.
//! - Read from or written to directly via the physical page array.
//!
//! ## Limitations
//!
//! Currently, VMOs only support:
//! - Contiguous virtual address ranges backed by physical pages.
//!  - Automatic cleanup on drop.
//!
//! Future versions may add support for:
//!  - Page-level reference counting.
//!  - Copy-on-write semantics.
//!  - Demand paging and swap.
//!
//! [`HandleTable`]: ../../ipc/handle/struct.HandleTable.html

use crate::mm::pmm::{alloc_page, free_page};
use alloc::vec::Vec;

/// A Virtual Memory Object (VMO) is a contiguous range of virtual memory
/// backed by an array of physical pages.
///
/// # Fields
///
/// * `size_bytes` - The size of the VMO in bytes.
/// * `physical_pages` - The physical page addresses that back the VMO.
///
/// # Lifecycle
///
/// When a [`Vmo`] is dropped, all of its physical pages are returned
/// to the PMM. This means that VMOs must be kept alive (via reference
/// counting) as long as their physical pages are in use.
pub struct Vmo {
    /// The size of the VMO in bytes.
    size_bytes: usize,
    /// The physical page addresses that back the VMO.
    physical_pages: Vec<usize>,
}

impl Vmo {
    /// Creates a new VMO with the given size.
    ///
    /// # Arguments
    ///
    /// * `size_bytes` - The size of the VMO in bytes. The actual size
    ///   will be rounded up to the nearest page boundary.
    ///
    /// # Returns
    ///
    /// `Some(Vmo)` on success, or `None` if the PMM is out of memory.
    /// On failure, any pages that were allocated during the attempt
    /// are freed before returning.
    pub fn new(size_bytes: usize) -> Option<Self> {
        // Round up to the nearest page boundary.
        let num_pages = (size_bytes + 4095) / 4096;
        let mut pages = Vec::with_capacity(num_pages);

        for _ in 0..num_pages {
            if let Some(page) = alloc_page() {
                pages.push(page);
            } else {
                // Out of memory, free what we allocated so far.
                for p in pages {
                    free_page(p);
                }
                return None;
            }
        }

        Some(Vmo {
            size_bytes,
            physical_pages: pages,
        })
    }

    /// Returns the size of the VMO in bytes.
    ///
    /// # Returns
    ///
    /// The size of the VMO in bytes. This is the value passed to
    /// [`Vmo::new`], which may have been rounded up to a page boundary.
    pub fn size(&self) -> usize {
        self.size_bytes
    }

    /// Returns a slice of the physical page addresses that back the VMO.
    ///
    /// # Returns
    ///
    /// A slice of `usize` physical page addresses. The length of the
    /// slice is `(size() + 4095) / 4096`.
    pub fn pages(&self) -> &[usize] {
        &self.physical_pages
    }
}

impl Drop for Vmo {
    /// Frees all physical pages when the VMO is dropped.
    ///
    /// This ensures that physical pages are returned to the PMM when
    /// the VMO is no longer needed.
    fn drop(&mut self) {
        for &page in &self.physical_pages {
            free_page(page);
        }
    }
}

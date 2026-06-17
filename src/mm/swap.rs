//! # In-Memory Compressed Swap (zram)
//!
//! This module provides in-memory compressed swap (zram) for OpenV. When
//! the physical memory manager (PMM) runs out of physical frames, this
//! module evicts cold user pages to a compressed in-memory pool, freeing
//! physical memory. On the next access to an evicted page, the page-fault
//! handler calls [`swap_in`], which decompresses the data back into a
//! fresh frame.
//!
//! ## Overview
//!
//! OpenV's swap implementation is a simple in-memory zram-style design:
//!
//! - **Eviction**: When the PMM is out of memory, [`try_evict_page`]
//!    walks all process page tables and finds a clean user page to
//!    evict. The page is compressed and stored in [`SWAP_MAP`], and
//!    the physical frame is freed.
//!  - **Swap-in**: When a process accesses a swapped-out page, the
//!    page-fault handler calls [`swap_in`], which decompresses the
//!    data into a fresh frame and remaps the page.
//!  - **Cleanup**: When a process exits, [`evict_all`] removes all
//!    swap entries belonging to that process.
//!
//! ## Compression
//!
//! The current implementation uses a simple byte-pair run-length encoding
//! (RLE). The format is alternating `(run_byte, count)` pairs followed
//! by a 0xFF terminator. A future pass can swap in lz4 or zstd without
//! changing the interface.
//!
//! ## Limitations
//!
//! - In-memory only: swapped-out pages consume physical memory (albeit
//!    less due to compression). A future version could swap to disk.
//!  - Simple RLE compression: not as effective as lz4 or zstd for
//!    many types of data.
//!  - No swap accounting: there's no way to tell how much memory is
//!    being used by swap.
//!
//! ## Safety
//!
//! The swap module uses unsafe operations to read and write page table
//! entries and page data. All unsafe operations are carefully documented
//! with safety preconditions and postconditions.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use crate::sync::Mutex;

pub struct SwappedPage {
    pub data: Vec<u8>,
    pub flags: usize,
}

/// Map from `(root_pa, page_va)` to compressed page data and flags.
///
/// The key is a tuple of the page table root physical address and the
/// virtual address of the page. The value is a SwappedPage containing the compressed data and original PTE flags.
static SWAP_MAP: Mutex<BTreeMap<(usize, usize), SwappedPage>> = Mutex::new(BTreeMap::new());

// ── Compression ───────────────────────────────────────────────────────────────

/// Compresses a 4 KB page using byte-pair run-length encoding (RLE).
///
/// The format is alternating `(run_byte, count)` pairs. A count of 0
/// is not used; the maximum run length is 255 bytes.
///
/// # Arguments
///
/// * `src` - The 4 KB page data to compress.
///
/// # Returns
///
/// A [`Vec<u8>`] containing the compressed data.
fn compress_page(src: &[u8; 4096]) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    let mut i = 0usize;
    while i < 4096 {
        let b = src[i];
        let mut run = 1usize;
        while i + run < 4096 && src[i + run] == b && run < 255 {
            run += 1;
        }
        out.push(b);
        out.push(run as u8);
        i += run;
    }
    out
}

/// Decompresses data produced by [`compress_page`] into `dst`.
///
/// # Arguments
///
/// * `src` - The compressed data.
/// * `dst` - The destination 4 KB buffer.
fn decompress_page(src: &[u8], dst: &mut [u8; 4096]) {
    let mut out_pos = 0usize;
    let mut i = 0usize;
    while i + 1 < src.len() && out_pos < 4096 {
        let b   = src[i];
        let run = src[i + 1] as usize;
        i += 2;
        let end = (out_pos + run).min(4096);
        dst[out_pos..end].fill(b);
        out_pos = end;
    }
}

// ── Eviction ──────────────────────────────────────────────────────────────────

/// Tries to evict one clean user page from any process to free physical memory.
///
/// # Returns
///
/// `true` if a page was evicted and freed, `false` if no evictable
/// page was found.
///
/// # Algorithm
///
/// The function:
///
/// 1. Locks the process table.
/// 2. For each process, walks the page table (L2 → L1 → L0).
/// 3. Finds the first valid, user-accessible, managed page.
/// 4. Reads the page data, compresses it, and stores it in [`SWAP_MAP`].
/// 5. Clears the PTE and frees the physical frame.
///
/// # Safety
///
/// This function performs unsafe operations to read and write page
/// table entries and page data. The caller must ensure that the
/// process table is in a consistent state.
pub fn try_evict_page() -> bool {
    use crate::mm::vmm::{PageTable, PTE_V, PTE_U, PTE_X, PTE_R};
    use crate::mm::pmm;

    let table = crate::posix::process::PROCESS_TABLE.lock();

    for proc in table.values() {
        let satp    = proc.satp_val.load(core::sync::atomic::Ordering::Relaxed);
        let root_pa = (satp & 0xFFF_FFFF_FFFF) << 12;
        if root_pa == 0 { continue; }

        // Walk L2 (root)
        // SAFETY: `root_pa` is a valid page table root physical address
        // read from the process's satp value.
        let root = unsafe { &*(root_pa as *const PageTable) };
        for l2i in 4..512 {                      // skip kernel identity (0-3)
            let l2e = root.entries[l2i];
            if l2e & PTE_V == 0 || (l2e & (PTE_R | PTE_X)) != 0 { continue; }
            let l1_pa = (l2e >> 10) << 12;
            // SAFETY: `l1_pa` is a valid physical address read from a
            // valid L2 page table entry.
            let l1 = unsafe { &*(l1_pa as *const PageTable) };
            for l1i in 0..512 {
                let l1e = l1.entries[l1i];
                if l1e & PTE_V == 0 || (l1e & (PTE_R | PTE_X)) != 0 { continue; }
                let l0_pa = (l1e >> 10) << 12;
                // SAFETY: `l0_pa` is a valid physical address read from a
                // valid L1 page table entry.
                let l0 = unsafe { &mut *(l0_pa as *mut PageTable) };
                for l0i in 0..512 {
                    let pte = l0.entries[l0i];
                    if pte & PTE_V == 0 || pte & PTE_U == 0 { continue; }
                    // Skip non-managed pages (MMIO etc.)
                    let pa = (pte >> 10) << 12;
                    if !pmm::is_managed_page(pa) { continue; }
                    // Reconstruct the virtual address from the indices
                    let va = (l2i << 30) | (l1i << 21) | (l0i << 12);

                    // Read the page data
                    // SAFETY: `pa` is a valid, managed physical page.
                    let page_data = unsafe { *(pa as *const [u8; 4096]) };
                    // Compress and store
                    let compressed = compress_page(&page_data);
                    let flags = pte & 0x3FF;
                    SWAP_MAP.lock().insert((root_pa, va), SwappedPage { data: compressed, flags });

                    // Clear the PTE and free the physical frame
                    l0.entries[l0i] = 0;
                    unsafe { core::arch::asm!("sfence.vma") };
                    pmm::decr_ref_and_maybe_free(pa);
                    return true;
                }
            }
        }
    }
    false
}

// ── Restore (swap-in) ─────────────────────────────────────────────────────────

/// Checks if `(root_pa, page_va)` has a swap entry.
///
/// # Arguments
///
/// * `root_pa` - The physical address of the page table root.
/// * `page_va` - The virtual address of the page.
///
/// # Returns
///
/// `true` if there is a swap entry for the given page, `false` otherwise.
pub fn lookup_swap(root_pa: usize, page_va: usize) -> bool {
    SWAP_MAP.lock().contains_key(&(root_pa, page_va))
}

/// Restores a swapped-out page.
///
/// This function:
/// 1. Removes the swap entry from [`SWAP_MAP`].
/// 2. Allocates a fresh physical frame.
/// 3. Decompresses the data into the new frame.
/// 4. Maps the new frame into the page table.
/// 5. Flushes the TLB.
///
/// # Arguments
///
/// * `root_pa` - The physical address of the page table root.
/// * `page_va` - The virtual address of the page.
///
/// # Returns
///
/// `Ok(())` on success, or an error if the swap entry was not found
/// or if a fresh frame could not be allocated.
///
/// # Safety
///
/// This function performs unsafe operations to read and write page
/// table entries and page data. The caller must ensure that `root_pa`
/// is a valid page table root physical address.
pub fn swap_in(root_pa: usize, page_va: usize) -> Result<(), &'static str> {
    use crate::mm::vmm::PageTable;
    use crate::mm::pmm;

    let swapped = SWAP_MAP.lock().remove(&(root_pa, page_va))
        .ok_or("swap entry not found")?;

    let frame = pmm::alloc_frame().ok_or("OOM during swap-in")?;
    // SAFETY: `frame.pa()` is a valid physical address returned by `alloc_frame`.
    let dst = unsafe { &mut *(frame.pa() as *mut [u8; 4096]) };
    decompress_page(&swapped.data, dst);

    // SAFETY: `root_pa` is a valid page table root physical address.
    let pt = unsafe { &mut *(root_pa as *mut PageTable) };
    pt.map_page(page_va, frame.pa(), swapped.flags)?;
    frame.into_raw();
    unsafe { core::arch::asm!("sfence.vma") };
    Ok(())
}

/// Removes all swap entries belonging to `root_pa`.
///
/// This function is called when a process exits to free the memory
/// used by its swapped-out pages.
///
/// # Arguments
///
/// * `root_pa` - The physical address of the page table root.
pub fn evict_all(root_pa: usize) {
    SWAP_MAP.lock().retain(|&(rp, _), _| rp != root_pa);
}

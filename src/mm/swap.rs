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
//!    walks all Stopped process page tables and finds a clean user page to
//!    evict. Only evicts from Stopped processes to avoid races with
//!    concurrent page faults on Running processes.
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
//!  - Eviction is O(n) in total pages and may block for many
//!    milliseconds under heavy memory pressure.
//!
//! ## Safety
//!
//! The swap module uses unsafe operations to read and write page table
//! entries and page data. All unsafe operations are carefully documented
//! with safety preconditions and postconditions.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use crate::sync::Mutex;

pub struct SwappedPage {
    pub data: Vec<u8>,
    pub flags: usize,
}

/// Map from `(root_pa, page_va)` to compressed page data and flags.
///
/// The key is a tuple of the page table root physical address and the
/// virtual address of the page. The value is a SwappedPage containing
/// the compressed data and original PTE flags.
static SWAP_MAP: Mutex<BTreeMap<(usize, usize), SwappedPage>> = Mutex::new(BTreeMap::new());

/// Total number of pages currently in the swap pool (evicted).
static SWAP_TOTAL_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
/// Total compressed size in bytes of all pages in the swap pool.
static SWAP_TOTAL_BYTES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Returns the number of pages currently swapped out.
pub fn swap_page_count() -> usize {
    SWAP_TOTAL_PAGES.load(Ordering::Relaxed)
}

/// Returns the total compressed size (bytes) of all swapped pages.
pub fn swap_total_bytes() -> usize {
    SWAP_TOTAL_BYTES.load(Ordering::Relaxed)
}

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
///
/// # Safety
///
/// This function is safe because it only reads from the provided byte
/// slice and does not perform any unsafe operations.
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

/// Tries to evict one clean user page from a Stopped process to free physical memory.
///
/// Only considers processes in the [`ProcState::Stopped`] state to avoid
/// racing with concurrent page faults on Running processes.  This means
/// the allocator may still fail under heavy memory pressure if no process
/// is currently blocked.
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
/// 2. For each Stopped process, walks the page table (L2 → L1 → L0).
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
    use crate::posix::process::ProcState;

    let table = crate::posix::process::PROCESS_TABLE.lock();

    for proc in table.values() {
        // Only evict from Stopped processes to avoid races with page faults
        // on Running processes.  A Stopped process is blocked in a syscall
        // (e.g. read, waitpid) and is not accessing user pages.
        let is_stopped = matches!(*proc.state.lock(), ProcState::Stopped);
        if !is_stopped {
            continue;
        }

        let satp    = proc.satp_val.load(Ordering::Relaxed);
        let root_pa = (satp & 0xFFF_FFFF_FFFF) << 12;
        if root_pa == 0 { continue; }

        // Walk L2 (root)
        let root = unsafe { &*(root_pa as *const PageTable) };
        for l2i in 4..512 {
            let l2e = root.entries[l2i];
            if l2e & PTE_V == 0 || (l2e & (PTE_R | PTE_X)) != 0 { continue; }
            let l1_pa = (l2e >> 10) << 12;
            let l1 = unsafe { &*(l1_pa as *const PageTable) };
            for l1i in 0..512 {
                let l1e = l1.entries[l1i];
                if l1e & PTE_V == 0 || (l1e & (PTE_R | PTE_X)) != 0 { continue; }
                let l0_pa = (l1e >> 10) << 12;
                let l0 = unsafe { &mut *(l0_pa as *mut PageTable) };
                for l0i in 0..512 {
                    let pte = l0.entries[l0i];
                    if pte & PTE_V == 0 || pte & PTE_U == 0 { continue; }
                    let pa = (pte >> 10) << 12;
                    if !pmm::is_managed_page(pa) { continue; }
                    let va = (l2i << 30) | (l1i << 21) | (l0i << 12);

                    // Read the page data
                    let page_data = unsafe { *(pa as *const [u8; 4096]) };
                    let compressed = compress_page(&page_data);
                    let compressed_len = compressed.len();
                    let flags = pte & 0x3FF;
                    SWAP_MAP.lock().insert((root_pa, va), SwappedPage { data: compressed, flags });

                    // Clear the PTE and free the physical frame
                    l0.entries[l0i] = 0;
                    unsafe { core::arch::asm!("sfence.vma") };
                    pmm::decr_ref_and_maybe_free(pa);

                    // Update accounting
                    SWAP_TOTAL_PAGES.fetch_add(1, Ordering::Relaxed);
                    SWAP_TOTAL_BYTES.fetch_add(compressed_len, Ordering::Relaxed);
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

    let compressed_len = swapped.data.len();
    let frame = pmm::alloc_frame().ok_or("OOM during swap-in")?;
    let dst = unsafe { &mut *(frame.pa() as *mut [u8; 4096]) };
    decompress_page(&swapped.data, dst);

    let pt = unsafe { &mut *(root_pa as *mut PageTable) };
    pt.map_page(page_va, frame.pa(), swapped.flags)?;
    frame.into_raw();
    unsafe { core::arch::asm!("sfence.vma") };

    // Update accounting
    SWAP_TOTAL_PAGES.fetch_sub(1, Ordering::Relaxed);
    SWAP_TOTAL_BYTES.fetch_sub(compressed_len, Ordering::Relaxed);
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
    let mut map = SWAP_MAP.lock();
    let freed: usize = map.iter()
        .filter(|&(&(rp, _), _)| rp == root_pa)
        .map(|(_, sp)| sp.data.len())
        .sum();
    let count = map.iter()
        .filter(|&(&(rp, _), _)| rp == root_pa)
        .count();
    map.retain(|&(rp, _), _| rp != root_pa);
    SWAP_TOTAL_PAGES.fetch_sub(count, Ordering::Relaxed);
    SWAP_TOTAL_BYTES.fetch_sub(freed, Ordering::Relaxed);
}

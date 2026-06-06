/// In-memory compressed swap (zram).
///
/// When the PMM runs out of physical frames, this module evicts cold user
/// pages to a compressed in-memory pool, freeing physical memory.  On the
/// next access to an evicted page the page-fault handler calls `swap_in`,
/// which decompresses the data back into a fresh frame.
///
/// Compression: simple run-length encoding (byte-pair RLE).
/// A future pass can swap in lz4 or zstd without changing the interface.
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use crate::sync::Mutex;

/// Map from (root_pa, page_va) to compressed page data.
static SWAP_MAP: Mutex<BTreeMap<(usize, usize), Vec<u8>>> = Mutex::new(BTreeMap::new());

// ── Compression ───────────────────────────────────────────────────────────────

/// Compress 4 KB page data with byte-pair RLE.
/// Format: alternating (run_byte, count) pairs followed by a 0xFF terminator.
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

/// Decompress data produced by `compress_page` into `dst`.
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

/// Try to evict one clean user page from any process to free physical memory.
/// Returns true if a page was evicted and freed.
pub fn try_evict_page() -> bool {
    use crate::mm::vmm::{PageTable, PTE_V, PTE_U, PTE_X, PTE_R};
    use crate::mm::pmm;

    let table = crate::posix::process::PROCESS_TABLE.lock();

    for proc in table.values() {
        let satp    = proc.satp_val.load(core::sync::atomic::Ordering::Relaxed);
        let root_pa = (satp & 0xFFF_FFFF_FFFF) << 12;
        if root_pa == 0 { continue; }

        // Walk L2 (root)
        let root = unsafe { &*(root_pa as *const PageTable) };
        for l2i in 4..512 {                      // skip kernel identity (0-3)
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
                    // Skip non-managed pages (MMIO etc.)
                    let pa = (pte >> 10) << 12;
                    if !pmm::is_managed_page(pa) { continue; }
                    // Reconstruct the virtual address from the indices
                    let va = (l2i << 30) | (l1i << 21) | (l0i << 12);

                    // Read the page data
                    let page_data = unsafe { *(pa as *const [u8; 4096]) };
                    // Compress and store
                    let compressed = compress_page(&page_data);
                    SWAP_MAP.lock().insert((root_pa, va), compressed);

                    // Clear the PTE and free the physical frame
                    l0.entries[l0i] = 0;
                    unsafe { core::arch::asm!("sfence.vma") };
                    let remaining = pmm::decr_ref(pa);
                    if remaining == 0 {
                        pmm::free_page(pa);
                    }
                    return true;
                }
            }
        }
    }
    false
}

// ── Restore (swap-in) ─────────────────────────────────────────────────────────

/// Check if `(root_pa, page_va)` has a swap entry.
pub fn lookup_swap(root_pa: usize, page_va: usize) -> bool {
    SWAP_MAP.lock().contains_key(&(root_pa, page_va))
}

/// Restore a swapped-out page: allocate a fresh frame, decompress data into it,
/// map it into the page table, and remove the swap entry.
/// Returns Ok(()) on success.
pub fn swap_in(root_pa: usize, page_va: usize) -> Result<(), &'static str> {
    use crate::mm::vmm::{PageTable, PTE_R, PTE_W, PTE_U};
    use crate::mm::pmm;

    let compressed = SWAP_MAP.lock().remove(&(root_pa, page_va))
        .ok_or("swap entry not found")?;

    let frame = pmm::alloc_frame().ok_or("OOM during swap-in")?;
    let dst = unsafe { &mut *(frame.pa() as *mut [u8; 4096]) };
    decompress_page(&compressed, dst);

    let pt = unsafe { &mut *(root_pa as *mut PageTable) };
    pt.map_page(page_va, frame.pa(), PTE_R | PTE_W | PTE_U)?;
    frame.into_raw();
    unsafe { core::arch::asm!("sfence.vma") };
    Ok(())
}

/// Remove all swap entries belonging to `root_pa` (called when a process exits).
pub fn evict_all(root_pa: usize) {
    SWAP_MAP.lock().retain(|&(rp, _), _| rp != root_pa);
}

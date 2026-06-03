use buddy_system_allocator::LockedHeap;
use crate::mm::pmm::{alloc_page, PAGE_SIZE};

#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap<32> = LockedHeap::empty();

pub fn init() {
    // Hand up to 2048 PMM pages (8 MB) to the buddy allocator.
    // Pages from alloc_page() may NOT be contiguous if the PMM free list
    // had gaps (e.g. FDT or initrd reservations).  We group consecutive
    // pages into segments and add each segment separately so the buddy
    // allocator never receives memory it does not own.
    let mut n = 0usize;
    let mut prev_pa: Option<usize> = None;
    let mut seg_start = 0;
    let mut total_added = 0usize;
    for _ in 0..2048 {
        if let Some(pa) = alloc_page() {
            match prev_pa {
                None => {
                    seg_start = pa;
                }
                Some(prev) if prev + PAGE_SIZE != pa => {
                    unsafe { HEAP_ALLOCATOR.lock().add_to_heap(seg_start, prev + PAGE_SIZE); }
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
        unsafe { HEAP_ALLOCATOR.lock().add_to_heap(seg_start, end + PAGE_SIZE); }
        total_added += (end - seg_start) + PAGE_SIZE;
    }
    crate::println!("Heap: {} pages ({} MB) added to heap allocator", n, total_added / 1024 / 1024);
}

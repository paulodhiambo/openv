use crate::println;
use spin::Mutex;

pub const PAGE_SIZE: usize = 4096;

unsafe extern "C" {
    static _stack_end: u8; // The end of the kernel's stack / bss
}

static mut NEXT_FREE_PAGE: usize = 0;
static mut RAM_END: usize = 0;
static mut RAM_START: usize = 0;

// O(1) refcount table: one u16 per 4KB page, indexed by (pa - ram_start) / PAGE_SIZE.
// Covers up to 1 GB of RAM (262144 pages × 4 KB). 512 KB BSS vs the prior 4 MB.
const MAX_PAGES: usize = 262144;
static PAGE_REF_COUNTS: Mutex<[u16; MAX_PAGES]> = Mutex::new([0u16; MAX_PAGES]);

#[inline]
fn page_index(pa: usize) -> usize {
    let base = unsafe { RAM_START };
    if pa < base {
        return MAX_PAGES; // sentinel: out of RAM range
    }
    let idx = (pa - base) / PAGE_SIZE;
    if idx < MAX_PAGES { idx } else { MAX_PAGES }
}

pub fn init(dtb_ptr: usize) {
    let fdt = unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8).unwrap() };
    
    // Find the memory node
    let memory = fdt.memory();
    let region = memory.regions().next().expect("No memory region found in DTB");
    
    let ram_start = region.starting_address as usize;
    let ram_size = region.size.unwrap_or(0);
    unsafe {
        RAM_START = ram_start;
        RAM_END = ram_start + ram_size;
    }
    
    // Find reserved regions (FDT and Initrd)
    let fdt_start = dtb_ptr;
    let fdt_end = fdt_start + fdt.total_size();
    
    let mut initrd_start = 0;
    let mut initrd_end = 0;
    if let Some(chosen) = fdt.find_node("/chosen") {
        if let (Some(start_prop), Some(end_prop)) = (chosen.property("linux,initrd-start"), chosen.property("linux,initrd-end")) {
            initrd_start = start_prop.as_usize().unwrap_or(0);
            initrd_end = end_prop.as_usize().unwrap_or(0);
        }
    }
    
    // Kernel ends at _stack_end. Align to next page.
    let kernel_end = unsafe { (&_stack_end as *const u8 as usize + PAGE_SIZE - 1) & !(PAGE_SIZE - 1) };
    
    // Initialize our free list
    unsafe {
        NEXT_FREE_PAGE = 0; // We will build the list backwards or just append carefully
        let mut prev_page = 0;
        
        let mut curr = kernel_end;
        while curr + PAGE_SIZE <= RAM_END {
            let next = curr + PAGE_SIZE;
            
            // Check if this page overlaps with FDT or Initrd
            let overlaps_fdt = curr < fdt_end && next > fdt_start;
            let overlaps_initrd = initrd_start > 0 && curr < initrd_end && next > initrd_start;
            
            if !overlaps_fdt && !overlaps_initrd {
                if NEXT_FREE_PAGE == 0 {
                    NEXT_FREE_PAGE = curr;
                } else {
                    (prev_page as *mut usize).write_volatile(curr);
                }
                prev_page = curr;
            }
            
            curr = next;
        }
        if prev_page != 0 {
            (prev_page as *mut usize).write_volatile(0);
        }
    }
    
    println!("PMM initialized.");
    println!("RAM Start: {:#x}", ram_start);
    println!("RAM Size: {} MB", ram_size / 1024 / 1024);
    println!("Kernel End: {:#x}", kernel_end);
    println!("Reserved FDT: {:#x} - {:#x}", fdt_start, fdt_end);
    if initrd_start > 0 {
        println!("Reserved Initrd: {:#x} - {:#x}", initrd_start, initrd_end);
    }
}

pub fn alloc_page() -> Option<usize> {
    unsafe {
        if NEXT_FREE_PAGE == 0 {
            return None;
        }
        let page = NEXT_FREE_PAGE;
        NEXT_FREE_PAGE = (page as *const usize).read_volatile();
        core::ptr::write_bytes(page as *mut u8, 0, PAGE_SIZE);

        let idx = page_index(page);
        if idx < MAX_PAGES {
            PAGE_REF_COUNTS.lock()[idx] = 1;
        }

        Some(page)
    }
}

pub fn free_page(page: usize) {
    assert!(page % PAGE_SIZE == 0);
    unsafe {
            (page as *mut usize).write_volatile(NEXT_FREE_PAGE);
        NEXT_FREE_PAGE = page;
    }
}

/// Increment reference count for the given physical page.
pub fn incr_ref(page: usize) {
    let idx = page_index(page);
    if idx < MAX_PAGES {
        let mut counts = PAGE_REF_COUNTS.lock();
        counts[idx] = counts[idx].saturating_add(1);
    }
}

/// Decrement reference count and return the new count. Returns 0 if untracked.
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

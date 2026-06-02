use crate::println;
use spin::Mutex;

pub const PAGE_SIZE: usize = 4096;

unsafe extern "C" {
    static _stack_end: u8; // The end of the kernel's stack / bss
}

static mut NEXT_FREE_PAGE: usize = 0;
static mut RAM_END: usize = 0;

// Fixed-size page refcount table to avoid using the global heap during early boot.
const MAX_PAGE_REFS: usize = 32768; // up to 128MB / 4KB

#[repr(C)]
struct PageRefTable {
    pas: [usize; MAX_PAGE_REFS],
    cnts: [usize; MAX_PAGE_REFS],
    len: usize,
}

static PAGE_REFS_TABLE: Mutex<PageRefTable> = Mutex::new(PageRefTable {
    pas: [0; MAX_PAGE_REFS],
    cnts: [0; MAX_PAGE_REFS],
    len: 0,
});

pub fn init(dtb_ptr: usize) {
    let fdt = unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8).unwrap() };
    
    // Find the memory node
    let memory = fdt.memory();
    let region = memory.regions().next().expect("No memory region found in DTB");
    
    let ram_start = region.starting_address as usize;
    let ram_size = region.size.unwrap_or(0);
    unsafe {
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
            None
        } else {
            let page = NEXT_FREE_PAGE;
            NEXT_FREE_PAGE = (page as *const usize).read_volatile();
            // Zero out the page
            core::ptr::write_bytes(page as *mut u8, 0, PAGE_SIZE);

                    // Initialize refcount to 1 in fixed table
            {
                let mut table = PAGE_REFS_TABLE.lock();
                // Try to find existing
                for i in 0..table.len {
                    if table.pas[i] == page {
                        table.cnts[i] = 1;
                        break;
                    }
                }
                if table.len < MAX_PAGE_REFS {
                    let idx = table.len;
                    table.pas[idx] = page;
                    table.cnts[idx] = 1;
                    table.len += 1;
                } else {
                    // Table full — best effort: ignore tracking
                }
            }

            Some(page)
        }
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
    let mut table = PAGE_REFS_TABLE.lock();
    for i in 0..table.len {
        if table.pas[i] == page {
            table.cnts[i] += 1;
            return;
        }
    }
    if table.len < MAX_PAGE_REFS {
        let idx = table.len;
        table.pas[idx] = page;
        table.cnts[idx] = 1;
        table.len += 1;
    } else {
        // Table full; ignore
    }
}

/// Decrement reference count and return the new count. If page wasn't tracked, returns 0.
pub fn decr_ref(page: usize) -> usize {
    let mut table = PAGE_REFS_TABLE.lock();
    for i in 0..table.len {
        if table.pas[i] == page {
            if table.cnts[i] > 1 {
                table.cnts[i] -= 1;
                return table.cnts[i];
            } else {
                // remove entry by swapping with last
                if table.len > 0 {
                    let last = table.len - 1;
                    table.pas[i] = table.pas[last];
                    table.cnts[i] = table.cnts[last];
                    table.pas[last] = 0;
                    table.cnts[last] = 0;
                    table.len -= 1;
                }
                return 0;
            }
        }
    }
    0
}

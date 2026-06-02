use riscv::register::satp;
use core::arch::asm;
use crate::println;
use crate::mm::pmm::alloc_page;

pub const PTE_V: usize = 1 << 0;
pub const PTE_R: usize = 1 << 1;
pub const PTE_W: usize = 1 << 2;
pub const PTE_X: usize = 1 << 3;
pub const PTE_U: usize = 1 << 4;

#[repr(C, align(4096))]
pub struct PageTable {
    pub entries: [usize; 512],
}

impl PageTable {
    pub const fn new() -> Self {
        PageTable { entries: [0; 512] }
    }

    pub fn new_process_table() -> Result<usize, &'static str> {
        crate::println!("new_process_table: allocating root page");
        let root_page = alloc_page().ok_or("Out of memory")?;
        crate::println!("new_process_table: allocated root page at {:#x}", root_page);
        let root = unsafe { &mut *(root_page as *mut PageTable) };
        
        crate::println!("new_process_table: mapping kernel 1GB pages");
        // Identity map kernel using 1GB mega-pages (0 to 4GB)
        for i in 0..4 {
            let pa = i * 0x40000000;
            let ppn = pa >> 12;
            // Kernel mappings are NOT PTE_U
            root.entries[i] = (ppn << 10) | PTE_V | PTE_R | PTE_W | PTE_X;
        }
        
        crate::println!("new_process_table: returning");
        Ok(root_page)
    }

    /// Maps a 4KB page at the given virtual address to the given physical address.
    pub fn map_page(&mut self, va: usize, pa: usize, flags: usize) -> Result<(), &'static str> {
        let vpn = [
            (va >> 12) & 0x1FF,
            (va >> 21) & 0x1FF,
            (va >> 30) & 0x1FF,
        ];
        
        let mut pt = self;
        for level in (1..=2).rev() {
            let entry = &mut pt.entries[vpn[level]];
            if *entry & PTE_V == 0 {
                let new_page = alloc_page().ok_or("Out of memory mapping page")?;
                let ppn = new_page >> 12;
                *entry = (ppn << 10) | PTE_V;
            }
            let next_pt_pa = (*entry >> 10) << 12;
            pt = unsafe { &mut *(next_pt_pa as *mut PageTable) };
        }
        
        let entry = &mut pt.entries[vpn[0]];
        if *entry & PTE_V != 0 {
            return Err("Page already mapped");
        }
        
        let ppn = pa >> 12;
        *entry = (ppn << 10) | PTE_V | flags;
        
        Ok(())
    }
}

/// Clone user portions of a parent's page table into a child page table, making pages copy-on-write.
/// parent_root_pa and child_root_pa are physical addresses of the root page tables.
pub fn clone_user_space(parent_root_pa: usize, child_root_pa: usize) -> Result<(), &'static str> {
    use crate::mm::pmm;

    unsafe fn clone_level(parent_pa: usize, child_pa: usize, level: usize) -> Result<(), &'static str> {
        let parent_pt = &mut *(parent_pa as *mut PageTable);
        let child_pt = &mut *(child_pa as *mut PageTable);

        for idx in 0..512 {
            let entry = parent_pt.entries[idx];
            if entry == 0 {
                child_pt.entries[idx] = 0;
                continue;
            }

            // If this is a pointer to next-level table (not a leaf)
            let is_leaf = (entry & (PTE_R | PTE_X)) != 0;
            if !is_leaf {
                // Allocate a new page for child's next level
                let new_page = crate::mm::pmm::alloc_page().ok_or("OOM cloning page table")?;
                let ppn = new_page >> 12;
                child_pt.entries[idx] = (ppn << 10) | PTE_V;

                let next_parent_pa = (entry >> 10) << 12;
                let next_child_pa = new_page;

                // Recurse into next level
                clone_level(next_parent_pa, next_child_pa, level - 1)?;
            } else {
                // Leaf mapping
                let pa = (entry >> 10) << 12;
                let mut flags = entry & 0x3FF; // low 10 bits

                if (flags & PTE_U) != 0 {
                    // User page: make read-only in both parent and child (COW)
                    if (flags & PTE_W) != 0 {
                        // Clear write in parent
                        let mut new_parent_entry = (pa >> 12) << 10 | (flags & !PTE_W);
                        parent_pt.entries[idx] = new_parent_entry;
                        flags &= !PTE_W;
                    }
                    // Increment refcount for shared page
                    pmm::incr_ref(pa);
                }

                child_pt.entries[idx] = (pa >> 12) << 10 | (flags & 0x3FF) | PTE_V;
            }
        }
        Ok(())
    }

    // Start recursion at level 2 (root has 3 levels: 2,1,0)
    unsafe { clone_level(parent_root_pa, child_root_pa, 2) }
}

/// Handle store page fault at given virtual address in the currently active page table.
/// Perform copy-on-write if the page is a user mapping that is currently read-only.
pub fn handle_store_page_fault(fault_va: usize) -> Result<(), &'static str> {
    use crate::mm::pmm;
    use riscv::register::satp;

    let satp_val = unsafe { satp::read().bits() } as usize;
    let root_pa = satp_val & 0xFFFFFFFFFFF;

    // Walk to find the leaf PTE
    let vpn = [
        (fault_va >> 12) & 0x1FF,
        (fault_va >> 21) & 0x1FF,
        (fault_va >> 30) & 0x1FF,
    ];

    let mut pt_pa = root_pa;
    for level in (1..=2).rev() {
        let pt = unsafe { &mut *(pt_pa as *mut PageTable) };
        let entry = pt.entries[vpn[level]];
        if entry & PTE_V == 0 {
            return Err("no mapping at level");
        }
        let is_leaf = (entry & (PTE_R | PTE_X)) != 0;
        if is_leaf {
            // Unexpected leaf above final level
            break;
        }
        pt_pa = ((entry >> 10) << 12) as usize;
    }

    let pt = unsafe { &mut *(pt_pa as *mut PageTable) };
    let entry = pt.entries[vpn[0]];
    if entry & PTE_V == 0 {
        return Err("no mapping for faulting VA");
    }

    let pa = (entry >> 10) << 12;
    let flags = entry & 0x3FF;

    // If page is user and not writable, perform COW
    if (flags & PTE_U) != 0 && (flags & PTE_W) == 0 {
        // Allocate a new page and copy contents
        let new_pa = pmm::alloc_page().ok_or("OOM in COW")?;
        unsafe {
            core::ptr::copy_nonoverlapping(pa as *const u8, new_pa as *mut u8, pmm::PAGE_SIZE);
        }

        // Update PTE to point to new page with write enabled
        let new_entry = ((new_pa >> 12) << 10) | (flags | PTE_W) | PTE_V;
        pt.entries[vpn[0]] = new_entry;

        // Decrement refcount of old page; free if zero
        let remaining = pmm::decr_ref(pa);
        if remaining == 0 {
            pmm::free_page(pa);
        }

        // Flush TLB
        unsafe { core::arch::asm!("sfence.vma") };

        Ok(())
    } else {
        Err("not a COW candidate")
    }
}

static mut ROOT_PAGE_TABLE: usize = 0;

unsafe fn destroy_level(pt_pa: usize, level: usize) {
    // pt_pa is physical address of a page table
    let pt = &mut *(pt_pa as *mut PageTable);
    for idx in 0..512 {
        let entry = pt.entries[idx];
        if entry == 0 { continue; }
        let is_leaf = (entry & (PTE_R | PTE_X)) != 0;
        if is_leaf {
            let pa = (entry >> 10) << 12;
            let flags = entry & 0x3FF;
            if (flags & PTE_U) != 0 {
                // user page: decrement ref and free if needed
                let remaining = crate::mm::pmm::decr_ref(pa);
                if remaining == 0 {
                    crate::mm::pmm::free_page(pa);
                }
            }
            pt.entries[idx] = 0;
        } else {
            // pointer to next-level page table
            let next_pa = (entry >> 10) << 12;
            if level > 0 {
                destroy_level(next_pa, level - 1);
            }
            // free the page table page itself
            crate::mm::pmm::free_page(next_pa);
            pt.entries[idx] = 0;
        }
    }
}

pub fn destroy_user_space(root_pa: usize) -> Result<(), &'static str> {
    // Walk levels and free user mappings and page-table pages (but keep kernel identity entries if present)
    unsafe {
        // root is level 2
        let root = &mut *(root_pa as *mut PageTable);
        // For entries that correspond to kernel identity mapping (first few), skip if they map kernel
        for i in 0..512 {
            let entry = root.entries[i];
            if entry == 0 { continue; }
            // If entry maps kernel (we detect by checking if PA < 0x100000000), skip
            let pa = (entry >> 10) << 12;
            if pa < 0x40000000 { // kernel region for identity mapping
                continue;
            }
            let is_leaf = (entry & (PTE_R | PTE_X)) != 0;
            if is_leaf {
                let flags = entry & 0x3FF;
                if (flags & PTE_U) != 0 {
                    let page_pa = pa;
                    let remaining = crate::mm::pmm::decr_ref(page_pa);
                    if remaining == 0 {
                        crate::mm::pmm::free_page(page_pa);
                    }
                }
                root.entries[i] = 0;
            } else {
                let next_pa = pa;
                destroy_level(next_pa, 1);
                // free the page table page
                crate::mm::pmm::free_page(next_pa);
                root.entries[i] = 0;
            }
        }
        Ok(())
    }
}

pub fn init() {
    let root_page = alloc_page().expect("Failed to allocate root page table");
    unsafe {
        ROOT_PAGE_TABLE = root_page;
    }
    
    let root = unsafe { &mut *(root_page as *mut PageTable) };
    
    // Identity map the first 4GB of physical address space using 1GB mega-pages
    // This covers our UART MMIO (0x1000_0000) and RAM (0x8000_0000)
    for i in 0..4 {
        let pa = i * 0x40000000;
        let ppn = pa >> 12;
        // Set Valid, Read, Write, Execute flags
        root.entries[i] = (ppn << 10) | PTE_V | PTE_R | PTE_W | PTE_X;
    }
    
    let ppn = root_page >> 12;
    // satp MODE: 8 means Sv39
    let satp_val = (8 << 60) | ppn; 
    
    unsafe {
        satp::write(satp_val);
        asm!("sfence.vma");
    }
    println!("VMM initialized. Sv39 paging enabled.");
}

use alloc::vec::Vec;
use crate::mm::pmm::{alloc_page, free_page};

/// A Virtual Memory Object (VMO) is a contiguous range of virtual memory 
/// backed by an array of physical pages.
pub struct Vmo {
    size_bytes: usize,
    physical_pages: Vec<usize>,
}

impl Vmo {
    pub fn new(size_bytes: usize) -> Option<Self> {
        let num_pages = (size_bytes + 4095) / 4096;
        let mut pages = Vec::with_capacity(num_pages);
        
        for _ in 0..num_pages {
            if let Some(page) = alloc_page() {
                pages.push(page);
            } else {
                // Out of memory, free what we allocated so far
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
    
    pub fn size(&self) -> usize {
        self.size_bytes
    }
    
    pub fn pages(&self) -> &[usize] {
        &self.physical_pages
    }
}

impl Drop for Vmo {
    fn drop(&mut self) {
        for &page in &self.physical_pages {
            free_page(page);
        }
    }
}

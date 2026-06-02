#![allow(dead_code)]
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};

pub struct BumpAllocator;

static OFFSET: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align();
        let size = layout.size();
        let heap_start = HEAP_SPACE.as_ptr() as usize;
        let heap_size = HEAP_SPACE.len();

        loop {
            let cur_off = OFFSET.load(Ordering::SeqCst);
            let cur_ptr = heap_start + cur_off;
            let aligned = (cur_ptr + align - 1) & !(align - 1);
            let next_ptr = match aligned.checked_add(size) { Some(n) => n, None => return null_mut() };
            let new_off = next_ptr - heap_start;
            if new_off > heap_size { return null_mut(); }
            if OFFSET.compare_exchange(cur_off, new_off, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                return aligned as *mut u8;
            }
            // otherwise retry
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // noop
    }
}

// Global allocator now provided by libos.
static A: BumpAllocator = BumpAllocator;

// Provide a small heap region (placed here so the binary contains it). Size tuned conservatively.
#[link_section = ".uninit" ]
static mut HEAP_SPACE: [u8; 128 * 1024] = [0u8; 128 * 1024];

pub unsafe fn init_allocator() {
    OFFSET.store(0, Ordering::SeqCst);
}

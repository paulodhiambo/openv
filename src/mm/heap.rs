use buddy_system_allocator::LockedHeap;
use crate::println;

#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap<32> = LockedHeap::empty();

// Placed in BSS (zero-init), zeroed by boot.s before kmain runs.
static mut HEAP_SPACE: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
const HEAP_SIZE: usize = 32 * 1024 * 1024;

pub fn init() {
    unsafe {
        let start = core::ptr::addr_of_mut!(HEAP_SPACE) as usize;
        HEAP_ALLOCATOR.lock().init(start, HEAP_SIZE);
    }
    println!("Heap initialized with {} MB", HEAP_SIZE / 1024 / 1024);
}

pub mod heap;
pub mod pmm;
pub mod swap;
pub mod vmm;
pub mod vmo;

pub fn init(dtb_ptr: usize) {
    pmm::init(dtb_ptr);
    vmm::init();
    heap::init();
    test_memory_manager();
}

fn test_memory_manager() {
    crate::println!("--- Testing PMM RAII ---");
    let pa = {
        let frame = pmm::alloc_frame().expect("OOM in PMM test");
        frame.pa()
        // frame drops here, returning the page to the free list
    };

    // Allocate again. Because the free list pushes to the head (LIFO),
    // we should get the exact same physical address back.
    let frame2 = pmm::alloc_frame().expect("OOM in PMM test");
    if pa != frame2.pa() {
        panic!("PMM RAII leak detected! Expected {:#x}, got {:#x}", pa, frame2.pa());
    }
    
    // Test manual reference counting
    pmm::incr_ref(frame2.pa());
    let remaining = pmm::decr_ref(frame2.pa());
    if remaining != 1 {
        panic!("PMM refcount logic failed!");
    }
    
    crate::println!("PMM RAII and Refcount verified.");
}

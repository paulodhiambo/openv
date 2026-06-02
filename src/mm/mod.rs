pub mod heap;
pub mod pmm;
pub mod vmm;
pub mod vmo;

const HEAP_SIZE: usize = 16 * 1024 * 1024;

pub fn init(dtb_ptr: usize) {
    pmm::init(dtb_ptr);
    vmm::init();
    heap::init();
}

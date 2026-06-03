pub mod heap;
pub mod pmm;
pub mod vmm;
pub mod vmo;

pub fn init(dtb_ptr: usize) {
    pmm::init(dtb_ptr);
    vmm::init();
    heap::init();
}

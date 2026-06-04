use crate::net::virtqueue::VirtQueue;
use alloc::boxed::Box;
use crate::sync::Mutex;

const OFF_MAGIC: usize = 0x000;
const OFF_DEVICE_ID: usize = 0x008;
const OFF_DEVICE_FEATURES: usize = 0x010;
const OFF_DRIVER_FEATURES: usize = 0x020;
const OFF_QUEUE_SEL: usize = 0x030;
const OFF_QUEUE_NUM_MAX: usize = 0x034;
const OFF_QUEUE_NUM: usize = 0x038;
const OFF_GUEST_PAGE_SIZE: usize = 0x028;
const OFF_QUEUE_ALIGN: usize = 0x03c;
const OFF_QUEUE_PFN: usize = 0x040;
const OFF_QUEUE_NOTIFY: usize = 0x050;
const OFF_STATUS: usize = 0x070;

const VIRTIO_MAGIC: u32 = 0x7472_6976;
pub const VIRTIO_BLK_DEVICE_ID: u32 = 2;

const VIRTIO_BLK_T_IN: u32 = 0;  // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write
const VIRTIO_BLK_S_OK: u8 = 0;

pub const BLOCK_SIZE: usize = 4096;
pub const SECTOR_SIZE: usize = 512;
const SECTORS_PER_BLOCK: u64 = (BLOCK_SIZE / SECTOR_SIZE) as u64;

#[repr(C)]
struct BlkReqHeader {
    blk_type: u32,
    reserved: u32,
    sector: u64,
}

pub struct VirtioBlk {
    base: usize,
    vq: Mutex<VirtQueue>,
}

static BLK_DRIVER: Mutex<Option<&'static VirtioBlk>> = Mutex::new(None);

impl VirtioBlk {
    fn mmio_r32(base: usize, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((base + off) as *const u32) }
    }
    fn mmio_w32(base: usize, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile((base + off) as *mut u32, v) }
    }

    fn do_rw(&self, sector: u64, buf_ptr: *mut u8, buf_len: usize, is_read: bool) -> bool {
        let hdr_pa = match crate::mm::pmm::alloc_page() {
            Some(p) => p,
            None => return false,
        };
        let stat_pa = match crate::mm::pmm::alloc_page() {
            Some(p) => p,
            None => {
                crate::mm::pmm::free_page(hdr_pa);
                return false;
            }
        };

        unsafe {
            let hdr = hdr_pa as *mut BlkReqHeader;
            (*hdr).blk_type = if is_read { VIRTIO_BLK_T_IN } else { VIRTIO_BLK_T_OUT };
            (*hdr).reserved = 0;
            (*hdr).sector = sector;
            *(stat_pa as *mut u8) = 0xFF;
        }

        let chain: &[(u64, u32, bool)] = &[
            (hdr_pa as u64, core::mem::size_of::<BlkReqHeader>() as u32, false),
            (buf_ptr as u64, buf_len as u32, is_read),
            (stat_pa as u64, 1, true),
        ];

        let head_id = {
            let vq = self.vq.lock();
            match vq.enqueue_chain(chain) {
                Ok(id) => id,
                Err(_) => {
                    crate::mm::pmm::free_page(hdr_pa);
                    crate::mm::pmm::free_page(stat_pa);
                    return false;
                }
            }
        };

        Self::mmio_w32(self.base, OFF_QUEUE_NOTIFY, 0);

        let mut found = false;
        for _ in 0..2_000_000 {
            let mut vq = self.vq.lock();
            for (id, _) in vq.pop_used() {
                vq.free_chain(id);
                if id == head_id {
                    found = true;
                }
            }
            if found {
                break;
            }
        }

        let ok_status = unsafe { *(stat_pa as *const u8) };
        crate::mm::pmm::free_page(hdr_pa);
        crate::mm::pmm::free_page(stat_pa);
        found && ok_status == VIRTIO_BLK_S_OK
    }

    pub fn read_block(&self, blk: u32, buf: &mut [u8; BLOCK_SIZE]) -> bool {
        self.do_rw(blk as u64 * SECTORS_PER_BLOCK, buf.as_mut_ptr(), BLOCK_SIZE, true)
    }

    pub fn write_block(&self, blk: u32, buf: &[u8; BLOCK_SIZE]) -> bool {
        self.do_rw(
            blk as u64 * SECTORS_PER_BLOCK,
            buf.as_ptr() as *mut u8,
            BLOCK_SIZE,
            false,
        )
    }
}

pub fn probe_driver(
    base: usize,
    irq: usize,
) -> Option<alloc::boxed::Box<dyn crate::drivers::Driver>> {
    if base == 0 {
        return None;
    }
    let magic = VirtioBlk::mmio_r32(base, OFF_MAGIC);
    if magic != VIRTIO_MAGIC {
        return None;
    }
    let device_id = VirtioBlk::mmio_r32(base, OFF_DEVICE_ID);
    if device_id != VIRTIO_BLK_DEVICE_ID {
        return None;
    }

    crate::println!("virtio-blk: found block device at {:#x}", base);

    VirtioBlk::mmio_w32(base, OFF_STATUS, 0); // reset
    VirtioBlk::mmio_w32(base, OFF_STATUS, 1); // ACKNOWLEDGE
    VirtioBlk::mmio_w32(base, OFF_STATUS, 3); // ACKNOWLEDGE | DRIVER
    let _features = VirtioBlk::mmio_r32(base, OFF_DEVICE_FEATURES);
    VirtioBlk::mmio_w32(base, OFF_DRIVER_FEATURES, 0);

    VirtioBlk::mmio_w32(base, OFF_QUEUE_SEL, 0);
    let qnum = VirtioBlk::mmio_r32(base, OFF_QUEUE_NUM_MAX);
    let qsize = if qnum == 0 { 16 } else { core::cmp::min(qnum, 16) } as usize;
    VirtioBlk::mmio_w32(base, OFF_QUEUE_NUM, qsize as u32);
    // Use align=4: device places used ring at 4-byte-aligned offset within the
    // single queue page, matching VirtQueue::new layout. align=4096 would push
    // the used ring to a second page (beyond our single allocation).
    VirtioBlk::mmio_w32(base, OFF_QUEUE_ALIGN, 4);

    let q_pa = crate::mm::pmm::alloc_page()?;
    unsafe { core::ptr::write_bytes(q_pa as *mut u8, 0, 4096) };
    // GuestPageSize must be set before QueuePFN: device computes PA = PFN × GuestPageSize.
    VirtioBlk::mmio_w32(base, OFF_GUEST_PAGE_SIZE, 4096);
    VirtioBlk::mmio_w32(base, OFF_QUEUE_PFN, (q_pa >> 12) as u32);
    VirtioBlk::mmio_w32(base, OFF_STATUS, 7); // ACK | DRIVER | DRIVER_OK

    let drv = VirtioBlk {
        base,
        vq: Mutex::new(VirtQueue::new(q_pa, qsize)),
    };
    let leaked: &'static VirtioBlk = Box::leak(Box::new(drv));
    *BLK_DRIVER.lock() = Some(leaked);

    crate::println!("virtio-blk: initialized, qsize={}, irq={}", qsize, irq);
    Some(Box::new(BlkDriverRef(leaked)))
}

pub fn get() -> Option<&'static VirtioBlk> {
    *BLK_DRIVER.lock()
}

#[allow(dead_code)]
struct BlkDriverRef(&'static VirtioBlk);
unsafe impl Send for BlkDriverRef {}
unsafe impl Sync for BlkDriverRef {}

impl crate::drivers::Driver for BlkDriverRef {
    fn on_interrupt(&self, _irq: usize) {}
}

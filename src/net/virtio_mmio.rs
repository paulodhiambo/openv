//! VirtIO MMIO network driver (legacy, device-id=1).
//!
//! Queue layout (QUEUE_ALIGN=4 so everything fits in one 4 KB page per queue):
//!   queue 0 = RX (device writes received frames here)
//!   queue 1 = TX (driver writes outgoing frames here)
//!
//! Each RX descriptor is a single 4 KB page (10-byte virtio-net header + ≤1514 B packet).
//! Each TX submission is a single 4 KB page (10-byte virtio-net header + packet data).

use crate::net::NetDevice;
use crate::net::virtqueue::VirtQueue;
use crate::println;
use alloc::boxed::Box;
use alloc::vec::Vec;
use crate::sync::Mutex;

static DRIVER_GLOBAL: Mutex<Option<&'static VirtioDriver>> = Mutex::new(None);

// ── MMIO register offsets (legacy spec) ──────────────────────────────────────

const OFF_MAGIC: usize          = 0x000;
const OFF_VERSION: usize        = 0x004;
const OFF_DEVICE_ID: usize      = 0x008;
const OFF_DEVICE_FEATURES: usize = 0x010;
const OFF_DRIVER_FEATURES: usize = 0x020;
const OFF_GUEST_PAGE_SIZE: usize = 0x028; // legacy: must be written before QUEUE_PFN
const OFF_QUEUE_SEL: usize      = 0x030;
const OFF_QUEUE_NUM_MAX: usize  = 0x034;
const OFF_QUEUE_NUM: usize      = 0x038;
const OFF_QUEUE_ALIGN: usize    = 0x03c;
const OFF_QUEUE_PFN: usize      = 0x040;
const OFF_QUEUE_NOTIFY: usize   = 0x050;
const OFF_ISR: usize            = 0x060;
const OFF_STATUS: usize         = 0x070;

const VIRTIO_MAGIC: u32               = 0x7472_6976; // "virt"
const VIRTIO_DEVICE_NET: u32          = 1;
const VIRTIO_STATUS_ACKNOWLEDGE: u32  = 1;
const VIRTIO_STATUS_DRIVER: u32       = 2;
const VIRTIO_STATUS_DRIVER_OK: u32    = 4;

// virtio-net header (legacy, no VIRTIO_NET_F_MRG_RXBUF)
const VNET_HDR_LEN: usize = 10;

// RX queue: pre-allocated write-descriptors; 8 is sufficient for bursts
const RX_BUF_COUNT: usize = 8;
// Descriptor ring size per queue (power-of-two, fits with QUEUE_ALIGN=4 in one page)
const QUEUE_SIZE: usize = 64;

// ── Driver struct ──────────────────────────────────────────────────────────────

struct VirtioDriver {
    base:   usize,
    rx_vq:  Mutex<VirtQueue>,
    tx_vq:  Mutex<VirtQueue>,
    irq:    u32,
}

unsafe impl Send for VirtioDriver {}
unsafe impl Sync for VirtioDriver {}

// ── MMIO helpers ──────────────────────────────────────────────────────────────

fn mmio_read32(base: usize, offset: usize) -> u32 {
    // SAFETY: `base` is a MMIO physical address obtained from the DTB and
    // identity-mapped by the kernel. read_volatile prevents the compiler
    // from eliding or reordering the hardware register read.
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

fn mmio_write32(base: usize, offset: usize, v: u32) {
    // SAFETY: `base` is a MMIO physical address obtained from the DTB and
    // identity-mapped by the kernel. write_volatile prevents the compiler
    // from eliding or reordering the hardware register write.
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, v) }
}

// ── Initialization ────────────────────────────────────────────────────────────

impl VirtioDriver {
    fn new(base: usize, rx_pa: usize, tx_pa: usize, qsize: usize, irq: u32) -> Self {
        VirtioDriver {
            base,
            rx_vq: Mutex::new(VirtQueue::new(rx_pa, qsize)),
            tx_vq: Mutex::new(VirtQueue::new(tx_pa, qsize)),
            irq,
        }
    }

    fn setup_queue_mmio(&self, sel: u32, pa: usize, size: usize) {
        mmio_write32(self.base, OFF_QUEUE_SEL, sel);
        // Ensure device supports the requested size
        let max = mmio_read32(self.base, OFF_QUEUE_NUM_MAX);
        let actual = if max == 0 { size } else { core::cmp::min(max as usize, size) } as u32;
        mmio_write32(self.base, OFF_QUEUE_NUM, actual);
        // QUEUE_ALIGN=4 matches VirtQueue's internal layout (used ring aligned to 4 bytes)
        mmio_write32(self.base, OFF_QUEUE_ALIGN, 4);
        mmio_write32(self.base, OFF_QUEUE_PFN, (pa >> 12) as u32);
    }

    fn init_device(&self, rx_pa: usize, tx_pa: usize, qsize: usize) {
        // Reset
        mmio_write32(self.base, OFF_STATUS, 0);
        // ACKNOWLEDGE + DRIVER status bits
        mmio_write32(self.base, OFF_STATUS, VIRTIO_STATUS_ACKNOWLEDGE);
        mmio_write32(self.base, OFF_STATUS, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);
        // Negotiate zero optional features
        let _ = mmio_read32(self.base, OFF_DEVICE_FEATURES);
        mmio_write32(self.base, OFF_DRIVER_FEATURES, 0);
        // Must set GuestPageSize before writing QueuePFN in legacy mode
        mmio_write32(self.base, OFF_GUEST_PAGE_SIZE, 4096);
        // Queue 0 = RX, queue 1 = TX
        self.setup_queue_mmio(0, rx_pa, qsize);
        self.setup_queue_mmio(1, tx_pa, qsize);
        // Signal DRIVER_OK to make the device live
        mmio_write32(self.base, OFF_STATUS,
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK);
        // Pre-fill the RX queue so the device has buffers to write into
        for _ in 0..RX_BUF_COUNT {
            if let Some(pa) = crate::mm::pmm::alloc_page() {
                let _ = self.rx_vq.lock().enqueue_chain(&[(pa as u64, 4096, true)]);
            }
        }
        // Tell the device new RX buffers are available
        mmio_write32(self.base, OFF_QUEUE_NOTIFY, 0);
    }
}

// ── NetDevice impl ────────────────────────────────────────────────────────────

impl NetDevice for VirtioDriver {
    fn send(&self, packet: &[u8]) {
        // Free any TX buffers completed by the device since last call
        let done_pages: Vec<usize> = {
            let mut tx = self.tx_vq.lock();
            let done = tx.pop_used();
            let mut pages = Vec::new();
            for (id, _) in done {
                let parts = tx.read_chain(id);
                tx.free_chain(id);
                for (pa, _) in parts {
                    pages.push(pa as usize);
                }
            }
            pages
        };
        for pa in done_pages {
            crate::mm::pmm::free_page(pa);
        }

        // Allocate one page: virtio-net header (10 B) + packet data
        let total = VNET_HDR_LEN + packet.len();
        if total > crate::mm::pmm::PAGE_SIZE {
            return; // oversized
        }
        let pa = match crate::mm::pmm::alloc_page() {
            Some(p) => p,
            None => return,
        };
        // SAFETY: `pa` is a freshly allocated PMM page exclusively owned
        // by this TX submission. `ptr` is valid for VNET_HDR_LEN + packet.len()
        // bytes, both of which fit within one 4 KiB page (checked above).
        unsafe {
            let ptr = pa as *mut u8;
            // Zero-filled virtio-net header (no checksum offload, no GSO)
            core::ptr::write_bytes(ptr, 0, VNET_HDR_LEN);
            // Packet data follows immediately
            core::ptr::copy_nonoverlapping(packet.as_ptr(), ptr.add(VNET_HDR_LEN), packet.len());
        }
        let ok = self.tx_vq.lock()
            .enqueue_chain(&[(pa as u64, total as u32, false)])
            .is_ok();
        if ok {
            mmio_write32(self.base, OFF_QUEUE_NOTIFY, 1); // notify TX queue (index 1)
        } else {
            crate::mm::pmm::free_page(pa);
        }
    }

    fn recv(&self, buf: &mut [u8]) -> usize {
        // Poll RX used ring for a completed receive
        let (rx_pa, rx_len) = {
            let mut rx = self.rx_vq.lock();
            let done = rx.pop_used();
            if done.is_empty() {
                return 0;
            }
            let (id, pkt_len) = done[0];
            // Recover physical address from the descriptor before freeing it
            let pa = rx.desc_addr(id) as usize;
            // Return descriptor to free list and immediately re-enqueue for next receive
            rx.free_chain(id);
            let _ = rx.enqueue_chain(&[(pa as u64, 4096, true)]);
            (pa, pkt_len as usize)
        };
        // Inform device that a fresh RX buffer is available
        mmio_write32(self.base, OFF_QUEUE_NOTIFY, 0);

        // Copy Ethernet frame, skipping the 10-byte virtio-net header
        let data_len = rx_len.saturating_sub(VNET_HDR_LEN);
        let to_copy = core::cmp::min(data_len, buf.len());
        if to_copy > 0 {
            // SAFETY: `rx_pa` is a PMM-allocated page that the device DMA'd
            // into. After pop_used() the device no longer owns this buffer;
            // we re-enqueue a fresh descriptor above, so this read does not
            // race with the device. `buf` is a caller-provided mutable slice
            // of at least `to_copy` bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (rx_pa + VNET_HDR_LEN) as *const u8,
                    buf.as_mut_ptr(),
                    to_copy,
                );
            }
        }
        to_copy
    }
}

// ── Driver framework integration ──────────────────────────────────────────────

impl crate::drivers::Driver for VirtioDriver {
    fn on_interrupt(&self, irq: usize) {
        handle_interrupt(irq as u32);
    }
}

pub fn handle_interrupt(irq: u32) {
    let guard = DRIVER_GLOBAL.lock();
    if let Some(drv) = *guard {
        if drv.irq != 0 && drv.irq != irq {
            return;
        }
        // Acknowledge ISR to clear device-side interrupt status
        let _ = mmio_read32(drv.base, OFF_ISR);
        // Actual packet processing happens in the next recv() / send() call.
    }
}

// ── Public probe entry-points ─────────────────────────────────────────────────

/// Called from `net::init()` via direct FDT scan (used before driver framework is wired).
pub fn probe_and_init() -> bool {
    let dtb_ptr = boot_dtb_ptr();
    if dtb_ptr == 0 {
        return false;
    }
    // SAFETY: `dtb_ptr` is the physical address of the DTB passed by OpenSBI
    // in register a1. It is valid for the entire kernel lifetime and correctly
    // aligned. The FDT crate only reads from it.
    let fdt = match unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8) } {
        Ok(f) => f,
        Err(_) => return false,
    };

    for node in fdt.all_nodes() {
        let Some(comp) = node.property("compatible") else { continue };
        let Some(s) = comp.as_str() else { continue };
        if !s.contains("virtio,mmio") {
            continue;
        }

        // Parse base address
        let mut base = node.property("reg").and_then(|p| p.as_usize()).unwrap_or(0);
        if base == 0 {
            if let Some(idx) = node.name.rfind('@') {
                if let Ok(x) = usize::from_str_radix(&node.name[idx + 1..], 16) {
                    base = x;
                }
            }
        }
        if base == 0 {
            continue;
        }

        // Validate magic and device type
        if mmio_read32(base, OFF_MAGIC) != VIRTIO_MAGIC {
            continue;
        }
        if mmio_read32(base, OFF_DEVICE_ID) != VIRTIO_DEVICE_NET {
            continue;
        }

        let irq = node.property("interrupts")
            .and_then(|p| p.as_usize())
            .unwrap_or(0) as u32;

        if let Some(drv) = init_net_device(base, irq) {
            println!("virtio-net: initialized at {:#x}, irq={}", base, irq);
            *DRIVER_GLOBAL.lock() = Some(drv);
            crate::net::register_device(drv);
            return true;
        }
    }
    false
}

/// Called by the driver framework when it finds a virtio,mmio node in the FDT.
pub fn probe_driver(base: usize, irq: usize) -> Option<Box<dyn crate::drivers::Driver>> {
    if base == 0 { return None; }
    if mmio_read32(base, OFF_MAGIC) != VIRTIO_MAGIC { return None; }
    if mmio_read32(base, OFF_DEVICE_ID) != VIRTIO_DEVICE_NET { return None; }

    let drv = init_net_device(base, irq as u32)?;
    println!("virtio-net: initialized (driver framework) at {:#x}, irq={}", base, irq);
    *DRIVER_GLOBAL.lock() = Some(drv);
    crate::net::register_device(drv);
    Some(Box::new(VirtioDriverRef(drv)))
}

// ── Internal helper ───────────────────────────────────────────────────────────

fn init_net_device(base: usize, irq: u32) -> Option<&'static VirtioDriver> {
    let qsize = {
        // Read supported queue size; use the smaller of QUEUE_SIZE and device max
        mmio_write32(base, OFF_QUEUE_SEL, 0);
        let max0 = mmio_read32(base, OFF_QUEUE_NUM_MAX);
        if max0 == 0 { QUEUE_SIZE } else { core::cmp::min(max0 as usize, QUEUE_SIZE) }
    };

    let rx_pa = crate::mm::pmm::alloc_page()?;
    let tx_pa = crate::mm::pmm::alloc_page()?;

    // SAFETY: `rx_pa` and `tx_pa` are freshly allocated PMM pages exclusively
    // owned here. PAGE_SIZE bytes are within each allocation.
    unsafe {
        core::ptr::write_bytes(rx_pa as *mut u8, 0, crate::mm::pmm::PAGE_SIZE);
        core::ptr::write_bytes(tx_pa as *mut u8, 0, crate::mm::pmm::PAGE_SIZE);
    }

    let drv = VirtioDriver::new(base, rx_pa, tx_pa, qsize, irq);
    drv.init_device(rx_pa, tx_pa, qsize);

    Some(Box::leak(Box::new(drv)))
}

struct VirtioDriverRef(&'static VirtioDriver);
unsafe impl Send for VirtioDriverRef {}
unsafe impl Sync for VirtioDriverRef {}
impl crate::drivers::Driver for VirtioDriverRef {
    fn on_interrupt(&self, irq: usize) { self.0.on_interrupt(irq); }
}

fn boot_dtb_ptr() -> usize {
    crate::boot_dtb_ptr()
}

/// Kernel API: dequeue one raw received frame (called by sys_net_recv).
pub fn try_dequeue_rx(buf: &mut [u8]) -> Option<usize> {
    let guard = DRIVER_GLOBAL.lock();
    let drv = (*guard)?;
    let n = drv.recv(buf);
    if n > 0 { Some(n) } else { None }
}

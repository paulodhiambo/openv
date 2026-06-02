//! Virtio-mmio helper: register layout, feature negotiation, and basic queue setup.
//!
//! This file implements a conservative legacy-style virtio-mmio setup that:
//!  - probes the FDT for a "virtio,mmio" node
//!  - reads basic MMIO registers
//!  - negotiates zero features (safe)
//!  - allocates a page for a virtqueue and writes the PFN
//!  - registers a simple runtime NetDevice that will later be wired to the queue

use crate::println;
use crate::net::NetDevice;
use alloc::boxed::Box;
use alloc::vec::Vec;
use crate::net::virtqueue::VirtQueue;

use spin::Mutex;
static DRIVER_GLOBAL: Mutex<Option<&'static VirtioDriver>> = Mutex::new(None);
// Store tuples of (pa, len)
static RX_QUEUE: Mutex<Vec<(usize, u32)>> = Mutex::new(Vec::new());

// Important legacy MMIO offsets (conservative subset)
const OFF_MAGIC: usize = 0x000;
const OFF_VERSION: usize = 0x004;
const OFF_DEVICE_ID: usize = 0x008;
const OFF_VENDOR_ID: usize = 0x00c;
const OFF_DEVICE_FEATURES: usize = 0x010;
const OFF_DRIVER_FEATURES: usize = 0x020;
const OFF_QUEUE_SEL: usize = 0x030;
const OFF_QUEUE_NUM_MAX: usize = 0x034;
const OFF_QUEUE_NUM: usize = 0x038;
const OFF_QUEUE_ALIGN: usize = 0x03c;
const OFF_QUEUE_PFN: usize = 0x040;
const OFF_QUEUE_NOTIFY: usize = 0x050;
const OFF_ISR: usize = 0x060; // legacy ISR
const OFF_STATUS: usize = 0x070;

const VIRTIO_MAGIC: u32 = 0x7472_6976; // "virt"

fn mmio_read32(base: usize, offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

fn mmio_write32(base: usize, offset: usize, v: u32) {
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, v) }
}

fn mmio_write64(base: usize, offset: usize, v: u64) {
    mmio_write32(base, offset, v as u32);
    mmio_write32(base, offset + 4, (v >> 32) as u32);
}

struct VirtioDriver {
    base: usize,
    q_pa: usize,
    vq: Mutex<VirtQueue>,
    irq: u32,
}

impl VirtioDriver {
    fn new(base: usize, q_pa: usize, qsize: usize, irq: u32) -> Self {
        let vq = VirtQueue::new(q_pa, qsize);
        VirtioDriver { base, q_pa, vq: Mutex::new(vq), irq }
    }

    fn reset(&self) {
        mmio_write32(self.base, OFF_STATUS, 0);
    }

    fn negotiate_features(&self) {
        let _features = mmio_read32(self.base, OFF_DEVICE_FEATURES);
        mmio_write32(self.base, OFF_DRIVER_FEATURES, 0);
    }

    fn setup_queue0(&self) {
        mmio_write32(self.base, OFF_QUEUE_SEL, 0);
        // Use vq size if available
        let qsize = self.vq.lock().size as u32;
        mmio_write32(self.base, OFF_QUEUE_NUM, qsize);
        mmio_write32(self.base, OFF_QUEUE_ALIGN, 4096);
        let pfn = (self.q_pa >> 12) as u32;
        mmio_write32(self.base, OFF_QUEUE_PFN, pfn);
    }
}

impl NetDevice for VirtioDriver {
    fn send(&self, packet: &[u8]) {
        // Split packet into PAGE-sized chunks and create descriptor chain
        let mut chunks: Vec<(u64, u32, bool)> = Vec::new();
        let mut offset = 0usize;
        while offset < packet.len() {
            if let Some(pa) = crate::mm::pmm::alloc_page() {
                let copy_len = core::cmp::min(packet.len() - offset, crate::mm::pmm::PAGE_SIZE);
                unsafe {
                    let dst = pa as *mut u8;
                    core::ptr::copy_nonoverlapping(packet[offset..].as_ptr(), dst, copy_len);
                }
                chunks.push((pa as u64, copy_len as u32, false));
                offset += copy_len;
            } else {
                // allocation failure: free already allocated pages
                for (pa, _len, _w) in chunks.iter() {
                    crate::mm::pmm::free_page(*pa as usize);
                }
                return;
            }
        }

        if chunks.is_empty() {
            return;
        }

        let ok = {
            let vq = self.vq.lock();
            vq.enqueue_chain(&chunks).is_ok()
        };

        if ok {
            // Notify device that queue 0 has new buffer(s)
            mmio_write32(self.base, OFF_QUEUE_NOTIFY, 0);
        } else {
            // Queue full: free pages
            for (pa, _len, _w) in chunks.iter() {
                crate::mm::pmm::free_page(*pa as usize);
            }
        }
    }

    fn recv(&self, buf: &mut [u8]) -> usize {
        // First, try the interrupt-populated RX queue
        if let Some(n) = try_dequeue_rx(buf) {
            return n;
        }

        // Fallback: poll the virtqueue directly
        let mut vq = self.vq.lock();
        let completed = vq.pop_used();
        if completed.is_empty() {
            return 0;
        }
        let (id, _len) = completed[0];
        // Read descriptor chain for this id
        let parts = vq.read_chain(id);
        let mut copied = 0usize;
        for (pa, len) in parts.iter() {
            let to_copy = core::cmp::min(*len as usize, buf.len() - copied);
            unsafe { core::ptr::copy_nonoverlapping(*pa as *const u8, buf[copied..].as_mut_ptr(), to_copy); }
            copied += to_copy;
            // Free page
            crate::mm::pmm::free_page(*pa as usize);
            if copied >= buf.len() { break; }
        }
        // Return descriptors to free list
        vq.free_chain(id);
        copied
    }
}

/// Probe DTB for virtio,mmio and initialize a minimal stub device.
pub fn probe_and_init() -> bool {
    let dtb_ptr = boot_dtb_ptr();
    if dtb_ptr == 0 {
        return false;
    }

    let fdt = match unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8) } {
        Ok(f) => f,
        Err(_) => return false,
    };

    for node in fdt.all_nodes() {
        if let Some(comp) = node.property("compatible") {
            if let Some(s) = comp.as_str() {
                if s.contains("virtio,mmio") {
                    // try to read reg property
                    let mut base = node.property("reg").and_then(|p| p.as_usize()).unwrap_or(0);
                    // Fallback: parse base from node name like 'virtio_mmio@10008000'
                    if base == 0 {
                        if let Some(idx) = node.name.rfind('@') {
                            if let Ok(x) = usize::from_str_radix(&node.name[idx+1..], 16) {
                                base = x;
                            }
                        }
                    }
                    println!("virtio-mmio: node '{}' compat contains virtio,mmio; base={:#x}", node.name, base);

                    if base != 0 {
                        // Read magic to verify MMIO presence (best-effort)
                        let magic = mmio_read32(base, OFF_MAGIC);
                        if magic == VIRTIO_MAGIC {
                            println!("virtio-mmio: magic OK at {:#x}", base);
                        } else {
                            println!("virtio-mmio: magic mismatch (read {:#x})", magic);
                        }

                        // Allocate a page for virtqueue metadata (guest physical page)
                        let q_pa = crate::mm::pmm::alloc_page().expect("failed to alloc vq page");
                        let qnum = mmio_read32(base, OFF_QUEUE_NUM_MAX);
                        let qsize = if qnum == 0 { 256 } else { core::cmp::min(qnum, 256) };
                        println!("virtio-mmio: allocated virtqueue page at PA {:#x}, qnum_max={}, using qsize={}", q_pa, qnum, qsize);

                        // Try to read an interrupt spec from DTB (best-effort). If unavailable, irq=0
                        let irq = node.property("interrupts").and_then(|p| p.as_usize()).unwrap_or(0) as u32;

                        let drv = VirtioDriver::new(base, q_pa, qsize as usize, irq);
                        drv.reset();
                        drv.negotiate_features();
                        drv.setup_queue0();

                        unsafe {
                            // Leak to 'static by leaking a Box; this is acceptable for kernel lifetime
                            let leaked: &'static VirtioDriver = Box::leak(Box::new(drv));
                            *DRIVER_GLOBAL.lock() = Some(leaked);
                            crate::net::register_device(leaked);
                        }

                        println!("virtio-mmio: driver registered (stub), irq={}", irq);
                        return true;
                    }
                }
            }
        }
    }

    false
}

fn boot_dtb_ptr() -> usize {
    crate::boot_dtb_ptr()
}

/// Interrupt handler invoked by trap handler when virtio IRQ is received.
pub fn handle_interrupt(irq: u32) {
    let guard = DRIVER_GLOBAL.lock();
    if let Some(drv) = *guard {
        // If driver knows its IRQ, only handle matching IRQs. If drv.irq == 0,
        // fall back to legacy ISR read.
        if drv.irq != 0 && drv.irq != irq {
            return;
        }

        // If legacy ISR is present, read it to clear device-side status (best-effort)
        let _ = mmio_read32(drv.base, OFF_ISR);

        let mut vq = drv.vq.lock();
        let completed = vq.pop_used();
        if !completed.is_empty() {
            let mut rx = RX_QUEUE.lock();
            for (id, _len) in completed {
                // read descriptor chain and enqueue physical pages into RX queue
                let parts = vq.read_chain(id);
                for (pa, len) in parts.iter() {
                    rx.push((*pa as usize, *len));
                }
                // free descriptor indices
                vq.free_chain(id);
            }
        }
    }
}

/// API used by kernel sys_net_recv to retrieve a pending received buffer if any.
pub fn try_dequeue_rx(buf: &mut [u8]) -> Option<usize> {
    let mut rx = RX_QUEUE.lock();
    if rx.is_empty() {
        return None;
    }
    let (pa, len) = rx.remove(0);
    // Copy from PA into buf
    let src = pa as *const u8;
    let to_copy = core::cmp::min(buf.len(), len as usize);
    unsafe { core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), to_copy); }
    // Free the physical page back to PMM
    crate::mm::pmm::free_page(pa);
    Some(to_copy)
}

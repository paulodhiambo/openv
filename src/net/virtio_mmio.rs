//! # VirtIO MMIO Network Driver
//!
//! This module provides a VirtIO MMIO network device driver for OpenV.
//! VirtIO is a standardized interface for virtual I/O devices, commonly
//! used in virtual machines and emulated environments.
//!
//! ## Overview
//!
//! This driver supports the legacy VirtIO MMIO specification with
//! `device-id=1` (network device). It uses two virtqueues:
//!
//! - **Queue 0 (RX)**: The device writes received frames into buffers
//!    provided by the driver.
//!  - **Queue 1 (TX)**: The driver writes outgoing frames for the
//!    device to transmit.
//!
//! Each RX descriptor is a single 4 KB page (10-byte virtio-net header
//! + ≤1514 B packet). Each TX submission is a single 4 KB page
//! (10-byte virtio-net header + packet data).
//!
//! ## Queue Layout
//!
//! The queue size is 64, with `QUEUE_ALIGN=4` so everything fits in one
//! 4 KB page per queue.
//!
//! ## Initialization
//!
//! The driver can be initialized in two ways:
//!
//! 1. **Direct FDT scan**: [`probe_and_init`] walks the DTB looking for
//!    VirtIO MMIO network devices.
//! 2. **Driver framework**: [`probe_driver`] is called by the driver
//!    framework when it finds a `virtio,mmio` node in the FDT.
//!
//! ## Public API
//!
//! - [`try_dequeue_rx`]: Dequeue one raw received frame. Called by
//!    `sys_net_recv`.

use crate::net::NetDevice;
use crate::net::virtqueue::VirtQueue;
use crate::println;
use alloc::boxed::Box;
use alloc::vec::Vec;
use crate::sync::Mutex;

/// Global storage for the active VirtIO driver instance.
static DRIVER_GLOBAL: Mutex<Option<&'static VirtioDriver>> = Mutex::new(None);

// ── MMIO register offsets (legacy spec) ──────────────────────────────────────

#[allow(dead_code)]
const OFF_MAGIC: usize          = 0x000;
#[allow(dead_code)]
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

/// VirtIO device magic value ("virt" in little-endian).
const VIRTIO_MAGIC: u32               = 0x7472_6976;
/// VirtIO device ID for network devices.
const VIRTIO_DEVICE_NET: u32          = 1;
/// VirtIO status: ACKNOWLEDGE.
const VIRTIO_STATUS_ACKNOWLEDGE: u32  = 1;
/// VirtIO status: DRIVER.
const VIRTIO_STATUS_DRIVER: u32       = 2;
/// VirtIO status: DRIVER_OK.
const VIRTIO_STATUS_DRIVER_OK: u32    = 4;

// virtio-net header (legacy, no VIRTIO_NET_F_MRG_RXBUF)
const VNET_HDR_LEN: usize = 10;

// RX queue: pre-allocated write-descriptors; 8 is sufficient for bursts
const RX_BUF_COUNT: usize = 8;
// Descriptor ring size per queue (power-of-two, fits with QUEUE_ALIGN=4 in one page)
const QUEUE_SIZE: usize = 64;

// ── Driver struct ──────────────────────────────────────────────────────────────

/// The VirtIO network driver.
///
/// # Fields
///
/// * `base` - The MMIO base address.
/// * `rx_vq` - The RX virtqueue, protected by a [`Mutex`].
/// * `tx_vq` - The TX virtqueue, protected by a [`Mutex`].
/// * `irq` - The interrupt request number.
///
/// # Safety
///
/// `VirtioDriver` is `Send` and `Sync` because all access to the MMIO
/// registers and virtqueues is serialized via the contained mutexes.
struct VirtioDriver {
    /// The MMIO base address.
    base:   usize,
    /// The RX virtqueue, protected by a [`Mutex`].
    rx_vq:  Mutex<VirtQueue>,
    /// The TX virtqueue, protected by a [`Mutex`].
    tx_vq:  Mutex<VirtQueue>,
    /// The interrupt request number.
    irq:    u32,
}

// SAFETY: See field-level documentation.
unsafe impl Send for VirtioDriver {}
unsafe impl Sync for VirtioDriver {}

// ── MMIO helpers ──────────────────────────────────────────────────────────────

/// Reads a 32-bit value from an MMIO register.
///
/// # Arguments
///
/// * `base` - The MMIO base address.
/// * `offset` - The register offset.
///
/// # Returns
///
/// The 32-bit value read from the register.
///
/// # Safety
///
/// `base` must be a valid MMIO physical address that is identity-mapped
/// by the kernel. The function uses `read_volatile` to prevent the
/// compiler from eliding or reordering the hardware register read.
fn mmio_read32(base: usize, offset: usize) -> u32 {
    // SAFETY: see function documentation.
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

/// Writes a 32-bit value to an MMIO register.
///
/// # Arguments
///
/// * `base` - The MMIO base address.
/// * `offset` - The register offset.
/// * `v` - The value to write.
///
/// # Safety
///
/// `base` must be a valid MMIO physical address that is identity-mapped
/// by the kernel. The function uses `write_volatile` to prevent the
/// compiler from eliding or reordering the hardware register write.
fn mmio_write32(base: usize, offset: usize, v: u32) {
    // SAFETY: see function documentation.
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, v) }
}

// ── Initialization ────────────────────────────────────────────────────────────

impl VirtioDriver {
    /// Creates a new [`VirtioDriver`] instance.
    fn new(base: usize, rx_pa: usize, tx_pa: usize, qsize: usize, irq: u32) -> Self {
        VirtioDriver {
            base,
            rx_vq: Mutex::new(VirtQueue::new(rx_pa, qsize)),
            tx_vq: Mutex::new(VirtQueue::new(tx_pa, qsize)),
            irq,
        }
    }

    /// Sets up a single virtqueue in the device.
    ///
    /// # Arguments
    ///
    /// * `sel` - The queue selector (0 for RX, 1 for TX).
    /// * `pa` - The physical address of the queue's memory.
    /// * `size` - The requested queue size.
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

    /// Initializes the device.
    ///
    /// This function:
    /// 1. Resets the device.
/// 2. Sets the ACKNOWLEDGE and DRIVER status bits.
/// 3. Negotiates zero optional features.
/// 4. Sets the guest page size.
/// 5. Sets up the RX and TX queues.
/// 6. Signals DRIVER_OK to make the device live.
/// 7. Pre-fills the RX queue with buffers.
/// 8. Notifies the device that new RX buffers are available.
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
    /// Sends a packet over the network.
    ///
    /// # Arguments
    ///
    /// * `packet` - The packet data to send.
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

    /// Receives a packet from the network.
    ///
    /// # Arguments
    ///
    /// * `buf` - The buffer to store the received packet.
    ///
    /// # Returns
    ///
    /// The number of bytes received, or 0 if no packet is available.
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
    /// Handles a device interrupt.
    ///
    /// # Arguments
    ///
    /// * `irq` - The interrupt request number.
    fn on_interrupt(&self, irq: usize) {
        handle_interrupt(irq as u32);
    }
}

/// Handles a device interrupt.
///
/// # Arguments
///
/// * `irq` - The interrupt request number.
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

/// Probes the DTB for a VirtIO MMIO network device and initializes it.
///
/// # Returns
///
/// `true` if a device was found and initialized, `false` otherwise.
///
/// # Safety
///
/// This function reads from the DTB, which must be a valid DTB address.
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
        if base == 0
            && let Some(idx) = node.name.rfind('@')
            && let Ok(x) = usize::from_str_radix(&node.name[idx + 1..], 16)
        {
            base = x;
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

/// Probes a specific MMIO base address for a VirtIO network device.
///
/// This is called by the driver framework when it finds a `virtio,mmio`
/// node in the FDT.
///
/// # Arguments
///
/// * `base` - The MMIO base address.
/// * `irq` - The interrupt request number.
///
/// # Returns
///
/// `Some(Box<dyn Driver>)` if a device was found and initialized,
/// `None` otherwise.
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

/// Initializes a VirtIO network device at the given MMIO base address.
///
/// # Arguments
///
/// * `base` - The MMIO base address.
/// * `irq` - The interrupt request number.
///
/// # Returns
///
/// `Some(&'static VirtioDriver)` on success, `None` if memory
/// allocation fails.
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

/// A wrapper that implements [`crate::drivers::Driver`] for a static
/// reference to a [`VirtioDriver`].
struct VirtioDriverRef(&'static VirtioDriver);
// SAFETY: Delegates to the inner VirtioDriver's safety guarantees.
unsafe impl Send for VirtioDriverRef {}
unsafe impl Sync for VirtioDriverRef {}
impl crate::drivers::Driver for VirtioDriverRef {
    fn on_interrupt(&self, irq: usize) { self.0.on_interrupt(irq); }
}

/// Returns the DTB pointer from the global storage.
fn boot_dtb_ptr() -> usize {
    crate::boot_dtb_ptr()
}

/// Dequeues one raw received frame.
///
/// This is called by `sys_net_recv` to provide network data to user-space.
///
/// # Arguments
///
/// * `buf` - The buffer to store the received frame.
///
/// # Returns
///
/// `Some(n)` with the number of bytes received, or `None` if no frame
/// is available.
pub fn try_dequeue_rx(buf: &mut [u8]) -> Option<usize> {
    let guard = DRIVER_GLOBAL.lock();
    let drv = (*guard)?;
    let n = drv.recv(buf);
    if n > 0 { Some(n) } else { None }
}

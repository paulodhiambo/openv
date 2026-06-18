//! Kernel-side VirtIO block driver (polling, device_id = 2).
//!
//! Used exclusively by kernel subsystems (e.g. the swap module) that need
//! direct block I/O without going through the userspace virtio-blk-driver
//! process.  Normal file I/O goes through the VFS server instead.
//!
//! The driver uses the legacy VirtIO 1.0 split virtqueue with polling
//! (no interrupts).  A single request queue (queue 0) is set up.

extern crate alloc;
use alloc::sync::Arc;
use core::ptr::{read_volatile, write_volatile};
use crate::sync::Mutex;
use super::BlockDevice;

// ── MMIO offsets (same as virtio_mmio.rs) ────────────────────────────────────
const OFF_MAGIC:           usize = 0x000;
const OFF_DEVICE_ID:       usize = 0x008;
const OFF_DEVICE_FEATURES: usize = 0x010;
const OFF_DRIVER_FEATURES: usize = 0x020;
const OFF_GUEST_PAGE_SIZE: usize = 0x028;
const OFF_QUEUE_SEL:       usize = 0x030;
const OFF_QUEUE_NUM_MAX:   usize = 0x034;
const OFF_QUEUE_NUM:       usize = 0x038;
const OFF_QUEUE_ALIGN:     usize = 0x03c;
const OFF_QUEUE_PFN:       usize = 0x040;
const OFF_QUEUE_NOTIFY:    usize = 0x050;
const OFF_STATUS:          usize = 0x070;

const VIRTIO_MAGIC:             u32 = 0x7472_6976;
const VIRTIO_DEVICE_BLK:        u32 = 2;
const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
const VIRTIO_STATUS_DRIVER:      u32 = 2;
const VIRTIO_STATUS_DRIVER_OK:   u32 = 4;

// ── VirtIO block request types ────────────────────────────────────────────────
const VIRTIO_BLK_T_IN:  u32 = 0; // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write

const QUEUE_SIZE: usize = 16;

// ── Request layout (three-descriptor chain) ───────────────────────────────────
//
//   desc[0] → VirtioBlkReqHdr  (device-readable)
//   desc[1] → data buffer      (device-readable for write, device-writable for read)
//   desc[2] → status byte      (device-writable)
//
#[repr(C, packed)]
struct VirtioBlkReqHdr {
    type_:    u32,
    reserved: u32,
    sector:   u64,
}

// ── Virtqueue helpers (minimal, mirrors virtqueue.rs) ─────────────────────────

#[repr(C)]
struct Desc {
    addr:  u64,
    len:   u32,
    flags: u16,
    next:  u16,
}

const DESC_F_NEXT:  u16 = 1;
const DESC_F_WRITE: u16 = 2;

/// Minimal synchronous virtqueue: only supports one in-flight request.
struct BlkVirtQueue {
    base:          usize, // physical address of the queue page
    descs:         *mut Desc,
    avail_flags:   *mut u16,
    avail_idx:     *mut u16,
    avail_ring:    *mut u16,
    used_idx:      *mut u16,
    last_used_idx: u16,
    size:          usize,
}

// SAFETY: BlkVirtQueue contains raw pointers but is only accessed under a Mutex.
unsafe impl Send for BlkVirtQueue {}
unsafe impl Sync for BlkVirtQueue {}

impl BlkVirtQueue {
    /// Build a virtqueue from a single 4 KB physical page.
    ///
    /// Layout (mirrors the virtqueue.rs layout):
    ///   [0..16*size]                          descriptor table (16 bytes each)
    ///   [desc_end..desc_end+2+2+2*size]       avail ring (flags, idx, ring[])
    ///   [aligned(avail_end, 4)..]             used ring (flags, idx, used_elem[])
    fn new(pa: usize, size: usize) -> Self {
        let descs     = pa as *mut Desc;
        let avail_ptr = unsafe { (pa as *mut u8).add(size * core::mem::size_of::<Desc>()) };
        let avail_flags = avail_ptr as *mut u16;
        let avail_idx   = unsafe { (avail_ptr as *mut u16).add(1) };
        let avail_ring  = unsafe { (avail_ptr as *mut u16).add(2) };
        // used ring: align to 4 bytes after avail ring
        let avail_end = (avail_ptr as usize) + 2 + 2 + 2 * size;
        let used_off  = (avail_end + 3) & !3;
        // used.flags at offset 0, used.idx at offset 2
        let used_idx  = unsafe { (used_off as *mut u16).add(1) };
        BlkVirtQueue { base: pa, descs, avail_flags, avail_idx, avail_ring,
                       used_idx, last_used_idx: 0, size }
    }

    /// Submit a chained request and poll until the device signals completion.
    /// Returns the status byte written by the device (0 = OK).
    fn submit_and_poll(
        &mut self,
        mmio: usize,
        hdr_pa: u64,
        data_pa: u64,
        data_len: u32,
        status_pa: u64,
        write: bool,
    ) -> u8 {
        // Build descriptor chain in slots 0→1→2.
        unsafe {
            let d = &mut *self.descs;
            // desc[0]: request header (device-readable)
            (*self.descs.add(0)) = Desc {
                addr:  hdr_pa,
                len:   core::mem::size_of::<VirtioBlkReqHdr>() as u32,
                flags: DESC_F_NEXT,
                next:  1,
            };
            // desc[1]: data buffer (readable or writable depending on direction)
            (*self.descs.add(1)) = Desc {
                addr:  data_pa,
                len:   data_len,
                flags: if write { DESC_F_NEXT } else { DESC_F_NEXT | DESC_F_WRITE },
                next:  2,
            };
            // desc[2]: status byte (always device-writable)
            (*self.descs.add(2)) = Desc {
                addr:  status_pa,
                len:   1,
                flags: DESC_F_WRITE,
                next:  0,
            };
            let _ = d; // suppress unused warning

            // Place desc[0] in the avail ring.
            let ai = read_volatile(self.avail_idx) as usize % self.size;
            write_volatile(self.avail_ring.add(ai), 0u16); // desc chain head = 0
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            write_volatile(self.avail_idx, read_volatile(self.avail_idx).wrapping_add(1));
        }

        // Notify the device: queue 0.
        unsafe {
            core::ptr::write_volatile((mmio + OFF_QUEUE_NOTIFY) as *mut u32, 0);
        }

        // Poll until the device advances the used ring.
        loop {
            core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
            let used_i = unsafe { read_volatile(self.used_idx) };
            if used_i != self.last_used_idx { break; }
        }
        self.last_used_idx = self.last_used_idx.wrapping_add(1);

        // Read the status byte written by the device.
        unsafe { read_volatile(status_pa as *const u8) }
    }
}

// ── Driver struct ─────────────────────────────────────────────────────────────

pub struct VirtioBlkDev {
    mmio:   usize,
    queue:  Mutex<BlkVirtQueue>,
    blocks: u64,
    /// Per-request scratch pages: [0]=header, [1]=status, [2]=data.
    hdr_pa:    usize,
    status_pa: usize,
}

unsafe impl Send for VirtioBlkDev {}
unsafe impl Sync for VirtioBlkDev {}

fn mmio_read32(base: usize, offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}
fn mmio_write32(base: usize, offset: usize, v: u32) {
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, v) }
}

impl BlockDevice for VirtioBlkDev {
    fn block_size(&self) -> usize { 512 }

    fn block_count(&self) -> u64 { self.blocks }

    fn read_blocks(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        assert_eq!(buf.len(), count as usize * 512, "buffer size mismatch");
        let mut q = self.queue.lock();

        // Write the request header.
        let hdr = self.hdr_pa as *mut VirtioBlkReqHdr;
        unsafe { core::ptr::write_volatile(hdr, VirtioBlkReqHdr {
            type_: VIRTIO_BLK_T_IN, reserved: 0, sector: lba,
        }) };

        // Clear status byte.
        unsafe { core::ptr::write_volatile(self.status_pa as *mut u8, 0xFF) };

        // Data buffer is the caller's buf — pin its physical address.
        // Since the kernel uses identity mapping (VA == PA), this is safe.
        let data_pa = buf.as_mut_ptr() as usize as u64;

        let status = q.submit_and_poll(
            self.mmio,
            self.hdr_pa as u64,
            data_pa,
            buf.len() as u32,
            self.status_pa as u64,
            false,
        );
        if status == 0 { Ok(()) } else { Err("virtio-blk read error") }
    }

    fn write_blocks(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), &'static str> {
        assert_eq!(buf.len(), count as usize * 512, "buffer size mismatch");
        let mut q = self.queue.lock();

        let hdr = self.hdr_pa as *mut VirtioBlkReqHdr;
        unsafe { core::ptr::write_volatile(hdr, VirtioBlkReqHdr {
            type_: VIRTIO_BLK_T_OUT, reserved: 0, sector: lba,
        }) };
        unsafe { core::ptr::write_volatile(self.status_pa as *mut u8, 0xFF) };

        let data_pa = buf.as_ptr() as usize as u64;

        let status = q.submit_and_poll(
            self.mmio,
            self.hdr_pa as u64,
            data_pa,
            buf.len() as u32,
            self.status_pa as u64,
            true,
        );
        if status == 0 { Ok(()) } else { Err("virtio-blk write error") }
    }
}

// ── Probing ───────────────────────────────────────────────────────────────────

/// Scan the VirtIO MMIO slots for a block device.  On success, registers
/// the device in the global registry under `"vda"` (or `"vdb"`, …).
///
/// Returns the number of block devices found.
pub fn probe_and_register() -> usize {
    use crate::mm::pmm::alloc_frame;

    let mut found = 0usize;
    let letters = b"abcdefgh";

    for slot in 1..=8u32 {
        let base = 0x1000_0000usize + slot as usize * 0x1000;
        if mmio_read32(base, OFF_MAGIC) != VIRTIO_MAGIC { continue; }
        if mmio_read32(base, OFF_DEVICE_ID) != VIRTIO_DEVICE_BLK { continue; }

        // Allocate pages: one for the virtqueue, one for the request header,
        // one for the status byte.
        let (Some(qf), Some(hf), Some(sf)) = (
            alloc_frame(), alloc_frame(), alloc_frame()
        ) else {
            crate::println!("block: virtio-blk slot {} — OOM during init", slot);
            continue;
        };
        let (q_pa, h_pa, s_pa) = (qf.pa(), hf.pa(), sf.pa());
        qf.into_raw(); hf.into_raw(); sf.into_raw();

        // Negotiate device.
        mmio_write32(base, OFF_STATUS, 0);
        mmio_write32(base, OFF_STATUS, VIRTIO_STATUS_ACKNOWLEDGE);
        mmio_write32(base, OFF_STATUS, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);
        let _ = mmio_read32(base, OFF_DEVICE_FEATURES);
        mmio_write32(base, OFF_DRIVER_FEATURES, 0);
        mmio_write32(base, OFF_GUEST_PAGE_SIZE, 4096);

        // Set up queue 0.
        mmio_write32(base, OFF_QUEUE_SEL, 0);
        let max = mmio_read32(base, OFF_QUEUE_NUM_MAX) as usize;
        let qsz = if max == 0 { QUEUE_SIZE } else { max.min(QUEUE_SIZE) };
        mmio_write32(base, OFF_QUEUE_NUM,   qsz as u32);
        mmio_write32(base, OFF_QUEUE_ALIGN, 4);
        mmio_write32(base, OFF_QUEUE_PFN,   (q_pa >> 12) as u32);

        mmio_write32(base, OFF_STATUS,
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK);

        // Read device capacity (two 32-bit registers at config offset 0x100).
        let cap_lo = mmio_read32(base, 0x100) as u64;
        let cap_hi = mmio_read32(base, 0x104) as u64;
        let blocks = cap_lo | (cap_hi << 32);

        let dev = Arc::new(VirtioBlkDev {
            mmio:      base,
            queue:     Mutex::new(BlkVirtQueue::new(q_pa, qsz)),
            blocks,
            hdr_pa:    h_pa,
            status_pa: s_pa,
        });

        let name = alloc::format!("vd{}", letters[found] as char);
        crate::println!("block: virtio-blk at {:#x} → /dev/{} ({} blocks)", base, name, blocks);
        super::register(&name, dev);
        found += 1;
    }
    found
}

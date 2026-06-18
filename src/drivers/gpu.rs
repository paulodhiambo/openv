//! # VirtIO GPU 2D Framebuffer Driver
//!
//! This module provides a VirtIO GPU 2D framebuffer driver for OpenV.
//! It scans QEMU virt MMIO slots for a VirtIO GPU device (device_id = 16),
//! initializes a 640×480 ARGB8888 framebuffer backed by scatter-gather
//! physical pages, and exposes a simple API for writing pixels and flushing
//! to the display.
//!
//! ## Framebuffer Layout
//!
//! 640×480×4 = 1,228,800 bytes = 300 pages (4 KiB each). Each page is
//! individually allocated from the PMM and registered as a backing entry
//! in the VirtIO GPU resource. Pixel writes go through a local scatter-gather
//! lookup, and flush sends TRANSFER_TO_HOST_2D + RESOURCE_FLUSH commands.
//!
//! ## VirtIO Protocol
//!
//! Uses the legacy VirtIO MMIO split virtqueue (control queue index 0).
//! Command/response pairs are sent synchronously by polling the used ring.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering, fence};
use crate::sync::Mutex;
use crate::println;

// ── Display geometry ──────────────────────────────────────────────────────────

const FB_WIDTH: u32  = 640;
const FB_HEIGHT: u32 = 480;
const FB_BPP: u32    = 4;           // bytes per pixel (ARGB8888)
const FB_BYTES: usize = (FB_WIDTH * FB_HEIGHT * FB_BPP) as usize; // 1,228,800
const PAGE_SIZE: usize = 4096;
const FB_PAGES: usize = (FB_BYTES + PAGE_SIZE - 1) / PAGE_SIZE;   // 300

// ── MMIO slot scan range (QEMU virt machine) ─────────────────────────────────

const MMIO_BASE_START: usize = 0x1000_1000;
const MMIO_BASE_END: usize   = 0x1000_8000;
const MMIO_STRIDE: usize     = 0x1000;

// ── MMIO register offsets (legacy VirtIO 1.0) ────────────────────────────────

const OFF_MAGIC: usize           = 0x000;
const OFF_DEVICE_ID: usize       = 0x008;
const OFF_DEVICE_FEATURES: usize = 0x010;
const OFF_DRIVER_FEATURES: usize = 0x020;
const OFF_GUEST_PAGE_SIZE: usize = 0x028;
const OFF_QUEUE_SEL: usize       = 0x030;
const OFF_QUEUE_NUM_MAX: usize   = 0x034;
const OFF_QUEUE_NUM: usize       = 0x038;
const OFF_QUEUE_ALIGN: usize     = 0x03c;
const OFF_QUEUE_PFN: usize       = 0x040;
const OFF_QUEUE_NOTIFY: usize    = 0x050;
const OFF_STATUS: usize          = 0x070;

const VIRTIO_MAGIC: u32             = 0x7472_6976; // "virt" LE
const VIRTIO_DEVICE_GPU: u32        = 16;
const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
const VIRTIO_STATUS_DRIVER: u32      = 2;
const VIRTIO_STATUS_DRIVER_OK: u32   = 4;

// ── VirtIO GPU command/response types ─────────────────────────────────────────

#[allow(dead_code)]
const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32    = 0x101;
const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x102;
const VIRTIO_GPU_CMD_SET_SCANOUT: u32           = 0x103;
const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32        = 0x104;
const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32   = 0x105;
#[allow(dead_code)]
const VIRTIO_GPU_RESP_OK_NODATA: u32            = 0x1100;

const VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM: u32 = 1;

// ── Simple virtqueue (QSZ = 16) ───────────────────────────────────────────────

const QSZ: usize = 16;

/// Descriptor flags.
const VIRTQ_DESC_F_NEXT: u16  = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// Raw split virtqueue backed by a single 4 KiB page.
struct SimpleVirtQ {
    /// Physical address of the queue page.
    pa: usize,
    /// Shadow avail_idx kept by the driver.
    avail_idx: u16,
    /// Shadow last_used_idx kept by the driver.
    last_used: u16,
}

impl SimpleVirtQ {
    /// Create from an already-zeroed page at `pa`.
    fn new(pa: usize) -> Self {
        SimpleVirtQ { pa, avail_idx: 0, last_used: 0 }
    }

    // ── Queue memory layout (offsets within the 4 KiB page) ──────────────────
    //
    //  [0 ..  QSZ*16)              Descriptor table  (16 bytes × QSZ)
    //  [QSZ*16 .. QSZ*16+4+QSZ*2) Avail ring:  flags(u16), idx(u16), ring[QSZ](u16)
    //  Align up to 4 bytes
    //  [aligned ..]                Used ring: flags(u16), idx(u16), ring[QSZ](UsedElem)

    fn desc_offset(i: usize) -> usize { i * 16 }
    #[allow(dead_code)]
    fn avail_flags_offset() -> usize  { QSZ * 16 }
    fn avail_idx_offset() -> usize    { QSZ * 16 + 2 }
    fn avail_ring_offset(i: usize) -> usize { QSZ * 16 + 4 + i * 2 }

    fn used_base_offset() -> usize {
        let raw = QSZ * 16 + 4 + QSZ * 2;
        (raw + 3) & !3
    }
    #[allow(dead_code)]
    fn used_flags_offset() -> usize { Self::used_base_offset() }
    fn used_idx_offset() -> usize   { Self::used_base_offset() + 2 }
    #[allow(dead_code)]
    fn used_ring_offset(i: usize) -> usize { Self::used_base_offset() + 4 + i * 8 }

    fn ptr<T>(&self, off: usize) -> *mut T {
        (self.pa + off) as *mut T
    }

    fn read_used_idx(&self) -> u16 {
        // SAFETY: used_idx_offset is within the zeroed PMM page.
        unsafe { core::ptr::read_volatile(self.ptr::<u16>(Self::used_idx_offset())) }
    }

    /// Submit a 2-descriptor request (device-readable cmd, device-writable resp)
    /// and spin until the device completes it.
    ///
    /// # Safety
    ///
    /// `cmd_pa`/`resp_pa` must be valid physical addresses for the given lengths.
    unsafe fn send_recv_sync(
        &mut self,
        cmd_pa:  usize, cmd_len:  u32,
        resp_pa: usize, resp_len: u32,
        mmio_base: usize,
    ) {
        // desc[0] = cmd (device-readable)
        // SAFETY: indices 0 and 1 are within QSZ; addresses are caller-guaranteed valid.
        unsafe {
            self.write_desc(0, cmd_pa as u64, cmd_len, VIRTQ_DESC_F_NEXT, 1);
            // desc[1] = resp (device-writable)
            self.write_desc(1, resp_pa as u64, resp_len, VIRTQ_DESC_F_WRITE, 0);

            // avail.ring[avail_idx % QSZ] = 0 (first desc index)
            let slot = (self.avail_idx as usize) % QSZ;
            core::ptr::write_volatile(self.ptr::<u16>(Self::avail_ring_offset(slot)), 0u16);
            fence(Ordering::Release);
            self.avail_idx = self.avail_idx.wrapping_add(1);
            core::ptr::write_volatile(self.ptr::<u16>(Self::avail_idx_offset()), self.avail_idx);
            fence(Ordering::Release);
        }

        // Notify device: queue index 0
        mmio_write32(mmio_base, OFF_QUEUE_NOTIFY, 0);

        // Poll used ring
        loop {
            fence(Ordering::Acquire);
            let ui = self.read_used_idx();
            if ui != self.last_used {
                self.last_used = self.last_used.wrapping_add(1);
                break;
            }
            core::hint::spin_loop();
        }
    }

    /// Write a descriptor entry.
    ///
    /// # Safety
    ///
    /// `i` must be < QSZ.
    unsafe fn write_desc(&self, i: usize, addr: u64, len: u32, flags: u16, next: u16) {
        let off = Self::desc_offset(i);
        // SAFETY: `off` is within the PMM page backing this virtqueue.
        unsafe {
            core::ptr::write_volatile(self.ptr::<u64>(off),       addr);
            core::ptr::write_volatile(self.ptr::<u32>(off + 8),   len);
            core::ptr::write_volatile(self.ptr::<u16>(off + 12),  flags);
            core::ptr::write_volatile(self.ptr::<u16>(off + 14),  next);
        }
    }
}

// ── GPU command structs (repr C, little-endian) ───────────────────────────────

#[repr(C)]
struct GpuCtrlHdr {
    type_:    u32,
    flags:    u32,
    fence_id: u64,
    ctx_id:   u32,
    ring_idx: u8,
    pad:      [u8; 3],
}

impl GpuCtrlHdr {
    fn new(type_: u32) -> Self {
        GpuCtrlHdr { type_, flags: 0, fence_id: 0, ctx_id: 0, ring_idx: 0, pad: [0; 3] }
    }
}

#[repr(C)]
struct GpuRect {
    x: u32, y: u32, width: u32, height: u32,
}

#[repr(C)]
struct GpuResourceCreate2d {
    hdr:         GpuCtrlHdr,
    resource_id: u32,
    format:      u32,
    width:        u32,
    height:       u32,
}

/// Variable-length: header + nr_entries × GpuMemEntry.
/// We use a fixed-size struct for the header only and build the payload in a Vec.
#[repr(C)]
struct GpuResourceAttachBackingHdr {
    hdr:         GpuCtrlHdr,
    resource_id: u32,
    nr_entries:  u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpuMemEntry {
    addr:    u64,
    length:  u32,
    padding: u32,
}

#[repr(C)]
struct GpuSetScanout {
    hdr:         GpuCtrlHdr,
    r:           GpuRect,
    scanout_id:  u32,
    resource_id: u32,
}

#[repr(C)]
struct GpuTransferToHost2d {
    hdr:         GpuCtrlHdr,
    r:           GpuRect,
    offset:      u64,
    resource_id: u32,
    padding:     u32,
}

#[repr(C)]
struct GpuResourceFlush {
    hdr:         GpuCtrlHdr,
    r:           GpuRect,
    resource_id: u32,
    padding:     u32,
}

/// Generic OK-nodata response.
#[repr(C)]
struct GpuRespNodata {
    hdr: GpuCtrlHdr,
}

// ── MMIO helpers ──────────────────────────────────────────────────────────────

fn mmio_read32(base: usize, off: usize) -> u32 {
    // SAFETY: caller guarantees base is a valid MMIO identity-mapped address.
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}

fn mmio_write32(base: usize, off: usize, v: u32) {
    // SAFETY: caller guarantees base is a valid MMIO identity-mapped address.
    unsafe { core::ptr::write_volatile((base + off) as *mut u32, v) }
}

// ── GPU driver state ───────────────────────────────────────────────────────────

struct GpuDriver {
    /// MMIO base of the VirtIO GPU device.
    mmio_base: usize,
    /// Control virtqueue (index 0).
    vq: Mutex<SimpleVirtQ>,
    /// Scatter-gather backing: Vec of (PA, bytes) pairs, one per 4 KiB page.
    fb_pages: Vec<usize>,
    /// Scratch command page (PA).
    cmd_pa: usize,
    /// Scratch response page (PA).
    resp_pa: usize,
}

// SAFETY: all mutable state behind Mutex; raw pointers stable for kernel lifetime.
unsafe impl Send for GpuDriver {}
unsafe impl Sync for GpuDriver {}

static GPU_READY: AtomicBool = AtomicBool::new(false);
static GPU_DRIVER: Mutex<Option<&'static GpuDriver>> = Mutex::new(None);

// ── Public API ────────────────────────────────────────────────────────────────

/// Scans QEMU virt MMIO slots for a VirtIO GPU device and initializes it.
///
/// Returns `true` if a GPU was found and initialized.
pub fn probe() -> bool {
    let mut base = MMIO_BASE_START;
    while base < MMIO_BASE_END {
        if mmio_read32(base, OFF_MAGIC) == VIRTIO_MAGIC
            && mmio_read32(base, OFF_DEVICE_ID) == VIRTIO_DEVICE_GPU
        {
            if let Some(drv) = init_gpu(base) {
                println!("virtio-gpu: initialized at {:#x} ({}×{})", base, FB_WIDTH, FB_HEIGHT);
                *GPU_DRIVER.lock() = Some(drv);
                GPU_READY.store(true, Ordering::Release);
                return true;
            }
        }
        base += MMIO_STRIDE;
    }
    println!("virtio-gpu: no device found");
    false
}

/// Write a single pixel to the local scatter-gather backing buffer.
///
/// `argb` is 0xAARRGGBB. Does nothing if GPU was not initialized.
pub fn write_pixel(x: u32, y: u32, argb: u32) {
    if !GPU_READY.load(Ordering::Acquire) { return; }
    if x >= FB_WIDTH || y >= FB_HEIGHT { return; }

    let guard = GPU_DRIVER.lock();
    let drv = match *guard { Some(d) => d, None => return };

    let byte_off = ((y * FB_WIDTH + x) * FB_BPP) as usize;
    let page_idx = byte_off / PAGE_SIZE;
    let page_off = byte_off % PAGE_SIZE;

    if page_idx >= drv.fb_pages.len() { return; }
    let pa = drv.fb_pages[page_idx] + page_off;
    // SAFETY: `pa` is a PMM-allocated page we own; offset within page.
    unsafe { core::ptr::write_volatile(pa as *mut u32, argb); }
}

/// Flush the entire framebuffer to the display.
///
/// Sends TRANSFER_TO_HOST_2D followed by RESOURCE_FLUSH. Blocks until both
/// commands complete (polling). Does nothing if GPU was not initialized.
pub fn flush() {
    if !GPU_READY.load(Ordering::Acquire) { return; }

    let guard = GPU_DRIVER.lock();
    let drv = match *guard { Some(d) => d, None => return };

    let mut vq = drv.vq.lock();
    let cmd_pa  = drv.cmd_pa;
    let resp_pa = drv.resp_pa;
    let base    = drv.mmio_base;

    // ── TRANSFER_TO_HOST_2D ───────────────────────────────────────────────────
    let full_rect = GpuRect { x: 0, y: 0, width: FB_WIDTH, height: FB_HEIGHT };
    let xfer = GpuTransferToHost2d {
        hdr:         GpuCtrlHdr::new(VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D),
        r:           full_rect,
        offset:      0,
        resource_id: 1,
        padding:     0,
    };
    write_cmd(cmd_pa, &xfer);
    clear_resp(resp_pa);
    // SAFETY: cmd_pa and resp_pa are valid PMM pages we own.
    unsafe {
        vq.send_recv_sync(
            cmd_pa, core::mem::size_of::<GpuTransferToHost2d>() as u32,
            resp_pa, core::mem::size_of::<GpuRespNodata>() as u32,
            base,
        );
    }

    // ── RESOURCE_FLUSH ────────────────────────────────────────────────────────
    let flush_rect = GpuRect { x: 0, y: 0, width: FB_WIDTH, height: FB_HEIGHT };
    let flush_cmd = GpuResourceFlush {
        hdr:         GpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_FLUSH),
        r:           flush_rect,
        resource_id: 1,
        padding:     0,
    };
    write_cmd(cmd_pa, &flush_cmd);
    clear_resp(resp_pa);
    unsafe {
        vq.send_recv_sync(
            cmd_pa, core::mem::size_of::<GpuResourceFlush>() as u32,
            resp_pa, core::mem::size_of::<GpuRespNodata>() as u32,
            base,
        );
    }
}

/// Returns the framebuffer width in pixels.
pub fn width() -> u32  { FB_WIDTH }
/// Returns the framebuffer height in pixels.
pub fn height() -> u32 { FB_HEIGHT }

// ── Initialization ────────────────────────────────────────────────────────────

fn init_gpu(base: usize) -> Option<&'static GpuDriver> {
    // ── VirtIO init sequence ──────────────────────────────────────────────────
    mmio_write32(base, OFF_STATUS, 0);                                             // reset
    mmio_write32(base, OFF_STATUS, VIRTIO_STATUS_ACKNOWLEDGE);
    mmio_write32(base, OFF_STATUS, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);
    let _ = mmio_read32(base, OFF_DEVICE_FEATURES);
    mmio_write32(base, OFF_DRIVER_FEATURES, 0);
    mmio_write32(base, OFF_GUEST_PAGE_SIZE, PAGE_SIZE as u32);

    // Set up control queue (queue 0)
    mmio_write32(base, OFF_QUEUE_SEL, 0);
    let qmax = mmio_read32(base, OFF_QUEUE_NUM_MAX);
    let qsz = if qmax == 0 || qmax as usize > QSZ { QSZ } else { qmax as usize };
    mmio_write32(base, OFF_QUEUE_NUM,   qsz as u32);
    mmio_write32(base, OFF_QUEUE_ALIGN, 4);

    let vq_pa = crate::mm::pmm::alloc_page()?;
    // SAFETY: freshly allocated page, zero it.
    unsafe { core::ptr::write_bytes(vq_pa as *mut u8, 0, PAGE_SIZE); }
    mmio_write32(base, OFF_QUEUE_PFN, (vq_pa >> 12) as u32);

    mmio_write32(base, OFF_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK);

    // ── Allocate scratch pages for commands and responses ─────────────────────
    let cmd_pa = crate::mm::pmm::alloc_page()?;
    let resp_pa = crate::mm::pmm::alloc_page()?;
    unsafe {
        core::ptr::write_bytes(cmd_pa as *mut u8, 0, PAGE_SIZE);
        core::ptr::write_bytes(resp_pa as *mut u8, 0, PAGE_SIZE);
    }

    // ── Allocate framebuffer backing pages ────────────────────────────────────
    let mut fb_pages: Vec<usize> = Vec::with_capacity(FB_PAGES);
    for _ in 0..FB_PAGES {
        let pa = crate::mm::pmm::alloc_page()?;
        // SAFETY: freshly allocated, zero the page.
        unsafe { core::ptr::write_bytes(pa as *mut u8, 0, PAGE_SIZE); }
        fb_pages.push(pa);
    }

    // ── Build driver struct (leaked into static lifetime) ─────────────────────
    let drv: &'static GpuDriver = alloc::boxed::Box::leak(alloc::boxed::Box::new(GpuDriver {
        mmio_base: base,
        vq:        Mutex::new(SimpleVirtQ::new(vq_pa)),
        fb_pages,
        cmd_pa,
        resp_pa,
    }));

    // ── GPU protocol initialization ───────────────────────────────────────────
    {
        let mut vq = drv.vq.lock();
        gpu_create_resource(&mut vq, base, cmd_pa, resp_pa)?;
        gpu_attach_backing(&mut vq, base, cmd_pa, resp_pa, &drv.fb_pages)?;
        gpu_set_scanout(&mut vq, base, cmd_pa, resp_pa)?;
    }

    Some(drv)
}

/// Send RESOURCE_CREATE_2D.
fn gpu_create_resource(
    vq: &mut SimpleVirtQ,
    base: usize,
    cmd_pa: usize,
    resp_pa: usize,
) -> Option<()> {
    let cmd = GpuResourceCreate2d {
        hdr:         GpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_CREATE_2D),
        resource_id: 1,
        format:      VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM,
        width:        FB_WIDTH,
        height:       FB_HEIGHT,
    };
    write_cmd(cmd_pa, &cmd);
    clear_resp(resp_pa);
    unsafe {
        vq.send_recv_sync(
            cmd_pa, core::mem::size_of::<GpuResourceCreate2d>() as u32,
            resp_pa, core::mem::size_of::<GpuRespNodata>() as u32,
            base,
        );
    }
    Some(())
}

/// Send RESOURCE_ATTACH_BACKING with scatter-gather pages.
///
/// Because the command is variable-length (header + nr_entries × GpuMemEntry),
/// we build it into the cmd_pa page directly. A single 4 KiB page can hold:
///   header (28 bytes) + 300 entries × 16 bytes = 4828 bytes < 4096 … oops.
///
/// 28 + 300*16 = 28 + 4800 = 4828 > 4096. We need two pages or a different
/// approach. Solution: allocate a second scratch page and use a 2-entry
/// descriptor chain: [header page (device-readable)] → [entries page (device-readable)]
/// → [response page (device-writable)].
///
/// We already have one scratch page for commands. We allocate a second one here
/// and leak it (it is used for the lifetime of the driver).
fn gpu_attach_backing(
    vq: &mut SimpleVirtQ,
    base: usize,
    cmd_pa: usize,
    resp_pa: usize,
    pages: &[usize],
) -> Option<()> {
    // Allocate an extra page to hold the GpuMemEntry array.
    let entries_pa = crate::mm::pmm::alloc_page()?;
    unsafe { core::ptr::write_bytes(entries_pa as *mut u8, 0, PAGE_SIZE); }

    let nr = pages.len() as u32;

    // Write the fixed-size header into cmd_pa.
    let hdr = GpuResourceAttachBackingHdr {
        hdr:         GpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING),
        resource_id: 1,
        nr_entries:  nr,
    };
    write_cmd(cmd_pa, &hdr);

    // Write the mem-entry array into entries_pa.
    for (i, &pa) in pages.iter().enumerate() {
        let entry = GpuMemEntry {
            addr:    pa as u64,
            length:  PAGE_SIZE as u32,
            padding: 0,
        };
        let off = i * core::mem::size_of::<GpuMemEntry>();
        // SAFETY: entries_pa is a fresh PMM page; off < FB_PAGES*16 = 4800 < 4096... wait.
        // 300 * 16 = 4800 > 4096. Need to check if this fits.
        // Actually PAGE_SIZE=4096 and 300*16=4800 > 4096.
        // We need to handle this differently: use multiple ATTACH_BACKING commands,
        // each with a batch of entries that fits in one page.
        // Max entries per page: (4096 - sizeof(GpuResourceAttachBackingHdr)) / sizeof(GpuMemEntry)
        // = (4096 - 28) / 16 = 4068 / 16 = 254 entries per page.
        // So we need ceil(300/254) = 2 commands. Let's do this in a loop below instead.
        let _ = (off, entry); // suppress unused warnings; we'll rewrite below.
        let _ = i;
        break; // exit immediately; full impl below
    }
    // Free the scratch entries page since we'll take a different approach.
    crate::mm::pmm::free_page(entries_pa);

    // ── Proper implementation: batch into page-sized chunks ───────────────────
    // Max entries that fit in one 4 KiB page alongside the header:
    // hdr_size = size_of::<GpuResourceAttachBackingHdr>() = 28 bytes
    // entry_size = size_of::<GpuMemEntry>() = 16 bytes
    // max_per_cmd = (PAGE_SIZE - hdr_size) / entry_size
    const HDR_SIZE: usize = core::mem::size_of::<GpuResourceAttachBackingHdr>();
    const ENTRY_SIZE: usize = core::mem::size_of::<GpuMemEntry>();
    const MAX_PER_CMD: usize = (PAGE_SIZE - HDR_SIZE) / ENTRY_SIZE;

    let mut remaining = pages;
    while !remaining.is_empty() {
        let batch_len = remaining.len().min(MAX_PER_CMD);
        let batch = &remaining[..batch_len];
        remaining = &remaining[batch_len..];

        let nr_batch = batch_len as u32;
        let cmd_bytes = HDR_SIZE + batch_len * ENTRY_SIZE;

        // Write header at start of cmd_pa
        let attach_hdr = GpuResourceAttachBackingHdr {
            hdr:         GpuCtrlHdr::new(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING),
            resource_id: 1,
            nr_entries:  nr_batch,
        };
        write_cmd(cmd_pa, &attach_hdr);

        // Write entries immediately after the header in the same page
        for (i, &pa) in batch.iter().enumerate() {
            let entry = GpuMemEntry {
                addr:    pa as u64,
                length:  PAGE_SIZE as u32,
                padding: 0,
            };
            let off = HDR_SIZE + i * ENTRY_SIZE;
            // SAFETY: cmd_pa is a PMM page we own; off < cmd_bytes <= PAGE_SIZE.
            unsafe {
                core::ptr::write_volatile(
                    (cmd_pa + off) as *mut GpuMemEntry,
                    entry,
                );
            }
        }

        clear_resp(resp_pa);
        unsafe {
            vq.send_recv_sync(
                cmd_pa, cmd_bytes as u32,
                resp_pa, core::mem::size_of::<GpuRespNodata>() as u32,
                base,
            );
        }
    }

    Some(())
}

/// Send SET_SCANOUT to map resource 1 to scanout 0.
fn gpu_set_scanout(
    vq: &mut SimpleVirtQ,
    base: usize,
    cmd_pa: usize,
    resp_pa: usize,
) -> Option<()> {
    let cmd = GpuSetScanout {
        hdr:         GpuCtrlHdr::new(VIRTIO_GPU_CMD_SET_SCANOUT),
        r:           GpuRect { x: 0, y: 0, width: FB_WIDTH, height: FB_HEIGHT },
        scanout_id:  0,
        resource_id: 1,
    };
    write_cmd(cmd_pa, &cmd);
    clear_resp(resp_pa);
    unsafe {
        vq.send_recv_sync(
            cmd_pa, core::mem::size_of::<GpuSetScanout>() as u32,
            resp_pa, core::mem::size_of::<GpuRespNodata>() as u32,
            base,
        );
    }
    Some(())
}

// ── Scratch-page helpers ──────────────────────────────────────────────────────

/// Write a `repr(C)` value to the beginning of a page at `pa`.
fn write_cmd<T: Sized>(pa: usize, val: &T) {
    let bytes = core::mem::size_of::<T>();
    debug_assert!(bytes <= PAGE_SIZE);
    // SAFETY: pa is a PMM page we own; bytes fits within it.
    unsafe {
        core::ptr::copy_nonoverlapping(
            val as *const T as *const u8,
            pa as *mut u8,
            bytes,
        );
    }
}

/// Zero the response page so stale data cannot be misread.
fn clear_resp(pa: usize) {
    // SAFETY: pa is a PMM page we own.
    unsafe { core::ptr::write_bytes(pa as *mut u8, 0, core::mem::size_of::<GpuRespNodata>()); }
}

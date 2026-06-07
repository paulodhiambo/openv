//! # VirtIO Virtqueue Implementation
//!
//! This module provides a VirtIO virtqueue implementation. Virtqueues
//! are the mechanism by which the driver and device exchange buffers.
//!
//! ## Overview
//!
//! A virtqueue consists of three parts:
//!
//! - **Descriptor table**: An array of descriptors, each describing a
//!    buffer (address, length, flags).
//!  - **Available ring**: A ring of descriptor indices that the driver
//!    has made available to the device.
//!  - **Used ring**: A ring of descriptor indices that the device has
//!    processed and returned to the driver.
//!
//! ## Memory Layout
//!
//! All three parts are packed into a single 4 KB page (per queue).
//! The layout is:
//!
//! ```text
//! Descriptor table: size * 16 bytes at offset 0
//! Available ring:   u16 flags, u16 idx, u16 ring[size], u16 used_event
//! Used ring:        u32 flags, u32 idx, UsedElem ring[size], u16 avail_event
//! ```
//!
//! ## Concurrency
//!
//! The virtqueue uses [`Mutex`] to protect the free list. Raw pointers
//! are used to reference the shared virtqueue memory, which is
//! intentionally shared with device hardware. Access is synchronized
//! by kernel-side locks and the memory region is stable for the
//! kernel lifetime.
//!
//! [`Mutex`]: ../../sync/struct.Mutex.html

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};
use crate::sync::Mutex;

/// Descriptor flag: NEXT. If set, the descriptor chains to the next descriptor.
const VIRTQ_DESC_F_NEXT: u16 = 1;
/// Descriptor flag: WRITE. If set, the buffer is writable (device writes to it).
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// A virtqueue descriptor.
///
/// # Fields
///
/// * `addr` - Physical address of the buffer.
/// * `len` - Length of the buffer in bytes.
/// * `flags` - Descriptor flags ([`VIRTQ_DESC_F_NEXT`], [`VIRTQ_DESC_F_WRITE`]).
/// * `next` - Index of the next descriptor in the chain (if `F_NEXT` is set).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Descriptor {
    /// Physical address of the buffer.
    pub addr: u64,
    /// Length of the buffer in bytes.
    pub len: u32,
    /// Descriptor flags.
    pub flags: u16,
    /// Index of the next descriptor in the chain.
    pub next: u16,
}

/// A used ring element, written by the device to signal completion.
///
/// # Fields
///
/// * `id` - Index of the descriptor that was used.
/// * `len` - Number of bytes written to the buffer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsedElem {
    /// Index of the descriptor that was used.
    pub id: u32,
    /// Number of bytes written to the buffer.
    pub len: u32,
}

/// A virtqueue.
///
/// # Fields
///
/// * `size` - The number of descriptors in the queue (power-of-two).
/// * `desc` - Pointer to the descriptor table.
/// * `avail_flags` - Pointer to the available ring flags.
/// * `avail_idx` - Pointer to the available ring index.
/// * `avail_ring` - Pointer to the available ring array.
/// * `used_flags` - Pointer to the used ring flags.
/// * `used_idx` - Pointer to the used ring index.
/// * `used_ring` - Pointer to the used ring array.
/// * `free_list` - List of free descriptor indices, protected by a [`Mutex`].
/// * `last_used_idx` - The last used index observed by the driver.
///
/// # Safety
///
/// `VirtQueue` is `Send` and `Sync` because all access to the shared
/// memory is synchronized by the contained mutex and the raw pointers
/// are valid for the kernel lifetime.
#[allow(dead_code)]
pub struct VirtQueue {
    /// The number of descriptors in the queue (power-of-two).
    pub size: usize,
    /// Pointer to the descriptor table.
    desc: *mut Descriptor,
    /// Pointer to the available ring flags.
    avail_flags: *mut u16,
    /// Pointer to the available ring index.
    avail_idx: *mut u16,
    /// Pointer to the available ring array.
    avail_ring: *mut u16,
    /// Pointer to the used ring flags.
    used_flags: *mut u16,
    /// Pointer to the used ring index.
    used_idx: *mut u16,
    /// Pointer to the used ring array.
    used_ring: *mut UsedElem,
    /// List of free descriptor indices, protected by a [`Mutex`].
    free_list: Mutex<Vec<u16>>,
    /// The last used index observed by the driver.
    last_used_idx: u32,
}

// Raw pointers are used to reference the shared virtqueue page which is
// intentionally shared with device hardware. Marking Send/Sync is unsafe but
// acceptable here because access is synchronized by kernel-side locks and the
// memory region is stable for kernel lifetime.
unsafe impl Send for VirtQueue {}
unsafe impl Sync for VirtQueue {}

impl VirtQueue {
    /// Initializes a virtqueue structure using a page allocated at `page_pa`.
    ///
    /// # Arguments
    ///
    /// * `page_pa` - The physical address of a 4 KB page to use for the queue.
/// * `size` - The number of descriptors. Should be a power of two and ≤ 32768.
    ///
    /// # Returns
    ///
    /// A new [`VirtQueue`] instance.
    pub fn new(page_pa: usize, size: usize) -> Self {
        // Layout per legacy virtio spec within a single page:
        // Descriptor table: size * 16 bytes at offset 0
        // Available ring: u16 flags, u16 idx, u16 ring[size], u16 used_event (optional)
        // Used ring: u32 flags, u32 idx, UsedElem ring[size], u16 avail_event (optional)
        let desc_ptr = page_pa as *mut Descriptor;
        let desc_table_bytes = size * core::mem::size_of::<Descriptor>();
        let avail_offset = (desc_table_bytes + 1) & !1; // align to 2
        let avail_ptr = (page_pa + avail_offset) as *mut u16;

        // avail header: flags(at avail_ptr[0]), idx(at avail_ptr[1])
        // SAFETY: `avail_ptr` is aligned to 2 bytes (avail_offset is 2-aligned).
        // Adding 2 u16 elements (flags + idx) stays within the allocated page.
        let avail_ring_ptr = unsafe { avail_ptr.add(2) };
        let avail_ring_bytes = size * core::mem::size_of::<u16>();
        let used_offset = avail_offset + 2 * 2 + avail_ring_bytes; // flags+idx (2*2 bytes) + ring
        // align used_offset to 4
        let used_offset = (used_offset + 3) & !3;
        // used ring: u16 flags + u16 idx (4 bytes total header), then UsedElem array
        let used_ptr = (page_pa + used_offset) as *mut u16;
        let used_ring_ptr = (page_pa + used_offset + 4) as *mut UsedElem;

        // Initialize free list
        let mut free = Vec::new();
        for i in 0..(size as u16) {
            free.push(i);
        }

        VirtQueue {
            size,
            desc: desc_ptr,
            avail_flags: avail_ptr,
            // SAFETY: `avail_ptr` points into a PMM-allocated page; adding 1
            // u16 (2 bytes) gives the `avail_idx` field, still within the page.
            avail_idx: unsafe { avail_ptr.add(1) },
            avail_ring: avail_ring_ptr,
            used_flags: used_ptr,
            // SAFETY: `used_ptr` points into the same PMM page; adding 1 u16
            // (2 bytes) gives the `used_idx` field, still within the page.
            used_idx: unsafe { used_ptr.add(1) }, // offset +2 (one u16)
            used_ring: used_ring_ptr,
            free_list: Mutex::new(free),
            last_used_idx: 0,
        }
    }

    /// Returns the descriptor's physical address for a given descriptor index.
    ///
    /// # Arguments
    ///
    /// * `idx` - The descriptor index.
    ///
    /// # Returns
    ///
    /// The physical address of the buffer described by the descriptor.
    pub fn desc_addr(&self, idx: u16) -> u64 {
        // SAFETY: `self.desc` points to the descriptor table of a PMM-allocated
        // virtqueue page. `idx` is always a valid free-list index (0..size).
        unsafe { (*self.desc.add(idx as usize)).addr }
    }

    /// Enqueues a chain of buffers.
    ///
    /// # Arguments
    ///
    /// * `buffers` - A slice of `(physical_address, length, write)` tuples.
    ///   `write` is `true` for device-writable buffers (RX) and `false` for
    ///   device-readable buffers (TX).
    ///
    /// # Returns
    ///
    /// `Ok(head_descriptor_index)` on success, or `Err(())` if there
    /// are not enough free descriptors.
    pub fn enqueue_chain(&self, buffers: &[(u64, u32, bool)]) -> Result<u16, ()> {
        let mut free = self.free_list.lock();
        if free.len() < buffers.len() {
            return Err(());
        }

        let mut ids = alloc::vec::Vec::with_capacity(buffers.len());
        for _ in 0..buffers.len() {
            // INVARIANT: we already verified free.len() >= buffers.len() above.
            ids.push(free.pop().unwrap_or(0));
        }

        // Populate descriptors
        for (i, &(pa, len, write)) in buffers.iter().enumerate() {
            let idx = ids[i] as usize;
            // SAFETY: `idx` was just popped from the free-list and is within
            // [0, size), so `self.desc.add(idx)` is a valid Descriptor slot.
            unsafe {
                let d = &mut *self.desc.add(idx);
                d.addr = pa;
                d.len = len;
                d.flags = if write { VIRTQ_DESC_F_WRITE } else { 0 };
                if i + 1 < buffers.len() {
                    d.flags |= VIRTQ_DESC_F_NEXT;
                    d.next = ids[i + 1];
                } else {
                    d.next = 0;
                }
            }
        }

        // Place head index into avail ring
        // SAFETY: `self.avail_idx` and `self.avail_ring` point into the
        // virtqueue page. All accesses are volatile so the device's DMA view
        // stays consistent. ring_pos is always < self.size.
        let avail_i = unsafe { read_volatile(self.avail_idx) } as usize;
        let ring_pos = avail_i % self.size;
        unsafe {
            write_volatile(self.avail_ring.add(ring_pos), ids[0]);
            // Ensure descriptor writes are visible before idx update
            fence(Ordering::Release);
            write_volatile(self.avail_idx, (avail_i as u16).wrapping_add(1));
        }

        Ok(ids[0])
    }

    /// Convenience single-buffer enqueue kept for backward compatibility.
    ///
    /// # Arguments
    ///
    /// * `buf_pa` - Physical address of the buffer.
/// * `len` - Length of the buffer.
/// * `write` - Whether the buffer is device-writable.
    ///
    /// # Returns
    ///
    /// `Ok(head_descriptor_index)` on success, or `Err(())` if there
    /// are not enough free descriptors.
    pub fn enqueue(&self, buf_pa: u64, len: u32, write: bool) -> Result<u16, ()> {
        self.enqueue_chain(&[(buf_pa, len, write)])
    }

    /// Checks the used ring and pops any completed entries.
    ///
    /// Note: this does NOT free descriptor chain entries — the caller
    /// should call [`free_chain`] to return descriptors to the free
    /// list after handling buffer contents.
    ///
    /// # Returns
    ///
    /// A vector of `(id, len)` tuples for each completed entry.
    ///
    /// [`free_chain`]: #method.free_chain
    pub fn pop_used(&mut self) -> Vec<(u16, u32)> {
        // Ensure we observe device writes
        fence(Ordering::Acquire);
        let mut out = Vec::new();
        // used_idx is u16; compare as u16 to handle wrapping at 65536.
        // SAFETY: `self.used_idx` points into the virtqueue page; volatile read
        // ensures we observe the device's DMA write of the completion index.
        let used_i = unsafe { read_volatile(self.used_idx) };
        while self.last_used_idx as u16 != used_i {
            let idx = (self.last_used_idx as usize) % self.size;
            // SAFETY: `idx` is within [0, size), and used_ring points to the
            // used-element array in the virtqueue page.
            let ue = unsafe { &*self.used_ring.add(idx) };
            out.push((ue.id as u16, ue.len));
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
        }
        out
    }

    /// Walks a descriptor chain starting at `start` and returns the list of
    /// `(physical_address, length)` pairs for each buffer in the chain.
    ///
    /// # Arguments
    ///
    /// * `start` - The head descriptor index of the chain.
    ///
    /// # Returns
    ///
    /// A vector of `(physical_address, length)` tuples.
    pub fn read_chain(&self, start: u16) -> Vec<(u64, u32)> {
        let mut out = Vec::new();
        let mut cur = start;
        loop {
            // SAFETY: `cur` is a valid descriptor index within [0, size);
            // read_volatile ensures we see the device's last DMA write.
            let d = unsafe { read_volatile(self.desc.add(cur as usize)) };
            out.push((d.addr, d.len));
            if (d.flags & VIRTQ_DESC_F_NEXT) == 0 {
                break;
            }
            cur = d.next;
        }
        out
    }

    /// Frees all descriptors in the chain starting at `start` and returns the
    /// list of freed indices.
    ///
    /// # Arguments
    ///
    /// * `start` - The head descriptor index of the chain.
    ///
    /// # Returns
    ///
    /// A vector of freed descriptor indices.
    pub fn free_chain(&self, start: u16) -> Vec<u16> {
        let mut freed = Vec::new();
        let mut cur = start;
        loop {
            // SAFETY: `cur` is a valid descriptor index within [0, size);
            // read_volatile ensures we see the device's last DMA write.
            let d = unsafe { read_volatile(self.desc.add(cur as usize)) };
            freed.push(cur);
            if (d.flags & VIRTQ_DESC_F_NEXT) == 0 {
                break;
            }
            cur = d.next;
        }
        // Return indices to free list
        let mut free = self.free_list.lock();
        for id in &freed {
            free.push(*id);
        }
        freed
    }
}

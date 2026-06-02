use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};
use alloc::vec::Vec;
use spin::Mutex;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsedElem {
    pub id: u32,
    pub len: u32,
}

pub struct VirtQueue {
    pub size: usize,
    desc: *mut Descriptor,
    avail_flags: *mut u16,
    avail_idx: *mut u16,
    avail_ring: *mut u16,
    used_flags: *mut u32,
    used_idx: *mut u32,
    used_ring: *mut UsedElem,
    free_list: Mutex<Vec<u16>>,
    last_used_idx: u32,
}

// Raw pointers are used to reference the shared virtqueue page which is
// intentionally shared with device hardware. Marking Send/Sync is unsafe but
// acceptable here because access is synchronized by kernel-side locks and the
// memory region is stable for kernel lifetime.
unsafe impl Send for VirtQueue {}
unsafe impl Sync for VirtQueue {}

impl VirtQueue {
    /// Initialize a VirtQueue structure using a page allocated at `page_pa`.
    /// `size` should be power-of-two and <= 32768.
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
        let avail_ring_ptr = unsafe { avail_ptr.add(2) };
        let avail_ring_bytes = size * core::mem::size_of::<u16>();
        let used_offset = avail_offset + 2 * 2 + avail_ring_bytes; // flags+idx (2*2 bytes) + ring
        // align used_offset to 4
        let used_offset = (used_offset + 3) & !3;
        let used_ptr = (page_pa + used_offset) as *mut u32; // flags and idx are u32 each
        let used_ring_ptr = unsafe { (page_pa + used_offset + 8) as *mut UsedElem };

        // Initialize free list
        let mut free = Vec::new();
        for i in 0..(size as u16) {
            free.push(i);
        }

        VirtQueue {
            size,
            desc: desc_ptr,
            avail_flags: avail_ptr,
            avail_idx: unsafe { avail_ptr.add(1) },
            avail_ring: avail_ring_ptr,
            used_flags: used_ptr,
            used_idx: unsafe { used_ptr.add(1) },
            used_ring: used_ring_ptr,
            free_list: Mutex::new(free),
            last_used_idx: 0,
        }
    }

    /// Return the descriptor's physical address for a given descriptor index.
    pub fn desc_addr(&self, idx: u16) -> u64 {
        unsafe { (*self.desc.add(idx as usize)).addr }
    }

    /// Enqueue a chain of buffers. Buffers is a slice of (pa, len, write).
    /// Returns head descriptor index on success.
    pub fn enqueue_chain(&self, buffers: &[(u64, u32, bool)]) -> Result<u16, ()> {
        let mut free = self.free_list.lock();
        if free.len() < buffers.len() {
            return Err(());
        }

        // Allocate descriptor indices
        let mut ids: Vec<u16> = Vec::new();
        for _ in 0..buffers.len() {
            ids.push(free.pop().unwrap());
        }

        // Populate descriptors
        for (i, &(pa, len, write)) in buffers.iter().enumerate() {
            let idx = ids[i] as usize;
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

    /// Convenience single-buffer enqueue kept for backward compat.
    pub fn enqueue(&self, buf_pa: u64, len: u32, write: bool) -> Result<u16, ()> {
        self.enqueue_chain(&[(buf_pa, len, write)])
    }

    /// Check used ring and pop any completed entries; returns list of (id,len)
    /// Note: this does NOT free descriptor chain entries — caller should call
    /// `free_chain` to return descriptors to the free list after handling buffer
    /// contents.
    pub fn pop_used(&mut self) -> Vec<(u16, u32)> {
        // Ensure we observe device writes
        fence(Ordering::Acquire);
        let mut out = Vec::new();
        let used_i = unsafe { read_volatile(self.used_idx) };
        while self.last_used_idx as u32 != used_i {
            let idx = (self.last_used_idx as usize) % self.size;
            let ue = unsafe { &*self.used_ring.add(idx) };
            out.push((ue.id as u16, ue.len));
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
        }
        out
    }

    /// Walk a descriptor chain starting at `start` and return list of descriptor
    /// indices in the chain. Also returns vector of (pa,len) for buffers.
    pub fn read_chain(&self, start: u16) -> Vec<(u64, u32)> {
        let mut out = Vec::new();
        let mut cur = start;
        loop {
            let d = unsafe { read_volatile(self.desc.add(cur as usize)) };
            out.push((d.addr, d.len));
            if (d.flags & VIRTQ_DESC_F_NEXT) == 0 {
                break;
            }
            cur = d.next;
        }
        out
    }

    /// Free all descriptors in the chain starting at `start` and return the
    /// list of freed indices.
    pub fn free_chain(&self, start: u16) -> Vec<u16> {
        let mut freed = Vec::new();
        let mut cur = start;
        loop {
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

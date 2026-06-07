#![no_std]
#![no_main]

extern crate alloc;
use alloc::format;
use core::alloc::Layout;
use libos::{blk_register, phys_map, virt_to_phys, irq_register, irq_enable, getpid, msg_receive, write};
use libos::ipc::Message;

fn print(s: &str) {
    write(1, s.as_ptr(), s.len());
}

macro_rules! my_println {
    () => (print("\n"));
    ($fmt:expr) => (print(&format!(concat!($fmt, "\n"))));
    ($fmt:expr, $($arg:tt)*) => (print(&format!(concat!($fmt, "\n"), $($arg)*)));
}

mod virtqueue;
use virtqueue::VirtQueue;
use spin::Mutex;

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
const OFF_ISR: usize = 0x060;
const OFF_ACK: usize = 0x064;
const OFF_STATUS: usize = 0x070;

const VIRTIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_BLK_DEVICE_ID: u32 = 2;

const VIRTIO_BLK_T_IN: u32 = 0;  // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write
const VIRTIO_BLK_S_OK: u8 = 0;

const BLOCK_SIZE: usize = 4096;
const SECTOR_SIZE: usize = 512;
const SECTORS_PER_BLOCK: u64 = (BLOCK_SIZE / SECTOR_SIZE) as u64;
const VIRTIO_BLK_MMIO_VA: usize = 0x300000000; // Above 4GB kernel map

#[repr(C)]
struct BlkReqHeader {
    blk_type: u32,
    reserved: u32,
    sector: u64,
}

struct VirtioBlk {
    base: usize,
    vq: Mutex<VirtQueue>,
    irq: u32,
}

impl VirtioBlk {
    fn mmio_r32(base: usize, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((base + off) as *const u32) }
    }
    fn mmio_w32(base: usize, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile((base + off) as *mut u32, v) }
    }

    fn do_rw(&self, sector: u64, buf_ptr: *mut u8, buf_len: usize, is_read: bool) -> bool {
        // Allocate physically contiguous memory for the header and status byte
        let hdr_ptr = unsafe { alloc::alloc::alloc_zeroed(Layout::from_size_align(4096, 4096).unwrap()) };
        let stat_ptr = unsafe { alloc::alloc::alloc_zeroed(Layout::from_size_align(4096, 4096).unwrap()) };
        
        let hdr_pa = virt_to_phys(hdr_ptr as usize).unwrap() as u64;
        let stat_pa = virt_to_phys(stat_ptr as usize).unwrap() as u64;
        let buf_pa = virt_to_phys(buf_ptr as usize).unwrap() as u64;

        unsafe {
            let hdr = hdr_ptr as *mut BlkReqHeader;
            (*hdr).blk_type = if is_read { VIRTIO_BLK_T_IN } else { VIRTIO_BLK_T_OUT };
            (*hdr).reserved = 0;
            (*hdr).sector = sector;
            *stat_ptr = 0xFF;
        }

        let chain: &[(u64, u32, bool)] = &[
            (hdr_pa, core::mem::size_of::<BlkReqHeader>() as u32, false),
            (buf_pa, buf_len as u32, is_read),
            (stat_pa, 1, true),
        ];

        let head_id = {
            let vq = self.vq.lock();
            match vq.enqueue_chain(chain) {
                Ok(id) => id,
                Err(_) => {
                    unsafe {
                        alloc::alloc::dealloc(hdr_ptr, Layout::from_size_align(4096, 4096).unwrap());
                        alloc::alloc::dealloc(stat_ptr, Layout::from_size_align(4096, 4096).unwrap());
                    }
                    return false;
                }
            }
        };

        Self::mmio_w32(self.base, OFF_QUEUE_NOTIFY, 0);

        // Instead of polling, we should ideally block and wait for interrupt IPC message.
        // But `do_rw` is called sequentially right now. The driver loop will receive the interrupt,
        // so maybe `do_rw` should just be an async/message based thing.
        // For simplicity, we can do a hybrid: wait for IPC message.
        let mut found = false;
        loop {
            let mut msg = Message::new();
            msg_receive(-1, &mut msg); // Block until we get a message (interrupt or other)
            
            if msg.type_ == -2 { // HARDWARE_INTERRUPT
                let irq = msg.data[0] as u32;
                my_println!("virtio-blk-driver: do_rw got interrupt irq={}", irq);
                if irq == self.irq {
                    // Read and clear the device interrupt status
                    let isr = Self::mmio_r32(self.base, OFF_ISR);
                    Self::mmio_w32(self.base, OFF_ACK, isr);
                    my_println!("virtio-blk-driver: cleared device ISR = {:#x}", isr);

                    let mut vq = self.vq.lock();
                    let popped = vq.pop_used();
                    my_println!("virtio-blk-driver: popped {} used entries, last_used_idx={}, used_idx={}", popped.len(), vq.last_used_idx, unsafe { core::ptr::read_volatile(vq.used_idx) });
                    for (id, len) in popped {
                        my_println!("  popped id={}, len={}", id, len);
                        vq.free_chain(id);
                        if id == head_id {
                            found = true;
                        }
                    }
                    irq_enable(self.irq); // Ack the interrupt at PLIC level
                    if found {
                        break;
                    }
                }
            } else {
                my_println!("virtio-blk-driver: do_rw got unexpected msg type={} source={}", msg.type_, msg.source);
            }
        }

        let ok_status = unsafe { *(stat_ptr as *const u8) };
        unsafe {
            alloc::alloc::dealloc(hdr_ptr, Layout::from_size_align(4096, 4096).unwrap());
            alloc::alloc::dealloc(stat_ptr, Layout::from_size_align(4096, 4096).unwrap());
        }
        
        ok_status == VIRTIO_BLK_S_OK
    }

    #[allow(dead_code)]
    pub fn read_block(&self, blk: u32, buf: &mut [u8; BLOCK_SIZE]) -> bool {
        self.do_rw(blk as u64 * SECTORS_PER_BLOCK, buf.as_mut_ptr(), BLOCK_SIZE, true)
    }

    #[allow(dead_code)]
    pub fn write_block(&self, blk: u32, buf: &[u8; BLOCK_SIZE]) -> bool {
        self.do_rw(
            blk as u64 * SECTORS_PER_BLOCK,
            buf.as_ptr() as *mut u8,
            BLOCK_SIZE,
            false,
        )
    }
}

fn probe_driver() -> Option<VirtioBlk> {
    let mut found_pa = 0;
    let mut found_irq = 0;
    let mut found_base = 0;

    for i in 1..=8 {
        let pa = 0x10000000 + i * 0x1000;
        let base = VIRTIO_BLK_MMIO_VA + i * 0x1000;
        let map_res = phys_map(base, pa, 4096);
        if map_res == usize::MAX {
            my_println!("probe: slot {} PA={:#x} phys_map failed", i, pa);
            continue;
        }
        let magic = VirtioBlk::mmio_r32(base, OFF_MAGIC);
        let device_id = VirtioBlk::mmio_r32(base, OFF_DEVICE_ID);
        my_println!("probe: slot {} PA={:#x} magic={:#x} device_id={}", i, pa, magic, device_id);
        if magic != VIRTIO_MAGIC { continue; }
        if device_id == VIRTIO_BLK_DEVICE_ID {
            found_pa = pa;
            found_irq = i as u32;
            found_base = base;
            break;
        }
    }

    if found_pa == 0 {
        my_println!("virtio-blk-driver: no virtio-blk device in any of the 8 MMIO slots");
        return None;
    }
    let base = found_base;

    my_println!("virtio-blk-driver: found block device at PA={:#x}, IRQ={}", found_pa, found_irq);

    VirtioBlk::mmio_w32(base, OFF_STATUS, 0); // reset
    VirtioBlk::mmio_w32(base, OFF_STATUS, 1); // ACKNOWLEDGE
    VirtioBlk::mmio_w32(base, OFF_STATUS, 3); // ACKNOWLEDGE | DRIVER
    let _features = VirtioBlk::mmio_r32(base, OFF_DEVICE_FEATURES);
    VirtioBlk::mmio_w32(base, OFF_DRIVER_FEATURES, 0);

    VirtioBlk::mmio_w32(base, OFF_QUEUE_SEL, 0);
    let qnum = VirtioBlk::mmio_r32(base, OFF_QUEUE_NUM_MAX);
    let qsize = if qnum == 0 { 16 } else { core::cmp::min(qnum, 16) } as usize;
    VirtioBlk::mmio_w32(base, OFF_QUEUE_NUM, qsize as u32);
    VirtioBlk::mmio_w32(base, OFF_QUEUE_ALIGN, 4);

    let q_ptr = unsafe { alloc::alloc::alloc_zeroed(Layout::from_size_align(4096, 4096).unwrap()) };
    let q_pa = virt_to_phys(q_ptr as usize).unwrap();

    VirtioBlk::mmio_w32(base, OFF_GUEST_PAGE_SIZE, 4096);
    VirtioBlk::mmio_w32(base, OFF_QUEUE_PFN, (q_pa >> 12) as u32);
    VirtioBlk::mmio_w32(base, OFF_STATUS, 7); // ACK | DRIVER | DRIVER_OK

    let drv = VirtioBlk {
        base,
        vq: Mutex::new(VirtQueue::new(q_ptr as usize, qsize)),
        irq: found_irq,
    };

    my_println!("virtio-blk-driver: initialized, qsize={}, irq={}", qsize, found_irq);
    Some(drv)
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    my_println!("virtio-blk-driver: starting");

    let drv = match probe_driver() {
        Some(d) => d,
        None => {
            my_println!("virtio-blk-driver: no device found, exiting");
            return 0;
        }
    };

    // Register for interrupts
    irq_register(drv.irq, getpid());
    irq_enable(drv.irq);

    // Announce this process as the block driver so vfs-server can find us
    // without relying on a hardcoded PID.
    blk_register();

    my_println!("virtio-blk-driver: initialized, irq={}, my pid={}", drv.irq, getpid());

    let bounce_buffer = unsafe { alloc::alloc::alloc_zeroed(Layout::from_size_align(4096, 4096).unwrap()) };

    loop {
        let mut msg = Message::new();
        msg_receive(-1, &mut msg);
        if msg.type_ == -2 {
            let irq = msg.data[0] as u32;
            if irq == drv.irq {
                let isr = VirtioBlk::mmio_r32(drv.base, OFF_ISR);
                VirtioBlk::mmio_w32(drv.base, OFF_ACK, isr);
                irq_enable(drv.irq); // Spurious or unhandled interrupt
            }
        } else if msg.type_ == 100 { // OP_BLOCK_READ
            let blk = u32::from_le_bytes(msg.data[0..4].try_into().unwrap());
            let req_pid = msg.source;
            let req_va = usize::from_le_bytes(msg.data[4..12].try_into().unwrap());
            my_println!("virtio-blk-driver: got OP_BLOCK_READ blk={} req_va={:#x} req_pid={}", blk, req_va, req_pid);
            let ok = drv.do_rw(blk as u64 * SECTORS_PER_BLOCK, bounce_buffer, 4096, true);
            my_println!("virtio-blk-driver: do_rw returned ok={}", ok);
            if ok {
                let res = libos::datacopy(getpid(), bounce_buffer, req_pid, req_va as *mut u8, 4096);
                my_println!("virtio-blk-driver: datacopy res={}", res);
                let mut reply = Message::new();
                reply.type_ = 0; // OK
                libos::msg_send(req_pid, &reply);
            } else {
                let mut reply = Message::new();
                reply.type_ = -1; // ERR
                libos::msg_send(req_pid, &reply);
            }
        } else if msg.type_ == 101 { // OP_BLOCK_WRITE
            let blk = u32::from_le_bytes(msg.data[0..4].try_into().unwrap());
            let req_pid = msg.source;
            let req_va = usize::from_le_bytes(msg.data[4..12].try_into().unwrap());

            libos::datacopy(req_pid, req_va as *const u8, getpid(), bounce_buffer, 4096);
            let ok = drv.do_rw(blk as u64 * SECTORS_PER_BLOCK, bounce_buffer, 4096, false);
            if ok {
                let mut reply = Message::new();
                reply.type_ = 0; // OK
                libos::msg_send(req_pid, &reply);
            } else {
                let mut reply = Message::new();
                reply.type_ = -1; // ERR
                libos::msg_send(req_pid, &reply);
            }
        }
    }
}

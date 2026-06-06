#![no_std]
#![no_main]
extern crate alloc;

use libos::{spawn_with_caps, sys_yield, waitpid, write, CAP_MMIO, CAP_INTERRUPT, CAP_DATACOPY, CAP_NET_RAW};

fn wrt(s: &[u8]) {
    write(1, s.as_ptr(), s.len());
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    wrt(b"rs-server: starting\n");

    let mut pids = alloc::collections::BTreeMap::new();

    let net_caps = CAP_NET_RAW | CAP_INTERRUPT | CAP_DATACOPY;
    let pid = spawn_with_caps(b"/net-smoltcp".as_ptr(), b"/net-smoltcp".len(), net_caps);
    if pid > 0 { pids.insert(pid, (b"/net-smoltcp".as_slice(), net_caps)); }

    let blk_caps = CAP_MMIO | CAP_INTERRUPT | CAP_DATACOPY;
    let pid = spawn_with_caps(b"/virtio-blk-driver".as_ptr(), b"/virtio-blk-driver".len(), blk_caps);
    if pid > 0 { pids.insert(pid, (b"/virtio-blk-driver".as_slice(), blk_caps)); }

    loop {
        let mut status: i32 = 0;
        let reaped = waitpid(-1, &mut status as *mut i32, 0);
        if reaped > 0 {
            if let Some((driver_path, caps)) = pids.remove(&reaped) {
                if status != 0 {
                    // Non-zero exit = crash/error — restart the driver.
                    wrt(b"rs-server: a driver crashed! Restarting...\n");
                    let pid = spawn_with_caps(driver_path.as_ptr(), driver_path.len(), caps);
                    if pid > 0 { pids.insert(pid, (driver_path, caps)); }
                } else {
                    wrt(b"rs-server: a driver exited cleanly (no device?), not restarting.\n");
                }
            }
        } else {
            sys_yield();
        }
    }
}

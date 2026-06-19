#![no_std]
#![no_main]
extern crate alloc;

use alloc::collections::BTreeMap;
use libos::{spawn_with_caps, sys_yield, waitpid, write, ipc_recv, ipc_send,
            CAP_MMIO, CAP_INTERRUPT, CAP_DATACOPY, CAP_NET_RAW};
use driver_abi::{DriverDesc, OP_DRIVER_REGISTER, REPLY_DRIVER_OK, REPLY_DRIVER_ERR};

fn wrt(s: &[u8]) {
    write(1, s.as_ptr(), s.len());
}

/// Block until the driver identified by `driver_pid` sends an OP_DRIVER_REGISTER
/// message.  Validates the descriptor and replies REPLY_DRIVER_OK or
/// REPLY_DRIVER_ERR.  Returns `true` when the driver registered successfully
/// with non-zero required_caps (i.e. a real device was found).
fn await_registration(driver_pid: i32) -> bool {
    let mut buf = [0u8; 1024];
    let mut from: i32 = 0;
    let n = ipc_recv(&mut buf, &mut from);

    if n < 4 {
        let err = (REPLY_DRIVER_ERR as i32).to_le_bytes();
        ipc_send(from, &err);
        return false;
    }

    // Check opcode in first 4 bytes of the serialized payload prefix.
    // The driver prepends the opcode before the DriverDesc bytes.
    let opcode = i32::from_le_bytes(buf[0..4].try_into().unwrap());
    if opcode != OP_DRIVER_REGISTER {
        wrt(b"rs-server: unexpected opcode from driver\n");
        let err = (REPLY_DRIVER_ERR as i32).to_le_bytes();
        ipc_send(from, &err);
        return false;
    }

    let desc = match DriverDesc::deserialize(&buf[4..n]) {
        Some(d) => d,
        None => {
            wrt(b"rs-server: malformed DriverDesc from driver\n");
            let err = (REPLY_DRIVER_ERR as i32).to_le_bytes();
            ipc_send(driver_pid, &err);
            return false;
        }
    };

    wrt(b"rs-server: registered driver: ");
    wrt(desc.name.as_bytes());
    wrt(b"\n");

    let ok = (REPLY_DRIVER_OK as i32).to_le_bytes();
    ipc_send(driver_pid, &ok);

    // required_caps == 0 means the driver found no device and will exit cleanly
    desc.required_caps != 0
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    wrt(b"rs-server: starting\n");

    let mut pids: BTreeMap<i32, (&[u8], u64)> = BTreeMap::new();

    let net_caps = CAP_NET_RAW | CAP_INTERRUPT | CAP_DATACOPY;
    let pid = spawn_with_caps(b"/net-smoltcp".as_ptr(), b"/net-smoltcp".len(), net_caps);
    if pid > 0 {
        pids.insert(pid, (b"/net-smoltcp", net_caps));
        await_registration(pid);
    }

    let blk_caps = CAP_MMIO | CAP_INTERRUPT | CAP_DATACOPY;
    let pid = spawn_with_caps(b"/virtio-blk-driver".as_ptr(), b"/virtio-blk-driver".len(), blk_caps);
    if pid > 0 {
        pids.insert(pid, (b"/virtio-blk-driver", blk_caps));
        await_registration(pid);
    }

    loop {
        let mut status: i32 = 0;
        let reaped = waitpid(-1, &mut status as *mut i32, 0);
        if reaped > 0 {
            if let Some((driver_path, caps)) = pids.remove(&reaped) {
                if status != 0 {
                    wrt(b"rs-server: a driver crashed! Restarting...\n");
                    let pid = spawn_with_caps(driver_path.as_ptr(), driver_path.len(), caps);
                    if pid > 0 {
                        pids.insert(pid, (driver_path, caps));
                        await_registration(pid);
                    }
                } else {
                    wrt(b"rs-server: driver exited cleanly (no device?), not restarting.\n");
                }
            }
        } else {
            sys_yield();
        }
    }
}

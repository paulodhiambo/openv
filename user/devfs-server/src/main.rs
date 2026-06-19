//! devfs-server — serves /dev filesystem requests forwarded by vfs-server.
//!
//! Registers with the kernel via syscall 116 (SYS_DEV_SERVER_REGISTER).
//! Handles /dev/null (EOF on read, discard on write), /dev/zero (zero bytes),
//! and /dev/tty (stub; reads return 0 bytes).

#![no_std]
#![no_main]

extern crate alloc;
extern crate libos;

use libos::ipc::Message;

// Opcodes (must match vfs-server constants)
const OP_DEV_LIST: i32 = 1;
const OP_DEV_READ: i32 = 2;
const OP_DEV_WRITE: i32 = 3;
const OP_DEV_STAT: i32 = 4;
const REPLY_OK: i32 = 100;
const REPLY_ERR: i32 = 101;

fn register() {
    libos::syscall(116, 0, 0, 0); // SYS_DEV_SERVER_REGISTER
}

#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: usize, _argv: usize) -> i32 {
    register();

    let mut msg = Message::new();
    loop {
        let sender = libos::msg_receive(-1, &mut msg);
        if sender < 0 {
            continue;
        }

        match msg.type_ {
            OP_DEV_LIST => {
                // Return NUL-separated list: "null\0zero\0tty\0"
                let buf_ptr = u64::from_le_bytes(msg.data[0..8].try_into().unwrap_or([0; 8])) as usize;
                let max_len = u32::from_le_bytes(msg.data[8..12].try_into().unwrap_or([0; 4])) as usize;
                let listing = b"null\0zero\0tty\0";
                let n = listing.len().min(max_len);
                let mut reply = Message::new();
                if buf_ptr != 0 {
                    let r = libos::datacopy(
                        libos::getpid(), listing.as_ptr(),
                        sender, buf_ptr as *mut u8, n,
                    );
                    if r >= 0 {
                        reply.type_ = REPLY_OK;
                        reply.data[0..4].copy_from_slice(&(n as u32).to_le_bytes());
                    } else {
                        reply.type_ = REPLY_ERR;
                    }
                } else {
                    reply.type_ = REPLY_OK;
                    reply.data[0..4].copy_from_slice(&(n as u32).to_le_bytes());
                }
                libos::msg_send(sender, &reply);
            }

            OP_DEV_READ => {
                // data[0]: device id (0=null, 1=zero, 2=tty)
                // data[4..8]: offset (u32, ignored)
                // data[8..12]: max_len (u32)
                // data[12..20]: buf_ptr (u64 in caller's addr space)
                let dev = msg.data[0];
                let max_len = u32::from_le_bytes(msg.data[8..12].try_into().unwrap_or([0; 4])) as usize;
                let buf_ptr = u64::from_le_bytes(msg.data[12..20].try_into().unwrap_or([0; 8])) as usize;

                let mut reply = Message::new();
                match dev {
                    0 => {
                        // /dev/null — always EOF
                        reply.type_ = REPLY_OK;
                        reply.data[0..4].copy_from_slice(&0u32.to_le_bytes());
                    }
                    1 => {
                        // /dev/zero — return zero bytes
                        let n = max_len.min(4096);
                        let zeros = alloc::vec![0u8; n];
                        let r = libos::datacopy(
                            libos::getpid(), zeros.as_ptr(),
                            sender, buf_ptr as *mut u8, n,
                        );
                        if r >= 0 {
                            reply.type_ = REPLY_OK;
                            reply.data[0..4].copy_from_slice(&(n as u32).to_le_bytes());
                        } else {
                            reply.type_ = REPLY_ERR;
                        }
                    }
                    2 => {
                        // /dev/tty stub — return 0 bytes
                        reply.type_ = REPLY_OK;
                        reply.data[0..4].copy_from_slice(&0u32.to_le_bytes());
                    }
                    _ => {
                        reply.type_ = REPLY_ERR;
                    }
                }
                libos::msg_send(sender, &reply);
            }

            OP_DEV_WRITE => {
                // data[0]: device id
                // data[4..8]: len (u32)
                // All writes succeed (null/zero/tty discard data)
                let mut reply = Message::new();
                reply.type_ = REPLY_OK;
                reply.data[0..4].copy_from_slice(&msg.data[4..8]); // echo len back
                libos::msg_send(sender, &reply);
            }

            OP_DEV_STAT => {
                // data[0]: device id (0xFF = stat /dev itself)
                let dev = msg.data[0];
                let mut reply = Message::new();
                if dev == 0xFF {
                    // /dev itself is a directory
                    reply.type_ = REPLY_OK;
                    reply.data[0] = 1; // is_dir = true
                } else if dev <= 2 {
                    // Known device: regular file, size = 0
                    reply.type_ = REPLY_OK;
                    reply.data[0] = 0; // is_dir = false
                    reply.data[4..8].copy_from_slice(&0u32.to_le_bytes()); // size
                } else {
                    reply.type_ = REPLY_ERR;
                }
                libos::msg_send(sender, &reply);
            }

            _ => {
                let mut reply = Message::new();
                reply.type_ = REPLY_ERR;
                libos::msg_send(sender, &reply);
            }
        }
    }
}

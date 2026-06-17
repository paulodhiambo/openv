//! component-manager — routes capabilities between components.
//!
//! Registers with the kernel via SYS_CM_REGISTER (syscall 163).
//! Components register a name; clients look up components by name
//! to obtain their PID and open a direct IPC channel.
//!
//! # Protocol (IPC Message)
//!
//! Each request is a [`libos::ipc::Message`] with `type_` set to an opcode:
//!
//! | OP | type_ | data layout |
//! |----|-------|-------------|
//! | REGISTER | 1 | `[name_len: u32][name: bytes]` |
//! | LOOKUP   | 2 | `[name_len: u32][name: bytes]` |
//! | LIST     | 3 | `[buf_ptr: u64][buf_len: u32]` |
//! | UNREGISTER | 4 | `[name_len: u32][name: bytes]` |
//!
//! Replies use `type_ = REPLY_OK (100)` or `REPLY_ERR (101)`.
//! LOOKUP reply packs the target PID in `data[0..4]`.
//! LIST reply packs count of bytes written in `data[0..4]`.

#![no_std]
#![no_main]

extern crate alloc;
extern crate libos;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use libos::ipc::Message;

const OP_REGISTER:   i32 = 1;
const OP_LOOKUP:     i32 = 2;
const OP_LIST:       i32 = 3;
const OP_UNREGISTER: i32 = 4;
const REPLY_OK:  i32 = 100;
const REPLY_ERR: i32 = 101;

fn cm_register() {
    libos::syscall(163, 0, 0, 0); // SYS_CM_REGISTER
}

fn extract_name(data: &[u8]) -> Option<String> {
    if data.len() < 4 { return None; }
    let name_len = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    if name_len == 0 || 4 + name_len > data.len() { return None; }
    let name_bytes = &data[4..4 + name_len];
    core::str::from_utf8(name_bytes).ok().map(|s| String::from(s))
}

#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: usize, _argv: usize) -> i32 {
    cm_register();

    // component name → PID
    let mut registry: BTreeMap<String, i32> = BTreeMap::new();
    let mut msg = Message::new();

    loop {
        let sender = libos::msg_receive(-1, &mut msg);
        if sender < 0 { continue; }

        let mut reply = Message::new();

        match msg.type_ {
            OP_REGISTER => {
                match extract_name(&msg.data) {
                    Some(name) => {
                        registry.insert(name, sender);
                        reply.type_ = REPLY_OK;
                    }
                    None => { reply.type_ = REPLY_ERR; }
                }
            }

            OP_LOOKUP => {
                match extract_name(&msg.data) {
                    Some(name) => match registry.get(&name) {
                        Some(&pid) => {
                            reply.type_ = REPLY_OK;
                            reply.data[0..4].copy_from_slice(&pid.to_le_bytes());
                        }
                        None => { reply.type_ = REPLY_ERR; }
                    },
                    None => { reply.type_ = REPLY_ERR; }
                }
            }

            OP_LIST => {
                if msg.data.len() < 12 {
                    reply.type_ = REPLY_ERR;
                } else {
                    let buf_ptr = u64::from_le_bytes(msg.data[0..8].try_into().unwrap_or([0;8])) as usize;
                    let buf_len = u32::from_le_bytes(msg.data[8..12].try_into().unwrap_or([0;4])) as usize;

                    let mut listing: Vec<u8> = Vec::new();
                    for (name, pid) in registry.iter() {
                        let entry = alloc::format!("{}:{}\n", name, pid);
                        listing.extend_from_slice(entry.as_bytes());
                    }

                    let copy_len = listing.len().min(buf_len);
                    let wrote = if buf_ptr != 0 && copy_len > 0 {
                        libos::datacopy(libos::getpid(), listing.as_ptr(), sender, buf_ptr as *mut u8, copy_len)
                    } else {
                        copy_len as isize
                    };

                    if wrote >= 0 {
                        reply.type_ = REPLY_OK;
                        reply.data[0..4].copy_from_slice(&(copy_len as u32).to_le_bytes());
                    } else {
                        reply.type_ = REPLY_ERR;
                    }
                }
            }

            OP_UNREGISTER => {
                match extract_name(&msg.data) {
                    Some(name) => {
                        // Only allow a component to unregister itself.
                        if registry.get(&name).copied() == Some(sender) {
                            registry.remove(&name);
                            reply.type_ = REPLY_OK;
                        } else {
                            reply.type_ = REPLY_ERR;
                        }
                    }
                    None => { reply.type_ = REPLY_ERR; }
                }
            }

            _ => { reply.type_ = REPLY_ERR; }
        }

        libos::msg_send(sender, &reply);
    }
}

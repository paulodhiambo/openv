//! procfs-server — serves /proc filesystem requests forwarded by vfs-server.
//!
//! Registers with the kernel via syscall 114 (SYS_PROC_SERVER_REGISTER).
//! The VFS server forwards /proc/* IPC here; we call kernel syscalls 110/111
//! to get live process data and datacopy it back to the caller.

#![no_std]
#![no_main]

extern crate alloc;
extern crate libos;

use libos::ipc::Message;

// Opcodes from vfs-server (must match vfs-server constants)
const OP_PROC_LIST: i32 = 1;
const OP_PROC_READ: i32 = 2;
const OP_PROC_STAT: i32 = 3;
const REPLY_OK: i32 = 100;
const REPLY_ERR: i32 = 101;

fn register() {
    libos::syscall(114, 0, 0, 0); // SYS_PROC_SERVER_REGISTER
}

fn proc_list(buf: &mut [u8]) -> usize {
    // syscall 110: proc_list(buf_ptr, max_entries) -> n
    let n = libos::syscall(110, buf.as_mut_ptr() as usize, buf.len() / 4, 0);
    if n == usize::MAX {
        return 0;
    }
    // kernel wrote n PIDs as u32 array; format as NUL-separated decimal strings
    let entries = n.min(buf.len() / 4);
    let raw = buf.as_ptr();
    let mut out = alloc::vec![0u8; buf.len()];
    let mut pos = 0usize;
    for i in 0..entries {
        let pid = u32::from_le_bytes(unsafe {
            *(raw.add(i * 4) as *const [u8; 4])
        });
        let s = alloc::format!("{}", pid);
        let b = s.as_bytes();
        if pos + b.len() + 1 > out.len() {
            break;
        }
        out[pos..pos + b.len()].copy_from_slice(b);
        pos += b.len();
        out[pos] = 0;
        pos += 1;
    }
    buf[..pos].copy_from_slice(&out[..pos]);
    pos
}

fn proc_status(pid: i32, offset: u32, buf: &mut [u8]) -> usize {
    let mut tmp = [0u8; 512];
    // syscall 111: proc_status(pid, buf_ptr, buf_len) -> n
    let n = libos::syscall(111, pid as usize, tmp.as_mut_ptr() as usize, tmp.len());
    if n == usize::MAX || n == 0 {
        return 0;
    }
    let start = (offset as usize).min(n);
    let take = (n - start).min(buf.len());
    buf[..take].copy_from_slice(&tmp[start..start + take]);
    take
}

fn proc_stat_exists(pid: i32) -> bool {
    let mut tmp = [0u8; 16];
    let n = libos::syscall(111, pid as usize, tmp.as_mut_ptr() as usize, tmp.len());
    n != usize::MAX && n > 0
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
            OP_PROC_LIST => {
                // data[0..8]: buf_ptr (u64 in caller's addr space)
                // data[8..12]: max_len (u32)
                let buf_ptr = u64::from_le_bytes(msg.data[0..8].try_into().unwrap_or([0; 8])) as usize;
                let max_len = u32::from_le_bytes(msg.data[8..12].try_into().unwrap_or([0; 4])) as usize;
                let cap = max_len.min(2048);
                let mut local = alloc::vec![0u8; cap];
                let n = proc_list(&mut local);
                let wrote = if buf_ptr != 0 && n > 0 {
                    libos::datacopy(libos::getpid(), local.as_ptr(), sender, buf_ptr as *mut u8, n)
                } else {
                    n as isize
                };
                let mut reply = Message::new();
                if wrote >= 0 {
                    reply.type_ = REPLY_OK;
                    reply.data[0..4].copy_from_slice(&(n as u32).to_le_bytes());
                } else {
                    reply.type_ = REPLY_ERR;
                }
                libos::msg_send(sender, &reply);
            }

            OP_PROC_READ => {
                // data[0..4]: pid (i32)
                // data[4..8]: offset (u32)
                // data[8..12]: max_len (u32)
                // data[12..20]: buf_ptr (u64 in caller's addr space)
                let pid = i32::from_le_bytes(msg.data[0..4].try_into().unwrap_or([0; 4]));
                let offset = u32::from_le_bytes(msg.data[4..8].try_into().unwrap_or([0; 4]));
                let max_len = u32::from_le_bytes(msg.data[8..12].try_into().unwrap_or([0; 4])) as usize;
                let buf_ptr = u64::from_le_bytes(msg.data[12..20].try_into().unwrap_or([0; 8])) as usize;

                let cap = max_len.min(512);
                let mut local = alloc::vec![0u8; cap];
                let n = proc_status(pid, offset, &mut local);

                let wrote = if buf_ptr != 0 && n > 0 {
                    libos::datacopy(libos::getpid(), local.as_ptr(), sender, buf_ptr as *mut u8, n)
                } else {
                    n as isize
                };

                let mut reply = Message::new();
                if wrote >= 0 {
                    reply.type_ = REPLY_OK;
                    reply.data[0..4].copy_from_slice(&(n as u32).to_le_bytes());
                } else {
                    reply.type_ = REPLY_ERR;
                }
                libos::msg_send(sender, &reply);
            }

            OP_PROC_STAT => {
                // data[0..4]: pid (i32); 0 = stat /proc itself (always a dir)
                let pid = i32::from_le_bytes(msg.data[0..4].try_into().unwrap_or([0; 4]));
                // data[4]: is_dir_query (1 = /proc/<pid>, 0 = /proc/<pid>/status)
                let is_dir_query = msg.data[4] != 0;

                let mut reply = Message::new();
                if pid == 0 {
                    // /proc itself is always a directory
                    reply.type_ = REPLY_OK;
                    reply.data[0] = 1; // is_dir = true
                } else if is_dir_query {
                    // /proc/<pid> — exists if pid is running
                    if proc_stat_exists(pid) {
                        reply.type_ = REPLY_OK;
                        reply.data[0] = 1; // is_dir = true
                    } else {
                        reply.type_ = REPLY_ERR;
                    }
                } else {
                    // /proc/<pid>/status — exists if pid is running
                    if proc_stat_exists(pid) {
                        reply.type_ = REPLY_OK;
                        reply.data[0] = 0; // is_dir = false
                        // approximate size: 256 bytes
                        reply.data[4..8].copy_from_slice(&256u32.to_le_bytes());
                    } else {
                        reply.type_ = REPLY_ERR;
                    }
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

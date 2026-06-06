#![no_std]

pub const OP_FORK: i32 = 1;
pub const OP_EXEC: i32 = 2;
pub const OP_WAITPID: i32 = 3;
pub const OP_EXIT: i32 = 4;

pub const REPLY_OK: i32 = 100;
pub const REPLY_ERR: i32 = 101;

pub fn pack_waitpid_req(data: &mut [u8; 56], target: i32, options: u32) {
    let mut i = 0;
    data[i..i+4].copy_from_slice(&target.to_le_bytes());
    i += 4;
    data[i..i+4].copy_from_slice(&options.to_le_bytes());
}

pub fn unpack_waitpid_req(data: &[u8; 56]) -> (i32, u32) {
    let target = i32::from_le_bytes(data[0..4].try_into().unwrap());
    let options = u32::from_le_bytes(data[4..8].try_into().unwrap());
    (target, options)
}

pub fn pack_exec_req(data: &mut [u8; 56], path: &str) {
    let bytes = path.as_bytes();
    let len = core::cmp::min(bytes.len(), 52);
    data[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    data[4..4+len].copy_from_slice(&bytes[..len]);
}

pub fn unpack_exec_req(data: &[u8; 56]) -> &str {
    let len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let bytes = &data[4..4+len];
    core::str::from_utf8(bytes).unwrap_or("")
}

pub fn pack_exit_req(data: &mut [u8; 56], status: i32) {
    data[0..4].copy_from_slice(&status.to_le_bytes());
}

pub fn unpack_exit_req(data: &[u8; 56]) -> i32 {
    i32::from_le_bytes(data[0..4].try_into().unwrap())
}

pub fn pack_reply_u32(data: &mut [u8; 56], val: u32) {
    data[0..4].copy_from_slice(&val.to_le_bytes());
}

pub fn unpack_reply_u32(data: &[u8; 56]) -> u32 {
    u32::from_le_bytes(data[0..4].try_into().unwrap())
}

#![no_std]

// Opcodes (request type — first byte of every IPC message).
pub const OP_OPEN:      i32 = 1;
pub const OP_READ:      i32 = 2;
pub const OP_WRITE:     i32 = 3;
pub const OP_CLOSE:     i32 = 4;
pub const OP_GETDENTS:  i32 = 5;
pub const OP_MKDIR:     i32 = 6;
pub const OP_UNLINK:    i32 = 7;
pub const OP_STAT:      i32 = 8;
pub const OP_CREATE:    i32 = 9;
pub const OP_RENAME:    i32 = 10;
pub const OP_FSYNC:     i32 = 11;
pub const OP_DUP:       i32 = 12;
pub const OP_READLINK:  i32 = 13;
pub const OP_SYMLINK:   i32 = 14;
pub const OP_LINK:      i32 = 15;
pub const OP_CHMOD:     i32 = 16;
pub const OP_CHOWN:     i32 = 17;
pub const OP_FTRUNCATE: i32 = 18;
pub const OP_LSEEK:     i32 = 19;
pub const OP_FPATH:     i32 = 20;
pub const OP_FUTIMENS:  i32 = 21;
pub const OP_FALLOCATE: i32 = 22;

// Reply status codes (for msg.type_)
pub const REPLY_OK:  i32 = 0;
pub const REPLY_ERR: i32 = -1;

// ── Encoding helpers ─────────────────────────────────────────────────────────

pub fn put_u32(buf: &mut [u8], off: &mut usize, v: u32) {
    buf[*off..*off + 4].copy_from_slice(&v.to_le_bytes());
    *off += 4;
}

pub fn put_u64(buf: &mut [u8], off: &mut usize, v: u64) {
    buf[*off..*off + 8].copy_from_slice(&v.to_le_bytes());
    *off += 8;
}

pub fn get_u32(buf: &[u8], off: &mut usize) -> u32 {
    let v = u32::from_le_bytes(buf[*off..*off + 4].try_into().unwrap_or([0; 4]));
    *off += 4;
    v
}

pub fn get_u64(buf: &[u8], off: &mut usize) -> u64 {
    let v = u64::from_le_bytes(buf[*off..*off + 8].try_into().unwrap_or([0; 8]));
    *off += 8;
    v
}

// ── Request builders ─────────────────────────────────────────────────────────

/// OP_OPEN / OP_CREATE / OP_MKDIR / OP_UNLINK / OP_STAT
/// Format: flags(4), path_ptr(8), path_len(4)
pub fn pack_path_req(data: &mut [u8; 56], flags: u32, path_ptr: usize, path_len: usize) {
    let mut off = 0;
    put_u32(data, &mut off, flags);
    put_u64(data, &mut off, path_ptr as u64);
    put_u32(data, &mut off, path_len as u32);
}

pub fn unpack_path_req(data: &[u8; 56]) -> (u32, usize, usize) {
    let mut off = 0;
    let flags = get_u32(data, &mut off);
    let path_ptr = get_u64(data, &mut off) as usize;
    let path_len = get_u32(data, &mut off) as usize;
    (flags, path_ptr, path_len)
}

/// OP_READ / OP_WRITE
/// Format: fd(4), offset(8), buf_ptr(8), len(4)
pub fn pack_rw_req(data: &mut [u8; 56], fd: u32, offset: u64, buf_ptr: usize, len: usize) {
    let mut off = 0;
    put_u32(data, &mut off, fd);
    put_u64(data, &mut off, offset);
    put_u64(data, &mut off, buf_ptr as u64);
    put_u32(data, &mut off, len as u32);
}

pub fn unpack_rw_req(data: &[u8; 56]) -> (u32, u64, usize, usize) {
    let mut off = 0;
    let fd = get_u32(data, &mut off);
    let offset = get_u64(data, &mut off);
    let buf_ptr = get_u64(data, &mut off) as usize;
    let len = get_u32(data, &mut off) as usize;
    (fd, offset, buf_ptr, len)
}

/// OP_GETDENTS
/// Format: path_ptr(8), path_len(4), buf_ptr(8), buf_len(4)
pub fn pack_getdents_req(data: &mut [u8; 56], path_ptr: usize, path_len: usize, buf_ptr: usize, buf_len: usize) {
    let mut off = 0;
    put_u64(data, &mut off, path_ptr as u64);
    put_u32(data, &mut off, path_len as u32);
    put_u64(data, &mut off, buf_ptr as u64);
    put_u32(data, &mut off, buf_len as u32);
}

pub fn unpack_getdents_req(data: &[u8; 56]) -> (usize, usize, usize, usize) {
    let mut off = 0;
    let path_ptr = get_u64(data, &mut off) as usize;
    let path_len = get_u32(data, &mut off) as usize;
    let buf_ptr = get_u64(data, &mut off) as usize;
    let buf_len = get_u32(data, &mut off) as usize;
    (path_ptr, path_len, buf_ptr, buf_len)
}

/// OP_CLOSE
/// Format: fd(4)
pub fn pack_close_req(data: &mut [u8; 56], fd: u32) {
    let mut off = 0;
    put_u32(data, &mut off, fd);
}

pub fn unpack_close_req(data: &[u8; 56]) -> u32 {
    let mut off = 0;
    get_u32(data, &mut off)
}

/// OP_RENAME
/// Format: old_ptr(8), old_len(4), new_ptr(8), new_len(4)
pub fn pack_rename_req(data: &mut [u8; 56], old_ptr: usize, old_len: usize, new_ptr: usize, new_len: usize) {
    let mut off = 0;
    put_u64(data, &mut off, old_ptr as u64);
    put_u32(data, &mut off, old_len as u32);
    put_u64(data, &mut off, new_ptr as u64);
    put_u32(data, &mut off, new_len as u32);
}

pub fn unpack_rename_req(data: &[u8; 56]) -> (usize, usize, usize, usize) {
    let mut off = 0;
    let old_ptr = get_u64(data, &mut off) as usize;
    let old_len = get_u32(data, &mut off) as usize;
    let new_ptr = get_u64(data, &mut off) as usize;
    let new_len = get_u32(data, &mut off) as usize;
    (old_ptr, old_len, new_ptr, new_len)
}

/// OP_SYMLINK / OP_LINK (two paths)
/// Format: path1_ptr(8), path1_len(4), path2_ptr(8), path2_len(4)
pub fn pack_two_paths_req(data: &mut [u8; 56], p1_ptr: usize, p1_len: usize, p2_ptr: usize, p2_len: usize) {
    let mut off = 0;
    put_u64(data, &mut off, p1_ptr as u64);
    put_u32(data, &mut off, p1_len as u32);
    put_u64(data, &mut off, p2_ptr as u64);
    put_u32(data, &mut off, p2_len as u32);
}

pub fn unpack_two_paths_req(data: &[u8; 56]) -> (usize, usize, usize, usize) {
    let mut off = 0;
    let p1_ptr = get_u64(data, &mut off) as usize;
    let p1_len = get_u32(data, &mut off) as usize;
    let p2_ptr = get_u64(data, &mut off) as usize;
    let p2_len = get_u32(data, &mut off) as usize;
    (p1_ptr, p1_len, p2_ptr, p2_len)
}

/// OP_FTRUNCATE
/// Format: fd(4), length(8)
pub fn pack_ftruncate_req(data: &mut [u8; 56], fd: u32, length: u64) {
    let mut off = 0;
    put_u32(data, &mut off, fd);
    put_u64(data, &mut off, length);
}

pub fn unpack_ftruncate_req(data: &[u8; 56]) -> (u32, u64) {
    let mut off = 0;
    let fd = get_u32(data, &mut off);
    let length = get_u64(data, &mut off);
    (fd, length)
}

/// OP_LSEEK
/// Format: fd(4), offset(8), whence(4)
pub fn pack_lseek_req(data: &mut [u8; 56], fd: u32, offset: i64, whence: u32) {
    let mut off = 0;
    put_u32(data, &mut off, fd);
    put_u64(data, &mut off, offset as u64);
    put_u32(data, &mut off, whence);
}

pub fn unpack_lseek_req(data: &[u8; 56]) -> (u32, i64, u32) {
    let mut off = 0;
    let fd = get_u32(data, &mut off);
    let offset = get_u64(data, &mut off) as i64;
    let whence = get_u32(data, &mut off);
    (fd, offset, whence)
}

/// OP_FPATH
/// Format: fd(4), buf_ptr(8), buf_len(4)
pub fn pack_fpath_req(data: &mut [u8; 56], fd: u32, buf_ptr: usize, buf_len: usize) {
    let mut off = 0;
    put_u32(data, &mut off, fd);
    put_u64(data, &mut off, buf_ptr as u64);
    put_u32(data, &mut off, buf_len as u32);
}

pub fn unpack_fpath_req(data: &[u8; 56]) -> (u32, usize, usize) {
    let mut off = 0;
    let fd = get_u32(data, &mut off);
    let buf_ptr = get_u64(data, &mut off) as usize;
    let buf_len = get_u32(data, &mut off) as usize;
    (fd, buf_ptr, buf_len)
}

/// OP_FUTIMENS
/// Format: fd(4), atime_sec(8), atime_nsec(8), mtime_sec(8), mtime_nsec(8)
pub fn pack_futimens_req(data: &mut [u8; 56], fd: u32, ats: i64, atns: i64, mts: i64, mtns: i64) {
    let mut off = 0;
    put_u32(data, &mut off, fd);
    put_u64(data, &mut off, ats as u64);
    put_u64(data, &mut off, atns as u64);
    put_u64(data, &mut off, mts as u64);
    put_u64(data, &mut off, mtns as u64);
}

pub fn unpack_futimens_req(data: &[u8; 56]) -> (u32, i64, i64, i64, i64) {
    let mut off = 0;
    let fd = get_u32(data, &mut off);
    let ats = get_u64(data, &mut off) as i64;
    let atns = get_u64(data, &mut off) as i64;
    let mts = get_u64(data, &mut off) as i64;
    let mtns = get_u64(data, &mut off) as i64;
    (fd, ats, atns, mts, mtns)
}

/// OP_FALLOCATE
/// Format: fd(4), offset(8), length(8)
pub fn pack_fallocate_req(data: &mut [u8; 56], fd: u32, offset: u64, length: u64) {
    let mut off = 0;
    put_u32(data, &mut off, fd);
    put_u64(data, &mut off, offset);
    put_u64(data, &mut off, length);
}

pub fn unpack_fallocate_req(data: &[u8; 56]) -> (u32, u64, u64) {
    let mut off = 0;
    let fd = get_u32(data, &mut off);
    let offset = get_u64(data, &mut off);
    let length = get_u64(data, &mut off);
    (fd, offset, length)
}

// ── Reply builders / parsers ─────────────────────────────────────────────────

pub fn pack_u32_reply(data: &mut [u8; 56], val: u32) {
    let mut off = 0;
    put_u32(data, &mut off, val);
}

pub fn unpack_u32_reply(data: &[u8; 56]) -> u32 {
    let mut off = 0;
    get_u32(data, &mut off)
}

pub fn pack_u64_reply(data: &mut [u8; 56], val: u64) {
    let mut off = 0;
    put_u64(data, &mut off, val);
}

pub fn unpack_u64_reply(data: &[u8; 56]) -> u64 {
    let mut off = 0;
    get_u64(data, &mut off)
}

pub fn pack_stat_reply(data: &mut [u8; 56], is_dir: bool, size: u64) {
    data[0] = if is_dir { 1 } else { 0 };
    let mut off = 1;
    put_u64(data, &mut off, size);
}

pub fn unpack_stat_reply(data: &[u8; 56]) -> (bool, u64) {
    let is_dir = data[0] != 0;
    let mut off = 1;
    let size = get_u64(data, &mut off);
    (is_dir, size)
}

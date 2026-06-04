#![no_std]

// Opcodes (request type — first byte of every IPC message).
pub const OP_OPEN:     u8 = 1;
pub const OP_READ:     u8 = 2;
pub const OP_WRITE:    u8 = 3;
pub const OP_CLOSE:    u8 = 4;
pub const OP_GETDENTS: u8 = 5;
pub const OP_MKDIR:    u8 = 6;
pub const OP_UNLINK:   u8 = 7;
pub const OP_STAT:     u8 = 8;
pub const OP_CREATE:   u8 = 9;
pub const OP_RENAME:   u8 = 10;

// Reply status codes (second byte of every reply).
pub const REPLY_OK:  u8 = 0;
pub const REPLY_ERR: u8 = 1;

// Max IPC message payload in bytes (must match kernel limit).
pub const MAX_MSG: usize = 4096;

// ── Encoding helpers ─────────────────────────────────────────────────────────

pub fn put_u32(buf: &mut [u8], off: &mut usize, v: u32) {
    buf[*off..*off + 4].copy_from_slice(&v.to_le_bytes());
    *off += 4;
}

pub fn put_u64(buf: &mut [u8], off: &mut usize, v: u64) {
    buf[*off..*off + 8].copy_from_slice(&v.to_le_bytes());
    *off += 8;
}

pub fn put_bytes(buf: &mut [u8], off: &mut usize, data: &[u8]) {
    buf[*off..*off + data.len()].copy_from_slice(data);
    *off += data.len();
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

/// Build an OP_OPEN request into `buf`. Returns byte count.
/// Format: [OP_OPEN, flags(4), path_bytes...]
pub fn build_open(buf: &mut [u8], flags: u32, path: &[u8]) -> usize {
    let mut off = 0;
    buf[off] = OP_OPEN; off += 1;
    put_u32(buf, &mut off, flags);
    put_bytes(buf, &mut off, path);
    off
}

/// Build an OP_READ request. Format: [OP_READ, fd(4), offset(8), len(4)]
pub fn build_read(buf: &mut [u8], fd: u32, offset: u64, len: u32) -> usize {
    let mut off = 0;
    buf[off] = OP_READ; off += 1;
    put_u32(buf, &mut off, fd);
    put_u64(buf, &mut off, offset);
    put_u32(buf, &mut off, len);
    off
}

/// Build an OP_WRITE request. Format: [OP_WRITE, fd(4), offset(8), data...]
pub fn build_write(buf: &mut [u8], fd: u32, offset: u64, data: &[u8]) -> usize {
    let mut off = 0;
    buf[off] = OP_WRITE; off += 1;
    put_u32(buf, &mut off, fd);
    put_u64(buf, &mut off, offset);
    put_bytes(buf, &mut off, data);
    off
}

/// Build an OP_CLOSE request. Format: [OP_CLOSE, fd(4)]
pub fn build_close(buf: &mut [u8], fd: u32) -> usize {
    let mut off = 0;
    buf[off] = OP_CLOSE; off += 1;
    put_u32(buf, &mut off, fd);
    off
}

/// Build an OP_GETDENTS request. Format: [OP_GETDENTS, path_bytes...]
pub fn build_getdents(buf: &mut [u8], path: &[u8]) -> usize {
    let mut off = 0;
    buf[off] = OP_GETDENTS; off += 1;
    put_bytes(buf, &mut off, path);
    off
}

/// Build a path-only request (MKDIR, UNLINK, CREATE). Format: [op, path...]
pub fn build_path_op(buf: &mut [u8], op: u8, path: &[u8]) -> usize {
    let mut off = 0;
    buf[off] = op; off += 1;
    put_bytes(buf, &mut off, path);
    off
}

/// Build an OP_RENAME request. Format: [OP_RENAME, old_len(4), old..., new...]
pub fn build_rename(buf: &mut [u8], old: &[u8], new: &[u8]) -> usize {
    let mut off = 0;
    buf[off] = OP_RENAME; off += 1;
    put_u32(buf, &mut off, old.len() as u32);
    put_bytes(buf, &mut off, old);
    put_bytes(buf, &mut off, new);
    off
}

/// Build an OP_STAT request. Format: [OP_STAT, path_bytes...]
pub fn build_stat(buf: &mut [u8], path: &[u8]) -> usize {
    build_path_op(buf, OP_STAT, path)
}

// ── Reply parsers ─────────────────────────────────────────────────────────────

/// Parse a reply: returns `(status, payload_slice)`.
/// status = REPLY_OK or REPLY_ERR; payload follows byte 0.
pub fn parse_reply(buf: &[u8], len: usize) -> (u8, &[u8]) {
    if len == 0 { return (REPLY_ERR, &[]); }
    (buf[0], &buf[1..len])
}

/// Parse an OP_OPEN reply payload: returns vfs_fd on OK.
pub fn parse_open_reply(payload: &[u8]) -> Option<u32> {
    if payload.len() < 4 { return None; }
    let mut off = 0;
    Some(get_u32(payload, &mut off))
}

/// Parse an OP_READ reply payload: returns data slice.
pub fn parse_read_reply(payload: &[u8]) -> &[u8] {
    payload
}

/// Parse an OP_WRITE reply payload: returns bytes_written.
pub fn parse_write_reply(payload: &[u8]) -> u32 {
    if payload.len() < 4 { return 0; }
    let mut off = 0;
    get_u32(payload, &mut off)
}

/// Parse an OP_STAT reply: (is_dir, size).
pub fn parse_stat_reply(payload: &[u8]) -> Option<(bool, u64)> {
    if payload.len() < 9 { return None; }
    let is_dir = payload[0] != 0;
    let mut off = 1;
    let size = get_u64(payload, &mut off);
    Some((is_dir, size))
}

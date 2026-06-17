use core::mem::size_of;

/// Universal Synchronous IPC Message (64 bytes, MINIX 3-style).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Message {
    pub source: i32,
    pub type_: i32,
    pub data: [u8; 56],
}

impl Message {
    pub const fn new() -> Self {
        Self { source: 0, type_: 0, data: [0; 56] }
    }
}

impl Default for Message {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(size_of::<Message>() == 64);

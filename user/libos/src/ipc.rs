use core::mem::size_of;

/// Universal Synchronous IPC Message
/// A fixed-size message struct for inter-process communication.
/// Following MINIX 3 conventions, it has a source endpoint (or PID),
/// a message type, and a payload.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Message {
    pub source: i32,  // Endpoint ID or PID of the sender
    pub type_: i32,   // Opcode
    pub data: [u8; 56], // Payload
}

impl Message {
    pub const fn new() -> Self {
        Self {
            source: 0,
            type_: 0,
            data: [0; 56],
        }
    }
}

impl Default for Message {
    fn default() -> Self {
        Self::new()
    }
}

// Assert that Message is exactly 64 bytes
const _: () = assert!(size_of::<Message>() == 64);

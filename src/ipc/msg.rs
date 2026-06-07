//! # Low-Level IPC Message Types
//!
//! This module provides low-level message types used by the IPC system.
//! These types are used internally by the kernel and may also be used
//! by user-space servers that need fine-grained control over IPC.
//!
//! ## Message Format
//!
//! The [`Message`] struct is a fixed-size, 64-byte structure that
//! follows the MINIX 3 conventions for IPC messages. It contains:
//!
//! - **source**: The endpoint ID or PID of the sender.
//!  - **type_**: An opcode that identifies the message type.
//!  - **data**: A 56-byte payload.
//!
//! This format is deliberately simple and fixed-size to make IPC
//! efficient and predictable. Larger messages can be sent by
//! passing a [`handle::Handle`] to a shared memory object.

use core::mem::size_of;

/// Universal Synchronous IPC Message.
///
/// A fixed-size message struct for inter-process communication.
/// Following MINIX 3 conventions, it has a source endpoint (or PID),
/// a message type, and a payload.
///
/// # Fields
///
/// * `source` - Endpoint ID or PID of the sender.
/// * `type_` - Opcode that identifies the message type.
/// * `data` - 56-byte payload.
///
/// # Size
///
/// The struct is exactly 64 bytes (4 + 4 + 56), as enforced by the
/// compile-time assertion at the bottom of this module. This fixed
/// size allows for efficient IPC and predictable memory usage.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    /// Endpoint ID or PID of the sender.
    pub source: i32,
    /// Opcode that identifies the message type.
    pub type_: i32,
    /// 56-byte payload.
    pub data: [u8; 56],
}

impl Default for Message {
    fn default() -> Self {
        Self::new()
    }
}

impl Message {
    /// Creates a new zeroed [`Message`].
    ///
    /// # Returns
    ///
    /// A new [`Message`] with all fields set to zero.
    pub const fn new() -> Self {
        Self {
            source: 0,
            type_: 0,
            data: [0; 56],
        }
    }
}

// Assert that Message is exactly 64 bytes
const _: () = assert!(size_of::<Message>() == 64);

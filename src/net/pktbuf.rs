//! # Packet Buffer Abstraction
//!
//! This module provides a simple packet buffer abstraction used by the
//! kernel and network drivers.
//!
//! ## Overview
//!
//! The [`PktBuf`] struct is a fixed-size buffer that can hold a single
//! network packet. The maximum packet size is [`MAX_PKT_SIZE`] (1536
//! bytes, which is the maximum Ethernet frame size).
//!
//! This abstraction is used to pass packets between the kernel and
//! network drivers without requiring heap allocation.

/// Maximum packet size in bytes (1536, the maximum Ethernet frame size).
pub const MAX_PKT_SIZE: usize = 1536;

/// A packet buffer.
///
/// # Fields
///
/// * `len` - The number of valid bytes in the buffer.
/// * `data` - The buffer data, with a maximum size of [`MAX_PKT_SIZE`].
pub struct PktBuf {
    /// The number of valid bytes in the buffer.
    pub len: usize,
    /// The buffer data, with a maximum size of [`MAX_PKT_SIZE`].
    pub data: [u8; MAX_PKT_SIZE],
}

impl PktBuf {
    /// Creates an empty packet buffer.
    ///
    /// # Returns
    ///
    /// A new [`PktBuf`] with `len = 0` and all data bytes set to zero.
    pub const fn empty() -> Self {
        Self {
            len: 0,
            data: [0u8; MAX_PKT_SIZE],
        }
    }

    /// Creates a packet buffer from a byte slice.
    ///
    /// If the slice is longer than [`MAX_PKT_SIZE`], it is truncated.
    ///
    /// # Arguments
    ///
    /// * `slice` - The byte slice to copy into the buffer.
    ///
    /// # Returns
    ///
    /// A new [`PktBuf`] containing the data from `slice`.
    pub fn from_slice(slice: &[u8]) -> Self {
        let mut b = Self::empty();
        let copy_len = core::cmp::min(slice.len(), MAX_PKT_SIZE);
        b.data[..copy_len].copy_from_slice(&slice[..copy_len]);
        b.len = copy_len;
        b
    }

    /// Returns a slice of the valid packet data.
    ///
    /// # Returns
    ///
    /// A slice of length `self.len` containing the packet data.
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    /// Returns a mutable slice of the valid packet data.
    ///
    /// # Returns
    ///
    /// A mutable slice of length `self.len` containing the packet data.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data[..self.len]
    }
}

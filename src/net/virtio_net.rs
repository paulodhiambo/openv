//! # VirtIO Network Device Driver
//!
//! This module provides a minimal VirtIO network device driver skeleton.
//! Currently, it only provides a loopback device for testing. A full
//! VirtIO MMIO driver is planned.
//!
//! ## Overview
//!
//! The [`LoopbackNet`] struct implements a simple loopback network
//! device. When a packet is sent, it is stored in a static buffer.
//! When a packet is received, the stored packet is returned and the
//! buffer is cleared.
//!
//! This is useful for testing the network stack without actual
//! hardware. In production, the [`init_virtio_mmio`] function would
//! probe the DTB for a VirtIO MMIO device and initialize a real driver.
//!
//! [`LoopbackNet`]: struct.LoopbackNet.html
//! [`init_virtio_mmio`]: struct.LoopbackNet.html#method.init_virtio_mmio

use crate::net::NetDevice;
use crate::sync::Mutex;

/// A loopback network device.
///
/// The loopback device stores the most recently sent packet in a
/// static buffer and returns it on the next receive call. This is
/// useful for testing the network stack without actual hardware.
pub struct LoopbackNet;

impl Default for LoopbackNet {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopbackNet {
    /// Creates a new [`LoopbackNet`] instance.
    pub const fn new() -> Self {
        LoopbackNet
    }

    /// Initializes and registers the loopback device.
    ///
    /// This function creates a static [`LoopbackNet`] instance and
    /// registers it with the global network device registry. After
    /// calling this function, [`crate::net::device()`] will return
    /// `Some(&loopback)`.
    pub fn init() {
        static DEVICE: LoopbackNet = LoopbackNet::new();
        crate::net::register_device(&DEVICE);
    }

    /// Initializes a real VirtIO MMIO device (not yet implemented).
    ///
    /// This function is a placeholder for the real VirtIO MMIO driver.
    /// It would probe the DTB for a VirtIO MMIO device, map its MMIO
    /// registers, negotiate features, and allocate virtqueues.
    ///
    /// # Returns
    ///
    /// `None` (not yet implemented).
    #[allow(dead_code)]
    pub fn init_virtio_mmio() -> Option<&'static Self> {
        // TODO: probe via FDT, map MMIO, negotiate features, allocate virtqueues
        None
    }
}

impl NetDevice for LoopbackNet {
    /// Sends a packet (stores it in the loopback buffer).
    ///
    /// # Arguments
    ///
    /// * `packet` - The packet data to send.
    fn send(&self, packet: &[u8]) {
        // Loopback: drop or stash packet. For now, store first packet in a static buffer to be returned on recv.
        let mut buf = LOOPBACK_BUF.lock();
        buf.len = 0;
        let copy = core::cmp::min(packet.len(), buf.data.len());
        buf.data[..copy].copy_from_slice(&packet[..copy]);
        buf.len = copy;
    }

    /// Receives a packet from the loopback buffer.
    ///
    /// # Arguments
    ///
    /// * `buf_out` - The buffer to store the received packet.
    ///
    /// # Returns
    ///
    /// The number of bytes received, or 0 if no packet is available.
    fn recv(&self, buf_out: &mut [u8]) -> usize {
        let mut buf = LOOPBACK_BUF.lock();
        if buf.len == 0 {
            return 0;
        }
        let copy = core::cmp::min(buf.len, buf_out.len());
        buf_out[..copy].copy_from_slice(&buf.data[..copy]);
        // Clear after read
        buf.len = 0;
        copy
    }
}

/// A static packet buffer used by the loopback device.
struct StaticPkt {
    len: usize,
    data: [u8; super::pktbuf::MAX_PKT_SIZE],
}

impl StaticPkt {
    /// Creates an empty static packet buffer.
    const fn empty() -> Self {
        Self {
            len: 0,
            data: [0u8; super::pktbuf::MAX_PKT_SIZE],
        }
    }
}

/// Global loopback buffer, protected by a [`Mutex`].
static LOOPBACK_BUF: Mutex<StaticPkt> = Mutex::new(StaticPkt::empty());

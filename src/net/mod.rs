//! # Networking
//!
//! This module provides networking support for OpenV, including device
//! drivers, packet buffers, sockets, and the VirtIO MMIO transport.
//!
//! ## Overview
//!
//! OpenV's networking stack is organized into the following submodules:
//!
//! - [`core`]: Core networking types and traits.
//!  - [`pktbuf`]: Packet buffer management.
//!  - [`socket`]: Socket abstractions.
//!  - [`virtio_mmio`]: VirtIO MMIO transport driver.
//!  - [`virtio_net`]: VirtIO network device driver.
//!  - [`virtqueue`]: VirtIO virtqueue implementation.
//!
//! ## Device Registration
//!
//! Network devices are registered via [`register_device`] and accessed
//! via [`device`]. Only one device is supported at a time.
//!
//! ## Initialization
//!
//! The [`init`] function initializes networking by:
//! 1. Probing the DTB for VirtIO MMIO devices.
//! 2. If a device is found, registering it.
//! 3. Otherwise, falling back to a loopback device for testing.
//!
//! [`core`]: core/index.html
//! [`pktbuf`]: pktbuf/index.html
//! [`socket`]: socket/index.html
//! [`virtio_mmio`]: virtio_mmio/index.html
//! [`virtio_net`]: virtio_net/index.html
//! [`virtqueue`]: virtqueue/index.html

/// Core networking types and traits.
pub mod core;
/// Packet buffer management.
pub mod pktbuf;
/// Socket abstractions.
pub mod socket;
/// VirtIO MMIO transport driver.
pub mod virtio_mmio;
/// VirtIO network device driver.
pub mod virtio_net;
/// VirtIO virtqueue implementation.
pub mod virtqueue;

use crate::sync::Mutex;

/// Global storage for the active network device.
///
/// Only one network device is supported at a time. The device is
/// registered via [`register_device`] and accessed via [`device`].
static NET_DEVICE: Mutex<Option<&'static dyn NetDevice>> = Mutex::new(None);

/// Trait implemented by all network devices.
///
/// Network devices must be safe to share across threads (`Sync`) and
/// must provide [`send`] and [`recv`] methods.
///
/// [`send`]: #tymethod.send
/// [`recv`]: #tymethod.recv
pub trait NetDevice: Sync {
    /// Sends a packet over the network.
    ///
    /// # Arguments
    ///
    /// * `packet` - The packet data to send.
    fn send(&self, packet: &[u8]);

    /// Receives a packet from the network.
    ///
    /// # Arguments
    ///
    /// * `buf` - The buffer to store the received packet.
    ///
    /// # Returns
    ///
    /// The number of bytes received, or 0 if no packet is available.
    fn recv(&self, buf: &mut [u8]) -> usize;
}

/// Registers a network device.
///
/// Only one device can be registered at a time. If a device is
/// already registered, it is replaced.
///
/// # Arguments
///
/// * `dev` - A static reference to the network device to register.
pub fn register_device(dev: &'static dyn NetDevice) {
    let mut slot = NET_DEVICE.lock();
    *slot = Some(dev);
}

/// Returns a reference to the registered network device, if any.
///
/// # Returns
///
/// `Some(&'static dyn NetDevice)` if a device is registered,
/// `None` otherwise.
pub fn device() -> Option<&'static dyn NetDevice> {
    let slot = NET_DEVICE.lock();
    slot.clone()
}

/// Initializes networking via the driver framework; falls back to loopback if nothing claims a device.
///
/// # Arguments
///
/// * `dtb_ptr` - The physical address of the device tree blob (DTB).
pub fn init(dtb_ptr: usize) {
    crate::drivers::probe_all(dtb_ptr);
    if device().is_some() {
        crate::println!("net: device registered via driver framework");
    } else {
        crate::println!("net: no device found; using loopback");
        virtio_net::LoopbackNet::init();
    }
}

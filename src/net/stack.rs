//! # Network Stack (smoltcp integration)
//!
//! This module integrates the smoltcp TCP/IP stack with OpenV's
//! [`NetDevice`] trait. smoltcp is a pure-Rust TCP/IP stack designed
//! for embedded systems.
//!
//! ## Overview
//!
//! OpenV uses smoltcp for its networking stack. This module provides:
//!
//! - [`SmolDevice`]: An adapter from OpenV's [`NetDevice`] to smoltcp's
///    [`smoltcp::phy::Device`] trait.
///  - [`RxToken`] and [`TxToken`]: smoltcp tokens for receiving and
///    transmitting packets.
///  - [`NetworkStack`]: The global network stack state.
///  - [`init`]: Initializes the network stack.
///  - [`poll`]: Polls the network stack to process incoming and outgoing
///    packets.
//!
//! ## IP Configuration
//!
//! The default IP configuration is:
//! - **IP address**: `10.0.2.15/24`
//! - **MAC address**: `02:00:00:00:00:01`
//!
//! This is the default QEMU 'virt' platform configuration.
//!
//! ## Usage
//!
//! The network stack should be polled regularly to process packets.
//! Typically, the trap handler or scheduler calls [`poll`] on each
//! timer tick.
//!
//! [`NetDevice`]: ../trait.NetDevice.html
//! [`SmolDevice`]: struct.SmolDevice.html
//! [`RxToken`]: struct.RxToken.html
//! [`TxToken`]: struct.TxToken.html
//! [`NetworkStack`]: struct.NetworkStack.html
//! [`init`]: fn.init.html
//! [`poll`]: fn.poll.html

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpCidr, IpAddress, Ipv4Address};
use smoltcp::phy::{Device, DeviceCapabilities, Medium};
use crate::sync::Mutex;
use alloc::vec::Vec;
use crate::net::NetDevice;

/// Adapter from OpenV's [`NetDevice`] to smoltcp's [`Device`] trait.
///
/// # Fields
///
/// * `inner` - The underlying OpenV network device.
pub struct SmolDevice {
    /// The underlying OpenV network device.
    pub inner: &'static dyn NetDevice,
}

impl Device for SmolDevice {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken;

    /// Receives a packet from the device.
    ///
    /// This method is called by smoltcp to receive a packet. It reads
    /// from the underlying device and returns receive and transmit
    /// tokens.
    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut buf = [0u8; 2048];
        let n = self.inner.recv(&mut buf);
        if n > 0 {
            let mut data = Vec::new();
            data.extend_from_slice(&buf[..n]);
            Some((RxToken { data }, TxToken { inner: self.inner }))
        } else {
            None
        }
    }

    /// Returns a transmit token.
    ///
    /// This method is called by smoltcp to get a transmit token. The
    /// token can be used to send a packet.
    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken { inner: self.inner })
    }

    /// Returns the device capabilities.
    ///
    /// The capabilities describe the maximum transmission unit (MTU)
    /// and the medium (Ethernet).
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ethernet;
        caps
    }
}

/// A smoltcp receive token.
///
/// The token contains the received packet data. It is consumed by
/// smoltcp to process the packet.
pub struct RxToken {
    data: Vec<u8>,
}

impl smoltcp::phy::RxToken for RxToken {
    /// Consumes the token and processes the packet.
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.data)
    }
}

/// A smoltcp transmit token.
///
/// The token contains a reference to the underlying network device.
/// It is consumed by smoltcp to send a packet.
pub struct TxToken {
    inner: &'static dyn NetDevice,
}

impl smoltcp::phy::TxToken for TxToken {
    /// Consumes the token and sends a packet.
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = Vec::new();
        buf.resize(len, 0);
        let ret = f(&mut buf);
        self.inner.send(&buf);
        ret
    }
}

/// The global network stack.
///
/// # Fields
///
/// * `iface` - The smoltcp network interface.
/// * `sockets` - The smoltcp socket set.
/// * `device` - The smoltcp device adapter.
pub struct NetworkStack {
    /// The smoltcp network interface.
    pub iface: Interface,
    /// The smoltcp socket set.
    pub sockets: SocketSet<'static>,
    /// The smoltcp device adapter.
    pub device: SmolDevice,
}

/// Global network stack instance, protected by a [`Mutex`].
static STACK: Mutex<Option<NetworkStack>> = Mutex::new(None);

/// Initializes the network stack.
///
/// This function:
/// 1. Gets the registered network device.
/// 2. Creates a smoltcp interface with the default IP configuration.
/// 3. Creates an empty socket set.
/// 4. Stores the stack in the global [`STACK`] static.
///
/// If no network device is registered, the function returns without
/// doing anything.
pub fn init() {
    let device = if let Some(dev) = crate::net::device() {
        SmolDevice { inner: dev }
    } else {
        return;
    };

    let mut config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).into());
    
    let mut iface = Interface::new(config, &mut SmolDevice { inner: device.inner }, Instant::from_micros(0));
    
    iface.update_ip_addrs(|addrs| {
        addrs.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24)).unwrap();
    });

    let sockets = SocketSet::new(Vec::new());

    let mut stack = STACK.lock();
    *stack = Some(NetworkStack {
        iface,
        sockets,
        device,
    });
    
    crate::println!("Network stack initialized: 10.0.2.15/24");
}

/// Polls the network stack to process incoming and outgoing packets.
///
/// This function should be called regularly (e.g., on each timer tick)
/// to ensure that packets are processed in a timely manner.
///
/// If the network stack has not been initialized, this function does nothing.
pub fn poll() {
    let mut stack_lock = STACK.lock();
    if let Some(stack) = stack_lock.as_mut() {
        let time = Instant::from_micros(0); // TODO: implement real clock
        stack.iface.poll(time, &mut stack.device, &mut stack.sockets);
    }
}

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpCidr, IpAddress, Ipv4Address};
use smoltcp::phy::{Device, DeviceCapabilities, Medium};
use spin::Mutex;
use alloc::vec::Vec;
use crate::net::NetDevice;

pub struct SmolDevice {
    pub inner: &'static dyn NetDevice,
}

impl Device for SmolDevice {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken;

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

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken { inner: self.inner })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ethernet;
        caps
    }
}

pub struct RxToken {
    data: Vec<u8>,
}

impl smoltcp::phy::RxToken for RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.data)
    }
}

pub struct TxToken {
    inner: &'static dyn NetDevice,
}

impl smoltcp::phy::TxToken for TxToken {
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

pub struct NetworkStack {
    pub iface: Interface,
    pub sockets: SocketSet<'static>,
    pub device: SmolDevice,
}

static STACK: Mutex<Option<NetworkStack>> = Mutex::new(None);

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

pub fn poll() {
    let mut stack_lock = STACK.lock();
    if let Some(stack) = stack_lock.as_mut() {
        let time = Instant::from_micros(0); // TODO: implement real clock
        stack.iface.poll(time, &mut stack.device, &mut stack.sockets);
    }
}

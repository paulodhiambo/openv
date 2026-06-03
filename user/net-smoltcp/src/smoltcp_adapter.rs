#![allow(dead_code)]

use heapless::Vec as HVec;
use smoltcp::phy::{self, ChecksumCapabilities, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

use crate::device::SmolDevice;

pub struct RxToken {
    buf: HVec<u8, 1536>,
}

impl RxToken {
    pub fn new(buf: HVec<u8, 1536>) -> Self {
        RxToken { buf }
    }
}

impl phy::RxToken for RxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buf)
    }
}

pub struct TxToken<'a> {
    dev: &'a mut SmolDevice,
}

impl<'a> TxToken<'a> {
    pub fn new(dev: &'a mut SmolDevice) -> Self {
        TxToken { dev }
    }
}

impl<'a> phy::TxToken for TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // stack buffer
        let mut tmp = [0u8; 1536];
        let r = f(&mut tmp[..len]);
        let _ok = self.dev.send(&tmp[..len]);
        r
    }
}

pub struct SmolPhyDevice {
    inner: SmolDevice,
    mtu: usize,
}

impl SmolPhyDevice {
    pub fn new(dev: SmolDevice, mtu: usize) -> Self {
        SmolPhyDevice { inner: dev, mtu }
    }
}

impl Device for SmolPhyDevice {
    type RxToken<'a>
        = RxToken
    where
        Self: 'a;
    type TxToken<'a>
        = TxToken<'a>
    where
        Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = self.mtu;
        caps.checksum = ChecksumCapabilities::default();
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut tmp_arr = [0u8; 1536];
        if let Some(got) = self.inner.recv(&mut tmp_arr) {
            let mut v: HVec<u8, 1536> = HVec::new();
            v.extend_from_slice(&tmp_arr[..got]).ok();
            let rx = RxToken::new(v);
            let tx = TxToken::new(&mut self.inner);
            Some((rx, tx))
        } else {
            None
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken::new(&mut self.inner))
    }
}

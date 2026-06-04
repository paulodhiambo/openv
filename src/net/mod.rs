pub mod core;
pub mod pktbuf;
pub mod socket;
pub mod virtio_mmio;
pub mod virtio_net;
pub mod virtqueue;

use crate::sync::Mutex;

static NET_DEVICE: Mutex<Option<&'static dyn NetDevice>> = Mutex::new(None);

pub trait NetDevice: Sync {
    fn send(&self, packet: &[u8]);
    fn recv(&self, buf: &mut [u8]) -> usize;
}

pub fn register_device(dev: &'static dyn NetDevice) {
    let mut slot = NET_DEVICE.lock();
    *slot = Some(dev);
}

pub fn device() -> Option<&'static dyn NetDevice> {
    let slot = NET_DEVICE.lock();
    slot.clone()
}

/// Initialize networking via the driver framework; fall back to loopback if nothing claims a device.
pub fn init(dtb_ptr: usize) {
    crate::drivers::probe_all(dtb_ptr);
    if device().is_some() {
        crate::println!("net: device registered via driver framework");
    } else {
        crate::println!("net: no device found; using loopback");
        virtio_net::LoopbackNet::init();
    }
}

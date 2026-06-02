pub mod pktbuf;
pub mod core;
pub mod virtio_net;
pub mod virtio_mmio;
pub mod virtqueue;
pub mod socket;

use spin::Mutex;

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

/// Initialize networking: try virtio-mmio probe; fallback to loopback.
pub fn init() {
    if virtio_mmio::probe_and_init() {
        crate::println!("net: virtio-mmio detected and initialized");
    } else {
        crate::println!("net: no virtio-mmio found; using loopback");
        virtio_net::LoopbackNet::init();
    }
}

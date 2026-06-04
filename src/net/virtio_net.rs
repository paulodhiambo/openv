// Minimal virtio-net driver skeleton. For M1 this provides a loopback device
use crate::net::NetDevice;

pub struct LoopbackNet;

impl LoopbackNet {
    pub const fn new() -> Self {
        LoopbackNet
    }

    /// Initialize and register the loopback device
    pub fn init() {
        static DEVICE: LoopbackNet = LoopbackNet::new();
        crate::net::register_device(&DEVICE);
    }

    /// For later: real init that probes virtio-mmio via FDT
    #[allow(dead_code)]
    pub fn init_virtio_mmio() -> Option<&'static Self> {
        // TODO: probe via FDT, map MMIO, negotiate features, allocate virtqueues
        None
    }
}

impl NetDevice for LoopbackNet {
    fn send(&self, packet: &[u8]) {
        // Loopback: drop or stash packet. For now, store first packet in a static buffer to be returned on recv.
        let mut buf = LOOPBACK_BUF.lock();
        buf.len = 0;
        let copy = core::cmp::min(packet.len(), buf.data.len());
        buf.data[..copy].copy_from_slice(&packet[..copy]);
        buf.len = copy;
    }

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

use crate::sync::Mutex;

struct StaticPkt {
    len: usize,
    data: [u8; super::pktbuf::MAX_PKT_SIZE],
}

impl StaticPkt {
    const fn empty() -> Self {
        Self {
            len: 0,
            data: [0u8; super::pktbuf::MAX_PKT_SIZE],
        }
    }
}

static LOOPBACK_BUF: Mutex<StaticPkt> = Mutex::new(StaticPkt::empty());

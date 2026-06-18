use crate::sync::Mutex;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicI32, Ordering};

pub const SIGNAL_READABLE: u32 = 1 << 0;
pub const SIGNAL_WRITABLE: u32 = 1 << 1;
pub const SIGNAL_PEER_CLOSED: u32 = 1 << 2;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct PortPacket {
    pub key: u64,
    pub type_: u32,
    pub status: i32,
    pub observed_signals: u32,
}

pub struct SignalObserver {
    pub port: Arc<Port>,
    pub key: u64,
    pub trigger_signals: u32,
}

pub struct Port {
    queue: Mutex<VecDeque<PortPacket>>,
    pub waiter: AtomicI32,
}

impl Port {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            waiter: AtomicI32::new(0),
        }
    }

    pub fn queue_packet(&self, packet: PortPacket) {
        let mut q = self.queue.lock();
        q.push_back(packet);
        drop(q);
        // AcqRel: acquire so we observe any store of the waiter's PID, release
        // so the waiter sees the queued packet before being re-scheduled.
        let waiter = self.waiter.swap(0, Ordering::AcqRel);
        if waiter > 0 {
            crate::posix::process::enqueue(waiter);
        }
    }

    pub fn try_recv(&self) -> Option<PortPacket> {
        self.queue.lock().pop_front()
    }
}

use crate::ipc::handle::{Handle, Koid, generate_koid};
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::task::{Poll, Waker};
use spin::Mutex;

/// A message sent over a channel.
pub struct Message {
    pub bytes: Vec<u8>,
    pub handles: Vec<Handle>,
}

/// The internal state of a channel endpoint.
pub struct EndpointState {
    queue: VecDeque<Message>,
    waker: Option<Waker>,
    peer_closed: bool,
}

/// A ChannelEndpoint represents one side of a bidirectional channel.
pub struct ChannelEndpoint {
    koid: Koid,
    state: Mutex<EndpointState>,
    peer: Mutex<Weak<ChannelEndpoint>>,
}

impl ChannelEndpoint {
    /// Creates a new pair of connected channel endpoints.
    pub fn create_pair() -> (Arc<Self>, Arc<Self>) {
        let ep1 = Arc::new(Self {
            koid: generate_koid(),
            state: Mutex::new(EndpointState {
                queue: VecDeque::new(),
                waker: None,
                peer_closed: false,
            }),
            peer: Mutex::new(Weak::new()),
        });

        let ep2 = Arc::new(Self {
            koid: generate_koid(),
            state: Mutex::new(EndpointState {
                queue: VecDeque::new(),
                waker: None,
                peer_closed: false,
            }),
            peer: Mutex::new(Arc::downgrade(&ep1)),
        });

        *ep1.peer.lock() = Arc::downgrade(&ep2);
        (ep1, ep2)
    }

    /// Writes a message to the peer's queue and wakes it if sleeping.
    pub fn write(&self, msg: Message) -> Result<(), &'static str> {
        let peer_arc = self.peer.lock().upgrade().ok_or("Peer closed")?;
        let mut peer_state = peer_arc.state.lock();

        peer_state.queue.push_back(msg);

        if let Some(waker) = peer_state.waker.take() {
            waker.wake();
        }

        Ok(())
    }

    /// Attempts to read a message. Returns `Poll::Pending` and registers the waker if empty.
    pub fn poll_recv(&self, waker: &Waker) -> Poll<Result<Message, &'static str>> {
        let mut state = self.state.lock();

        if let Some(msg) = state.queue.pop_front() {
            Poll::Ready(Ok(msg))
        } else if state.peer_closed {
            Poll::Ready(Err("Peer closed"))
        } else {
            state.waker = Some(waker.clone());
            Poll::Pending
        }
    }

    /// Synchronous read for testing, without involving a real executor.
    pub fn try_recv(&self) -> Option<Message> {
        self.state.lock().queue.pop_front()
    }
}

impl Drop for ChannelEndpoint {
    fn drop(&mut self) {
        if let Some(peer_arc) = self.peer.lock().upgrade() {
            let mut peer_state = peer_arc.state.lock();
            peer_state.peer_closed = true;
            if let Some(waker) = peer_state.waker.take() {
                waker.wake();
            }
        }
    }
}

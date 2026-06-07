//! # Channel-Based IPC
//!
//! This module provides channel-based IPC primitives. Channels are the
//! primary mechanism for inter-process communication in OpenV.
//!
//! ## Overview
//!
//! A channel consists of two [`ChannelEndpoint`]s, one for each side of
//! the communication. Messages sent on one endpoint are received on the
//! other. Channels are bidirectional and can carry both data (bytes) and
//! capabilities ([`Handle`]s).
//!
//! ## Message Format
//!
//! A [`Message`] consists of:
//! - **bytes**: An arbitrary byte payload.
//! - **handles**: A list of [`Handle`]s (capabilities) that are
//!   transferred with the message.
//!
//! When a message with handles is sent, the handles are transferred
//! from the sender to the receiver. This allows processes to pass
//! capabilities to kernel resources (such as VMOs, channels, and
//! interrupts) to each other.
//!
//! ## Synchronous and Asynchronous IPC
//!
//! Channels support both synchronous and asynchronous IPC:
//!
//! - **Asynchronous**: Messages can be queued in the channel's
//!   [`VecDeque`] and read later via [`try_recv`] or [`poll_recv`].
//!  - **Synchronous**: A process can block in `sys_read` waiting for
//!    a message. The [`waiter`] field tracks the PID of a blocked
//!    reader.
//!  - **Async/await**: The [`poll_recv`] method integrates with
//!    Rust's async/await via [`Waker`]s.
//!
//! [`Handle`]: ../handle/struct.Handle.html
//! [`try_recv`]: struct.ChannelEndpoint.html#method.try_recv
//! [`poll_recv`]: struct.ChannelEndpoint.html#method.poll_recv
//! [`waiter`]: struct.ChannelEndpoint.html#field.waiter
//! [`Waker`]: https://doc.rust-lang.org/core/task/struct.Waker.html
//! [`VecDeque`]: https://doc.rust-lang.org/alloc/collections/struct.VecDeque.html

use crate::ipc::handle::{Handle, Koid, generate_koid};
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, Ordering};
use core::task::{Poll, Waker};
use crate::sync::Mutex;

/// A message sent over a channel.
///
/// # Fields
///
/// * `bytes` - The byte payload of the message.
/// * `handles` - The [`Handle`]s (capabilities) transferred with the
///   message.
///
/// [`Handle`]: ../handle/struct.Handle.html
pub struct Message {
    /// The byte payload of the message.
    pub bytes: Vec<u8>,
    /// The [`Handle`]s (capabilities) transferred with the message.
    pub handles: Vec<Handle>,
}

/// The internal state of a channel endpoint.
///
/// This struct is protected by a [`Mutex`] and contains the message
/// queue, waker, and peer-closed flag.
pub struct EndpointState {
    /// Queue of messages waiting to be received.
    queue: VecDeque<Message>,
    /// Waker for async/await integration.
    waker: Option<Waker>,
    /// Whether the peer endpoint has been dropped.
    peer_closed: bool,
}

/// A `ChannelEndpoint` represents one side of a bidirectional channel.
///
/// Each channel has two endpoints. Messages sent on one endpoint are
/// received on the other. When an endpoint is dropped, the peer is
/// notified (via the `peer_closed` flag) and any blocked reader is
/// woken up.
///
/// # Fields
///
/// * `koid` - Kernel object ID, a unique identifier for this endpoint.
/// * `state` - The endpoint's internal state (queue, waker, peer-closed flag).
/// * `peer` - A weak reference to the peer endpoint.
/// * `waiter` - PID of a process blocked in a synchronous `sys_read` on
///   this endpoint (0 = none).
pub struct ChannelEndpoint {
    /// Kernel object ID, a unique identifier for this endpoint.
    #[allow(dead_code)]
    koid: Koid,
    /// The endpoint's internal state.
    state: Mutex<EndpointState>,
    /// A weak reference to the peer endpoint.
    peer: Mutex<Weak<ChannelEndpoint>>,
    /// PID of a process blocked in a synchronous `sys_read` on this endpoint (0 = none).
    pub waiter: AtomicI32,
}

impl ChannelEndpoint {
    /// Creates a new pair of connected channel endpoints.
    ///
    /// # Returns
    ///
    /// A tuple `(ep1, ep2)` of [`Arc<ChannelEndpoint>`]s. Messages sent
    /// on `ep1` are received on `ep2`, and vice versa.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let (ep1, ep2) = ChannelEndpoint::create_pair();
    /// ep1.write(Message { bytes: b"hello".to_vec(), handles: vec![] }).unwrap();
    /// let msg = ep2.try_recv().unwrap();
    /// assert_eq!(msg.bytes, b"hello");
    /// ```
    pub fn create_pair() -> (Arc<Self>, Arc<Self>) {
        let ep1 = Arc::new(Self {
            koid: generate_koid(),
            state: Mutex::new(EndpointState {
                queue: VecDeque::new(),
                waker: None,
                peer_closed: false,
            }),
            peer: Mutex::new(Weak::new()),
            waiter: AtomicI32::new(0),
        });

        let ep2 = Arc::new(Self {
            koid: generate_koid(),
            state: Mutex::new(EndpointState {
                queue: VecDeque::new(),
                waker: None,
                peer_closed: false,
            }),
            peer: Mutex::new(Arc::downgrade(&ep1)),
            waiter: AtomicI32::new(0),
        });

        *ep1.peer.lock() = Arc::downgrade(&ep2);
        (ep1, ep2)
    }

    /// Writes a message to the peer's queue and wakes it if sleeping.
    ///
    /// # Arguments
    ///
    /// * `msg` - The message to send.
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or `Err("Peer closed")` if the peer
    /// endpoint has been dropped.
    ///
    /// # Side Effects
    ///
    /// - Adds the message to the peer's message queue.
///  - Wakes the peer's waker (if any) for async/await readers.
///  - Wakes any synchronous reader blocked in `sys_read`.
    pub fn write(&self, msg: Message) -> Result<(), &'static str> {
        let peer_arc = self.peer.lock().upgrade().ok_or("Peer closed")?;
        let mut peer_state = peer_arc.state.lock();
        peer_state.queue.push_back(msg);
        if let Some(waker) = peer_state.waker.take() {
            waker.wake();
        }
        drop(peer_state);
        // Wake any synchronous (non-async) reader blocked in sys_read.
        let waiter = peer_arc.waiter.swap(0, Ordering::Relaxed);
        if waiter > 0 {
            crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
        }
        Ok(())
    }

    /// Attempts to read a message. Returns `Poll::Pending` and registers
    /// the waker if empty.
    ///
    /// This method integrates with Rust's async/await via [`Waker`]s.
    /// If a message is available, it returns `Poll::Ready(Ok(msg))`. If
    /// the peer has been closed, it returns `Poll::Ready(Err("Peer closed"))`.
    /// Otherwise, it registers the waker and returns `Poll::Pending`.
    ///
    /// # Arguments
    ///
    /// * `waker` - The waker to register if no message is available.
    ///
    /// # Returns
    ///
    /// - `Poll::Ready(Ok(msg))` if a message is available.
///  - `Poll::Ready(Err("Peer closed"))` if the peer has been closed.
///  - `Poll::Pending` if no message is available (the waker is registered).
    ///
    /// [`Waker`]: https://doc.rust-lang.org/core/task/struct.Waker.html
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
    ///
    /// # Returns
    ///
    /// `Some(msg)` if a message is available, `None` otherwise.
    pub fn try_recv(&self) -> Option<Message> {
        self.state.lock().queue.pop_front()
    }
}

impl Drop for ChannelEndpoint {
    /// Handles cleanup when an endpoint is dropped.
    ///
    /// If the peer endpoint still exists, this method:
    /// 1. Sets the peer's `peer_closed` flag to `true`.
    /// 2. Wakes the peer's waker (if any) so async readers can observe
    ///    the close.
    /// 3. Wakes any synchronous reader blocked in `sys_read`.
    fn drop(&mut self) {
        if let Some(peer_arc) = self.peer.lock().upgrade() {
            let mut peer_state = peer_arc.state.lock();
            peer_state.peer_closed = true;
            if let Some(waker) = peer_state.waker.take() {
                waker.wake();
            }
            drop(peer_state);
            // Also wake any synchronous reader so it can observe EOF.
            let waiter = peer_arc.waiter.swap(0, Ordering::Relaxed);
            if waiter > 0 {
                crate::posix::process::RUN_QUEUE.lock().push_back(waiter);
            }
        }
    }
}

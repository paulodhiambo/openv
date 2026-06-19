//! # Inter-Process Communication (IPC)
//!
//! This module provides the inter-process communication (IPC) primitives
//! for OpenV. IPC is the primary mechanism for processes to communicate
//! and synchronize with each other.
//!
//! ## Overview
//!
//! OpenV's IPC system is message-based, inspired by microkernel designs
//! like L4 and Zircon. Processes communicate by sending messages over
//! channels, which can carry both data and capabilities (handles to
//! kernel resources).
//!
//! ## Submodules
//!
//! - [`channel`]: Channel-based IPC primitives, including [`ChannelEndpoint`]
//!   and [`Message`].
//! - [`handle`]: Kernel object handles that can be passed over IPC channels.
//!  - [`msg`]: Low-level message types used by the IPC system.
//!
//! ## Usage
//!
//! Processes typically use IPC as follows:
//!
//! 1. Create a pair of channel endpoints via [`channel::ChannelEndpoint::create_pair`].
//! 2. Pass one endpoint to another process (e.g., via a capability or
//!    environment variable).
//! 3. Send messages via [`channel::ChannelEndpoint::write`] and receive
//!    them via [`channel::ChannelEndpoint::try_recv`].
//!
//! [`channel::ChannelEndpoint::create_pair`]: channel/struct.ChannelEndpoint.html#method.create_pair
//! [`channel::ChannelEndpoint::write`]: channel/struct.ChannelEndpoint.html#method.write
//! [`channel::ChannelEndpoint::try_recv`]: channel/struct.ChannelEndpoint.html#method.try_recv
//! [`ChannelEndpoint`]: channel/struct.ChannelEndpoint.html
//! [`Message`]: channel/struct.Message.html

/// Channel-based IPC primitives.
pub mod channel;
/// Kernel object handles that can be passed over IPC channels.
pub mod handle;
/// Low-level message types used by the IPC system.
pub mod msg;
/// Asynchronous port event multiplexing primitives.
pub mod port;


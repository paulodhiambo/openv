//! # TTY (Terminal) Support
//!
//! This module provides per-session TTY line discipline for OpenV. A TTY
//! (teletypewriter) is a terminal device that provides line editing and
//! signal generation for interactive user sessions.
//!
//! ## Overview
//!
//! Each process session (created via `setsid`) gets its own [`TtyState`]
//! so that Ctrl-C handling, raw/echo mode, and the input buffer are
//! isolated between login sessions. All file descriptors that refer to
//! the same terminal share one [`Arc<TtyState>`].
//!
//! ## TTY State
//!
//! Each [`TtyState`] contains:
//!
//! - **Input buffer**: A [`VecDeque`] of bytes waiting to be read by
//!   user-space processes.
//!  - **Echo mode**: Whether input characters should be echoed back to
//!    the terminal.
//!  - **Raw mode**: Whether input is passed through without line editing.
//!  - **Waiter**: The PID of a process blocked in `sys_read` waiting
//!    for input (0 = nobody).
//!  - **Ctrl-C flag**: Set by the UART ISR when Ctrl-C is pressed;
//!    `sys_read` returns 0 (EOF) when this flag is set.
//!
//! ## Active TTY
//!
//! The [`ACTIVE_TTY`] static holds the current TTY that the UART ISR
//! routes characters to. It is updated by `sys_set_fg_pid` when the
//! foreground process changes.
//!
//! [`TtyState`]: struct.TtyState.html
//! [`Arc<TtyState>`]: https://doc.rust-lang.org/alloc/sync/struct.Arc.html
//! [`VecDeque`]: https://doc.rust-lang.org/alloc/collections/struct.VecDeque.html
//! [`ACTIVE_TTY`]: static.ACTIVE_TTY.html

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32};
use crate::ipc::handle::EpollInstance;
use crate::sync::Mutex;

/// Per-session TTY line discipline state.
///
/// Each process session (created via `setsid`) gets its own [`TtyState`]
/// so that Ctrl-C handling, raw/echo mode, and the input buffer are
/// isolated between login sessions. All file descriptors that refer to
/// the same terminal share one [`Arc<TtyState>`].
///
/// # Fields
///
/// * `buf` - Input buffer containing bytes waiting to be read by
///   user-space processes.
/// * `echo` - Whether input characters should be echoed back to the
///   terminal.
/// * `raw` - Whether input is passed through without line editing.
/// * `waiter` - PID of a process blocked in `sys_read` waiting for
///   input (0 = nobody).
/// * `ctrlc` - Set by the UART ISR when Ctrl-C is pressed; `sys_read`
///   returns 0 (EOF) when this flag is set.
///
/// # Thread Safety
///
/// The input buffer is protected by a [`Mutex`], while the other fields
/// use atomic operations. This allows the UART ISR to update the
/// `ctrlc` flag without holding a lock.
///
/// [`Arc<TtyState>`]: https://doc.rust-lang.org/alloc/sync/struct.Arc.html
/// [`Mutex`]: ../sync/struct.Mutex.html
pub struct TtyState {
    /// Input buffer containing bytes waiting to be read by user-space processes.
    pub buf:    Mutex<VecDeque<u8>>,
    /// Whether input characters should be echoed back to the terminal.
    pub echo:   AtomicBool,
    /// Whether input is passed through without line editing.
    pub raw:    AtomicBool,
    /// PID of a process blocked in `sys_read` waiting for input (0 = nobody).
    pub waiter: AtomicI32,
    /// Set by the UART ISR when Ctrl-C is pressed; `sys_read` returns 0 (EOF).
    pub ctrlc:  AtomicBool,
    /// Epoll instances waiting for readability on this TTY.
    pub epoll_waiters: Mutex<Vec<Weak<EpollInstance>>>,
}

impl TtyState {
    /// Creates a new [`TtyState`] with default settings.
    ///
    /// # Returns
    ///
    /// An [`Arc<TtyState>`] with the following default settings:
    /// - Echo mode enabled
    /// - Raw mode disabled
    /// - No waiter (0)
    /// - Ctrl-C flag cleared
    /// - Empty input buffer
    ///
    /// [`Arc<TtyState>`]: https://doc.rust-lang.org/alloc/sync/struct.Arc.html
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            buf:    Mutex::new(VecDeque::new()),
            echo:   AtomicBool::new(true),
            raw:    AtomicBool::new(false),
            waiter: AtomicI32::new(0),
            ctrlc:  AtomicBool::new(false),
            epoll_waiters: Mutex::new(Vec::new()),
        })
    }
}

/// The TTY that the UART ISR routes characters to.
///
/// This static holds the current TTY that the UART ISR routes characters
/// to. It is updated by `sys_set_fg_pid` when the foreground process
/// changes. It is wrapped in a [`Mutex`] to allow safe access from any
/// context.
///
/// [`Mutex`]: ../sync/struct.Mutex.html
pub static ACTIVE_TTY: Mutex<Option<Arc<TtyState>>> = Mutex::new(None);

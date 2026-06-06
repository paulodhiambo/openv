/// Per-session TTY line discipline.
///
/// Each process session (setsid) gets its own TtyState so that Ctrl-C, raw/echo
/// mode, and the input buffer are isolated between login sessions.  All fds that
/// refer to the same terminal share one Arc<TtyState>.
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32};
use crate::sync::Mutex;

pub struct TtyState {
    pub buf:    Mutex<VecDeque<u8>>,
    pub echo:   AtomicBool,
    pub raw:    AtomicBool,
    /// PID of a process blocked in sys_read waiting for input (0 = nobody).
    pub waiter: AtomicI32,
    /// Set by the UART ISR when Ctrl-C is pressed; sys_read returns 0 (EOF).
    pub ctrlc:  AtomicBool,
}

impl TtyState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            buf:    Mutex::new(VecDeque::new()),
            echo:   AtomicBool::new(true),
            raw:    AtomicBool::new(false),
            waiter: AtomicI32::new(0),
            ctrlc:  AtomicBool::new(false),
        })
    }
}

/// The TTY that the UART ISR routes characters to.
/// Updated by sys_set_fg_pid when the foreground process changes.
pub static ACTIVE_TTY: Mutex<Option<Arc<TtyState>>> = Mutex::new(None);

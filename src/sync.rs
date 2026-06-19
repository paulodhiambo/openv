//! # Synchronization Primitives
//!
//! This module provides synchronization primitives for the OpenV kernel,
//! including a deadlock-detecting [`Mutex`] wrapper. The kernel is a
//! multi-hart (multi-threaded) environment, so careful synchronization
//! is required to prevent race conditions and deadlocks.
//!
//! ## Lock Ordering
//!
//! To avoid deadlocks, locks must be acquired **only** in the order
//! listed below. Violating this order will cause a spin-deadlock.
//! The recursive-lock detector in [`Mutex::lock`] will catch same-HART
//! violations, but cross-HART ones will livelock.
//!
//! 1. `PROCESS_TABLE` (outermost — covers the whole process namespace)
//! 2. Per-process locks, one at a time (state, trap_frame, fds, ipc_state,
//!    senders, children, wait_target, wait_result, mailbox)
//! 3. `RUN_QUEUE` (leaf — never held while acquiring #1 or #2)
//! 4. `SATP_REFCOUNT` (leaf — never held while acquiring #1)
//! 5. `FUTEX_TABLE` (leaf)
//! 6. `SLEEP_QUEUE` (leaf)
//! 7. `PAGE_REF_COUNTS` then `FREE_LIST` (PMM — must acquire PAGE_REF_COUNTS
//!    before FREE_LIST when holding both simultaneously; alloc_frame acquires
//!    them sequentially so it does not hold both at once)
//!
//! ### Rules
//!
//! - Never lock `PROCESS_TABLE` while already holding `RUN_QUEUE`.
//! - Never hold two different per-process locks simultaneously; drop one first.
//! - IPC paths (`sys_send`, `sys_recv`): lock `sender.ipc_state` before
//!   `receiver.senders` — always in the direction sender → receiver, never
//!   both at once.
//! - TTY / `ACTIVE_TTY`: treated as leaf; never acquire while holding
//!   `PROCESS_TABLE`.
//!
//! ## Deadlock Detection
//!
//! The [`Mutex`] wrapper tracks the owning HART (hardware thread) and
//! panics if a HART attempts to acquire a lock it already holds. This
//! detects same-HART recursive locking deadlocks. Cross-HART deadlocks
//! are not detected and must be prevented by following the lock ordering
//! rules above.
//!
//! ## Usage
//!
//! Use [`Mutex`] instead of `spin::Mutex` directly to get deadlock detection:
//!
//! ```ignore
//! use crate::sync::Mutex;
//!
//! static COUNTER: Mutex<u32> = Mutex::new(0);
//!
//! let mut guard = COUNTER.lock();
//! *guard += 1;
//! // guard is automatically released when it goes out of scope
//! ```
//!
//! [`Mutex::lock`]: struct.Mutex.html#method.lock

use core::sync::atomic::{AtomicUsize, Ordering};

/// A [`Mutex`] wrapper around `spin::Mutex` that tracks the owning HART
/// (hardware thread) to detect and prominently panic on recursive locking
/// deadlocks.
///
/// # How It Works
///
/// Each [`Mutex`] has an `owner` field that stores the HART ID of the
/// HART that currently holds the lock. When a HART attempts to acquire
/// the lock, the [`lock`] method checks if the HART is already the
/// owner. If so, it prints a deadlock message and panics.
///
/// # Limitations
///
/// - Only detects same-HART recursive locking deadlocks.
/// - Does not detect cross-HART deadlocks. Follow the lock ordering
///   rules in the module-level documentation to prevent these.
/// - The deadlock detection is best-effort and may have false negatives
///   in rare race conditions.
///
/// # Examples
///
/// ```ignore
/// static COUNTER: Mutex<u32> = Mutex::new(0);
///
/// let mut guard = COUNTER.lock();
/// *guard += 1;
/// // guard is automatically released when it goes out of scope
/// ```
///
/// [`lock`]: struct.Mutex.html#method.lock
/// [`Mutex`]: struct.Mutex.html
pub struct Mutex<T> {
    /// The underlying `spin::Mutex`.
    inner: spin::Mutex<T>,
    /// The HART ID of the current owner, or `usize::MAX` if unlocked.
    owner: AtomicUsize,
}

impl<T> Mutex<T> {
    /// Creates a new [`Mutex`] in an unlocked state.
    ///
    /// # Arguments
    ///
    /// * `value` - The initial value to be stored in the mutex.
    ///
    /// # Returns
    ///
    /// A new [`Mutex`] containing the provided value.
    ///
    /// [`Mutex`]: struct.Mutex.html
    pub const fn new(value: T) -> Self {
        Self {
            inner: spin::Mutex::new(value),
            // usize::MAX is used as a sentinel value to indicate that
            // no HART currently holds the lock.
            owner: AtomicUsize::new(usize::MAX),
        }
    }

    /// Acquires the mutex, blocking the current thread until it can be acquired.
    ///
    /// If the current HART already holds the mutex, this function prints
    /// a deadlock message and panics. Otherwise, it blocks until the
    /// mutex is available, then returns a [`MutexGuard`].
    ///
    /// # Returns
    ///
    /// A [`MutexGuard`] that can be dereferenced to access the protected value.
    ///
    /// # Panics
    ///
    /// Panics if the current HART already holds the mutex (recursive lock).
    ///
    /// [`MutexGuard`]: struct.MutexGuard.html
    #[track_caller]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let hart = crate::smp::current_hartid();
        let owner = self.owner.load(Ordering::Relaxed);
        
        // Check for recursive locking deadlock. If the current HART
        // already holds the lock, print a deadlock message and panic.
        if owner == hart {
            crate::raw_print!("DEADLOCK: hart=");
            crate::uart::write_dec(hart);
            crate::raw_print!(" owner=");
            crate::uart::write_dec(owner);
            crate::raw_print!(" caller=");
            crate::uart::write_str(core::panic::Location::caller().file());
            crate::raw_print!(":");
            crate::uart::write_dec(core::panic::Location::caller().line() as usize);
            crate::raw_print!("\n");
            panic!("Recursive lock deadlock detected!");
        }

        // Acquire the underlying spin::Mutex. This will spin until the
        // lock is available.
        let guard = self.inner.lock();
        // Record the current HART as the owner. We use Relaxed ordering
        // because the spin::Mutex already provides the necessary synchronization.
        self.owner.store(hart, Ordering::Relaxed);

        MutexGuard {
            mutex: self,
            guard,
        }
    }

    /// Attempts to acquire the mutex without blocking.
    ///
    /// If the mutex is already locked by the current HART, this returns
    /// `None` (to prevent recursive locking). If the mutex is locked by
    /// another HART, this returns `None`. If the mutex is unlocked, this
    /// acquires it and returns `Some(MutexGuard)`.
    ///
    /// # Returns
    ///
    /// `Some(MutexGuard)` if the mutex was successfully acquired,
    /// `None` otherwise.
    ///
    /// [`MutexGuard`]: struct.MutexGuard.html
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let hart = crate::smp::current_hartid();
        // If the current HART already holds the lock, return None to
        // prevent recursive locking.
        if self.owner.load(Ordering::Relaxed) == hart {
            return None;
        }
        
        // Try to acquire the underlying spin::Mutex without blocking.
        self.inner.try_lock().map(|guard| {
            // Record the current HART as the owner.
            self.owner.store(hart, Ordering::Relaxed);
            MutexGuard {
                mutex: self,
                guard,
            }
        })
    }
}

/// A guard that provides access to the data protected by a [`Mutex`].
///
/// This guard is returned by [`Mutex::lock`] and [`Mutex::try_lock`].
/// It implements [`Deref`] and [`DerefMut`] to allow access to the
/// protected data. When the guard is dropped, the mutex is released.
///
/// [`Mutex`]: struct.Mutex.html
/// [`Mutex::lock`]: struct.Mutex.html#method.lock
/// [`Mutex::try_lock`]: struct.Mutex.html#method.try_lock
/// [`Deref`]: https://doc.rust-lang.org/core/ops/trait.Deref.html
/// [`DerefMut`]: https://doc.rust-lang.org/core/ops/trait.DerefMut.html
pub struct MutexGuard<'a, T> {
    /// Reference to the [`Mutex`] that created this guard.
    mutex: &'a Mutex<T>,
    /// The underlying `spin::MutexGuard`.
    guard: spin::MutexGuard<'a, T>,
}

impl<'a, T> core::ops::Deref for MutexGuard<'a, T> {
    type Target = T;
    /// Dereferences the guard to access the protected data.
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<'a, T> core::ops::DerefMut for MutexGuard<'a, T> {
    /// Dereferences the guard mutably to access the protected data.
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    /// Releases the mutex when the guard is dropped.
    ///
    /// This clears the owner field before releasing the underlying
    /// spin::Mutex. This is important to allow other HARTs to acquire
    /// the lock.
    fn drop(&mut self) {
        // Clear the owner before releasing the lock. We use a sentinel
        // value (usize::MAX) to indicate that no HART currently holds
        // the lock.
        self.mutex.owner.store(usize::MAX, Ordering::Relaxed);
    }
}

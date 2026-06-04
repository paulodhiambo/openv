use core::sync::atomic::{AtomicUsize, Ordering};

/// A Mutex wrapper around `spin::Mutex` that tracks the owning HART (hardware thread)
/// to detect and prominently panic on recursive locking deadlocks.
pub struct Mutex<T> {
    inner: spin::Mutex<T>,
    owner: AtomicUsize,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: spin::Mutex::new(value),
            owner: AtomicUsize::new(usize::MAX),
        }
    }

    #[track_caller]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let hart = crate::smp::current_hartid();
        let owner = self.owner.load(Ordering::Relaxed);
        
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

        let guard = self.inner.lock();
        self.owner.store(hart, Ordering::Relaxed);

        MutexGuard {
            mutex: self,
            guard,
        }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let hart = crate::smp::current_hartid();
        if self.owner.load(Ordering::Relaxed) == hart {
            return None;
        }
        
        self.inner.try_lock().map(|guard| {
            self.owner.store(hart, Ordering::Relaxed);
            MutexGuard {
                mutex: self,
                guard,
            }
        })
    }
}

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
    guard: spin::MutexGuard<'a, T>,
}

impl<'a, T> core::ops::Deref for MutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<'a, T> core::ops::DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        // Clear the owner before releasing the lock
        self.mutex.owner.store(usize::MAX, Ordering::Relaxed);
    }
}

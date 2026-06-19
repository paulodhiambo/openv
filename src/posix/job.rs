use crate::sync::Mutex;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

pub struct Job {
    pub parent: Mutex<Option<Weak<Job>>>,
    pub children: Mutex<Vec<Arc<Job>>>,
    pub processes: Mutex<Vec<i32>>, // PIDs of processes in this Job
    
    // Resource Limits (policy)
    pub max_memory: AtomicUsize,
    pub current_memory: AtomicUsize,
}

impl Job {
    pub fn new(parent: Option<Weak<Job>>) -> Self {
        Self {
            parent: Mutex::new(parent),
            children: Mutex::new(Vec::new()),
            processes: Mutex::new(Vec::new()),
            max_memory: AtomicUsize::new(usize::MAX),
            current_memory: AtomicUsize::new(0),
        }
    }

    /// Checks if adding `bytes` would exceed the job's or any of its ancestors' memory limits.
    pub fn check_memory_limit(&self, bytes: usize) -> bool {
        let limit = self.max_memory.load(Ordering::Relaxed);
        let current = self.current_memory.load(Ordering::Relaxed);
        if current.saturating_add(bytes) > limit {
            return false;
        }
        
        let mut curr = self.parent.lock().as_ref().and_then(|w| w.upgrade());
        while let Some(job) = curr {
            let limit = job.max_memory.load(Ordering::Relaxed);
            let current = job.current_memory.load(Ordering::Relaxed);
            if current.saturating_add(bytes) > limit {
                return false;
            }
            let next = job.parent.lock().as_ref().and_then(|w| w.upgrade());
            curr = next;
        }
        true
    }

    /// Allocates memory usage on this job and all its ancestors.
    pub fn alloc_memory(&self, bytes: usize) {
        self.current_memory.fetch_add(bytes, Ordering::SeqCst);
        
        let mut curr = self.parent.lock().as_ref().and_then(|w| w.upgrade());
        while let Some(job) = curr {
            job.current_memory.fetch_add(bytes, Ordering::SeqCst);
            let next = job.parent.lock().as_ref().and_then(|w| w.upgrade());
            curr = next;
        }
    }

    /// Deallocates memory usage on this job and all its ancestors.
    pub fn dealloc_memory(&self, bytes: usize) {
        self.current_memory.fetch_sub(bytes, Ordering::SeqCst);
        
        let mut curr = self.parent.lock().as_ref().and_then(|w| w.upgrade());
        while let Some(job) = curr {
            job.current_memory.fetch_sub(bytes, Ordering::SeqCst);
            let next = job.parent.lock().as_ref().and_then(|w| w.upgrade());
            curr = next;
        }
    }
}

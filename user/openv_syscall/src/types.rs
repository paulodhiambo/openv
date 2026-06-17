/// Time value structure (seconds + microseconds).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TimeVal {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// Time specification structure (seconds + nanoseconds).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// POSIX sigaction structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SigAction {
    pub sa_handler: usize,
    pub sa_flags: usize,
    pub sa_mask: u32,
    pub sa_restorer: usize,
}

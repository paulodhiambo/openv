//! POSIX-compatible error return values for kernel syscall handlers.
//!
//! All syscalls return a `usize` in `tf.regs[10]` (a0). Errors are encoded as
//! large unsigned values that wrap around from `usize::MAX` downwards, matching
//! the convention used by most RISC-V Linux user-space libraries (libc treats
//! any return value in the range `[usize::MAX - 4095, usize::MAX]` as `-errno`).
//!
//! These constants are named after the POSIX error symbols for readability; the
//! exact numeric values below match the common Linux/glibc mapping on RISC-V.

/// Operation not permitted.
pub const EPERM: usize = usize::MAX - 0;   // -1
/// No such file or directory.
pub const ENOENT: usize = usize::MAX - 1;  // -2
/// No such process.
pub const ESRCH: usize = usize::MAX - 2;   // -3
/// Interrupted system call (used for blocking-retry paths).
pub const EINTR: usize = usize::MAX - 3;   // -4
/// I/O error.
pub const EIO: usize = usize::MAX - 4;     // -5
/// Bad file descriptor.
pub const EBADF: usize = usize::MAX - 8;   // -9
/// Out of memory.
pub const ENOMEM: usize = usize::MAX - 11; // -12
/// Permission denied.
pub const EACCES: usize = usize::MAX - 12; // -13
/// Bad address / pointer fault.
pub const EFAULT: usize = usize::MAX - 13; // -14
/// Invalid argument.
pub const EINVAL: usize = usize::MAX - 21; // -22
/// Too many open files.
pub const EMFILE: usize = usize::MAX - 23; // -24
/// Function not implemented.
pub const ENOSYS: usize = usize::MAX - 37; // -38
/// Connection refused.
pub const ECONNREFUSED: usize = usize::MAX - 110; // -111

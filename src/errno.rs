//! # POSIX Error Codes
//!
//! This module defines POSIX-compatible error return values for kernel syscall
//! handlers. OpenV follows the POSIX convention for error codes, which allows
//! user-space programs to use standard C library functions like `strerror` and
//! `perror` to handle errors returned by the kernel.
//!
//! ## Error Encoding
//!
//! All syscalls return a `usize` in `tf.regs[10]` (a0). Errors are encoded as
//! large unsigned values that wrap around from `usize::MAX` downwards, matching
//! the convention used by most RISC-V Linux user-space libraries (libc treats
//! any return value in the range `[usize::MAX - 4095, usize::MAX]` as `-errno`).
//!
//! For example, `EPERM` (Operation not permitted) is encoded as `usize::MAX - 0`,
//! which is `0xFFFF_FFFF_FFFF_FFFF` on a 64-bit system. When interpreted as a
//! signed integer, this is `-1`.
//!
//! ## Error Code Values
//!
//! The numeric values below match the common Linux/glibc mapping on RISC-V.
//! This ensures compatibility with existing user-space code that depends on
//! specific errno values.
//!
//! ## Usage
//!
//! Syscall handlers should return one of these constants when an error occurs.
//! For example:
//!
//! ```ignore
//! if ptr.is_null() {
//!     return Err(usize::MAX - EFAULT);
//! }
//! ```
//!
//! User-space code can then check the return value against these constants to
//! determine what went wrong.

/// Operation not permitted.
///
/// Returned when a process attempts to perform an operation that it does not
/// have permission to perform, such as accessing a resource it doesn't own
/// or using a capability it doesn't have.
///
/// **POSIX name**: `EPERM` (1)
pub const EPERM: usize = usize::MAX - 0;   // -1

/// No such file or directory.
///
/// Returned when a file or directory is requested but does not exist. This
/// is one of the most common errors and is returned by syscalls like `open`,
/// `stat`, and `chdir`.
///
/// **POSIX name**: `ENOENT` (2)
pub const ENOENT: usize = usize::MAX - 1;  // -2

/// No such process.
///
/// Returned when a process ID is requested but does not correspond to an
/// active process. This is returned by syscalls like `kill`, `wait`, and
/// `getpriority`.
///
/// **POSIX name**: `ESRCH` (3)
pub const ESRCH: usize = usize::MAX - 2;   // -3

/// Interrupted system call (used for blocking-retry paths).
///
/// Returned when a blocking syscall is interrupted by a signal or other
/// event. The caller should retry the syscall if appropriate.
///
/// **POSIX name**: `EINTR` (4)
pub const EINTR: usize = usize::MAX - 3;   // -4

/// Input/output error.
///
/// Returned when an I/O operation fails for an unspecified reason. This is
/// a catch-all error for I/O failures that don't have a more specific
/// error code.
///
/// **POSIX name**: `EIO` (5)
pub const EIO: usize = usize::MAX - 4;     // -5

/// Bad file descriptor.
///
/// Returned when a file descriptor is invalid or has been closed. This is
/// returned by syscalls like `read`, `write`, `close`, and `fstat`.
///
/// **POSIX name**: `EBADF` (9)
pub const EBADF: usize = usize::MAX - 8;   // -9

/// Out of memory.
///
/// Returned when a memory allocation fails. This is typically a fatal
/// error and may cause the calling process to be terminated.
///
/// **POSIX name**: `ENOMEM` (12)
pub const ENOMEM: usize = usize::MAX - 11; // -12

/// Permission denied.
///
/// Returned when a process attempts to access a resource that it has
/// permission to use in principle, but the specific access mode is not
/// allowed. For example, reading from a file that is write-only.
///
/// **POSIX name**: `EACCES` (13)
pub const EACCES: usize = usize::MAX - 12; // -13

/// Bad address / pointer fault.
///
/// Returned when a pointer argument to a syscall is invalid. This includes
/// null pointers, pointers to unmapped memory, and pointers that are not
/// aligned correctly for the operation being performed.
///
/// **POSIX name**: `EFAULT` (14)
pub const EFAULT: usize = usize::MAX - 13; // -14

/// Invalid argument.
///
/// Returned when an argument to a syscall is invalid for a reason other
/// than a bad pointer. This includes values that are out of range, invalid
/// flags, and unsupported operations.
///
/// **POSIX name**: `EINVAL` (22)
pub const EINVAL: usize = usize::MAX - 21; // -22

/// Resource temporarily unavailable (non-blocking retry).
///
/// Returned when an operation would block but the file descriptor or
/// resource is in non-blocking mode. The caller should retry the operation
/// later.
///
/// **POSIX name**: `EAGAIN` (11)
pub const EAGAIN: usize = usize::MAX - 10; // -11

/// Too many open files.
///
/// Returned when a process attempts to open more file descriptors than
/// the system limit allows. This limit is typically set by `RLIMIT_NOFILE`
/// or the system-wide `fs.file-max` setting.
///
/// **POSIX name**: `EMFILE` (24)
pub const EMFILE: usize = usize::MAX - 23; // -24

/// Function not implemented.
///
/// Returned when a syscall is not implemented in the current kernel build.
/// This is useful for user-space code that wants to detect which features
/// are available.
///
/// **POSIX name**: `ENOSYS` (38)
pub const ENOSYS: usize = usize::MAX - 37; // -38

/// Connection refused.
///
/// Returned when a connection attempt is refused by the remote host.
/// This typically happens when the remote host is not listening on the
/// specified port, or when a firewall is blocking the connection.
///
/// **POSIX name**: `ECONNREFUSED` (111)
pub const ECONNREFUSED: usize = usize::MAX - 110; // -111

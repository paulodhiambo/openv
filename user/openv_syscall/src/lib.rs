#![no_std]

use core::arch::asm;

pub mod types;

// ── Syscall numbers ──────────────────────────────────────────────────────────

// Process / yield
pub const SYS_YIELD: usize = 0;
pub const SYS_EXIT: usize = 1;
pub const SYS_SPAWN: usize = 6;
pub const SYS_SPAWN_WITH_CAPS: usize = 7;
pub const SYS_FORK: usize = 50;
pub const SYS_EXEC: usize = 51;
pub const SYS_WAITPID: usize = 52;
pub const SYS_GETPID: usize = 53;
pub const SYS_GETPPID: usize = 54;
pub const SYS_PRIVCTL: usize = 59;
pub const SYS_SET_FG_PID: usize = 60;
pub const SYS_CLONE: usize = 120;
pub const SYS_FUTEX: usize = 121;
pub const SYS_GETTID: usize = 122;

// File I/O
pub const SYS_LSEEK: usize = 19;
pub const SYS_WRITE: usize = 2;
pub const SYS_READ: usize = 5;
pub const SYS_CLOSE: usize = 9;
pub const SYS_CHDIR: usize = 55;
pub const SYS_GETCWD: usize = 56;
pub const SYS_FSYNC: usize = 130;
pub const SYS_FDATASYNC: usize = 131;
pub const SYS_FSTAT: usize = 78;
pub const SYS_FSTATAT: usize = 79;
pub const SYS_POLL: usize = 68;

// Memory management
pub const SYS_MMAP: usize = 135;
pub const SYS_MUNMAP: usize = 136;
pub const SYS_BRK: usize = 137;
pub const SYS_ABI_VERSION: usize = 138;
pub const SYS_UNSHARE: usize = 139;

// IPC
pub const SYS_PIPE: usize = 3;
pub const SYS_DUP: usize = 57;
pub const SYS_DUP2: usize = 58;
pub const SYS_FCNTL: usize = 81;
pub const SYS_MAILBOX_SEND: usize = 61;
pub const SYS_MAILBOX_RECV: usize = 62;
pub const SYS_PORT_CREATE: usize = 70;
pub const SYS_PORT_WAIT: usize = 71;
pub const SYS_PORT_QUEUE: usize = 72;
pub const SYS_PORT_BIND: usize = 73;
pub const SYS_HANDLE_DUPLICATE: usize = 76;
pub const SYS_HANDLE_GET_RIGHTS: usize = 77;
pub const SYS_IPC_SEND: usize = 90;
pub const SYS_IPC_RECV: usize = 91;
pub const SYS_IPC_SENDREC: usize = 92;
pub const SYS_DATACOPY: usize = 93;
pub const SYS_CHANNEL_CREATE: usize = 94;
pub const SYS_CHANNEL_WRITE: usize = 95;
pub const SYS_CHANNEL_READ: usize = 96;

// Networking
pub const SYS_NET_SEND: usize = 10;
pub const SYS_NET_RECV: usize = 11;
pub const SYS_SOCKET: usize = 40;
pub const SYS_DAEMON_NEXT_SOCKET: usize = 41;
pub const SYS_DAEMON_CREATE_CONN: usize = 42;
pub const SYS_ACCEPT: usize = 43;
pub const SYS_BIND: usize = 44;
pub const SYS_LISTEN: usize = 45;
pub const SYS_CONNECT: usize = 46;
pub const SYS_SOCK_SEND: usize = 47;
pub const SYS_SOCK_RECV: usize = 48;
pub const SYS_TRY_RECV: usize = 49;

// Signals / time
pub const SYS_GETTIMEOFDAY: usize = 82;
pub const SYS_NANOSLEEP: usize = 83;
pub const SYS_SETPGID: usize = 84;
pub const SYS_GETPGID: usize = 85;
pub const SYS_SETSID: usize = 86;
pub const SYS_KILL: usize = 87;
pub const SYS_SIGACTION: usize = 88;
pub const SYS_SIGRETURN: usize = 89;
pub const SYS_SIGPROCMASK: usize = 69;
pub const SYS_SIGPENDING: usize = 97;
pub const SYS_ARCH_PRCTL: usize = 98;

// User / credentials
pub const SYS_SETUID: usize = 23;
pub const SYS_SETGID: usize = 24;
pub const SYS_GETUID: usize = 30;
pub const SYS_GETEUID: usize = 31;
pub const SYS_GETGID: usize = 32;
pub const SYS_GETEGID: usize = 33;
pub const SYS_AUTHENTICATE: usize = 34;
pub const SYS_CAN_SUDO: usize = 35;

// TTY
pub const SYS_SET_ECHO: usize = 37;
pub const SYS_SET_RAW: usize = 38;

// VFS / filesystem servers
pub const SYS_VFS_REGISTER: usize = 63;
pub const SYS_GET_VFS_PID: usize = 64;
pub const SYS_INITRD_DATA: usize = 65;
pub const SYS_PM_REGISTER: usize = 66;
pub const SYS_GET_PM_PID: usize = 67;
pub const SYS_PROC_LIST: usize = 110;
pub const SYS_PROC_STATUS: usize = 111;
pub const SYS_BLK_REGISTER: usize = 112;
pub const SYS_GET_BLK_PID: usize = 113;
pub const SYS_PROC_SERVER_REGISTER: usize = 114;
pub const SYS_GET_PROC_SERVER_PID: usize = 115;
pub const SYS_DEV_SERVER_REGISTER: usize = 116;
pub const SYS_GET_DEV_SERVER_PID: usize = 117;

// Process manager primitives (kernel-only, for pm-server)
pub const SYS_CLONE_PROCESS: usize = 100;
pub const SYS_SET_TRAPFRAME: usize = 101;
pub const SYS_SET_PROCESS_STATE: usize = 102;
pub const SYS_DESTROY_USER_SPACE: usize = 103;
pub const SYS_ALLOC_USER_PAGE: usize = 104;
pub const SYS_REAP_PROCESS: usize = 105;
pub const SYS_PHYS_MAP: usize = 106;
pub const SYS_IRQ_REGISTER: usize = 107;
pub const SYS_IRQ_ENABLE: usize = 108;
pub const SYS_VIRT_TO_PHYS: usize = 109;

// Observability (eBPF-compatible tracepoints)
pub const SYS_TRACE_LOAD: usize = 140;
pub const SYS_TRACE_ATTACH: usize = 141;
pub const SYS_TRACE_READ: usize = 142;

// Epoll I/O event notification
pub const SYS_EPOLL_CTL: usize = 233;
pub const SYS_EPOLL_PWAIT: usize = 244;
pub const SYS_EPOLL_CREATE1: usize = 291;

// VMO / VMAR (Zircon-compatible virtual memory objects)
pub const SYS_VMO_CREATE: usize = 150;
pub const SYS_VMO_MAP: usize = 151;
pub const SYS_VMO_UNMAP: usize = 152;
pub const SYS_VMO_READ: usize = 153;
pub const SYS_VMO_WRITE: usize = 154;
pub const SYS_HANDLE_CLOSE: usize = 155;

// Thread lifecycle
pub const SYS_EXIT_GROUP: usize = 158;
pub const SYS_SET_TID_ADDRESS: usize = 159;
pub const SYS_TGKILL: usize = 160;

// Object signal wait (Zircon-compatible)
pub const SYS_OBJECT_WAIT_ONE: usize = 161;

// Task kill
pub const SYS_TASK_KILL: usize = 162;

// Component manager
pub const SYS_CM_REGISTER: usize = 163;
pub const SYS_GET_CM_PID: usize = 164;

// Driver framework isolation
pub const SYS_CREATE_RESOURCE: usize = 165;
pub const SYS_GRANT_DRIVER_CAP: usize = 166;

// Priority scheduling + capability delegation
pub const SYS_SETPRIORITY: usize = 167;
pub const SYS_GETPRIORITY: usize = 168;
pub const SYS_CAP_GRANT: usize = 169;
pub const SYS_GETNSID: usize = 170;

// ── Job / handle management (Zircon/Fuchsia-compatible) ─────────────────────
pub const SYS_JOB_CREATE: usize = 74;
pub const SYS_JOB_SET_POLICY: usize = 75;

/// Current kernel ABI version. Must match `KERNEL_ABI_VERSION` in the kernel.
pub const KERNEL_ABI_VERSION: usize = 1;

// ── Error type ───────────────────────────────────────────────────────────────

/// POSIX errno values encoded as kernel return values.
///
/// The kernel returns large `usize` values encoding negative errno:
/// `usize::MAX - (posix_errno - 1)` = `-posix_errno` when interpreted as `isize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    EPERM,
    ENOENT,
    ESRCH,
    EINTR,
    EIO,
    EBADF,
    ENOMEM,
    EACCES,
    EFAULT,
    EAGAIN,
    EINVAL,
    EMFILE,
    ENOSYS,
    ECONNREFUSED,
    Other(i32),
}

/// Convert a raw kernel usize return value to a `Result`.
///
/// Returns `Ok(value)` if the value represents success (≤ usize::MAX - 4095),
/// or `Err(error)` if the value encodes a negative errno.
pub fn result_from(ret: usize) -> Result<usize, Error> {
    if ret > (usize::MAX - 4096) {
        Err(error_from_ret(ret))
    } else {
        Ok(ret)
    }
}

/// Interpret a kernel return value as an `Error`.
fn error_from_ret(ret: usize) -> Error {
    let err = usize::MAX.wrapping_sub(ret) as i32;
    match err {
        1 => Error::EPERM,
        2 => Error::ENOENT,
        3 => Error::ESRCH,
        4 => Error::EINTR,
        5 => Error::EIO,
        9 => Error::EBADF,
        11 => Error::EAGAIN,
        12 => Error::ENOMEM,
        13 => Error::EACCES,
        14 => Error::EFAULT,
        22 => Error::EINVAL,
        24 => Error::EMFILE,
        38 => Error::ENOSYS,
        111 => Error::ECONNREFUSED,
        other => Error::Other(other),
    }
}

// ── Raw syscall wrappers ─────────────────────────────────────────────────────

/// Perform a syscall with 0 arguments (besides the syscall number in a7).
///
/// # Safety
///
/// This is a raw kernel interface. The caller must ensure the arguments
/// are valid for the given syscall number.
#[inline]
pub unsafe fn syscall0(n: usize) -> usize {
    let ret: usize;
    unsafe {
        asm!("ecall", inlateout("a0") 0usize => ret, in("a7") n, options(nostack));
    }
    ret
}

/// Perform a syscall with 1 argument.
///
/// Argument layout: `a0 = arg0`.
#[inline]
pub unsafe fn syscall1(n: usize, arg0: usize) -> usize {
    let ret: usize;
    unsafe {
        asm!("ecall", inlateout("a0") arg0 => ret, in("a7") n, options(nostack));
    }
    ret
}

/// Perform a syscall with 2 arguments.
///
/// Argument layout: `a0 = arg0`, `a1 = arg1`.
#[inline]
pub unsafe fn syscall2(n: usize, arg0: usize, arg1: usize) -> usize {
    let ret: usize;
    unsafe {
        asm!("ecall", inlateout("a0") arg0 => ret, in("a1") arg1, in("a7") n, options(nostack));
    }
    ret
}

/// Perform a syscall with 3 arguments.
///
/// Argument layout: `a0 = arg0`, `a1 = arg1`, `a2 = arg2`.
#[inline]
pub unsafe fn syscall3(n: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    let ret: usize;
    unsafe {
        asm!("ecall", inlateout("a0") arg0 => ret, in("a1") arg1, in("a2") arg2, in("a7") n, options(nostack));
    }
    ret
}

/// Perform a syscall with 4 arguments.
///
/// Argument layout: `a0 = arg0`, `a1 = arg1`, `a2 = arg2`, `a3 = arg3`.
#[inline]
pub unsafe fn syscall4(n: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize) -> usize {
    let ret: usize;
    unsafe {
        asm!("ecall", inlateout("a0") arg0 => ret, in("a1") arg1, in("a2") arg2, in("a3") arg3, in("a7") n, options(nostack));
    }
    ret
}

/// Perform a syscall with 5 arguments.
///
/// Argument layout: `a0 = arg0`, `a1 = arg1`, `a2 = arg2`, `a3 = arg3`,
/// `a4 = arg4`.
#[inline]
pub unsafe fn syscall5(n: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize, arg4: usize) -> usize {
    let ret: usize;
    unsafe {
        asm!("ecall", inlateout("a0") arg0 => ret, in("a1") arg1, in("a2") arg2, in("a3") arg3, in("a4") arg4, in("a7") n, options(nostack));
    }
    ret
}

/// Perform a syscall with 6 arguments.
///
/// Argument layout: `a0 = arg0`, `a1 = arg1`, `a2 = arg2`, `a3 = arg3`,
/// `a4 = arg4`, `a5 = arg5`.
#[inline]
pub unsafe fn syscall6(n: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> usize {
    let ret: usize;
    unsafe {
        asm!("ecall", inlateout("a0") arg0 => ret, in("a1") arg1, in("a2") arg2, in("a3") arg3, in("a4") arg4, in("a5") arg5, in("a7") n, options(nostack));
    }
    ret
}

// ── Convenience wrappers for common POSIX-like syscalls ──────────────────────

/// Exit the current process with the given exit status.
pub fn exit(status: i32) -> ! {
    unsafe { syscall1(SYS_EXIT, status as usize); }
    loop { unsafe { asm!("wfi") } }
}

/// Write `count` bytes from `buf` to `fd`. Returns bytes written or error.
pub fn write(fd: i32, buf: *const u8, count: usize) -> Result<usize, Error> {
    result_from(unsafe { syscall3(SYS_WRITE, fd as usize, buf as usize, count) })
}

/// Read up to `count` bytes from `fd` into `buf`. Returns bytes read or error.
pub fn read(fd: i32, buf: *mut u8, count: usize) -> Result<usize, Error> {
    result_from(unsafe { syscall3(SYS_READ, fd as usize, buf as usize, count) })
}

/// Close a file descriptor.
pub fn close(fd: i32) -> Result<(), Error> {
    let ret = unsafe { syscall1(SYS_CLOSE, fd as usize) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Fork the current process. Returns 0 in the child, child PID in the parent.
pub fn fork() -> Result<i32, Error> {
    let ret = unsafe { syscall0(SYS_FORK) };
    if ret <= (usize::MAX - 4096) { Ok(ret as i32) } else { Err(error_from_ret(ret)) }
}

/// Get the current process PID.
pub fn getpid() -> i32 {
    unsafe { syscall0(SYS_GETPID) as i32 }
}

/// Get the parent process PID.
pub fn getppid() -> i32 {
    unsafe { syscall0(SYS_GETPPID) as i32 }
}

/// Sleep for the given duration.
pub fn nanosleep(req: *const crate::types::Timespec, rem: *mut crate::types::Timespec) -> Result<(), Error> {
    let ret = unsafe { syscall2(SYS_NANOSLEEP, req as usize, rem as usize) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Get the current time.
pub fn gettimeofday(tv: *mut crate::types::TimeVal) -> Result<(), Error> {
    let ret = unsafe { syscall1(SYS_GETTIMEOFDAY, tv as usize) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Duplicate a file descriptor.
pub fn dup(oldfd: i32) -> Result<i32, Error> {
    let ret = unsafe { syscall1(SYS_DUP, oldfd as usize) };
    if ret <= (usize::MAX - 4096) { Ok(ret as i32) } else { Err(error_from_ret(ret)) }
}

/// Duplicate a file descriptor to a specific target.
pub fn dup2(oldfd: i32, newfd: i32) -> Result<i32, Error> {
    let ret = unsafe { syscall2(SYS_DUP2, oldfd as usize, newfd as usize) };
    if ret <= (usize::MAX - 4096) { Ok(ret as i32) } else { Err(error_from_ret(ret)) }
}

/// Send a signal to a process.
pub fn kill(pid: i32, sig: i32) -> Result<(), Error> {
    let ret = unsafe { syscall2(SYS_KILL, pid as usize, sig as usize) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Set a signal handler.
pub fn sigaction(sig: i32, act: *const crate::types::SigAction, oact: *mut crate::types::SigAction) -> Result<(), Error> {
    let ret = unsafe { syscall3(SYS_SIGACTION, sig as usize, act as usize, oact as usize) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Create a pipe. `fds` must point to a `[i32; 2]` buffer.
pub fn pipe(fds: *mut [i32; 2]) -> Result<(), Error> {
    let ret = unsafe { syscall1(SYS_PIPE, fds as usize) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Change the working directory.
pub fn chdir(path: *const u8, len: usize) -> Result<(), Error> {
    let ret = unsafe { syscall2(SYS_CHDIR, path as usize, len) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Get the working directory.
pub fn getcwd(buf: *mut u8, len: usize) -> Result<usize, Error> {
    result_from(unsafe { syscall2(SYS_GETCWD, buf as usize, len) })
}

/// Map memory.
pub fn mmap(addr: usize, len: usize, prot: usize, flags: usize, fd: i32, offset: usize) -> Result<usize, Error> {
    result_from(unsafe { syscall6(SYS_MMAP, addr, len, prot, flags, fd as usize, offset) })
}

/// Unmap memory.
pub fn munmap(addr: usize, len: usize) -> Result<(), Error> {
    let ret = unsafe { syscall2(SYS_MUNMAP, addr, len) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Set the program break (heap end).
pub fn brk(addr: usize) -> usize {
    unsafe { syscall1(SYS_BRK, addr) }
}

/// Get the kernel ABI version.
pub fn abi_version() -> usize {
    unsafe { syscall0(SYS_ABI_VERSION) }
}

// ── Priority constants ────────────────────────────────────────────────────────

pub const PRIO_REALTIME: i32 = 0;
pub const PRIO_HIGH: i32     = 8;
pub const PRIO_NORMAL: i32   = 16;
pub const PRIO_LOW: i32      = 24;
pub const PRIO_IDLE: i32     = 31;

/// Sets the calling process's scheduling priority (0 = highest, 31 = lowest).
pub fn setpriority(prio: i32) -> Result<(), Error> {
    let ret = unsafe { syscall1(SYS_SETPRIORITY, prio as usize) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Returns the calling process's scheduling priority.
pub fn getpriority() -> i32 {
    unsafe { syscall0(SYS_GETPRIORITY) as i32 }
}

/// Grants a subset of the caller's capability mask to `target_pid`.
pub fn cap_grant(target_pid: i32, caps: u64) -> Result<(), Error> {
    let ret = unsafe { syscall2(SYS_CAP_GRANT, target_pid as usize, caps as usize) };
    if ret == 0 { Ok(()) } else { Err(error_from_ret(ret)) }
}

/// Returns the calling process's mount namespace ID.
pub fn getnsid() -> u32 {
    unsafe { syscall0(SYS_GETNSID) as u32 }
}

// ── Capability bit constants ─────────────────────────────────────────────────

pub const CAP_NONE: u64      = 0;
pub const CAP_MMIO: u64      = 1 << 0;
pub const CAP_DATACOPY: u64  = 1 << 1;
pub const CAP_NET_RAW: u64   = 1 << 2;
pub const CAP_PROCESS: u64   = 1 << 3;
pub const CAP_INTERRUPT: u64 = 1 << 4;
pub const CAP_SYS_ADMIN: u64 = 1 << 5;
pub const CAP_DRIVER: u64    = 1 << 6;
pub const CAP_NET_ADMIN: u64 = 1 << 7;
pub const CAP_MOUNT: u64     = 1 << 8;
pub const CAP_KILL_ANY: u64  = 1 << 9;

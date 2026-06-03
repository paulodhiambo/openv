#![no_std]
extern crate alloc;

use buddy_system_allocator::LockedHeap;
use core::arch::asm;
use core::arch::global_asm;
use core::panic::PanicInfo;

// ── User-space heap (2 MB in BSS, zero-filled before main) ───────────────────
const USER_HEAP_SIZE: usize = 2 * 1024 * 1024;

#[global_allocator]
static USER_HEAP: LockedHeap<32> = LockedHeap::empty();

static mut USER_HEAP_DATA: [u8; USER_HEAP_SIZE] = [0; USER_HEAP_SIZE];

#[unsafe(no_mangle)]
pub extern "C" fn libos_init() {
    unsafe {
        USER_HEAP.lock().init(
            core::ptr::addr_of_mut!(USER_HEAP_DATA) as usize,
            USER_HEAP_SIZE,
        );
    }
}

global_asm!(
    r#"
    .section .text._start
    .global _start
    _start:
        call libos_init
        call main
        mv a0, zero
        call exit
    "#
);

#[inline]
pub fn syscall(sys_num: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    let mut ret: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") arg0 => ret,
            in("a1") arg1,
            in("a2") arg2,
            in("a7") sys_num,
            options(nostack)
        );
    }
    ret
}

#[inline]
pub fn syscall4(sys_num: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize) -> usize {
    let mut ret: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") arg0 => ret,
            in("a1") arg1,
            in("a2") arg2,
            in("a3") arg3,
            in("a7") sys_num,
            options(nostack)
        );
    }
    ret
}

#[unsafe(no_mangle)]
pub extern "C" fn sys_yield() {
    syscall(0, 0, 0, 0);
}

#[unsafe(no_mangle)]
pub extern "C" fn exit(status: i32) -> ! {
    syscall(1, status as usize, 0, 0);
    loop {}
}

#[unsafe(no_mangle)]
pub extern "C" fn write(fd: usize, buf: *const u8, len: usize) -> isize {
    syscall(2, fd, buf as usize, len) as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn pipe(fds: *mut [u32; 2]) -> i32 {
    syscall(3, fds as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn read(fd: usize, buf: *mut u8, len: usize) -> isize {
    syscall(5, fd, buf as usize, len) as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn open(path_ptr: *const u8, path_len: usize, flags: u32) -> i32 {
    syscall(8, path_ptr as usize, path_len, flags as usize) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn getdents(
    path_ptr: *const u8,
    path_len: usize,
    buf: *mut u8,
    len: usize,
) -> isize {
    syscall4(12, path_ptr as usize, path_len, buf as usize, len) as isize
}

/// Create or truncate a file at `path`, returning a writable fd (or -1 on error).
pub fn create(path: &[u8]) -> i32 {
    syscall(26, path.as_ptr() as usize, path.len(), 0) as i32
}

/// Create a directory at `path`. Returns 0 on success, -1 on error.
pub fn mkdir(path: &[u8]) -> i32 {
    syscall(27, path.as_ptr() as usize, path.len(), 0) as i32
}

/// Remove a file or directory at `path`. Returns 0 on success, -1 on error.
pub fn unlink(path: &[u8]) -> i32 {
    syscall(28, path.as_ptr() as usize, path.len(), 0) as i32
}

/// Rename `old` to `new`. Returns 0 on success, -1 on error.
pub fn rename(old: &[u8], new: &[u8]) -> i32 {
    syscall4(
        29,
        old.as_ptr() as usize,
        old.len(),
        new.as_ptr() as usize,
        new.len(),
    ) as i32
}

/// Enter (1) or leave (0) raw terminal mode (character-by-character input).
pub fn set_raw(enabled: u32) -> i32 {
    syscall(38, enabled as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn close(fd: i32) -> i32 {
    syscall(9, fd as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn spawn(path_ptr: *const u8, path_len: usize) -> i32 {
    syscall(6, path_ptr as usize, path_len, 0) as i32
}

/// Returns the real UID of the calling process.
#[unsafe(no_mangle)]
pub extern "C" fn getuid() -> u32 {
    syscall(30, 0, 0, 0) as u32
}

/// Returns the effective UID of the calling process.
#[unsafe(no_mangle)]
pub extern "C" fn geteuid() -> u32 {
    syscall(31, 0, 0, 0) as u32
}

/// Returns the real GID of the calling process.
#[unsafe(no_mangle)]
pub extern "C" fn getgid() -> u32 {
    syscall(32, 0, 0, 0) as u32
}

/// Returns the effective GID of the calling process.
#[unsafe(no_mangle)]
pub extern "C" fn getegid() -> u32 {
    syscall(33, 0, 0, 0) as u32
}

/// Authenticate `username` with `password`. Returns uid on success, `u32::MAX` on failure.
pub fn authenticate(username: &[u8], password: &[u8]) -> u32 {
    syscall4(
        34,
        username.as_ptr() as usize,
        username.len(),
        password.as_ptr() as usize,
        password.len(),
    ) as u32
}

/// Returns 1 if the given uid is allowed to use sudo, 0 otherwise.
/// Pass uid=0 to check the calling process's own uid.
#[unsafe(no_mangle)]
pub extern "C" fn can_sudo(uid: u32) -> u32 {
    syscall(35, uid as usize, 0, 0) as u32
}

/// Enable (1) or disable (0) terminal echo. Use disable during password input.
#[unsafe(no_mangle)]
pub extern "C" fn set_echo(enabled: u32) -> i32 {
    syscall(37, enabled as usize, 0, 0) as i32
}

pub mod net_adapter;
pub mod smoltcp_device;

// POSIX-like socket wrappers using kernel proxy syscalls

/// Create a socket. Parameters ignored by kernel proxy but kept for API compatibility.
#[unsafe(no_mangle)]
pub extern "C" fn socket(domain: i32, type_: i32, protocol: i32) -> i32 {
    syscall(40, domain as usize, type_ as usize, protocol as usize) as i32
}

/// Bind a socket to an address blob (e.g., sockaddr). Returns 0 on success or -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn bind(fd: i32, addr_ptr: *const u8, addr_len: usize) -> i32 {
    syscall(44, fd as usize, addr_ptr as usize, addr_len) as i32
}

/// Listen on a socket. backlog is advisory.
#[unsafe(no_mangle)]
pub extern "C" fn listen(fd: i32, backlog: i32) -> i32 {
    syscall(45, fd as usize, backlog as usize, 0) as i32
}

/// Connect a socket to an address blob. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn connect(fd: i32, addr_ptr: *const u8, addr_len: usize) -> i32 {
    syscall(46, fd as usize, addr_ptr as usize, addr_len) as i32
}

/// Accept a connection on a listening socket. Blocks until a connection is available. Returns new fd or -1.
#[unsafe(no_mangle)]
pub extern "C" fn accept(fd: i32) -> i32 {
    syscall(43, fd as usize, 0, 0) as i32
}

/// Send data on a connected socket. Returns number of bytes sent or -1.
#[unsafe(no_mangle)]
pub extern "C" fn send(fd: i32, buf: *const u8, len: usize, _flags: i32) -> isize {
    syscall(47, fd as usize, buf as usize, len) as isize
}

/// Receive data from a connected socket. Blocks until data is available. Returns bytes read or -1.
#[unsafe(no_mangle)]
pub extern "C" fn recv(fd: i32, buf: *mut u8, len: usize, _flags: i32) -> isize {
    syscall(48, fd as usize, buf as usize, len) as isize
}

/// Return the PID of the calling process.
pub fn getpid() -> i32 {
    syscall(53, 0, 0, 0) as i32
}

/// Return the PID of the parent process.
pub fn getppid() -> i32 {
    syscall(54, 0, 0, 0) as i32
}

/// Change the current working directory.  Returns 0 on success, -1 on error.
pub fn chdir(path: &[u8]) -> i32 {
    syscall(55, path.as_ptr() as usize, path.len(), 0) as i32
}

/// Copy the current working directory into `buf`.  Returns bytes written.
pub fn getcwd(buf: &mut [u8]) -> usize {
    syscall(56, buf.as_mut_ptr() as usize, buf.len(), 0)
}

/// Duplicate `fd` to the lowest available descriptor.  Returns new fd or -1.
pub fn dup(fd: i32) -> i32 {
    syscall(57, fd as usize, 0, 0) as i32
}

/// Duplicate `oldfd` to `newfd`, closing `newfd` first.  Returns `newfd` or -1.
pub fn dup2(oldfd: i32, newfd: i32) -> i32 {
    syscall(58, oldfd as usize, newfd as usize, 0) as i32
}

/// Register `pid` as the current foreground process for Ctrl+C delivery.
/// Pass -1 to clear (no foreground process).
pub fn set_fg_pid(pid: i32) {
    syscall(60, pid as usize, 0, 0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit(1);
}

// Ergonomic errno and result helpers

#[derive(Debug, PartialEq, Eq)]
pub enum Errno {
    EPERM,
    EACCES,
    ENOENT,
    UNKNOWN,
    Other(i32),
}

fn errno_from_usize(v: usize) -> Option<Errno> {
    if v == usize::MAX {
        return Some(Errno::UNKNOWN);
    }
    // If value encodes usize::MAX - ERR, extract ERR
    let err = (usize::MAX).wrapping_sub(v) as i32;
    if err == 0 {
        return None;
    }
    match err {
        1 => Some(Errno::EPERM),
        12 => Some(Errno::EACCES),
        2 => Some(Errno::ENOENT),
        e => Some(Errno::Other(e)),
    }
}

pub fn check_ret(ret: usize) -> Result<usize, Errno> {
    if let Some(e) = errno_from_usize(ret) {
        Err(e)
    } else {
        Ok(ret)
    }
}

// Convenience wrappers that return Result types while keeping the existing ABI wrappers.

pub fn write_res(fd: usize, buf: *const u8, len: usize) -> Result<usize, Errno> {
    check_ret(syscall(2, fd, buf as usize, len))
}

pub fn read_res(fd: usize, buf: *mut u8, len: usize) -> Result<usize, Errno> {
    check_ret(syscall(5, fd, buf as usize, len))
}

pub fn open_res(path_ptr: *const u8, path_len: usize, flags: u32) -> Result<i32, Errno> {
    match check_ret(syscall(8, path_ptr as usize, path_len, flags as usize)) {
        Ok(v) => Ok(v as i32),
        Err(e) => Err(e),
    }
}

pub fn fork_res() -> Result<i32, Errno> {
    match check_ret(syscall(50, 0, 0, 0)) {
        Ok(v) => Ok(v as i32),
        Err(e) => Err(e),
    }
}

/// Fork the calling process. Returns child PID in parent, 0 in child, or -1 on error.
pub fn fork() -> i32 {
    syscall(50, 0, 0, 0) as i32
}

/// Replace the current process image with the binary at `path`.
/// Returns -1 on error (does not return on success).
pub fn exec(path: &[u8]) -> i32 {
    syscall(51, path.as_ptr() as usize, path.len(), 0) as i32
}

/// Wait for a child process to change state. If target_pid == -1, waits for any child.
/// Returns child's pid on success and writes exit status to `status_ptr` if non-null.
pub fn waitpid_res(target_pid: i32, status_ptr: *mut i32, options: i32) -> Result<i32, Errno> {
    match check_ret(syscall(
        52,
        target_pid as usize,
        status_ptr as usize,
        options as usize,
    )) {
        Ok(v) => Ok(v as i32),
        Err(e) => Err(e),
    }
}

/// Wait for a specific child (or any child if pid == -1). Returns child PID or -1.
pub fn waitpid(pid: i32, status: *mut i32) -> i32 {
    syscall(52, pid as usize, status as usize, 0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_errno_from_usize() {
        assert_eq!(errno_from_usize(usize::MAX), Some(Errno::UNKNOWN));
        assert_eq!(errno_from_usize(usize::MAX - 1), Some(Errno::EPERM));
        assert_eq!(errno_from_usize(usize::MAX - 12), Some(Errno::EACCES));
        assert_eq!(errno_from_usize(100), None);
    }

    #[test]
    fn test_check_ret_ok() {
        assert_eq!(check_ret(10), Ok(10));
    }

    #[test]
    fn test_check_ret_err() {
        assert!(check_ret(usize::MAX).is_err());
    }
}

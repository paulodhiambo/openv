#![no_std]

pub mod ipc;
pub mod pm;
pub mod vfs;

// Re-export the raw syscall layer for convenience.
pub use openv_syscall::*;

// ── C-compatible POSIX wrappers ──────────────────────────────────────────────
// These are used by relibc's platform module (src/platform/openv/).
// They dispatch between kernel syscalls and VFS server IPC as needed.

use core::ffi::{c_char, c_int, c_void};

/// Open a file. Returns fd on success, -1 on error.
/// Dispatches to VFS server if available, otherwise falls back to kernel.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openv_open(path: *const c_char, flags: c_int, mode: c_int) -> c_int {
    let _ = mode;
    if path.is_null() { return -1; }
    let len = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes().len();
    let path_slice = unsafe { core::slice::from_raw_parts(path as *const u8, len) };

    if let Some(_pid) = vfs::vfs_pid() {
        let vfs_fd = vfs::vfs_open(path_slice, flags as u32);
        if vfs_fd > 0 {
            // Register the VFS fd in the kernel's fd table via fcntl.
            let ret = unsafe { syscall3(SYS_FCNTL, usize::MAX, 5, vfs_fd as usize) };
            return ret as c_int;
        }
    }
    // Fallback: open via kernel (syscall 8 — may fail if kernel doesn't support it).
    let ret = unsafe { syscall3(8, path as usize, len, flags as usize) };
    if ret <= (usize::MAX - 4096) { ret as c_int } else { -1 }
}

/// Read from a file descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openv_read(fd: c_int, buf: *mut c_void, count: usize) -> isize {
    // Check if this fd has a VFS backing.
    let vfs_fd = unsafe { syscall3(SYS_FCNTL, fd as usize, 6, 0) };
    if vfs_fd != usize::MAX {
        let buf_slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, count) };
        return vfs::vfs_read(vfs_fd as u32, u64::MAX, buf_slice);
    }
    unsafe { syscall3(SYS_READ, fd as usize, buf as usize, count) as isize }
}

/// Write to a file descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openv_write(fd: c_int, buf: *const c_void, count: usize) -> isize {
    let vfs_fd = unsafe { syscall3(SYS_FCNTL, fd as usize, 6, 0) };
    if vfs_fd != usize::MAX {
        let buf_slice = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
        return vfs::vfs_write(vfs_fd as u32, u64::MAX, buf_slice);
    }
    unsafe { syscall3(SYS_WRITE, fd as usize, buf as usize, count) as isize }
}

/// Close a file descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openv_close(fd: c_int) -> c_int {
    let vfs_fd = unsafe { syscall3(SYS_FCNTL, fd as usize, 6, 0) };
    if vfs_fd != usize::MAX {
        vfs::vfs_close(vfs_fd as u32);
    }
    let ret = unsafe { syscall1(SYS_CLOSE, fd as usize) };
    if ret == 0 { 0 } else { -1 }
}

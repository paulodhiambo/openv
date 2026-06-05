#![no_std]
extern crate alloc;

use buddy_system_allocator::LockedHeap;
use core::arch::asm;
use core::arch::global_asm;
use core::panic::PanicInfo;

// ── User-space heap ────────────────────────────────────────────────────────────
// Default: 2 MiB. Enable `large-heap` feature for 8 MiB (used by vfs-server).
#[cfg(feature = "large-heap")]
const USER_HEAP_SIZE: usize = 8 * 1024 * 1024;
#[cfg(not(feature = "large-heap"))]
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
        mv s0, a0
        mv s1, a1
        call libos_init
        call vfs_connect_inner   # cache VFS server PID before main()
        mv a0, s0
        mv a1, s1
        call main
        mv a0, zero
        call exit
    "#
);

// ── VFS file descriptor table ─────────────────────────────────────────────────
//
// User-visible fd numbers for VFS-server-backed files start at VFS_FD_BASE.
// Kernel fds (console, pipe, channel) remain in the range 0..VFS_FD_BASE.
// Each process has its own copy of this table (no cross-process sharing needed).

const VFS_FD_BASE: u32 = 1000;
const VFS_FD_SLOTS: usize = 64;

struct VfsFdSlot {
    active: bool,
    vfs_fd: u32,  // fd assigned by the VFS server
    offset: u64,  // sequential read/write cursor
}

// SAFETY: single-threaded user process; no re-entrant access.
static mut VFS_FDES: [VfsFdSlot; VFS_FD_SLOTS] = {
    const EMPTY: VfsFdSlot = VfsFdSlot { active: false, vfs_fd: 0, offset: 0 };
    [EMPTY; VFS_FD_SLOTS]
};

fn vfs_fd_slot(idx: usize) -> *mut VfsFdSlot {
    unsafe { core::ptr::addr_of_mut!(VFS_FDES).cast::<VfsFdSlot>().add(idx) }
}

fn vfs_fd_alloc(server_fd: u32) -> i32 {
    for i in 0..VFS_FD_SLOTS {
        let slot = vfs_fd_slot(i);
        if !unsafe { (*slot).active } {
            unsafe {
                (*slot).active = true;
                (*slot).vfs_fd = server_fd;
                (*slot).offset = 0;
            }
            return (VFS_FD_BASE + i as u32) as i32;
        }
    }
    -1
}

fn vfs_fd_get(user_fd: i32) -> Option<(u32, u64)> {
    if user_fd < VFS_FD_BASE as i32 { return None; }
    let idx = (user_fd as u32 - VFS_FD_BASE) as usize;
    if idx >= VFS_FD_SLOTS { return None; }
    let slot = vfs_fd_slot(idx);
    let (active, vfs_fd, offset) = unsafe { ((*slot).active, (*slot).vfs_fd, (*slot).offset) };
    if active { Some((vfs_fd, offset)) } else { None }
}

fn vfs_fd_advance(user_fd: i32, delta: u64) {
    if user_fd < VFS_FD_BASE as i32 { return; }
    let idx = (user_fd as u32 - VFS_FD_BASE) as usize;
    if idx < VFS_FD_SLOTS {
        let slot = vfs_fd_slot(idx);
        unsafe { if (*slot).active { (*slot).offset += delta; } }
    }
}

fn vfs_fd_free(user_fd: i32) {
    if user_fd < VFS_FD_BASE as i32 { return; }
    let idx = (user_fd as u32 - VFS_FD_BASE) as usize;
    if idx < VFS_FD_SLOTS {
        unsafe { (*vfs_fd_slot(idx)).active = false; }
    }
}

// Send OP_CLOSE to the VFS server for every active VFS fd slot.
// Called before exit() and exec() so the server doesn't accumulate orphaned entries.
fn close_all_vfs_fds() {
    for i in 0..VFS_FD_SLOTS {
        let user_fd = (VFS_FD_BASE + i as u32) as i32;
        if vfs_fd_get(user_fd).is_some() {
            close(user_fd);
        }
    }
}

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
    close_all_vfs_fds();
    syscall(1, status as usize, 0, 0);
    #[allow(clippy::empty_loop)]
    loop {}
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn write(fd: usize, buf: *const u8, len: usize) -> isize {
    if let Some((vfs_fd, offset)) = vfs_fd_get(fd as i32) {
        if let Some(server) = vfs_pid() {
            let data = unsafe { core::slice::from_raw_parts(buf, len) };
            const HDR: usize = 1 + 4 + 8; // op + fd + offset
            let chunk = data.len().min(vfs_proto::MAX_MSG - HDR);
            let mut req = [0u8; vfs_proto::MAX_MSG];
            let n = vfs_proto::build_write(&mut req, vfs_fd, offset, &data[..chunk]);
            if ipc_send(server, &req[..n]) != 0 { return -1; }
            let mut reply = [0u8; 8];
            let mut from = 0i32;
            let rlen = ipc_recv(&mut reply, &mut from);
            let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
            if status == vfs_proto::REPLY_OK {
                let written = vfs_proto::parse_write_reply(payload) as usize;
                vfs_fd_advance(fd as i32, written as u64);
                return written as isize;
            }
        }
        return -1;
    }
    syscall(2, fd, buf as usize, len) as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn pipe(fds: *mut [u32; 2]) -> i32 {
    syscall(3, fds as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn read(fd: usize, buf: *mut u8, len: usize) -> isize {
    if let Some((vfs_fd, offset)) = vfs_fd_get(fd as i32) {
        if let Some(server) = vfs_pid() {
            let want = (len as u32).min((vfs_proto::MAX_MSG - 1) as u32);
            let mut req = [0u8; 17]; // 1+4+8+4
            let n = vfs_proto::build_read(&mut req, vfs_fd, offset, want);
            if ipc_send(server, &req[..n]) != 0 { return 0; }
            let mut reply = [0u8; vfs_proto::MAX_MSG];
            let mut from = 0i32;
            let rlen = ipc_recv(&mut reply, &mut from);
            let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
            if status == vfs_proto::REPLY_OK {
                let data = vfs_proto::parse_read_reply(payload);
                let copy = data.len().min(len);
                unsafe { core::slice::from_raw_parts_mut(buf, copy).copy_from_slice(&data[..copy]); }
                vfs_fd_advance(fd as i32, copy as u64);
                return copy as isize;
            }
        }
        return 0;
    }
    syscall(5, fd, buf as usize, len) as isize
}

/// Open a file. Tries the VFS server first; falls back to the kernel.
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn open(path_ptr: *const u8, path_len: usize, flags: u32) -> i32 {
    if let Some(server) = vfs_pid() {
        let path = unsafe { core::slice::from_raw_parts(path_ptr, path_len) };
        let mut req = [0u8; vfs_proto::MAX_MSG];
        let n = vfs_proto::build_open(&mut req, flags, path);
        if ipc_send(server, &req[..n]) != 0 { return syscall(8, path_ptr as usize, path_len, flags as usize) as i32; }
        let mut reply = [0u8; vfs_proto::MAX_MSG];
        let mut from = 0i32;
        let rlen = ipc_recv(&mut reply, &mut from);
        let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
        if status == vfs_proto::REPLY_OK && let Some(vfs_fd) = vfs_proto::parse_open_reply(payload) {
            return vfs_fd_alloc(vfs_fd);
        }
    }
    syscall(8, path_ptr as usize, path_len, flags as usize) as i32
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn getdents(
    path_ptr: *const u8,
    path_len: usize,
    buf: *mut u8,
    len: usize,
) -> isize {
    if let Some(server) = vfs_pid() {
        let path = unsafe { core::slice::from_raw_parts(path_ptr, path_len) };
        let mut req = [0u8; vfs_proto::MAX_MSG];
        let n = vfs_proto::build_getdents(&mut req, path);
        if ipc_send(server, &req[..n]) != 0 { return syscall4(12, path_ptr as usize, path_len, buf as usize, len) as isize; }
        let mut reply = [0u8; vfs_proto::MAX_MSG];
        let mut from = 0i32;
        let rlen = ipc_recv(&mut reply, &mut from);
        let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
        if status == vfs_proto::REPLY_OK {
            let copy = payload.len().min(len);
            unsafe { core::slice::from_raw_parts_mut(buf, copy).copy_from_slice(&payload[..copy]); }
            return copy as isize;
        }
    }
    syscall4(12, path_ptr as usize, path_len, buf as usize, len) as isize
}

/// Create or truncate a file at `path`, returning a writable fd (or -1 on error).
pub fn create(path: &[u8]) -> i32 {
    if let Some(server) = vfs_pid() {
        let mut req = [0u8; vfs_proto::MAX_MSG];
        let n = vfs_proto::build_path_op(&mut req, vfs_proto::OP_CREATE, path);
        if ipc_send(server, &req[..n]) != 0 { return syscall(26, path.as_ptr() as usize, path.len(), 0) as i32; }
        let mut reply = [0u8; vfs_proto::MAX_MSG];
        let mut from = 0i32;
        let rlen = ipc_recv(&mut reply, &mut from);
        let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
        if status == vfs_proto::REPLY_OK && let Some(vfs_fd) = vfs_proto::parse_open_reply(payload) {
            return vfs_fd_alloc(vfs_fd);
        }
    }
    syscall(26, path.as_ptr() as usize, path.len(), 0) as i32
}

/// Create a directory at `path`. Returns 0 on success, -1 on error.
pub fn mkdir(path: &[u8]) -> i32 {
    if let Some(server) = vfs_pid() {
        let mut req = [0u8; vfs_proto::MAX_MSG];
        let n = vfs_proto::build_path_op(&mut req, vfs_proto::OP_MKDIR, path);
        if ipc_send(server, &req[..n]) != 0 { return syscall(27, path.as_ptr() as usize, path.len(), 0) as i32; }
        let mut reply = [0u8; 4];
        let mut from = 0i32;
        let rlen = ipc_recv(&mut reply, &mut from);
        let (status, _) = vfs_proto::parse_reply(&reply, rlen);
        if status == vfs_proto::REPLY_OK { return 0; }
    }
    syscall(27, path.as_ptr() as usize, path.len(), 0) as i32
}

/// Remove a file or directory at `path`. Returns 0 on success, -1 on error.
pub fn unlink(path: &[u8]) -> i32 {
    if let Some(server) = vfs_pid() {
        let mut req = [0u8; vfs_proto::MAX_MSG];
        let n = vfs_proto::build_path_op(&mut req, vfs_proto::OP_UNLINK, path);
        if ipc_send(server, &req[..n]) != 0 { return syscall(28, path.as_ptr() as usize, path.len(), 0) as i32; }
        let mut reply = [0u8; 4];
        let mut from = 0i32;
        let rlen = ipc_recv(&mut reply, &mut from);
        let (status, _) = vfs_proto::parse_reply(&reply, rlen);
        if status == vfs_proto::REPLY_OK { return 0; }
    }
    syscall(28, path.as_ptr() as usize, path.len(), 0) as i32
}

/// Rename `old` to `new`. Returns 0 on success, -1 on error.
pub fn rename(old: &[u8], new: &[u8]) -> i32 {
    if let Some(server) = vfs_pid() {
        let mut req = [0u8; vfs_proto::MAX_MSG];
        let n = vfs_proto::build_rename(&mut req, old, new);
        if ipc_send(server, &req[..n]) != 0 { return -1; }
        let mut reply = [0u8; 4];
        let mut from = 0i32;
        let rlen = ipc_recv(&mut reply, &mut from);
        let (status, _) = vfs_proto::parse_reply(&reply, rlen);
        if status == vfs_proto::REPLY_OK { return 0; }
    }
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
    if let Some((vfs_fd, _)) = vfs_fd_get(fd) {
        if let Some(server) = vfs_pid() {
            let mut req = [0u8; 5];
            let n = vfs_proto::build_close(&mut req, vfs_fd);
            if ipc_send(server, &req[..n]) == 0 {
                let mut reply = [0u8; 4];
                let mut from = 0i32;
                ipc_recv(&mut reply, &mut from);
            }
        }
        vfs_fd_free(fd);
        return 0;
    }
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

/// Non-blocking channel read. Returns bytes read, or 0 if no message is pending. Never sleeps.
pub fn try_recv(fd: usize, buf: *mut u8, len: usize) -> isize {
    syscall(49, fd, buf as usize, len) as isize
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

#[repr(C)]
pub struct TimeVal {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

#[repr(C)]
pub struct TimeSpec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[unsafe(no_mangle)]
pub extern "C" fn gettimeofday(tv: *mut TimeVal, _tz: *mut u8) -> i32 {
    syscall(82, tv as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn nanosleep(req: *const TimeSpec, rem: *mut TimeSpec) -> i32 {
    syscall(83, req as usize, rem as usize, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn setpgid(pid: i32, pgid: i32) -> i32 {
    syscall(84, pid as usize, pgid as usize, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn getpgid(pid: i32) -> i32 {
    syscall(85, pid as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn setsid() -> i32 {
    syscall(86, 0, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn kill(pid: i32, sig: i32) -> i32 {
    syscall(87, pid as usize, sig as usize, 0) as i32
}

#[repr(C)]
pub struct SigAction {
    pub sa_handler: usize,
    pub sa_flags: usize,
    pub sa_mask: u32,
}

#[unsafe(no_mangle)]
pub extern "C" fn sigaction(sig: i32, act: *const SigAction, oact: *mut SigAction) -> i32 {
    syscall(88, sig as usize, act as usize, oact as usize) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn sigreturn() {
    syscall(89, 0, 0, 0);
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

// ── IPC primitives ────────────────────────────────────────────────────────────

/// Send up to 4096 bytes to `to_pid`'s mailbox. Returns 0 on success, -1 on error.
pub fn ipc_send(to_pid: i32, buf: &[u8]) -> i32 {
    syscall(61, to_pid as usize, buf.as_ptr() as usize, buf.len()) as i32
}

/// Block until a message arrives in this process's mailbox.
/// Returns bytes copied; writes sender PID into `*from`.
pub fn ipc_recv(buf: &mut [u8], from: &mut i32) -> usize {
    syscall(62, buf.as_mut_ptr() as usize, buf.len(), from as *mut i32 as usize)
}

/// Register the calling process as the global VFS server.
pub fn vfs_register() {
    syscall(63, 0, 0, 0);
}

/// Return the PID of the registered VFS server, or -1 if not yet started.
pub fn get_vfs_pid() -> i32 {
    let v = syscall(64, 0, 0, 0);
    if v == usize::MAX { -1 } else { v as i32 }
}

// ── VFS client (talks to vfs-server via IPC) ──────────────────────────────────

use core::sync::atomic::{AtomicI32, Ordering as AtomicOrdering};

static VFS_SERVER_PID: AtomicI32 = AtomicI32::new(0);

/// Cache the VFS server PID.  Called automatically from `_start` before `main`.
#[unsafe(no_mangle)]
pub extern "C" fn vfs_connect_inner() {
    let pid = get_vfs_pid();
    if pid > 0 {
        VFS_SERVER_PID.store(pid, AtomicOrdering::Relaxed);
    }
}

/// Re-query the VFS server PID (call if it might have (re-)registered after startup).
pub fn vfs_connect() {
    vfs_connect_inner();
}

fn vfs_pid() -> Option<i32> {
    let pid = VFS_SERVER_PID.load(AtomicOrdering::Relaxed);
    if pid > 0 { Some(pid) } else { None }
}

fn vfs_call(req: &[u8], reply: &mut [u8]) -> usize {
    let server = match vfs_pid() { Some(p) => p, None => return 0 };
    if ipc_send(server, req) != 0 { return 0; }
    let mut from = 0i32;
    ipc_recv(reply, &mut from)
}

/// Open a file on the VFS server. Returns vfs_fd (≥1) or -1 on error.
pub fn vfs_open(path: &[u8], flags: u32) -> i32 {
    let mut req = [0u8; vfs_proto::MAX_MSG];
    let n = vfs_proto::build_open(&mut req, flags, path);
    let mut reply = [0u8; vfs_proto::MAX_MSG];
    let rlen = vfs_call(&req[..n], &mut reply);
    let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
    if status == vfs_proto::REPLY_OK {
        vfs_proto::parse_open_reply(payload).map(|fd| fd as i32).unwrap_or(-1)
    } else { -1 }
}

/// Create or truncate a file on the VFS server. Returns vfs_fd or -1.
pub fn vfs_create(path: &[u8]) -> i32 {
    let mut req = [0u8; vfs_proto::MAX_MSG];
    let n = vfs_proto::build_path_op(&mut req, vfs_proto::OP_CREATE, path);
    let mut reply = [0u8; vfs_proto::MAX_MSG];
    let rlen = vfs_call(&req[..n], &mut reply);
    let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
    if status == vfs_proto::REPLY_OK {
        vfs_proto::parse_open_reply(payload).map(|fd| fd as i32).unwrap_or(-1)
    } else { -1 }
}

/// Read up to `buf.len()` bytes from a VFS file at `offset`. Returns bytes read or -1.
/// Pass `offset = u64::MAX` to use the server's tracked sequential offset.
pub fn vfs_read(vfs_fd: u32, offset: u64, buf: &mut [u8]) -> isize {
    let want = (buf.len() as u32).min((vfs_proto::MAX_MSG - 1) as u32);
    let mut req = [0u8; 17]; // 1+4+8+4
    let n = vfs_proto::build_read(&mut req, vfs_fd, offset, want);
    let mut reply = [0u8; vfs_proto::MAX_MSG];
    let rlen = vfs_call(&req[..n], &mut reply);
    let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
    if status == vfs_proto::REPLY_OK {
        let data = vfs_proto::parse_read_reply(payload);
        let copy = data.len().min(buf.len());
        buf[..copy].copy_from_slice(&data[..copy]);
        copy as isize
    } else { -1 }
}

/// Write `data` to a VFS file at `offset`. Returns bytes written or -1.
/// Pass `offset = u64::MAX` to append at the server's tracked sequential offset.
pub fn vfs_write(vfs_fd: u32, offset: u64, data: &[u8]) -> isize {
    let mut req = [0u8; vfs_proto::MAX_MSG];
    let n = vfs_proto::build_write(&mut req, vfs_fd, offset, data);
    let mut reply = [0u8; vfs_proto::MAX_MSG];
    let rlen = vfs_call(&req[..n], &mut reply);
    let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
    if status == vfs_proto::REPLY_OK {
        vfs_proto::parse_write_reply(payload) as isize
    } else { -1 }
}

/// Close a VFS fd. Returns 0 on success, -1 on error.
pub fn vfs_close(vfs_fd: u32) -> i32 {
    let mut req = [0u8; 5];
    let n = vfs_proto::build_close(&mut req, vfs_fd);
    let mut reply = [0u8; 4];
    let rlen = vfs_call(&req[..n], &mut reply);
    let (status, _) = vfs_proto::parse_reply(&reply, rlen);
    if status == vfs_proto::REPLY_OK { 0 } else { -1 }
}

/// List directory entries on the VFS server. Returns null-separated names in `buf`.
pub fn vfs_getdents(path: &[u8], buf: &mut [u8]) -> isize {
    let mut req = [0u8; vfs_proto::MAX_MSG];
    let n = vfs_proto::build_getdents(&mut req, path);
    let mut reply = [0u8; vfs_proto::MAX_MSG];
    let rlen = vfs_call(&req[..n], &mut reply);
    let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
    if status == vfs_proto::REPLY_OK {
        let copy = payload.len().min(buf.len());
        buf[..copy].copy_from_slice(&payload[..copy]);
        copy as isize
    } else { -1 }
}

/// Create a directory on the VFS server. Returns 0 on success, -1 on error.
pub fn vfs_mkdir(path: &[u8]) -> i32 {
    let mut req = [0u8; vfs_proto::MAX_MSG];
    let n = vfs_proto::build_path_op(&mut req, vfs_proto::OP_MKDIR, path);
    let mut reply = [0u8; 4];
    let rlen = vfs_call(&req[..n], &mut reply);
    let (status, _) = vfs_proto::parse_reply(&reply, rlen);
    if status == vfs_proto::REPLY_OK { 0 } else { -1 }
}

/// Remove a file or directory on the VFS server. Returns 0 on success, -1 on error.
pub fn vfs_unlink(path: &[u8]) -> i32 {
    let mut req = [0u8; vfs_proto::MAX_MSG];
    let n = vfs_proto::build_path_op(&mut req, vfs_proto::OP_UNLINK, path);
    let mut reply = [0u8; 4];
    let rlen = vfs_call(&req[..n], &mut reply);
    let (status, _) = vfs_proto::parse_reply(&reply, rlen);
    if status == vfs_proto::REPLY_OK { 0 } else { -1 }
}

/// Stat a path on the VFS server. Returns `(is_dir, size)` or `None` on error.
pub fn vfs_stat(path: &[u8]) -> Option<(bool, u64)> {
    let mut req = [0u8; vfs_proto::MAX_MSG];
    let n = vfs_proto::build_stat(&mut req, path);
    let mut reply = [0u8; vfs_proto::MAX_MSG];
    let rlen = vfs_call(&req[..n], &mut reply);
    let (status, payload) = vfs_proto::parse_reply(&reply, rlen);
    if status == vfs_proto::REPLY_OK {
        vfs_proto::parse_stat_reply(payload)
    } else { None }
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
    close_all_vfs_fds();
    syscall4(51, path.as_ptr() as usize, path.len(), 0, 0) as i32
}

/// Replace the current process image, passing `argv_buf` (packed null-terminated strings)
/// as the argument vector.  `argv_buf` format: `"arg0\0arg1\0...argN\0"`.
/// Returns -1 on error (does not return on success).
pub fn exec_args(path: &[u8], argv_buf: &[u8]) -> i32 {
    close_all_vfs_fds();
    syscall4(
        51,
        path.as_ptr() as usize,
        path.len(),
        argv_buf.as_ptr() as usize,
        argv_buf.len(),
    ) as i32
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

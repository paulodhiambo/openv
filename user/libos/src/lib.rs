#![no_std]
extern crate alloc;

use buddy_system_allocator::LockedHeap;
use core::arch::asm;
use core::arch::global_asm;
use core::panic::PanicInfo;

// ── User-space heap ────────────────────────────────────────────────────────────
#[cfg(feature = "huge-heap")]
const USER_HEAP_SIZE: usize = 32 * 1024 * 1024;
#[cfg(all(feature = "large-heap", not(feature = "huge-heap")))]
const USER_HEAP_SIZE: usize = 8 * 1024 * 1024;
#[cfg(all(not(feature = "large-heap"), not(feature = "huge-heap")))]
const USER_HEAP_SIZE: usize = 2 * 1024 * 1024;

#[global_allocator]
static USER_HEAP: LockedHeap<32> = LockedHeap::empty();

static mut USER_HEAP_DATA: [u8; USER_HEAP_SIZE] = [0; USER_HEAP_SIZE];

/// ABI version this libos was compiled against.  Must match the kernel.
pub const LIBOS_ABI_VERSION: usize = 1;

#[unsafe(no_mangle)]
pub extern "C" fn libos_init() {
    unsafe {
        USER_HEAP.lock().init(
            core::ptr::addr_of_mut!(USER_HEAP_DATA) as usize,
            USER_HEAP_SIZE,
        );
    }
    // Verify the running kernel exposes the expected ABI version.
    let kver = syscall(138, 0, 0, 0);
    if kver != LIBOS_ABI_VERSION {
        // Kernel ABI mismatch — abort immediately with a distinctive exit code.
        syscall(1, usize::MAX, 0, 0);
        loop {
            unsafe {
                core::arch::asm!("wfi");
            }

        }
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
        call vfs_connect_inner
        mv a0, s0
        mv a1, s1
        call main
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

#[inline]
pub fn syscall5(
    sys_num: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
) -> usize {
    let mut ret: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") arg0 => ret,
            in("a1") arg1,
            in("a2") arg2,
            in("a3") arg3,
            in("a4") arg4,
            in("a7") sys_num,
            options(nostack)
        );
    }
    ret
}

#[inline]
pub fn syscall6(
    sys_num: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> usize {
    let mut ret: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") arg0 => ret,
            in("a1") arg1,
            in("a2") arg2,
            in("a3") arg3,
            in("a4") arg4,
            in("a5") arg5,
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

fn close_all_vfs_fds() {
    for i in 0..256 {
        let res = syscall(81, i, 1001 /* F_GET_VFS_FD */, 0);
        if res != usize::MAX {
            vfs_close(res as u32);
            syscall(9, i, 0, 0); // remove the stale VfsFile handle from the kernel table
        }
    }
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
    let res = syscall(81, fd, 1001, 0);
    if res != usize::MAX {
        let vfs_fd = res as u32;
        return vfs_write(vfs_fd, u64::MAX, unsafe {
            core::slice::from_raw_parts(buf, len)
        });
    }
    syscall(2, fd, buf as usize, len) as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn pipe(fds: *mut [u32; 2]) -> i32 {
    syscall(3, fds as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn channel_create(fds: *mut [u32; 2]) -> i32 {
    syscall(94, fds as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn channel_write(
    fd: usize,
    buf: *const u8,
    buf_len: usize,
    handles: *const u32,
    handles_len: usize,
) -> i32 {
    syscall5(95, fd, buf as usize, buf_len, handles as usize, handles_len) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn channel_read(
    fd: usize,
    buf: *mut u8,
    buf_len: usize,
    handles: *mut u32,
    handles_len: usize,
    out_bytes_read: *mut usize,
    out_handles_read: *mut usize,
) -> i32 {
    let mut ret_bytes: usize;
    let mut ret_handles: usize;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd => ret_bytes,
            inlateout("a1") buf as usize => ret_handles,
            in("a2") buf_len,
            in("a3") handles as usize,
            in("a4") handles_len,
            in("a7") 96,
            options(nostack)
        );
    }
    if ret_bytes >= (usize::MAX - 200) {
        return ret_bytes as i32;
    }
    unsafe {
        if !out_bytes_read.is_null() {
            *out_bytes_read = ret_bytes;
        }
        if !out_handles_read.is_null() {
            *out_handles_read = ret_handles;
        }
    }
    0
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn read(fd: usize, buf: *mut u8, len: usize) -> isize {
    let res = syscall(81, fd, 1001, 0);
    if res != usize::MAX {
        let vfs_fd = res as u32;
        return vfs_read(vfs_fd, u64::MAX, unsafe {
            core::slice::from_raw_parts_mut(buf, len)
        });
    }
    syscall(5, fd, buf as usize, len) as isize
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn open(path_ptr: *const u8, path_len: usize, flags: u32) -> i32 {
    if vfs_pid().is_some() {
        let path = unsafe { core::slice::from_raw_parts(path_ptr, path_len) };
        let vfs_fd = vfs_open(path, flags);
        if vfs_fd > 0 {
            return syscall(81, usize::MAX, 5, vfs_fd as usize) as i32;
        }
        // VFS is running but the file was not found — propagate the error
        // rather than falling through to the kernel-level open.
        return -1;
    }
    // VFS not yet started: ask the kernel (returns ENOENT — no native FS).
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
    if vfs_pid().is_some() {
        let path = unsafe { core::slice::from_raw_parts(path_ptr, path_len) };
        let ret = vfs_getdents(path, unsafe { core::slice::from_raw_parts_mut(buf, len) });
        if ret >= 0 {
            return ret;
        }
    }
    syscall4(12, path_ptr as usize, path_len, buf as usize, len) as isize
}

pub fn create(path: &[u8]) -> i32 {
    if vfs_pid().is_some() {
        let vfs_fd = vfs_create(path);
        if vfs_fd > 0 {
            return syscall(81, usize::MAX, 5, vfs_fd as usize) as i32;
        }
    }
    syscall(26, path.as_ptr() as usize, path.len(), 0) as i32
}

pub fn mkdir(path: &[u8]) -> i32 {
    if vfs_pid().is_some() && vfs_mkdir(path) == 0 {
        return 0;
    }
    syscall(27, path.as_ptr() as usize, path.len(), 0) as i32
}

pub fn unlink(path: &[u8]) -> i32 {
    if vfs_pid().is_some() && vfs_unlink(path) == 0 {
        return 0;
    }
    syscall(28, path.as_ptr() as usize, path.len(), 0) as i32
}

pub fn rename(old: &[u8], new: &[u8]) -> i32 {
    if vfs_pid().is_some() && vfs_rename(old, new) == 0 {
        return 0;
    }
    syscall4(
        29,
        old.as_ptr() as usize,
        old.len(),
        new.as_ptr() as usize,
        new.len(),
    ) as i32
}

pub fn set_raw(enabled: u32) -> i32 {
    syscall(38, enabled as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn close(fd: i32) -> i32 {
    let res = syscall(81, fd as usize, 1001, 0);
    if res != usize::MAX {
        let vfs_fd = res as u32;
        vfs_close(vfs_fd);
    }
    syscall(9, fd as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn spawn(path_ptr: *const u8, path_len: usize) -> i32 {
    syscall(6, path_ptr as usize, path_len, 0) as i32
}

pub const CAP_NONE: u64 = 0;
pub const CAP_MMIO: u64 = 1 << 0;
pub const CAP_DATACOPY: u64 = 1 << 1;
pub const CAP_NET_RAW: u64 = 1 << 2;
pub const CAP_PROCESS: u64 = 1 << 3;
pub const CAP_INTERRUPT: u64 = 1 << 4;
pub const CAP_SYS_ADMIN: u64 = 1 << 5;

pub fn spawn_with_caps(path_ptr: *const u8, path_len: usize, caps: u64) -> i32 {
    syscall(7, path_ptr as usize, path_len, caps as usize) as i32
}

/// Map physical address range [pa, pa+len) into the caller's VA space at `va`.
/// Returns `va` on success, `usize::MAX` on failure.
pub fn phys_map(va: usize, pa: usize, len: usize) -> usize {
    syscall(106, va, pa, len)
}

/// Register the calling process as the system block driver (requires CAP_MMIO).
pub fn blk_register() -> i32 {
    syscall(112, 0, 0, 0) as i32
}

/// Return the PID of the registered block driver, or -1 if none.
pub fn get_blk_pid() -> i32 {
    let v = syscall(113, 0, 0, 0);
    if v == usize::MAX { -1 } else { v as i32 }
}

pub fn virt_to_phys(virt_addr: usize) -> Option<usize> {
    let pa = syscall(109, virt_addr, 0, 0);
    if pa == usize::MAX { None } else { Some(pa) }
}

pub fn irq_register(irq: u32, handler_pid: i32) -> i32 {
    syscall(107, irq as usize, handler_pid as usize, 0) as i32
}

pub fn irq_enable(irq: u32) -> i32 {
    syscall(108, irq as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn getuid() -> u32 {
    syscall(30, 0, 0, 0) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn geteuid() -> u32 {
    syscall(31, 0, 0, 0) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn getgid() -> u32 {
    syscall(32, 0, 0, 0) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn getegid() -> u32 {
    syscall(33, 0, 0, 0) as u32
}

pub fn authenticate(username: &[u8], password: &[u8]) -> u32 {
    syscall4(
        34,
        username.as_ptr() as usize,
        username.len(),
        password.as_ptr() as usize,
        password.len(),
    ) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn can_sudo(uid: u32) -> u32 {
    syscall(35, uid as usize, 0, 0) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn set_echo(enabled: u32) -> i32 {
    syscall(37, enabled as usize, 0, 0) as i32
}

pub mod ipc;
pub mod net_adapter;
pub mod smoltcp_device;

pub const IPC_ANY: i32 = -1;

#[unsafe(no_mangle)]
pub extern "C" fn msg_send(dest: i32, msg: *const ipc::Message) -> i32 {
    syscall(90, dest as usize, msg as usize, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn msg_receive(src: i32, msg: *mut ipc::Message) -> i32 {
    syscall(91, src as usize, msg as usize, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn msg_sendrec(dest: i32, msg: *mut ipc::Message) -> i32 {
    syscall(92, dest as usize, msg as usize, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn socket(domain: i32, type_: i32, protocol: i32) -> i32 {
    syscall(40, domain as usize, type_ as usize, protocol as usize) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn bind(fd: i32, addr_ptr: *const u8, addr_len: usize) -> i32 {
    syscall(44, fd as usize, addr_ptr as usize, addr_len) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn listen(fd: i32, backlog: i32) -> i32 {
    syscall(45, fd as usize, backlog as usize, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn connect(fd: i32, addr_ptr: *const u8, addr_len: usize) -> i32 {
    syscall(46, fd as usize, addr_ptr as usize, addr_len) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn accept(fd: i32) -> i32 {
    syscall(43, fd as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn send(fd: i32, buf: *const u8, len: usize, _flags: i32) -> isize {
    syscall(47, fd as usize, buf as usize, len) as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn recv(fd: i32, buf: *mut u8, len: usize, _flags: i32) -> isize {
    syscall(48, fd as usize, buf as usize, len) as isize
}

pub fn try_recv(fd: usize, buf: *mut u8, len: usize) -> isize {
    syscall(49, fd, buf as usize, len) as isize
}

/// `recv` with a yield-count timeout. Returns -1 when no data arrives within
/// `max_yields` scheduler turns. Non-blocking per turn; the caller can treat a
/// negative return as a network error.
pub fn recv_timeout(fd: usize, buf: *mut u8, len: usize, max_yields: usize) -> isize {
    for _ in 0..max_yields {
        let n = try_recv(fd, buf, len);
        if n != 0 { return n; }
        sys_yield();
    }
    -1
}

pub fn getpid() -> i32 {
    syscall(53, 0, 0, 0) as i32
}

pub fn getppid() -> i32 {
    syscall(54, 0, 0, 0) as i32
}

pub fn chdir(path: &[u8]) -> i32 {
    syscall(55, path.as_ptr() as usize, path.len(), 0) as i32
}

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

#[unsafe(no_mangle)]
pub extern "C" fn sigprocmask(how: i32, set: *const u32, old_set: *mut u32) -> i32 {
    syscall(69, how as usize, set as usize, old_set as usize) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn sigpending(set: *mut u32) -> i32 {
    syscall(97, set as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_prctl(code: usize, val: usize) -> i32 {
    syscall(98, code, val, 0) as i32
}

pub fn dup(oldfd: i32) -> i32 {
    let res = syscall(81, oldfd as usize, 1001, 0);
    if res != usize::MAX {
        let vfs_fd = res as u32;
        if let Some(new_server_fd) = vfs_dup(vfs_fd) {
            return syscall(81, usize::MAX, 5, new_server_fd as usize) as i32;
        }
        return -1;
    }
    syscall(57, oldfd as usize, 0, 0) as i32
}

pub fn dup2(oldfd: i32, newfd: i32) -> i32 {
    let res = syscall(81, oldfd as usize, 1001, 0);
    if res != usize::MAX {
        let vfs_fd = res as u32;
        if let Some(new_server_fd) = vfs_dup(vfs_fd) {
            syscall(81, newfd as usize, 5, new_server_fd as usize);
            return newfd;
        }
        return -1;
    }
    syscall(58, oldfd as usize, newfd as usize, 0) as i32
}

pub fn set_fg_pid(pid: i32) {
    syscall(60, pid as usize, 0, 0);
}

#[unsafe(no_mangle)]
pub extern "C" fn lseek(fd: i32, offset: i64, whence: i32) -> i64 {
    // Check if this is a VFS-backed fd.
    let res = syscall(81, fd as usize, 1001 /* F_GET_VFS_FD */, 0);
    if res != usize::MAX {
        return vfs_lseek(res as u32, offset, whence as u32);
    }
    // Fall back to kernel lseek.
    let ret = syscall(19, fd as usize, offset as usize, whence as usize);
    if ret == usize::MAX { -1 } else { ret as i64 }
}

#[repr(C)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// Simple poll(2) wrapper — iterates the fd array once without blocking.
/// Returns the number of ready fds, or -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32 {
    syscall(68, fds as usize, nfds, timeout as usize) as i32
}

/// tcsetpgrp — set the foreground process group of the terminal.
/// Equivalent to calling `ioctl(fd, TIOCSPGRP, &pgrp)`.
#[unsafe(no_mangle)]
pub extern "C" fn tcsetpgrp(fd: i32, pgrp: i32) -> i32 {
    // Validate fd is a TTY via fcntl; then delegate to kernel.
    if fd >= 0 {
        syscall(60, pgrp as usize, 0, 0);
        0
    } else {
        -1
    }
}

pub fn ipc_send(to_pid: i32, buf: &[u8]) -> i32 {
    syscall(61, to_pid as usize, buf.as_ptr() as usize, buf.len()) as i32
}

pub fn ipc_recv(buf: &mut [u8], from: &mut i32) -> usize {
    syscall(
        62,
        buf.as_mut_ptr() as usize,
        buf.len(),
        from as *mut i32 as usize,
    )
}

pub fn vfs_register() {
    syscall(63, 0, 0, 0);
}

pub fn get_vfs_pid() -> i32 {
    let v = syscall(64, 0, 0, 0);
    if v == usize::MAX { -1 } else { v as i32 }
}

/// Registers the calling process as the component manager.
pub fn cm_register() {
    syscall(163, 0, 0, 0);
}

/// Returns the PID of the registered component manager (−1 if not started).
pub fn get_cm_pid() -> i32 {
    let v = syscall(164, 0, 0, 0);
    if v == 0 || v == usize::MAX { -1 } else { v as i32 }
}

pub fn datacopy(
    src_pid: i32,
    src_addr: *const u8,
    dst_pid: i32,
    dst_addr: *mut u8,
    len: usize,
) -> isize {
    syscall5(
        93,
        src_pid as usize,
        src_addr as usize,
        dst_pid as usize,
        dst_addr as usize,
        len,
    ) as isize
}

// ── VFS client (talks to vfs-server via IPC) ──────────────────────────────────

use core::sync::atomic::{AtomicI32, Ordering as AtomicOrdering};

static VFS_SERVER_PID: AtomicI32 = AtomicI32::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn vfs_connect_inner() {
    let pid = get_vfs_pid();
    if pid > 0 {
        VFS_SERVER_PID.store(pid, AtomicOrdering::Relaxed);
    }
}

pub fn vfs_connect() {
    vfs_connect_inner();
}

pub fn vfs_pid() -> Option<i32> {
    let pid = VFS_SERVER_PID.load(AtomicOrdering::Relaxed);
    if pid > 0 { Some(pid) } else { None }
}

fn vfs_call(msg: &mut ipc::Message) -> bool {
    let server = match vfs_pid() {
        Some(p) => p,
        None => return false,
    };
    msg_sendrec(server, msg) == 0
}

pub fn vfs_open(path: &[u8], flags: u32) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_OPEN;
    vfs_proto::pack_path_req(&mut msg.data, flags, path.as_ptr() as usize, path.len(), getnsid());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as i32
    } else {
        -1
    }
}

pub fn vfs_create(path: &[u8]) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_CREATE;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len(), getnsid());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as i32
    } else {
        -1
    }
}

pub fn vfs_read(vfs_fd: u32, offset: u64, buf: &mut [u8]) -> isize {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_READ;
    vfs_proto::pack_rw_req(
        &mut msg.data,
        vfs_fd,
        offset,
        buf.as_mut_ptr() as usize,
        buf.len(),
    );
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_write(vfs_fd: u32, offset: u64, data: &[u8]) -> isize {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_WRITE;
    vfs_proto::pack_rw_req(
        &mut msg.data,
        vfs_fd,
        offset,
        data.as_ptr() as usize,
        data.len(),
    );
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_close(vfs_fd: u32) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_CLOSE;
    vfs_proto::pack_close_req(&mut msg.data, vfs_fd);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_getdents(path: &[u8], buf: &mut [u8]) -> isize {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_GETDENTS;
    vfs_proto::pack_getdents_req(
        &mut msg.data,
        path.as_ptr() as usize,
        path.len(),
        buf.as_mut_ptr() as usize,
        buf.len(),
    );
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_mkdir(path: &[u8]) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_MKDIR;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len(), getnsid());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_unlink(path: &[u8]) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_UNLINK;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len(), getnsid());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_rename(old: &[u8], new: &[u8]) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_RENAME;
    vfs_proto::pack_rename_req(
        &mut msg.data,
        old.as_ptr() as usize,
        old.len(),
        new.as_ptr() as usize,
        new.len(),
    );
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_stat(path: &[u8]) -> Option<(bool, u64)> {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_STAT;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len(), getnsid());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        Some(vfs_proto::unpack_stat_reply(&msg.data))
    } else {
        None
    }
}

pub fn vfs_dup(vfs_fd: u32) -> Option<u32> {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_DUP;
    vfs_proto::pack_u32_reply(&mut msg.data, vfs_fd);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        Some(vfs_proto::unpack_u32_reply(&msg.data))
    } else {
        None
    }
}

pub fn vfs_readlink(path: &[u8], out: &mut [u8]) -> isize {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_READLINK;
    vfs_proto::pack_getdents_req(&mut msg.data, path.as_ptr() as usize, path.len(), out.as_mut_ptr() as usize, out.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_symlink(link_path: &[u8], target: &[u8]) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_SYMLINK;
    vfs_proto::pack_two_paths_req(&mut msg.data, link_path.as_ptr() as usize, link_path.len(), target.as_ptr() as usize, target.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_link(old: &[u8], new: &[u8]) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_LINK;
    vfs_proto::pack_two_paths_req(&mut msg.data, old.as_ptr() as usize, old.len(), new.as_ptr() as usize, new.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_chmod(path: &[u8], mode: u32) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_CHMOD;
    vfs_proto::pack_path_req(&mut msg.data, mode, path.as_ptr() as usize, path.len(), getnsid());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_chown(path: &[u8], owner: u32, _group: u32) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_CHOWN;
    vfs_proto::pack_path_req(&mut msg.data, owner, path.as_ptr() as usize, path.len(), getnsid());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_ftruncate(vfs_fd: u32, length: u64) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_FTRUNCATE;
    vfs_proto::pack_ftruncate_req(&mut msg.data, vfs_fd, length);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_lseek(vfs_fd: u32, offset: i64, whence: u32) -> i64 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_LSEEK;
    vfs_proto::pack_lseek_req(&mut msg.data, vfs_fd, offset, whence);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u64_reply(&msg.data) as i64
    } else {
        -1
    }
}

pub fn vfs_fpath(vfs_fd: u32, buf: &mut [u8]) -> isize {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_FPATH;
    vfs_proto::pack_fpath_req(&mut msg.data, vfs_fd, buf.as_mut_ptr() as usize, buf.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_futimens(vfs_fd: u32, atime_sec: i64, atime_nsec: i64, mtime_sec: i64, mtime_nsec: i64) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_FUTIMENS;
    vfs_proto::pack_futimens_req(&mut msg.data, vfs_fd, atime_sec, atime_nsec, mtime_sec, mtime_nsec);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_fallocate(vfs_fd: u32, offset: u64, length: u64) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_FALLOCATE;
    vfs_proto::pack_fallocate_req(&mut msg.data, vfs_fd, offset, length);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Write a minimal message so OOM/assert panics are visible on stderr.
    let msg = b"panic: process aborted\n";
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 2usize,   // sys_write
            in("a0") 2usize,   // fd=2 (stderr)
            in("a1") msg.as_ptr(),
            in("a2") msg.len(),
            options(nostack),
        );
    }
    let _ = info;
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

pub fn write_res(fd: usize, buf: *const u8, len: usize) -> Result<usize, Errno> {
    check_ret(write(fd, buf, len) as usize)
}

pub fn read_res(fd: usize, buf: *mut u8, len: usize) -> Result<usize, Errno> {
    check_ret(read(fd, buf, len) as usize)
}

pub fn open_res(path_ptr: *const u8, path_len: usize, flags: u32) -> Result<i32, Errno> {
    let ret = open(path_ptr, path_len, flags);
    if ret < 0 {
        Err(Errno::UNKNOWN)
    } else {
        Ok(ret)
    }
}

pub fn fork_res() -> Result<i32, Errno> {
    match check_ret(syscall(50, 0, 0, 0)) {
        Ok(v) => Ok(v as i32),
        Err(e) => Err(e),
    }
}

pub fn fork() -> i32 {
    syscall(50, 0, 0, 0) as i32
}

pub fn exec(path: &[u8]) -> i32 {
    close_all_vfs_fds();
    syscall6(51, path.as_ptr() as usize, path.len(), 0, 0, 0, 0) as i32
}

pub fn exec_args(path: &[u8], argv_buf: &[u8]) -> i32 {
    close_all_vfs_fds();
    syscall6(
        51,
        path.as_ptr() as usize,
        path.len(),
        argv_buf.as_ptr() as usize,
        argv_buf.len(),
        0,
        0,
    ) as i32
}

pub fn exec_args_envp(path: &[u8], argv_buf: &[u8], envp_buf: &[u8]) -> i32 {
    close_all_vfs_fds();
    syscall6(
        51,
        path.as_ptr() as usize,
        path.len(),
        argv_buf.as_ptr() as usize,
        argv_buf.len(),
        envp_buf.as_ptr() as usize,
        envp_buf.len(),
    ) as i32
}

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

pub fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32 {
    syscall(52, pid as usize, status as usize, options as usize) as i32
}

// ── Thread support ─────────────────────────────────────────────────────────────

pub const CLONE_VM: u32 = 0x0000_0100;
pub const CLONE_THREAD: u32 = 0x0001_0000;
pub const CLONE_SETTLS: u32 = 0x0008_0000;

/// Spawn a new thread sharing the caller's address space.
/// `stack` must point to the TOP of the new thread's stack (already allocated).
/// `tls`   is written to the tp register of the new thread.
/// Returns the child TID (> 0) in the parent, 0 in the child.
pub fn thread_create(stack: *mut u8, stack_size: usize, tls: usize) -> i32 {
    let stack_top = stack as usize + stack_size;
    let flags = (CLONE_VM | CLONE_THREAD | CLONE_SETTLS) as usize;
    syscall(120, flags, stack_top, tls) as i32
}

/// Low-level clone — mirrors the Linux clone(2) ABI for the three-argument form.
pub fn clone(flags: u32, stack: usize, tls: usize) -> i32 {
    syscall(120, flags as usize, stack, tls) as i32
}

/// Return the calling thread's TID.
pub fn gettid() -> i32 {
    syscall(122, 0, 0, 0) as i32
}

pub const FUTEX_WAIT: usize = 0;
pub const FUTEX_WAKE: usize = 1;

/// Block until `*uaddr != val` is violated (woken by FUTEX_WAKE on the same address).
/// Returns 0 on wakeup, EAGAIN if `*uaddr != val` at call time.
pub fn futex_wait(uaddr: *const u32, val: u32) -> i32 {
    syscall(121, uaddr as usize, FUTEX_WAIT, val as usize) as i32
}

/// Wake up to `count` threads sleeping on `uaddr`. Returns the number woken.
pub fn futex_wake(uaddr: *const u32, count: usize) -> usize {
    syscall(121, uaddr as usize, FUTEX_WAKE, count)
}

// ── fsync / fdatasync ──────────────────────────────────────────────────────────

/// Send OP_FSYNC to the vfs-server for the given server-side fd.
pub fn vfs_fsync(vfs_fd: u32) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_FSYNC;
    vfs_proto::pack_close_req(&mut msg.data, vfs_fd);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

/// Flush all pending writes for `fd` to durable storage.
pub fn fsync(fd: i32) -> i32 {
    let res = syscall(81, fd as usize, 1001, 0); // F_GET_VFS_FD
    if res != usize::MAX {
        return vfs_fsync(res as u32);
    }
    syscall(130, fd as usize, 0, 0) as i32
}

/// Same as fsync for this kernel (no separate metadata-only path).
pub fn fdatasync(fd: i32) -> i32 {
    fsync(fd)
}

// ── mmap / munmap / brk ───────────────────────────────────────────────────────

pub const PROT_NONE: u32 = 0;
pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;

pub const MAP_SHARED: u32 = 0x01;
pub const MAP_PRIVATE: u32 = 0x02;
pub const MAP_ANONYMOUS: u32 = 0x20;
pub const MAP_FAILED: usize = usize::MAX;

/// Map `len` bytes of anonymous memory.  Returns the virtual address, or
/// `MAP_FAILED` on error.  Physical pages are demand-allocated on first touch.
pub fn mmap(addr: usize, len: usize, prot: u32, flags: u32, fd: i32, offset: usize) -> usize {
    let mut ret: usize;
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") addr => ret,
            in("a1") len,
            in("a2") prot as usize,
            in("a3") flags as usize,
            in("a4") fd as usize,
            in("a5") offset,
            in("a7") 135usize,
            options(nostack)
        );
    }
    ret
}

/// Unmap the region `[addr, addr+len)`.  Returns 0 on success.
pub fn munmap(addr: usize, len: usize) -> i32 {
    syscall(136, addr, len, 0) as i32
}

/// Set (or query) the program break.  `addr = 0` returns the current break.
pub fn brk(addr: usize) -> usize {
    syscall(137, addr, 0, 0)
}

// ── eBPF-compatible trace VM ──────────────────────────────────────────────────

/// Load a trace program (bytecode slice) into the kernel.
/// Returns a program handle (>= 0) on success, or usize::MAX on error.
pub fn trace_load(prog: &[u8]) -> usize {
    syscall(140, prog.as_ptr() as usize, prog.len(), 0)
}

/// Attach a loaded trace program (by handle) to a trace point ID.
/// Returns 0 on success.
pub fn trace_attach(handle: usize, tp_id: usize) -> usize {
    syscall(141, handle, tp_id, 0)
}

/// Read up to `buf.len()` bytes from the kernel trace ring buffer.
/// Returns the number of bytes written into `buf`.
pub fn trace_read(buf: &mut [u8]) -> usize {
    syscall(142, buf.as_mut_ptr() as usize, buf.len(), 0)
}

// ── Ports and Jobs (Fuchsia/Zircon level capabilities) ───────────────────────

pub const SIGNAL_READABLE: u32 = 1 << 0;
pub const SIGNAL_WRITABLE: u32 = 1 << 1;
pub const SIGNAL_PEER_CLOSED: u32 = 1 << 2;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PortPacket {
    pub key: u64,
    pub type_: u32,
    pub status: i32,
    pub observed_signals: u32,
}

#[unsafe(no_mangle)]
pub extern "C" fn port_create(out_handle: *mut u32) -> i32 {
    syscall(70, out_handle as usize, 0, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn port_wait(port_handle: u32, out_packet: *mut PortPacket) -> i32 {
    syscall(71, port_handle as usize, out_packet as usize, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn port_queue(port_handle: u32, packet: *const PortPacket) -> i32 {
    syscall(72, port_handle as usize, packet as usize, 0) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn port_bind(
    object_handle: u32,
    port_handle: u32,
    key: u64,
    trigger_signals: u32,
) -> i32 {
    syscall4(
        73,
        object_handle as usize,
        port_handle as usize,
        key as usize,
        trigger_signals as usize,
    ) as i32
}

pub const JOB_POLICY_MAX_MEMORY: u32 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn job_create(
    parent_job_handle: u32,
    options: u32,
    out_job_handle: *mut u32,
) -> i32 {
    let ret = syscall(74, parent_job_handle as usize, options as usize, 0);
    if ret >= (usize::MAX - 200) {
        return ret as i32;
    }
    unsafe {
        if !out_job_handle.is_null() {
            *out_job_handle = ret as u32;
        }
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn job_set_policy(job_handle: u32, policy_type: u32, value: usize) -> i32 {
    syscall(75, job_handle as usize, policy_type as usize, value) as i32
}

pub const RIGHTS_READ: u32 = 1 << 0;
pub const RIGHTS_WRITE: u32 = 1 << 1;
pub const RIGHTS_DUPLICATE: u32 = 1 << 2;
pub const RIGHTS_TRANSFER: u32 = 1 << 3;

#[unsafe(no_mangle)]
pub extern "C" fn handle_duplicate(handle: u32, rights: u32, out_handle: *mut u32) -> i32 {
    let ret = syscall(76, handle as usize, rights as usize, 0);
    if ret >= (usize::MAX - 200) {
        return ret as i32;
    }
    unsafe {
        if !out_handle.is_null() {
            *out_handle = ret as u32;
        }
    }
    0
}

// ── Namespace ─────────────────────────────────────────────────────────────────

/// Returns the calling process's mount namespace ID.
pub fn getnsid() -> u32 {
    syscall(170, 0, 0, 0) as u32
}

// ── Priority scheduling ────────────────────────────────────────────────────────

pub const PRIO_REALTIME: i32 = 0;
pub const PRIO_HIGH: i32     = 8;
pub const PRIO_NORMAL: i32   = 16;
pub const PRIO_LOW: i32      = 24;
pub const PRIO_IDLE: i32     = 31;

/// Sets the calling process's scheduling priority (0 = highest, 31 = lowest).
pub fn setpriority(prio: i32) -> i32 {
    syscall(167, prio as usize, 0, 0) as i32
}

/// Returns the calling process's scheduling priority.
pub fn getpriority() -> i32 {
    syscall(168, 0, 0, 0) as i32
}

/// Grants a subset of the caller's capability mask to another process.
pub fn cap_grant(target_pid: i32, caps: u64) -> i32 {
    syscall(169, target_pid as usize, caps as usize, 0) as i32
}

// ── VMO wrappers ───────────────────────────────────────────────────────────────

/// Creates a Virtual Memory Object of `size` bytes. Returns a handle or negative errno.
pub fn vmo_create(size: usize) -> i32 {
    syscall(150, size, 0, 0) as i32
}

/// Maps a VMO handle into the calling process's address space.
/// Returns the mapped virtual address or `usize::MAX` on error.
pub fn vmo_map(handle: i32, addr: usize, size: usize, prot: u32) -> usize {
    syscall(151, handle as usize, addr, size | ((prot as usize) << 32))
}

/// Unmaps a previously mapped VMO region.
pub fn vmo_unmap(addr: usize, size: usize) -> i32 {
    syscall(152, addr, size, 0) as i32
}


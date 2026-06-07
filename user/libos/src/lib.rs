#![no_std]
extern crate alloc;

use buddy_system_allocator::LockedHeap;
use core::arch::asm;
use core::arch::global_asm;
use core::panic::PanicInfo;

// ── User-space heap ────────────────────────────────────────────────────────────
#[cfg(feature = "large-heap")]
const USER_HEAP_SIZE: usize = 8 * 1024 * 1024;
#[cfg(not(feature = "large-heap"))]
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

#[unsafe(no_mangle)]
pub extern "C" fn sys_yield() {
    syscall(0, 0, 0, 0);
}

fn close_all_vfs_fds() {
    for i in 0..256 {
        let res = syscall(81, i, 6 /* F_GET_VFS_FD */, 0);
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
    let res = syscall(81, fd, 6, 0);
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
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn read(fd: usize, buf: *mut u8, len: usize) -> isize {
    let res = syscall(81, fd, 6, 0);
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
    let res = syscall(81, fd as usize, 6, 0);
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

pub fn dup(oldfd: i32) -> i32 {
    let res = syscall(81, oldfd as usize, 6, 0);
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
    let res = syscall(81, oldfd as usize, 6, 0);
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
    vfs_proto::pack_path_req(&mut msg.data, flags, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as i32
    } else {
        -1
    }
}

pub fn vfs_create(path: &[u8]) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_CREATE;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len());
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
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_unlink(path: &[u8]) -> i32 {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_UNLINK;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len());
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
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        Some(vfs_proto::unpack_stat_reply(&msg.data))
    } else {
        None
    }
}

pub fn vfs_dup(vfs_fd: u32) -> Option<u32> {
    let mut msg = ipc::Message::new();
    msg.type_ = vfs_proto::OP_DUP;
    vfs_proto::pack_u32_reply(&mut msg.data, vfs_fd); // reuse u32 pack for the single fd
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        Some(vfs_proto::unpack_u32_reply(&msg.data))
    } else {
        None
    }
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
    syscall4(51, path.as_ptr() as usize, path.len(), 0, 0) as i32
}

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
    let res = syscall(81, fd as usize, 6, 0); // F_GET_VFS_FD
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

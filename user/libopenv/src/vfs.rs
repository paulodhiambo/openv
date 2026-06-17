use crate::ipc::Message;
use core::sync::atomic::{AtomicI32, Ordering};
use openv_syscall::{syscall0, syscall3, SYS_GET_VFS_PID, SYS_IPC_SENDREC};

/// PID of the VFS server (cached after first lookup).
static VFS_SERVER_PID: AtomicI32 = AtomicI32::new(0);

/// Returns the cached VFS server PID, or queries the kernel for it.
pub fn vfs_pid() -> Option<i32> {
    let pid = VFS_SERVER_PID.load(Ordering::Relaxed);
    if pid > 0 {
        return Some(pid);
    }
    let ret = unsafe { syscall0(SYS_GET_VFS_PID) };
    if ret != usize::MAX && ret > 0 {
        let pid = ret as i32;
        VFS_SERVER_PID.store(pid, Ordering::Relaxed);
        Some(pid)
    } else {
        None
    }
}

fn vfs_call(msg: &mut Message) -> bool {
    let server = match vfs_pid() {
        Some(p) => p,
        None => return false,
    };
    let ret = unsafe { syscall3(SYS_IPC_SENDREC, server as usize, msg as *mut Message as usize, 0) };
    ret == 0
}

pub fn vfs_open(path: &[u8], flags: u32) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_OPEN;
    vfs_proto::pack_path_req(&mut msg.data, flags, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as i32
    } else {
        -1
    }
}

pub fn vfs_create(path: &[u8]) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_CREATE;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as i32
    } else {
        -1
    }
}

pub fn vfs_read(vfs_fd: u32, offset: u64, buf: &mut [u8]) -> isize {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_READ;
    vfs_proto::pack_rw_req(&mut msg.data, vfs_fd, offset, buf.as_mut_ptr() as usize, buf.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_write(vfs_fd: u32, offset: u64, data: &[u8]) -> isize {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_WRITE;
    vfs_proto::pack_rw_req(&mut msg.data, vfs_fd, offset, data.as_ptr() as usize, data.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_close(vfs_fd: u32) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_CLOSE;
    vfs_proto::pack_close_req(&mut msg.data, vfs_fd);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_getdents(path: &[u8], buf: &mut [u8]) -> isize {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_GETDENTS;
    vfs_proto::pack_getdents_req(&mut msg.data, path.as_ptr() as usize, path.len(), buf.as_mut_ptr() as usize, buf.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_mkdir(path: &[u8]) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_MKDIR;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_unlink(path: &[u8]) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_UNLINK;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_rename(old: &[u8], new: &[u8]) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_RENAME;
    vfs_proto::pack_rename_req(&mut msg.data, old.as_ptr() as usize, old.len(), new.as_ptr() as usize, new.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_stat(path: &[u8]) -> Option<(bool, u64)> {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_STAT;
    vfs_proto::pack_path_req(&mut msg.data, 0, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        Some(vfs_proto::unpack_stat_reply(&msg.data))
    } else {
        None
    }
}

pub fn vfs_dup(vfs_fd: u32) -> Option<u32> {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_DUP;
    vfs_proto::pack_u32_reply(&mut msg.data, vfs_fd);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        Some(vfs_proto::unpack_u32_reply(&msg.data))
    } else {
        None
    }
}

pub fn vfs_fsync(vfs_fd: u32) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_FSYNC;
    vfs_proto::pack_close_req(&mut msg.data, vfs_fd);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        0
    } else {
        -1
    }
}

pub fn vfs_readlink(path: &[u8], out: &mut [u8]) -> isize {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_READLINK;
    vfs_proto::pack_getdents_req(&mut msg.data, path.as_ptr() as usize, path.len(), out.as_mut_ptr() as usize, out.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_symlink(link_path: &[u8], target: &[u8]) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_SYMLINK;
    vfs_proto::pack_two_paths_req(&mut msg.data, link_path.as_ptr() as usize, link_path.len(), target.as_ptr() as usize, target.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_link(old: &[u8], new: &[u8]) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_LINK;
    vfs_proto::pack_two_paths_req(&mut msg.data, old.as_ptr() as usize, old.len(), new.as_ptr() as usize, new.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_chmod(path: &[u8], mode: u32) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_CHMOD;
    vfs_proto::pack_path_req(&mut msg.data, mode, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_chown(path: &[u8], owner: u32, group: u32) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_CHOWN;
    vfs_proto::pack_path_req(&mut msg.data, owner, path.as_ptr() as usize, path.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_ftruncate(vfs_fd: u32, length: u64) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_FTRUNCATE;
    vfs_proto::pack_ftruncate_req(&mut msg.data, vfs_fd, length);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_lseek(vfs_fd: u32, offset: i64, whence: u32) -> i64 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_LSEEK;
    vfs_proto::pack_lseek_req(&mut msg.data, vfs_fd, offset, whence);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u64_reply(&msg.data) as i64
    } else {
        -1
    }
}

pub fn vfs_fpath(vfs_fd: u32, buf: &mut [u8]) -> isize {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_FPATH;
    vfs_proto::pack_fpath_req(&mut msg.data, vfs_fd, buf.as_mut_ptr() as usize, buf.len());
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK {
        vfs_proto::unpack_u32_reply(&msg.data) as isize
    } else {
        -1
    }
}

pub fn vfs_futimens(vfs_fd: u32, atime_sec: i64, atime_nsec: i64, mtime_sec: i64, mtime_nsec: i64) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_FUTIMENS;
    vfs_proto::pack_futimens_req(&mut msg.data, vfs_fd, atime_sec, atime_nsec, mtime_sec, mtime_nsec);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

pub fn vfs_fallocate(vfs_fd: u32, offset: u64, length: u64) -> i32 {
    let mut msg = Message::new();
    msg.type_ = vfs_proto::OP_FALLOCATE;
    vfs_proto::pack_fallocate_req(&mut msg.data, vfs_fd, offset, length);
    if vfs_call(&mut msg) && msg.type_ == vfs_proto::REPLY_OK { 0 } else { -1 }
}

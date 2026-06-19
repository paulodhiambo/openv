use core::fmt;
use libos::ipc::Message;

#[derive(Debug)]
#[allow(dead_code)]
pub enum FsError {
    NotFound,
    NotFile,
    NotDir,
    Exists,
    NoSpace,
    BadInode,
    MissingBlock,
    BlockRead,
    BlockWrite,
    InvalidName,
    CrossMount,
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsError::NotFound => write!(f, "not found"),
            FsError::NotFile => write!(f, "not a file"),
            FsError::NotDir => write!(f, "not a directory"),
            FsError::Exists => write!(f, "already exists"),
            FsError::NoSpace => write!(f, "no space left"),
            FsError::BadInode => write!(f, "bad inode"),
            FsError::MissingBlock => write!(f, "missing block"),
            FsError::BlockRead => write!(f, "block read error"),
            FsError::BlockWrite => write!(f, "block write error"),
            FsError::InvalidName => write!(f, "invalid name"),
            FsError::CrossMount => write!(f, "cross-mount not supported"),
        }
    }
}

#[allow(dead_code)]
pub struct InodeInfo {
    pub itype: u8,
    pub size: u32,
    pub nlink: u32,
}

pub struct VirtioBlkProxy {
    pid: i32,
}

impl VirtioBlkProxy {
    pub fn new(pid: i32) -> Self { Self { pid } }

    pub fn read_block(&self, blk: u32, buf: &mut [u8; 4096]) -> bool {
        let mut msg = Message::new();
        msg.type_ = 100;
        msg.data[0..4].copy_from_slice(&blk.to_le_bytes());
        msg.data[4..12].copy_from_slice(&(buf.as_mut_ptr() as usize).to_le_bytes());
        if libos::msg_sendrec(self.pid, &mut msg) != 0 { return false; }
        msg.type_ == 0
    }

    pub fn write_block(&self, blk: u32, buf: &[u8; 4096]) -> bool {
        let mut msg = Message::new();
        msg.type_ = 101;
        msg.data[0..4].copy_from_slice(&blk.to_le_bytes());
        msg.data[4..12].copy_from_slice(&(buf.as_ptr() as usize).to_le_bytes());
        if libos::msg_sendrec(self.pid, &mut msg) != 0 { return false; }
        msg.type_ == 0
    }
}

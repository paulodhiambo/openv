use crate::vfs::{Vnode, VnodeType, Stat, DirEntry};
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::string::String;

/// Root of /dev — contains null, zero, tty.
pub struct DevRoot;

impl Vnode for DevRoot {
    fn stat(&self) -> Stat {
        Stat { mode: 0o755, uid: 0, gid: 0, size: 0, vtype: VnodeType::Directory }
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Vnode>, &'static str> {
        match name {
            "null" => Ok(Arc::new(DevNull)),
            "zero" => Ok(Arc::new(DevZero)),
            "tty"  => Ok(Arc::new(DevTty)),
            _      => Err("Not found"),
        }
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, &'static str> {
        Ok(alloc::vec![
            DirEntry { name: String::from("null"), vtype: VnodeType::File },
            DirEntry { name: String::from("zero"), vtype: VnodeType::File },
            DirEntry { name: String::from("tty"),  vtype: VnodeType::File },
        ])
    }
}

/// /dev/null — reads return 0 bytes, writes succeed silently.
pub struct DevNull;

impl Vnode for DevNull {
    fn stat(&self) -> Stat {
        Stat { mode: 0o666, uid: 0, gid: 0, size: 0, vtype: VnodeType::File }
    }

    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, &'static str> {
        Ok(0)
    }

    fn write_at(&self, _offset: usize, buf: &[u8]) -> Result<usize, &'static str> {
        Ok(buf.len())
    }
}

/// /dev/zero — reads return zeroed bytes.
pub struct DevZero;

impl Vnode for DevZero {
    fn stat(&self) -> Stat {
        Stat { mode: 0o666, uid: 0, gid: 0, size: 0, vtype: VnodeType::File }
    }

    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        for b in buf.iter_mut() {
            *b = 0;
        }
        Ok(buf.len())
    }

    fn write_at(&self, _offset: usize, buf: &[u8]) -> Result<usize, &'static str> {
        Ok(buf.len())
    }
}

/// /dev/tty — alias for console (reads/writes go to UART).
pub struct DevTty;

impl Vnode for DevTty {
    fn stat(&self) -> Stat {
        Stat { mode: 0o666, uid: 0, gid: 0, size: 0, vtype: VnodeType::File }
    }

    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        let mut uart = crate::uart::Uart::new();
        if let Some(c) = uart.try_get_char() {
            buf[0] = c;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    fn write_at(&self, _offset: usize, buf: &[u8]) -> Result<usize, &'static str> {
        if let Ok(s) = core::str::from_utf8(buf) {
            crate::print!("{}", s);
        }
        Ok(buf.len())
    }
}

use crate::vfs::{Vnode, VnodeType, Stat, DirEntry};
use core::sync::atomic::Ordering;
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::string::String;

/// Root of /proc — enumerates live PIDs as subdirectories.
pub struct ProcRoot;

impl Vnode for ProcRoot {
    fn stat(&self) -> Stat {
        Stat { mode: 0o555, uid: 0, gid: 0, size: 0, vtype: VnodeType::Directory }
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Vnode>, &'static str> {
        let pid: i32 = name.parse().map_err(|_| "Not found")?;
        let table = crate::posix::process::PROCESS_TABLE.lock();
        if table.contains_key(&pid) {
            Ok(Arc::new(ProcPidDir { pid }))
        } else {
            Err("Not found")
        }
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, &'static str> {
        let table = crate::posix::process::PROCESS_TABLE.lock();
        let mut entries = Vec::new();
        for pid in table.keys() {
            let mut s = String::new();
            // format pid as decimal
            let n = *pid as u32;
            let digits = fmt_u32(n);
            s.push_str(core::str::from_utf8(&digits).unwrap_or("?"));
            entries.push(DirEntry { name: s, vtype: VnodeType::Directory });
        }
        Ok(entries)
    }
}

/// /proc/<pid> directory — contains "status".
pub struct ProcPidDir {
    pid: i32,
}

impl Vnode for ProcPidDir {
    fn stat(&self) -> Stat {
        Stat { mode: 0o555, uid: 0, gid: 0, size: 0, vtype: VnodeType::Directory }
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Vnode>, &'static str> {
        if name == "status" {
            Ok(Arc::new(ProcPidStatus { pid: self.pid }))
        } else {
            Err("Not found")
        }
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, &'static str> {
        Ok(alloc::vec![DirEntry { name: String::from("status"), vtype: VnodeType::File }])
    }
}

/// /proc/<pid>/status — text file with pid, ppid, state, uid.
pub struct ProcPidStatus {
    pid: i32,
}

impl Vnode for ProcPidStatus {
    fn stat(&self) -> Stat {
        Stat { mode: 0o444, uid: 0, gid: 0, size: 0, vtype: VnodeType::File }
    }

    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        let content = self.generate();
        let bytes = content.as_bytes();
        if offset >= bytes.len() {
            return Ok(0);
        }
        let available = &bytes[offset..];
        let to_copy = core::cmp::min(available.len(), buf.len());
        buf[..to_copy].copy_from_slice(&available[..to_copy]);
        Ok(to_copy)
    }
}

impl ProcPidStatus {
    fn generate(&self) -> String {
        let table = crate::posix::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.get(&self.pid) {
            let state_str = match *proc.state.lock() {
                crate::posix::process::ProcState::Running => "running",
                crate::posix::process::ProcState::Stopped => "stopped",
                crate::posix::process::ProcState::Zombie(_) => "zombie",
            };
            let mut s = String::new();
            s.push_str("Name:\t");
            s.push_str(state_str);
            s.push('\n');
            s.push_str("Pid:\t");
            append_u32(&mut s, self.pid as u32);
            s.push('\n');
            s.push_str("PPid:\t");
            append_u32(&mut s, proc.ppid as u32);
            s.push('\n');
            s.push_str("State:\t");
            s.push_str(state_str);
            s.push('\n');
            s.push_str("Uid:\t");
            append_u32(&mut s, proc.uid.load(Ordering::Relaxed));
            s.push('\n');
            s
        } else {
            String::from("pid not found\n")
        }
    }
}

fn append_u32(s: &mut String, n: u32) {
    let digits = fmt_u32(n);
    if let Ok(d) = core::str::from_utf8(&digits) {
        s.push_str(d);
    }
}

fn fmt_u32(mut n: u32) -> Vec<u8> {
    if n == 0 {
        return alloc::vec![b'0'];
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push(b'0' + (n % 10) as u8);
        n /= 10;
    }
    digits.reverse();
    digits
}

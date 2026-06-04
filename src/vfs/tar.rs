use crate::vfs::memfs::{MemDir, RoFile};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// Parses a basic ustar archive from memory and returns a populated MemDir root.
pub fn parse_tar(data: &[u8]) -> Arc<MemDir> {
    let root = Arc::new(MemDir::new(0, 0, 0o755));
    let mut dirs: BTreeMap<String, Arc<MemDir>> = BTreeMap::new();
    dirs.insert(String::new(), root.clone());

    let mut offset = 0;

    while offset + 512 <= data.len() {
        let header = &data[offset..offset + 512];

        // Check for empty block (end of archive)
        if header.iter().all(|&b| b == 0) {
            break;
        }

        // Parse filename
        let mut name_len = 0;
        while name_len < 100 && header[name_len] != 0 {
            name_len += 1;
        }
        let name_str = core::str::from_utf8(&header[0..name_len]).unwrap_or("unknown");

        // Parse mode (octal)
        let mut mode = 0;
        for i in 100..107 {
            let b = header[i];
            if b >= b'0' && b <= b'7' {
                mode = (mode << 3) | (b - b'0') as u32;
            }
        }

        // Parse uid (octal)
        let mut uid = 0;
        for i in 108..115 {
            let b = header[i];
            if b >= b'0' && b <= b'7' {
                uid = (uid << 3) | (b - b'0') as u32;
            }
        }

        // Parse gid (octal)
        let mut gid = 0;
        for i in 116..123 {
            let b = header[i];
            if b >= b'0' && b <= b'7' {
                gid = (gid << 3) | (b - b'0') as u32;
            }
        }

        // Parse size (octal ascii)
        let mut size = 0;
        for i in 124..135 {
            let b = header[i];
            if b >= b'0' && b <= b'7' {
                size = (size << 3) | (b - b'0') as usize;
            }
        }

        let typeflag = header[156];

        offset += 512;

        // Split path into components, strip leading/trailing slashes.
        let components: Vec<&str> = name_str
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .collect();
        if components.is_empty() {
            offset += (size + 511) & !511;
            continue;
        }

        if typeflag == b'5' {
            // Directory entry — create the directory and any missing parents.
            let mut parent_path = String::new();
            for &comp in &components {
                let child_path = if parent_path.is_empty() {
                    String::from(comp)
                } else {
                    let mut s = String::with_capacity(parent_path.len() + 1 + comp.len());
                    s.push_str(&parent_path);
                    s.push('/');
                    s.push_str(comp);
                    s
                };
                if !dirs.contains_key(&child_path) {
                    let parent_dir = dirs
                        .get(&parent_path)
                        .cloned()
                        .unwrap_or_else(|| root.clone());
                    let new_dir = Arc::new(MemDir::new(uid, gid, mode));
                    parent_dir.add_child(comp, new_dir.clone() as _);
                    dirs.insert(child_path.clone(), new_dir);
                }
                parent_path = child_path;
            }
        } else if typeflag == b'0' || typeflag == 0 {
            // Regular file
            let file_slice: &'static [u8] =
                unsafe { core::slice::from_raw_parts(data.as_ptr().add(offset), size) };
            let file_node = Arc::new(RoFile::new(file_slice, uid, gid, mode));

            // Ensure parent directory exists
            let parent_path = {
                if components.len() > 1 {
                    components[..components.len() - 1].join("/")
                } else {
                    String::new()
                }
            };
            if !parent_path.is_empty() && !dirs.contains_key(&parent_path) {
                // Create missing parent directories
                let mut pp = String::new();
                for &comp in &components[..components.len() - 1] {
                    let cp = if pp.is_empty() {
                        String::from(comp)
                    } else {
                        let mut s = String::with_capacity(pp.len() + 1 + comp.len());
                        s.push_str(&pp);
                        s.push('/');
                        s.push_str(comp);
                        s
                    };
                    if !dirs.contains_key(&cp) {
                        let parent_dir = dirs.get(&pp).cloned().unwrap_or_else(|| root.clone());
                        let new_dir = Arc::new(MemDir::new(0, 0, 0o755));
                        parent_dir.add_child(comp, new_dir.clone() as _);
                        dirs.insert(cp.clone(), new_dir);
                    }
                    pp = cp;
                }
            }

            let parent_dir = dirs
                .get(&parent_path)
                .cloned()
                .unwrap_or_else(|| root.clone());
            let components: Vec<&str> = name_str.split('/').filter(|c| !c.is_empty()).collect();
            let basename = components.last().unwrap_or(&"");
            parent_dir.add_child(basename, file_node);
        }

        // Advance offset past the file data, aligned to 512 bytes
        offset += (size + 511) & !511;
    }

    root
}

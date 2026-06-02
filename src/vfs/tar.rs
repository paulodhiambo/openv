use crate::vfs::memfs::{MemDir, MemFile};
use alloc::sync::Arc;

/// Parses a basic ustar archive from memory and returns a populated MemDir root.
pub fn parse_tar(data: &[u8]) -> Arc<MemDir> {
    let root = Arc::new(MemDir::new(0, 0, 0o755));
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
        
        if typeflag == b'0' || typeflag == 0 { // Regular file
            let file_data = data[offset..offset + size].to_vec();
            let file_node = Arc::new(MemFile::new(file_data, uid, gid, mode));
            
            // For simplicity, we just add the file to the root directory
            let basename = name_str.split('/').last().unwrap_or(name_str);
            if !basename.is_empty() {
                root.add_child(basename, file_node);
            }
        }
        
        // Advance offset past the file data, aligned to 512 bytes
        offset += (size + 511) & !511;
    }

    root
}

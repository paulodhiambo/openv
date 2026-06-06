static INITRD_DATA: spin::Once<Option<&'static [u8]>> = spin::Once::new();

pub unsafe fn init(data: &'static [u8]) {
    INITRD_DATA.call_once(|| Some(data));
}

/// A minimal TAR parser that scans linearly to find a file by exact name.
/// Used only by `posix_spawn` and `sys_exec` to load binaries before the VFS server is running.
pub fn kernel_initrd_lookup(path: &str) -> Option<&'static [u8]> {
    let data = INITRD_DATA.get()?.expect("Initrd data not initialized");
    let mut offset = 0;

    // Remove leading slash for matching since TAR paths usually don't have it,
    // or just match exact component. 
    // In our initrd TAR, paths are usually like "init", "bin/ls", etc.
    let target = path.trim_start_matches('/');

    while offset + 512 <= data.len() {
        let header = &data[offset..offset + 512];

        if header.iter().all(|&b| b == 0) {
            break; // End of archive
        }

        let mut name_len = 0;
        while name_len < 100 && header[name_len] != 0 {
            name_len += 1;
        }
        let name_str = core::str::from_utf8(&header[0..name_len]).unwrap_or("");

        let mut size = 0;
        for i in 124..135 {
            let b = header[i];
            if b >= b'0' && b <= b'7' {
                size = (size << 3) | (b - b'0') as usize;
            }
        }

        let typeflag = header[156];
        offset += 512; // Advance past header

        // Regular file
        if typeflag == b'0' || typeflag == 0 {
            let clean_name = name_str.trim_start_matches("./").trim_start_matches("/");
            crate::println!("[INITRD] looking for '{}', found '{}' (clean: '{}')", target, name_str, clean_name);
            if clean_name == target {
                // SAFETY: Ensure offset + size does not exceed data.len() to prevent out-of-bounds access.
                if offset + size > data.len() {
                    crate::println!("[INITRD] ERROR: Found '{}:{}' but out of bounds (data len: {})", name_str, size, data.len());
                    return None; // Malformed TAR header - data goes past end of archive
                }
                let file_slice = unsafe { core::slice::from_raw_parts(data.as_ptr().add(offset), size) };
                return Some(file_slice);
            }
        }

        // Advance past data, aligned to 512 blocks
        offset += (size + 511) & !511;
    }

    None
}

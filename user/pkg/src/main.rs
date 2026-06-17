#![no_std]
#![no_main]

extern crate alloc;

mod gzip;
mod ar;
mod http;
mod deb;
mod dns;

use libos::{close, create, getdents, mkdir, open, read, unlink, write};
use alloc::string::ToString;
use alloc::vec::Vec;
use alloc::vec;

const INSTALLED: &[u8] = b"/mnt/pkg/installed";
const CACHE_DIR: &[u8] = b"/mnt/pkg/cache";

// Default Debian mirror
const MIRROR_HOST: &[u8] = b"deb.debian.org";
const MIRROR_PORT: u16 = 80;
const MIRROR_PATH: &[u8] = b"/debian";
const ARCH: &[u8] = b"riscv64";
const SUITE: &[u8] = b"stable";
// Fallback hardcoded IP if DNS fails
const FALLBACK_MIRROR_IP: [u8; 4] = [151, 101, 2, 132];

fn stdout(msg: &[u8]) {
    write(1, msg.as_ptr(), msg.len());
}

fn stderr(msg: &[u8]) {
    write(2, msg.as_ptr(), msg.len());
}

// ── File helpers ──

fn read_file(path: &[u8], max: usize) -> Option<Vec<u8>> {
    let fd = open(path.as_ptr(), path.len(), 0);
    if fd < 0 { return None; }
    let mut buf = vec![0u8; max];
    let n = read(fd as usize, buf.as_mut_ptr(), max);
    close(fd);
    if n <= 0 { return None; }
    buf.truncate(n as usize);
    Some(buf)
}

fn write_file(path: &[u8], data: &[u8]) -> bool {
    let fd = create(path);
    if fd < 0 { return false; }
    let n = write(fd as usize, data.as_ptr(), data.len());
    close(fd);
    n as usize == data.len()
}

fn ensure_dir(path: &[u8]) -> bool {
    if mkdir(path) == 0 { return true; }
    true
}

fn resolve_mirror_ip() -> [u8; 4] {
    if let Some(ip) = dns::resolve_hostname(MIRROR_HOST) {
        return ip;
    }
    stdout(b"pkg: DNS resolution failed, using fallback IP\n");
    FALLBACK_MIRROR_IP
}

fn path_join(base: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(base.len() + 1 + name.len());
    p.extend_from_slice(base);
    p.push(b'/');
    p.extend_from_slice(name);
    p
}

// ── Tar helpers ──

fn octal_val(bytes: &[u8]) -> usize {
    let mut v = 0usize;
    for &b in bytes {
        if b == 0 || b == b' ' { break; }
        if b.is_ascii_digit() && b < b'8' { v = v * 8 + (b - b'0') as usize; }
    }
    v
}

struct TarEntry {
    name: Vec<u8>,
    data: Vec<u8>,
}

fn extract_tar(data: &[u8]) -> Vec<TarEntry> {
    let mut entries = Vec::new();
    let mut pos = 0usize;
    let mut consecutive_zero = 0u32;

    while pos + 512 <= data.len() {
        let header = &data[pos..pos + 512];
        if header.iter().all(|&b| b == 0) {
            consecutive_zero += 1;
            if consecutive_zero >= 2 { break; }
            pos += 512;
            continue;
        }
        consecutive_zero = 0;

        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = &header[..name_end];
        let size = octal_val(&header[124..136]);
        let type_flag = header[156];

        let data_start = pos + 512;
        let data_end = data_start + size;
        if data_end > data.len() { break; }

        if type_flag == 0 || type_flag == b'0' {
            entries.push(TarEntry {
                name: name.to_vec(),
                data: data[data_start..data_end].to_vec(),
            });
        }

        let blocks = size.div_ceil(512);
        pos = data_start + blocks * 512;
    }

    entries
}

fn tar_entry<'a>(entries: &'a [TarEntry], name: &[u8]) -> Option<&'a TarEntry> {
    entries.iter().find(|e| e.name.as_slice() == name)
}

// ── .deb installation ──

fn install_deb(deb_data: &[u8]) -> i32 {
    let ar_entries = match ar::parse_ar(deb_data) {
        Some(e) => e,
        None => { stderr(b"pkg: invalid .deb (not an ar archive)\n"); return 1; }
    };

    let control_tgz = match ar_entries.iter().find(|e| e.name == b"control.tar.gz" || e.name == b"control.tar.xz") {
        Some(e) => e,
        None => { stderr(b"pkg: .deb missing control.tar.gz\n"); return 1; }
    };

    if control_tgz.name.ends_with(b".xz") {
        stderr(b"pkg: .xz control not supported\n");
        return 1;
    }

    let control_tar = match gzip::gunzip(&control_tgz.data) {
        Some(d) => d,
        None => { stderr(b"pkg: failed to decompress control.tar.gz\n"); return 1; }
    };

    let control_entries = extract_tar(&control_tar);
    let control_file = match tar_entry(&control_entries, b"./control")
        .or_else(|| tar_entry(&control_entries, b"control"))
    {
        Some(e) => e,
        None => { stderr(b"pkg: control file not found in control.tar.gz\n"); return 1; }
    };

    let pkg_info = match deb::parse_control(&control_file.data) {
        Some(p) => p,
        None => { stderr(b"pkg: failed to parse control file\n"); return 1; }
    };

    let data_tgz = match ar_entries.iter().find(|e| e.name.starts_with(b"data.tar")) {
        Some(e) => e,
        None => { stderr(b"pkg: .deb missing data.tar.gz\n"); return 1; }
    };

    if !data_tgz.name.ends_with(b".gz") {
        stderr(b"pkg: only gzipped data.tar supported\n");
        return 1;
    }

    let data_tar = match gzip::gunzip(&data_tgz.data) {
        Some(d) => d,
        None => { stderr(b"pkg: failed to decompress data.tar.gz\n"); return 1; }
    };

    let data_entries = extract_tar(&data_tar);

    // Create /mnt/pkg/installed/<package>/
    let install_dir = path_join(INSTALLED, &pkg_info.package);
    ensure_dir(&install_dir);

    // Install files to /mnt/
    let mut manifest = Vec::new();
    for entry in &data_entries {
        // Remove leading "./" from tar paths
        let rel_path = if entry.name.starts_with(b"./") {
            &entry.name[2..]
        } else {
            &entry.name
        };
        if rel_path.is_empty() { continue; }

        let target = path_join(b"/mnt", rel_path);
        if let Some(slash) = target[..target.len() - 1].iter().rposition(|&b| b == b'/') {
            ensure_dir(&target[..slash]);
        }

        if write_file(&target, &entry.data) {
            manifest.extend_from_slice(rel_path);
            manifest.push(b'\n');
        } else {
            stderr(b"pkg: failed to write ");
            stderr(&target);
            stderr(b"\n");
        }
    }

    // Write manifest
    let manifest_path = path_join(&install_dir, b"manifest");
    write_file(&manifest_path, &manifest);

    // Write pkg-control metadata
    let ctrl_path = path_join(&install_dir, b"pkg-control");
    let mut ctrl_content = Vec::new();
    ctrl_content.extend_from_slice(b"Package: ");
    ctrl_content.extend_from_slice(&pkg_info.package);
    ctrl_content.extend_from_slice(b"\nVersion: ");
    ctrl_content.extend_from_slice(&pkg_info.version);
    ctrl_content.extend_from_slice(b"\nDescription: ");
    ctrl_content.extend_from_slice(&pkg_info.description);
    ctrl_content.extend_from_slice(b"\n");
    write_file(&ctrl_path, &ctrl_content);

    stdout(b"pkg: installed ");
    stdout(&pkg_info.package);
    stdout(b" ");
    stdout(&pkg_info.version);
    stdout(b" from .deb\n");

    0
}

// ── Apt repo commands ──

fn repo_url(suffix: &[u8]) -> Vec<u8> {
    let mut url = Vec::new();
    url.extend_from_slice(MIRROR_PATH);
    url.extend_from_slice(b"/dists/");
    url.extend_from_slice(SUITE);
    url.extend_from_slice(suffix);
    url
}

fn cmd_update() -> i32 {
    stdout(b"pkg: updating package index from Debian mirror...\n");

    let mirror_ip = resolve_mirror_ip();
    stdout(b"pkg: resolved ");
    stdout(MIRROR_HOST);
    stdout(b" to ");
    let ip_str = alloc::format!("{}.{}.{}.{}", mirror_ip[0], mirror_ip[1], mirror_ip[2], mirror_ip[3]);
    stdout(ip_str.as_bytes());
    stdout(b"\n");

    // Fetch Packages.gz for the architecture
    let pkg_gz_path = {
        let mut p = Vec::new();
        p.extend_from_slice(MIRROR_PATH);
        p.extend_from_slice(b"/dists/");
        p.extend_from_slice(SUITE);
        p.extend_from_slice(b"/main/binary-");
        p.extend_from_slice(ARCH);
        p.extend_from_slice(b"/Packages.gz");
        p
    };

    stdout(b"pkg: fetching ");
    stdout(&pkg_gz_path);
    stdout(b"\n");

    let (status, body) = match http::http_get(mirror_ip, MIRROR_PORT, MIRROR_HOST, &pkg_gz_path) {
        Some(r) => r,
        None => { stderr(b"pkg: HTTP request failed (network may be down)\n"); return 1; }
    };

    if status != 200 {
        stderr(b"pkg: HTTP ");
        stderr(status.to_string().as_bytes());
        stderr(b" fetching Packages.gz\n");
        return 1;
    }

    let index_data = match gzip::gunzip(&body) {
        Some(d) => d,
        None => { stderr(b"pkg: failed to decompress Packages.gz\n"); return 1; }
    };

    let packages = deb::parse_index(&index_data);
    stdout(b"pkg: found ");
    stdout(packages.len().to_string().as_bytes());
    stdout(b" packages in index\n");

    // Save index to /mnt/pkg/cache/Packages
    ensure_dir(CACHE_DIR);
    let index_path = path_join(CACHE_DIR, b"Packages");
    write_file(&index_path, &index_data);

    // Save the first 5 package names as a preview
    for (i, pkg) in packages.iter().enumerate() {
        if i >= 5 { break; }
        stdout(b"  - ");
        stdout(&pkg.package);
        stdout(b"\n");
        if i == 0 {
            // Save filename for the first package so we can download it
            if !pkg.filename.is_empty() {
                let fn_path = path_join(CACHE_DIR, b"last_filename");
                write_file(&fn_path, &pkg.filename);
            }
        }
    }

    0
}

fn resolve_deps(
    pkg: &deb::DebPackage,
    packages: &[deb::DebPackage],
    wanted: &mut Vec<Vec<u8>>,
    _depth: usize,
) {
    for dep in &pkg.depends {
        if !wanted.iter().any(|w| w.as_slice() == dep.as_slice()) {
            // Check if it's already installed
            let install_dir = path_join(INSTALLED, dep);
            let manifest_path = path_join(&install_dir, b"manifest");
            let already = read_file(&manifest_path, 1).is_some();
            if !already {
                wanted.push(dep.clone());
                // Also resolve sub-deps
                if let Some(dep_pkg) = packages.iter().find(|p| p.package.as_slice() == dep.as_slice()) {
                    resolve_deps(dep_pkg, packages, wanted, _depth + 1);
                }
            }
        }
    }
}

fn cmd_install_pkg(pkg_name: &[u8]) -> i32 {
    let mirror_ip = resolve_mirror_ip();
    // Read cached index
    let index_path = path_join(CACHE_DIR, b"Packages");
    let index_data = match read_file(&index_path, 4 * 1024 * 1024) {
        Some(d) => d,
        None => { stderr(b"pkg: no package index. Run 'pkg update' first.\n"); return 1; }
    };

    let packages = deb::parse_index(&index_data);

    // Find the requested package
    let target_pkg = match packages.iter().find(|p| p.package.as_slice() == pkg_name) {
        Some(p) => p,
        None => {
            stderr(b"pkg: package '");
            stderr(pkg_name);
            stderr(b"' not found in index\n");
            return 1;
        }
    };

    // Resolve dependencies
    let mut wanted = Vec::new();
    resolve_deps(target_pkg, &packages, &mut wanted, 0);
    // Insert the target package at the beginning
    wanted.insert(0, pkg_name.to_vec());

    stdout(b"pkg: will install:");
    for w in &wanted {
        stdout(b" ");
        stdout(w);
    }
    stdout(b"\n");

    // Download and install each package
    for w in &wanted {
        let pkg = match packages.iter().find(|p| p.package.as_slice() == w.as_slice()) {
            Some(p) => p,
            None => {
                stderr(b"pkg: dependency '");
                stderr(w);
                stderr(b"' not found\n");
                return 1;
            }
        };

        if pkg.filename.is_empty() {
            stderr(b"pkg: no filename for package ");
            stderr(w);
            stderr(b"\n");
            return 1;
        }

        let deb_url = {
            let mut u = Vec::new();
            u.extend_from_slice(MIRROR_PATH);
            u.push(b'/');
            u.extend_from_slice(&pkg.filename);
            u
        };

        stdout(b"pkg: downloading ");
        stdout(&pkg.package);
        stdout(b"...\n");

        let (status, body) = match http::http_get(mirror_ip, MIRROR_PORT, MIRROR_HOST, &deb_url) {
            Some(r) => r,
            None => {
                stderr(b"pkg: failed to download ");
                stderr(&pkg.package);
                stderr(b"\n");
                return 1;
            }
        };

        if status != 200 {
            stderr(b"pkg: HTTP ");
            stderr(status.to_string().as_bytes());
            stderr(b" downloading ");
            stderr(&pkg.package);
            stderr(b"\n");
            return 1;
        }

        stdout(b"pkg: installing ");
        stdout(&pkg.package);
        stdout(b"...\n");

        let ret = install_deb(&body);
        if ret != 0 {
            stderr(b"pkg: failed to install ");
            stderr(&pkg.package);
            stderr(b"\n");
            return ret;
        }
    }

    0
}

// ── Original commands ──

fn parse_kv(line: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let eq = line.iter().position(|&b| b == b'=')?;
    let key = line[..eq].iter().map(|&b| b.to_ascii_lowercase()).collect();
    let val = line[eq + 1..].to_vec();
    Some((key, val))
}

struct PkgControl {
    name: Vec<u8>,
    version: Vec<u8>,
}

fn parse_control_old(data: &[u8]) -> Option<PkgControl> {
    let mut name = None;
    let mut version = None;
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() { continue; }
        if let Some((key, val)) = parse_kv(line) {
            match key.as_slice() {
                b"name" => name = Some(val),
                b"version" => version = Some(val),
                _ => {}
            }
        }
    }
    Some(PkgControl { name: name?, version: version.unwrap_or(b"0.0".to_vec()) })
}

fn cmd_install(path: &[u8]) -> i32 {
    // Check if it's a .deb file
    if path.ends_with(b".deb") {
        let data = match read_file(path, 32 * 1024 * 1024) {
            Some(d) => d,
            None => { stderr(b"pkg: cannot open package file\n"); return 1; }
        };
        return install_deb(&data);
    }

    // Original tar-based install
    let fd = open(path.as_ptr(), path.len(), 0);
    if fd < 0 { stderr(b"pkg: cannot open package file\n"); return 1; }

    let mut pkg_name: Option<Vec<u8>> = None;
    let mut pkg_version: Option<Vec<u8>> = None;
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    let mut header = [0u8; 512];
    let mut consecutive_zero = 0u32;

    loop {
        let n = read(fd as usize, header.as_mut_ptr(), 512);
        if n != 512 { break; }
        if header.iter().all(|&b| b == 0) {
            consecutive_zero += 1;
            if consecutive_zero >= 2 { break; }
            continue;
        }
        consecutive_zero = 0;

        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = &header[..name_end];
        if name.is_empty() { read(fd as usize, header.as_mut_ptr(), 512); continue; }
        let type_flag = header[156];
        let size = octal_val(&header[124..136]);

        let blocks = size.div_ceil(512);
        let total = blocks * 512;

        match type_flag {
            0 | b'0' => {
                let mut data = vec![0u8; size];
                let n = read(fd as usize, data.as_mut_ptr(), size);
                if n < 0 { close(fd); return 1; }
                let padding = total - size;
                if padding > 0 { let mut pad = [0u8; 512]; read(fd as usize, pad.as_mut_ptr(), padding); }

                if name == b"pkg-control" {
                    if let Some(ctrl) = parse_control_old(&data) {
                        pkg_name = Some(ctrl.name);
                        pkg_version = Some(ctrl.version);
                    }
                } else {
                    entries.push((name.to_vec(), data));
                }
            }
            _ => {
                let mut skip = vec![0u8; total];
                read(fd as usize, skip.as_mut_ptr(), total);
            }
        }
    }
    close(fd);

    let name = match pkg_name {
        Some(n) => n,
        None => { stderr(b"pkg: package has no pkg-control\n"); return 1; }
    };
    let version = pkg_version.clone().unwrap_or(b"0.0".to_vec());

    let install_dir = path_join(INSTALLED, &name);
    ensure_dir(&install_dir);

    let ctrl_path = path_join(&install_dir, b"pkg-control");
    let mut ctrl_content = Vec::new();
    ctrl_content.extend_from_slice(b"name = ");
    ctrl_content.extend_from_slice(&name);
    ctrl_content.extend_from_slice(b"\nversion = ");
    ctrl_content.extend_from_slice(&version);
    ctrl_content.extend_from_slice(b"\n");
    write_file(&ctrl_path, &ctrl_content);

    let mut manifest = Vec::new();
    for (rel_path, data) in &entries {
        let target = path_join(b"/mnt", rel_path);
        if let Some(slash) = target[..target.len() - 1].iter().rposition(|&b| b == b'/') {
            ensure_dir(&target[..slash]);
        }
        if write_file(&target, data) {
            manifest.extend_from_slice(rel_path);
            manifest.push(b'\n');
        } else {
            stderr(b"pkg: failed to write ");
            stderr(&target);
            stderr(b"\n");
        }
    }

    let manifest_path = path_join(&install_dir, b"manifest");
    write_file(&manifest_path, &manifest);

    stdout(b"pkg: installed ");
    stdout(&name);
    stdout(b" ");
    stdout(&version);
    stdout(b"\n");

    0
}

fn cmd_remove(name: &[u8]) -> i32 {
    let install_dir = path_join(INSTALLED, name);
    let manifest_path = path_join(&install_dir, b"manifest");

    let manifest_data = match read_file(&manifest_path, 65536) {
        Some(d) => d,
        None => { stderr(b"pkg: package not installed\n"); return 1; }
    };

    for line in manifest_data.split(|&b| b == b'\n') {
        if line.is_empty() { continue; }
        let target = path_join(b"/mnt", line);
        unlink(&target);
    }

    unlink(&manifest_path);
    let ctrl_path = path_join(&install_dir, b"pkg-control");
    unlink(&ctrl_path);
    unlink(&install_dir);

    stdout(b"pkg: removed ");
    stdout(name);
    stdout(b"\n");

    0
}

fn cmd_list() -> i32 {
    let mut buf = [0u8; 4096];
    let n = getdents(INSTALLED.as_ptr(), INSTALLED.len(), buf.as_mut_ptr(), buf.len());
    if n < 0 { stdout(b"pkg: no packages installed\n"); return 0; }
    let mut pos = 0;
    while pos < n as usize {
        let start = pos;
        while pos < n as usize && buf[pos] != 0 { pos += 1; }
        if pos > start {
            stdout(&buf[start..pos]);
            stdout(b"\n");
        }
        pos += 1;
    }
    0
}

fn cmd_info(name: &[u8]) -> i32 {
    let ctrl_path = path_join(&path_join(INSTALLED, name), b"pkg-control");
    let data = match read_file(&ctrl_path, 4096) {
        Some(d) => d,
        None => { stderr(b"pkg: package not installed\n"); return 1; }
    };
    stdout(&data);
    0
}

fn usage() -> i32 {
    stdout(b"Usage: pkg <command> [args]\n");
    stdout(b"Commands:\n");
    stdout(b"  install <path>     Install a .tar or .deb package file\n");
    stdout(b"  remove <name>      Remove an installed package\n");
    stdout(b"  list               List installed packages\n");
    stdout(b"  info <name>        Show package info\n");
    stdout(b"  update             Update package index from Debian mirror\n");
    stdout(b"  install-pkg <name> Install package from Debian repo\n");
    1
}

unsafe fn cstr_slice(ptr: *const u8) -> &'static [u8] {
    let mut len = 0;
    unsafe {
        while *ptr.add(len) != 0 { len += 1; }
        core::slice::from_raw_parts(ptr, len)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn main(argc: i32, argv: *const *const u8) -> i32 {
    if argc < 2 { return usage(); }
    let cmd = unsafe { cstr_slice(*argv.add(1)) };

    match cmd {
        b"install" | b"deb-install" => {
            if argc < 3 { stderr(b"Usage: pkg install <path>\n"); return 1; }
            let path = unsafe { cstr_slice(*argv.add(2)) };
            cmd_install(path)
        }
        b"remove" => {
            if argc < 3 { stderr(b"Usage: pkg remove <name>\n"); return 1; }
            let name = unsafe { cstr_slice(*argv.add(2)) };
            cmd_remove(name)
        }
        b"list" => cmd_list(),
        b"info" => {
            if argc < 3 { stderr(b"Usage: pkg info <name>\n"); return 1; }
            let name = unsafe { cstr_slice(*argv.add(2)) };
            cmd_info(name)
        }
        b"update" => cmd_update(),
        b"install-pkg" => {
            if argc < 3 { stderr(b"Usage: pkg install-pkg <name>\n"); return 1; }
            let name = unsafe { cstr_slice(*argv.add(2)) };
            cmd_install_pkg(name)
        }
        _ => usage(),
    }
}

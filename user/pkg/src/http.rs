use libos::{close, connect, recv_timeout, send, socket};
use alloc::vec::Vec;

fn sockaddr_in(ip: [u8; 4], port: u16) -> [u8; 16] {
    let mut addr = [0u8; 16];
    addr[0] = 2; addr[1] = 0; // AF_INET (LE)
    addr[2] = (port >> 8) as u8;
    addr[3] = (port & 0xff) as u8; // port (BE)
    addr[4] = ip[0]; addr[5] = ip[1]; addr[6] = ip[2]; addr[7] = ip[3];
    addr
}

pub fn http_get(ip: [u8; 4], port: u16, host: &[u8], path: &[u8]) -> Option<(u16, Vec<u8>)> {
    use libos::write as raw_write;
    raw_write(1, b"http[A] socket\n".as_ptr(), 15);
    let fd = socket(2, 1, 0);
    if fd < 0 {
        return None;
    }
    raw_write(1, b"http[B] connect\n".as_ptr(), 16);

    let addr = sockaddr_in(ip, port);
    if connect(fd, addr.as_ptr(), 16) < 0 {
        close(fd);
        return None;
    }
    raw_write(1, b"http[C] send\n".as_ptr(), 13);

    let mut req = Vec::new();
    req.extend_from_slice(b"GET ");
    req.extend_from_slice(path);
    req.extend_from_slice(b" HTTP/1.0\r\n");
    req.extend_from_slice(b"Host: ");
    req.extend_from_slice(host);
    req.extend_from_slice(b"\r\n");
    req.extend_from_slice(b"Connection: close\r\n");
    req.extend_from_slice(b"User-Agent: openv-pkg/0.1\r\n");
    req.extend_from_slice(b"\r\n");

    let n = send(fd, req.as_ptr(), req.len(), 0);
    if n != req.len() as isize {
        close(fd);
        return None;
    }
    raw_write(1, b"http[D] recv\n".as_ptr(), 13);

    let mut buf = [0u8; 8192];
    let mut response = Vec::new();
    loop {
        let n = recv_timeout(fd as usize, buf.as_mut_ptr(), buf.len(), 50000);
        if n <= 0 {
            break;
        }
        response.extend_from_slice(&buf[..n as usize]);
    }
    raw_write(1, b"http[E] done\n".as_ptr(), 13);
    close(fd);

    if response.is_empty() {
        raw_write(1, b"http[E1] empty\n".as_ptr(), 15);
        return None;
    }
    raw_write(1, b"http[F] parsing\n".as_ptr(), 16);

    // Parse status line
    let status_end = response.iter().position(|&b| b == b'\r').unwrap_or(response.len());
    let status_line = &response[..status_end];
    if status_line.len() < 12 {
        raw_write(1, b"http[F1] short status\n".as_ptr(), 22);
        return None;
    }
    let status_str = core::str::from_utf8(&status_line[9..12]).ok()?;
    let status: u16 = status_str.parse().ok()?;
    raw_write(1, b"http[G] headers\n".as_ptr(), 16);

    // Find header end
    let header_end = match response.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(h) => h,
        None => {
            raw_write(1, b"http[G1] no hdr end\n".as_ptr(), 20);
            return None;
        }
    };
    let body_start = header_end + 4;

    fn trim_ascii_start(s: &[u8]) -> &[u8] {
        let mut i = 0;
        while i < s.len() && (s[i] == b' ' || s[i] == b'\t') { i += 1; }
        &s[i..]
    }

    // Parse Content-Length — guard against status_end+2 > header_end
    let headers_start = (status_end + 2).min(header_end);
    let headers = &response[headers_start..header_end];
    let mut content_length = None;
    for line in headers.split(|&b| b == b'\n') {
        let line = if line.last() == Some(&b'\r') { &line[..line.len() - 1] } else { line };
        if let Some(rest) = line.strip_prefix(b"Content-Length:") {
            let rest = trim_ascii_start(rest);
            if let Ok(s) = core::str::from_utf8(rest) {
                if let Ok(n) = s.parse::<usize>() {
                    content_length = Some(n);
                }
            }
        } else if let Some(rest) = line.strip_prefix(b"content-length:") {
            let rest = trim_ascii_start(rest);
            if let Ok(s) = core::str::from_utf8(rest) {
                if let Ok(n) = s.parse::<usize>() {
                    content_length = Some(n);
                }
            }
        }
    }
    raw_write(1, b"http[H] body\n".as_ptr(), 13);

    let body = if let Some(len) = content_length {
        if body_start + len > response.len() {
            raw_write(1, b"http[H1] truncated\n".as_ptr(), 19);
            return None;
        }
        response[body_start..body_start + len].to_vec()
    } else {
        response[body_start..].to_vec()
    };
    // Free the full response buffer now; caller will decompress body separately.
    drop(response);
    raw_write(1, b"http[I] return\n".as_ptr(), 15);

    Some((status, body))
}

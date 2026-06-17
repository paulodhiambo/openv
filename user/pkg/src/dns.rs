use libos::{close, connect, recv, send, socket};
use alloc::vec::Vec;

const DNS_SERVER: [u8; 4] = [10, 0, 2, 3];

fn sockaddr_in(ip: [u8; 4], port: u16) -> [u8; 16] {
    let mut addr = [0u8; 16];
    addr[0] = 2; addr[1] = 0;
    addr[2] = (port >> 8) as u8;
    addr[3] = (port & 0xff) as u8;
    addr[4] = ip[0]; addr[5] = ip[1]; addr[6] = ip[2]; addr[7] = ip[3];
    addr
}

fn encode_dns_name(name: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for label in name.split(|&b| b == b'.') {
        encoded.push(label.len() as u8);
        encoded.extend_from_slice(label);
    }
    encoded.push(0);
    encoded
}

fn build_dns_query(name: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::new();
    // Header: ID=1, flags=0x0100 (standard query, recursion desired)
    pkt.extend_from_slice(&[0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    // Question
    pkt.extend_from_slice(&encode_dns_name(name));
    pkt.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // QTYPE=A, QCLASS=IN
    pkt
}

fn parse_dns_name(data: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    let mut name = Vec::new();
    loop {
        if *pos >= data.len() { return None; }
        let len = data[*pos] as usize;
        if len == 0 { *pos += 1; break; }
        // Check for pointer (top 2 bits set)
        if len & 0xc0 == 0xc0 {
            if *pos + 1 >= data.len() { return None; }
            let ptr = ((len & 0x3f) << 8) | data[*pos + 1] as usize;
            *pos += 2;
            let mut inner_pos = ptr;
            let suffix = parse_dns_name(data, &mut inner_pos)?;
            if !name.is_empty() { name.push(b'.'); }
            name.extend_from_slice(&suffix);
            return Some(name);
        }
        if *pos + 1 + len > data.len() { return None; }
        *pos += 1;
        if !name.is_empty() { name.push(b'.'); }
        name.extend_from_slice(&data[*pos..*pos + len]);
        *pos += len;
    }
    Some(name)
}

fn parse_ip(data: &[u8], pos: &mut usize, rdlength: usize) -> Option<[u8; 4]> {
    if rdlength != 4 || *pos + 4 > data.len() { return None; }
    let ip = [data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]];
    *pos += rdlength;
    Some(ip)
}

pub fn resolve_hostname(name: &[u8]) -> Option<[u8; 4]> {
    let query = build_dns_query(name);

    // Wrap with 2-byte length prefix for TCP mode
    let mut tcp_query = Vec::new();
    let len = query.len() as u16;
    tcp_query.push((len >> 8) as u8);
    tcp_query.push((len & 0xff) as u8);
    tcp_query.extend_from_slice(&query);

    let fd = socket(2, 1, 0);
    if fd < 0 { return None; }

    let addr = sockaddr_in(DNS_SERVER, 53);
    if connect(fd, addr.as_ptr(), 16) < 0 {
        close(fd);
        return None;
    }

    let n = send(fd, tcp_query.as_ptr(), tcp_query.len(), 0);
    if n != tcp_query.len() as isize {
        close(fd);
        return None;
    }

    let mut buf = [0u8; 2048];
    let mut response = Vec::new();
    loop {
        let n = recv(fd, buf.as_mut_ptr(), buf.len(), 0);
        if n <= 0 { break; }
        response.extend_from_slice(&buf[..n as usize]);
    }
    close(fd);

    if response.len() < 2 { return None; }
    let pkt_len = (response[0] as usize) << 8 | response[1] as usize;
    if response.len() < pkt_len + 2 { return None; }

    let pkt = &response[2..2 + pkt_len];
    if pkt.len() < 12 { return None; }

    // Check reply code (last 4 bits of flags[1])
    let flags_hi = pkt[2];
    let flags_lo = pkt[3];
    let rcode = flags_lo & 0x0f;
    if rcode != 0 { return None; }

    let qdcount = (pkt[4] as usize) << 8 | pkt[5] as usize;
    let ancount = (pkt[6] as usize) << 8 | pkt[7] as usize;
    if ancount == 0 { return None; }

    // Skip question section
    let mut pos = 12usize;
    for _ in 0..qdcount {
        parse_dns_name(pkt, &mut pos)?;
        pos += 4; // skip QTYPE + QCLASS
    }

    // Parse answer section
    for _ in 0..ancount {
        parse_dns_name(pkt, &mut pos)?;
        if pos + 10 > pkt.len() { return None; }
        let _atype = (pkt[pos] as usize) << 8 | pkt[pos + 1] as usize;
        let _aclass = (pkt[pos + 2] as usize) << 8 | pkt[pos + 3] as usize;
        let _ttl = (pkt[pos + 4] as u32) << 24 | (pkt[pos + 5] as u32) << 16
            | (pkt[pos + 6] as u32) << 8 | pkt[pos + 7] as u32;
        let rdlength = (pkt[pos + 8] as usize) << 8 | pkt[pos + 9] as usize;
        pos += 10;

        if _atype == 1 { // A record
            if let Some(ip) = parse_ip(pkt, &mut pos, rdlength) {
                return Some(ip);
            }
        } else {
            pos += rdlength;
        }
    }

    None
}

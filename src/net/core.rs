//! Network core utilities: ethernet, ARP, IPv4 helpers and checksums.

pub type MacAddr = [u8; 6];

pub const BROADCAST_MAC: MacAddr = [0xff; 6];

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_IPV6: u16 = 0x86DD;

pub const IP_PROTO_ICMP: u8 = 1;
pub const IP_PROTO_TCP: u8 = 6;
pub const IP_PROTO_UDP: u8 = 17;

#[derive(Debug, Clone, Copy)]
pub struct EthHeader {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
}

impl EthHeader {
    pub const LEN: usize = 14;

    pub fn parse(buf: &[u8]) -> Option<(Self, &[u8])> {
        if buf.len() < Self::LEN {
            return None;
        }
        let mut dst = [0u8; 6];
        let mut src = [0u8; 6];
        dst.copy_from_slice(&buf[0..6]);
        src.copy_from_slice(&buf[6..12]);
        let ethertype = u16::from_be_bytes([buf[12], buf[13]]);
        Some((EthHeader { dst, src, ethertype }, &buf[Self::LEN..]))
    }

    pub fn write_into(&self, dst_buf: &mut [u8]) -> Option<usize> {
        if dst_buf.len() < Self::LEN { return None; }
        dst_buf[0..6].copy_from_slice(&self.dst);
        dst_buf[6..12].copy_from_slice(&self.src);
        dst_buf[12..14].copy_from_slice(&self.ethertype.to_be_bytes());
        Some(Self::LEN)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ArpPacket {
    pub htype: u16,
    pub ptype: u16,
    pub hlen: u8,
    pub plen: u8,
    pub oper: u16,
    pub sha: MacAddr,
    pub spa: [u8; 4],
    pub tha: MacAddr,
    pub tpa: [u8; 4],
}

impl ArpPacket {
    pub const LEN: usize = 28; // fixed for IPv4+Ethernet

    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::LEN { return None; }
        let htype = u16::from_be_bytes([buf[0], buf[1]]);
        let ptype = u16::from_be_bytes([buf[2], buf[3]]);
        let hlen = buf[4];
        let plen = buf[5];
        let oper = u16::from_be_bytes([buf[6], buf[7]]);
        let mut sha = [0u8; 6]; sha.copy_from_slice(&buf[8..14]);
        let mut spa = [0u8; 4]; spa.copy_from_slice(&buf[14..18]);
        let mut tha = [0u8; 6]; tha.copy_from_slice(&buf[18..24]);
        let mut tpa = [0u8; 4]; tpa.copy_from_slice(&buf[24..28]);
        Some(ArpPacket { htype, ptype, hlen, plen, oper, sha, spa, tha, tpa })
    }

    pub fn write_into(&self, out: &mut [u8]) -> Option<usize> {
        if out.len() < Self::LEN { return None; }
        out[0..2].copy_from_slice(&self.htype.to_be_bytes());
        out[2..4].copy_from_slice(&self.ptype.to_be_bytes());
        out[4] = self.hlen;
        out[5] = self.plen;
        out[6..8].copy_from_slice(&self.oper.to_be_bytes());
        out[8..14].copy_from_slice(&self.sha);
        out[14..18].copy_from_slice(&self.spa);
        out[18..24].copy_from_slice(&self.tha);
        out[24..28].copy_from_slice(&self.tpa);
        Some(Self::LEN)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Ipv4Header {
    pub ihl: u8,
    pub tos: u8,
    pub total_len: u16,
    pub id: u16,
    pub flags_fragment: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub checksum: u16,
    pub src: [u8;4],
    pub dst: [u8;4],
}

impl Ipv4Header {
    pub const MIN_LEN: usize = 20;

    pub fn parse(buf: &[u8]) -> Option<(Self, &[u8])> {
        if buf.len() < Self::MIN_LEN { return None; }
        let vihl = buf[0];
        let ihl = vihl & 0x0f;
        let header_len = (ihl as usize) * 4;
        if buf.len() < header_len { return None; }
        let tos = buf[1];
        let total_len = u16::from_be_bytes([buf[2], buf[3]]);
        let id = u16::from_be_bytes([buf[4], buf[5]]);
        let flags_fragment = u16::from_be_bytes([buf[6], buf[7]]);
        let ttl = buf[8];
        let protocol = buf[9];
        let checksum = u16::from_be_bytes([buf[10], buf[11]]);
        let mut src = [0u8;4]; src.copy_from_slice(&buf[12..16]);
        let mut dst = [0u8;4]; dst.copy_from_slice(&buf[16..20]);
        let payload = if (total_len as usize) >= header_len && (total_len as usize) <= buf.len() {
            &buf[header_len..(total_len as usize)]
        } else {
            &buf[header_len..]
        };
        Some((Ipv4Header { ihl, tos, total_len, id, flags_fragment, ttl, protocol, checksum, src, dst }, payload))
    }
}

/// Compute 16-bit ones-complement checksum over data (RFC 1071 style)
pub fn checksum16(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0usize;
    while i + 1 < data.len() {
        let word = ((data[i] as u16) << 8) | (data[i+1] as u16);
        sum = sum.wrapping_add(word as u32);
        i += 2;
    }
    if i < data.len() {
        let word = (data[i] as u16) << 8;
        sum = sum.wrapping_add(word as u32);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// IPv4 header checksum helper
pub fn ipv4_checksum(header_bytes: &[u8]) -> u16 {
    checksum16(header_bytes)
}

/// Helper to build Ethernet frame header into provided buffer
pub fn build_eth_header(dst: MacAddr, src: MacAddr, ethertype: u16, out: &mut [u8]) -> Option<usize> {
    if out.len() < EthHeader::LEN { return None; }
    out[0..6].copy_from_slice(&dst);
    out[6..12].copy_from_slice(&src);
    out[12..14].copy_from_slice(&ethertype.to_be_bytes());
    Some(EthHeader::LEN)
}

//! # Network Core Utilities
//!
//! This module provides low-level network protocol utilities, including
//! Ethernet, ARP, and IPv4 header parsing and serialization, as well as
//! checksum computation.
//!
//! ## Overview
//!
//! The module provides:
//!
//! - **Ethernet**: [`EthHeader`] for parsing and writing Ethernet headers.
//!  - **ARP**: [`ArpPacket`] for parsing and writing ARP packets.
//!  - **IPv4**: [`Ipv4Header`] for parsing IPv4 headers.
//!  - **Checksums**: [`checksum16`] and [`ipv4_checksum`] for computing
//!    Internet checksums (RFC 1071).
//!
//! ## Protocol Constants
//!
//! The module defines common protocol constants:
//!
//! - **Ethernet types**: [`ETHERTYPE_IPV4`], [`ETHERTYPE_ARP`], [`ETHERTYPE_IPV6`]
//!  - **IP protocols**: [`IP_PROTO_ICMP`], [`IP_PROTO_TCP`], [`IP_PROTO_UDP`]
//!
//! ## Usage
//!
//! These utilities are used by the network stack to parse incoming
//! packets and construct outgoing packets.

/// A 6-byte MAC address.
pub type MacAddr = [u8; 6];

/// The broadcast MAC address (`ff:ff:ff:ff:ff:ff`).
pub const BROADCAST_MAC: MacAddr = [0xff; 6];

/// Ethertype: IPv4.
pub const ETHERTYPE_IPV4: u16 = 0x0800;
/// Ethertype: ARP.
pub const ETHERTYPE_ARP: u16 = 0x0806;
/// Ethertype: IPv6.
pub const ETHERTYPE_IPV6: u16 = 0x86DD;

/// IP protocol: ICMP.
pub const IP_PROTO_ICMP: u8 = 1;
/// IP protocol: TCP.
pub const IP_PROTO_TCP: u8 = 6;
/// IP protocol: UDP.
pub const IP_PROTO_UDP: u8 = 17;

/// An Ethernet header.
///
/// # Fields
///
/// * `dst` - Destination MAC address.
/// * `src` - Source MAC address.
/// * `ethertype` - EtherType (e.g., [`ETHERTYPE_IPV4`]).
#[derive(Debug, Clone, Copy)]
pub struct EthHeader {
    /// Destination MAC address.
    pub dst: MacAddr,
    /// Source MAC address.
    pub src: MacAddr,
    /// EtherType (e.g., [`ETHERTYPE_IPV4`]).
    pub ethertype: u16,
}

impl EthHeader {
    /// Length of an Ethernet header in bytes (14).
    pub const LEN: usize = 14;

    /// Parses an Ethernet header from a byte buffer.
    ///
    /// # Arguments
    ///
    /// * `buf` - The byte buffer to parse.
    ///
    /// # Returns
    ///
    /// `Some((header, payload))` on success, where `payload` is the
    /// remaining bytes after the header. Returns `None` if the buffer
    /// is too short.
    pub fn parse(buf: &[u8]) -> Option<(Self, &[u8])> {
        if buf.len() < Self::LEN {
            return None;
        }
        let mut dst = [0u8; 6];
        let mut src = [0u8; 6];
        dst.copy_from_slice(&buf[0..6]);
        src.copy_from_slice(&buf[6..12]);
        let ethertype = u16::from_be_bytes([buf[12], buf[13]]);
        Some((
            EthHeader {
                dst,
                src,
                ethertype,
            },
            &buf[Self::LEN..],
        ))
    }

    /// Writes the header into a byte buffer.
    ///
    /// # Arguments
    ///
    /// * `dst_buf` - The destination buffer.
    ///
    /// # Returns
    ///
    /// `Some(bytes_written)` on success, or `None` if the buffer is
    /// too small.
    pub fn write_into(&self, dst_buf: &mut [u8]) -> Option<usize> {
        if dst_buf.len() < Self::LEN {
            return None;
        }
        dst_buf[0..6].copy_from_slice(&self.dst);
        dst_buf[6..12].copy_from_slice(&self.src);
        dst_buf[12..14].copy_from_slice(&self.ethertype.to_be_bytes());
        Some(Self::LEN)
    }
}

/// An ARP packet (fixed-size for IPv4 over Ethernet).
///
/// # Fields
///
/// * `htype` - Hardware type (1 for Ethernet).
/// * `ptype` - Protocol type (0x0800 for IPv4).
/// * `hlen` - Hardware address length (6 for Ethernet).
/// * `plen` - Protocol address length (4 for IPv4).
/// * `oper` - Operation (1 for request, 2 for reply).
/// * `sha` - Sender hardware address.
/// * `spa` - Sender protocol address.
/// * `tha` - Target hardware address.
/// * `tpa` - Target protocol address.
#[derive(Debug, Clone, Copy)]
pub struct ArpPacket {
    /// Hardware type (1 for Ethernet).
    pub htype: u16,
    /// Protocol type (0x0800 for IPv4).
    pub ptype: u16,
    /// Hardware address length (6 for Ethernet).
    pub hlen: u8,
    /// Protocol address length (4 for IPv4).
    pub plen: u8,
    /// Operation (1 for request, 2 for reply).
    pub oper: u16,
    /// Sender hardware address.
    pub sha: MacAddr,
    /// Sender protocol address.
    pub spa: [u8; 4],
    /// Target hardware address.
    pub tha: MacAddr,
    /// Target protocol address.
    pub tpa: [u8; 4],
}

impl ArpPacket {
    /// Length of an ARP packet in bytes (28, fixed for IPv4+Ethernet).
    pub const LEN: usize = 28;

    /// Parses an ARP packet from a byte buffer.
    ///
    /// # Arguments
    ///
    /// * `buf` - The byte buffer to parse.
    ///
    /// # Returns
    ///
    /// `Some(packet)` on success, or `None` if the buffer is too short.
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::LEN {
            return None;
        }
        let htype = u16::from_be_bytes([buf[0], buf[1]]);
        let ptype = u16::from_be_bytes([buf[2], buf[3]]);
        let hlen = buf[4];
        let plen = buf[5];
        let oper = u16::from_be_bytes([buf[6], buf[7]]);
        let mut sha = [0u8; 6];
        sha.copy_from_slice(&buf[8..14]);
        let mut spa = [0u8; 4];
        spa.copy_from_slice(&buf[14..18]);
        let mut tha = [0u8; 6];
        tha.copy_from_slice(&buf[18..24]);
        let mut tpa = [0u8; 4];
        tpa.copy_from_slice(&buf[24..28]);
        Some(ArpPacket {
            htype,
            ptype,
            hlen,
            plen,
            oper,
            sha,
            spa,
            tha,
            tpa,
        })
    }

    /// Writes the packet into a byte buffer.
    ///
    /// # Arguments
    ///
    /// * `out` - The destination buffer.
    ///
    /// # Returns
    ///
    /// `Some(bytes_written)` on success, or `None` if the buffer is
    /// too small.
    pub fn write_into(&self, out: &mut [u8]) -> Option<usize> {
        if out.len() < Self::LEN {
            return None;
        }
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

/// An IPv4 header.
///
/// # Fields
///
/// * `ihl` - Internet Header Length (in 32-bit words).
/// * `tos` - Type of Service.
/// * `total_len` - Total length of the IP packet.
/// * `id` - Identification.
/// * `flags_fragment` - Flags and fragment offset.
/// * `ttl` - Time to live.
/// * `protocol` - Protocol (e.g., [`IP_PROTO_TCP`]).
/// * `checksum` - Header checksum.
/// * `src` - Source IP address.
/// * `dst` - Destination IP address.
#[derive(Debug, Clone, Copy)]
pub struct Ipv4Header {
    /// Internet Header Length (in 32-bit words).
    pub ihl: u8,
    /// Type of Service.
    pub tos: u8,
    /// Total length of the IP packet.
    pub total_len: u16,
    /// Identification.
    pub id: u16,
    /// Flags and fragment offset.
    pub flags_fragment: u16,
    /// Time to live.
    pub ttl: u8,
    /// Protocol (e.g., [`IP_PROTO_TCP`]).
    pub protocol: u8,
    /// Header checksum.
    pub checksum: u16,
    /// Source IP address.
    pub src: [u8; 4],
    /// Destination IP address.
    pub dst: [u8; 4],
}

impl Ipv4Header {
    /// Minimum length of an IPv4 header in bytes (20, without options).
    pub const MIN_LEN: usize = 20;

    /// Parses an IPv4 header from a byte buffer.
    ///
    /// # Arguments
    ///
    /// * `buf` - The byte buffer to parse.
    ///
    /// # Returns
    ///
    /// `Some((header, payload))` on success, where `payload` is the
    /// remaining bytes after the header. Returns `None` if the buffer
    /// is too short or the header is malformed.
    pub fn parse(buf: &[u8]) -> Option<(Self, &[u8])> {
        if buf.len() < Self::MIN_LEN {
            return None;
        }
        let vihl = buf[0];
        let ihl = vihl & 0x0f;
        let header_len = (ihl as usize) * 4;
        if buf.len() < header_len {
            return None;
        }
        let tos = buf[1];
        let total_len = u16::from_be_bytes([buf[2], buf[3]]);
        let id = u16::from_be_bytes([buf[4], buf[5]]);
        let flags_fragment = u16::from_be_bytes([buf[6], buf[7]]);
        let ttl = buf[8];
        let protocol = buf[9];
        let checksum = u16::from_be_bytes([buf[10], buf[11]]);
        let mut src = [0u8; 4];
        src.copy_from_slice(&buf[12..16]);
        let mut dst = [0u8; 4];
        dst.copy_from_slice(&buf[16..20]);
        let payload = if (total_len as usize) >= header_len && (total_len as usize) <= buf.len() {
            &buf[header_len..(total_len as usize)]
        } else {
            &buf[header_len..]
        };
        Some((
            Ipv4Header {
                ihl,
                tos,
                total_len,
                id,
                flags_fragment,
                ttl,
                protocol,
                checksum,
                src,
                dst,
            },
            payload,
        ))
    }
}

/// Computes the 16-bit ones-complement Internet checksum over data (RFC 1071).
///
/// # Arguments
///
/// * `data` - The data to checksum.
///
/// # Returns
///
/// The 16-bit checksum.
pub fn checksum16(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0usize;
    while i + 1 < data.len() {
        let word = ((data[i] as u16) << 8) | (data[i + 1] as u16);
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

/// Computes the IPv4 header checksum.
///
/// This is a convenience function that calls [`checksum16`].
///
/// # Arguments
///
/// * `header_bytes` - The IPv4 header bytes.
///
/// # Returns
///
/// The 16-bit checksum.
pub fn ipv4_checksum(header_bytes: &[u8]) -> u16 {
    checksum16(header_bytes)
}

/// Helper to build an Ethernet frame header into a provided buffer.
///
/// # Arguments
///
/// * `dst` - Destination MAC address.
/// * `src` - Source MAC address.
/// * `ethertype` - EtherType.
/// * `out` - The destination buffer.
///
/// # Returns
///
/// `Some(bytes_written)` on success, or `None` if the buffer is too small.
pub fn build_eth_header(
    dst: MacAddr,
    src: MacAddr,
    ethertype: u16,
    out: &mut [u8],
) -> Option<usize> {
    if out.len() < EthHeader::LEN {
        return None;
    }
    out[0..6].copy_from_slice(&dst);
    out[6..12].copy_from_slice(&src);
    out[12..14].copy_from_slice(&ethertype.to_be_bytes());
    Some(EthHeader::LEN)
}

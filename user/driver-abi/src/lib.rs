//! Driver ABI — stable descriptor a driver publishes to rs-server at startup.
//!
//! A driver sends `OP_DRIVER_REGISTER` to rs-server on startup carrying a
//! serialized `DriverDesc`. The registry validates the requested resources
//! and capability bits before granting them.
#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Op-code sent from a driver to rs-server to register itself.
pub const OP_DRIVER_REGISTER: i32 = 0x4400;
/// Reply: registration succeeded.
pub const REPLY_DRIVER_OK: i32 = 0x4401;
/// Reply: registration failed (missing caps, unknown resource, etc.).
pub const REPLY_DRIVER_ERR: i32 = 0x4402;

/// Kind of hardware resource a driver needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceKind {
    /// Memory-mapped I/O region: (base_pa, size_bytes).
    Mmio { base: usize, size: usize },
    /// Hardware IRQ line.
    Irq(u32),
    /// DMA buffer (kernel allocates and maps).
    Dma { size: usize },
}

/// A single resource request inside a `DriverDesc`.
#[derive(Debug, Clone)]
pub struct ResourceReq {
    pub kind: ResourceKind,
    /// Human-readable name for diagnostics.
    pub name: String,
}

/// The descriptor a driver submits to rs-server when it starts up.
#[derive(Debug, Clone)]
pub struct DriverDesc {
    /// Short stable identifier, e.g. `"virtio-blk"`.
    pub name: String,
    /// Semver-style version: (major, minor, patch).
    pub version: (u16, u16, u16),
    /// Capability bits the driver needs granted (subset of CAP_* constants).
    pub required_caps: u64,
    /// Resources this driver needs mapped / assigned.
    pub resources: Vec<ResourceReq>,
}

impl DriverDesc {
    /// Serialize the descriptor to a flat byte buffer for IPC transfer.
    ///
    /// Wire format:
    /// ```text
    /// [4] magic = 0x44524956 ("DRIV")
    /// [2] name_len,  [name_len] name bytes
    /// [6] version (u16 × 3)
    /// [8] required_caps
    /// [4] n_resources
    /// for each resource:
    ///   [4] kind_tag (0=mmio, 1=irq, 2=dma)
    ///   [8] param0   (mmio base | irq num | dma size)
    ///   [8] param1   (mmio size | 0       | 0)
    ///   [2] name_len, [name_len] name bytes
    /// ```
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        // Magic
        buf.extend_from_slice(&0x44524956u32.to_le_bytes());
        // Name
        let nb = self.name.as_bytes();
        buf.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        buf.extend_from_slice(nb);
        // Version
        buf.extend_from_slice(&self.version.0.to_le_bytes());
        buf.extend_from_slice(&self.version.1.to_le_bytes());
        buf.extend_from_slice(&self.version.2.to_le_bytes());
        // Caps
        buf.extend_from_slice(&self.required_caps.to_le_bytes());
        // Resources
        buf.extend_from_slice(&(self.resources.len() as u32).to_le_bytes());
        for r in &self.resources {
            let (tag, p0, p1) = match r.kind {
                ResourceKind::Mmio { base, size } => (0u32, base as u64, size as u64),
                ResourceKind::Irq(n)             => (1u32, n as u64, 0u64),
                ResourceKind::Dma { size }       => (2u32, size as u64, 0u64),
            };
            buf.extend_from_slice(&tag.to_le_bytes());
            buf.extend_from_slice(&p0.to_le_bytes());
            buf.extend_from_slice(&p1.to_le_bytes());
            let rn = r.name.as_bytes();
            buf.extend_from_slice(&(rn.len() as u16).to_le_bytes());
            buf.extend_from_slice(rn);
        }
        buf
    }

    /// Deserialize from a byte buffer produced by `serialize()`.
    /// Returns `None` if the buffer is malformed or the magic is wrong.
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        let mut off = 0;
        let magic = u32::from_le_bytes(data.get(off..off+4)?.try_into().ok()?);
        off += 4;
        if magic != 0x44524956 { return None; }
        let name_len = u16::from_le_bytes(data.get(off..off+2)?.try_into().ok()?) as usize;
        off += 2;
        let name = String::from_utf8(data.get(off..off+name_len)?.to_vec()).ok()?;
        off += name_len;
        let v0 = u16::from_le_bytes(data.get(off..off+2)?.try_into().ok()?); off += 2;
        let v1 = u16::from_le_bytes(data.get(off..off+2)?.try_into().ok()?); off += 2;
        let v2 = u16::from_le_bytes(data.get(off..off+2)?.try_into().ok()?); off += 2;
        let required_caps = u64::from_le_bytes(data.get(off..off+8)?.try_into().ok()?); off += 8;
        let n_res = u32::from_le_bytes(data.get(off..off+4)?.try_into().ok()?) as usize; off += 4;
        let mut resources = Vec::new();
        for _ in 0..n_res {
            let tag  = u32::from_le_bytes(data.get(off..off+4)?.try_into().ok()?); off += 4;
            let p0   = u64::from_le_bytes(data.get(off..off+8)?.try_into().ok()?); off += 8;
            let p1   = u64::from_le_bytes(data.get(off..off+8)?.try_into().ok()?); off += 8;
            let kind = match tag {
                0 => ResourceKind::Mmio { base: p0 as usize, size: p1 as usize },
                1 => ResourceKind::Irq(p0 as u32),
                2 => ResourceKind::Dma { size: p0 as usize },
                _ => return None,
            };
            let rn_len = u16::from_le_bytes(data.get(off..off+2)?.try_into().ok()?) as usize; off += 2;
            let rname = String::from_utf8(data.get(off..off+rn_len)?.to_vec()).ok()?; off += rn_len;
            resources.push(ResourceReq { kind, name: rname });
        }
        Some(DriverDesc { name, version: (v0, v1, v2), required_caps, resources })
    }
}

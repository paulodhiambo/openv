// Simple packet buffer abstraction used by kernel and driver
pub const MAX_PKT_SIZE: usize = 1536;

pub struct PktBuf {
    pub len: usize,
    pub data: [u8; MAX_PKT_SIZE],
}

impl PktBuf {
    pub const fn empty() -> Self {
        Self {
            len: 0,
            data: [0u8; MAX_PKT_SIZE],
        }
    }

    pub fn from_slice(slice: &[u8]) -> Self {
        let mut b = Self::empty();
        let copy_len = core::cmp::min(slice.len(), MAX_PKT_SIZE);
        b.data[..copy_len].copy_from_slice(&slice[..copy_len]);
        b.len = copy_len;
        b
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data[..self.len]
    }
}

use libos::smoltcp_device;

pub struct SmolDevice {
    // Internal temporary buffer for RX
    rx_buf: [u8; 1536],
}

impl SmolDevice {
    pub fn new() -> Self {
        SmolDevice {
            rx_buf: [0u8; 1536],
        }
    }

    /// Try to receive a frame. Returns Some(len) on success, None if no frame available.
    pub fn recv(&mut self, out: &mut [u8]) -> Option<usize> {
        let got = smoltcp_device::recv_frame_rs(&mut self.rx_buf) as isize;
        if got <= 0 {
            None
        } else {
            let got_usize = got as usize;
            let n = core::cmp::min(out.len(), got_usize);
            out[..n].copy_from_slice(&self.rx_buf[..n]);
            Some(n)
        }
    }

    /// Send a frame. Returns true on success.
    pub fn send(&mut self, pkt: &[u8]) -> bool {
        let sent = smoltcp_device::send_frame_rs(pkt) as isize;
        sent >= 0
    }
}

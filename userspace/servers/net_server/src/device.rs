/// smoltcp Device implementation backed by the e1000 userspace driver over IPC.
///
/// The e1000 IPC wire format:
///   TX request → e1000: [0x01][len: u16 LE][frame: len bytes]
///   RX notify  ← e1000: [len: u16 LE][frame: len bytes]
///   Subscribe  → e1000: [0x02][send_ep: u64 LE]

use smoltcp::phy::{Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use ulib::{sys_try_channel_recv, sys_try_channel_send};
use kernel_api_types::IPC_OK;

pub const MSG_TX_PACKET: u8 = 1;
pub const MSG_SUBSCRIBE: u8 = 2;

/// Maximum Ethernet frame size (header + payload, no FCS).
const MTU: usize = 1514;

// ── Device ────────────────────────────────────────────────────────────────────

pub struct E1000Client {
    /// Send endpoint of the e1000 service (for TX + subscribe).
    e1000_ep: u64,
    /// Our receive endpoint; e1000 pushes RX notifications here.
    rx_recv_ep: u64,
}

impl E1000Client {
    pub fn new(e1000_ep: u64, rx_recv_ep: u64) -> Self {
        Self { e1000_ep, rx_recv_ep }
    }

    fn send_frame(&mut self, data: &[u8]) {
        if data.is_empty() || data.len() > MTU {
            return;
        }
        let mut msg = [0u8; 3 + MTU];
        msg[0] = MSG_TX_PACKET;
        msg[1..3].copy_from_slice(&(data.len() as u16).to_le_bytes());
        msg[3..3 + data.len()].copy_from_slice(data);
        sys_try_channel_send(self.e1000_ep, &msg[..3 + data.len()]);
    }
}

impl Device for E1000Client {
    type RxToken<'a> = OwnedRxToken where Self: 'a;
    type TxToken<'a> = E1000TxToken<'a> where Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // [len: u16 LE][frame: up to MTU bytes]
        let mut msg = [0u8; 2 + MTU];
        let (ret, n) = sys_try_channel_recv(self.rx_recv_ep, &mut msg);
        if ret != IPC_OK || n < 2 {
            return None;
        }
        let len = u16::from_le_bytes([msg[0], msg[1]]) as usize;
        if len == 0 || len > MTU || 2 + len > n as usize {
            return None;
        }
        // Copy into an owned buffer so TxToken can also borrow &mut self.
        let mut buf = [0u8; MTU];
        buf[..len].copy_from_slice(&msg[2..2 + len]);
        Some((OwnedRxToken { buf, len }, E1000TxToken { device: self }))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(E1000TxToken { device: self })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps
    }
}

// ── Tokens ────────────────────────────────────────────────────────────────────

/// Owns a received Ethernet frame so it doesn't borrow from E1000Client,
/// allowing TxToken to simultaneously hold a &mut E1000Client.
pub struct OwnedRxToken {
    buf: [u8; MTU],
    len: usize,
}

impl smoltcp::phy::RxToken for OwnedRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buf[..self.len])
    }
}

pub struct E1000TxToken<'a> {
    device: &'a mut E1000Client,
}

impl<'a> smoltcp::phy::TxToken for E1000TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = [0u8; MTU];
        let len = len.min(MTU);
        let result = f(&mut buf[..len]);
        self.device.send_frame(&buf[..len]);
        result
    }
}

/// Net-server IPC wire protocol.
///
/// All multi-byte integers are little-endian.
///
/// Client → net_server message layout:
///
///   MSG_CONNECT (1):  [type:u8][reply_ep:u64][ip:4][port:u16]     = 15 B
///   MSG_SEND    (2):  [type:u8][sock_id:u32][data:…]              = 5+N B (N ≤ NET_MAX_SEND)
///   MSG_RECV    (3):  [type:u8][sock_id:u32][notify_ep:u64]        = 13 B
///   MSG_CLOSE   (4):  [type:u8][sock_id:u32]                       = 5 B
///   MSG_RESOLVE (5):  [type:u8][reply_ep:u64][hostname:…]          = 9+N B
///
/// net_server → client reply layout (sent via reply_ep / notify_ep):
///
///   connect reply:  [sock_id:u32][err:u32]   = 8 B
///   resolve reply:  [err:u32][ip:4]          = 8 B
///   recv data push: raw bytes, ≤ 4096 B each (zero-length = EOF)
// Message type bytes
pub const NET_MSG_CONNECT: u8 = 1;
pub const NET_MSG_SEND:    u8 = 2;
pub const NET_MSG_RECV:    u8 = 3;
pub const NET_MSG_CLOSE:   u8 = 4;
pub const NET_MSG_RESOLVE: u8 = 5;

// Error codes (u32, little-endian in wire format)
pub const NET_OK:          u32 = 0;
pub const NET_ERR_REFUSED: u32 = 1; // TCP RST / connect rejected
pub const NET_ERR_TIMEOUT: u32 = 2; // DNS timeout / connect timeout
pub const NET_ERR_INVALID: u32 = 3; // bad arguments (e.g. invalid hostname UTF-8)
pub const NET_ERR_FULL:    u32 = 4; // no free socket / DNS slots

/// Maximum data bytes per MSG_SEND message (4096 - 5 byte header).
pub const NET_MAX_SEND: usize = 4091;

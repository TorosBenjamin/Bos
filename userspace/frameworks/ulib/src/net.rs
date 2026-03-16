/// Client-side wrappers for the net_server TCP/DNS socket API.
///
/// Typical usage:
/// ```
/// let net = net_lookup();                              // find net_server
/// let sock = net_connect(net, [93,184,216,34], 80)?;  // TCP connect
/// let rx   = net_recv_subscribe(net, sock);            // register RX channel
/// net_send(net, sock, b"GET / HTTP/1.0\r\n\r\n");
/// let mut buf = [0u8; 4096];
/// let (_ret, n) = ulib::sys_channel_recv(rx, &mut buf); // blocking read
/// net_close(net, sock);
/// ```

use kernel_api_types::net::*;
use kernel_api_types::SVC_ERR_NOT_FOUND;

/// Spin-loop until the `"net"` service is registered. Returns the send endpoint.
pub fn net_lookup() -> u64 {
    loop {
        let ep = crate::sys_lookup_service(b"net");
        if ep != SVC_ERR_NOT_FOUND {
            return ep;
        }
        crate::sys_sleep_ms(10);
    }
}

/// Blocking TCP connect.
///
/// Sends `MSG_CONNECT` to `net_ep` and blocks until the connection is
/// established (or fails). Returns `Some(socket_id)` on success, `None` on
/// error (connection refused, no free slots, etc.).
pub fn net_connect(net_ep: u64, ip: [u8; 4], port: u16) -> Option<u32> {
    let (reply_send, reply_recv) = crate::sys_channel_create(1);

    let mut msg = [0u8; 15];
    msg[0] = NET_MSG_CONNECT;
    msg[1..9].copy_from_slice(&reply_send.to_le_bytes());
    msg[9..13].copy_from_slice(&ip);
    msg[13..15].copy_from_slice(&port.to_le_bytes());
    crate::sys_channel_send(net_ep, &msg);

    let mut reply = [0u8; 8];
    crate::sys_channel_recv(reply_recv, &mut reply);
    crate::sys_channel_close(reply_recv);
    crate::sys_channel_close(reply_send);

    let err = u32::from_le_bytes(reply[4..8].try_into().unwrap());
    if err != NET_OK {
        return None;
    }
    Some(u32::from_le_bytes(reply[0..4].try_into().unwrap()))
}

/// Send data on a TCP socket (fire-and-forget, broken into ≤ NET_MAX_SEND chunks).
///
/// Blocks if net_server's IPC channel is full (backpressure).
pub fn net_send(net_ep: u64, socket_id: u32, data: &[u8]) {
    let mut buf = [0u8; 5 + NET_MAX_SEND];
    buf[0] = NET_MSG_SEND;
    buf[1..5].copy_from_slice(&socket_id.to_le_bytes());

    let mut offset = 0;
    while offset < data.len() {
        let n = (data.len() - offset).min(NET_MAX_SEND);
        buf[5..5 + n].copy_from_slice(&data[offset..offset + n]);
        crate::sys_channel_send(net_ep, &buf[..5 + n]);
        offset += n;
    }
}

/// Register a receive-notification channel for a TCP socket.
///
/// Sends `MSG_RECV` to net_server. Returns the **receive** endpoint of a new
/// channel; net_server holds the send endpoint and pushes data to it whenever
/// TCP data arrives. A zero-length push signals EOF (connection closed).
///
/// The caller should pass this endpoint to `sys_wait_for_event` to sleep
/// efficiently until data arrives, then drain it with `sys_try_channel_recv`.
pub fn net_recv_subscribe(net_ep: u64, socket_id: u32) -> u64 {
    let (notify_send, notify_recv) = crate::sys_channel_create(16);

    let mut msg = [0u8; 13];
    msg[0] = NET_MSG_RECV;
    msg[1..5].copy_from_slice(&socket_id.to_le_bytes());
    msg[5..13].copy_from_slice(&notify_send.to_le_bytes());
    crate::sys_channel_send(net_ep, &msg);

    notify_recv
}

/// Close a TCP socket. Fire-and-forget.
pub fn net_close(net_ep: u64, socket_id: u32) {
    let mut msg = [0u8; 5];
    msg[0] = NET_MSG_CLOSE;
    msg[1..5].copy_from_slice(&socket_id.to_le_bytes());
    crate::sys_try_channel_send(net_ep, &msg);
}

/// Blocking DNS A-record resolve.
///
/// Returns `Some([a, b, c, d])` on success, `None` on timeout or error.
/// `hostname` should be ASCII bytes without a trailing dot, e.g. `b"example.com"`.
pub fn net_resolve(net_ep: u64, hostname: &[u8]) -> Option<[u8; 4]> {
    let n = hostname.len().min(253);
    let (reply_send, reply_recv) = crate::sys_channel_create(1);

    let mut msg = [0u8; 9 + 253];
    msg[0] = NET_MSG_RESOLVE;
    msg[1..9].copy_from_slice(&reply_send.to_le_bytes());
    msg[9..9 + n].copy_from_slice(&hostname[..n]);
    crate::sys_channel_send(net_ep, &msg[..9 + n]);

    let mut reply = [0u8; 8];
    crate::sys_channel_recv(reply_recv, &mut reply);
    crate::sys_channel_close(reply_recv);
    crate::sys_channel_close(reply_send);

    let err = u32::from_le_bytes(reply[0..4].try_into().unwrap());
    if err != NET_OK {
        return None;
    }
    Some([reply[4], reply[5], reply[6], reply[7]])
}

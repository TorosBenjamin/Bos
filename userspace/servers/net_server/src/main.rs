#![no_std]
#![no_main]

extern crate alloc;

mod device;

use alloc::vec;
use alloc::vec::Vec;
use device::{E1000Client, MSG_GET_MAC, MSG_SUBSCRIBE};
use kernel_api_types::net::*;
use kernel_api_types::{IPC_OK, IPC_ERR_PEER_CLOSED, SVC_ERR_NOT_FOUND};
use linked_list_allocator::LockedHeap;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::socket::{dns, icmp, tcp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    DnsQueryType, EthernetAddress, HardwareAddress, IpAddress, IpCidr, Icmpv4Packet, Icmpv4Repr,
    Ipv4Address,
};
use ulib::{
    sys_channel_close, sys_channel_create, sys_channel_recv, sys_get_time_ns, sys_lookup_service,
    sys_register_service, sys_sleep_ms, sys_try_channel_recv, sys_try_channel_send,
    sys_wait_for_event,
};

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

const HEAP_SIZE: usize = 1024 * 1024; // 1 MiB (TCP buffers need more headroom)

static mut HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

fn now() -> Instant {
    Instant::from_millis((sys_get_time_ns() / 1_000_000) as i64)
}

// ── Test-ping constants ────────────────────────────────────────────────────────
const PING_IDENT: u16 = 0x0B05;
const PING_SEQ:   u16 = 1;

const TAG_PING_SENT:    u64 = 0x00BE_EF01;
const TAG_PING_REPLY:   u64 = 0x00BE_EF02;
const TAG_PING_TIMEOUT: u64 = 0x00BE_EF03;

// ── TCP socket table ───────────────────────────────────────────────────────────
const MAX_TCP: usize = 16;
const MAX_DNS: usize = 4;

struct TcpEntry {
    handle:        smoltcp::iface::SocketHandle,
    reply_ep:      Option<u64>, // pending connect; cleared once ESTABLISHED or CLOSED
    notify_ep:     Option<u64>, // RX data pushed here; client registers via MSG_RECV
    local_port:    u16,
    created_at_ms: i64,        // for connect timeout
}

struct DnsEntry {
    qhandle:  dns::QueryHandle, // Copy
    reply_ep: u64,
}

fn alloc_local_port(next: &mut u16) -> u16 {
    let p = *next;
    *next = if *next >= 65535 { 49152 } else { *next + 1 };
    p
}

// ── Entry point ────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    unsafe {
        let heap_start = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        ALLOCATOR.lock().init(heap_start, HEAP_SIZE);
    }

    // Wait for the e1000 driver to register itself.
    let e1000_ep = loop {
        let ep = sys_lookup_service(b"e1000");
        if ep != SVC_ERR_NOT_FOUND {
            break ep;
        }
        sys_sleep_ms(10);
    };

    // Create a channel so e1000 can push received frames to us.
    let (rx_notify_send_ep, rx_notify_recv_ep) = sys_channel_create(64);

    // Subscribe to e1000 for RX notifications.
    let mut sub = [0u8; 9];
    sub[0] = MSG_SUBSCRIBE;
    sub[1..9].copy_from_slice(&rx_notify_send_ep.to_le_bytes());
    sys_try_channel_send(e1000_ep, &sub);

    let mut device = E1000Client::new(e1000_ep, rx_notify_recv_ep);

    // Query the real MAC from the e1000 driver.
    let mac = {
        let (reply_send_ep, reply_recv_ep) = sys_channel_create(1);
        let mut req = [0u8; 9];
        req[0] = MSG_GET_MAC;
        req[1..9].copy_from_slice(&reply_send_ep.to_le_bytes());
        sys_try_channel_send(e1000_ep, &req);
        let mut buf = [0u8; 6];
        sys_channel_recv(reply_recv_ep, &mut buf);
        sys_channel_close(reply_recv_ep);
        sys_channel_close(reply_send_ep);
        EthernetAddress(buf)
    };

    let mut config = Config::new(HardwareAddress::Ethernet(mac));
    config.random_seed = sys_get_time_ns();

    let mut iface = Interface::new(config, &mut device, now());

    // QEMU user-mode networking: guest is 10.0.2.15/24, gateway 10.0.2.2.
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24));
    });
    iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2))
        .unwrap();

    let mut sockets: SocketSet<'static> = SocketSet::new(Vec::new());

    // ── Test-ping ICMP socket ──────────────────────────────────────────────────
    let icmp_handle = {
        let rx_buf = icmp::PacketBuffer::new(
            vec![icmp::PacketMetadata::EMPTY; 4],
            vec![0u8; 512],
        );
        let tx_buf = icmp::PacketBuffer::new(
            vec![icmp::PacketMetadata::EMPTY; 4],
            vec![0u8; 512],
        );
        let mut s = icmp::Socket::new(rx_buf, tx_buf);
        s.bind(icmp::Endpoint::Ident(PING_IDENT)).unwrap();
        sockets.add(s)
    };

    // ── DNS socket (one shared, queries tracked in dns_table) ─────────────────
    let dns_handle = {
        let s = dns::Socket::new(&[IpAddress::v4(10, 0, 2, 3)], Vec::new());
        sockets.add(s)
    };

    // ── Service registration ───────────────────────────────────────────────────
    let (net_send_ep, net_recv_ep) = sys_channel_create(64);
    sys_register_service(b"net", net_send_ep);

    // ── State ─────────────────────────────────────────────────────────────────
    let ping_target = IpAddress::v4(10, 0, 2, 2);
    let mut ping_sent    = false;
    let mut ping_sent_at = 0u64;
    let mut ping_done    = false;

    let mut tcp_table: [Option<TcpEntry>; MAX_TCP] = [const { None }; MAX_TCP];
    let mut dns_table: [Option<DnsEntry>; MAX_DNS] = [const { None }; MAX_DNS];
    let mut next_local_port: u16 = 49152;

    let mut req_buf = [0u8; 4096];

    // ── Main loop ─────────────────────────────────────────────────────────────
    loop {
        iface.poll(now(), &mut device, &mut sockets);

        // ── 1. Test-ping ──────────────────────────────────────────────────────
        if !ping_done {
            let socket = sockets.get_mut::<icmp::Socket>(icmp_handle);

            if !ping_sent && socket.can_send() {
                const PAYLOAD: &[u8] = b"bos-ping";
                let repr = Icmpv4Repr::EchoRequest {
                    ident:  PING_IDENT,
                    seq_no: PING_SEQ,
                    data:   PAYLOAD,
                };
                if let Ok(buf) = socket.send(repr.buffer_len(), ping_target) {
                    let mut pkt = Icmpv4Packet::new_unchecked(buf);
                    repr.emit(&mut pkt, &ChecksumCapabilities::default());
                    ping_sent    = true;
                    ping_sent_at = sys_get_time_ns();
                    ulib::sys_debug_log(PING_SEQ as u64, TAG_PING_SENT);
                }
            }

            if ping_sent {
                while socket.can_recv() {
                    if let Ok((data, _src)) = socket.recv() {
                        if let Ok(pkt) = Icmpv4Packet::new_checked(data) {
                            let caps = ChecksumCapabilities::default();
                            if let Ok(icmp_repr) = Icmpv4Repr::parse(&pkt, &caps) {
                                if let Icmpv4Repr::EchoReply { ident, seq_no, .. } = icmp_repr {
                                    if ident == PING_IDENT && seq_no == PING_SEQ {
                                        let rtt_ms =
                                            (sys_get_time_ns() - ping_sent_at) / 1_000_000;
                                        ulib::sys_debug_log(rtt_ms, TAG_PING_REPLY);
                                        ping_done = true;
                                    }
                                }
                            }
                        }
                    }
                }

                if !ping_done
                    && sys_get_time_ns().saturating_sub(ping_sent_at) > 5_000_000_000
                {
                    ulib::sys_debug_log(0, TAG_PING_TIMEOUT);
                    ping_done = true;
                }
            }
        }

        // ── 2. TCP connect completions ────────────────────────────────────────
        let now_ms = now().total_millis();
        for id in 0..MAX_TCP {
            let (handle, reply_ep, _notify_ep, created_at_ms) = match &tcp_table[id] {
                None => continue,
                Some(e) => (e.handle, e.reply_ep, e.notify_ep, e.created_at_ms),
            };

            let Some(rp) = reply_ep else { continue };

            // 10-second connect timeout: avoid blocking the client forever.
            let timed_out = now_ms - created_at_ms > 10_000;

            let state = sockets.get::<tcp::Socket>(handle).state();
            match state {
                tcp::State::Established => {
                    ulib::sys_debug_log(id as u64, 0xBEEF_2004);
                    let mut rep = [0u8; 8];
                    rep[0..4].copy_from_slice(&(id as u32).to_le_bytes());
                    // rep[4..8] = NET_OK (zero)
                    sys_try_channel_send(rp, &rep);
                    sys_channel_close(rp);
                    tcp_table[id].as_mut().unwrap().reply_ep = None;
                }
                tcp::State::Closed | tcp::State::CloseWait => {
                    ulib::sys_debug_log(id as u64, 0xBEEF_2005);
                    let mut rep = [0u8; 8];
                    rep[4..8].copy_from_slice(&NET_ERR_REFUSED.to_le_bytes());
                    sys_try_channel_send(rp, &rep);
                    sys_channel_close(rp);
                    sockets.remove(handle);
                    tcp_table[id] = None;
                }
                _ if timed_out => {
                    // SynSent / SynReceived too long — give up.
                    ulib::sys_debug_log(id as u64, 0xBEEF_2006);
                    sockets.get_mut::<tcp::Socket>(handle).abort();
                    let mut rep = [0u8; 8];
                    rep[4..8].copy_from_slice(&NET_ERR_TIMEOUT.to_le_bytes());
                    sys_try_channel_send(rp, &rep);
                    sys_channel_close(rp);
                    sockets.remove(handle);
                    tcp_table[id] = None;
                }
                _ => {} // SynSent / SynReceived — still connecting
            }
        }

        // ── 3. TCP RX push ────────────────────────────────────────────────────
        for id in 0..MAX_TCP {
            let (handle, notify_ep) = match &tcp_table[id] {
                None => continue,
                Some(e) if e.reply_ep.is_some() => continue, // not yet connected
                Some(e) => (e.handle, e.notify_ep),
            };

            let Some(nep) = notify_ep else { continue };

            let sock = sockets.get_mut::<tcp::Socket>(handle);
            let mut buf = [0u8; 4096];
            loop {
                match sock.recv_slice(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let ret = sys_try_channel_send(nep, &buf[..n]);
                        if ret == IPC_ERR_PEER_CLOSED {
                            tcp_table[id].as_mut().unwrap().notify_ep = None;
                            break;
                        }
                    }
                    Err(_) => {
                        // EOF — zero-length push signals connection closed
                        sys_try_channel_send(nep, &[]);
                        tcp_table[id].as_mut().unwrap().notify_ep = None;
                        break;
                    }
                }
            }
        }

        // ── 4. DNS completions ────────────────────────────────────────────────
        for slot in &mut dns_table {
            let Some(entry) = slot else { continue };
            let dns_sock = sockets.get_mut::<dns::Socket>(dns_handle);
            match dns_sock.get_query_result(entry.qhandle) {
                Ok(addrs) => {
                    let mut rep = [0u8; 8];
                    if let Some(IpAddress::Ipv4(v4)) = addrs.first().copied() {
                        rep[4..8].copy_from_slice(&v4.0);
                        // DEBUG: log resolved IP as u32 big-endian
                        ulib::sys_debug_log(u32::from_be_bytes(v4.0) as u64, 0xBEEF_1001);
                    } else {
                        // No IPv4 address: treat as failure so caller gets DnsError
                        ulib::sys_debug_log(addrs.len() as u64, 0xBEEF_1002);
                        rep[0..4].copy_from_slice(&NET_ERR_TIMEOUT.to_le_bytes());
                    }
                    sys_try_channel_send(entry.reply_ep, &rep);
                    sys_channel_close(entry.reply_ep);
                    *slot = None;
                }
                Err(dns::GetQueryResultError::Pending) => {}
                Err(_) => {
                    ulib::sys_debug_log(0, 0xBEEF_1003);
                    let mut rep = [0u8; 8];
                    rep[0..4].copy_from_slice(&NET_ERR_TIMEOUT.to_le_bytes());
                    sys_try_channel_send(entry.reply_ep, &rep);
                    sys_channel_close(entry.reply_ep);
                    *slot = None;
                }
            }
        }

        // ── 5. Drain client IPC requests ──────────────────────────────────────
        loop {
            let (ret, n) = sys_try_channel_recv(net_recv_ep, &mut req_buf);
            if ret != IPC_OK {
                break;
            }
            let n = n as usize;
            if n == 0 {
                continue;
            }

            match req_buf[0] {
                // MSG_CONNECT: [1][reply_ep:u64][ip:4][port:u16]
                NET_MSG_CONNECT if n >= 15 => {
                    let reply_ep = u64::from_le_bytes(req_buf[1..9].try_into().unwrap());
                    let ip = [req_buf[9], req_buf[10], req_buf[11], req_buf[12]];
                    let port = u16::from_le_bytes([req_buf[13], req_buf[14]]);

                    let slot = tcp_table.iter().position(|s| s.is_none());
                    let Some(id) = slot else {
                        let mut rep = [0u8; 8];
                        rep[4..8].copy_from_slice(&NET_ERR_FULL.to_le_bytes());
                        sys_try_channel_send(reply_ep, &rep);
                        sys_channel_close(reply_ep);
                        continue;
                    };

                    let local_port = alloc_local_port(&mut next_local_port);
                    let remote =
                        (IpAddress::v4(ip[0], ip[1], ip[2], ip[3]), port);

                    let rx_buf = tcp::SocketBuffer::new(vec![0u8; 4096]);
                    let tx_buf = tcp::SocketBuffer::new(vec![0u8; 4096]);
                    let mut socket = tcp::Socket::new(rx_buf, tx_buf);

                    let cx = iface.context();
                    // DEBUG: log connect attempt — high 32 bits = IP, low 16 = port
                    let ip_u32 = u32::from_be_bytes(ip);
                    ulib::sys_debug_log(((ip_u32 as u64) << 16) | port as u64, 0xBEEF_2001);
                    match socket.connect(cx, remote, local_port) {
                        Ok(()) => {
                            ulib::sys_debug_log(id as u64, 0xBEEF_2002);
                            let handle = sockets.add(socket);
                            tcp_table[id] = Some(TcpEntry {
                                handle,
                                reply_ep: Some(reply_ep),
                                notify_ep: None,
                                local_port,
                                created_at_ms: now().total_millis(),
                            });
                        }
                        Err(_) => {
                            ulib::sys_debug_log(ip_u32 as u64, 0xBEEF_2003);
                            let mut rep = [0u8; 8];
                            rep[4..8].copy_from_slice(&NET_ERR_INVALID.to_le_bytes());
                            sys_try_channel_send(reply_ep, &rep);
                            sys_channel_close(reply_ep);
                        }
                    }
                }

                // MSG_SEND: [2][sock_id:u32][data:...]
                NET_MSG_SEND if n >= 6 => {
                    let sock_id =
                        u32::from_le_bytes(req_buf[1..5].try_into().unwrap()) as usize;
                    if sock_id < MAX_TCP {
                        if let Some(entry) = &tcp_table[sock_id] {
                            let handle = entry.handle;
                            let _ = sockets
                                .get_mut::<tcp::Socket>(handle)
                                .send_slice(&req_buf[5..n]);
                        }
                    }
                }

                // MSG_RECV: [3][sock_id:u32][notify_ep:u64]
                NET_MSG_RECV if n >= 13 => {
                    let sock_id =
                        u32::from_le_bytes(req_buf[1..5].try_into().unwrap()) as usize;
                    let notify_ep =
                        u64::from_le_bytes(req_buf[5..13].try_into().unwrap());
                    if sock_id < MAX_TCP {
                        if let Some(entry) = tcp_table[sock_id].as_mut() {
                            entry.notify_ep = Some(notify_ep);
                        }
                    }
                }

                // MSG_CLOSE: [4][sock_id:u32]
                NET_MSG_CLOSE if n >= 5 => {
                    let sock_id =
                        u32::from_le_bytes(req_buf[1..5].try_into().unwrap()) as usize;
                    if sock_id < MAX_TCP {
                        if let Some(entry) = tcp_table[sock_id].take() {
                            sockets.get_mut::<tcp::Socket>(entry.handle).close();
                            sockets.remove(entry.handle);
                            if let Some(ep) = entry.reply_ep {
                                sys_channel_close(ep);
                            }
                            if let Some(ep) = entry.notify_ep {
                                sys_channel_close(ep);
                            }
                        }
                    }
                }

                // MSG_RESOLVE: [5][reply_ep:u64][hostname:...]
                NET_MSG_RESOLVE if n >= 10 => {
                    let reply_ep =
                        u64::from_le_bytes(req_buf[1..9].try_into().unwrap());
                    let hostname_bytes = &req_buf[9..n];

                    let hostname = match core::str::from_utf8(hostname_bytes) {
                        Ok(s) => s,
                        Err(_) => {
                            let mut rep = [0u8; 8];
                            rep[0..4].copy_from_slice(&NET_ERR_INVALID.to_le_bytes());
                            sys_try_channel_send(reply_ep, &rep);
                            sys_channel_close(reply_ep);
                            continue;
                        }
                    };

                    let cx = iface.context();
                    let dns_sock = sockets.get_mut::<dns::Socket>(dns_handle);
                    match dns_sock.start_query(cx, hostname, DnsQueryType::A) {
                        Ok(qhandle) => {
                            let free = dns_table.iter().position(|s| s.is_none());
                            match free {
                                Some(i) => {
                                    dns_table[i] = Some(DnsEntry { qhandle, reply_ep });
                                }
                                None => {
                                    dns_sock.cancel_query(qhandle);
                                    let mut rep = [0u8; 8];
                                    rep[0..4].copy_from_slice(&NET_ERR_FULL.to_le_bytes());
                                    sys_try_channel_send(reply_ep, &rep);
                                    sys_channel_close(reply_ep);
                                }
                            }
                        }
                        Err(_) => {
                            let mut rep = [0u8; 8];
                            rep[0..4].copy_from_slice(&NET_ERR_INVALID.to_le_bytes());
                            sys_try_channel_send(reply_ep, &rep);
                            sys_channel_close(reply_ep);
                        }
                    }
                }

                _ => {} // unknown or malformed — ignore
            }
        }

        // ── 6. Sleep until RX frame or client request arrives ─────────────────
        let timeout_ms = match iface.poll_delay(now(), &sockets) {
            Some(d) => (d.total_millis() as u64).clamp(1, 100),
            None    => 100,
        };
        sys_wait_for_event(&[rx_notify_recv_ep, net_recv_ep], 0, timeout_ms);
    }
}

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    ulib::sys_exit(1);
}

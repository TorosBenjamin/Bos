#![no_std]
#![no_main]

extern crate alloc;

mod config;
mod device;

use alloc::vec;
use alloc::vec::Vec;
use device::{E1000Client, MSG_GET_MAC, MSG_SUBSCRIBE};
use kernel_api_types::net::*;
use kernel_api_types::{IPC_OK, IPC_ERR_PEER_CLOSED, SVC_ERR_NOT_FOUND};
use linked_list_allocator::LockedHeap;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::socket::{dhcpv4, dns, icmp, tcp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    DnsQueryType, EthernetAddress, HardwareAddress, IpAddress, IpCidr, Icmpv4Packet, Icmpv4Repr,
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
    _local_port:   u16,
    created_at_ms: i64,        // for connect timeout
}

struct DnsEntry {
    qhandle:  dns::QueryHandle, // Copy
    reply_ep: u64,
}

fn alloc_local_port(next: &mut u16) -> u16 {
    let p = *next;
    *next = if *next == 65535 { 49152 } else { *next + 1 };
    p
}

// ── Config loading ────────────────────────────────────────────────────────────

fn load_config() -> config::NetConfig {
    // Wait up to 200 yields for the fatfs service.
    let fs_ep = {
        let mut ep = SVC_ERR_NOT_FOUND;
        for _ in 0..200u32 {
            ep = sys_lookup_service(b"fatfs");
            if ep != SVC_ERR_NOT_FOUND { break; }
            ulib::sys_yield();
        }
        ep
    };
    if fs_ep == SVC_ERR_NOT_FOUND {
        return config::NetConfig::default();
    }

    let (buf_id, file_size) = match ulib::fs::fs_map_file(fs_ep, "/net.conf") {
        Some(v) => v,
        None => return config::NetConfig::default(),
    };

    let ptr = ulib::sys_map_shared_buf(buf_id);
    if ptr.is_null() {
        ulib::sys_destroy_shared_buf(buf_id);
        return config::NetConfig::default();
    }

    let bytes = unsafe { core::slice::from_raw_parts(ptr, file_size as usize) };
    let cfg = config::NetConfig::parse(bytes);

    ulib::sys_munmap(ptr, file_size);
    ulib::sys_destroy_shared_buf(buf_id);

    cfg
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

    // Query the real MAC from the e1000 driver via blocking channel recv.
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

    // ── Load network config from disk ────────────────────────────────────────
    let net_cfg = load_config();
    let use_dhcp = net_cfg.mode == config::NetMode::Dhcp;

    let mut iface_config = Config::new(HardwareAddress::Ethernet(mac));
    iface_config.random_seed = sys_get_time_ns();

    let mut iface = Interface::new(iface_config, &mut device, now());

    let mut sockets: SocketSet<'static> = SocketSet::new(Vec::new());

    // ── DHCP socket (only when mode = dhcp) ─────────────────────────────────
    let dhcp_handle = if use_dhcp {
        Some(sockets.add(dhcpv4::Socket::new()))
    } else {
        // Static mode: apply address, gateway, DNS from config immediately.
        if let Some(cidr) = net_cfg.address {
            iface.update_ip_addrs(|addrs| { let _ = addrs.push(cidr); });
        }
        if let Some(gw) = net_cfg.gateway {
            let _ = iface.routes_mut().add_default_ipv4_route(gw);
        }
        None
    };

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
        let initial_dns: Vec<IpAddress> = if use_dhcp {
            Vec::new() // populated by DHCP Configured event
        } else {
            net_cfg.dns.iter().map(|a| IpAddress::Ipv4(*a)).collect()
        };
        let s = dns::Socket::new(&initial_dns, Vec::new());
        sockets.add(s)
    };

    // ── Service registration ───────────────────────────────────────────────────
    let (net_send_ep, net_recv_ep) = sys_channel_create(64);
    sys_register_service(b"net", net_send_ep);

    // ── State ─────────────────────────────────────────────────────────────────
    // In static mode the gateway is known upfront; in DHCP mode it's set later.
    let mut ping_target = if use_dhcp {
        None
    } else {
        net_cfg.gateway.map(IpAddress::Ipv4)
    };
    let mut ping_sent    = false;
    let mut ping_sent_at = 0u64;
    let mut ping_done    = !net_cfg.ping_enabled;

    let mut tcp_table: [Option<TcpEntry>; MAX_TCP] = [const { None }; MAX_TCP];
    let mut dns_table: [Option<DnsEntry>; MAX_DNS] = [const { None }; MAX_DNS];
    let mut next_local_port: u16 = 49152;

    let mut req_buf = [0u8; 4096];

    // ── Main loop ─────────────────────────────────────────────────────────────
    loop {
        iface.poll(now(), &mut device, &mut sockets);

        // ── 0. DHCP state (skipped in static mode) ─────────────────────────
        // Extract config data before re-borrowing sockets (borrow checker).
        enum DhcpAction {
            None,
            Configured { address: smoltcp::wire::Ipv4Cidr, router: Option<smoltcp::wire::Ipv4Address>, dns: Vec<IpAddress> },
            Deconfigured,
        }
        let dhcp_action = if let Some(dh) = dhcp_handle {
            match sockets.get_mut::<dhcpv4::Socket>(dh).poll() {
                core::prelude::rust_2024::None => DhcpAction::None,
                Some(dhcpv4::Event::Configured(config)) => {
                    let dns: Vec<IpAddress> = config.dns_servers.iter()
                        .map(|a| IpAddress::Ipv4(*a))
                        .collect();
                    DhcpAction::Configured { address: config.address, router: config.router, dns }
                }
                Some(dhcpv4::Event::Deconfigured) => DhcpAction::Deconfigured,
            }
        } else {
            DhcpAction::None
        };
        match dhcp_action {
            DhcpAction::None => {}
            DhcpAction::Configured { address, router, dns } => {
                ulib::sys_debug_log(u32::from_be_bytes(address.address().0) as u64, 0x00DC_0001);

                iface.update_ip_addrs(|addrs| {
                    addrs.clear();
                    addrs.push(IpCidr::Ipv4(address)).unwrap();
                });

                if let Some(gw) = router {
                    iface.routes_mut().add_default_ipv4_route(gw).unwrap();
                } else {
                    iface.routes_mut().remove_default_ipv4_route();
                }

                // DEBUG: log DNS server count and first server IP
                ulib::sys_debug_log(dns.len() as u64, 0x00DC_0003);
                if let Some(IpAddress::Ipv4(a)) = dns.first() {
                    ulib::sys_debug_log(u32::from_be_bytes(a.0) as u64, 0x00DC_0004);
                }

                sockets.get_mut::<dns::Socket>(dns_handle).update_servers(&dns);

                // Set ping target to gateway once DHCP is configured.
                ping_target = router.map(IpAddress::Ipv4);
            }
            DhcpAction::Deconfigured => {
                ulib::sys_debug_log(0, 0x00DC_0002);
                iface.update_ip_addrs(|addrs| addrs.clear());
                iface.routes_mut().remove_default_ipv4_route();
                ping_target = None;
            }
        }
        if !ping_done {
            let socket = sockets.get_mut::<icmp::Socket>(icmp_handle);

            #[allow(clippy::collapsible_if)]
            if let Some(target) = ping_target {
                if !ping_sent && socket.can_send() {
                    const PAYLOAD: &[u8] = b"bos-ping";
                    let repr = Icmpv4Repr::EchoRequest {
                        ident:  PING_IDENT,
                        seq_no: PING_SEQ,
                        data:   PAYLOAD,
                    };
                    if let Ok(buf) = socket.send(repr.buffer_len(), target) {
                        let mut pkt = Icmpv4Packet::new_unchecked(buf);
                        repr.emit(&mut pkt, &ChecksumCapabilities::default());
                        ping_sent    = true;
                        ping_sent_at = sys_get_time_ns();
                        ulib::sys_debug_log(PING_SEQ as u64, TAG_PING_SENT);
                    }
                }
            }

            if ping_sent {
                while socket.can_recv() {
                    #[allow(clippy::collapsible_if)]
                    if let Ok((data, _src)) = socket.recv() {
                        if let Ok(pkt) = Icmpv4Packet::new_checked(data) {
                            let caps = ChecksumCapabilities::default();
                            if let Ok(Icmpv4Repr::EchoReply { ident, seq_no, .. }) = Icmpv4Repr::parse(&pkt, &caps) {
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
        for (id, slot) in tcp_table.iter_mut().enumerate() {
            let (handle, reply_ep, created_at_ms) = match slot {
                None => continue,
                Some(e) => (e.handle, e.reply_ep, e.created_at_ms),
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
                    slot.as_mut().unwrap().reply_ep = None;
                }
                tcp::State::Closed | tcp::State::CloseWait => {
                    ulib::sys_debug_log(id as u64, 0xBEEF_2005);
                    let mut rep = [0u8; 8];
                    rep[4..8].copy_from_slice(&NET_ERR_REFUSED.to_le_bytes());
                    sys_try_channel_send(rp, &rep);
                    sys_channel_close(rp);
                    sockets.remove(handle);
                    *slot = None;
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
                    *slot = None;
                }
                _ => {} // SynSent / SynReceived — still connecting
            }
        }

        // ── 3. TCP RX push ────────────────────────────────────────────────────
        for slot in tcp_table.iter_mut() {
            let (handle, notify_ep) = match slot {
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
                            slot.as_mut().unwrap().notify_ep = None;
                            break;
                        }
                    }
                    Err(_) => {
                        // EOF — zero-length push signals connection closed
                        sys_try_channel_send(nep, &[]);
                        slot.as_mut().unwrap().notify_ep = None;
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
                                _local_port: local_port,
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
                    if let Some(entry) = tcp_table.get(sock_id).and_then(|s| s.as_ref()) {
                        let handle = entry.handle;
                        let _ = sockets
                            .get_mut::<tcp::Socket>(handle)
                            .send_slice(&req_buf[5..n]);
                    }
                }

                // MSG_RECV: [3][sock_id:u32][notify_ep:u64]
                NET_MSG_RECV if n >= 13 => {
                    let sock_id =
                        u32::from_le_bytes(req_buf[1..5].try_into().unwrap()) as usize;
                    let notify_ep =
                        u64::from_le_bytes(req_buf[5..13].try_into().unwrap());
                    if let Some(entry) = tcp_table.get_mut(sock_id).and_then(|s| s.as_mut()) {
                        entry.notify_ep = Some(notify_ep);
                    }
                }

                // MSG_CLOSE: [4][sock_id:u32]
                NET_MSG_CLOSE if n >= 5 => {
                    let sock_id =
                        u32::from_le_bytes(req_buf[1..5].try_into().unwrap()) as usize;
                    if let Some(entry) = tcp_table.get_mut(sock_id).and_then(|s| s.take()) {
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
            Some(d) => d.total_millis().clamp(1, 100),
            None    => 100,
        };
        sys_wait_for_event(&[rx_notify_recv_ep, net_recv_ep], 0, timeout_ms);
    }
}

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    ulib::sys_exit(1);
}

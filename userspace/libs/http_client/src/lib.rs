//! HTTP/1.0 client for Bos OS with HTTPS (TLS 1.3) support.
//!
//! Uses the net_server IPC API (via `ulib::net`) to open TCP connections.
//! HTTPS is handled via `embedded-tls` (TLS 1.3, no certificate verification).
//! Follows up to 3 redirects automatically.
//!
//! # Example
//! ```no_run
//! let net = ulib::net::net_lookup();
//! match http_client::http_get(net, "https://example.com/") {
//!     Ok(resp) => { /* resp.status, resp.body */ }
//!     Err(e)   => { /* handle error */ }
//! }
//! ```

#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use kernel_api_types::IPC_OK;
use ulib::net::{net_close, net_connect, net_recv_subscribe, net_resolve, net_send};

/// Maximum accumulated response size before returning `HttpError::TooLarge`.
pub const MAX_RESPONSE_BYTES: usize = 1 * 1024 * 1024; // 1 MiB

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum HttpError {
    /// DNS resolution failed (host not found or timeout).
    DnsError,
    /// TCP connection refused or timed out.
    ConnectError,
    /// Response was larger than `MAX_RESPONSE_BYTES`.
    TooLarge,
    /// The server's response could not be parsed as HTTP.
    ParseError,
    /// TLS handshake or encrypted I/O failed.
    TlsError,
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

// ── embedded-io adapter over IPC TCP sockets ────────────────────────────────
//
// IPC channels are message-based: each sys_channel_recv returns one complete
// message and discards bytes that don't fit in the caller's buffer.  TLS needs
// a byte-stream, so we buffer the IPC messages internally.

struct TcpStream {
    net_ep: u64,
    sock_id: u32,
    rx_ep: u64,
    /// Internal read buffer holding the last IPC message.
    rxbuf: [u8; 4096],
    /// Start offset of unconsumed data in `rxbuf`.
    rx_pos: usize,
    /// End offset (exclusive) of valid data in `rxbuf`.
    rx_len: usize,
    eof: bool,
}

impl TcpStream {
    fn new(net_ep: u64, sock_id: u32, rx_ep: u64) -> Self {
        Self {
            net_ep,
            sock_id,
            rx_ep,
            rxbuf: [0u8; 4096],
            rx_pos: 0,
            rx_len: 0,
            eof: false,
        }
    }
}

impl embedded_io::ErrorType for TcpStream {
    type Error = embedded_io::ErrorKind;
}

impl embedded_io::Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Return buffered data first.
        if self.rx_pos < self.rx_len {
            let avail = self.rx_len - self.rx_pos;
            let n = avail.min(buf.len());
            buf[..n].copy_from_slice(&self.rxbuf[self.rx_pos..self.rx_pos + n]);
            self.rx_pos += n;
            ulib::sys_debug_log(n as u64, 0xAA00_0001); // buffered read
            return Ok(n);
        }

        if self.eof {
            return Ok(0);
        }

        // Buffer empty — receive a new IPC message into our internal buffer.
        ulib::sys_debug_log(buf.len() as u64, 0xAA00_0002); // waiting for IPC
        let (ret, n) = ulib::sys_channel_recv(self.rx_ep, &mut self.rxbuf);
        let n = n as usize;
        ulib::sys_debug_log(((ret as u64) << 32) | n as u64, 0xAA00_0003); // IPC result
        if ret != IPC_OK || n == 0 {
            self.eof = true;
            return Ok(0);
        }

        // Copy as much as the caller wants.
        let copy = n.min(buf.len());
        buf[..copy].copy_from_slice(&self.rxbuf[..copy]);
        // Buffer the rest for next read.
        self.rx_pos = copy;
        self.rx_len = n;
        Ok(copy)
    }
}

impl embedded_io::Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        ulib::sys_debug_log(buf.len() as u64, 0xAA00_0010); // TLS write
        net_send(self.net_ep, self.sock_id, buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

// ── Hardware RNG via RDRAND ─────────────────────────────────────────────────

struct RdRandRng;

impl rand_core::CryptoRng for RdRandRng {}

impl rand_core::RngCore for RdRandRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        let val: u64;
        unsafe {
            core::arch::asm!(
                "2: rdrand {val}",
                "jnc 2b",
                val = out(reg) val,
            );
        }
        val
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut i = 0;
        while i + 8 <= dest.len() {
            let val = self.next_u64();
            dest[i..i + 8].copy_from_slice(&val.to_le_bytes());
            i += 8;
        }
        if i < dest.len() {
            let val = self.next_u64();
            let bytes = val.to_le_bytes();
            for j in 0..dest.len() - i {
                dest[i + j] = bytes[j];
            }
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Fetch `url` via HTTP/1.0 GET.
///
/// Supports both `http://` and `https://` URLs.
/// Bare `"host/path"` (without scheme) defaults to HTTP.
/// Follows up to 3 redirects. Returns the final response.
pub fn http_get(net_ep: u64, url: &str) -> Result<HttpResponse, HttpError> {
    let (host, path, use_tls) = parse_url(url);
    http_get_host(net_ep, host, path, use_tls, 3)
}

// ── Internal implementation ───────────────────────────────────────────────────

fn http_get_host(
    net_ep: u64,
    host: &str,
    path: &str,
    use_tls: bool,
    hops_left: usize,
) -> Result<HttpResponse, HttpError> {
    // ── 1. DNS resolve ───────────────────────────────────────────────────────
    let dns_host = match host.rfind(':') {
        Some(i) => &host[..i],
        None    => host,
    };
    let ip = if let Some(ip) = parse_ipv4_literal(dns_host) {
        ip
    } else {
        net_resolve(net_ep, dns_host.as_bytes()).ok_or(HttpError::DnsError)?
    };

    // ── 2. TCP connect ───────────────────────────────────────────────────────
    let default_port: u16 = if use_tls { 443 } else { 80 };
    let port: u16 = match host.rfind(':') {
        Some(i) => host[i + 1..].parse().unwrap_or(default_port),
        None    => default_port,
    };
    let sock = net_connect(net_ep, ip, port).ok_or(HttpError::ConnectError)?;
    let rx_ep = net_recv_subscribe(net_ep, sock);

    // ── 3. Send request + read response ──────────────────────────────────────
    let mut req_buf = [0u8; 2048];
    let req_len = write_get_request(&mut req_buf, host, path);

    let mut raw: Vec<u8> = Vec::new();

    if use_tls {
        let result = do_tls_request(net_ep, sock, rx_ep, dns_host, &req_buf[..req_len], &mut raw);
        if let Err(e) = result {
            ulib::sys_channel_close(rx_ep);
            net_close(net_ep, sock);
            return Err(e);
        }
    } else {
        net_send(net_ep, sock, &req_buf[..req_len]);

        let mut chunk = [0u8; 4096];
        loop {
            let (ret, n) = ulib::sys_channel_recv(rx_ep, &mut chunk);
            let n = n as usize;
            if ret != IPC_OK || n == 0 {
                break;
            }
            if raw.len() + n > MAX_RESPONSE_BYTES {
                ulib::sys_channel_close(rx_ep);
                net_close(net_ep, sock);
                return Err(HttpError::TooLarge);
            }
            raw.extend_from_slice(&chunk[..n]);
        }
    }
    ulib::sys_channel_close(rx_ep);
    net_close(net_ep, sock);

    // ── 4. Parse HTTP response headers ───────────────────────────────────────
    let (status, body_offset, redirect) = {
        let mut hdr_storage = [httparse::EMPTY_HEADER; 64];
        let mut resp = httparse::Response::new(&mut hdr_storage);

        let body_offset = match resp.parse(&raw) {
            Ok(httparse::Status::Complete(n)) => n,
            _ => return Err(HttpError::ParseError),
        };
        let status = resp.code.unwrap_or(0);

        let redirect: Option<([u8; 512], usize)> =
            if matches!(status, 301 | 302 | 303 | 307 | 308) && hops_left > 0 {
                find_header(resp.headers, b"location").map(|loc| {
                    let mut buf = [0u8; 512];
                    let n = loc.len().min(511);
                    buf[..n].copy_from_slice(&loc[..n]);
                    (buf, n)
                })
            } else {
                None
            };

        (status, body_offset, redirect)
    };

    // ── 5. Follow redirect if present ────────────────────────────────────────
    if let Some((loc_buf, loc_len)) = redirect {
        if let Ok(loc) = core::str::from_utf8(&loc_buf[..loc_len]) {
            if loc.starts_with("https://") || loc.starts_with("http://") {
                let (new_host, new_path, new_tls) = parse_url(loc);
                return http_get_host(net_ep, new_host, new_path, new_tls, hops_left - 1);
            } else if loc.starts_with('/') {
                return http_get_host(net_ep, host, loc, use_tls, hops_left - 1);
            }
        }
    }

    // ── 6. Return response ───────────────────────────────────────────────────
    let body = raw[body_offset..].to_vec();
    Ok(HttpResponse { status, body })
}

/// Perform a TLS handshake, send the HTTP request, and read the response.
/// On success, the response bytes are appended to `raw`.
fn do_tls_request(
    net_ep: u64,
    sock_id: u32,
    rx_ep: u64,
    server_name: &str,
    request: &[u8],
    raw: &mut Vec<u8>,
) -> Result<(), HttpError> {
    let stream = TcpStream::new(net_ep, sock_id, rx_ep);

    // Heap-allocate TLS record buffers (16 KiB each) to avoid stack overflow.
    let mut read_buf = alloc::vec![0u8; 16384];
    let mut write_buf = alloc::vec![0u8; 16384];

    let config = embedded_tls::TlsConfig::new()
        .with_server_name(server_name);

    let mut tls: embedded_tls::blocking::TlsConnection<_, embedded_tls::Aes128GcmSha256> =
        embedded_tls::blocking::TlsConnection::new(stream, &mut read_buf, &mut write_buf);

    let context = embedded_tls::TlsContext::new(
        &config,
        embedded_tls::UnsecureProvider::new::<embedded_tls::Aes128GcmSha256>(RdRandRng),
    );
    match tls.open(context) {
        Ok(()) => {}
        Err(e) => {
            // Log TLS error discriminant for debugging
            let code = match e {
                embedded_tls::TlsError::IoError => 1,
                embedded_tls::TlsError::InvalidRecord => 2,
                embedded_tls::TlsError::UnknownContentType => 3,
                embedded_tls::TlsError::InvalidHandshake => 4,
                embedded_tls::TlsError::InvalidCertificate => 5,
                embedded_tls::TlsError::InvalidSignature => 6,
                embedded_tls::TlsError::DecodeError => 7,
                embedded_tls::TlsError::InternalError => 8,
                embedded_tls::TlsError::InvalidApplicationData => 9,
                embedded_tls::TlsError::MissingHandshake => 10,
                _ => 99,
            };
            ulib::sys_debug_log(code, 0x1F5E_0001);
            return Err(HttpError::TlsError);
        }
    }

    // Send the HTTP request through TLS.
    write_all_tls(&mut tls, request)?;
    tls.flush().map_err(|_| HttpError::TlsError)?;

    // Read response until EOF.
    let mut chunk = [0u8; 4096];
    loop {
        match tls.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if raw.len() + n > MAX_RESPONSE_BYTES {
                    return Err(HttpError::TooLarge);
                }
                raw.extend_from_slice(&chunk[..n]);
            }
            Err(_) => break, // TLS close_notify or error → treat as EOF
        }
    }

    let _ = tls.close();
    Ok(())
}

fn write_all_tls(
    tls: &mut embedded_tls::blocking::TlsConnection<TcpStream, embedded_tls::Aes128GcmSha256>,
    mut data: &[u8],
) -> Result<(), HttpError> {
    while !data.is_empty() {
        let n = tls.write(data).map_err(|_| HttpError::TlsError)?;
        if n == 0 {
            return Err(HttpError::TlsError);
        }
        data = &data[n..];
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Split a URL into `(host, path, use_tls)`.
/// Host may include a port: `"example.com:8080"`.
fn parse_url(url: &str) -> (&str, &str, bool) {
    if let Some(rest) = url.strip_prefix("https://") {
        let (host, path) = split_host_path(rest);
        (host, path, true)
    } else {
        let rest = url.strip_prefix("http://").unwrap_or(url);
        let (host, path) = split_host_path(rest);
        (host, path, false)
    }
}

fn split_host_path(s: &str) -> (&str, &str) {
    match s.find('/') {
        Some(i) => (&s[..i], &s[i..]),
        None    => (s, "/"),
    }
}

/// Build `GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n`
/// into `buf`. Returns the number of bytes written.
fn write_get_request(buf: &mut [u8], host: &str, path: &str) -> usize {
    let parts: &[&str] = &[
        "GET ",
        path,
        " HTTP/1.0\r\nHost: ",
        host,
        "\r\nConnection: close\r\n\r\n",
    ];
    let mut pos = 0;
    for part in parts {
        let b = part.as_bytes();
        let n = b.len().min(buf.len().saturating_sub(pos));
        buf[pos..pos + n].copy_from_slice(&b[..n]);
        pos += n;
    }
    pos
}

/// If `s` is a dotted-decimal IPv4 literal ("a.b.c.d"), parse and return it.
fn parse_ipv4_literal(s: &str) -> Option<[u8; 4]> {
    let mut parts = s.splitn(5, '.');
    let a = parts.next()?.parse::<u8>().ok()?;
    let b = parts.next()?.parse::<u8>().ok()?;
    let c = parts.next()?.parse::<u8>().ok()?;
    let d = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() { return None; }
    Some([a, b, c, d])
}

/// Case-insensitive header lookup. Returns the header value bytes if found.
fn find_header<'h>(headers: &[httparse::Header<'h>], name: &[u8]) -> Option<&'h [u8]> {
    for h in headers {
        if h.name.as_bytes().eq_ignore_ascii_case(name) {
            return Some(h.value);
        }
    }
    None
}

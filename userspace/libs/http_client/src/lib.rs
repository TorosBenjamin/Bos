//! HTTP/1.0 client for Bos OS.
//!
//! Uses the net_server IPC API (via `ulib::net`) to open TCP connections.
//! HTTPS is not supported. Follows up to 3 redirects automatically.
//!
//! # Example
//! ```no_run
//! let net = ulib::net::net_lookup();
//! match http_client::http_get(net, "http://example.com/") {
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
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Fetch `url` via HTTP/1.0 GET.
///
/// `url` must be an `http://` URL (e.g. `"http://example.com/page"`).
/// Bare `"host/path"` (without scheme) is also accepted.
/// Follows up to 3 redirects. Returns the final response.
pub fn http_get(net_ep: u64, url: &str) -> Result<HttpResponse, HttpError> {
    let (host, path) = parse_url(url);
    http_get_host(net_ep, host, path, 3)
}

// ── Internal implementation ───────────────────────────────────────────────────

fn http_get_host(
    net_ep: u64,
    host: &str,
    path: &str,
    hops_left: usize,
) -> Result<HttpResponse, HttpError> {
    // ── 1. DNS resolve ───────────────────────────────────────────────────────
    // Strip port from host for DNS (e.g. "example.com:8080" → "example.com").
    let dns_host = match host.rfind(':') {
        Some(i) => &host[..i],
        None    => host,
    };
    // If the host is already a dotted-decimal IPv4 literal, skip DNS.
    let ip = if let Some(ip) = parse_ipv4_literal(dns_host) {
        ip
    } else {
        net_resolve(net_ep, dns_host.as_bytes()).ok_or(HttpError::DnsError)?
    };

    // ── 2. TCP connect ───────────────────────────────────────────────────────
    let port: u16 = match host.rfind(':') {
        Some(i) => host[i + 1..].parse().unwrap_or(80),
        None    => 80,
    };
    let sock = net_connect(net_ep, ip, port).ok_or(HttpError::ConnectError)?;
    let rx_ep = net_recv_subscribe(net_ep, sock);

    // ── 3. Send HTTP/1.0 GET request ─────────────────────────────────────────
    let mut req_buf = [0u8; 2048];
    let req_len = write_get_request(&mut req_buf, host, path);
    net_send(net_ep, sock, &req_buf[..req_len]);

    // ── 4. Read response until EOF ───────────────────────────────────────────
    let mut raw: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let (ret, n) = ulib::sys_channel_recv(rx_ep, &mut chunk);
        let n = n as usize;
        // DEBUG: log each recv result — ret in high 32, n in low 32
        ulib::sys_debug_log(((ret as u64) << 32) | n as u64, 0xBEEF_3002);
        if ret != IPC_OK || n == 0 {
            break; // IPC error or zero-length EOF signal from net_server
        }
        if raw.len() + n > MAX_RESPONSE_BYTES {
            ulib::sys_channel_close(rx_ep);
            net_close(net_ep, sock);
            return Err(HttpError::TooLarge);
        }
        raw.extend_from_slice(&chunk[..n]);
    }
    ulib::sys_channel_close(rx_ep);
    net_close(net_ep, sock);
    // DEBUG: log total received bytes
    ulib::sys_debug_log(raw.len() as u64, 0xBEEF_3001);

    // ── 5. Parse HTTP response headers ───────────────────────────────────────
    // Use a scoped block so httparse borrows on `raw` are released before we
    // potentially call http_get_host again for a redirect.
    let (status, body_offset, redirect) = {
        let mut hdr_storage = [httparse::EMPTY_HEADER; 64];
        let mut resp = httparse::Response::new(&mut hdr_storage);

        let body_offset = match resp.parse(&raw) {
            Ok(httparse::Status::Complete(n)) => n,
            _ => return Err(HttpError::ParseError),
        };
        let status = resp.code.unwrap_or(0);

        // For redirects: copy Location into a fixed stack buffer so we can
        // drop the httparse borrow on `raw` before recursing.
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
        // resp and hdr_storage drop here — httparse borrows on `raw` released
    };

    // ── 6. Follow redirect if present ────────────────────────────────────────
    if let Some((loc_buf, loc_len)) = redirect {
        if let Ok(loc) = core::str::from_utf8(&loc_buf[..loc_len]) {
            if loc.starts_with("http://") {
                // Absolute URL redirect
                let (new_host, new_path) = parse_url(loc);
                return http_get_host(net_ep, new_host, new_path, hops_left - 1);
            } else if loc.starts_with('/') {
                // Root-relative redirect — reuse same host
                return http_get_host(net_ep, host, loc, hops_left - 1);
            }
            // Other schemes (https://, //) or relative paths: fall through and
            // return the redirect response as-is.
        }
    }

    // ── 7. Return response ───────────────────────────────────────────────────
    let body = raw[body_offset..].to_vec();
    Ok(HttpResponse { status, body })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Split `http://host/path` (or bare `host/path`) into `(host, path)`.
/// Host may include a port: `"example.com:8080"`.
fn parse_url(url: &str) -> (&str, &str) {
    let without_scheme = url.strip_prefix("http://").unwrap_or(url);
    match without_scheme.find('/') {
        Some(i) => (&without_scheme[..i], &without_scheme[i..]),
        None    => (without_scheme, "/"),
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

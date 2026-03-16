//! HTTP/1.0 client with HTTPS (TLS 1.3) support.
//!
//! Uses [`bos_std::net`] for DNS resolution and TCP connections, so it works
//! on both Bos and Linux without modification.
//!
//! # Example
//! ```no_run
//! match http_client::http_get("https://example.com/") {
//!     Ok(resp) => { /* resp.status, resp.body */ }
//!     Err(e)   => { /* handle error */ }
//! }
//! ```

#![no_std]
extern crate alloc;

mod tls;
mod url;

use alloc::vec::Vec;
use bos_std::net;
use tls::{do_tls_request_128, do_tls_request_256};
use url::{parse_url, write_get_request, parse_ipv4_literal, find_header};

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

// ── Public API ────────────────────────────────────────────────────────────────

/// Fetch `url` via HTTP/1.0 GET.
///
/// Supports both `http://` and `https://` URLs.
/// Bare `"host/path"` (without scheme) defaults to HTTP.
/// Follows up to 3 redirects. Returns the final response.
pub fn http_get(url: &str) -> Result<HttpResponse, HttpError> {
    let (host, path, use_tls) = parse_url(url);
    http_get_host(host, path, use_tls, 3)
}

// ── Internal implementation ───────────────────────────────────────────────────

fn http_get_host(
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
        net::resolve(dns_host.as_bytes()).ok_or(HttpError::DnsError)?
    };

    // ── 2. TCP connect ───────────────────────────────────────────────────────
    let default_port: u16 = if use_tls { 443 } else { 80 };
    let port: u16 = match host.rfind(':') {
        Some(i) => host[i + 1..].parse().unwrap_or(default_port),
        None    => default_port,
    };
    let mut stream = net::TcpStream::connect(ip, port).ok_or(HttpError::ConnectError)?;

    // ── 3. Send request + read response ──────────────────────────────────────
    let mut req_buf = [0u8; 2048];
    let req_len = write_get_request(&mut req_buf, host, path);

    let mut raw: Vec<u8> = Vec::new();

    if use_tls {
        // Try AES-128-GCM first; if TLS fails, reconnect and try AES-256-GCM.
        let tls_result = do_tls_request_128(&mut stream, dns_host, &req_buf[..req_len], &mut raw);
        if tls_result.is_err() {
            drop(stream);
            raw.clear();
            let mut stream2 = net::TcpStream::connect(ip, port).ok_or(HttpError::ConnectError)?;
            do_tls_request_256(&mut stream2, dns_host, &req_buf[..req_len], &mut raw)?;
            stream = stream2;
        }
    } else {
        use embedded_io::Write as _;
        stream.write_all(&req_buf[..req_len]).map_err(|_| HttpError::ConnectError)?;

        use embedded_io::Read as _;
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if raw.len() + n > MAX_RESPONSE_BYTES {
                        return Err(HttpError::TooLarge);
                    }
                    raw.extend_from_slice(&chunk[..n]);
                }
                Err(_) => break,
            }
        }
    }
    drop(stream);

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
                return http_get_host(new_host, new_path, new_tls, hops_left - 1);
            } else if loc.starts_with('/') {
                return http_get_host(host, loc, use_tls, hops_left - 1);
            }
        }
    }

    // ── 6. Return response ───────────────────────────────────────────────────
    let body = raw[body_offset..].to_vec();
    Ok(HttpResponse { status, body })
}

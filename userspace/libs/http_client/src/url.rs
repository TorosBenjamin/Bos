//! URL parsing, HTTP request building, and small helpers.

/// Parse a URL into `(host, path, use_tls)`.
///
/// `https://` URLs set `use_tls = true`. Bare `host/path` defaults to HTTP.
pub(crate) fn parse_url(url: &str) -> (&str, &str, bool) {
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

/// Write an HTTP/1.0 GET request into `buf`, returning the number of bytes written.
pub(crate) fn write_get_request(buf: &mut [u8], host: &str, path: &str) -> usize {
    let parts: &[&str] = &[
        "GET ",
        path,
        " HTTP/1.0\r\nHost: ",
        host,
        "\r\nUser-Agent: Boser/0.1 (BosOS; https://github.com/nicheOS)\r\nConnection: close\r\n\r\n",
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

/// Try to parse `s` as a dotted-decimal IPv4 address.
pub(crate) fn parse_ipv4_literal(s: &str) -> Option<[u8; 4]> {
    let mut parts = s.splitn(5, '.');
    let a = parts.next()?.parse::<u8>().ok()?;
    let b = parts.next()?.parse::<u8>().ok()?;
    let c = parts.next()?.parse::<u8>().ok()?;
    let d = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() { return None; }
    Some([a, b, c, d])
}

/// Find an HTTP header by name (case-insensitive).
pub(crate) fn find_header<'h>(headers: &[httparse::Header<'h>], name: &[u8]) -> Option<&'h [u8]> {
    for h in headers {
        if h.name.as_bytes().eq_ignore_ascii_case(name) {
            return Some(h.value);
        }
    }
    None
}

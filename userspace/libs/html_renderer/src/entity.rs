//! HTML entity decoding and attribute extraction.

use alloc::string::String;

/// Decode an HTML entity starting at `bytes[0]` (which should be `&`).
///
/// Returns `(decoded_char, bytes_consumed)`. If the entity is recognized but
/// invisible (e.g. `&shy;`), `decoded_char` is `None`.
pub(crate) fn decode_entity(bytes: &[u8]) -> (Option<char>, usize) {
    if bytes.starts_with(b"&amp;") { return (Some('&'), 5); }
    if bytes.starts_with(b"&lt;") { return (Some('<'), 4); }
    if bytes.starts_with(b"&gt;") { return (Some('>'), 4); }
    if bytes.starts_with(b"&nbsp;") || bytes.starts_with(b"&#160;") { return (Some(' '), 6); }
    if bytes.starts_with(b"&quot;") { return (Some('"'), 6); }
    if bytes.starts_with(b"&apos;") { return (Some('\''), 6); }
    // Soft hyphen — invisible word-break hint, strip it
    if bytes.starts_with(b"&shy;") || bytes.starts_with(b"&#173;") { return (None, 5); }
    if bytes.starts_with(b"&mdash;") { return (Some('-'), 7); }
    if bytes.starts_with(b"&ndash;") { return (Some('-'), 7); }
    if bytes.starts_with(b"&laquo;") { return (Some('"'), 7); }
    if bytes.starts_with(b"&raquo;") { return (Some('"'), 7); }
    if bytes.starts_with(b"&ldquo;") { return (Some('"'), 7); }
    if bytes.starts_with(b"&rdquo;") { return (Some('"'), 7); }
    if bytes.starts_with(b"&lsquo;") { return (Some('\''), 7); }
    if bytes.starts_with(b"&rsquo;") { return (Some('\''), 7); }
    if bytes.starts_with(b"&hellip;") { return (Some('.'), 8); }
    if bytes.starts_with(b"&#") {
        if let Some(semi) = bytes.iter().position(|&c| c == b';') {
            return (None, semi + 1);
        }
    }
    // Unknown named entity — skip to semicolon if present
    if bytes.starts_with(b"&") {
        if let Some(semi) = bytes[1..].iter().position(|&c| c == b';') {
            if semi < 10 { // reasonable entity name length
                return (None, semi + 2);
            }
        }
    }
    (None, 1)
}

/// Extract the value of an attribute from a tag's raw bytes.
///
/// E.g. `extract_attr_value(b"<a href=\"url\">", b"href")` returns `Some("url")`.
pub(crate) fn extract_attr_value(tag_bytes: &[u8], attr: &[u8]) -> Option<String> {
    let mut j = 0;
    while j + attr.len() < tag_bytes.len() {
        if j > 0 && tag_bytes[j - 1].is_ascii_whitespace() {
            let candidate = &tag_bytes[j..j + attr.len()];
            if candidate.eq_ignore_ascii_case(attr) {
                let after_idx = j + attr.len();
                if after_idx < tag_bytes.len() && tag_bytes[after_idx] == b'=' {
                    let val_start = after_idx + 1;
                    if val_start >= tag_bytes.len() { return None; }
                    let quote = tag_bytes[val_start];
                    if quote == b'"' || quote == b'\'' {
                        let content_start = val_start + 1;
                        if let Some(end) = tag_bytes[content_start..].iter().position(|&c| c == quote) {
                            let val = &tag_bytes[content_start..content_start + end];
                            return core::str::from_utf8(val).ok().map(|s| String::from(s));
                        }
                    } else {
                        let end = tag_bytes[val_start..].iter()
                            .position(|&c| c == b' ' || c == b'>' || c == b'\t' || c == b'\n')
                            .unwrap_or(tag_bytes.len() - val_start);
                        let val = &tag_bytes[val_start..val_start + end];
                        return core::str::from_utf8(val).ok().map(|s| String::from(s));
                    }
                }
            }
        }
        j += 1;
    }
    None
}

/// Check if a tag's raw bytes contain a given attribute name.
pub(crate) fn has_attr(tag_bytes: &[u8], attr: &[u8]) -> bool {
    let mut j = 0;
    while j + attr.len() <= tag_bytes.len() {
        if j > 0
            && tag_bytes[j - 1].is_ascii_whitespace()
            && tag_bytes[j..].len() >= attr.len()
        {
            let candidate = &tag_bytes[j..j + attr.len()];
            if candidate.eq_ignore_ascii_case(attr) {
                let after = if j + attr.len() < tag_bytes.len() {
                    tag_bytes[j + attr.len()]
                } else {
                    b'>'
                };
                if after == b' ' || after == b'>' || after == b'=' || after == b'/'
                    || after == b'\n' || after == b'\r' || after == b'\t'
                {
                    return true;
                }
            }
        }
        j += 1;
    }
    false
}

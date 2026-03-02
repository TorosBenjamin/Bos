/// Display server configuration, loaded from `/HYPR.CONF` on the FAT32 filesystem.
///
/// Format:
/// ```
/// # comment
/// [general]
/// outer_gap = 8
/// inner_gap = 8
/// border_size = 2
///
/// [colors]
/// border_focused   = #8aadf4
/// border_unfocused = #363a4f
/// bg_top           = #1e3a5f
/// bg_bottom        = #0a0a0f
/// ```
/// Unknown keys/sections are silently ignored.
pub struct DisplayConfig {
    pub outer_gap:        u32,
    pub inner_gap:        u32,
    pub border_size:      i32,
    pub border_focused:   (u8, u8, u8),
    pub border_unfocused: (u8, u8, u8),
    pub bg_top:           (u8, u8, u8),
    pub bg_bottom:        (u8, u8, u8),
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            outer_gap:        8,
            inner_gap:        8,
            border_size:      2,
            border_focused:   (0x8a, 0xad, 0xf4), // #8aadf4 Catppuccin Macchiato blue
            border_unfocused: (0x36, 0x3a, 0x4f), // #363a4f Catppuccin Macchiato surface0
            bg_top:           (0x1e, 0x3a, 0x5f), // #1e3a5f
            bg_bottom:        (0x0a, 0x0a, 0x0f), // #0a0a0f
        }
    }
}

impl DisplayConfig {
    /// Parse a TOML-like config file from a byte slice.
    /// Returns a config populated from the file, with defaults for any missing keys.
    pub fn parse(bytes: &[u8]) -> Self {
        let mut cfg = Self::default();

        #[derive(Clone, Copy, PartialEq)]
        enum Section { General, Colors, Unknown }
        let mut section = Section::Unknown;

        for raw_line in bytes.split(|&b| b == b'\n') {
            let line = trim_bytes(raw_line);

            // Skip blank lines and comment lines
            if line.is_empty() || line[0] == b'#' {
                continue;
            }

            // Section header: [name]
            if line[0] == b'[' {
                if let Some(end) = line.iter().position(|&b| b == b']') {
                    section = match &line[1..end] {
                        b"general" => Section::General,
                        b"colors"  => Section::Colors,
                        _          => Section::Unknown,
                    };
                }
                continue;
            }

            // key = value
            if let Some(eq_pos) = line.iter().position(|&b| b == b'=') {
                let key = trim_bytes(&line[..eq_pos]);
                let raw_val = trim_bytes(&line[eq_pos + 1..]);
                // Strip trailing inline comment
                let val = match raw_val.iter().position(|&b| b == b'#') {
                    Some(hash) => trim_bytes(&raw_val[..hash]),
                    None => raw_val,
                };

                match section {
                    Section::General => match key {
                        b"outer_gap"   => { if let Some(v) = parse_u32(val) { cfg.outer_gap   = v; } }
                        b"inner_gap"   => { if let Some(v) = parse_u32(val) { cfg.inner_gap   = v; } }
                        b"border_size" => { if let Some(v) = parse_i32(val) { cfg.border_size = v; } }
                        _ => {}
                    },
                    Section::Colors => match key {
                        b"border_focused"   => { if let Some(v) = parse_hex_color(val) { cfg.border_focused   = v; } }
                        b"border_unfocused" => { if let Some(v) = parse_hex_color(val) { cfg.border_unfocused = v; } }
                        b"bg_top"           => { if let Some(v) = parse_hex_color(val) { cfg.bg_top           = v; } }
                        b"bg_bottom"        => { if let Some(v) = parse_hex_color(val) { cfg.bg_bottom        = v; } }
                        _ => {}
                    },
                    Section::Unknown => {}
                }
            }
        }

        cfg
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn trim_bytes(s: &[u8]) -> &[u8] {
    let start = match s.iter().position(|b| !b.is_ascii_whitespace()) {
        Some(i) => i,
        None => return &[],
    };
    let end = match s.iter().rposition(|b| !b.is_ascii_whitespace()) {
        Some(i) => i,
        None => return &[],
    };
    &s[start..=end]
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut val: u32 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(val)
}

fn parse_i32(s: &[u8]) -> Option<i32> {
    if s.is_empty() {
        return None;
    }
    let (neg, digits) = if s[0] == b'-' { (true, &s[1..]) } else { (false, s) };
    let u = parse_u32(digits)?;
    if neg { Some(-(u as i32)) } else { Some(u as i32) }
}

fn parse_hex_color(s: &[u8]) -> Option<(u8, u8, u8)> {
    let s = if s.first() == Some(&b'#') { &s[1..] } else { s };
    if s.len() < 6 {
        return None;
    }
    let r = hex_byte(s[0], s[1])?;
    let g = hex_byte(s[2], s[3])?;
    let b = hex_byte(s[4], s[5])?;
    Some((r, g, b))
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn hex_byte(hi: u8, lo: u8) -> Option<u8> {
    Some((hex_nibble(hi)? << 4) | hex_nibble(lo)?)
}

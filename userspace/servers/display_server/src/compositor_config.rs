/// Display server configuration, loaded from `/bos_ds.conf` on the FAT32 filesystem.
///
/// Format:
/// ```
/// # comment
/// [general]
/// outer_gap = 8
/// inner_gap = 8
/// border_size = 2
/// inactive_opacity = 80   # percent, 0–100; default 100 (fully opaque)
///
/// [colors]
/// border_focused   = #8aadf4
/// border_unfocused = #363a4f
/// bg_top           = #1e3a5f
/// bg_bottom        = #0a0a0f
///
/// [window_rules]
/// hello_egui = float
/// bouncing_cube_1 = tile
///
/// [shortcuts]
/// close_window  = super+q
/// focus_next    = alt+tab
/// focus_prev    = alt+shift+tab
/// focus_left    = super+left
/// focus_right   = super+right
/// focus_up      = super+up
/// focus_down    = super+down
/// ```
/// Unknown keys/sections are silently ignored.

use kernel_api_types::{KeyEventType, KEY_MOD_SHIFT, KEY_MOD_ALT, KEY_MOD_SUPER};

#[derive(Clone, Copy, PartialEq)]
pub enum WindowMode { Tiled, Floating }

pub struct WindowRule {
    pub app_id:     [u8; 32],
    pub app_id_len: u8,
    pub mode:       WindowMode,
}

/// Actions that can be triggered by a keyboard shortcut.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ShortcutAction {
    CloseWindow = 0,
    FocusNext   = 1,   // cycle focus forward  (default: Alt+Tab)
    FocusPrev   = 2,   // cycle focus backward (default: Alt+Shift+Tab)
    FocusLeft   = 3,   // focus window to the left  (default: Super+Left)
    FocusRight  = 4,   // focus window to the right (default: Super+Right)
    FocusUp     = 5,   // focus window above        (default: Super+Up)
    FocusDown   = 6,   // focus window below        (default: Super+Down)
}

/// A single key binding: modifier bitmask + key type + optional character.
#[derive(Clone, Copy)]
pub struct ShortcutBinding {
    pub action:    ShortcutAction,
    /// Bitmask of required modifiers (KEY_MOD_* from kernel_api_types).
    pub modifiers: u8,
    pub key_type:  KeyEventType,
    /// For Char events: the expected character (lowercase). Ignored for non-Char keys.
    pub character: u8,
}

pub const MAX_SHORTCUTS: usize = 16;

pub struct DisplayConfig {
    pub outer_gap:        u32,
    pub inner_gap:        u32,
    pub border_size:      i32,
    pub border_focused:   (u8, u8, u8),
    pub border_unfocused: (u8, u8, u8),
    pub bg_top:           (u8, u8, u8),
    pub bg_bottom:        (u8, u8, u8),
    pub window_rules:     [Option<WindowRule>; 16],
    pub n_window_rules:   usize,
    /// Opacity for inactive (unfocused) tiled windows, 0–255 (255 = fully opaque, no dimming).
    pub inactive_opacity: u8,
    /// Opacity for inactive (unfocused) floating windows, 0–255.
    pub inactive_opacity_floating: u8,
    pub shortcuts:        [Option<ShortcutBinding>; MAX_SHORTCUTS],
    pub n_shortcuts:      usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        const NONE_RULE:     Option<WindowRule>      = None;
        const NONE_SHORTCUT: Option<ShortcutBinding> = None;

        let mut shortcuts = [NONE_SHORTCUT; MAX_SHORTCUTS];
        let mut n = 0usize;

        // Default shortcuts — mirrors the [shortcuts] section in bos_ds.conf.
        shortcuts[n] = Some(ShortcutBinding { action: ShortcutAction::CloseWindow, modifiers: KEY_MOD_SUPER,                key_type: KeyEventType::Char,       character: b'q' }); n += 1;
        shortcuts[n] = Some(ShortcutBinding { action: ShortcutAction::FocusNext,   modifiers: KEY_MOD_ALT,                  key_type: KeyEventType::Tab,        character: 0   }); n += 1;
        shortcuts[n] = Some(ShortcutBinding { action: ShortcutAction::FocusPrev,   modifiers: KEY_MOD_ALT | KEY_MOD_SHIFT,  key_type: KeyEventType::Tab,        character: 0   }); n += 1;
        shortcuts[n] = Some(ShortcutBinding { action: ShortcutAction::FocusLeft,   modifiers: KEY_MOD_SUPER,                key_type: KeyEventType::ArrowLeft,  character: 0   }); n += 1;
        shortcuts[n] = Some(ShortcutBinding { action: ShortcutAction::FocusRight,  modifiers: KEY_MOD_SUPER,                key_type: KeyEventType::ArrowRight, character: 0   }); n += 1;
        shortcuts[n] = Some(ShortcutBinding { action: ShortcutAction::FocusUp,     modifiers: KEY_MOD_SUPER,                key_type: KeyEventType::ArrowUp,    character: 0   }); n += 1;
        shortcuts[n] = Some(ShortcutBinding { action: ShortcutAction::FocusDown,   modifiers: KEY_MOD_SUPER,                key_type: KeyEventType::ArrowDown,  character: 0   }); n += 1;

        Self {
            outer_gap:        8,
            inner_gap:        8,
            border_size:      2,
            border_focused:   (0x8a, 0xad, 0xf4),
            border_unfocused: (0x36, 0x3a, 0x4f),
            bg_top:           (0x1e, 0x3a, 0x5f),
            bg_bottom:        (0x0a, 0x0a, 0x0f),
            window_rules:     [NONE_RULE; 16],
            n_window_rules:   0,
            inactive_opacity: 204,
            inactive_opacity_floating: 255,
            shortcuts,
            n_shortcuts: n,
        }
    }
}

impl DisplayConfig {
    /// Parse a TOML-like config file from a byte slice.
    /// Returns a config populated from the file, with defaults for any missing keys.
    pub fn parse(bytes: &[u8]) -> Self {
        let mut cfg = Self::default();

        #[derive(Clone, Copy, PartialEq)]
        enum Section { General, Colors, WindowRules, Shortcuts, Unknown }
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
                        b"general"      => Section::General,
                        b"colors"       => Section::Colors,
                        b"window_rules" => Section::WindowRules,
                        b"shortcuts"    => Section::Shortcuts,
                        _               => Section::Unknown,
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
                        b"inactive_opacity" => {
                            if let Some(v) = parse_u32(val) {
                                cfg.inactive_opacity = ((v.min(100) * 255) / 100) as u8;
                            }
                        }
                        b"inactive_opacity_floating" => {
                            if let Some(v) = parse_u32(val) {
                                cfg.inactive_opacity_floating = ((v.min(100) * 255) / 100) as u8;
                            }
                        }
                        _ => {}
                    },
                    Section::Colors => match key {
                        b"border_focused"   => { if let Some(v) = parse_hex_color(val) { cfg.border_focused   = v; } }
                        b"border_unfocused" => { if let Some(v) = parse_hex_color(val) { cfg.border_unfocused = v; } }
                        b"bg_top"           => { if let Some(v) = parse_hex_color(val) { cfg.bg_top           = v; } }
                        b"bg_bottom"        => { if let Some(v) = parse_hex_color(val) { cfg.bg_bottom        = v; } }
                        _ => {}
                    },
                    Section::WindowRules => {
                        if cfg.n_window_rules < 16 {
                            let mode = match val {
                                b"float" => Some(WindowMode::Floating),
                                b"tile"  => Some(WindowMode::Tiled),
                                _        => None,
                            };
                            if let Some(mode) = mode {
                                let len = key.len().min(32);
                                let mut app_id = [0u8; 32];
                                app_id[..len].copy_from_slice(&key[..len]);
                                cfg.window_rules[cfg.n_window_rules] = Some(WindowRule {
                                    app_id,
                                    app_id_len: len as u8,
                                    mode,
                                });
                                cfg.n_window_rules += 1;
                            }
                        }
                    }
                    Section::Shortcuts => {
                        if cfg.n_shortcuts < MAX_SHORTCUTS {
                            if let Some(action) = parse_shortcut_action(key) {
                                if let Some((mods, kt, ch)) = parse_key_combo(val) {
                                    // Override any existing binding for this action.
                                    let mut replaced = false;
                                    for slot in cfg.shortcuts[..cfg.n_shortcuts].iter_mut() {
                                        if let Some(b) = slot {
                                            if b.action == action {
                                                *b = ShortcutBinding { action, modifiers: mods, key_type: kt, character: ch };
                                                replaced = true;
                                                break;
                                            }
                                        }
                                    }
                                    if !replaced {
                                        cfg.shortcuts[cfg.n_shortcuts] = Some(ShortcutBinding {
                                            action, modifiers: mods, key_type: kt, character: ch,
                                        });
                                        cfg.n_shortcuts += 1;
                                    }
                                }
                            }
                        }
                    }
                    Section::Unknown => {}
                }
            }
        }

        cfg
    }

    /// Look up any config-defined window rule for the given app_id bytes.
    pub fn lookup_window_rule(&self, app_id: &[u8]) -> Option<WindowMode> {
        for i in 0..self.n_window_rules {
            if let Some(ref r) = self.window_rules[i] {
                if &r.app_id[..r.app_id_len as usize] == app_id {
                    return Some(r.mode);
                }
            }
        }
        None
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn parse_shortcut_action(key: &[u8]) -> Option<ShortcutAction> {
    match key {
        b"close_window" => Some(ShortcutAction::CloseWindow),
        b"focus_next"   => Some(ShortcutAction::FocusNext),
        b"focus_prev"   => Some(ShortcutAction::FocusPrev),
        b"focus_left"   => Some(ShortcutAction::FocusLeft),
        b"focus_right"  => Some(ShortcutAction::FocusRight),
        b"focus_up"     => Some(ShortcutAction::FocusUp),
        b"focus_down"   => Some(ShortcutAction::FocusDown),
        _ => None,
    }
}

/// Parse a key combo string such as `super+q`, `alt+tab`, `alt+shift+tab`, `super+left`.
/// Returns `(modifier_bitmask, KeyEventType, character_byte)`.
fn parse_key_combo(val: &[u8]) -> Option<(u8, KeyEventType, u8)> {
    let mut mods      = 0u8;
    let mut key_type  = None::<KeyEventType>;
    let mut character = 0u8;

    let mut remaining = val;
    loop {
        let (token, rest) = match remaining.iter().position(|&b| b == b'+') {
            Some(pos) => (&remaining[..pos], &remaining[pos + 1..]),
            None      => (remaining, &[][..]),
        };
        let token = trim_bytes(token);
        match token {
            b"super" | b"win" => mods |= KEY_MOD_SUPER,
            b"alt"            => mods |= KEY_MOD_ALT,
            b"ctrl"           => mods |= kernel_api_types::KEY_MOD_CTRL,
            b"shift"          => mods |= KEY_MOD_SHIFT,
            b"tab"            => { key_type = Some(KeyEventType::Tab);        character = 0; }
            b"enter"          => { key_type = Some(KeyEventType::Enter);      character = 0; }
            b"escape" | b"esc" => { key_type = Some(KeyEventType::Escape);   character = 0; }
            b"left"           => { key_type = Some(KeyEventType::ArrowLeft);  character = 0; }
            b"right"          => { key_type = Some(KeyEventType::ArrowRight); character = 0; }
            b"up"             => { key_type = Some(KeyEventType::ArrowUp);    character = 0; }
            b"down"           => { key_type = Some(KeyEventType::ArrowDown);  character = 0; }
            other if other.len() == 1 => {
                key_type  = Some(KeyEventType::Char);
                character = other[0].to_ascii_lowercase();
            }
            _ => {}
        }
        if rest.is_empty() { break; }
        remaining = rest;
    }

    key_type.map(|kt| (mods, kt, character))
}

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

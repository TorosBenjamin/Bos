/// Launcher configuration parsed from LAUNCH.CFG.
///
/// Sections:
///   [apps]    — `Name = FILE.ELF` entries
///   [pinned]  — one app name per line (marks that app as pinned)
///   [visual]  — `width = N`

pub const MAX_APPS: usize = 32;

pub struct AppEntry {
    pub name:     [u8; 32],
    pub name_len: u8,
    pub path:     [u8; 16],
    pub path_len: u8,
    pub pinned:   bool,
}

pub struct LauncherConfig {
    pub apps:   [AppEntry; MAX_APPS],
    pub n_apps: usize,
    pub width:  u32,
}

const EMPTY_ENTRY: AppEntry = AppEntry {
    name:     [0u8; 32],
    name_len: 0,
    path:     [0u8; 16],
    path_len: 0,
    pinned:   false,
};

impl Default for LauncherConfig {
    fn default() -> Self {
        LauncherConfig {
            apps:   [EMPTY_ENTRY; MAX_APPS],
            n_apps: 0,
            width:  400,
        }
    }
}

impl LauncherConfig {
    pub fn parse(bytes: &[u8]) -> Self {
        let mut cfg = LauncherConfig::default();

        #[derive(Clone, Copy, PartialEq)]
        enum Section { Apps, Pinned, Visual, Unknown }
        let mut section = Section::Unknown;

        for raw_line in bytes.split(|&b| b == b'\n') {
            let line = trim_bytes(raw_line);
            if line.is_empty() || line[0] == b'#' { continue; }

            if line[0] == b'[' {
                if let Some(end) = line.iter().position(|&b| b == b']') {
                    section = match &line[1..end] {
                        b"apps"   => Section::Apps,
                        b"pinned" => Section::Pinned,
                        b"visual" => Section::Visual,
                        _         => Section::Unknown,
                    };
                }
                continue;
            }

            match section {
                Section::Apps => {
                    if let Some(eq_pos) = line.iter().position(|&b| b == b'=') {
                        let key = trim_bytes(&line[..eq_pos]);
                        let val = trim_bytes(&line[eq_pos + 1..]);
                        if cfg.n_apps < MAX_APPS && !key.is_empty() && !val.is_empty() {
                            let name_len = key.len().min(32);
                            let path_len = val.len().min(16);
                            let entry = &mut cfg.apps[cfg.n_apps];
                            entry.name[..name_len].copy_from_slice(&key[..name_len]);
                            entry.name_len = name_len as u8;
                            entry.path[..path_len].copy_from_slice(&val[..path_len]);
                            entry.path_len = path_len as u8;
                            entry.pinned = false;
                            cfg.n_apps += 1;
                        }
                    }
                }
                Section::Pinned => {
                    // Mark any app whose name matches this line as pinned
                    let name = line;
                    for i in 0..cfg.n_apps {
                        let entry = &mut cfg.apps[i];
                        if &entry.name[..entry.name_len as usize] == name {
                            entry.pinned = true;
                        }
                    }
                }
                Section::Visual => {
                    if let Some(eq_pos) = line.iter().position(|&b| b == b'=') {
                        let key = trim_bytes(&line[..eq_pos]);
                        let val = trim_bytes(&line[eq_pos + 1..]);
                        if key == b"width" {
                            if let Some(v) = parse_u32(val) {
                                cfg.width = v;
                            }
                        }
                    }
                }
                Section::Unknown => {}
            }
        }

        cfg
    }
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
    if s.is_empty() { return None; }
    let mut val: u32 = 0;
    for &b in s {
        if !b.is_ascii_digit() { return None; }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(val)
}

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use bos_egui::{egui, egui::KeyEventType, App};

// 10.0.2.2 = QEMU SLIRP gateway; the runner spawns an HTTP stub server on
// the host at 127.0.0.1:8000, reachable from the guest at 10.0.2.2:8000.
const URL: &str = "https://vms.gesztenye.eu/";

// ── Panic / entry ─────────────────────────────────────────────────────────────

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    bos_egui::run("boser", BoserApp::new())
}

// ── App ───────────────────────────────────────────────────────────────────────

enum State {
    /// First frame: render "Connecting..." and schedule WillFetch.
    Idle,
    /// Second frame: do the blocking HTTP fetch.
    WillFetch,
    /// Fetch complete; lines is the word-wrapped body text.
    Ready { lines: Vec<String>, scroll: usize },
    /// Fetch failed.
    Error(String),
}

struct BoserApp {
    state: State,
}

impl BoserApp {
    fn new() -> Self {
        Self { state: State::Idle }
    }
}

impl App for BoserApp {
    fn update(&mut self, ctx: &egui::Context) {
        // ── State transitions ─────────────────────────────────────────────────
        // Frame 1: Idle → WillFetch (renders "Connecting...", requests frame 2)
        if matches!(self.state, State::Idle) {
            self.state = State::WillFetch;
            bos_egui::request_redraw();
        }
        // Frame 2: WillFetch → blocking HTTP fetch (previous frame was presented)
        else if matches!(self.state, State::WillFetch) {
            self.state = do_fetch();
        }

        // ── Keyboard scrolling ────────────────────────────────────────────────
        if let State::Ready { ref lines, ref mut scroll } = self.state {
            let (_, h) = ctx.screen_size();
            // estimate visible lines: each label is ~17px, heading 20, subtract header area
            let visible: usize = (h.saturating_sub(60) / 17) as usize;
            let max_scroll = lines.len().saturating_sub(visible);

            if ctx.key_pressed(KeyEventType::ArrowDown) && *scroll < max_scroll {
                *scroll += 1;
            }
            if ctx.key_pressed(KeyEventType::ArrowUp) && *scroll > 0 {
                *scroll -= 1;
            }
            if ctx.key_pressed(KeyEventType::PageDown) {
                *scroll = (*scroll + visible).min(max_scroll);
            }
            if ctx.key_pressed(KeyEventType::PageUp) {
                *scroll = scroll.saturating_sub(visible);
            }
            if ctx.key_pressed(KeyEventType::Home) {
                *scroll = 0;
            }
            if ctx.key_pressed(KeyEventType::End) {
                *scroll = max_scroll;
            }
        }

        // ── Draw ──────────────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("boser — Bos text browser");
            ui.separator();

            match &self.state {
                State::Idle | State::WillFetch => {
                    ui.label("Connecting...");
                    ui.label(URL);
                }
                State::Error(msg) => {
                    ui.label("Fetch error:");
                    ui.label(msg.as_str());
                }
                State::Ready { lines, scroll } => {
                    let (_, h) = ctx.screen_size();
                    let visible: usize = (h.saturating_sub(60) / 17) as usize;
                    let end = (*scroll + visible).min(lines.len());
                    for line in &lines[*scroll..end] {
                        ui.label(line.as_str());
                    }
                }
            }
        });
    }
}

// ── HTTP fetch + HTML extraction ──────────────────────────────────────────────

fn do_fetch() -> State {
    let net_ep = ulib::net::net_lookup();

    match http_client::http_get(net_ep, URL) {
        Err(e) => {
            let code: u64 = match e {
                http_client::HttpError::DnsError     => 1,
                http_client::HttpError::ConnectError => 2,
                http_client::HttpError::TooLarge     => 3,
                http_client::HttpError::ParseError   => 4,
                http_client::HttpError::TlsError     => 5,
            };
            ulib::sys_debug_log(code, 0xB05E_0001);
            let msg = match e {
                http_client::HttpError::DnsError     => "DNS resolution failed",
                http_client::HttpError::ConnectError => "TCP connection failed",
                http_client::HttpError::TooLarge     => "Response too large (>1 MiB)",
                http_client::HttpError::ParseError   => "Invalid HTTP response",
                http_client::HttpError::TlsError     => "TLS handshake failed",
            };
            State::Error(String::from(msg))
        }
        Ok(resp) => {
            ulib::sys_debug_log(resp.status as u64, 0xB05E_0002);
            let (_, h) = screen_width_cols();
            let body_text = core::str::from_utf8(&resp.body).unwrap_or("(binary body)");
            let plain = extract_text(body_text);
            let status_line = if resp.status == 200 {
                alloc::format!("HTTP 200 OK  — {URL}")
            } else {
                alloc::format!("HTTP {}  — {URL}", resp.status)
            };
            let mut lines = Vec::new();
            lines.push(status_line);
            lines.push(String::new());
            wrap_into(&plain, h, &mut lines);
            State::Ready { lines, scroll: 0 }
        }
    }
}

/// Returns an approximate column width for word-wrapping based on screen width.
/// Each FONT_8X13 char is 8px wide; leave 24px margin (12 each side).
fn screen_width_cols() -> (usize, u32) {
    // We don't have access to the Context here, so use a fixed fallback.
    // The renderer will clip long lines naturally.
    (100, 768)
}

/// Strip HTML tags and decode basic entities, producing plain text.
fn extract_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let bytes = html.as_bytes();
    let mut i = 0;
    let mut in_tag = false;
    let mut in_script = false; // skip <script> and <style> blocks
    let mut prev_space = true; // collapse whitespace

    while i < bytes.len() {
        let b = bytes[i];

        if in_tag {
            if b == b'>' {
                in_tag = false;
                // Insert a space at tag boundaries to separate words
                if !prev_space {
                    out.push(' ');
                    prev_space = true;
                }
            }
            i += 1;
            continue;
        }

        if b == b'<' {
            in_tag = true;
            // Detect <script / <style to skip their content
            let rest = &html[i..];
            if rest.starts_with("<script") || rest.starts_with("<SCRIPT")
                || rest.starts_with("<style") || rest.starts_with("<STYLE")
            {
                // Skip to the closing tag
                let close = if rest.starts_with("<script") || rest.starts_with("<SCRIPT") {
                    "</script>"
                } else {
                    "</style>"
                };
                if let Some(end) = find_substr(&bytes[i..], close.as_bytes()) {
                    i += end + close.len();
                    in_tag = false;
                    in_script = false;
                    continue;
                }
                in_script = true;
            }
            i += 1;
            continue;
        }

        if in_script {
            i += 1;
            continue;
        }

        // HTML entity decoding
        if b == b'&' {
            if bytes[i..].starts_with(b"&amp;") {
                out.push('&'); prev_space = false; i += 5; continue;
            } else if bytes[i..].starts_with(b"&lt;") {
                out.push('<'); prev_space = false; i += 4; continue;
            } else if bytes[i..].starts_with(b"&gt;") {
                out.push('>'); prev_space = false; i += 4; continue;
            } else if bytes[i..].starts_with(b"&nbsp;") || bytes[i..].starts_with(b"&#160;") {
                out.push(' '); prev_space = true; i += 6; continue;
            } else if bytes[i..].starts_with(b"&quot;") {
                out.push('"'); prev_space = false; i += 6; continue;
            } else if bytes[i..].starts_with(b"&apos;") {
                out.push('\''); prev_space = false; i += 6; continue;
            } else if bytes[i..].starts_with(b"&#") {
                // Skip numeric entity
                if let Some(semi) = bytes[i..].iter().position(|&c| c == b';') {
                    out.push(' '); prev_space = true; i += semi + 1; continue;
                }
            }
        }

        // Whitespace collapsing
        if b == b'\n' || b == b'\r' || b == b'\t' || b == b' ' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            i += 1;
            continue;
        }

        // Printable ASCII (skip non-ASCII for simplicity)
        if b >= 0x20 && b < 0x80 {
            out.push(b as char);
            prev_space = false;
        }
        i += 1;
    }
    out
}

/// Find `needle` in `haystack`, returning the byte offset of the first match.
fn find_substr(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() { return Some(0); }
    haystack.windows(needle.len()).position(|w| {
        w.iter().zip(needle).all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
    })
}

/// Word-wrap `text` into `lines` at ~`cols` characters per line.
fn wrap_into(text: &str, _screen_h: u32, lines: &mut Vec<String>) {
    // Each char is 8px wide; screen is typically 1024px; margin=24px each side → ~122 cols
    const COLS: usize = 100;

    let mut current = String::new();
    for word in text.split_ascii_whitespace() {
        if word.is_empty() { continue; }
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= COLS {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current.clone());
            current.clear();
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
}

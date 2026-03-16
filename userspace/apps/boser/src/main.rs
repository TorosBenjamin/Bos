#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, string::String, vec::Vec};
use bos_egui::{egui, egui::KeyEventType, App};
use html_renderer::StyledLine;

/// Line height in pixels (uniform for all content lines).
const LINE_H: i32 = 17;
/// Header area: title(20) + sep(14) + search bar(28+8) + sep(14) = ~84
const HEADER_H: u32 = 84;

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

enum Page {
    /// Empty start page — search bar is focused, nothing loaded yet.
    Home,
    /// Fetching a URL (shows "Loading...").
    Loading { url: String },
    /// A page has been fetched and parsed.
    #[allow(dead_code)]
    Ready { url: String, lines: Vec<StyledLine>, scroll: usize },
    /// Fetch failed.
    #[allow(dead_code)]
    Error { url: String, msg: String },
}

struct BoserApp {
    /// Text in the search/URL bar.
    input: String,
    /// Whether the search bar is focused (accepts text input).
    input_focused: bool,
    page: Page,
    /// Set to Some(url) to trigger a fetch on the next frame.
    pending_fetch: Option<String>,
}

impl BoserApp {
    fn new() -> Self {
        Self {
            input: String::new(),
            input_focused: true,
            page: Page::Home,
            pending_fetch: None,
        }
    }

    fn navigate(&mut self, url: String) {
        self.page = Page::Loading { url: url.clone() };
        self.pending_fetch = Some(url);
        self.input_focused = false;
        bos_egui::request_redraw();
    }

    fn handle_submit(&mut self) {
        let query = self.input.trim();
        if query.is_empty() {
            return;
        }

        // If it looks like a URL, navigate directly
        if query.starts_with("http://") || query.starts_with("https://") {
            let url = String::from(query);
            self.navigate(url);
        } else {
            // Search via DuckDuckGo HTML endpoint
            let encoded = url_encode(query);
            let url = format!("https://search.marginalia.nu/search?query={encoded}");
            self.navigate(url);
        }
    }
}

impl App for BoserApp {
    fn update(&mut self, ctx: &egui::Context) {
        // ── Deferred fetch ───────────────────────────────────────────────────
        if let Some(url) = self.pending_fetch.take() {
            self.page = do_fetch(&url);
        }

        // ── Keyboard input ───────────────────────────────────────────────────
        if let Some(key) = ctx.key_event() {
            if key.pressed {
                if self.input_focused {
                    match key.event_type {
                        KeyEventType::Enter => {
                            self.handle_submit();
                        }
                        KeyEventType::Backspace => {
                            self.input.pop();
                        }
                        KeyEventType::Escape => {
                            self.input_focused = false;
                        }
                        KeyEventType::Char => {
                            let ch = key.character;
                            if ch >= 0x20 && ch < 0x7f {
                                self.input.push(ch as char);
                            }
                        }
                        _ => {}
                    }
                } else {
                    // Page scrolling and focus shortcuts
                    match key.event_type {
                        // '/' or Tab focuses the search bar
                        KeyEventType::Tab => {
                            self.input_focused = true;
                        }
                        KeyEventType::Char if key.character == b'/' => {
                            self.input_focused = true;
                        }
                        _ => {}
                    }

                    // Scroll the page content
                    if let Page::Ready { ref lines, ref mut scroll, .. } = self.page {
                        let (_, h) = ctx.screen_size();
                        let visible = (h.saturating_sub(HEADER_H) / LINE_H as u32) as usize;
                        let max_scroll = lines.len().saturating_sub(visible);

                        match key.event_type {
                            KeyEventType::ArrowDown if *scroll < max_scroll => *scroll += 1,
                            KeyEventType::ArrowUp if *scroll > 0 => *scroll -= 1,
                            KeyEventType::PageDown => *scroll = (*scroll + visible).min(max_scroll),
                            KeyEventType::PageUp => *scroll = scroll.saturating_sub(visible),
                            KeyEventType::Home => *scroll = 0,
                            KeyEventType::End => *scroll = max_scroll,
                            _ => {}
                        }
                    }
                }
            }
        }

        // ── Draw ──────────────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("boser");
            // Search / URL bar
            ui.text_edit_singleline(&mut self.input);
            ui.separator();

            match &self.page {
                Page::Home => {
                    ui.label("Type a search query or URL and press Enter.");
                }
                Page::Loading { url } => {
                    ui.label("Loading...");
                    ui.label(url.as_str());
                }
                Page::Error { msg, .. } => {
                    ui.label("Error:");
                    ui.label(msg.as_str());
                }
                Page::Ready { lines, scroll, url } => {
                    if let Some(href) = render_styled(ui, ctx, lines, *scroll) {
                        // Resolve relative URLs against the current page
                        let nav_url = resolve_url(url, &href);
                        self.input = nav_url.clone();
                        self.navigate(nav_url);
                    }
                }
            }
        });
    }
}

// ── Styled content rendering ─────────────────────────────────────────────────

/// Render styled lines and return the href of a clicked link, if any.
fn render_styled(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    lines: &[StyledLine],
    scroll: usize,
) -> Option<String> {
    use egui::{
        FONT_8X13, FONT_8X13_BOLD,
        FG, HEADING, LINK, CODE, EMPHASIS, DIMMED,
    };

    let click = ctx.take_click();
    let (_, h) = ctx.screen_size();
    let visible: usize = (h.saturating_sub(HEADER_H) / LINE_H as u32) as usize;
    let end = (scroll + visible).min(lines.len());

    let mut canvas = ui.canvas();

    // Click coordinates relative to canvas origin
    let click_rel = click.map(|(cx, cy)| {
        (cx as i32 - canvas.origin_x, cy as i32 - canvas.origin_y)
    });

    let mut clicked_href: Option<String> = None;
    let mut y: i32 = 0;

    for line in &lines[scroll..end] {
        if line.spans.is_empty() {
            y += LINE_H;
            continue;
        }

        let mut x: i32 = 0;
        for span in &line.spans {
            let color = if span.style.heading {
                HEADING
            } else if span.style.link {
                LINK
            } else if span.style.code {
                CODE
            } else if span.style.emphasis {
                EMPHASIS
            } else if span.style.indent > 0 && !span.style.bold {
                DIMMED
            } else {
                FG
            };

            let font = if span.style.bold { &FONT_8X13_BOLD } else { &FONT_8X13 };

            let span_w = (span.text.len() as i32) * 8;

            // Check if click hits this span
            if let Some((cx, cy)) = click_rel {
                if span.href.is_some()
                    && cx >= x && cx < x + span_w
                    && cy >= y && cy < y + LINE_H
                {
                    clicked_href = span.href.clone();
                }
            }

            canvas.draw_text(&span.text, x, y, color, font);
            x += span_w;
        }

        y += LINE_H;
    }

    clicked_href
}

// ── HTTP fetch ───────────────────────────────────────────────────────────────

fn do_fetch(url: &str) -> Page {
    match http_client::http_get(url) {
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
            Page::Error { url: String::from(url), msg: String::from(msg) }
        }
        Ok(resp) => {
            ulib::sys_debug_log(resp.status as u64, 0xB05E_0002);
            let body_text = core::str::from_utf8(&resp.body).unwrap_or("(binary body)");
            let lines = html_renderer::parse_html(body_text, 100);
            Page::Ready { url: String::from(url), lines, scroll: 0 }
        }
    }
}

// ── URL encoding ─────────────────────────────────────────────────────────────

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(hex_digit(b >> 4));
                out.push(hex_digit(b & 0x0f));
            }
        }
    }
    out
}

/// Resolve a possibly-relative URL against a base URL.
fn resolve_url(base: &str, href: &str) -> String {
    // Absolute URL — use as-is
    if href.starts_with("http://") || href.starts_with("https://") {
        return String::from(href);
    }

    // Protocol-relative (//example.com/path)
    if href.starts_with("//") {
        if base.starts_with("https://") {
            return format!("https:{href}");
        } else {
            return format!("http:{href}");
        }
    }

    // Extract scheme + host from base
    let (scheme, rest) = if let Some(r) = base.strip_prefix("https://") {
        ("https://", r)
    } else if let Some(r) = base.strip_prefix("http://") {
        ("http://", r)
    } else {
        ("http://", base)
    };

    let host = match rest.find('/') {
        Some(i) => &rest[..i],
        None => rest,
    };

    if href.starts_with('/') {
        // Absolute path
        format!("{scheme}{host}{href}")
    } else {
        // Relative path — append to base directory
        let base_path = match rest.rfind('/') {
            Some(i) => &rest[..i + 1],
            None => rest,
        };
        format!("{scheme}{base_path}{href}")
    }
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + n - 10) as char,
    }
}

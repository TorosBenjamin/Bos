#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, string::String, vec, vec::Vec};
use bos_egui::{egui, egui::KeyEventType, App};
use html_renderer::{ContentBlock, StyledLine};

/// Line height in pixels (uniform for text lines).
const LINE_H: i32 = 17;
/// Header area: title(20) + sep(14) + search bar(28+8) + sep(14) = ~84
const HEADER_H: u32 = 84;
/// Maximum image file size to fetch (200 KB).
const MAX_IMAGE_BYTES: usize = 200 * 1024;
/// Maximum number of images to fetch per page.
const MAX_IMAGES: usize = 10;

// ── Panic / entry ─────────────────────────────────────────────────────────────

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    bos_egui::run("boser", BoserApp::new())
}

// ── Resolved content blocks ──────────────────────────────────────────────────

enum ResolvedBlock {
    Text(StyledLine),
    Image { width: u32, height: u32, pixels: Vec<u8> },
    ImagePending { url: String },
}

impl ResolvedBlock {
    fn height(&self) -> i32 {
        match self {
            ResolvedBlock::Text(_) | ResolvedBlock::ImagePending { .. } => LINE_H,
            ResolvedBlock::Image { height, .. } => *height as i32,
        }
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

enum Page {
    Home,
    Loading { url: String },
    #[allow(dead_code)]
    Ready { url: String, blocks: Vec<ResolvedBlock>, scroll_y: i32 },
    #[allow(dead_code)]
    Error { url: String, msg: String },
}

struct BoserApp {
    input: String,
    input_focused: bool,
    page: Page,
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

        if query.starts_with("http://") || query.starts_with("https://") {
            let url = String::from(query);
            self.navigate(url);
        } else {
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

        // ── Lazy image loading (one per frame) ──────────────────────────────
        if let Page::Ready { ref url, ref mut blocks, .. } = self.page {
            if let Some(idx) = blocks.iter().position(|b| matches!(b, ResolvedBlock::ImagePending { .. })) {
                let img_src = match &blocks[idx] {
                    ResolvedBlock::ImagePending { url } => url.clone(),
                    _ => unreachable!(),
                };
                let resolved_url = resolve_url(url, &img_src);
                match fetch_and_decode_image(&resolved_url) {
                    Some((pixels, w, h)) => {
                        blocks[idx] = ResolvedBlock::Image { width: w, height: h, pixels };
                    }
                    None => {
                        blocks.remove(idx);
                    }
                }
                bos_egui::request_redraw();
            }
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
                    match key.event_type {
                        KeyEventType::Tab => {
                            self.input_focused = true;
                        }
                        KeyEventType::Char if key.character == b'/' => {
                            self.input_focused = true;
                        }
                        _ => {}
                    }

                    // Pixel-based scrolling
                    if let Page::Ready { ref blocks, ref mut scroll_y, .. } = self.page {
                        let (_, h) = ctx.screen_size();
                        let viewport_h = h.saturating_sub(HEADER_H) as i32;
                        let total_h: i32 = blocks.iter().map(|b| b.height()).sum();
                        let max_scroll = (total_h - viewport_h).max(0);

                        match key.event_type {
                            KeyEventType::ArrowDown => *scroll_y = (*scroll_y + LINE_H).min(max_scroll),
                            KeyEventType::ArrowUp => *scroll_y = (*scroll_y - LINE_H).max(0),
                            KeyEventType::PageDown => *scroll_y = (*scroll_y + viewport_h).min(max_scroll),
                            KeyEventType::PageUp => *scroll_y = (*scroll_y - viewport_h).max(0),
                            KeyEventType::Home => *scroll_y = 0,
                            KeyEventType::End => *scroll_y = max_scroll,
                            _ => {}
                        }
                    }
                }
            }
        }

        // ── Draw ──────────────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("boser");
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
                Page::Ready { blocks, scroll_y, url } => {
                    if let Some(href) = render_blocks(ui, ctx, blocks, *scroll_y) {
                        let nav_url = resolve_url(url, &href);
                        self.input = nav_url.clone();
                        self.navigate(nav_url);
                    }
                }
            }
        });
    }
}

// ── Content rendering ────────────────────────────────────────────────────────

fn render_blocks(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    blocks: &[ResolvedBlock],
    scroll_y: i32,
) -> Option<String> {
    use egui::{
        FONT_8X13, FONT_8X13_BOLD,
        FG, HEADING, LINK, CODE, EMPHASIS, DIMMED,
    };

    let click = ctx.take_click();
    let mut canvas = ui.canvas();
    let canvas_h = canvas.height;

    let click_rel = click.map(|(cx, cy)| {
        (cx as i32 - canvas.origin_x, cy as i32 - canvas.origin_y)
    });

    let mut clicked_href: Option<String> = None;
    let mut y: i32 = -scroll_y;

    for block in blocks {
        let block_h = block.height();

        // Skip blocks entirely above viewport
        if y + block_h <= 0 {
            y += block_h;
            continue;
        }
        // Stop if past the bottom
        if y >= canvas_h {
            break;
        }

        match block {
            ResolvedBlock::Text(line) => {
                if !line.spans.is_empty() {
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
                }
            }
            ResolvedBlock::Image { width, height, pixels } => {
                canvas.draw_image(pixels, *width, *height, 0, y);
            }
            ResolvedBlock::ImagePending { .. } => {
                canvas.draw_text("[loading image...]", 0, y, DIMMED, &FONT_8X13);
            }
        }

        y += block_h;
    }

    clicked_href
}

// ── HTTP fetch + image resolution ────────────────────────────────────────────

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
            let content = html_renderer::parse_html(body_text, 100);
            let blocks = build_blocks(&content);
            Page::Ready { url: String::from(url), blocks, scroll_y: 0 }
        }
    }
}

fn build_blocks(content: &[ContentBlock]) -> Vec<ResolvedBlock> {
    let mut blocks = Vec::with_capacity(content.len());
    let mut image_count = 0;

    for block in content {
        match block {
            ContentBlock::Text(line) => {
                blocks.push(ResolvedBlock::Text(line.clone()));
            }
            ContentBlock::Image { url } => {
                if image_count >= MAX_IMAGES {
                    continue;
                }
                blocks.push(ResolvedBlock::ImagePending { url: url.clone() });
                image_count += 1;
            }
        }
    }

    blocks
}

fn fetch_and_decode_image(url: &str) -> Option<(Vec<u8>, u32, u32)> {
    let resp = http_client::http_get(url).ok()?;
    if resp.body.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let img = bos_image::decode(&resp.body).ok()?;
    Some(scale_to_fit(img.pixels, img.width, img.height, 800))
}

/// Nearest-neighbor downscale if image is wider than `max_w`.
fn scale_to_fit(pixels: Vec<u8>, w: u32, h: u32, max_w: u32) -> (Vec<u8>, u32, u32) {
    if w <= max_w {
        return (pixels, w, h);
    }
    let new_w = max_w;
    let new_h = (h as u64 * max_w as u64 / w as u64) as u32;
    if new_h == 0 {
        return (vec![0u8; (new_w * 4) as usize], new_w, 1);
    }
    let mut out = vec![0u8; (new_w * new_h * 4) as usize];
    for dy in 0..new_h {
        let sy = (dy as u64 * h as u64 / new_h as u64) as u32;
        let sy = sy.min(h - 1);
        for dx in 0..new_w {
            let sx = (dx as u64 * w as u64 / new_w as u64) as u32;
            let sx = sx.min(w - 1);
            let src_i = (sy * w + sx) as usize * 4;
            let dst_i = (dy * new_w + dx) as usize * 4;
            out[dst_i..dst_i + 4].copy_from_slice(&pixels[src_i..src_i + 4]);
        }
    }
    (out, new_w, new_h)
}

// ── URL helpers ──────────────────────────────────────────────────────────────

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

fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return String::from(href);
    }
    if href.starts_with("//") {
        if base.starts_with("https://") {
            return format!("https:{href}");
        } else {
            return format!("http:{href}");
        }
    }

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
        format!("{scheme}{host}{href}")
    } else {
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

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;
use alloc::string::String;

mod config;
use config::{LauncherConfig, MAX_APPS};

use kernel_api_types::{MMAP_WRITE, SVC_ERR_NOT_FOUND};
use ulib::window::{Window, WindowEvent, WINDOW_FLAG_HIDDEN};

#[global_allocator]
static ALLOCATOR: linked_list_allocator::LockedHeap = linked_list_allocator::LockedHeap::empty();

#[alloc_error_handler]
fn oom(_: core::alloc::Layout) -> ! {
    loop { core::hint::spin_loop(); }
}

#[panic_handler]
fn rust_panic(_: &core::panic::PanicInfo) -> ! {
    loop { ulib::sys_yield(); }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Returns true if `haystack` contains `needle` (case-insensitive ASCII).
fn bytes_contains_icase(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() { return true; }
    if needle.len() > haystack.len() { return false; }
    'outer: for start in 0..=(haystack.len() - needle.len()) {
        for (i, &nc) in needle.iter().enumerate() {
            if haystack[start + i].to_ascii_lowercase() != nc.to_ascii_lowercase() {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

// ── Launcher state ───────────────────────────────────────────────────────────

enum Action {
    None,
    Launch(usize),
    Hide,
}

struct Launcher {
    config:          LauncherConfig,
    search:          String,
    filtered:        [usize; MAX_APPS],
    nfilt:           usize,
    sel:             usize,
    fs_ep:           u64,
    dirty:           bool,
    frame_presented: bool,
    cursor_x:        f32,
    cursor_y:        f32,
    click:           Option<(f32, f32)>,
}

impl Launcher {
    fn new(config: LauncherConfig, fs_ep: u64) -> Self {
        let mut s = Launcher {
            config,
            search: String::new(),
            filtered: [0usize; MAX_APPS],
            nfilt: 0,
            sel: 0,
            fs_ep,
            dirty: true,
            frame_presented: true,
            cursor_x: 0.0,
            cursor_y: 0.0,
            click: None,
        };
        s.filter();
        s
    }

    fn filter(&mut self) {
        self.nfilt = 0;
        // When search is empty: pinned items first, then others
        if self.search.is_empty() {
            for i in 0..self.config.n_apps {
                if self.config.apps[i].pinned {
                    self.filtered[self.nfilt] = i;
                    self.nfilt += 1;
                }
            }
            for i in 0..self.config.n_apps {
                if !self.config.apps[i].pinned {
                    self.filtered[self.nfilt] = i;
                    self.nfilt += 1;
                }
            }
        } else {
            // Search active: match all, pinned still first
            for i in 0..self.config.n_apps {
                let entry = &self.config.apps[i];
                let name = &entry.name[..entry.name_len as usize];
                if entry.pinned && bytes_contains_icase(name, self.search.as_bytes()) {
                    self.filtered[self.nfilt] = i;
                    self.nfilt += 1;
                }
            }
            for i in 0..self.config.n_apps {
                let entry = &self.config.apps[i];
                let name = &entry.name[..entry.name_len as usize];
                if !entry.pinned && bytes_contains_icase(name, self.search.as_bytes()) {
                    self.filtered[self.nfilt] = i;
                    self.nfilt += 1;
                }
            }
        }
        if self.sel >= self.nfilt && self.nfilt > 0 {
            self.sel = self.nfilt - 1;
        }
        if self.nfilt == 0 {
            self.sel = 0;
        }
    }

    fn reset(&mut self) {
        self.search.clear();
        self.filter();
        self.sel = 0;
        self.dirty = true;
    }

    fn do_launch(&self, idx: usize, window: &mut Window) {
        let entry = &self.config.apps[idx];
        let path_len = entry.path_len as usize;
        let path_bytes = &entry.path[..path_len];

        // Build null-terminated path string for fs_map_file
        let mut path_str = [0u8; 17];
        path_str[..path_len].copy_from_slice(path_bytes);
        // Convert to &str (ASCII 8.3 FAT name)
        if let Ok(path) = core::str::from_utf8(&path_str[..path_len]) {
            if let Some((buf_id, size)) = ulib::fs::fs_map_file(self.fs_ep, path) {
                let ptr = ulib::sys_map_shared_buf(buf_id);
                if !ptr.is_null() {
                    let elf = unsafe { core::slice::from_raw_parts(ptr as *const u8, size as usize) };
                    let name = &entry.name[..entry.name_len as usize];
                    let _ = ulib::sys_spawn_named(elf, 0, name);
                    ulib::sys_munmap(ptr, size);
                }
                ulib::sys_destroy_shared_buf(buf_id);
            }
        }
        window.hide();
    }

    fn handle_key(&mut self, k: kernel_api_types::KeyEvent) -> Action {
        use kernel_api_types::KeyEventType;
        match k.event_type {
            KeyEventType::Escape => return Action::Hide,
            KeyEventType::Enter => {
                if self.nfilt > 0 {
                    return Action::Launch(self.filtered[self.sel]);
                }
            }
            KeyEventType::ArrowDown => {
                if self.nfilt > 0 && self.sel + 1 < self.nfilt {
                    self.sel += 1;
                    self.dirty = true;
                }
            }
            KeyEventType::ArrowUp => {
                if self.sel > 0 {
                    self.sel -= 1;
                    self.dirty = true;
                }
            }
            KeyEventType::Backspace => {
                self.search.pop();
                self.filter();
                self.dirty = true;
            }
            KeyEventType::Char => {
                let ch = k.character;
                if k.modifiers != 0 { return Action::None; }
                // Digit 1-9: quick-launch nth item
                if ch >= b'1' && ch <= b'9' {
                    let idx = (ch - b'1') as usize;
                    if idx < self.nfilt {
                        return Action::Launch(self.filtered[idx]);
                    }
                    return Action::None;
                }
                // Backspace via char (some keyboards)
                if ch == 0x7f {
                    self.search.pop();
                    self.filter();
                    self.dirty = true;
                    return Action::None;
                }
                // Other printable ASCII → append to search
                if ch >= 0x20 && ch < 0x7f {
                    self.search.push(ch as char);
                    self.filter();
                    self.sel = 0;
                    self.dirty = true;
                }
            }
            _ => {}
        }
        Action::None
    }

    fn render(&mut self, window: &mut Window) -> Action {
        use kernel_api_types::graphics::DisplayInfo;

        let w = window.width();
        let h = window.height();
        let info: DisplayInfo = *window.display_info();
        let pixels = window.pixels_mut();

        // Clear to background
        let bg = info.build_pixel(0x18, 0x18, 0x1e);
        pixels.iter_mut().for_each(|p| *p = bg);

        // Simple software renderer (no bos_egui dependency to avoid cycle)
        let mut renderer = Renderer {
            pixels,
            width: w,
            height: h,
            info,
            draw_y: 12,
            margin: 12,
            cursor_x: self.cursor_x as i32,
            cursor_y: self.cursor_y as i32,
            click: self.click.take(),
            launch_idx: None,
            should_hide: false,
        };

        renderer.render_launcher(self);

        let launch_idx = renderer.launch_idx;
        let should_hide = renderer.should_hide;

        window.mark_dirty_all();
        window.present();

        if should_hide {
            return Action::Hide;
        }
        if let Some(idx) = launch_idx {
            return Action::Launch(idx);
        }
        Action::None
    }
}

// ── Simple renderer ──────────────────────────────────────────────────────────

struct Renderer<'a> {
    pixels:   &'a mut [u32],
    width:    u32,
    height:   u32,
    info:     kernel_api_types::graphics::DisplayInfo,
    draw_y:   i32,
    margin:   i32,
    cursor_x: i32,
    cursor_y: i32,
    click:    Option<(f32, f32)>,
    launch_idx: Option<usize>,
    should_hide: bool,
}

impl<'a> Renderer<'a> {
    fn px(&self, r: u8, g: u8, b: u8) -> u32 { self.info.build_pixel(r, g, b) }

    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = ((x + w).max(0) as u32).min(self.width);
        let y1 = ((y + h).max(0) as u32).min(self.height);
        for row in y0..y1 {
            for col in x0..x1 {
                self.pixels[(row as usize) * (self.width as usize) + col as usize] = color;
            }
        }
    }

    fn draw_hline(&mut self, x0: i32, x1: i32, y: i32, color: u32) {
        if y < 0 || y >= self.height as i32 { return; }
        let xa = x0.max(0) as usize;
        let xb = (x1.min(self.width as i32)) as usize;
        let row = y as usize * self.width as usize;
        for x in xa..xb {
            self.pixels[row + x] = color;
        }
    }

    fn draw_text_simple(&mut self, text: &str, x: i32, y: i32, color: u32) {
        // Use a minimal 6x10 embedded bitmap font
        for (i, &b) in text.as_bytes().iter().enumerate() {
            self.draw_glyph(b, x + i as i32 * 8, y, color);
        }
    }

    fn draw_glyph(&mut self, ch: u8, x: i32, y: i32, color: u32) {
        // Minimal readable glyphs for ASCII 0x20–0x7e using a 5-wide × 9-tall bitmap
        let bitmap = glyph_bitmap(ch);
        for row in 0..9i32 {
            let bits = bitmap[row as usize];
            for col in 0..5i32 {
                if bits & (1 << (4 - col)) != 0 {
                    let px = x + col;
                    let py = y + row;
                    if px >= 0 && py >= 0 && (px as u32) < self.width && (py as u32) < self.height {
                        self.pixels[py as usize * self.width as usize + px as usize] = color;
                    }
                }
            }
        }
    }

    fn hittest(&self, bx: i32, by: i32, bw: i32, bh: i32) -> (bool, bool) {
        let hovered = self.cursor_x >= bx && self.cursor_x < bx + bw
            && self.cursor_y >= by && self.cursor_y < by + bh;
        let clicked = if let Some((clx, cly)) = self.click {
            let (clx, cly) = (clx as i32, cly as i32);
            clx >= bx && clx < bx + bw && cly >= by && cly < by + bh
        } else { false };
        (hovered, clicked)
    }

    fn render_launcher(&mut self, state: &mut Launcher) {
        // Glyph height is 9px; this offset vertically centers text inside a row.
        const GLYPH_H: i32 = 9;
        const ROW_H:   i32 = 28;
        const ROW_GAP: i32 = 2;
        fn text_top(row_h: i32) -> i32 { (row_h - GLYPH_H) / 2 }

        let w = self.width as i32;
        let m = self.margin;
        let fg      = self.px(0xca, 0xd3, 0xf5);
        let dim     = self.px(0x6e, 0x73, 0x8d);   // dimmed number / pinned marker
        let heading = self.px(0x8a, 0xad, 0xf4);
        let sep     = self.px(0x36, 0x3a, 0x4f);
        let row_bg  = self.px(0x24, 0x27, 0x3a);
        let sel_bg  = self.px(0x49, 0x4d, 0x64);
        let hov_bg  = self.px(0x2e, 0x32, 0x44);
        let inp_bg  = self.px(0x1e, 0x1e, 0x2e);
        let inp_br  = self.px(0x49, 0x4d, 0x64);

        // ── Search box ───────────────────────────────────────────────────────
        {
            let box_h = 32i32;
            let box_w = w - m * 2;
            let bx = m;
            let by = self.draw_y;
            self.fill_rect(bx, by, box_w, box_h, inp_bg);
            // Border: top, bottom, left, right
            self.draw_hline(bx, bx + box_w, by,              inp_br);
            self.draw_hline(bx, bx + box_w, by + box_h - 1,  inp_br);
            for py in by..by + box_h {
                if py >= 0 && (py as u32) < self.height {
                    let row = py as usize * self.width as usize;
                    self.pixels[row + bx as usize]              = inp_br;
                    self.pixels[row + (bx + box_w - 1) as usize] = inp_br;
                }
            }
            // Small "Search" hint in heading colour when empty
            let txt_y = by + text_top(box_h);
            if state.search.is_empty() {
                self.draw_text_simple("Search...|", bx + 8, txt_y, dim);
            } else {
                let mut display = state.search.clone();
                display.push('|');
                self.draw_text_simple(&display, bx + 8, txt_y, fg);
            }
            self.draw_y += box_h + 6;
        }

        // ── Separator ────────────────────────────────────────────────────────
        self.draw_hline(m, w - m, self.draw_y + 4, sep);
        self.draw_y += 12;

        // ── Unified numbered list ────────────────────────────────────────────
        // "Pinned" section header (only when search is empty and there are pinned apps)
        let n_pinned = (0..state.nfilt)
            .filter(|&j| state.config.apps[state.filtered[j]].pinned)
            .count();
        let n_others = state.nfilt - n_pinned;

        if state.search.is_empty() && n_pinned > 0 {
            self.draw_text_simple("Pinned", m + 2, self.draw_y, dim);
            self.draw_y += 13;
        }

        let mut list_num = 1usize;  // display number (1-indexed, ≤9)

        for pass in 0..2usize {
            // pass 0 = pinned items, pass 1 = non-pinned
            // When search is active we do a single combined pass
            if state.search.is_empty() && pass == 1 && n_pinned > 0 && n_others > 0 {
                // separator between pinned and non-pinned
                self.draw_hline(m, w - m, self.draw_y + 4, sep);
                self.draw_y += 12;
                self.draw_text_simple("Apps", m + 2, self.draw_y, dim);
                self.draw_y += 13;
            }

            for j in 0..state.nfilt {
                let i = state.filtered[j];
                let entry = &state.config.apps[i];
                let is_pinned = entry.pinned;

                // pass 0 → only pinned; pass 1 → only non-pinned
                // When searching we do everything in pass 0
                if state.search.is_empty() {
                    if pass == 0 && !is_pinned { continue; }
                    if pass == 1 && is_pinned  { continue; }
                } else if pass == 1 {
                    break;  // already done everything in pass 0
                }

                if list_num > 9 { break; }  // only show 1-9

                let name = &entry.name[..entry.name_len as usize];
                let row_w = w - m * 2;
                let (hov, clicked) = self.hittest(m, self.draw_y, row_w, ROW_H);

                let bg = if j == state.sel { sel_bg }
                         else if hov       { hov_bg }
                         else              { row_bg };
                self.fill_rect(m, self.draw_y, row_w, ROW_H, bg);

                let txt_y = self.draw_y + text_top(ROW_H);

                // Number prefix  "1."  in heading colour for selected, dim otherwise
                let num_col = if j == state.sel { heading } else { dim };
                let num_ch = b'0' + list_num as u8;
                self.draw_glyph(num_ch, m + 4,  txt_y, num_col);
                self.draw_glyph(b'.',   m + 10, txt_y, num_col);

                // Pinned marker
                if is_pinned && state.search.is_empty() {
                    self.draw_glyph(b'*', w - m - 12, txt_y, dim);
                }

                // App name
                let name_col = if j == state.sel { fg } else { fg };
                if let Ok(s) = core::str::from_utf8(name) {
                    self.draw_text_simple(s, m + 20, txt_y, name_col);
                }

                if clicked {
                    state.sel = j;
                    self.launch_idx = Some(i);
                }

                self.draw_y += ROW_H + ROW_GAP;
                list_num += 1;
            }

            if state.search.is_empty() && pass == 0 && n_pinned == 0 {
                // No pinned items — skip straight to pass 1
            }
            if !state.search.is_empty() { break; }
        }
    }
}

// ── Minimal 5×9 bitmap font ──────────────────────────────────────────────────

fn glyph_bitmap(ch: u8) -> [u8; 9] {
    // Very minimal 5-wide glyphs for printable ASCII.
    // Each byte is a bitmask for 5 pixels (bits 4..0 = cols 0..4).
    match ch {
        b' ' => [0,0,0,0,0,0,0,0,0],
        b'!' => [0b00100,0b00100,0b00100,0b00100,0b00100,0,0b00100,0,0],
        b'>' => [0b10000,0b01000,0b00100,0b00010,0b00001,0b00010,0b00100,0b01000,0b10000],
        b'A' => [0b01110,0b10001,0b10001,0b11111,0b10001,0b10001,0b10001,0,0],
        b'B' => [0b11110,0b10001,0b10001,0b11110,0b10001,0b10001,0b11110,0,0],
        b'C' => [0b01110,0b10001,0b10000,0b10000,0b10000,0b10001,0b01110,0,0],
        b'D' => [0b11110,0b10001,0b10001,0b10001,0b10001,0b10001,0b11110,0,0],
        b'E' => [0b11111,0b10000,0b10000,0b11110,0b10000,0b10000,0b11111,0,0],
        b'F' => [0b11111,0b10000,0b10000,0b11110,0b10000,0b10000,0b10000,0,0],
        b'G' => [0b01110,0b10001,0b10000,0b10111,0b10001,0b10001,0b01111,0,0],
        b'H' => [0b10001,0b10001,0b10001,0b11111,0b10001,0b10001,0b10001,0,0],
        b'I' => [0b11111,0b00100,0b00100,0b00100,0b00100,0b00100,0b11111,0,0],
        b'J' => [0b00111,0b00010,0b00010,0b00010,0b10010,0b10010,0b01100,0,0],
        b'K' => [0b10001,0b10010,0b10100,0b11000,0b10100,0b10010,0b10001,0,0],
        b'L' => [0b10000,0b10000,0b10000,0b10000,0b10000,0b10000,0b11111,0,0],
        b'M' => [0b10001,0b11011,0b10101,0b10001,0b10001,0b10001,0b10001,0,0],
        b'N' => [0b10001,0b11001,0b10101,0b10011,0b10001,0b10001,0b10001,0,0],
        b'O' => [0b01110,0b10001,0b10001,0b10001,0b10001,0b10001,0b01110,0,0],
        b'P' => [0b11110,0b10001,0b10001,0b11110,0b10000,0b10000,0b10000,0,0],
        b'Q' => [0b01110,0b10001,0b10001,0b10001,0b10101,0b10010,0b01101,0,0],
        b'R' => [0b11110,0b10001,0b10001,0b11110,0b10100,0b10010,0b10001,0,0],
        b'S' => [0b01111,0b10000,0b10000,0b01110,0b00001,0b00001,0b11110,0,0],
        b'T' => [0b11111,0b00100,0b00100,0b00100,0b00100,0b00100,0b00100,0,0],
        b'U' => [0b10001,0b10001,0b10001,0b10001,0b10001,0b10001,0b01110,0,0],
        b'V' => [0b10001,0b10001,0b10001,0b10001,0b10001,0b01010,0b00100,0,0],
        b'W' => [0b10001,0b10001,0b10001,0b10101,0b10101,0b11011,0b10001,0,0],
        b'X' => [0b10001,0b01010,0b00100,0b00100,0b00100,0b01010,0b10001,0,0],
        b'Y' => [0b10001,0b10001,0b01010,0b00100,0b00100,0b00100,0b00100,0,0],
        b'Z' => [0b11111,0b00001,0b00010,0b00100,0b01000,0b10000,0b11111,0,0],
        b'a' => [0,0,0b01110,0b00001,0b01111,0b10001,0b01111,0,0],
        b'b' => [0b10000,0b10000,0b11110,0b10001,0b10001,0b10001,0b11110,0,0],
        b'c' => [0,0,0b01110,0b10000,0b10000,0b10001,0b01110,0,0],
        b'd' => [0b00001,0b00001,0b01111,0b10001,0b10001,0b10001,0b01111,0,0],
        b'e' => [0,0,0b01110,0b10001,0b11111,0b10000,0b01110,0,0],
        b'f' => [0b00110,0b01000,0b11110,0b01000,0b01000,0b01000,0b01000,0,0],
        b'g' => [0,0,0b01111,0b10001,0b10001,0b01111,0b00001,0b00001,0b01110],
        b'h' => [0b10000,0b10000,0b11110,0b10001,0b10001,0b10001,0b10001,0,0],
        b'i' => [0b00100,0,0b00100,0b00100,0b00100,0b00100,0b00110,0,0],
        b'j' => [0b00010,0,0b00010,0b00010,0b00010,0b10010,0b10010,0b01100,0],
        b'k' => [0b10000,0b10010,0b10100,0b11000,0b10100,0b10010,0b10001,0,0],
        b'l' => [0b01100,0b00100,0b00100,0b00100,0b00100,0b00100,0b01110,0,0],
        b'm' => [0,0,0b11010,0b10101,0b10101,0b10001,0b10001,0,0],
        b'n' => [0,0,0b11110,0b10001,0b10001,0b10001,0b10001,0,0],
        b'o' => [0,0,0b01110,0b10001,0b10001,0b10001,0b01110,0,0],
        b'p' => [0,0,0b11110,0b10001,0b10001,0b11110,0b10000,0b10000,0],
        b'q' => [0,0,0b01111,0b10001,0b10001,0b01111,0b00001,0b00001,0],
        b'r' => [0,0,0b01110,0b10001,0b10000,0b10000,0b10000,0,0],
        b's' => [0,0,0b01110,0b10000,0b01110,0b00001,0b11110,0,0],
        b't' => [0b01000,0b01000,0b11110,0b01000,0b01000,0b01001,0b00110,0,0],
        b'u' => [0,0,0b10001,0b10001,0b10001,0b10011,0b01101,0,0],
        b'v' => [0,0,0b10001,0b10001,0b10001,0b01010,0b00100,0,0],
        b'w' => [0,0,0b10001,0b10001,0b10101,0b10101,0b01010,0,0],
        b'x' => [0,0,0b10001,0b01010,0b00100,0b01010,0b10001,0,0],
        b'y' => [0,0,0b10001,0b10001,0b01111,0b00001,0b01110,0,0],
        b'z' => [0,0,0b11111,0b00010,0b00100,0b01000,0b11111,0,0],
        b'0' => [0b01110,0b10011,0b10101,0b10101,0b11001,0b01110,0,0,0],
        b'1' => [0b00100,0b01100,0b00100,0b00100,0b00100,0b01110,0,0,0],
        b'2' => [0b01110,0b10001,0b00001,0b00110,0b01000,0b11111,0,0,0],
        b'3' => [0b11111,0b00010,0b00110,0b00001,0b10001,0b01110,0,0,0],
        b'4' => [0b00010,0b00110,0b01010,0b10010,0b11111,0b00010,0,0,0],
        b'5' => [0b11111,0b10000,0b11110,0b00001,0b10001,0b01110,0,0,0],
        b'6' => [0b01110,0b10000,0b11110,0b10001,0b10001,0b01110,0,0,0],
        b'7' => [0b11111,0b00001,0b00010,0b00100,0b01000,0b01000,0,0,0],
        b'8' => [0b01110,0b10001,0b01110,0b10001,0b10001,0b01110,0,0,0],
        b'9' => [0b01110,0b10001,0b01111,0b00001,0b10001,0b01110,0,0,0],
        b'.' => [0,0,0,0,0,0b00100,0,0,0],
        b',' => [0,0,0,0,0,0b00100,0b00100,0b01000,0],
        b':' => [0,0b00100,0,0,0,0b00100,0,0,0],
        b'-' => [0,0,0,0b11111,0,0,0,0,0],
        b'_' => [0,0,0,0,0,0,0b11111,0,0],
        b'/' => [0b00001,0b00010,0b00100,0b01000,0b10000,0,0,0,0],
        b'\\' => [0b10000,0b01000,0b00100,0b00010,0b00001,0,0,0,0],
        b'|' => [0b00100,0b00100,0b00100,0b00100,0b00100,0b00100,0b00100,0b00100,0b00100],
        _ => [0b11111,0b10001,0b10001,0b10001,0b10001,0b10001,0b11111,0,0],
    }
}

// ── Entry point ──────────────────────────────────────────────────────────────

fn wait_for_service(name: &[u8]) -> u64 {
    loop {
        let ep = ulib::sys_lookup_service(name);
        if ep != SVC_ERR_NOT_FOUND {
            return ep;
        }
        ulib::sys_yield();
    }
}

fn load_config(fs_ep: u64) -> LauncherConfig {
    if let Some((buf_id, size)) = ulib::fs::fs_map_file(fs_ep, "LAUNCH.CFG") {
        let ptr = ulib::sys_map_shared_buf(buf_id);
        if !ptr.is_null() {
            let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, size as usize) };
            let cfg = LauncherConfig::parse(bytes);
            ulib::sys_munmap(ptr, size);
            ulib::sys_destroy_shared_buf(buf_id);
            return cfg;
        }
        ulib::sys_destroy_shared_buf(buf_id);
    }
    LauncherConfig::default()
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    // Heap: 8 MB
    let heap_size: usize = 8 * 1024 * 1024;
    let heap_ptr = ulib::sys_mmap(heap_size as u64, MMAP_WRITE);
    unsafe { ALLOCATOR.lock().init(heap_ptr, heap_size) }

    let ds_ep = wait_for_service(b"display");

    // Create window hidden at startup
    let mut window = loop {
        // Height: 12 margin + 32 search + 6 gap + 12 sep + (9 items × 30) + 12 margin = 344
        match Window::new_floating(ds_ep, "launcher", 0, 400, 344, WINDOW_FLAG_HIDDEN) {
            Some(w) => break w,
            None    => ulib::sys_yield(),
        }
    };

    let fs_ep = wait_for_service(b"fatfs");
    let config = load_config(fs_ep);
    let mut state = Launcher::new(config, fs_ep);

    loop {
        while let Some(ev) = window.poll_event() {
            match ev {
                WindowEvent::FocusGained => {
                    state.reset();
                }
                WindowEvent::KeyPress(k) => {
                    match state.handle_key(k) {
                        Action::Hide => {
                            window.hide();
                            state.reset();
                        }
                        Action::Launch(idx) => {
                            state.do_launch(idx, &mut window);
                            state.reset();
                        }
                        Action::None => {}
                    }
                }
                WindowEvent::MouseMove { x, y } => {
                    state.cursor_x = x as f32;
                    state.cursor_y = y as f32;
                    state.dirty = true;
                }
                WindowEvent::MouseButtonPress { x, y, .. } => {
                    state.cursor_x = x as f32;
                    state.cursor_y = y as f32;
                    state.click = Some((x as f32, y as f32));
                    state.dirty = true;
                    state.frame_presented = true;
                }
                WindowEvent::FramePresented => {
                    state.frame_presented = true;
                }
                WindowEvent::Configure { shared_buf_id, width, height } => {
                    window.apply_configure(shared_buf_id, width, height);
                    state.frame_presented = true;
                    state.dirty = true;
                }
                _ => {}
            }
        }

        if state.dirty && state.frame_presented {
            state.frame_presented = false;
            state.dirty = false;
            match state.render(&mut window) {
                Action::Launch(idx) => {
                    state.do_launch(idx, &mut window);
                    state.reset();
                }
                Action::Hide => {
                    window.hide();
                    state.reset();
                }
                Action::None => {}
            }
        }

        ulib::sys_wait_for_event(&[window.event_recv_ep()], 0, 100);
    }
}

#![no_std]
#![no_main]

extern crate alloc;

mod commands;
mod editor;

use alloc::{format, string::String, vec::Vec};
use bos_egui::{egui, App};
use egui::{CentralPanel, Rgb888, FONT_8X13, FONT_8X13_BOLD, KeyEventType};

use editor::EditorMode;

// ── Constants ─────────────────────────────────────────────────────────────────

pub(crate) const LINE_H: i32 = 17;
const MAX_SCROLLBACK: usize = 1000;
const MAX_HISTORY: usize = 64;

pub(crate) const FG:            Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);
pub(crate) const PROMPT_COLOR:  Rgb888 = Rgb888::new(0x8a, 0xad, 0xf4);
pub(crate) const ERR_COLOR:     Rgb888 = Rgb888::new(0xed, 0x87, 0x96);
pub(crate) const DIR_COLOR:     Rgb888 = Rgb888::new(0x8b, 0xd5, 0xca);
pub(crate) const DIM_COLOR:     Rgb888 = Rgb888::new(0xa5, 0xad, 0xcb);
pub(crate) const CURSOR_COLOR:  Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);
pub(crate) const STATUS_BG:     Rgb888 = Rgb888::new(0x36, 0x3a, 0x4f);
pub(crate) const DIRTY_COLOR:   Rgb888 = Rgb888::new(0xed, 0x87, 0x96);

// ── Panic / entry ─────────────────────────────────────────────────────────────

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    bos_egui::run("shell", ShellApp::new())
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct Line {
    pub text:  String,
    pub color: Rgb888,
}

impl Line {
    pub(crate) fn normal(s: String) -> Self { Self { text: s, color: FG } }
    pub(crate) fn colored(s: String, color: Rgb888) -> Self { Self { text: s, color } }
}

pub(crate) struct ShellApp {
    pub lines:          Vec<Line>,
    pub scroll_offset:  usize,
    pub auto_scroll:    bool,
    pub input:          String,
    pub cursor_pos:     usize,
    pub history:        Vec<String>,
    pub history_index:  Option<usize>,
    pub saved_input:    String,
    pub fs_fd:          Option<u32>,
    pub initialized:    bool,
    pub cursor_visible: bool,
    pub last_blink_tick: u64,
    pub cwd:            String,
    pub editor:         Option<EditorMode>,
}

impl ShellApp {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            input: String::new(),
            cursor_pos: 0,
            history: Vec::new(),
            history_index: None,
            saved_input: String::new(),
            fs_fd: None,
            initialized: false,
            cursor_visible: true,
            last_blink_tick: 0,
            cwd: String::new(), // set to "/" in initialized block (allocator not ready here)
            editor: None,
        }
    }

    pub(crate) fn prompt_str(&self) -> String {
        format!("bos:{}$ ", self.cwd)
    }

    pub(crate) fn resolve_path(&self, input: &str) -> String {
        if input.starts_with('/') {
            return String::from(input);
        }
        let base = if self.cwd == "/" { String::new() } else { self.cwd.clone() };
        let joined = format!("{}/{}", base, input);
        let mut parts: Vec<&str> = Vec::new();
        for component in joined.split('/') {
            match component {
                "" | "." => {}
                ".." => { parts.pop(); }
                c => parts.push(c),
            }
        }
        let mut result = String::from("/");
        for (i, p) in parts.iter().enumerate() {
            if i > 0 { result.push('/'); }
            result.push_str(p);
        }
        result
    }

    pub(crate) fn push_line(&mut self, line: Line) {
        self.lines.push(line);
        if self.lines.len() > MAX_SCROLLBACK {
            let excess = self.lines.len() - MAX_SCROLLBACK;
            self.lines.drain(0..excess);
            if self.scroll_offset > excess {
                self.scroll_offset -= excess;
            } else {
                self.scroll_offset = 0;
            }
        }
    }

    pub(crate) fn push_normal(&mut self, s: String) { self.push_line(Line::normal(s)); }
    pub(crate) fn push_err(&mut self, s: String) { self.push_line(Line::colored(s, ERR_COLOR)); }

    pub(crate) fn ensure_fs(&mut self) -> u32 {
        if let Some(fd) = self.fs_fd {
            return fd;
        }
        let fd = ulib::fs::fs_lookup();
        self.fs_fd = Some(fd);
        fd
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    pub(crate) fn reset_blink(&mut self) {
        self.cursor_visible = true;
        self.last_blink_tick = ulib::sys_get_ticks();
    }

    // ── Shell: input handling ─────────────────────────────────────────────────

    fn handle_key(&mut self, key: kernel_api_types::KeyEvent) {
        if !key.pressed { return; }
        let ctrl = key.modifiers & kernel_api_types::KEY_MOD_CTRL != 0;

        match key.event_type {
            KeyEventType::Char => {
                if ctrl {
                    match key.character {
                        b'l' | b'L' => { self.cmd_clear(); return; }
                        b'c' | b'C' => {
                            self.input.clear();
                            self.cursor_pos = 0;
                            self.history_index = None;
                            self.scroll_to_bottom();
                            return;
                        }
                        _ => return,
                    }
                }
                let ch = key.character;
                if (0x20..0x7f).contains(&ch) {
                    self.input.insert(self.cursor_pos, ch as char);
                    self.cursor_pos += 1;
                    self.scroll_to_bottom();
                    self.reset_blink();
                }
            }
            KeyEventType::Enter => {
                let cmd = String::from(self.input.trim());
                self.input.clear();
                self.cursor_pos = 0;
                self.history_index = None;
                self.scroll_to_bottom();
                if !cmd.is_empty() {
                    if self.history.last().is_none_or(|h| h != &cmd) {
                        self.history.push(cmd.clone());
                        if self.history.len() > MAX_HISTORY {
                            self.history.remove(0);
                        }
                    }
                    self.execute(&cmd);
                }
            }
            KeyEventType::Backspace if self.cursor_pos > 0 => {
                self.cursor_pos -= 1;
                self.input.remove(self.cursor_pos);
                self.scroll_to_bottom();
                self.reset_blink();
            }
            KeyEventType::Delete if self.cursor_pos < self.input.len() => {
                self.input.remove(self.cursor_pos);
                self.scroll_to_bottom();
                self.reset_blink();
            }
            KeyEventType::ArrowLeft if self.cursor_pos > 0 => {
                self.cursor_pos -= 1;
                self.scroll_to_bottom();
                self.reset_blink();
            }
            KeyEventType::ArrowRight if self.cursor_pos < self.input.len() => {
                self.cursor_pos += 1;
                self.scroll_to_bottom();
                self.reset_blink();
            }
            KeyEventType::Home => {
                self.cursor_pos = 0;
                self.scroll_to_bottom();
                self.reset_blink();
            }
            KeyEventType::End => {
                self.cursor_pos = self.input.len();
                self.scroll_to_bottom();
                self.reset_blink();
            }
            KeyEventType::ArrowUp => {
                if self.history.is_empty() { return; }
                match self.history_index {
                    None => {
                        self.saved_input = self.input.clone();
                        let idx = self.history.len() - 1;
                        self.history_index = Some(idx);
                        self.input = self.history[idx].clone();
                    }
                    Some(idx) if idx > 0 => {
                        let idx = idx - 1;
                        self.history_index = Some(idx);
                        self.input = self.history[idx].clone();
                    }
                    _ => {}
                }
                self.cursor_pos = self.input.len();
                self.scroll_to_bottom();
                self.reset_blink();
            }
            KeyEventType::ArrowDown => {
                if let Some(idx) = self.history_index {
                    if idx + 1 < self.history.len() {
                        let idx = idx + 1;
                        self.history_index = Some(idx);
                        self.input = self.history[idx].clone();
                    } else {
                        self.history_index = None;
                        self.input = core::mem::take(&mut self.saved_input);
                    }
                    self.cursor_pos = self.input.len();
                    self.scroll_to_bottom();
                    self.reset_blink();
                }
            }
            KeyEventType::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
                self.auto_scroll = false;
            }
            KeyEventType::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
                if self.scroll_offset == 0 {
                    self.auto_scroll = true;
                }
            }
            _ => {}
        }
    }
}

// ── App impl (rendering) ──────────────────────────────────────────────────────

impl App for ShellApp {
    fn update(&mut self, ctx: &egui::Context) {
        if !self.initialized {
            self.initialized = true;
            self.cwd = String::from("/");
            self.push_normal(String::from("Bos Shell v0.1"));
            self.push_normal(String::from("Type 'help' for available commands."));
            self.push_normal(String::new());
            self.last_blink_tick = ulib::sys_get_ticks();
        }

        // Cursor blink
        let now = ulib::sys_get_ticks();
        if now.wrapping_sub(self.last_blink_tick) >= 500 {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink_tick = now;
        }
        bos_egui::request_timed_redraw(500);

        if self.editor.is_some() {
            // Process key events before creating canvas to avoid a blank frame.
            {
                let (_, h) = ctx.screen_size();
                let text_rows = ((h as i32 / LINE_H) as usize).saturating_sub(1);
                if let Some(key) = ctx.key_event() {
                    self.handle_editor_key(key, text_rows);
                }
            }

            CentralPanel::default().show(ctx, |ui| {
                let mut canvas = ui.canvas();
                let cursor_visible = self.cursor_visible;
                self.draw_editor(&mut canvas, cursor_visible);
            });
        } else {
            if let Some(key) = ctx.key_event() {
                self.handle_key(key);
            }

            CentralPanel::default().show(ctx, |ui| {
                let mut canvas = ui.canvas();
                let cols = (canvas.width / 8) as usize;
                let visible_rows = (canvas.height / LINE_H) as usize;
                if cols == 0 || visible_rows == 0 { return; }

                // Build output visual rows
                let mut all_visual: Vec<(&str, Rgb888)> = Vec::new();
                for line in &self.lines {
                    let color = line.color;
                    let text = &line.text;
                    if text.is_empty() {
                        all_visual.push(("", color));
                    } else {
                        let mut pos = 0;
                        while pos < text.len() {
                            let end = (pos + cols).min(text.len());
                            all_visual.push((&text[pos..end], color));
                            pos = end;
                        }
                    }
                }

                // Build live prompt rows
                let prompt_prefix = self.prompt_str();
                let prompt_full = format!("{}{}", prompt_prefix, self.input);
                let prefix_len = prompt_prefix.len();

                let mut prompt_row_starts: Vec<usize> = Vec::new();
                if prompt_full.is_empty() {
                    prompt_row_starts.push(0);
                } else {
                    let mut pos = 0;
                    while pos < prompt_full.len() {
                        prompt_row_starts.push(pos);
                        pos += cols;
                    }
                }
                let num_prompt_rows = prompt_row_starts.len();

                let total = all_visual.len() + num_prompt_rows;
                let view_end = if self.auto_scroll {
                    total
                } else {
                    total.saturating_sub(self.scroll_offset)
                };
                let view_start = view_end.saturating_sub(visible_rows);

                if !self.auto_scroll {
                    let max_offset = total.saturating_sub(1);
                    if self.scroll_offset > max_offset {
                        self.scroll_offset = max_offset;
                    }
                }

                let mut y: i32 = 0;
                for i in view_start..view_end {
                    if i < all_visual.len() {
                        let (text, color) = all_visual[i];
                        canvas.draw_text(text, 0, y, color, &FONT_8X13);
                    } else {
                        let row_idx = i - all_visual.len();
                        let row_start = prompt_row_starts[row_idx];
                        let row_end = (row_start + cols).min(prompt_full.len());
                        let row_text = &prompt_full[row_start..row_end];

                        if row_end <= prefix_len {
                            canvas.draw_text(row_text, 0, y, PROMPT_COLOR, &FONT_8X13_BOLD);
                        } else if row_start >= prefix_len {
                            canvas.draw_text(row_text, 0, y, FG, &FONT_8X13);
                        } else {
                            let split = prefix_len - row_start;
                            canvas.draw_text(&row_text[..split], 0, y, PROMPT_COLOR, &FONT_8X13_BOLD);
                            canvas.draw_text(&row_text[split..], (split as i32) * 8, y, FG, &FONT_8X13);
                        }
                    }
                    y += LINE_H;
                }

                // Cursor
                if self.cursor_visible {
                    let cursor_abs = prefix_len + self.cursor_pos;
                    let cursor_total_row = all_visual.len() + cursor_abs / cols;
                    let cursor_col = cursor_abs % cols;
                    if cursor_total_row >= view_start && cursor_total_row < view_end {
                        let screen_row = (cursor_total_row - view_start) as i32;
                        canvas.draw_text("_", (cursor_col as i32) * 8, screen_row * LINE_H, CURSOR_COLOR, &FONT_8X13);
                    }
                }
            });
        }
    }
}

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, string::String, vec::Vec};
use bos_egui::{egui, App};
use egui::{CentralPanel, Rgb888, FONT_8X13, FONT_8X13_BOLD, KeyEventType};

// ── Constants ────────────────────────────────────────────────────────────────

const LINE_H: i32 = 17;
const MAX_SCROLLBACK: usize = 1000;
const MAX_HISTORY: usize = 64;

const FG: Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);
const PROMPT_COLOR: Rgb888 = Rgb888::new(0x8a, 0xad, 0xf4);
const ERR_COLOR: Rgb888 = Rgb888::new(0xed, 0x87, 0x96);
const DIR_COLOR: Rgb888 = Rgb888::new(0x8b, 0xd5, 0xca);
const DIM_COLOR: Rgb888 = Rgb888::new(0xa5, 0xad, 0xcb);
const CURSOR_COLOR: Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);

// ── Panic / entry ────────────────────────────────────────────────────────────

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    bos_egui::run("shell", ShellApp::new())
}

// ── Shell state ──────────────────────────────────────────────────────────────

struct ShellApp {
    lines: Vec<Line>,
    scroll_offset: usize,
    input: String,
    cursor_pos: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    saved_input: String,
    fs_fd: Option<u32>,
    initialized: bool,
    cursor_visible: bool,
    last_blink_tick: u64,
    cwd: String,
}

#[derive(Clone)]
struct Line {
    text: String,
    color: Rgb888,
}

impl Line {
    fn normal(s: String) -> Self { Self { text: s, color: FG } }
    fn colored(s: String, color: Rgb888) -> Self { Self { text: s, color } }
}

impl ShellApp {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            scroll_offset: 0,
            input: String::new(),
            cursor_pos: 0,
            history: Vec::new(),
            history_index: None,
            saved_input: String::new(),
            fs_fd: None,
            initialized: false,
            cursor_visible: true,
            last_blink_tick: 0,
            cwd: String::new(), // set to "/" in initialized block (allocator not yet ready here)
        }
    }

    /// Resolve `input` against the current working directory.
    fn resolve_path(&self, input: &str) -> String {
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

    fn push_line(&mut self, line: Line) {
        self.lines.push(line);
        if self.lines.len() > MAX_SCROLLBACK {
            let excess = self.lines.len() - MAX_SCROLLBACK;
            self.lines.drain(0..excess);
            self.scroll_offset = self.scroll_offset.saturating_sub(excess);
        }
    }

    fn push_normal(&mut self, s: String) { self.push_line(Line::normal(s)); }
    fn push_err(&mut self, s: String) { self.push_line(Line::colored(s, ERR_COLOR)); }

    fn ensure_fs(&mut self) -> u32 {
        if let Some(fd) = self.fs_fd {
            return fd;
        }
        let fd = ulib::fs::fs_lookup();
        self.fs_fd = Some(fd);
        fd
    }

    // ── Command dispatch ─────────────────────────────────────────────────────

    fn execute(&mut self, cmd_line: &str) {
        self.push_line(Line::colored(format!("bos:{}$ {}", self.cwd, cmd_line), PROMPT_COLOR));

        let parts: Vec<&str> = cmd_line.split_whitespace().collect();
        if parts.is_empty() { return; }

        match parts[0] {
            "help"  => self.cmd_help(),
            "echo"  => self.cmd_echo(&parts[1..]),
            "clear" => self.cmd_clear(),
            "time"  => self.cmd_time(),
            "ls"    => self.cmd_ls(parts.get(1).copied()),
            "cat"   => self.cmd_cat(parts.get(1).copied()),
            "stat"  => self.cmd_stat(parts.get(1).copied()),
            "run"   => self.cmd_run(parts.get(1).copied()),
            "cd"    => self.cmd_cd(parts.get(1).copied()),
            "mkdir" => self.cmd_mkdir(parts.get(1).copied()),
            "touch" => self.cmd_touch(parts.get(1).copied()),
            "rm"    => self.cmd_rm(parts.get(1).copied()),
            "mv"    => self.cmd_mv(parts.get(1).copied(), parts.get(2).copied()),
            other   => self.push_err(format!("unknown command: {}", other)),
        }
    }

    fn cmd_help(&mut self) {
        self.push_normal(String::from("Available commands:"));
        self.push_normal(String::from("  help              Show this help"));
        self.push_normal(String::from("  echo <text>       Print text"));
        self.push_normal(String::from("  clear             Clear screen"));
        self.push_normal(String::from("  time              Show time (seconds since epoch)"));
        self.push_normal(String::from("  ls [path]         List directory contents"));
        self.push_normal(String::from("  cat <path>        Print file contents"));
        self.push_normal(String::from("  stat <path>       Show file metadata"));
        self.push_normal(String::from("  run <path>        Launch an ELF from the filesystem"));
        self.push_normal(String::from("  cd <path>         Change working directory"));
        self.push_normal(String::from("  mkdir <path>      Create a directory"));
        self.push_normal(String::from("  touch <path>      Create an empty file"));
        self.push_normal(String::from("  rm <path>         Delete a file"));
        self.push_normal(String::from("  mv <old> <new>    Rename a file or directory"));
    }

    fn cmd_echo(&mut self, args: &[&str]) {
        let mut out = String::new();
        for (i, a) in args.iter().enumerate() {
            if i > 0 { out.push(' '); }
            out.push_str(a);
        }
        self.push_normal(out);
    }

    fn cmd_clear(&mut self) {
        self.lines.clear();
        self.scroll_offset = 0;
    }

    fn cmd_time(&mut self) {
        let ns = ulib::sys_get_time_ns();
        let secs = ns / 1_000_000_000;
        let h = (secs / 3600) % 24;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        self.push_normal(format!("{:02}:{:02}:{:02} UTC ({}s since epoch)", h, m, s, secs));
    }

    fn cmd_ls(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => self.resolve_path(p),
            None    => self.cwd.clone(),
        };
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_readdir(fs_fd, &path) {
            Some(resp) => {
                for i in 0..(resp.count as usize) {
                    let entry = &resp.entries[i];
                    let name_len = (entry.name_len as usize).min(entry.name.len());
                    let name = core::str::from_utf8(&entry.name[..name_len])
                        .unwrap_or("???");
                    if entry.is_dir != 0 {
                        self.push_line(Line::colored(
                            format!("  <DIR>     {}", name),
                            DIR_COLOR,
                        ));
                    } else {
                        self.push_line(Line::colored(
                            format!("  {:>7}   {}", entry.size, name),
                            DIM_COLOR,
                        ));
                    }
                }
                if resp.count == 0 {
                    self.push_normal(String::from("  (empty)"));
                }
            }
            None => self.push_err(format!("ls: cannot read '{}'", path)),
        };
    }

    fn cmd_cat(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("cat: missing path")); return; }
        };
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_map_file(fs_fd, path) {
            Some((buf_id, file_size)) => {
                let ptr = ulib::sys_map_shared_buf(buf_id);
                if ptr.is_null() {
                    self.push_err(String::from("cat: failed to map file"));
                    ulib::sys_destroy_shared_buf(buf_id);
                    return;
                }
                let cap = (file_size as usize).min(64 * 1024);
                let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, cap) };
                // Push each line
                for line in bytes.split(|&b| b == b'\n') {
                    let s = core::str::from_utf8(line).unwrap_or("(binary data)");
                    // Strip trailing \r
                    let s = s.trim_end_matches('\r');
                    self.push_normal(String::from(s));
                }
                if file_size > 64 * 1024 {
                    self.push_line(Line::colored(
                        format!("... truncated ({} bytes total)", file_size),
                        DIM_COLOR,
                    ));
                }
                ulib::sys_munmap(ptr, file_size);
                ulib::sys_destroy_shared_buf(buf_id);
            }
            None => self.push_err(format!("cat: file not found: '{}'", path)),
        }
    }

    fn cmd_stat(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("stat: missing path")); return; }
        };
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_stat(fs_fd, path) {
            Some(resp) => {
                let kind = if resp.is_dir != 0 { "directory" } else { "file" };
                self.push_normal(format!("  type: {}", kind));
                self.push_normal(format!("  size: {} bytes", resp.size));
            }
            None => self.push_err(format!("stat: not found: '{}'", path)),
        }
    }

    fn cmd_run(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("run: missing ELF path")); return; }
        };
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_map_file(fs_fd, path) {
            Some((buf_id, size)) => {
                let ptr = ulib::sys_map_shared_buf(buf_id);
                if ptr.is_null() {
                    self.push_err(String::from("run: failed to map ELF"));
                    ulib::sys_destroy_shared_buf(buf_id);
                    return;
                }
                let elf = unsafe { core::slice::from_raw_parts(ptr as *const u8, size as usize) };
                let name = path.as_bytes();
                let task_id = ulib::sys_spawn_named(elf, 0, name);
                ulib::sys_munmap(ptr, size);
                ulib::sys_destroy_shared_buf(buf_id);
                if task_id == 0 {
                    self.push_err(format!("run: failed to spawn '{}'", path));
                } else {
                    self.push_normal(format!("spawned '{}' (task {})", path, task_id));
                }
            }
            None => self.push_err(format!("run: file not found: '{}'", path)),
        }
    }

    fn cmd_cd(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.cwd = String::from("/"); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_stat(fs_fd, &resolved) {
            Some(resp) if resp.is_dir != 0 => { self.cwd = resolved; }
            Some(_) => self.push_err(format!("cd: not a directory: '{}'", path)),
            None    => self.push_err(format!("cd: no such directory: '{}'", path)),
        }
    }

    fn cmd_mkdir(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("mkdir: missing path")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_mkdir(fs_fd, &resolved) {
            ulib::fs::FsResult::Ok => {}
            _ => self.push_err(format!("mkdir: failed to create '{}'", path)),
        }
    }

    fn cmd_touch(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("touch: missing path")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_create_file(fs_fd, &resolved) {
            ulib::fs::FsResult::Ok => {}
            _ => self.push_err(format!("touch: failed to create '{}'", path)),
        }
    }

    fn cmd_rm(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("rm: missing path")); return; }
        };
        let resolved = self.resolve_path(path);
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_rm(fs_fd, &resolved) {
            ulib::fs::FsResult::Ok => {}
            ulib::fs::FsResult::NotFound => self.push_err(format!("rm: no such file: '{}'", path)),
            ulib::fs::FsResult::IsDir    => self.push_err(format!("rm: is a directory: '{}'", path)),
            _ => self.push_err(format!("rm: failed to remove '{}'", path)),
        }
    }

    fn cmd_mv(&mut self, old: Option<&str>, new: Option<&str>) {
        let (old, new) = match (old, new) {
            (Some(o), Some(n)) => (o, n),
            _ => { self.push_err(String::from("mv: usage: mv <old> <new>")); return; }
        };
        let resolved_old = self.resolve_path(old);
        // `new` is the bare new name (same directory), not a full path.
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_rename(fs_fd, &resolved_old, new) {
            ulib::fs::FsResult::Ok => {}
            ulib::fs::FsResult::NotFound => self.push_err(format!("mv: no such file: '{}'", old)),
            _ => self.push_err(format!("mv: failed to rename '{}'", old)),
        }
    }

    // ── Input handling ───────────────────────────────────────────────────────

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
                            return;
                        }
                        _ => return,
                    }
                }
                let ch = key.character;
                if (0x20..0x7f).contains(&ch) {
                    self.input.insert(self.cursor_pos, ch as char);
                    self.cursor_pos += 1;
                    self.reset_blink();
                }
            }
            KeyEventType::Enter => {
                let cmd = String::from(self.input.trim());
                self.input.clear();
                self.cursor_pos = 0;
                self.history_index = None;
                if !cmd.is_empty() {
                    // Don't duplicate last history entry
                    if self.history.last().map_or(true, |h| h != &cmd) {
                        self.history.push(cmd.clone());
                        if self.history.len() > MAX_HISTORY {
                            self.history.remove(0);
                        }
                    }
                    self.execute(&cmd);
                }
                self.scroll_offset = 0;
            }
            KeyEventType::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.input.remove(self.cursor_pos);
                    self.reset_blink();
                }
            }
            KeyEventType::Delete => {
                if self.cursor_pos < self.input.len() {
                    self.input.remove(self.cursor_pos);
                    self.reset_blink();
                }
            }
            KeyEventType::ArrowLeft => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.reset_blink();
                }
            }
            KeyEventType::ArrowRight => {
                if self.cursor_pos < self.input.len() {
                    self.cursor_pos += 1;
                    self.reset_blink();
                }
            }
            KeyEventType::Home => {
                self.cursor_pos = 0;
                self.reset_blink();
            }
            KeyEventType::End => {
                self.cursor_pos = self.input.len();
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
                self.reset_blink();
            }
            KeyEventType::ArrowDown => {
                match self.history_index {
                    Some(idx) => {
                        if idx + 1 < self.history.len() {
                            let idx = idx + 1;
                            self.history_index = Some(idx);
                            self.input = self.history[idx].clone();
                        } else {
                            self.history_index = None;
                            self.input = core::mem::take(&mut self.saved_input);
                        }
                        self.cursor_pos = self.input.len();
                        self.reset_blink();
                    }
                    None => {}
                }
            }
            KeyEventType::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
                let max = self.lines.len();
                if self.scroll_offset > max { self.scroll_offset = max; }
            }
            KeyEventType::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
            }
            _ => {}
        }
    }

    fn reset_blink(&mut self) {
        self.cursor_visible = true;
        self.last_blink_tick = ulib::sys_get_ticks();
    }

}

impl App for ShellApp {
    fn update(&mut self, ctx: &egui::Context) {
        if !self.initialized {
            self.initialized = true;
            self.cwd = String::from("/"); // first allocation — heap is ready at this point
            self.push_normal(String::from("Bos Shell v0.1"));
            self.push_normal(String::from("Type 'help' for available commands."));
            self.push_normal(String::new());
            self.last_blink_tick = ulib::sys_get_ticks();
        }

        // Handle key input
        if let Some(key) = ctx.key_event() {
            self.handle_key(key);
        }

        // Cursor blink: toggle every ~500ms
        let now = ulib::sys_get_ticks();
        if now.wrapping_sub(self.last_blink_tick) >= 500 {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink_tick = now;
        }
        bos_egui::request_timed_redraw(500);

        // Render
        CentralPanel::default().show(ctx, |ui| {
            let mut canvas = ui.canvas();
            let cols = (canvas.width / 8) as usize;
            let visible_rows = (canvas.height / LINE_H) as usize;
            if cols == 0 || visible_rows < 2 { return; }

            // Reserve bottom row for prompt
            let output_rows = visible_rows - 1;

            // Flatten all lines into visual rows (wrapping long lines).
            // O(n) but scrollback is capped at 1000 lines — fine.
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

            let total = all_visual.len();
            let max_scroll = total.saturating_sub(output_rows);
            let scroll = self.scroll_offset.min(max_scroll);

            let visible_start = if total > output_rows + scroll {
                total - output_rows - scroll
            } else {
                0
            };
            let visible_end = if total > scroll {
                total - scroll
            } else {
                0
            };

            let mut y: i32 = 0;
            for i in visible_start..visible_end {
                let (text, color) = all_visual[i];
                canvas.draw_text(text, 0, y, color, &FONT_8X13);
                y += LINE_H;
            }

            // Draw prompt line at the bottom
            let prompt_y = (visible_rows as i32 - 1) * LINE_H;
            let prompt_str = format!("bos:{}$ ", self.cwd);
            canvas.draw_text(&prompt_str, 0, prompt_y, PROMPT_COLOR, &FONT_8X13_BOLD);

            let prompt_offset = (prompt_str.len() as i32) * 8;
            // Render input text, possibly scrolled if wider than the window
            let input_cols = cols.saturating_sub(prompt_str.len());
            let display_input = if self.input.len() > input_cols {
                // Show the tail of input around cursor
                let start = self.cursor_pos.saturating_sub(input_cols / 2);
                let start = start.min(self.input.len().saturating_sub(input_cols));
                &self.input[start..start + input_cols.min(self.input.len() - start)]
            } else {
                &self.input
            };
            canvas.draw_text(display_input, prompt_offset, prompt_y, FG, &FONT_8X13);

            // Draw cursor
            if self.cursor_visible {
                let cursor_display_pos = if self.input.len() > input_cols {
                    let start = self.cursor_pos.saturating_sub(input_cols / 2);
                    let start = start.min(self.input.len().saturating_sub(input_cols));
                    self.cursor_pos - start
                } else {
                    self.cursor_pos
                };
                let cx = prompt_offset + (cursor_display_pos as i32) * 8;
                canvas.draw_text("_", cx, prompt_y, CURSOR_COLOR, &FONT_8X13);
            }
        });
    }
}

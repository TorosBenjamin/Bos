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

// Editor-specific colors
const STATUS_BG: Rgb888 = Rgb888::new(0x36, 0x3a, 0x4f);
const DIRTY_COLOR: Rgb888 = Rgb888::new(0xed, 0x87, 0x96);

// ── Panic / entry ────────────────────────────────────────────────────────────

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    bos_egui::run("shell", ShellApp::new())
}

// ── Editor state ─────────────────────────────────────────────────────────────

struct EditorMode {
    path: String,
    lines: Vec<String>,      // file content; always at least one entry
    cursor_row: usize,
    cursor_col: usize,
    view_top: usize,         // first visible line index
    dirty: bool,
    quit_requested: bool,    // first Esc/^Q when dirty; second confirms quit
    status_msg: String,      // transient status message
    status_tick: u64,        // tick when status_msg was set (show for ~2s)
}

// ── Shell state ──────────────────────────────────────────────────────────────

struct ShellApp {
    lines: Vec<Line>,
    scroll_offset: usize,
    auto_scroll: bool,
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
    editor: Option<EditorMode>,
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

    fn prompt_str(&self) -> String {
        format!("bos:{}$ ", self.cwd)
    }

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
            if self.scroll_offset > excess {
                self.scroll_offset -= excess;
            } else {
                self.scroll_offset = 0;
            }
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
        self.push_line(Line::colored(format!("{}{}", self.prompt_str(), cmd_line), PROMPT_COLOR));

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
            "edit"  => self.cmd_edit(parts.get(1).copied()),
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
        self.push_normal(String::from("  edit <path>       Open text editor"));
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
        self.auto_scroll = true;
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
                    let name = core::str::from_utf8(&entry.name[..name_len]).unwrap_or("???");
                    if entry.is_dir != 0 {
                        self.push_line(Line::colored(format!("  <DIR>     {}", name), DIR_COLOR));
                    } else {
                        self.push_line(Line::colored(format!("  {:>7}   {}", entry.size, name), DIM_COLOR));
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
                for line in bytes.split(|&b| b == b'\n') {
                    let s = core::str::from_utf8(line).unwrap_or("(binary data)");
                    self.push_normal(String::from(s.trim_end_matches('\r')));
                }
                if file_size > 64 * 1024 {
                    self.push_line(Line::colored(
                        format!("... truncated ({} bytes total)", file_size), DIM_COLOR,
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
                let task_id = ulib::sys_spawn_named(elf, 0, path.as_bytes());
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
        let fs_fd = self.ensure_fs();
        match ulib::fs::fs_rename(fs_fd, &resolved_old, new) {
            ulib::fs::FsResult::Ok => {}
            ulib::fs::FsResult::NotFound => self.push_err(format!("mv: no such file: '{}'", old)),
            _ => self.push_err(format!("mv: failed to rename '{}'", old)),
        }
    }

    fn cmd_edit(&mut self, path: Option<&str>) {
        let path = match path {
            Some(p) => p,
            None => { self.push_err(String::from("edit: usage: edit <path>")); return; }
        };
        let path = self.resolve_path(path);
        let fs_fd = self.ensure_fs();

        let lines = match ulib::fs::fs_map_file(fs_fd, &path) {
            Some((buf_id, size)) => {
                let ptr = ulib::sys_map_shared_buf(buf_id);
                if ptr.is_null() {
                    ulib::sys_destroy_shared_buf(buf_id);
                    self.push_err(format!("edit: failed to map '{}'", path));
                    return;
                }
                let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, size as usize) };
                let mut lines: Vec<String> = bytes
                    .split(|&b| b == b'\n')
                    .map(|l| String::from(core::str::from_utf8(l).unwrap_or("").trim_end_matches('\r')))
                    .collect();
                if lines.is_empty() { lines.push(String::new()); }
                ulib::sys_munmap(ptr, size);
                ulib::sys_destroy_shared_buf(buf_id);
                lines
            }
            // New file — open with one empty line
            None => { let mut v = Vec::new(); v.push(String::new()); v }
        };

        self.editor = Some(EditorMode {
            path,
            lines,
            cursor_row: 0,
            cursor_col: 0,
            view_top: 0,
            dirty: false,
            quit_requested: false,
            status_msg: String::new(),
            status_tick: 0,
        });
    }

    // ── Editor: save ─────────────────────────────────────────────────────────

    fn editor_save(&mut self) {
        let fs_fd = self.ensure_fs();
        let ed = match self.editor.as_mut() { Some(e) => e, None => return };

        // Calculate total byte size: all lines joined with '\n'
        let mut total = 0usize;
        for (i, line) in ed.lines.iter().enumerate() {
            total += line.len();
            if i + 1 < ed.lines.len() { total += 1; } // '\n'
        }

        let (buf_id, ptr) = ulib::sys_create_shared_buf(total.max(1) as u64);
        if ptr.is_null() || buf_id == u64::MAX {
            let ed = self.editor.as_mut().unwrap();
            ed.status_msg = String::from("Error: out of memory.");
            ed.status_tick = ulib::sys_get_ticks();
            return;
        }

        // Serialize lines into the shared buffer
        let mut off = 0usize;
        for (i, line) in ed.lines.iter().enumerate() {
            let b = line.as_bytes();
            unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), ptr.add(off), b.len()); }
            off += b.len();
            if i + 1 < ed.lines.len() {
                unsafe { *ptr.add(off) = b'\n'; }
                off += 1;
            }
        }

        let path = ed.path.clone();
        let result = ulib::fs::fs_write_file(fs_fd, &path, buf_id, total as u64);
        ulib::sys_destroy_shared_buf(buf_id);

        let ed = self.editor.as_mut().unwrap();
        ed.status_tick = ulib::sys_get_ticks();
        if result == ulib::fs::FsResult::Ok {
            ed.dirty = false;
            ed.status_msg = String::from("Saved.");
        } else {
            ed.status_msg = String::from("Error: save failed.");
        }
    }

    // ── Editor: key handling ──────────────────────────────────────────────────

    fn handle_editor_key(&mut self, key: kernel_api_types::KeyEvent, text_rows: usize) {
        if !key.pressed { return; }
        let ctrl = key.modifiers & kernel_api_types::KEY_MOD_CTRL != 0;

        let ed = match self.editor.as_mut() { Some(e) => e, None => return };

        match key.event_type {
            KeyEventType::Char => {
                if ctrl {
                    match key.character {
                        b's' | b'S' => {
                            let _ = ed; // release borrow so editor_save can take &mut self
                            self.editor_save();
                            return;
                        }
                        b'q' | b'Q' => {
                            let ed = self.editor.as_mut().unwrap();
                            if ed.dirty && !ed.quit_requested {
                                ed.quit_requested = true;
                                ed.status_msg = String::from("Unsaved! ^Q/Esc again to quit.");
                                ed.status_tick = ulib::sys_get_ticks();
                            } else {
                                self.editor = None;
                            }
                            return;
                        }
                        b'h' | b'e' => {} // Ctrl+H = backspace handled below, ignore others
                        _ => return,
                    }
                }
                let ch = key.character;
                if (0x20..0x7f).contains(&ch) {
                    let ed = self.editor.as_mut().unwrap();
                    ed.lines[ed.cursor_row].insert(ed.cursor_col, ch as char);
                    ed.cursor_col += 1;
                    ed.dirty = true;
                    ed.quit_requested = false;
                }
            }
            KeyEventType::Tab => {
                ed.lines[ed.cursor_row].insert_str(ed.cursor_col, "    ");
                ed.cursor_col += 4;
                ed.dirty = true;
                ed.quit_requested = false;
            }
            KeyEventType::Enter => {
                let rest = String::from(&ed.lines[ed.cursor_row][ed.cursor_col..]);
                ed.lines[ed.cursor_row].truncate(ed.cursor_col);
                ed.cursor_row += 1;
                ed.lines.insert(ed.cursor_row, rest);
                ed.cursor_col = 0;
                ed.dirty = true;
                ed.quit_requested = false;
            }
            KeyEventType::Backspace => {
                ed.quit_requested = false;
                if ed.cursor_col > 0 {
                    ed.cursor_col -= 1;
                    ed.lines[ed.cursor_row].remove(ed.cursor_col);
                    ed.dirty = true;
                } else if ed.cursor_row > 0 {
                    let cur_line = ed.lines.remove(ed.cursor_row);
                    ed.cursor_row -= 1;
                    ed.cursor_col = ed.lines[ed.cursor_row].len();
                    ed.lines[ed.cursor_row].push_str(&cur_line);
                    ed.dirty = true;
                }
            }
            KeyEventType::Delete => {
                ed.quit_requested = false;
                let line_len = ed.lines[ed.cursor_row].len();
                if ed.cursor_col < line_len {
                    ed.lines[ed.cursor_row].remove(ed.cursor_col);
                    ed.dirty = true;
                } else if ed.cursor_row + 1 < ed.lines.len() {
                    let next = ed.lines.remove(ed.cursor_row + 1);
                    ed.lines[ed.cursor_row].push_str(&next);
                    ed.dirty = true;
                }
            }
            KeyEventType::ArrowLeft => {
                ed.quit_requested = false;
                if ed.cursor_col > 0 {
                    ed.cursor_col -= 1;
                } else if ed.cursor_row > 0 {
                    ed.cursor_row -= 1;
                    ed.cursor_col = ed.lines[ed.cursor_row].len();
                }
            }
            KeyEventType::ArrowRight => {
                ed.quit_requested = false;
                let line_len = ed.lines[ed.cursor_row].len();
                if ed.cursor_col < line_len {
                    ed.cursor_col += 1;
                } else if ed.cursor_row + 1 < ed.lines.len() {
                    ed.cursor_row += 1;
                    ed.cursor_col = 0;
                }
            }
            KeyEventType::ArrowUp => {
                ed.quit_requested = false;
                if ed.cursor_row > 0 {
                    ed.cursor_row -= 1;
                    ed.cursor_col = ed.cursor_col.min(ed.lines[ed.cursor_row].len());
                }
            }
            KeyEventType::ArrowDown => {
                ed.quit_requested = false;
                if ed.cursor_row + 1 < ed.lines.len() {
                    ed.cursor_row += 1;
                    ed.cursor_col = ed.cursor_col.min(ed.lines[ed.cursor_row].len());
                }
            }
            KeyEventType::Home => {
                ed.quit_requested = false;
                ed.cursor_col = 0;
            }
            KeyEventType::End => {
                ed.quit_requested = false;
                ed.cursor_col = ed.lines[ed.cursor_row].len();
            }
            KeyEventType::PageUp => {
                ed.quit_requested = false;
                let step = text_rows.saturating_sub(1).max(1);
                ed.view_top = ed.view_top.saturating_sub(step);
                ed.cursor_row = ed.cursor_row.saturating_sub(step).max(ed.view_top);
                ed.cursor_col = ed.cursor_col.min(ed.lines[ed.cursor_row].len());
            }
            KeyEventType::PageDown => {
                ed.quit_requested = false;
                let step = text_rows.saturating_sub(1).max(1);
                let last_line = ed.lines.len().saturating_sub(1);
                ed.view_top = (ed.view_top + step).min(last_line);
                ed.cursor_row = (ed.cursor_row + step).min(last_line);
                ed.cursor_col = ed.cursor_col.min(ed.lines[ed.cursor_row].len());
            }
            KeyEventType::Escape => {
                if ed.dirty && !ed.quit_requested {
                    ed.quit_requested = true;
                    ed.status_msg = String::from("Unsaved! Esc/^Q again to quit.");
                    ed.status_tick = ulib::sys_get_ticks();
                } else {
                    self.editor = None;
                }
                return;
            }
            _ => {}
        }

        // Scroll viewport to keep cursor visible
        if let Some(ed) = self.editor.as_mut() {
            if ed.cursor_row < ed.view_top {
                ed.view_top = ed.cursor_row;
            }
            if text_rows > 0 && ed.cursor_row >= ed.view_top + text_rows {
                ed.view_top = ed.cursor_row + 1 - text_rows;
            }
        }
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
                    if self.history.last().map_or(true, |h| h != &cmd) {
                        self.history.push(cmd.clone());
                        if self.history.len() > MAX_HISTORY {
                            self.history.remove(0);
                        }
                    }
                    self.execute(&cmd);
                }
            }
            KeyEventType::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.input.remove(self.cursor_pos);
                    self.scroll_to_bottom();
                    self.reset_blink();
                }
            }
            KeyEventType::Delete => {
                if self.cursor_pos < self.input.len() {
                    self.input.remove(self.cursor_pos);
                    self.scroll_to_bottom();
                    self.reset_blink();
                }
            }
            KeyEventType::ArrowLeft => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.scroll_to_bottom();
                    self.reset_blink();
                }
            }
            KeyEventType::ArrowRight => {
                if self.cursor_pos < self.input.len() {
                    self.cursor_pos += 1;
                    self.scroll_to_bottom();
                    self.reset_blink();
                }
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
                        self.scroll_to_bottom();
                        self.reset_blink();
                    }
                    None => {}
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

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
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
            self.cwd = String::from("/");
            self.push_normal(String::from("Bos Shell v0.1"));
            self.push_normal(String::from("Type 'help' for available commands."));
            self.push_normal(String::new());
            self.last_blink_tick = ulib::sys_get_ticks();
        }

        // Cursor blink (shared between shell and editor)
        let now = ulib::sys_get_ticks();
        if now.wrapping_sub(self.last_blink_tick) >= 500 {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink_tick = now;
        }
        bos_egui::request_timed_redraw(500);

        if self.editor.is_some() {
            // Process key events before creating canvas — same pattern as shell mode.
            // This avoids a blank frame: handling inside the closure would require
            // drop(canvas)+return which leaves the frame empty.
            {
                let (_, h) = ctx.screen_size();
                let text_rows = ((h as i32 / LINE_H) as usize).saturating_sub(1);
                if let Some(key) = ctx.key_event() {
                    self.handle_editor_key(key, text_rows);
                }
            }

            // ── Editor mode ───────────────────────────────────────────────────
            CentralPanel::default().show(ctx, |ui| {
                let mut canvas = ui.canvas();
                let cols = (canvas.width / 8) as usize;
                let visible_rows = (canvas.height / LINE_H) as usize;
                if cols == 0 || visible_rows == 0 { return; }

                let text_rows = visible_rows.saturating_sub(1); // last row = status bar
                let status_y = (visible_rows as i32 - 1) * LINE_H;

                let ed = match self.editor.as_ref() { Some(e) => e, None => return };

                // ── Text area ─────────────────────────────────────────────────
                let view_end = (ed.view_top + text_rows).min(ed.lines.len());
                for (row_idx, line) in ed.lines[ed.view_top..view_end].iter().enumerate() {
                    let y = (row_idx as i32) * LINE_H;
                    let visible_len = cols.min(line.len());
                    canvas.draw_text(&line[..visible_len], 0, y, FG, &FONT_8X13);
                }

                // ── Cursor ────────────────────────────────────────────────────
                if self.cursor_visible {
                    if let Some(ed) = self.editor.as_ref() {
                        if ed.cursor_row >= ed.view_top && ed.cursor_row < ed.view_top + text_rows {
                            let screen_row = (ed.cursor_row - ed.view_top) as i32;
                            let cx = (ed.cursor_col as i32) * 8;
                            let cy = screen_row * LINE_H;
                            canvas.draw_text("_", cx, cy, CURSOR_COLOR, &FONT_8X13);
                        }
                    }
                }

                // ── Status bar ────────────────────────────────────────────────
                canvas.fill_rect(0, status_y, canvas.width, LINE_H, STATUS_BG);

                let ed = match self.editor.as_ref() { Some(e) => e, None => return };

                // Show transient status message for up to 2000ms
                let elapsed = ulib::sys_get_ticks().wrapping_sub(ed.status_tick);
                if !ed.status_msg.is_empty() && elapsed < 2000 {
                    canvas.draw_text(&ed.status_msg, 8, status_y, PROMPT_COLOR, &FONT_8X13_BOLD);
                } else {
                    // Left: dirty indicator + filename
                    if ed.dirty {
                        canvas.draw_text("[+] ", 8, status_y, DIRTY_COLOR, &FONT_8X13_BOLD);
                        canvas.draw_text(&ed.path, 8 + 4 * 8, status_y, PROMPT_COLOR, &FONT_8X13_BOLD);
                    } else {
                        canvas.draw_text(&ed.path, 8, status_y, PROMPT_COLOR, &FONT_8X13_BOLD);
                    }
                    // Right: position + hints
                    let total_lines = ed.lines.len();
                    let right_text = format!(
                        "Ln {}/{}  Col {}  | ^S:Save  Esc:Quit",
                        ed.cursor_row + 1, total_lines, ed.cursor_col + 1
                    );
                    let right_x = (canvas.width - (right_text.len() as i32) * 8).max(0);
                    canvas.draw_text(&right_text, right_x, status_y, DIM_COLOR, &FONT_8X13);
                }
            });
        } else {
            // ── Shell mode ────────────────────────────────────────────────────
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

                // Render rows
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

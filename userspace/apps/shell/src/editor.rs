use alloc::string::String;
use alloc::vec::Vec;
use bos_egui::egui::KeyEventType;

use crate::{ShellApp, LINE_H, PROMPT_COLOR, DIRTY_COLOR, DIM_COLOR, FG, CURSOR_COLOR, STATUS_BG};

// ── Editor state ──────────────────────────────────────────────────────────────

pub struct EditorMode {
    pub path:           String,
    pub lines:          Vec<String>, // file content; always at least one entry
    pub cursor_row:     usize,
    pub cursor_col:     usize,
    pub view_top:       usize,       // first visible line index
    pub dirty:          bool,
    pub quit_requested: bool,        // first Esc/^Q when dirty; second confirms quit
    pub status_msg:     String,      // transient status message
    pub status_tick:    u64,         // tick when status_msg was set (show for ~2s)
}

// ── Editor methods on ShellApp ────────────────────────────────────────────────

impl ShellApp {
    pub(crate) fn cmd_edit(&mut self, path: Option<&str>) {
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
                    self.push_err(alloc::format!("edit: failed to map '{}'", path));
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
            None => alloc::vec![String::new()],
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

    pub(crate) fn editor_save(&mut self) {
        let fs_fd = self.ensure_fs();
        let ed = match self.editor.as_mut() { Some(e) => e, None => return };

        let mut total = 0usize;
        for (i, line) in ed.lines.iter().enumerate() {
            total += line.len();
            if i + 1 < ed.lines.len() { total += 1; }
        }

        let (buf_id, ptr) = ulib::sys_create_shared_buf(total.max(1) as u64);
        if ptr.is_null() || buf_id == u64::MAX {
            let ed = self.editor.as_mut().unwrap();
            ed.status_msg = String::from("Error: out of memory.");
            ed.status_tick = ulib::sys_get_ticks();
            return;
        }

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

    pub(crate) fn handle_editor_key(&mut self, key: kernel_api_types::KeyEvent, text_rows: usize) {
        if !key.pressed { return; }
        let ctrl = key.modifiers & kernel_api_types::KEY_MOD_CTRL != 0;

        let ed = match self.editor.as_mut() { Some(e) => e, None => return };

        match key.event_type {
            KeyEventType::Char => {
                if ctrl {
                    match key.character {
                        b's' | b'S' => {
                            let _ = ed;
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
                        b'h' | b'e' => {}
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

    pub(crate) fn draw_editor(&self, canvas: &mut bos_egui::egui::Canvas, cursor_visible: bool) {
        let ed = match self.editor.as_ref() { Some(e) => e, None => return };
        let cols = (canvas.width / 8) as usize;
        let visible_rows = (canvas.height / LINE_H) as usize;
        if cols == 0 || visible_rows == 0 { return; }

        let text_rows = visible_rows.saturating_sub(1);
        let status_y = (visible_rows as i32 - 1) * LINE_H;

        // Text area
        let view_end = (ed.view_top + text_rows).min(ed.lines.len());
        for (row_idx, line) in ed.lines[ed.view_top..view_end].iter().enumerate() {
            let y = (row_idx as i32) * LINE_H;
            let visible_len = cols.min(line.len());
            canvas.draw_text(&line[..visible_len], 0, y, FG, &bos_egui::egui::FONT_8X13);
        }

        // Cursor
        if cursor_visible && ed.cursor_row >= ed.view_top && ed.cursor_row < ed.view_top + text_rows {
            let screen_row = (ed.cursor_row - ed.view_top) as i32;
            let cx = (ed.cursor_col as i32) * 8;
            let cy = screen_row * LINE_H;
            canvas.draw_text("_", cx, cy, CURSOR_COLOR, &bos_egui::egui::FONT_8X13);
        }

        // Status bar
        canvas.fill_rect(0, status_y, canvas.width, LINE_H, STATUS_BG);

        let elapsed = ulib::sys_get_ticks().wrapping_sub(ed.status_tick);
        if !ed.status_msg.is_empty() && elapsed < 2000 {
            canvas.draw_text(&ed.status_msg, 8, status_y, PROMPT_COLOR, &bos_egui::egui::FONT_8X13_BOLD);
        } else {
            if ed.dirty {
                canvas.draw_text("[+] ", 8, status_y, DIRTY_COLOR, &bos_egui::egui::FONT_8X13_BOLD);
                canvas.draw_text(&ed.path, 8 + 4 * 8, status_y, PROMPT_COLOR, &bos_egui::egui::FONT_8X13_BOLD);
            } else {
                canvas.draw_text(&ed.path, 8, status_y, PROMPT_COLOR, &bos_egui::egui::FONT_8X13_BOLD);
            }
            let total_lines = ed.lines.len();
            let right_text = alloc::format!(
                "Ln {}/{}  Col {}  | ^S:Save  Esc:Quit",
                ed.cursor_row + 1, total_lines, ed.cursor_col + 1
            );
            let right_x = (canvas.width - (right_text.len() as i32) * 8).max(0);
            canvas.draw_text(&right_text, right_x, status_y, DIM_COLOR, &bos_egui::egui::FONT_8X13);
        }
    }
}

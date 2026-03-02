use crate::compositor_config::{DisplayConfig, WindowMode, WindowRule};
use crate::cursor::{CURSOR_H, CURSOR_W};
use crate::window::Window;
use kernel_api_types::window::*;
use kernel_api_types::{IPC_OK, MMAP_WRITE, MOUSE_LEFT, MOUSE_RIGHT, MOUSE_MIDDLE};

mod layout;
mod render;
mod handlers;

pub const MAX_WINDOWS: usize = 32;
const MAX_MSG_SIZE: usize = 4096;

pub struct Compositor {
    display: ulib::display::Display,
    display_info: kernel_api_types::graphics::DisplayInfo,
    windows: [Option<Window>; MAX_WINDOWS],
    next_window_id: WindowId,
    recv_endpoint: u64,
    /// z_order[0] = bottom-most, z_order[n_windows-1] = top-most
    z_order: [WindowId; MAX_WINDOWS],
    n_windows: usize,
    /// Pre-rendered gradient background (width × height pixels, native fb format)
    background_buf: *mut u32,
    /// Current cursor position (hot spot, clamped to screen)
    cursor_x: i32,
    cursor_y: i32,
    /// Cursor colours pre-built in native framebuffer pixel format
    cursor_black: u32,
    cursor_white: u32,
    /// Damage accumulated this loop iteration from mouse/window-move events.
    /// Window content updates are tracked per-window via Window::pending_dirty instead,
    /// to avoid merging two distant windows into a single huge bounding box.
    pending_damage: Option<DirtyRect>,
    /// True when a full redraw is needed (window add/remove/reorder)
    pending_full_redraw: bool,
    /// Currently focused window (receives keyboard events)
    focused_window: Option<WindowId>,
    /// Mouse button state from the previous frame
    prev_mouse_buttons: u8,
    /// Border colour for the focused window (native fb pixel format)
    border_focused: u32,
    /// Border colour for unfocused windows (native fb pixel format)
    border_unfocused: u32,
    /// Layout configuration (loaded from /HYPR.CONF)
    pub(super) outer_gap: u32,
    pub(super) inner_gap: u32,
    pub(super) border_width: i32,
    /// Window placement rules loaded from /HYPR.CONF [window_rules]
    window_rules:   [Option<WindowRule>; 16],
    n_window_rules: usize,
}

impl Compositor {
    pub fn new(recv_endpoint: u64, config: DisplayConfig) -> Self {
        let display = ulib::display::Display::new();
        let display_info = ulib::sys_get_display_info();

        const NONE_WINDOW: Option<Window> = None;

        let width = display_info.width as usize;
        let height = display_info.height as usize;
        let screen_pixels = width * height;

        // Pre-render gradient background using config colors
        let bg_bytes = (screen_pixels * 4) as u64;
        let background_buf = ulib::sys_mmap(bg_bytes, MMAP_WRITE) as *mut u32;

        if !background_buf.is_null() {
            let (tr, tg, tb) = config.bg_top;
            let (br, bg, bb) = config.bg_bottom;
            for y in 0..height {
                let t = if height > 1 { y * 255 / (height - 1) } else { 0 } as u32;
                let r = (tr as u32 * (255 - t) + br as u32 * t) / 255;
                let g = (tg as u32 * (255 - t) + bg as u32 * t) / 255;
                let b = (tb as u32 * (255 - t) + bb as u32 * t) / 255;
                let pixel = display_info.build_pixel(r as u8, g as u8, b as u8);
                unsafe {
                    for x in 0..width {
                        *background_buf.add(y * width + x) = pixel;
                    }
                }
            }
        }

        let cursor_black = display_info.build_pixel(0, 0, 0);
        let cursor_white = display_info.build_pixel(255, 255, 255);
        let (fr, fg, fb) = config.border_focused;
        let (ur, ug, ub) = config.border_unfocused;
        let border_focused   = display_info.build_pixel(fr, fg, fb);
        let border_unfocused = display_info.build_pixel(ur, ug, ub);
        let window_rules   = config.window_rules;
        let n_window_rules = config.n_window_rules;

        Compositor {
            display,
            display_info,
            windows: [NONE_WINDOW; MAX_WINDOWS],
            next_window_id: 1,
            recv_endpoint,
            z_order: [0; MAX_WINDOWS],
            n_windows: 0,
            background_buf,
            cursor_x: display_info.width as i32 / 2,
            cursor_y: display_info.height as i32 / 2,
            cursor_black,
            cursor_white,
            pending_damage: None,
            pending_full_redraw: false,
            focused_window: None,
            prev_mouse_buttons: 0,
            border_focused,
            border_unfocused,
            outer_gap: config.outer_gap,
            inner_gap: config.inner_gap,
            border_width: config.border_size,
            window_rules,
            n_window_rules,
        }
    }

    fn resolve_floating(&self, app_id: &[u8], flags: u32, parent_id: u64) -> bool {
        // Config rule has highest priority
        for i in 0..self.n_window_rules {
            if let Some(ref r) = self.window_rules[i] {
                if &r.app_id[..r.app_id_len as usize] == app_id {
                    return r.mode == WindowMode::Floating;
                }
            }
        }
        // Dialog parent → always float
        if parent_id != 0 { return true; }
        // Client flag
        flags & kernel_api_types::window::WINDOW_FLAG_FLOATING != 0
    }

    // --- Z-order helpers ---

    fn z_push(&mut self, id: WindowId) {
        if self.n_windows < MAX_WINDOWS {
            self.z_order[self.n_windows] = id;
            self.n_windows += 1;
        }
    }

    fn z_remove(&mut self, id: WindowId) {
        if let Some(pos) = self.z_order[..self.n_windows].iter().position(|&x| x == id) {
            for i in pos..self.n_windows - 1 {
                self.z_order[i] = self.z_order[i + 1];
            }
            self.n_windows -= 1;
        }
    }

    /// Push a Toplevel window above other Toplevels but below all Panels.
    fn z_push_toplevel(&mut self, id: WindowId) {
        // Find the index of the first panel in z_order (panels live above toplevels)
        let panel_start = (0..self.n_windows).find(|&i| {
            let zid = self.z_order[i];
            self.windows.iter()
                .filter_map(|w| w.as_ref())
                .find(|w| w.id == zid)
                .map(|w| w.is_panel)
                .unwrap_or(false)
        });

        match panel_start {
            Some(pos) => {
                if self.n_windows < MAX_WINDOWS {
                    for i in (pos..self.n_windows).rev() {
                        self.z_order[i + 1] = self.z_order[i];
                    }
                    self.z_order[pos] = id;
                    self.n_windows += 1;
                }
            }
            None => self.z_push(id),
        }
    }

    fn z_raise(&mut self, id: WindowId) {
        let is_panel = self.windows.iter()
            .filter_map(|w| w.as_ref())
            .find(|w| w.id == id)
            .map(|w| w.is_panel)
            .unwrap_or(false);

        self.z_remove(id);
        if is_panel {
            self.z_push(id);
        } else {
            self.z_push_toplevel(id);
        }
    }

    fn z_lower(&mut self, id: WindowId) {
        self.z_remove(id);
        if self.n_windows < MAX_WINDOWS {
            for i in (0..self.n_windows).rev() {
                self.z_order[i + 1] = self.z_order[i];
            }
            self.z_order[0] = id;
            self.n_windows += 1;
        }
    }

    // --- Damage / pending state helpers ---

    fn screen_rect(&self, x: i32, y: i32, w: u32, h: u32) -> Option<DirtyRect> {
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = ((x + w as i32).max(0) as u32).min(self.display_info.width);
        let y1 = ((y + h as i32).max(0) as u32).min(self.display_info.height);
        if x0 < x1 && y0 < y1 {
            Some(DirtyRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
        } else {
            None
        }
    }

    fn cursor_rect(&self) -> Option<DirtyRect> {
        self.screen_rect(self.cursor_x, self.cursor_y, CURSOR_W, CURSOR_H)
    }

    fn expand_pending(&mut self, rect: DirtyRect) {
        match &mut self.pending_damage {
            Some(d) => d.expand(rect.x, rect.y, rect.w, rect.h),
            None => self.pending_damage = Some(rect),
        }
    }

    fn mark_full_redraw(&mut self) {
        self.pending_full_redraw = true;
        self.pending_damage = None;
    }

    fn mark_damage(&mut self, rect: DirtyRect) {
        self.expand_pending(rect);
    }

    // --- Focus management ---

    /// Hit-test (x, y) against Toplevels in z-order (top-to-bottom). Panels are skipped.
    fn hit_test(&self, x: i32, y: i32) -> Option<WindowId> {
        for i in (0..self.n_windows).rev() {
            let id = self.z_order[i];
            if let Some(w) = self.windows.iter().filter_map(|w| w.as_ref()).find(|w| w.id == id) {
                if w.is_panel {
                    continue;
                }
                if x >= w.x && x < w.x + w.width as i32
                    && y >= w.y && y < w.y + w.height as i32
                {
                    return Some(id);
                }
            }
        }
        None
    }

    fn set_focus(&mut self, new_id: Option<WindowId>) {
        if self.focused_window == new_id {
            return;
        }

        let old_id = self.focused_window;
        self.focused_window = new_id;

        if let Some(old_id) = old_id {
            let ep = self.window_event_ep(old_id);
            if ep != 0 {
                ulib::sys_try_channel_send(ep, &[WindowEventType::FocusLost as u8]);
            }
        }

        if let Some(new_id) = new_id {
            let ep = self.window_event_ep(new_id);
            if ep != 0 {
                ulib::sys_try_channel_send(ep, &[WindowEventType::FocusGained as u8]);
            }
        }

        // Border colours change on focus change — rebuild the scene.
        self.mark_full_redraw();
    }

    fn window_event_ep(&self, id: WindowId) -> u64 {
        self.windows.iter()
            .filter_map(|w| w.as_ref())
            .find(|w| w.id == id)
            .map(|w| w.event_send_ep)
            .unwrap_or(0)
    }

    // --- Main loop ---

    pub fn run(&mut self) -> ! {
        let msg_buf = ulib::sys_mmap(MAX_MSG_SIZE as u64, MMAP_WRITE);
        if msg_buf.is_null() {
            loop { ulib::sys_yield(); }
        }

        // Initial full composite
        self.mark_full_redraw();

        loop {
            // Drain all pending IPC messages (non-blocking) before compositing.
            loop {
                let msg_slice = unsafe { core::slice::from_raw_parts_mut(msg_buf, MAX_MSG_SIZE) };
                let (result, bytes_read) = ulib::sys_try_channel_recv(self.recv_endpoint, msg_slice);
                if result != IPC_OK || bytes_read == 0 {
                    break;
                }
                let msg = unsafe { core::slice::from_raw_parts(msg_buf, bytes_read as usize) };
                self.process_message(msg);
            }

            // Drain all pending mouse events; accumulate into a single cursor move.
            let mut total_dx = 0i32;
            let mut total_dy = 0i32;
            let mut cur_buttons = self.prev_mouse_buttons;
            while let Some(ev) = ulib::sys_read_mouse() {
                total_dx += ev.dx as i32;
                total_dy += ev.dy as i32;
                cur_buttons = ev.buttons;
            }

            if total_dx != 0 || total_dy != 0 {
                let old_rect = self.cursor_rect();
                self.cursor_x = (self.cursor_x + total_dx)
                    .clamp(0, self.display_info.width as i32 - 1);
                self.cursor_y = (self.cursor_y + total_dy)
                    .clamp(0, self.display_info.height as i32 - 1);
                let new_rect = self.cursor_rect();

                let cursor_damage = match (old_rect, new_rect) {
                    (Some(mut a), Some(b)) => { a.expand(b.x, b.y, b.w, b.h); Some(a) }
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                };
                if let Some(cd) = cursor_damage {
                    self.expand_pending(cd);
                }

            // Notify the focused window of the new cursor position (window-relative).
            if let Some(fw_id) = self.focused_window {
                let info = self.windows.iter()
                    .filter_map(|w| w.as_ref())
                    .find(|w| w.id == fw_id)
                    .map(|w| (w.x, w.y, w.event_send_ep));
                if let Some((wx, wy, ep)) = info {
                    if ep != 0 {
                        send_event(ep, &MouseMoveEvent {
                            event_type: WindowEventType::MouseMove as u8,
                            _pad: [0; 3],
                            x: self.cursor_x - wx,
                            y: self.cursor_y - wy,
                        });
                    }
                }
            }
            }

            // Click-to-focus: left button just pressed
            let just_pressed  = cur_buttons & !self.prev_mouse_buttons;
            let just_released = self.prev_mouse_buttons & !cur_buttons;
            if just_pressed & MOUSE_LEFT != 0 {
                let hit = self.hit_test(self.cursor_x, self.cursor_y);
                if let Some(id) = hit {
                    self.z_raise(id);
                    self.mark_full_redraw();
                }
                self.set_focus(hit);
            }
            self.prev_mouse_buttons = cur_buttons;

            // Route mouse button events to the focused window
            if (just_pressed | just_released) != 0 {
                if let Some(fw_id) = self.focused_window {
                    let pos = self.windows.iter()
                        .filter_map(|w| w.as_ref())
                        .find(|w| w.id == fw_id)
                        .map(|w| (w.x, w.y));
                    if let Some((wx, wy)) = pos {
                        let ep = self.window_event_ep(fw_id);
                        if ep != 0 {
                            for &bit in &[MOUSE_LEFT, MOUSE_RIGHT, MOUSE_MIDDLE] {
                                if just_pressed & bit != 0 {
                                    send_event(ep, &MouseButtonEvent {
                                        event_type: WindowEventType::MouseButtonPress as u8,
                                        button: bit,
                                        _pad: [0; 2],
                                        x: self.cursor_x - wx,
                                        y: self.cursor_y - wy,
                                    });
                                }
                                if just_released & bit != 0 {
                                    send_event(ep, &MouseButtonEvent {
                                        event_type: WindowEventType::MouseButtonRelease as u8,
                                        button: bit,
                                        _pad: [0; 2],
                                        x: self.cursor_x - wx,
                                        y: self.cursor_y - wy,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Route keyboard events to the focused window
            while let Some(key) = ulib::sys_try_read_key() {
                if let Some(fw_id) = self.focused_window {
                    let ep = self.window_event_ep(fw_id);
                    if ep != 0 {
                        let ev = KeyPressEvent {
                            event_type: WindowEventType::KeyPress as u8,
                            key,
                        };
                        send_event(ep, &ev);
                    }
                }
            }

            // Single composite for everything accumulated this iteration.
            self.flush();

            ulib::sys_wait_for_event(
                &[self.recv_endpoint],
                ulib::WAIT_MOUSE | ulib::WAIT_KEYBOARD,
                0,
            );
        }
    }
}

/// Send an event struct to a persistent event channel (does NOT close the endpoint).
/// Non-blocking: if the channel is full, the event is dropped rather than blocking
/// the compositor. This prevents any blocking calls in the DS hot path.
fn send_event<T>(ep: u64, event: &T) {
    let bytes = unsafe {
        core::slice::from_raw_parts(
            event as *const T as *const u8,
            core::mem::size_of::<T>(),
        )
    };
    ulib::sys_try_channel_send(ep, bytes);
}

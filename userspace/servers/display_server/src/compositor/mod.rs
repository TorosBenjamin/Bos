use crate::compositor_config::{
    DisplayConfig, WindowMode, WindowRule,
    ShortcutBinding, MAX_SHORTCUTS,
};
use crate::cursor::{CURSOR_H, CURSOR_W};
use crate::window::Window;
use kernel_api_types::window::*;
use kernel_api_types::MMAP_WRITE;
use layout::LayoutDir;

mod layout;
mod render;
mod handlers;
mod focus;
mod drag;
mod event_loop;

pub const MAX_WINDOWS: usize = 32;
const MAX_MSG_SIZE: usize = 4096;
pub(super) const CLOSE_MAX_ATTEMPTS: u32 = 20;   // 20 × 100 ms ≈ 2-second timeout
const _CLOSE_POLL_TIMEOUT_MS: u64 = 100;

const MIN_SIZE: u32 = 100;
pub(super) const MIN_RATIO: f32 = 0.1;

#[derive(Clone, Copy)]
enum DragKind {
    MoveFloating { start_x: i32, start_y: i32 },
    MoveTiled,
    ResizeFloating {
        start_x: i32, start_y: i32,
        start_w: u32, start_h: u32,
        resize_left: bool, resize_top: bool,
    },
    ResizeTiled {
        tiled_index: usize,
        start_ratios: [f32; MAX_WINDOWS],
        n_tiled: usize,
    },
}

#[derive(Clone, Copy)]
struct DragState {
    window_id: WindowId,
    kind: DragKind,
    start_cx: i32,
    start_cy: i32,
}

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
    /// Layout configuration (loaded from /bos_ds.conf)
    pub(super) outer_gap: u32,
    pub(super) inner_gap: u32,
    pub(super) border_width: i32,
    /// Window placement rules from /bos_ds.conf [window_rules]
    window_rules:   [Option<WindowRule>; 16],
    n_window_rules: usize,
    /// Opacity for inactive (unfocused) tiled windows: 0–255. 255 = no dimming.
    inactive_opacity: u8,
    /// Opacity for inactive (unfocused) floating windows: 0–255. 255 = no dimming.
    inactive_opacity_floating: u8,
    /// Keyboard shortcuts from /bos_ds.conf [shortcuts]
    shortcuts:   [Option<ShortcutBinding>; MAX_SHORTCUTS],
    n_shortcuts: usize,
    /// App IDs that cannot be killed by the CloseWindow shortcut.
    protected:      [[u8; 32]; 16],
    protected_lens: [u8; 16],
    n_protected:    usize,
    /// Previous focus before the launcher was shown (restored on hide).
    launcher_prev_focus: Option<WindowId>,
    /// Tiling layout direction (changed via Super+drag to screen edge)
    layout_dir: LayoutDir,
    /// Per-window split ratios for tiled layout (sum ≈ 1.0)
    tiled_ratios: [f32; MAX_WINDOWS],
    /// Number of valid entries in tiled_ratios
    n_tiled_ratios: usize,
    /// Active Super+drag operation, if any
    drag_state: Option<DragState>,
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
            inactive_opacity: config.inactive_opacity,
            inactive_opacity_floating: config.inactive_opacity_floating,
            shortcuts:   config.shortcuts,
            n_shortcuts: config.n_shortcuts,
            protected:      config.protected,
            protected_lens: config.protected_lens,
            n_protected:    config.n_protected,
            launcher_prev_focus: None,
            layout_dir: LayoutDir::Horizontal,
            tiled_ratios: [0.0f32; MAX_WINDOWS],
            n_tiled_ratios: 0,
            drag_state: None,
        }
    }

    fn is_protected(&self, id: WindowId) -> bool {
        let w = match self.windows.iter().filter_map(|w| w.as_ref()).find(|w| w.id == id) {
            Some(w) => w,
            None    => return false,
        };
        let app_id = &w.app_id[..w.app_id_len as usize];
        for i in 0..self.n_protected {
            if &self.protected[i][..self.protected_lens[i] as usize] == app_id {
                return true;
            }
        }
        false
    }

    fn resolve_floating(&self, app_id: &[u8], flags: u32, parent_id: u64) -> bool {
        // Config rule has highest priority
        for i in 0..self.n_window_rules {
            if let Some(ref r) = self.window_rules[i] && &r.app_id[..r.app_id_len as usize] == app_id {
                return r.mode == WindowMode::Floating;
            }
        }
        // Dialog parent → always float
        if parent_id != 0 { return true; }
        // Client flag
        flags & kernel_api_types::window::WINDOW_FLAG_FLOATING != 0
    }

    // --- Hide / show helpers ---

    /// Find a window by its app_id string. Returns the WindowId if found.
    fn find_by_app_id(&self, id: &[u8]) -> Option<WindowId> {
        for slot in &self.windows {
            if let Some(w) = slot.as_ref() && &w.app_id[..w.app_id_len as usize] == id {
                return Some(w.id);
            }
        }
        None
    }

    /// Hide a window: remove from z-order without closing it.
    pub(super) fn hide_window(&mut self, id: WindowId) {
        if let Some(w) = self.windows.iter_mut().filter_map(|w| w.as_mut()).find(|w| w.id == id) {
            w.hidden = true;
        }
        self.z_remove(id);
        if self.focused_window == Some(id) {
            self.focused_window = None;
        }
        self.pending_full_redraw = true;
    }

    /// Show a previously hidden window: add it back to the z-order.
    pub(super) fn show_window(&mut self, id: WindowId) {
        if let Some(w) = self.windows.iter_mut().filter_map(|w| w.as_mut()).find(|w| w.id == id) {
            w.hidden = false;
        }
        self.z_push_toplevel(id);
        self.pending_full_redraw = true;
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

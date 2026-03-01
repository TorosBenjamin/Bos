use crate::cursor::{CURSOR_H, CURSOR_IMAGE, CURSOR_MASK, CURSOR_W};
use crate::window::Window;
use kernel_api_types::window::*;
use kernel_api_types::{IPC_OK, MMAP_WRITE, MOUSE_LEFT};

pub const MAX_WINDOWS: usize = 32;
const MAX_MSG_SIZE: usize = 4096;

/// Pixels between a window edge and the screen / available-area boundary.
const OUTER_GAP: u32 = 8;
/// Pixels between two adjacent tiled windows.
const INNER_GAP: u32 = 8;
/// Width of the colored border drawn around each Toplevel window (in the gap area).
const BORDER_WIDTH: i32 = 2;

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
    /// Off-screen composite: background + all windows blended, no cursor.
    scene_buf: *mut u32,
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
}

impl Compositor {
    pub fn new(recv_endpoint: u64) -> Self {
        let display = ulib::display::Display::new();
        let display_info = ulib::sys_get_display_info();

        const NONE_WINDOW: Option<Window> = None;

        let width = display_info.width as usize;
        let height = display_info.height as usize;
        let screen_pixels = width * height;

        // Pre-render gradient background
        let bg_bytes = (screen_pixels * 4) as u64;
        let background_buf = ulib::sys_mmap(bg_bytes, MMAP_WRITE) as *mut u32;

        if !background_buf.is_null() {
            for y in 0..height {
                let t = if height > 1 { y * 255 / (height - 1) } else { 0 } as u32;
                let r = (0x1eu32 * (255 - t) + 0x0au32 * t) / 255;
                let g = (0x3au32 * (255 - t) + 0x0au32 * t) / 255;
                let b = (0x5fu32 * (255 - t) + 0x0fu32 * t) / 255;
                let pixel = display_info.build_pixel(r as u8, g as u8, b as u8);
                unsafe {
                    for x in 0..width {
                        *background_buf.add(y * width + x) = pixel;
                    }
                }
            }
        }

        // Allocate scene buffer (same size as framebuffer); initialise with background.
        let scene_buf = ulib::sys_mmap(bg_bytes, MMAP_WRITE) as *mut u32;
        if !scene_buf.is_null() && !background_buf.is_null() {
            unsafe { core::ptr::copy_nonoverlapping(background_buf, scene_buf, screen_pixels) };
        }

        let cursor_black = display_info.build_pixel(0, 0, 0);
        let cursor_white = display_info.build_pixel(255, 255, 255);
        // Catppuccin Macchiato: blue #8aadf4 for focused, surface0 #363a4f for unfocused
        let border_focused   = display_info.build_pixel(138, 173, 244);
        let border_unfocused = display_info.build_pixel(54,  58,  79);

        Compositor {
            display,
            display_info,
            windows: [NONE_WINDOW; MAX_WINDOWS],
            next_window_id: 1,
            recv_endpoint,
            z_order: [0; MAX_WINDOWS],
            n_windows: 0,
            background_buf,
            scene_buf,
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
        }
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

    // --- Tiling layout ---

    fn count_toplevels(&self) -> usize {
        self.windows.iter()
            .filter_map(|w| w.as_ref())
            .filter(|w| !w.is_panel)
            .count()
    }

    /// Returns `(x, y, w, h)` — the screen area available to Toplevels after panels claim their edges.
    fn available_area(&self) -> (i32, i32, u32, u32) {
        let mut ax = 0i32;
        let mut ay = 0i32;
        let mut aw = self.display_info.width;
        let mut ah = self.display_info.height;

        for slot in &self.windows {
            if let Some(w) = slot {
                if !w.is_panel {
                    continue;
                }
                match w.anchor {
                    0 => { // Top
                        let zone = w.exclusive_zone.min(ah);
                        ay += zone as i32;
                        ah -= zone;
                    }
                    1 => { // Bottom
                        ah -= w.exclusive_zone.min(ah);
                    }
                    2 => { // Left
                        let zone = w.exclusive_zone.min(aw);
                        ax += zone as i32;
                        aw -= zone;
                    }
                    3 => { // Right
                        aw -= w.exclusive_zone.min(aw);
                    }
                    _ => {}
                }
            }
        }

        (ax, ay, aw, ah)
    }

    /// Redistribute horizontal tile space among all Toplevels. Sends Configure events to
    /// any window whose buffer dimensions change.
    fn recalculate_toplevel_layout(&mut self) {
        let n = self.count_toplevels();
        if n == 0 {
            return;
        }

        let (ax, ay, aw, ah) = self.available_area();
        // Distribute gaps: OUTER_GAP on every side, INNER_GAP between adjacent windows.
        let total_h_gaps = 2 * OUTER_GAP + (n as u32 - 1) * INNER_GAP;
        let usable_w = aw.saturating_sub(total_h_gaps);
        let usable_h = ah.saturating_sub(2 * OUTER_GAP);
        let tile_w = usable_w / n as u32;

        // Collect (event_send_ep, ConfigureEvent) for windows that need a new buffer.
        // Use a fixed-size array since we're no_std.
        let mut pending: [(u64, ConfigureEvent); MAX_WINDOWS] = [(0, ConfigureEvent {
            event_type: WindowEventType::Configure as u8,
            _pad: [0; 3],
            width: 0,
            height: 0,
            shared_buf_id: 0,
        }); MAX_WINDOWS];
        let mut n_pending = 0usize;

        let mut i = 0usize;
        for slot in &mut self.windows {
            if let Some(window) = slot {
                if window.is_panel {
                    continue;
                }
                let new_x = ax + OUTER_GAP as i32 + (i as u32 * (tile_w + INNER_GAP)) as i32;
                let new_y = ay + OUTER_GAP as i32;
                // Last toplevel gets any remaining pixels so rounding doesn't leave a sliver
                let new_w = if i == n - 1 { usable_w - tile_w * i as u32 } else { tile_w };
                let new_h = usable_h;

                let reconfigured = window.reconfigure(new_x, new_y, new_w, new_h);
                if reconfigured && n_pending < MAX_WINDOWS {
                    pending[n_pending] = (
                        window.event_send_ep,
                        ConfigureEvent {
                            event_type: WindowEventType::Configure as u8,
                            _pad: [0; 3],
                            width: new_w,
                            height: new_h,
                            shared_buf_id: window.shared_buf_id,
                        },
                    );
                    n_pending += 1;
                }
                i += 1;
            }
        }

        for j in 0..n_pending {
            let (ep, ref ev) = pending[j];
            send_event(ep, ev);
        }

        self.mark_full_redraw();
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

    // --- Border rendering ---

    /// Fill a rectangle in `scene_buf`, clipped to `clip` (or unconstrained if None).
    fn fill_scene_rect_clipped(
        &mut self,
        x: i32, y: i32, w: u32, h: u32,
        clip: Option<DirtyRect>,
        color: u32,
    ) {
        if self.scene_buf.is_null() || w == 0 || h == 0 {
            return;
        }
        let screen_w = self.display_info.width as usize;
        let screen_h = self.display_info.height as usize;

        let mut x0 = x.max(0) as usize;
        let mut y0 = y.max(0) as usize;
        let mut x1 = ((x + w as i32).max(0) as usize).min(screen_w);
        let mut y1 = ((y + h as i32).max(0) as usize).min(screen_h);

        if let Some(c) = clip {
            x0 = x0.max(c.x as usize);
            y0 = y0.max(c.y as usize);
            x1 = x1.min((c.x + c.w) as usize);
            y1 = y1.min((c.y + c.h) as usize);
        }

        if x0 >= x1 || y0 >= y1 {
            return;
        }

        unsafe {
            for row in y0..y1 {
                let row_ptr = self.scene_buf.add(row * screen_w + x0);
                core::slice::from_raw_parts_mut(row_ptr, x1 - x0).fill(color);
            }
        }
    }

    /// Draw a `BORDER_WIDTH`-pixel border around every non-panel Toplevel.
    /// `clip` limits writes to within a damage rect (pass `None` for full-scene draws).
    fn draw_borders(&mut self, clip: Option<DirtyRect>) {
        let focused = self.focused_window;
        let focused_color   = self.border_focused;
        let unfocused_color = self.border_unfocused;

        // Collect window geometry to avoid borrow conflict with fill_scene_rect_clipped.
        let mut infos: [(i32, i32, u32, u32, u32); MAX_WINDOWS] =
            [(0, 0, 0, 0, 0); MAX_WINDOWS];
        let mut n = 0usize;
        for slot in &self.windows {
            if let Some(w) = slot {
                if w.is_panel {
                    continue;
                }
                let color = if focused == Some(w.id) { focused_color } else { unfocused_color };
                infos[n] = (w.x, w.y, w.width, w.height, color);
                n += 1;
            }
        }

        for i in 0..n {
            let (wx, wy, ww, wh, color) = infos[i];
            let bw = BORDER_WIDTH;
            let bwu = bw as u32;
            // Top strip
            self.fill_scene_rect_clipped(wx - bw, wy - bw, ww + 2 * bwu, bwu, clip, color);
            // Bottom strip
            self.fill_scene_rect_clipped(wx - bw, wy + wh as i32, ww + 2 * bwu, bwu, clip, color);
            // Left strip (side only, corners already covered by top/bottom)
            self.fill_scene_rect_clipped(wx - bw, wy, bwu, wh, clip, color);
            // Right strip
            self.fill_scene_rect_clipped(wx + ww as i32, wy, bwu, wh, clip, color);
        }
    }

    // --- Scene buffer compositing ---

    /// Blit `w×h` pixels from `src` into `scene_buf` at `(dst_x, dst_y)`,
    /// clipped to both screen bounds and the optional `clip` rect.
    fn blit_to_scene(
        &mut self,
        src: *const u32,
        src_width: u32,
        dst_x: i32,
        dst_y: i32,
        w: u32,
        h: u32,
        clip: Option<DirtyRect>,
    ) {
        if self.scene_buf.is_null() {
            return;
        }
        let screen_w = self.display_info.width as usize;
        let screen_h = self.display_info.height as usize;

        let mut x0 = dst_x.max(0) as usize;
        let mut y0 = dst_y.max(0) as usize;
        let mut x1 = ((dst_x + w as i32).max(0) as usize).min(screen_w);
        let mut y1 = ((dst_y + h as i32).max(0) as usize).min(screen_h);

        // Further clip to damage rect so we only touch the pixels that need compositing.
        if let Some(c) = clip {
            x0 = x0.max(c.x as usize);
            y0 = y0.max(c.y as usize);
            x1 = x1.min((c.x + c.w) as usize);
            y1 = y1.min((c.y + c.h) as usize);
        }

        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let clipped_w = x1 - x0;
        let src_x_off = (x0 as i32 - dst_x).max(0) as usize;
        let src_y_off = (y0 as i32 - dst_y).max(0) as usize;

        for row in 0..(y1 - y0) {
            let src_off = (src_y_off + row) * src_width as usize + src_x_off;
            let dst_off = (y0 + row) * screen_w + x0;
            unsafe {
                core::ptr::copy_nonoverlapping(src.add(src_off), self.scene_buf.add(dst_off), clipped_w);
            }
        }
    }

    fn update_scene_region(&mut self, damage: DirtyRect) {
        if !self.background_buf.is_null() {
            let screen_w = self.display_info.width;
            let src = unsafe {
                self.background_buf.add(damage.y as usize * screen_w as usize + damage.x as usize)
            };
            self.blit_to_scene(src, screen_w, damage.x as i32, damage.y as i32, damage.w, damage.h, None);
        }

        for i in 0..self.n_windows {
            let id = self.z_order[i];
            let info = self.windows.iter()
                .filter_map(|w| w.as_ref())
                .find(|w| w.id == id)
                .map(|w| (w.x, w.y, w.width, w.height, w.buffer as *const u32));

            if let Some((wx, wy, ww, wh, wbuf)) = info {
                let wx1 = wx + ww as i32;
                let wy1 = wy + wh as i32;
                let dx1 = damage.x as i32 + damage.w as i32;
                let dy1 = damage.y as i32 + damage.h as i32;
                if wx >= dx1 || wx1 <= damage.x as i32 || wy >= dy1 || wy1 <= damage.y as i32 {
                    continue;
                }
                // Clip the window blit to the damage rect — only touch pixels that changed.
                self.blit_to_scene(wbuf, ww, wx, wy, ww, wh, Some(damage));
            }
        }

        // Redraw borders clipped to the damage rect so they're never erased by background blits.
        self.draw_borders(Some(damage));
    }

    fn update_scene_full(&mut self) {
        if self.scene_buf.is_null() {
            return;
        }
        if !self.background_buf.is_null() {
            let n = self.display_info.width as usize * self.display_info.height as usize;
            unsafe { core::ptr::copy_nonoverlapping(self.background_buf, self.scene_buf, n) };
        }
        for i in 0..self.n_windows {
            let id = self.z_order[i];
            let info = self.windows.iter()
                .filter_map(|w| w.as_ref())
                .find(|w| w.id == id)
                .map(|w| (w.x, w.y, w.width, w.height, w.buffer as *const u32));

            if let Some((wx, wy, ww, wh, wbuf)) = info {
                self.blit_to_scene(wbuf, ww, wx, wy, ww, wh, None);
            }
        }
        // Draw borders on top of all window content.
        self.draw_borders(None);
    }

    fn flush(&mut self) {
        if self.pending_full_redraw {
            self.pending_full_redraw = false;
            self.pending_damage = None;
            // Clear per-window dirty rects subsumed by the full redraw.
            for slot in &mut self.windows {
                if let Some(w) = slot { w.pending_dirty = None; }
            }
            self.update_scene_full();
            let sw = self.display_info.width;
            let sh = self.display_info.height;
            if !self.scene_buf.is_null() {
                self.display.blit_raw(self.scene_buf, sw, 0, 0, sw, sh);
            }
            self.display.blit_cursor(
                self.cursor_x, self.cursor_y,
                &CURSOR_MASK, &CURSOR_IMAGE,
                CURSOR_W, CURSOR_H,
                self.cursor_black, self.cursor_white,
            );
            self.display.present();
            // Full redraw: every window's content is now on screen.
            for slot in &self.windows {
                if let Some(w) = slot {
                    ulib::sys_try_channel_send(
                        w.event_send_ep,
                        &[WindowEventType::FramePresented as u8],
                    );
                }
            }
            return;
        }

        // Collect each window's independent dirty rect without holding a borrow on self.windows.
        // Keeping them separate prevents the bounding-box explosion caused by two windows at
        // opposite screen corners merging into a rect that covers the entire display.
        let mut dirty_rects: [Option<DirtyRect>; MAX_WINDOWS] = [None; MAX_WINDOWS];
        for (i, slot) in self.windows.iter_mut().enumerate() {
            if let Some(w) = slot {
                dirty_rects[i] = w.pending_dirty.take();
            }
        }
        // Extra damage from cursor movement and explicit window moves.
        let extra_damage = self.pending_damage.take();

        let has_window = dirty_rects.iter().any(|d| d.is_some());
        if !has_window && extra_damage.is_none() {
            return;
        }

        let screen_w = self.display_info.width;

        // Update the scene buffer and blit each region to the back buffer independently.
        for opt_dr in &dirty_rects {
            if let Some(dr) = opt_dr {
                self.update_scene_region(*dr);
                if !self.scene_buf.is_null() {
                    let src = unsafe {
                        self.scene_buf.add(dr.y as usize * screen_w as usize + dr.x as usize)
                    };
                    self.display.blit_raw(src, screen_w, dr.x as i32, dr.y as i32, dr.w, dr.h);
                }
            }
        }

        if let Some(cd) = extra_damage {
            self.update_scene_region(cd);
            if !self.scene_buf.is_null() {
                let src = unsafe {
                    self.scene_buf.add(cd.y as usize * screen_w as usize + cd.x as usize)
                };
                self.display.blit_raw(src, screen_w, cd.x as i32, cd.y as i32, cd.w, cd.h);
            }
        }

        // Draw cursor on top of the composited back buffer (once, after all scene updates),
        // then present every accumulated dirty rect to VRAM independently via Display::present().
        // Because Display now tracks a list of dirty rects instead of a single bounding box,
        // each small rect is written to VRAM separately — no cross-window merging.
        self.display.blit_cursor(
            self.cursor_x, self.cursor_y,
            &CURSOR_MASK, &CURSOR_IMAGE,
            CURSOR_W, CURSOR_H,
            self.cursor_black, self.cursor_white,
        );
        self.display.present();

        // Notify each window whose pixels were composited this frame.
        for (i, opt_dr) in dirty_rects.iter().enumerate() {
            if opt_dr.is_some() {
                if let Some(w) = &self.windows[i] {
                    ulib::sys_try_channel_send(
                        w.event_send_ep,
                        &[WindowEventType::FramePresented as u8],
                    );
                }
            }
        }
    }

    // --- Window handlers ---

    fn handle_create_toplevel(&mut self, req: &CreateWindowRequest, reply_ep: u64) {
        let slot_idx = match self.windows.iter().position(|w| w.is_none()) {
            Some(i) => i,
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                    shared_buf_id: 0,
                    width: 0,
                    height: 0,
                });
                return;
            }
        };

        let window_id = self.next_window_id;
        self.next_window_id += 1;

        // Compute this window's initial tile position (it will be the n_current-th toplevel).
        let n_current = self.count_toplevels();
        let (ax, ay, aw, ah) = self.available_area();
        let n_new = n_current + 1;
        let total_h_gaps = 2 * OUTER_GAP + n_current as u32 * INNER_GAP; // (n_new-1) inner gaps
        let usable_w = aw.saturating_sub(total_h_gaps);
        let usable_h = ah.saturating_sub(2 * OUTER_GAP);
        let tile_w = usable_w / n_new as u32;
        let new_x = ax + OUTER_GAP as i32 + (n_current as u32 * (tile_w + INNER_GAP)) as i32;
        // Last window gets remainder so rounding doesn't leave a sliver
        let init_w = usable_w - n_current as u32 * tile_w;
        let init_h = usable_h;

        match Window::new(window_id, new_x, ay + OUTER_GAP as i32, init_w, init_h, req.event_send_ep) {
            Some(window) => {
                let shared_buf_id = window.shared_buf_id;
                self.windows[slot_idx] = Some(window);
                self.z_push_toplevel(window_id);

                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::Ok,
                    window_id,
                    shared_buf_id,
                    width: init_w,
                    height: init_h,
                });

                // Focus the new window
                self.set_focus(Some(window_id));

                // Recalculate layout: redistributes existing toplevels (new window already
                // has the correct size, so its reconfigure() will return false).
                self.recalculate_toplevel_layout();
            }
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                    shared_buf_id: 0,
                    width: 0,
                    height: 0,
                });
            }
        }
    }

    fn handle_create_panel(&mut self, req: &CreatePanelRequest, reply_ep: u64) {
        if req.width == 0 || req.height == 0 || req.width > 4096 || req.height > 4096 {
            self.send_response(reply_ep, &CreateWindowResponse {
                result: WindowResult::ErrorInvalidDimensions,
                window_id: 0,
                shared_buf_id: 0,
                width: 0,
                height: 0,
            });
            return;
        }

        let slot_idx = match self.windows.iter().position(|w| w.is_none()) {
            Some(i) => i,
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                    shared_buf_id: 0,
                    width: 0,
                    height: 0,
                });
                return;
            }
        };

        let window_id = self.next_window_id;
        self.next_window_id += 1;

        let sw = self.display_info.width;
        let sh = self.display_info.height;
        let (px, py, pw, ph) = match req.anchor {
            0 => (0i32, 0i32, sw, req.height),                          // Top
            1 => (0i32, sh as i32 - req.height as i32, sw, req.height), // Bottom
            2 => (0i32, 0i32, req.width, sh),                           // Left
            3 => (sw as i32 - req.width as i32, 0i32, req.width, sh),   // Right
            _ => (0i32, 0i32, req.width, req.height),
        };

        match Window::new(window_id, px, py, pw, ph, req.event_send_ep) {
            Some(mut window) => {
                window.is_panel = true;
                window.anchor = req.anchor;
                window.exclusive_zone = req.exclusive_zone;
                let shared_buf_id = window.shared_buf_id;
                self.windows[slot_idx] = Some(window);
                // Panels live at the top of z_order
                self.z_push(window_id);

                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::Ok,
                    window_id,
                    shared_buf_id,
                    width: pw,
                    height: ph,
                });

                // Reflow toplevels into the reduced available area
                self.recalculate_toplevel_layout();
            }
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                    shared_buf_id: 0,
                    width: 0,
                    height: 0,
                });
            }
        }
    }

    fn handle_update_window(&mut self, header: &UpdateWindowRequest) {
        let damage = {
            let window = match self.windows.iter_mut()
                .filter_map(|w| w.as_mut())
                .find(|w| w.id == header.window_id)
            {
                Some(w) => w,
                None => return,
            };
            if header.dirty_x + header.dirty_width > window.width
                || header.dirty_y + header.dirty_height > window.height
            {
                return;
            }

            // Clean up old buffer if client has acknowledged a Configure event
            if let Some(old_id) = window.pending_old_buf_id.take() {
                ulib::sys_destroy_shared_buf(old_id);
            }

            // Translate the client's window-local dirty rect to screen coordinates.
            (
                window.x + header.dirty_x as i32,
                window.y + header.dirty_y as i32,
                header.dirty_width,
                header.dirty_height,
            )
        };

        if let Some(rect) = self.screen_rect(damage.0, damage.1, damage.2, damage.3) {
            // Store in the window's own pending_dirty, NOT the global pending_damage.
            // This prevents two windows at opposite screen corners from merging their
            // tiny dirty rects into a huge bounding box that covers the entire screen.
            if let Some(window) = self.windows.iter_mut()
                .filter_map(|w| w.as_mut())
                .find(|w| w.id == header.window_id)
            {
                match &mut window.pending_dirty {
                    Some(d) => d.expand(rect.x, rect.y, rect.w, rect.h),
                    None => window.pending_dirty = Some(rect),
                }
            }
        }
    }

    fn handle_close_window(&mut self, req: &CloseWindowRequest) {
        for slot in self.windows.iter_mut() {
            if let Some(window) = slot {
                if window.id == req.window_id {
                    ulib::sys_munmap(window.buffer as *mut u8, window.buf_size);
                    let id = window.id;
                    let shared_buf_id = window.shared_buf_id;
                    let event_ep = window.event_send_ep;
                    let pending_old = window.pending_old_buf_id.take();
                    *slot = None;
                    self.z_remove(id);
                    ulib::sys_destroy_shared_buf(shared_buf_id);
                    if let Some(old_id) = pending_old {
                        ulib::sys_destroy_shared_buf(old_id);
                    }
                    // Close event channel to signal client
                    if event_ep != 0 {
                        ulib::sys_channel_close(event_ep);
                    }
                    // Update focus
                    if self.focused_window == Some(id) {
                        // Focus the topmost remaining toplevel, if any
                        let new_focus = self.topmost_toplevel_id();
                        self.focused_window = None; // prevent set_focus from sending FocusLost to dead window
                        self.set_focus(new_focus);
                    }
                    self.mark_full_redraw();
                    // Redistribute space among remaining toplevels
                    self.recalculate_toplevel_layout();
                    return;
                }
            }
        }
    }

    fn topmost_toplevel_id(&self) -> Option<WindowId> {
        for i in (0..self.n_windows).rev() {
            let id = self.z_order[i];
            if let Some(w) = self.windows.iter().filter_map(|w| w.as_ref()).find(|w| w.id == id) {
                if !w.is_panel {
                    return Some(id);
                }
            }
        }
        None
    }

    fn handle_move_window(&mut self, req: &MoveWindowRequest) {
        let old_pos = self.windows.iter_mut()
            .filter_map(|w| w.as_mut())
            .find(|w| w.id == req.window_id)
            .map(|window| {
                let old = (window.x, window.y, window.width, window.height);
                window.x = req.x;
                window.y = req.y;
                old
            });

        if let Some((ox, oy, w, h)) = old_pos {
            let mut damage = self.screen_rect(ox, oy, w, h);
            if let Some(new_rect) = self.screen_rect(req.x, req.y, w, h) {
                match &mut damage {
                    Some(d) => d.expand(new_rect.x, new_rect.y, new_rect.w, new_rect.h),
                    None => damage = Some(new_rect),
                }
            }
            if let Some(d) = damage {
                self.mark_damage(d);
            }
        }
    }

    fn handle_raise_window(&mut self, req: &RaiseWindowRequest) {
        self.z_raise(req.window_id);
        self.mark_full_redraw();
    }

    fn handle_lower_window(&mut self, req: &LowerWindowRequest) {
        self.z_lower(req.window_id);
        self.mark_full_redraw();
    }

    fn send_response<T>(&self, reply_ep: u64, response: &T) {
        let bytes = unsafe {
            core::slice::from_raw_parts(
                response as *const T as *const u8,
                core::mem::size_of::<T>(),
            )
        };
        ulib::sys_channel_send(reply_ep, bytes);
        ulib::sys_channel_close(reply_ep);
    }

    fn process_message(&mut self, msg: &[u8]) {
        if msg.is_empty() {
            return;
        }

        let msg_type = msg[0];

        match msg_type {
            t if t == WindowMessageType::CreateWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<CreateWindowRequest>() + 8 {
                    return;
                }
                let req: CreateWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const CreateWindowRequest)
                };
                let ep_off = 1 + core::mem::size_of::<CreateWindowRequest>();
                let reply_ep = u64::from_le_bytes(msg[ep_off..ep_off + 8].try_into().unwrap_or([0; 8]));
                self.handle_create_toplevel(&req, reply_ep);
            }
            t if t == WindowMessageType::CreatePanel as u8 => {
                if msg.len() < 1 + core::mem::size_of::<CreatePanelRequest>() + 8 {
                    return;
                }
                let req: CreatePanelRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const CreatePanelRequest)
                };
                let ep_off = 1 + core::mem::size_of::<CreatePanelRequest>();
                let reply_ep = u64::from_le_bytes(msg[ep_off..ep_off + 8].try_into().unwrap_or([0; 8]));
                self.handle_create_panel(&req, reply_ep);
            }
            t if t == WindowMessageType::UpdateWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<UpdateWindowRequest>() {
                    return;
                }
                let header: UpdateWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const UpdateWindowRequest)
                };
                self.handle_update_window(&header);
            }
            t if t == WindowMessageType::CloseWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<CloseWindowRequest>() {
                    return;
                }
                let req: CloseWindowRequest = unsafe {
                    core::ptr::read(msg.as_ptr().add(1) as *const CloseWindowRequest)
                };
                self.handle_close_window(&req);
            }
            t if t == WindowMessageType::MoveWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<MoveWindowRequest>() {
                    return;
                }
                let req: MoveWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const MoveWindowRequest)
                };
                self.handle_move_window(&req);
            }
            t if t == WindowMessageType::RaiseWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<RaiseWindowRequest>() {
                    return;
                }
                let req: RaiseWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const RaiseWindowRequest)
                };
                self.handle_raise_window(&req);
            }
            t if t == WindowMessageType::LowerWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<LowerWindowRequest>() {
                    return;
                }
                let req: LowerWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const LowerWindowRequest)
                };
                self.handle_lower_window(&req);
            }
            _ => {}
        }
    }

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
            }

            // Click-to-focus: left button just pressed
            let just_pressed = cur_buttons & !self.prev_mouse_buttons;
            if just_pressed & MOUSE_LEFT != 0 {
                let hit = self.hit_test(self.cursor_x, self.cursor_y);
                if let Some(id) = hit {
                    self.z_raise(id);
                    self.mark_full_redraw();
                }
                self.set_focus(hit);
            }
            self.prev_mouse_buttons = cur_buttons;

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

            ulib::sys_yield();
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

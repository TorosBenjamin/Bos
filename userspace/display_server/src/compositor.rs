use crate::cursor::{CURSOR_H, CURSOR_IMAGE, CURSOR_MASK, CURSOR_W};
use crate::window::Window;
use kernel_api_types::window::*;
use kernel_api_types::{IPC_OK, MMAP_WRITE};

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
    /// Off-screen composite: background + all windows blended, no cursor.
    /// Cursor movement reads only this buffer — never touches window shared memory
    /// mid-render, eliminating tearing.
    scene_buf: *mut u32,
    /// Current cursor position (hot spot, clamped to screen)
    cursor_x: i32,
    cursor_y: i32,
    /// Cursor colours pre-built in native framebuffer pixel format
    cursor_black: u32,
    cursor_white: u32,
    /// Damage accumulated this loop iteration from IPC messages (requires scene update)
    pending_damage: Option<DirtyRect>,
    pending_scene_update: bool,
    /// True when a full redraw is needed (window add/remove/reorder)
    pending_full_redraw: bool,
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
            pending_scene_update: false,
            pending_full_redraw: false,
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

    fn z_raise(&mut self, id: WindowId) {
        self.z_remove(id);
        self.z_push(id);
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
        self.pending_scene_update = true;
        self.pending_damage = None;
    }

    fn mark_damage(&mut self, rect: DirtyRect) {
        self.expand_pending(rect);
        self.pending_scene_update = true;
    }

    // --- Scene buffer compositing ---

    /// Blit raw pixels from `src` into `scene_buf` (same clipping logic as Display::blit_raw).
    fn blit_to_scene(&mut self, src: *const u32, src_width: u32, dst_x: i32, dst_y: i32, w: u32, h: u32) {
        if self.scene_buf.is_null() {
            return;
        }
        let screen_w = self.display_info.width as usize;
        let screen_h = self.display_info.height as usize;

        let x0 = dst_x.max(0) as usize;
        let y0 = dst_y.max(0) as usize;
        let x1 = ((dst_x + w as i32).max(0) as usize).min(screen_w);
        let y1 = ((dst_y + h as i32).max(0) as usize).min(screen_h);
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

    /// Update scene_buf for `damage` region: blit background then all overlapping windows.
    fn update_scene_region(&mut self, damage: DirtyRect) {
        // Background
        if !self.background_buf.is_null() {
            let screen_w = self.display_info.width;
            let src = unsafe {
                self.background_buf.add(damage.y as usize * screen_w as usize + damage.x as usize)
            };
            self.blit_to_scene(src, screen_w, damage.x as i32, damage.y as i32, damage.w, damage.h);
        }

        // Windows in z-order (only those overlapping damage)
        for i in 0..self.n_windows {
            let id = self.z_order[i];
            // Collect fields to avoid borrow conflict with blit_to_scene(&mut self)
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
                self.blit_to_scene(wbuf, ww, wx, wy, ww, wh);
            }
        }
    }

    /// Rebuild the entire scene_buf from background + all windows.
    fn update_scene_full(&mut self) {
        if self.scene_buf.is_null() {
            return;
        }
        // Copy background
        if !self.background_buf.is_null() {
            let n = self.display_info.width as usize * self.display_info.height as usize;
            unsafe { core::ptr::copy_nonoverlapping(self.background_buf, self.scene_buf, n) };
        }
        // Blit all windows in z-order
        for i in 0..self.n_windows {
            let id = self.z_order[i];
            let info = self.windows.iter()
                .filter_map(|w| w.as_ref())
                .find(|w| w.id == id)
                .map(|w| (w.x, w.y, w.width, w.height, w.buffer as *const u32));

            if let Some((wx, wy, ww, wh, wbuf)) = info {
                self.blit_to_scene(wbuf, ww, wx, wy, ww, wh);
            }
        }
    }

    /// Blit `damage` region from scene_buf into the display back buffer, draw cursor
    /// on top if it overlaps, then present.
    fn present_region(&mut self, damage: DirtyRect) {
        if !self.scene_buf.is_null() {
            let screen_w = self.display_info.width;
            let src = unsafe {
                self.scene_buf.add(damage.y as usize * screen_w as usize + damage.x as usize)
            };
            self.display.blit_raw(src, screen_w, damage.x as i32, damage.y as i32, damage.w, damage.h);
        }

        // Cursor is always on top — draw it if it overlaps the damage rect
        if let Some(cr) = self.cursor_rect() {
            let dx1 = damage.x + damage.w;
            let dy1 = damage.y + damage.h;
            if cr.x < dx1 && cr.x + cr.w > damage.x && cr.y < dy1 && cr.y + cr.h > damage.y {
                self.display.blit_cursor(
                    self.cursor_x, self.cursor_y,
                    &CURSOR_MASK, &CURSOR_IMAGE,
                    CURSOR_W, CURSOR_H,
                    self.cursor_black, self.cursor_white,
                );
            }
        }

        self.display.present();
    }

    /// Flush all pending damage: update scene if needed, then present.
    fn flush(&mut self) {
        if self.pending_full_redraw {
            self.pending_full_redraw = false;
            self.pending_scene_update = false;
            self.pending_damage = None;
            self.update_scene_full();
            let w = self.display_info.width;
            let h = self.display_info.height;
            self.present_region(DirtyRect { x: 0, y: 0, w, h });
        } else if let Some(damage) = self.pending_damage.take() {
            if self.pending_scene_update {
                self.pending_scene_update = false;
                self.update_scene_region(damage);
            }
            self.present_region(damage);
        }
    }

    // --- Window handlers ---

    fn handle_create_window(&mut self, req: &CreateWindowRequest, reply_ep: u64) {
        if req.width == 0 || req.height == 0 || req.width > 4096 || req.height > 4096 {
            self.send_response(reply_ep, &CreateWindowResponse {
                result: WindowResult::ErrorInvalidDimensions,
                window_id: 0,
                shared_buf_id: 0,
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
                });
                return;
            }
        };

        let window_id = self.next_window_id;
        self.next_window_id += 1;

        match Window::new(window_id, req.x, req.y, req.width, req.height) {
            Some(window) => {
                let shared_buf_id = window.shared_buf_id;
                self.windows[slot_idx] = Some(window);
                self.z_push(window_id);
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::Ok,
                    window_id,
                    shared_buf_id,
                });
                self.mark_full_redraw();
            }
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                    shared_buf_id: 0,
                });
            }
        }
    }

    fn handle_update_window(&mut self, header: &UpdateWindowRequest) {
        let pos = {
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
            (window.x, window.y, window.width, window.height)
        };

        if let Some(rect) = self.screen_rect(pos.0, pos.1, pos.2, pos.3) {
            self.mark_damage(rect);
        }
    }

    fn handle_close_window(&mut self, req: &CloseWindowRequest) {
        for slot in self.windows.iter_mut() {
            if let Some(window) = slot {
                if window.id == req.window_id {
                    ulib::sys_munmap(window.buffer as *mut u8, window.buf_size);
                    let id = window.id;
                    let shared_buf_id = window.shared_buf_id;
                    *slot = None;
                    self.z_remove(id);
                    ulib::sys_destroy_shared_buf(shared_buf_id);
                    self.mark_full_redraw();
                    return;
                }
            }
        }
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
                    core::ptr::read(msg.as_ptr().add(1) as *const CreateWindowRequest)
                };
                let reply_ep = u64::from_le_bytes([
                    msg[1 + core::mem::size_of::<CreateWindowRequest>()],
                    msg[2 + core::mem::size_of::<CreateWindowRequest>()],
                    msg[3 + core::mem::size_of::<CreateWindowRequest>()],
                    msg[4 + core::mem::size_of::<CreateWindowRequest>()],
                    msg[5 + core::mem::size_of::<CreateWindowRequest>()],
                    msg[6 + core::mem::size_of::<CreateWindowRequest>()],
                    msg[7 + core::mem::size_of::<CreateWindowRequest>()],
                    msg[8 + core::mem::size_of::<CreateWindowRequest>()],
                ]);
                self.handle_create_window(&req, reply_ep);
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
            // Drain all pending IPC messages before compositing.
            loop {
                let msg_slice = unsafe { core::slice::from_raw_parts_mut(msg_buf, MAX_MSG_SIZE) };
                let (result, bytes_read) = ulib::sys_channel_recv(self.recv_endpoint, msg_slice);
                if result != IPC_OK || bytes_read == 0 {
                    break;
                }
                let msg = unsafe { core::slice::from_raw_parts(msg_buf, bytes_read as usize) };
                self.process_message(msg);
            }

            // Drain all pending mouse events; accumulate into a single cursor move.
            let mut total_dx = 0i32;
            let mut total_dy = 0i32;
            while let Some(ev) = ulib::sys_read_mouse() {
                total_dx += ev.dx as i32;
                total_dy += ev.dy as i32;
            }
            if total_dx != 0 || total_dy != 0 {
                let old_rect = self.cursor_rect();
                self.cursor_x = (self.cursor_x + total_dx)
                    .clamp(0, self.display_info.width as i32 - 1);
                self.cursor_y = (self.cursor_y + total_dy)
                    .clamp(0, self.display_info.height as i32 - 1);
                let new_rect = self.cursor_rect();

                // Expand pending damage to cover old and new cursor positions.
                // No scene update needed — cursor lives above scene_buf.
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

            // Single composite for everything accumulated this iteration.
            self.flush();

            ulib::sys_yield();
        }
    }
}

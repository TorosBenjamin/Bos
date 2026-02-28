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
    /// Current cursor position (hot spot, clamped to screen)
    cursor_x: i32,
    cursor_y: i32,
    /// Cursor colours pre-built in native framebuffer pixel format
    cursor_black: u32,
    cursor_white: u32,
}

impl Compositor {
    pub fn new(recv_endpoint: u64) -> Self {
        let display = ulib::display::Display::new();
        let display_info = ulib::sys_get_display_info();

        const NONE_WINDOW: Option<Window> = None;

        let width = display_info.width as usize;
        let height = display_info.height as usize;

        // Pre-render gradient background
        let bg_bytes = (width * height * 4) as u64;
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
            cursor_x: display_info.width as i32 / 2,
            cursor_y: display_info.height as i32 / 2,
            cursor_black,
            cursor_white,
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

    // --- Damage helpers ---

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

    // --- Compositing ---

    /// Blit windows in z-order that overlap `damage` (or all windows if `damage` is None).
    fn blit_windows(&mut self, damage: Option<DirtyRect>) {
        for i in 0..self.n_windows {
            let id = self.z_order[i];
            if let Some(window) = self.windows.iter()
                .filter_map(|w| w.as_ref())
                .find(|w| w.id == id)
            {
                if let Some(d) = damage {
                    let wx1 = window.x + window.width as i32;
                    let wy1 = window.y + window.height as i32;
                    let dx1 = d.x as i32 + d.w as i32;
                    let dy1 = d.y as i32 + d.h as i32;
                    if window.x >= dx1 || wx1 <= d.x as i32
                        || window.y >= dy1 || wy1 <= d.y as i32
                    {
                        continue;
                    }
                }
                window.composite_to_display(&mut self.display);
            }
        }
    }

    /// Full composite: entire background + all windows + cursor → present.
    pub fn composite_all(&mut self) {
        if !self.background_buf.is_null() {
            self.display.blit_raw(
                self.background_buf, self.display_info.width,
                0, 0, self.display_info.width, self.display_info.height,
            );
        }
        self.blit_windows(None);
        self.display.blit_cursor(
            self.cursor_x, self.cursor_y,
            &CURSOR_MASK, &CURSOR_IMAGE,
            CURSOR_W, CURSOR_H,
            self.cursor_black, self.cursor_white,
        );
        self.display.present();
    }

    /// Damage-region composite: repaints only `damage` rect → present.
    fn composite_damage(&mut self, damage: DirtyRect) {
        if !self.background_buf.is_null() {
            let screen_w = self.display_info.width as usize;
            let src = unsafe {
                self.background_buf
                    .add(damage.y as usize * screen_w + damage.x as usize)
            };
            self.display.blit_raw(
                src, self.display_info.width,
                damage.x as i32, damage.y as i32, damage.w, damage.h,
            );
        }
        self.blit_windows(Some(damage));
        // Cursor is always on top — blit it if it overlaps the damage rect.
        if let Some(cr) = self.cursor_rect() {
            let dx1 = damage.x + damage.w;
            let dy1 = damage.y + damage.h;
            let cx1 = cr.x + cr.w;
            let cy1 = cr.y + cr.h;
            if cr.x < dx1 && cx1 > damage.x && cr.y < dy1 && cy1 > damage.y {
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

    // --- Window handlers ---

    fn handle_create_window(&mut self, req: &CreateWindowRequest, reply_ep: u64) {
        if req.width == 0 || req.height == 0 || req.width > 4096 || req.height > 4096 {
            self.send_response(reply_ep, &CreateWindowResponse {
                result: WindowResult::ErrorInvalidDimensions,
                window_id: 0,
            });
            return;
        }

        let slot_idx = match self.windows.iter().position(|w| w.is_none()) {
            Some(i) => i,
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                });
                return;
            }
        };

        let window_id = self.next_window_id;
        self.next_window_id += 1;

        match Window::new(window_id, req.x, req.y, req.width, req.height) {
            Some(window) => {
                self.windows[slot_idx] = Some(window);
                self.z_push(window_id);
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::Ok,
                    window_id,
                });
                self.composite_all();
            }
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                });
            }
        }
    }

    fn handle_update_window(&mut self, header: &UpdateWindowRequest, pixels: &[u8]) {
        let pos = {
            let window = match self.windows.iter_mut()
                .filter_map(|w| w.as_mut())
                .find(|w| w.id == header.window_id)
            {
                Some(w) => w,
                None => return,
            };
            let ok = window.update_region(
                header.buffer_width,
                header.dirty_x, header.dirty_y,
                header.dirty_width, header.dirty_height,
                pixels,
            );
            if !ok { return; }
            (window.x, window.y, window.width, window.height)
        };

        if let Some(damage) = self.screen_rect(pos.0, pos.1, pos.2, pos.3) {
            self.composite_damage(damage);
        }
    }

    fn handle_close_window(&mut self, req: &CloseWindowRequest) {
        for slot in self.windows.iter_mut() {
            if let Some(window) = slot {
                if window.id == req.window_id {
                    let buf_size = (window.width as u64) * (window.height as u64) * 4;
                    ulib::sys_munmap(window.buffer as *mut u8, buf_size);
                    let id = window.id;
                    *slot = None;
                    self.z_remove(id);
                    self.composite_all();
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
                self.composite_damage(d);
            }
        }
    }

    fn handle_raise_window(&mut self, req: &RaiseWindowRequest) {
        self.z_raise(req.window_id);
        self.composite_all();
    }

    fn handle_lower_window(&mut self, req: &LowerWindowRequest) {
        self.z_lower(req.window_id);
        self.composite_all();
    }

    /// Move cursor by the given accumulated delta and composite the affected region once.
    fn move_cursor(&mut self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        let old_rect = self.cursor_rect();

        self.cursor_x = (self.cursor_x + dx)
            .clamp(0, self.display_info.width as i32 - 1);
        self.cursor_y = (self.cursor_y + dy)
            .clamp(0, self.display_info.height as i32 - 1);

        let new_rect = self.cursor_rect();

        let damage = match (old_rect, new_rect) {
            (Some(mut a), Some(b)) => { a.expand(b.x, b.y, b.w, b.h); Some(a) }
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        if let Some(d) = damage {
            self.composite_damage(d);
        }
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
                let pixel_offset = 1 + core::mem::size_of::<UpdateWindowRequest>();
                let pixel_byte_count = header.buffer_size as usize * 4;
                if msg.len() < pixel_offset + pixel_byte_count {
                    return;
                }
                let pixels = &msg[pixel_offset..pixel_offset + pixel_byte_count];
                self.handle_update_window(&header, pixels);
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

        loop {
            let msg_slice = unsafe {
                core::slice::from_raw_parts_mut(msg_buf, MAX_MSG_SIZE)
            };
            let (result, bytes_read) = ulib::sys_channel_recv(self.recv_endpoint, msg_slice);
            if result == IPC_OK && bytes_read > 0 {
                let msg = &msg_slice[..bytes_read as usize];
                self.process_message(msg);
            }
            // Drain all pending mouse events, accumulate into one delta, composite once.
            let mut total_dx = 0i32;
            let mut total_dy = 0i32;
            while let Some(ev) = ulib::sys_read_mouse() {
                total_dx += ev.dx as i32;
                total_dy += ev.dy as i32;
            }
            self.move_cursor(total_dx, total_dy);
            ulib::sys_yield();
        }
    }
}

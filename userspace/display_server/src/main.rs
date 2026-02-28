#![no_std]
#![no_main]
use ulib::display::Display;
use kernel_api_types::window::*;
use kernel_api_types::{IPC_OK, MMAP_WRITE};

/// Maximum number of windows that can be created
const MAX_WINDOWS: usize = 32;

/// Maximum message size for IPC (4KB)
const MAX_MSG_SIZE: usize = 4096;

/// Represents a client window in the compositor
struct Window {
    id: WindowId,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    /// Pixel buffer for this window (owned by compositor)
    buffer: *mut u32,
}

impl Window {
    fn new(id: WindowId, x: i32, y: i32, width: u32, height: u32) -> Option<Self> {
        let buf_size = (width as u64) * (height as u64) * 4;
        let buffer = ulib::sys_mmap(buf_size, MMAP_WRITE) as *mut u32;

        if buffer.is_null() {
            return None;
        }
        // Buffer is already zeroed by sys_mmap (black in any pixel format)

        Some(Window {
            id,
            x,
            y,
            width,
            height,
            buffer,
        })
    }

    /// Update a region of this window's buffer from incoming pixel data (as raw bytes)
    fn update_region(
        &mut self,
        _buffer_width: u32,
        dirty_x: u32,
        dirty_y: u32,
        dirty_width: u32,
        dirty_height: u32,
        pixels: &[u8],
    ) -> bool {
        // Validate dimensions
        if dirty_x + dirty_width > self.width || dirty_y + dirty_height > self.height {
            return false;
        }

        if pixels.len() < (dirty_width * dirty_height * 4) as usize {
            return false;
        }

        // Copy pixels into window buffer as bytes to avoid alignment issues
        unsafe {
            for row in 0..dirty_height {
                let src_offset = (row * dirty_width * 4) as usize;
                let dest_offset = ((dirty_y + row) * self.width + dirty_x) as usize;

                core::ptr::copy_nonoverlapping(
                    pixels.as_ptr().add(src_offset),
                    self.buffer.add(dest_offset) as *mut u8,
                    (dirty_width * 4) as usize,
                );
            }
        }

        true
    }

    /// Composite this window onto the display using fast raw blit.
    /// Window buffer pixels are already in native framebuffer format.
    fn composite_to_display(&self, display: &mut Display) {
        display.blit_raw(
            self.buffer,
            self.width,
            self.x,
            self.y,
            self.width,
            self.height,
        );
    }
}

struct Compositor {
    display: Display,
    display_info: kernel_api_types::graphics::DisplayInfo,
    windows: [Option<Window>; MAX_WINDOWS],
    next_window_id: WindowId,
    recv_endpoint: u64,
    /// z_order[0] = bottom-most window id, z_order[n_windows-1] = top-most
    z_order: [WindowId; MAX_WINDOWS],
    n_windows: usize,
    /// Pre-rendered gradient background (width × height pixels, native fb format)
    background_buf: *mut u32,
}

impl Compositor {
    fn new(recv_endpoint: u64) -> Self {
        let display = Display::new();
        let display_info = ulib::sys_get_display_info();

        const NONE_WINDOW: Option<Window> = None;

        let width = display_info.width as usize;
        let height = display_info.height as usize;

        // Allocate and pre-render gradient background
        let bg_bytes = (width * height * 4) as u64;
        let background_buf = ulib::sys_mmap(bg_bytes, MMAP_WRITE) as *mut u32;

        if !background_buf.is_null() {
            // Gradient: top #1e3a5f (dark navy) → bottom #0a0a0f (near-black blue)
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

        Compositor {
            display,
            display_info,
            windows: [NONE_WINDOW; MAX_WINDOWS],
            next_window_id: 1,
            recv_endpoint,
            z_order: [0; MAX_WINDOWS],
            n_windows: 0,
            background_buf,
        }
    }

    // --- Z-order helpers (O(n), n ≤ 32) ---

    /// Append id to the top of the z-stack (new topmost window).
    fn z_push(&mut self, id: WindowId) {
        if self.n_windows < MAX_WINDOWS {
            self.z_order[self.n_windows] = id;
            self.n_windows += 1;
        }
    }

    /// Remove id from the z-stack, shifting remaining entries left.
    fn z_remove(&mut self, id: WindowId) {
        if let Some(pos) = self.z_order[..self.n_windows].iter().position(|&x| x == id) {
            for i in pos..self.n_windows - 1 {
                self.z_order[i] = self.z_order[i + 1];
            }
            self.n_windows -= 1;
        }
    }

    /// Move id to the top of the z-stack.
    fn z_raise(&mut self, id: WindowId) {
        self.z_remove(id);
        self.z_push(id);
    }

    /// Move id to the bottom of the z-stack.
    fn z_lower(&mut self, id: WindowId) {
        self.z_remove(id);
        if self.n_windows < MAX_WINDOWS {
            // Shift everything up by one
            for i in (0..self.n_windows).rev() {
                self.z_order[i + 1] = self.z_order[i];
            }
            self.z_order[0] = id;
            self.n_windows += 1;
        }
    }

    // --- Damage helpers ---

    /// Returns the screen-clipped bounding rect of a window, or None if off-screen.
    fn screen_rect(
        &self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    ) -> Option<DirtyRect> {
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

    // --- Compositing ---

    /// Full composite: blits the entire background, all windows in z-order, then presents.
    ///
    /// Use for z-order changes and window close — these are rare operations where
    /// correctness (no trails anywhere) matters more than minimising VRAM writes.
    fn composite_all(&mut self) {
        if !self.background_buf.is_null() {
            self.display.blit_raw(
                self.background_buf,
                self.display_info.width,
                0,
                0,
                self.display_info.width,
                self.display_info.height,
            );
        }
        for i in 0..self.n_windows {
            let id = self.z_order[i];
            if let Some(window) = self.windows.iter()
                .filter_map(|w| w.as_ref())
                .find(|w| w.id == id)
            {
                window.composite_to_display(&mut self.display);
            }
        }
        // TODO: draw cursor sprite here (always on top, after all windows)
        self.display.present();
    }

    /// Damage-region composite: only repaints `damage` and presents it.
    ///
    /// 1. Blits the background sub-region for `damage` only (avoids full-screen clear).
    /// 2. Blits every window in z-order that overlaps `damage`.
    /// 3. Calls present() — dirty rect equals `damage` ∪ any overlapping window bboxes.
    ///
    /// Use for UpdateWindow and MoveWindow, which are called every animation frame.
    /// Keeps VRAM writes proportional to the affected area rather than the screen size.
    fn composite_damage(&mut self, damage: DirtyRect) {
        if !self.background_buf.is_null() {
            // Offset the source pointer so that blit_raw reads the correct background sub-region.
            // blit_raw with dst_x=damage.x, dst_y=damage.y computes src_x_off = 0, src_y_off = 0,
            // so row r reads from src_ptr[r * src_width], i.e. the start of row r in the sub-region.
            let screen_w = self.display_info.width as usize;
            let src = unsafe {
                self.background_buf
                    .add(damage.y as usize * screen_w + damage.x as usize)
            };
            self.display.blit_raw(
                src,
                self.display_info.width,
                damage.x as i32,
                damage.y as i32,
                damage.w,
                damage.h,
            );
        }

        // Blit every window that overlaps the damage rect, in z-order (bottom → top).
        let dx1 = damage.x as i32 + damage.w as i32;
        let dy1 = damage.y as i32 + damage.h as i32;
        for i in 0..self.n_windows {
            let id = self.z_order[i];
            if let Some(window) = self.windows.iter()
                .filter_map(|w| w.as_ref())
                .find(|w| w.id == id)
            {
                let wx1 = window.x + window.width as i32;
                let wy1 = window.y + window.height as i32;
                if window.x < dx1 && wx1 > damage.x as i32
                    && window.y < dy1 && wy1 > damage.y as i32
                {
                    window.composite_to_display(&mut self.display);
                }
            }
        }

        // TODO: draw cursor sprite here (always on top, after all windows)
        self.display.present();
    }

    // --- Window handlers ---

    fn handle_create_window(&mut self, req: &CreateWindowRequest, reply_ep: u64) {
        // Validate dimensions
        if req.width == 0 || req.height == 0 || req.width > 4096 || req.height > 4096 {
            let response = CreateWindowResponse {
                result: WindowResult::ErrorInvalidDimensions,
                window_id: 0,
            };
            self.send_response(reply_ep, &response);
            return;
        }

        // Find empty slot
        let slot_idx = match self.windows.iter().position(|w| w.is_none()) {
            Some(i) => i,
            None => {
                let response = CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                };
                self.send_response(reply_ep, &response);
                return;
            }
        };

        let window_id = self.next_window_id;
        self.next_window_id += 1;

        match Window::new(window_id, req.x, req.y, req.width, req.height) {
            Some(window) => {
                self.windows[slot_idx] = Some(window);
                self.z_push(window_id);
                let response = CreateWindowResponse {
                    result: WindowResult::Ok,
                    window_id,
                };
                self.send_response(reply_ep, &response);
                // Full composite on window creation so the gradient background
                // is painted across the entire screen, not just the window area.
                self.composite_all();
            }
            None => {
                let response = CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                };
                self.send_response(reply_ep, &response);
            }
        }
    }

    fn handle_update_window(&mut self, header: &UpdateWindowRequest, pixels: &[u8]) {
        // Find and update the window buffer, then remember its screen position.
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
                header.dirty_x,
                header.dirty_y,
                header.dirty_width,
                header.dirty_height,
                pixels,
            );
            if !ok {
                return;
            }
            (window.x, window.y, window.width, window.height)
        };

        // Damage = the window's on-screen bounding box.
        // Only this region needs to be re-blitted to VRAM — the rest of the screen is unchanged.
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
                    // Full composite: the closed window's area must be repainted with background.
                    self.composite_all();
                    return;
                }
            }
        }
    }

    fn handle_move_window(&mut self, req: &MoveWindowRequest) {
        // Update the window position and capture the old one in a single pass,
        // releasing the mutable borrow before we call screen_rect / composite_damage.
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
            // Damage = old bbox ∪ new bbox.
            // Old position needs background repaint; new position needs window repaint.
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
        // Full composite: z-order changes can affect any pixel on screen.
        self.composite_all();
    }

    fn handle_lower_window(&mut self, req: &LowerWindowRequest) {
        self.z_lower(req.window_id);
        // Full composite: z-order changes can affect any pixel on screen.
        self.composite_all();
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

                // read_unaligned: header starts at offset 1 (not 8-byte aligned)
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

                // read_unaligned: MoveWindowRequest starts at offset 1 (not 8-byte aligned)
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
            _ => {
                // Unknown message type, ignore
            }
        }
    }

    fn run(&mut self) -> ! {
        // Allocate message buffer
        let msg_buf = ulib::sys_mmap(MAX_MSG_SIZE as u64, MMAP_WRITE);
        if msg_buf.is_null() {
            loop {
                ulib::sys_yield();
            }
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

            ulib::sys_yield();
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    // Create our own IPC channel and self-register under "display"
    let (send_ep, recv_ep) = ulib::sys_channel_create(16);
    ulib::sys_register_service(b"display", send_ep);

    let mut compositor = Compositor::new(recv_ep);
    compositor.run()
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

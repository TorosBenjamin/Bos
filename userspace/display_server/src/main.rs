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
    #[allow(dead_code)]
    display_info: kernel_api_types::graphics::DisplayInfo,
    windows: [Option<Window>; MAX_WINDOWS],
    next_window_id: WindowId,
    recv_endpoint: u64,
}

impl Compositor {
    fn new(recv_endpoint: u64) -> Self {
        let display = Display::new();
        let display_info = ulib::sys_get_display_info();

        const NONE_WINDOW: Option<Window> = None;

        Compositor {
            display,
            display_info,
            windows: [NONE_WINDOW; MAX_WINDOWS],
            next_window_id: 1,
            recv_endpoint,
        }
    }

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
        let slot = match self.windows.iter_mut().find(|w| w.is_none()) {
            Some(s) => s,
            None => {
                let response = CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                };
                self.send_response(reply_ep, &response);
                return;
            }
        };

        // Create window
        let window_id = self.next_window_id;
        self.next_window_id += 1;

        match Window::new(window_id, req.x, req.y, req.width, req.height) {
            Some(window) => {
                *slot = Some(window);
                let response = CreateWindowResponse {
                    result: WindowResult::Ok,
                    window_id,
                };
                self.send_response(reply_ep, &response);
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
        // Find the window
        let window = match self.windows.iter_mut()
            .filter_map(|w| w.as_mut())
            .find(|w| w.id == header.window_id) {
            Some(w) => w,
            None => return,
        };

        // Update the window's buffer
        let ok = window.update_region(
            header.buffer_width,
            header.dirty_x,
            header.dirty_y,
            header.dirty_width,
            header.dirty_height,
            pixels,
        );
        if ok {
            self.composite_all();
        }
    }

    fn handle_close_window(&mut self, req: &CloseWindowRequest) {
        for slot in self.windows.iter_mut() {
            if let Some(window) = slot {
                if window.id == req.window_id {
                    let buf_size = (window.width as u64) * (window.height as u64) * 4;
                    ulib::sys_munmap(window.buffer as *mut u8, buf_size);
                    *slot = None;
                    self.composite_all();
                    return;
                }
            }
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

    /// Composite all windows onto the display and present
    fn composite_all(&mut self) {
        for window_slot in &self.windows {
            if let Some(window) = window_slot {
                window.composite_to_display(&mut self.display);
            }
        }
        self.display.present();
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

                // The next 8 bytes are the reply endpoint
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

                // Use read_unaligned: header starts at offset 1 (not 8-byte aligned)
                let header: UpdateWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const UpdateWindowRequest)
                };

                // Extract pixel data as bytes to avoid unaligned *const u32 at offset 33
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
            // Receive a message (blocking)
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
unsafe extern "sysv64" fn entry_point(arg: u64) -> ! {
    // arg contains the receive endpoint from init_task
    let recv_endpoint = arg;

    if recv_endpoint == 0 {
        loop {
            ulib::sys_yield();
        }
    }

    let mut compositor = Compositor::new(recv_endpoint);
    compositor.run()
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

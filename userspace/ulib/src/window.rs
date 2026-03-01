/// Client-side window abstraction for communicating with the display_server.
///
/// The pixel backing store lives in shared physical memory allocated by the
/// display server. The client maps those same pages and writes pixels directly;
/// present() sends only a tiny dirty-rect notification (no pixel copy).
///
/// # Tiling model
/// Toplevel windows have their size assigned by the DS (auto-tiling). Create them
/// with `Window::new()`; the response includes the DS-assigned dimensions.
/// If the DS later resizes the window (e.g. another toplevel is opened/closed),
/// a `Configure` event arrives via `poll_event()`. The client must call
/// `apply_configure()` to swap to the new shared buffer.

use core::convert::Infallible;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Size;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::OriginDimensions;
use embedded_graphics::Pixel;
use kernel_api_types::graphics::DisplayInfo;
use kernel_api_types::window::{
    ConfigureEvent, CreatePanelRequest, CreateWindowRequest, CreateWindowResponse,
    UpdateWindowRequest, WindowEventType, WindowMessageType, WindowResult, WindowId,
};
pub use kernel_api_types::window::DirtyRect;
pub use kernel_api_types::{KeyEvent, KeyEventType};

/// Events delivered from the display server to this window.
pub enum WindowEvent {
    KeyPress(KeyEvent),
    FocusGained,
    FocusLost,
    /// DS has reallocated the backing buffer. Call `apply_configure()` to activate it.
    Configure { shared_buf_id: u64, width: u32, height: u32 },
}

/// A client window backed by shared physical memory.
pub struct Window {
    /// Window ID assigned by display_server
    window_id: WindowId,
    /// IPC send endpoint to display_server
    send_endpoint: u64,
    /// Pointer into the shared buffer (same physical pages as the server's copy)
    buffer: *mut u32,
    /// Shared buffer ID (needed for cleanup / apply_configure)
    shared_buf_id: u64,
    /// Size of the buffer in bytes
    buf_size: u64,
    width: u32,
    height: u32,
    info: DisplayInfo,
    dirty: Option<DirtyRect>,
    /// Receive endpoint for DS-pushed events (key presses, focus changes, configure).
    event_recv_ep: u64,
}

impl Window {
    /// Create a new Toplevel window via the display_server.
    ///
    /// The DS assigns size and position via auto-tiling; the response contains the
    /// actual dimensions. Pass the returned `Window` directly to the draw loop.
    pub fn new(display_server_send_ep: u64) -> Option<Self> {
        // Create the event channel the DS will use to push events to us.
        let (event_send, event_recv) = crate::sys_channel_create(32);

        let (our_send, our_recv) = crate::sys_channel_create(1);

        const MSG_SIZE: usize = 1 + core::mem::size_of::<CreateWindowRequest>() + 8;
        let mut msg = [0u8; MSG_SIZE];
        msg[0] = WindowMessageType::CreateWindow as u8;

        let req = CreateWindowRequest { event_send_ep: event_send };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const CreateWindowRequest as *const u8,
                msg.as_mut_ptr().add(1),
                core::mem::size_of::<CreateWindowRequest>(),
            );
        }

        let ep_offset = 1 + core::mem::size_of::<CreateWindowRequest>();
        msg[ep_offset..ep_offset + 8].copy_from_slice(&our_send.to_le_bytes());

        let result = crate::sys_channel_send(display_server_send_ep, &msg);
        if result != kernel_api_types::IPC_OK {
            crate::sys_channel_close(our_send);
            crate::sys_channel_close(our_recv);
            crate::sys_channel_close(event_send);
            crate::sys_channel_close(event_recv);
            return None;
        }

        let mut response_buf = [0u8; core::mem::size_of::<CreateWindowResponse>()];
        let (recv_result, bytes_read) = loop {
            let (res, len) = crate::sys_channel_recv(our_recv, &mut response_buf);
            if res == kernel_api_types::IPC_ERR_CHANNEL_FULL {
                crate::sys_yield();
                continue;
            }
            break (res, len);
        };

        crate::sys_channel_close(our_send);
        crate::sys_channel_close(our_recv);

        if recv_result != kernel_api_types::IPC_OK
            || bytes_read != core::mem::size_of::<CreateWindowResponse>() as u64
        {
            crate::sys_channel_close(event_send);
            crate::sys_channel_close(event_recv);
            return None;
        }

        let response: CreateWindowResponse = unsafe {
            core::ptr::read(response_buf.as_ptr() as *const CreateWindowResponse)
        };

        if response.result != WindowResult::Ok {
            crate::sys_channel_close(event_send);
            crate::sys_channel_close(event_recv);
            return None;
        }

        let buf_size = (response.width as u64) * (response.height as u64) * 4;
        let buffer = crate::sys_map_shared_buf(response.shared_buf_id) as *mut u32;
        if buffer.is_null() {
            crate::sys_channel_close(event_send);
            crate::sys_channel_close(event_recv);
            return None;
        }

        // event_send stays open; DS holds a reference to it and sends events through it.
        // It will be cleaned up when the task exits (owned_endpoints) or DS closes it.
        let _ = event_send; // suppress unused warning; still in owned_endpoints

        let info = crate::sys_get_display_info();

        Some(Window {
            window_id: response.window_id,
            send_endpoint: display_server_send_ep,
            buffer,
            shared_buf_id: response.shared_buf_id,
            buf_size,
            width: response.width,
            height: response.height,
            info,
            dirty: None,
            event_recv_ep: event_recv,
        })
    }

    /// Create a Panel anchored to a screen edge.
    ///
    /// Panels have a fixed position (DS-placed at the specified anchor edge). They reduce
    /// the area available to Toplevels by `exclusive_zone` pixels.
    pub fn new_panel(
        display_server_send_ep: u64,
        anchor: u8,
        exclusive_zone: u32,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        let (event_send, event_recv) = crate::sys_channel_create(32);

        let (our_send, our_recv) = crate::sys_channel_create(1);

        const MSG_SIZE: usize = 1 + core::mem::size_of::<CreatePanelRequest>() + 8;
        let mut msg = [0u8; MSG_SIZE];
        msg[0] = WindowMessageType::CreatePanel as u8;

        let req = CreatePanelRequest {
            anchor,
            _pad: [0; 3],
            exclusive_zone,
            width,
            height,
            event_send_ep: event_send,
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const CreatePanelRequest as *const u8,
                msg.as_mut_ptr().add(1),
                core::mem::size_of::<CreatePanelRequest>(),
            );
        }

        let ep_offset = 1 + core::mem::size_of::<CreatePanelRequest>();
        msg[ep_offset..ep_offset + 8].copy_from_slice(&our_send.to_le_bytes());

        let result = crate::sys_channel_send(display_server_send_ep, &msg);
        if result != kernel_api_types::IPC_OK {
            crate::sys_channel_close(our_send);
            crate::sys_channel_close(our_recv);
            crate::sys_channel_close(event_send);
            crate::sys_channel_close(event_recv);
            return None;
        }

        let mut response_buf = [0u8; core::mem::size_of::<CreateWindowResponse>()];
        let (recv_result, bytes_read) = loop {
            let (res, len) = crate::sys_channel_recv(our_recv, &mut response_buf);
            if res == kernel_api_types::IPC_ERR_CHANNEL_FULL {
                crate::sys_yield();
                continue;
            }
            break (res, len);
        };

        crate::sys_channel_close(our_send);
        crate::sys_channel_close(our_recv);

        if recv_result != kernel_api_types::IPC_OK
            || bytes_read != core::mem::size_of::<CreateWindowResponse>() as u64
        {
            crate::sys_channel_close(event_send);
            crate::sys_channel_close(event_recv);
            return None;
        }

        let response: CreateWindowResponse = unsafe {
            core::ptr::read(response_buf.as_ptr() as *const CreateWindowResponse)
        };

        if response.result != WindowResult::Ok {
            crate::sys_channel_close(event_send);
            crate::sys_channel_close(event_recv);
            return None;
        }

        let buf_size = (response.width as u64) * (response.height as u64) * 4;
        let buffer = crate::sys_map_shared_buf(response.shared_buf_id) as *mut u32;
        if buffer.is_null() {
            crate::sys_channel_close(event_send);
            crate::sys_channel_close(event_recv);
            return None;
        }

        let _ = event_send;
        let info = crate::sys_get_display_info();

        Some(Window {
            window_id: response.window_id,
            send_endpoint: display_server_send_ep,
            buffer,
            shared_buf_id: response.shared_buf_id,
            buf_size,
            width: response.width,
            height: response.height,
            info,
            dirty: None,
            event_recv_ep: event_recv,
        })
    }

    /// Poll for an event from the display server (non-blocking).
    ///
    /// Returns `None` immediately if no event is pending.
    /// For `Configure` events, call `apply_configure()` to activate the new buffer.
    pub fn poll_event(&mut self) -> Option<WindowEvent> {
        let mut buf = [0u8; 32];
        let (res, bytes_read) = crate::sys_try_channel_recv(self.event_recv_ep, &mut buf);
        if res != kernel_api_types::IPC_OK || bytes_read == 0 {
            return None;
        }

        if bytes_read == 0 {
            return None;
        }

        let event_type = buf[0];
        if event_type == WindowEventType::KeyPress as u8 {
            if bytes_read >= core::mem::size_of::<kernel_api_types::window::KeyPressEvent>() as u64 {
                let ev: kernel_api_types::window::KeyPressEvent = unsafe {
                    core::ptr::read_unaligned(buf.as_ptr() as *const _)
                };
                return Some(WindowEvent::KeyPress(ev.key));
            }
        } else if event_type == WindowEventType::FocusGained as u8 {
            return Some(WindowEvent::FocusGained);
        } else if event_type == WindowEventType::FocusLost as u8 {
            return Some(WindowEvent::FocusLost);
        } else if event_type == WindowEventType::Configure as u8 {
            if bytes_read >= core::mem::size_of::<ConfigureEvent>() as u64 {
                let ev: ConfigureEvent = unsafe {
                    core::ptr::read_unaligned(buf.as_ptr() as *const _)
                };
                return Some(WindowEvent::Configure {
                    shared_buf_id: ev.shared_buf_id,
                    width: ev.width,
                    height: ev.height,
                });
            }
        }

        None
    }

    /// Apply a Configure event: unmap old buffer, map new buffer, update dimensions.
    ///
    /// Must be called after receiving `WindowEvent::Configure`. After returning, the
    /// window's `size()` reflects the new dimensions and pixels can be written to the
    /// new buffer immediately.
    pub fn apply_configure(&mut self, shared_buf_id: u64, width: u32, height: u32) {
        // Unmap old buffer
        crate::sys_munmap(self.buffer as *mut u8, self.buf_size);

        // Map new buffer
        let new_buf_size = (width as u64) * (height as u64) * 4;
        let new_buf = crate::sys_map_shared_buf(shared_buf_id) as *mut u32;

        self.buffer = new_buf;
        self.shared_buf_id = shared_buf_id;
        self.buf_size = new_buf_size;
        self.width = width;
        self.height = height;
        self.dirty = None;
    }

    /// Notify the display server of the dirty region — no pixel data is sent.
    /// Pixels were already written directly into the shared buffer.
    pub fn present(&mut self) {
        if let Some(dirty) = self.dirty.take() {
            let header = UpdateWindowRequest {
                window_id: self.window_id,
                dirty_x: dirty.x,
                dirty_y: dirty.y,
                dirty_width: dirty.w,
                dirty_height: dirty.h,
            };
            const MSG_SIZE: usize = 1 + core::mem::size_of::<UpdateWindowRequest>();
            let mut msg = [0u8; MSG_SIZE];
            msg[0] = WindowMessageType::UpdateWindow as u8;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &header as *const UpdateWindowRequest as *const u8,
                    msg.as_mut_ptr().add(1),
                    core::mem::size_of::<UpdateWindowRequest>(),
                );
            }
            crate::sys_channel_send(self.send_endpoint, &msg);
        }
    }

    fn expand_dirty(&mut self, x: u32, y: u32, w: u32, h: u32) {
        match &mut self.dirty {
            Some(d) => d.expand(x, y, w, h),
            None => {
                self.dirty = Some(DirtyRect { x, y, w, h });
            }
        }
    }
}

impl OriginDimensions for Window {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl DrawTarget for Window {
    type Color = Rgb888;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let x = point.x;
            let y = point.y;
            if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
                continue;
            }
            let x = x as u32;
            let y = y as u32;
            let pixel = self.info.build_pixel(color.r(), color.g(), color.b());
            unsafe {
                let offset = (y as usize) * (self.width as usize) + (x as usize);
                *self.buffer.add(offset) = pixel;
            }
            self.expand_dirty(x, y, 1, 1);
        }
        Ok(())
    }

    fn fill_solid(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        color: Self::Color,
    ) -> Result<(), Self::Error> {
        let pixel = self.info.build_pixel(color.r(), color.g(), color.b());

        let x0 = (area.top_left.x.max(0) as u32).min(self.width);
        let y0 = (area.top_left.y.max(0) as u32).min(self.height);
        let x1 = ((area.top_left.x + area.size.width as i32).max(0) as u32).min(self.width);
        let y1 = ((area.top_left.y + area.size.height as i32).max(0) as u32).min(self.height);

        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }

        for y in y0..y1 {
            let row_start = (y as usize) * (self.width as usize) + (x0 as usize);
            for x_off in 0..(x1 - x0) as usize {
                unsafe {
                    *self.buffer.add(row_start + x_off) = pixel;
                }
            }
        }

        self.expand_dirty(x0, y0, x1 - x0, y1 - y0);
        Ok(())
    }
}

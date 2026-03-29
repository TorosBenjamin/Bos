//! Client-side window abstraction for communicating with the display_server.
//!
//! The pixel backing store lives in shared physical memory allocated by the
//! display server. The client maps those same pages and writes pixels directly;
//! present() sends only a tiny dirty-rect notification (no pixel copy).
//!
//! # Tiling model
//! Toplevel windows have their size assigned by the DS (auto-tiling). Create them
//! with `Window::new()`; the response includes the DS-assigned dimensions.
//! If the DS later resizes the window (e.g. another toplevel is opened/closed),
//! a `Configure` event arrives via `poll_event()`. The client must call
//! `apply_configure()` to swap to the new shared buffer.

use core::convert::Infallible;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Size;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::OriginDimensions;
use embedded_graphics::Pixel;
use kernel_api_types::graphics::DisplayInfo;
use kernel_api_types::window::{
    ConfigureEvent, CreatePanelRequest, CreateWindowRequest, CreateWindowResponse,
    MouseButtonEvent, MouseMoveEvent, UpdateWindowRequest, WindowEventType, WindowMessageType,
    WindowResult, WindowId, WINDOW_FLAG_FLOATING, CloseWindowRequest,
    HideWindowRequest, ShowWindowRequest,
};
pub use kernel_api_types::window::{DirtyRect, WINDOW_FLAG_HIDDEN};
pub use kernel_api_types::{KeyEvent, KeyEventType};

/// Events delivered from the display server to this window.
pub enum WindowEvent {
    KeyPress(KeyEvent),
    FocusGained,
    FocusLost,
    /// DS has reallocated the backing buffer. Call `apply_configure()` to activate it.
    Configure { shared_buf_id: u64, width: u32, height: u32 },
    /// The compositor has finished presenting this window's pixels to the screen.
    /// Clients should wait for this event before drawing the next frame so they
    /// pace themselves to the compositor's actual output rate rather than spinning.
    FramePresented,
    /// Mouse button pressed inside this window. `button` is one of the MOUSE_* bitmask
    /// values. `x`/`y` are relative to the window's top-left corner.
    MouseButtonPress { button: u8, x: i32, y: i32 },
    /// Mouse button released. Same coordinate convention as `MouseButtonPress`.
    MouseButtonRelease { button: u8, x: i32, y: i32 },
    /// Cursor moved while this window is focused. `x`/`y` are window-relative.
    /// `delta_ns` is the compositor's last frame duration — use it for frame-rate-independent
    /// velocity or physics (e.g. `pixels_per_second = dx as f32 / (delta_ns as f32 / 1e9)`).
    MouseMove { x: i32, y: i32, delta_ns: u64 },
    /// DS is destroying this window. Stop accessing the pixel buffer.
    /// Call `acknowledge_close()` then exit, or let the process exit naturally.
    Close,
}

/// A client window backed by shared physical memory.
pub struct Window {
    /// Window ID assigned by display_server
    window_id: WindowId,
    /// Handle (fd) for sending messages to display_server
    send_fd: u32,
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
    /// Handle (fd) for receiving DS-pushed events (key presses, focus changes, configure).
    event_recv_fd: u32,
}

impl Window {
    /// Internal IPC round-trip: send CreateWindow request, receive response, map buffer.
    fn new_inner(display_server_send_fd: u32, req: CreateWindowRequest) -> Option<Self> {
        // Create the event channel the DS will use to push events to us.
        let (event_send, event_recv) = crate::sys_channel_create(32);

        // Wrap event_recv as a handle for reading events (clones the endpoint).
        let event_recv_fd = match crate::handle::handle_from_channel(event_recv, 0) {
            Some(fd) => fd,
            None => {
                crate::sys_channel_close(event_send);
                crate::sys_channel_close(event_recv);
                return None;
            }
        };
        // Close the original raw recv endpoint — the handle owns its clone.
        crate::sys_channel_close(event_recv);

        // Inject the actual event endpoint into the request
        let req = CreateWindowRequest { event_send_ep: event_send, ..req };

        // Create reply channel. We wrap recv as a handle (for reading the reply)
        // but keep send as a raw endpoint — it's embedded in the IPC message for
        // the server to reply through (the server closes it after replying).
        let (our_send_ep, our_recv_ep) = crate::sys_channel_create(1);
        let recv_fd = match crate::handle::handle_from_channel(our_recv_ep, 0) {
            Some(fd) => fd,
            None => {
                crate::sys_channel_close(our_send_ep);
                crate::sys_channel_close(our_recv_ep);
                crate::sys_channel_close(event_send);
                crate::handle::close(event_recv_fd);
                return None;
            }
        };
        // Close original raw recv — the handle owns its clone.
        crate::sys_channel_close(our_recv_ep);

        const MSG_SIZE: usize = 1 + core::mem::size_of::<CreateWindowRequest>() + 8;
        let mut msg = [0u8; MSG_SIZE];
        msg[0] = WindowMessageType::CreateWindow as u8;
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const CreateWindowRequest as *const u8,
                msg.as_mut_ptr().add(1),
                core::mem::size_of::<CreateWindowRequest>(),
            );
        }
        // Embed the raw send endpoint so the DS can reply through it.
        let ep_offset = 1 + core::mem::size_of::<CreateWindowRequest>();
        msg[ep_offset..ep_offset + 8].copy_from_slice(&our_send_ep.to_le_bytes());

        if crate::handle::write(display_server_send_fd, &msg).is_none() {
            crate::sys_channel_close(our_send_ep);
            crate::handle::close(recv_fd);
            crate::sys_channel_close(event_send);
            crate::handle::close(event_recv_fd);
            return None;
        }
        // Message sent — server now owns our_send_ep and will close it after replying.

        let mut response_buf = [0u8; core::mem::size_of::<CreateWindowResponse>()];
        let bytes_read = match crate::handle::read(recv_fd, &mut response_buf) {
            Some(n) => n,
            None => {
                crate::handle::close(recv_fd);
                crate::sys_channel_close(event_send);
                crate::handle::close(event_recv_fd);
                return None;
            }
        };

        crate::handle::close(recv_fd);

        if bytes_read != core::mem::size_of::<CreateWindowResponse>() {
            crate::sys_channel_close(event_send);
            crate::handle::close(event_recv_fd);
            return None;
        }

        let response: CreateWindowResponse = unsafe {
            core::ptr::read(response_buf.as_ptr() as *const CreateWindowResponse)
        };

        if response.result != WindowResult::Ok {
            crate::sys_channel_close(event_send);
            crate::handle::close(event_recv_fd);
            return None;
        }

        let buf_size = (response.width as u64) * (response.height as u64) * 4;
        let buffer = crate::sys_map_shared_buf(response.shared_buf_id) as *mut u32;
        if buffer.is_null() {
            crate::sys_channel_close(event_send);
            crate::handle::close(event_recv_fd);
            return None;
        }

        // event_send stays open — DS holds this raw endpoint and sends events through it.

        let info = crate::sys_get_display_info();

        Some(Window {
            window_id: response.window_id,
            send_fd: display_server_send_fd,
            buffer,
            shared_buf_id: response.shared_buf_id,
            buf_size,
            width: response.width,
            height: response.height,
            info,
            dirty: None,
            event_recv_fd,
        })
    }

    /// Create a new Toplevel window via the display_server.
    ///
    /// The DS assigns size and position via auto-tiling; the response contains the
    /// actual dimensions. `app_id` is a string identifier used for config-based rules.
    pub fn new(display_server_send_fd: u32, app_id: &str) -> Option<Self> {
        let mut id_bytes = [0u8; 32];
        let len = app_id.len().min(32);
        id_bytes[..len].copy_from_slice(app_id.as_bytes()[..len].as_ref());

        let req = CreateWindowRequest {
            event_send_ep: 0, // overridden by new_inner
            flags: 0,
            app_id_len: len as u8,
            _pad: [0; 3],
            app_id: id_bytes,
            parent_id: 0,
            init_w: 0,
            init_h: 0,
        };
        Self::new_inner(display_server_send_fd, req)
    }

    /// Create a floating window with a specific size, optionally parented to another window.
    ///
    /// `parent_id` should be the `window_id()` of the owning window (0 = no parent).
    /// `w` / `h` are the desired pixel dimensions (0 = DS default 400x300).
    /// `extra_flags` are OR'd with `WINDOW_FLAG_FLOATING` (e.g. `WINDOW_FLAG_HIDDEN`).
    pub fn new_floating(
        display_server_send_fd: u32,
        app_id: &str,
        parent_id: u64,
        w: u32,
        h: u32,
        extra_flags: u32,
    ) -> Option<Self> {
        let mut id_bytes = [0u8; 32];
        let len = app_id.len().min(32);
        id_bytes[..len].copy_from_slice(app_id.as_bytes()[..len].as_ref());

        let req = CreateWindowRequest {
            event_send_ep: 0,
            flags: WINDOW_FLAG_FLOATING | extra_flags,
            app_id_len: len as u8,
            _pad: [0; 3],
            app_id: id_bytes,
            parent_id,
            init_w: w,
            init_h: h,
        };
        Self::new_inner(display_server_send_fd, req)
    }

    /// Return the window ID assigned by the display server.
    pub fn window_id(&self) -> u64 { self.window_id }

    /// Return the event receive handle fd (for use with `handle::wait`).
    pub fn event_recv_fd(&self) -> u32 { self.event_recv_fd }

    /// Hide this window (remove from compositor z-order without closing).
    pub fn hide(&mut self) {
        const MSG_SIZE: usize = 1 + core::mem::size_of::<HideWindowRequest>();
        let mut msg = [0u8; MSG_SIZE];
        msg[0] = WindowMessageType::HideWindow as u8;
        let req = HideWindowRequest { window_id: self.window_id };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const HideWindowRequest as *const u8,
                msg.as_mut_ptr().add(1),
                core::mem::size_of::<HideWindowRequest>(),
            );
        }
        crate::handle::try_write(self.send_fd, &msg);
    }

    /// Show a previously hidden window (re-add to compositor z-order).
    pub fn show(&mut self) {
        const MSG_SIZE: usize = 1 + core::mem::size_of::<ShowWindowRequest>();
        let mut msg = [0u8; MSG_SIZE];
        msg[0] = WindowMessageType::ShowWindow as u8;
        let req = ShowWindowRequest { window_id: self.window_id };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const ShowWindowRequest as *const u8,
                msg.as_mut_ptr().add(1),
                core::mem::size_of::<ShowWindowRequest>(),
            );
        }
        crate::handle::try_write(self.send_fd, &msg);
    }

    /// Close the window, unmapping its buffer and closing the event channel.
    ///
    /// Sends a non-blocking CloseWindow notification to the DS.
    pub fn close(self) {
        const MSG_SIZE: usize = 1 + core::mem::size_of::<CloseWindowRequest>();
        let mut msg = [0u8; MSG_SIZE];
        msg[0] = WindowMessageType::CloseWindow as u8;
        let req = CloseWindowRequest { window_id: self.window_id };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const CloseWindowRequest as *const u8,
                msg.as_mut_ptr().add(1),
                core::mem::size_of::<CloseWindowRequest>(),
            );
        }
        crate::sys_munmap(self.buffer as *mut u8, self.buf_size);
        crate::handle::try_write(self.send_fd, &msg);
        crate::handle::close(self.event_recv_fd);
    }

    /// Acknowledge a DS-initiated close. Call this when your app wants to keep running
    /// after receiving `WindowEvent::Close` (e.g. multi-window app closing one window).
    /// For single-window apps it is sufficient to let the process exit naturally.
    pub fn acknowledge_close(self) {
        crate::sys_munmap(self.buffer as *mut u8, self.buf_size);
        crate::handle::close(self.event_recv_fd);
        // Do NOT send CloseWindow — DS already initiated the close.
    }

    /// Create a Panel anchored to a screen edge.
    ///
    /// Panels have a fixed position (DS-placed at the specified anchor edge). They reduce
    /// the area available to Toplevels by `exclusive_zone` pixels.
    /// Pass `flags = WINDOW_FLAG_ALPHA` to opt into premultiplied-alpha compositing.
    pub fn new_panel(
        display_server_send_fd: u32,
        anchor: u8,
        exclusive_zone: u32,
        width: u32,
        height: u32,
        flags: u32,
    ) -> Option<Self> {
        let (event_send, event_recv) = crate::sys_channel_create(32);

        // Wrap event_recv as a handle for reading events (clones the endpoint).
        let event_recv_fd = match crate::handle::handle_from_channel(event_recv, 0) {
            Some(fd) => fd,
            None => {
                crate::sys_channel_close(event_send);
                crate::sys_channel_close(event_recv);
                return None;
            }
        };
        // Close original raw recv — the handle owns its clone.
        crate::sys_channel_close(event_recv);

        // Create reply channel. Keep send as raw (embedded in message for server),
        // wrap recv as handle for reading the reply.
        let (our_send_ep, our_recv_ep) = crate::sys_channel_create(1);
        let recv_fd = match crate::handle::handle_from_channel(our_recv_ep, 0) {
            Some(fd) => fd,
            None => {
                crate::sys_channel_close(our_send_ep);
                crate::sys_channel_close(our_recv_ep);
                crate::sys_channel_close(event_send);
                crate::handle::close(event_recv_fd);
                return None;
            }
        };
        // Close original raw recv — the handle owns its clone.
        crate::sys_channel_close(our_recv_ep);

        const MSG_SIZE: usize = 1 + core::mem::size_of::<CreatePanelRequest>() + 8;
        let mut msg = [0u8; MSG_SIZE];
        msg[0] = WindowMessageType::CreatePanel as u8;

        let req = CreatePanelRequest {
            anchor,
            flags: flags as u8,
            _pad: [0; 2],
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
        msg[ep_offset..ep_offset + 8].copy_from_slice(&our_send_ep.to_le_bytes());

        if crate::handle::write(display_server_send_fd, &msg).is_none() {
            crate::sys_channel_close(our_send_ep);
            crate::handle::close(recv_fd);
            crate::sys_channel_close(event_send);
            crate::handle::close(event_recv_fd);
            return None;
        }
        // Message sent — server now owns our_send_ep and will close it after replying.

        let mut response_buf = [0u8; core::mem::size_of::<CreateWindowResponse>()];
        let bytes_read = match crate::handle::read(recv_fd, &mut response_buf) {
            Some(n) => n,
            None => {
                crate::handle::close(recv_fd);
                crate::sys_channel_close(event_send);
                crate::handle::close(event_recv_fd);
                return None;
            }
        };

        crate::handle::close(recv_fd);

        if bytes_read != core::mem::size_of::<CreateWindowResponse>() {
            crate::sys_channel_close(event_send);
            crate::handle::close(event_recv_fd);
            return None;
        }

        let response: CreateWindowResponse = unsafe {
            core::ptr::read(response_buf.as_ptr() as *const CreateWindowResponse)
        };

        if response.result != WindowResult::Ok {
            crate::sys_channel_close(event_send);
            crate::handle::close(event_recv_fd);
            return None;
        }

        let buf_size = (response.width as u64) * (response.height as u64) * 4;
        let buffer = crate::sys_map_shared_buf(response.shared_buf_id) as *mut u32;
        if buffer.is_null() {
            crate::sys_channel_close(event_send);
            crate::handle::close(event_recv_fd);
            return None;
        }

        // event_send stays open — DS holds this raw endpoint and sends events through it.
        let info = crate::sys_get_display_info();

        Some(Window {
            window_id: response.window_id,
            send_fd: display_server_send_fd,
            buffer,
            shared_buf_id: response.shared_buf_id,
            buf_size,
            width: response.width,
            height: response.height,
            info,
            dirty: None,
            event_recv_fd,
        })
    }

    /// Poll for an event from the display server (non-blocking).
    ///
    /// Returns `None` immediately if no event is pending.
    /// For `Configure` events, call `apply_configure()` to activate the new buffer.
    pub fn poll_event(&mut self) -> Option<WindowEvent> {
        let mut buf = [0u8; 32];
        let bytes_read = crate::handle::try_read(self.event_recv_fd, &mut buf)?;

        if bytes_read == 0 {
            return None;
        }

        let event_type = buf[0];
        if event_type == WindowEventType::KeyPress as u8 {
            if bytes_read >= core::mem::size_of::<kernel_api_types::window::KeyPressEvent>() {
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
            if bytes_read >= core::mem::size_of::<ConfigureEvent>() {
                let ev: ConfigureEvent = unsafe {
                    core::ptr::read_unaligned(buf.as_ptr() as *const _)
                };
                return Some(WindowEvent::Configure {
                    shared_buf_id: ev.shared_buf_id,
                    width: ev.width,
                    height: ev.height,
                });
            }
        } else if event_type == WindowEventType::FramePresented as u8 {
            return Some(WindowEvent::FramePresented);
        } else if event_type == WindowEventType::MouseButtonPress as u8
            || event_type == WindowEventType::MouseButtonRelease as u8
        {
            if bytes_read >= core::mem::size_of::<MouseButtonEvent>() {
                let ev: MouseButtonEvent = unsafe {
                    core::ptr::read_unaligned(buf.as_ptr() as *const _)
                };
                return Some(if event_type == WindowEventType::MouseButtonPress as u8 {
                    WindowEvent::MouseButtonPress { button: ev.button, x: ev.x, y: ev.y }
                } else {
                    WindowEvent::MouseButtonRelease { button: ev.button, x: ev.x, y: ev.y }
                });
            }
        } else if event_type == WindowEventType::MouseMove as u8 {
            if bytes_read >= core::mem::size_of::<MouseMoveEvent>() {
                let ev: MouseMoveEvent = unsafe {
                    core::ptr::read_unaligned(buf.as_ptr() as *const _)
                };
                return Some(WindowEvent::MouseMove { x: ev.x, y: ev.y, delta_ns: ev.delta_ns });
            }
        } else if event_type == WindowEventType::Close as u8 {
            return Some(WindowEvent::Close);
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

        if new_buf.is_null() {
            // Log the failure: tag 0xBAD_BUF0 = shared_buf_id that failed,
            //                  tag 0xBAD_BUF1 = (width << 32 | height).
            crate::sys_debug_log(shared_buf_id, 0xBADB_0001);
            crate::sys_debug_log((width as u64) << 32 | height as u64, 0xBADB_0002);
            // Leave the window in a zero-size inert state so pixels_mut() returns
            // an empty slice rather than crashing by writing through null.
            self.buffer = core::ptr::null_mut();
            self.buf_size = 0;
            self.width = 0;
            self.height = 0;
            self.dirty = None;
            return;
        }

        self.buffer = new_buf;
        self.shared_buf_id = shared_buf_id;
        self.buf_size = new_buf_size;
        self.width = width;
        self.height = height;
        self.dirty = None;
    }

    /// Notify the display server of the dirty region — no pixel data is sent.
    /// Pixels were already written directly into the shared buffer.
    ///
    /// Uses a non-blocking send; if the DS channel is full the notification is
    /// held and retried on the next call to `present()` (the accumulated dirty
    /// rect covers all changes since the last successful send).
    pub fn present(&mut self) {
        if let Some(dirty) = self.dirty.as_ref().copied() {
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
            let result = crate::handle::try_write_raw(self.send_fd, &msg);
            if result == msg.len() as u64 {
                // Message delivered — clear the dirty rect.
                self.dirty = None;
            }
            // If channel full: leave dirty intact so it's retried next frame.
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

    pub fn width(&self) -> u32 { self.width }
    pub fn height(&self) -> u32 { self.height }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        if self.buffer.is_null() {
            return unsafe { core::slice::from_raw_parts_mut(core::ptr::NonNull::dangling().as_ptr(), 0) };
        }
        unsafe { core::slice::from_raw_parts_mut(self.buffer, (self.width * self.height) as usize) }
    }

    pub fn display_info(&self) -> &kernel_api_types::graphics::DisplayInfo { &self.info }

    pub fn mark_dirty_all(&mut self) {
        self.dirty = Some(DirtyRect { x: 0, y: 0, w: self.width, h: self.height });
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

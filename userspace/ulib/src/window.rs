/// Client-side window abstraction for communicating with the display_server.
///
/// This provides a high-level API for creating windows and sending pixel buffers
/// to the display server compositor via IPC channels.

use core::convert::Infallible;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Size;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::OriginDimensions;
use embedded_graphics::Pixel;
use kernel_api_types::graphics::DisplayInfo;
use kernel_api_types::window::{
    CreateWindowRequest, CreateWindowResponse, UpdateWindowRequest, WindowMessageType,
    WindowResult, WindowId, RaiseWindowRequest, LowerWindowRequest, MoveWindowRequest,
};
pub use kernel_api_types::window::DirtyRect;
use kernel_api_types::MMAP_WRITE;

/// A client window that composites to the display_server via IPC
pub struct Window {
    /// Window ID assigned by display_server
    window_id: WindowId,
    /// IPC send endpoint to display_server
    send_endpoint: u64,
    /// Local pixel buffer
    buffer: *mut u32,
    width: u32,
    height: u32,
    info: DisplayInfo,
    dirty: Option<DirtyRect>,
    /// Pre-allocated IPC message buffer (worst-case: full window update)
    msg_buf: *mut u8,
    msg_buf_cap: usize,
}

impl Window {
    /// Create a new window via the display_server.
    ///
    /// # Arguments
    /// * `display_server_send_ep` - IPC send endpoint to display_server
    /// * `width` - Window width in pixels
    /// * `height` - Window height in pixels
    /// * `x` - Initial x position
    /// * `y` - Initial y position
    ///
    /// # Returns
    /// `Some(Window)` on success, `None` if window creation failed
    pub fn new(
        display_server_send_ep: u64,
        width: u32,
        height: u32,
        x: i32,
        y: i32,
    ) -> Option<Self> {
        // Create a channel to receive the response (needed before sending request)
        let (our_send, our_recv) = crate::sys_channel_create(1);

        // Build create window message: [type][CreateWindowRequest][reply_send_ep]
        // The display_server expects all three in a single message.
        const MSG_SIZE: usize = 1 + core::mem::size_of::<CreateWindowRequest>() + 8;
        let mut msg = [0u8; MSG_SIZE];
        msg[0] = WindowMessageType::CreateWindow as u8;

        let req = CreateWindowRequest { width, height, x, y };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const CreateWindowRequest as *const u8,
                msg.as_mut_ptr().add(1),
                core::mem::size_of::<CreateWindowRequest>(),
            );
        }

        // Append reply endpoint
        let ep_offset = 1 + core::mem::size_of::<CreateWindowRequest>();
        msg[ep_offset..ep_offset + 8].copy_from_slice(&our_send.to_le_bytes());

        // Send combined create window request
        let result = crate::sys_channel_send(display_server_send_ep, &msg);
        if result != kernel_api_types::IPC_OK {
            crate::sys_channel_close(our_send);
            crate::sys_channel_close(our_recv);
            return None;
        }

        // Wait for response (block until display_server replies).
        // sys_channel_recv has EINTR semantics: if the timer fires while the task
        // is sleeping inside the syscall, it returns IPC_ERR_CHANNEL_FULL to user
        // space (via the in_syscall_handler / CpuContext fallback rax mechanism).
        // We must retry on that code until real data arrives.
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
            || bytes_read != core::mem::size_of::<CreateWindowResponse>() as u64 {
            return None;
        }

        // Parse response
        let response: CreateWindowResponse = unsafe {
            core::ptr::read(response_buf.as_ptr() as *const CreateWindowResponse)
        };

        if response.result != WindowResult::Ok {
            return None;
        }

        // Allocate local pixel buffer
        let buf_size = (width as u64) * (height as u64) * 4;
        let buffer = crate::sys_mmap(buf_size, MMAP_WRITE) as *mut u32;
        if buffer.is_null() {
            return None;
        }

        // Pre-allocate worst-case IPC message buffer: one full-window UpdateWindow message.
        let msg_buf_cap = 1
            + core::mem::size_of::<UpdateWindowRequest>()
            + (width as usize) * (height as usize) * 4;
        let msg_buf = crate::sys_mmap(msg_buf_cap as u64, MMAP_WRITE);
        if msg_buf.is_null() {
            crate::sys_munmap(buffer as *mut u8, buf_size);
            return None;
        }

        // Get display info for pixel format
        let info = crate::sys_get_display_info();

        Some(Window {
            window_id: response.window_id,
            send_endpoint: display_server_send_ep,
            buffer,
            width,
            height,
            info,
            dirty: None,
            msg_buf,
            msg_buf_cap,
        })
    }

    /// Raise this window to the top of the z-order (fire-and-forget).
    pub fn raise(&self) {
        const MSG_SIZE: usize = 1 + core::mem::size_of::<RaiseWindowRequest>();
        let mut buf = [0u8; MSG_SIZE];
        buf[0] = WindowMessageType::RaiseWindow as u8;
        let req = RaiseWindowRequest { window_id: self.window_id };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const RaiseWindowRequest as *const u8,
                buf.as_mut_ptr().add(1),
                core::mem::size_of::<RaiseWindowRequest>(),
            );
        }
        crate::sys_channel_send(self.send_endpoint, &buf);
    }

    /// Lower this window to the bottom of the z-order (fire-and-forget).
    pub fn lower(&self) {
        const MSG_SIZE: usize = 1 + core::mem::size_of::<LowerWindowRequest>();
        let mut buf = [0u8; MSG_SIZE];
        buf[0] = WindowMessageType::LowerWindow as u8;
        let req = LowerWindowRequest { window_id: self.window_id };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const LowerWindowRequest as *const u8,
                buf.as_mut_ptr().add(1),
                core::mem::size_of::<LowerWindowRequest>(),
            );
        }
        crate::sys_channel_send(self.send_endpoint, &buf);
    }

    /// Move this window to `(x, y)` (fire-and-forget).
    pub fn move_to(&self, x: i32, y: i32) {
        const MSG_SIZE: usize = 1 + core::mem::size_of::<MoveWindowRequest>();
        let mut buf = [0u8; MSG_SIZE];
        buf[0] = WindowMessageType::MoveWindow as u8;
        let req = MoveWindowRequest { window_id: self.window_id, x, y };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &req as *const MoveWindowRequest as *const u8,
                buf.as_mut_ptr().add(1),
                core::mem::size_of::<MoveWindowRequest>(),
            );
        }
        crate::sys_channel_send(self.send_endpoint, &buf);
    }

    /// Send the dirty region to the display server for compositing
    pub fn present(&mut self) {
        if let Some(dirty) = self.dirty.take() {
            // Build update message header
            let header = UpdateWindowRequest {
                window_id: self.window_id,
                buffer_width: self.width,
                dirty_x: dirty.x,
                dirty_y: dirty.y,
                dirty_width: dirty.w,
                dirty_height: dirty.h,
                buffer_size: dirty.w * dirty.h,
            };

            let dirty_pixels = (dirty.w * dirty.h) as usize;
            let msg_size = 1 + core::mem::size_of::<UpdateWindowRequest>() + dirty_pixels * 4;

            // msg_buf_cap is always >= msg_size (worst case is the full window)
            debug_assert!(msg_size <= self.msg_buf_cap);

            unsafe {
                let msg_buf = self.msg_buf;

                // Write message type
                *msg_buf = WindowMessageType::UpdateWindow as u8;

                // Write header (unaligned write — offset 1 is not UpdateWindowRequest-aligned)
                core::ptr::copy_nonoverlapping(
                    &header as *const UpdateWindowRequest as *const u8,
                    msg_buf.add(1),
                    core::mem::size_of::<UpdateWindowRequest>(),
                );

                // Copy dirty region pixels as bytes (offset 33 is not u32-aligned)
                let pixel_bytes = msg_buf.add(1 + core::mem::size_of::<UpdateWindowRequest>());
                for row in 0..dirty.h {
                    let src_row = (dirty.y + row) * self.width + dirty.x;
                    let dest_row = row * dirty.w;
                    core::ptr::copy_nonoverlapping(
                        self.buffer.add(src_row as usize) as *const u8,
                        pixel_bytes.add((dest_row * 4) as usize),
                        (dirty.w * 4) as usize,
                    );
                }

                // Send via IPC
                let msg_slice = core::slice::from_raw_parts(msg_buf, msg_size);
                crate::sys_channel_send(self.send_endpoint, msg_slice);
            }
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

        // Clamp to window bounds
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

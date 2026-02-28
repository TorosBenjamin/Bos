/// Client-side window abstraction for communicating with the display_server.
///
/// The pixel backing store lives in shared physical memory allocated by the
/// display server. The client maps those same pages and writes pixels directly;
/// present() sends only a tiny dirty-rect notification (no pixel copy).

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

/// A client window backed by shared physical memory.
pub struct Window {
    /// Window ID assigned by display_server
    window_id: WindowId,
    /// IPC send endpoint to display_server
    send_endpoint: u64,
    /// Pointer into the shared buffer (same physical pages as the server's copy)
    buffer: *mut u32,
    /// Shared buffer ID (needed for cleanup)
    shared_buf_id: u64,
    /// Size of the buffer in bytes
    buf_size: u64,
    width: u32,
    height: u32,
    info: DisplayInfo,
    dirty: Option<DirtyRect>,
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
    pub fn new(
        display_server_send_ep: u64,
        width: u32,
        height: u32,
        x: i32,
        y: i32,
    ) -> Option<Self> {
        let (our_send, our_recv) = crate::sys_channel_create(1);

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

        let ep_offset = 1 + core::mem::size_of::<CreateWindowRequest>();
        msg[ep_offset..ep_offset + 8].copy_from_slice(&our_send.to_le_bytes());

        let result = crate::sys_channel_send(display_server_send_ep, &msg);
        if result != kernel_api_types::IPC_OK {
            crate::sys_channel_close(our_send);
            crate::sys_channel_close(our_recv);
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
            return None;
        }

        let response: CreateWindowResponse = unsafe {
            core::ptr::read(response_buf.as_ptr() as *const CreateWindowResponse)
        };

        if response.result != WindowResult::Ok {
            return None;
        }

        // Map the shared buffer the server created — zero-copy backing store.
        let buf_size = (width as u64) * (height as u64) * 4;
        let buffer = crate::sys_map_shared_buf(response.shared_buf_id) as *mut u32;
        if buffer.is_null() {
            return None;
        }

        let info = crate::sys_get_display_info();

        Some(Window {
            window_id: response.window_id,
            send_endpoint: display_server_send_ep,
            buffer,
            shared_buf_id: response.shared_buf_id,
            buf_size,
            width,
            height,
            info,
            dirty: None,
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

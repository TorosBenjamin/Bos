use kernel_api_types::window::WindowId;
use ulib::display::Display;

pub struct Window {
    pub id: WindowId,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Pointer into the shared physical buffer (readable by the compositor).
    pub buffer: *mut u32,
    /// Opaque ID returned to the client in CreateWindowResponse so it can map the same pages.
    pub shared_buf_id: u64,
    /// Size in bytes — needed to call sys_munmap before destroying the shared buf.
    pub buf_size: u64,
}

impl Window {
    pub fn new(id: WindowId, x: i32, y: i32, width: u32, height: u32) -> Option<Self> {
        let buf_size = (width as u64) * (height as u64) * 4;
        let (shared_buf_id, buffer_ptr) = ulib::sys_create_shared_buf(buf_size);
        if buffer_ptr.is_null() || shared_buf_id == u64::MAX {
            return None;
        }
        Some(Window {
            id,
            x,
            y,
            width,
            height,
            buffer: buffer_ptr as *mut u32,
            shared_buf_id,
            buf_size,
        })
    }

    pub fn composite_to_display(&self, display: &mut Display) {
        display.blit_raw(self.buffer, self.width, self.x, self.y, self.width, self.height);
    }
}

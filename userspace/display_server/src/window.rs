use kernel_api_types::window::WindowId;
use kernel_api_types::MMAP_WRITE;
use ulib::display::Display;

pub struct Window {
    pub id: WindowId,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub buffer: *mut u32,
}

impl Window {
    pub fn new(id: WindowId, x: i32, y: i32, width: u32, height: u32) -> Option<Self> {
        let buf_size = (width as u64) * (height as u64) * 4;
        let buffer = ulib::sys_mmap(buf_size, MMAP_WRITE) as *mut u32;

        if buffer.is_null() {
            return None;
        }

        Some(Window { id, x, y, width, height, buffer })
    }

    pub fn update_region(
        &mut self,
        _buffer_width: u32,
        dirty_x: u32,
        dirty_y: u32,
        dirty_width: u32,
        dirty_height: u32,
        pixels: &[u8],
    ) -> bool {
        if dirty_x + dirty_width > self.width || dirty_y + dirty_height > self.height {
            return false;
        }
        if pixels.len() < (dirty_width * dirty_height * 4) as usize {
            return false;
        }
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

    pub fn composite_to_display(&self, display: &mut Display) {
        display.blit_raw(self.buffer, self.width, self.x, self.y, self.width, self.height);
    }
}

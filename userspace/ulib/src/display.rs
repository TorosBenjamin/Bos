use core::convert::Infallible;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Size;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::OriginDimensions;
use embedded_graphics::Pixel;
use kernel_api_types::graphics::{DisplayInfo, FRAMEBUFFER_USER_VADDR};
use kernel_api_types::MMAP_WRITE;
use crate::window::DirtyRect;

pub struct Display {
    back_buffer: &'static mut [u32],
    front_buffer: &'static mut [u32],
    width: u32,
    height: u32,
    info: DisplayInfo,
    dirty: Option<DirtyRect>,
}

impl Display {
    /// Create a new buffered display. Queries the kernel for display info and
    /// allocates a user-space pixel buffer via sys_mmap.
    pub fn new() -> Self {
        let info = crate::sys_get_display_info();
        let width = info.width;
        let height = info.height;

        let buf_size = (width as u64) * (height as u64) * 4;

        // Allocate the Back Buffer (RAM)
        // This is just normal heap memory or anonymous mmap
        let back_ptr = crate::sys_mmap(buf_size, MMAP_WRITE);
        let back_buffer = unsafe { core::slice::from_raw_parts_mut(back_ptr as *mut u32, buf_size as usize / 4) };

        // Get the Front Buffer (VRAM) - mapped by TransferDisplay syscall
        let front_ptr = FRAMEBUFFER_USER_VADDR as *mut u32;
        let front_buffer = unsafe { core::slice::from_raw_parts_mut(front_ptr, buf_size as usize / 4) };

        Display {
            back_buffer,
            front_buffer,
            width,
            height,
            info,
            dirty: None,
        }
    }

    /// Flushes only the dirty region from the back buffer to the hardware front buffer.
    pub fn present(&mut self) {
        if let Some(dirty) = self.dirty.take() {
            let x_start = dirty.x as usize;
            let y_start = dirty.y as usize;
            let width = dirty.w as usize;
            let height = dirty.h as usize;

            // Perform row-by-row copy
            for row in 0..height {
                let current_y = y_start + row;
                let offset = current_y * self.width as usize + x_start;

                unsafe {
                    core::ptr::copy_nonoverlapping(
                        self.back_buffer.as_ptr().add(offset),
                        self.front_buffer.as_mut_ptr().add(offset),
                        width,
                    );
                }
            }
        }
    }

    /// Blit raw u32 pixels directly into the back buffer (no color conversion).
    /// Pixels are already in the native framebuffer format.
    pub fn blit_raw(
        &mut self,
        src: *const u32,
        src_width: u32,
        dst_x: i32,
        dst_y: i32,
        w: u32,
        h: u32,
    ) {
        // Clip to display bounds
        let x0 = dst_x.max(0) as u32;
        let y0 = dst_y.max(0) as u32;
        let x1 = ((dst_x + w as i32).max(0) as u32).min(self.width);
        let y1 = ((dst_y + h as i32).max(0) as u32).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let clipped_w = x1 - x0;
        let src_x_off = (x0 as i32 - dst_x) as u32;
        let src_y_off = (y0 as i32 - dst_y) as u32;

        for row in 0..(y1 - y0) {
            let src_offset = ((src_y_off + row) * src_width + src_x_off) as usize;
            let dst_offset = ((y0 + row) as usize) * (self.width as usize) + x0 as usize;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src.add(src_offset),
                    self.back_buffer.as_mut_ptr().add(dst_offset),
                    clipped_w as usize,
                );
            }
        }
        self.expand_dirty(x0, y0, clipped_w, y1 - y0);
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

impl OriginDimensions for Display {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl DrawTarget for Display {
    type Color = Rgb888;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let x = point.x;
            let y = point.y;

            // Keep as i32 to handle negative coordinates from embedded-graphics
            if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
                continue;
            }

            let ux = x as u32;
            let uy = y as u32;

            // Map color to hardware format
            let pixel = self.info.build_pixel(color.r(), color.g(), color.b());

            // Write to Back Buffer (Safe indexing)
            let offset = (uy as usize) * (self.width as usize) + (ux as usize);
            self.back_buffer[offset] = pixel;

            // Update dirty tracking
            self.expand_dirty(ux, uy, 1, 1);
        }
        Ok(())
    }

    fn fill_solid(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        color: Self::Color,
    ) -> Result<(), Self::Error> {
        let pixel = self.info.build_pixel(color.r(), color.g(), color.b());

        // Clamp to display bounds
        let x0 = (area.top_left.x.max(0) as u32).min(self.width);
        let y0 = (area.top_left.y.max(0) as u32).min(self.height);
        let x1 = ((area.top_left.x + area.size.width as i32).max(0) as u32).min(self.width);
        let y1 = ((area.top_left.y + area.size.height as i32).max(0) as u32).min(self.height);

        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }

        let width = (x1 - x0) as usize;

        // Fill the back buffer row by row
        for y in y0..y1 {
            let row_start = (y as usize) * (self.width as usize) + (x0 as usize);
            // Using fill() on a slice is often optimized to a SIMD memset
            self.back_buffer[row_start..row_start + width].fill(pixel);
        }

        // Track the rectangle as dirty
        self.expand_dirty(x0, y0, x1 - x0, y1 - y0);
        Ok(())
    }
}

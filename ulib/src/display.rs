use core::convert::Infallible;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Size;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::OriginDimensions;
use embedded_graphics::Pixel;
use kernel_api_types::graphics::DisplayInfo;
use kernel_api_types::MMAP_WRITE;

pub struct Display {
    buffer: *mut u32,
    width: u32,
    height: u32,
    info: DisplayInfo,
    dirty: Option<DirtyRect>,
}

struct DirtyRect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl DirtyRect {
    fn expand(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let x2 = self.x + self.w;
        let y2 = self.y + self.h;
        let new_x2 = (x + w).max(x2);
        let new_y2 = (y + h).max(y2);
        self.x = self.x.min(x);
        self.y = self.y.min(y);
        self.w = new_x2 - self.x;
        self.h = new_y2 - self.y;
    }
}

impl Display {
    /// Create a new buffered display. Queries the kernel for display info and
    /// allocates a user-space pixel buffer via sys_mmap.
    pub fn new() -> Self {
        let info = crate::sys_get_display_info();
        let width = info.width;
        let height = info.height;
        let buf_size = (width as u64) * (height as u64) * 4;
        let buffer = crate::sys_mmap(buf_size, MMAP_WRITE) as *mut u32;

        Display {
            buffer,
            width,
            height,
            info,
            dirty: None,
        }
    }

    /// Flush the dirty region to the kernel framebuffer.
    pub fn present(&mut self) {
        if let Some(dirty) = self.dirty.take() {
            crate::sys_present_display(
                self.buffer as *const u32,
                self.width,
                dirty.x,
                dirty.y,
                dirty.w,
                dirty.h,
            );
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

        // Clamp to display bounds
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

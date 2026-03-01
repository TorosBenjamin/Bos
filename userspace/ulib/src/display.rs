use core::convert::Infallible;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Size;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::OriginDimensions;
use embedded_graphics::Pixel;
use kernel_api_types::graphics::{DisplayInfo, FRAMEBUFFER_USER_VADDR};
use kernel_api_types::MMAP_WRITE;
use crate::window::DirtyRect;

/// Maximum number of independent dirty rectangles tracked before falling back to a bounding box.
const MAX_DIRTY: usize = 8;

pub struct Display {
    back_buffer: &'static mut [u32],
    front_buffer: &'static mut [u32],
    width: u32,
    height: u32,
    /// Row stride of the VRAM front buffer in u32 units. May be larger than `width`
    /// when the hardware pads rows for alignment (pitch > width * 4).
    pitch_pixels: u32,
    info: DisplayInfo,
    /// List of independent dirty rects to present. Using a list instead of a single bounding
    /// box avoids the 879× VRAM-write explosion that occurs when two windows at opposite screen
    /// corners both update in the same frame (their tiny rects would otherwise merge into a
    /// huge bounding box covering most of the screen).
    dirty: [Option<DirtyRect>; MAX_DIRTY],
    n_dirty: usize,
}

impl Display {
    /// Create a new buffered display. Queries the kernel for display info and
    /// allocates a user-space pixel buffer via sys_mmap.
    pub fn new() -> Self {
        let info = crate::sys_get_display_info();
        let width = info.width;
        let height = info.height;

        // pitch is bytes per row; pitch_pixels is u32 units per row (may be > width).
        let pitch_pixels = info.pitch / 4;
        let back_size = (width as u64) * (height as u64) * 4;
        // The VRAM mapping covers pitch * height bytes (hardware may pad each row).
        let front_len = (pitch_pixels as usize) * (height as usize);

        // Allocate the Back Buffer (RAM) — flat, no padding, stride == width.
        let back_ptr = crate::sys_mmap(back_size, MMAP_WRITE);
        let back_buffer = unsafe { core::slice::from_raw_parts_mut(back_ptr as *mut u32, back_size as usize / 4) };

        // Get the Front Buffer (VRAM) - mapped by TransferDisplay syscall.
        let front_ptr = FRAMEBUFFER_USER_VADDR as *mut u32;
        let front_buffer = unsafe { core::slice::from_raw_parts_mut(front_ptr, front_len) };

        Display {
            back_buffer,
            front_buffer,
            width,
            height,
            pitch_pixels,
            info,
            dirty: [None; MAX_DIRTY],
            n_dirty: 0,
        }
    }

    /// Flush all accumulated dirty rectangles from the back buffer to the hardware front buffer.
    ///
    /// Each rect in the dirty list is written to VRAM independently — no bounding-box merging —
    /// so two small windows at opposite screen corners only write their individual pixels.
    pub fn present(&mut self) {
        for i in 0..self.n_dirty {
            if let Some(dirty) = self.dirty[i].take() {
                let x_start = dirty.x as usize;
                let y_start = dirty.y as usize;
                let w = dirty.w as usize;
                let h = dirty.h as usize;
                for row in 0..h {
                    // back_buffer is flat (stride = width); front_buffer may have padding (stride = pitch_pixels).
                    let back_offset  = (y_start + row) * self.width as usize + x_start;
                    let front_offset = (y_start + row) * self.pitch_pixels as usize + x_start;
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            self.back_buffer.as_ptr().add(back_offset),
                            self.front_buffer.as_mut_ptr().add(front_offset),
                            w,
                        );
                    }
                }
            }
        }
        self.n_dirty = 0;
    }

    /// Returns a raw mutable pointer to the start of the back buffer.
    /// The back buffer is flat with stride == width (no padding).
    pub fn back_buffer_ptr(&self) -> *mut u32 {
        self.back_buffer.as_ptr() as *mut u32
    }

    /// Stride of the back buffer in u32 units (equals display width, no padding).
    pub fn back_buffer_width(&self) -> u32 {
        self.width
    }

    /// Register a dirty rectangle for the next `present()` call without writing any pixels.
    /// Use this after compositing directly into `back_buffer_ptr()`.
    pub fn mark_dirty(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.expand_dirty(x, y, w, h);
    }

    /// Blit a two-layer bitmask cursor sprite into the back buffer.
    ///
    /// `mask[row]`  — bit=1 (MSB=col 0) means the pixel is opaque.
    /// `image[row]` — among opaque pixels, bit=1 → `white`, bit=0 → `black`.
    ///
    /// `black` and `white` must already be in the native framebuffer pixel format
    /// (build them once with `DisplayInfo::build_pixel`).
    pub fn blit_cursor(
        &mut self,
        cx: i32,
        cy: i32,
        mask: &[u16],
        image: &[u16],
        w: u32,
        h: u32,
        black: u32,
        white: u32,
    ) {
        let sw = self.width as i32;
        let sh = self.height as i32;

        // Expand dirty to the cursor's clipped bounding rect upfront (one call, not per pixel).
        let x0 = cx.max(0) as u32;
        let y0 = cy.max(0) as u32;
        let x1 = ((cx + w as i32).max(0)).min(sw) as u32;
        let y1 = ((cy + h as i32).max(0)).min(sh) as u32;
        if x0 < x1 && y0 < y1 {
            self.expand_dirty(x0, y0, x1 - x0, y1 - y0);
        }

        for row in 0..h as i32 {
            let sy = cy + row;
            if sy < 0 || sy >= sh {
                continue;
            }
            let row_mask = mask[row as usize];
            let row_image = image[row as usize];
            for col in 0..w as i32 {
                if (row_mask >> (15 - col)) & 1 == 0 {
                    continue; // transparent
                }
                let sx = cx + col;
                if sx < 0 || sx >= sw {
                    continue;
                }
                let pixel = if (row_image >> (15 - col)) & 1 == 1 { white } else { black };
                let off = sy as usize * self.width as usize + sx as usize;
                self.back_buffer[off] = pixel;
            }
        }
    }

    fn expand_dirty(&mut self, x: u32, y: u32, w: u32, h: u32) {
        if self.n_dirty < MAX_DIRTY {
            self.dirty[self.n_dirty] = Some(DirtyRect { x, y, w, h });
            self.n_dirty += 1;
        } else {
            // List full: merge new rect into the last entry as a bounding-box fallback.
            if let Some(last) = &mut self.dirty[MAX_DIRTY - 1] {
                last.expand(x, y, w, h);
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

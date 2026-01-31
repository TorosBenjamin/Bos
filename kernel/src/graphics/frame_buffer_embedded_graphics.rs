use crate::graphics::frame_buffer_info::FrameBufferInfo;
use core::convert::Infallible;
use core::num::NonZero;
use core::ptr::{NonNull, slice_from_raw_parts_mut};
use embedded_graphics::Pixel;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Dimensions, Point, Size};
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::primitives::Rectangle;

pub struct FrameBufferEmbeddedGraphics<'a> {
    buffer: &'a mut [u32],
    pub info: FrameBufferInfo,
    pub pixel_pitch: usize,
    pub bounding_box: Rectangle,
}

impl FrameBufferEmbeddedGraphics<'_> {
    /// # Safety
    /// The frame buffer must be mapped at `addr`
    pub unsafe fn new(addr: NonZero<usize>, info: FrameBufferInfo) -> Self {
        if info.bits_per_pixel as u32 == u32::BITS {
            Self {
                buffer: {
                    let mut ptr = NonNull::new(slice_from_raw_parts_mut(
                        addr.get() as *mut u32,
                        (info.pitch * info.height) as usize / size_of::<u32>(),
                    ))
                    .unwrap();
                    // Safety: This memory is mapped
                    unsafe { ptr.as_mut() }
                },
                info,
                pixel_pitch: info.pitch as usize / size_of::<u32>(),
                bounding_box: Rectangle {
                    top_left: Point::zero(),
                    size: Size {
                        width: info.width.try_into().unwrap(),
                        height: info.height.try_into().unwrap(),
                    },
                },
            }
        } else {
            panic!("DrawTarget implemented for RGB888, but bpp doesn't match RGB888");
        }
    }

    /// Replace whole buffer
    pub fn put_buffer(&mut self, frame: u32) {
        self.buffer.fill(frame);
    }

    pub fn put_pixel(&mut self, x: usize, y: usize, color: Rgb888) {
        if x >= self.info.width as usize || y >= self.info.height as usize {
            return;
        }

        let idx = y * self.pixel_pitch + x;
        self.buffer[idx] = self.info.pixel.build(color);
    }

    pub fn fill_rect(&mut self, area: Rectangle, color: Rgb888) {
        let area = area.intersection(&self.bounding_box);
        let pixel = self.info.pixel.build(color);

        let width = area.size.width as usize;
        let x0 = area.top_left.x as usize;

        for y in area.top_left.y as usize..area.top_left.y as usize + area.size.height as usize {
            let idx = y * self.pixel_pitch + x0;
            self.buffer[idx..idx + width].fill(pixel);
        }
    }

    pub fn shift_up(&mut self, amount: usize) {
        self.buffer.copy_within(amount * self.pixel_pitch.., 0);
    }

    /// Copy a dirty rectangle from a user-space pixel buffer into the framebuffer.
    ///
    /// # Safety
    /// `user_buf` must point to a valid user buffer of at least
    /// `(dirty_y + dirty_h) * user_width` u32 elements.
    pub unsafe fn copy_rect_from_user(
        &mut self,
        user_buf: *const u32,
        user_width: usize,
        dirty_x: usize,
        dirty_y: usize,
        dirty_w: usize,
        dirty_h: usize,
    ) {
        for row in 0..dirty_h {
            let src_offset = (dirty_y + row) * user_width + dirty_x;
            let dst_offset = (dirty_y + row) * self.pixel_pitch + dirty_x;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    user_buf.add(src_offset),
                    self.buffer.as_mut_ptr().add(dst_offset),
                    dirty_w,
                );
            }
        }
    }
}

impl Dimensions for FrameBufferEmbeddedGraphics<'_> {
    fn bounding_box(&self) -> embedded_graphics::primitives::Rectangle {
        self.bounding_box
    }
}

impl DrawTarget for FrameBufferEmbeddedGraphics<'_> {
    type Color = Rgb888;

    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>>,
    {
        let bounding_box = self.bounding_box();
        pixels
            .into_iter()
            .filter(|Pixel(point, _)| bounding_box.contains(*point))
            .for_each(|Pixel(point, color)| {
                self.put_pixel(point.x as usize, point.y as usize, color);
            });
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.fill_rect(*area, color);
        Ok(())
    }
}

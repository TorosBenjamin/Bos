use core::convert::Infallible;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Dimensions;
use embedded_graphics::Pixel;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::primitives::Rectangle;
use crate::graphics::rgb_pixel::RgbPixel;
use crate::graphics::frame_buffer_embedded_graphics::FrameBufferEmbeddedGraphics;

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct FrameBufferInfo {
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bits_per_pixel: u16,
    pub pixel: RgbPixel,
}

impl From<&limine::framebuffer::Framebuffer<'_>> for FrameBufferInfo {
    fn from(framebuffer: &limine::framebuffer::Framebuffer) -> Self {
        FrameBufferInfo {
            width: framebuffer.width(),
            height: framebuffer.height(),
            pitch: framebuffer.pitch(),
            bits_per_pixel: framebuffer.bpp(),
            pixel: RgbPixel {
                red_mask_size: framebuffer.red_mask_size(),
                red_mask_shift: framebuffer.red_mask_shift(),
                green_mask_size: framebuffer.green_mask_size(),
                green_mask_shift: framebuffer.green_mask_shift(),
                blue_mask_size: framebuffer.blue_mask_size(),
                blue_mask_shift: framebuffer.blue_mask_shift(),
            },
        }
    }
}
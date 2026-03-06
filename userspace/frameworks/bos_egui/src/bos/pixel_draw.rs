use embedded_graphics::{
    draw_target::DrawTarget,
    geometry::{Size, OriginDimensions},
    pixelcolor::{Rgb888, RgbColor},
    Pixel,
};
use kernel_api_types::graphics::DisplayInfo;

/// A thin DrawTarget wrapper over a raw `u32` pixel buffer,
/// allowing `embedded_graphics` primitives and fonts to render into it.
pub struct PixelBuf<'a> {
    pub pixels: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub info: DisplayInfo,
}

impl OriginDimensions for PixelBuf<'_> {
    fn size(&self) -> Size { Size::new(self.width, self.height) }
}

impl DrawTarget for PixelBuf<'_> {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where I: IntoIterator<Item = Pixel<Rgb888>>
    {
        for Pixel(point, color) in pixels {
            let x = point.x;
            let y = point.y;
            if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 { continue; }
            let idx = y as usize * self.width as usize + x as usize;
            self.pixels[idx] = self.info.build_pixel(color.r(), color.g(), color.b());
        }
        Ok(())
    }
}

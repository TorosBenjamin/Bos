use crate::graphics::frame_buffer_embedded_graphics::FrameBufferEmbeddedGraphics;
use core::convert::Infallible;
use embedded_graphics::Pixel;
use embedded_graphics::geometry::{Dimensions, Point};
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::DrawTarget;
use embedded_graphics::primitives::Rectangle;
use limine::response::FramebufferResponse;

/// Safety: Call display init before accessing it
pub static DISPLAY: Display = Display {
    inner: spin::Mutex::new(Inner { fb: None }),
};

pub struct Display {
    inner: spin::Mutex<Inner>,
}

pub struct DisplayDraw;

struct Inner {
    fb: Option<FrameBufferEmbeddedGraphics<'static>>,
}

impl Display {
    /// Shifts display rows by amount
    pub fn shift_up(&self, amount: usize) {
        let mut inner = self.inner.lock();
        inner
            .fb
            .as_mut()
            .expect("Display not initialized")
            .shift_up(amount);
    }

    pub fn bounding_box(&self) -> Rectangle {
        let inner = self.inner.lock();
        inner
            .fb
            .as_ref()
            .expect("Display not initialized")
            .bounding_box
    }

    pub fn draw_iter<I>(&self, pixels: I) -> Result<(), Infallible>
    where
        I: IntoIterator<Item = Pixel<Rgb888>>,
    {
        let mut inner = self.inner.lock();
        let fb = inner.fb.as_mut().expect("Display not initialized");
        fb.draw_iter(pixels)
    }

    /// Fill a solid rectangle
    pub fn fill_solid(&self, area: &Rectangle, color: Rgb888) -> Result<(), Infallible> {
        let mut inner = self.inner.lock();
        let fb = inner.fb.as_mut().expect("Display not initialized");
        fb.fill_solid(area, color)
    }
}

impl Dimensions for DisplayDraw {
    fn bounding_box(&self) -> Rectangle {
        DISPLAY.bounding_box()
    }
}

/// Wrapper so it can be passed in as a reference
impl DrawTarget for DisplayDraw {
    type Color = Rgb888;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        DISPLAY.draw_iter(pixels)
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        DISPLAY.fill_solid(area, color)
    }
}

pub fn init(framebuffer: &'static FramebufferResponse) {
    let mut inner = DISPLAY.inner.lock();
    let frame_buffer = framebuffer.framebuffers().next().unwrap();
    let addr = frame_buffer.addr().addr().try_into().unwrap();
    let info = (&frame_buffer).into();
    inner.fb = Some(unsafe { FrameBufferEmbeddedGraphics::new(addr, info) });
}

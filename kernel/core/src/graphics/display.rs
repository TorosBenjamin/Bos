use crate::graphics::frame_buffer_embedded_graphics::FrameBufferEmbeddedGraphics;
use core::convert::Infallible;
use core::sync::atomic::{AtomicU64, Ordering};
use embedded_graphics::Pixel;
use embedded_graphics::geometry::Dimensions;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::DrawTarget;
use embedded_graphics::primitives::Rectangle;
use kernel_api_types::graphics::DisplayInfo;
use limine::response::FramebufferResponse;



/// TaskId (as u64) of the current display owner. u64::MAX = no owner.
pub static DISPLAY_OWNER: AtomicU64 = AtomicU64::new(u64::MAX);

pub fn is_display_owner() -> bool {
    let cpu = crate::memory::cpu_local_data::get_local();
    let rq = cpu.run_queue.get().unwrap().lock();
    match &rq.current_task {
        Some(task) => task.id.to_u64() == DISPLAY_OWNER.load(Ordering::Relaxed),
        None => false,
    }
}

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

    /// Copy a dirty rectangle from a user-space pixel buffer into the framebuffer.
    ///
    /// # Safety
    /// `user_buf` must point to valid readable memory of sufficient size.
    pub unsafe fn copy_rect_from_user(
        &self,
        user_buf: *const u32,
        user_width: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
    ) {
        let mut inner = self.inner.lock();
        let fb = inner.fb.as_mut().expect("Display not initialized");
        unsafe { fb.copy_rect_from_user(user_buf, user_width, x, y, w, h) };
    }

    /// Get display info (dimensions and pixel format).
    pub fn get_display_info(&self) -> DisplayInfo {
        let inner = self.inner.lock();
        let fb = inner.fb.as_ref().expect("Display not initialized");
        DisplayInfo {
            width: fb.info.width as u32,
            height: fb.info.height as u32,
            red_mask_size: fb.info.pixel.red_mask_size,
            red_mask_shift: fb.info.pixel.red_mask_shift,
            green_mask_size: fb.info.pixel.green_mask_size,
            green_mask_shift: fb.info.pixel.green_mask_shift,
            blue_mask_size: fb.info.pixel.blue_mask_size,
            blue_mask_shift: fb.info.pixel.blue_mask_shift,
        }
    }

    /// Get the framebuffer's physical address and total size in bytes.
    /// Used by syscall to map the framebuffer into user space.
    pub fn get_fb_phys_and_size(&self) -> (x86_64::PhysAddr, u64) {
        let inner = self.inner.lock();
        let fb = inner.fb.as_ref().expect("Display not initialized");
        let size = fb.info.pitch * fb.info.height;
        (fb.info.phys_addr, size)
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

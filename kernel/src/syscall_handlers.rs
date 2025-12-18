use crate::graphics::display::DISPLAY;
use embedded_graphics::Pixel;
use embedded_graphics::geometry::Point;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::primitives::Rectangle;
use kernel_api_types::graphics::{GraphicsResult, PixelData, Rect, Rgb888Raw};

/// Syscall: draw multiple pixels from user-space
pub fn sys_draw_iter(pixels_ptr: u64, len: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    let pixels: &[PixelData] =
        unsafe { core::slice::from_raw_parts(pixels_ptr as *const PixelData, len as usize) };

    let pixels_iter = pixels.iter().map(|p| {
        let color = raw_to_rgb888(p.rgb_raw);
        Pixel(Point::new(p.x as i32, p.y as i32), color)
    });

    // Draw the pixels
    if DISPLAY.draw_iter(pixels_iter).is_err() {
        return GraphicsResult::InvalidInput as u64;
    }

    GraphicsResult::Ok as u64
}

/// Syscall: fill a solid rectangle
pub fn sys_fill_solid(rect_ptr: u64, rgb_raw: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    // SAFETY: rect_ptr comes from userspace, must be validated in real kernel
    // TODO: Pointer validation
    let rect = unsafe { &*(rect_ptr as *const Rect) };

    let color = raw_to_rgb888(rgb_raw as u32);

    let eg_rect = Rectangle::new(
        Point::new(rect.x as i32, rect.y as i32),
        embedded_graphics::geometry::Size::new(rect.width, rect.height),
    );

    if DISPLAY.fill_solid(&eg_rect, color).is_err() {
        return GraphicsResult::InvalidInput as u64;
    }

    GraphicsResult::Ok as u64
}

/// Syscall: return the bounding box of the framebuffer
pub fn sys_get_bounding_box(rect_out_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    // TODO: Pointer validation
    let rect_out = unsafe { &mut *(rect_out_ptr as *mut Rect) };

    let bb = DISPLAY.bounding_box();

    rect_out.x = bb.top_left.x as u32;
    rect_out.y = bb.top_left.y as u32;
    rect_out.width = bb.size.width;
    rect_out.height = bb.size.height;

    GraphicsResult::Ok as u64
}

pub fn raw_to_rgb888(raw: Rgb888Raw) -> Rgb888 {
    let r = ((raw >> 16) & 0xFF) as u8;
    let g = ((raw >> 8) & 0xFF) as u8;
    let b = (raw & 0xFF) as u8;
    Rgb888::new(r, g, b)
}

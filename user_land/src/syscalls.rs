use crate::syscall;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use kernel_api_types::SysCallNumber;
use kernel_api_types::graphics::{GraphicsResult, PixelData, Rect, Rgb888Raw};

pub fn sys_draw_iter(pixels: &[PixelData]) -> GraphicsResult {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::DrawIter as u64;
    // Pointer to do pixels
    args[1] = pixels.as_ptr() as u64;
    // Number of pixels
    args[2] = pixels.len() as u64;

    syscall(&mut args);

    // Get output
    let ret = args[6];
    GraphicsResult::from_u64(ret)
}

pub fn sys_fill_solid(rect: &Rect, color: Rgb888) -> GraphicsResult {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::FillSolid as u64;
    args[1] = rect as *const Rect as u64;
    args[2] = rgb888_to_raw(color) as u64;

    syscall(&mut args);

    let ret = args[6];
    GraphicsResult::from_u64(ret)
}

pub fn sys_get_bounding_box(out_rect: &mut Rect) -> GraphicsResult {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::GetBoundingBox as u64;
    args[1] = out_rect as *const Rect as u64;

    syscall(&mut args);

    // Get output
    let ret = args[6];
    GraphicsResult::from_u64(ret)
}

pub fn rgb888_to_raw(color: Rgb888) -> Rgb888Raw {
    ((color.r() as u32) << 16) | ((color.g() as u32) << 8) | (color.b() as u32)
}

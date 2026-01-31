#![allow(dead_code)]

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

pub fn sys_read_key() -> kernel_api_types::KeyEvent {
    let mut event = kernel_api_types::KeyEvent::EMPTY;
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::ReadKey as u64;
    args[1] = &mut event as *mut kernel_api_types::KeyEvent as u64;

    syscall(&mut args);

    event
}

pub fn sys_yield() {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Yield as u64;
    syscall(&mut args);
}

pub fn sys_mmap(size: u64, flags: u64) -> *mut u8 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Mmap as u64;
    args[1] = size;
    args[2] = flags;
    syscall(&mut args);
    args[6] as *mut u8
}

pub fn sys_munmap(addr: *mut u8, size: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Munmap as u64;
    args[1] = addr as u64;
    args[2] = size;
    syscall(&mut args);
    args[6]
}

pub fn sys_spawn(elf_bytes: &[u8], child_arg: u64) -> u64 {
    let mut args = [0u64; 7];
    args[0] = SysCallNumber::Spawn as u64;
    args[1] = elf_bytes.as_ptr() as u64;
    args[2] = elf_bytes.len() as u64;
    args[3] = child_arg;
    syscall(&mut args);
    args[6]
}

pub fn rgb888_to_raw(color: Rgb888) -> Rgb888Raw {
    ((color.r() as u32) << 16) | ((color.g() as u32) << 8) | (color.b() as u32)
}

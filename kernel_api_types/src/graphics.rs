pub type Rgb888Raw = u32; // 0x00RRGGBB

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PixelData {
    pub x: u32,
    pub y: u32,
    pub rgb_raw: Rgb888Raw,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Return code for graphics syscalls
#[repr(u64)]
#[derive(Clone, Copy, Debug)]
pub enum GraphicsResult {
    Ok = 0,
    OutOfBounds = 1,
    InvalidInput = 2,
}

impl GraphicsResult {
    pub fn from_u64(value: u64) -> Self {
        match value {
            x if x == GraphicsResult::Ok as u64 => GraphicsResult::Ok,
            x if x == GraphicsResult::OutOfBounds as u64 => GraphicsResult::OutOfBounds,
            _ => GraphicsResult::InvalidInput,
        }
    }
}

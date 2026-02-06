pub type Rgb888Raw = u32; // 0x00RRGGBB

/// Virtual address where the framebuffer is mapped after TransferDisplay syscall.
/// This is a canonical lower-half address (user space).
pub const FRAMEBUFFER_USER_VADDR: u64 = 0x7F00_0000_0000;

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
    PermissionDenied = 3,
}

impl GraphicsResult {
    pub fn from_u64(value: u64) -> Self {
        match value {
            x if x == GraphicsResult::Ok as u64 => GraphicsResult::Ok,
            x if x == GraphicsResult::OutOfBounds as u64 => GraphicsResult::OutOfBounds,
            x if x == GraphicsResult::PermissionDenied as u64 => GraphicsResult::PermissionDenied,
            _ => GraphicsResult::InvalidInput,
        }
    }
}

/// Display information returned by GetDisplayInfo syscall.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
}

impl DisplayInfo {
    /// Encode an RGB888 color into a u32 pixel value using the display's mask info.
    pub fn build_pixel(&self, r: u8, g: u8, b: u8) -> u32 {
        let mut n = 0u32;
        n |= ((r as u32) & ((1 << self.red_mask_size) - 1)) << self.red_mask_shift;
        n |= ((g as u32) & ((1 << self.green_mask_size) - 1)) << self.green_mask_shift;
        n |= ((b as u32) & ((1 << self.blue_mask_size) - 1)) << self.blue_mask_shift;
        n
    }
}

/// Window management IPC protocol for communicating with the display_server.
///
/// Clients send WindowMessage requests to the display_server via IPC channels,
/// and the server responds with WindowResponse messages.

/// Maximum window buffer size that can be sent via IPC (1MB)
pub const MAX_WINDOW_BUFFER_SIZE: usize = 1024 * 1024;

/// Window ID assigned by the display server
pub type WindowId = u64;

/// Client-to-server window management messages
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMessageType {
    /// Create a new window with given dimensions
    CreateWindow = 0,
    /// Update a window's pixel buffer
    UpdateWindow = 1,
    /// Close a window
    CloseWindow = 2,
    /// Move a window to a new position
    MoveWindow = 3,
    /// Resize a window
    ResizeWindow = 4,
    /// Bring window to front (change z-order)
    RaiseWindow = 5,
}

/// Create window request
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CreateWindowRequest {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

/// Update window request - includes dirty rectangle
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UpdateWindowRequest {
    pub window_id: WindowId,
    /// Width of the pixel buffer (in pixels, not bytes)
    pub buffer_width: u32,
    /// Dirty rectangle to update
    pub dirty_x: u32,
    pub dirty_y: u32,
    pub dirty_width: u32,
    pub dirty_height: u32,
    /// Number of pixels in the buffer that follows this header
    pub buffer_size: u32,
}

/// Close window request
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CloseWindowRequest {
    pub window_id: WindowId,
}

/// Move window request
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MoveWindowRequest {
    pub window_id: WindowId,
    pub x: i32,
    pub y: i32,
}

/// Resize window request
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ResizeWindowRequest {
    pub window_id: WindowId,
    pub width: u32,
    pub height: u32,
}

/// Raise window request
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RaiseWindowRequest {
    pub window_id: WindowId,
}

/// Server-to-client response codes
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowResult {
    Ok = 0,
    ErrorInvalidWindowId = 1,
    ErrorOutOfMemory = 2,
    ErrorInvalidDimensions = 3,
    ErrorBufferTooLarge = 4,
    ErrorInvalidMessage = 5,
}

impl WindowResult {
    pub fn from_u64(v: u64) -> Self {
        match v {
            0 => WindowResult::Ok,
            1 => WindowResult::ErrorInvalidWindowId,
            2 => WindowResult::ErrorOutOfMemory,
            3 => WindowResult::ErrorInvalidDimensions,
            4 => WindowResult::ErrorBufferTooLarge,
            5 => WindowResult::ErrorInvalidMessage,
            _ => WindowResult::ErrorInvalidMessage,
        }
    }

    pub fn is_ok(self) -> bool {
        matches!(self, WindowResult::Ok)
    }
}

/// Response to CreateWindow
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CreateWindowResponse {
    pub result: WindowResult,
    pub window_id: WindowId,
}

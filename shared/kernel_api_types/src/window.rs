/// Tracks the bounding box of dirty (modified) pixels that need to be flushed to the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl DirtyRect {
    /// Expand this dirty rect to include the region `(x, y, w, h)`.
    pub fn expand(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let x2 = self.x + self.w;
        let y2 = self.y + self.h;
        let new_x2 = (x + w).max(x2);
        let new_y2 = (y + h).max(y2);
        self.x = self.x.min(x);
        self.y = self.y.min(y);
        self.w = new_x2 - self.x;
        self.h = new_y2 - self.y;
    }
}

#[cfg(test)]
mod tests {
    use super::DirtyRect;

    #[test]
    fn expand_same_rect_is_noop() {
        let mut d = DirtyRect { x: 0, y: 0, w: 10, h: 10 };
        d.expand(0, 0, 10, 10);
        assert_eq!(d, DirtyRect { x: 0, y: 0, w: 10, h: 10 });
    }

    #[test]
    fn expand_grows_in_all_directions() {
        // Start with [5..10, 5..10], expand to include [0..15, 0..15]
        let mut d = DirtyRect { x: 5, y: 5, w: 5, h: 5 };
        d.expand(0, 0, 15, 15);
        assert_eq!(d, DirtyRect { x: 0, y: 0, w: 15, h: 15 });
    }

    #[test]
    fn expand_union_non_overlapping() {
        // [0..5, 0..5] union [10..15, 10..15] → [0..15, 0..15]
        let mut d = DirtyRect { x: 0, y: 0, w: 5, h: 5 };
        d.expand(10, 10, 5, 5);
        assert_eq!(d, DirtyRect { x: 0, y: 0, w: 15, h: 15 });
    }

    #[test]
    fn expand_contained_rect_is_noop() {
        // Outer [0..20, 0..20] expanded with inner [5..10, 5..10] → unchanged
        let mut d = DirtyRect { x: 0, y: 0, w: 20, h: 20 };
        d.expand(5, 5, 5, 5);
        assert_eq!(d, DirtyRect { x: 0, y: 0, w: 20, h: 20 });
    }
}

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
    /// Send window to back (change z-order)
    LowerWindow = 6,
    /// Mouse movement/button event forwarded by the mouse_reader task
    MouseInput = 7,
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

/// Update window request — dirty-rect notification only (no pixel data).
/// Pixels live in the shared buffer mapped at window creation time.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UpdateWindowRequest {
    pub window_id: WindowId,
    pub dirty_x: u32,
    pub dirty_y: u32,
    pub dirty_width: u32,
    pub dirty_height: u32,
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

/// Lower window request
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LowerWindowRequest {
    pub window_id: WindowId,
}

/// Mouse input message sent by the mouse_reader task to the display server.
/// dx/dy are relative movement deltas in PS/2 coordinates (positive dy = mouse moved up).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MouseInputMessage {
    pub dx: i16,
    pub dy: i16,
    pub buttons: u8,
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
    /// Opaque shared-buffer ID — client passes this to sys_map_shared_buf
    /// to get a writable pointer to the window's pixel backing store.
    pub shared_buf_id: u64,
}

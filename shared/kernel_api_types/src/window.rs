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

/// Window ID assigned by the display server
pub type WindowId = u64;

/// Client-to-server window management messages
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMessageType {
    /// Create a new toplevel window (DS assigns size via tiling)
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
    /// Create a panel anchored to a screen edge
    CreatePanel = 7,
}

/// Panel anchor edge
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelAnchor {
    Top    = 0,
    Bottom = 1,
    Left   = 2,
    Right  = 3,
}

/// DS-to-client event type sent over the event channel
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowEventType {
    KeyPress       = 0,
    FocusGained    = 1,
    FocusLost      = 2,
    Configure      = 3,
    /// Sent once per frame after display.present() completes, for every window whose
    /// pixels were composited that frame. Clients should wait for this before drawing
    /// the next frame so they pace themselves to the compositor's actual output rate.
    FramePresented = 4,
}

/// Create toplevel window request — DS assigns position and size via tiling.
/// Wire: [type=0: u8][CreateWindowRequest][reply_ep: u64]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CreateWindowRequest {
    /// Client's event receive channel send endpoint; DS keeps this open to push events.
    pub event_send_ep: u64,
}

/// Create panel request — client specifies anchor and size.
/// Wire: [type=7: u8][CreatePanelRequest][reply_ep: u64]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CreatePanelRequest {
    pub anchor: u8,
    pub _pad: [u8; 3],
    pub exclusive_zone: u32,
    pub width: u32,
    pub height: u32,
    pub event_send_ep: u64,
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

/// Response to CreateWindow / CreatePanel
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CreateWindowResponse {
    pub result: WindowResult,
    pub window_id: WindowId,
    /// Opaque shared-buffer ID — client passes this to sys_map_shared_buf
    /// to get a writable pointer to the window's pixel backing store.
    pub shared_buf_id: u64,
    /// DS-assigned dimensions
    pub width: u32,
    pub height: u32,
}

// --- DS-to-client event structs (sent over the event channel) ---

/// Key press event from DS to focused window.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KeyPressEvent {
    pub event_type: u8,  // WindowEventType::KeyPress
    pub key: crate::KeyEvent,
}

/// Focus gained/lost event.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FocusEvent {
    pub event_type: u8,  // WindowEventType::FocusGained or FocusLost
}

/// Configure event: DS has resized the window and allocated a new shared buffer.
/// Client must call apply_configure() to map the new buffer and start using it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ConfigureEvent {
    pub event_type: u8,  // WindowEventType::Configure
    pub _pad: [u8; 3],
    pub width: u32,
    pub height: u32,
    pub shared_buf_id: u64,
}

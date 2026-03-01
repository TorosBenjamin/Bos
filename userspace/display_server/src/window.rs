use kernel_api_types::window::{DirtyRect, WindowId};

pub struct Window {
    pub id: WindowId,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Pointer into the shared physical buffer (readable by the compositor).
    pub buffer: *mut u32,
    /// Opaque ID returned to the client in CreateWindowResponse so it can map the same pages.
    pub shared_buf_id: u64,
    /// Size in bytes — needed to call sys_munmap before destroying the shared buf.
    pub buf_size: u64,
    /// DS sends events (key presses, focus, configure) here.
    pub event_send_ep: u64,
    /// True for panels anchored to a screen edge.
    pub is_panel: bool,
    /// PanelAnchor value (only meaningful when is_panel == true).
    pub anchor: u8,
    /// Pixels to subtract from available area for Toplevels (panels only).
    pub exclusive_zone: u32,
    /// Old shared_buf_id awaiting sys_destroy_shared_buf after the client acknowledges Configure.
    pub pending_old_buf_id: Option<u64>,
    /// Dirty region (screen coordinates) from the latest UpdateWindow, pending compositing.
    /// Stored per-window to avoid merging two distant windows into one huge bounding box.
    pub pending_dirty: Option<DirtyRect>,
}

impl Window {
    pub fn new(
        id: WindowId,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        event_send_ep: u64,
    ) -> Option<Self> {
        let buf_size = (width as u64) * (height as u64) * 4;
        let (shared_buf_id, buffer_ptr) = ulib::sys_create_shared_buf(buf_size);
        if buffer_ptr.is_null() || shared_buf_id == u64::MAX {
            return None;
        }
        Some(Window {
            id,
            x,
            y,
            width,
            height,
            buffer: buffer_ptr as *mut u32,
            shared_buf_id,
            buf_size,
            event_send_ep,
            is_panel: false,
            anchor: 0,
            exclusive_zone: 0,
            pending_old_buf_id: None,
            pending_dirty: None,
        })
    }

    /// Update position/size. Allocates a new shared buffer if dimensions changed.
    ///
    /// Returns `true` if the buffer was reallocated (client must receive Configure event),
    /// or `false` if only x/y changed (no Configure needed).
    pub fn reconfigure(&mut self, new_x: i32, new_y: i32, new_w: u32, new_h: u32) -> bool {
        self.x = new_x;
        self.y = new_y;

        if new_w == self.width && new_h == self.height {
            return false;
        }

        let new_buf_size = (new_w as u64) * (new_h as u64) * 4;
        let (new_shared_buf_id, new_buf_ptr) = ulib::sys_create_shared_buf(new_buf_size);
        if new_buf_ptr.is_null() || new_shared_buf_id == u64::MAX {
            // Allocation failed; keep old dimensions and position
            self.x = new_x;
            self.y = new_y;
            return false;
        }

        // Unmap old buffer from DS address space (physical pages kept alive by shared_buf object)
        ulib::sys_munmap(self.buffer as *mut u8, self.buf_size);

        // Store old shared_buf_id so we can destroy it after the client acknowledges
        self.pending_old_buf_id = Some(self.shared_buf_id);

        self.width = new_w;
        self.height = new_h;
        self.buf_size = new_buf_size;
        self.shared_buf_id = new_shared_buf_id;
        self.buffer = new_buf_ptr as *mut u32;
        true
    }
}

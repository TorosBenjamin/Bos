use super::{Compositor, MAX_WINDOWS, MIN_RATIO};
use kernel_api_types::window::{ConfigureEvent, WindowEventType, WindowId};

/// Four-state direction for the golden-ratio spiral layout.
///
/// The sequence H → V → HR → VR → H cycles the split orientation and which
/// side the *existing* (older) windows occupy. The newest window always appears
/// on the opposite side, producing the classic golden-spiral order:
///   1st window: full screen
///   2nd window: RIGHT  (Horizontal split, older → left)
///   3rd window: BOTTOM (Vertical split, older → top)
///   4th window: LEFT   (HorizontalReversed, older → right)
///   5th window: TOP    (VerticalReversed, older → bottom)
#[derive(Clone, Copy, PartialEq)]
pub enum LayoutDir {
    /// Split line vertical; window[0] occupies the LEFT portion.
    Horizontal,
    /// Split line horizontal; window[0] occupies the TOP portion.
    Vertical,
    /// Split line vertical; window[0] occupies the RIGHT portion (new window → left).
    HorizontalReversed,
    /// Split line horizontal; window[0] occupies the BOTTOM portion (new window → top).
    VerticalReversed,
}

impl LayoutDir {
    /// Advance to the next direction in the golden-spiral cycle.
    pub fn next_spiral(self) -> Self {
        match self {
            Self::Horizontal         => Self::Vertical,
            Self::Vertical           => Self::HorizontalReversed,
            Self::HorizontalReversed => Self::VerticalReversed,
            Self::VerticalReversed   => Self::Horizontal,
        }
    }


}

/// Recursively compute golden-spiral (4-state-split) positions.
/// `windows[0]` takes `ratios[0]` fraction of the rect, on the side determined
/// by `dir`. The rest recurse with `dir.next_spiral()`.
#[allow(clippy::too_many_arguments)]
fn dwindle_recurse(
    windows:   &[WindowId],
    ratios:    &[f32],
    x: i32, y: i32, w: u32, h: u32,
    dir:       LayoutDir,
    inner_gap: u32,
    positions: &mut [(i32, i32, u32, u32)],
) {
    if windows.is_empty() { return; }
    if windows.len() == 1 {
        positions[0] = (x, y, w, h);
        return;
    }
    let ratio = ratios[0].clamp(MIN_RATIO, 1.0 - MIN_RATIO);
    let next  = dir.next_spiral();
    match dir {
        LayoutDir::Horizontal => {
            // window[0] LEFT, rest RIGHT
            let lw = ((w as f32 * ratio) as u32).min(w.saturating_sub(inner_gap + 1));
            let rw = w.saturating_sub(lw + inner_gap);
            positions[0] = (x, y, lw, h);
            dwindle_recurse(&windows[1..], &ratios[1..], x + lw as i32 + inner_gap as i32, y, rw, h, next, inner_gap, &mut positions[1..]);
        }
        LayoutDir::Vertical => {
            // window[0] TOP, rest BOTTOM
            let th = ((h as f32 * ratio) as u32).min(h.saturating_sub(inner_gap + 1));
            let bh = h.saturating_sub(th + inner_gap);
            positions[0] = (x, y, w, th);
            dwindle_recurse(&windows[1..], &ratios[1..], x, y + th as i32 + inner_gap as i32, w, bh, next, inner_gap, &mut positions[1..]);
        }
        LayoutDir::HorizontalReversed => {
            // window[0] RIGHT, rest LEFT
            let rw = ((w as f32 * ratio) as u32).min(w.saturating_sub(inner_gap + 1));
            let lw = w.saturating_sub(rw + inner_gap);
            positions[0] = (x + lw as i32 + inner_gap as i32, y, rw, h);
            dwindle_recurse(&windows[1..], &ratios[1..], x, y, lw, h, next, inner_gap, &mut positions[1..]);
        }
        LayoutDir::VerticalReversed => {
            // window[0] BOTTOM, rest TOP
            let bh = ((h as f32 * ratio) as u32).min(h.saturating_sub(inner_gap + 1));
            let th = h.saturating_sub(bh + inner_gap);
            positions[0] = (x, y + th as i32 + inner_gap as i32, w, bh);
            dwindle_recurse(&windows[1..], &ratios[1..], x, y, w, th, next, inner_gap, &mut positions[1..]);
        }
    }
}

impl Compositor {
    pub(super) fn tiled_ids(&self) -> ([WindowId; MAX_WINDOWS], usize) {
        let mut ids = [0u64; MAX_WINDOWS];
        let mut n = 0usize;
        for i in 0..self.n_windows {
            let id = self.z_order[i];
            let is_tiled = self.windows.iter()
                .filter_map(|w| w.as_ref())
                .find(|w| w.id == id)
                .map(|w| !w.is_panel && !w.is_floating)
                .unwrap_or(false);
            if is_tiled {
                ids[n] = id;
                n += 1;
            }
        }
        (ids, n)
    }

    pub(super) fn reset_tiled_ratios(&mut self, n: usize) {
        if n == 0 {
            self.n_tiled_ratios = 0;
            return;
        }
        for i in 0..n {
            self.tiled_ratios[i] = 0.5;
        }
        self.n_tiled_ratios = n;
    }

    pub(super) fn count_toplevels(&self) -> usize {
        self.windows.iter()
            .filter_map(|w| w.as_ref())
            .filter(|w| !w.is_panel && !w.is_floating)
            .count()
    }

    /// Returns `(x, y, w, h)` — the screen area available to Toplevels after panels claim their edges.
    pub(super) fn available_area(&self) -> (i32, i32, u32, u32) {
        let mut ax = 0i32;
        let mut ay = 0i32;
        let mut aw = self.display_info.width;
        let mut ah = self.display_info.height;

        for slot in &self.windows {
            if let Some(w) = slot && w.is_panel {
                match w.anchor {
                    0 => { // Top
                        let zone = w.exclusive_zone.min(ah);
                        ay += zone as i32;
                        ah -= zone;
                    }
                    1 => { // Bottom
                        ah -= w.exclusive_zone.min(ah);
                    }
                    2 => { // Left
                        let zone = w.exclusive_zone.min(aw);
                        ax += zone as i32;
                        aw -= zone;
                    }
                    3 => { // Right
                        aw -= w.exclusive_zone.min(aw);
                    }
                    _ => {}
                }
            }
        }

        (ax, ay, aw, ah)
    }

    /// Returns the split direction at tiling level `index` (0 = first split).
    pub(super) fn dir_at_level(&self, index: usize) -> LayoutDir {
        let mut dir = self.layout_dir;
        for _ in 0..index { dir = dir.next_spiral(); }
        dir
    }

    /// Returns the usable span (pixels) available at split level `index`.
    /// Used by drag-resize to map cursor delta → ratio delta correctly.
    pub(super) fn level_span_at(&self, index: usize) -> u32 {
        let (_, _, aw, ah) = self.available_area();
        let og = self.outer_gap;
        let ig = self.inner_gap;
        let mut w = aw.saturating_sub(2 * og);
        let mut h = ah.saturating_sub(2 * og);
        let mut dir = self.layout_dir;

        for i in 0..index {
            let ratio = self.tiled_ratios[i].clamp(MIN_RATIO, 1.0 - MIN_RATIO);
            // H and HR both consume (w * ratio) from the width; V and VR from height.
            match dir {
                LayoutDir::Horizontal | LayoutDir::HorizontalReversed => {
                    let used = (w as f32 * ratio) as u32;
                    w = w.saturating_sub(used + ig);
                }
                LayoutDir::Vertical | LayoutDir::VerticalReversed => {
                    let used = (h as f32 * ratio) as u32;
                    h = h.saturating_sub(used + ig);
                }
            }
            dir = dir.next_spiral();
        }

        match dir {
            LayoutDir::Horizontal | LayoutDir::HorizontalReversed => w,
            LayoutDir::Vertical   | LayoutDir::VerticalReversed   => h,
        }
    }

    /// Redistribute tile space among all Toplevels using the dwindle (alternating-split) algorithm.
    /// Sends Configure events to any window whose buffer dimensions change.
    pub(super) fn recalculate_toplevel_layout(&mut self) {
        let (tiled, n) = self.tiled_ids();
        if n == 0 {
            return;
        }

        if self.n_tiled_ratios != n {
            self.reset_tiled_ratios(n);
        }

        let (ax, ay, aw, ah) = self.available_area();
        let og = self.outer_gap;
        let ig = self.inner_gap;

        let gx = ax + og as i32;
        let gy = ay + og as i32;
        let gw = aw.saturating_sub(2 * og);
        let gh = ah.saturating_sub(2 * og);

        let mut positions = [(0i32, 0i32, 0u32, 0u32); MAX_WINDOWS];
        dwindle_recurse(
            &tiled[..n],
            &self.tiled_ratios[..n],
            gx, gy, gw, gh,
            self.layout_dir,
            ig,
            &mut positions[..n],
        );

        // Apply reconfigure and collect pending configure events
        let mut pending: [(u64, ConfigureEvent); MAX_WINDOWS] = [(0, ConfigureEvent {
            event_type: WindowEventType::Configure as u8,
            _pad: [0; 3],
            width: 0,
            height: 0,
            shared_buf_id: 0,
        }); MAX_WINDOWS];
        let mut n_pending = 0usize;

        for i in 0..n {
            let id = tiled[i];
            let (new_x, new_y, new_w, new_h) = positions[i];
            if let Some(window) = self.windows.iter_mut()
                .filter_map(|w| w.as_mut())
                .find(|w| w.id == id)
            {
                let reconfigured = window.reconfigure(new_x, new_y, new_w, new_h);
                if reconfigured && n_pending < MAX_WINDOWS {
                    pending[n_pending] = (
                        window.event_send_ep,
                        ConfigureEvent {
                            event_type: WindowEventType::Configure as u8,
                            _pad: [0; 3],
                            width: new_w,
                            height: new_h,
                            shared_buf_id: window.shared_buf_id,
                        },
                    );
                    n_pending += 1;
                }
            }
        }

        for (ep, ev) in &pending[..n_pending] {
            super::send_event(*ep, ev);
        }

        self.mark_full_redraw();
    }
}

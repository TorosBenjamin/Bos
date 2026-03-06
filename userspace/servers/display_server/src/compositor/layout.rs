use super::{Compositor, MAX_WINDOWS};
use kernel_api_types::window::{ConfigureEvent, WindowEventType, WindowId};

#[derive(Clone, Copy, PartialEq)]
pub enum LayoutDir {
    Horizontal,
    Vertical,
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
        let ratio = 1.0f32 / n as f32;
        for i in 0..n {
            self.tiled_ratios[i] = ratio;
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
            if let Some(w) = slot {
                if !w.is_panel {
                    continue;
                }
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

    /// Redistribute tile space among all Toplevels using per-window ratios and layout direction.
    /// Sends Configure events to any window whose buffer dimensions change.
    pub(super) fn recalculate_toplevel_layout(&mut self) {
        let (tiled, n) = self.tiled_ids();
        if n == 0 {
            return;
        }

        // Sync ratios if window count changed
        if self.n_tiled_ratios != n {
            self.reset_tiled_ratios(n);
        }

        let (ax, ay, aw, ah) = self.available_area();
        let total_gaps = 2 * self.outer_gap + (n as u32 - 1) * self.inner_gap;

        let layout_dir = self.layout_dir;
        let outer_gap = self.outer_gap;
        let inner_gap = self.inner_gap;

        let (usable_w, usable_h) = match layout_dir {
            LayoutDir::Horizontal => (
                aw.saturating_sub(total_gaps),
                ah.saturating_sub(2 * outer_gap),
            ),
            LayoutDir::Vertical => (
                aw.saturating_sub(2 * outer_gap),
                ah.saturating_sub(total_gaps),
            ),
        };

        // Pre-compute positions for all tiled windows
        let mut positions = [(0i32, 0i32, 0u32, 0u32); MAX_WINDOWS];
        let mut accum = 0i32;
        for i in 0..n {
            let ratio = self.tiled_ratios[i];
            let (x, y, w, h) = match layout_dir {
                LayoutDir::Horizontal => {
                    let tw = if i == n - 1 {
                        usable_w.saturating_sub(accum as u32)
                    } else {
                        (ratio * usable_w as f32) as u32
                    };
                    let pos = (ax + outer_gap as i32 + accum, ay + outer_gap as i32, tw, usable_h);
                    accum += tw as i32 + inner_gap as i32;
                    pos
                }
                LayoutDir::Vertical => {
                    let th = if i == n - 1 {
                        usable_h.saturating_sub(accum as u32)
                    } else {
                        (ratio * usable_h as f32) as u32
                    };
                    let pos = (ax + outer_gap as i32, ay + outer_gap as i32 + accum, usable_w, th);
                    accum += th as i32 + inner_gap as i32;
                    pos
                }
            };
            positions[i] = (x, y, w, h);
        }

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

        for j in 0..n_pending {
            let (ep, ref ev) = pending[j];
            super::send_event(ep, ev);
        }

        self.mark_full_redraw();
    }
}

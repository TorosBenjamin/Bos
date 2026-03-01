use super::{Compositor, MAX_WINDOWS, OUTER_GAP, INNER_GAP};
use kernel_api_types::window::{ConfigureEvent, WindowEventType};

impl Compositor {
    pub(super) fn count_toplevels(&self) -> usize {
        self.windows.iter()
            .filter_map(|w| w.as_ref())
            .filter(|w| !w.is_panel)
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

    /// Redistribute horizontal tile space among all Toplevels. Sends Configure events to
    /// any window whose buffer dimensions change.
    pub(super) fn recalculate_toplevel_layout(&mut self) {
        let n = self.count_toplevels();
        if n == 0 {
            return;
        }

        let (ax, ay, aw, ah) = self.available_area();
        // Distribute gaps: OUTER_GAP on every side, INNER_GAP between adjacent windows.
        let total_h_gaps = 2 * OUTER_GAP + (n as u32 - 1) * INNER_GAP;
        let usable_w = aw.saturating_sub(total_h_gaps);
        let usable_h = ah.saturating_sub(2 * OUTER_GAP);
        let tile_w = usable_w / n as u32;

        // Collect (event_send_ep, ConfigureEvent) for windows that need a new buffer.
        // Use a fixed-size array since we're no_std.
        let mut pending: [(u64, ConfigureEvent); MAX_WINDOWS] = [(0, ConfigureEvent {
            event_type: WindowEventType::Configure as u8,
            _pad: [0; 3],
            width: 0,
            height: 0,
            shared_buf_id: 0,
        }); MAX_WINDOWS];
        let mut n_pending = 0usize;

        let mut i = 0usize;
        for slot in &mut self.windows {
            if let Some(window) = slot {
                if window.is_panel {
                    continue;
                }
                let new_x = ax + OUTER_GAP as i32 + (i as u32 * (tile_w + INNER_GAP)) as i32;
                let new_y = ay + OUTER_GAP as i32;
                // Last toplevel gets any remaining pixels so rounding doesn't leave a sliver
                let new_w = if i == n - 1 { usable_w - tile_w * i as u32 } else { tile_w };
                let new_h = usable_h;

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
                i += 1;
            }
        }

        for j in 0..n_pending {
            let (ep, ref ev) = pending[j];
            super::send_event(ep, ev);
        }

        self.mark_full_redraw();
    }
}

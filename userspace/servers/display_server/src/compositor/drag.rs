use kernel_api_types::window::*;
use super::{Compositor, DragKind, DragState, MAX_WINDOWS, MIN_SIZE, MIN_RATIO, send_event};
use super::layout::LayoutDir;

impl Compositor {
    /// Move tiled window `id` to the front of the tiled zone (z_order[0]).
    pub(super) fn move_tiled_to_front(&mut self, id: WindowId) {
        self.z_remove(id);
        if self.n_windows < MAX_WINDOWS {
            for i in (0..self.n_windows).rev() {
                self.z_order[i + 1] = self.z_order[i];
            }
            self.z_order[0] = id;
            self.n_windows += 1;
        }
    }

    /// Move tiled window `id` to the back of the tiled zone (just before first float/panel).
    pub(super) fn move_tiled_to_back(&mut self, id: WindowId) {
        self.z_remove(id);
        // Find end of tiled zone
        let tiled_end = (0..self.n_windows)
            .find(|&i| {
                let zid = self.z_order[i];
                self.windows.iter().filter_map(|w| w.as_ref())
                    .find(|w| w.id == zid)
                    .map(|w| w.is_floating || w.is_panel)
                    .unwrap_or(false)
            })
            .unwrap_or(self.n_windows);
        if self.n_windows < MAX_WINDOWS {
            for i in (tiled_end..self.n_windows).rev() {
                self.z_order[i + 1] = self.z_order[i];
            }
            self.z_order[tiled_end] = id;
            self.n_windows += 1;
        }
    }

    pub(super) fn swap_tiled_windows(&mut self, id1: WindowId, id2: WindowId) {
        let pos1 = self.z_order[..self.n_windows].iter().position(|&x| x == id1);
        let pos2 = self.z_order[..self.n_windows].iter().position(|&x| x == id2);
        if let (Some(p1), Some(p2)) = (pos1, pos2) {
            self.z_order.swap(p1, p2);
        }
    }

    pub(super) fn start_move_drag(&mut self, id: WindowId) {
        let info = self.windows.iter()
            .filter_map(|w| w.as_ref())
            .find(|w| w.id == id)
            .map(|w| (w.x, w.y, w.is_floating));
        let (wx, wy, is_floating) = match info { Some(v) => v, None => return };

        let kind = if is_floating {
            DragKind::MoveFloating { start_x: wx, start_y: wy }
        } else {
            DragKind::MoveTiled
        };
        self.drag_state = Some(DragState {
            window_id: id,
            kind,
            start_cx: self.cursor_x,
            start_cy: self.cursor_y,
        });
    }

    pub(super) fn start_resize_drag(&mut self, id: WindowId) {
        let info = self.windows.iter()
            .filter_map(|w| w.as_ref())
            .find(|w| w.id == id)
            .map(|w| (w.x, w.y, w.width, w.height, w.is_floating));
        let (wx, wy, ww, wh, is_floating) = match info { Some(v) => v, None => return };

        let resize_left = self.cursor_x < wx + ww as i32 / 2;
        let resize_top  = self.cursor_y < wy + wh as i32 / 2;

        let kind = if is_floating {
            DragKind::ResizeFloating {
                start_x: wx, start_y: wy,
                start_w: ww, start_h: wh,
                resize_left, resize_top,
            }
        } else {
            let (tiled, n_tiled) = self.tiled_ids();
            let tiled_index = match (0..n_tiled).find(|&i| tiled[i] == id) {
                Some(idx) => idx,
                None => return,
            };
            let mut start_ratios = [0.0f32; MAX_WINDOWS];
            start_ratios[..n_tiled].copy_from_slice(&self.tiled_ratios[..n_tiled]);
            DragKind::ResizeTiled { tiled_index, start_ratios, n_tiled }
        };
        self.drag_state = Some(DragState {
            window_id: id,
            kind,
            start_cx: self.cursor_x,
            start_cy: self.cursor_y,
        });
    }

    pub(super) fn update_drag(&mut self) {
        let drag = match self.drag_state { Some(d) => d, None => return };

        match drag.kind {
            DragKind::MoveFloating { start_x, start_y } => {
                let new_x = (start_x + (self.cursor_x - drag.start_cx))
                    .clamp(0, self.display_info.width as i32 - 1);
                let new_y = (start_y + (self.cursor_y - drag.start_cy))
                    .clamp(0, self.display_info.height as i32 - 1);

                // Read old geometry before the mutable borrow.
                let old_info = self.windows.iter()
                    .filter_map(|w| w.as_ref())
                    .find(|w| w.id == drag.window_id)
                    .map(|w| (w.x, w.y, w.width, w.height));

                if let Some((old_x, old_y, ww, wh)) = old_info && (old_x != new_x || old_y != new_y) {
                    if let Some(w) = self.windows.iter_mut()
                        .filter_map(|w| w.as_mut())
                        .find(|w| w.id == drag.window_id)
                    {
                        w.x = new_x;
                        w.y = new_y;
                    }

                    // Dirty = old_rect ∪ new_rect, both padded by border_width.
                    // Avoids a full-screen redraw: only the vacated area and the
                    // new area need compositing, typically a small delta per frame.
                    let bw = self.border_width;
                    let bwu = bw.max(0) as u32;
                    let old_rect = self.screen_rect(old_x - bw, old_y - bw, ww + 2 * bwu, wh + 2 * bwu);
                    let new_rect = self.screen_rect(new_x - bw, new_y - bw, ww + 2 * bwu, wh + 2 * bwu);
                    let damage = match (old_rect, new_rect) {
                        (Some(mut a), Some(b)) => { a.expand(b.x, b.y, b.w, b.h); Some(a) }
                        (Some(a), None) => Some(a),
                        (None, Some(b)) => Some(b),
                        (None, None) => None,
                    };
                    if let Some(d) = damage {
                        self.mark_damage(d);
                    }
                }
            }
            DragKind::MoveTiled => {
                // Live swap: the dragged window follows the cursor by swapping
                // z-order positions with whichever tiled window is under it.
                // After the swap, the dragged window occupies the cursor's tile,
                // so hit_test_tiled returns the dragged window itself until the
                // cursor moves into a different tile — preventing repeated swaps.
                if let Some(target_id) = self.hit_test_tiled(self.cursor_x, self.cursor_y) && target_id != drag.window_id {
                    self.swap_tiled_windows(drag.window_id, target_id);
                    self.recalculate_toplevel_layout();
                }
            }
            DragKind::ResizeFloating { start_x, start_y, start_w, start_h, resize_left, resize_top } => {
                let dx = self.cursor_x - drag.start_cx;
                let dy = self.cursor_y - drag.start_cy;

                let (new_x, new_w) = if resize_left {
                    let clamped_h = (start_w as i32 - dx).max(MIN_SIZE as i32) as u32;
                    let actual_dx = start_w as i32 - clamped_h as i32;
                    (start_x + actual_dx, clamped_h)
                } else {
                    (start_x, (start_w as i32 + dx).max(MIN_SIZE as i32) as u32)
                };

                let (new_y, new_h) = if resize_top {
                    let clamped_h = (start_h as i32 - dy).max(MIN_SIZE as i32) as u32;
                    let actual_dy = start_h as i32 - clamped_h as i32;
                    (start_y + actual_dy, clamped_h)
                } else {
                    (start_y, (start_h as i32 + dy).max(MIN_SIZE as i32) as u32)
                };

                let mut configure_info: Option<(u64, u32, u32, u64)> = None;
                if let Some(w) = self.windows.iter_mut()
                    .filter_map(|w| w.as_mut())
                    .find(|w| w.id == drag.window_id)
                    && w.reconfigure(new_x, new_y, new_w, new_h)
                {
                    configure_info = Some((w.event_send_ep, new_w, new_h, w.shared_buf_id));
                }
                if let Some((ep, w, h, buf_id)) = configure_info {
                    send_event(ep, &ConfigureEvent {
                        event_type: WindowEventType::Configure as u8,
                        _pad: [0; 3],
                        width: w,
                        height: h,
                        shared_buf_id: buf_id,
                    });
                }
                self.mark_full_redraw();
            }
            DragKind::ResizeTiled { tiled_index, start_ratios, n_tiled } => {
                if n_tiled < 2 { return; }

                // In dwindle, each tiled_ratios[i] is the fraction of its own level's
                // area taken by window i. Resize only adjusts that one ratio.
                let dir  = self.dir_at_level(tiled_index);
                let span = self.level_span_at(tiled_index) as f32;

                let delta = match dir {
                    // For normal splits, dragging toward the larger side grows window[0].
                    LayoutDir::Horizontal => self.cursor_x - drag.start_cx,
                    LayoutDir::Vertical   => self.cursor_y - drag.start_cy,
                    // For reversed splits, window[0] is on the opposite side, so negate.
                    LayoutDir::HorizontalReversed => drag.start_cx - self.cursor_x,
                    LayoutDir::VerticalReversed   => drag.start_cy - self.cursor_y,
                };
                let ratio_delta = if span > 0.0 { delta as f32 / span } else { 0.0 };
                let new_ratio = (start_ratios[tiled_index] + ratio_delta)
                    .clamp(MIN_RATIO, 1.0 - MIN_RATIO);

                self.tiled_ratios[tiled_index] = new_ratio;
                self.recalculate_toplevel_layout();
            }
        }
    }

    pub(super) fn apply_tiled_drop(&mut self, id: WindowId, cx: i32, cy: i32) {
        let (ax, ay, aw, ah) = self.available_area();
        let zone_w = (aw / 5) as i32;
        let zone_h = (ah / 5) as i32;

        let (_, n_tiled) = self.tiled_ids();

        if cx < ax + zone_w {
            self.layout_dir = LayoutDir::Horizontal;
            self.move_tiled_to_front(id);
        } else if cx > ax + aw as i32 - zone_w {
            self.layout_dir = LayoutDir::Horizontal;
            self.move_tiled_to_back(id);
        } else if cy < ay + zone_h {
            self.layout_dir = LayoutDir::Vertical;
            self.move_tiled_to_front(id);
        } else if cy > ay + ah as i32 - zone_h {
            self.layout_dir = LayoutDir::Vertical;
            self.move_tiled_to_back(id);
        } else {
            // Swap with the tiled window under the cursor
            if let Some(target) = self.hit_test_tiled(cx, cy) && target != id {
                self.swap_tiled_windows(id, target);
            }
        }

        self.reset_tiled_ratios(n_tiled);
        self.recalculate_toplevel_layout();
        self.mark_full_redraw();
    }
}

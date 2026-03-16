use kernel_api_types::window::*;
use super::{Compositor, MAX_WINDOWS};

impl Compositor {
    // --- Z-order helpers ---

    pub(super) fn z_push(&mut self, id: WindowId) {
        if self.n_windows < MAX_WINDOWS {
            self.z_order[self.n_windows] = id;
            self.n_windows += 1;
        }
    }

    pub(super) fn z_remove(&mut self, id: WindowId) {
        if let Some(pos) = self.z_order[..self.n_windows].iter().position(|&x| x == id) {
            for i in pos..self.n_windows - 1 {
                self.z_order[i] = self.z_order[i + 1];
            }
            self.n_windows -= 1;
        }
    }

    /// Push a Toplevel window into the correct z-zone:
    ///   tiled windows  <  floating windows  <  panels  (bottom → top)
    ///
    /// A floating window is inserted just below the first panel (top of floating zone).
    /// A tiled window is inserted just below the first floating window or panel
    /// (top of tiled zone, still below all floats).
    pub(super) fn z_push_toplevel(&mut self, id: WindowId) {
        let is_floating = self.windows.iter()
            .filter_map(|w| w.as_ref())
            .find(|w| w.id == id)
            .map(|w| w.is_floating)
            .unwrap_or(false);

        let insert_pos = if is_floating {
            // Insert below the first panel, above all tiled windows and other floats.
            (0..self.n_windows)
                .find(|&i| {
                    let zid = self.z_order[i];
                    self.windows.iter().filter_map(|w| w.as_ref())
                        .find(|w| w.id == zid)
                        .map(|w| w.is_panel)
                        .unwrap_or(false)
                })
                .unwrap_or(self.n_windows)
        } else {
            // Insert below the first floating window or panel, above all other tiled windows.
            (0..self.n_windows)
                .find(|&i| {
                    let zid = self.z_order[i];
                    self.windows.iter().filter_map(|w| w.as_ref())
                        .find(|w| w.id == zid)
                        .map(|w| w.is_floating || w.is_panel)
                        .unwrap_or(false)
                })
                .unwrap_or(self.n_windows)
        };

        if self.n_windows < MAX_WINDOWS {
            for i in (insert_pos..self.n_windows).rev() {
                self.z_order[i + 1] = self.z_order[i];
            }
            self.z_order[insert_pos] = id;
            self.n_windows += 1;
        }
    }

    pub(super) fn z_raise(&mut self, id: WindowId) {
        let is_panel = self.windows.iter()
            .filter_map(|w| w.as_ref())
            .find(|w| w.id == id)
            .map(|w| w.is_panel)
            .unwrap_or(false);

        self.z_remove(id);
        if is_panel {
            self.z_push(id);
        } else {
            self.z_push_toplevel(id);
        }
    }

    pub(super) fn z_lower(&mut self, id: WindowId) {
        self.z_remove(id);
        if self.n_windows < MAX_WINDOWS {
            for i in (0..self.n_windows).rev() {
                self.z_order[i + 1] = self.z_order[i];
            }
            self.z_order[0] = id;
            self.n_windows += 1;
        }
    }

    // --- Focus management ---

    /// Hit-test (x, y) against Toplevels in z-order (top-to-bottom). Panels and hidden windows are skipped.
    pub(super) fn hit_test(&self, x: i32, y: i32) -> Option<WindowId> {
        for i in (0..self.n_windows).rev() {
            let id = self.z_order[i];
            if let Some(w) = self.windows.iter().filter_map(|w| w.as_ref()).find(|w| w.id == id) {
                if w.is_panel || w.hidden {
                    continue;
                }
                if x >= w.x && x < w.x + w.width as i32
                    && y >= w.y && y < w.y + w.height as i32
                {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Hit-test against tiled (non-floating, non-panel) windows only.
    pub(super) fn hit_test_tiled(&self, x: i32, y: i32) -> Option<WindowId> {
        for i in (0..self.n_windows).rev() {
            let id = self.z_order[i];
            if let Some(w) = self.windows.iter().filter_map(|w| w.as_ref()).find(|w| w.id == id) {
                if w.is_panel || w.is_floating {
                    continue;
                }
                if x >= w.x && x < w.x + w.width as i32
                    && y >= w.y && y < w.y + w.height as i32
                {
                    return Some(id);
                }
            }
        }
        None
    }

    pub(super) fn set_focus(&mut self, new_id: Option<WindowId>) {
        // Don't focus a hidden window
        let new_id = new_id.filter(|&id| {
            !self.windows.iter().filter_map(|w| w.as_ref()).find(|w| w.id == id).map(|w| w.hidden).unwrap_or(false)
        });

        if self.focused_window == new_id {
            return;
        }

        let old_id = self.focused_window;
        self.focused_window = new_id;

        if let Some(old_id) = old_id {
            let ep = self.window_event_ep(old_id);
            if ep != 0 {
                ulib::sys_try_channel_send(ep, &[WindowEventType::FocusLost as u8]);
            }
        }

        if let Some(new_id) = new_id {
            let ep = self.window_event_ep(new_id);
            if ep != 0 {
                ulib::sys_try_channel_send(ep, &[WindowEventType::FocusGained as u8]);
            }
        }

        // Border colours change on focus change — rebuild the scene.
        self.mark_full_redraw();
    }

    pub(super) fn window_event_ep(&self, id: WindowId) -> u64 {
        self.windows.iter()
            .filter_map(|w| w.as_ref())
            .find(|w| w.id == id)
            .map(|w| w.event_send_ep)
            .unwrap_or(0)
    }

    // --- Keyboard shortcuts ---

    /// Check `key` against configured shortcuts. Returns `true` if the event was consumed.
    pub(super) fn handle_shortcut(&mut self, key: &kernel_api_types::KeyEvent) -> bool {
        if !key.pressed {
            return false;
        }
        use kernel_api_types::KeyEventType;
        for i in 0..self.n_shortcuts {
            if let Some(b) = self.shortcuts[i] {
                if key.modifiers != b.modifiers { continue; }
                if key.event_type != b.key_type  { continue; }
                if b.key_type == KeyEventType::Char
                    && key.character.to_ascii_lowercase() != b.character
                {
                    continue;
                }
                match b.action {
                    crate::compositor_config::ShortcutAction::CloseWindow => {
                        if let Some(id) = self.focused_window && !self.is_protected(id) {
                                self.initiate_close(id);
                        }
                    }
                    crate::compositor_config::ShortcutAction::FocusNext
                    | crate::compositor_config::ShortcutAction::FocusRight
                    | crate::compositor_config::ShortcutAction::FocusDown => {
                        self.cycle_focus(true);
                    }
                    crate::compositor_config::ShortcutAction::FocusPrev
                    | crate::compositor_config::ShortcutAction::FocusLeft
                    | crate::compositor_config::ShortcutAction::FocusUp => {
                        self.cycle_focus(false);
                    }
                    crate::compositor_config::ShortcutAction::ToggleLauncher => {
                        if let Some(wid) = self.find_by_app_id(b"launcher") {
                            let is_hidden = self.windows.iter()
                                .filter_map(|w| w.as_ref())
                                .find(|w| w.id == wid)
                                .map(|w| w.hidden)
                                .unwrap_or(true);
                            if is_hidden {
                                self.launcher_prev_focus = self.focused_window;
                                self.show_window(wid);
                                self.set_focus(Some(wid));
                            } else {
                                self.hide_window(wid);
                                let prev = self.launcher_prev_focus.take();
                                self.set_focus(prev);
                            }
                        }
                    }
                }
                return true;
            }
        }
        false
    }

    /// Cycle focus through non-panel toplevels in z-order. `forward = true` → next window.
    fn cycle_focus(&mut self, forward: bool) {
        // Collect toplevels (non-panel) in current z-order (bottom → top).
        let mut ids = [0u64; MAX_WINDOWS];
        let mut n = 0usize;
        for i in 0..self.n_windows {
            let id = self.z_order[i];
            let is_panel = self.windows.iter()
                .filter_map(|w| w.as_ref())
                .find(|w| w.id == id)
                .map(|w| w.is_panel || w.closing || w.hidden)
                .unwrap_or(true);
            if !is_panel {
                ids[n] = id;
                n += 1;
            }
        }
        if n <= 1 { return; }

        let cur_idx = self.focused_window
            .and_then(|id| (0..n).find(|&i| ids[i] == id));

        let next_idx = if forward {
            cur_idx.map(|i| (i + 1) % n).unwrap_or(0)
        } else {
            cur_idx.map(|i| (i + n - 1) % n).unwrap_or(n - 1)
        };

        let next_id = ids[next_idx];
        self.z_raise(next_id);
        self.set_focus(Some(next_id)); // set_focus calls mark_full_redraw internally
    }
}

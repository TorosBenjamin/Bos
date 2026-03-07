use super::{Compositor, MAX_WINDOWS, CLOSE_MAX_ATTEMPTS};
use crate::window::Window;
use kernel_api_types::window::*;
use kernel_api_types::IPC_ERR_PEER_CLOSED;

fn read_unaligned_at<T: Copy>(msg: &[u8], offset: usize) -> T {
    unsafe { core::ptr::read_unaligned(msg.as_ptr().add(offset) as *const T) }
}

impl Compositor {
    pub(super) fn handle_create_toplevel(&mut self, req: &CreateWindowRequest, reply_ep: u64) {
        let slot_idx = match self.windows.iter().position(|w| w.is_none()) {
            Some(i) => i,
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                    shared_buf_id: 0,
                    width: 0,
                    height: 0,
                });
                return;
            }
        };

        let window_id = self.next_window_id;
        self.next_window_id += 1;

        let app_id = &req.app_id[..req.app_id_len.min(32) as usize];
        let is_floating = self.resolve_floating(app_id, req.flags, req.parent_id);

        let (ax, ay, aw, ah) = self.available_area();

        let (init_x, init_y, init_w, init_h) = if is_floating {
            let fw = if req.init_w > 0 { req.init_w } else { 400 };
            let fh = if req.init_h > 0 { req.init_h } else { 300 };
            let fx = ax + (aw.saturating_sub(fw)) as i32 / 2;
            let fy = ay + (ah.saturating_sub(fh)) as i32 / 2;
            (fx, fy, fw, fh)
        } else {
            // Tiled: compute initial position as the n_current-th column.
            let n_current = self.count_toplevels();
            let n_new = n_current + 1;
            let total_h_gaps = 2 * self.outer_gap + n_current as u32 * self.inner_gap;
            let usable_w = aw.saturating_sub(total_h_gaps);
            let usable_h = ah.saturating_sub(2 * self.outer_gap);
            let tile_w = usable_w / n_new as u32;
            let new_x = ax + self.outer_gap as i32
                + (n_current as u32 * (tile_w + self.inner_gap)) as i32;
            let init_w = usable_w - n_current as u32 * tile_w;
            (new_x, ay + self.outer_gap as i32, init_w, usable_h)
        };

        let starts_hidden = req.flags & WINDOW_FLAG_HIDDEN != 0;

        match Window::new(window_id, init_x, init_y, init_w, init_h, req.event_send_ep) {
            Some(mut window) => {
                window.is_floating = is_floating;
                window.has_alpha = req.flags & WINDOW_FLAG_ALPHA != 0;
                window.hidden = starts_hidden;
                let app_id_len = req.app_id_len.min(32) as usize;
                window.app_id[..app_id_len].copy_from_slice(&req.app_id[..app_id_len]);
                window.app_id_len = app_id_len as u8;
                let shared_buf_id = window.shared_buf_id;
                self.windows[slot_idx] = Some(window);
                if !starts_hidden {
                    self.z_push_toplevel(window_id);
                }

                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::Ok,
                    window_id,
                    shared_buf_id,
                    width: init_w,
                    height: init_h,
                });

                if !starts_hidden {
                    self.set_focus(Some(window_id));
                }

                if !is_floating && !starts_hidden {
                    // Recalculate layout: redistributes existing tiled toplevels.
                    self.recalculate_toplevel_layout();
                } else {
                    self.mark_full_redraw();
                }
            }
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                    shared_buf_id: 0,
                    width: 0,
                    height: 0,
                });
            }
        }
    }

    pub(super) fn handle_create_panel(&mut self, req: &CreatePanelRequest, reply_ep: u64) {
        if req.width == 0 || req.height == 0 || req.width > 4096 || req.height > 4096 {
            self.send_response(reply_ep, &CreateWindowResponse {
                result: WindowResult::ErrorInvalidDimensions,
                window_id: 0,
                shared_buf_id: 0,
                width: 0,
                height: 0,
            });
            return;
        }

        let slot_idx = match self.windows.iter().position(|w| w.is_none()) {
            Some(i) => i,
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                    shared_buf_id: 0,
                    width: 0,
                    height: 0,
                });
                return;
            }
        };

        let window_id = self.next_window_id;
        self.next_window_id += 1;

        let sw = self.display_info.width;
        let sh = self.display_info.height;
        let (px, py, pw, ph) = match req.anchor {
            0 => (0i32, 0i32, sw, req.height),                          // Top
            1 => (0i32, sh as i32 - req.height as i32, sw, req.height), // Bottom
            2 => (0i32, 0i32, req.width, sh),                           // Left
            3 => (sw as i32 - req.width as i32, 0i32, req.width, sh),   // Right
            _ => (0i32, 0i32, req.width, req.height),
        };

        match Window::new(window_id, px, py, pw, ph, req.event_send_ep) {
            Some(mut window) => {
                window.is_panel = true;
                window.anchor = req.anchor;
                window.exclusive_zone = req.exclusive_zone;
                window.has_alpha = req.flags as u32 & WINDOW_FLAG_ALPHA != 0;
                let shared_buf_id = window.shared_buf_id;
                self.windows[slot_idx] = Some(window);
                // Panels live at the top of z_order
                self.z_push(window_id);

                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::Ok,
                    window_id,
                    shared_buf_id,
                    width: pw,
                    height: ph,
                });

                // Reflow toplevels into the reduced available area
                self.recalculate_toplevel_layout();
            }
            None => {
                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::ErrorOutOfMemory,
                    window_id: 0,
                    shared_buf_id: 0,
                    width: 0,
                    height: 0,
                });
            }
        }
    }

    pub(super) fn handle_update_window(&mut self, header: &UpdateWindowRequest) {
        let damage = {
            let window = match self.windows.iter_mut()
                .filter_map(|w| w.as_mut())
                .find(|w| w.id == header.window_id)
            {
                Some(w) => w,
                None => return,
            };
            if header.dirty_x + header.dirty_width > window.width
                || header.dirty_y + header.dirty_height > window.height
            {
                return;
            }

            // Client has acknowledged all pending Configure events — destroy all queued old bufs.
            for i in 0..window.n_pending_old {
                ulib::sys_destroy_shared_buf(window.pending_old_buf_ids[i]);
            }
            window.n_pending_old = 0;

            // Translate the client's window-local dirty rect to screen coordinates.
            (
                window.x + header.dirty_x as i32,
                window.y + header.dirty_y as i32,
                header.dirty_width,
                header.dirty_height,
            )
        };

        if let Some(rect) = self.screen_rect(damage.0, damage.1, damage.2, damage.3) {
            // Store in the window's own pending_dirty, NOT the global pending_damage.
            // This prevents two windows at opposite screen corners from merging their
            // tiny dirty rects into a huge bounding box that covers the entire screen.
            if let Some(window) = self.windows.iter_mut()
                .filter_map(|w| w.as_mut())
                .find(|w| w.id == header.window_id)
            {
                match &mut window.pending_dirty {
                    Some(d) => d.expand(rect.x, rect.y, rect.w, rect.h),
                    None => window.pending_dirty = Some(rect),
                }
            }
        }
    }

    /// Free all server-side resources for a window. Safe to call even if the window
    /// is not in the z-order (z_remove is idempotent).
    pub(super) fn complete_cleanup(&mut self, id: WindowId) {
        let slot = self.windows.iter_mut().find(|s| s.as_ref().map(|w| w.id) == Some(id));
        let (buf, buf_size, shared_buf_id, event_ep, pending_ids, n_pending) = match slot {
            Some(s @ &mut Some(_)) => {
                let w = s.as_mut().unwrap();
                let t = (w.buffer as *mut u8, w.buf_size, w.shared_buf_id, w.event_send_ep, w.pending_old_buf_ids, w.n_pending_old);
                *s = None;
                t
            }
            _ => return,
        };

        ulib::sys_munmap(buf, buf_size);
        for i in 0..n_pending {
            ulib::sys_destroy_shared_buf(pending_ids[i]);
        }
        ulib::sys_destroy_shared_buf(shared_buf_id);
        if event_ep != 0 {
            ulib::sys_channel_close(event_ep);
        }
        self.z_remove(id);

        // Update focus
        if self.focused_window == Some(id) {
            let new_focus = self.topmost_toplevel_id();
            self.focused_window = None;
            self.set_focus(new_focus);
        }

        self.mark_full_redraw();
        self.recalculate_toplevel_layout();
    }

    /// Begin graceful destruction of a window. Sends a Close event to the client;
    /// if the client is already gone, cleans up immediately.
    pub(super) fn initiate_close(&mut self, id: WindowId) {
        let (ep, already_closing) = match self.windows.iter()
            .filter_map(|w| w.as_ref())
            .find(|w| w.id == id)
        {
            Some(w) => (w.event_send_ep, w.closing),
            None    => return,
        };
        if already_closing {
            return;
        }

        let result = ulib::sys_try_channel_send(ep, &[WindowEventType::Close as u8]);
        if result == IPC_ERR_PEER_CLOSED {
            self.complete_cleanup(id);
        } else {
            // Mark closing; remove from compositing immediately.
            if let Some(w) = self.windows.iter_mut()
                .filter_map(|w| w.as_mut())
                .find(|w| w.id == id)
            {
                w.closing = true;
                w.close_attempts = 0;
            }
            self.z_remove(id);
            self.mark_full_redraw();
        }
    }

    /// Called once per run-loop iteration; probes closing windows and finishes cleanup
    /// once the client has closed its end of the event channel (or the timeout expires).
    pub(super) fn poll_closing_windows(&mut self) {
        let mut to_cleanup = [0u64; MAX_WINDOWS];
        let mut n_cleanup  = 0usize;
        let mut to_inc     = [0u64; MAX_WINDOWS];
        let mut n_inc      = 0usize;

        for slot in &self.windows {
            if let Some(w) = slot.as_ref() {
                if !w.closing { continue; }
                let result = ulib::sys_try_channel_send(w.event_send_ep, &[0xFF]);
                if result == IPC_ERR_PEER_CLOSED || w.close_attempts >= CLOSE_MAX_ATTEMPTS {
                    to_cleanup[n_cleanup] = w.id;
                    n_cleanup += 1;
                } else {
                    to_inc[n_inc] = w.id;
                    n_inc += 1;
                }
            }
        }

        for i in 0..n_cleanup {
            self.complete_cleanup(to_cleanup[i]);
        }

        for slot in &mut self.windows {
            if let Some(w) = slot.as_mut() {
                if to_inc[..n_inc].contains(&w.id) {
                    w.close_attempts += 1;
                }
            }
        }
    }

    pub(super) fn handle_close_window(&mut self, req: &CloseWindowRequest) {
        self.initiate_close(req.window_id);
    }

    fn topmost_toplevel_id(&self) -> Option<WindowId> {
        for i in (0..self.n_windows).rev() {
            let id = self.z_order[i];
            if let Some(w) = self.windows.iter().filter_map(|w| w.as_ref()).find(|w| w.id == id) {
                if !w.is_panel {
                    return Some(id);
                }
            }
        }
        None
    }

    pub(super) fn handle_move_window(&mut self, req: &MoveWindowRequest) {
        let old_pos = self.windows.iter_mut()
            .filter_map(|w| w.as_mut())
            .find(|w| w.id == req.window_id)
            .map(|window| {
                let old = (window.x, window.y, window.width, window.height);
                window.x = req.x;
                window.y = req.y;
                old
            });

        if let Some((ox, oy, w, h)) = old_pos {
            let mut damage = self.screen_rect(ox, oy, w, h);
            if let Some(new_rect) = self.screen_rect(req.x, req.y, w, h) {
                match &mut damage {
                    Some(d) => d.expand(new_rect.x, new_rect.y, new_rect.w, new_rect.h),
                    None => damage = Some(new_rect),
                }
            }
            if let Some(d) = damage {
                self.mark_damage(d);
            }
        }
    }

    pub(super) fn handle_raise_window(&mut self, req: &RaiseWindowRequest) {
        self.z_raise(req.window_id);
        self.mark_full_redraw();
    }

    pub(super) fn handle_lower_window(&mut self, req: &LowerWindowRequest) {
        self.z_lower(req.window_id);
        self.mark_full_redraw();
    }

    fn send_response<T>(&self, reply_ep: u64, response: &T) {
        let bytes = unsafe {
            core::slice::from_raw_parts(
                response as *const T as *const u8,
                core::mem::size_of::<T>(),
            )
        };
        ulib::sys_channel_send(reply_ep, bytes);
        ulib::sys_channel_close(reply_ep);
    }

    pub(super) fn process_message(&mut self, msg: &[u8]) {
        if msg.is_empty() {
            return;
        }

        let msg_type = msg[0];

        match msg_type {
            t if t == WindowMessageType::CreateWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<CreateWindowRequest>() + 8 {
                    return;
                }
                let req: CreateWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const CreateWindowRequest)
                };
                let ep_off = 1 + core::mem::size_of::<CreateWindowRequest>();
                let reply_ep = u64::from_le_bytes(msg[ep_off..ep_off + 8].try_into().unwrap_or([0; 8]));
                self.handle_create_toplevel(&req, reply_ep);
            }
            t if t == WindowMessageType::CreatePanel as u8 => {
                if msg.len() < 1 + core::mem::size_of::<CreatePanelRequest>() + 8 {
                    return;
                }
                let req: CreatePanelRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const CreatePanelRequest)
                };
                let ep_off = 1 + core::mem::size_of::<CreatePanelRequest>();
                let reply_ep = u64::from_le_bytes(msg[ep_off..ep_off + 8].try_into().unwrap_or([0; 8]));
                self.handle_create_panel(&req, reply_ep);
            }
            t if t == WindowMessageType::UpdateWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<UpdateWindowRequest>() {
                    return;
                }
                let header: UpdateWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const UpdateWindowRequest)
                };
                self.handle_update_window(&header);
            }
            t if t == WindowMessageType::CloseWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<CloseWindowRequest>() {
                    return;
                }
                let req: CloseWindowRequest = unsafe {
                    core::ptr::read(msg.as_ptr().add(1) as *const CloseWindowRequest)
                };
                self.handle_close_window(&req);
            }
            t if t == WindowMessageType::MoveWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<MoveWindowRequest>() {
                    return;
                }
                let req: MoveWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const MoveWindowRequest)
                };
                self.handle_move_window(&req);
            }
            t if t == WindowMessageType::RaiseWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<RaiseWindowRequest>() {
                    return;
                }
                let req: RaiseWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const RaiseWindowRequest)
                };
                self.handle_raise_window(&req);
            }
            t if t == WindowMessageType::LowerWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<LowerWindowRequest>() {
                    return;
                }
                let req: LowerWindowRequest = unsafe {
                    core::ptr::read_unaligned(msg.as_ptr().add(1) as *const LowerWindowRequest)
                };
                self.handle_lower_window(&req);
            }
            t if t == WindowMessageType::HideWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<HideWindowRequest>() {
                    return;
                }
                let req: HideWindowRequest = read_unaligned_at(msg, 1);
                let is_visible = self.windows.iter()
                    .filter_map(|w| w.as_ref())
                    .find(|w| w.id == req.window_id)
                    .map(|w| !w.hidden)
                    .unwrap_or(false);
                if is_visible {
                    self.hide_window(req.window_id);
                }
            }
            t if t == WindowMessageType::ShowWindow as u8 => {
                if msg.len() < 1 + core::mem::size_of::<ShowWindowRequest>() {
                    return;
                }
                let req: ShowWindowRequest = read_unaligned_at(msg, 1);
                let is_hidden = self.windows.iter()
                    .filter_map(|w| w.as_ref())
                    .find(|w| w.id == req.window_id)
                    .map(|w| w.hidden)
                    .unwrap_or(false);
                if is_hidden {
                    self.show_window(req.window_id);
                }
            }
            _ => {}
        }
    }
}

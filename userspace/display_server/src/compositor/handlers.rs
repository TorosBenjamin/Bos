use super::Compositor;
use crate::window::Window;
use kernel_api_types::window::*;

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

        match Window::new(window_id, init_x, init_y, init_w, init_h, req.event_send_ep) {
            Some(mut window) => {
                window.is_floating = is_floating;
                let shared_buf_id = window.shared_buf_id;
                self.windows[slot_idx] = Some(window);
                self.z_push_toplevel(window_id);

                self.send_response(reply_ep, &CreateWindowResponse {
                    result: WindowResult::Ok,
                    window_id,
                    shared_buf_id,
                    width: init_w,
                    height: init_h,
                });

                self.set_focus(Some(window_id));

                if !is_floating {
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

            // Clean up old buffer if client has acknowledged a Configure event
            if let Some(old_id) = window.pending_old_buf_id.take() {
                ulib::sys_destroy_shared_buf(old_id);
            }

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

    pub(super) fn handle_close_window(&mut self, req: &CloseWindowRequest) {
        for slot in self.windows.iter_mut() {
            if let Some(window) = slot {
                if window.id == req.window_id {
                    ulib::sys_munmap(window.buffer as *mut u8, window.buf_size);
                    let id = window.id;
                    let shared_buf_id = window.shared_buf_id;
                    let event_ep = window.event_send_ep;
                    let pending_old = window.pending_old_buf_id.take();
                    *slot = None;
                    self.z_remove(id);
                    ulib::sys_destroy_shared_buf(shared_buf_id);
                    if let Some(old_id) = pending_old {
                        ulib::sys_destroy_shared_buf(old_id);
                    }
                    // Close event channel to signal client
                    if event_ep != 0 {
                        ulib::sys_channel_close(event_ep);
                    }
                    // Update focus
                    if self.focused_window == Some(id) {
                        // Focus the topmost remaining toplevel, if any
                        let new_focus = self.topmost_toplevel_id();
                        self.focused_window = None; // prevent set_focus from sending FocusLost to dead window
                        self.set_focus(new_focus);
                    }
                    self.mark_full_redraw();
                    // Redistribute space among remaining toplevels
                    self.recalculate_toplevel_layout();
                    return;
                }
            }
        }
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
            _ => {}
        }
    }
}

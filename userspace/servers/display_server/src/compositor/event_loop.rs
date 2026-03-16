use kernel_api_types::window::*;
use kernel_api_types::{IPC_OK, MMAP_WRITE, MOUSE_LEFT, MOUSE_RIGHT, MOUSE_MIDDLE, KEY_MOD_SUPER};
use super::{Compositor, DragKind, MAX_MSG_SIZE, send_event};

impl Compositor {
    pub fn run(&mut self) -> ! {
        let msg_buf = ulib::sys_mmap(MAX_MSG_SIZE as u64, MMAP_WRITE);
        if msg_buf.is_null() {
            loop { ulib::sys_yield(); }
        }

        // 60 fps cap: one frame every 16 666 666 ns.
        const FRAME_BUDGET_NS: u64 = 1_000_000_000 / 60;

        // Initialise the frame anchor so delta_ns is reasonable on the first frame.
        let mut last_frame_ns = ulib::sys_get_time_ns();

        // Initial full composite
        self.mark_full_redraw();

        loop {
            let frame_start_ns = ulib::sys_get_time_ns();
            // Actual duration of the previous frame (clamped to at least 1 ns).
            let delta_ns = frame_start_ns.saturating_sub(last_frame_ns).max(1);
            last_frame_ns = frame_start_ns;
            // Poll any windows that are in the process of graceful close.
            self.poll_closing_windows();

            // Drain pending IPC messages (non-blocking) before compositing.
            // Capped at 64 per frame so a spamming client can't starve the compositor.
            for _ in 0..64 {
                let msg_slice = unsafe { core::slice::from_raw_parts_mut(msg_buf, MAX_MSG_SIZE) };
                let (result, bytes_read) = ulib::sys_try_channel_recv(self.recv_endpoint, msg_slice);
                if result != IPC_OK || bytes_read == 0 {
                    break;
                }
                let msg = unsafe { core::slice::from_raw_parts(msg_buf, bytes_read as usize) };
                self.process_message(msg);
            }

            // Drain all pending mouse events; accumulate into a single cursor move.
            let mut total_dx = 0i32;
            let mut total_dy = 0i32;
            let mut cur_buttons = self.prev_mouse_buttons;
            let mut cur_modifiers = 0u8;
            while let Some(ev) = ulib::sys_read_mouse() {
                total_dx += ev.dx as i32;
                total_dy += ev.dy as i32;
                cur_buttons = ev.buttons;
                cur_modifiers = ev.modifiers;
            }

            if total_dx != 0 || total_dy != 0 {
                let old_rect = self.cursor_rect();
                self.cursor_x = (self.cursor_x + total_dx)
                    .clamp(0, self.display_info.width as i32 - 1);
                self.cursor_y = (self.cursor_y + total_dy)
                    .clamp(0, self.display_info.height as i32 - 1);
                let new_rect = self.cursor_rect();

                let cursor_damage = match (old_rect, new_rect) {
                    (Some(mut a), Some(b)) => { a.expand(b.x, b.y, b.w, b.h); Some(a) }
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                };
                if let Some(cd) = cursor_damage {
                    self.expand_pending(cd);
                }
            }

            // Update any active drag (position/size/ratio changes based on cursor movement).
            if self.drag_state.is_some() {
                self.update_drag();
            }

            // Notify the focused window of the new cursor position (window-relative),
            // but only when no drag is consuming mouse input.
            if self.drag_state.is_none() && (total_dx != 0 || total_dy != 0)
                && let Some(fw_id) = self.focused_window
            {
                let info = self.windows.iter()
                    .filter_map(|w| w.as_ref())
                    .find(|w| w.id == fw_id)
                    .map(|w| (w.x, w.y, w.event_send_ep));
                if let Some((wx, wy, ep)) = info && ep != 0 {
                    send_event(ep, &MouseMoveEvent {
                        event_type: WindowEventType::MouseMove as u8,
                        _pad: [0; 3],
                        x: self.cursor_x - wx,
                        y: self.cursor_y - wy,
                        delta_ns,
                    });
                }
            }

            let had_drag = self.drag_state.is_some();
            let just_pressed  = cur_buttons & !self.prev_mouse_buttons;
            let just_released = self.prev_mouse_buttons & !cur_buttons;
            let super_held    = cur_modifiers & KEY_MOD_SUPER != 0;

            // Complete drag on button release
            if had_drag && just_released & (MOUSE_LEFT | MOUSE_RIGHT) != 0
                && let Some(drag) = self.drag_state.take()
            {
                if matches!(drag.kind, DragKind::MoveTiled) {
                    self.apply_tiled_drop(drag.window_id, self.cursor_x, self.cursor_y);
                }
                self.mark_full_redraw();
            }

            // Start drag or handle regular click (only when no drag was already active)
            if !had_drag {
                if just_pressed & MOUSE_LEFT != 0 {
                    if super_held {
                        if let Some(id) = self.hit_test(self.cursor_x, self.cursor_y) {
                            self.start_move_drag(id);
                        }
                    } else {
                        let hit = self.hit_test(self.cursor_x, self.cursor_y);
                        if let Some(id) = hit {
                            self.z_raise(id);
                            self.mark_full_redraw();
                        }
                        self.set_focus(hit);
                    }
                }
                if just_pressed & MOUSE_RIGHT != 0 && super_held
                    && let Some(id) = self.hit_test(self.cursor_x, self.cursor_y)
                {
                    self.start_resize_drag(id);
                }
            }

            self.prev_mouse_buttons = cur_buttons;

            // Route mouse button events to the focused window (skip during drag)
            if !had_drag && self.drag_state.is_none() && (just_pressed | just_released) != 0
                && let Some(fw_id) = self.focused_window
            {
                let pos = self.windows.iter()
                    .filter_map(|w| w.as_ref())
                    .find(|w| w.id == fw_id)
                    .map(|w| (w.x, w.y));
                if let Some((wx, wy)) = pos {
                    let ep = self.window_event_ep(fw_id);
                    if ep != 0 {
                        for &bit in &[MOUSE_LEFT, MOUSE_RIGHT, MOUSE_MIDDLE] {
                            if just_pressed & bit != 0 {
                                send_event(ep, &MouseButtonEvent {
                                    event_type: WindowEventType::MouseButtonPress as u8,
                                    button: bit,
                                    _pad: [0; 2],
                                    x: self.cursor_x - wx,
                                    y: self.cursor_y - wy,
                                });
                            }
                            if just_released & bit != 0 {
                                send_event(ep, &MouseButtonEvent {
                                    event_type: WindowEventType::MouseButtonRelease as u8,
                                    button: bit,
                                    _pad: [0; 2],
                                    x: self.cursor_x - wx,
                                    y: self.cursor_y - wy,
                                });
                            }
                        }
                    }
                }
            }

            // Route keyboard events to the focused window, intercepting shortcuts first.
            while let Some(key) = ulib::sys_try_read_key() {
                if self.handle_shortcut(&key) {
                    continue;
                }
                if let Some(fw_id) = self.focused_window {
                    let ep = self.window_event_ep(fw_id);
                    if ep != 0 {
                        let ev = KeyPressEvent {
                            event_type: WindowEventType::KeyPress as u8,
                            key,
                        };
                        send_event(ep, &ev);
                    }
                }
            }

            // Single composite for everything accumulated this iteration.
            self.flush();

            // Sleep for whatever budget remains in this 16.67ms frame window.
            // This caps the compositor at ~60 fps while remaining event-driven:
            // sys_wait_for_event returns early as soon as mouse/keyboard/IPC arrives.
            let elapsed_ns = ulib::sys_get_time_ns().saturating_sub(frame_start_ns);
            let remaining_ns = FRAME_BUDGET_NS.saturating_sub(elapsed_ns);
            // Convert to ms (ceiling so a sub-ms remainder doesn't collapse to 0 = infinite).
            let timeout_ms = (remaining_ns / 1_000_000).max(remaining_ns.min(1));
            if timeout_ms > 0 {
                ulib::sys_wait_for_event(
                    &[self.recv_endpoint],
                    ulib::WAIT_MOUSE | ulib::WAIT_KEYBOARD,
                    timeout_ms,
                );
            }
        }
    }
}

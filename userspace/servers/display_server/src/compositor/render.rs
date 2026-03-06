use super::{Compositor, MAX_WINDOWS};
use crate::cursor::{CURSOR_H, CURSOR_IMAGE, CURSOR_MASK, CURSOR_W};
use kernel_api_types::window::{DirtyRect, WindowEventType};
use kernel_api_types::IPC_ERR_PEER_CLOSED;

impl Compositor {
    /// Fill a rectangle in the back buffer, clipped to `clip` (or unconstrained if None).
    fn fill_back_rect_clipped(
        &mut self,
        x: i32, y: i32, w: u32, h: u32,
        clip: Option<DirtyRect>,
        color: u32,
    ) {
        let dst = self.display.back_buffer_ptr();
        if dst.is_null() || w == 0 || h == 0 {
            return;
        }
        let screen_w = self.display_info.width as usize;
        let screen_h = self.display_info.height as usize;

        let mut x0 = x.max(0) as usize;
        let mut y0 = y.max(0) as usize;
        let mut x1 = ((x + w as i32).max(0) as usize).min(screen_w);
        let mut y1 = ((y + h as i32).max(0) as usize).min(screen_h);

        if let Some(c) = clip {
            x0 = x0.max(c.x as usize);
            y0 = y0.max(c.y as usize);
            x1 = x1.min((c.x + c.w) as usize);
            y1 = y1.min((c.y + c.h) as usize);
        }

        if x0 >= x1 || y0 >= y1 {
            return;
        }

        unsafe {
            for row in y0..y1 {
                let row_ptr = dst.add(row * screen_w + x0);
                core::slice::from_raw_parts_mut(row_ptr, x1 - x0).fill(color);
            }
        }
    }

    /// Blit `w×h` pixels from `src` into the back buffer at `(dst_x, dst_y)`,
    /// clipped to both screen bounds and the optional `clip` rect.
    fn blit_to_back(
        &mut self,
        src: *const u32,
        src_width: u32,
        dst_x: i32,
        dst_y: i32,
        w: u32,
        h: u32,
        clip: Option<DirtyRect>,
    ) {
        let dst = self.display.back_buffer_ptr();
        if dst.is_null() {
            return;
        }
        let screen_w = self.display_info.width as usize;
        let screen_h = self.display_info.height as usize;

        let mut x0 = dst_x.max(0) as usize;
        let mut y0 = dst_y.max(0) as usize;
        let mut x1 = ((dst_x + w as i32).max(0) as usize).min(screen_w);
        let mut y1 = ((dst_y + h as i32).max(0) as usize).min(screen_h);

        // Further clip to damage rect so we only touch the pixels that need compositing.
        if let Some(c) = clip {
            x0 = x0.max(c.x as usize);
            y0 = y0.max(c.y as usize);
            x1 = x1.min((c.x + c.w) as usize);
            y1 = y1.min((c.y + c.h) as usize);
        }

        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let clipped_w = x1 - x0;
        let src_x_off = (x0 as i32 - dst_x).max(0) as usize;
        let src_y_off = (y0 as i32 - dst_y).max(0) as usize;

        for row in 0..(y1 - y0) {
            let src_off = (src_y_off + row) * src_width as usize + src_x_off;
            let dst_off = (y0 + row) * screen_w + x0;
            unsafe {
                core::ptr::copy_nonoverlapping(src.add(src_off), dst.add(dst_off), clipped_w);
            }
        }
    }

    /// Blend a single pixel: uniform (non-premultiplied) alpha blend.
    #[inline(always)]
    fn blend_pixel(src: u32, dst: u32, alpha: u32, info: &kernel_api_types::graphics::DisplayInfo) -> u32 {
        let inv = 255 - alpha;
        let r = ((src >> info.red_mask_shift   & 0xFF) * alpha
               + (dst >> info.red_mask_shift   & 0xFF) * inv) >> 8;
        let g = ((src >> info.green_mask_shift & 0xFF) * alpha
               + (dst >> info.green_mask_shift & 0xFF) * inv) >> 8;
        let b = ((src >> info.blue_mask_shift  & 0xFF) * alpha
               + (dst >> info.blue_mask_shift  & 0xFF) * inv) >> 8;
        info.build_pixel(r as u8, g as u8, b as u8)
    }

    /// Blit with a uniform opacity (same alpha for every pixel). Used for inactive window dimming.
    fn blit_to_back_uniform(
        &mut self,
        src: *const u32,
        src_width: u32,
        dst_x: i32,
        dst_y: i32,
        w: u32,
        h: u32,
        clip: Option<DirtyRect>,
        opacity: u8,
    ) {
        if opacity == 255 {
            self.blit_to_back(src, src_width, dst_x, dst_y, w, h, clip);
            return;
        }
        let dst = self.display.back_buffer_ptr();
        if dst.is_null() {
            return;
        }
        let info = self.display_info;
        let screen_w = info.width as usize;
        let screen_h = info.height as usize;

        let mut x0 = dst_x.max(0) as usize;
        let mut y0 = dst_y.max(0) as usize;
        let mut x1 = ((dst_x + w as i32).max(0) as usize).min(screen_w);
        let mut y1 = ((dst_y + h as i32).max(0) as usize).min(screen_h);

        if let Some(c) = clip {
            x0 = x0.max(c.x as usize);
            y0 = y0.max(c.y as usize);
            x1 = x1.min((c.x + c.w) as usize);
            y1 = y1.min((c.y + c.h) as usize);
        }

        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let alpha = opacity as u32;
        let src_x_off = (x0 as i32 - dst_x).max(0) as usize;
        let src_y_off = (y0 as i32 - dst_y).max(0) as usize;

        for row in 0..(y1 - y0) {
            let src_row_off = (src_y_off + row) * src_width as usize + src_x_off;
            let dst_row_off = (y0 + row) * screen_w + x0;
            for col in 0..(x1 - x0) {
                unsafe {
                    let src_px = *src.add(src_row_off + col);
                    let bg_px  = *dst.add(dst_row_off + col);
                    *dst.add(dst_row_off + col) = Self::blend_pixel(src_px, bg_px, alpha, &info);
                }
            }
        }
    }

    /// Blit with per-pixel premultiplied alpha (bits 31–24 of each source pixel).
    /// `dim` applies an additional uniform scale (255 = no extra dimming).
    fn blit_to_back_premul_alpha(
        &mut self,
        src: *const u32,
        src_width: u32,
        dst_x: i32,
        dst_y: i32,
        w: u32,
        h: u32,
        clip: Option<DirtyRect>,
        dim: u8,
    ) {
        let dst = self.display.back_buffer_ptr();
        if dst.is_null() {
            return;
        }
        let info = self.display_info;
        let screen_w = info.width as usize;
        let screen_h = info.height as usize;

        let mut x0 = dst_x.max(0) as usize;
        let mut y0 = dst_y.max(0) as usize;
        let mut x1 = ((dst_x + w as i32).max(0) as usize).min(screen_w);
        let mut y1 = ((dst_y + h as i32).max(0) as usize).min(screen_h);

        if let Some(c) = clip {
            x0 = x0.max(c.x as usize);
            y0 = y0.max(c.y as usize);
            x1 = x1.min((c.x + c.w) as usize);
            y1 = y1.min((c.y + c.h) as usize);
        }

        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let dim_u32 = dim as u32;
        let src_x_off = (x0 as i32 - dst_x).max(0) as usize;
        let src_y_off = (y0 as i32 - dst_y).max(0) as usize;

        for row in 0..(y1 - y0) {
            let src_row_off = (src_y_off + row) * src_width as usize + src_x_off;
            let dst_row_off = (y0 + row) * screen_w + x0;
            for col in 0..(x1 - x0) {
                unsafe {
                    let src_px = *src.add(src_row_off + col);
                    let pixel_alpha = (src_px >> 24) & 0xFF;
                    if pixel_alpha == 0 {
                        continue; // fully transparent — skip
                    }
                    let effective_alpha = (pixel_alpha * dim_u32) >> 8;
                    if effective_alpha == 0 {
                        continue;
                    }
                    let dst_off = dst_row_off + col;
                    if effective_alpha >= 255 {
                        // Fully opaque: reconstruct via channel fields so the result
                        // is correct regardless of pixel format (avoids hardcoding
                        // alpha in bits 24-31).
                        let r = (src_px >> info.red_mask_shift)   & 0xFF;
                        let g = (src_px >> info.green_mask_shift) & 0xFF;
                        let b = (src_px >> info.blue_mask_shift)  & 0xFF;
                        *dst.add(dst_off) = info.build_pixel(r as u8, g as u8, b as u8);
                        continue;
                    }
                    // src channels are already premultiplied by pixel_alpha; scale by dim.
                    let r_s = ((src_px >> info.red_mask_shift   & 0xFF) * dim_u32) >> 8;
                    let g_s = ((src_px >> info.green_mask_shift & 0xFF) * dim_u32) >> 8;
                    let b_s = ((src_px >> info.blue_mask_shift  & 0xFF) * dim_u32) >> 8;
                    let bg  = *dst.add(dst_off);
                    let inv = 255 - effective_alpha;
                    let r_d = bg >> info.red_mask_shift   & 0xFF;
                    let g_d = bg >> info.green_mask_shift & 0xFF;
                    let b_d = bg >> info.blue_mask_shift  & 0xFF;
                    *dst.add(dst_off) = info.build_pixel(
                        (r_s + (r_d * inv >> 8)) as u8,
                        (g_s + (g_d * inv >> 8)) as u8,
                        (b_s + (b_d * inv >> 8)) as u8,
                    );
                }
            }
        }
    }

    /// Composite all windows and their borders in z-order (bottom to top).
    ///
    /// Each window is blitted first, then its border is drawn immediately after.
    /// This means a floating window blitted at z+1 will overwrite any tiled border
    /// drawn at z, so floating windows correctly appear on top of tiled borders.
    /// `clip` restricts all writes to the given damage rect (pass `None` for full-scene draws).
    fn composite_in_z_order(&mut self, clip: Option<DirtyRect>) {
        let focused = self.focused_window;
        let focused_color   = self.border_focused;
        let unfocused_color = self.border_unfocused;

        // Collect window data in z-order into local arrays to avoid
        // borrow conflicts when calling fill_back_rect_clipped / blit_to_back.
        let mut wxs       = [0i32; MAX_WINDOWS];
        let mut wys       = [0i32; MAX_WINDOWS];
        let mut wws       = [0u32; MAX_WINDOWS];
        let mut whs       = [0u32; MAX_WINDOWS];
        let mut bufs      = [core::ptr::null::<u32>(); MAX_WINDOWS];
        let mut panels    = [false; MAX_WINDOWS];
        let mut colors    = [0u32; MAX_WINDOWS];
        let mut has_alphas   = [false; MAX_WINDOWS];
        let mut is_focused   = [false; MAX_WINDOWS];
        let mut is_floating  = [false; MAX_WINDOWS];
        let mut n = 0usize;

        for i in 0..self.n_windows {
            let id = self.z_order[i];
            if let Some(w) = self.windows.iter().filter_map(|w| w.as_ref()).find(|w| w.id == id) {
                wxs[n]        = w.x;
                wys[n]        = w.y;
                wws[n]        = w.width;
                whs[n]        = w.height;
                bufs[n]       = w.buffer as *const u32;
                panels[n]     = w.is_panel;
                colors[n]     = if focused == Some(id) { focused_color } else { unfocused_color };
                has_alphas[n]  = w.has_alpha;
                is_focused[n]  = focused == Some(id);
                is_floating[n] = w.is_floating;
                n += 1;
            }
        }

        let inactive_opacity          = self.inactive_opacity;
        let inactive_opacity_floating = self.inactive_opacity_floating;

        for i in 0..n {
            let (wx, wy, ww, wh) = (wxs[i], wys[i], wws[i], whs[i]);
            let dim = if is_focused[i] || panels[i] {
                255u8
            } else if is_floating[i] {
                inactive_opacity_floating
            } else {
                inactive_opacity
            };
            if has_alphas[i] {
                self.blit_to_back_premul_alpha(bufs[i], ww, wx, wy, ww, wh, clip, dim);
            } else if dim < 255 {
                self.blit_to_back_uniform(bufs[i], ww, wx, wy, ww, wh, clip, dim);
            } else {
                self.blit_to_back(bufs[i], ww, wx, wy, ww, wh, clip);
            }

            if !panels[i] && self.border_width > 0 {
                let bw = self.border_width;
                let bwu = bw as u32;
                let color = colors[i];
                // Top strip
                self.fill_back_rect_clipped(wx - bw, wy - bw, ww + 2 * bwu, bwu, clip, color);
                // Bottom strip
                self.fill_back_rect_clipped(wx - bw, wy + wh as i32, ww + 2 * bwu, bwu, clip, color);
                // Left strip (side only, corners already covered by top/bottom)
                self.fill_back_rect_clipped(wx - bw, wy, bwu, wh, clip, color);
                // Right strip
                self.fill_back_rect_clipped(wx + ww as i32, wy, bwu, wh, clip, color);
            }
        }
    }

    fn update_scene_region(&mut self, damage: DirtyRect) {
        if !self.background_buf.is_null() {
            let screen_w = self.display_info.width;
            let src = unsafe {
                self.background_buf.add(damage.y as usize * screen_w as usize + damage.x as usize)
            };
            self.blit_to_back(src, screen_w, damage.x as i32, damage.y as i32, damage.w, damage.h, None);
        }
        self.composite_in_z_order(Some(damage));
    }

    fn update_scene_full(&mut self) {
        let dst = self.display.back_buffer_ptr();
        if dst.is_null() {
            return;
        }
        if !self.background_buf.is_null() {
            let n = self.display_info.width as usize * self.display_info.height as usize;
            unsafe { core::ptr::copy_nonoverlapping(self.background_buf, dst, n) };
        }
        self.composite_in_z_order(None);
    }

    pub(super) fn flush(&mut self) {
        if self.pending_full_redraw {
            self.pending_full_redraw = false;
            self.pending_damage = None;
            // Clear per-window dirty rects subsumed by the full redraw.
            for slot in &mut self.windows {
                if let Some(w) = slot { w.pending_dirty = None; }
            }
            self.update_scene_full();
            let sw = self.display_info.width;
            let sh = self.display_info.height;
            self.display.mark_dirty(0, 0, sw, sh);
            self.display.blit_cursor(
                self.cursor_x, self.cursor_y,
                &CURSOR_MASK, &CURSOR_IMAGE,
                CURSOR_W, CURSOR_H,
                self.cursor_black, self.cursor_white,
            );
            self.display.present();
            // Full redraw: every window's content is now on screen.
            let mut crashed = [0u64; MAX_WINDOWS];
            let mut n_crashed = 0usize;
            for slot in &self.windows {
                if let Some(w) = slot {
                    if w.closing { continue; }
                    let result = ulib::sys_try_channel_send(
                        w.event_send_ep,
                        &[WindowEventType::FramePresented as u8],
                    );
                    if result == IPC_ERR_PEER_CLOSED {
                        crashed[n_crashed] = w.id;
                        n_crashed += 1;
                    }
                }
            }
            for i in 0..n_crashed {
                self.initiate_close(crashed[i]);
            }
            return;
        }

        // Collect each window's independent dirty rect without holding a borrow on self.windows.
        // Keeping them separate prevents the bounding-box explosion caused by two windows at
        // opposite screen corners merging into a rect that covers the entire display.
        let mut dirty_rects: [Option<DirtyRect>; MAX_WINDOWS] = [None; MAX_WINDOWS];
        for (i, slot) in self.windows.iter_mut().enumerate() {
            if let Some(w) = slot {
                dirty_rects[i] = w.pending_dirty.take();
            }
        }
        // Extra damage from cursor movement and explicit window moves.
        let extra_damage = self.pending_damage.take();

        let has_window = dirty_rects.iter().any(|d| d.is_some());
        if !has_window && extra_damage.is_none() {
            return;
        }

        // Composite each region directly into back_buffer, then mark it dirty for present().
        for opt_dr in &dirty_rects {
            if let Some(dr) = opt_dr {
                self.update_scene_region(*dr);
                self.display.mark_dirty(dr.x, dr.y, dr.w, dr.h);
            }
        }

        if let Some(cd) = extra_damage {
            self.update_scene_region(cd);
            self.display.mark_dirty(cd.x, cd.y, cd.w, cd.h);
        }

        // Draw cursor on top of the composited back buffer (once, after all scene updates),
        // then present every accumulated dirty rect to VRAM independently via Display::present().
        // Because Display now tracks a list of dirty rects instead of a single bounding box,
        // each small rect is written to VRAM separately — no cross-window merging.
        self.display.blit_cursor(
            self.cursor_x, self.cursor_y,
            &CURSOR_MASK, &CURSOR_IMAGE,
            CURSOR_W, CURSOR_H,
            self.cursor_black, self.cursor_white,
        );
        self.display.present();

        // Notify each window whose pixels were composited this frame.
        let mut crashed = [0u64; MAX_WINDOWS];
        let mut n_crashed = 0usize;
        for (i, opt_dr) in dirty_rects.iter().enumerate() {
            if opt_dr.is_some() {
                if let Some(w) = &self.windows[i] {
                    if w.closing { continue; }
                    let result = ulib::sys_try_channel_send(
                        w.event_send_ep,
                        &[WindowEventType::FramePresented as u8],
                    );
                    if result == IPC_ERR_PEER_CLOSED {
                        crashed[n_crashed] = w.id;
                        n_crashed += 1;
                    }
                }
            }
        }
        for i in 0..n_crashed {
            self.initiate_close(crashed[i]);
        }
    }
}

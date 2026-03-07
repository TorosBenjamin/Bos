// Host-testable compositor logic.
// No_std when compiled for the baremetal target; std is available in test mode.
#![cfg_attr(not(test), no_std)]

// All public types and all tests are test-only — this lib exists solely to
// enable `cargo test -p display_server`.
#[cfg(test)]
mod compositor_tests {
    use kernel_api_types::window::DirtyRect;

    const SCREEN_W: u32 = 1280;
    const SCREEN_H: u32 = 720;
    const CUBE1_SIZE: u32 = 20;
    const CUBE2_SIZE: u32 = 25;
    const OUTER_GAP: u32 = 8;
    const INNER_GAP: u32 = 8;

    // -----------------------------------------------------------------------
    // Damage state machine (mirrors compositor.rs)
    // -----------------------------------------------------------------------

    #[derive(Default, Debug)]
    struct DamageState {
        pending_damage: Option<DirtyRect>,
        pending_scene_update: bool,
        pending_full_redraw: bool,
    }

    #[derive(Debug, PartialEq)]
    enum FlushResult {
        Nothing,
        FullRedraw,
        PartialUpdate { damage: DirtyRect },
    }

    impl DamageState {
        fn mark_damage(&mut self, rect: DirtyRect) {
            match &mut self.pending_damage {
                Some(d) => d.expand(rect.x, rect.y, rect.w, rect.h),
                None => self.pending_damage = Some(rect),
            }
            self.pending_scene_update = true;
        }

        fn mark_full_redraw(&mut self) {
            self.pending_full_redraw = true;
            self.pending_scene_update = true;
            self.pending_damage = None;
        }

        /// Mirrors `Compositor::flush_window_update()`.
        fn flush_window_update(&mut self) -> Option<DirtyRect> {
            if self.pending_full_redraw {
                return None;
            }
            if let Some(damage) = self.pending_damage.take() {
                self.pending_scene_update = false;
                return Some(damage);
            }
            None
        }

        /// Mirrors `Compositor::flush()`.
        fn flush(&mut self) -> FlushResult {
            if self.pending_full_redraw {
                self.pending_full_redraw = false;
                self.pending_scene_update = false;
                self.pending_damage = None;
                FlushResult::FullRedraw
            } else if let Some(damage) = self.pending_damage.take() {
                self.pending_scene_update = false;
                FlushResult::PartialUpdate { damage }
            } else {
                FlushResult::Nothing
            }
        }
    }

    // -----------------------------------------------------------------------
    // Tiling helpers (mirrors recalculate_toplevel_layout)
    // -----------------------------------------------------------------------

    fn compute_tile_layout(n: usize, ax: i32, ay: i32, aw: u32, ah: u32) -> Vec<(i32, i32, u32, u32)> {
        if n == 0 { return vec![]; }
        let total_h_gaps = 2 * OUTER_GAP + (n as u32 - 1) * INNER_GAP;
        let usable_w = aw.saturating_sub(total_h_gaps);
        let usable_h = ah.saturating_sub(2 * OUTER_GAP);
        let tile_w = usable_w / n as u32;
        (0..n).map(|i| {
            let x = ax + OUTER_GAP as i32 + (i as u32 * (tile_w + INNER_GAP)) as i32;
            let y = ay + OUTER_GAP as i32;
            let w = if i == n - 1 { usable_w - tile_w * i as u32 } else { tile_w };
            (x, y, w, usable_h)
        }).collect()
    }

    // -----------------------------------------------------------------------
    // Available area helpers (mirrors available_area)
    // -----------------------------------------------------------------------

    struct Panel { anchor: u8, zone: u32 }

    fn available_area(sw: u32, sh: u32, panels: &[Panel]) -> (i32, i32, u32, u32) {
        let mut ax = 0i32; let mut ay = 0i32;
        let mut aw = sw;   let mut ah = sh;
        for p in panels {
            match p.anchor {
                0 => { let z = p.zone.min(ah); ay += z as i32; ah -= z; }
                1 => { ah -= p.zone.min(ah); }
                2 => { let z = p.zone.min(aw); ax += z as i32; aw -= z; }
                3 => { aw -= p.zone.min(aw); }
                _ => {}
            }
        }
        (ax, ay, aw, ah)
    }

    // -----------------------------------------------------------------------
    // Scene buffer compositor (mirrors blit_to_scene / update_scene_region)
    // -----------------------------------------------------------------------

    struct SceneBuffer { pixels: Vec<u32>, width: u32, height: u32 }

    impl SceneBuffer {
        fn new(w: u32, h: u32) -> Self { Self { pixels: vec![0; (w * h) as usize], width: w, height: h } }
        fn filled(w: u32, h: u32, c: u32) -> Self { Self { pixels: vec![c; (w * h) as usize], width: w, height: h } }

        fn blit(&mut self, src: &[u32], src_w: u32, dx: i32, dy: i32, w: u32, h: u32, clip: Option<DirtyRect>) -> usize {
            let sw = self.width as usize; let sh = self.height as usize;
            let mut x0 = dx.max(0) as usize;
            let mut y0 = dy.max(0) as usize;
            let mut x1 = ((dx + w as i32).max(0) as usize).min(sw);
            let mut y1 = ((dy + h as i32).max(0) as usize).min(sh);
            if let Some(c) = clip {
                x0 = x0.max(c.x as usize); y0 = y0.max(c.y as usize);
                x1 = x1.min((c.x + c.w) as usize); y1 = y1.min((c.y + c.h) as usize);
            }
            if x0 >= x1 || y0 >= y1 { return 0; }
            let cw = x1 - x0;
            let sx = (x0 as i32 - dx).max(0) as usize;
            let sy = (y0 as i32 - dy).max(0) as usize;
            let mut written = 0;
            for row in 0..(y1 - y0) {
                let s = (sy + row) * src_w as usize + sx;
                let d = (y0 + row) * sw + x0;
                self.pixels[d..d + cw].copy_from_slice(&src[s..s + cw]);
                written += cw;
            }
            written
        }

        /// Mirrors update_scene_region: background first, then windows clipped to damage.
        fn update_region(&mut self, bg: &[u32], wins: &[(&[u32], i32, i32, u32, u32)], dmg: DirtyRect) -> usize {
            let sw = self.width;
            let mut total = self.blit(bg, sw, dmg.x as i32, dmg.y as i32, dmg.w, dmg.h, None);
            for &(wbuf, wx, wy, ww, wh) in wins {
                let dx1 = dmg.x as i32 + dmg.w as i32; let dy1 = dmg.y as i32 + dmg.h as i32;
                if wx >= dx1 || wx + ww as i32 <= dmg.x as i32 || wy >= dy1 || wy + wh as i32 <= dmg.y as i32 { continue; }
                total += self.blit(wbuf, ww, wx, wy, ww, wh, Some(dmg));
            }
            total
        }

        fn at(&self, x: u32, y: u32) -> u32 { self.pixels[(y * self.width + x) as usize] }
    }

    // -----------------------------------------------------------------------
    // Z-order (mirrors compositor.rs z_push_toplevel / z_push / z_raise)
    // -----------------------------------------------------------------------

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Kind { Toplevel, Panel }

    struct ZOrder { order: Vec<(u64, Kind)> }
    impl ZOrder {
        fn new() -> Self { Self { order: vec![] } }
        fn push_panel(&mut self, id: u64) { self.order.push((id, Kind::Panel)); }
        fn push_toplevel(&mut self, id: u64) {
            match self.order.iter().position(|(_, k)| *k == Kind::Panel) {
                Some(p) => self.order.insert(p, (id, Kind::Toplevel)),
                None => self.order.push((id, Kind::Toplevel)),
            }
        }
        fn remove(&mut self, id: u64) { self.order.retain(|(x, _)| *x != id); }
        fn raise(&mut self, id: u64, kind: Kind) {
            self.remove(id);
            match kind { Kind::Panel => self.push_panel(id), Kind::Toplevel => self.push_toplevel(id) }
        }
        fn ids(&self) -> Vec<u64> { self.order.iter().map(|(id, _)| *id).collect() }
    }

    // -----------------------------------------------------------------------
    // Damage / bounding-box tests
    // -----------------------------------------------------------------------

    /// Two cubes at opposite corners → their bounding-box covers most of the screen.
    /// This is the root cause of 1–2 fps: one enormous VRAM write per frame.
    #[test]
    fn bounding_box_explosion_with_merged_damage() {
        let mut state = DamageState::default();
        state.mark_damage(DirtyRect { x: 5, y: 5, w: CUBE1_SIZE, h: CUBE1_SIZE });
        state.mark_damage(DirtyRect { x: SCREEN_W - CUBE2_SIZE - 5, y: SCREEN_H - CUBE2_SIZE - 5, w: CUBE2_SIZE, h: CUBE2_SIZE });

        match state.flush() {
            FlushResult::PartialUpdate { damage } => {
                let bbox = damage.w as u64 * damage.h as u64;
                let cubes = (CUBE1_SIZE * CUBE1_SIZE + CUBE2_SIZE * CUBE2_SIZE) as u64;
                eprintln!("\n=== Bounding-box explosion ===");
                eprintln!("  Two cubes combined : {} px", cubes);
                eprintln!("  Bounding box       : {}×{} = {} px", damage.w, damage.h, bbox);
                eprintln!("  Overhead           : {}× actual dirty area", bbox / cubes);
                // The bounding box must be vastly larger than the actual dirty pixels.
                assert!(bbox / cubes > 100, "Expected huge overhead, got {}×", bbox / cubes);
            }
            other => panic!("Expected PartialUpdate, got {:?}", other),
        }
    }

    /// Per-message flush: each UpdateWindow is flushed before the next one
    /// accumulates into a bounding box. VRAM writes stay small.
    #[test]
    fn per_message_flush_keeps_each_rect_small() {
        let mut state = DamageState::default();
        let mut flushed: Vec<DirtyRect> = vec![];

        state.mark_damage(DirtyRect { x: 5, y: 5, w: CUBE1_SIZE, h: CUBE1_SIZE });
        if let Some(d) = state.flush_window_update() { flushed.push(d); }

        state.mark_damage(DirtyRect { x: SCREEN_W - CUBE2_SIZE - 5, y: SCREEN_H - CUBE2_SIZE - 5, w: CUBE2_SIZE, h: CUBE2_SIZE });
        if let Some(d) = state.flush_window_update() { flushed.push(d); }

        assert_eq!(state.flush(), FlushResult::Nothing, "Nothing left after per-message flushes");
        assert_eq!(flushed.len(), 2);

        for (i, d) in flushed.iter().enumerate() {
            let area = d.w as u64 * d.h as u64;
            eprintln!("Per-message flush #{}: {}×{} = {} px", i + 1, d.w, d.h, area);
            // Each flush should be roughly one cube, not screen-sized.
            assert!(area <= (CUBE1_SIZE.max(CUBE2_SIZE) as u64).pow(2) * 4,
                "Flush #{} area {} px is too large", i + 1, area);
        }
    }

    /// When N messages are queued (DS was delayed), per-message flush does N present() calls.
    /// If VRAM writes are slow (WRITE_THROUGH + KVM EPT tracking), N calls >> 1 call.
    /// This is the trade-off my flush_window_update "fix" introduced.
    #[test]
    fn per_message_flush_multiplies_present_calls() {
        // Simulate 5 queued updates: cube1 moved 3 frames, cube2 moved 2 frames.
        let updates = [
            DirtyRect { x: 100, y: 100, w: CUBE1_SIZE, h: CUBE1_SIZE },
            DirtyRect { x: 102, y: 103, w: CUBE1_SIZE, h: CUBE1_SIZE },
            DirtyRect { x: 104, y: 106, w: CUBE1_SIZE, h: CUBE1_SIZE },
            DirtyRect { x: 600, y: 400, w: CUBE2_SIZE, h: CUBE2_SIZE },
            DirtyRect { x: 603, y: 403, w: CUBE2_SIZE, h: CUBE2_SIZE },
        ];

        // Strategy A: per-message flush (my fix)
        let mut calls_a = 0;
        let mut vram_px_a = 0u64;
        let mut s = DamageState::default();
        for &r in &updates {
            s.mark_damage(r);
            if let Some(d) = s.flush_window_update() {
                calls_a += 1;
                vram_px_a += d.w as u64 * d.h as u64;
            }
        }
        s.flush(); // nothing left

        // Strategy B: single end-of-loop flush (original)
        let mut calls_b = 0;
        let mut vram_px_b = 0u64;
        let mut s = DamageState::default();
        for &r in &updates { s.mark_damage(r); }
        if let FlushResult::PartialUpdate { damage } = s.flush() {
            calls_b += 1;
            vram_px_b += damage.w as u64 * damage.h as u64;
        }

        eprintln!("\n=== present() call comparison ({} queued messages) ===", updates.len());
        eprintln!("  Per-message flush  : {} present() calls, {} VRAM px written", calls_a, vram_px_a);
        eprintln!("  End-of-loop flush  : {} present() call,  {} VRAM px written", calls_b, vram_px_b);
        eprintln!("  Call overhead      : {}×", calls_a / calls_b.max(1));
        eprintln!("  VRAM pixel savings : strategy A writes {}× less per present()", vram_px_b / vram_px_a.max(1));

        // Per-message flush always does more present() calls.
        // If VRAM is fast → A wins (smaller per-call). If VRAM is slow → B wins (fewer calls).
        assert!(calls_a > calls_b, "Per-message flush must produce more present() calls");
    }

    /// flush_window_update must skip while a full-redraw is pending.
    /// If it didn't, it would composite into a stale scene_buf.
    #[test]
    fn flush_window_update_deferred_during_full_redraw() {
        let mut state = DamageState::default();
        state.mark_full_redraw(); // CreateWindow / layout recalc

        // Cube sends its first UpdateWindow before DS runs flush()
        state.mark_damage(DirtyRect { x: 10, y: 10, w: 25, h: 25 });

        let r = state.flush_window_update();
        assert_eq!(r, None, "flush_window_update must skip when full_redraw is set");

        // Main flush does the full redraw
        assert_eq!(state.flush(), FlushResult::FullRedraw);
    }

    /// Cursor damage (set after the IPC drain loop) must not be consumed by
    /// flush_window_update; it must survive to the main flush() call.
    #[test]
    fn cursor_damage_survives_to_main_flush() {
        let mut state = DamageState::default();

        // Window update flushed in drain loop
        state.mark_damage(DirtyRect { x: 10, y: 10, w: 25, h: 25 });
        assert!(state.flush_window_update().is_some());

        // Cursor moves — expand_pending only (no pending_scene_update)
        let cursor_dmg = DirtyRect { x: 100, y: 100, w: 26, h: 21 };
        match &mut state.pending_damage {
            Some(d) => d.expand(cursor_dmg.x, cursor_dmg.y, cursor_dmg.w, cursor_dmg.h),
            None => state.pending_damage = Some(cursor_dmg),
        }

        // Main flush must see the cursor damage
        match state.flush() {
            FlushResult::PartialUpdate { damage } => {
                eprintln!("Cursor flush damage: {:?}", damage);
                // pending_scene_update is false — scene not re-composited, just re-presented.
                // This is correct: cursor is drawn on top of the existing back_buffer.
                assert!(!state.pending_scene_update);
            }
            other => panic!("Expected cursor PartialUpdate, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Tiling layout tests
    // -----------------------------------------------------------------------

    #[test]
    fn single_toplevel_fills_available_area() {
        let tiles = compute_tile_layout(1, 0, 0, SCREEN_W, SCREEN_H);
        assert_eq!(tiles.len(), 1);
        let (x, y, w, h) = tiles[0];
        assert_eq!(x, OUTER_GAP as i32);
        assert_eq!(y, OUTER_GAP as i32);
        assert_eq!(w, SCREEN_W - 2 * OUTER_GAP);
        assert_eq!(h, SCREEN_H - 2 * OUTER_GAP);
        eprintln!("Single toplevel: ({x},{y}) {w}×{h}");
    }

    #[test]
    fn two_toplevels_split_with_gap() {
        let tiles = compute_tile_layout(2, 0, 0, SCREEN_W, SCREEN_H);
        assert_eq!(tiles.len(), 2);
        let (x0, y0, w0, h0) = tiles[0];
        let (x1, y1, w1, h1) = tiles[1];
        assert_eq!(y0, y1, "Same top edge");
        assert_eq!(h0, h1, "Same height");
        assert_eq!(x1, x0 + w0 as i32 + INNER_GAP as i32, "INNER_GAP between tiles");
        assert_eq!(w0 + w1, SCREEN_W - 2 * OUTER_GAP - INNER_GAP, "Tiles fill usable width");
        eprintln!("Tile 0: ({x0},{y0}) {w0}×{h0}");
        eprintln!("Tile 1: ({x1},{y1}) {w1}×{h1}");
    }

    #[test]
    fn tiles_never_overflow_screen() {
        for n in 1..=5 {
            let tiles = compute_tile_layout(n, 0, 0, SCREEN_W, SCREEN_H);
            assert_eq!(tiles.len(), n);
            for &(x, y, w, h) in &tiles {
                assert!(x >= 0 && (x + w as i32) <= SCREEN_W as i32, "n={n}: tile x overflow");
                assert!(y >= 0 && (y + h as i32) <= SCREEN_H as i32, "n={n}: tile y overflow");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Available area tests
    // -----------------------------------------------------------------------

    #[test]
    fn no_panels_gives_full_screen() {
        assert_eq!(available_area(SCREEN_W, SCREEN_H, &[]), (0, 0, SCREEN_W, SCREEN_H));
    }

    #[test]
    fn top_panel_shifts_y_and_reduces_h() {
        let (ax, ay, aw, ah) = available_area(SCREEN_W, SCREEN_H, &[Panel { anchor: 0, zone: 40 }]);
        assert_eq!((ax, ay, aw, ah), (0, 40, SCREEN_W, SCREEN_H - 40));
    }

    #[test]
    fn toplevels_respect_top_panel() {
        let (ax, ay, aw, ah) = available_area(SCREEN_W, SCREEN_H, &[Panel { anchor: 0, zone: 40 }]);
        let tiles = compute_tile_layout(2, ax, ay, aw, ah);
        for &(_, y, _, h) in &tiles {
            assert!(y >= 40, "Tile y={y} must be below panel");
            assert!((y + h as i32) <= SCREEN_H as i32);
        }
    }

    // -----------------------------------------------------------------------
    // Scene compositing tests
    // -----------------------------------------------------------------------

    #[test]
    fn blit_full_window_writes_correct_region() {
        let mut scene = SceneBuffer::new(100, 100);
        let win: Vec<u32> = vec![0xFF_FF_00_00; 50 * 50];
        let written = scene.blit(&win, 50, 10, 10, 50, 50, None);
        assert_eq!(written, 50 * 50);
        assert_eq!(scene.at(10, 10), 0xFF_FF_00_00);
        assert_eq!(scene.at(59, 59), 0xFF_FF_00_00);
        assert_eq!(scene.at(9, 9), 0, "Outside window must be zero");
    }

    #[test]
    fn blit_with_clip_limits_writes() {
        let mut scene = SceneBuffer::new(200, 200);
        let win: Vec<u32> = vec![0xFF; 100 * 100];
        let clip = DirtyRect { x: 60, y: 60, w: 20, h: 20 };
        let written = scene.blit(&win, 100, 50, 50, 100, 100, Some(clip));
        assert_eq!(written, 20 * 20, "Clip must limit writes to 20×20");
        assert_eq!(scene.at(60, 60), 0xFF);
        assert_eq!(scene.at(50, 50), 0, "Outside clip must remain zero");
    }

    #[test]
    fn update_region_skips_non_intersecting_window() {
        let bg: Vec<u32> = vec![0xAA; (SCREEN_W * SCREEN_H) as usize];
        let win: Vec<u32> = vec![0xBB; 100 * 100];
        let mut scene = SceneBuffer::new(SCREEN_W, SCREEN_H);
        // Damage in top-left, window at (600, 400) — no intersection.
        let written = scene.update_region(&bg, &[(&win, 600, 400, 100, 100)], DirtyRect { x: 0, y: 0, w: 50, h: 50 });
        assert_eq!(written, 50 * 50, "Only background damage area should be written");
        assert_ne!(scene.at(25, 25), 0xBB);
    }

    #[test]
    fn update_region_window_overwrites_background() {
        let bg: Vec<u32> = vec![0xAAAA_AAAA; 100 * 100];
        let win: Vec<u32> = vec![0xBBBB_BBBB; 50 * 50];
        let mut scene = SceneBuffer::filled(100, 100, 0);
        scene.update_region(&bg, &[(&win, 10, 10, 50, 50)], DirtyRect { x: 10, y: 10, w: 50, h: 50 });
        assert_eq!(scene.at(10, 10), 0xBBBB_BBBB, "Window must overwrite background");
        assert_eq!(scene.at(59, 59), 0xBBBB_BBBB);
    }

    #[test]
    fn update_region_clips_window_to_damage() {
        let bg: Vec<u32> = vec![0xAA; 200 * 200];
        let win: Vec<u32> = vec![0xBB; 100 * 100];
        let mut scene = SceneBuffer::new(200, 200);
        // Window at (50, 50), damage at (80, 80, 40, 40) — overlap is (80..120, 80..120).
        scene.update_region(&bg, &[(&win, 50, 50, 100, 100)], DirtyRect { x: 80, y: 80, w: 40, h: 40 });
        assert_eq!(scene.at(80, 80), 0xBB, "Overlap area must be window colour");
        assert_eq!(scene.at(50, 50), 0, "Outside damage must not be written");
    }

    // -----------------------------------------------------------------------
    // Z-order tests
    // -----------------------------------------------------------------------

    #[test]
    fn panels_always_topmost() {
        let mut z = ZOrder::new();
        z.push_toplevel(1); z.push_toplevel(2);
        z.push_panel(3); // panel added after toplevels
        assert_eq!(z.ids(), vec![1, 2, 3]);
    }

    #[test]
    fn new_toplevel_inserted_below_panel() {
        let mut z = ZOrder::new();
        z.push_toplevel(1); z.push_panel(2); z.push_toplevel(3);
        let ids = z.ids();
        let p2 = ids.iter().position(|&x| x == 2).unwrap();
        let p3 = ids.iter().position(|&x| x == 3).unwrap();
        assert!(p3 < p2, "Toplevel 3 (pos {p3}) must be below panel 2 (pos {p2}): {:?}", ids);
    }

    #[test]
    fn raise_toplevel_stays_below_panel() {
        let mut z = ZOrder::new();
        z.push_toplevel(1); z.push_toplevel(2); z.push_panel(3);
        z.raise(1, Kind::Toplevel);
        let ids = z.ids();
        assert_eq!(ids.last(), Some(&3), "Panel must stay on top: {:?}", ids);
    }
}

// -----------------------------------------------------------------------
// Dwindle layout tests (pure-Rust mirror of layout.rs dwindle_recurse)
// Used to pinpoint the page-fault crash when a 4th tiled window is opened.
// -----------------------------------------------------------------------
#[cfg(test)]
mod dwindle_layout_tests {
    const SCREEN_W: u32 = 1280;
    const SCREEN_H: u32 = 720;
    const OUTER_GAP: u32 = 8;
    const INNER_GAP: u32 = 8;
    const MIN_RATIO: f32 = 0.1;

    /// Mirror of the 4-state `LayoutDir` in layout.rs.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum LayoutDir { Horizontal, Vertical, HorizontalReversed, VerticalReversed }

    impl LayoutDir {
        fn next_spiral(self) -> Self {
            match self {
                Self::Horizontal         => Self::Vertical,
                Self::Vertical           => Self::HorizontalReversed,
                Self::HorizontalReversed => Self::VerticalReversed,
                Self::VerticalReversed   => Self::Horizontal,
            }
        }
    }

    /// Exact mirror of `dwindle_recurse` in layout.rs (golden-spiral, 4-state).
    fn dwindle_recurse(
        n: usize, ratios: &[f32],
        x: i32, y: i32, w: u32, h: u32,
        dir: LayoutDir, inner_gap: u32,
        out: &mut Vec<(i32, i32, u32, u32)>,
    ) {
        if n == 0 { return; }
        if n == 1 { out.push((x, y, w, h)); return; }
        let ratio = ratios[0].clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        let next  = dir.next_spiral();
        match dir {
            LayoutDir::Horizontal => {
                let lw = ((w as f32 * ratio) as u32).min(w.saturating_sub(inner_gap + 1));
                let rw = w.saturating_sub(lw + inner_gap);
                out.push((x, y, lw, h));
                dwindle_recurse(n - 1, &ratios[1..], x + lw as i32 + inner_gap as i32, y, rw, h, next, inner_gap, out);
            }
            LayoutDir::Vertical => {
                let th = ((h as f32 * ratio) as u32).min(h.saturating_sub(inner_gap + 1));
                let bh = h.saturating_sub(th + inner_gap);
                out.push((x, y, w, th));
                dwindle_recurse(n - 1, &ratios[1..], x, y + th as i32 + inner_gap as i32, w, bh, next, inner_gap, out);
            }
            LayoutDir::HorizontalReversed => {
                let rw = ((w as f32 * ratio) as u32).min(w.saturating_sub(inner_gap + 1));
                let lw = w.saturating_sub(rw + inner_gap);
                out.push((x + lw as i32 + inner_gap as i32, y, rw, h));
                dwindle_recurse(n - 1, &ratios[1..], x, y, lw, h, next, inner_gap, out);
            }
            LayoutDir::VerticalReversed => {
                let bh = ((h as f32 * ratio) as u32).min(h.saturating_sub(inner_gap + 1));
                let th = h.saturating_sub(bh + inner_gap);
                out.push((x, y + th as i32 + inner_gap as i32, w, bh));
                dwindle_recurse(n - 1, &ratios[1..], x, y, w, th, next, inner_gap, out);
            }
        }
    }

    /// Build n golden-ratio spiral windows for the standard screen (starting dir = Horizontal).
    fn compute_dwindle(n: usize) -> Vec<(i32, i32, u32, u32)> {
        if n == 0 { return vec![]; }
        let gx = OUTER_GAP as i32;
        let gy = OUTER_GAP as i32;
        let gw = SCREEN_W.saturating_sub(2 * OUTER_GAP);
        let gh = SCREEN_H.saturating_sub(2 * OUTER_GAP);
        let ratios = vec![0.618f32; n];
        let mut out = Vec::new();
        dwindle_recurse(n, &ratios, gx, gy, gw, gh, LayoutDir::Horizontal, INNER_GAP, &mut out);
        out
    }

    /// Exact mirror of `dir_at_level` in layout.rs (initial dir = Horizontal).
    fn dir_at_level(index: usize) -> LayoutDir {
        let mut dir = LayoutDir::Horizontal;
        for _ in 0..index { dir = dir.next_spiral(); }
        dir
    }

    /// Exact mirror of `level_span_at` in layout.rs.
    fn level_span_at(index: usize) -> u32 {
        let ig = INNER_GAP;
        let mut w = SCREEN_W.saturating_sub(2 * OUTER_GAP);
        let mut h = SCREEN_H.saturating_sub(2 * OUTER_GAP);
        let ratios = [0.618f32; 16];
        let mut dir = LayoutDir::Horizontal;
        for i in 0..index {
            let ratio = ratios[i].clamp(MIN_RATIO, 1.0 - MIN_RATIO);
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

    // --- correctness: single window ---

    #[test]
    fn dwindle_single_fills_area() {
        let tiles = compute_dwindle(1);
        assert_eq!(tiles.len(), 1);
        let (x, y, w, h) = tiles[0];
        assert_eq!(x, OUTER_GAP as i32);
        assert_eq!(y, OUTER_GAP as i32);
        assert_eq!(w, SCREEN_W - 2 * OUTER_GAP);
        assert_eq!(h, SCREEN_H - 2 * OUTER_GAP);
    }

    // --- no zero-dimension windows ---

    #[test]
    fn dwindle_four_windows_no_zero_dims() {
        let tiles = compute_dwindle(4);
        assert_eq!(tiles.len(), 4);
        eprintln!("\n=== 4-window dwindle (crash case) ===");
        for (i, &(x, y, w, h)) in tiles.iter().enumerate() {
            eprintln!("  Window {i}: ({x},{y}) {w}×{h}  buf={}", w as u64 * h as u64 * 4);
            assert!(w > 0, "window {i} has zero width");
            assert!(h > 0, "window {i} has zero height");
        }
    }

    #[test]
    fn dwindle_no_zero_dims_n1_to_n8() {
        for n in 1..=8 {
            let tiles = compute_dwindle(n);
            assert_eq!(tiles.len(), n, "n={n}: wrong tile count");
            for (i, &(x, y, w, h)) in tiles.iter().enumerate() {
                assert!(w > 0, "n={n} window {i}: zero width (x={x},y={y},w={w},h={h})");
                assert!(h > 0, "n={n} window {i}: zero height (x={x},y={y},w={w},h={h})");
            }
        }
    }

    // --- all windows fit within the screen ---

    #[test]
    fn dwindle_all_in_screen_bounds() {
        for n in 1..=8 {
            let tiles = compute_dwindle(n);
            for (i, &(x, y, w, h)) in tiles.iter().enumerate() {
                assert!(x >= 0, "n={n} window {i}: x={x} < 0");
                assert!(y >= 0, "n={n} window {i}: y={y} < 0");
                let r = x + w as i32;
                let b = y + h as i32;
                assert!(r <= SCREEN_W as i32, "n={n} window {i}: right={r} > {SCREEN_W}");
                assert!(b <= SCREEN_H as i32, "n={n} window {i}: bottom={b} > {SCREEN_H}");
            }
        }
    }

    // --- no overlapping windows ---

    #[test]
    fn dwindle_no_overlap() {
        for n in 2..=6 {
            let tiles = compute_dwindle(n);
            for i in 0..n {
                for j in (i + 1)..n {
                    let (ax, ay, aw, ah) = tiles[i];
                    let (bx, by, bw, bh) = tiles[j];
                    let ox = ax.max(bx) < (ax + aw as i32).min(bx + bw as i32);
                    let oy = ay.max(by) < (ay + ah as i32).min(by + bh as i32);
                    assert!(
                        !(ox && oy),
                        "n={n}: windows {i}+{j} overlap — ({ax},{ay},{aw},{ah}) vs ({bx},{by},{bw},{bh})"
                    );
                }
            }
        }
    }

    // --- direction alternation ---

    #[test]
    fn dir_at_level_alternates() {
        // Golden spiral: H → V → HR → VR → H (4-state cycle)
        assert_eq!(dir_at_level(0), LayoutDir::Horizontal,         "level 0: H  (new window RIGHT)");
        assert_eq!(dir_at_level(1), LayoutDir::Vertical,           "level 1: V  (new window BOTTOM)");
        assert_eq!(dir_at_level(2), LayoutDir::HorizontalReversed, "level 2: HR (new window LEFT)");
        assert_eq!(dir_at_level(3), LayoutDir::VerticalReversed,   "level 3: VR (new window TOP)");
        assert_eq!(dir_at_level(4), LayoutDir::Horizontal,         "level 4: H  (cycle repeats)");
    }

    // --- level_span_at returns positive spans ---

    #[test]
    fn level_span_positive_for_first_eight_levels() {
        eprintln!("\n=== level_span_at (drag-resize spans) ===");
        for i in 0..8 {
            let span = level_span_at(i);
            let dir  = dir_at_level(i);
            eprintln!("  level {i} ({dir:?}): span={span}px");
            assert!(span > 0, "level {i}: span=0 — drag resize would divide by zero");
        }
    }

    // --- the Configure geometry actually sent to clients ---

    #[test]
    fn configure_dims_for_four_windows_are_reasonable() {
        let tiles = compute_dwindle(4);
        eprintln!("\n=== Configure events sent when 4th tiled window opens ===");
        for (i, &(x, y, w, h)) in tiles.iter().enumerate() {
            let buf_bytes = w as u64 * h as u64 * 4;
            eprintln!("  Configure #{i}: width={w} height={h} buf_bytes={buf_bytes}  pos=({x},{y})");
            // Every client must be able to allocate a non-empty buffer.
            assert!(w >= 1 && h >= 1, "window {i}: degenerate {w}×{h}");
            // Sanity: buffer must not exceed available memory budget (256 MB).
            assert!(buf_bytes < 256 * 1024 * 1024,
                "window {i}: buf={buf_bytes} bytes suspiciously large");
        }
    }

    // --- total area accounting ---

    #[test]
    fn dwindle_area_sum_close_to_available() {
        for n in 1..=6 {
            let tiles = compute_dwindle(n);
            let tile_area: u64 = tiles.iter().map(|&(_, _, w, h)| w as u64 * h as u64).sum();
            let avail = (SCREEN_W - 2 * OUTER_GAP) as u64 * (SCREEN_H - 2 * OUTER_GAP) as u64;
            // With gaps the tiles never cover the full area, but they must cover at least 50%.
            assert!(tile_area * 2 >= avail,
                "n={n}: tiles cover only {tile_area}px of {avail}px available — suspiciously small");
            eprintln!("n={n}: tile_area={tile_area}/{avail} = {:.1}%", tile_area as f64 / avail as f64 * 100.0);
        }
    }

    // --- incremental add (simulates what the compositor does each time a window opens) ---

    #[test]
    fn incremental_window_add_all_valid() {
        eprintln!("\n=== Incremental add simulation (like real compositor) ===");
        for n in 1..=5 {
            let tiles = compute_dwindle(n);
            eprintln!("  n={n}:");
            for (i, &(x, y, w, h)) in tiles.iter().enumerate() {
                eprintln!("    Window {i}: ({x},{y}) {w}×{h}");
                assert!(w > 0 && h > 0, "n={n} window {i}: zero dimension {w}×{h}");
                assert!(x + w as i32 <= SCREEN_W as i32 && y + h as i32 <= SCREEN_H as i32,
                    "n={n} window {i}: out of bounds");
            }
        }
    }
}

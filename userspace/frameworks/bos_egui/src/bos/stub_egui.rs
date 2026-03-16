/// Minimal egui-compatible API for the Bos software-rendered path.
///
/// Provides the same surface (Context, CentralPanel, Ui, Response) that
/// hello_egui calls through `use bos_egui::egui`.  On Linux the real egui
/// crate is used instead; on Bos this stub renders directly into the
/// window's shared pixel buffer via embedded_graphics.
use core::cell::RefCell;
use alloc::string::{String, ToString};

pub use embedded_graphics::{
    mono_font::{MonoFont, ascii::{FONT_8X13, FONT_8X13_BOLD}},
    pixelcolor::Rgb888,
};
use embedded_graphics::{
    mono_font::MonoTextStyle,
    prelude::*,
    primitives::{Rectangle, PrimitiveStyle, Line},
    text::Text,
};

use kernel_api_types::graphics::DisplayInfo;
pub use kernel_api_types::{KeyEvent, KeyEventType};
use super::pixel_draw::PixelBuf;

// ── Colours ──────────────────────────────────────────────────────────────────

pub const BG:      Rgb888 = Rgb888::new(0x18, 0x18, 0x1e);
pub const FG:      Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);
pub const HEADING: Rgb888 = Rgb888::new(0x8a, 0xad, 0xf4);
pub const SEP:     Rgb888 = Rgb888::new(0x36, 0x3a, 0x4f);
const BTN_BG:  Rgb888 = Rgb888::new(0x36, 0x3a, 0x4f);
const BTN_HOV: Rgb888 = Rgb888::new(0x49, 0x4d, 0x64);
const BTN_FG:  Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);
const INPUT_BG:      Rgb888 = Rgb888::new(0x1e, 0x1e, 0x2e);
const INPUT_FG:      Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);
const INPUT_BR:      Rgb888 = Rgb888::new(0x49, 0x4d, 0x64);
const INPUT_BR_HOV:  Rgb888 = Rgb888::new(0x6c, 0x70, 0x86); // hover border
const INPUT_BR_FOCUS: Rgb888 = Rgb888::new(0x8a, 0xad, 0xf4); // focused border (accent)

// Additional theme colours for styled HTML content.
pub const LINK:     Rgb888 = Rgb888::new(0x8b, 0xd5, 0xca); // teal
pub const CODE:     Rgb888 = Rgb888::new(0xa6, 0xda, 0x95); // green
pub const EMPHASIS: Rgb888 = Rgb888::new(0xb7, 0xbf, 0xf8); // lavender
pub const DIMMED:   Rgb888 = Rgb888::new(0xa5, 0xad, 0xcb); // subtext

// ── Context ───────────────────────────────────────────────────────────────────

struct Inner {
    pixels: *mut u32,
    width:  u32,
    height: u32,
    info:   DisplayInfo,
    cursor_x: f32,
    cursor_y: f32,
    /// Mouse click coordinates this frame (window-relative), if any.
    click: Option<(f32, f32)>,
    /// Key event this frame, if any.
    key: Option<KeyEvent>,
    /// Current vertical drawing position for the layout pass.
    draw_y: i32,
    /// Left margin.
    margin: i32,
    /// Current horizontal drawing position (used inside `horizontal` layout).
    draw_x: i32,
    /// True when inside a `horizontal` layout group.
    in_horizontal: bool,
    /// Maximum widget height seen so far in the current horizontal group.
    horiz_max_h: i32,
}

pub struct Context {
    inner: RefCell<Inner>,
}

impl Context {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pixels: &mut [u32],
        width: u32,
        height: u32,
        info: DisplayInfo,
        cursor_x: f32,
        cursor_y: f32,
        click: Option<(f32, f32)>,
        key: Option<KeyEvent>,
    ) -> Self {
        // Clear to background
        let bg = info.build_pixel(BG.r(), BG.g(), BG.b());
        pixels.iter_mut().for_each(|p| *p = bg);

        Context {
            inner: RefCell::new(Inner {
                pixels: pixels.as_mut_ptr(),
                width,
                height,
                info,
                cursor_x,
                cursor_y,
                click,
                key,
                draw_y: 0,
                margin: 12,
                draw_x: 0,
                in_horizontal: false,
                horiz_max_h: 0,
            }),
        }
    }

    /// Returns `true` if a key of the given type was pressed this frame.
    pub fn key_pressed(&self, code: KeyEventType) -> bool {
        self.inner.borrow().key
            .is_some_and(|k| k.event_type == code && k.pressed)
    }

    /// Returns the key event for this frame, if any.
    pub fn key_event(&self) -> Option<KeyEvent> {
        self.inner.borrow().key
    }

    /// Returns the window size in pixels.
    pub fn screen_size(&self) -> (u32, u32) {
        let inner = self.inner.borrow();
        (inner.width, inner.height)
    }

    /// Returns the current mouse position.
    pub fn mouse_pos(&self) -> (f32, f32) {
        let inner = self.inner.borrow();
        (inner.cursor_x, inner.cursor_y)
    }

    /// Returns and consumes the click coordinates for this frame, if any.
    pub fn take_click(&self) -> Option<(f32, f32)> {
        self.inner.borrow_mut().click.take()
    }

    fn pixels_mut<'a>(&'a self, inner: &'a mut Inner) -> PixelBuf<'a> {
        PixelBuf {
            pixels: unsafe {
                core::slice::from_raw_parts_mut(
                    inner.pixels,
                    (inner.width * inner.height) as usize,
                )
            },
            width:  inner.width,
            height: inner.height,
            info:   inner.info,
        }
    }
}

// ── CentralPanel ─────────────────────────────────────────────────────────────

pub struct CentralPanel;

impl CentralPanel {
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Self { CentralPanel }

    pub fn show<R, F>(self, ctx: &Context, add_contents: F) -> R
    where F: FnOnce(&mut Ui) -> R
    {
        let mut ui = Ui { ctx };
        // Reset vertical cursor for layout
        ctx.inner.borrow_mut().draw_y = 12;
        add_contents(&mut ui)
    }
}

// ── Ui ───────────────────────────────────────────────────────────────────────

pub struct Ui<'a> {
    ctx: &'a Context,
}

impl<'a> Ui<'a> {
    pub fn heading(&mut self, text: impl ToString) -> Response {
        let s = text.to_string();
        let mut inner = self.ctx.inner.borrow_mut();
        let x = inner.margin;
        let y = inner.draw_y;
        let mut buf = self.ctx.pixels_mut(&mut inner);
        draw_text(&mut buf, &s, x, y, HEADING, &FONT_8X13_BOLD);
        inner.draw_y += 20;
        Response { clicked: false }
    }

    pub fn label(&mut self, text: impl ToString) -> Response {
        let s = text.to_string();
        let mut inner = self.ctx.inner.borrow_mut();
        let x = inner.margin;
        let y = inner.draw_y;
        let mut buf = self.ctx.pixels_mut(&mut inner);
        draw_text(&mut buf, &s, x, y, FG, &FONT_8X13);
        inner.draw_y += 17;
        Response { clicked: false }
    }

    pub fn separator(&mut self) -> Response {
        let mut inner = self.ctx.inner.borrow_mut();
        let x0 = inner.margin;
        let y  = inner.draw_y + 4;
        let x1 = inner.width as i32 - inner.margin;
        let mut buf = self.ctx.pixels_mut(&mut inner);
        let _ = Line::new(
            embedded_graphics::geometry::Point::new(x0, y),
            embedded_graphics::geometry::Point::new(x1, y),
        )
        .into_styled(PrimitiveStyle::with_stroke(SEP, 1))
        .draw(&mut buf);
        inner.draw_y += 14;
        Response { clicked: false }
    }

    pub fn button(&mut self, text: impl ToString) -> Response {
        let s = text.to_string();
        let mut inner = self.ctx.inner.borrow_mut();

        // Measure approximate button size: each char ≈ 8px wide, 13px tall + padding
        let btn_w = (s.len() as i32) * 8 + 16; // 8px padding per side
        let btn_h = 22i32;
        let bx = if inner.in_horizontal { inner.draw_x } else { inner.margin };
        let by_ = inner.draw_y;

        // Hit-test
        let (cx_i, cy_i) = (inner.cursor_x as i32, inner.cursor_y as i32);
        let hovered = cx_i >= bx && cx_i < bx + btn_w && cy_i >= by_ && cy_i < by_ + btn_h;
        let clicked = if let Some((clx, cly)) = inner.click {
            let (clx, cly) = (clx as i32, cly as i32);
            clx >= bx && clx < bx + btn_w && cly >= by_ && cly < by_ + btn_h
        } else {
            false
        };
        if clicked { inner.click = None; } // consume click

        let bg_col = if hovered || clicked { BTN_HOV } else { BTN_BG };
        let mut buf = self.ctx.pixels_mut(&mut inner);

        // Background
        let _ = Rectangle::new(
            embedded_graphics::geometry::Point::new(bx, by_),
            embedded_graphics::geometry::Size::new(btn_w as u32, btn_h as u32),
        )
        .into_styled(PrimitiveStyle::with_fill(bg_col))
        .draw(&mut buf);

        // Hover border
        if hovered {
            let _ = Rectangle::new(
                embedded_graphics::geometry::Point::new(bx, by_),
                embedded_graphics::geometry::Size::new(btn_w as u32, btn_h as u32),
            )
            .into_styled(PrimitiveStyle::with_stroke(HEADING, 1))
            .draw(&mut buf);
        }

        // Label centred vertically: btn_h=22, font_h=13 → top at (22-13)/2 = 4
        draw_text(&mut buf, &s, bx + 8, by_ + 4, BTN_FG, &FONT_8X13);

        if inner.in_horizontal {
            inner.draw_x += btn_w + 6;
            if btn_h > inner.horiz_max_h { inner.horiz_max_h = btn_h; }
        } else {
            inner.draw_y += btn_h + 8;
        }
        Response { clicked }
    }

    /// Draw a single-line text input with focus and cursor blink support.
    ///
    /// - `focused`: whether the input has keyboard focus (affects border color)
    /// - `cursor_visible`: whether to show the text cursor (for blink animation)
    ///
    /// Returns a `Response` whose `clicked` field is `true` if the user clicked
    /// inside the input box this frame.
    #[allow(clippy::ptr_arg)]
    pub fn text_edit_singleline(
        &mut self,
        text: &mut String,
        focused: bool,
        cursor_visible: bool,
    ) -> Response {
        let mut inner = self.ctx.inner.borrow_mut();

        let box_h = 28i32;
        let padding = 6;
        let box_w = inner.width as i32 - (inner.margin * 2);
        let bx = inner.margin;
        let by = inner.draw_y;

        // Hit-test for hover and click
        let (mx, my) = (inner.cursor_x as i32, inner.cursor_y as i32);
        let hovered = mx >= bx && mx < bx + box_w && my >= by && my < by + box_h;
        let clicked = if let Some((clx, cly)) = inner.click {
            let (clx, cly) = (clx as i32, cly as i32);
            clx >= bx && clx < bx + box_w && cly >= by && cly < by + box_h
        } else {
            false
        };
        if clicked { inner.click = None; }

        // Choose border color based on state
        let border = if focused {
            INPUT_BR_FOCUS
        } else if hovered {
            INPUT_BR_HOV
        } else {
            INPUT_BR
        };

        let mut buf = self.ctx.pixels_mut(&mut inner);

        // Background
        let _ = Rectangle::new(
            embedded_graphics::geometry::Point::new(bx, by),
            embedded_graphics::geometry::Size::new(box_w as u32, box_h as u32),
        )
        .into_styled(PrimitiveStyle::with_fill(INPUT_BG))
        .draw(&mut buf);

        // Border
        let _ = Rectangle::new(
            embedded_graphics::geometry::Point::new(bx, by),
            embedded_graphics::geometry::Size::new(box_w as u32, box_h as u32),
        )
        .into_styled(PrimitiveStyle::with_stroke(border, 1))
        .draw(&mut buf);

        // Text
        draw_text(&mut buf, text, bx + padding, by + padding, INPUT_FG, &FONT_8X13);

        // Cursor (only when focused and visible for blink)
        if focused && cursor_visible {
            let cursor_x = bx + padding + (text.len() as i32) * 8;
            draw_text(&mut buf, "|", cursor_x, by + padding, INPUT_FG, &FONT_8X13);
        }

        inner.draw_y += box_h + 8;
        Response { clicked }
    }

    pub fn selectable_label(&mut self, selected: bool, text: impl ToString) -> Response {
        let s = text.to_string();
        let mut inner = self.ctx.inner.borrow_mut();

        let row_h = 30i32;
        let bx = inner.margin;
        let by_ = inner.draw_y;
        let row_w = inner.width as i32 - (inner.margin * 2);

        let (cx_i, cy_i) = (inner.cursor_x as i32, inner.cursor_y as i32);
        let hovered = cx_i >= bx && cx_i < bx + row_w && cy_i >= by_ && cy_i < by_ + row_h;
        let clicked = if let Some((clx, cly)) = inner.click {
            let (clx, cly) = (clx as i32, cly as i32);
            clx >= bx && clx < bx + row_w && cly >= by_ && cly < by_ + row_h
        } else {
            false
        };
        if clicked { inner.click = None; }

        let mut buf = self.ctx.pixels_mut(&mut inner);

        if selected {
            let _ = Rectangle::new(
                embedded_graphics::geometry::Point::new(bx, by_),
                embedded_graphics::geometry::Size::new(row_w as u32, row_h as u32),
            )
            .into_styled(PrimitiveStyle::with_fill(BTN_HOV))
            .draw(&mut buf);
        } else if hovered {
            let _ = Rectangle::new(
                embedded_graphics::geometry::Point::new(bx, by_),
                embedded_graphics::geometry::Size::new(row_w as u32, row_h as u32),
            )
            .into_styled(PrimitiveStyle::with_stroke(HEADING, 1))
            .draw(&mut buf);
        }

        draw_text(&mut buf, &s, bx + 8, by_ + 8, FG, &FONT_8X13);

        inner.draw_y += row_h + 2;
        Response { clicked }
    }

    pub fn horizontal<R, F>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut Ui) -> R,
    {
        {
            let mut inner = self.ctx.inner.borrow_mut();
            inner.in_horizontal = true;
            inner.draw_x = inner.margin;
            inner.horiz_max_h = 0;
        }
        let result = f(self);
        {
            let mut inner = self.ctx.inner.borrow_mut();
            inner.in_horizontal = false;
            inner.draw_y += inner.horiz_max_h + 8;
        }
        result
    }

    /// Claim the remaining vertical area as a raw pixel canvas for direct rendering.
    ///
    /// The returned `Canvas` starts at the current `draw_y` and extends to the
    /// bottom of the window. `draw_y` is advanced to the window bottom so that
    /// no further widgets are laid out below.
    pub fn canvas(&mut self) -> Canvas<'_> {
        let (origin_x, origin_y, width, height, pixels_ptr, pw, ph, info) = {
            let mut inner = self.ctx.inner.borrow_mut();
            let origin_x = inner.margin;
            let origin_y = inner.draw_y;
            let width = inner.width as i32 - inner.margin * 2;
            let height = inner.height as i32 - origin_y;
            inner.draw_y = inner.height as i32;
            (origin_x, origin_y, width, height, inner.pixels, inner.width, inner.height, inner.info)
        };
        // Safety: the pixel buffer lives for the duration of the frame (owned by
        // the Window). The RefMut is released above but the raw pointer remains valid.
        let buf = PixelBuf {
            pixels: unsafe { core::slice::from_raw_parts_mut(pixels_ptr, (pw * ph) as usize) },
            width: pw,
            height: ph,
            info,
        };
        Canvas { buf, origin_x, origin_y, width, height }
    }

    #[allow(clippy::ptr_arg)]
    pub fn text_edit_multiline(&mut self, text: &mut String) -> Response {
        let mut inner = self.ctx.inner.borrow_mut();

        // Layout Calculations
        // For a multiline stub, we'll wrap the box based on line count or fixed height
        let line_count = text.lines().count().max(1) as i32;
        let padding = 8;
        let char_height = 13;
        let box_w = inner.width as i32 - (inner.margin * 2);
        let box_h = (line_count * char_height) + (padding * 2);

        let bx = inner.margin;
        let by = inner.draw_y;

        // Rendering
        let mut buf = self.ctx.pixels_mut(&mut inner);

        // Draw Background
        let _ = Rectangle::new(
            embedded_graphics::geometry::Point::new(bx, by),
            embedded_graphics::geometry::Size::new(box_w as u32, box_h as u32),
        )
            .into_styled(PrimitiveStyle::with_fill(INPUT_BG))
            .draw(&mut buf);

        // Draw Border
        let _ = Rectangle::new(
            embedded_graphics::geometry::Point::new(bx, by),
            embedded_graphics::geometry::Size::new(box_w as u32, box_h as u32),
        )
            .into_styled(PrimitiveStyle::with_stroke(INPUT_BR, 1))
            .draw(&mut buf);

        // Draw the actual text
        // Note: In a real implementation, you'd handle wrapping here
        draw_text(&mut buf, text, bx + padding, by + padding, INPUT_FG, &FONT_8X13);

        // 4. Update Layout Cursor
        inner.draw_y += box_h + 10;

        // In a stub, we aren't handling focus/keyboard yet
        Response { clicked: false }
    }
}

// ── Canvas ───────────────────────────────────────────────────────────────────

/// A raw pixel canvas for direct rendering into the remaining vertical space.
///
/// Obtained via `Ui::canvas()`. Provides low-level drawing primitives so that
/// apps can render styled content (e.g. HTML) without going through the
/// label/heading widget API.
pub struct Canvas<'a> {
    pub buf: PixelBuf<'a>,
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: i32,
    pub height: i32,
}

impl<'a> Canvas<'a> {
    /// Draw a string at `(x, y)` relative to the canvas origin.
    pub fn draw_text(
        &mut self,
        text: &str,
        x: i32,
        y: i32,
        color: Rgb888,
        font: &MonoFont<'_>,
    ) {
        draw_text(
            &mut self.buf,
            text,
            self.origin_x + x,
            self.origin_y + y,
            color,
            font,
        );
    }

    /// Blit RGBA8 pixels onto the canvas at `(x, y)` relative to canvas origin.
    ///
    /// `src_w` and `src_h` are the source image dimensions.
    /// Pixels outside the canvas are clipped. Fully transparent pixels are skipped.
    pub fn draw_image(&mut self, pixels: &[u8], src_w: u32, src_h: u32, x: i32, y: i32) {
        for row in 0..src_h as i32 {
            let dst_y = self.origin_y + y + row;
            if dst_y < 0 || dst_y >= self.buf.height as i32 { continue; }
            for col in 0..src_w as i32 {
                let dst_x = self.origin_x + x + col;
                if dst_x < 0 || dst_x >= self.buf.width as i32 { continue; }
                let src_idx = (row as usize * src_w as usize + col as usize) * 4;
                if src_idx + 3 >= pixels.len() { continue; }
                let r = pixels[src_idx];
                let g = pixels[src_idx + 1];
                let b = pixels[src_idx + 2];
                let a = pixels[src_idx + 3];
                if a == 0 { continue; }
                let dst_idx = dst_y as usize * self.buf.width as usize + dst_x as usize;
                self.buf.pixels[dst_idx] = self.buf.info.build_pixel(r, g, b);
            }
        }
    }

    /// Draw a horizontal line spanning the full canvas width at `y` (relative to origin).
    pub fn draw_hline(&mut self, y: i32, color: Rgb888) {
        let abs_y = self.origin_y + y;
        let _ = Line::new(
            embedded_graphics::geometry::Point::new(self.origin_x, abs_y),
            embedded_graphics::geometry::Point::new(self.origin_x + self.width, abs_y),
        )
        .into_styled(PrimitiveStyle::with_stroke(color, 1))
        .draw(&mut self.buf);
    }
}

// ── Response ─────────────────────────────────────────────────────────────────

pub struct Response {
    pub clicked: bool,
}

impl Response {
    pub fn clicked(&self) -> bool { self.clicked }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn draw_text(buf: &mut PixelBuf<'_>, text: &str, x: i32, y: i32, color: Rgb888, font: &MonoFont<'_>) {
    let style = MonoTextStyle::new(font, color);
    // Text::new positions at the baseline; add font.baseline so callers treat y as top-of-cell.
    let _ = Text::new(text, embedded_graphics::geometry::Point::new(x, y + font.baseline as i32), style)
        .draw(buf);
}

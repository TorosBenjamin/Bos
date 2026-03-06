/// Minimal egui-compatible API for the Bos software-rendered path.
///
/// Provides the same surface (Context, CentralPanel, Ui, Response) that
/// hello_egui calls through `use bos_egui::egui`.  On Linux the real egui
/// crate is used instead; on Bos this stub renders directly into the
/// window's shared pixel buffer via embedded_graphics.
use core::cell::RefCell;
use alloc::string::{String, ToString};

use embedded_graphics::{
    mono_font::{MonoFont, MonoTextStyle, ascii::{FONT_8X13, FONT_8X13_BOLD}},
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{Rectangle, PrimitiveStyle, Line},
    text::Text,
};

use kernel_api_types::graphics::DisplayInfo;
use super::pixel_draw::PixelBuf;

// ── Colours ──────────────────────────────────────────────────────────────────

const BG:      Rgb888 = Rgb888::new(0x18, 0x18, 0x1e);
const FG:      Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);
const HEADING: Rgb888 = Rgb888::new(0x8a, 0xad, 0xf4);
const SEP:     Rgb888 = Rgb888::new(0x36, 0x3a, 0x4f);
const BTN_BG:  Rgb888 = Rgb888::new(0x36, 0x3a, 0x4f);
const BTN_HOV: Rgb888 = Rgb888::new(0x49, 0x4d, 0x64);
const BTN_FG:  Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);
const INPUT_BG: Rgb888 = Rgb888::new(0x1e, 0x1e, 0x2e);
const INPUT_FG: Rgb888 = Rgb888::new(0xca, 0xd3, 0xf5);
const INPUT_BR: Rgb888 = Rgb888::new(0x49, 0x4d, 0x64);

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
    /// Current vertical drawing position for the layout pass.
    draw_y: i32,
    /// Left margin.
    margin: i32,
}

pub struct Context {
    inner: RefCell<Inner>,
}

impl Context {
    pub fn new(
        pixels: &mut [u32],
        width: u32,
        height: u32,
        info: DisplayInfo,
        cursor_x: f32,
        cursor_y: f32,
        click: Option<(f32, f32)>,
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
                draw_y: 0,
                margin: 12,
            }),
        }
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
        let bx = inner.margin;
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

        inner.draw_y += btn_h + 8;
        Response { clicked }
    }

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

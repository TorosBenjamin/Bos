use core::fmt::{Display, Write};
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Dimensions, Point, Size};
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::prelude::Primitive;
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
use embedded_graphics::text::{Baseline, Text};
use unicode_segmentation::UnicodeSegmentation;
use crate::graphics::display::{DisplayDraw, DISPLAY};
use crate::graphics::frame_buffer_embedded_graphics::FrameBufferEmbeddedGraphics;

pub struct Writer<'a> {
    pub position: &'a mut Point,
    pub text_color: <FrameBufferEmbeddedGraphics<'a> as DrawTarget>::Color,
}

/// Safety: DISPLAY must be initialized before
impl Write for Writer<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let font = FONT_10X20;
        let background_color = Rgb888::BLACK;
        let mut display_draw = DisplayDraw;
        for c in s.graphemes(true) {
            let height_not_seen = self.position.y + font.character_size.height as i32
                - display_draw.bounding_box().size.height as i32;
            if height_not_seen > 0 {
                DISPLAY.shift_up(height_not_seen as usize);
                self.position.y -= height_not_seen;
            }
            match c {
                "\r" => {
                    // We do not handle special cursor movements
                }
                "\n" | "\r\n" => {
                    // Fill the remaining space with background color
                    Rectangle::new(
                        *self.position,
                        Size::new(
                            DISPLAY.bounding_box().size.width
                                - self.position.x as u32,
                            font.character_size.height,
                        ),
                    )
                        .into_styled(
                            PrimitiveStyleBuilder::new()
                                .fill_color(background_color)
                                .build(),
                        )
                        .draw(&mut display_draw)
                        .map_err(|_| core::fmt::Error)?;
                    self.position.y += font.character_size.height as i32;
                    self.position.x = 0;
                }
                c => {
                    let style = MonoTextStyleBuilder::new()
                        .font(&font)
                        .text_color(self.text_color)
                        .background_color(background_color)
                        .build();
                    *self.position =
                        Text::with_baseline(c, *self.position, style, Baseline::Top)
                            .draw(&mut display_draw)
                            .map_err(|_| core::fmt::Error)?;
                    if self.position.x as u32 + font.character_size.width
                        > DISPLAY.bounding_box().size.width
                    {
                        self.position.y += font.character_size.height as i32;
                        self.position.x = 0;
                    }
                }
            }
        }
        Ok(())
    }
}
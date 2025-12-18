use crate::syscalls::{rgb888_to_raw, sys_draw_iter, sys_fill_solid, sys_get_bounding_box};
use core::convert::Infallible;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::{OriginDimensions, Primitive};
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Rectangle};
use embedded_graphics::{Drawable, Pixel};
use kernel_api_types::graphics::{GraphicsResult, PixelData, Rect};

pub struct Display;

/// Just for funsies
pub fn draw_fun(display: &mut Display) {
    // Draw a red rectangle
    Rectangle::new(Point::new(0, 0), Size::new(50, 50))
        .into_styled(PrimitiveStyle::with_fill(Rgb888::new(255, 0, 0)))
        .draw(display)
        .unwrap();

    // Draw a green circle
    Circle::new(Point::new(25, 25), 20)
        .into_styled(PrimitiveStyle::with_fill(Rgb888::new(0, 255, 0)))
        .draw(display)
        .unwrap();
}

impl OriginDimensions for Display {
    fn size(&self) -> Size {
        let mut bb = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        let result = sys_get_bounding_box(&mut bb);

        match result {
            GraphicsResult::Ok => Size {
                width: bb.width,
                height: bb.height,
            },
            _ => panic!("Failed to get bounding box"),
        }
    }
}
impl DrawTarget for Display {
    type Color = Rgb888;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        // Convert embedded-graphics pixels into PixelData
        let pixels_data: heapless::Vec<PixelData, 1024> = pixels
            .into_iter()
            .map(|Pixel(point, color)| PixelData {
                x: point.x as u32,
                y: point.y as u32,
                rgb_raw: rgb888_to_raw(color),
            })
            .collect();

        // Call the kernel syscall
        sys_draw_iter(&pixels_data);

        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let rect = Rect {
            x: area.top_left.x as u32,
            y: area.top_left.y as u32,
            width: area.size.width,
            height: area.size.height,
        };
        sys_fill_solid(&rect, color);

        Ok(())
    }
}

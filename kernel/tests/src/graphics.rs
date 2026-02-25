use kernel::graphics::display::DISPLAY;
use crate::TestResult;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Rectangle, PrimitiveStyle};

pub fn basic_draw() -> TestResult {
    // Try to draw a simple rectangle
    let style = PrimitiveStyle::with_fill(Rgb888::RED);
    let rect = Rectangle::new(Point::new(0, 0), Size::new(10, 10))
        .into_styled(style);
    
    match rect.draw(&mut kernel::graphics::display::DisplayDraw) {
        Ok(_) => TestResult::Ok,
        Err(_) => TestResult::Failed(alloc::string::String::from("Failed to draw to display")),
    }
}

pub fn bounding_box_valid() -> TestResult {
    let bb = DISPLAY.bounding_box();
    if bb.size.width > 0 && bb.size.height > 0 {
        TestResult::Ok
    } else {
        TestResult::Failed(alloc::format!("Invalid bounding box: {:?}", bb))
    }
}

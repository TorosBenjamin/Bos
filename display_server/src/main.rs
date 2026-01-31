#![no_std]
#![no_main]

use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::Primitive;
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Rectangle};
use embedded_graphics::Drawable;
use ulib::display::Display;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    let mut display = Display::new();

    // Draw a red rectangle
    Rectangle::new(Point::new(0, 0), Size::new(50, 50))
        .into_styled(PrimitiveStyle::with_fill(Rgb888::new(255, 0, 0)))
        .draw(&mut display)
        .unwrap();

    // Draw a green circle
    Circle::new(Point::new(25, 25), 20)
        .into_styled(PrimitiveStyle::with_fill(Rgb888::new(0, 255, 0)))
        .draw(&mut display)
        .unwrap();

    // Flush dirty region to framebuffer
    display.present();

    loop {
        ulib::sys_yield();
    }
}

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

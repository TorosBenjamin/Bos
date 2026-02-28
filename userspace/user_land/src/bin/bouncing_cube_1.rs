#![no_std]
#![no_main]

use ulib::window::Window;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::prelude::OriginDimensions;
use embedded_graphics::primitives::{Primitive, Rectangle, PrimitiveStyle};
use embedded_graphics::Drawable;
use ulib::sys_yield;
use kernel_api_types::SVC_ERR_NOT_FOUND;

#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

const CUBE_SIZE: u32 = 20;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(_arg: u64) -> ! {
    // Wait for the display service to register itself
    let display_server_ep = loop {
        let ep = ulib::sys_lookup_service(b"display");
        if ep != SVC_ERR_NOT_FOUND {
            break ep;
        }
        sys_yield();
    };

    // Create a window (200x200 at position 10,10)
    let mut window = match Window::new(display_server_ep, 200, 200, 10, 10) {
        Some(w) => w,
        None => {
            // Failed to create window
            loop {
                sys_yield();
            }
        }
    };

    let width = window.size().width;
    let height = window.size().height;

    let mut x: i32 = 0;
    let mut y: i32 = 0;
    let mut dx: i32 = 2; // X velocity
    let mut dy: i32 = 2; // Y velocity

    loop {
        // Clear the old cube position by drawing a black rectangle
        let clear_rect = Rectangle::new(
            Point::new(x, y),
            Size::new(CUBE_SIZE, CUBE_SIZE),
        );
        let _ = clear_rect
            .into_styled(PrimitiveStyle::with_fill(Rgb888::new(0, 0, 0)))
            .draw(&mut window);

        // Update position
        x += dx;
        y += dy;

        // Collision detection and response
        if x < 0 {
            x = 0;
            dx = -dx;
        } else if (x + CUBE_SIZE as i32) > width as i32 {
            x = width as i32 - CUBE_SIZE as i32;
            dx = -dx;
        }

        if y < 0 {
            y = 0;
            dy = -dy;
        } else if (y + CUBE_SIZE as i32) > height as i32 {
            y = height as i32 - CUBE_SIZE as i32;
            dy = -dy;
        }

        // Draw the new cube (red)
        let cube_rect = Rectangle::new(
            Point::new(x, y),
            Size::new(CUBE_SIZE, CUBE_SIZE),
        );
        let _ = cube_rect
            .into_styled(PrimitiveStyle::with_fill(Rgb888::RED))
            .draw(&mut window);

        // Send update to display_server
        window.present();

        sys_yield();
    }
}

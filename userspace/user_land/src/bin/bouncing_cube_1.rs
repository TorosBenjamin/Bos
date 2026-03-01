#![no_std]
#![no_main]

use ulib::window::{Window, WindowEvent};
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

    // Create a toplevel window — DS assigns size via tiling
    let mut window = match Window::new(display_server_ep) {
        Some(w) => w,
        None => {
            loop {
                sys_yield();
            }
        }
    };

    let mut width = window.size().width;
    let mut height = window.size().height;

    let mut x: i32 = 0;
    let mut y: i32 = 0;
    let mut dx: i32 = 2;
    let mut dy: i32 = 2;

    loop {
        // Handle events from the display server
        while let Some(event) = window.poll_event() {
            if let WindowEvent::Configure { shared_buf_id, width: new_w, height: new_h } = event {
                window.apply_configure(shared_buf_id, new_w, new_h);
                width = new_w;
                height = new_h;
                // Clamp position to new bounds
                x = x.min(width as i32 - CUBE_SIZE as i32).max(0);
                y = y.min(height as i32 - CUBE_SIZE as i32).max(0);
            }
        }

        // Clear the old cube position
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

        // Collision detection
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

        window.present();

        sys_yield();
    }
}

#![no_std]
#![no_main]

use core::panic::PanicInfo;
use kernel::graphics::display;
use kernel::limine_requests::FRAME_BUFFER_REQUEST;
use kernel::logger;

#[unsafe(no_mangle)]
unsafe extern "C" fn kernel_main() -> ! {
    // Enable display
    let frame_buffer = FRAME_BUFFER_REQUEST.get_response().unwrap();
    display::init(&frame_buffer);

    // Enable logger
    logger::init().unwrap();
    log::info!("Welcome to Bos! V:0.3.0");

    // Call the generated test harness
    tests::run_tests();
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    tests::test_panic_handler(_info);
}
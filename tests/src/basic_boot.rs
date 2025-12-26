#![no_std]
#![no_main]

use core::panic::PanicInfo;
use kernel::graphics::display;
use kernel::limine_requests::{FRAME_BUFFER_REQUEST, MEMORY_MAP_REQUEST};
use kernel::{hlt_loop, logger};

#[unsafe(no_mangle)]
unsafe extern "C" fn kernel_main() -> ! {
    // Enable display
    let frame_buffer = FRAME_BUFFER_REQUEST.get_response().unwrap();
    display::init(&frame_buffer);

    // Enable logger
    logger::init().unwrap();
    log::info!("Welcome to Bos! V:0.3.0");

    let memory_map = MEMORY_MAP_REQUEST.get_response().unwrap();
    unsafe { kernel::memory::init_bsp(memory_map) };
    unsafe {
        kernel::memory::cpu_local_data::init_bsp();
    }
    log::info!("BSP memory initialized.");

    // Call the generated test harness
    tests::run_tests();

    hlt_loop()
}
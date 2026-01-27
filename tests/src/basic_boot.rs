#![no_std]
#![no_main]

use kernel::graphics::display;
use kernel::limine_requests::{FRAME_BUFFER_REQUEST, MEMORY_MAP_REQUEST, RSDP_REQUEST, MP_REQUEST};
use kernel::interrupt::nmi_handler_state;
use kernel::{acpi, apic, gdt, interrupt, logger, time};

#[unsafe(no_mangle)]
unsafe extern "C" fn kernel_main() -> ! {
    // Enable display
    let frame_buffer = FRAME_BUFFER_REQUEST.get_response().unwrap();
    display::init(&frame_buffer);

    // Enable logger
    logger::init().unwrap();
    log::info!("Welcome to Bos! V:0.3.0");

    let _ = MP_REQUEST.get_response(); // Ensure MP response is available
    let memory_map = MEMORY_MAP_REQUEST.get_response().unwrap();
    unsafe { kernel::memory::init_bsp(memory_map) };
    unsafe {
        kernel::memory::cpu_local_data::init_bsp();
    }
    log::info!("BSP memory initialized.");

    // Initialize core kernel features before tests
    nmi_handler_state::init();
    gdt::init();
    interrupt::idt::init();

    let rsdp = RSDP_REQUEST.get_response().unwrap();
    let acpi_tables = acpi::parse(rsdp);
    apic::init_bsp(&acpi_tables);
    apic::init_local_apic();

    time::tsc::calibrate();
    time::lapic_timer::init();
    time::lapic_timer::set_deadline(1_000_000);

    kernel::task::local_scheduler::init_run_queue();

    // Call the generated test harness
    tests::run_tests();
}
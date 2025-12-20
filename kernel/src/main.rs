#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]

extern crate alloc;
extern crate kernel;

use crate::kernel::limine_requests::{FRAME_BUFFER_REQUEST, MEMORY_MAP_REQUEST, MP_REQUEST};
use core::sync::atomic::{AtomicBool, Ordering};
use log::log;
use x86_64::instructions::interrupts;
use x86_64::registers::model_specific::Msr;
use kernel::graphics::display;
use kernel::limine_requests::{BASE_REVISION, RSDP_REQUEST};
use kernel::memory::cpu_local_data::get_local;
use kernel::memory::guarded_stack::{GuardedStack, NORMAL_STACK_SIZE, StackId, StackType};
use kernel::user_land::run_user_land;
use kernel::{acpi, apic, gdt, hlt_loop, interrupt, nmi_handler_state};
use kernel::apic::{init_timer, LocalApicAccess, LOCAL_APIC_ACCESS};
use kernel::task::global_scheduler::spawn_task;
use kernel::task::local_scheduler::init_run_queue;
use kernel::task::process::KernelThread;

mod logger;

#[unsafe(no_mangle)]
unsafe extern "C" fn kernel_main() -> ! {
    assert!(BASE_REVISION.is_supported());

    // Enable display
    let frame_buffer = FRAME_BUFFER_REQUEST.get_response().unwrap();
    display::init(&frame_buffer);

    // Enable logger
    logger::init().unwrap();
    log::info!("Welcome to BogOS! V:0.3.0");

    let memory_map = MEMORY_MAP_REQUEST.get_response().unwrap();
    unsafe { kernel::memory::init_bsp(memory_map) };
    unsafe {
        kernel::memory::cpu_local_data::init_bsp();
    }
    log::info!("BSP memory initialized.");

    GuardedStack::new_kernel(
        NORMAL_STACK_SIZE,
        StackId {
            _type: StackType::Normal,
            cpu_id: get_local().kernel_id,
        },
    )
    .switch(init_bsp);

    // For now pause
    hlt_loop();
}

/// BSP - Bootstrap Processor
extern "sysv64" fn init_bsp() -> ! {
    nmi_handler_state::init();
    log::info!("BSP NMI handler initialized.");

    gdt::init();
    interrupt::init();

    let rsdp = RSDP_REQUEST.get_response().unwrap();
    let acpi_tables = acpi::parse(rsdp);
    apic::init_bsp(&acpi_tables);
    apic::init_local_apic();

    init_timer();

    #[cfg(test)]
    test_main();

    init_run_queue();

    let mp_response = MP_REQUEST.get_response().unwrap();
    for cpu in mp_response.cpus() {
        cpu.goto_address.write(ap_entry);
    }

    run_user_land();

    hlt_loop();
}

/// AP - Application processor
unsafe extern "C" fn ap_entry(_cpu: &limine::mp::Cpu) -> ! {
    log::info!("New CPU initialized.");

    unsafe { kernel::memory::init_ap() };
    unsafe { kernel::memory::cpu_local_data::init_ap(_cpu) };

    GuardedStack::new_kernel(
        NORMAL_STACK_SIZE,
        StackId {
            _type: StackType::Normal,
            cpu_id: get_local().kernel_id,
        },
    )
    .switch(init_ap);

    // Shouldn't run
    hlt_loop();
}

extern "sysv64" fn init_ap() -> ! {
    gdt::init();
    interrupt::init();
    apic::init_local_apic();
    init_run_queue();
    init_timer();

    hlt_loop()
}

fn example_log() -> ! {
    log::info!("Hello I'm under the water!");
    hlt_loop()
}

static DID_PANIC: AtomicBool = AtomicBool::new(false);
#[panic_handler]
fn rust_panic(_info: &core::panic::PanicInfo) -> ! {
    if !DID_PANIC.swap(true, Ordering::Relaxed) {
        log::error!("{_info}");
        hlt_loop();
    } else {
        hlt_loop();
    }
}

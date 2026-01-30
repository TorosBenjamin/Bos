#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]

extern crate alloc;
extern crate kernel;

use crate::kernel::limine_requests::{FRAME_BUFFER_REQUEST, MEMORY_MAP_REQUEST};
use core::sync::atomic::{AtomicBool, Ordering};
use kernel::graphics::display;
use kernel::limine_requests::{BASE_REVISION, RSDP_REQUEST};
use kernel::memory::cpu_local_data::get_local;
use kernel::memory::guarded_stack::{GuardedStack, StackId, StackType, NORMAL_STACK_SIZE};
use kernel::{acpi, apic, gdt, hlt_loop, interrupt, logger, project_version, raw_syscall_handler, time, user_land};
use kernel::interrupt::nmi_handler_state;
use kernel::task::global_scheduler::spawn_task;
use kernel::task::local_scheduler::init_run_queue;
use kernel::task::task::Task;

#[unsafe(no_mangle)]
unsafe extern "C" fn kernel_main() -> ! {
    assert!(BASE_REVISION.is_supported());

    // Enable display
    let frame_buffer = FRAME_BUFFER_REQUEST.get_response().unwrap();
    display::init(&frame_buffer);

    // Enable logger
    logger::init().unwrap();
    log::info!("Welcome to Bos! V:{}", project_version());

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
    interrupt::idt::init();

    let rsdp = RSDP_REQUEST.get_response().unwrap();
    let acpi_tables = acpi::parse(rsdp);
    apic::init_bsp(&acpi_tables);
    apic::init_local_apic();

    time::tsc::calibrate();
    time::lapic_timer::init();
    time::lapic_timer::set_deadline(1_000_000);

    raw_syscall_handler::init();
    init_run_queue();

    spawn_task(Task::new(idle_task));

    // Spawn user task from Limine module
    let user_task = user_land::create_user_task_from_elf();
    spawn_task(user_task);

    /*
    let mp_response = MP_REQUEST.get_response().unwrap();
    for cpu in mp_response.cpus() {
        cpu.goto_address.write(ap_entry);
    }
    */

    x86_64::instructions::interrupts::enable();

    hlt_loop();
}

#[allow(dead_code)]
/// AP - Application processor
unsafe extern "C" fn ap_entry(_cpu: &limine::mp::Cpu) -> ! {
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

    hlt_loop();
}

#[allow(dead_code)]
extern "sysv64" fn init_ap() -> ! {
    gdt::init();
    interrupt::idt::init();
    apic::init_local_apic();

    raw_syscall_handler::init();
    init_run_queue();

    spawn_task(Task::new(idle_task));

    x86_64::instructions::interrupts::enable();
    time::lapic_timer::set_deadline(1_000_000);

    log::info!("Initialized AP");

    hlt_loop()
}

fn idle_task() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
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

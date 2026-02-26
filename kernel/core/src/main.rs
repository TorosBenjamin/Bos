#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]

extern crate alloc;
extern crate kernel;

use crate::kernel::limine_requests::{FRAME_BUFFER_REQUEST, MEMORY_MAP_REQUEST};
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use embedded_graphics::geometry::Point;
use embedded_graphics::pixelcolor::{Rgb888, RgbColor};
use kernel::graphics::display::{self, DISPLAY};
use kernel::graphics::writer::Writer;
use kernel::limine_requests::{BASE_REVISION, MP_REQUEST, RSDP_REQUEST};
use kernel::memory::cpu_local_data::get_local;
use kernel::memory::guarded_stack::{GuardedStack, StackId, StackType, NORMAL_STACK_SIZE};
use kernel::{acpi, apic, gdt, hlt_loop, interrupt, ioapic, logger, project_version, raw_syscall_handler, time, user_task_from_elf};
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

    ioapic::init(&acpi_tables);
    ioapic::enable_keyboard_irq(
        u8::from(interrupt::InterruptVector::Keyboard),
        get_local().local_apic_id,
    );

    time::tsc::calibrate();
    time::lapic_timer::init();
    time::lapic_timer::set_deadline(1_000_000);

    raw_syscall_handler::init();
    init_run_queue();

    spawn_task(Task::new(idle_task));

    // Spawn user task from Limine module
    let user_task = user_task_from_elf::create_user_task_from_elf();
    display::DISPLAY_OWNER.store(user_task.id.to_u64(), Ordering::Relaxed);
    spawn_task(user_task);


    let mp_response = MP_REQUEST.get_response().unwrap();
    for cpu in mp_response.cpus() {
        if cpu.lapic_id != mp_response.bsp_lapic_id() {
            cpu.goto_address.write(ap_entry);
        }
    }

    log::info!("BSP: enabling interrupts");
    x86_64::instructions::interrupts::enable();
    log::info!("BSP: in hlt_loop");

    hlt_loop();
}

/// AP - Application processor
unsafe extern "C" fn ap_entry(_cpu: &limine::mp::Cpu) -> ! {
    log::info!("AP entry (lapic_id={})", _cpu.lapic_id);
    unsafe { kernel::memory::init_ap() };
    log::info!("AP (lapic_id={}): CR3 switched", _cpu.lapic_id);
    unsafe { kernel::memory::cpu_local_data::init_ap(_cpu) };
    log::info!("AP (lapic_id={}): cpu_local_data initialized", _cpu.lapic_id);

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

extern "sysv64" fn init_ap() -> ! {
    let cpu_id = get_local().kernel_id;
    log::info!("AP {}: on new stack, calling gdt::init", cpu_id);
    gdt::init();
    log::info!("AP {}: gdt done, calling idt::init", cpu_id);
    interrupt::idt::init();
    log::info!("AP {}: idt done, calling apic::init_local_apic", cpu_id);
    apic::init_local_apic();
    log::info!("AP {}: apic done", cpu_id);

    raw_syscall_handler::init();
    init_run_queue();

    spawn_task(Task::new(idle_task));

    x86_64::instructions::interrupts::enable();
    time::lapic_timer::init();
    time::lapic_timer::set_deadline(1_000_000);

    log::info!("Initialized AP {}", cpu_id);

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

        // Take over the framebuffer for a visual crash dump
        let bb = DISPLAY.bounding_box();
        let _ = DISPLAY.fill_solid(&bb, Rgb888::new(0, 0, 128)); // dark blue
        let mut position = Point::new(10, 10);
        let mut writer = Writer {
            position: &mut position,
            text_color: Rgb888::WHITE,
        };
        let _ = write!(writer, "KERNEL PANIC\n\n{_info}");

        hlt_loop();
    } else {
        hlt_loop();
    }
}

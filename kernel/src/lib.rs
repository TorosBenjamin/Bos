#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::panic::PanicInfo;

pub const HIGHER_HALF_START: u64 = 0xFFFF800000000000;
pub const LOWER_HALF_END: u64 = 0x800000000000;

pub const USER_MIN: u64 = 0x1000;
pub const USER_MAX: u64 = LOWER_HALF_END - 1;

pub mod acpi;
pub mod apic;
pub mod gdt;

pub mod graphics;
pub mod interrupt;
pub mod limine_requests;
pub mod memory;
pub mod nmi_handler_state;
pub mod raw_syscall_handler;
pub mod syscall_handlers;
pub mod task;
pub mod user_land;

pub mod logger;

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

// -- Testing --
pub fn test_runner(tests: &[&dyn Fn()]) {
    log::info!("Running {} tests", tests.len());
    for test in tests {
        test();
    }
    exit_qemu(QemuExitCode::Success);

    hlt_loop();
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    log::error!("[failed]");
    log::error!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);

    hlt_loop();
}

// Custom test harness
pub trait KernelTest {
    fn name(&self) -> &'static str;
    fn run(&self);
}

impl<F> KernelTest for F
where
    F: Fn(),
{
    fn name(&self) -> &'static str {
        core::any::type_name::<F>()
    }

    fn run(&self) {
        log::info!("{}:\t", core::any::type_name::<F>());

        self();

        log::info!("\x1b[32m[ok]\x1b[0m");
    }
}

#[cfg(feature = "kernel_test")]
pub fn tests() -> &'static [&'static dyn KernelTest] {
    &[
        &trivial_assertion,
        // add more here
    ]
}

#[cfg(feature = "kernel_test")]
pub fn run_tests() -> ! {
    let tests = tests();

    log::info!("Running {} kernel tests", tests.len());

    for test in tests {
        test.run();
    }

    exit_qemu(QemuExitCode::Success);
    hlt_loop();
}

#[cfg(feature = "kernel_test")]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed  = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

#[cfg(feature = "kernel_test")]
fn trivial_assertion() {
    assert_eq!(1, 1);
}

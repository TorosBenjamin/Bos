#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

// Enable custom test framework
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

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

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

// -- Testing --

pub trait Testable {
    fn run(&self) -> ();
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        log::info!("{}...\t", core::any::type_name::<T>());
        self();
        log::info!("[ok]");
    }
}

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

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

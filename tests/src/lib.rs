#![no_std]
#![no_main]

use core::panic::PanicInfo;
use kernel::hlt_loop;

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


pub fn tests() -> &'static [&'static dyn KernelTest] {
    &[
        &trivial_assertion,
        // add more here
    ]
}

pub fn run_tests() -> ! {
    let tests = tests();

    log::info!("Running {} kernel tests", tests.len());

    for test in tests {
        test.run();
    }

    exit_qemu(QemuExitCode::Success);
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

fn trivial_assertion() {
    assert_eq!(1, 1);
}

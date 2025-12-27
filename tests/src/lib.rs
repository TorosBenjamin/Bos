#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;
use kernel::hlt_loop;

pub mod panic_handler;
pub mod physical_memory;

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
    fn run(&self) -> TestResult;
}

impl<F> KernelTest for F
where
    F: Fn() -> TestResult,
{
    fn name(&self) -> &'static str {
        core::any::type_name::<F>()
    }

    fn run(&self) -> TestResult {
        self()
    }
}


#[derive(Debug)]
pub enum TestResult {
    Ok,
    Failed(String),
}


pub fn tests() -> &'static [&'static dyn KernelTest] {
    &[
        &trivial_assertion,

        // Tests with expected panic can't be really be caught right now
        // so they should be commented out so others can run
        &physical_memory::alloc_one_frame,
        &physical_memory::free_and_reuse_kernel_frame,
        &physical_memory::frame_alignment,
        &physical_memory::kernel_type,
        &physical_memory::user_type,
        &physical_memory::exhaustion,
        &physical_memory::duplicate_allocation,
    ]
}

pub fn run_tests() -> ! {
    let tests = tests();

    log::info!("Running {} kernel tests", tests.len());
    let mut failed = 0;

    for test in tests {
        let result = test.run();
        match result {
            TestResult::Ok => log::info!("{} [ok]", test.name()),
            TestResult::Failed(msg) => {
                log::error!("{} [failed] - {}", test.name(), msg);
                failed += 1;
            }
        }
    }

    if failed == 0 {
        exit_qemu(QemuExitCode::Success);
    } else {
        log::info!("{} kernel tests failed", failed);
        exit_qemu(QemuExitCode::Failed);
    }

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

fn trivial_assertion() -> TestResult {
    let a = 1;
    let b = 1;
    if a == b {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("{} != {}", a, b))
    }
}
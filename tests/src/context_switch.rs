use crate::TestResult;
use kernel::task::task::Task;
use kernel::task::global_scheduler::spawn_task;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use alloc::format;

static TASK_SWITCH_SUCCESS: AtomicU64 = AtomicU64::new(0);

fn test_task_entry() -> ! {
    TASK_SWITCH_SUCCESS.store(1, Ordering::SeqCst);
    loop {
        core::hint::spin_loop();
    }
}

pub fn test_context_switch_tasks() -> TestResult {
    TASK_SWITCH_SUCCESS.store(0, Ordering::SeqCst);

    // Spawn a task and let the timer-driven scheduler pick it up
    spawn_task(Task::new(test_task_entry));

    x86_64::instructions::interrupts::enable();

    let start = kernel::time::tsc::value();
    while TASK_SWITCH_SUCCESS.load(Ordering::SeqCst) == 0 {
        if kernel::time::tsc::value() - start > 100_000_000 { // timeout
            return TestResult::Failed("Timeout waiting for task switch".into());
        }
        x86_64::instructions::hlt();
    }

    TestResult::Ok
}

pub fn test_context_save_restore_macros() -> TestResult {
    // This test verifies that save_context! and restore_context! work correctly
    // by pushing and then popping registers.

    let mut r13_val: u64;
    let mut r12_val: u64;

    unsafe {
        asm!(
            "mov r13, 0x1122334455667788",
            "mov r12, 0x8877665544332211",
            kernel::save_registers_regular!(),
            "mov r13, 0",
            "mov r12, 0",
            kernel::restore_registers_regular!(),
            out("r13") r13_val,
            out("r12") r12_val,
        );
    }

    if r13_val == 0x1122334455667788 && r12_val == 0x8877665544332211 {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("Registers not restored correctly: r13={:#x}, r12={:#x}", r13_val, r12_val))
    }
}

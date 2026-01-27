use crate::TestResult;
use kernel::task::context;
use kernel::task::task::{Task, TaskState};
use kernel::task::global_scheduler::{spawn_task, TASK_TABLE};
use kernel::task::local_scheduler::{get_run_queue, schedule};
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use alloc::format;
use alloc::sync::Arc;

static TASK_SWITCH_SUCCESS: AtomicU64 = AtomicU64::new(0);

fn test_task_entry() -> ! {
    TASK_SWITCH_SUCCESS.store(1, Ordering::SeqCst);
    loop {
        core::hint::spin_loop();
    }
}

pub fn test_context_switch_tasks() -> TestResult {
    TASK_SWITCH_SUCCESS.store(0, Ordering::SeqCst);
    
    // Create a new task
    let task = Task::new(test_task_entry);
    
    // Switch to it manually for testing
    // Note: This is a bit dangerous because we are hijacking the current execution
    // but for a kernel test it should be fine if we don't plan to return or if we handle it.
    // However, Task::new is designed for the scheduler.
    
    // Let's use the scheduler properly.
    spawn_task(task);
    
    // Yield to the new task
    schedule();
    
    // If we are back here, it means the task might have yielded back or we are still in the main task
    // But our test task doesn't yield back.
    // Wait, the scheduler will pick the next task.
    
    // In this test environment, we expect to be back here ONLY if the scheduler picks us again.
    // But we didn't register the "current" context as a task.
    
    // Let's keep it simple: if the flag is 1, it means we at least reached the task.
    let start = kernel::time::tsc::value();
    while TASK_SWITCH_SUCCESS.load(Ordering::SeqCst) == 0 {
        if kernel::time::tsc::value() - start > 100_000_000 { // timeout
            return TestResult::Failed("Timeout waiting for task switch".into());
        }
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
            kernel::save_context!(),
            "mov r13, 0",
            "mov r12, 0",
            kernel::restore_context!(),
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

use crate::TestResult;
use kernel::task::task::Task;
use kernel_api_types::Priority;
use kernel::task::global_scheduler::spawn_task;
use core::sync::atomic::{AtomicU64, Ordering};

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
    spawn_task(Task::new(test_task_entry, 0, Priority::Normal, None));

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


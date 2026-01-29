use crate::TestResult;
use kernel::task::task::{Task, TaskState};
use kernel::task::global_scheduler::spawn_task;
use kernel::time::tsc;
use core::sync::atomic::{AtomicU64, Ordering};
use alloc::format;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn task_increment() -> ! {
    TEST_COUNTER.fetch_add(1, Ordering::SeqCst);

    loop {
        core::hint::spin_loop();
    }
}

pub fn task_spawn_and_run() -> TestResult {
    // Now spawn our incrementing tasks
    spawn_task(Task::new(task_increment));
    spawn_task(Task::new(task_increment));

    // Enable interrupts and wait for the tasks to run via timer
    x86_64::instructions::interrupts::enable();

    log::info!("Waiting for automatic task switches...");

    // Wait for at least 10ms
    let start_tsc = tsc::value();
    let timeout = tsc::TSC_HZ.load(Ordering::SeqCst) * 100; // 100ms
    while TEST_COUNTER.load(Ordering::SeqCst) < 2 {
        if tsc::value() - start_tsc > timeout {
            break;
        }
        x86_64::instructions::hlt();
    }

    let count = TEST_COUNTER.load(Ordering::SeqCst);
    if count >= 2 {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("Counter did not increment enough: {} < 2. Automatic scheduling might not be working.", count))
    }
}

pub fn simple_task_creation() -> TestResult {
    let task = Task::new(|| loop {});
    if task.run_state() == TaskState::Initializing {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("Task state should be Initializing, but is {:?}", task.run_state()))
    }
}

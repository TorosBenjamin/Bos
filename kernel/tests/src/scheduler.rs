use crate::{TestResult, exit_qemu, QemuExitCode};
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

/// A task that waits for TEST_COUNTER to reach 2 and exits QEMU.
///
/// This must run as a scheduled task because the test harness is
/// abandoned once the scheduler starts switching to tasks.
fn checker_task() -> ! {
    let start_tsc = tsc::value();
    let timeout = tsc::TSC_HZ.load(Ordering::SeqCst) / 10; // 100ms

    while TEST_COUNTER.load(Ordering::SeqCst) < 2 {
        if tsc::value() - start_tsc > timeout {
            break;
        }
        core::hint::spin_loop();
    }

    if TEST_COUNTER.load(Ordering::SeqCst) >= 2 {
        log::info!("tests::scheduler::task_spawn_and_run [ok]");
        log::info!("All tests passed!");
        exit_qemu(QemuExitCode::Success);
    } else {
        let count = TEST_COUNTER.load(Ordering::SeqCst);
        log::error!(
            "tests::scheduler::task_spawn_and_run [failed] - Counter: {} < 2",
            count
        );
        exit_qemu(QemuExitCode::Failed);
    }

    loop {
        core::hint::spin_loop();
    }
}

/// This test spawns tasks and lets the scheduler run them.
///
/// **Important**: This test hands control to the scheduler and never returns.
/// It must be the LAST test in the test list. The checker task exits QEMU
/// with the appropriate exit code.
pub fn task_spawn_and_run() -> TestResult {
    TEST_COUNTER.store(0, Ordering::SeqCst);

    spawn_task(Task::new(task_increment));
    spawn_task(Task::new(task_increment));
    spawn_task(Task::new(checker_task));

    // Enable interrupts — the scheduler will take over on the next timer tick
    // and this code will be abandoned (it's not a scheduled task).
    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }

    // Unreachable: the checker_task exits QEMU with the result.
}

pub fn simple_task_creation() -> TestResult {
    let task = Task::new(|| loop {});
    if task.run_state() == TaskState::Initializing {
        TestResult::Ok
    } else {
        TestResult::Failed(format!("Task state should be Initializing, but is {:?}", task.run_state()))
    }
}

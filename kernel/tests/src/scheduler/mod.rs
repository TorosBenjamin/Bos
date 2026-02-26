pub mod context_switch;
pub mod spawn;
pub mod stack;

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

/// Checker task for `test_kernel_tasks_run`.
///
/// Logs immediately on entry so the serial output shows it started even if
/// something goes wrong afterward. Waits up to `TSC_HZ * 1000` ticks for
/// both increment tasks to have run, then exits QEMU with the result.
fn kernel_tasks_checker() -> ! {
    let count_at_start = TEST_COUNTER.load(Ordering::SeqCst);
    log::info!(
        "kernel_tasks_checker: started, counter={}",
        count_at_start
    );

    // TSC_HZ may be miscalibrated (~27 000 ticks/ms on this machine instead of
    // ~3 000 000), so multiply by 1000 to get a timeout that is generous even
    // with a badly calibrated TSC.
    let start_tsc = tsc::value();
    let timeout = tsc::TSC_HZ.load(Ordering::SeqCst).saturating_mul(1000);

    while TEST_COUNTER.load(Ordering::SeqCst) < 2 {
        if tsc::value().wrapping_sub(start_tsc) > timeout {
            break;
        }
        core::hint::spin_loop();
    }

    let count = TEST_COUNTER.load(Ordering::SeqCst);
    if count >= 2 {
        log::info!("tests::scheduler::test_kernel_tasks_run [ok]");
        exit_qemu(QemuExitCode::Success);
    } else {
        log::error!(
            "tests::scheduler::test_kernel_tasks_run [failed] - \
             kernel tasks did not run (counter={}/2)",
            count
        );
        exit_qemu(QemuExitCode::Failed);
    }

    loop {
        core::hint::spin_loop();
    }
}

/// Verify that the scheduler can bootstrap and round-robin kernel tasks.
///
/// Spawns two increment tasks and a checker.  The checker exits QEMU
/// with the test result, so this function **never returns**.
/// Use `cargo ktest-sched` to run only this test.
pub fn test_kernel_tasks_run() -> TestResult {
    TEST_COUNTER.store(0, Ordering::SeqCst);

    spawn_task(Task::new(task_increment));
    spawn_task(Task::new(task_increment));
    spawn_task(Task::new(kernel_tasks_checker));

    // Explicitly arm the LAPIC timer so the scheduler fires even if the
    // timer was not re-armed by a previous test (e.g. test_timer_stack_alignment
    // returns early when TIMER_STACK_ALIGNMENT_OK is already set).
    kernel::time::lapic_timer::set_deadline(1_000_000);

    // Enable interrupts — the scheduler takes over on the next timer tick.
    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
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

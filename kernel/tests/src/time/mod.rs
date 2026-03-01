use kernel::time::tsc;
use kernel::time::pit;
use crate::TestResult;
use core::sync::atomic::Ordering;

// ─── TSC / PIT ────────────────────────────────────────────────────────────────

pub fn tsc_calibration() -> TestResult {
    let tsc_hz = tsc::TSC_HZ.load(Ordering::SeqCst);
    if tsc_hz > 0 {
        TestResult::Ok
    } else {
        TestResult::Failed(alloc::format!("TSC not calibrated: {}", tsc_hz))
    }
}

pub fn pit_sleep() -> TestResult {
    let start = tsc::value();
    let _ = pit::sleep_qs(1000); // 1ms
    let end = tsc::value();

    if end > start {
        TestResult::Ok
    } else {
        TestResult::Failed(alloc::format!("TSC did not advance during PIT sleep: start={}, end={}", start, end))
    }
}

// ─── sys_sleep_ms argument validation ────────────────────────────────────────

/// sys_sleep_ms(0) returns 0 immediately — no task context required, no blocking.
pub fn sleep_ms_zero_noop() -> TestResult {
    let ret = kernel::syscall_handlers::sys_sleep_ms(0, 0, 0, 0, 0, 0);
    if ret != 0 {
        return TestResult::Failed(alloc::format!("expected 0, got {ret}"));
    }
    TestResult::Ok
}

/// sys_sleep_ms(1) with no current task in the run-queue returns 1 (error).
/// Interrupts must not be enabled here — the function returns immediately.
pub fn sleep_ms_no_task_returns_error() -> TestResult {
    // The test harness runs on the BSP outside of any scheduled task, so
    // run_queue.current_task is None and sys_sleep_ms must return 1.
    let ret = kernel::syscall_handlers::sys_sleep_ms(1, 0, 0, 0, 0, 0);
    if ret != 1 {
        return TestResult::Failed(alloc::format!("expected 1 (no task), got {ret}"));
    }
    TestResult::Ok
}

// ─── sleep_queue unit tests ───────────────────────────────────────────────────

/// A task enqueued with an already-expired deadline is set to Ready by tick().
pub fn sleep_queue_tick_wakes_expired_task() -> TestResult {
    use alloc::sync::Arc;
    use kernel::task::task::{Task, TaskState};
    use kernel::time::sleep_queue;
    use kernel::memory::cpu_local_data::get_local;

    let task = Arc::new(Task::new(|| loop {}));
    let cpu_id = get_local().kernel_id;

    // wake_tsc = 0: always in the past.
    sleep_queue::enqueue(task.clone(), cpu_id, 0);

    // Tick with u64::MAX — every enqueued entry is expired.
    sleep_queue::tick(u64::MAX);

    if task.run_state() == TaskState::Ready {
        TestResult::Ok
    } else {
        TestResult::Failed(alloc::format!(
            "expected Ready after tick, got {:?}",
            task.run_state()
        ))
    }
}

/// Only entries whose deadline has passed are woken; a far-future entry stays put.
pub fn sleep_queue_only_expired_entries_woken() -> TestResult {
    use alloc::sync::Arc;
    use kernel::task::task::{Task, TaskState};
    use kernel::time::sleep_queue;
    use kernel::memory::cpu_local_data::get_local;

    let cpu_id = get_local().kernel_id;
    let now = tsc::value();

    let task_expired = Arc::new(Task::new(|| loop {}));
    let task_future  = Arc::new(Task::new(|| loop {}));

    // Expired: deadline is in the past.
    sleep_queue::enqueue(task_expired.clone(), cpu_id, now.saturating_sub(1));
    // Not expired: deadline is far in the future (u64::MAX).
    sleep_queue::enqueue(task_future.clone(), cpu_id, u64::MAX);

    // Tick at 'now' — only task_expired should be woken.
    sleep_queue::tick(now);

    let expired_state = task_expired.run_state();
    let future_state  = task_future.run_state();

    if expired_state != TaskState::Ready {
        return TestResult::Failed(alloc::format!(
            "expired task: expected Ready, got {:?}", expired_state
        ));
    }
    if future_state == TaskState::Ready {
        return TestResult::Failed(alloc::format!(
            "future task: should not be Ready yet, got {:?}", future_state
        ));
    }
    TestResult::Ok
}

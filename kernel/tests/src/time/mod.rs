use kernel::time::tsc;
use kernel::time::pit;
use crate::TestResult;
use core::sync::atomic::Ordering;

// ─── TSC / PIT ────────────────────────────────────────────────────────────────

pub fn tsc_calibration() -> TestResult {
    let tsc_hz = tsc::TSC_TICKS_PER_MS.load(Ordering::SeqCst);
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


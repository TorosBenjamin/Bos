use kernel::time::tsc;
use kernel::time::pit;
use crate::TestResult;
use core::sync::atomic::Ordering;

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

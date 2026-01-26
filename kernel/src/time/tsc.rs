use core::arch::x86_64::{__cpuid, __rdtscp, _mm_lfence, _rdtsc};
use core::sync::atomic::{AtomicU64, Ordering};
use crate::time::{pit, rtc, Period};

pub static TSC_HZ: AtomicU64 = AtomicU64::new(0);

pub fn value() -> u64 {
    if has_rdtscp() {
        let mut aux = 0;
        unsafe { __rdtscp(&mut aux) }
    } else {
        unsafe {
            _mm_lfence();
            _rdtsc()
        }
    }
}

fn has_rdtscp() -> bool {
    // First check if extended CPUID leaves are supported
    let max_ext = unsafe { __cpuid(0x8000_0000) }.eax;
    if max_ext < 0x8000_0001 {
        return false;
    }

    let res = unsafe { __cpuid(0x8000_0001) };
    (res.edx & (1 << 27)) != 0
}

fn calibrate_with_pit() -> u64 {
    const PIT_WAIT_QS: u32 = 10_000;

    let start = value();
    let _ = pit::sleep_qs(PIT_WAIT_QS);
    let end = value();
    log::info!("{}, {}", start, end);

    let elapsed = end.checked_sub(start).unwrap();
    elapsed / PIT_WAIT_QS as u64
}

/// Safety: must be called once during early boot
pub fn calibrate() {
    //TODO: Check if cpu has invariant tsc

    // let tms = calibrate_with_pit();
    let tms = 1000;

    log::info!("Tsc {} ticks per qs", tms);
    TSC_HZ.store(tms, Ordering::SeqCst);
}
use core::arch::x86_64::{__cpuid, __rdtscp, _mm_lfence, _rdtsc};
use core::sync::atomic::{AtomicU64, Ordering};
use crate::time::pit;

/// TSC ticks per millisecond, calibrated once at boot against the PIT.
pub static TSC_TICKS_PER_MS: AtomicU64 = AtomicU64::new(0);

/// TSC value captured at the same moment the RTC was read.
static BOOT_TSC: AtomicU64 = AtomicU64::new(0);

/// Unix wall-clock time (nanoseconds since epoch) at the BOOT_TSC moment.
static BOOT_UNIX_NS: AtomicU64 = AtomicU64::new(0);

/// Read the current TSC value, serialised with lfence or rdtscp.
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
    let max_ext = unsafe { __cpuid(0x8000_0000) }.eax;
    if max_ext < 0x8000_0001 {
        return false;
    }
    let res = unsafe { __cpuid(0x8000_0001) };
    (res.edx & (1 << 27)) != 0
}

fn calibrate_with_pit() -> u64 {
    // Wait 10 ms using PIT Channel 2 and count TSC ticks over that interval.
    const PIT_WAIT_US: u32 = 10_000;

    let start = value();
    let _ = pit::sleep_qs(PIT_WAIT_US);
    let end = value();

    let elapsed = end.checked_sub(start).unwrap_or(1);
    // ticks_per_ms = elapsed_ticks * 1000 / wait_us
    //              = elapsed_ticks / 10  (since wait_us = 10_000)
    elapsed * 1_000 / PIT_WAIT_US as u64
}

/// Calibrate the TSC frequency. Call once during early boot before the LAPIC timer.
pub fn calibrate() {
    let ticks_per_ms = calibrate_with_pit();
    log::info!("TSC: {} ticks/ms ({} MHz)", ticks_per_ms, ticks_per_ms / 1_000);
    TSC_TICKS_PER_MS.store(ticks_per_ms, Ordering::SeqCst);
}

/// Record the wall-clock anchor. Call once after calibrate() and after reading the RTC.
///
/// `boot_unix_ns` is Unix time in nanoseconds at the moment of the RTC read.
/// The current TSC is captured here as the matching TSC anchor.
pub fn set_wall_clock(boot_unix_ns: u64) {
    BOOT_TSC.store(value(), Ordering::SeqCst);
    BOOT_UNIX_NS.store(boot_unix_ns, Ordering::SeqCst);
}

/// Returns nanoseconds since the Unix epoch (wall-clock time).
///
/// Uses the TSC for high-resolution elapsed time on top of the RTC-anchored base.
/// Returns 0 if calibrate() or set_wall_clock() have not been called yet.
pub fn now_ns() -> u64 {
    let ticks_per_ms = TSC_TICKS_PER_MS.load(Ordering::Relaxed);
    if ticks_per_ms == 0 {
        return 0;
    }
    let elapsed_ticks = value().saturating_sub(BOOT_TSC.load(Ordering::Relaxed));
    // Split to avoid u64 overflow: ticks * 1_000_000 / ticks_per_ms
    // = (whole_ms * 1_000_000) + (remainder_ticks * 1_000_000 / ticks_per_ms)
    let whole_ms = elapsed_ticks / ticks_per_ms;
    let remainder = elapsed_ticks % ticks_per_ms;
    let elapsed_ns = whole_ms * 1_000_000 + remainder * 1_000_000 / ticks_per_ms;
    BOOT_UNIX_NS.load(Ordering::Relaxed).saturating_add(elapsed_ns)
}

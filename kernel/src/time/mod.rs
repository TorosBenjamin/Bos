use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use crate::memory::cpu_local_data::get_local;
use crate::time::tsc::TSC_HZ;

mod pit;
pub mod lapic_timer;
pub mod tsc;
mod rtc;

pub fn on_timer_tick() {
    lapic_timer::set_deadline(100_000_000_000); // 1 ms
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Period(u64);

impl Period {
    const MAX: Self = Self(u64::MAX);

    /// Creates a new period with the specified microseconds.
    pub fn new(period: u64) -> Self {
        Self(period)
    }
}

impl From<Period> for u64 {
    /// Returns the period in microseconds.
    fn from(f: Period) -> Self {
        f.0
    }
}
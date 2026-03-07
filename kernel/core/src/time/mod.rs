pub mod pit;
pub mod lapic_timer;
pub mod tsc;
mod rtc;

pub fn on_timer_tick() {
    lapic_timer::set_deadline(1_000_000); // 1 ms
}

/// Read the RTC for wall-clock time and anchor it to the current TSC.
/// Call once during boot after tsc::calibrate() and before spawning user tasks.
pub fn init_wall_clock() {
    let unix_secs = rtc::read_unix_timestamp();
    tsc::set_wall_clock(unix_secs * 1_000_000_000);
}

/// Nanoseconds since the Unix epoch. Wraps tsc::now_ns().
#[inline]
pub fn now_ns() -> u64 {
    tsc::now_ns()
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Period(u64);

impl Period {
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

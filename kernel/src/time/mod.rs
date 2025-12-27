use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use crate::memory::cpu_local_data::get_local;

mod pit;
pub mod lapic;

static MONOTONIC_TIME_MS: AtomicU64 = AtomicU64::new(0);
pub static TIMEKEEPER_CPU: AtomicU32 = AtomicU32::new(0);

pub fn on_timer_tick() {
    let cpu = get_local();

    if cpu.kernel_id == TIMEKEEPER_CPU.load(Ordering::Relaxed) {
        MONOTONIC_TIME_MS.fetch_add(1, Ordering::Relaxed);
    }
}
use core::sync::atomic::Ordering;
use x86::msr::IA32_TSC_DEADLINE;
use crate::memory::cpu_local_data::get_local;
use crate::time::tsc;
use crate::time::tsc::{TSC_TPQS};

pub fn set_deadline(qs: u64) {
    let tsc_now = tsc::value();
    let tsc_tpqs = TSC_TPQS.load(Ordering::Relaxed);

    let delta = qs.saturating_mul(tsc_tpqs);
    let deadline = tsc_now.saturating_add(delta);

    unsafe {
        x86::msr::wrmsr(IA32_TSC_DEADLINE, deadline);
    }
}
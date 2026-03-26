//! Reusable benchmark harness for Bos OS.
//!
//! Any benchmark binary can use `run_benchmark` to get consistent
//! warmup, measurement, per-task CPU tick reporting, and shutdown.

use crate::{sys_debug_log, sys_get_task_cpu_ticks, sys_get_ticks, sys_shutdown};

// Debug log tags — parsed by bench_runner on the host side.
// Range 0x4243_48xx is reserved for the benchmark framework.
pub const TAG_BENCH_START: u64 = 0x4243_4800;
pub const TAG_ELAPSED_TICKS: u64 = 0x4243_4801;
pub const TAG_SUCCESS_COUNT: u64 = 0x4243_4802;
pub const TAG_CPU_TICKS: u64 = 0x4243_4803;
pub const TAG_BENCH_DONE: u64 = 0x4243_48FF;
pub const TAG_SCENARIO: u64 = 0x4243_4810;
pub const TAG_PARAM_N: u64 = 0x4243_4811;
pub const TAG_WORKERS: u64 = 0x4243_4812;
pub const TAG_BENCH_TYPE: u64 = 0x4243_4813;
pub const TAG_BENCH_NAME: u64 = 0x4243_4814;

/// Run a benchmark: warmup, then measure `rounds` iterations of `f`.
///
/// Reports elapsed ticks, success count, and per-task CPU ticks via
/// `sys_debug_log`, then shuts down the VM.
///
/// `f` returns `true` on success, `false` on failure (for success counting).
pub fn run_benchmark(warmup: u64, rounds: u64, mut f: impl FnMut() -> bool) -> ! {
    // Warmup — not measured
    for _ in 0..warmup {
        f();
    }

    sys_debug_log(rounds, TAG_BENCH_START);
    let start = sys_get_ticks();

    let mut success: u64 = 0;
    for _ in 0..rounds {
        if f() {
            success += 1;
        }
    }

    let elapsed = sys_get_ticks() - start;
    sys_debug_log(elapsed, TAG_ELAPSED_TICKS);
    sys_debug_log(success, TAG_SUCCESS_COUNT);

    report_cpu_ticks();

    sys_debug_log(0, TAG_BENCH_DONE);
    sys_shutdown(0x10);
}

/// Emit per-task CPU ticks for fairness analysis.
/// Scans task IDs 2..64 (skips bench_init at ID 1).
pub fn report_cpu_ticks() {
    for id in 2..64u64 {
        let ticks = sys_get_task_cpu_ticks(id);
        if ticks != u64::MAX {
            let encoded = (id & 0xFFFF) | (ticks << 16);
            sys_debug_log(encoded, TAG_CPU_TICKS);
        }
    }
}

/// Emit scenario metadata tags. Call from bench_init before spawning workloads.
pub fn emit_metadata(bench_type: u64, scenario: u64, param_n: u64, workers: u64) {
    sys_debug_log(bench_type, TAG_BENCH_TYPE);
    sys_debug_log(scenario, TAG_SCENARIO);
    sys_debug_log(param_n, TAG_PARAM_N);
    sys_debug_log(workers, TAG_WORKERS);
}

#![no_std]
#![no_main]

use kernel_api_types::MMAP_WRITE;
use ulib::bench_harness::run_benchmark;

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

// ── Configuration ───────────────────────────────────────────────────────────
// SCENARIO selects which syscall to benchmark:
//   0 = sys_get_ticks  (minimal syscall, measures raw entry/exit overhead)
//   1 = sys_yield       (scheduler round-trip)
//   2 = sys_mmap + sys_munmap (page table manipulation)
//   3 = sys_get_time_ns (TSC read path)
//   4 = sys_channel_create + sys_channel_close (IPC setup/teardown)

const WARMUP_ROUNDS: u64 = 500;
const MEASURE_ROUNDS: u64 = 5000;

// Roles (upper 32 bits of child_arg)
const ROLE_BENCH: u64 = 0;

// ── Entry ───────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(child_arg: u64) -> ! {
    let role = child_arg >> 32;
    let scenario = child_arg & 0xFFFF_FFFF;

    match role {
        ROLE_BENCH => run_scenario(scenario),
        _ => ulib::sys_shutdown(0x11),
    }
}

fn run_scenario(scenario: u64) -> ! {
    match scenario {
        0 => bench_get_ticks(),
        1 => bench_yield(),
        2 => bench_mmap_munmap(),
        3 => bench_get_time_ns(),
        4 => bench_channel_lifecycle(),
        _ => ulib::sys_shutdown(0x11),
    }
}

// ── Scenario 0: sys_get_ticks (raw syscall overhead) ────────────────────────

fn bench_get_ticks() -> ! {
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        let _ = ulib::sys_get_ticks();
        true
    });
}

// ── Scenario 1: sys_yield (scheduler round-trip) ────────────────────────────

fn bench_yield() -> ! {
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        ulib::sys_yield();
        true
    });
}

// ── Scenario 2: sys_mmap + sys_munmap (page alloc/free cycle) ───────────────

fn bench_mmap_munmap() -> ! {
    const PAGE_SIZE: u64 = 4096;
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        let ptr = ulib::sys_mmap(PAGE_SIZE, MMAP_WRITE);
        if ptr.is_null() {
            return false;
        }
        // Touch the page to force actual mapping
        unsafe { core::ptr::write_volatile(ptr, 0xAA) };
        ulib::sys_munmap(ptr, PAGE_SIZE);
        true
    });
}

// ── Scenario 3: sys_get_time_ns (TSC read path) ────────────────────────────

fn bench_get_time_ns() -> ! {
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        let _ = ulib::sys_get_time_ns();
        true
    });
}

// ── Scenario 4: channel create + close (IPC setup/teardown) ─────────────────

fn bench_channel_lifecycle() -> ! {
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        let (send_ep, recv_ep) = ulib::sys_channel_create(1);
        if send_ep == 0 || recv_ep == 0 {
            return false;
        }
        ulib::sys_channel_close(send_ep);
        ulib::sys_channel_close(recv_ep);
        true
    });
}

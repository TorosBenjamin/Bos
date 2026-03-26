#![no_std]
#![no_main]

use kernel_api_types::MMAP_WRITE;
use ulib::bench_harness::run_benchmark;

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

// ── Configuration ───────────────────────────────────────────────────────────
// SCENARIO selects the memory operation to benchmark:
//   0 = mmap 4K + write + munmap (single page lifecycle)
//   1 = mmap 64K + write + munmap (multi-page allocation)
//   2 = shared buffer create + map + destroy (IPC shared memory path)
//   3 = mmap 4K + mprotect (protection flag change)

const WARMUP_ROUNDS: u64 = 500;
const MEASURE_ROUNDS: u64 = 5000;

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
        0 => bench_mmap_4k(),
        1 => bench_mmap_64k(),
        2 => bench_shared_buf(),
        3 => bench_mprotect(),
        _ => ulib::sys_shutdown(0x11),
    }
}

// ── Scenario 0: mmap 4K + touch + munmap ────────────────────────────────────

fn bench_mmap_4k() -> ! {
    const SIZE: u64 = 4096;
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        let ptr = ulib::sys_mmap(SIZE, MMAP_WRITE);
        if ptr.is_null() {
            return false;
        }
        unsafe { core::ptr::write_volatile(ptr, 0xBB) };
        ulib::sys_munmap(ptr, SIZE);
        true
    });
}

// ── Scenario 1: mmap 64K + touch + munmap ───────────────────────────────────

fn bench_mmap_64k() -> ! {
    const SIZE: u64 = 64 * 1024;
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        let ptr = ulib::sys_mmap(SIZE, MMAP_WRITE);
        if ptr.is_null() {
            return false;
        }
        // Touch first and last page
        unsafe {
            core::ptr::write_volatile(ptr, 0xCC);
            core::ptr::write_volatile(ptr.add(SIZE as usize - 1), 0xDD);
        }
        ulib::sys_munmap(ptr, SIZE);
        true
    });
}

// ── Scenario 2: shared buffer create + map + destroy ────────────────────────

fn bench_shared_buf() -> ! {
    const SIZE: u64 = 4096;
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        let (buf_id, ptr) = ulib::sys_create_shared_buf(SIZE);
        if buf_id == u64::MAX || ptr.is_null() {
            return false;
        }
        unsafe { core::ptr::write_volatile(ptr, 0xEE) };
        ulib::sys_munmap(ptr, SIZE);
        ulib::sys_destroy_shared_buf(buf_id);
        true
    });
}

// ── Scenario 3: mmap + mprotect ────────────────────────────────────────────

fn bench_mprotect() -> ! {
    const SIZE: u64 = 4096;
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        let ptr = ulib::sys_mmap(SIZE, MMAP_WRITE);
        if ptr.is_null() {
            return false;
        }
        unsafe { core::ptr::write_volatile(ptr, 0xFF) };
        // Remove write permission, then restore it
        ulib::sys_mprotect(ptr, SIZE, 0);
        ulib::sys_mprotect(ptr, SIZE, MMAP_WRITE);
        ulib::sys_munmap(ptr, SIZE);
        true
    });
}

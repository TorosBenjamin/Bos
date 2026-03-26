#![no_std]
#![no_main]

use kernel_api_types::IPC_OK;
use ulib::bench_harness::run_benchmark;

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

// ── Configuration ───────────────────────────────────────────────────────────
// SCENARIO selects the context-switch measurement method:
//   0 = yield ping-pong (2 tasks yield back and forth)
//   1 = IPC ping-pong (measures context switch via blocking channel send/recv)
//
// For scenario 0, the driver task does yield-based round-trips.
// For scenario 1, two tasks exchange messages — the blocking recv triggers
// a context switch to the sender, measuring the scheduler path.

const WARMUP_ROUNDS: u64 = 500;
const MEASURE_ROUNDS: u64 = 5000;

const ROLE_DRIVER: u64 = 0;
const ROLE_PARTNER: u64 = 1;

// ── Entry ───────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(child_arg: u64) -> ! {
    let role = child_arg >> 32;
    let low = child_arg & 0xFFFF_FFFF;

    match role {
        ROLE_DRIVER => run_driver(low),
        ROLE_PARTNER => run_partner(low),
        _ => ulib::sys_shutdown(0x11),
    }
}

// ── Scenario 0: yield ping-pong ─────────────────────────────────────────────
// The driver yields MEASURE_ROUNDS times. With exactly 2 runnable tasks,
// each yield triggers a context switch to the partner and back.

fn run_driver(scenario: u64) -> ! {
    match scenario {
        0 => bench_yield_pingpong(),
        1 => {
            // IPC-based: low bits encode endpoints
            // This path is set up by bench_init which passes endpoints
            ulib::sys_shutdown(0x11); // should not reach here — bench_init encodes eps
        }
        _ => ulib::sys_shutdown(0x11),
    }
}

fn bench_yield_pingpong() -> ! {
    // Both tasks just yield. Each yield = 1 context switch.
    // 2 yields = 1 round-trip (driver→partner→driver).
    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        ulib::sys_yield();
        ulib::sys_yield();
        true
    });
}

fn run_partner(scenario: u64) -> ! {
    match scenario {
        0 => {
            // Yield partner: just yield forever
            loop {
                ulib::sys_yield();
            }
        }
        1 => {
            // IPC partner: should not be reached without endpoints
            ulib::sys_shutdown(0x11);
        }
        _ => ulib::sys_shutdown(0x11),
    }
}

// ── IPC ping-pong driver (called from bench_init with endpoint args) ────────

/// IPC-based context switch measurement. bench_init passes endpoints via child_arg.
pub fn ipc_driver(send_ep: u64, recv_ep: u64) -> ! {
    let msg = [0xAAu8; 8];
    let mut buf = [0u8; 64];

    run_benchmark(WARMUP_ROUNDS, MEASURE_ROUNDS, || {
        if ulib::sys_channel_send(send_ep, &msg) != IPC_OK {
            return false;
        }
        let (r, _) = ulib::sys_channel_recv(recv_ep, &mut buf);
        r == IPC_OK
    });
}

/// IPC echo partner. Receives and echoes back forever.
pub fn ipc_partner(recv_ep: u64, send_ep: u64) -> ! {
    let mut buf = [0u8; 64];
    loop {
        let (r, n) = ulib::sys_channel_recv(recv_ep, &mut buf);
        if r == IPC_OK {
            ulib::sys_channel_send(send_ep, &buf[..n as usize]);
        }
    }
}

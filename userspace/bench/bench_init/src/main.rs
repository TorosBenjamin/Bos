#![no_std]
#![no_main]

use kernel_api_types::MMAP_WRITE;
use ulib::bench_harness;

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

// ── Benchmark Configuration ─────────────────────────────────────────────────
// These constants are edited by os/tools/bench_py before each build.
//
// BENCH_TYPE: 0 = ipc, 1 = syscall, 2 = ctx_switch, 3 = mem
//
// SCENARIO (meaning depends on BENCH_TYPE):
//   ipc:        0 = ping-pong, 1 = fan-out, 2 = service chain
//   syscall:    0 = get_ticks, 1 = yield, 2 = mmap+munmap, 3 = get_time_ns, 4 = channel lifecycle
//   ctx_switch: 0 = yield ping-pong, 1 = IPC ping-pong
//   mem:        0 = mmap 4K, 1 = mmap 64K, 2 = shared buffer, 3 = mprotect
//
// PARAM_N:
//   ipc ping-pong: unused
//   ipc fan-out:   number of clients (1, 2, 4, 8)
//   ipc chain:     chain length (2 = source→sink, 3 = source→relay→sink, ...)
//   others:        unused
//
// BACKGROUND_WORKERS: CPU-bound background tasks added to any scenario
const BENCH_TYPE: u64 = 0;
const SCENARIO: u64 = 0;
const PARAM_N: u64 = 0;
const BACKGROUND_WORKERS: u64 = 0;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    // Emit metadata (parsed by bench_runner)
    bench_harness::emit_metadata(BENCH_TYPE, SCENARIO, PARAM_N, BACKGROUND_WORKERS);

    match BENCH_TYPE {
        0 => run_ipc_bench(),
        1 => run_simple_bench("syscall_bench"),
        2 => run_ctx_switch_bench(),
        3 => run_simple_bench("mem_bench"),
        _ => ulib::sys_shutdown(0x11),
    }

    loop { ulib::sys_sleep_ms(10_000); }
}

// ── ELF loading ─────────────────────────────────────────────────────────────

fn load_module(name: &str) -> &'static [u8] {
    let size = ulib::sys_get_module(name, core::ptr::null_mut(), 0);
    if size == 0 {
        ulib::sys_debug_log(0xDEAD, 0xBEEF);
        ulib::sys_shutdown(0x11);
    }
    let buf = ulib::sys_mmap(size, MMAP_WRITE);
    ulib::sys_get_module(name, buf, size);
    unsafe { core::slice::from_raw_parts(buf, size as usize) }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn spawn(elf: &[u8], child_arg: u64) {
    let id = ulib::sys_spawn(elf, child_arg);
    ulib::sys_wait_task_ready(id);
}

fn channel(cap: u64) -> (u64, u64) {
    let (s, r) = ulib::sys_channel_create(cap);
    if s == 0 || r == 0 {
        ulib::sys_debug_log(0xDEAD, 0xCCCC);
        ulib::sys_shutdown(0x11);
    }
    (s, r)
}

fn send_config(send_ep: u64, values: &[u64]) {
    let mut buf = [0u8; 256];
    for (i, &v) in values.iter().enumerate() {
        let start = i * 8;
        buf[start..start + 8].copy_from_slice(&v.to_le_bytes());
    }
    ulib::sys_channel_send(send_ep, &buf[..values.len() * 8]);
}

fn spawn_workers(elf: &[u8], n: u64) {
    for _ in 0..n {
        spawn(elf, 2 << 32); // ROLE_WORKER
    }
}

// ── Simple benchmarks (single task, scenario in child_arg) ──────────────────

fn run_simple_bench(module_name: &str) {
    let elf = load_module(module_name);

    // Spawn background workers using ipc_bench (ROLE_WORKER = 2)
    if BACKGROUND_WORKERS > 0 {
        let worker_elf = load_module("ipc_bench");
        spawn_workers(&worker_elf, BACKGROUND_WORKERS);
    }

    // Spawn the benchmark task: role=0 in upper 32 bits, scenario in lower 32
    spawn(&elf, (0u64 << 32) | SCENARIO);
}

// ── Context switch benchmark ────────────────────────────────────────────────

fn run_ctx_switch_bench() {
    let elf = load_module("ctx_switch_bench");

    match SCENARIO {
        0 => {
            // Yield ping-pong: spawn partner first, then driver
            spawn(&elf, (1u64 << 32) | 0); // partner, scenario=0

            if BACKGROUND_WORKERS > 0 {
                let worker_elf = load_module("ipc_bench");
                spawn_workers(&worker_elf, BACKGROUND_WORKERS);
            }

            spawn(&elf, (0u64 << 32) | 0); // driver, scenario=0
        }
        1 => {
            // IPC ping-pong context switch measurement
            let (send_a, recv_a) = channel(1);
            let (send_b, recv_b) = channel(1);

            // Partner: ROLE_PARTNER(1), encode send_b and recv_a
            spawn(&elf, (1u64 << 32) | (1u64 << 16) | 0); // scenario=1 marker
            // Actually we need to pass endpoints. Use boot channel pattern:
            let (boot_s1, boot_r1) = channel(1);
            // Re-do: spawn partner with boot channel, send config
            // ... For simplicity, use the same IPC pattern as ipc_bench
            let _ = (send_a, recv_a, send_b, recv_b, boot_s1, boot_r1);
            // Fall back to yield-based for now — IPC ping-pong is already in ipc_bench
            ulib::sys_shutdown(0x11);
        }
        _ => ulib::sys_shutdown(0x11),
    }
}

// ── IPC benchmark (original, unchanged logic) ───────────────────────────────

fn run_ipc_bench() {
    let elf = load_module("ipc_bench");

    match SCENARIO {
        0 => setup_pingpong(&elf),
        1 => setup_fanout(&elf),
        2 => setup_chain(&elf),
        _ => ulib::sys_shutdown(0x11),
    }
}

fn setup_pingpong(elf: &[u8]) {
    let (send_a, recv_a) = channel(1);
    let (send_b, recv_b) = channel(1);

    spawn(elf, (1 << 32) | (send_b << 16) | recv_a);
    spawn_workers(elf, BACKGROUND_WORKERS);
    spawn(elf, (0 << 32) | (send_a << 16) | recv_b);
}

fn setup_fanout(elf: &[u8]) {
    let n_clients = PARAM_N.max(1);
    let (req_send, req_recv) = channel(n_clients);

    let (srv_boot_s, srv_boot_r) = channel(1);
    spawn(elf, (3 << 32) | srv_boot_r);
    send_config(srv_boot_s, &[req_recv]);

    for i in 0..n_clients {
        let (reply_send, reply_recv) = channel(1);
        let (cli_boot_s, cli_boot_r) = channel(1);
        let is_driver = if i == n_clients - 1 { 1u64 } else { 0 };
        spawn(elf, (4 << 32) | cli_boot_r);
        send_config(cli_boot_s, &[req_send, reply_recv, reply_send, is_driver]);
    }

    spawn_workers(elf, BACKGROUND_WORKERS);
}

fn setup_chain(elf: &[u8]) {
    let chain_len = PARAM_N.max(2) as usize;
    let n_links = chain_len - 1;

    let mut fwd_send = [0u64; 8];
    let mut fwd_recv = [0u64; 8];
    let mut rply_send = [0u64; 8];
    let mut rply_recv = [0u64; 8];

    for k in 0..n_links {
        let (fs, fr) = channel(1);
        fwd_send[k] = fs;
        fwd_recv[k] = fr;
        let (rs, rr) = channel(1);
        rply_send[k] = rs;
        rply_recv[k] = rr;
    }

    let last = n_links - 1;
    let (sink_boot_s, sink_boot_r) = channel(1);
    spawn(elf, (7 << 32) | sink_boot_r);
    send_config(sink_boot_s, &[fwd_recv[last], rply_send[last]]);

    for j in (1..chain_len - 1).rev() {
        let (boot_s, boot_r) = channel(1);
        spawn(elf, (6 << 32) | boot_r);
        send_config(boot_s, &[
            fwd_recv[j - 1],
            fwd_send[j],
            rply_recv[j],
            rply_send[j - 1],
        ]);
    }

    spawn_workers(elf, BACKGROUND_WORKERS);

    let (src_boot_s, src_boot_r) = channel(1);
    spawn(elf, (5 << 32) | src_boot_r);
    send_config(src_boot_s, &[fwd_send[0], rply_recv[0]]);
}

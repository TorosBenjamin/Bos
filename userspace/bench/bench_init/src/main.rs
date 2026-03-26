#![no_std]
#![no_main]

use kernel_api_types::MMAP_WRITE;

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

// ── Benchmark Configuration ─────────────────────────────────────────────────
// These constants are edited by paper/run_bench.py before each build.
//
// SCENARIO: 0 = ping-pong, 1 = fan-out, 2 = service chain
// PARAM_N:
//   ping-pong: unused (always 1 pair)
//   fan-out:   number of clients (1, 2, 4, 8)
//   chain:     chain length in nodes (2 = source→sink, 3 = source→relay→sink, ...)
// BACKGROUND_WORKERS: CPU-bound background tasks added to any scenario
const SCENARIO: u64 = 0;
const PARAM_N: u64 = 0;
const BACKGROUND_WORKERS: u64 = 0;

// Debug log tags for scenario metadata
const TAG_SCENARIO: u64 = 0x4243_4810;
const TAG_PARAM_N: u64  = 0x4243_4811;
const TAG_WORKERS: u64  = 0x4243_4812;

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point() -> ! {
    let elf = load_ipc_bench();

    // Emit scenario metadata (parsed by bench_runner)
    ulib::sys_debug_log(SCENARIO, TAG_SCENARIO);
    ulib::sys_debug_log(PARAM_N, TAG_PARAM_N);
    ulib::sys_debug_log(BACKGROUND_WORKERS, TAG_WORKERS);

    match SCENARIO {
        0 => setup_pingpong(&elf),
        1 => setup_fanout(&elf),
        2 => setup_chain(&elf),
        _ => ulib::sys_shutdown(0x11),
    }

    loop { ulib::sys_sleep_ms(10_000); }
}

// ── ELF loading ─────────────────────────────────────────────────────────────

fn load_ipc_bench() -> &'static [u8] {
    let size = ulib::sys_get_module("ipc_bench", core::ptr::null_mut(), 0);
    if size == 0 {
        ulib::sys_debug_log(0xDEAD, 0xBEEF);
        ulib::sys_shutdown(0x11);
    }
    let buf = ulib::sys_mmap(size, MMAP_WRITE);
    ulib::sys_get_module("ipc_bench", buf, size);
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

// ── Scenario 0: Ping-Pong ───────────────────────────────────────────────────

fn setup_pingpong(elf: &[u8]) {
    let (send_a, recv_a) = channel(1); // ping → pong
    let (send_b, recv_b) = channel(1); // pong → ping

    // Spawn pong
    spawn(elf, (1 << 32) | (send_b << 16) | recv_a);

    // Spawn background workers (before ping so they're already in the queue)
    spawn_workers(elf, BACKGROUND_WORKERS);

    // Spawn ping (driver) — last, so it competes with workers
    spawn(elf, (0 << 32) | (send_a << 16) | recv_b);
}

// ── Scenario 1: Fan-out ─────────────────────────────────────────────────────

fn setup_fanout(elf: &[u8]) {
    let n_clients = PARAM_N.max(1);

    // Shared request channel (capacity = n_clients so sends don't block)
    let (req_send, req_recv) = channel(n_clients);

    // Spawn server
    let (srv_boot_s, srv_boot_r) = channel(1);
    spawn(elf, (3 << 32) | srv_boot_r); // ROLE_FANOUT_SERVER
    send_config(srv_boot_s, &[req_recv]);

    // Spawn clients (last one is the driver/measurer)
    for i in 0..n_clients {
        let (reply_send, reply_recv) = channel(1);
        let (cli_boot_s, cli_boot_r) = channel(1);
        let is_driver = if i == n_clients - 1 { 1u64 } else { 0 };

        spawn(elf, (4 << 32) | cli_boot_r); // ROLE_FANOUT_CLIENT
        send_config(cli_boot_s, &[req_send, reply_recv, reply_send, is_driver]);
    }

    spawn_workers(elf, BACKGROUND_WORKERS);
}

// ── Scenario 2: Service Chain ───────────────────────────────────────────────

fn setup_chain(elf: &[u8]) {
    // chain_len = total nodes. Minimum 2 (source → sink).
    let chain_len = PARAM_N.max(2) as usize;
    let n_links = chain_len - 1;

    // Create forward + reply channels for each link
    // Link k: node k → node k+1
    let mut fwd_send  = [0u64; 8];
    let mut fwd_recv  = [0u64; 8];
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

    // Spawn sink (node chain_len-1)
    let last = n_links - 1;
    let (sink_boot_s, sink_boot_r) = channel(1);
    spawn(elf, (7 << 32) | sink_boot_r); // ROLE_CHAIN_SINK
    send_config(sink_boot_s, &[fwd_recv[last], rply_send[last]]);

    // Spawn relays (nodes 1..chain_len-2) — from sink-side inward
    for j in (1..chain_len - 1).rev() {
        // Node j: upstream = link j-1, downstream = link j
        let (boot_s, boot_r) = channel(1);
        spawn(elf, (6 << 32) | boot_r); // ROLE_CHAIN_RELAY
        send_config(boot_s, &[
            fwd_recv[j - 1],  // upstream recv
            fwd_send[j],      // downstream send
            rply_recv[j],     // downstream reply recv
            rply_send[j - 1], // upstream reply send
        ]);
    }

    spawn_workers(elf, BACKGROUND_WORKERS);

    // Spawn source (node 0) — driver, last to start
    let (src_boot_s, src_boot_r) = channel(1);
    spawn(elf, (5 << 32) | src_boot_r); // ROLE_CHAIN_SOURCE
    send_config(src_boot_s, &[fwd_send[0], rply_recv[0]]);
}

#![no_std]
#![no_main]

use kernel_api_types::IPC_OK;

#[panic_handler]
fn rust_panic(info: &core::panic::PanicInfo) -> ! {
    ulib::default_panic(info)
}

// ── Constants ────────────────────────────────────────────────────────────────

const WARMUP_ROUNDS: u64 = 500;
const MEASURE_ROUNDS: u64 = 5000;

// Debug log tags (parsed by bench_runner)
const TAG_BENCH_START: u64   = 0x4243_4800;
const TAG_ELAPSED_TICKS: u64 = 0x4243_4801;
const TAG_SUCCESS_COUNT: u64 = 0x4243_4802;
const TAG_CPU_TICKS: u64     = 0x4243_4803; // value = task_id | (cpu_ticks << 16)
const TAG_BENCH_DONE: u64    = 0x4243_48FF;

// Roles (encoded in upper 32 bits of child_arg)
const ROLE_PING: u64           = 0;
const ROLE_PONG: u64           = 1;
const ROLE_WORKER: u64         = 2;
const ROLE_FANOUT_SERVER: u64  = 3;
const ROLE_FANOUT_CLIENT: u64  = 4;
const ROLE_CHAIN_SOURCE: u64   = 5;
const ROLE_CHAIN_RELAY: u64    = 6;
const ROLE_CHAIN_SINK: u64     = 7;

// ── Entry ────────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
unsafe extern "sysv64" fn entry_point(child_arg: u64) -> ! {
    let role = child_arg >> 32;
    let low  = child_arg & 0xFFFF_FFFF;

    match role {
        ROLE_PING          => run_ping((low >> 16) & 0xFFFF, low & 0xFFFF),
        ROLE_PONG          => run_pong((low >> 16) & 0xFFFF, low & 0xFFFF),
        ROLE_WORKER        => run_worker(),
        ROLE_FANOUT_SERVER => run_fanout_server(low),
        ROLE_FANOUT_CLIENT => run_fanout_client(low),
        ROLE_CHAIN_SOURCE  => run_chain_source(low),
        ROLE_CHAIN_RELAY   => run_chain_relay(low),
        ROLE_CHAIN_SINK    => run_chain_sink(low),
        _ => ulib::sys_shutdown(0x11),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn read_config(bootstrap_ep: u64, out: &mut [u64]) -> usize {
    let mut buf = [0u8; 256];
    let (res, bytes) = ulib::sys_channel_recv(bootstrap_ep, &mut buf);
    if res != IPC_OK { return 0; }
    let count = (bytes as usize / 8).min(out.len());
    for i in 0..count {
        let start = i * 8;
        out[i] = u64::from_le_bytes([
            buf[start], buf[start+1], buf[start+2], buf[start+3],
            buf[start+4], buf[start+5], buf[start+6], buf[start+7],
        ]);
    }
    count
}

/// Measure MEASURE_ROUNDS send/recv round-trips and report results.
fn measure_loop(send_ep: u64, recv_ep: u64, msg: &[u8]) -> ! {
    let mut buf = [0u8; 64];

    // Warm-up
    for _ in 0..WARMUP_ROUNDS {
        ulib::sys_channel_send(send_ep, msg);
        ulib::sys_channel_recv(recv_ep, &mut buf);
    }

    ulib::sys_debug_log(MEASURE_ROUNDS, TAG_BENCH_START);
    let start = ulib::sys_get_ticks();

    let mut success: u64 = 0;
    for _ in 0..MEASURE_ROUNDS {
        if ulib::sys_channel_send(send_ep, msg) == IPC_OK {
            let (r, _) = ulib::sys_channel_recv(recv_ep, &mut buf);
            if r == IPC_OK { success += 1; }
        }
    }

    let elapsed = ulib::sys_get_ticks() - start;
    ulib::sys_debug_log(elapsed, TAG_ELAPSED_TICKS);
    ulib::sys_debug_log(success, TAG_SUCCESS_COUNT);

    // Report per-task CPU ticks for fairness analysis.
    // Scan task IDs 2..64; skip task 1 (bench_init orchestrator, not part of workload).
    for id in 2..64u64 {
        let ticks = ulib::sys_get_task_cpu_ticks(id);
        if ticks != u64::MAX {
            // Encode: value = task_id (low 16 bits) | cpu_ticks (shifted left 16)
            let encoded = (id & 0xFFFF) | (ticks << 16);
            ulib::sys_debug_log(encoded, TAG_CPU_TICKS);
        }
    }

    ulib::sys_debug_log(0, TAG_BENCH_DONE);
    ulib::sys_shutdown(0x10);
}

/// Echo loop: receive a message, send it back. Runs forever.
fn echo_loop(recv_ep: u64, send_ep: u64) -> ! {
    let mut buf = [0u8; 64];
    loop {
        let (r, n) = ulib::sys_channel_recv(recv_ep, &mut buf);
        if r == IPC_OK {
            ulib::sys_channel_send(send_ep, &buf[..n as usize]);
        }
    }
}

// ── Scenario 1: Ping-Pong ───────────────────────────────────────────────────

fn run_ping(send_ep: u64, recv_ep: u64) -> ! {
    measure_loop(send_ep, recv_ep, &[0xAA; 8]);
}

fn run_pong(send_ep: u64, recv_ep: u64) -> ! {
    echo_loop(recv_ep, send_ep)
}

fn run_worker() -> ! {
    // CPU-bound spin. Burns scheduler ticks without doing IPC.
    let mut x: u64 = 1;
    loop {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        // Yield occasionally so the scheduler runs. Without this, the worker
        // would hold the CPU for its entire quantum anyway (preempted by timer),
        // but an explicit yield makes the behavior clearer.
        if x & 0xFFFF == 0 {
            core::hint::black_box(x);
        }
    }
}

// ── Scenario 2: Fan-out ─────────────────────────────────────────────────────

fn run_fanout_server(bootstrap_ep: u64) -> ! {
    // Config: [req_recv_ep]
    let mut cfg = [0u64; 1];
    read_config(bootstrap_ep, &mut cfg);
    let req_recv = cfg[0];

    let mut buf = [0u8; 64];
    let reply = [0xBB; 8];

    loop {
        let (r, n) = ulib::sys_channel_recv(req_recv, &mut buf);
        if r == IPC_OK && n >= 8 {
            // First 8 bytes of request = client's reply_send_ep
            let reply_ep = u64::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3],
                buf[4], buf[5], buf[6], buf[7],
            ]);
            ulib::sys_channel_send(reply_ep, &reply);
        }
    }
}

fn run_fanout_client(bootstrap_ep: u64) -> ! {
    // Config: [req_send_ep, reply_recv_ep, reply_send_ep, is_driver]
    let mut cfg = [0u64; 4];
    read_config(bootstrap_ep, &mut cfg);
    let req_send    = cfg[0];
    let reply_recv  = cfg[1];
    let reply_send  = cfg[2];
    let is_driver   = cfg[3] != 0;

    // Request message: [reply_send_ep as LE bytes]
    let mut req = [0u8; 8];
    req.copy_from_slice(&reply_send.to_le_bytes());

    if is_driver {
        measure_loop(req_send, reply_recv, &req);
    } else {
        // Non-measuring client: loop forever adding load
        let mut buf = [0u8; 64];
        loop {
            ulib::sys_channel_send(req_send, &req);
            ulib::sys_channel_recv(reply_recv, &mut buf);
        }
    }
}

// ── Scenario 3: Service Chain ───────────────────────────────────────────────

fn run_chain_source(bootstrap_ep: u64) -> ! {
    // Config: [fwd_send_ep, reply_recv_ep]
    let mut cfg = [0u64; 2];
    read_config(bootstrap_ep, &mut cfg);
    measure_loop(cfg[0], cfg[1], &[0xCC; 8]);
}

fn run_chain_relay(bootstrap_ep: u64) -> ! {
    // Config: [upstream_recv, downstream_send, downstream_reply_recv, upstream_reply_send]
    let mut cfg = [0u64; 4];
    read_config(bootstrap_ep, &mut cfg);
    let up_recv    = cfg[0];
    let down_send  = cfg[1];
    let down_reply = cfg[2];
    let up_reply   = cfg[3];

    let mut buf = [0u8; 64];
    loop {
        // Forward request downstream
        let (r, n) = ulib::sys_channel_recv(up_recv, &mut buf);
        if r == IPC_OK {
            ulib::sys_channel_send(down_send, &buf[..n as usize]);
        }
        // Forward reply upstream
        let (r, n) = ulib::sys_channel_recv(down_reply, &mut buf);
        if r == IPC_OK {
            ulib::sys_channel_send(up_reply, &buf[..n as usize]);
        }
    }
}

fn run_chain_sink(bootstrap_ep: u64) -> ! {
    // Config: [recv_ep, reply_send_ep]
    let mut cfg = [0u64; 2];
    read_config(bootstrap_ep, &mut cfg);
    echo_loop(cfg[0], cfg[1])
}

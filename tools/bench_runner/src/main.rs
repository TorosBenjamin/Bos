use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::{env, process};

/// Tags emitted by ipc_bench via sys_debug_log.
/// The kernel prints: `DBG[{tag:#x}]: {value:#x}` (wrapped in ANSI color codes).
const TAG_BENCH_START: u64      = 0x4243_4800;
const TAG_ELAPSED_TICKS: u64    = 0x4243_4801;
const TAG_SUCCESS_COUNT: u64    = 0x4243_4802;
const TAG_CPU_TICKS: u64        = 0x4243_4803;
const TAG_BENCH_DONE: u64       = 0x4243_48FF;
const TAG_SCENARIO: u64         = 0x4243_4810;
const TAG_PARAM_N: u64          = 0x4243_4811;
const TAG_WORKERS: u64          = 0x4243_4812;

/// Strip ANSI escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until we hit a letter (the terminator of the escape sequence)
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn find_ovmf() -> (String, String) {
    const CANDIDATES: &[(&str, &str)] = &[
        ("/usr/share/edk2/x64/OVMF_CODE.4m.fd",    "/usr/share/edk2/x64/OVMF_VARS.4m.fd"),
        ("/usr/share/OVMF/OVMF_CODE_4M.fd",         "/usr/share/OVMF/OVMF_VARS_4M.fd"),
        ("/usr/share/OVMF/OVMF_CODE.fd",            "/usr/share/OVMF/OVMF_VARS.fd"),
        ("/usr/share/edk2/ovmf/OVMF_CODE.fd",       "/usr/share/edk2/ovmf/OVMF_VARS.fd"),
    ];
    for &(code, vars) in CANDIDATES {
        if std::path::Path::new(code).exists() && std::path::Path::new(vars).exists() {
            return (code.to_string(), vars.to_string());
        }
    }
    panic!(
        "OVMF firmware not found. Install it (e.g. `pacman -S edk2-ovmf` or `apt install ovmf`) \
         or set OVMF_CODE / OVMF_VARS environment variables."
    );
}

fn main() {
    let (ovmf_code, ovmf_vars_readonly) = (
        env::var("OVMF_CODE").unwrap_or_else(|_| find_ovmf().0),
        env::var("OVMF_VARS").unwrap_or_else(|_| find_ovmf().1),
    );

    // Local writable copy of OVMF vars
    let out_dir = env::current_dir().unwrap().join("target");
    let local_vars = out_dir.join("OVMF_VARS_BENCH.fd");
    if !local_vars.exists() {
        std::fs::copy(&ovmf_vars_readonly, &local_vars)
            .expect("Failed to copy OVMF_VARS to local directory");
    }

    let disk_img = env!("DISK_IMG");

    // Wrap QEMU invocation with taskset to pin to a single host CPU core.
    let mut cmd = Command::new("taskset");
    cmd.args(["-c", "0"]);
    cmd.arg("qemu-system-x86_64");

    // KVM acceleration
    cmd.arg("-enable-kvm");

    // Single vCPU — isolates scheduling policy as the only variable
    cmd.arg("-smp").arg("1");

    // Headless — no display, no GPU overhead
    cmd.arg("-display").arg("none");

    // Serial on stdout — benchmark results come through here
    cmd.arg("-serial").arg("stdio");

    // Clean shutdown via isa-debug-exit (guest writes to port 0xf4)
    cmd.arg("-device").arg("isa-debug-exit,iobase=0xf4,iosize=0x04");

    // No reboot on triple-fault or shutdown — just exit
    cmd.arg("--no-reboot");

    // Fixed CPU model for reproducibility
    cmd.arg("-cpu").arg("host");

    // OVMF firmware
    cmd.arg("-drive").arg(format!(
        "if=pflash,format=raw,unit=0,file={ovmf_code},readonly=on"
    ));
    cmd.arg("-drive").arg(format!(
        "if=pflash,format=raw,unit=1,file={}", local_vars.display()
    ));

    // Boot ISO (bench.iso with bench_init as init_task)
    cmd.arg("-cdrom").arg(env!("ISO"));

    // FAT32 disk (kernel expects an IDE drive even if unused by bench)
    cmd.arg("-drive").arg(format!(
        "file={disk_img},if=ide,format=raw,media=disk"
    ));

    // No network — benchmark doesn't need it
    cmd.arg("-nic").arg("none");

    // Capture serial output to parse benchmark results
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    eprintln!("[bench_runner] Starting QEMU (headless, 1 vCPU, pinned to core 0)...");

    let mut child = cmd.spawn().expect("Failed to start QEMU");

    let stdout = child.stdout.take().expect("Failed to capture QEMU stdout");
    let reader = BufReader::new(stdout);

    let mut bench_started = false;
    let mut elapsed_ns: Option<u64> = None;
    let mut success_count: Option<u64> = None;
    let mut scenario: Option<u64> = None;
    let mut param_n: Option<u64> = None;
    let mut workers: Option<u64> = None;
    let mut cpu_ticks: Vec<(u64, u64)> = Vec::new(); // (task_id, ticks)

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        // Strip ANSI color codes before parsing
        let clean = strip_ansi(&line);

        // Echo all serial output so user can see boot messages
        eprintln!("[serial] {clean}");

        // Parse benchmark tags from kernel debug log output.
        // The kernel's sys_debug_log prints: DBG[{tag:#x}]: {value:#x}
        // Full line after stripping ANSI: " INFO [0] [  123ms] DBG[0x42434800]: 0x1388"
        if let Some(dbg_pos) = clean.find("DBG[") {
            let rest = &clean[dbg_pos + 4..]; // after "DBG["
            if let Some(bracket_end) = rest.find("]: ") {
                let tag_str = &rest[..bracket_end];
                let val_str = rest[bracket_end + 3..].trim();

                let tag = u64::from_str_radix(
                    tag_str.trim_start_matches("0x"),
                    16,
                ).unwrap_or(0);
                let value = u64::from_str_radix(
                    val_str.trim_start_matches("0x"),
                    16,
                ).unwrap_or(0);

                match tag {
                    TAG_SCENARIO => {
                        scenario = Some(value);
                        println!("SCENARIO:{value}");
                    }
                    TAG_PARAM_N => {
                        param_n = Some(value);
                        println!("PARAM_N:{value}");
                    }
                    TAG_WORKERS => {
                        workers = Some(value);
                        println!("WORKERS:{value}");
                    }
                    TAG_BENCH_START => {
                        bench_started = true;
                        eprintln!("[bench_runner] Benchmark started (rounds={value})");
                    }
                    TAG_ELAPSED_TICKS if bench_started => {
                        elapsed_ns = Some(value);
                        eprintln!("[bench_runner] Elapsed: {value} ticks");
                    }
                    TAG_SUCCESS_COUNT if bench_started => {
                        success_count = Some(value);
                        eprintln!("[bench_runner] Success count: {value}");
                    }
                    TAG_CPU_TICKS if bench_started => {
                        let task_id = value & 0xFFFF;
                        let ticks = value >> 16;
                        cpu_ticks.push((task_id, ticks));
                        eprintln!("[bench_runner] Task {task_id} cpu_ticks: {ticks}");
                    }
                    TAG_BENCH_DONE => {
                        eprintln!("[bench_runner] Benchmark complete.");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    let status = child.wait().expect("Failed to wait for QEMU");

    // QEMU with isa-debug-exit: guest writes (val) to port 0xf4,
    // QEMU exits with code (val << 1) | 1. So 0x10 → exit code 0x21 = 33.
    let code = status.code().unwrap_or(-1);
    let success = code == 33; // 0x10 << 1 | 1

    eprintln!();
    eprintln!("═══════════════════════════════════════════");
    eprintln!("  Benchmark Results");
    eprintln!("═══════════════════════════════════════════");

    match (elapsed_ns, success_count) {
        (Some(ticks), Some(count)) => {
            eprintln!("  Elapsed:        {ticks} ticks");
            eprintln!("  Round-trips:    {count}/{}", 5000);
            eprintln!("  Ticks/5000 rts: {ticks}");
            // Machine-readable output for the Python script
            println!("ELAPSED_TICKS:{ticks}");
            println!("SUCCESS:{count}");
            for (task_id, task_ticks) in &cpu_ticks {
                println!("CPU_TICKS:{task_id}:{task_ticks}");
            }
        }
        _ => {
            eprintln!("  No results captured!");
        }
    }

    eprintln!("  QEMU exit code: {code} ({})", if success { "OK" } else { "UNEXPECTED" });
    eprintln!("═══════════════════════════════════════════");

    process::exit(if success { 0 } else { 1 });
}

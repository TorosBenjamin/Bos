"""Build, QEMU management, and single-run execution."""

import os
import signal
import subprocess
import time

from .config import OS_DIR, RUN_TIMEOUT
from .stats import compute_jain_index


def build_bench() -> bool:
    """Build bench_runner (rebuilds bench_init if source changed)."""
    env = os.environ.copy()
    env.setdefault("LIMINE_PATH", "/usr/local/share/limine")
    result = subprocess.run(
        ["cargo", "build", "-p", "bench_runner"],
        cwd=OS_DIR, env=env,
        capture_output=True,
    )
    if result.returncode != 0:
        print("  [BUILD FAIL]")
        print(result.stderr.decode("utf-8", errors="replace")[-500:])
        return False
    return True


def kill_stale_qemu():
    """Kill any leftover qemu-system-x86_64 processes from a previous run."""
    subprocess.run(
        ["pkill", "-f", "qemu-system-x86_64.*bench"],
        capture_output=True,
    )
    time.sleep(0.5)


def run_single_bench(run_index: int, total: int) -> dict | None:
    """Run bench_runner once and return parsed results or None."""
    env = os.environ.copy()
    env.setdefault("LIMINE_PATH", "/usr/local/share/limine")

    print(f"\n{'─'*50}")
    print(f"  Run {run_index + 1}/{total}")
    print(f"{'─'*50}")

    proc = subprocess.Popen(
        ["cargo", "run", "-p", "bench_runner", "--"],
        cwd=OS_DIR,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )

    try:
        stdout, stderr = proc.communicate(timeout=RUN_TIMEOUT)
    except subprocess.TimeoutExpired:
        print(f"  [TIMEOUT] Run {run_index + 1} timed out after {RUN_TIMEOUT}s")
        os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        proc.wait()
        time.sleep(1)
        return None

    stdout_text = stdout.decode("utf-8", errors="replace")
    stderr_text = stderr.decode("utf-8", errors="replace")

    if proc.returncode != 0:
        print(f"  [FAIL] Exit code {proc.returncode}")
        for line in stderr_text.splitlines()[-10:]:
            print(f"    {line}")
        return None

    # Parse: KEY:value and CPU_TICKS:task_id:ticks from stdout
    result = {}
    cpu_ticks_map = {}
    for line in stdout_text.splitlines():
        line = line.strip()
        if line.startswith("CPU_TICKS:"):
            parts = line.split(":")
            if len(parts) == 3:
                try:
                    tid = int(parts[1])
                    ticks_val = int(parts[2])
                    cpu_ticks_map[tid] = ticks_val
                except ValueError:
                    pass
        elif ":" in line:
            key, _, val = line.partition(":")
            try:
                result[key.strip()] = int(val.strip())
            except ValueError:
                pass

    if "ELAPSED_TICKS" not in result:
        print("  [FAIL] No ELAPSED_TICKS in output")
        return None

    result["cpu_ticks"] = cpu_ticks_map

    ticks = result["ELAPSED_TICKS"]
    success = result.get("SUCCESS", 0)
    ticks_per_rt = ticks / success if success > 0 else 0

    fairness_str = ""
    if cpu_ticks_map:
        jain = compute_jain_index(list(cpu_ticks_map.values()))
        result["jain_index"] = jain
        fairness_str = f"  Jain={jain:.4f}"

    print(f"  [OK] {ticks} ticks for {success} ops ({ticks_per_rt:.2f} ticks/op){fairness_str}")
    return result

"""Paths, constants, and benchmark type/scenario definitions."""

from pathlib import Path

# ── Paths ────────────────────────────────────────────────────────────────────

# bench_py lives at os/tools/bench_py/
_THIS_DIR = Path(__file__).resolve().parent
OS_DIR = _THIS_DIR.parent.parent          # os/
REPO_ROOT = OS_DIR.parent                 # Bos/
RESULTS_DIR = REPO_ROOT / "paper" / "results"

BENCH_INIT_SRC = OS_DIR / "userspace" / "bench" / "bench_init" / "src" / "main.rs"
SCHEDULER_SRC = OS_DIR / "kernel" / "core" / "src" / "task" / "local_scheduler.rs"
POLICY_MOD_SRC = OS_DIR / "kernel" / "core" / "src" / "task" / "policy" / "mod.rs"

# ── Timing ───────────────────────────────────────────────────────────────────

RUN_TIMEOUT = 300   # seconds per QEMU invocation
MEASURE_ROUNDS = 5000

# ── Scheduling policies ─────────────────────────────────────────────────────

POLICIES = {
    "round_robin": "RoundRobinPolicy",
    "priority": "PriorityPolicy",
    "ipc_aware": "IpcAwarePolicy",
}

# ── Benchmark types ──────────────────────────────────────────────────────────

BENCH_TYPES = {
    "ipc": 0,
    "syscall": 1,
    "ctx_switch": 2,
    "mem": 3,
}

BENCH_TYPE_NAMES = {v: k for k, v in BENCH_TYPES.items()}

SCENARIO_NAMES = {
    "ipc": {0: "pingpong", 1: "fanout", 2: "chain"},
    "syscall": {0: "get_ticks", 1: "yield", 2: "mmap_munmap", 3: "get_time_ns", 4: "channel_lifecycle"},
    "ctx_switch": {0: "yield_pingpong", 1: "ipc_pingpong"},
    "mem": {0: "mmap_4k", 1: "mmap_64k", 2: "shared_buf", 3: "mprotect"},
}

# ── Suite configurations ─────────────────────────────────────────────────────

# IPC suite: (scenario, param_n, workers)
IPC_SUITE_CONFIGS = [
    (0, 0, 0),
    (0, 0, 1),
    (0, 0, 2),
    (0, 0, 4),
    (0, 0, 8),
    (1, 1, 0),
    (1, 2, 0),
    (1, 4, 0),
    (1, 8, 0),
    (1, 4, 4),
    (2, 2, 0),
    (2, 3, 0),
    (2, 4, 0),
    (2, 5, 0),
    (2, 3, 4),
]

# All-types suite: (bench_type_name, scenario, param_n, workers)
ALL_TYPES_CONFIGS = [
    ("syscall", 0, 0, 0),
    ("syscall", 1, 0, 0),
    ("syscall", 2, 0, 0),
    ("syscall", 3, 0, 0),
    ("syscall", 4, 0, 0),
    ("ctx_switch", 0, 0, 0),
    ("mem", 0, 0, 0),
    ("mem", 1, 0, 0),
    ("mem", 2, 0, 0),
    ("mem", 3, 0, 0),
    ("ipc", 0, 0, 0),
]


def config_label(bench_type: str, scenario: int, param_n: int, workers: int) -> str:
    """Generate a descriptive label for a benchmark configuration."""
    scenarios = SCENARIO_NAMES.get(bench_type, {})
    name = scenarios.get(scenario, f"s{scenario}")

    if bench_type == "ipc":
        if scenario == 0:
            return f"ipc_{name}_{workers}w"
        elif scenario == 1:
            return f"ipc_{name}_{param_n}c_{workers}w"
        elif scenario == 2:
            return f"ipc_{name}_{param_n}n_{workers}w"
        return f"ipc_{name}_p{param_n}_{workers}w"
    else:
        suffix = f"_{workers}w" if workers > 0 else ""
        return f"{bench_type}_{name}{suffix}"

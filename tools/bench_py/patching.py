"""Source-patching helpers that edit Rust constants before building."""

import re
import sys

from .config import BENCH_INIT_SRC, SCHEDULER_SRC, POLICY_MOD_SRC, POLICIES


def patch_bench_init(bench_type: int, scenario: int, param_n: int, workers: int):
    """Edit the constants in bench_init/src/main.rs."""
    src = BENCH_INIT_SRC.read_text()
    src = re.sub(r"const BENCH_TYPE: u64 = \d+;", f"const BENCH_TYPE: u64 = {bench_type};", src)
    src = re.sub(r"const SCENARIO: u64 = \d+;", f"const SCENARIO: u64 = {scenario};", src)
    src = re.sub(r"const PARAM_N: u64 = \d+;", f"const PARAM_N: u64 = {param_n};", src)
    src = re.sub(r"const BACKGROUND_WORKERS: u64 = \d+;", f"const BACKGROUND_WORKERS: u64 = {workers};", src)
    BENCH_INIT_SRC.write_text(src)


def patch_boost_budget(budget: int):
    """Set the IPC_BOOST_BUDGET constant in policy/mod.rs."""
    src = POLICY_MOD_SRC.read_text()
    src = re.sub(
        r"pub const IPC_BOOST_BUDGET: u8 = \d+;",
        f"pub const IPC_BOOST_BUDGET: u8 = {budget};",
        src,
    )
    POLICY_MOD_SRC.write_text(src)


def patch_scheduler_policy(policy_label: str):
    """Set the ActivePolicy type alias in local_scheduler.rs."""
    rust_type = POLICIES.get(policy_label)
    if not rust_type:
        print(f"Unknown policy '{policy_label}'. Valid: {list(POLICIES.keys())}")
        sys.exit(1)
    src = SCHEDULER_SRC.read_text()
    src = re.sub(
        r"pub type ActivePolicy = \w+;",
        f"pub type ActivePolicy = {rust_type};",
        src,
    )
    SCHEDULER_SRC.write_text(src)

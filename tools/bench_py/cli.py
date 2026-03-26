"""Command-line interface: argparse and all run modes."""

import argparse

from .config import (
    BENCH_TYPES, POLICIES, SCENARIO_NAMES, MEASURE_ROUNDS, RESULTS_DIR,
    IPC_SUITE_CONFIGS, ALL_TYPES_CONFIGS, config_label,
)
from .patching import patch_bench_init, patch_boost_budget, patch_scheduler_policy
from .runner import build_bench, kill_stale_qemu, run_single_bench
from .stats import compute_stats, print_stats
from .results import save_results, save_incremental
from .plotting import plot_single, plot_comparison


def run_scenario(bench_type: str, scenario: int, param_n: int, workers: int,
                 runs: int, policy_label: str, no_plot: bool,
                 skip_build: bool = False) -> dict | None:
    """Patch, build, run N times, save results. Returns stats or None."""
    cfg_label = config_label(bench_type, scenario, param_n, workers)
    full_label = f"{policy_label}_{cfg_label}"

    scenarios = SCENARIO_NAMES.get(bench_type, {})
    scenario_name = scenarios.get(scenario, f"s{scenario}")

    print(f"\n{'━'*60}")
    print(f"  Type: {bench_type}  Scenario: {scenario_name}  "
          f"param_n={param_n}  workers={workers}")
    print(f"  Label: {full_label}")
    print(f"{'━'*60}")

    bench_type_id = BENCH_TYPES[bench_type]
    patch_bench_init(bench_type_id, scenario, param_n, workers)

    if not skip_build:
        print("Building...")
        if not build_bench():
            return None

    raw_results: list[dict] = []
    failures = 0

    for i in range(runs):
        result = run_single_bench(i, runs)
        if result is not None:
            raw_results.append(result)
            save_incremental(full_label, raw_results, bench_type, scenario, param_n, workers)
        else:
            failures += 1
            kill_stale_qemu()

    if not raw_results:
        print(f"\n  All {runs} runs failed for {full_label}!")
        return None

    if failures > 0:
        print(f"\n[WARN] {failures}/{runs} runs failed and were excluded.")

    tick_values = [r["ELAPSED_TICKS"] for r in raw_results]
    stats = compute_stats(tick_values)
    print_stats(full_label, stats)

    jain_values = [r.get("jain_index") for r in raw_results if r.get("jain_index") is not None]
    if jain_values:
        mean_jain = sum(jain_values) / len(jain_values)
        print(f"  Jain fairness index (mean): {mean_jain:.4f}")

    save_results(full_label, raw_results, stats, bench_type, scenario, param_n, workers)

    partial_path = RESULTS_DIR / f"{full_label}_partial.json"
    if partial_path.exists():
        partial_path.unlink()

    if not no_plot:
        plot_single(full_label, tick_values, stats)

    return stats


def _cmd_sweep_budget(args):
    budget_values = [1, 2, 4, 8, 16]
    print(f"Running B-sensitivity sweep: B={budget_values}")
    print(f"Scenario: pingpong, 4 workers, policy: ipc_aware")

    patch_scheduler_policy("ipc_aware")

    all_stats = []
    for b in budget_values:
        label = f"ipc_aware_B{b}_pingpong_4w"
        print(f"\n\n{'▓'*60}")
        print(f"  BOOST BUDGET B = {b}")
        print(f"{'▓'*60}")

        patch_boost_budget(b)
        patch_bench_init(0, 0, 0, 4)

        print("Building...")
        if not build_bench():
            continue

        raw_results: list[dict] = []
        for i in range(args.runs):
            result = run_single_bench(i, args.runs)
            if result is not None:
                raw_results.append(result)
                save_incremental(label, raw_results, "ipc", 0, 0, 4)
            else:
                kill_stale_qemu()

        if raw_results:
            tick_values = [r["ELAPSED_TICKS"] for r in raw_results]
            stats = compute_stats(tick_values)
            jain_values = [r.get("jain_index") for r in raw_results if r.get("jain_index") is not None]
            mean_jain = sum(jain_values) / len(jain_values) if jain_values else 0
            print_stats(label, stats)
            print(f"  Jain fairness index (mean): {mean_jain:.4f}")
            save_results(label, raw_results, stats, "ipc", 0, 0, 4)
            all_stats.append((b, stats, mean_jain))
            partial_path = RESULTS_DIR / f"{label}_partial.json"
            if partial_path.exists():
                partial_path.unlink()

    print(f"\n\n{'═'*70}")
    print(f"  B-SENSITIVITY SWEEP SUMMARY (pingpong, 4 workers)")
    print(f"{'═'*70}")
    print(f"  {'B':>4} {'Mean ticks':>12} {'ticks/op':>10} {'Jain':>8}")
    print(f"{'─'*70}")
    for b, s, j in all_stats:
        tpr = s["mean"] / MEASURE_ROUNDS
        print(f"  {b:>4} {s['mean']:>12.0f} {tpr:>10.2f} {j:>8.4f}")
    print(f"{'═'*70}")

    patch_bench_init(0, 0, 0, 0)
    patch_scheduler_policy("priority")
    patch_boost_budget(4)


def _cmd_all_types(args):
    policy = args.policy or "priority"
    label = args.label or policy

    print(f"Running ALL benchmark types ({len(ALL_TYPES_CONFIGS)} configs × {args.runs} runs)")
    print(f"Policy: {policy}")

    patch_scheduler_policy(policy)

    all_stats = []
    for bench_type_name, scenario, param_n, workers in ALL_TYPES_CONFIGS:
        stats = run_scenario(bench_type_name, scenario, param_n, workers,
                             args.runs, label, args.no_plot)
        if stats:
            cfg = config_label(bench_type_name, scenario, param_n, workers)
            all_stats.append((cfg, bench_type_name, stats))

    print(f"\n\n{'═'*80}")
    print(f"  ALL-TYPES SUMMARY — {label}")
    print(f"{'═'*80}")
    print(f"  {'Config':<30} {'Type':<12} {'Mean ticks':>12} {'ticks/op':>10} {'CV%':>7}")
    print(f"{'─'*80}")
    for cfg, bt, s in all_stats:
        tpr = s["mean"] / MEASURE_ROUNDS
        print(f"  {cfg:<30} {bt:<12} {s['mean']:>12.0f} {tpr:>10.2f} {s['cv_percent']:>6.2f}%")
    print(f"{'═'*80}")

    patch_bench_init(0, 0, 0, 0)
    patch_scheduler_policy("priority")


def _cmd_full_suite(args):
    print(f"Running FULL IPC benchmark suite: {len(POLICIES)} policies × "
          f"{len(IPC_SUITE_CONFIGS)} configs × {args.runs} runs")

    for policy_name in POLICIES:
        print(f"\n\n{'▓'*60}")
        print(f"  POLICY: {policy_name} ({POLICIES[policy_name]})")
        print(f"{'▓'*60}")

        patch_scheduler_policy(policy_name)

        all_stats = []
        for scenario, param_n, workers in IPC_SUITE_CONFIGS:
            stats = run_scenario("ipc", scenario, param_n, workers, args.runs,
                                 policy_name, args.no_plot)
            if stats:
                cfg = config_label("ipc", scenario, param_n, workers)
                all_stats.append((cfg, stats))

        _print_suite_summary(policy_name, all_stats)

    patch_bench_init(0, 0, 0, 0)
    patch_scheduler_policy("priority")


def _cmd_suite(args):
    policy = args.policy or "priority"
    label = args.label or policy

    print(f"Running IPC benchmark suite ({len(IPC_SUITE_CONFIGS)} configs × {args.runs} runs)")
    print(f"Policy: {policy} ({POLICIES[policy]})")

    patch_scheduler_policy(policy)

    all_stats = []
    for scenario, param_n, workers in IPC_SUITE_CONFIGS:
        stats = run_scenario("ipc", scenario, param_n, workers, args.runs,
                             label, args.no_plot)
        if stats:
            cfg = config_label("ipc", scenario, param_n, workers)
            all_stats.append((cfg, stats))

    _print_suite_summary(label, all_stats)

    patch_bench_init(0, 0, 0, 0)
    patch_scheduler_policy("priority")


def _cmd_single(args):
    policy = args.policy or "priority"
    label = args.label or policy
    bench_type = args.bench_type
    scenario = args.scenario if args.scenario is not None else 0
    param_n = args.param_n
    workers = args.workers

    scenarios = SCENARIO_NAMES.get(bench_type, {})
    scenario_name = scenarios.get(scenario, f"s{scenario}")

    print(f"Running {bench_type} benchmark {args.runs} times (policy: {label})")
    print(f"Scenario: {scenario_name} (param_n={param_n}, workers={workers})")

    patch_scheduler_policy(policy)
    run_scenario(bench_type, scenario, param_n, workers, args.runs, label, args.no_plot)

    patch_bench_init(0, 0, 0, 0)
    patch_scheduler_policy("priority")


def _print_suite_summary(label: str, all_stats: list[tuple]):
    print(f"\n\n{'═'*75}")
    print(f"  SUITE SUMMARY — {label}")
    print(f"{'═'*75}")
    print(f"  {'Config':<25} {'Mean ticks':>12} {'ticks/op':>10} {'CV%':>7} {'n':>4}")
    print(f"{'─'*75}")
    for cfg, s in all_stats:
        tpr = s["mean"] / MEASURE_ROUNDS
        print(f"  {cfg:<25} {s['mean']:>12.0f} {tpr:>10.2f} {s['cv_percent']:>6.2f}% {s['n']:>4}")
    print(f"{'═'*75}")


def main():
    parser = argparse.ArgumentParser(description="Bos OS Benchmark Runner & Visualizer")
    parser.add_argument("--runs", type=int, default=3,
                        help="Number of benchmark runs per config (default: 3)")
    parser.add_argument("--bench-type", type=str, default="ipc",
                        choices=list(BENCH_TYPES.keys()),
                        help="Benchmark type (default: ipc)")
    parser.add_argument("--policy", type=str, default=None,
                        choices=list(POLICIES.keys()),
                        help="Scheduling policy to use (edits kernel source)")
    parser.add_argument("--label", type=str, default=None,
                        help="Label override (default: same as --policy)")
    parser.add_argument("--scenario", type=int, default=None,
                        help="Scenario number (meaning depends on --bench-type)")
    parser.add_argument("--param-n", type=int, default=0,
                        help="Scenario parameter (fanout: clients, chain: nodes)")
    parser.add_argument("--workers", type=int, default=0,
                        help="Number of background workers")
    parser.add_argument("--suite", action="store_true",
                        help="Run the full IPC benchmark suite (all scenarios)")
    parser.add_argument("--all-types", action="store_true",
                        help="Run all benchmark types with default scenarios")
    parser.add_argument("--full-suite", action="store_true",
                        help="Run the full IPC suite for ALL three policies")
    parser.add_argument("--compare", nargs="+", metavar="FILE",
                        help="Compare mode: pass 2+ result JSON files")
    parser.add_argument("--sweep-budget", action="store_true",
                        help="Run B-sensitivity sweep: pingpong+4w with B=1,2,4,8,16")
    parser.add_argument("--no-plot", action="store_true",
                        help="Skip plot generation")
    args = parser.parse_args()

    if args.compare:
        plot_comparison(args.compare)
        return

    kill_stale_qemu()

    if args.sweep_budget:
        _cmd_sweep_budget(args)
    elif args.all_types:
        _cmd_all_types(args)
    elif args.full_suite:
        _cmd_full_suite(args)
    elif args.suite:
        _cmd_suite(args)
    else:
        _cmd_single(args)

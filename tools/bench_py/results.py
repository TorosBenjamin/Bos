"""Saving benchmark results to JSON."""

import json
import time

from .config import RESULTS_DIR, MEASURE_ROUNDS, SCENARIO_NAMES
from .stats import compute_stats


def save_results(label: str, raw_results: list[dict], stats: dict,
                 bench_type: str = "ipc", scenario: int = 0,
                 param_n: int = 0, workers: int = 0):
    """Save final results (raw data + stats) to a JSON file."""
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    out_path = RESULTS_DIR / f"{label}.json"

    tick_values = [r["ELAPSED_TICKS"] for r in raw_results]
    fairness_data = _collect_fairness(raw_results)

    jain_values = [r.get("jain_index") for r in raw_results if r.get("jain_index") is not None]
    jain_stats = {}
    if jain_values:
        jain_stats = {
            "mean": sum(jain_values) / len(jain_values),
            "min": min(jain_values),
            "max": max(jain_values),
        }

    scenarios = SCENARIO_NAMES.get(bench_type, {})

    payload = {
        "label": label,
        "timestamp": time.strftime("%Y-%m-%d %H:%M:%S"),
        "measure_rounds": MEASURE_ROUNDS,
        "warmup_rounds": 500,
        "bench_type": bench_type,
        "scenario": scenario,
        "scenario_name": scenarios.get(scenario, f"s{scenario}"),
        "param_n": param_n,
        "background_workers": workers,
        "elapsed_ticks": tick_values,
        "stats": stats,
        "fairness": fairness_data,
        "jain_stats": jain_stats,
    }
    with open(out_path, "w") as f:
        json.dump(payload, f, indent=2)
    print(f"\nResults saved to {out_path}")


def save_incremental(label: str, raw_results: list[dict],
                     bench_type: str = "ipc", scenario: int = 0,
                     param_n: int = 0, workers: int = 0):
    """Save partial results after each run so crashes don't lose data."""
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    out_path = RESULTS_DIR / f"{label}_partial.json"

    tick_values = [r["ELAPSED_TICKS"] for r in raw_results]
    stats = compute_stats(tick_values) if tick_values else {}

    payload = {
        "label": label,
        "timestamp": time.strftime("%Y-%m-%d %H:%M:%S"),
        "measure_rounds": MEASURE_ROUNDS,
        "bench_type": bench_type,
        "scenario": scenario,
        "param_n": param_n,
        "background_workers": workers,
        "elapsed_ticks": tick_values,
        "stats": stats,
        "fairness": _collect_fairness(raw_results),
        "partial": True,
        "completed_runs": len(raw_results),
    }
    with open(out_path, "w") as f:
        json.dump(payload, f, indent=2)


def _collect_fairness(raw_results: list[dict]) -> list[dict]:
    fairness_data = []
    for r in raw_results:
        cpu_ticks = r.get("cpu_ticks", {})
        jain = r.get("jain_index", None)
        fairness_data.append({
            "cpu_ticks": {str(k): v for k, v in cpu_ticks.items()},
            "jain_index": jain,
        })
    return fairness_data

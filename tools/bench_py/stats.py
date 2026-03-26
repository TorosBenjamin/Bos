"""Statistics and fairness index computation."""

import math

from .config import MEASURE_ROUNDS


def compute_jain_index(values: list[float]) -> float:
    """Jain's fairness index: (sum(x))^2 / (n * sum(x^2)). 1.0 = perfect."""
    n = len(values)
    if n == 0:
        return 0.0
    s = sum(values)
    ss = sum(x * x for x in values)
    if ss == 0:
        return 1.0
    return (s * s) / (n * ss)


def compute_stats(data: list[int]) -> dict:
    """Compute summary statistics for a list of tick values."""
    n = len(data)
    if n == 0:
        return {}

    sorted_data = sorted(data)
    mean = sum(data) / n
    variance = sum((x - mean) ** 2 for x in data) / n
    stddev = math.sqrt(variance)

    median = (
        sorted_data[n // 2]
        if n % 2 == 1
        else (sorted_data[n // 2 - 1] + sorted_data[n // 2]) / 2
    )

    p5 = sorted_data[max(0, int(n * 0.05))]
    p95 = sorted_data[min(n - 1, int(n * 0.95))]
    cv = (stddev / mean * 100) if mean > 0 else 0

    return {
        "n": n,
        "mean": mean,
        "median": median,
        "stddev": stddev,
        "cv_percent": round(cv, 2),
        "min": sorted_data[0],
        "max": sorted_data[-1],
        "p5": p5,
        "p95": p95,
    }


def print_stats(label: str, stats: dict):
    """Pretty-print statistics to stdout."""
    mean_per_op = stats["mean"] / MEASURE_ROUNDS
    print(f"\n{'═'*55}")
    print(f"  {label}  (n={stats['n']})")
    print(f"{'═'*55}")
    print(f"  Mean:     {stats['mean']:.1f} ticks / {MEASURE_ROUNDS} ops")
    print(f"  Median:   {stats['median']:.1f} ticks")
    print(f"  Std Dev:  {stats['stddev']:.1f} ticks")
    print(f"  CV:       {stats['cv_percent']:.2f}%")
    print(f"  Min:      {stats['min']} ticks")
    print(f"  Max:      {stats['max']} ticks")
    print(f"  P5:       {stats['p5']} ticks")
    print(f"  P95:      {stats['p95']} ticks")
    print(f"  ──────────────────────────────────────")
    print(f"  Mean ticks/op: {mean_per_op:.4f}")
    print(f"{'═'*55}")

"""Matplotlib-based plotting for benchmark results."""

import json

from .config import RESULTS_DIR, MEASURE_ROUNDS


def plot_single(label: str, data: list[int], stats: dict):
    """Generate a histogram + box plot for a single configuration."""
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        print("\n[WARN] matplotlib not installed — skipping plot.")
        return

    RESULTS_DIR.mkdir(parents=True, exist_ok=True)

    fig, (ax_hist, ax_box) = plt.subplots(1, 2, figsize=(12, 5), width_ratios=[3, 1])
    fig.suptitle(f"Benchmark: {label}", fontsize=14, fontweight="bold")

    ax_hist.hist(data, bins=20, color="#4C72B0", edgecolor="white", alpha=0.85)
    ax_hist.axvline(stats["mean"], color="#C44E52", linestyle="--", linewidth=1.5,
                    label=f"Mean = {stats['mean']:.0f}")
    ax_hist.axvline(stats["median"], color="#55A868", linestyle="-.", linewidth=1.5,
                    label=f"Median = {stats['median']:.0f}")
    ax_hist.set_xlabel(f"Total ticks for {MEASURE_ROUNDS} ops")
    ax_hist.set_ylabel("Frequency (across runs)")
    ax_hist.legend(fontsize=9)
    ax_hist.set_title("Distribution")

    ax_box.boxplot(data, vert=True, widths=0.5, patch_artist=True,
                   boxprops=dict(facecolor="#4C72B0", alpha=0.7),
                   medianprops=dict(color="#C44E52", linewidth=2))
    ax_box.set_ylabel(f"Ticks / {MEASURE_ROUNDS} ops")
    ax_box.set_title("Spread")
    ax_box.set_xticklabels([label])

    plt.tight_layout()
    out_path = RESULTS_DIR / f"{label}.png"
    fig.savefig(out_path, dpi=150)
    plt.close(fig)
    print(f"Plot saved to {out_path}")


def plot_comparison(files: list[str]):
    """Generate comparison charts from multiple saved result files."""
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        print("\n[WARN] matplotlib not installed — skipping comparison plot.")
        return

    datasets = []
    for fpath in files:
        with open(fpath) as f:
            datasets.append(json.load(f))

    if len(datasets) < 2:
        print("Need at least 2 result files for comparison.")
        return

    RESULTS_DIR.mkdir(parents=True, exist_ok=True)

    fig, axes = plt.subplots(1, 3, figsize=(16, 5.5))
    fig.suptitle(f"Benchmark Comparison — Ticks per {MEASURE_ROUNDS} ops",
                 fontsize=14, fontweight="bold")

    labels = [d["label"] for d in datasets]
    colors = ["#4C72B0", "#55A868", "#C44E52", "#8172B2", "#CCB974"]

    ax = axes[0]
    all_data = [d["elapsed_ticks"] for d in datasets]
    bp = ax.boxplot(all_data, vert=True, patch_artist=True, labels=labels,
                    medianprops=dict(color="black", linewidth=2))
    for patch, color in zip(bp["boxes"], colors):
        patch.set_facecolor(color)
        patch.set_alpha(0.7)
    ax.set_ylabel(f"Ticks / {MEASURE_ROUNDS} ops")
    ax.set_title("Distribution Comparison")

    ax = axes[1]
    means = [d["stats"]["mean"] for d in datasets]
    stddevs = [d["stats"]["stddev"] for d in datasets]
    bars = ax.bar(labels, means, yerr=stddevs, capsize=5,
                  color=colors[:len(labels)], alpha=0.8, edgecolor="white")
    ax.set_ylabel(f"Mean ticks / {MEASURE_ROUNDS} ops")
    ax.set_title("Mean +/- Std Dev")
    for bar, mean in zip(bars, means):
        ax.text(bar.get_x() + bar.get_width() / 2, bar.get_height() + 5,
                f"{mean:.0f}", ha="center", va="bottom", fontsize=10, fontweight="bold")

    ax = axes[2]
    for i, d in enumerate(datasets):
        ax.hist(d["elapsed_ticks"], bins=20, alpha=0.5, label=d["label"],
                color=colors[i % len(colors)], edgecolor="white")
    ax.set_xlabel(f"Ticks / {MEASURE_ROUNDS} ops")
    ax.set_ylabel("Frequency")
    ax.set_title("Overlaid Distributions")
    ax.legend(fontsize=9)

    plt.tight_layout()
    out_path = RESULTS_DIR / "comparison.png"
    fig.savefig(out_path, dpi=150)
    plt.close(fig)
    print(f"\nComparison plot saved to {out_path}")

    # Print comparison table
    print(f"\n{'═'*80}")
    print(f"  {'Label':<25} {'Type':<12} {'Mean ticks':>12} {'Median':>12} {'StdDev':>10} {'CV%':>7}")
    print(f"{'─'*80}")
    for d in datasets:
        s = d["stats"]
        bt = d.get("bench_type", "ipc")
        print(f"  {d['label']:<25} {bt:<12} {s['mean']:>12.0f} {s['median']:>12.0f} "
              f"{s['stddev']:>10.0f} {s['cv_percent']:>6.2f}%")
    print(f"{'═'*80}")

    print(f"\n  Ticks per op:")
    for d in datasets:
        tpr = d["stats"]["mean"] / MEASURE_ROUNDS
        print(f"    {d['label']:<25} {tpr:.4f}")

"""Build comparisons and a standalone research figure from exported results."""
import csv
import json
import os
from pathlib import Path

os.environ.setdefault("MPLCONFIGDIR", "/private/tmp/dusk-tranche-matplotlib")
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.ticker import PercentFormatter, FuncFormatter

ROOT = Path(__file__).resolve().parent
OUT = ROOT / "results"
POLICIES = ["reserve_capped", "reserve_uncapped", "junior_fixed", "junior_share", "junior_debt"]
LABELS = {
    "reserve_capped": "Cash reserve · capped",
    "reserve_uncapped": "Cash reserve · uncapped",
    "junior_fixed": "Junior · fixed APR",
    "junior_share": "Junior · 20% participation",
    "junior_debt": "Junior · coverage / debt",
}
COLORS = dict(zip(POLICIES, ["#67758A", "#A8B0BB", "#BA7337", "#2968AE", "#89700D"]))


def read(path):
    with path.open() as f:
        return list(csv.DictReader(f))


def main():
    rows = read(OUT / "summary.csv")
    lookup = {(r["variant"], r["scenario"], r["policy"]): r for r in rows}
    base = {(s, p): r for (v, s, p), r in lookup.items() if v == "base"}
    scenarios = list(dict.fromkeys(r["scenario"] for r in rows))
    variants = list(dict.fromkeys(r["variant"] for r in rows))
    pairs = []
    counts = {}
    for s in scenarios:
        counts[s] = {"lower_senior_loss": 0, "equal_senior_loss": 0, "higher_senior_loss": 0,
                     "lower_average_debt": 0, "settings": len(variants)}
        for v in variants:
            a, b = lookup[v, s, "junior_debt"], lookup[v, s, "junior_share"]
            loss = float(a["senior_credit_loss"]) - float(b["senior_credit_loss"])
            debt = float(a["debt_per_initial_capital"]) - float(b["debt_per_initial_capital"])
            pairs.append({"variant": v, "scenario": s, "debt_target_minus_share_target_senior_loss": loss,
                          "debt_target_minus_share_target_average_debt_per_capital": debt})
            counts[s]["equal_senior_loss" if abs(loss) < 1e-5 else
                      "lower_senior_loss" if loss < 0 else "higher_senior_loss"] += 1
            counts[s]["lower_average_debt"] += int(debt < -1e-8)
    with (OUT / "paired_comparisons.csv").open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(pairs[0]))
        w.writeheader()
        w.writerows(pairs)
    convergence = {}
    for field in ("senior_credit_loss", "debt_per_initial_capital", "borrower_paid_apr"):
        diffs = [(abs(float(r[field]) - float(base[r["scenario"], r["policy"]][field])),
                  r["scenario"], r["policy"], float(r["step_days"]))
                 for r in read(OUT / "step_sensitivity.csv")]
        maximum = max(diffs)
        convergence[field] = dict(zip(("max_absolute_difference", "scenario", "policy", "step_days"), maximum))
    findings = {"comparisons": counts, "step_size_check": convergence,
                "interpretation": "Counts describe this assumption grid; they are not probabilities or statistical confidence."}
    (OUT / "findings.json").write_text(json.dumps(findings, indent=2) + "\n")

    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 11,
                         "text.parse_math": False,
                         "axes.titlesize": 13, "axes.titleweight": "bold",
                         "axes.spines.top": False, "axes.spines.right": False,
                         "axes.edgecolor": "#BDC3CC", "text.color": "#202A36",
                         "axes.labelcolor": "#344054", "xtick.color": "#475467",
                         "ytick.color": "#475467", "savefig.facecolor": "white"})
    fig, axes = plt.subplots(2, 2, figsize=(13.5, 9))
    fig.subplots_adjust(left=.17, right=.97, top=.84, bottom=.17, hspace=.58, wspace=.55)
    fig.suptitle("Productive insurance capital: capacity and protection move differently",
                 x=.075, y=.98, ha="left", fontsize=18, fontweight="bold")
    fig.text(.075, .935, "Synthetic mechanism study · $10m total starting capital · $5.6m starting debt · unchanged liquidation rule",
             fontsize=11, color="#536172")
    ax = axes[0, 0]
    for policy in ("reserve_capped", "junior_share", "junior_debt"):
        trace = read(OUT / "traces" / f"demand_growth__{policy}.csv")
        ax.plot([float(x["day"]) for x in trace], [float(x["debt"]) / 1e6 for x in trace],
                color=COLORS[policy], lw=2.5, label=LABELS[policy],
                linestyle="--" if policy == "reserve_capped" else "-")
    ax.axvline(20, color="#9AA3AF", lw=1, linestyle=":")
    ax.set(title="A. Borrowing when demand grows", xlabel="Day", ylabel="Outstanding debt ($m)",
           xlim=(0, 90), ylim=(0, 10.8))
    ax.set_xticks([0, 20, 40, 60, 90])
    ax.grid(axis="y", alpha=.17)
    handles, labels = ax.get_legend_handles_labels()
    fig.legend(handles, labels, loc="upper left", bbox_to_anchor=(.065, .905), ncol=3, frameon=False, fontsize=10)

    ax = axes[0, 1]
    for policy in ("junior_share", "junior_debt"):
        trace = read(OUT / "traces" / f"demand_growth__{policy}.csv")
        ax.plot([float(x["day"]) for x in trace],
                [float(x["committed_junior"]) / float(x["debt"]) for x in trace],
                color=COLORS[policy], lw=2.5)
    ax.axhline(2 / 5.6, color="#344054", lw=1, linestyle="--")
    ax.text(1, 2 / 5.6 + .012, "Initial coverage: 35.7% of debt", fontsize=10)
    ax.set(title="B. Committed junior capital / debt", xlabel="Day", ylabel="Coverage",
           xlim=(0, 90), ylim=(0, .43))
    ax.set_xticks([0, 20, 40, 60, 90])
    ax.yaxis.set_major_formatter(PercentFormatter(1))
    ax.grid(axis="y", alpha=.17)

    for ax, scenario, title in ((axes[1, 0], "joint_severe", "C. Senior bad debt: unexpected severe crash"),
                                (axes[1, 1], "growth_then_crash", "D. Senior bad debt: growth followed by crash")):
        values = [float(base[scenario, p]["senior_credit_loss"]) / 1e6 for p in POLICIES]
        ax.barh(range(5), values, color=[COLORS[p] for p in POLICIES], height=.6)
        ax.set_yticks(range(5), ["Reserve, capped", "Reserve, uncapped", "Junior, fixed APR",
                               "20% participation", "Coverage / debt"], fontsize=10)
        ax.invert_yaxis()
        ax.set(title=title, xlabel="Loss borne by senior / ordinary LPs ($m)", xlim=(0, 1.38))
        ax.xaxis.set_major_formatter(FuncFormatter(lambda value, _: f"{value:.1f}"))
        for i, v in enumerate(values):
            ax.text(v + .025, i, f"${v * 1e3:,.0f}k", va="center", fontsize=10)
        ax.grid(axis="x", alpha=.17)
        ax.set_axisbelow(True)
    fig.text(.075, .065, "Loss bars exclude ordinary inventory losses. The uncapped reserve isolates the cost of draw limits; it is not current policy.",
             fontsize=10, color="#536172")
    fig.text(.075, .038, "Behavior and shocks are assumptions, not historical evidence. Junior entry reallocates existing LP capital; no outside capital is added.",
             fontsize=10, color="#536172")
    fig.savefig(OUT / "capital-and-coverage.png", dpi=160)
    plt.close(fig)
    print(json.dumps(findings, indent=2))


if __name__ == "__main__":
    main()

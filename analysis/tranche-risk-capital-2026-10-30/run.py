"""Run all declared comparisons. Standard-library-only numerical simulation."""
import csv
import hashlib
import json
import platform
import subprocess
from pathlib import Path

from model import POLICIES, SCENARIOS, simulate

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]

# Predeclared one-at-a-time sensitivities and adverse interaction corners.
VARIANTS = {
    "base": {},
    "demand_inelastic": {"borrower_elasticity": 0.0},
    "demand_low_elasticity": {"borrower_elasticity": 0.5},
    "demand_high_elasticity": {"borrower_elasticity": 2.0},
    "allocation_frozen": {"lp_response_days": 0.0},
    "allocation_fast": {"lp_response_days": 2.0},
    "allocation_slow": {"lp_response_days": 30.0},
    "junior_share_35pct": {"junior_interest_share": 0.35},
    "junior_share_65pct": {"junior_interest_share": 0.65},
    "exit_immediate": {"exit_notice_days": 0.0},
    "exit_30days": {"exit_notice_days": 30.0},
    "controller_fast": {"controller_half_life_days": 2.0},
    "controller_slow": {"controller_half_life_days": 30.0},
    "no_inventory_drawdown": {"inventory_loss_multiplier": 0.0},
    "double_inventory_drawdown": {"inventory_loss_multiplier": 2.0},
    "repayment_slow": {"borrower_response_days": 30.0},
    "run_adverse_corner": {"exit_notice_days": 0.0, "borrower_elasticity": 0.0,
                           "lp_response_days": 30.0, "inventory_loss_multiplier": 2.0},
    "slow_capital_elastic_borrowers": {"lp_response_days": 30.0,
                                       "borrower_elasticity": 2.0,
                                       "controller_half_life_days": 2.0},
}


def csv_write(path, rows):
    if not rows:
        return
    with path.open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def main():
    params = json.loads((ROOT / "config.json").read_text())
    out = ROOT / "results"
    (out / "traces").mkdir(parents=True, exist_ok=True)
    summaries, events, convergence = [], [], []
    for variant, overrides in VARIANTS.items():
        p = {**params, **overrides}
        for scenario in SCENARIOS:
            for policy in POLICIES:
                summary, trace, event_rows = simulate(policy, scenario, p)
                summaries.append({"variant": variant, **summary})
                events.extend({"variant": variant, "scenario": scenario.name,
                               "policy": policy, **event} for event in event_rows)
                if variant == "base":
                    csv_write(out / "traces" / f"{scenario.name}__{policy}.csv", trace)
    for step in (0.125, 1 / 24):
        for scenario in SCENARIOS:
            for policy in POLICIES:
                summary, _, _ = simulate(policy, scenario, {**params, "step_days": step})
                convergence.append({"step_days": step, **summary})
    csv_write(out / "summary.csv", summaries)
    csv_write(out / "events.csv", events)
    csv_write(out / "step_sensitivity.csv", convergence)
    (out / "variants.json").write_text(json.dumps(VARIANTS, indent=2) + "\n")
    source_paths = [
        REPO / "programs/dusk/src/constants.rs",
        REPO / "programs/dusk/src/state/market.rs",
        REPO / "programs/dusk/src/math/risk.rs",
        REPO / "programs/dusk/src/transitions/ledger.rs",
        REPO / "programs/dusk/src/transitions/lending/mod.rs",
        REPO / "programs/dusk/src/transitions/lending/liquidation.rs",
        REPO.parent / "omnipair-rs/programs/omnipair/src/instructions/lending/liquidate.rs",
    ]
    authored = sorted(ROOT.glob("*.py")) + [ROOT / "PREANALYSIS.md", ROOT / "config.json"]
    manifest = {
        "kind": "synthetic mechanism experiment; no historical observations",
        "python": platform.python_version(),
        "repo_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
        "source_warning": "Source hashes describe the working tree; HEAD alone is insufficient.",
        "parameters": params, "grid_runs": len(summaries), "step_runs": len(convergence),
        "base_trace_files": len(SCENARIOS) * len(POLICIES),
        "randomness": "None; no event probabilities or confidence intervals.",
        "hashes": {str(path.relative_to(REPO.parent)): hashlib.sha256(path.read_bytes()).hexdigest()
                   for path in source_paths + authored},
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({k: manifest[k] for k in ("grid_runs", "step_runs", "base_trace_files")}))
    for row in summaries:
        if row["variant"] == "base":
            print(f'{row["scenario"]:18} {row["policy"]:18} '
                  f'debt/G={row["debt_per_initial_capital"]:.3f} '
                  f'APR={row["borrower_paid_apr"]:.3%} '
                  f'senior credit loss=${row["senior_credit_loss"]:,.0f} '
                  f'total P&L=${row["total_capital_pnl"]:,.0f}')


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Build the dependency-free reproducible notebook for the risk-pricing study."""

from __future__ import annotations

import json
from pathlib import Path


HERE = Path(__file__).resolve().parent


def markdown(source: str) -> dict[str, object]:
    return {"cell_type": "markdown", "metadata": {}, "source": source}


def code(source: str) -> dict[str, object]:
    return {
        "cell_type": "code",
        "execution_count": None,
        "metadata": {},
        "outputs": [],
        "source": source,
    }


def build() -> dict[str, object]:
    cells = [
        markdown(
            "# Dusk risk pricing: SOL collateral / USDC debt\n\n"
            "This notebook reproduces the illustrative 7-day expected-loss study. It keeps "
            "Dusk's utilization IRM out of the calculation so the output is a pure risk-price "
            "add-on. SOLUSDT is used as a return proxy for SOL/USDC."
        ),
        markdown(
            "## Method and definitions\n\n"
            "The 15 sealed, stratified 12-hour score windows are bootstrapped into 50,000 "
            "seven-day paths under three explicit regime mixtures. Each path is revalued at "
            "100%, 75%, and 50% liquidation depth using Dusk's default CPMM full-collateral "
            "exact-in value. Gross pool shortfall is priced as expected loss plus a 15% annual "
            "return on ES99 minus expected loss. Regime weights are policy scenarios, not "
            "estimated real-world frequencies."
        ),
        code(
            "from pathlib import Path\n"
            "import csv\n"
            "import json\n"
            "from risk_pricing import DEFAULT_HISTORY, MarketConfig, run_analysis\n\n"
            "NOTEBOOK_DIR = Path.cwd()\n"
            "RESULTS_DIR = NOTEBOOK_DIR / 'results'\n"
            "CONFIG = MarketConfig(path_count=50_000, seed=42)\n"
            "summary = run_analysis(DEFAULT_HISTORY, RESULTS_DIR, CONFIG)\n"
            "print('analysis status:', summary['status'])\n"
            "print('history checksum:', summary['history_diagnostics']['sha256'])"
        ),
        markdown("## Central illustrative result"),
        code(
            "central = summary['central_scenario']\n"
            "print(json.dumps({\n"
            "    'pair_risk_apr_bps': central['pair_risk_apr_bps'],\n"
            "    'expected_loss_bps_of_ead_7d': central['expected_loss_bps_of_ead'],\n"
            "    'probability_any_liquidation': central['probability_any_liquidation'],\n"
            "    'probability_lender_shortfall': central['probability_lender_shortfall'],\n"
            "    'marginal_high_ltv_fee_bps': central['marginal_high_ltv_fee_bps'],\n"
            "}, indent=2))"
        ),
        markdown("## Position allocation and cross-subsidy"),
        code(
            "for row in summary['central_position_pricing']:\n"
            "    print(\n"
            "        f\"{row['position']:8s} | LTV {row['impact_ltv_pct']:4.0f}% | \"\n"
            "        f\"liq {100 * row['liquidation_probability']:5.1f}% | \"\n"
            "        f\"risk APR {100 * row['position_risk_apr']:6.2f}% | \"\n"
            "        f\"pair-minus-position {row['cross_subsidy_bps']:8.1f} bps\"\n"
            "    )"
        ),
        markdown("## Scenario grid"),
        code(
            "with (RESULTS_DIR / 'scenario_results.csv').open() as handle:\n"
            "    scenario_rows = list(csv.DictReader(handle))\n"
            "for row in scenario_rows:\n"
            "    print(\n"
            "        f\"{row['regime_mix']:18s} | depth {row['depth_label']:>4s} | \"\n"
            "        f\"EL {float(row['expected_loss_bps_of_ead']):6.1f} bps/7d | \"\n"
            "        f\"risk APR {float(row['pair_risk_apr_bps']) / 100:6.2f}%\"\n"
            "    )"
        ),
        markdown("## Validation"),
        code(
            "validation = summary['validation']\n"
            "assert validation['history_checksum_matches']\n"
            "assert validation['history_has_15_segments']\n"
            "assert validation['risk_apr_monotonic_with_depth_loss']\n"
            "assert validation['risk_apr_monotonic_with_stress_weight']\n"
            "assert validation['shapley_revenue_identity_pass']\n"
            "assert validation['central_repeat_within_3pct_relative']\n"
            "assert validation['all_losses_bounded_by_ead']\n"
            "assert validation['all_checks_pass']\n"
            "print(json.dumps(validation, indent=2))"
        ),
        markdown(
            "## Interpretation boundary\n\n"
            "The modeled shortfall is before any pre-funded USDC insurance draw. Dusk's auction "
            "transfers SOL to an external liquidator rather than swapping it through the internal "
            "AMM, so the joint CPMM unwind is a conservative shared-resale-depth proxy. Production "
            "calibration needs keeper fills, external venue depth, position repayment duration, "
            "and the funded insurance balance."
        ),
    ]
    return {
        "cells": cells,
        "metadata": {
            "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
            "language_info": {"name": "python", "version": "3.9"},
        },
        "nbformat": 4,
        "nbformat_minor": 5,
    }


def main() -> None:
    output = HERE / "dusk_risk_pricing.ipynb"
    output.write_text(json.dumps(build(), indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()

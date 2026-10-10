"""Quick target sensitivity using the selected 80-bps insurance/LP allocation.

Run: PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/insurance_target_analysis.py
Prints JSON; --write also saves the small Markdown report alongside this script.
This is deterministic sensitivity and descriptive stress analysis, not a forecast.
"""

import csv
import hashlib
import json
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from statistics import median

from insurance_policy_analysis import BPS, PRINCIPAL, SEED, UNIT, insurance_allocation

DIRECTORY = Path(__file__).resolve().parent


def funding(target_bps, turnover_bps, knee_bps):
    target = PRINCIPAL * target_bps // BPS
    monthly_base = PRINCIPAL * turnover_bps // BPS
    monthly_fee = monthly_base * 80 // BPS
    balance, lp, collected, day95 = SEED, 0, 0, None
    for day in range(1, 36_001):
        fee = monthly_fee * day // 30 - monthly_fee * (day - 1) // 30
        credit, remainder = insurance_allocation(fee, balance, target, knee_bps)
        balance += credit
        lp += remainder
        collected += fee
        assert balance + lp == SEED + collected
        assert SEED <= balance <= target
        if day == 360:
            fund12, lp12 = balance, lp
        if day95 is None and balance * 100 >= target * 95:
            day95 = day
        if day >= 360 and day95 is not None:
            break
    assert day95 is not None
    return dict(target_pct=target_bps / 100, monthly_repayment_pct=turnover_bps / 100,
                taper_knee_pct=knee_bps / 100, months95=day95 / 30,
                fund12=fund12 / UNIT, lp12=lp12 / UNIT,
                monthly_designated_fee=monthly_fee / UNIT)


def percentile(values, q):
    """Linear interpolation, descriptive only; fixtures are not random samples."""
    values = sorted(values)
    index = (len(values) - 1) * q
    lower = int(index)
    upper = min(lower + 1, len(values) - 1)
    return values[lower] + (values[upper] - values[lower]) * (index - lower)


def analyze():
    filename = DIRECTORY / "native-emergency-thresholds.csv"
    with filename.open() as handle:
        rows = list(csv.DictReader(handle))
    key_fields = ("amplification", "controller", "hlp", "debt_side", "pieces",
                  "path", "withdrawal_bps", "transfer_fee_bps", "emergency_first")
    counts = Counter(row["critical_bps"] for row in rows)
    assert counts == {"7500": 768, "6667": 768, "5000": 768}
    expected_keys = None
    for threshold in counts:
        selected = [r for r in rows if r["critical_bps"] == threshold]
        keys = {tuple(r[k] for k in key_fields) for r in selected}
        assert len(keys) == len(selected)
        if expected_keys is not None:
            assert keys == expected_keys
        expected_keys = keys
    for row in rows:
        assert int(row["original_debt"]) > 0
        assert int(row["original_debt"]) == sum(int(row[k]) for k in
                                                ("repaid", "insurance", "loss", "remaining"))
        assert row["blocked"] in ("", "entry:LeverageInitialMarginTooLow")
    stress = []
    for threshold in ("7500", "6667", "5000"):
        selected = [r for r in rows if r["critical_bps"] == threshold]
        fractions = [(int(r["insurance"]) + int(r["loss"])) / int(r["original_debt"])
                     for r in selected]
        stress.append(dict(emergency_mm_pct=int(threshold) / 100, scenarios=len(selected),
                           median_loss_pct=100 * median(fractions),
                           p90_loss_pct=100 * percentile(fractions, .90),
                           max_loss_pct=100 * max(fractions),
                           median_implied_target_pct=150 * median(fractions),
                           unresolved_eligible_cases=sum(int(r["eligible"]) > 0 for r in selected)))
    funding_rows = [funding(t, v, k) for t in (200, 500, 1000)
                    for v in (1000, 5000, 10000) for k in (5000, 7500, 9000)]
    capacity = [dict(target_pct=t, fund=100_000 * t / 100,
                     daily_budget=100_000 * t / 100 * .5,
                     loss_limit_pct=t * .5 / .75) for t in (2, 5, 10)]
    for row in capacity:
        assert abs(row["loss_limit_pct"] * 1000 * .75 - row["daily_budget"]) < 1e-8
    return dict(executed_at=datetime.now(timezone.utc).isoformat(),
                input_sha256=hashlib.sha256(filename.read_bytes()).hexdigest(),
                input_rows=len(rows), matched_fixtures=768,
                entry_rejection_rows=sum(bool(r["blocked"]) for r in rows),
                capacity=capacity, funding=funding_rows, stress=stress)


def report(result):
    lines = ["# Insurance target sensitivity — 2026-10-10", "",
             "## Recommendation", "",
             "The user approved 5% of outstanding covered borrower/leverage principal per "
             "debt token as the provisional calibration target. Zero-insurance launches and "
             "optional pre-funding are approved; the illustrated seed is not required. The user "
             "selected an additional one-fifteenth of indexed hLP funding debt in the same token; "
             "the funding tables below exclude that component and hLP claims. This is an operating budget, not a statistically "
             "established optimum or a claim that it covers the saved stress scenarios. "
             "The user also approved full designated funding through 75% of target, then "
             "a linear taper to zero at the target.", "",
             "## Selected mechanics and illustrative assumptions", "",
             "Selected: 1% fee on net debt repaid; 0.2% to protocol and 0.8% to insurance/LPs; "
             "up to 75% principal-loss coverage under a shared 50% fund budget per 24 hours.", "",
             "Funding illustration: constant 100,000 covered principal, 1,000 seed, no losses, "
             "no transfer fees, fully collected solvent fees, 30-day months and daily credits. "
             "Tested monthly repayment bases are assumptions, not volume estimates. "
             "The main comparison uses full designated funding until 75% of target, then a "
             "linear taper to zero. Knee sensitivities at 50% and 90% are also computed. "
             "No existing fund is paid out when its target declines.", "",
             "## Fully funded capacity", "",
             "No inflows, no previous window spending, no competing hLP claims and no token "
             "transfer fees. To cover 75% of a loss L within a 50% draw budget requires "
             "fund >= 1.5 × L. Capacity depends on actual balance, not the configured target.", "",
             "| Target / principal | Fund | Window budget | Loss / principal receiving full 75% coverage |",
             "| ---: | ---: | ---: | ---: |"]
    for r in result["capacity"]:
        lines.append(f'| {r["target_pct"]}% | {r["fund"]:,.0f} | {r["daily_budget"]:,.0f} | {r["loss_limit_pct"]:.2f}% |')
    lines += ["", "## Months to 95% of target", "",
              "| Target | Monthly repayment = 10% of principal | 50% | 100% |",
              "| ---: | ---: | ---: | ---: |"]
    for target in (2, 5, 10):
        selected = [r for r in result["funding"] if r["target_pct"] == target and r["taper_knee_pct"] == 75]
        lines.append(f'| {target}% | ' + " | ".join(f'{r["months95"]:.1f}' for r in selected) + " |")
    lines += ["", "At the 5% target and 100% monthly repayment base, moving the taper knee "
              "to 50% / 75% / 90% reaches 95% of target in " + " / ".join(
                  f'{r["months95"]:.2f}' for r in result["funding"]
                  if r["target_pct"] == 5 and r["monthly_repayment_pct"] == 100) + " months.", "",
              "While fully underfunded, replenishment is at most 0.008 × eligible net repayment "
              "volume. Covering 75% of losses therefore requires losses <= 1.067% of that "
              "repayment volume for fee inflows alone to keep pace, before hLP claims and costs. "
              "Near target the taper reduces funding further. Large initial claims require seed capital.", "",
              "## Existing stress evidence", "",
              "2,304 saved rows represent 768 matched fixtures at three counterfactual thresholds. "
              "They are deliberately chosen stresses, not independent observations or an empirical "
              "loss-frequency distribution. Each ratio uses that scenario's admitted original debt; "
              "this is not total market TVL. Realized principal shortfall before insurance equals "
              "insurance credited + principal written off. Unsettled debt is excluded from that "
              "realized measure and is not treated as recovered. 912 rows record a later "
              "requested entry rejected by initial-margin checks; admitted debt, not requested "
              "exposure, is the denominator.", "",
              "| Historical threshold / MM | Median shortfall / original debt | P90 (fixture grid) | Max | Cases still eligible |",
              "| ---: | ---: | ---: | ---: | ---: |"]
    for r in result["stress"]:
        lines.append(f'| {r["emergency_mm_pct"]:.2f}% | {r["median_loss_pct"]:.2f}% | {r["p90_loss_pct"]:.2f}% | {r["max_loss_pct"]:.2f}% | {r["unresolved_eligible_cases"]} / 768 |')
    lines += ["", "These median shortfalls would require roughly 37–40% of original debt in "
              "insurance to cover 75% within one 50% draw window, if realized in that window. "
              "Even 10% does not cover these median stresses at the intended 75% rate. "
              "That is a conditional sizing check, not a measured 24-hour risk estimate.", "",
              "The fixtures use the earlier full-emergency / health-only incentive / 0.2% fee "
              "design, with frozen interest and no external routes. They do not validate the "
              "selected 70%-of-MM, 120-second incentive, partial-first, 1%-fee redesign. "
              "hLP affects native pool behavior, but these loss columns do not aggregate hLP "
              "insurance claims. A principal-only target therefore needs an additional hLP allowance.", "",
              "## Validation and next decision", "",
              "Checked unique and matched fixture keys, 768 rows per threshold, positive debt, "
              "only the expected entry-rejection label, and principal conservation in all "
              "2,304 rows. All 27 funding "
              "cases conserve seed + fees = fund + LP allocations and respect target bounds. "
              "Capacity formulas reconcile with hand-calculated 75% / 50% limits. "
              "This Python analysis is not an instruction-level implementation test.", "",
              "No unique optimum can be estimated without an accepted uncovered-loss objective, "
              "loss-frequency data, solvent repayment volume, correlated hLP claims and seed policy. "
              "Carry the selected provisional target into integrated calibration; do not describe it as "
              "a release-ready insurance guarantee.", "",
              f'Executed: {result["executed_at"]}. Input SHA-256: `{result["input_sha256"]}`.', "",
              "Sources: `LIQUIDATION_DECISIONS.md`, `insurance_policy_analysis.py`, "
              "`native-emergency-thresholds.csv`, `EMERGENCY_THRESHOLD_RESULTS.md`. "
              "Run `PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/insurance_target_analysis.py --write` "
              "to regenerate this report and print the complete result JSON.", ""]
    return "\n".join(lines)


if __name__ == "__main__":
    result = analyze()
    if "--write" in sys.argv:
        (DIRECTORY / "INSURANCE_TARGET_RESULTS.md").write_text(report(result))
    print(json.dumps(result, indent=2))

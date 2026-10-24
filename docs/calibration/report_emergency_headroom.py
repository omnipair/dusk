"""Validate native checkpoints and report recovery before insurance/write-off."""

import csv
import statistics
import sys
from pathlib import Path

DIRECTORY = Path(__file__).resolve().parent
KEYS = ("critical_bps", "amplification", "controller", "hlp", "debt_side",
        "pieces", "path", "withdrawal_bps", "transfer_fee_bps", "emergency_first")
LABELS = {"7500": "75%", "6667": "Two-thirds", "5000": "50%"}


def extract(log, tag):
    rows = list(csv.DictReader(line.removeprefix(tag) for line in log.splitlines()
                              if line.startswith(tag)))
    if not rows or any(None in row or None in row.values() for row in rows):
        raise ValueError(f"Missing or malformed {tag} data")
    return rows


def number(row, key):
    return int(row[key])


def coverage(row):
    return number(row, "net_recovery") / number(row, "debt")


def main():
    log = Path(sys.argv[1]).read_text()
    if "test result: ok. 1 passed" not in log or "test result: FAILED" in log:
        raise ValueError("Expected a complete passing native threshold sweep")
    outcomes = extract(log, "EMERGENCY_THRESHOLD,")
    with (DIRECTORY / "native-emergency-thresholds.csv").open() as handle:
        prior = list(csv.DictReader(handle))
    if len(outcomes) != 2304 or outcomes != prior:
        raise ValueError("Observations changed the previous experiment's outcomes")
    rows = extract(log, "EMERGENCY_HEADROOM,")
    seen = set()
    for row in rows:
        key = tuple(row[k] for k in (*KEYS, "position_index", "stage"))
        if key in seen:
            raise ValueError(f"Duplicate checkpoint: {key}")
        seen.add(key)
        assert row["stage"] in ("ordinary_eligible", "emergency_permitted", "emergency_selected")
        assert number(row, "debt") > 0
        assert number(row, "health_bps") <= number(row, "mm_bps")
        if row["stage"] != "ordinary_eligible":
            assert number(row, "health_bps") * 10000 <= number(row, "mm_bps") * number(row, "critical_bps")
        if row["quote_ok"] == "true":
            payment = number(row, "output") - number(row, "reward")
            assert number(row, "contribution") == min(max(0, payment - number(row, "debt")), number(row, "debt") * 20 // 10000)
            assert number(row, "net_recovery") == payment - number(row, "contribution")
    assert sum(r["stage"] == "emergency_selected" for r in rows) == sum(number(r, "emergency") for r in outcomes)
    with (DIRECTORY / "native-emergency-headroom.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=rows[0].keys(), lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)

    report = [
        "# Leverage recovery available at emergency access", "",
        "2026-10-09. PR #45 source `58b02fa` plus local test-only calibration adapters. "
        "No runtime policy or default changed. The user authorized keeping the existing IM/MM "
        "curve and calibrating emergency access against remaining execution recovery.", "",
        "## Recommendation for the next implementation decision", "",
        "**Use 75% of stored effective MM as the proposed initial emergency boundary.** "
        "At 7% MM this is 5.25% symmetric-EMA equity. Keep the existing 10% small-position IM, "
        "7/12/17% marginal maintenance bands, +3-point entry buffer and incremental crowding. "
        "Keep ordinary fixed-payment partial purchases available from MM; they may execute "
        "through Dusk, external venues or buyer inventory. Emergency permission remains based "
        "on EMA health, never solely on a manipulable spot quote. No timer or withdrawn solvent "
        "full-close shortcut is introduced.", "",
        "This changes the previous two-thirds recommendation because the user's latest priority "
        "is preserving execution headroom. Among the three tested boundaries, 75% generally "
        "allows action with more remaining recovery. It is not optimal on every path: the "
        "[matched comparison](EMERGENCY_THRESHOLD_RESULTS.md) has 454 cases with lower realized "
        "shortfall than 50%, 164 with higher shortfall, and 150 ties. Earlier forced full sales "
        "can interrupt beneficial later partial fills. This is a product tradeoff pending "
        "selection, not proof that 75% is universally safe.", "",
        "**The 4–5% buffer is conditional, not a guarantee.** In many stress checkpoints a "
        "hypothetical full Dusk sale already recovers less than debt at ordinary eligibility. "
        "Changing only the emergency boundary cannot create missing liquidity. That quote "
        "does not establish that partial purchases or external execution cannot repay the debt. "
               "Insurance and write-off are excluded from recovery measurements below.", "",
        "## What the existing margins imply under a fixed haircut", "",
        "For unchanged collateral quantity, fixed debt D, reference value C, equity h = 1-D/C "
        "and a constant all-in execution haircut k, the additional reference decline to "
        "break-even is `1 - (1-h)/(1-k)`. A negative result means the assumed sale already "
        "falls short. IM governs admission; these calculations begin at MM or the emergency "
        "boundary. Growing incentives, interest and changing slippage invalidate a constant "
        "haircut forecast; these numbers are arithmetic sensitivity only.", "",
        "| Small-position stage | EMA equity | Further decline with 2% all-in haircut | With 3% | With 5% |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for label, h in (("Ordinary liquidation", .07), ("75% emergency", .07 * .75),
                     ("Two-thirds emergency", .07 * .6667), ("50% emergency", .07 * .5)):
        values = [1 - (1 - h) / (1 - cost) for cost in (.02, .03, .05)]
        report.append(f"| {label} | {100*h:.2f}% | " + " | ".join(
            f"{100*x:.2f}%" if x >= 0 else "Already short" for x in values) + " |")
    report += ["", "At 30% of stored side depth, the retained curve gives approximately "
               "13.67% effective MM and 16.67% IM before additional crowding (6x reference "
               "leverage). 75% emergency permission would open at 10.25% EMA equity. "
               "The higher reference cushion is not evidence of low execution impact.", "",
               "## Native checkpoint evidence", "",
               "This is a leverage-position experiment. Both debt directions are covered, "
               "but borrowing accounts, their collateral combinations and liquidation thresholds "
               "are not calibrated by this run. Shared flash accounting and borrowing-specific "
               "pricing still need their own instruction-level coverage.", "",
               f"The same 2,304 runs produce {len(rows):,} position checkpoints. All original "
               "aggregate rows are byte-value equivalent after CSV parsing: adding quotes "
               "did not change execution outcomes. Admission, curve, concentration, controller, "
               "hLP, both debt directions, fees, withdrawals and paths are unchanged. See the "
               "[original experiment](EMERGENCY_THRESHOLD_RESULTS.md) for the full fixture.", "",
               "Measure the first observed ordinary eligibility and the first observed emergency "
               "permission for each admitted position, before choosing that observation's fill. "
               "A position already closed by an ordinary fill has no later emergency checkpoint. "
               "Within an observation, preceding positions may already have changed the market. "
               "Consequently checkpoint populations differ and counts below are descriptive, "
               "not a matched causal comparison or empirical probability. Discrete observations "
               "can overshoot the nominal threshold; no claim of exact continuous triggering.", "",
               "Net full-sale recovery is native swap output less the provisional progressive "
               "0–1% caller reward and the solvent-only capped insurance contribution. Native "
               "swap fees and collateral transfer fees are already included. Repayment nets "
               "principal internally; only real outflows transfer debt tokens. The health "
               "reference column values collateral after its modeled transfer fee, matching "
               "the eligibility adapter. Accrued interest is frozen, so debt equals principal. "
               "Neither the 1% reward cap nor the 0.2% contribution has been selected as a "
               "runtime default. No keeper-profit gate is applied to observation quotes.", "",
               "| Boundary | Checkpoint | Valid quotes | Full sale covers debt | Median recovery / debt |",
               "| --- | --- | ---: | ---: | ---: |"]
    for key, label in LABELS.items():
        for stage, name in (("ordinary_eligible", "First ordinary eligibility"),
                            ("emergency_permitted", "First emergency permission")):
            subset = [r for r in rows if r["critical_bps"] == key and r["stage"] == stage and r["quote_ok"] == "true"]
            report.append(f"| {label} | {name} | {len(subset)} | "
                          f"{sum(coverage(r) >= 1 for r in subset)} | "
                          f"{100*statistics.median(coverage(r) for r in subset):.2f}% |")
    report += ["", f"Quote failures: {sum(r['quote_ok'] != 'true' for r in rows)}. "
               "A covering quote is not proof that every required token transfer, cash reservation "
               "or future transaction will execute. It does not authorize a full sale outside "
               "the agreed health and settlement rules.", "",
               "### Concrete concentrated-pool checkpoint", "",
               "First admitted position in the amplification-4/controller-on/hLP-on quote-debt "
               "fixture, ten requested 1,500-spend positions at 2x, slow decline, no token fees "
               "or LP withdrawal, ordinary-first ordering. Six positions are admitted. Values "
               "below are debt-token units; both checkpoint quotes refer to the 75% policy run.", "",
               "| Checkpoint | EMA equity | EMA collateral value | Debt | Net full Dusk sale | Shortfall before insurance |",
               "| --- | ---: | ---: | ---: | ---: | ---: |"]
    for row in rows:
        if tuple(row[k] for k in (*KEYS, "position_index")) != (
            "7500", "4", "true", "true", "1", "10", "0", "0", "0", "false", "0"
        ) or row["stage"] == "emergency_selected":
            continue
        report.append(f"| {row['stage']} | {number(row,'health_bps')/100:.2f}% | "
                      f"{number(row,'reference_value')/1000:.3f} | {number(row,'debt')/1000:.3f} | "
                      f"{number(row,'net_recovery')/1000:.3f} | "
                      f"{max(0, number(row,'debt')-number(row,'net_recovery'))/1000:.3f} |")
    report += ["", "This illustrates the limit of the earlier constant 2–3% haircut example. "
               "It is not a new liquidation trigger and does not justify marking a position "
               "unhealthy solely because AMM liquidity falls.", "",
               "## Implementation handoff and unresolved product choices", "",
               "1. Keep the implemented stored margin terms, aggregate crowding and symmetric-EMA "
               "eligibility. Do not restore the 2% unwind cap or change MM to satisfy a presumed "
               "universal loss buffer.",
               "2. Select the proposed 75% boundary before wiring an emergency runtime default. "
               "Keep ordinary fixed-payment flash purchases and internal emergency sales as "
               "distinct permissions; keep both atomic and principal-first.",
               "3. Implement the shared flash/session/payment/accounting contract with explicit "
               "parameters. Reward cap, insurance contribution and dust funding still need "
               "their previously requested decisions; illustrative fixture values are not approvals.",
               "4. Preserve instruction-level acceptance gates: actual net credit, same-market "
               "routing/netting, LP and parameter composition, transfer fees, accrued interest, "
               "insurance limits and write-off. The native adapter does not close finding #287652.",
               "5. Retain the known stalled-fill risk: a position can stabilize below MM, above "
               "emergency permission, with no profitable ordinary fill. Do not add a timer, "
               "unapproved subsidy or early full-sale shortcut to hide this risk.", "",
               "## Reproduce", "", "```sh",
               "DUSK_EMERGENCY_THRESHOLD_SWEEP=1 CARGO_TARGET_DIR=/private/tmp/dusk-emergency-headroom-20261009 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_OPT_LEVEL=2 CARGO_INCREMENTAL=0 cargo test -p dusk native_emergency_threshold_comparison -- --nocapture > /private/tmp/dusk-emergency-headroom-20261009.log 2>&1",
               "PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/report_emergency_headroom.py /private/tmp/dusk-emergency-headroom-20261009.log",
               "```", "", "Raw checkpoints: [CSV](native-emergency-headroom.csv). "
               "Temporary build products and the extraction log can be removed after validation."]
    (DIRECTORY / "EMERGENCY_HEADROOM_RESULTS.md").write_text("\n".join(report) + "\n")
    print(f"Validated {len(outcomes)} unchanged runs and {len(rows)} unique checkpoints")


if __name__ == "__main__":
    main()

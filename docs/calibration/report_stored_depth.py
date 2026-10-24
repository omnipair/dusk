"""Summarize successful native calibration exports without selecting defaults."""

import csv
from collections import Counter
from pathlib import Path

from flash_purchase_model import Terms


def main():
    directory = Path(__file__).resolve().parent
    entries = list(csv.DictReader((directory / "leverage-stored-depth-entry.csv").open()))
    paths = list(csv.DictReader((directory / "leverage-stored-depth-paths.csv").open()))
    if len(entries) != 384 or len(paths) != 384:
        raise SystemExit("Unexpected coverage; regenerate the successful native exports")
    admitted = Counter(r["schedule"] for r in entries if r["admitted"] == "true")
    labels = ["7/8/10%, at 20/60% depth", "7/12/30%, at 5/15% depth",
              "7/20/40%, at 5/15% depth", "7/25/45%, at 5/15% depth"]
    out = ["# Stored-depth margin candidates", "", "2026-10-05. Calibration only; no runtime defaults changed.", "",
           "## What changed", "",
           "All candidates preserve 7% maintenance in the first band. Higher percentages apply only to "
           "the collateral inside each subsequent band. Initial margin is the greater of effective "
           "maintenance plus 3 percentage points and the same-side crowding requirement. "
           "The bands, buffer and crowding slope remain proposed settings.", "",
           "The liquidity reference is stored in collateral token units. The candidate uses the smaller "
           "of collateral-side cash and debt-side cash converted at the reference price, then scales it "
           "down if the existing pessimistic depth EMA is below current curve depth. It excludes loan "
           "receivables and does not multiply physical cash by concentration. The position retains this "
           "reference after LP withdrawals. Other positions' entry changes IM for new risk, not its MM.", "",
           "## What the rates mean", "",
           "Illustration: stored collateral-side depth worth $50,000 at the reference price. This is "
           "one side's reference, **not $50,000 total TVL**. A balanced $100,000 pool starts with $50,000 "
           "per side; entry spending can reduce the actual stored reference. Exposure below means the "
           "reference value of held collateral, not wallet deposit or entry spending.", "",
           "| Collateral exposure | 7/12/30%: effective MM / minimum IM | 7/20/40%: effective MM / minimum IM |",
           "| ---: | ---: | ---: |"]
    for exposure in [1_000, 2_500, 5_000, 10_000, 15_000]:
        cells = []
        for rates in [(0.07, 0.12, 0.30), (0.07, 0.20, 0.40)]:
            mm = Terms(rates=rates, bands=(0.05, 0.15)).maintenance(exposure, 50_000)
            cells.append(f"{mm:.2%} / {mm + 0.03:.2%}")
        out.append(f"| ${exposure:,} | {' | '.join(cells)} |")
    out += ["", "IM can be higher with crowding. These are equity/reference-collateral ratios; fees and "
            "entry price impact mean their inverses are not promises of wallet leverage.", "",
            "## Native admission comparison", "",
            "384 rows: 96 native entry/exit scenarios evaluated under four schedules. The sweep uses "
            "$25k/$100k/$1m initially balanced markets, amplification 1 and 4, both debt directions, "
            "entry spending at 1/5/15/30% of initial pool value and 20/50% wallet funding. The native "
            "swap/debt accounting is executed; candidate admission replaces the 2% rule **only in this "
            "test harness**. Runtime still uses the existing admission rules.", "",
            "| Marginal schedule | Admitted / 96 tested |", "| --- | ---: |"]
    for i, label in enumerate(labels):
        out.append(f"| {label} | {admitted[str(i)]} / 96 |")
    out += ["", "There is no representative-market weighting here. Admission counts compare these "
            "fixtures, not expected user conversion rates. Native concentrated execution improves some "
            "entries but must not be treated as permanently available exit depth.", "",
            "## Earlier eligibility is not a profitable fill", "",
            "The native path experiment opens a position, then sells 1% of the current collateral-side "
            "cash each observation, every 10 or 60 modeled seconds. Both use a 60-second EMA. **This "
            "differs from the Python model's 0.5%/minute and 3%/10-second price paths.** No liquidation "
            "is applied: each threshold is a counterfactual observation along the same history. The "
            "table reports AMM quotes before caller rewards and insurance, not realized losses.", "",
            "Example: $100k initial pool, amplification 1, quote-token debt, $15k entry spending from "
            "$7.5k wallet + $7.5k debt. Candidate 7/20/40% sets effective MM to 27.80% and IM to 30.80% "
            "against stored depth; admission passes. Rows below use the 60-second observations.", "",
            "| Threshold as fraction of MM | EMA equity at first sampled crossing | Full AMM quote | Principal shortfall before reward / insurance |",
            "| ---: | ---: | ---: | ---: |"]
    example = [r for r in paths if all(r[k] == v for k, v in {
        "amplification": "1", "debt_side": "1", "spend": "15000",
        "step_slots": "150", "schedule": "2"}.items())]
    for r in sorted(example, key=lambda r: -int(r["threshold_fraction_bps"])):
        out.append(f'| {int(r["threshold_fraction_bps"])/100:.0f}% | '
                   f'{int(r["reference_health"])/100:.2f}% | ${int(r["full_exit"])/1000:,.2f} | '
                   f'${int(r["full_principal_shortfall_before_reward"])/1000:,.2f} |')
    profitable = sum(int(r["profitable_partial_bps"]) > 0 for r in paths)
    out += ["", f"At these {len(paths)} sampled threshold crossings, {profitable} cases have a profitable "
            "partial on the tested 1% size grid with the proposed 0.5–3% EMA discount, 0.2% insurance "
            "fee and $0.05 buyer cost. These continuing-decline fixtures make the symmetric EMA lag "
            "the executable price. That does not prove no profitable size exists between samples, "
            "outside Dusk or below the grid resolution. It does show that raising MM alone cannot be "
            "claimed to create early flash fills. Eligibility, payment pricing and emergency permission "
            "must be calibrated together.", "",
            "The 25%, 50% and 75% critical fractions are experiments; none is selected. An earlier "
            "emergency can realize a better quote on this declining path, but also lets the keeper "
            "force a full sale earlier. No on-chain proof establishes that outside buyers were absent. "
            "The emergency AMM execution/manipulation tradeoff remains explicit.", "",
            "## Covered and still missing", "",
            "The native regression verifies that a same-slot proportional LP deposit does not inflate "
            "this candidate's stored reference, and that a subsequent withdrawal lowers new-admission "
            "capacity without rebasing an existing position's MM or linear EMA valuation. It is a "
            "focused regression, not a proof against arbitrary manipulation or concentration changes.", "",
            "The independent [flash-purchase model](FLASH_PURCHASE_RESULTS.md) executes candidate "
            "partial/full economic settlements across one versus ten positions and outside venue "
            "depths. Steeper individual tiers do not prevent splitting into small positions, and "
            "insurance's per-event cap changes the comparison. Report principal shortfall before "
            "insurance as well as write-off after insurance.", "",
            "Remaining coverage includes native proposed settlement and partial repayment, token fees, "
            "borrow positions, hLP, interest, cross-position/LP/parameter composition, depth/price "
            "manipulation, dust and keeper costs. A margin candidate is not release-ready solely "
            "because these native quote tests pass. The full flash redesign remains unimplemented.", "",
            "## Reproduce", "", "```sh",
            "cargo test -p dusk stored_depth -- --nocapture > /private/tmp/dusk-stored-depth.log 2>&1",
            "PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/extract_reports.py /private/tmp/dusk-stored-depth.log",
            "PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/report_stored_depth.py", "```"]
    (directory / "STORED_DEPTH_RESULTS.md").write_text("\n".join(out) + "\n")
    print("Wrote STORED_DEPTH_RESULTS.md from native exports; no defaults selected.")


if __name__ == "__main__":
    main()

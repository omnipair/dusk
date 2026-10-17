"""Reproduce the solvent-window comparison without creating build/temp files."""

from itertools import product
from pathlib import Path

from emergency_model import save_csv
from flash_purchase_model import Terms, simulate
from repayment_cushion import solvent_amm_sale


def main():
    rows = []
    for pieces, outside, path, withdrawal, critical, width in product(
            (1, 10), (0, 1, 5), ("slow", "fast", "gap_flat"),
            (0, 0.25), (0.5, 0.75), (None, 0.005, 0.01, 0.02, 0.05)):
        for priority in ((False,) if width is None else (False, True)):
            terms = Terms(rates=(0.07, 0.12, 0.17), bands=(0.05, 0.15),
                          maximum_discount=0.05, critical_fraction=critical)
            rows.append(simulate(terms=terms, pieces=pieces, outside_depth=outside,
                                 path=path, withdrawal=withdrawal,
                                 cushion_limit=width, solvent_first=priority))
    assert len(rows) == 648
    assert all(r["opened"] == r["pieces"] and r["original_debt"] == 7_500 for r in rows)
    directory = Path(__file__).resolve().parent
    save_csv(directory / "repayment-cushion-candidates.csv", rows)
    report = [
        "# Solvent AMM repayment cushion — candidate results", "",
        "2026-10-05. Economic model only; no new on-chain instruction has been implemented. "
        "The user approved modeling this direction, not a numeric cushion or final loss-mode threshold.", "",
        "For concentrated pools and hLP, read the [native follow-up](CONCENTRATED_CUSHION_RESULTS.md). "
        "The 648 scenarios below remain plain CPMM and cannot stand in for that native execution coverage.", "",
        "## Proposed permission", "",
        "1. Symmetric EMA health must already permit liquidation; retain linear collateral valuation.",
        "2. Quote the full internal sale after execution costs. Deduct the bounded health-based caller reward "
        "and the solvent insurance contribution; compare the remainder with principal plus accrued interest.",
        "3. Permit this additional full-close mode only when that remainder covers all debt and its excess "
        "is at most a candidate fraction of debt. Recheck actual execution atomically; any shortfall reverts "
        "this mode. Owner receives the residual. No insurance draw, principal write-off or interest cancellation.",
        "4. Keep a separate explicitly authorized loss-taking mode for gaps and already-insolvent execution. "
        "A negative AMM cushion alone does not authorize it. The old critically-low-EMA threshold is still "
        "a calibration variable, not a selected default.", "",
        "```text", "D = principal + interest", "R = actual spendable AMM output - bounded caller reward",
        "F = min(max(R - D, 0), D * insurance contribution rate)",
        "solvent window: EMA-eligible AND 0 <= (R - F - D) / D <= cushion limit",
        "```", "",
        "Do not count insurance as execution proceeds or subtract an AMM fee twice. Keeper transaction "
        "costs are funded from its reward, not a caller-defined extra charge against the position. "
        "The full-close permission can end an unhealthy position that a partial buyer might otherwise repair; "
        "a partial-first off-chain preference is not an enforced priority.", "",
        "## Matched paths", "",
        "All rows start with a $100,000 plain constant-product pool, $15,000 total entry spending, "
        "$7,500 wallet equity and $7,500 debt. This is a 2x spending/equity fixture, not a simulated 6x entry. "
        "The provisional marginal MM schedule is 7/12/17% at 5/15% of stored side depth: it meets the "
        "user's 6x reference-margin target at 30% depth with the approved +3-point IM buffer. "
        "It does not select the entire curve or its behavior at larger sizes.", "",
        "Selected discount candidate: 0.5% to 5% by EMA health. Other experiment inputs remain unselected: "
        "up to 1% internal caller reward, 0.2% solvent insurance funding, recovery MM +2 points, "
        "$0.05 fixed keeper cost. The table uses the separately permitted loss mode at half MM, "
        "no outside venue or LP withdrawal, and partial/ordinary purchase first. "
        "Slow = 0.5% spot decline per minute; fast = 3% per ten seconds; gap = one 45% drop then flat. "
        "EMA half-life is 60 seconds, sampled from the prior observed price.", "",
        "| Positions | Path | Cushion limit | Solvent full closes | Other emergency closes | "
        "Principal loss after insurance | Insurance drawn | Owner cash returned | Unresolved eligible |",
        "| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for r in rows:
        if (r["outside_depth"] or r["withdrawal"] or r["critical_fraction"] != 0.5
                or r["solvent_first"] or r["cushion_limit"] not in (None, 0.02)):
            continue
        width = "Disabled" if r["cushion_limit"] is None else f'{r["cushion_limit"]:.0%}'
        report.append(f'| {r["pieces"]} | {r["path"]} | {width} | {r["solvent_full"]} | '
                      f'{r["emergency_full"]} | ${r["loss"]:.2f} | ${r["insurance_drawn"]:.2f} | '
                      f'${r["owner_value"]:.2f} | {r["eligible_remaining"]} |')
    report += ["", "## What this comparison supports", "",
               "The extra mode can preserve a still-solvent exit on a slow decline. In the ten-position "
               "fixture, the 2% candidate repays all debt without drawing insurance. The single larger "
               "position and fast/gap fixtures still require the separate loss mode: the added permission "
               "does not create an opportunity that is absent when the position is observed EMA-eligible.", "",
               "Widening the window permits earlier full closures with more residual equity. A narrower "
               "window gives partial buyers more room to act, but can be skipped between observations. "
               "These are policy costs, not a guarantee that any keeper will submit.", "",
               "## Same EMA, different executable quote", "",
               "Counterfactual inputs: $900 principal + $50 interest; EMA equity 6%, maintenance 7%; "
               "2% window; same reward/funding candidates. These seeded inputs test the predicate, "
               "not an admitted position, native attack, or attacker profitability.", "",
               "| Spendable sale output | Solvent mode permitted? | Owner after all modeled charges |",
               "| ---: | --- | ---: |"]
    for output in (990, 980, 960, 952, 949):
        result = solvent_amm_sale(principal=900, interest=50, output=output,
                                  equity=0.06, maintenance=0.07, cushion_limit=0.02,
                                  reward_max=0.01, insurance_rate=0.002)
        owner = f'${result["owner"]:.2f}' if result else "—"
        report.append(f'| ${output} | {"Yes" if result else "No"} | {owner} |')
    priority_pairs = []
    index = {(r["pieces"], r["outside_depth"], r["path"], r["withdrawal"],
              r["critical_fraction"], r["cushion_limit"], r["solvent_first"]): r for r in rows}
    for key, r in index.items():
        if key[-1]:
            other = index[key[:-1] + (False,)]
            if any(abs(r[k] - other[k]) > 1e-6 for k in (
                    "owner_value", "remaining_owner_ema", "loss", "insurance_drawn", "partials")):
                priority_pairs.append((other, r))
    report += ["", "A lower AMM quote can enable a full close of an already EMA-unhealthy position and "
               "reduce the owner's residual. Full debt recovery protects lenders in this mode; it does not "
               "prove fair execution, resistance to manipulation, or closure of finding #287652. "
               "Healthy EMA positions are rejected regardless of the quote.", "",
               "## Scope and unresolved policy", "",
               f"The CSV contains {len(rows)} runs: 1/10 positions, 0/1/5x outside depth, three price paths, "
               "0/25% early depth withdrawal, loss-mode threshold 50/75% of MM, and disabled/0.5/1/2/5% "
               "windows. Enabled windows compare partial-first and solvent-full-first callers. "
               f"Ordering changes outcomes in {len(priority_pairs)} of 288 matched enabled-window pairs. "
               "Do not present partial-first results as guaranteed by permissionless racing.", "",
               "All runs admit the same $7,500 debt. CSV fields include owner cash, surviving owner EMA "
               "equity, unresolved debt, insurance draws, and realized losses so an unfilled position is "
               f"not mistaken for successful recovery. {sum(r['eligible_remaining'] > 0 for r in rows)} "
               f"runs still have eligible positions at the finite horizon (up to {max(r['eligible_remaining'] for r in rows)}); "
               "this model does not demonstrate guaranteed cleanup. The modeled insurance event cap favors splitting; "
               "compare insurance plus write-off as well as write-off alone.", "",
               "This shares the historical floating-point CPMM accounting: entry impact is reset before "
               "the stress path, no restorative Dusk arbitrage follows, and outside depth reprices between "
               "samples. No transfer-fee tokens, native concentration, hLP, public borrowing, concurrent "
               "transactions, integer rounding, or scheduled token-fee changes are validated. Path sweeps "
               "have no accrued interest; independent allocation tests require full interest repayment. "
               "Quote equals execution in the deterministic path model; actual-credit rollback remains "
               "an instruction-level implementation/test requirement. Percentage rewards can still "
               "leave dust uneconomic. No supplied exploit PoC was run.", "",
               "Decisions still needed: cushion width, loss-mode EMA threshold, numeric reward/insurance "
               "caps, recovery/dust rules, and full margin/crowding calibration. Preserve the separate "
               "loss permission rather than allowing a manipulated negative quote to authorize a write-off.", "",
               "## Reproduce", "", "```sh",
               "PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s docs/calibration -p 'test_*.py' -v",
               "PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/report_repayment_cushion.py",
               "```", ""]
    (directory / "REPAYMENT_CUSHION_RESULTS.md").write_text("\n".join(report))
    print(f"Wrote {len(rows)} scenarios; {len(priority_pairs)} ordering-sensitive pairs")


if __name__ == "__main__":
    main()

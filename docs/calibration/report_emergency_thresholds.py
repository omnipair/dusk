"""Extract a complete passing native sweep; compare matched stress fixtures."""

import csv
import statistics
import sys
from pathlib import Path

TAG = "EMERGENCY_THRESHOLD,"
KEYS = ("amplification", "controller", "hlp", "debt_side", "pieces", "path",
        "withdrawal_bps", "transfer_fee_bps", "emergency_first")
LABELS = {"7500": "75%", "6667": "Two-thirds (66.67%)", "5000": "50%"}


def number(row, key):
    return int(row[key])


def shortfall(row):
    return number(row, "insurance") + number(row, "loss")


def including_residual(row):
    return shortfall(row) + number(row, "residual_shortfall")


def owner_equity(row):
    return number(row, "owner") + number(row, "survivor_equity")


def usable(row):
    return number(row, "opened") > 0 and row["blocked"] in ("", "entry:LeverageInitialMarginTooLow")


def compare(a, b, metric):
    less = sum(metric(x) < metric(y) for x, y in zip(a, b))
    more = sum(metric(x) > metric(y) for x, y in zip(a, b))
    return f"{less} / {len(a) - less - more} / {more}"


def main():
    log = Path(sys.argv[1]).read_text()
    if "test result: ok. 1 passed" not in log or "test result: FAILED" in log:
        raise ValueError("Expected a passing complete native threshold test")
    rows = list(csv.DictReader(line.removeprefix(TAG) for line in log.splitlines() if line.startswith(TAG)))
    if len(rows) != 2304 or any(None in row or None in row.values() for row in rows):
        raise ValueError("Incomplete or malformed sweep")
    groups = {}
    for row in rows:
        key = tuple(row[k] for k in KEYS)
        group = groups.setdefault(key, {})
        if row["critical_bps"] in group:
            raise ValueError("Duplicate scenario")
        group[row["critical_bps"]] = row
        assert number(row, "original_debt") == sum(number(row, k) for k in ("repaid", "insurance", "loss", "remaining"))
    for group in groups.values():
        assert set(group) == set(LABELS)
        assert len({(r["opened"], r["original_debt"]) for r in group.values()}) == 1
    matched = [g for g in groups.values() if all(usable(r) for r in g.values())]
    directory = Path(__file__).resolve().parent
    with (directory / "native-emergency-thresholds.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=rows[0].keys(), lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)
    report = [
        "# Emergency threshold comparison", "",
        "2026-10-08. Native economic experiment on PR #45 source `58b02fa`, plus the "
        "test-only adapter in this change. No runtime policy, instruction or default is changed.", "",
        "## Initial recommendation, superseded on 2026-10-09", "",
        "The [recovery checkpoint follow-up](EMERGENCY_HEADROOM_RESULTS.md) now recommends "
        "75% of MM for the stated priority of preserving execution recovery, pending "
        "selection. The comparison below remains unchanged evidence; neither boundary "
        "guarantees a solvent sale or cleanup.", "",
        "Use **two-thirds of MM as the starting emergency candidate**. This is a compromise "
        "between preserving ordinary partial-repair opportunities and intervening earlier on "
        "fast declines; the experiment does not establish a mathematically optimal threshold. "
        "75% generally reduces realized shortfall sooner, but it also worsens some slow paths "
        "where later partial fills reduce the amount sold at once. 50% retains more partial "
        "fills and sometimes preserves borrower recovery, but leaves more eligible debt and "
        "performs worse on the tested fast declines.", "",
        "**No candidate solves cleanup by itself.** A position can remain below MM but above "
        "the emergency boundary with no profitable ordinary Dusk fill, especially after the "
        "external target stabilizes. An incentive based only on health also stops increasing "
        "when health stops deteriorating. External routing may help, but is not guaranteed. "
        "Do not label this risk resolved or claim reliable cleanup from a passing sweep.", "",
        "## Question and current design", "",
        "Compare emergency full AMM-sale permission at 75%, two-thirds (rounded to 66.67%), "
        "and 50% of the position's stored maintenance requirement. At 7% MM these correspond "
        "to 5.25%, approximately 4.67%, and 3.50% symmetric-EMA equity. No timer and no "
        "early solvent full-close shortcut are used.", "",
        "## Experiment", "",
        "2,304 counterfactual runs: three thresholds × amplification 1/4/10 × controller off/on "
        "× hLP off/on × both debt directions × one/ten requested positions × four price paths "
        "× two fee/withdrawal fixtures × ordinary-first/emergency-first ordering. Each starts "
        "with 100,000 total wallet LP deposits and requests 15,000 total entry spending at 2×. "
        "Entry uses the current native open path, stored margin terms and incremental crowding; "
        "later entries can be rejected. Only identically admitted portfolios are compared.", "",
        "Paths are a 0.5% decline per 60-second observation, a 3% decline per 10-second observation, "
        "a 45% gap followed by a flat external target, and a 0.6% decline for 60 observations "
        "followed by recovery. Price targets are reached through actual native trades in either "
        "direction, including arbitrage after liquidation. A step-2 stress fixture combines "
        "25% yLP-share withdrawal with immutable 1% uncapped transfer fees on both tokens. "
        "The separate baseline has neither. This combination does not isolate their individual effects.", "",
        "All ordinary modeled buyers route through Dusk. The model searches collateral slices "
        "on a 1% grid, accepts health-improving partials and targets remaining MM +2 points. "
        "Payment uses the EMA value of collateral delivered after its transfer fee, discounted "
        "by the selected 0.5–5% curve. Full fixed-payment purchases can have an authorized "
        "shortfall. The separate emergency can sell below that floor. Every full debt leg is "
        "cleared through recovery, capped insurance and native principal write-off.", "",
        "Provisional economics held fixed: emergency reward 0–1% of proceeds as health "
        "falls from MM to zero; solvent insurance contribution 0.2%; keeper cost 0.05 debt "
        "tokens per fill; initial insurance 1,000 debt tokens with native draw limits. "
        "Keeper profitability uses actual net reward after the modeled outgoing transfer fee. "
        "Debt repayment is netted internally; only real outgoing profit, owner residual and "
        "insurance funding transfer debt tokens. Insurance transfer fees reduce the credit "
        "reaching repayment. Collateral custody-to-AMM transfer uses SPL fee rounding. "
        "Additional external-route/session-vault transfers are not modeled.", "",
        "Emergency-first is a permitted-ordering stress case, not a prediction of keeper behavior. "
        "Ordinary-first cannot be claimed as an on-chain priority guarantee. Neither assumes "
        "keepers wait deliberately for a larger future reward.", "",
        "## Coverage and exclusions", "",
        f"{len(groups)} unique portfolios/path/ordering fixtures; {len(matched)} have admitted "
        "positions and no path/withdrawal error under all three thresholds. All rows satisfy "
        "original principal = sale repayment + insurance credit + write-off + remaining debt. "
        "Market reserve invariants and aggregate position-collateral exposure are checked "
        "after settlement. Rejected hypothetical operations commit no state.", "",
        "| Threshold | No positions admitted | Path/withdrawal errors | Cash-rejected settlement attempts |",
        "| --- | ---: | ---: | ---: |",
    ]
    for key, label in LABELS.items():
        subset = [r for r in rows if r["critical_bps"] == key]
        report.append(f"| {label} | {sum(number(r, 'opened') == 0 for r in subset)} | "
                      f"{sum(r['blocked'] not in ('', 'entry:LeverageInitialMarginTooLow') for r in subset)} | "
                      f"{sum(number(r, 'cash_rejections') for r in subset)} |")
    report += ["", "## Matched outcomes", "",
               "Counts are deterministic stress cases, not probabilities. Shortfall is insurance "
               "credit plus principal write-off, so using insurance does not hide execution loss. "
               "Remaining debt is not counted as repaid. Values below are percentages of each "
               "run's original debt, then medians across matched fixtures; both debt directions "
               "are normalized separately rather than summing unlike tokens.", "",
               "| Threshold | Median shortfall / initial debt | Median write-off / initial debt | Runs with eligible debt left | Emergency closes | Partial fills |",
               "| --- | ---: | ---: | ---: | ---: | ---: |"]
    for key, label in LABELS.items():
        subset = [g[key] for g in matched]
        rate = statistics.median(100 * shortfall(r) / number(r, "original_debt") for r in subset)
        loss = statistics.median(100 * number(r, "loss") / number(r, "original_debt") for r in subset)
        report.append(f"| {label} | {rate:.3f}% | {loss:.3f}% | "
                      f"{sum(number(r, 'eligible') > 0 for r in subset)} | "
                      f"{sum(number(r, 'emergency') for r in subset)} | {sum(number(r, 'partial') for r in subset)} |")
    report += ["", "### Pairwise comparison", "",
               "Each cell reports **lower / equal / higher** for the earlier boundary versus the later one. "
               "Owner value = realized owner residual + positive EMA equity of surviving positions "
               "at the final observation; it is marked value, not a guaranteed executable exit.", "",
               "| Earlier vs later | Matched runs | Realized shortfall | Including quoted eligible-residual deficit | Write-off | Owner value |",
               "| --- | ---: | --- | --- | --- | --- |"]
    for early, late in (("7500", "6667"), ("7500", "5000"), ("6667", "5000")):
        a, b = [g[early] for g in matched], [g[late] for g in matched]
        report.append(f"| {LABELS[early]} vs {LABELS[late]} | {len(a)} | {compare(a,b,shortfall)} | {compare(a,b,including_residual)} | "
                      f"{compare(a,b,lambda r: number(r,'loss'))} | {compare(a,b,owner_equity)} |")
    report += ["", "### Break down 75% versus 50%", "",
               "| Path | Ordering | Matched runs | Shortfall: lower / equal / higher | Owner value: lower / equal / higher |",
               "| --- | --- | ---: | --- | --- |"]
    for path, name in (("0", "Slow decline"), ("1", "Fast decline"), ("2", "Gap"), ("3", "Dip/recovery")):
        for order in ("false", "true"):
            selected = [g for g in matched if g["7500"]["path"] == path and g["7500"]["emergency_first"] == order]
            a, b = [g["7500"] for g in selected], [g["5000"] for g in selected]
            report.append(f"| {name} | {'Emergency first' if order == 'true' else 'Ordinary first'} | "
                          f"{len(a)} | {compare(a,b,shortfall)} | {compare(a,b,owner_equity)} |")
    report += ["", "## Representative concentrated case", "",
               "Amplification 4, controller and hLP enabled, quote debt, ten requested 1,500-spend "
               "positions at 2×, slow decline, no transfer fees/withdrawals, ordinary-first. "
               "Amounts below are debt-token units.", "",
               "| Threshold | Opened | Initial debt | Insurance credit | Write-off | Owner residual | Eligible debt left | First emergency equity / MM |",
               "| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |"]
    for r in rows:
        if tuple(r[k] for k in KEYS) != ("4", "true", "true", "1", "10", "0", "0", "0", "false"):
            continue
        amounts = " | ".join(f"{number(r,k)/1000:.3f}" for k in ("original_debt", "insurance", "loss", "owner", "eligible_debt"))
        report.append(f"| {LABELS[r['critical_bps']]} | {r['opened']} | {amounts} | "
                      f"{number(r,'first_emergency_health')/100:.2f}% / {number(r,'first_emergency_mm')/100:.2f}% |")
    report += ["", "### Counterexample: earlier full sale can recover less", "",
               "Amplification 4, controller off, hLP off, quote debt, one 15,000-spend 2× "
               "position, slow decline, 25% LP-share withdrawal and 1% transfer fees, ordinary-first. "
               "All thresholds start with 7,500 debt tokens. Later permission allows additional "
               "partial fills as the path evolves, reducing the eventual sale size.", "",
               "| Threshold | Partial fills | Emergency closes | Insurance credit | Write-off | Remaining debt |",
               "| --- | ---: | ---: | ---: | ---: | ---: |"]
    for r in rows:
        if tuple(r[k] for k in KEYS) != ("4", "false", "false", "1", "1", "0", "2500", "100", "false"):
            continue
        report.append(f"| {LABELS[r['critical_bps']]} | {r['partial']} | {r['emergency']} | "
                      f"{number(r,'insurance')/1000:.3f} | {number(r,'loss')/1000:.3f} | {number(r,'remaining')/1000:.3f} |")
    report += ["", "## Eligible residuals", "",
               "The extra comparison above adds the final full-sale quote deficit (after the "
               "same emergency reward) on remaining eligible positions to already realized "
               "shortfall. This is an indicative reserve quote, not permission or proof that "
               "the liquidation can execute. Unquoteable residuals are reported separately.", "",
               "| Threshold | Eligible positions | Above emergency boundary | Largest eligible debt | Residual quote failures |",
               "| --- | ---: | ---: | ---: | ---: |"]
    for key, label in LABELS.items():
        subset = [g[key] for g in matched]
        report.append(f"| {label} | {sum(number(r,'eligible') for r in subset)} | "
                      f"{sum(number(r,'eligible_above_emergency') for r in subset)} | "
                      f"{max(number(r,'largest_eligible_debt') for r in subset)/1000:.3f} debt tokens | "
                      f"{sum(number(r,'residual_quote_failures') for r in subset)} |")
    report += ["", "## Limits", "",
               "- No external liquidity/buyer inventory is assumed. Better outside execution could "
               "repair positions before any emergency, or make an early forced close avoidable.",
               "- Accrual is frozen. These results do not validate principal-first allocation of "
               "accrued interest, canceled interest, referral fees or production flash accounting.",
               "- Fixed 2× requested openings, one initial LP scale, one insurance seed, a discrete "
               "fill grid and four deterministic paths do not establish optimal parameters or loss probabilities.",
               "- Permissionless keeper competition is assumed; no latency, transaction contention, "
               "strategic waiting, manipulation or adversarial LP activity is simulated.",
               "- This is a native economic adapter. Explicit sessions, reserve-credit verification, "
               "token CPIs, shared-state composition and finding #287652 still require implementation/audit.",
               "", "## Reproduce", "", "```sh",
               "DUSK_EMERGENCY_THRESHOLD_SWEEP=1 CARGO_TARGET_DIR=/private/tmp/dusk-emergency-threshold-20261008 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_OPT_LEVEL=2 CARGO_INCREMENTAL=0 cargo test -p dusk native_emergency_threshold_comparison -- --nocapture > /private/tmp/dusk-emergency-threshold-20261008-full.log 2>&1",
               "PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/report_emergency_thresholds.py /private/tmp/dusk-emergency-threshold-20261008-full.log",
               "```", "",
               "The focused normal test runs three thresholds on one concentrated/hLP fixture. "
               "The complete report requires all 2,304 rows and a passing test. Temporary build "
               "artifacts can be removed after extraction; the CSV and report preserve results.", ""]
    (directory / "EMERGENCY_THRESHOLD_RESULTS.md").write_text("\n".join(report))
    print(f"Saved {len(rows)} runs; {len(matched)} matched admitted fixtures")


if __name__ == "__main__":
    main()

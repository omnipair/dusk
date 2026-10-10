"""Save the complete native parameter comparison and residual-cost diagnosis."""

import csv
from pathlib import Path
import sys


def extract(log, tag, expected, keys):
    rows = list(csv.DictReader(line.removeprefix(tag + ",") for line in log.splitlines()
                              if line.startswith(tag + ",")))
    if len(rows) != expected or len({tuple(r[k] for k in keys) for r in rows}) != expected:
        raise ValueError(f"Incomplete or duplicated {tag} sweep")
    if any(None in r or None in r.values() for r in rows):
        raise ValueError(f"Malformed {tag} row")
    return rows


def save(directory, name, rows):
    with (directory / name).open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=rows[0].keys(), lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)


def main():
    log = Path(sys.argv[1]).read_text()
    if "test result: ok. 2 passed" not in log or "test result: FAILED" in log:
        raise ValueError("A passing complete parameter and residual test log is required")
    keys = ("amplification", "controller", "hlp", "pieces", "path", "wallet_bps")
    rows = extract(log, "NATIVE_PARAMETERS", 1080, ("variant", *keys))
    residual = extract(log, "NATIVE_RESIDUAL", 48,
                       ("amplification", "hlp", "debt_side", "cushion_bps", "withdrawal_bps"))
    for r in rows:
        assert int(r["original_debt"]) == sum(int(r[k]) for k in ("repaid", "insurance", "loss", "remaining"))
        assert int(r["failed_fills"]) == 0
        assert r["blocked"] in ("", "entry_margin")
    for r in residual:
        assert r["eligible"] == r["below_keeper_cost"]
    directory = Path(__file__).resolve().parent
    save(directory, "native-parameter-comparison.csv", rows)
    save(directory, "native-residual-cost.csv", residual)

    baseline = {tuple(r[k] for k in keys): r for r in rows if r["variant"] == "baseline"}
    report = [
        "# Native parameter comparison", "",
        "2026-10-07. Calibration evidence only; runtime defaults are unchanged. "
        "See [the recommendation](PARAMETER_RECOMMENDATION.md) for pending product decisions.", "",
        "## Scope", "",
        "1,080 counterfactual runs: five policies × amplification 1/4/10 × controller off/on × "
        "hLP off/on × one/ten requested positions × slow/fast/gap decline × wallet funding "
        "16.67%/20%/50% (approximately 6×/5×/2× requested entry leverage). Quote-token debt, "
        "100,000 initial wallet liquidity, 15,000 total requested entry spending, no withdrawals. "
        "The original 576-case comparison separately covers both debt directions and withdrawals.", "",
        "The baseline uses 7/12/17% marginal MM at 5/15% stored side depth, IM = max(MM +3 points, "
        "10% + one point per ten points of crowding above 20%), recovery MM +2 points, "
        "0.5–5% buyer discount, 2% solvent-close window, critical health at half MM, "
        "0–1% internal caller reward, 0.2% insurance contribution and 0.05 keeper cost. "
        "The caller reward scales linearly with EMA distress and is capped at zero equity.", "",
        "Controller-on also enables the configured divergence fee; it is a combined fixture "
        "change, not an isolated measurement of controller effects. This experiment retains "
        "the original native engine, hLP accounting, curve, cash checks and insurance caps. "
        "It freezes accrual, has no token transfer fees, uses a 1% collateral search grid and "
        "models purchases executed inside Dusk only. No flash instruction or external route "
        "is tested. Keeper entry is assumed whenever the modeled net reward covers cost.", "",
        "## Admission", "",
        "| Requested leverage | Runs | No entry admitted | Runs with at least one entry |",
        "| --- | ---: | ---: | ---: |",
    ]
    for wallet, label in (("1667", "≈6×"), ("2000", "5×"), ("5000", "2×")):
        group = [r for r in baseline.values() if r["wallet_bps"] == wallet]
        empty = sum(int(r["opened"]) == 0 for r in group)
        report.append(f"| {label} | {len(group)} | {empty} | {len(group) - empty} |")
    report += ["", "All single 15,000-spend entries requested at 5× or approximately 6× fail "
               "this adapter's admission checks. Splitting into 1,500-spend requests admits some "
               "positions before later requests fail. This does not meet a promise of 6× wallet "
               "leverage for a large position. The selected 6× target concerns reference margin "
               "at 30% stored depth; AMM impact, fees and reference/exit equity still constrain entry.", "",
               "## Matched policy changes", "",
               "Only runs with admitted positions and identical initial debt enter this comparison. "
               "Counts are deterministic stress fixtures, not market probabilities or expected returns. "
               "Compare insurance consumption and residual debt too: write-off alone understates loss.", "",
               "| Change from baseline | Matched runs | Lower write-off | Same | Higher | Admission count changed |",
               "| --- | ---: | ---: | ---: | ---: | ---: |"]
    labels = {"critical_75": "Critical EMA boundary: 75% of MM", "reward_025": "Internal reward cap: 0.25%",
              "recovery_1": "Recovery target: MM +1 point", "crowding_steeper": "Twice the crowding slope"}
    for variant, label in labels.items():
        pairs = [(r, baseline[tuple(r[k] for k in keys)]) for r in rows if r["variant"] == variant]
        matched = [(r, b) for r, b in pairs if r["original_debt"] == b["original_debt"] and int(r["opened"])]
        better = sum(int(r["loss"]) < int(b["loss"]) for r, b in matched)
        worse = sum(int(r["loss"]) > int(b["loss"]) for r, b in matched)
        changed = sum(r["opened"] != b["opened"] for r, b in pairs)
        report.append(f"| {label} | {len(matched)} | {better} | {len(matched) - better - worse} | {worse} | {changed} |")
    report += ["", "The earlier emergency's result includes the smaller reward at lower distress, "
               "not just a different sale price. It permits an earlier full forced close. Smaller "
               "rewards mechanically retain more sale proceeds when execution occurs; this model "
               "cannot establish that real keepers would act as quickly at the lower reward. "
               "The crowding comparison did not change admission on these fixtures, so it does not "
               "identify an optimal crowding slope. Recovery settings alter fill counts and insurance "
               "use even where final write-off is unchanged.", "",
               "## Representative slow-decline outcomes", "",
               "Both controller settings below use amplification 4, active hLP, ten requested 2× entries. "
               "Amounts are debt-token units. All three policies admit the same positions for "
               "each controller setting. No retained debt is counted as recovered.", "",
               "| Controller | Policy | Opened | Initial debt | Partial fills | Solvent closes | Emergency closes | Insurance | Write-off | Remaining debt |",
               "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for r in rows:
        if (r["amplification"], r["hlp"], r["pieces"], r["path"], r["wallet_bps"]) != ("4", "true", "10", "0", "5000"):
            continue
        if r["variant"] not in ("baseline", "critical_75", "reward_025"):
            continue
        report.append(f'| {r["controller"]} | {r["variant"]} | {r["opened"]} | {int(r["original_debt"])/1000:.2f} | '
                      f'{r["partial"]} | {r["solvent"]} | {r["emergency"]} | {int(r["insurance"])/1000:.3f} | '
                      f'{int(r["loss"])/1000:.3f} | {int(r["remaining"])/1000:.3f} |')
    affected = [r for r in residual if int(r["eligible"])]
    report += ["", "## Residual-cost diagnosis", "",
               f"Replayed the 48 slow/controller-off combinations covering every unfinished run "
               f"in the prior 576-case sweep. {len(affected)} runs retain {sum(int(r['eligible']) for r in affected)} "
               "eligible positions. All have an allowed fill when the same final state is evaluated "
               "with zero keeper cost, and no selected fill at the modeled 0.05 cost. This isolates "
               "an economic cleanup obstacle in those final states. It does not simulate a subsidy "
               "or prove that every possible size/route fails at nonzero cost.", "",
               "Total remaining debt per affected run ranges from 0.146 to 2.605 debt tokens. "
               "Some remaining positions are healthy; the eligible-debt column separates them. "
               "A progressive percentage reward remains too small for sufficiently tiny balances. "
               "No unconditional cleanup guarantee follows from keeper competition.", "",
               "## Reproduce", "", "```sh",
               "DUSK_PARAMETER_SWEEP=1 DUSK_RESIDUAL_SWEEP=1 cargo test -p dusk concentrated_cushion::parameters -- --nocapture > /private/tmp/dusk-parameters.log 2>&1",
               "PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/report_parameters.py /private/tmp/dusk-parameters.log",
               "```", "",
               "Normal CI runs three representative parameter cases plus two residual cases. "
               "The full sweep validates conservation and surfaces unexpected admission, "
               "execution or accounting failures. The report requires a complete passing log. "
               "These economic tests do not replace runtime integration, adversarial instruction "
               "tests or validation of principal-first loss accounting with accrued interest.", ""]
    (directory / "PARAMETER_RESULTS.md").write_text("\n".join(report))
    print(f"Saved {len(rows)} parameter cases and {len(residual)} residual cases")


if __name__ == "__main__":
    main()

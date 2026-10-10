"""Small deterministic insurance-funding and draw-budget sensitivity study.

Run with PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/insurance_policy_analysis.py.
Uses USDC micro-units; no price paths, probability estimates, or runtime changes.
"""

import csv
from dataclasses import dataclass
from pathlib import Path

UNIT = 1_000_000
BPS = 10_000
DIRECTORY = Path(__file__).resolve().parent
PRINCIPAL = 100_000 * UNIT
SEED = 1_000 * UNIT
MONTHLY_BASE = 100_000 * UNIT


def insurance_allocation(amount, fund, target, knee_bps):
    """Pre-credit funding ratio; cap at target, allocate remainder to LPs."""
    assert amount >= 0 and fund >= 0 and target > 0 and 0 <= knee_bps < BPS
    knee = target * knee_bps // BPS
    if fund >= target:
        return 0, amount
    allocation = amount if fund <= knee else amount * (target - fund) // (target - knee)
    insurance = min(allocation, target - fund)
    return insurance, amount - insurance


def funding_case(target_bps, levy_bps, knee_bps):
    target = PRINCIPAL * target_bps // BPS
    fund, lp, collected = SEED, 0, 0
    gross_monthly = MONTHLY_BASE * levy_bps // BPS
    day_95 = None
    at_12 = at_24 = None
    for day in range(1, 7201):
        # Exactly the specified monthly fee revenue across each 30-day month.
        fee = gross_monthly * day // 30 - gross_monthly * (day - 1) // 30
        credit, lp_credit = insurance_allocation(fee, fund, target, knee_bps)
        fund += credit
        collected += fee
        lp += lp_credit
        assert SEED + collected == fund + lp
        assert SEED <= fund <= target
        if day_95 is None and fund * 100 >= target * 95:
            day_95 = day
        if day == 360:
            at_12 = (fund, lp)
        if day == 720:
            at_24 = (fund, lp)
        if day >= 720 and day_95 is not None:
            break
    assert at_12 is not None and at_24 is not None and day_95 is not None
    return dict(target_bps=target_bps, levy_bps=levy_bps, knee_bps=knee_bps,
                target=target, monthly_fee=gross_monthly, day_95=day_95,
                fund_12=at_12[0], lp_12=at_12[1], fund_24=at_24[0], lp_24=at_24[1])


@dataclass
class Fund:
    available: int
    event_bps: int
    day_bps: int
    day: int = -1
    opening: int = 0
    credited: int = 0
    drawn: int = 0
    coverage_bps: int = BPS

    def checkpoint(self, day):
        if self.day != day:
            self.day, self.opening, self.credited, self.drawn = day, self.available, 0, 0

    def credit(self, amount, day):
        self.checkpoint(day)
        self.available += amount
        self.credited += amount

    def cover(self, shortfall, day):
        self.checkpoint(day)
        event_limit = self.available * self.event_bps // BPS
        remaining = max(0, (self.opening + self.credited) * self.day_bps // BPS - self.drawn)
        covered_loss = shortfall * self.coverage_bps // BPS
        draw = min(covered_loss, self.available, event_limit, remaining)
        self.available -= draw
        self.drawn += draw
        return draw


POLICIES = {
    "current_20_event_50_day": (2000, 5000, 10000),
    "shared_50_day": (10000, 5000, 10000),
    "shared_100_day": (10000, 10000, 10000),
    "coverage_75_shared_50_day": (10000, 5000, 7500),
}
ORIGINAL_POLICIES = tuple(POLICIES)[:3]
CANDIDATE = "coverage_75_shared_50_day"
SHOCKS = {
    "single_2500": [(0, 2500)],
    "split_5x500": [(0, 500)] * 5,
    "cluster_5x1000": [(0, 1000)] * 5,
    "two_days_2500_each": [(0, 2500), (1, 2500)],
    "severe_20000": [(0, 20000)],
    "borrow_leverage_hlp_2000_2000_1000": [(0, 2000), (0, 2000), (0, 1000)],
}


def loss_case(target_bps, policy, shock):
    initial = PRINCIPAL * target_bps // BPS
    event_bps, day_bps, coverage_bps = POLICIES[policy]
    fund = Fund(initial, event_bps, day_bps, coverage_bps=coverage_bps)
    requested = covered = 0
    for day, loss in SHOCKS[shock]:
        requested += loss * UNIT
        covered += fund.cover(loss * UNIT, day)
    assert initial == covered + fund.available
    assert 0 <= covered <= requested
    return dict(target_bps=target_bps, policy=policy, shock=shock, initial=initial,
                requested=requested, covered=covered, uncovered=requested-covered,
                remaining=fund.available)


def validate():
    # Conservation and monotonic taper across all tested integer funding ratios.
    for target in (1, 2_000 * UNIT, 5_000 * UNIT, 10_000 * UNIT):
        for knee in (5000, 7500, 9000):
            previous = UNIT
            for ratio in range(0, 121):
                f = target * ratio // 100
                credit, lp = insurance_allocation(UNIT, f, target, knee)
                assert credit + lp == UNIT and 0 <= credit <= max(0, target-f)
                assert credit <= previous
                previous = credit
    # Hand-calculated native draw-policy anchors: current percentage-of-balance
    # event cap, opening-plus-credits daily basis, and next-window reset.
    f = Fund(1000 * UNIT, 2000, 5000)
    assert f.cover(1000 * UNIT, 0) == 200 * UNIT
    assert f.cover(1000 * UNIT, 0) == 160 * UNIT
    assert f.cover(1000 * UNIT, 0) == 128 * UNIT
    assert f.cover(1000 * UNIT, 0) == 12 * UNIT
    assert f.cover(1000 * UNIT, 0) == 0
    f.credit(200 * UNIT, 0)
    assert f.cover(1000 * UNIT, 0) == 100 * UNIT
    assert f.cover(1000 * UNIT, 1) == 120 * UNIT
    for policy in ("shared_50_day", "shared_100_day", CANDIDATE):
        for target in (200, 500, 1000):
            assert loss_case(target, policy, "single_2500")["covered"] == loss_case(target, policy, "split_5x500")["covered"]
    for target in (200, 500, 1000):
        for shock in SHOCKS:
            paid = [loss_case(target, policy, shock)["covered"] for policy in ORIGINAL_POLICIES]
            assert paid == sorted(paid)
            candidate = loss_case(target, CANDIDATE, shock)
            assert candidate["covered"] <= candidate["requested"] * 7500 // BPS
            assert candidate["covered"] <= loss_case(target, "shared_50_day", shock)["covered"]
    # Loss sharing and aggregate exhaustion: a new position cannot reset the budget.
    f = Fund(5000 * UNIT, BPS, 5000, coverage_bps=7500)
    assert [f.cover(1000 * UNIT, 0) for _ in range(5)] == [750 * UNIT] * 3 + [250 * UNIT, 0]
    assert f.available == 2500 * UNIT
    f.credit(200 * UNIT, 0)
    assert f.cover(1000 * UNIT, 0) == 100 * UNIT
    # The next complete window resets against the then-current balance, not the original seed.
    assert f.cover(2500 * UNIT, 1) == 1300 * UNIT
    # Split claims can lose atom fractions through rounding; never gain coverage.
    for total in range(1, 33):
        whole = Fund(1000, BPS, 5000, coverage_bps=7500).cover(total, 0)
        for first in range(total + 1):
            f = Fund(1000, BPS, 5000, coverage_bps=7500)
            split = f.cover(first, 0) + f.cover(total-first, 0)
            assert 0 <= whole-split <= 1


def money(atoms):
    return f"{atoms / UNIT:,.2f}"


def write_csv(name, rows):
    with (DIRECTORY / name).open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=rows[0].keys(), lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)


def main():
    validate()
    funding = [funding_case(t, fee, knee) for t in (200, 500, 1000)
               for fee in (10, 20, 50) for knee in (5000, 7500, 9000)]
    losses = [loss_case(t, policy, shock) for t in (200, 500, 1000)
              for policy in POLICIES for shock in SHOCKS]
    assert len(funding) == 27 and len(losses) == 72
    write_csv("insurance-funding-sensitivity.csv", funding)
    write_csv("insurance-draw-sensitivity.csv", losses)
    report = [
        "# Insurance funding and spending sensitivity", "",
        "2026-10-09 · PR #45 design analysis · 27 funding cases and 72 loss-budget cases. "
        "No runtime change. Values are hypothetical USDC amounts, with one USDC treated as $1. "
        "This is a small deterministic budget study, not actuarial calibration, a loss forecast, "
        "or a replay of the revised liquidation mechanism.", "",
        "**2026-10-10 decision update:** the user selected a 1% total charge on net "
        "debt repaid, split as 0.8% to insurance/LPs and 0.2% to protocol. The user "
        "confirmed that the 0.8% allocation supplies insurance through the target-based "
        "taper and sends its remainder to LPs; the protocol allocation stays separate. "
        "Target and taper parameters remain unselected. The historical "
        "funding cases below do not model this split; neither the full 1% nor the "
        "protocol's 0.2% may be assumed to fund insurance. The ordinary buyer discount "
        "and emergency caller reward remain unchanged.", "",
        "## Historical funding candidates and current spending direction", "",
        "- Test a **5% target against covered borrowing/leverage principal per debt token**, "
        "plus a separately calibrated allowance for hLP claims sharing that vault and a minimum "
        "seed. Compare 2% and 10%; 5% is a middle capital allocation, not a proven sufficient buffer.",
        "- Allocate all designated realized fee income to insurance until **75% of target**, "
        "then taper linearly to zero at target. Direct the remainder to the designated LP "
        "fee recipients; don't automatically distribute the existing fund when the target falls.",
        "- The historical **0.2% secondary liquidation-levy candidate** was superseded "
        "by the selected 1% total charge and split above. The older assumed insurance "
        "fee flow replenishes slowly at the illustrative turnover below. "
        "Initial seed and any allocation from other realized fees require a budget/source decision.",
        "- The user's latest spending candidate is **up to 75% of eligible losses, subject to "
        "a shared 50% fund budget**. The user confirmed that this shared budget "
        "refreshes every 24 hours against the remaining insurance balance. "
        "It replaces the existing 20%-of-fund per-event cap. LPs bear at least 25% of "
        "eligible borrower/leverage principal losses, and more when the shared cap binds. "
        "The user subsequently affirmed this spending direction: 75% principal-loss "
        "coverage within the 50% budget and 24-hour refresh. These rules are not "
        "implemented yet; the funding target, taper parameters and hLP claim terms "
        "remain unselected.", "",
        "## Assumptions and units", "",
        "Funding cases hold protected borrowing/leverage principal at 100,000 USDC, seed the "
        "fund with 1,000 USDC, and supply 100,000 USDC of eligible fee base per 30-day month. "
        "At 0.2% that yields 200 USDC/month before allocation. These are scenario inputs, "
        "not estimates of Dusk activity or universal token-denominated constants. Every fee "
        "base unit is assumed collectible without creating a shortfall; real solvent-surplus "
        "caps can reduce collection. No extra charge is taken on an insolvent full liquidation.", "",
        "Each loss case starts independently with a fully funded target, assumes no concurrent "
        "fee income and uses the stated shortfall after collateral recovery/rewards. The study "
        "does not derive these losses from collateral prices or count debt repayment as fee "
        "income. Losses are deterministic fixtures, not probabilities. Funding and loss tables "
        "must not be combined as if the seeded fund were already fully funded.", "",
        "The budget uses raw atoms in the covered debt mint, avoiding a cross-token dollar "
        "oracle. No token transfer fee is modeled; real draw accounting must distinguish "
        "gross vault debit from spendable debt credit. The proposed 75% targets net loss "
        "coverage; the shared fund budget limits gross debit, so transfer fees can reduce "
        "achievable coverage. No interest is covered in the "
        "borrowing/leverage shortfall examples. Existing hLP terminal funding claims remain "
        "a separate claim type; this study does not remove or redefine them.", "",
        "## Funding results", "",
        "All fund allocations use the pre-credit balance, clamp to the target, and preserve "
        "`seed + collected fees = insurance balance + LP allocation`. The 30-day fee flow is "
        "credited daily. A linear taper approaches the target asymptotically, so report time "
        "to 95%, not a promise of reaching 100% exactly.", "",
        "### Target size at 0.2% levy and 75% taper start", "",
        "| Target / principal | Target | Insurance after 12 months | LP allocation after 12 months | Months to 95% funded |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for r in funding:
        if r["levy_bps"] == 20 and r["knee_bps"] == 7500:
            report.append(f"| {r['target_bps']/100:g}% | {money(r['target'])} | {money(r['fund_12'])} | "
                          f"{money(r['lp_12'])} | {r['day_95']/30:.1f} |")
    report += ["", "### Replenishment tradeoff at a 5% target", "",
               "| Levy on eligible base | Monthly designated fee income | Taper starts at | Months to 95% funded | Insurance after 12 months | LP allocation after 12 months |",
               "| --- | ---: | --- | ---: | ---: | ---: |"]
    for r in funding:
        if r["target_bps"] == 500 and (r["levy_bps"] == 20 or r["knee_bps"] == 7500):
            report.append(f"| {r['levy_bps']/100:g}% | {money(r['monthly_fee'])} | {r['knee_bps']/100:g}% | "
                          f"{r['day_95']/30:.1f} | {money(r['fund_12'])} | {money(r['lp_12'])} |")
    report += ["", "Starting the taper at 50% gives LPs income sooner but lengthens the "
               "underfunded period. Increasing the levy speeds funding by charging more on "
               "eligible liquidations; allocating existing fee revenue instead reduces another "
               "recipient's yield. These are real transfers, not new yield creation.", "",
               "### Proposed allocation curve at a 5,000 target", "",
               "| Insurance balance | Share to insurance | Share to LPs |",
               "| --- | ---: | ---: |"]
    for f in (0, 2500, 3750, 4375, 5000):
        credit, lp = insurance_allocation(100 * UNIT, f * UNIT, 5000 * UNIT, 7500)
        report.append(f"| {f:,} | {credit/UNIT:g}% | {lp/UNIT:g}% |")
    report += ["", "## Spending results", "",
               "`current_20_event_50_day` mirrors the current native budget arithmetic: "
               "20% of current balance per event and 50% of opening balance plus credited "
               "income in the draw window. The alternatives remove the percentage-per-event "
               "limit and retain 50% or allow 100% of that shared basis. Day boundaries below "
               "mean a complete accounting window has elapsed; they are not UTC-midnight resets.", "",
               "### Same 2,500 shortfall, different fund targets", "",
               "| Fund at target | Current: covered / uncovered | Shared 50%: covered / uncovered | Shared 100%: covered / uncovered |",
               "| ---: | --- | --- | --- |"]
    for t in (200, 500, 1000):
        selected = [loss_case(t, p, "single_2500") for p in ORIGINAL_POLICIES]
        report.append(f"| {money(selected[0]['initial'])} | " + " | ".join(
            f"{money(r['covered'])} / {money(r['uncovered'])}" for r in selected) + " |")
    report += ["", "### A 5,000 fund: what the spending policy preserves or spends", "",
               "Each cell is **covered / uncovered / fund remaining**. Uncovered borrowing "
               "and leverage principal must complete the selected write-off waterfall; it "
               "does not remain as a debt on a fully settled leg. hLP claims follow their "
               "own terminal loss accounting. No unpaid interest becomes new fee yield.", "",
               "| Loss sequence | Current 20% event / 50% window | Shared 50% | Shared 100% |",
               "| --- | --- | --- | --- |"]
    for shock in SHOCKS:
        selected = [loss_case(500, p, shock) for p in ORIGINAL_POLICIES]
        report.append(f"| {shock} | " + " | ".join(
            " / ".join(money(r[k]) for k in ("covered", "uncovered", "remaining"))
            for r in selected) + " |")
    report += ["", "The single 2,500 loss versus five 500 losses demonstrates that the existing "
               "per-event percentage makes coverage sensitive to position splitting. The "
               "shared-budget alternatives are invariant to those same-window splits, given "
               "identical eligible shortfalls, no incoming funds and unchanged parameters. "
               "A daily limit still creates ordering competition for a finite shared fund; "
               "this is not a proof against fabricated or manipulated losses.", "",
               "### User candidate: 75% of loss, capped by a shared 50% fund budget", "",
               "For the no-transfer-fee examples: `payment = min(0.75 * eligible_loss, "
               "available_fund, max(0, 0.50 * (window_opening_fund + credited_inflows) "
               "- already_drawn))`. All operations sharing this debt-token fund consume "
               "the same window budget. This is not 50% of the remaining balance allowed "
               "afresh on each liquidation. No additional 20%-of-fund per-event cap is "
               "applied in this candidate.", "",
               "Each row below starts with a fresh 5,000 fund and unused 2,500 budget. "
               "The rows are independent, not successive draws.", "",
               "| Principal shortfall | Insurance payment | Principal written off | Fund remaining |",
               "| ---: | ---: | ---: | ---: |"]
    for shortfall in (1000, 2500, 5000):
        f = Fund(5000 * UNIT, BPS, 5000, coverage_bps=7500)
        paid = f.cover(shortfall * UNIT, 0)
        report.append(f"| {shortfall:,} | {money(paid)} | {money(shortfall*UNIT-paid)} | {money(f.available)} |")
    report += ["", "Repeated 1,000 principal losses in one window receive 750, 750, 750, "
               "250, then 0. The 2,500 aggregate cap binds on the fourth loss; 2,500 "
               "remains in the vault. In the next complete accounting window, with no "
               "credits, the new budget is 50% of that remaining 2,500 = 1,250. The "
               "preserved half is not permanently untouchable.", "",
               "| Loss sequence | Candidate covered | Uncovered | Fund remaining |",
               "| --- | ---: | ---: | ---: |"]
    for shock in SHOCKS:
        r = loss_case(500, CANDIDATE, shock)
        report.append(f"| {shock} | {money(r['covered'])} | {money(r['uncovered'])} | {money(r['remaining'])} |")
    report += ["", "For one 2,500 principal loss or five 500 losses, candidate coverage is "
               "1,875 either way. Raw-atom rounding can make split coverage slightly "
               "smaller, never larger in the tested same-window fixed-parameter examples. "
               "A loss must not be claimed again on the uninsured remainder: full "
               "settlement covers once, writes off the rest and clears that debt leg.", "",
               "The mixed borrowing/leverage/hLP fixture applies the percentage uniformly "
               "only as a budget sensitivity. It does not select a 75% payout rule for "
               "hLP funding-interest claims. Their entitlement must be explicit while "
               "sharing the aggregate draw budget; existing protection must not silently "
               "change through a generic insurance helper.", "",
               "## hLP and target stability", "",
               "Source inspection confirms that hLP terminal settlement also calls the same "
               "insurance draw-capacity function. Its present shortfall is bounded by funding "
               "interest, not the borrowing/leverage principal-only policy. Therefore a target "
               "based only on external-loan principal omits an existing insurance claimant. "
               "A candidate definition is `target = max(minimum_seed, q * covered_principal + "
               "hLP_loss_allowance)`, with the allowance estimated from hLP stress and no "
               "double counting. The funding table holds that extra allowance at zero solely "
               "to isolate the selected principal-target sensitivity.", "",
               "For 100,000 protected principal and q=5%, hLP allowances of 0 / 1,000 / 5,000 "
               "produce targets of 5,000 / 6,000 / 10,000. A current 5,000 balance is respectively "
               "100% / 83.33% / 50% funded. Those allowances are examples, not loss estimates.", "",
               "The implementation must reserve pending insurance obligations and avoid an "
               "instantaneous manipulable denominator. Capture pre-liquidation exposure for "
               "the fee quote; bind its amount/allocation through atomic settlement, and use "
               "a justified observation/target adjustment rule across transactions. A same-slot "
               "borrow/repay or LP withdrawal must not cheaply redirect funding or release the "
               "existing fund. Target decreases affect future fee allocation only. Shared "
               "hLP/borrow/leverage ordering and draw windows need native instruction tests.", "",
               "## Computational cost and model limits", "",
               "The proposed funding split needs bounded integer arithmetic: compare to knee "
               "and target, multiply by remaining target gap, divide by taper width, clamp. "
               "It needs no transcendental calculation or scan of every position. Aggregate "
               "insured exposure, reservations and target observation still require accounting "
               "state and measured compute tests; cheap math is not zero-cost implementation.", "",
               "The pre-credit allocation rule is sensitive to fee batching inside the taper. "
               "Production rounding, sub-atom carry and split/merge incentives need an explicit "
               "policy; this study uses equal daily credits. No price manipulation, future "
               "liquidity, fee elasticity, keeper behavior, interest accrual, token CPIs or "
               "revised time-based liquidation outcomes are simulated. The 5% target and "
               "75% knee are proposals for integrated testing, not sufficiency guarantees.", "",
               "## Validation and reproduction", "",
               "99 deterministic cases. Assertions cover cash conservation, bounded/monotone "
               "allocation, native-budget arithmetic anchors, new credits, window reset, "
               "same-window splitting under shared caps, and increasing coverage with relaxed "
               "caps, 75% loss sharing, aggregate cap exhaustion and sub-atom splitting. "
               "This Python study is not an executable native-code parity test.", "",
               "```sh", "PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/insurance_policy_analysis.py", "```", "",
               "Raw amounts are USDC micro-units: [funding CSV](insurance-funding-sensitivity.csv), "
               "[draw CSV](insurance-draw-sensitivity.csv). No build products are required.", "",
               "Source anchors: `programs/dusk/src/transitions/ledger.rs` (`Insurance::draw_capacity`), "
               "`programs/dusk/src/constants.rs` (20% / 50% ceilings), "
               "`programs/dusk/src/transitions/liquidity/hlp/engine.rs` (shared terminal claims)."]
    (DIRECTORY / "INSURANCE_POLICY_RESULTS.md").write_text("\n".join(report) + "\n")
    print(f"Validated {len(funding)} funding cases and {len(losses)} loss-budget cases")
    for r in funding:
        if r["levy_bps"] == 20 and r["knee_bps"] == 7500:
            print(f"target={r['target_bps']/100:g}%: fund after 12 months={money(r['fund_12'])}; months to 95%={r['day_95']/30:.1f}")


if __name__ == "__main__":
    main()

"""Exploratory model of the 2026-10-05 decisions; NOT on-chain calibration.

Shares the historical model's CPMM arithmetic and principal-only insurance caps.
There are no distress clocks or cash rewards on ordinary flash purchases. Prices
are sampled on a scenario grid, which is not a protocol waiting period. Native
concentrated execution, transfer fees, hLP, borrowing and concurrent transaction
composition still require their own integration/calibration. No external PoC.
"""

from dataclasses import dataclass
from itertools import product
from pathlib import Path
import copy
import math

from emergency_model import EPS, Pool, Insurance, band_amount, health, save_csv


@dataclass(frozen=True)
class Terms:
    rates: tuple = (0.07, 0.08, 0.10)
    bands: tuple = (0.20, 0.60)
    entry_buffer: float = 0.03
    recovery_buffer: float = 0.02
    free_exposure: float = 0.20
    crowding_slope: float = 0.10
    minimum_discount: float = 0.005
    maximum_discount: float = 0.03
    critical_fraction: float = 0.5
    insurance_rate: float = 0.002
    # The proceeds-share mechanism is approved; this numeric cap is a candidate.
    emergency_reward_max: float = 0.01

    def maintenance(self, collateral, stored_depth):
        if collateral <= EPS:
            return 0
        return band_amount(collateral, tuple(b * stored_depth for b in self.bands), self.rates) / collateral

    def discount(self, equity, maintenance):
        severity = min(1, max(0, (maintenance - equity) / maintenance))
        return self.minimum_discount + (self.maximum_discount - self.minimum_discount) * severity


def principal_first(principal, interest, payment, insurance, funding_rate):
    principal_paid = min(principal, payment)
    interest_paid = min(interest, max(0, payment - principal))
    residual = max(0, payment - principal - interest)
    contribution = min(residual, (principal + interest) * funding_rate)
    insurance.credited += contribution
    covered = insurance.cover(principal - principal_paid)
    return dict(principal_paid=principal_paid, interest_paid=interest_paid,
                canceled_interest=interest - interest_paid, insurance=covered,
                loss=max(0, principal - principal_paid - covered),
                contribution=contribution, owner=residual - contribution)


def partial(collateral, debt, depth, reference, pool, outside, terms, cost):
    """Select on a 1% grid; permit incremental repair with a fixed EMA snapshot.

    Prefer the first fill reaching the target; otherwise the largest useful fill
    below it. This deliberately exposes grid/size limits rather than claiming an
    exact on-chain maximum seizure algorithm.
    """
    before = health(collateral * reference, debt)
    mm = terms.maintenance(collateral, depth)
    discount = terms.discount(before, mm)
    best = None
    for percent in range(1, 100):
        sold = collateral * percent / 100
        output, venue = max([(pool.quote(sold), "dusk")]
                            + ([(outside.quote(sold), "outside")] if outside else []))
        payment = sold * reference * (1 - discount)
        contribution = payment * terms.insurance_rate
        repay = payment - contribution
        if repay > debt or output - payment < cost:
            continue
        left = collateral - sold
        after = health(left * reference, debt - repay)
        after_mm = terms.maintenance(left, depth)
        # No insurance subsidy or negative remaining equity for partial repair.
        if after < 0 or after <= before + EPS or after - after_mm <= before - mm + EPS:
            continue
        best = dict(sold=sold, payment=payment, repay=repay,
                    contribution=contribution, profit=output-payment-cost, venue=venue,
                    reached_target=after >= after_mm + terms.recovery_buffer,
                    health_before=before, health_after=after)
        if best["reached_target"]:
            return best
    return best


def simulate(terms=Terms(), tvl=100_000, pieces=10, spending_ratio=0.15,
             wallet_fraction=0.5, outside_depth=0, path="fast", withdrawal=0,
             half_life=60, fixed_cost=0.05, max_steps=120):
    pool = Pool(tvl / 2, tvl / 2)
    positions = []
    spending = tvl * spending_ratio / pieces
    for _ in range(pieces):
        trial = copy.copy(pool)
        debt = spending * (1 - wallet_fraction)
        if debt > pool.y - sum(p[1] for p in positions):
            break
        collateral = trial.buy(spending)
        depth = min(tvl / 2, trial.x)
        crowding = (sum(p[0] for p in positions) + collateral) / depth
        required = max(terms.maintenance(collateral, depth) + terms.entry_buffer,
                       0.10 + terms.crowding_slope * max(0, crowding - terms.free_exposure))
        if min(health(collateral, debt), health(trial.quote(collateral), debt)) < required:
            break
        positions.append([collateral, debt, depth])
        pool = trial
    insurance = Insurance(tvl * 0.01)
    original_debt = sum(p[1] for p in positions)
    stats = dict(opened=len(positions), original_debt=original_debt, partials=0,
                 flash_full=0, emergency_full=0, loss=0.0, buyer_profit=0.0,
                 emergency_reward=0.0, insurance_contribution=0.0, owner_value=0.0)
    # Match the historical model: entry price impact is admission cost; unrelated
    # price discovery resets price before the scenario. Dusk has no restorative
    # arbitrage after this reset. Outside depth reprices only between samples.
    pool.x = pool.y = math.sqrt(pool.x * pool.y)
    outside = Pool(tvl / 2 * outside_depth, tvl / 2 * outside_depth) if outside_depth else None
    reference = cached = world = 1.0
    dt = 60 if path == "slow" else 10
    for step in range(1, max_steps + 1):
        reference = cached + (reference - cached) * 2 ** (-dt / half_life)
        if step == 2:
            pool.x *= 1 - withdrawal
            pool.y *= 1 - withdrawal
        multiplier = (0.995 if path == "slow" else 0.97)
        if path == "gap_flat":
            multiplier = 0.55 if step == 1 else 1
        sale = pool.x * (1 / math.sqrt(multiplier) - 1) / (1 - pool.fee)
        if pool.quote(sale) > pool.y - sum(p[1] for p in positions) + EPS:
            raise ValueError("Stress trade exceeds physical cash; native cash constraints needed")
        pool.sell(sale)
        world *= multiplier
        if outside:
            outside.x = math.sqrt(outside.x * outside.y / world)
            outside.y = outside.x * world
        survivors = []
        for collateral, debt, depth in positions:
            # Racing keepers may fill repeatedly at the same observation. The
            # sampling grid must not introduce an artificial liquidation wait.
            while True:
                h = health(collateral * reference, debt)
                mm = terms.maintenance(collateral, depth)
                if h > mm:
                    survivors.append([collateral, debt, depth])
                    break
                fill = partial(collateral, debt, depth, reference, pool, outside, terms, fixed_cost)
                if fill:
                    (pool if fill["venue"] == "dusk" else outside).sell(fill["sold"])
                    insurance.credited += fill["contribution"]
                    stats["insurance_contribution"] += fill["contribution"]
                    stats["partials"] += 1
                    stats["buyer_profit"] += fill["profit"]
                    collateral -= fill["sold"]
                    debt -= fill["repay"]
                    continue
                output, venue = max([(pool.quote(collateral), "dusk")]
                                    + ([(outside.quote(collateral), "outside")] if outside else []))
                payment = collateral * reference * (1 - terms.discount(h, mm))
                if output - payment >= fixed_cost:
                    stats["flash_full"] += 1
                    stats["buyer_profit"] += output - payment - fixed_cost
                elif h <= mm * terms.critical_fraction:
                    venue = "dusk"
                    output = pool.quote(collateral)
                    severity = min(1, max(0, (mm - h) / mm))
                    reward = output * terms.emergency_reward_max * severity
                    if reward < fixed_cost:
                        survivors.append([collateral, debt, depth])
                        break
                    payment = output - reward
                    stats["emergency_reward"] += reward
                    stats["emergency_full"] += 1
                else:
                    survivors.append([collateral, debt, depth])
                    break
                (pool if venue == "dusk" else outside).sell(collateral)
                result = principal_first(debt, 0, payment, insurance, terms.insurance_rate)
                pool.y -= result["loss"]
                stats["loss"] += result["loss"]
                stats["insurance_contribution"] += result["contribution"]
                stats["owner_value"] += result["owner"]
                break
        positions = survivors
        if pool.y + EPS < sum(p[1] for p in positions):
            raise ValueError("Remaining debt exceeds physical backing")
        cached = pool.price
        if not positions:
            break
    return dict(tvl=tvl, pieces=pieces, spending_ratio=spending_ratio,
                outside_depth=outside_depth, path=path, withdrawal=withdrawal,
                rates="/".join(str(r) for r in terms.rates), bands="/".join(str(b) for b in terms.bands),
                critical_fraction=terms.critical_fraction,
                emergency_reward_max=terms.emergency_reward_max, **stats,
                loss_fraction=stats["loss"] / original_debt if original_debt else 0,
                insurance_drawn=insurance.drawn, remaining_debt=sum(p[1] for p in positions),
                remaining_positions=len(positions),
                eligible_remaining=sum(health(c * reference, d) <= terms.maintenance(c, depth)
                                       for c, d, depth in positions))


if __name__ == "__main__":
    rows = []
    for (rates, bands), pieces, outside, path, critical in product(
            [((0.07, 0.08, 0.10), (0.20, 0.60)),
             ((0.07, 0.12, 0.30), (0.05, 0.15)),
             ((0.07, 0.20, 0.40), (0.05, 0.15)),
             ((0.07, 0.25, 0.45), (0.05, 0.15))],
            [1, 10], [0, 1, 5], ["slow", "fast", "gap_flat"], [0.25, 0.5, 0.75]):
        rows.append(simulate(terms=Terms(rates=rates, bands=bands, critical_fraction=critical),
                             pieces=pieces, outside_depth=outside, path=path))
    directory = Path(__file__).resolve().parent
    save_csv(directory / "flash-purchase-candidates.csv", rows)
    report = ["# Flash purchase candidates — preliminary", "",
              "2026-10-05. Exploratory CPMM results; no runtime settings selected.", "",
              "## Selected-policy assumptions", "",
              "EMA-only ordinary eligibility, stored-depth marginal MM, IM >= MM + 3 points plus crowding; "
              "partial fills may improve health without fully restoring the target. Fixed-payment buyers "
              "keep execution upside. Critical EMA equity permits internal emergency execution without a timer.", "",
              "Candidate inputs: 0.5–3% health-based discount; 0.2% solvent insurance contribution; "
              "internal caller reward up to 1% of proceeds (mechanism approved, numeric cap unselected). "
              "Compare old 20%/60% depth bands with steeper rates above 5%/15% of stored collateral-side depth. "
              "These are experiments, not defaults. All candidates retain a 7% first-band maintenance rate.", "",
              "## Matched request: $100k pool, $15k total entry spending, 50% wallet equity", "",
              "Rows use critical equity = half maintenance and no outside venue. A fast sample is a "
              "3% spot decline every ten seconds with a 60-second EMA; slow is 0.5% per minute. "
              "These are stress assumptions, not forecasts. Different admission counts require care.", "",
              "| MM rates (depth bands) | Positions requested/opened | Path | Admitted debt | Partials | Emergency sales | "
              "Principal loss after insurance | Insurance drawn | Unresolved eligible |",
              "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for r in rows:
        if r["critical_fraction"] == 0.5 and r["outside_depth"] == 0 and r["path"] != "gap_flat":
            report.append(f'| {r["rates"]} ({r["bands"]}) | {r["pieces"]}/{r["opened"]} | {r["path"]} | '
                          f'{r["original_debt"]:.2f} | {r["partials"]} | {r["emergency_full"]} | '
                          f'{r["loss"]:.2f} ({r["loss_fraction"]:.2%}) | {r["insurance_drawn"]:.2f} | '
                          f'{r["eligible_remaining"]} |')
    report += ["", "## Limits before selecting defaults", "",
               "This model shares the previous independent CPMM formulas, not a new native implementation. "
               "It omits token transfer fees, accrued interest in the sweep, concentrated curves, hLP, public "
               "borrowing, adversarial price/depth additions, instruction locks and cross-position composition. "
               "Insurance starts at 1% of pool value, with 20% event / 50% window caps. The debt side includes "
               "principal receivables; cash constraints are checked. A partial search uses a 1% grid; "
               "keepers may execute repeated useful fills at each observation. No claim of best execution, exact minimal seizure, or guaranteed "
               "cleanup follows from these results. Fees and numerical thresholds require further native coverage.", "",
               "The larger-MM candidates deliberately show the leverage cost of earlier intervention. "
               "Neither their loss rates nor the gentle schedule have an approved loss budget. "
               "Do not remove the existing admission cap based only on this preliminary comparison."]
    (directory / "FLASH_PURCHASE_RESULTS.md").write_text("\n".join(report) + "\n")
    print(f"Wrote {len(rows)} candidate scenarios; no protocol code or defaults changed.")

"""Exploratory CPMM economics, not executable protocol or a transaction simulator.

Run: python3 docs/calibration/emergency_model.py --native-log /path/to/native.log
No network, signing, deployed program calls, or historical exploit PoC execution.
Amounts use debt-token units and floating-point economic arithmetic. Native
anchors use integer atoms; production must use checked integers and real fees.
"""

from __future__ import annotations

import argparse
import copy
import csv
from dataclasses import dataclass, field
from itertools import product
import math
from pathlib import Path

EPS = 1e-8
SCHEDULES = ("fixed", "depth_gentle", "depth_earlier", "stack_earlier")


def band_amount(size, boundaries, rates):
    a, b = boundaries
    return (min(size, a) * rates[0]
            + min(max(size - a, 0), b - a) * rates[1]
            + max(size - b, 0) * rates[2])


def depth_amount(size, depth, earlier):
    bands = (0.1 * depth, 0.3 * depth) if earlier else (0.2 * depth, 0.6 * depth)
    rates = (0.1, 0.2, 0.35) if earlier else (0.07, 0.08, 0.1)
    return band_amount(size, bands, rates)


@dataclass
class Clock:
    last: float | None = None
    age: float = 0

    def observe(self, now, unhealthy, max_gap):
        if not unhealthy:
            self.last, self.age = None, 0
        elif self.last is None or now - self.last > max_gap:
            self.last, self.age = now, 0
        else:
            assert now >= self.last
            self.age += now - self.last
            self.last = now


@dataclass
class Pool:
    x: float  # collateral curve reserve
    y: float  # debt curve reserve, INCLUDING principal receivables for Dusk
    fee: float = 0.003  # input fee removed from this economic curve

    @property
    def price(self):
        return self.y / self.x

    def quote(self, amount):
        credited = amount * (1 - self.fee)
        return self.y * credited / (self.x + credited)

    def sell(self, amount):
        output = self.quote(amount)
        self.x += amount * (1 - self.fee)
        self.y -= output
        return output

    def buy(self, spending):
        credited = spending * (1 - self.fee)
        output = self.x * credited / (self.y + credited)
        self.y += credited
        self.x -= output
        return output


@dataclass
class Insurance:
    opening: float
    credited: float = 0
    drawn: float = 0

    @property
    def balance(self):
        return self.opening + self.credited - self.drawn

    def cover(self, principal_shortfall):
        # Same maximum rate settings and basis as ledger.rs, one draw window.
        day_room = max(0, (self.opening + self.credited) * 0.5 - self.drawn)
        amount = min(principal_shortfall, self.balance * 0.2, day_room)
        self.drawn += amount
        return amount


def waterfall(principal, interest, output, reward_rate, insurance, funding_rate=0.02):
    reward = output * reward_rate
    recovery = output - reward
    principal_paid = min(principal, recovery)
    interest_paid = min(interest, max(0, recovery - principal))
    residual = max(0, recovery - principal - interest)
    covered = insurance.cover(principal - principal_paid)
    loss = max(0, principal - principal_paid - covered)
    contribution = min(residual, (principal + interest) * funding_rate)
    insurance.credited += contribution
    assert abs(output - reward - principal_paid - interest_paid - residual) < 1e-6
    return dict(reward=reward, principal_paid=principal_paid, interest_paid=interest_paid,
                insurance=covered, loss=loss, canceled_interest=interest-interest_paid,
                contribution=contribution, owner=residual-contribution)


@dataclass
class Position:
    collateral: float
    principal: float
    depth: float
    schedule: str
    stored_rate: float = 0
    clock: Clock = field(default_factory=Clock)
    first_distress: float | None = None
    last_quote: float | None = None

    def maintenance(self, reference, collateral=None):
        size = self.collateral if collateral is None else collateral
        if size <= EPS:
            return 0
        if self.schedule == "fixed":
            value = size * reference
            return band_amount(value, (10_000, 30_000), (0.07, 0.08, 0.1)) / value
        if self.schedule == "stack_earlier":
            return self.stored_rate
        return depth_amount(size, self.depth, self.schedule == "depth_earlier") / size


def health(value, debt):
    # Signed, unlike the runtime equity metric that saturates at zero.
    return 1 - debt / value if value > EPS else -math.inf


def terms(h, maintenance, age):
    severity = min(1, max(0, (maintenance - h) / max(maintenance, EPS)))
    aging = min(1, age / 60)
    reward = min(0.03, 0.005 + 0.025 * severity + 0.005 * aging)
    discount = min(0.05, 0.005 + 0.02 * severity + 0.015 * aging)
    return reward, discount


def route_quote(pool, outside, amount):
    options = [(pool.quote(amount), "dusk")]
    if outside is not None:
        options.append((outside.quote(amount), "outside"))
    return max(options)


def partial_candidate(position, pool, outside, reference, reward, discount, delivery, cost):
    # Exhaustive 5% grid: deliberately not an exact minimal-repair claim.
    for sold_bps in range(500, 10_000, 500):
        sold = position.collateral * sold_bps / 10_000
        actual, venue = route_quote(pool, outside, sold)
        floor = sold * reference * (1 - discount)
        if actual + EPS < floor:
            continue
        credit = floor if delivery == "floor" else actual
        repay = credit * (1 - reward)
        if actual - credit + credit * reward < cost:
            continue
        left = position.collateral - sold
        debt_left = max(0, position.principal - repay)
        target = position.maintenance(reference, left) + 0.02
        if health(left * reference, debt_left) < target:
            continue
        post_pool = copy.copy(pool)
        if venue == "dusk":
            post_pool.sell(sold)
        if health(post_pool.quote(left), debt_left) >= target:
            return sold, actual, credit, venue
    return None


def simulate(schedule="depth_earlier", im_rule="buffer", tvl=100_000, pieces=10,
             exposure=0.3, outside_depth=0, path="fast", half_life=60,
             terminal_age=60, confirm_age=15, withdrawal=0, delivery="floor",
             fixed_cost=0.05, strict_floor=False, wallet_fraction=0.5,
             free_band=0.2, im_slope=0.1, max_steps=120, critical_fraction=0,
             terminal_route="dusk"):
    settings = dict(schedule=schedule, im_rule=im_rule, tvl=tvl, pieces=pieces,
                    exposure=exposure, outside_depth=outside_depth, path=path,
                    half_life=half_life, terminal_age=terminal_age, confirm_age=confirm_age,
                    withdrawal=withdrawal, delivery=delivery, fixed_cost=fixed_cost,
                    strict_floor=strict_floor, wallet_fraction=wallet_fraction,
                    free_band=free_band, im_slope=im_slope,
                    critical_fraction=critical_fraction, terminal_route=terminal_route)
    pool = Pool(tvl / 2, tvl / 2)
    positions = []
    spending = tvl / 2 * exposure / pieces
    for _ in range(pieces):
        trial = copy.copy(pool)
        debt = spending * (1 - wallet_fraction)
        if debt > pool.y - sum(p.principal for p in positions):
            break
        collateral = trial.buy(spending)
        depth = min(tvl / 2, trial.x)
        before = sum(p.collateral for p in positions)
        stored = (depth_amount(before + collateral, depth, True)
                  - depth_amount(before, depth, True)) / collateral
        position = Position(collateral, debt, depth, schedule, stored)
        mm = position.maintenance(1)
        size_im = 2 * mm if im_rule == "double" else mm + 0.03
        crowded_im = 0.1 + im_slope * max(0, (before + collateral) / depth - free_band)
        required = max(size_im, crowded_im)
        if min(health(collateral, debt), health(trial.quote(collateral), debt)) < required:
            break
        positions.append(position)
        pool = trial
    opened = len(positions)
    original_debt = sum(p.principal for p in positions)
    initial_mm = sum(p.collateral * p.maintenance(1) for p in positions)
    initial_collateral = sum(p.collateral for p in positions)
    insurance = Insurance(tvl * 0.01)
    stats = dict(opened=opened, original_debt=original_debt,
                 entry_mm=initial_mm / initial_collateral if initial_collateral else 0,
                 partials=0, protected_full=0, guarded_full=0, terminal_full=0,
                 principal_loss=0.0, keeper_reward=0.0, hidden_route_surplus=0.0,
                 owner_residual=0.0, debt_free_collateral_reference=0.0, insurance_funding=0.0,
                 socialization_price_drop=0.0, max_resolution_seconds=0.0,
                 rejected_better_terminal_output=0.0)
    # Repricing after entry represents unrelated price discovery. The entry
    # impact was charged in admission. It is not counted as a liquidation loss.
    k = pool.x * pool.y
    pool.x = pool.y = math.sqrt(k)
    outside = Pool(tvl / 2 * outside_depth, tvl / 2 * outside_depth) if outside_depth else None
    reference = cached = world = 1.0
    dt = 60 if path == "slow" else 10
    for step in range(1, max_steps + 1):
        now = step * dt
        reference = cached + (reference - cached) * 2 ** (-dt / half_life)
        if step == 2 and withdrawal:
            # Proportional reserve shrink is a stress assumption, not proof
            # that all native LP-withdrawal/cash guards permit this withdrawal.
            pool.x *= 1 - withdrawal
            pool.y *= 1 - withdrawal
        multiplier = 0.995 if path == "slow" else 0.97
        if path == "gap_flat":
            multiplier = 0.55 if step == 1 else 1
        # Exogenous spot sales at this time step, with no inter-liquidation
        # arbitrage during the batch. Keep liquidation-induced reserve losses.
        sale = pool.x * (1 / math.sqrt(multiplier) - 1) / (1 - pool.fee)
        available_cash = pool.y - sum(p.principal for p in positions)
        assert pool.quote(sale) <= available_cash + 1e-6, "Requested stress trade exceeds physical cash"
        pool.sell(sale)
        world *= multiplier
        if outside is not None:
            # External venue is refreshed to the world price between steps;
            # finite depth is consumed by all fills WITHIN a step.
            outside_k = outside.x * outside.y
            outside.x = math.sqrt(outside_k / world)
            outside.y = outside.x * world
        survivors = []
        for p in positions:
            h = health(p.collateral * reference, p.principal)
            mm = p.maintenance(reference)
            p.clock.observe(now, h <= mm, dt * 2)
            full_quote = pool.quote(p.collateral)
            eligible = h <= mm and health(full_quote, p.principal) <= mm
            if not eligible:
                p.last_quote = full_quote
                survivors.append(p)
                continue
            if p.first_distress is None:
                p.first_distress = now
            reward, discount = terms(h, mm, p.clock.age)
            partial = partial_candidate(p, pool, outside, reference, reward, discount, delivery, fixed_cost)
            if partial:
                sold, actual, credit, venue = partial
                (pool if venue == "dusk" else outside).sell(sold)
                reward_paid = credit * reward
                repayment = min(p.principal, credit - reward_paid)
                p.collateral -= sold
                p.principal -= repayment
                stats["partials"] += 1
                stats["keeper_reward"] += reward_paid
                stats["hidden_route_surplus"] += actual - credit
                # Any over-repayment credit belongs to the owner; retain its
                # economics here, rather than silently dropping the remainder.
                stats["owner_residual"] += max(0, credit - reward_paid - repayment)
                p.clock.observe(now, health(p.collateral * reference, p.principal) <= p.maintenance(reference), dt * 2)
                p.last_quote = pool.quote(p.collateral)
                if p.principal > EPS:
                    survivors.append(p)
                else:
                    stats["debt_free_collateral_reference"] += p.collateral * reference
                continue
            actual, venue = route_quote(pool, outside, p.collateral)
            floor = p.collateral * reference * (1 - discount)
            credit = floor if delivery == "floor" else actual
            kind = None
            if actual + EPS >= floor and actual - credit + credit * reward >= fixed_cost:
                kind = "protected_full"
            elif not strict_floor:
                terminal = h <= mm * critical_fraction or p.clock.age >= terminal_age
                guarded = (p.clock.age >= confirm_age and p.last_quote is not None
                           and full_quote >= p.last_quote * 0.98)
                if terminal or guarded:
                    best_output, best_venue = actual, venue
                    actual = credit = full_quote
                    venue = "dusk"
                    kind = "terminal_full" if terminal else "guarded_full"
                    # Alternative ONLY for comparison: bind a terminal flash
                    # minimum to the program's internal quote. Not approved.
                    if terminal_route == "internal_minimum" and best_output > full_quote:
                        actual, venue = best_output, best_venue
                        credit = full_quote if delivery == "floor" else best_output
                    if credit * reward < fixed_cost:
                        kind = None
                    elif venue == "dusk":
                        stats["rejected_better_terminal_output"] += max(0, best_output - full_quote)
            if kind is None:
                p.last_quote = full_quote
                survivors.append(p)
                continue
            (pool if venue == "dusk" else outside).sell(p.collateral)
            result = waterfall(p.principal, 0, credit, reward, insurance)
            before_loss = pool.price
            assert pool.y > result["loss"], "Market reserve exhausted; scenario needs a terminal market model"
            pool.y -= result["loss"]
            stats["socialization_price_drop"] += before_loss - pool.price
            stats[kind] += 1
            stats["principal_loss"] += result["loss"]
            stats["keeper_reward"] += result["reward"]
            stats["hidden_route_surplus"] += actual - credit
            stats["owner_residual"] += result["owner"]
            stats["insurance_funding"] += result["contribution"]
            stats["max_resolution_seconds"] = max(stats["max_resolution_seconds"], now - p.first_distress)
            p.principal = 0
        positions = survivors
        assert pool.y + 1e-6 >= sum(p.principal for p in positions), "Negative debt-side cash backing"
        cached = pool.price
        if not positions:
            break
    remaining_debt = sum(p.principal for p in positions)
    shortfall = sum(max(0, p.principal - route_quote(pool, outside, p.collateral)[0]) for p in positions)
    stats.update(insurance_drawn=insurance.drawn, remaining_positions=len(positions),
                 remaining_debt=remaining_debt, unresolved_individual_shortfall=shortfall,
                 unresolved_eligible=sum(health(p.collateral * reference, p.principal) <= p.maintenance(reference)
                                         and health(pool.quote(p.collateral), p.principal) <= p.maintenance(reference)
                                         for p in positions),
                 loss_fraction=stats["principal_loss"] / original_debt if original_debt else 0,
                 final_pool_price=pool.price)
    return settings | stats


def verify_native_log(path, directory):
    text = Path(path).read_text()
    assert "test result: ok." in text and "test result: FAILED" not in text
    records = [line.split(",")[1:] for line in text.splitlines() if line.startswith("TERMINAL_ANCHOR,")]
    header = records[0]
    rows = [dict(zip(header, map(int, row))) for row in records[1:] if row != header]
    assert len(rows) == 12
    for r in rows:
        x, y, sold = r["collateral_reserve"], r["debt_reserve"], r["collateral_sold"]
        expected = y * sold // (x + sold)
        assert abs(r["output"] - expected) <= 4
        assert r["principal_loss"] == r["debt"] - r["output"]
        assert r["post_collateral_reserve"] == x + sold
        assert r["post_debt_reserve"] == y - r["output"] - r["principal_loss"]
    save_csv(directory / "terminal-native-anchors.csv", rows)
    return len(rows)


def save_csv(path, rows):
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=rows[0].keys(), lineterminator="\n")
        writer.writeheader()
        for row in rows:
            writer.writerow({k: format(v, ".10g") if isinstance(v, float) else v for k, v in row.items()})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native-log", required=True)
    args = parser.parse_args()
    directory = Path(__file__).resolve().parent
    native_count = verify_native_log(args.native_log, directory)
    rows = []
    for schedule, im, tvl, pieces, exposure, outside, path in product(
            SCHEDULES, ("buffer", "double"), (25_000, 100_000, 1_000_000),
            (1, 10), (0.1, 0.3, 0.6), (0, 1, 5), ("slow", "fast")):
        rows.append(simulate(schedule=schedule, im_rule=im, tvl=tvl, pieces=pieces,
                             exposure=exposure, outside_depth=outside, path=path))
    save_csv(directory / "emergency-margin-comparison.csv", rows)
    sensitivity = []
    for schedule, half_life, terminal_age, path, strict in product(
            ("depth_gentle", "depth_earlier"), (15, 60, 240), (30, 60, 120),
            ("fast", "gap_flat"), (False, True)):
        sensitivity.append(simulate(schedule=schedule, half_life=half_life,
                                    terminal_age=terminal_age, path=path, strict_floor=strict))
    for schedule, pieces, withdrawal, delivery, outside in product(
            SCHEDULES, (1, 10, 100), (0, 0.5), ("floor", "measured"), (0, 5)):
        sensitivity.append(simulate(schedule=schedule, pieces=pieces, withdrawal=withdrawal,
                                    delivery=delivery, outside_depth=outside))
    for cost, tvl in product((0.05, 1, 5), (100, 1_000, 100_000)):
        sensitivity.append(simulate(tvl=tvl, fixed_cost=cost, path="gap_flat"))
    for schedule, critical, outside, terminal_route, delivery in product(
            ("depth_gentle", "depth_earlier"), (0, 0.5, 1), (0, 1, 5),
            ("dusk", "internal_minimum"), ("floor", "measured")):
        sensitivity.append(simulate(schedule=schedule, critical_fraction=critical,
                                    outside_depth=outside, terminal_route=terminal_route, delivery=delivery))
    save_csv(directory / "emergency-policy-sensitivity.csv", sensitivity)
    summary = ["# Emergency policy and margin comparison", "",
               "Generated by `emergency_model.py`. Exploratory CPMM model; no final parameters selected.", "",
               f"Native zero-fee anchors verified: {native_count}. Main cases: {len(rows)}. Sensitivity cases: {len(sensitivity)}.", "",
               "## Matched requested entries: $100k pool, spending $15k, 50% wallet share, Dusk-only fast decline", "",
               "Loss percentages use admitted initial debt. Different admission counts are NOT matched risk portfolios.", "",
               "| Schedule | IM rule | Requested pieces | Opened | Entry MM | Principal loss / debt | Insurance | Unresolved eligible |",
               "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    selected = [r for r in rows if r["tvl"] == 100_000 and r["exposure"] == 0.3
                and r["outside_depth"] == 0 and r["path"] == "fast"]
    for r in selected:
        summary.append(f'| {r["schedule"]} | {r["im_rule"]} | {r["pieces"]} | {r["opened"]} | '
                       f'{r["entry_mm"]:.2%} | {r["loss_fraction"]:.2%} | {r["insurance_drawn"]:.2f} | {r["unresolved_eligible"]} |')
    summary += ["", "## Matched admitted portfolio, different fallback policy", "",
                "Depth-gentle, 10 requested pieces, $100k pool, $15k total requested spending, 60s EMA. Initial admission is identical in these rows.", "",
                "| Path | Terminal age | Permanent protected floor | Loss / debt | Insurance | Remaining debt | Unresolved eligible |",
                "| --- | ---: | --- | ---: | ---: | ---: | ---: |"]
    for r in sensitivity:
        if r["schedule"] == "depth_gentle" and r["half_life"] == 60 and r["withdrawal"] == 0 and r["delivery"] == "floor" and r["pieces"] == 10 and r["outside_depth"] == 0 and r["fixed_cost"] == 0.05 and r["path"] in ("fast", "gap_flat") and r["critical_fraction"] == 0 and r["terminal_route"] == "dusk":
            # Deduplicate the baseline repeated in the sensitivity matrix.
            line = (f'| {r["path"]} | {r["terminal_age"]} | {r["strict_floor"]} | {r["loss_fraction"]:.2%} | '
                    f'{r["insurance_drawn"]:.2f} | {r["remaining_debt"]:.2f} | {r["unresolved_eligible"]} |')
            if line not in summary:
                summary.append(line)
    summary += ["", "## Earlier terminal permission and external execution sensitivity", "",
                "Same $7,500 initial debt in ten positions; depth-gentle schedule, fast decline. `internal_minimum` is an UNAPPROVED terminal flash alternative. Floor delivery lets the keeper retain route output above the obligation; measured delivery assumes all route output reaches settlement.", "",
                "| Critical reference level | Outside depth | Terminal route | Delivery | Loss / debt | Insurance | Hidden route surplus |",
                "| --- | ---: | --- | --- | ---: | ---: | ---: |"]
    for r in sensitivity:
        if r["schedule"] == "depth_gentle" and r["outside_depth"] == 5 and r["path"] == "fast" and r["withdrawal"] == 0 and r["pieces"] == 10 and r["half_life"] == 60 and r["terminal_age"] == 60:
            line = (f'| {r["critical_fraction"]:.0%} of MM | {r["outside_depth"]} | {r["terminal_route"]} | {r["delivery"]} | '
                    f'{r["loss_fraction"]:.2%} | {r["insurance_drawn"]:.2f} | {r["hidden_route_surplus"]:.2f} |')
            if line not in summary:
                summary.append(line)
    summary += ["", "## Interpretation limits", "",
                "- Inspect admission counts before comparing loss percentages. Zero admitted risk is not successful liquidation.",
                "- Remaining debt and unresolved eligible positions accompany realized losses; a stalled floor is not zero-risk.",
                "- One liquidation opportunity per position per step, 5% partial-sale grid, and entry-order processing. Timing and order need native confirmation.",
                "- 0.3% input fees; no transfer fees, accrued interest, hLP, public borrowing, dynamic fees, or concentrated curves in the sweep. Interest allocation is covered separately in model tests.",
                "- Outside fixtures have finite within-step depth and are repriced between steps. They are not guaranteed liquidity or an optimized aggregator route.",
                "- No new exposure increase/withdrawal after entry except the specified LP stress. Stack splitting neutrality is tested only at fixed depth.",
                "- Snapshot guard is the preceding observation's same-position quote with 2% tolerance, not an implemented slot-opening snapshot.",
                "- Insurance starts at 1% of pool value; max event/day settings are 20% of available / 50% of opening plus credits in one window.",
                "- Principal-only insurance and residual-funded 2% contribution are model candidates. Keeper cost 0.05 is illustrative, not measured compute/priority cost.",
                "- Native anchors validate zero-fee quote and socialization identities only. No claim of production safety, optimal parameters, or resolution of #287652.", ""]
    (directory / "EMERGENCY_MARGIN_RESULTS.md").write_text("\n".join(summary))
    print(f"Verified {native_count} native anchors; wrote {len(rows)} main and {len(sensitivity)} sensitivity cases.")


if __name__ == "__main__":
    main()

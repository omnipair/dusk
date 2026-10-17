"""Exact-arithmetic admission-policy checks; not a native Dusk simulation.

Run with: PYTHONDONTWRITEBYTECODE=1 python3 docs/calibration/check_incremental_crowding.py
All values are abstract collateral-value units at a fixed price. This deliberately
isolates the proposed margin bookkeeping from execution, EMA and liquidations.
"""

from dataclasses import dataclass
from fractions import Fraction as Q
import random


ZERO = Q(0)


def capital(exposure, depth):
    """Illustrative 10/15/20% marginal IM: proposed MM bands plus 3 points."""
    assert depth > 0 and exposure >= 0
    first, second = depth / 20, depth / 10
    return (min(exposure, first) / 10
            + min(max(exposure - first, ZERO), second) * Q(15, 100)
            + max(exposure - first - second, ZERO) / 5)


def ceiling(value):
    return (value.numerator + value.denominator - 1) // value.denominator


def floor(value):
    return value.numerator // value.denominator


def opening_costs(parts, depth, two_state):
    exposure = ZERO
    costs = []
    for size in parts:
        after = exposure + size
        before_depth = depth(exposure) if two_state else depth(after)
        costs.append(capital(after, depth(after)) - capital(exposure, before_depth))
        exposure = after
    return costs


@dataclass
class Position:
    collateral: Q
    obligation: Q


def lifecycle(seed, moving_depth, integer):
    """Track minimum admission obligations, not actual equity during losses.

    New collateral adds its incremental capital obligation. All debt increases
    and equity withdrawals must retain that obligation. A partial close releases
    at most the closed fraction of the obligation. No IM gate is imposed on pure
    repayments or forced/risk-reducing sales. Zero collateral clears the record.
    """
    rng = random.Random(seed)
    positions = []
    starting_depth = Q(80000)

    def depth(exposure):
        # Synthetic depth path, NOT constant-product or concentrated execution.
        return starting_depth - exposure if moving_depth else Q(50000)

    def potential(exposure):
        return capital(exposure, depth(exposure))

    events = {key: 0 for key in ("open", "increase", "partial", "close", "withdraw")}
    for _ in range(100):
        exposure = sum((p.collateral for p in positions), ZERO)
        action = rng.choice(tuple(events)) if positions else "open"
        if action in ("open", "increase"):
            room = floor(Q(60000) - exposure)
            if room <= 0:
                action = "close"
            else:
                size = Q(rng.randint(1, min(5000, room)))
                charge = potential(exposure + size) - potential(exposure)
                assert charge > 0
                if integer:
                    charge = Q(ceiling(charge))
                if action == "open":
                    positions.append(Position(size, charge))
                else:
                    position = rng.choice(positions)
                    position.collateral += size
                    position.obligation += charge
        if action == "partial":
            position = rng.choice(positions)
            removed = position.collateral * Q(rng.randint(1, 9), 10)
            released = position.obligation * removed / position.collateral
            if integer:
                released = Q(floor(released))
            position.collateral -= removed
            position.obligation -= released
        elif action == "close":
            positions.pop(rng.randrange(len(positions)))
        elif action == "withdraw":
            position = rng.choice(positions)
            # At an unchanged price, surplus can be withdrawn; a cent below the
            # retained obligation must fail even though exposure does not change.
            equity = position.obligation + Q(rng.randint(1, 100))
            available = equity - position.obligation
            assert equity - available >= position.obligation
            assert equity - available - Q(1, 100) < position.obligation
        events[action] += 1
        remaining = sum((p.collateral for p in positions), ZERO)
        retained = sum((p.obligation for p in positions), ZERO)
        assert retained >= potential(remaining), (seed, moving_depth, integer, action)
    return events


def main():
    fixed = lambda exposure: Q(50000)
    moving = lambda exposure: Q(80000) - exposure
    parts = [Q(15000)] * 3
    single = capital(Q(45000), Q(50000))
    assert single == 8500
    assert opening_costs(parts, fixed, False) == [2500, 3000, 3000]
    print("Fixed depth: one 45,000 = 8,500; three 15,000 = 2,500 + 3,000 + 3,000.")

    # Deliberate counterexample: ordinary per-position IM would refund the
    # additional crowding capital immediately after admission.
    own_only = sum((capital(size, Q(50000)) for size in parts), ZERO)
    assert own_only == 7500 and single - own_only == 1000
    print("Opening-only policy FAILS: withdrawing down to own-size IM releases 1,000.")

    naive = opening_costs(parts, moving, False)
    corrected = opening_costs(parts, moving, True)
    assert naive == [2350, 3000, 3000]
    assert corrected == [2350, 3150, 3150]
    assert sum(naive) == 8350 and sum(corrected) == capital(Q(45000), Q(35000)) == 8650
    print("Changing depth: naive split 8,350 vs single 8,650; two-state delta matches 8,650.")

    # Later LP liquidity changes deliberately do not retroactively re-margin
    # old positions under the selected product policy.
    assert capital(Q(45000), Q(1000000)) == 4500
    assert capital(Q(45000), Q(50000)) == 8500
    print("External liquidity withdrawal: old obligation 4,500 vs fresh requirement 8,500.")

    # Static splitting check across arbitrary wallets and integer atom rounding.
    rng = random.Random(451)
    for _ in range(1000):
        total = rng.randint(100, 60000)
        count = rng.randint(1, min(100, total))
        cuts = [0, *sorted(rng.sample(range(1, total), count - 1)), total]
        split = [Q(b - a) for a, b in zip(cuts, cuts[1:])]
        for depth in (fixed, moving):
            charges = opening_costs(split, depth, True)
            whole = capital(Q(total), depth(Q(total)))
            assert sum(charges) == whole
            assert sum(ceiling(c) for c in charges) >= ceiling(whole)
    print("PASS: 2,000 partition cases, exact and conservative integer charge rounding.")

    events = dict.fromkeys(("open", "increase", "partial", "close", "withdraw"), 0)
    for moving_depth in (False, True):
        for integer in (False, True):
            for seed in range(250):
                for action, count in lifecycle(seed, moving_depth, integer).items():
                    events[action] += count
    assert all(events.values()) and sum(events.values()) == 100000
    print("PASS: 1,000 lifecycle paths / 100,000 modeled actions:", events)
    print("Scope: arithmetic/bookkeeping only; no native swaps, losses, EMA, fees or instruction safety.")


if __name__ == "__main__":
    main()

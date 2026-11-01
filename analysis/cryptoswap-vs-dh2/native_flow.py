"""Common, external-price-denominated order intents for native replay.

The generator is deliberately policy-blind.  Neither Curve nor Dusk can see
the future reference prices used to label informed orders; those prices are
available only to the simulated trader and the evaluator.
"""

from __future__ import annotations

from dataclasses import dataclass
import math
import random
from typing import Sequence


@dataclass(frozen=True)
class NativeOrder:
    minute: int
    external_notional: float
    direction: str
    informed: bool


def _poisson(randomizer: random.Random, mean: float) -> int:
    if mean <= 0:
        return 0
    threshold = math.exp(-mean)
    product = 1.0
    count = 0
    while product > threshold:
        count += 1
        product *= randomizer.random()
    return count - 1


def _bounded_scale(
    weights: Sequence[float],
    total: float,
    maximum: float,
) -> list[float]:
    if not weights or total <= 0 or maximum <= 0:
        raise ValueError("positive weights, total, and maximum are required")
    if total > maximum * len(weights) + 1e-12:
        raise ValueError("target order volume exceeds the per-order cap")
    values = [0.0] * len(weights)
    remaining = set(range(len(weights)))
    remaining_total = total
    while remaining:
        weight_total = sum(weights[index] for index in remaining)
        if weight_total <= 0:
            equal = remaining_total / len(remaining)
            for index in remaining:
                values[index] = equal
            break
        clipped = []
        for index in remaining:
            candidate = remaining_total * weights[index] / weight_total
            if candidate > maximum:
                values[index] = maximum
                remaining_total -= maximum
                clipped.append(index)
        if not clipped:
            for index in remaining:
                values[index] = remaining_total * weights[index] / weight_total
            break
        remaining.difference_update(clipped)
    return values


def generate_native_order_tape(
    prices: Sequence[float],
    *,
    pool_value: float,
    daily_volume_pool_ratio: float,
    informed_share: float,
    informed_accuracy: float,
    signal_horizon_minutes: int,
    mean_arrivals_per_minute: float,
    seed: int,
    max_order_bps: float = 2_000.0,
    step_minutes: int = 1,
) -> tuple[NativeOrder, ...]:
    """Create one common-seed Poisson/heavy-tail intent tape.

    Total notional is fixed by the declared daily-volume ratio. Individual
    order sizes use a capped Pareto draw and are then scaled without exceeding
    ``max_order_bps`` of initial TVL.
    """

    if not prices or min(prices) <= 0:
        raise ValueError("prices must be non-empty and positive")
    if pool_value <= 0 or daily_volume_pool_ratio <= 0:
        raise ValueError("pool value and volume ratio must be positive")
    if not 0 <= informed_share <= 1 or not 0 <= informed_accuracy <= 1:
        raise ValueError("shares and probabilities must be in [0, 1]")
    if signal_horizon_minutes <= 0 or mean_arrivals_per_minute <= 0:
        raise ValueError("horizon and arrival intensity must be positive")
    if step_minutes <= 0:
        raise ValueError("step_minutes must be positive")

    randomizer = random.Random(seed)
    descriptors: list[tuple[int, float, str, bool]] = []
    horizon_steps = max(1, math.ceil(signal_horizon_minutes / step_minutes))
    for minute, current_price in enumerate(prices):
        arrivals = _poisson(
            randomizer,
            mean_arrivals_per_minute * step_minutes,
        )
        for _ in range(arrivals):
            # Pareto(alpha=1.7) supplies many small intents and a bounded tail.
            raw_weight = min(randomizer.paretovariate(1.7), 2_000.0)
            informed = randomizer.random() < informed_share
            if informed:
                future = prices[min(minute + horizon_steps, len(prices) - 1)]
                if future == current_price:
                    # A tied future contains no directional information.
                    # Randomizing prevents a flat synthetic path from turning
                    # nominally informed flow into deterministic sell flow.
                    informed = False
                    direction = (
                        "x_to_y" if randomizer.random() < 0.5 else "y_to_x"
                    )
                else:
                    correct = randomizer.random() < informed_accuracy
                    rising = future > current_price
                    if not correct:
                        rising = not rising
                    direction = "y_to_x" if rising else "x_to_y"
            else:
                direction = "x_to_y" if randomizer.random() < 0.5 else "y_to_x"
            descriptors.append((minute, raw_weight, direction, informed))

    if not descriptors:
        descriptors.append((0, 1.0, "x_to_y", False))
    target_volume = (
        pool_value
        * daily_volume_pool_ratio
        * len(prices)
        * step_minutes
        / (24.0 * 60.0)
    )
    amounts = _bounded_scale(
        [item[1] for item in descriptors],
        target_volume,
        pool_value * max_order_bps / 10_000.0,
    )
    return tuple(
        NativeOrder(
            minute=minute,
            external_notional=amount,
            direction=direction,
            informed=informed,
        )
        for (minute, _, direction, informed), amount in zip(descriptors, amounts)
    )


def orders_by_minute(
    tape: Sequence[NativeOrder],
    length: int,
) -> tuple[tuple[NativeOrder, ...], ...]:
    grouped: list[list[NativeOrder]] = [[] for _ in range(length)]
    for order in tape:
        if not 0 <= order.minute < length:
            raise ValueError("order minute lies outside the price path")
        grouped[order.minute].append(order)
    return tuple(tuple(rows) for rows in grouped)

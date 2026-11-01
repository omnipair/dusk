"""Fair, price-space comparison utilities for the three concentrated curves.

The functions in this module deliberately exclude fees, controllers, and
implementation cost. They answer the narrower economic question: at the same
marginal price, externally marked pool value, and order notional, what
execution service does each invariant provide? A separate one-jump test starts
every invariant from identical balanced reserves to measure exposed LP value.
"""

from __future__ import annotations

from dataclasses import dataclass, replace
from functools import lru_cache
import math
from typing import Callable, Iterable, Sequence

from models import (
    Amm,
    CryptoSwap,
    CurrentDh2,
    HyperbolicSharpDh2,
    LogPotentialDh2,
    SharpDh2,
    with_zero_fees,
)


POOL_VALUE = 2_000_000.0
PROFILE_PRICE_RATIOS = (
    2.0 / 3.0,
    0.80,
    0.90,
    0.95,
    0.98,
    1.00,
    1.02,
    1.05,
    1.10,
    1.25,
    1.50,
)
PROFILE_TRADE_BPS = (1, 2, 5, 10, 20, 50, 100, 200, 500, 1_000, 1_500, 2_000)
JUMP_RATIOS = (0.80, 0.90, 0.95, 0.99, 1.01, 1.05, 1.10, 1.25, 1.50)
CURRENT_PEAK_DEPTH_MAX = 2_000.0
CURVE_ANN_NORMALIZER = 4 * 10_000
CURVE_MIN_ANN_RAW = 4_000
CURVE_MAX_ANN_RAW = 40_000_000
CURVE_AMPLIFICATION_MIN = CURVE_MIN_ANN_RAW / CURVE_ANN_NORMALIZER
CURVE_AMPLIFICATION_MAX = CURVE_MAX_ANN_RAW / CURVE_ANN_NORMALIZER
CURVE_GAMMA_MIN = 1e-8
CURVE_GAMMA_MAX = 0.199
PROPOSED_DEPTH_MIN = 3.0
PROPOSED_DEPTH_MAX = 2_001.0
PROPOSED_HALF_WIDTH_BPS_MIN = 1.0
PROPOSED_HALF_WIDTH_BPS_MAX = 20_000.0
ISO_FIRST_GRID_POINTS = 17
ISO_SECOND_SCAN_POINTS = 129
ISO_FIRST_MAX_REFINEMENTS = 2
# The two iso objectives are denominated in basis points.  Requiring the
# incumbent to move by no more than one thousandth of a basis point between
# successive first-parameter grids keeps sub-bp family gaps from being an
# artefact of the coarse 17-point sweep.
ISO_FIRST_OBJECTIVE_CONVERGENCE_BPS = 1e-3
ISO_FIRST_PARAMETER_LOG_CONVERGENCE = 1e-7
ISO_SECOND_AUDIT_SCAN_POINTS = (257, 513)
ISO_SECOND_PARAMETER_LOG_CONVERGENCE = 1e-7


@dataclass(frozen=True)
class ServiceObservation:
    normalized_price: float
    trade_bps: int
    direction: str
    impact_bps: float


@dataclass(frozen=True)
class JumpObservation:
    price_ratio: float
    arbitrage_profit: float
    lp_loss_vs_hold: float
    ending_base: float
    ending_quote: float


@dataclass(frozen=True)
class CandidateScore:
    model: Amm
    execution_index_bps: float
    jump_loss_index_bps: float
    center_100bps_impact: float
    low_slippage_width_bps: float
    tail_100bps_impact: float


_PRICE_RATIO_CACHE: dict[tuple[str, float], float] = {}


def model_family(model: Amm) -> str:
    if isinstance(model, CryptoSwap):
        return "curve"
    if isinstance(model, SharpDh2):
        return "sharp_dh2"
    if isinstance(model, HyperbolicSharpDh2):
        return "hyperbolic_p4_dh2"
    if isinstance(model, CurrentDh2):
        return "current_dh2"
    if isinstance(model, LogPotentialDh2):
        return "proposed_dh2"
    return "cpmm"


def reserve_ratio_for_price(model: Amm, normalized_price: float) -> float:
    """Invert the invariant's marginal price in log-reserve space.

    A coarse reserve grid is materially misleading for highly concentrated
    curves: a tiny price displacement can require a large reserve movement.
    This direct root solve therefore uses price, not reserve imbalance, as the
    comparison coordinate.
    """
    if normalized_price <= 0:
        raise ValueError("normalized_price must be positive")
    if math.isclose(normalized_price, 1.0, rel_tol=0.0, abs_tol=1e-14):
        return 1.0
    zero_fee = with_zero_fees(model)
    return _reserve_ratio_for_zero_fee_price(zero_fee, normalized_price)


def _reserve_ratio_for_zero_fee_price(
    zero_fee: Amm,
    normalized_price: float,
) -> float:
    """Invert marginal price for a model whose fee fields are already zero."""

    key = (repr(zero_fee), round(normalized_price, 12))
    cached = _PRICE_RATIO_CACHE.get(key)
    if cached is not None:
        return cached
    reciprocal_key = (repr(zero_fee), round(1.0 / normalized_price, 12))
    reciprocal = _PRICE_RATIO_CACHE.get(reciprocal_key)
    if reciprocal is not None:
        result = 1.0 / reciprocal
        _PRICE_RATIO_CACHE[key] = result
        return result

    low = -18.0
    high = 18.0

    def price_at(log_ratio: float) -> float:
        ratio = math.exp(log_ratio)
        return zero_fee.marginal_price_raw(math.sqrt(ratio), 1.0 / math.sqrt(ratio))

    price_low = price_at(low)
    price_high = price_at(high)
    if not price_high <= normalized_price <= price_low:
        raise ArithmeticError(
            f"price {normalized_price} outside root bracket "
            f"[{price_high}, {price_low}]"
        )
    for _ in range(64):
        middle = (low + high) / 2.0
        if price_at(middle) > normalized_price:
            low = middle
        else:
            high = middle
    result = math.exp((low + high) / 2.0)
    _PRICE_RATIO_CACHE[key] = result
    return result


def reserves_at_price(
    model: Amm,
    normalized_price: float,
    *,
    pool_value: float = POOL_VALUE,
) -> tuple[float, float]:
    """Return common-coordinate reserves with equal external marked value."""
    zero_fee = with_zero_fees(model)
    return _reserves_at_price_for_zero_fee(
        zero_fee,
        normalized_price,
        pool_value=pool_value,
    )


def _reserves_at_price_for_zero_fee(
    zero_fee: Amm,
    normalized_price: float,
    *,
    pool_value: float = POOL_VALUE,
) -> tuple[float, float]:
    """Return common-coordinate reserves for an already zero-fee model."""

    if normalized_price <= 0:
        raise ValueError("normalized_price must be positive")
    ratio = (
        1.0
        if math.isclose(normalized_price, 1.0, rel_tol=0.0, abs_tol=1e-14)
        else _reserve_ratio_for_zero_fee_price(zero_fee, normalized_price)
    )
    x_shape = math.sqrt(ratio)
    y_shape = 1.0 / math.sqrt(ratio)
    scale = pool_value / (normalized_price * x_shape + y_shape)
    return scale * x_shape, scale * y_shape


def execution_impact_bps(
    model: Amm,
    normalized_price: float,
    trade_bps: int,
    direction: str,
    *,
    pool_value: float = POOL_VALUE,
) -> float:
    """Zero-fee impact for an order sized in external quote-value bps.

    ``x`` is base inventory in center-price common units and ``y`` is quote
    inventory.  The center is normalized to one, so a base-sell input with
    external notional ``n`` enters the invariant as ``n / price``.
    """
    zero_fee = with_zero_fees(model)
    x, y = _reserves_at_price_for_zero_fee(
        zero_fee,
        normalized_price,
        pool_value=pool_value,
    )
    invariant = zero_fee.invariant(x, y)
    return _execution_impact_bps_from_state(
        zero_fee,
        x,
        y,
        invariant,
        normalized_price,
        trade_bps,
        direction,
        pool_value=pool_value,
    )


def _execution_impact_bps_from_state(
    zero_fee: Amm,
    x: float,
    y: float,
    invariant: float,
    normalized_price: float,
    trade_bps: int,
    direction: str,
    *,
    pool_value: float = POOL_VALUE,
) -> float:
    """Quote one order from an already prepared zero-fee reserve state."""

    notional = pool_value * trade_bps / 10_000.0
    if direction == "x_to_y":
        common_input = notional / normalized_price
        quote = zero_fee.quote(
            x,
            y,
            common_input,
            direction,
            apply_fee=False,
            include_marginals=False,
            invariant_override=invariant,
        )
        execution_price = quote.amount_out / common_input
        impact = 1.0 - execution_price / normalized_price
    elif direction == "y_to_x":
        quote = zero_fee.quote(
            x,
            y,
            notional,
            direction,
            apply_fee=False,
            include_marginals=False,
            invariant_override=invariant,
        )
        execution_price = notional / quote.amount_out
        impact = execution_price / normalized_price - 1.0
    else:
        raise ValueError(f"unknown direction {direction}")
    return max(impact * 10_000.0, 1e-9)


def execution_impact_bps_exact_out(
    model: Amm,
    normalized_price: float,
    trade_bps: int,
    direction: str,
    *,
    pool_value: float = POOL_VALUE,
) -> float:
    """Zero-fee exact-output cost for an externally valued output amount."""
    zero_fee = with_zero_fees(model)
    x, y = _reserves_at_price_for_zero_fee(
        zero_fee,
        normalized_price,
        pool_value=pool_value,
    )
    output_notional = pool_value * trade_bps / 10_000.0
    target_output = (
        output_notional
        if direction == "x_to_y"
        else output_notional / normalized_price
    )
    low = 0.0
    high = pool_value

    def output_for(common_input: float) -> float:
        return zero_fee.quote(
            x,
            y,
            common_input,
            direction,
            apply_fee=False,
            include_marginals=False,
        ).amount_out

    while output_for(high) < target_output:
        high *= 2.0
        if high > pool_value * 100:
            raise ArithmeticError("exact-output target is not executable")
    for _ in range(64):
        middle = (low + high) / 2.0
        if output_for(middle) < target_output:
            low = middle
        else:
            high = middle
    common_input = high
    if direction == "x_to_y":
        input_value = common_input * normalized_price
    elif direction == "y_to_x":
        input_value = common_input
    else:
        raise ValueError(f"unknown direction {direction}")
    return max((input_value / output_notional - 1.0) * 10_000.0, 1e-9)


def service_profile(
    model: Amm,
    prices: Sequence[float] = PROFILE_PRICE_RATIOS,
    trade_bps: Sequence[int] = PROFILE_TRADE_BPS,
) -> list[ServiceObservation]:
    # Every order at one normalized price starts from the same reserve state.
    # Preparing that state once avoids resolving D for every size/direction
    # while preserving the exact quote path through ``invariant_override``.
    zero_fee = with_zero_fees(model)
    observations: list[ServiceObservation] = []
    for price in prices:
        x, y = _reserves_at_price_for_zero_fee(zero_fee, price)
        invariant = zero_fee.invariant(x, y)
        for size in trade_bps:
            for direction in ("x_to_y", "y_to_x"):
                observations.append(
                    ServiceObservation(
                        normalized_price=price,
                        trade_bps=size,
                        direction=direction,
                        impact_bps=_execution_impact_bps_from_state(
                            zero_fee,
                            x,
                            y,
                            invariant,
                            price,
                            size,
                            direction,
                        ),
                    )
                )
    return observations


def service_distance(candidate: Amm, target: Sequence[ServiceObservation]) -> float:
    """Log-RMSE of execution impact, never total token output."""
    observed = service_profile(
        candidate,
        tuple(dict.fromkeys(item.normalized_price for item in target)),
        tuple(dict.fromkeys(item.trade_bps for item in target)),
    )
    if len(observed) != len(target):
        raise AssertionError("profile shape mismatch")
    squared = []
    for actual, expected in zip(observed, target):
        if (
            actual.normalized_price != expected.normalized_price
            or actual.trade_bps != expected.trade_bps
            or actual.direction != expected.direction
        ):
            raise AssertionError("profile ordering mismatch")
        squared.append(
            math.log(max(actual.impact_bps, 1e-4) / max(expected.impact_bps, 1e-4))
            ** 2
        )
    return math.sqrt(sum(squared) / len(squared))


def service_max_log_error(candidate: Amm, target: Sequence[ServiceObservation]) -> float:
    """Worst multiplicative quote-impact error over the service surface."""
    observed = service_profile(
        candidate,
        tuple(dict.fromkeys(item.normalized_price for item in target)),
        tuple(dict.fromkeys(item.trade_bps for item in target)),
    )
    return max(
        abs(
            math.log(
                max(actual.impact_bps, 1e-4)
                / max(expected.impact_bps, 1e-4)
            )
        )
        for actual, expected in zip(observed, target)
    )


def low_slippage_width_bps(
    model: Amm,
    *,
    trade_bps: int = 100,
    impact_threshold_bps: float = 10.0,
) -> float:
    """Two-sided log-price half-width meeting one common quote threshold."""
    def worst_impact(distance: float) -> float:
        return max(
            execution_impact_bps(model, price, trade_bps, direction)
            for price in (math.exp(-distance), math.exp(distance))
            for direction in ("x_to_y", "y_to_x")
        )

    if worst_impact(0.0) > impact_threshold_bps:
        return 0.0
    low = 0.0
    high = 0.001
    while high < 0.60 and worst_impact(high) <= impact_threshold_bps:
        low = high
        high *= 2.0
    if high >= 0.60 and worst_impact(0.60) <= impact_threshold_bps:
        return 6_000.0
    for _ in range(36):
        middle = (low + high) / 2.0
        if worst_impact(middle) <= impact_threshold_bps:
            low = middle
        else:
            high = middle
    return low * 10_000.0


def _profit_for_zero_fee_trade(
    model: Amm,
    base: float,
    quote_reserve: float,
    center: float,
    external_price: float,
    direction: str,
    external_notional: float,
) -> tuple[float, float, float]:
    common_x = base * center
    if direction == "x_to_y":
        base_input = external_notional / external_price
        common_input = base_input * center
        quote = with_zero_fees(model).quote(
            common_x,
            quote_reserve,
            common_input,
            direction,
            apply_fee=False,
            include_marginals=False,
        )
        profit = quote.amount_out - external_notional
        return profit, base + base_input, quote_reserve - quote.amount_out
    quote = with_zero_fees(model).quote(
        common_x,
        quote_reserve,
        external_notional,
        direction,
        apply_fee=False,
        include_marginals=False,
    )
    base_output = quote.amount_out / center
    profit = base_output * external_price - external_notional
    return profit, base - base_output, quote_reserve + external_notional


def zero_fee_jump_observation(
    model: Amm,
    price_ratio: float,
    *,
    pool_value: float = POOL_VALUE,
) -> JumpObservation:
    """Profit-maximizing arbitrage after a one-step external price jump."""
    if price_ratio <= 0:
        raise ValueError("price_ratio must be positive")
    center = 1.0
    initial_base = pool_value / 2.0
    initial_quote = pool_value / 2.0
    direction = "y_to_x" if price_ratio > 1.0 else "x_to_y"

    # A broad deterministic log scan followed by golden-section refinement is
    # robust across all three non-linear invariants and does not assume the
    # reserve ratio of the optimum.
    minimum = pool_value * 1e-10
    maximum = pool_value * 0.90
    grid = [
        minimum * math.exp(math.log(maximum / minimum) * index / 79.0)
        for index in range(80)
    ]
    profits = [
        _profit_for_zero_fee_trade(
            model,
            initial_base,
            initial_quote,
            center,
            price_ratio,
            direction,
            amount,
        )[0]
        for amount in grid
    ]
    best_index = max(range(len(grid)), key=profits.__getitem__)
    if profits[best_index] <= 0:
        return JumpObservation(
            price_ratio=price_ratio,
            arbitrage_profit=0.0,
            lp_loss_vs_hold=0.0,
            ending_base=initial_base,
            ending_quote=initial_quote,
        )
    left = grid[max(0, best_index - 1)]
    right = grid[min(len(grid) - 1, best_index + 1)]
    phi = (math.sqrt(5.0) - 1.0) / 2.0
    c = right - phi * (right - left)
    d = left + phi * (right - left)

    def profit_at(amount: float) -> float:
        return _profit_for_zero_fee_trade(
            model,
            initial_base,
            initial_quote,
            center,
            price_ratio,
            direction,
            amount,
        )[0]

    fc = profit_at(c)
    fd = profit_at(d)
    for _ in range(48):
        if fc > fd:
            right, d, fd = d, c, fc
            c = right - phi * (right - left)
            fc = profit_at(c)
        else:
            left, c, fc = c, d, fd
            d = left + phi * (right - left)
            fd = profit_at(d)
    amount = (left + right) / 2.0
    profit, ending_base, ending_quote = _profit_for_zero_fee_trade(
        model,
        initial_base,
        initial_quote,
        center,
        price_ratio,
        direction,
        amount,
    )
    hold = initial_base * price_ratio + initial_quote
    ending = ending_base * price_ratio + ending_quote
    return JumpObservation(
        price_ratio=price_ratio,
        arbitrage_profit=max(profit, 0.0),
        lp_loss_vs_hold=max(hold - ending, 0.0),
        ending_base=ending_base,
        ending_quote=ending_quote,
    )


@lru_cache(maxsize=None)
def execution_index_bps(model: Amm) -> float:
    """Geometric mean quote impact across the declared service surface."""
    impacts = [item.impact_bps for item in service_profile(model)]
    return math.exp(sum(math.log(max(value, 1e-4)) for value in impacts) / len(impacts))


@lru_cache(maxsize=None)
def jump_loss_index_bps(model: Amm) -> float:
    """Average one-jump loss versus hold, as bps of starting pool value."""
    losses = [
        zero_fee_jump_observation(model, ratio).lp_loss_vs_hold / POOL_VALUE * 10_000.0
        for ratio in JUMP_RATIOS
    ]
    return sum(losses) / len(losses)


def candidate_score(model: Amm) -> CandidateScore:
    tail_impacts = [
        execution_impact_bps(model, price, 100, direction)
        for price in (2.0 / 3.0, 1.5)
        for direction in ("x_to_y", "y_to_x")
    ]
    return CandidateScore(
        model=model,
        execution_index_bps=execution_index_bps(model),
        jump_loss_index_bps=jump_loss_index_bps(model),
        center_100bps_impact=max(
            execution_impact_bps(model, 1.0, 100, "x_to_y"),
            execution_impact_bps(model, 1.0, 100, "y_to_x"),
        ),
        low_slippage_width_bps=low_slippage_width_bps(model),
        tail_100bps_impact=max(tail_impacts),
    )


def _log_grid(low: float, high: float, count: int) -> list[float]:
    if count < 2:
        return [math.sqrt(low * high)]
    values = [
        low * math.exp(math.log(high / low) * index / (count - 1))
        for index in range(count)
    ]
    values[0] = low
    values[-1] = high
    return values


def curve_candidates() -> list[CryptoSwap]:
    return [
        CryptoSwap(amplification=amplification, gamma=gamma)
        for amplification in _log_grid(
            CURVE_AMPLIFICATION_MIN, CURVE_AMPLIFICATION_MAX, 9
        )
        for gamma in _log_grid(CURVE_GAMMA_MIN, CURVE_GAMMA_MAX, 9)
    ]


def current_candidates() -> list[CurrentDh2]:
    return [
        CurrentDh2(peak_depth=peak_depth, fade_scale=fade_scale)
        for peak_depth in _log_grid(2.0, CURRENT_PEAK_DEPTH_MAX, 7)
        for fade_scale in _log_grid(1e-6, 0.50, 11)
    ]


def proposed_candidates() -> list[LogPotentialDh2]:
    return [
        LogPotentialDh2(depth_multiplier=depth, half_width_bps=width)
        for depth in _log_grid(PROPOSED_DEPTH_MIN, PROPOSED_DEPTH_MAX, 9)
        for width in _log_grid(
            PROPOSED_HALF_WIDTH_BPS_MIN,
            PROPOSED_HALF_WIDTH_BPS_MAX,
            9,
        )
    ]


def _refine_match(
    constructor: Callable[[float, float], Amm],
    initial: tuple[float, float],
    target: Sequence[ServiceObservation],
) -> Amm:
    best = constructor(*initial)
    best_parameters = parameter_dict(best)
    first_value = best_parameters["parameter_1"]
    second_value = best_parameters["parameter_2"]
    first = math.log(first_value)
    second = math.log(second_value)
    best_score = service_max_log_error(best, target)
    for step in (0.50, 0.25, 0.125, 0.0625, 0.03125, 0.015625):
        choices_by_model: dict[str, tuple[float, Amm]] = {}
        for first_shift in (-step, 0.0, step):
            for second_shift in (-step, 0.0, step):
                candidate = constructor(
                    (
                        first_value
                        if first_shift == 0.0
                        else math.exp(first + first_shift)
                    ),
                    (
                        second_value
                        if second_shift == 0.0
                        else math.exp(second + second_shift)
                    ),
                )
                key = repr(candidate)
                if key in choices_by_model:
                    continue
                choices_by_model[key] = (
                    service_max_log_error(candidate, target),
                    candidate,
                )
        score, candidate = min(
            choices_by_model.values(),
            key=lambda item: (item[0], repr(item[1])),
        )
        if score <= best_score:
            best_score = score
            best = candidate
            # Constructors clamp to their declared parameter domains.  Keep
            # the latent search coordinates synchronized with the actual
            # selected model instead of drifting beyond a boundary and
            # repeatedly rescoring the same clamped candidate.
            parameters = parameter_dict(best)
            first_value = parameters["parameter_1"]
            second_value = parameters["parameter_2"]
            first = math.log(first_value)
            second = math.log(second_value)
    return best


def match_to_curve(
    target_curve: CryptoSwap | None = None,
) -> tuple[CryptoSwap, CurrentDh2, LogPotentialDh2, dict[str, float]]:
    """Find full-service matches to one explicit Curve reference point."""
    curve = target_curve or CryptoSwap(amplification=400.0, gamma=0.001)
    target = service_profile(curve)
    current_seed = min(
        current_candidates(), key=lambda item: service_max_log_error(item, target)
    )
    proposed_seed = min(
        proposed_candidates(), key=lambda item: service_max_log_error(item, target)
    )
    current = _refine_match(
        lambda first, second: CurrentDh2(
            peak_depth=min(max(first, 2.0), CURRENT_PEAK_DEPTH_MAX),
            fade_scale=min(max(second, 1e-6), 0.50),
        ),
        (current_seed.peak_depth, current_seed.fade_scale),
        target,
    )
    proposed = _refine_match(
        lambda first, second: LogPotentialDh2(
            depth_multiplier=min(
                max(first, PROPOSED_DEPTH_MIN), PROPOSED_DEPTH_MAX
            ),
            half_width_bps=min(
                max(second, PROPOSED_HALF_WIDTH_BPS_MIN),
                PROPOSED_HALF_WIDTH_BPS_MAX,
            ),
        ),
        (proposed_seed.depth_multiplier, proposed_seed.half_width_bps),
        target,
    )
    return curve, current, proposed, {
        "curve": 0.0,
        "current_dh2_rmse": service_distance(current, target),
        "proposed_dh2_rmse": service_distance(proposed, target),
        "current_dh2_max_log_error": service_max_log_error(current, target),
        "proposed_dh2_max_log_error": service_max_log_error(proposed, target),
    }


def pareto_frontier(scores: Iterable[CandidateScore]) -> list[CandidateScore]:
    """Return candidates not dominated in execution cost and jump loss."""
    all_scores = list(scores)
    frontier = []
    for candidate in all_scores:
        dominated = any(
            other.execution_index_bps <= candidate.execution_index_bps
            and other.jump_loss_index_bps <= candidate.jump_loss_index_bps
            and (
                other.execution_index_bps < candidate.execution_index_bps
                or other.jump_loss_index_bps < candidate.jump_loss_index_bps
            )
            for other in all_scores
        )
        if not dominated:
            frontier.append(candidate)
    return sorted(frontier, key=lambda item: item.execution_index_bps)


def parameter_dict(model: Amm) -> dict[str, float]:
    if isinstance(model, CryptoSwap):
        return {"parameter_1": model.amplification, "parameter_2": model.gamma}
    if isinstance(model, CurrentDh2):
        return {
            "parameter_1": model.peak_depth,
            "parameter_2": model.fade_scale,
        }
    if isinstance(model, LogPotentialDh2):
        return {
            "parameter_1": model.depth_multiplier,
            "parameter_2": model.half_width_bps,
        }
    return {"parameter_1": 0.0, "parameter_2": 0.0}


def declared_parameter_bounds(
    model: Amm,
) -> tuple[tuple[float, float], tuple[float, float]]:
    """Return the explicit analytical sweep bounds for one curve family."""

    if isinstance(model, CryptoSwap):
        return (
            CURVE_AMPLIFICATION_MIN,
            CURVE_AMPLIFICATION_MAX,
        ), (CURVE_GAMMA_MIN, CURVE_GAMMA_MAX)
    if isinstance(model, CurrentDh2):
        return (2.0, CURRENT_PEAK_DEPTH_MAX), (1e-6, 0.50)
    if isinstance(model, LogPotentialDh2):
        return (
            PROPOSED_DEPTH_MIN,
            PROPOSED_DEPTH_MAX,
        ), (PROPOSED_HALF_WIDTH_BPS_MIN, PROPOSED_HALF_WIDTH_BPS_MAX)
    return (0.0, 0.0), (0.0, 0.0)


def _iso_target_best(
    constructor: Callable[[float, float], Amm],
    first_values: Sequence[float],
    second_bounds: tuple[float, float],
    target: float,
    metric: Callable[[Amm], float],
    objective: Callable[[Amm], float],
    *,
    diagnostics: dict[str, object] | None = None,
) -> Amm:
    """Return the converged best point across every scan-exposed iso root.

    The second-parameter metric is not assumed to be monotone.  In
    particular, current DH2 can cross the same aggregate execution target on
    both sides of an interior extremum.  We therefore scan the complete log
    domain, refine every sign-changing bracket independently, and retain all
    distinct scan-exposed roots before applying the economic objective.  No
    finite grid can prove that an arbitrarily narrow root was not hidden
    between samples, so the selected first-parameter winner is audited again
    at 257 and 513 second-axis points and must remain stable.

    ``first_values`` is the explicit coarse declared sweep.  For a sweep with
    at least two points, every adjacent log interval is bisected twice and the
    full scan-exposed-root search is repeated only for newly inserted
    first-parameter values.  The incumbent movement from the
    33-point-equivalent grid to the
    final 65-point-equivalent grid must be no more than
    ``ISO_FIRST_OBJECTIVE_CONVERGENCE_BPS``.  Otherwise the search fails closed
    instead of reporting a grid-sensitive sub-bp winner.  Both refinements are
    mandatory: an unchanged first midpoint can otherwise hide an optimum at a
    quarter-cell midpoint.

    A winner at either declared endpoint is domain-bounded and must not be
    described as a global family optimum.  Optional ``diagnostics`` records
    the evaluated grid densities, incumbent objective movement, root count,
    and convergence reason without changing the historical return type.
    """

    if not first_values:
        raise ValueError("first_values cannot be empty")
    if any(not math.isfinite(value) or value <= 0.0 for value in first_values):
        raise ValueError("first_values must be finite and positive")
    if (
        not math.isfinite(second_bounds[0])
        or not math.isfinite(second_bounds[1])
        or second_bounds[0] <= 0.0
        or second_bounds[0] >= second_bounds[1]
    ):
        raise ValueError("second_bounds must be finite, positive, and ordered")

    coarse_first_values = sorted(set(float(value) for value in first_values))
    candidates_by_first: dict[tuple[float, int], list[Amm]] = {}
    candidate_coordinates: dict[str, tuple[float, float]] = {}
    finite_metric_values: list[float] = []

    def candidates_at_first(
        first: float,
        scan_points: int = ISO_SECOND_SCAN_POINTS,
    ) -> list[Amm]:
        cache_key = (first, scan_points)
        cached = candidates_by_first.get(cache_key)
        if cached is not None:
            return cached

        log_low = math.log(second_bounds[0])
        log_high = math.log(second_bounds[1])
        grid = [
            log_low
            + (log_high - log_low) * index / (scan_points - 1)
            for index in range(scan_points)
        ]
        grid[0] = log_low
        grid[-1] = log_high

        def second_at(point: float) -> float:
            if point <= log_low:
                return second_bounds[0]
            if point >= log_high:
                return second_bounds[1]
            return math.exp(point)

        def residual_at(point: float) -> float | None:
            try:
                value = metric(constructor(first, second_at(point))) - target
            except (ArithmeticError, OverflowError, ValueError, ZeroDivisionError):
                return None
            return value if math.isfinite(value) else None

        residuals = [residual_at(point) for point in grid]
        finite_metric_values.extend(
            target + residual
            for residual in residuals
            if residual is not None
        )
        root_logs: list[float] = []
        exact_tolerance = max(1e-6, abs(target) * 1e-8)
        for point, residual in zip(grid, residuals):
            if residual is not None and abs(residual) <= exact_tolerance:
                root_logs.append(point)
        for left, right, left_residual, right_residual in zip(
            grid,
            grid[1:],
            residuals,
            residuals[1:],
        ):
            if left_residual is None or right_residual is None:
                continue
            if left_residual == 0.0 or right_residual == 0.0:
                continue
            if left_residual * right_residual > 0.0:
                continue
            low = left
            high = right
            low_residual = left_residual
            for _ in range(48):
                middle = (low + high) / 2.0
                middle_residual = residual_at(middle)
                if middle_residual is None:
                    # Never bridge a non-finite island.  The finite scan on
                    # either side will discover any independently bracketed
                    # root.
                    break
                if middle_residual == 0.0:
                    root_logs.append(middle)
                    break
                if low_residual * middle_residual <= 0.0:
                    high = middle
                else:
                    low = middle
                    low_residual = middle_residual
            else:
                root_logs.append((low + high) / 2.0)

        # A sign-change scan cannot see an even-multiplicity tangent.  Search
        # only finite, same-sign, three-point local minima of |residual| and
        # accept the bounded minimum only when it satisfies the same strict
        # metric tolerance as a bracketed root.
        phi = (math.sqrt(5.0) - 1.0) / 2.0
        for index in range(1, len(grid) - 1):
            left_residual = residuals[index - 1]
            middle_residual = residuals[index]
            right_residual = residuals[index + 1]
            if (
                left_residual is None
                or middle_residual is None
                or right_residual is None
                or left_residual * right_residual <= 0.0
                or left_residual * middle_residual <= 0.0
                or middle_residual * right_residual <= 0.0
                or abs(middle_residual) >= abs(left_residual)
                or abs(middle_residual) >= abs(right_residual)
            ):
                continue
            low = grid[index - 1]
            high = grid[index + 1]
            c = high - phi * (high - low)
            d = low + phi * (high - low)

            def absolute_residual(point: float) -> float:
                residual = residual_at(point)
                return math.inf if residual is None else abs(residual)

            fc = absolute_residual(c)
            fd = absolute_residual(d)
            for _ in range(48):
                if fc <= fd:
                    high, d, fd = d, c, fc
                    c = high - phi * (high - low)
                    fc = absolute_residual(c)
                else:
                    low, c, fc = c, d, fd
                    d = low + phi * (high - low)
                    fd = absolute_residual(d)
            tangent = (low + high) / 2.0
            if absolute_residual(tangent) <= exact_tolerance:
                root_logs.append(tangent)

        accepted_root_logs: list[float] = []
        resolved_candidates: list[Amm] = []
        for root_log in sorted(root_logs):
            if any(
                abs(root_log - existing) <= 1e-8
                for existing in accepted_root_logs
            ):
                continue
            candidate = constructor(first, second_at(root_log))
            resolved = residual_at(root_log)
            if resolved is None or abs(resolved) > exact_tolerance:
                continue
            accepted_root_logs.append(root_log)
            resolved_candidates.append(candidate)
            candidate_coordinates[repr(candidate)] = (
                first,
                second_at(root_log),
            )
        candidates_by_first[cache_key] = resolved_candidates
        return resolved_candidates

    def coordinates(model: Amm) -> tuple[float, float]:
        try:
            return candidate_coordinates[repr(model)]
        except KeyError:
            parameters = parameter_dict(model)
            return parameters["parameter_1"], parameters["parameter_2"]

    def finite_incumbent(candidates: Sequence[Amm]) -> tuple[Amm, float]:
        finite_candidates: list[tuple[float, Amm]] = []
        for model in candidates:
            try:
                value = float(objective(model))
            except (ArithmeticError, OverflowError, ValueError, ZeroDivisionError):
                continue
            if math.isfinite(value):
                finite_candidates.append((value, model))
        if finite_candidates:
            value, model = min(
                finite_candidates,
                key=lambda item: (item[0], repr(item[1])),
            )
            return model, value
        raise ArithmeticError("no finite objective exists for scan-exposed iso roots")

    def incumbent(values: Sequence[float]) -> tuple[Amm, float]:
        candidates = [
            model
            for first in values
            for model in candidates_at_first(first)
        ]
        if candidates:
            try:
                return finite_incumbent(candidates)
            except ArithmeticError:
                pass

        observed = (
            f"; finite sampled metric range "
            f"[{min(finite_metric_values):.12g}, {max(finite_metric_values):.12g}]"
            if finite_metric_values
            else "; no finite metric evaluations"
        )
        raise ArithmeticError(
            "no finite iso-target root exists in the declared parameter domain"
            f" for target {target:.12g}{observed}"
        )

    def densify(values: Sequence[float]) -> list[float]:
        refined: list[float] = []
        for left, right in zip(values, values[1:]):
            refined.extend((left, math.sqrt(left * right)))
        refined.append(values[-1])
        return refined

    active_values = coarse_first_values
    best, best_objective = incumbent(active_values)
    level_records: list[dict[str, object]] = [
        {
            "level": 0,
            "first_grid_points": len(active_values),
            "incumbent_first_parameter": coordinates(best)[0],
            "incumbent_second_parameter": coordinates(best)[1],
            "incumbent_objective": best_objective,
            "objective_improvement_from_previous": None,
        }
    ]
    converged = len(active_values) == 1
    convergence_reason = "single_declared_first_parameter"
    last_improvement = 0.0
    last_first_parameter_log_change = 0.0
    last_second_parameter_log_change = 0.0

    for refinement in range(1, ISO_FIRST_MAX_REFINEMENTS + 1):
        if len(active_values) == 1:
            break
        active_values = densify(active_values)
        refined_best, refined_objective = incumbent(active_values)
        prior_first, prior_second = coordinates(best)
        refined_first, refined_second = coordinates(refined_best)
        # Adding candidates cannot worsen the deterministic minimum.  Clamp
        # only sub-ulp negative noise so diagnostics remain easy to interpret.
        last_improvement = max(best_objective - refined_objective, 0.0)
        last_first_parameter_log_change = abs(
            math.log(refined_first / prior_first)
        )
        last_second_parameter_log_change = abs(
            math.log(refined_second / prior_second)
        )
        level_records.append(
            {
                "level": refinement,
                "first_grid_points": len(active_values),
                "incumbent_first_parameter": coordinates(refined_best)[0],
                "incumbent_second_parameter": coordinates(refined_best)[1],
                "incumbent_objective": refined_objective,
                "objective_improvement_from_previous": last_improvement,
                "first_parameter_log_change_from_previous": (
                    last_first_parameter_log_change
                ),
                "second_parameter_log_change_from_previous": (
                    last_second_parameter_log_change
                ),
            }
        )
        best = refined_best
        best_objective = refined_objective
        if refinement == ISO_FIRST_MAX_REFINEMENTS:
            if (
                last_improvement <= ISO_FIRST_OBJECTIVE_CONVERGENCE_BPS
                and last_first_parameter_log_change
                <= ISO_FIRST_PARAMETER_LOG_CONVERGENCE
                and last_second_parameter_log_change
                <= ISO_SECOND_PARAMETER_LOG_CONVERGENCE
            ):
                converged = True
                convergence_reason = (
                    "objective_and_parameters_stable_between_final_global_"
                    "log_midpoint_grids"
                )
            else:
                convergence_reason = (
                    "objective_still_moving_after_final_global_log_midpoints"
                )

    first_converged = converged
    second_converged = False
    second_convergence_reason = "not_run_because_first_axis_did_not_converge"
    second_level_records: list[dict[str, object]] = []
    if first_converged:
        winner_first = coordinates(best)[0]
        for scan_points in (ISO_SECOND_SCAN_POINTS, *ISO_SECOND_AUDIT_SCAN_POINTS):
            scan_candidates = candidates_at_first(winner_first, scan_points)
            try:
                scan_best, scan_objective = finite_incumbent(scan_candidates)
            except ArithmeticError:
                second_convergence_reason = (
                    f"no_finite_winner_at_{scan_points}_second_axis_points"
                )
                break
            scan_first, scan_second = coordinates(scan_best)
            second_level_records.append(
                {
                    "second_scan_points": scan_points,
                    "scan_exposed_root_count": len(scan_candidates),
                    "incumbent_first_parameter": scan_first,
                    "incumbent_second_parameter": scan_second,
                    "incumbent_objective": scan_objective,
                }
            )
        if len(second_level_records) == 3:
            penultimate = second_level_records[-2]
            final = second_level_records[-1]
            objective_delta = abs(
                float(final["incumbent_objective"])
                - float(penultimate["incumbent_objective"])
            )
            second_log_delta = abs(
                math.log(
                    float(final["incumbent_second_parameter"])
                    / float(penultimate["incumbent_second_parameter"])
                )
            )
            root_count_stable = (
                int(final["scan_exposed_root_count"])
                == int(penultimate["scan_exposed_root_count"])
            )
            second_converged = (
                objective_delta <= ISO_FIRST_OBJECTIVE_CONVERGENCE_BPS
                and second_log_delta <= ISO_SECOND_PARAMETER_LOG_CONVERGENCE
                and root_count_stable
            )
            second_convergence_reason = (
                "winner_objective_parameter_and_scan_exposed_root_count_stable_"
                "between_257_and_513_points"
                if second_converged
                else "winner_changed_between_257_and_513_second_axis_points"
            )
            if second_converged:
                best, best_objective = finite_incumbent(
                    candidates_at_first(winner_first, ISO_SECOND_AUDIT_SCAN_POINTS[-1])
                )

    metadata: dict[str, object] = {
        "coarse_first_grid_points": len(coarse_first_values),
        "final_first_grid_points": len(active_values),
        "second_scan_points_per_first": ISO_SECOND_SCAN_POINTS,
        "max_first_refinements": ISO_FIRST_MAX_REFINEMENTS,
        "first_grid_levels": level_records,
        "unique_first_values_evaluated": len(
            {
                first
                for first, scan_points in candidates_by_first
                if scan_points == ISO_SECOND_SCAN_POINTS
            }
        ),
        "accepted_iso_roots": sum(
            len(values)
            for (first, scan_points), values in candidates_by_first.items()
            if scan_points == ISO_SECOND_SCAN_POINTS
        ),
        "objective_convergence_tolerance_bps": (
            ISO_FIRST_OBJECTIVE_CONVERGENCE_BPS
        ),
        "last_objective_improvement_bps": last_improvement,
        "first_axis_parameter_log_convergence_tolerance": (
            ISO_FIRST_PARAMETER_LOG_CONVERGENCE
        ),
        "last_first_parameter_log_change": last_first_parameter_log_change,
        "last_second_parameter_log_change": last_second_parameter_log_change,
        "first_axis_converged": first_converged,
        "convergence_reason": convergence_reason,
        "second_axis_winner_audit_points": [
            ISO_SECOND_SCAN_POINTS,
            *ISO_SECOND_AUDIT_SCAN_POINTS,
        ],
        "second_axis_winner_levels": second_level_records,
        "second_axis_parameter_log_convergence_tolerance": (
            ISO_SECOND_PARAMETER_LOG_CONVERGENCE
        ),
        "second_axis_converged": second_converged,
        "second_axis_convergence_reason": second_convergence_reason,
        "converged": first_converged and second_converged,
    }
    if diagnostics is not None:
        diagnostics.clear()
        diagnostics.update(metadata)
    if not first_converged:
        raise ArithmeticError(
            "first-parameter iso-target optimum did not converge within "
            f"{ISO_FIRST_OBJECTIVE_CONVERGENCE_BPS:.12g} objective bps after "
            f"{len(active_values)} globally interleaved log-grid points; "
            f"last improvement was {last_improvement:.12g} bps, first-axis "
            f"parameter log change was {last_first_parameter_log_change:.12g}, "
            "and second-axis parameter log change was "
            f"{last_second_parameter_log_change:.12g}"
        )
    if not second_converged:
        raise ArithmeticError(
            "second-parameter iso-target winner audit did not converge between "
            "257 and 513 scan points: "
            f"{second_convergence_reason}"
        )
    return best


def equal_execution_models(
    target_execution_bps: float,
    *,
    diagnostics: dict[str, dict[str, object]] | None = None,
) -> tuple[CryptoSwap, CurrentDh2, LogPotentialDh2]:
    curve_diagnostics: dict[str, object] = {}
    curve = _iso_target_best(
        lambda first, second: CryptoSwap(amplification=first, gamma=second),
        _log_grid(
            CURVE_AMPLIFICATION_MIN,
            CURVE_AMPLIFICATION_MAX,
            ISO_FIRST_GRID_POINTS,
        ),
        (CURVE_GAMMA_MIN, CURVE_GAMMA_MAX),
        target_execution_bps,
        execution_index_bps,
        jump_loss_index_bps,
        diagnostics=curve_diagnostics,
    )
    current_diagnostics: dict[str, object] = {}
    current = _iso_target_best(
        lambda first, second: CurrentDh2(
            peak_depth=first,
            fade_scale=second,
        ),
        _log_grid(2.0, CURRENT_PEAK_DEPTH_MAX, ISO_FIRST_GRID_POINTS),
        (1e-6, 0.50),
        target_execution_bps,
        execution_index_bps,
        jump_loss_index_bps,
        diagnostics=current_diagnostics,
    )
    proposed_diagnostics: dict[str, object] = {}
    proposed = _iso_target_best(
        lambda first, second: LogPotentialDh2(
            depth_multiplier=first,
            half_width_bps=second,
        ),
        _log_grid(
            PROPOSED_DEPTH_MIN,
            PROPOSED_DEPTH_MAX,
            ISO_FIRST_GRID_POINTS,
        ),
        (PROPOSED_HALF_WIDTH_BPS_MIN, PROPOSED_HALF_WIDTH_BPS_MAX),
        target_execution_bps,
        execution_index_bps,
        jump_loss_index_bps,
        diagnostics=proposed_diagnostics,
    )
    if diagnostics is not None:
        diagnostics.clear()
        diagnostics.update(
            {
                "curve": curve_diagnostics,
                "current_dh2": current_diagnostics,
                "proposed_dh2": proposed_diagnostics,
            }
        )
    return curve, current, proposed


def equal_risk_models(
    target_jump_loss_bps: float,
    *,
    diagnostics: dict[str, dict[str, object]] | None = None,
) -> tuple[CryptoSwap, CurrentDh2, LogPotentialDh2]:
    curve_diagnostics: dict[str, object] = {}
    curve = _iso_target_best(
        lambda first, second: CryptoSwap(amplification=first, gamma=second),
        _log_grid(
            CURVE_AMPLIFICATION_MIN,
            CURVE_AMPLIFICATION_MAX,
            ISO_FIRST_GRID_POINTS,
        ),
        (CURVE_GAMMA_MIN, CURVE_GAMMA_MAX),
        target_jump_loss_bps,
        jump_loss_index_bps,
        execution_index_bps,
        diagnostics=curve_diagnostics,
    )
    current_diagnostics: dict[str, object] = {}
    current = _iso_target_best(
        lambda first, second: CurrentDh2(
            peak_depth=first,
            fade_scale=second,
        ),
        _log_grid(2.0, CURRENT_PEAK_DEPTH_MAX, ISO_FIRST_GRID_POINTS),
        (1e-6, 0.50),
        target_jump_loss_bps,
        jump_loss_index_bps,
        execution_index_bps,
        diagnostics=current_diagnostics,
    )
    proposed_diagnostics: dict[str, object] = {}
    proposed = _iso_target_best(
        lambda first, second: LogPotentialDh2(
            depth_multiplier=first,
            half_width_bps=second,
        ),
        _log_grid(
            PROPOSED_DEPTH_MIN,
            PROPOSED_DEPTH_MAX,
            ISO_FIRST_GRID_POINTS,
        ),
        (PROPOSED_HALF_WIDTH_BPS_MIN, PROPOSED_HALF_WIDTH_BPS_MAX),
        target_jump_loss_bps,
        jump_loss_index_bps,
        execution_index_bps,
        diagnostics=proposed_diagnostics,
    )
    if diagnostics is not None:
        diagnostics.clear()
        diagnostics.update(
            {
                "curve": curve_diagnostics,
                "current_dh2": current_diagnostics,
                "proposed_dh2": proposed_diagnostics,
            }
        )
    return curve, current, proposed

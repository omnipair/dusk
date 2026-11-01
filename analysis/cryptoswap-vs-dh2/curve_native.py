"""Independent analytical policy model for a native two-coin CryptoSwap pool.

The invariant comes from :class:`models.CryptoSwap`.  This module supplies
the surrounding state machine: physical reserves, price normalization,
output-side fee retention, an internal trade-price EMA, profit accounting,
and profit-gated movement of the normalization price.

The implementation is intentionally organized around benchmark concepts
rather than any deployed contract's storage or call structure.  In
particular, the controller API never receives an external reference price.
External prices are used only by the simulated order generator, arbitrageur,
and mark-to-market ledger.
"""

from __future__ import annotations

import copy
from dataclasses import dataclass
import math
import random
from typing import Optional, Sequence

from economics import (
    ARBITRAGE_GOLDEN_ITERATIONS,
    ARBITRAGE_SEARCH_GRID_COUNT,
    POOL_VALUE,
    SLOTS_PER_MINUTE,
    order_tape_for_step,
)
from models import CryptoSwap


OBSERVATION_CAP_MULTIPLE = 2.0
# Curve can move its price scale after each profitable fill, reopening the
# same-slot arbitrage.  Keep this emergency bound local so the shared
# DH2/CPMM cascade limit remains unchanged.
MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT = 64
REFERENCE_RELEASE = "Twocrypto-NG v2.1.0"
REFERENCE_COMMIT = "5cbe558902402e8fcb331463089db65fc56c11f9"
_ACCOUNTING_REL_TOLERANCE = 2e-11
# V2.1 hard-codes a 50% admin share of the post-rebalancing half of
# accumulated xcp profit: 0.50 / 2 = 0.25 of gross profit accrual.
ADMIN_PROFIT_LIABILITY_FRACTION = 0.25


@dataclass(frozen=True)
class CurveNativeConfig:
    """Controller parameters that are independent of invariant shape."""

    ema_half_life_minutes: float = 30.0
    min_adjustment_step: float = 0.001
    allowed_extra_profit: float = 0.000002
    arb_min_profit_quote: float = 0.10

    def validate(self) -> None:
        if self.ema_half_life_minutes < 0:
            raise ValueError("EMA half-life cannot be negative")
        if not 0 <= self.min_adjustment_step < 1:
            raise ValueError("minimum adjustment step must be in [0, 1)")
        if self.allowed_extra_profit < 0:
            raise ValueError("allowed extra profit cannot be negative")
        if self.arb_min_profit_quote < 0:
            raise ValueError("minimum arbitrage profit cannot be negative")


@dataclass
class CurveNativeState:
    """Pool state with token reserves kept in their physical units."""

    base: float
    quote: float
    price_scale: float
    ema_price: float
    last_state_price: float
    last_observation_slot: int
    initial_xcp: float
    virtual_price: float = 1.0
    xcp_profit: float = 1.0
    xcp_profit_a: float = 1.0
    retained_base_fees: float = 0.0
    retained_quote_fees: float = 0.0
    gross_fee_value: float = 0.0
    baseline_fee_value: float = 0.0
    excess_fee_value: float = 0.0
    realized_arb_profit: float = 0.0
    arbitrage_fill_count: int = 0
    arbitrage_capped_event_count: int = 0
    arbitrage_numerical_failure_count: int = 0
    informed_flow_profit: float = 0.0
    uninformed_flow_profit: float = 0.0
    pool_flow_pnl: float = 0.0
    inventory_carry: float = 0.0
    recenter_impairment_xcp: float = 0.0
    requested_recenter_impairment_xcp: float = 0.0
    recenter_count: int = 0
    recenter_attempt_count: int = 0
    blocked_recenter_count: int = 0
    requested_center_move_log: float = 0.0
    funded_center_move_log: float = 0.0
    stale_steps: int = 0
    quote_eligible_steps: int = 0
    total_steps: int = 0
    peak_wealth: float = 0.0
    max_drawdown: float = 0.0


@dataclass(frozen=True)
class CurveNativeTrade:
    direction: str
    gross_input: float
    input_token: str
    raw_output: float
    output_amount: float
    output_token: str
    retained_output_fee: float
    fee_rate: float
    marginal_price_before: float
    marginal_price_after: float
    recentered: bool
    accounting_identity_error: float


@dataclass(frozen=True)
class CurveNativeOrderResult:
    trade: CurveNativeTrade
    external_notional: float
    trader_profit_at_reference: float
    total_fee_value: float
    baseline_fee_value: float
    excess_fee_value: float


def _require_positive_finite(value: float, name: str) -> None:
    if value <= 0 or not math.isfinite(value):
        raise ValueError(f"{name} must be positive and finite")


def normalized_reserves(state: CurveNativeState) -> tuple[float, float]:
    """Return reserves in the invariant's common quote-value coordinates."""

    return state.base * state.price_scale, state.quote


def xcp_at_scale(
    model: CryptoSwap,
    state: CurveNativeState,
    price_scale: Optional[float] = None,
) -> float:
    """Balanced-reserve value implied by D at a candidate normalization."""

    scale = state.price_scale if price_scale is None else price_scale
    _require_positive_finite(scale, "price scale")
    d = model.invariant(state.base * scale, state.quote)
    return d / (2.0 * math.sqrt(scale))


def state_price(model: CryptoSwap, state: CurveNativeState) -> float:
    """Fee-free marginal base price implied only by the pool's own state."""

    common_base, quote = normalized_reserves(state)
    return state.price_scale * model.marginal_price_raw(common_base, quote)


def total_wealth(state: CurveNativeState, external_price: float) -> float:
    _require_positive_finite(external_price, "external price")
    return state.base * external_price + state.quote


def initialize_curve_native_pool(
    model: CryptoSwap,
    initial_price: float,
    *,
    pool_value: float = POOL_VALUE,
    config: Optional[CurveNativeConfig] = None,
) -> CurveNativeState:
    _require_positive_finite(initial_price, "initial price")
    _require_positive_finite(pool_value, "pool value")
    controller = config or CurveNativeConfig()
    controller.validate()
    state = CurveNativeState(
        base=pool_value / (2.0 * initial_price),
        quote=pool_value / 2.0,
        price_scale=initial_price,
        ema_price=initial_price,
        last_state_price=initial_price,
        last_observation_slot=0,
        initial_xcp=1.0,
        peak_wealth=pool_value,
    )
    state.initial_xcp = xcp_at_scale(model, state)
    return state


def _roll_ema_before_trade(
    state: CurveNativeState,
    config: CurveNativeConfig,
    current_slot: int,
) -> None:
    """Fold the preceding successful trade observation into the EMA.

    The observation is bounded above by twice the current normalization
    price.  This limits how much one extreme state can pull the oracle in a
    single elapsed observation interval without importing an outside price.
    """

    if current_slot < state.last_observation_slot:
        raise ValueError("trade slot cannot move backwards")
    if current_slot == state.last_observation_slot:
        return
    elapsed_minutes = (
        current_slot - state.last_observation_slot
    ) / SLOTS_PER_MINUTE
    observation = min(
        state.last_state_price,
        OBSERVATION_CAP_MULTIPLE * state.price_scale,
    )
    if config.ema_half_life_minutes == 0:
        state.ema_price = observation
    else:
        old_weight = math.exp(
            -math.log(2.0)
            * elapsed_minutes
            / config.ema_half_life_minutes
        )
        state.ema_price = (
            old_weight * state.ema_price
            + (1.0 - old_weight) * observation
        )
    state.last_observation_slot = current_slot


def _refresh_profit_after_swap(
    model: CryptoSwap,
    state: CurveNativeState,
) -> None:
    old_virtual_price = state.virtual_price
    if old_virtual_price <= 0:
        raise ArithmeticError("stored virtual price must be positive")
    new_virtual_price = xcp_at_scale(model, state) / state.initial_xcp
    tolerance = max(old_virtual_price * 5e-11, 5e-12)
    if new_virtual_price + tolerance < old_virtual_price:
        raise ArithmeticError("an output-retained swap reduced virtual price")
    if new_virtual_price < old_virtual_price:
        new_virtual_price = old_virtual_price
    state.xcp_profit *= new_virtual_price / old_virtual_price
    state.virtual_price = new_virtual_price


def native_fee_rate(model: CryptoSwap, x: float, y: float) -> float:
    """Twocrypto-NG v2.1 imbalance fee evaluated in normalized reserves.

    ``q`` is one at equal normalized reserves and tends to zero in either
    tail.  The mixing weight therefore selects ``mid_fee`` at balance and
    approaches ``out_fee`` with imbalance.
    """

    _require_positive_finite(x, "normalized base reserve")
    _require_positive_finite(y, "normalized quote reserve")
    q = 4.0 * x * y / ((x + y) * (x + y))
    denominator = model.fee_gamma * q + 1.0 - q
    weight = 1.0 if denominator == 0.0 else model.fee_gamma * q / denominator
    return model.out_fee - weight * (model.out_fee - model.mid_fee)


def maybe_recenter(
    model: CryptoSwap,
    state: CurveNativeState,
    config: CurveNativeConfig,
) -> bool:
    """Move price_scale only when at least half of gross profit survives.

    Distance and step are relative fractions.  A requested move covers one
    fifth of the remaining EMA gap unless the configured minimum step is
    larger.  The physical token reserves never change during this operation.
    """

    config.validate()
    # v2.1 uses the safety allowance only to decide whether an attempt is
    # worth making.  The proposed endpoint is then tested against the exact
    # half-profit condition without that allowance.
    pre_gate = state.virtual_price - config.allowed_extra_profit
    if pre_gate <= 0.0 or pre_gate * pre_gate <= state.xcp_profit:
        return False
    distance = abs(state.ema_price / state.price_scale - 1.0)
    step = max(config.min_adjustment_step, distance / 5.0)
    if distance <= step or distance == 0:
        return False

    direction = 1.0 if state.ema_price > state.price_scale else -1.0
    candidate_scale = state.price_scale * (1.0 + direction * step)
    if direction > 0:
        candidate_scale = min(candidate_scale, state.ema_price)
    else:
        candidate_scale = max(candidate_scale, state.ema_price)
    _require_positive_finite(candidate_scale, "candidate price scale")

    old_xcp = xcp_at_scale(model, state)
    candidate_xcp = xcp_at_scale(model, state, candidate_scale)
    state.recenter_attempt_count += 1
    requested_move = abs(math.log(candidate_scale / state.price_scale))
    state.requested_center_move_log += requested_move
    state.requested_recenter_impairment_xcp += max(old_xcp - candidate_xcp, 0.0)

    candidate_virtual_price = candidate_xcp / state.initial_xcp
    # Squaring normalized realized wealth is the exact v2.1 gate.  Its
    # first-order expansion is the familiar rule that a move may spend at
    # most roughly half of accumulated gross profit.
    if (
        candidate_virtual_price <= 1.0
        or candidate_virtual_price * candidate_virtual_price <= state.xcp_profit
    ):
        state.blocked_recenter_count += 1
        return False

    base_before = state.base
    quote_before = state.quote
    state.price_scale = candidate_scale
    state.virtual_price = candidate_virtual_price
    state.funded_center_move_log += requested_move
    state.recenter_impairment_xcp += max(old_xcp - candidate_xcp, 0.0)
    state.recenter_count += 1
    if state.base != base_before or state.quote != quote_before:
        raise ArithmeticError("recentring changed physical reserves")
    return True


def execute_curve_native_trade(
    model: CryptoSwap,
    state: CurveNativeState,
    gross_input: float,
    direction: str,
    current_slot: int,
    config: Optional[CurveNativeConfig] = None,
) -> CurveNativeTrade:
    """Execute one physical exact-input swap without an external oracle.

    The invariant first determines a fee-free output.  The native imbalance
    fee is then withheld from that output, leaving the fee in the output-side
    reserve.  Consequently the entire gross input also enters its reserve.
    """

    _require_positive_finite(gross_input, "gross input")
    controller = config or CurveNativeConfig()
    controller.validate()
    _roll_ema_before_trade(state, controller, current_slot)

    base_before = state.base
    quote_before = state.quote
    invariant_before = model.invariant(*normalized_reserves(state))
    marginal_before = state_price(model, state)
    common_base, quote = normalized_reserves(state)

    if direction == "x_to_y":
        common_input = gross_input * state.price_scale
        raw_quote = model.quote(
            common_base,
            quote,
            common_input,
            direction,
            apply_fee=False,
            include_marginals=False,
        )
        raw_output = raw_quote.amount_out
        raw_post_common_base = common_base + common_input
        raw_post_quote = quote - raw_output
        fee_rate = native_fee_rate(model, raw_post_common_base, raw_post_quote)
        retained_fee = raw_output * fee_rate
        output = raw_output - retained_fee
        state.base += gross_input
        state.quote -= output
        state.retained_quote_fees += retained_fee
        input_token = "base"
        output_token = "quote"
        accounting_error = max(
            abs((state.base - base_before) - gross_input),
            abs((quote_before - state.quote) - output),
            abs(raw_output - output - retained_fee),
        )
    elif direction == "y_to_x":
        raw_base_common = model.quote(
            common_base,
            quote,
            gross_input,
            direction,
            apply_fee=False,
            include_marginals=False,
        )
        raw_output_common = raw_base_common.amount_out
        raw_post_common_base = common_base - raw_output_common
        raw_post_quote = quote + gross_input
        fee_rate = native_fee_rate(model, raw_post_common_base, raw_post_quote)
        retained_fee_common = raw_output_common * fee_rate
        output_common = raw_output_common - retained_fee_common
        raw_output = raw_output_common / state.price_scale
        retained_fee = retained_fee_common / state.price_scale
        output = output_common / state.price_scale
        state.quote += gross_input
        state.base -= output
        state.retained_base_fees += retained_fee
        input_token = "quote"
        output_token = "base"
        accounting_error = max(
            abs((state.quote - quote_before) - gross_input),
            abs((base_before - state.base) - output),
            abs(raw_output - output - retained_fee),
        )
    else:
        raise ValueError(f"unknown direction {direction}")

    scale = max(base_before, quote_before, gross_input, raw_output, 1.0)
    if accounting_error > scale * _ACCOUNTING_REL_TOLERANCE:
        raise ArithmeticError("native Curve swap accounting did not reconcile")
    invariant_after = model.invariant(*normalized_reserves(state))
    invariant_tolerance = max(invariant_before * 5e-11, 1e-8)
    if invariant_after + invariant_tolerance < invariant_before:
        raise ArithmeticError("retaining the output fee reduced the invariant")

    _refresh_profit_after_swap(model, state)
    post_trade_state_price = state_price(model, state)
    state.last_state_price = post_trade_state_price
    recentered = maybe_recenter(model, state, controller)
    marginal_after = state_price(model, state)

    return CurveNativeTrade(
        direction=direction,
        gross_input=gross_input,
        input_token=input_token,
        raw_output=raw_output,
        output_amount=output,
        output_token=output_token,
        retained_output_fee=retained_fee,
        fee_rate=fee_rate,
        marginal_price_before=marginal_before,
        marginal_price_after=marginal_after,
        recentered=recentered,
        accounting_identity_error=accounting_error,
    )


def execute_curve_native_order(
    model: CryptoSwap,
    state: CurveNativeState,
    external_notional: float,
    direction: str,
    external_price: float,
    current_slot: int,
    config: Optional[CurveNativeConfig] = None,
) -> CurveNativeOrderResult:
    """Size/mark an order externally, while keeping the pool call oracle-free."""

    _require_positive_finite(external_notional, "external notional")
    _require_positive_finite(external_price, "external price")
    wealth_before = total_wealth(state, external_price)
    gross_input = (
        external_notional / external_price
        if direction == "x_to_y"
        else external_notional
    )
    trade = execute_curve_native_trade(
        model,
        state,
        gross_input,
        direction,
        current_slot,
        config,
    )
    if direction == "x_to_y":
        output_value = trade.output_amount
        fee_value = trade.retained_output_fee
    else:
        output_value = trade.output_amount * external_price
        fee_value = trade.retained_output_fee * external_price
    trader_profit = output_value - external_notional
    wealth_after = total_wealth(state, external_price)
    identity_error = (wealth_after - wealth_before) + trader_profit
    tolerance = max(wealth_before * 2e-11, 1e-7)
    if abs(identity_error) > tolerance:
        raise ArithmeticError("pool and trader marked-value ledgers do not reconcile")
    state.gross_fee_value += fee_value
    baseline_fraction = (
        min(model.mid_fee, trade.fee_rate) / trade.fee_rate
        if trade.fee_rate > 0
        else 0.0
    )
    baseline_fee_value = fee_value * baseline_fraction
    excess_fee_value = fee_value - baseline_fee_value
    state.baseline_fee_value += baseline_fee_value
    state.excess_fee_value += excess_fee_value
    state.pool_flow_pnl -= trader_profit
    return CurveNativeOrderResult(
        trade=trade,
        external_notional=external_notional,
        trader_profit_at_reference=trader_profit,
        total_fee_value=fee_value,
        baseline_fee_value=baseline_fee_value,
        excess_fee_value=excess_fee_value,
    )


def preview_curve_native_order(
    model: CryptoSwap,
    state: CurveNativeState,
    external_notional: float,
    direction: str,
    external_price: float,
    current_slot: int,
    config: Optional[CurveNativeConfig] = None,
) -> tuple[CurveNativeState, CurveNativeOrderResult]:
    candidate = copy.deepcopy(state)
    result = execute_curve_native_order(
        model,
        candidate,
        external_notional,
        direction,
        external_price,
        current_slot,
        config,
    )
    return candidate, result


def _best_arbitrage(
    model: CryptoSwap,
    state: CurveNativeState,
    external_price: float,
    current_slot: int,
    config: CurveNativeConfig,
    numerical_failures: Optional[list[int]] = None,
) -> Optional[tuple[CurveNativeState, CurveNativeOrderResult]]:
    pool_price = state_price(model, state)
    relative_error = pool_price / external_price - 1.0
    if abs(relative_error) < 1e-8:
        return None
    direction = "x_to_y" if relative_error > 0 else "y_to_x"
    maximum = min(
        total_wealth(state, external_price) * 0.60,
        (
            state.base * external_price
            if direction == "x_to_y"
            else state.quote
        )
        * 0.90,
    )
    minimum = max(maximum * 1e-8, 1e-6)
    if maximum <= minimum:
        return None

    grid_count = ARBITRAGE_SEARCH_GRID_COUNT
    amounts = [
        minimum
        * math.exp(math.log(maximum / minimum) * index / (grid_count - 1))
        for index in range(grid_count)
    ]

    def evaluate(
        amount: float,
    ) -> tuple[float, Optional[CurveNativeState], Optional[CurveNativeOrderResult]]:
        try:
            candidate, result = preview_curve_native_order(
                model,
                state,
                amount,
                direction,
                external_price,
                current_slot,
                config,
            )
            return result.trader_profit_at_reference, candidate, result
        except ValueError:
            return -math.inf, None, None
        except ArithmeticError:
            if numerical_failures is not None:
                numerical_failures[0] += 1
            return -math.inf, None, None

    samples = [evaluate(amount) for amount in amounts]
    best_index = max(range(len(samples)), key=lambda index: samples[index][0])
    if samples[best_index][0] <= config.arb_min_profit_quote:
        return None

    left = amounts[max(best_index - 1, 0)]
    right = amounts[min(best_index + 1, len(amounts) - 1)]
    phi = (math.sqrt(5.0) - 1.0) / 2.0
    c = right - phi * (right - left)
    d = left + phi * (right - left)
    fc = evaluate(c)
    fd = evaluate(d)
    for _ in range(ARBITRAGE_GOLDEN_ITERATIONS):
        if fc[0] > fd[0]:
            right, d, fd = d, c, fc
            c = right - phi * (right - left)
            fc = evaluate(c)
        else:
            left, c, fc = c, d, fd
            d = left + phi * (right - left)
            fd = evaluate(d)
    best = max((samples[best_index], fc, fd), key=lambda item: item[0])
    if (
        best[0] <= config.arb_min_profit_quote
        or best[1] is None
        or best[2] is None
    ):
        return None
    return best[1], best[2]


def execute_profitable_arbitrage(
    model: CryptoSwap,
    state: CurveNativeState,
    external_price: float,
    current_slot: int,
    config: Optional[CurveNativeConfig] = None,
) -> float:
    """Execute a bounded same-slot sequence after any controller movement."""

    controller = config or CurveNativeConfig()
    controller.validate()
    total_profit = 0.0
    fills = 0
    while fills < MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT:
        numerical_failures = [0]
        best = _best_arbitrage(
            model,
            state,
            external_price,
            current_slot,
            controller,
            numerical_failures,
        )
        if best is None:
            state.arbitrage_numerical_failure_count += numerical_failures[0]
            break
        candidate, result = best
        state.__dict__.update(candidate.__dict__)
        state.arbitrage_numerical_failure_count += numerical_failures[0]
        profit = max(result.trader_profit_at_reference, 0.0)
        state.realized_arb_profit += profit
        state.arbitrage_fill_count += 1
        total_profit += profit
        fills += 1

    if fills == MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT:
        numerical_failures = [0]
        profitable_follow_up = _best_arbitrage(
            model,
            state,
            external_price,
            current_slot,
            controller,
            numerical_failures,
        )
        state.arbitrage_numerical_failure_count += numerical_failures[0]
        if profitable_follow_up is not None:
            state.arbitrage_capped_event_count += 1
    return total_profit


def current_execution_impact_bps(
    model: CryptoSwap,
    state: CurveNativeState,
    external_price: float,
    direction: str,
    *,
    trade_bps: int = 100,
) -> float:
    """Fee-free state impact, matching the controlled benchmark service test."""

    notional = total_wealth(state, external_price) * trade_bps / 10_000.0
    common_base, quote = normalized_reserves(state)
    common_input = (
        notional / external_price * state.price_scale
        if direction == "x_to_y"
        else notional
    )
    result = model.quote(
        common_base,
        quote,
        common_input,
        direction,
        apply_fee=False,
        include_marginals=False,
    )
    output_value = (
        result.amount_out
        if direction == "x_to_y"
        else result.amount_out / state.price_scale * external_price
    )
    return max((1.0 - output_value / notional) * 10_000.0, 0.0)


def simulate_curve_native_path(
    model: CryptoSwap,
    path_name: str,
    prices: Sequence[float],
    *,
    step_minutes: int,
    arb_latency_steps: int,
    informed_share: float,
    daily_volume_pool_ratio: float,
    seed: int,
    config: Optional[CurveNativeConfig] = None,
    pool_value: float = POOL_VALUE,
) -> dict[str, object]:
    """Replay the common order tape through Curve's native policy model."""

    if not prices:
        raise ValueError("price path cannot be empty")
    if step_minutes <= 0 or arb_latency_steps <= 0:
        raise ValueError("step and arbitrage latency must be positive")
    controller = config or CurveNativeConfig()
    controller.validate()
    randomizer = random.Random(seed)
    state = initialize_curve_native_pool(
        model,
        prices[0],
        pool_value=pool_value,
        config=controller,
    )
    initial_base = state.base
    initial_quote = state.quote
    previous_external = prices[0]

    for index, external in enumerate(prices):
        current_slot = index * step_minutes * SLOTS_PER_MINUTE
        if index:
            state.inventory_carry += (
                state.base - initial_base
            ) * (external - previous_external)
        if index % arb_latency_steps == 0:
            execute_profitable_arbitrage(
                model,
                state,
                external,
                current_slot,
                controller,
            )

        next_price = prices[min(index + 1, len(prices) - 1)]
        orders = order_tape_for_step(
            pool_value,
            daily_volume_pool_ratio,
            step_minutes,
            informed_share,
            external,
            next_price,
            randomizer,
        )
        for order in orders:
            try:
                result = execute_curve_native_order(
                    model,
                    state,
                    order.external_notional,
                    order.direction,
                    external,
                    current_slot,
                    controller,
                )
            except (ArithmeticError, OverflowError, ValueError):
                continue
            trade = result.trade
            if order.direction == "x_to_y":
                markout_profit = trade.output_amount - trade.gross_input * next_price
            else:
                markout_profit = trade.output_amount * next_price - trade.gross_input
            if order.informed:
                state.informed_flow_profit += markout_profit
            else:
                state.uninformed_flow_profit += markout_profit

        state.total_steps += 1
        try:
            worst_impact = max(
                current_execution_impact_bps(
                    model,
                    state,
                    external,
                    direction,
                )
                for direction in ("x_to_y", "y_to_x")
            )
        except (ArithmeticError, OverflowError, ValueError):
            worst_impact = math.inf
        if worst_impact <= 50.0:
            state.quote_eligible_steps += 1
        else:
            state.stale_steps += 1
        wealth = total_wealth(state, external)
        state.peak_wealth = max(state.peak_wealth, wealth)
        state.max_drawdown = max(
            state.max_drawdown,
            1.0 - wealth / state.peak_wealth,
        )
        previous_external = external

    final_price = prices[-1]
    final_wealth = total_wealth(state, final_price)
    hold_value = initial_base * final_price + initial_quote
    ledger_gap = final_wealth - hold_value
    xcp_to_value = 2.0 * math.sqrt(final_price)
    required_realized = math.sqrt(state.xcp_profit) + controller.allowed_extra_profit
    available_profit_xcp = max(
        state.virtual_price - required_realized,
        0.0,
    ) * state.initial_xcp
    return {
        "model": "curve",
        "system": "curve_native",
        "path": path_name,
        "path_class": (
            "historical" if path_name.startswith("historical_") else "synthetic"
        ),
        "arb_latency_minutes": arb_latency_steps * step_minutes,
        "informed_flow_share": informed_share,
        "daily_volume_pool_ratio": daily_volume_pool_ratio,
        "seed": seed,
        "final_lp_wealth": final_wealth,
        "hold_value": hold_value,
        "net_lp_pnl_vs_hold": ledger_gap,
        "loss_vs_hold": -ledger_gap,
        "gross_fee_value": state.gross_fee_value,
        "base_fee_value": 0.0,
        "dynamic_fee_value": state.gross_fee_value,
        "retained_surcharge_value": state.gross_fee_value,
        "distributed_surcharge_value": 0.0,
        "claimable_base": 0.0,
        "claimable_quote": 0.0,
        "realized_arb_profit": state.realized_arb_profit,
        "informed_flow_profit": state.informed_flow_profit,
        "uninformed_flow_profit": state.uninformed_flow_profit,
        "recenter_impairment_value": state.recenter_impairment_xcp * xcp_to_value,
        "requested_recenter_impairment_value": (
            state.requested_recenter_impairment_xcp * xcp_to_value
        ),
        "recenter_count": state.recenter_count,
        "recenter_attempt_count": state.recenter_attempt_count,
        "protected_budget_value": available_profit_xcp * xcp_to_value,
        "stale_fraction": state.stale_steps / max(state.total_steps, 1),
        "center_tracking_error_bps": abs(
            math.log(final_price / state.price_scale)
        )
        * 10_000.0,
        "ema_tracking_error_bps": abs(math.log(final_price / state.ema_price))
        * 10_000.0,
        "max_drawdown": state.max_drawdown,
        "ending_base": state.base,
        "ending_quote": state.quote,
        "pool_flow_pnl": state.pool_flow_pnl,
        "inventory_carry": state.inventory_carry,
        "accounting_identity_error": ledger_gap
        - (state.pool_flow_pnl + state.inventory_carry),
        "price_scale": state.price_scale,
        "ema_price": state.ema_price,
        "virtual_price": state.virtual_price,
        "xcp_profit": state.xcp_profit,
    }

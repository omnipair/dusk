"""Controlled economic simulator for concentrated AMM curve comparison.

All curve families receive the same Dusk fee, observation, retention, and
funded-center controller.  External prices are visible only to simulated
traders and to the final accounting ledger; the AMM controller observes its
own successful trade prices.
"""

from __future__ import annotations

from dataclasses import dataclass, replace
import copy
import math
import random
from typing import Optional, Sequence

from comparison import model_family
from dusk_policy import (
    NAD,
    DynamicFeeConfig,
    DynamicFeePreState,
    SwapFeeAssessment,
    assess_swap_fee,
    volatility_after_assessed_swap,
)
from models import Amm, Cpmm, CryptoSwap, CurrentDh2, LogPotentialDh2, with_zero_fees


TOKEN_SCALE = 1_000_000_000
SLOTS_PER_MINUTE = 150
POOL_VALUE = 2_000_000.0
PROTECTED_COVERAGE = 1.25
PROTECTED_GUARD = 0.0001
PROTECTED_CAP = 0.01
PROTECTED_HYSTERESIS = 0.10
MAX_STEP_HALVINGS = 8
ARBITRAGE_SEARCH_GRID_COUNT = 16
ARBITRAGE_GOLDEN_ITERATIONS = 14
MAX_ARBITRAGE_FILLS_PER_EVENT = 16


@dataclass(frozen=True)
class ControlledConfig:
    fee: DynamicFeeConfig
    center_ema_half_life_minutes: float = 30.0
    adjustment_threshold: float = 0.010
    adjustment_step: float = 0.0025
    min_adjustment_interval_minutes: int = 5
    arb_min_profit_quote: float = 0.10
    solver_haircut_ppm: float = 0.0
    recenter_enabled: bool = True
    manager_fee_bps: int = 0
    protocol_fee_bps: int = 0


def default_controlled_config() -> ControlledConfig:
    return ControlledConfig(
        fee=DynamicFeeConfig(
            base_fee_rate_nad=500_000,  # 5 bps
            max_fee_rate_nad=20_500_000,  # 205 bps total
            divergence_coefficient_nad=20_000_000,
            volatility_coefficient_nad=20_000_000,
            volatility_half_life_ms=60 * 60 * 1_000,
            volatility_shock_cap_nad=100_000_000,
            volatility_accumulator_cap_nad=NAD,
        )
    )


@dataclass
class PoolState:
    base: float
    quote: float
    center: float
    ema: float
    last_trade_price: float
    last_observation_slot: int
    volatility_accumulator_nad: int = 0
    claimable_base: float = 0.0
    claimable_quote: float = 0.0
    non_lp_claimable_base: float = 0.0
    non_lp_claimable_quote: float = 0.0
    protected_floor_q: float = 0.0
    current_q: float = 0.0
    retention_required_q: float = 0.0
    retention_stop_q: float = 0.0
    retention_hard_cap_q: float = 0.0
    retention_target_saturated: bool = False
    retention_target_stale: bool = False
    retain_dynamic_surcharge: bool = True
    gross_fee_value: float = 0.0
    base_fee_value: float = 0.0
    dynamic_fee_value: float = 0.0
    retained_surcharge_value: float = 0.0
    distributed_surcharge_value: float = 0.0
    solver_haircut_value: float = 0.0
    manager_fee_value: float = 0.0
    protocol_fee_value: float = 0.0
    realized_arb_profit: float = 0.0
    arbitrage_fill_count: int = 0
    arbitrage_capped_event_count: int = 0
    arbitrage_numerical_failure_count: int = 0
    informed_flow_profit: float = 0.0
    uninformed_flow_profit: float = 0.0
    pool_flow_pnl: float = 0.0
    inventory_carry: float = 0.0
    recenter_impairment_q: float = 0.0
    requested_recenter_impairment_q: float = 0.0
    recenter_count: int = 0
    requested_recenter_count: int = 0
    blocked_recenter_count: int = 0
    requested_center_move_log: float = 0.0
    funded_center_move_log: float = 0.0
    last_adjustment_slot: int = 0
    stale_steps: int = 0
    quote_eligible_steps: int = 0
    total_steps: int = 0
    peak_wealth: float = 0.0
    max_drawdown: float = 0.0


@dataclass(frozen=True)
class TradeResult:
    direction: str
    gross_input: float
    input_token: str
    output_amount: float
    output_token: str
    external_notional: float
    trader_profit_at_reference: float
    total_fee_value: float
    base_fee_value: float
    dynamic_fee_value: float
    retained_surcharge_value: float
    solver_haircut_value: float
    manager_fee_value: float
    protocol_fee_value: float
    assessment: Optional[SwapFeeAssessment]


def common_balanced_reserve(model: Amm, common_x: float, quote_y: float) -> float:
    invariant = model.invariant(common_x, quote_y)
    if isinstance(model, (CurrentDh2, CryptoSwap)):
        return invariant / 2.0
    return invariant


def protected_q(model: Amm, state: PoolState, *, center: Optional[float] = None) -> float:
    candidate_center = state.center if center is None else center
    common = common_balanced_reserve(
        model,
        state.base * candidate_center,
        state.quote,
    )
    return common / math.sqrt(candidate_center)


def spot_price(model: Amm, state: PoolState) -> float:
    return state.center * model.marginal_price_raw(
        state.base * state.center,
        state.quote,
    )


def total_wealth(state: PoolState, external_price: float) -> float:
    return (
        (state.base + state.claimable_base) * external_price
        + state.quote
        + state.claimable_quote
    )


def initialize_pool(
    model: Amm,
    initial_price: float,
    *,
    pool_value: float = POOL_VALUE,
    config: Optional[ControlledConfig] = None,
) -> PoolState:
    state = PoolState(
        base=pool_value / (2.0 * initial_price),
        quote=pool_value / 2.0,
        center=initial_price,
        ema=initial_price,
        last_trade_price=initial_price,
        last_observation_slot=0,
        peak_wealth=pool_value,
    )
    state.current_q = protected_q(model, state)
    state.protected_floor_q = state.current_q
    refresh_retention_target(model, state, config or default_controlled_config())
    return state


def _amount_to_integer(value: float) -> int:
    if value <= 0 or not math.isfinite(value):
        raise ValueError("token amount must be positive and finite")
    integer = int(math.floor(value * TOKEN_SCALE))
    if integer <= 0:
        raise ValueError("token amount is below simulator precision")
    return integer


def _price_to_nad(value: float) -> int:
    if value <= 0 or not math.isfinite(value):
        raise ValueError("price must be positive and finite")
    integer = int(round(value * NAD))
    if not 0 < integer < (1 << 64):
        raise OverflowError("price does not fit Dusk u64 fixed point")
    return integer


def _update_ema_before_trade(
    state: PoolState,
    config: ControlledConfig,
    current_slot: int,
) -> None:
    if current_slot <= state.last_observation_slot:
        return
    elapsed_minutes = (
        current_slot - state.last_observation_slot
    ) / SLOTS_PER_MINUTE
    if config.center_ema_half_life_minutes <= 0:
        state.ema = state.last_trade_price
        return
    alpha = math.exp(
        -math.log(2.0)
        * elapsed_minutes
        / config.center_ema_half_life_minutes
    )
    state.ema = state.ema * alpha + state.last_trade_price * (1.0 - alpha)


def spendable_protected_profit_q(state: PoolState) -> float:
    return max(state.current_q - state.protected_floor_q, 0.0)


def _refresh_retention_gate(state: PoolState) -> None:
    if state.retention_target_stale:
        state.retain_dynamic_surcharge = True
        return
    if state.retention_required_q <= 0.0:
        state.retain_dynamic_surcharge = False
        return
    buffer = spendable_protected_profit_q(state)
    if state.retain_dynamic_surcharge:
        if buffer >= state.retention_stop_q:
            state.retain_dynamic_surcharge = False
    elif buffer < state.retention_required_q:
        state.retain_dynamic_surcharge = True


def _sync_stale_retention_cap(state: PoolState) -> None:
    if not state.retention_target_stale:
        return
    hard_cap = PROTECTED_CAP * state.current_q
    if state.retention_required_q > hard_cap:
        state.retention_required_q = hard_cap
        state.retention_target_saturated = True
    state.retention_stop_q = min(state.retention_stop_q, hard_cap)
    state.retention_hard_cap_q = hard_cap


def mark_retention_target_stale(state: PoolState) -> None:
    state.retention_target_stale = True
    _sync_stale_retention_cap(state)
    _refresh_retention_gate(state)


def checkpoint_neutral_liquidity(state: PoolState, new_q: float) -> None:
    """Mirror AmmState::checkpoint_neutral_liquidity.

    Ordinary reserve/share changes move the floor with Q and preserve the
    already-earned retained-surcharge budget.  They never create new budget.
    """

    prior_buffer = spendable_protected_profit_q(state)
    state.current_q = new_q
    state.protected_floor_q = max(new_q - prior_buffer, 0.0)
    _sync_stale_retention_cap(state)
    _refresh_retention_gate(state)


def checkpoint_retained_surcharge(state: PoolState, new_q: float) -> None:
    """Mirror AmmState::checkpoint_retained_surcharge."""

    tolerance = max(state.current_q * 1e-9, 1e-9)
    if new_q + tolerance < state.current_q:
        raise ArithmeticError("retained surcharge unexpectedly reduced protected Q")
    state.current_q = max(new_q, state.current_q)
    mark_retention_target_stale(state)


def checkpoint_recenter_or_loss(state: PoolState, new_q: float) -> None:
    """Mirror AmmState::checkpoint_recenter_or_loss."""

    if new_q > state.current_q:
        state.protected_floor_q += new_q - state.current_q
    state.current_q = new_q
    _sync_stale_retention_cap(state)
    _refresh_retention_gate(state)


def release_stale_retention_probe(state: PoolState) -> bool:
    """Release surcharge routing for the one quote about to execute."""

    if (
        not state.retention_target_stale
        or state.retention_stop_q <= 0.0
        or spendable_protected_profit_q(state) < state.retention_stop_q
    ):
        return False
    state.retain_dynamic_surcharge = False
    return True


def finish_stale_retention_probe(state: PoolState) -> None:
    """Re-arm retention unless the quote refreshed the target or recentered."""

    if state.retention_target_stale:
        state.retain_dynamic_surcharge = True


def preview_trade(
    model: Amm,
    state: PoolState,
    external_notional: float,
    direction: str,
    external_price: float,
    current_slot: int,
    config: ControlledConfig,
    *,
    fee_mode: str = "dusk",
) -> tuple[PoolState, TradeResult]:
    candidate = copy.deepcopy(state)
    result = _execute_trade_mutating(
        model,
        candidate,
        external_notional,
        direction,
        external_price,
        current_slot,
        config,
        fee_mode=fee_mode,
    )
    return candidate, result


def _execute_trade_mutating(
    model: Amm,
    state: PoolState,
    external_notional: float,
    direction: str,
    external_price: float,
    current_slot: int,
    config: ControlledConfig,
    *,
    fee_mode: str = "dusk",
) -> TradeResult:
    """Execute one externally sized exact-input order.

    The caller supplies quote-value notional.  This creates an identical order
    tape across models even when their internal centers are stale.
    """
    if external_notional <= 0:
        raise ValueError("external_notional must be positive")
    if (
        config.manager_fee_bps < 0
        or config.protocol_fee_bps < 0
        or config.manager_fee_bps + config.protocol_fee_bps > 10_000
    ):
        raise ValueError("manager and protocol fee shares must fit within 10,000 bps")
    if fee_mode == "dusk":
        # Production opens a stale-target release only on the mutable clone or
        # market instance that is about to quote an executable swap.
        release_stale_retention_probe(state)
    _update_ema_before_trade(state, config, current_slot)
    common_x = state.base * state.center
    quote_y = state.quote
    start_invariant: Optional[float] = None
    if isinstance(model, CurrentDh2):
        # Marginal price, balanced reserve, and the exact-input quote all read
        # the same pre-trade reserves.  Solve D once and pass that identical
        # value through each calculation instead of repeating the bisection.
        start_invariant = model.invariant(common_x, quote_y)
        start_raw = model.marginal_price_raw_with_invariant(
            common_x,
            quote_y,
            start_invariant,
        )
        q0_common = start_invariant / 2.0
    else:
        start_raw = model.marginal_price_raw(common_x, quote_y)
        q0_common = common_balanced_reserve(model, common_x, quote_y)
    start_price = state.center * start_raw

    if direction == "x_to_y":
        gross_base = external_notional / external_price
        gross_common = gross_base * state.center
        gross_integer = _amount_to_integer(gross_common)
        input_coordinate = common_x
    elif direction == "y_to_x":
        gross_base = 0.0
        gross_common = external_notional
        gross_integer = _amount_to_integer(gross_common)
        input_coordinate = quote_y
    else:
        raise ValueError(f"unknown direction {direction}")

    assessment: Optional[SwapFeeAssessment]
    if fee_mode == "zero":
        quote_input_common = gross_common
        claimable_common = 0.0
        retained_common = 0.0
        base_fee_common = 0.0
        dynamic_fee_common = 0.0
        assessment = None
    elif fee_mode == "dusk":
        assessment = assess_swap_fee(
            config.fee,
            DynamicFeePreState(
                center_price_nad=_price_to_nad(state.center),
                volatility_accumulator_nad=state.volatility_accumulator_nad,
                volatility_last_update_slot=state.last_observation_slot,
            ),
            reserve_credit=gross_integer,
            current_slot=current_slot,
            center_input_reserve=_amount_to_integer(q0_common),
            start_input_reserve=_amount_to_integer(input_coordinate),
            retain_dynamic_surcharge=state.retain_dynamic_surcharge,
        )
        quote_input_common = assessment.amount_in_for_quote / TOKEN_SCALE
        claimable_common = assessment.claimable_fee_debit / TOKEN_SCALE
        retained_common = assessment.retained_surcharge / TOKEN_SCALE
        base_fee_common = assessment.base_fee_debit / TOKEN_SCALE
        dynamic_fee_common = assessment.dynamic_surcharge_debit / TOKEN_SCALE
    else:
        raise ValueError(f"unknown fee mode {fee_mode}")

    quote = model.quote(
        common_x,
        quote_y,
        quote_input_common,
        direction,
        apply_fee=False,
        include_marginals=True,
        invariant_override=start_invariant,
        marginal_before_override=(
            start_raw if start_invariant is not None else None
        ),
    )
    if not 0.0 <= config.solver_haircut_ppm < 1_000_000.0:
        raise ValueError("solver haircut must be in [0, 1_000_000) ppm")
    raw_output_common = quote.amount_out
    haircut_common = raw_output_common * config.solver_haircut_ppm / 1_000_000.0
    net_output_common = raw_output_common - haircut_common

    if direction == "x_to_y":
        quote_base_credit = quote_input_common / state.center
        output_quote = net_output_common
        state.base += quote_base_credit
        state.quote -= output_quote
        state.claimable_base += claimable_common / state.center
        output_amount = output_quote
        trader_profit = output_quote - gross_base * external_price
        input_token = "base"
        output_token = "quote"
        common_to_reference = external_price / state.center
    else:
        output_base = net_output_common / state.center
        state.quote += quote_input_common
        state.base -= output_base
        state.claimable_quote += claimable_common
        output_amount = output_base
        trader_profit = output_base * external_price - external_notional
        input_token = "quote"
        output_token = "base"
        common_to_reference = 1.0

    total_fee_common = base_fee_common + dynamic_fee_common
    manager_fee_common = base_fee_common * config.manager_fee_bps / 10_000.0
    protocol_fee_common = base_fee_common * config.protocol_fee_bps / 10_000.0
    if direction == "x_to_y":
        state.non_lp_claimable_base += (
            manager_fee_common + protocol_fee_common
        ) / state.center
    else:
        state.non_lp_claimable_quote += manager_fee_common + protocol_fee_common
    total_fee_value = total_fee_common * common_to_reference
    base_fee_value = base_fee_common * common_to_reference
    dynamic_fee_value = dynamic_fee_common * common_to_reference
    retained_value = retained_common * common_to_reference
    haircut_value = haircut_common * common_to_reference
    manager_fee_value = manager_fee_common * common_to_reference
    protocol_fee_value = protocol_fee_common * common_to_reference
    distributed_value = max(dynamic_fee_value - retained_value, 0.0)
    state.gross_fee_value += total_fee_value
    state.base_fee_value += base_fee_value
    state.dynamic_fee_value += dynamic_fee_value
    state.retained_surcharge_value += retained_value
    state.distributed_surcharge_value += distributed_value
    state.solver_haircut_value += haircut_value
    state.manager_fee_value += manager_fee_value
    state.protocol_fee_value += protocol_fee_value
    state.pool_flow_pnl -= trader_profit

    # The frozen trader-visible endpoint excludes every fee, matching
    # AmmSwapQuote::end_price_nad.  Retained surcharge is credited only after
    # this neutral checkpoint and therefore cannot masquerade as flow.
    neutral_q: Optional[float] = None
    if isinstance(model, CurrentDh2):
        end_common_x = state.base * state.center
        end_invariant = model.invariant(end_common_x, state.quote)
        end_price = state.center * model.marginal_price_raw_with_invariant(
            end_common_x,
            state.quote,
            end_invariant,
        )
        neutral_q = end_invariant / (2.0 * math.sqrt(state.center))
    else:
        end_price = spot_price(model, state)

    if fee_mode == "dusk" and assessment is not None:
        checkpoint_neutral_liquidity(
            state,
            neutral_q if neutral_q is not None else protected_q(model, state),
        )
        if retained_common > 0.0:
            if direction == "x_to_y":
                state.base += retained_common / state.center
            else:
                state.quote += retained_common
            checkpoint_retained_surcharge(state, protected_q(model, state))

        # A fresh target is recomputed only when the reserve endpoint did not
        # make the previously certified target stale.  This is the production
        # finalization ordering, not an optimization shortcut.
        if not state.retention_target_stale:
            refresh_retention_target(model, state, config)
        state.volatility_accumulator_nad = volatility_after_assessed_swap(
            config.fee,
            assessment,
            _price_to_nad(start_price),
            _price_to_nad(end_price),
        )
    else:
        checkpoint_neutral_liquidity(
            state,
            neutral_q if neutral_q is not None else protected_q(model, state),
        )

    state.last_trade_price = end_price
    state.last_observation_slot = current_slot
    if fee_mode == "dusk":
        maybe_recenter(model, state, config, current_slot)
        finish_stale_retention_probe(state)

    return TradeResult(
        direction=direction,
        gross_input=gross_base if direction == "x_to_y" else external_notional,
        input_token=input_token,
        output_amount=output_amount,
        output_token=output_token,
        external_notional=external_notional,
        trader_profit_at_reference=trader_profit,
        total_fee_value=total_fee_value,
        base_fee_value=base_fee_value,
        dynamic_fee_value=dynamic_fee_value,
        retained_surcharge_value=retained_value,
        solver_haircut_value=haircut_value,
        manager_fee_value=manager_fee_value,
        protocol_fee_value=protocol_fee_value,
        assessment=assessment,
    )


def execute_trade(
    model: Amm,
    state: PoolState,
    external_notional: float,
    direction: str,
    external_price: float,
    current_slot: int,
    config: ControlledConfig,
    *,
    fee_mode: str = "dusk",
) -> TradeResult:
    """Atomically execute one trade and commit only its successful state.

    Solana rolls the whole instruction back when any quote, fee, checkpoint,
    or recenter solve fails.  Running the fallible simulator path on a clone
    preserves the same guarantee for EMA and stale-probe state as well as for
    reserves.
    """

    candidate = copy.deepcopy(state)
    result = _execute_trade_mutating(
        model,
        candidate,
        external_notional,
        direction,
        external_price,
        current_slot,
        config,
        fee_mode=fee_mode,
    )
    state.__dict__.update(candidate.__dict__)
    return result


def _center_step(center: float, target: float, step: float) -> float:
    if target > center:
        return min(center * (1.0 + step), target)
    if target < center:
        return max(center / (1.0 + step), target)
    return center


def _candidate_recenter(
    model: Amm,
    state: PoolState,
    target: float,
    step: float,
) -> tuple[float, float, float]:
    candidate_center = _center_step(state.center, target, step)
    candidate_q = protected_q(model, state, center=candidate_center)
    impairment = max(state.current_q - candidate_q, 0.0)
    covered = (
        PROTECTED_COVERAGE * impairment
        + (PROTECTED_GUARD * state.current_q if impairment > 0 else 0.0)
    )
    return candidate_center, candidate_q, covered


def refresh_retention_target_from_impairment(
    state: PoolState,
    worst_next_step_impairment_q: float,
) -> None:
    """Install one exact forward target from an already-valued impairment.

    This is the float-coordinate analogue of ``retention_target`` in
    ``amm.rs``.  Curve evaluation is intentionally outside this helper so
    failed and successful recenter paths can use the same state transition as
    production.
    """

    impairment = max(worst_next_step_impairment_q, 0.0)
    hard_cap = PROTECTED_CAP * state.current_q
    if state.current_q <= 0.0 or impairment <= 0.0:
        required = 0.0
        stop = 0.0
        saturated = False
    else:
        guard = PROTECTED_GUARD * state.current_q
        raw_required = PROTECTED_COVERAGE * impairment + guard
        required = min(raw_required, hard_cap)
        hysteresis = max(PROTECTED_HYSTERESIS * required, guard)
        stop = min(required + hysteresis, hard_cap)
        saturated = raw_required > hard_cap
    state.retention_required_q = required
    state.retention_stop_q = stop
    state.retention_hard_cap_q = hard_cap
    state.retention_target_saturated = saturated
    state.retention_target_stale = False
    _refresh_retention_gate(state)


def refresh_retention_target(
    model: Amm,
    state: PoolState,
    config: ControlledConfig,
) -> None:
    if isinstance(model, Cpmm) or config.adjustment_step <= 0:
        refresh_retention_target_from_impairment(state, 0.0)
        return
    worst = 0.0
    for target in (
        state.center * (1.0 + config.adjustment_step),
        state.center / (1.0 + config.adjustment_step),
    ):
        candidate_q = protected_q(model, state, center=target)
        worst = max(worst, state.current_q - candidate_q)
    refresh_retention_target_from_impairment(state, worst)


def maybe_recenter(
    model: Amm,
    state: PoolState,
    config: ControlledConfig,
    current_slot: int,
) -> bool:
    if (
        not config.recenter_enabled
        or config.adjustment_step <= 0
    ):
        return False
    earliest = state.last_adjustment_slot + (
        config.min_adjustment_interval_minutes * SLOTS_PER_MINUTE
    )
    if current_slot < earliest:
        return False
    relative_distance = max(state.center, state.ema) / min(state.center, state.ema) - 1.0
    if relative_distance < config.adjustment_threshold:
        return False

    buffer = spendable_protected_profit_q(state)
    # A retained endpoint keeps the last exact target sticky.  Production does
    # not pay for a new candidate solve until that target's required amount is
    # plausibly funded.  The hysteresis stop controls quote routing; this gate
    # deliberately uses ``required``, not ``stop``.
    if state.retention_target_stale and buffer < state.retention_required_q:
        return False

    state.requested_recenter_count += 1

    full_center, full_q, _ = _candidate_recenter(
        model,
        state,
        state.ema,
        config.adjustment_step,
    )
    full_impairment = max(state.current_q - full_q, 0.0)
    state.requested_recenter_impairment_q += full_impairment
    state.requested_center_move_log += abs(math.log(full_center / state.center))
    admitted: Optional[tuple[float, float, float]] = None
    for halving in range(MAX_STEP_HALVINGS + 1):
        step = config.adjustment_step / (2**halving)
        center, candidate_q, covered = _candidate_recenter(
            model,
            state,
            state.ema,
            step,
        )
        if center == state.center:
            continue
        if covered <= buffer + 1e-12 and covered <= state.retention_hard_cap_q + 1e-12:
            admitted = (center, candidate_q, covered)
            break
    if admitted is None:
        state.blocked_recenter_count += 1
        refresh_retention_target_from_impairment(state, full_impairment)
        return False

    center, candidate_q, _ = admitted
    old_q = state.current_q
    actual_impairment = max(old_q - candidate_q, 0.0)
    state.funded_center_move_log += abs(math.log(center / state.center))
    state.center = center
    checkpoint_recenter_or_loss(state, candidate_q)
    state.recenter_impairment_q += actual_impairment
    state.recenter_count += 1
    state.last_adjustment_slot = current_slot
    # Production rebuilds the target from the exact impairment just realized
    # by the admitted step.  It does not immediately pay for two more forward
    # candidate solves here.
    refresh_retention_target_from_impairment(state, actual_impairment)
    return True


def _best_arbitrage(
    model: Amm,
    state: PoolState,
    external_price: float,
    current_slot: int,
    config: ControlledConfig,
    numerical_failures: Optional[list[int]] = None,
) -> Optional[tuple[PoolState, TradeResult]]:
    pool_spot = spot_price(model, state)
    relative_error = pool_spot / external_price - 1.0
    if abs(relative_error) < 1e-8:
        return None
    direction = "x_to_y" if relative_error > 0 else "y_to_x"
    maximum = min(
        total_wealth(state, external_price) * 0.60,
        (state.base * external_price if direction == "x_to_y" else state.quote)
        * 0.90,
    )
    minimum = max(maximum * 1e-8, 1e-6)
    if maximum <= minimum:
        return None

    candidates: list[tuple[float, Optional[PoolState], Optional[TradeResult]]] = []
    # Match the native Curve replay's search resolution so neither system
    # receives a mechanically weaker simulated arbitrageur.
    grid_count = ARBITRAGE_SEARCH_GRID_COUNT
    for index in range(grid_count):
        amount = minimum * math.exp(
            math.log(maximum / minimum) * index / (grid_count - 1)
        )
        try:
            candidate_state, result = preview_trade(
                model,
                state,
                amount,
                direction,
                external_price,
                current_slot,
                config,
            )
            candidates.append(
                (result.trader_profit_at_reference, candidate_state, result)
            )
        except ValueError:
            candidates.append((-math.inf, None, None))
        except ArithmeticError:
            if numerical_failures is not None:
                numerical_failures[0] += 1
            candidates.append((-math.inf, None, None))
    best_index = max(range(len(candidates)), key=lambda index: candidates[index][0])
    if candidates[best_index][0] <= config.arb_min_profit_quote:
        return None

    grid_amounts = [
        minimum
        * math.exp(
            math.log(maximum / minimum) * index / (grid_count - 1)
        )
        for index in range(grid_count)
    ]
    left = grid_amounts[max(best_index - 1, 0)]
    right = grid_amounts[min(best_index + 1, len(grid_amounts) - 1)]
    phi = (math.sqrt(5.0) - 1.0) / 2.0

    def evaluate(amount: float) -> tuple[float, Optional[PoolState], Optional[TradeResult]]:
        try:
            candidate_state, result = preview_trade(
                model,
                state,
                amount,
                direction,
                external_price,
                current_slot,
                config,
            )
            return result.trader_profit_at_reference, candidate_state, result
        except ValueError:
            return -math.inf, None, None
        except ArithmeticError:
            if numerical_failures is not None:
                numerical_failures[0] += 1
            return -math.inf, None, None

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
    best = max(
        [candidates[best_index], fc, fd],
        key=lambda item: item[0],
    )
    if (
        best[0] <= config.arb_min_profit_quote
        or best[1] is None
        or best[2] is None
    ):
        return None
    return best[1], best[2]


def execute_profitable_arbitrage(
    model: Amm,
    state: PoolState,
    external_price: float,
    current_slot: int,
    config: ControlledConfig,
) -> float:
    """Execute a bounded same-event sequence of profitable arb fills.

    A fill can move the concentration center after its quote has executed.
    That new center defines a new curve, so the arbitrageur re-optimizes from
    the committed endpoint in the same slot.  The hard cap bounds simulation
    work even under an unexpectedly self-renewing controller interaction.
    """

    total_profit = 0.0
    fills = 0
    while fills < MAX_ARBITRAGE_FILLS_PER_EVENT:
        numerical_failures = [0]
        best = _best_arbitrage(
            model,
            state,
            external_price,
            current_slot,
            config,
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

    if fills == MAX_ARBITRAGE_FILLS_PER_EVENT:
        # Count only cap hits that leave another executable profitable fill,
        # rather than an event whose final admissible fill happened to be the
        # last one allowed by the hard cap.
        numerical_failures = [0]
        profitable_follow_up = _best_arbitrage(
            model,
            state,
            external_price,
            current_slot,
            config,
            numerical_failures,
        )
        state.arbitrage_numerical_failure_count += numerical_failures[0]
        if profitable_follow_up is not None:
            state.arbitrage_capped_event_count += 1
    return total_profit


@dataclass(frozen=True)
class UserOrder:
    external_notional: float
    direction: str
    informed: bool


def order_tape_for_step(
    pool_value: float,
    daily_volume_pool_ratio: float,
    step_minutes: int,
    informed_share: float,
    current_price: float,
    next_price: float,
    randomizer: random.Random,
) -> list[UserOrder]:
    step_volume = (
        pool_value
        * daily_volume_pool_ratio
        * step_minutes
        / (24.0 * 60.0)
    )
    weights = [randomizer.uniform(0.15, 0.85) for _ in range(2)]
    scale = step_volume / sum(weights)
    orders = []
    for weight in weights:
        informed = randomizer.random() < informed_share and next_price != current_price
        if informed:
            direction = "y_to_x" if next_price > current_price else "x_to_y"
        else:
            direction = "x_to_y" if randomizer.random() < 0.5 else "y_to_x"
        orders.append(
            UserOrder(
                external_notional=weight * scale,
                direction=direction,
                informed=informed,
            )
        )
    return orders


def current_execution_impact_bps(
    model: Amm,
    state: PoolState,
    external_price: float,
    direction: str,
    *,
    trade_bps: int = 100,
) -> float:
    """Fee-free impact of one marked-TVL order from the actual pool state."""
    wealth = total_wealth(state, external_price)
    notional = wealth * trade_bps / 10_000.0
    common_x = state.base * state.center
    if direction == "x_to_y":
        common_input = notional / external_price * state.center
        quote = with_zero_fees(model).quote(
            common_x,
            state.quote,
            common_input,
            direction,
            apply_fee=False,
            include_marginals=False,
        )
        output_value = quote.amount_out
    else:
        quote = with_zero_fees(model).quote(
            common_x,
            state.quote,
            notional,
            direction,
            apply_fee=False,
            include_marginals=False,
        )
        output_value = quote.amount_out / state.center * external_price
    return max((1.0 - output_value / notional) * 10_000.0, 0.0)


def simulate_path(
    model: Amm,
    path_name: str,
    prices: Sequence[float],
    *,
    step_minutes: int,
    arb_latency_steps: int,
    informed_share: float,
    daily_volume_pool_ratio: float,
    seed: int,
    config: Optional[ControlledConfig] = None,
    pool_value: float = POOL_VALUE,
) -> dict[str, object]:
    if not prices:
        raise ValueError("price path cannot be empty")
    if arb_latency_steps <= 0:
        raise ValueError("arb_latency_steps must be positive")
    controlled = config or default_controlled_config()
    randomizer = random.Random(seed)
    if pool_value <= 0:
        raise ValueError("pool_value must be positive")
    state = initialize_pool(
        model,
        prices[0],
        pool_value=pool_value,
        config=controlled,
    )
    initial_base = state.base
    initial_quote = state.quote

    previous_external = prices[0]
    attempted_user_orders = 0
    executed_user_orders = 0
    rejected_user_orders = 0
    attempted_user_volume = 0.0
    executed_user_volume = 0.0
    execution_cost_value = 0.0
    center_tracking_errors_bps: list[float] = []
    ema_tracking_errors_bps: list[float] = []
    for index, external in enumerate(prices):
        current_slot = index * step_minutes * SLOTS_PER_MINUTE
        if index:
            state.inventory_carry += (
                state.base + state.claimable_base - initial_base
            ) * (external - previous_external)
        if index % arb_latency_steps == 0:
            execute_profitable_arbitrage(
                model,
                state,
                external,
                current_slot,
                controlled,
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
            attempted_user_orders += 1
            attempted_user_volume += order.external_notional
            try:
                result = execute_trade(
                    model,
                    state,
                    order.external_notional,
                    order.direction,
                    external,
                    current_slot,
                    controlled,
                )
            except (ArithmeticError, OverflowError, ValueError):
                rejected_user_orders += 1
                continue
            executed_user_orders += 1
            executed_user_volume += order.external_notional
            execution_cost_value -= result.trader_profit_at_reference
            if order.direction == "x_to_y":
                markout_profit = result.output_amount - (
                    result.gross_input * next_price
                )
            else:
                markout_profit = (
                    result.output_amount * next_price - result.gross_input
                )
            if order.informed:
                state.informed_flow_profit += markout_profit
            else:
                state.uninformed_flow_profit += markout_profit

        # A single common executable-service definition: a 100-bps order must
        # clear at <=50 bps impact at the pool's current market displacement.
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
        center_tracking_errors_bps.append(
            abs(math.log(external / state.center)) * 10_000.0
        )
        ema_tracking_errors_bps.append(
            abs(math.log(external / state.ema)) * 10_000.0
        )
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
    recenter_impairment_value = (
        2.0 * state.recenter_impairment_q * math.sqrt(final_price)
    )
    requested_impairment_value = (
        2.0
        * state.requested_recenter_impairment_q
        * math.sqrt(final_price)
    )
    return {
        "model": model_family(model),
        "path": path_name,
        "path_class": "historical" if path_name.startswith("historical_") else "synthetic",
        "arb_latency_minutes": arb_latency_steps * step_minutes,
        "informed_flow_share": informed_share,
        "daily_volume_pool_ratio": daily_volume_pool_ratio,
        "initial_pool_value": pool_value,
        "seed": seed,
        "final_lp_wealth": final_wealth,
        "hold_value": hold_value,
        "net_lp_pnl_vs_hold": ledger_gap,
        "loss_vs_hold": -ledger_gap,
        "gross_fee_value": state.gross_fee_value,
        "base_fee_value": state.base_fee_value,
        "dynamic_fee_value": state.dynamic_fee_value,
        "retained_surcharge_value": state.retained_surcharge_value,
        "distributed_surcharge_value": state.distributed_surcharge_value,
        "claimable_base": state.claimable_base,
        "claimable_quote": state.claimable_quote,
        "realized_arb_profit": state.realized_arb_profit,
        "informed_flow_profit": state.informed_flow_profit,
        "uninformed_flow_profit": state.uninformed_flow_profit,
        "recenter_impairment_value": recenter_impairment_value,
        "requested_recenter_impairment_value": requested_impairment_value,
        "recenter_count": state.recenter_count,
        "requested_recenter_count": state.requested_recenter_count,
        "blocked_recenter_count": state.blocked_recenter_count,
        "requested_center_move_log": state.requested_center_move_log,
        "funded_center_move_log": state.funded_center_move_log,
        "center_move_funding_fraction": (
            state.funded_center_move_log / state.requested_center_move_log
            if state.requested_center_move_log > 0
            else 1.0
        ),
        "protected_budget_value": 2.0
        * max(state.current_q - state.protected_floor_q, 0.0)
        * math.sqrt(final_price),
        "stale_fraction": state.stale_steps / max(state.total_steps, 1),
        "center_tracking_error_bps": abs(math.log(final_price / state.center))
        * 10_000.0,
        "center_tracking_rmse_bps": math.sqrt(
            sum(value * value for value in center_tracking_errors_bps)
            / max(len(center_tracking_errors_bps), 1)
        ),
        "center_tracking_p95_bps": sorted(center_tracking_errors_bps)[
            min(
                int(0.95 * max(len(center_tracking_errors_bps) - 1, 0)),
                max(len(center_tracking_errors_bps) - 1, 0),
            )
        ],
        "center_tracking_max_bps": max(center_tracking_errors_bps, default=0.0),
        "ema_tracking_rmse_bps": math.sqrt(
            sum(value * value for value in ema_tracking_errors_bps)
            / max(len(ema_tracking_errors_bps), 1)
        ),
        "attempted_user_orders": attempted_user_orders,
        "executed_user_orders": executed_user_orders,
        "rejected_user_orders": rejected_user_orders,
        "attempted_user_volume": attempted_user_volume,
        "executed_user_volume": executed_user_volume,
        "captured_user_volume_fraction": (
            executed_user_volume / attempted_user_volume
            if attempted_user_volume > 0
            else 1.0
        ),
        "volume_weighted_user_execution_cost_bps": (
            execution_cost_value / executed_user_volume * 10_000.0
            if executed_user_volume > 0
            else 0.0
        ),
        "max_drawdown": state.max_drawdown,
        "ending_base": state.base,
        "ending_quote": state.quote,
        "pool_flow_pnl": state.pool_flow_pnl,
        "inventory_carry": state.inventory_carry,
        "accounting_identity_error": ledger_gap
        - (state.pool_flow_pnl + state.inventory_carry),
    }

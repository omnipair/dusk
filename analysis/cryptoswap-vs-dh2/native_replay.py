"""Common-intent native Dusk replay and routing utilities."""

from __future__ import annotations

from dataclasses import dataclass
import math
from typing import Optional, Sequence

from economics import (
    POOL_VALUE,
    SLOTS_PER_MINUTE,
    ControlledConfig,
    PoolState,
    TradeResult,
    current_execution_impact_bps,
    default_controlled_config,
    execute_profitable_arbitrage,
    initialize_pool,
    preview_trade,
    total_wealth,
)
from models import Amm
from models import CryptoSwap
from native_flow import NativeOrder, orders_by_minute
from curve_native import (
    ADMIN_PROFIT_LIABILITY_FRACTION,
    CurveNativeConfig,
    CurveNativeOrderResult,
    CurveNativeState,
    current_execution_impact_bps as curve_execution_impact_bps,
    execute_profitable_arbitrage as execute_curve_profitable_arbitrage,
    initialize_curve_native_pool,
    normalized_reserves,
    preview_curve_native_order,
    total_wealth as curve_total_wealth,
)


ARBITRAGE_CAP_COUNTER_SCOPE = "all_periods_including_burn_in_and_score"
SERVICE_PROBE_BPS = 100
STATE_DEVIATION_THRESHOLD_BPS = 50.0


@dataclass(frozen=True)
class RoutingConfig:
    mode: str = "fixed"
    outside_depth_multiple: float = 1.0
    outside_fee_bps: float = 5.0

    def validate(self) -> None:
        if self.mode not in {"fixed", "elastic"}:
            raise ValueError("routing mode must be fixed or elastic")
        if self.outside_depth_multiple <= 0 or self.outside_fee_bps < 0:
            raise ValueError("outside depth and fee must be non-negative")


def outside_output_value(
    external_notional: float,
    external_price: float,
    direction: str,
    *,
    pool_value: float,
    config: RoutingConfig,
) -> float:
    """Quote a stateless balanced CPMM reference venue at the external mid."""

    config.validate()
    depth_value = pool_value * config.outside_depth_multiple
    fee = config.outside_fee_bps / 10_000.0
    quote_reserve = depth_value / 2.0
    base_reserve = depth_value / (2.0 * external_price)
    if direction == "x_to_y":
        amount_in = external_notional / external_price
        zero_fee_output = quote_reserve * amount_in / (base_reserve + amount_in)
        return zero_fee_output * (1.0 - fee)
    if direction == "y_to_x":
        zero_fee_output = base_reserve * external_notional / (
            quote_reserve + external_notional
        )
        return zero_fee_output * (1.0 - fee) * external_price
    raise ValueError(f"unknown direction {direction}")


def _preview_dusk_order(
    model: Amm,
    state: PoolState,
    order: NativeOrder,
    external_price: float,
    current_slot: int,
    controlled: ControlledConfig,
) -> tuple[PoolState, TradeResult, float]:
    candidate, result = preview_trade(
        model,
        state,
        order.external_notional,
        order.direction,
        external_price,
        current_slot,
        controlled,
    )
    if order.direction == "x_to_y":
        output_value = result.output_amount
    else:
        output_value = result.output_amount * external_price
    return candidate, result, output_value


def _all_in_dusk_cost_bps(
    model: Amm,
    state: PoolState,
    external_price: float,
    direction: str,
    current_slot: int,
    controlled: ControlledConfig,
    trade_bps: int = 100,
) -> float:
    notional = total_wealth(state, external_price) * trade_bps / 10_000.0
    _, result = preview_trade(
        model,
        state,
        notional,
        direction,
        external_price,
        current_slot,
        controlled,
    )
    return -result.trader_profit_at_reference / notional * 10_000.0


def _quantile(values: Sequence[float], probability: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    position = (len(ordered) - 1) * probability
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    weight = position - lower
    return ordered[lower] * (1.0 - weight) + ordered[upper] * weight


def _marginal_tracking_error_bps(
    normalized_marginal_price: float,
    normalization_price: float,
    external_price: float,
) -> float:
    """Return fee-free pool-mid tracking error without trade-size impact.

    The analytical invariants operate on common-value reserves.  Their
    normalized marginal price must therefore be multiplied by the pool's
    internal normalization price before it is compared with the external mark.
    """

    pool_price = normalized_marginal_price * normalization_price
    if (
        normalized_marginal_price <= 0.0
        or normalization_price <= 0.0
        or external_price <= 0.0
        or not math.isfinite(pool_price)
    ):
        return math.inf
    return abs(math.log(pool_price / external_price)) * 10_000.0


def _counter_snapshot(state: PoolState) -> dict[str, float | int]:
    fields = (
        "gross_fee_value",
        "base_fee_value",
        "dynamic_fee_value",
        "retained_surcharge_value",
        "distributed_surcharge_value",
        "solver_haircut_value",
        "manager_fee_value",
        "protocol_fee_value",
        "realized_arb_profit",
        "arbitrage_fill_count",
        "arbitrage_capped_event_count",
        "arbitrage_numerical_failure_count",
        "informed_flow_profit",
        "uninformed_flow_profit",
        "pool_flow_pnl",
        "recenter_impairment_q",
        "requested_recenter_impairment_q",
        "recenter_count",
        "requested_recenter_count",
        "blocked_recenter_count",
        "requested_center_move_log",
        "funded_center_move_log",
    )
    return {field: getattr(state, field) for field in fields}


def _curve_counter_snapshot(
    state: CurveNativeState,
) -> dict[str, float | int]:
    fields = (
        "gross_fee_value",
        "baseline_fee_value",
        "excess_fee_value",
        "realized_arb_profit",
        "arbitrage_fill_count",
        "arbitrage_capped_event_count",
        "arbitrage_numerical_failure_count",
        "informed_flow_profit",
        "uninformed_flow_profit",
        "pool_flow_pnl",
        "recenter_impairment_xcp",
        "requested_recenter_impairment_xcp",
        "recenter_count",
        "recenter_attempt_count",
        "blocked_recenter_count",
        "requested_center_move_log",
        "funded_center_move_log",
        "xcp_profit",
        "xcp_profit_a",
        "virtual_price",
    )
    return {field: getattr(state, field) for field in fields}


def simulate_dusk_native_tape(
    model: Amm,
    path_name: str,
    prices: Sequence[float],
    tape: Sequence[NativeOrder],
    *,
    score_start_index: int = 0,
    arb_latency_minutes: int,
    pool_value: float = POOL_VALUE,
    config: Optional[ControlledConfig] = None,
    routing: Optional[RoutingConfig] = None,
    step_minutes: int = 1,
) -> dict[str, object]:
    """Replay Dusk's native fee/funded-center policy on a fixed intent tape."""

    if not prices or min(prices) <= 0:
        raise ValueError("prices must be non-empty and positive")
    if not 0 <= score_start_index < len(prices):
        raise ValueError("score boundary lies outside the path")
    if arb_latency_minutes <= 0 or pool_value <= 0 or step_minutes <= 0:
        raise ValueError("latency, pool value, and step must be positive")
    if arb_latency_minutes % step_minutes != 0:
        raise ValueError("arb_latency_minutes must be a multiple of step_minutes")
    controlled = config or default_controlled_config()
    route = routing or RoutingConfig()
    route.validate()
    grouped_orders = orders_by_minute(tape, len(prices))
    state = initialize_pool(
        model,
        prices[0],
        pool_value=pool_value,
        config=controlled,
    )
    centered_state = initialize_pool(
        model,
        prices[0],
        pool_value=pool_value,
        config=controlled,
    )
    centered_fee_free_probe_cost_bps = {
        direction: current_execution_impact_bps(
            model,
            centered_state,
            prices[0],
            direction,
            trade_bps=SERVICE_PROBE_BPS,
        )
        for direction in ("x_to_y", "y_to_x")
    }

    score_start_base = 0.0
    score_start_quote = 0.0
    score_start_non_lp_base = 0.0
    score_start_non_lp_quote = 0.0
    score_counters: dict[str, float | int] = {}
    previous_scored_price = prices[score_start_index]
    inventory_carry = 0.0
    attempted_orders = executed_orders = rejected_orders = routed_out_orders = 0
    attempted_volume = executed_volume = routed_out_volume = 0.0
    native_execution_cost_value = 0.0
    selected_execution_cost_value = 0.0
    center_errors: list[float] = []
    ema_errors: list[float] = []
    raw_all_in_probe_costs: list[float] = []
    raw_fee_free_probe_costs: list[float] = []
    depth_degradation_probe_costs: list[float] = []
    depth_degradation_flags: list[bool] = []
    marginal_tracking_errors: list[float] = []
    marginal_tracking_stale_flags: list[bool] = []
    marginal_tracking_stale_episode_lengths: list[int] = []
    active_marginal_tracking_stale_episode = 0
    marginal_tracking_stuck_steps = 0
    peak_wealth = 0.0
    max_drawdown = 0.0

    for minute, external_price in enumerate(prices):
        elapsed_minutes = minute * step_minutes
        current_slot = elapsed_minutes * SLOTS_PER_MINUTE
        scoring = minute >= score_start_index
        if minute == score_start_index:
            score_start_base = state.base + state.claimable_base
            score_start_quote = state.quote + state.claimable_quote
            score_start_non_lp_base = state.non_lp_claimable_base
            score_start_non_lp_quote = state.non_lp_claimable_quote
            score_counters = _counter_snapshot(state)
            previous_scored_price = external_price
            peak_wealth = total_wealth(state, external_price)
        elif scoring:
            inventory_carry += (
                state.base + state.claimable_base - score_start_base
            ) * (external_price - previous_scored_price)

        if elapsed_minutes % arb_latency_minutes == 0:
            execute_profitable_arbitrage(
                model,
                state,
                external_price,
                current_slot,
                controlled,
            )

        blocked_before = state.blocked_recenter_count
        for order in grouped_orders[minute]:
            if scoring:
                attempted_orders += 1
                attempted_volume += order.external_notional
            try:
                candidate, result, native_output_value = _preview_dusk_order(
                    model,
                    state,
                    order,
                    external_price,
                    current_slot,
                    controlled,
                )
            except (ArithmeticError, OverflowError, ValueError):
                if scoring and route.mode == "elastic":
                    outside_value = outside_output_value(
                        order.external_notional,
                        external_price,
                        order.direction,
                        pool_value=pool_value,
                        config=route,
                    )
                    routed_out_orders += 1
                    routed_out_volume += order.external_notional
                    selected_execution_cost_value += (
                        order.external_notional - outside_value
                    )
                elif scoring:
                    rejected_orders += 1
                continue
            outside_value = outside_output_value(
                order.external_notional,
                external_price,
                order.direction,
                pool_value=pool_value,
                config=route,
            )
            if route.mode == "elastic" and native_output_value < outside_value:
                if scoring:
                    routed_out_orders += 1
                    routed_out_volume += order.external_notional
                    selected_execution_cost_value += (
                        order.external_notional - outside_value
                    )
                continue
            # The preview is an atomic execution on a deep clone.  No state
            # changes between routing and this commit, so replaying the same
            # deterministic trade would only duplicate every invariant solve.
            state.__dict__.update(candidate.__dict__)
            if scoring:
                executed_orders += 1
                executed_volume += order.external_notional
                native_execution_cost_value -= result.trader_profit_at_reference
                selected_execution_cost_value -= result.trader_profit_at_reference
            markout_steps = max(1, math.ceil(5 / step_minutes))
            next_price = prices[min(minute + markout_steps, len(prices) - 1)]
            if order.direction == "x_to_y":
                markout_profit = result.output_amount - result.gross_input * next_price
            else:
                markout_profit = result.output_amount * next_price - result.gross_input
            if order.informed:
                state.informed_flow_profit += markout_profit
            else:
                state.uninformed_flow_profit += markout_profit

        if scoring:
            try:
                worst_all_in_cost = max(
                    _all_in_dusk_cost_bps(
                        model,
                        state,
                        external_price,
                        direction,
                        current_slot,
                        controlled,
                    )
                    for direction in ("x_to_y", "y_to_x")
                )
            except (ArithmeticError, OverflowError, ValueError):
                worst_all_in_cost = math.inf
            raw_all_in_probe_costs.append(worst_all_in_cost)
            try:
                fee_free_costs = {
                    direction: current_execution_impact_bps(
                        model,
                        state,
                        external_price,
                        direction,
                        trade_bps=SERVICE_PROBE_BPS,
                    )
                    for direction in ("x_to_y", "y_to_x")
                }
                worst_fee_free_impact = max(fee_free_costs.values())
                depth_degradation_cost = max(
                    max(
                        fee_free_costs[direction]
                        - centered_fee_free_probe_cost_bps[direction],
                        0.0,
                    )
                    for direction in ("x_to_y", "y_to_x")
                )
            except (ArithmeticError, OverflowError, ValueError):
                worst_fee_free_impact = math.inf
                depth_degradation_cost = math.inf
            raw_fee_free_probe_costs.append(worst_fee_free_impact)
            depth_degradation_probe_costs.append(depth_degradation_cost)
            depth_degraded = (
                depth_degradation_cost > STATE_DEVIATION_THRESHOLD_BPS
            )
            depth_degradation_flags.append(depth_degraded)
            try:
                normalized_marginal = model.marginal_price_raw(
                    state.base * state.center,
                    state.quote,
                )
                marginal_tracking_error = _marginal_tracking_error_bps(
                    normalized_marginal,
                    state.center,
                    external_price,
                )
            except (ArithmeticError, OverflowError, ValueError):
                marginal_tracking_error = math.inf
            marginal_tracking_errors.append(marginal_tracking_error)
            marginal_tracking_stale = (
                marginal_tracking_error > STATE_DEVIATION_THRESHOLD_BPS
            )
            marginal_tracking_stale_flags.append(marginal_tracking_stale)
            if marginal_tracking_stale:
                active_marginal_tracking_stale_episode += 1
                if (
                    state.blocked_recenter_count > blocked_before
                    and state.retain_dynamic_surcharge
                ):
                    marginal_tracking_stuck_steps += 1
            elif active_marginal_tracking_stale_episode:
                marginal_tracking_stale_episode_lengths.append(
                    active_marginal_tracking_stale_episode * step_minutes
                )
                active_marginal_tracking_stale_episode = 0
            center_errors.append(
                abs(math.log(external_price / state.center)) * 10_000.0
            )
            ema_errors.append(
                abs(math.log(external_price / state.ema)) * 10_000.0
            )
            wealth = total_wealth(state, external_price)
            peak_wealth = max(peak_wealth, wealth)
            max_drawdown = max(max_drawdown, 1.0 - wealth / peak_wealth)
            previous_scored_price = external_price

    if active_marginal_tracking_stale_episode:
        marginal_tracking_stale_episode_lengths.append(
            active_marginal_tracking_stale_episode * step_minutes
        )

    final_price = prices[-1]
    final_wealth = total_wealth(state, final_price)
    hold_value = score_start_base * final_price + score_start_quote
    net_lp_pnl = final_wealth - hold_value
    starting_non_lp_fee_liability_value = (
        score_start_non_lp_base * final_price + score_start_non_lp_quote
    )
    final_non_lp_fee_liability_value = (
        state.non_lp_claimable_base * final_price
        + state.non_lp_claimable_quote
    )
    scoring_period_non_lp_fee_liability_value = (
        final_non_lp_fee_liability_value
        - starting_non_lp_fee_liability_value
    )
    lp_net_final_wealth = final_wealth - final_non_lp_fee_liability_value
    lp_net_hold_value = (
        (score_start_base - score_start_non_lp_base) * final_price
        + score_start_quote
        - score_start_non_lp_quote
    )
    net_lp_pnl_after_revenue_split = lp_net_final_wealth - lp_net_hold_value
    counters = _counter_snapshot(state)

    def delta(field: str) -> float:
        return float(counters[field]) - float(score_counters[field])

    pool_flow_pnl = delta("pool_flow_pnl")
    accounting_error = net_lp_pnl - (pool_flow_pnl + inventory_carry)
    requested_move = delta("requested_center_move_log")
    funded_move = delta("funded_center_move_log")
    recenter_impairment_value = (
        2.0 * delta("recenter_impairment_q") * math.sqrt(final_price)
    )
    requested_impairment_value = (
        2.0
        * delta("requested_recenter_impairment_q")
        * math.sqrt(final_price)
    )
    return {
        "system": "dusk_native",
        "model": "current_dh2",
        "path": path_name,
        "routing_mode": route.mode,
        "score_start_minute": score_start_index * step_minutes,
        "score_minutes": (len(prices) - score_start_index) * step_minutes,
        "step_minutes": step_minutes,
        "arb_latency_minutes": arb_latency_minutes,
        "initial_pool_value": pool_value,
        "final_lp_wealth": final_wealth,
        "hold_value": hold_value,
        "net_lp_pnl_vs_hold": net_lp_pnl,
        "net_lp_return_vs_hold_bps": net_lp_pnl / hold_value * 10_000.0,
        "non_lp_fee_liability_value": scoring_period_non_lp_fee_liability_value,
        "starting_non_lp_fee_liability_value": (
            starting_non_lp_fee_liability_value
        ),
        "final_non_lp_fee_liability_value": final_non_lp_fee_liability_value,
        "manager_fee_liability_value_at_trade_marks": delta("manager_fee_value"),
        "protocol_fee_liability_value_at_trade_marks": delta("protocol_fee_value"),
        "net_lp_pnl_after_protocol_fee": net_lp_pnl_after_revenue_split,
        "net_lp_pnl_after_revenue_split": net_lp_pnl_after_revenue_split,
        "net_lp_return_after_protocol_fee_bps": (
            net_lp_pnl_after_revenue_split / lp_net_hold_value * 10_000.0
        ),
        "net_lp_return_after_revenue_split_bps": (
            net_lp_pnl_after_revenue_split / lp_net_hold_value * 10_000.0
        ),
        "lp_net_final_wealth": lp_net_final_wealth,
        "lp_net_hold_value": lp_net_hold_value,
        "manager_fee_bps": controlled.manager_fee_bps,
        "protocol_fee_bps": controlled.protocol_fee_bps,
        "loss_vs_hold": -net_lp_pnl,
        "gross_fee_value": delta("gross_fee_value"),
        "baseline_fee_value": delta("base_fee_value"),
        "excess_fee_value": delta("dynamic_fee_value"),
        "total_retained_fee_value": delta("retained_surcharge_value"),
        "recenter_reserved_fee_value": delta("retained_surcharge_value"),
        "base_fee_value": delta("base_fee_value"),
        "dynamic_fee_value": delta("dynamic_fee_value"),
        "retained_surcharge_value": delta("retained_surcharge_value"),
        "distributed_surcharge_value": delta("distributed_surcharge_value"),
        "solver_haircut_value": delta("solver_haircut_value"),
        "manager_fee_value": delta("manager_fee_value"),
        "protocol_fee_value": delta("protocol_fee_value"),
        "realized_arb_profit": delta("realized_arb_profit"),
        "realized_arbitrage_extraction_proxy_value": delta(
            "realized_arb_profit"
        ),
        "arbitrage_fill_count": int(delta("arbitrage_fill_count")),
        # Cap failures are validity diagnostics, not scored economics.  Keep
        # the canonical field all-period so burn-in cannot hide a replay that
        # advanced time while a profitable same-event fill remained.
        "arbitrage_capped_event_count": int(
            state.arbitrage_capped_event_count
        ),
        "scored_arbitrage_capped_event_count": int(
            delta("arbitrage_capped_event_count")
        ),
        "arbitrage_numerical_failure_count": int(
            state.arbitrage_numerical_failure_count
        ),
        "scored_arbitrage_numerical_failure_count": int(
            delta("arbitrage_numerical_failure_count")
        ),
        "informed_flow_markout": delta("informed_flow_profit"),
        "uninformed_flow_markout": delta("uninformed_flow_profit"),
        "pool_flow_pnl": pool_flow_pnl,
        "inventory_carry": inventory_carry,
        "accounting_identity_error": accounting_error,
        "recenter_impairment_value": recenter_impairment_value,
        "requested_recenter_impairment_value": requested_impairment_value,
        "recenter_count": int(delta("recenter_count")),
        "requested_recenter_count": int(delta("requested_recenter_count")),
        "blocked_recenter_count": int(delta("blocked_recenter_count")),
        "requested_center_move_log": requested_move,
        "funded_center_move_log": funded_move,
        "center_move_funding_fraction": (
            funded_move / requested_move if requested_move > 0 else 1.0
        ),
        "available_recenter_budget_value": (
            2.0
            * max(state.current_q - state.protected_floor_q, 0.0)
            * math.sqrt(final_price)
        ),
        "attempted_user_orders": attempted_orders,
        "executed_user_orders": executed_orders,
        "rejected_user_orders": rejected_orders,
        "routed_out_orders": routed_out_orders,
        "attempted_user_volume": attempted_volume,
        "executed_user_volume": executed_volume,
        "routed_out_volume": routed_out_volume,
        "captured_user_volume_fraction": (
            executed_volume / attempted_volume if attempted_volume > 0 else 1.0
        ),
        "volume_weighted_user_execution_cost_bps": (
            native_execution_cost_value / executed_volume * 10_000.0
            if executed_volume > 0
            else 0.0
        ),
        "native_only_execution_cost_bps": (
            native_execution_cost_value / executed_volume * 10_000.0
            if executed_volume > 0
            else 0.0
        ),
        "selected_execution_cost_bps": (
            selected_execution_cost_value / attempted_volume * 10_000.0
            if attempted_volume > 0 and rejected_orders == 0
            else math.inf
        ),
        "raw_all_in_service_median_cost_bps": _quantile(
            raw_all_in_probe_costs, 0.50
        ),
        "raw_all_in_service_p90_cost_bps": _quantile(
            raw_all_in_probe_costs, 0.90
        ),
        "raw_fee_free_service_median_cost_bps": _quantile(
            raw_fee_free_probe_costs, 0.50
        ),
        "raw_fee_free_service_p90_cost_bps": _quantile(
            raw_fee_free_probe_costs, 0.90
        ),
        "centered_fee_free_service_cost_bps": max(
            centered_fee_free_probe_cost_bps.values()
        ),
        "depth_degradation_median_cost_bps": _quantile(
            depth_degradation_probe_costs, 0.50
        ),
        "depth_degradation_p90_cost_bps": _quantile(
            depth_degradation_probe_costs, 0.90
        ),
        "depth_degradation_fraction": (
            sum(depth_degradation_flags)
            / max(len(depth_degradation_flags), 1)
        ),
        "marginal_tracking_stale_fraction": (
            sum(marginal_tracking_stale_flags)
            / max(len(marginal_tracking_stale_flags), 1)
        ),
        "marginal_tracking_rmse_bps": math.sqrt(
            sum(value * value for value in marginal_tracking_errors)
            / max(len(marginal_tracking_errors), 1)
        ),
        "marginal_tracking_p95_bps": _quantile(
            marginal_tracking_errors, 0.95
        ),
        "marginal_tracking_max_bps": max(
            marginal_tracking_errors, default=0.0
        ),
        "marginal_tracking_stale_episode_count": len(
            marginal_tracking_stale_episode_lengths
        ),
        "marginal_tracking_stale_episode_p95_minutes": _quantile(
            marginal_tracking_stale_episode_lengths, 0.95
        ),
        "marginal_tracking_stale_episode_max_minutes": max(
            marginal_tracking_stale_episode_lengths, default=0
        ),
        "marginal_tracking_stuck_fraction": (
            marginal_tracking_stuck_steps
            / max(len(marginal_tracking_stale_flags), 1)
        ),
        "center_tracking_rmse_bps": math.sqrt(
            sum(value * value for value in center_errors)
            / max(len(center_errors), 1)
        ),
        "center_tracking_p95_bps": _quantile(center_errors, 0.95),
        "center_tracking_max_bps": max(center_errors, default=0.0),
        "ema_tracking_rmse_bps": math.sqrt(
            sum(value * value for value in ema_errors) / max(len(ema_errors), 1)
        ),
        "max_drawdown": max_drawdown,
        "ending_base": state.base,
        "ending_quote": state.quote,
        "ending_claimable_base": state.claimable_base,
        "ending_claimable_quote": state.claimable_quote,
        "controller_external_price_reads": 0,
    }


def _preview_curve_order(
    model: CryptoSwap,
    state: CurveNativeState,
    order: NativeOrder,
    external_price: float,
    current_slot: int,
    config: CurveNativeConfig,
) -> tuple[CurveNativeState, CurveNativeOrderResult, float]:
    candidate, result = preview_curve_native_order(
        model,
        state,
        order.external_notional,
        order.direction,
        external_price,
        current_slot,
        config,
    )
    if order.direction == "x_to_y":
        output_value = result.trade.output_amount
    else:
        output_value = result.trade.output_amount * external_price
    return candidate, result, output_value


def _all_in_curve_cost_bps(
    model: CryptoSwap,
    state: CurveNativeState,
    external_price: float,
    direction: str,
    current_slot: int,
    config: CurveNativeConfig,
    trade_bps: int = 100,
) -> float:
    notional = curve_total_wealth(state, external_price) * trade_bps / 10_000.0
    _, result = preview_curve_native_order(
        model,
        state,
        notional,
        direction,
        external_price,
        current_slot,
        config,
    )
    return -result.trader_profit_at_reference / notional * 10_000.0


def simulate_curve_native_tape(
    model: CryptoSwap,
    path_name: str,
    prices: Sequence[float],
    tape: Sequence[NativeOrder],
    *,
    score_start_index: int = 0,
    arb_latency_minutes: int,
    pool_value: float = POOL_VALUE,
    config: Optional[CurveNativeConfig] = None,
    routing: Optional[RoutingConfig] = None,
    step_minutes: int = 1,
) -> dict[str, object]:
    """Replay pinned Twocrypto-NG v2.1 behavior on the common intent tape."""

    if not prices or min(prices) <= 0:
        raise ValueError("prices must be non-empty and positive")
    if not 0 <= score_start_index < len(prices):
        raise ValueError("score boundary lies outside the path")
    if arb_latency_minutes <= 0 or pool_value <= 0 or step_minutes <= 0:
        raise ValueError("latency, pool value, and step must be positive")
    if arb_latency_minutes % step_minutes != 0:
        raise ValueError("arb_latency_minutes must be a multiple of step_minutes")
    controller = config or CurveNativeConfig()
    controller.validate()
    route = routing or RoutingConfig()
    route.validate()
    grouped_orders = orders_by_minute(tape, len(prices))
    state = initialize_curve_native_pool(
        model,
        prices[0],
        pool_value=pool_value,
        config=controller,
    )
    centered_state = initialize_curve_native_pool(
        model,
        prices[0],
        pool_value=pool_value,
        config=controller,
    )
    centered_fee_free_probe_cost_bps = {
        direction: curve_execution_impact_bps(
            model,
            centered_state,
            prices[0],
            direction,
            trade_bps=SERVICE_PROBE_BPS,
        )
        for direction in ("x_to_y", "y_to_x")
    }

    score_start_base = score_start_quote = 0.0
    score_counters: dict[str, float | int] = {}
    previous_scored_price = prices[score_start_index]
    inventory_carry = 0.0
    attempted_orders = executed_orders = rejected_orders = routed_out_orders = 0
    attempted_volume = executed_volume = routed_out_volume = 0.0
    native_execution_cost_value = 0.0
    selected_execution_cost_value = 0.0
    center_errors: list[float] = []
    ema_errors: list[float] = []
    raw_all_in_probe_costs: list[float] = []
    raw_fee_free_probe_costs: list[float] = []
    depth_degradation_probe_costs: list[float] = []
    depth_degradation_flags: list[bool] = []
    marginal_tracking_errors: list[float] = []
    marginal_tracking_stale_flags: list[bool] = []
    marginal_tracking_stale_episode_lengths: list[int] = []
    active_marginal_tracking_stale_episode = 0
    marginal_tracking_stuck_steps = 0
    peak_wealth = 0.0
    max_drawdown = 0.0

    for minute, external_price in enumerate(prices):
        elapsed_minutes = minute * step_minutes
        current_slot = elapsed_minutes * SLOTS_PER_MINUTE
        scoring = minute >= score_start_index
        if minute == score_start_index:
            score_start_base = state.base
            score_start_quote = state.quote
            score_counters = _curve_counter_snapshot(state)
            previous_scored_price = external_price
            peak_wealth = curve_total_wealth(state, external_price)
        elif scoring:
            inventory_carry += (state.base - score_start_base) * (
                external_price - previous_scored_price
            )

        if elapsed_minutes % arb_latency_minutes == 0:
            execute_curve_profitable_arbitrage(
                model,
                state,
                external_price,
                current_slot,
                controller,
            )

        blocked_before = state.blocked_recenter_count
        for order in grouped_orders[minute]:
            if scoring:
                attempted_orders += 1
                attempted_volume += order.external_notional
            try:
                candidate, result, native_output_value = _preview_curve_order(
                    model,
                    state,
                    order,
                    external_price,
                    current_slot,
                    controller,
                )
            except (ArithmeticError, OverflowError, ValueError):
                if scoring and route.mode == "elastic":
                    outside_value = outside_output_value(
                        order.external_notional,
                        external_price,
                        order.direction,
                        pool_value=pool_value,
                        config=route,
                    )
                    routed_out_orders += 1
                    routed_out_volume += order.external_notional
                    selected_execution_cost_value += (
                        order.external_notional - outside_value
                    )
                elif scoring:
                    rejected_orders += 1
                continue
            outside_value = outside_output_value(
                order.external_notional,
                external_price,
                order.direction,
                pool_value=pool_value,
                config=route,
            )
            if route.mode == "elastic" and native_output_value < outside_value:
                if scoring:
                    routed_out_orders += 1
                    routed_out_volume += order.external_notional
                    selected_execution_cost_value += (
                        order.external_notional - outside_value
                    )
                continue
            # Routing already produced the exact atomic post-trade clone.
            # No state changes before commit, so a second execution is
            # identical work and cannot produce a different valid result.
            state.__dict__.update(candidate.__dict__)
            if scoring:
                executed_orders += 1
                executed_volume += order.external_notional
                native_execution_cost_value -= result.trader_profit_at_reference
                selected_execution_cost_value -= result.trader_profit_at_reference
            markout_steps = max(1, math.ceil(5 / step_minutes))
            next_price = prices[min(minute + markout_steps, len(prices) - 1)]
            trade = result.trade
            if order.direction == "x_to_y":
                markout_profit = trade.output_amount - trade.gross_input * next_price
            else:
                markout_profit = trade.output_amount * next_price - trade.gross_input
            if order.informed:
                state.informed_flow_profit += markout_profit
            else:
                state.uninformed_flow_profit += markout_profit

        if scoring:
            try:
                worst_all_in_cost = max(
                    _all_in_curve_cost_bps(
                        model,
                        state,
                        external_price,
                        direction,
                        current_slot,
                        controller,
                    )
                    for direction in ("x_to_y", "y_to_x")
                )
            except (ArithmeticError, OverflowError, ValueError):
                worst_all_in_cost = math.inf
            raw_all_in_probe_costs.append(worst_all_in_cost)
            try:
                fee_free_costs = {
                    direction: curve_execution_impact_bps(
                        model,
                        state,
                        external_price,
                        direction,
                        trade_bps=SERVICE_PROBE_BPS,
                    )
                    for direction in ("x_to_y", "y_to_x")
                }
                worst_fee_free_impact = max(fee_free_costs.values())
                depth_degradation_cost = max(
                    max(
                        fee_free_costs[direction]
                        - centered_fee_free_probe_cost_bps[direction],
                        0.0,
                    )
                    for direction in ("x_to_y", "y_to_x")
                )
            except (ArithmeticError, OverflowError, ValueError):
                worst_fee_free_impact = math.inf
                depth_degradation_cost = math.inf
            raw_fee_free_probe_costs.append(worst_fee_free_impact)
            depth_degradation_probe_costs.append(depth_degradation_cost)
            depth_degraded = (
                depth_degradation_cost > STATE_DEVIATION_THRESHOLD_BPS
            )
            depth_degradation_flags.append(depth_degraded)
            try:
                common_base, common_quote = normalized_reserves(state)
                normalized_marginal = model.marginal_price_raw(
                    common_base,
                    common_quote,
                )
                marginal_tracking_error = _marginal_tracking_error_bps(
                    normalized_marginal,
                    state.price_scale,
                    external_price,
                )
            except (ArithmeticError, OverflowError, ValueError):
                marginal_tracking_error = math.inf
            marginal_tracking_errors.append(marginal_tracking_error)
            marginal_tracking_stale = (
                marginal_tracking_error > STATE_DEVIATION_THRESHOLD_BPS
            )
            marginal_tracking_stale_flags.append(marginal_tracking_stale)
            if marginal_tracking_stale:
                active_marginal_tracking_stale_episode += 1
                distance = abs(state.ema_price / state.price_scale - 1.0)
                step = max(controller.min_adjustment_step, distance / 5.0)
                pre_gate = state.virtual_price - controller.allowed_extra_profit
                if (
                    state.blocked_recenter_count > blocked_before
                    or (
                        distance > step
                        and pre_gate * pre_gate <= state.xcp_profit
                    )
                ):
                    marginal_tracking_stuck_steps += 1
            elif active_marginal_tracking_stale_episode:
                marginal_tracking_stale_episode_lengths.append(
                    active_marginal_tracking_stale_episode * step_minutes
                )
                active_marginal_tracking_stale_episode = 0
            center_errors.append(
                abs(math.log(external_price / state.price_scale)) * 10_000.0
            )
            ema_errors.append(
                abs(math.log(external_price / state.ema_price)) * 10_000.0
            )
            wealth = curve_total_wealth(state, external_price)
            peak_wealth = max(peak_wealth, wealth)
            max_drawdown = max(max_drawdown, 1.0 - wealth / peak_wealth)
            previous_scored_price = external_price

    if active_marginal_tracking_stale_episode:
        marginal_tracking_stale_episode_lengths.append(
            active_marginal_tracking_stale_episode * step_minutes
        )

    final_price = prices[-1]
    final_wealth = curve_total_wealth(state, final_price)
    hold_value = score_start_base * final_price + score_start_quote
    net_lp_pnl = final_wealth - hold_value
    counters = _curve_counter_snapshot(state)

    def delta(field: str) -> float:
        return float(counters[field]) - float(score_counters[field])

    pool_flow_pnl = delta("pool_flow_pnl")
    accounting_error = net_lp_pnl - (pool_flow_pnl + inventory_carry)
    requested_move = delta("requested_center_move_log")
    funded_move = delta("funded_center_move_log")
    xcp_to_value = 2.0 * math.sqrt(final_price)
    required_realized = (
        math.sqrt(state.xcp_profit) + controller.allowed_extra_profit
    )
    available_profit_xcp = max(
        state.virtual_price - required_realized,
        0.0,
    ) * state.initial_xcp
    starting_protocol_profit = max(
        float(score_counters["xcp_profit"])
        - float(score_counters["xcp_profit_a"]),
        0.0,
    )
    starting_protocol_fee_liability_fraction = (
        ADMIN_PROFIT_LIABILITY_FRACTION
        * starting_protocol_profit
        / max(float(score_counters["virtual_price"]), 1e-18)
    )
    final_protocol_profit = max(state.xcp_profit - state.xcp_profit_a, 0.0)
    final_protocol_fee_liability_fraction = (
        ADMIN_PROFIT_LIABILITY_FRACTION
        * final_protocol_profit
        / max(state.virtual_price, 1e-18)
    )
    starting_protocol_fee_liability_value = (
        hold_value * starting_protocol_fee_liability_fraction
    )
    protocol_fee_liability_value = (
        final_wealth * final_protocol_fee_liability_fraction
    )
    lp_net_hold_value = hold_value - starting_protocol_fee_liability_value
    lp_net_final_wealth = final_wealth - protocol_fee_liability_value
    net_lp_pnl_after_protocol_fee = lp_net_final_wealth - lp_net_hold_value
    return {
        "system": "curve_native_v2_1",
        "model": "curve",
        "path": path_name,
        "routing_mode": route.mode,
        "score_start_minute": score_start_index * step_minutes,
        "score_minutes": (len(prices) - score_start_index) * step_minutes,
        "step_minutes": step_minutes,
        "arb_latency_minutes": arb_latency_minutes,
        "initial_pool_value": pool_value,
        "final_lp_wealth": final_wealth,
        "hold_value": hold_value,
        "net_lp_pnl_vs_hold": net_lp_pnl,
        "net_lp_return_vs_hold_bps": net_lp_pnl / hold_value * 10_000.0,
        "protocol_fee_liability_value": protocol_fee_liability_value,
        "starting_protocol_fee_liability_value": (
            starting_protocol_fee_liability_value
        ),
        "scoring_period_protocol_fee_liability_value": (
            protocol_fee_liability_value
            - starting_protocol_fee_liability_value
        ),
        "starting_protocol_fee_liability_fraction": (
            starting_protocol_fee_liability_fraction
        ),
        "final_protocol_fee_liability_fraction": (
            final_protocol_fee_liability_fraction
        ),
        "net_lp_pnl_after_protocol_fee": net_lp_pnl_after_protocol_fee,
        "net_lp_pnl_after_revenue_split": net_lp_pnl_after_protocol_fee,
        "net_lp_return_after_protocol_fee_bps": (
            net_lp_pnl_after_protocol_fee / lp_net_hold_value * 10_000.0
        ),
        "net_lp_return_after_revenue_split_bps": (
            net_lp_pnl_after_protocol_fee / lp_net_hold_value * 10_000.0
        ),
        "lp_net_final_wealth": lp_net_final_wealth,
        "lp_net_hold_value": lp_net_hold_value,
        "loss_vs_hold": -net_lp_pnl,
        "gross_fee_value": delta("gross_fee_value"),
        "baseline_fee_value": delta("baseline_fee_value"),
        "excess_fee_value": delta("excess_fee_value"),
        "total_retained_fee_value": delta("gross_fee_value"),
        "recenter_reserved_fee_value": delta("gross_fee_value"),
        "base_fee_value": 0.0,
        "dynamic_fee_value": delta("gross_fee_value"),
        "retained_surcharge_value": delta("gross_fee_value"),
        "distributed_surcharge_value": 0.0,
        "realized_arb_profit": delta("realized_arb_profit"),
        "realized_arbitrage_extraction_proxy_value": delta(
            "realized_arb_profit"
        ),
        "arbitrage_fill_count": int(delta("arbitrage_fill_count")),
        # Match Dusk: the canonical cap diagnostic covers burn-in and score.
        "arbitrage_capped_event_count": int(
            state.arbitrage_capped_event_count
        ),
        "scored_arbitrage_capped_event_count": int(
            delta("arbitrage_capped_event_count")
        ),
        "arbitrage_numerical_failure_count": int(
            state.arbitrage_numerical_failure_count
        ),
        "scored_arbitrage_numerical_failure_count": int(
            delta("arbitrage_numerical_failure_count")
        ),
        "informed_flow_markout": delta("informed_flow_profit"),
        "uninformed_flow_markout": delta("uninformed_flow_profit"),
        "pool_flow_pnl": pool_flow_pnl,
        "inventory_carry": inventory_carry,
        "accounting_identity_error": accounting_error,
        "recenter_impairment_value": (
            delta("recenter_impairment_xcp") * xcp_to_value
        ),
        "requested_recenter_impairment_value": (
            delta("requested_recenter_impairment_xcp") * xcp_to_value
        ),
        "recenter_count": int(delta("recenter_count")),
        "requested_recenter_count": int(delta("recenter_attempt_count")),
        "blocked_recenter_count": int(delta("blocked_recenter_count")),
        "requested_center_move_log": requested_move,
        "funded_center_move_log": funded_move,
        "center_move_funding_fraction": (
            funded_move / requested_move if requested_move > 0 else 1.0
        ),
        "available_recenter_budget_value": available_profit_xcp * xcp_to_value,
        "attempted_user_orders": attempted_orders,
        "executed_user_orders": executed_orders,
        "rejected_user_orders": rejected_orders,
        "routed_out_orders": routed_out_orders,
        "attempted_user_volume": attempted_volume,
        "executed_user_volume": executed_volume,
        "routed_out_volume": routed_out_volume,
        "captured_user_volume_fraction": (
            executed_volume / attempted_volume if attempted_volume > 0 else 1.0
        ),
        "volume_weighted_user_execution_cost_bps": (
            native_execution_cost_value / executed_volume * 10_000.0
            if executed_volume > 0
            else 0.0
        ),
        "native_only_execution_cost_bps": (
            native_execution_cost_value / executed_volume * 10_000.0
            if executed_volume > 0
            else 0.0
        ),
        "selected_execution_cost_bps": (
            selected_execution_cost_value / attempted_volume * 10_000.0
            if attempted_volume > 0 and rejected_orders == 0
            else math.inf
        ),
        "raw_all_in_service_median_cost_bps": _quantile(
            raw_all_in_probe_costs, 0.50
        ),
        "raw_all_in_service_p90_cost_bps": _quantile(
            raw_all_in_probe_costs, 0.90
        ),
        "raw_fee_free_service_median_cost_bps": _quantile(
            raw_fee_free_probe_costs, 0.50
        ),
        "raw_fee_free_service_p90_cost_bps": _quantile(
            raw_fee_free_probe_costs, 0.90
        ),
        "centered_fee_free_service_cost_bps": max(
            centered_fee_free_probe_cost_bps.values()
        ),
        "depth_degradation_median_cost_bps": _quantile(
            depth_degradation_probe_costs, 0.50
        ),
        "depth_degradation_p90_cost_bps": _quantile(
            depth_degradation_probe_costs, 0.90
        ),
        "depth_degradation_fraction": (
            sum(depth_degradation_flags)
            / max(len(depth_degradation_flags), 1)
        ),
        "marginal_tracking_stale_fraction": (
            sum(marginal_tracking_stale_flags)
            / max(len(marginal_tracking_stale_flags), 1)
        ),
        "marginal_tracking_rmse_bps": math.sqrt(
            sum(value * value for value in marginal_tracking_errors)
            / max(len(marginal_tracking_errors), 1)
        ),
        "marginal_tracking_p95_bps": _quantile(
            marginal_tracking_errors, 0.95
        ),
        "marginal_tracking_max_bps": max(
            marginal_tracking_errors, default=0.0
        ),
        "marginal_tracking_stale_episode_count": len(
            marginal_tracking_stale_episode_lengths
        ),
        "marginal_tracking_stale_episode_p95_minutes": _quantile(
            marginal_tracking_stale_episode_lengths, 0.95
        ),
        "marginal_tracking_stale_episode_max_minutes": max(
            marginal_tracking_stale_episode_lengths, default=0
        ),
        "marginal_tracking_stuck_fraction": (
            marginal_tracking_stuck_steps
            / max(len(marginal_tracking_stale_flags), 1)
        ),
        "center_tracking_rmse_bps": math.sqrt(
            sum(value * value for value in center_errors)
            / max(len(center_errors), 1)
        ),
        "center_tracking_p95_bps": _quantile(center_errors, 0.95),
        "center_tracking_max_bps": max(center_errors, default=0.0),
        "ema_tracking_rmse_bps": math.sqrt(
            sum(value * value for value in ema_errors) / max(len(ema_errors), 1)
        ),
        "max_drawdown": max_drawdown,
        "ending_base": state.base,
        "ending_quote": state.quote,
        "ending_claimable_base": 0.0,
        "ending_claimable_quote": 0.0,
        "price_scale": state.price_scale,
        "ema_price": state.ema_price,
        "virtual_price": state.virtual_price,
        "xcp_profit": state.xcp_profit,
        "controller_external_price_reads": 0,
    }

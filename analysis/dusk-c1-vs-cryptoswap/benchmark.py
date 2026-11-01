#!/usr/bin/env python3
"""Run the focused CryptoSwap versus finite-tail Dusk C1 benchmark.

The run is deterministic, offline, and intentionally much smaller than the
sealed legacy controller study.  It answers the invariant decision first:

* execution from the same balanced reserves;
* one-jump arbitrage loss from the same balanced reserves;
* reachable tail behavior inside x/D <= 200;
* center-movement impairment under identical candidate center changes;
* numerical safety and nonlinear-solver work.

Actual Solana compute units are outside this analytical package.  The solver
counts here are only a shortlist gate for a later Q48/u128 SBF prototype.
"""

from __future__ import annotations

import csv
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path
import tempfile

from models import (
    Cpmm,
    CryptoSwap,
    CryptoSwapLimit,
    DuskC1,
    execution_price_premium_bps,
    jump_loss_bps,
    normalized_reserves_at_price,
    output_shortfall_bps,
)


ROOT = Path(__file__).resolve().parent
REPOSITORY = ROOT.parents[1]
RESULTS = ROOT / "results"
BRIDGE_FRACTIONS = (0.25, 0.5, 0.75)
SELECTED_BRIDGE_FRACTION = 0.5

PROFILES = (
    {
        "profile": "broad",
        "peak_depth": 20.0,
        "fade_scale": 0.05,
        "curve_A": 10.0,
        "description": "broad low-concentration long-tail profile",
    },
    {
        "profile": "balanced",
        "peak_depth": 200.0,
        "fade_scale": 0.005,
        "curve_A": 100.0,
        "description": "middle concentration and fade profile",
    },
    {
        "profile": "aggressive",
        "peak_depth": 2_000.0,
        "fade_scale": 0.0005,
        "curve_A": 1_000.0,
        "description": "high central depth with a narrow fade",
    },
)

TRADE_SIZES_BPS = (1, 2, 5, 10, 20, 50, 100, 200, 500, 1_000, 1_500, 2_000)
UPWARD_JUMP_FACTORS = (1.01, 1.02, 1.05, 1.10, 1.25, 1.50, 2.0, 4.0, 10.0)
JUMP_FACTORS = tuple(1.0 / factor for factor in reversed(UPWARD_JUMP_FACTORS)) + UPWARD_JUMP_FACTORS
TAIL_X_OVER_D = (0.5, 0.75, 1.0, 2.0, 5.0, 10.0, 50.0, 100.0, 200.0)
CENTER_MOVE_FRACTIONS = (0.01, 0.05, 0.10, 0.25, 0.50, 1.0)
SAFETY_PEAK_DEPTHS = (2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0)
SAFETY_FADE_SCALES = (
    1e-7,
    3e-7,
    1e-6,
    3e-6,
    1e-5,
    3e-5,
    1e-4,
    3e-4,
    1e-3,
    3e-3,
    1e-2,
    3e-2,
    1e-1,
    0.199,
)
PARETO_PEAK_DEPTHS = (2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0)
PARETO_FADE_SCALES = (1e-4, 3e-4, 5e-4, 1e-3, 3e-3, 5e-3, 1e-2, 3e-2, 5e-2, 1e-1)
PARETO_TRADE_SIZES_BPS = (10, 100, 1_000, 2_000)
SERVICE_PEAK_DEPTH_MIN = 2.0
SERVICE_PEAK_DEPTH_MAX = 2_000.0
SERVICE_PEAK_DEPTH_POINTS = 31
SERVICE_FADE_SCALE_MIN = 1e-7
SERVICE_FADE_SCALE_MAX = 0.199
SERVICE_FADE_BISECTION_ITERATIONS = 35
SERVICE_FEASIBILITY_TOLERANCE_BPS = 1e-9


def _logspace(start: float, stop: float, count: int) -> tuple[float, ...]:
    if start <= 0.0 or stop <= start or count < 2:
        raise ValueError("invalid logarithmic grid")
    log_start = math.log(start)
    log_stop = math.log(stop)
    return tuple(
        start
        if index == 0
        else stop
        if index == count - 1
        else math.exp(log_start + (log_stop - log_start) * index / (count - 1))
        for index in range(count)
    )


SERVICE_PEAK_DEPTHS = _logspace(
    SERVICE_PEAK_DEPTH_MIN,
    SERVICE_PEAK_DEPTH_MAX,
    SERVICE_PEAK_DEPTH_POINTS,
)


def _geometric_path(final_price: float, steps: int) -> tuple[float, ...]:
    return tuple(final_price ** (index / steps) for index in range(steps + 1))


SYNTHETIC_PATHS = {
    "single_jump_2x": (1.0, 2.0),
    "staircase_2x": _geometric_path(2.0, 20),
    "staircase_4x": _geometric_path(4.0, 40),
    "jump_reversal_2x": (1.0, 2.0, 1.0),
    "alternating_chop_10pct": (1.0,) + tuple(
        1.1 if index % 2 == 0 else 1.0 / 1.1 for index in range(20)
    ) + (1.0,),
    "volatility_cluster": (1.0, 1.01, 0.99, 1.02, 0.98, 1.05, 0.95, 1.10, 0.90, 1.0),
}


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _write_csv(path: Path, rows: list[dict[str, object]]) -> None:
    if not rows:
        raise ValueError(f"refusing to write empty CSV {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", newline="", dir=path.parent, delete=False) as temporary:
        writer = csv.DictWriter(temporary, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
        temporary_path = Path(temporary.name)
    temporary_path.replace(path)


def _write_json(path: Path, payload: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", dir=path.parent, delete=False) as temporary:
        json.dump(payload, temporary, indent=2, sort_keys=True)
        temporary.write("\n")
        temporary_path = Path(temporary.name)
    temporary_path.replace(path)


def models_for_profile(profile: dict[str, object], bridge_fraction: float = SELECTED_BRIDGE_FRACTION):
    peak_depth = float(profile["peak_depth"])
    fade_scale = float(profile["fade_scale"])
    return (
        Cpmm(),
        CryptoSwap(peak_depth=peak_depth, fade_scale=fade_scale),
        DuskC1(
            peak_depth=peak_depth,
            fade_scale=fade_scale,
            bridge_start_fraction=bridge_fraction,
        ),
    )


def model_id(model) -> str:
    if isinstance(model, DuskC1):
        return "dusk_c1"
    if isinstance(model, CryptoSwap):
        return "cryptoswap"
    return "cpmm"


def execution_rows() -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for profile in PROFILES:
        for model in models_for_profile(profile):
            for size_bps in TRADE_SIZES_BPS:
                quote = model.quote_exact_in(1.0, 1.0, 2.0 * size_bps / 10_000.0)
                rows.append(
                    {
                        "profile": profile["profile"],
                        "model": model_id(model),
                        "model_label": model.name,
                        "peak_depth": profile["peak_depth"],
                        "curve_A": profile["curve_A"],
                        "fade_scale_gamma": profile["fade_scale"],
                        "trade_size_bps_of_initial_tvl": size_bps,
                        "average_execution_price_premium_bps": execution_price_premium_bps(model, size_bps),
                        "exact_in_output_shortfall_bps": output_shortfall_bps(model, size_bps),
                        "amount_out": quote.amount_out,
                        "q_root_iterations": quote.q_root_iterations,
                        "reserve_root_iterations": quote.reserve_root_iterations,
                        "exact_tail_fast_path": quote.used_exact_tail,
                    }
                )
    return rows


def jump_rows() -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for profile in PROFILES:
        for model in models_for_profile(profile):
            for external_price in JUMP_FACTORS:
                loss_bps, base, quote = jump_loss_bps(model, external_price)
                v = model.imbalance(base, quote)
                region = model.region_for_v(v)
                rows.append(
                    {
                        "profile": profile["profile"],
                        "model": model_id(model),
                        "model_label": model.name,
                        "peak_depth": profile["peak_depth"],
                        "curve_A": profile["curve_A"],
                        "fade_scale_gamma": profile["fade_scale"],
                        "external_price_multiple": external_price,
                        "direction": "up" if external_price > 1.0 else "down",
                        "external_jump_bps_log": math.log(external_price) * 10_000.0,
                        "one_jump_arbitrage_loss_bps_of_initial_tvl": loss_bps,
                        "one_jump_arbitrage_loss_bps_of_hold_value": (
                            loss_bps * 2.0 / (external_price + 1.0)
                        ),
                        "base_after_arbitrage": base,
                        "quote_after_arbitrage": quote,
                        "x_over_d_max": max(base, quote) / 2.0,
                        "curve_region": region,
                    }
                )
    return rows


def tail_rows() -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for profile in PROFILES:
        for model in models_for_profile(profile):
            for x_over_d in TAIL_X_OVER_D:
                x = 2.0 * x_over_d
                y, q_iterations, reserve_iterations = model.reserve_y(x, 2.0)
                marginal = model.marginal_price(x, y)
                local_cpmm = y / x
                amount_in = 0.01 * x
                quoted = model.quote_exact_in(x, y, amount_in)
                marginal_ideal = marginal * amount_in
                rows.append(
                    {
                        "profile": profile["profile"],
                        "model": model_id(model),
                        "model_label": model.name,
                        "peak_depth": profile["peak_depth"],
                        "curve_A": profile["curve_A"],
                        "fade_scale_gamma": profile["fade_scale"],
                        "x_over_d": x_over_d,
                        "reserve_ratio_x_over_y": x / y,
                        "marginal_price": marginal,
                        "local_cpmm_price": local_cpmm,
                        "local_exponent": marginal / local_cpmm,
                        "one_percent_x_trade_slippage_bps": (
                            marginal_ideal / quoted.amount_out - 1.0
                        )
                        * 10_000.0,
                        "q_root_iterations": q_iterations + quoted.q_root_iterations,
                        "reserve_root_iterations": reserve_iterations + quoted.reserve_root_iterations,
                        "exact_tail_fast_path": quoted.used_exact_tail,
                        "curve_region": model.region_for_v(model.imbalance(x, y)),
                    }
                )
    return rows


def protected_value(model, base: float, quote: float, center: float) -> float:
    common_base = base * center
    invariant = model.invariant(common_base, quote)
    return invariant / (2.0 * math.sqrt(center))


def recenter_rows() -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for profile in PROFILES:
        for model in models_for_profile(profile):
            for external_price in (1.10, 1.25, 1.50, 2.0, 4.0):
                native_reserves = normalized_reserves_at_price(model, external_price)[:2]
                common_reserves = normalized_reserves_at_price(Cpmm(), external_price)[:2]
                for reserve_basis, (base, quote) in (
                    ("native_post_arbitrage", native_reserves),
                    ("common_cpmm_price_state", common_reserves),
                ):
                    old_value = protected_value(model, base, quote, 1.0)
                    old_v = model.imbalance(base, quote)
                    for movement_fraction in CENTER_MOVE_FRACTIONS:
                        candidate_center = external_price**movement_fraction
                        candidate_value = protected_value(model, base, quote, candidate_center)
                        impairment = max(old_value - candidate_value, 0.0)
                        candidate_v = model.imbalance(base * candidate_center, quote)
                        same_exact_tail = (
                            isinstance(model, DuskC1)
                            and model.region_for_v(old_v) == "tail"
                            and model.region_for_v(candidate_v) == "tail"
                            and (base - quote) * (base * candidate_center - quote) > 0.0
                        )
                        rows.append(
                            {
                                "profile": profile["profile"],
                                "model": model_id(model),
                                "model_label": model.name,
                                "peak_depth": profile["peak_depth"],
                                "curve_A": profile["curve_A"],
                                "fade_scale_gamma": profile["fade_scale"],
                                "reserve_basis": reserve_basis,
                                "external_price_multiple": external_price,
                                "center_movement_fraction_of_log_gap": movement_fraction,
                                "candidate_center": candidate_center,
                                "protected_q_before": old_value,
                                "protected_q_after": candidate_value,
                                "protected_q_impairment_bps": impairment / old_value * 10_000.0,
                                "same_exact_tail": same_exact_tail,
                                "starting_region": model.region_for_v(old_v),
                                "candidate_region": model.region_for_v(candidate_v),
                            }
                        )
    return rows


def path_lvr_rows() -> list[dict[str, object]]:
    """Measure cumulative fee-free arbitrage loss along deterministic paths."""

    rows: list[dict[str, object]] = []
    for profile in PROFILES:
        for model in models_for_profile(profile):
            for scenario, path in SYNTHETIC_PATHS.items():
                base = quote = 1.0
                cumulative_loss = 0.0
                maximum_v = 0.0
                transition_steps = 0
                tail_steps = 0
                for target_price in path[1:]:
                    before = base * target_price + quote
                    base, quote, _ = normalized_reserves_at_price(model, target_price)
                    after = base * target_price + quote
                    step_loss = before - after
                    if step_loss < -2e-10 * max(before, 1.0):
                        raise RuntimeError("arbitrage step increased LP marked value")
                    cumulative_loss += max(step_loss, 0.0)
                    v = model.imbalance(base, quote)
                    maximum_v = max(maximum_v, v)
                    region = model.region_for_v(v)
                    transition_steps += region == "transition"
                    tail_steps += region == "tail"
                final_price = path[-1]
                final_hold = final_price + 1.0
                final_pool = base * final_price + quote
                final_loss = final_hold - final_pool
                if final_loss < -2e-10 * max(final_hold, 1.0):
                    raise RuntimeError("final pool value exceeded initial-inventory hold value")
                rows.append(
                    {
                        "profile": profile["profile"],
                        "model": model_id(model),
                        "model_label": model.name,
                        "scenario": scenario,
                        "price_steps": len(path) - 1,
                        "final_price_multiple": final_price,
                        "cumulative_path_lvr_bps_of_initial_tvl": cumulative_loss / 2.0 * 10_000.0,
                        "final_loss_versus_hold_bps_of_initial_tvl": max(final_loss, 0.0)
                        / 2.0
                        * 10_000.0,
                        "maximum_imbalance_v": maximum_v,
                        "transition_steps": transition_steps,
                        "tail_steps": tail_steps,
                        "fees_included": False,
                    }
                )
    return rows


def restoring_rows() -> list[dict[str, object]]:
    """Measure center-restoring quotes from each model's native off-center state."""

    rows: list[dict[str, object]] = []
    cpmm = Cpmm()
    for profile in PROFILES:
        for model in models_for_profile(profile):
            for starting_price in (1.10, 1.25, 1.50, 2.0, 4.0):
                base, quote, _ = normalized_reserves_at_price(model, starting_price)
                starting_v = model.imbalance(base, quote)
                for restoring_fraction in (0.10, 0.25, 0.50, 1.0):
                    target_price = starting_price ** (1.0 - restoring_fraction)
                    target_base, target_quote, _ = normalized_reserves_at_price(model, target_price)
                    amount_in = target_base - base
                    amount_out = quote - target_quote
                    if amount_in <= 0.0 or amount_out <= 0.0:
                        raise RuntimeError("restoring path did not move toward balance")
                    cpmm_out = cpmm.quote_exact_in(base, quote, amount_in).amount_out
                    target_v = model.imbalance(target_base, target_quote)
                    rows.append(
                        {
                            "profile": profile["profile"],
                            "model": model_id(model),
                            "model_label": model.name,
                            "starting_price_multiple": starting_price,
                            "restoring_fraction_of_log_price_gap": restoring_fraction,
                            "target_price_multiple": target_price,
                            "amount_in": amount_in,
                            "amount_out": amount_out,
                            "same_reserve_cpmm_amount_out": cpmm_out,
                            "output_difference_vs_same_reserve_cpmm": amount_out - cpmm_out,
                            "output_difference_vs_same_reserve_cpmm_bps": (
                                amount_out / cpmm_out - 1.0
                            )
                            * 10_000.0,
                            "starting_region": model.region_for_v(starting_v),
                            "target_region": model.region_for_v(target_v),
                            "crosses_region": model.region_for_v(starting_v)
                            != model.region_for_v(target_v),
                            "fees_included": False,
                        }
                    )
    return rows


def _pareto_flags(rows: list[dict[str, object]], objective_keys: tuple[str, ...]) -> list[bool]:
    flags: list[bool] = []
    tolerance = 1e-10
    for candidate in rows:
        objectives = tuple(float(candidate[key]) for key in objective_keys)
        dominated = False
        for other in rows:
            if other is candidate:
                continue
            comparison = tuple(float(other[key]) for key in objective_keys)
            no_worse = all(left <= right + tolerance for left, right in zip(comparison, objectives))
            materially_better = any(left < right - 1e-8 for left, right in zip(comparison, objectives))
            if no_worse and materially_better:
                dominated = True
                break
        flags.append(not dominated)
    return flags


def pareto_rows() -> list[dict[str, object]]:
    """Sweep both invariant families so same-parameter results are not mistaken for a fair frontier."""

    rows: list[dict[str, object]] = []
    for model_kind in ("cryptoswap", "dusk_c1"):
        for peak_depth in PARETO_PEAK_DEPTHS:
            for fade_scale in PARETO_FADE_SCALES:
                model = (
                    CryptoSwap(peak_depth, fade_scale)
                    if model_kind == "cryptoswap"
                    else DuskC1(peak_depth, fade_scale, SELECTED_BRIDGE_FRACTION)
                )
                execution = {
                    size: execution_price_premium_bps(model, size) for size in PARETO_TRADE_SIZES_BPS
                }
                rows.append(
                    {
                        "model": model_kind,
                        "peak_depth": peak_depth,
                        "curve_A_equivalent": peak_depth / 2.0,
                        "fade_scale_gamma": fade_scale,
                        "execution_premium_10bps_trade": execution[10],
                        "execution_premium_100bps_trade": execution[100],
                        "execution_premium_1000bps_trade": execution[1_000],
                        "execution_premium_2000bps_trade": execution[2_000],
                        "one_jump_loss_2x_bps_initial_tvl": jump_loss_bps(model, 2.0)[0],
                        "one_jump_loss_4x_bps_initial_tvl": jump_loss_bps(model, 4.0)[0],
                    }
                )

    two_way = (
        "execution_premium_100bps_trade",
        "one_jump_loss_2x_bps_initial_tvl",
    )
    four_way = (
        "execution_premium_10bps_trade",
        "execution_premium_100bps_trade",
        "execution_premium_1000bps_trade",
        "execution_premium_2000bps_trade",
        "one_jump_loss_2x_bps_initial_tvl",
    )
    for row, two_flag, four_flag in zip(rows, _pareto_flags(rows, two_way), _pareto_flags(rows, four_way)):
        row["pareto_100bps_execution_vs_2x_loss"] = two_flag
        row["pareto_multi_size_execution_vs_2x_loss"] = four_flag
    return rows


def service_constrained_rows() -> list[dict[str, object]]:
    """Search the bounded parameter envelope for the safest no-worse-service CryptoSwap."""

    rows: list[dict[str, object]] = []
    execution_keys = tuple(
        f"execution_premium_{size}bps_trade" for size in PARETO_TRADE_SIZES_BPS
    )
    for profile in PROFILES:
        dusk = DuskC1(
            peak_depth=float(profile["peak_depth"]),
            fade_scale=float(profile["fade_scale"]),
            bridge_start_fraction=SELECTED_BRIDGE_FRACTION,
        )
        dusk_service = {
            key: execution_price_premium_bps(dusk, size)
            for key, size in zip(execution_keys, PARETO_TRADE_SIZES_BPS)
        }
        limit_constraints: list[tuple[float, int]] = []
        for size in PARETO_TRADE_SIZES_BPS:
            amount_in = 2.0 * size / 10_000.0
            quote = dusk.quote_exact_in(1.0, 1.0, amount_in)
            x_after = 1.0 + amount_in
            y_after = 1.0 - quote.amount_out
            q_after = x_after * y_after
            delta = 1.0 - q_after
            h_after = (x_after + y_after - 2.0) / 2.0
            if min(q_after, delta, h_after) <= 0.0:
                raise ArithmeticDomainError("invalid service-limit geometry")
            concentration_budget = delta**3 / (2.0 * q_after * h_after)
            limit_constraints.append((concentration_budget, size))
        limit_budget, limit_active_size = max(limit_constraints)
        limit_curve = CryptoSwapLimit(limit_budget)
        limit_service = {
            key: execution_price_premium_bps(limit_curve, size)
            for key, size in zip(execution_keys, PARETO_TRADE_SIZES_BPS)
        }
        if not all(
            limit_service[key] <= dusk_service[key] + 2e-7 for key in execution_keys
        ):
            raise RuntimeError(f"invalid singular-limit service envelope for {profile['profile']}")

        def curve_metrics(peak_depth: float, fade_scale: float) -> dict[str, float]:
            curve = CryptoSwap(peak_depth, fade_scale)
            execution = {
                key: execution_price_premium_bps(curve, size)
                for key, size in zip(execution_keys, PARETO_TRADE_SIZES_BPS)
            }
            return {
                **execution,
                "one_jump_loss_2x_bps_initial_tvl": jump_loss_bps(curve, 2.0)[0],
            }

        def feasible(metrics: dict[str, float]) -> bool:
            return all(
                metrics[key] <= dusk_service[key] + SERVICE_FEASIBILITY_TOLERANCE_BPS
                for key in execution_keys
            )

        candidates: list[dict[str, float]] = []
        for peak_depth in SERVICE_PEAK_DEPTHS:
            high = SERVICE_FADE_SCALE_MAX
            high_metrics = curve_metrics(peak_depth, high)
            if not feasible(high_metrics):
                continue
            low = SERVICE_FADE_SCALE_MIN
            low_metrics = curve_metrics(peak_depth, low)
            if feasible(low_metrics):
                high = low
                high_metrics = low_metrics
            else:
                for _ in range(SERVICE_FADE_BISECTION_ITERATIONS):
                    middle = math.sqrt(low * high)
                    middle_metrics = curve_metrics(peak_depth, middle)
                    if feasible(middle_metrics):
                        high = middle
                        high_metrics = middle_metrics
                    else:
                        low = middle
            candidates.append(
                {
                    "peak_depth": peak_depth,
                    "curve_A_equivalent": peak_depth / 2.0,
                    "fade_scale_gamma": high,
                    **high_metrics,
                }
            )

        if not candidates:
            raise RuntimeError(f"no bounded equal-service CryptoSwap match for {profile['profile']}")
        match = min(candidates, key=lambda row: row["one_jump_loss_2x_bps_initial_tvl"])
        dusk_loss = jump_loss_bps(dusk, 2.0)[0]
        limit_loss = jump_loss_bps(limit_curve, 2.0)[0]
        rows.append(
            {
                "dusk_profile": profile["profile"],
                "dusk_peak_depth": profile["peak_depth"],
                "dusk_fade_scale": profile["fade_scale"],
                **{f"dusk_{key}": value for key, value in dusk_service.items()},
                "dusk_one_jump_loss_2x_bps_initial_tvl": dusk_loss,
                "matched_curve_peak_depth": match["peak_depth"],
                "matched_curve_A": match["curve_A_equivalent"],
                "matched_curve_gamma": match["fade_scale_gamma"],
                **{
                    f"matched_curve_{key}": match[key]
                    for key in execution_keys
                },
                "matched_curve_one_jump_loss_2x_bps_initial_tvl": match[
                    "one_jump_loss_2x_bps_initial_tvl"
                ],
                "dusk_loss_reduction_vs_no_worse_execution_curve_bps": float(
                    match["one_jump_loss_2x_bps_initial_tvl"]
                )
                - dusk_loss,
                "limit_concentration_budget_P_times_s_squared": limit_budget,
                "limit_active_trade_size_bps": limit_active_size,
                **{
                    f"limit_curve_{key}": value
                    for key, value in limit_service.items()
                },
                "limit_curve_one_jump_loss_2x_bps_initial_tvl": limit_loss,
                "dusk_loss_reduction_vs_limit_curve_bps": limit_loss - dusk_loss,
                "limit_status": "infimum as fade_scale→0 and peak_depth→∞; no finite parameter pair attains it",
                "feasible_peak_depth_grid_points": len(candidates),
                "search_peak_depth_min": SERVICE_PEAK_DEPTH_MIN,
                "search_peak_depth_max": SERVICE_PEAK_DEPTH_MAX,
                "search_peak_depth_points": SERVICE_PEAK_DEPTH_POINTS,
                "search_fade_scale_min": SERVICE_FADE_SCALE_MIN,
                "search_fade_scale_max": SERVICE_FADE_SCALE_MAX,
                "search_fade_bisection_iterations": SERVICE_FADE_BISECTION_ITERATIONS,
                "search_scope": "balanced-start outward exact-in only",
            }
        )
    return rows


def solver_rows() -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for profile in PROFILES:
        c1 = models_for_profile(profile)[2]
        geometry = c1.bridge_geometry()
        transition_v = (geometry.v_start + geometry.v_tail) / 2.0
        tail_v = max(geometry.v_tail * 2.0, 2.0)
        cases = (
            ("center", "outward", 0.0, None),
            ("transition", "outward", transition_v, None),
            ("transition", "restoring", -transition_v, None),
            ("tail", "outward", tail_v, None),
            ("tail", "restoring", -tail_v, None),
            ("tail_crossing", "restoring", -geometry.v_tail * 1.5, -transition_v),
        )
        for model in models_for_profile(profile):
            for state_name, direction, signed_v, target_signed_v in cases:
                x, y = model.coordinates(2.0, math.asinh(signed_v))
                if target_signed_v is None:
                    amount_in = 0.01 * x
                else:
                    target_x, _, = model.coordinates(2.0, math.asinh(target_signed_v))
                    amount_in = target_x - x
                exact_in = model.quote_exact_in(x, y, amount_in)
                exact_out = model.quote_exact_out(x, y, exact_in.amount_out)
                for quote_mode, quote in (("exact_in", exact_in), ("exact_out", exact_out)):
                    rows.append(
                        {
                            "profile": profile["profile"],
                            "model": model_id(model),
                            "model_label": model.name,
                            "state": state_name,
                            "direction": direction,
                            "quote_mode": quote_mode,
                            "state_v": abs(signed_v),
                            "q_root_iterations": quote.q_root_iterations,
                            "reserve_root_iterations": quote.reserve_root_iterations,
                            "total_nonlinear_iterations_proxy": (
                                quote.q_root_iterations + quote.reserve_root_iterations
                            ),
                            "exact_tail_fast_path": quote.used_exact_tail,
                            "note": "Reference-algorithm proxy only; not Solana compute units",
                        }
                    )
    return rows


def _log_curvature(model, v: float, step: float) -> float:
    low = max(v - step, 0.0)
    high = v + step
    x0, y0 = model.coordinates(2.0, math.asinh(low))
    x1, y1 = model.coordinates(2.0, math.asinh(high))
    p0 = model.marginal_price(x0, y0)
    p1 = model.marginal_price(x1, y1)
    return abs(math.log(p1 / p0) / math.log(x1 / x0))


def _one_sided_log_curvature(model, v: float, step: float, side: str) -> float:
    low, high = (v - step, v) if side == "left" else (v, v + step)
    x0, y0 = model.coordinates(2.0, math.asinh(max(low, 0.0)))
    x1, y1 = model.coordinates(2.0, math.asinh(high))
    p0 = model.marginal_price(x0, y0)
    p1 = model.marginal_price(x1, y1)
    return abs(math.log(p1 / p0) / math.log(x1 / x0))


def bridge_sensitivity_rows() -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for profile in PROFILES:
        curve = CryptoSwap(float(profile["peak_depth"]), float(profile["fade_scale"]))
        curve_execution = [execution_price_premium_bps(curve, size) for size in TRADE_SIZES_BPS]
        curve_jump = [jump_loss_bps(curve, jump)[0] for jump in UPWARD_JUMP_FACTORS]
        for fraction in BRIDGE_FRACTIONS:
            model = DuskC1(
                float(profile["peak_depth"]),
                float(profile["fade_scale"]),
                bridge_start_fraction=fraction,
            )
            geometry = model.bridge_geometry()
            join_step = max(geometry.length * 1e-5, geometry.v_start * 1e-6, 1e-12)
            inner_join_curvature = _one_sided_log_curvature(
                model, geometry.v_start, join_step, "left"
            )
            bridge_join_curvature = _one_sided_log_curvature(
                model, geometry.v_start, join_step, "right"
            )
            execution = [execution_price_premium_bps(model, size) for size in TRADE_SIZES_BPS]
            jump = [jump_loss_bps(model, factor)[0] for factor in UPWARD_JUMP_FACTORS]
            curvature_samples = [
                _log_curvature(
                    model,
                    geometry.v_start + geometry.length * index / 100.0,
                    max(geometry.length * 1e-4, 1e-12),
                )
                for index in range(1, 100)
            ]
            rows.append(
                {
                    "profile": profile["profile"],
                    "bridge_start_fraction": fraction,
                    "v_start": geometry.v_start,
                    "v_tail": geometry.v_tail,
                    "transition_width_v": geometry.length,
                    "stress_grid_mean_execution_premium_bps": sum(execution) / len(execution),
                    "stress_grid_mean_cryptoswap_execution_premium_bps": sum(curve_execution) / len(curve_execution),
                    "stress_grid_mean_one_jump_loss_bps": sum(jump) / len(jump),
                    "stress_grid_mean_cryptoswap_one_jump_loss_bps": sum(curve_jump) / len(curve_jump),
                    "stress_grid_mean_jump_loss_reduction_vs_cryptoswap_bps": (
                        sum(curve_jump) - sum(jump)
                    )
                    / len(jump),
                    "stress_grid_mean_execution_premium_increase_vs_cryptoswap_bps": (
                        sum(execution) - sum(curve_execution)
                    )
                    / len(execution),
                    "maximum_absolute_log_price_curvature": max(curvature_samples),
                    "inner_join_absolute_log_price_curvature": inner_join_curvature,
                    "bridge_join_absolute_log_price_curvature": bridge_join_curvature,
                    "inner_join_curvature_ratio": (
                        bridge_join_curvature / inner_join_curvature
                        if inner_join_curvature > 0.0
                        else math.inf
                    ),
                }
            )
    return rows


def safety_rows() -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    sizes = (1, 10, 100, 1_000, 2_000, 10_000)
    for peak_depth in SAFETY_PEAK_DEPTHS:
        for fade_scale in SAFETY_FADE_SCALES:
            for bridge_fraction in BRIDGE_FRACTIONS:
                curve = CryptoSwap(peak_depth, fade_scale)
                model = DuskC1(
                    peak_depth=peak_depth,
                    fade_scale=fade_scale,
                    bridge_start_fraction=bridge_fraction,
                )
                cpmm = Cpmm()
                geometry = model.bridge_geometry()
                points = sorted(
                    set(
                        [geometry.v_start * index / 100.0 for index in range(101)]
                        + [
                            geometry.v_start + geometry.length * index / 200.0
                            for index in range(201)
                        ]
                        + [geometry.v_tail * multiplier for multiplier in (1.01, 1.1, 2, 10, 100)]
                    )
                )
                minimum_x_derivative = math.inf
                minimum_y_derivative_magnitude = math.inf
                maximum_price_increase = 0.0
                maximum_symmetry_error = 0.0
                previous_price = 1.0
                for v in points:
                    q, q_prime, _ = model.q_and_prime(v)
                    cosh = math.sqrt(1.0 + v * v)
                    x_derivative = q_prime / (2.0 * q) + 1.0 / cosh
                    y_derivative = q_prime / (2.0 * q) - 1.0 / cosh
                    minimum_x_derivative = min(minimum_x_derivative, x_derivative)
                    minimum_y_derivative_magnitude = min(
                        minimum_y_derivative_magnitude, -y_derivative
                    )
                    x, y = model.coordinates(2.0, math.asinh(v))
                    price = model.marginal_price(x, y)
                    reverse = model.marginal_price(y, x)
                    maximum_symmetry_error = max(
                        maximum_symmetry_error, abs(price * reverse - 1.0)
                    )
                    maximum_price_increase = max(
                        maximum_price_increase, price - previous_price
                    )
                    previous_price = price

                maximum_quote_order_error = 0.0
                maximum_roundtrip_error = 0.0
                maximum_split_error = 0.0
                maximum_offcenter_invariant_error = 0.0
                maximum_offcenter_roundtrip_error = 0.0
                maximum_offcenter_split_error = 0.0
                offcenter_solver_failures = 0
                for size in sizes:
                    amount = 2.0 * size / 10_000.0
                    cpmm_quote = cpmm.quote_exact_in(1.0, 1.0, amount)
                    dusk_quote = model.quote_exact_in(1.0, 1.0, amount)
                    curve_quote = curve.quote_exact_in(1.0, 1.0, amount)
                    maximum_quote_order_error = max(
                        maximum_quote_order_error,
                        cpmm_quote.amount_out - dusk_quote.amount_out,
                        dusk_quote.amount_out - curve_quote.amount_out,
                    )
                    exact_out = model.quote_exact_out(1.0, 1.0, dusk_quote.amount_out)
                    maximum_roundtrip_error = max(
                        maximum_roundtrip_error,
                        abs(exact_out.amount_in / amount - 1.0),
                    )
                    first = model.quote_exact_in(1.0, 1.0, amount / 2.0)
                    second = model.quote_exact_in(
                        first.reserve_in_after,
                        first.reserve_out_after,
                        amount / 2.0,
                    )
                    maximum_split_error = max(
                        maximum_split_error,
                        abs(
                            (first.amount_out + second.amount_out)
                            / dusk_quote.amount_out
                            - 1.0
                        ),
                    )

                state_points = (
                    max(geometry.v_start * 0.5, 1e-12),
                    (geometry.v_start + geometry.v_tail) / 2.0,
                    max(geometry.v_tail * 2.0, 2.0),
                    200.0,
                )
                for v in state_points:
                    for signed_v in (v, -v):
                        x, y = model.coordinates(2.0, math.asinh(signed_v))
                        amounts = [x * 1e-4, x * 0.1]
                        if x < 1.0:
                            amounts.append(1.0 - x)
                        for amount in amounts:
                            try:
                                whole = model.quote_exact_in(x, y, amount)
                                invariant_after = model.invariant(
                                    whole.reserve_in_after,
                                    whole.reserve_out_after,
                                )
                                maximum_offcenter_invariant_error = max(
                                    maximum_offcenter_invariant_error,
                                    abs(invariant_after / 2.0 - 1.0),
                                )
                                inverse = model.quote_exact_out(x, y, whole.amount_out)
                                maximum_offcenter_roundtrip_error = max(
                                    maximum_offcenter_roundtrip_error,
                                    abs(inverse.amount_in / amount - 1.0),
                                )
                                first = model.quote_exact_in(x, y, amount / 2.0)
                                second = model.quote_exact_in(
                                    first.reserve_in_after,
                                    first.reserve_out_after,
                                    amount / 2.0,
                                )
                                maximum_offcenter_split_error = max(
                                    maximum_offcenter_split_error,
                                    abs(
                                        (first.amount_out + second.amount_out)
                                        / whole.amount_out
                                        - 1.0
                                    ),
                                )
                            except (ArithmeticError, ValueError):
                                offcenter_solver_failures += 1

                q_left, q_prime_left, _ = model.q_and_prime(
                    math.nextafter(geometry.v_start, 0.0)
                )
                q_right, q_prime_right, _ = model.q_and_prime(geometry.v_start)
                tail_left_q, tail_left_prime, _ = model.q_and_prime(
                    math.nextafter(geometry.v_tail, 0.0)
                )
                tail_right_q, tail_right_prime, _ = model.q_and_prime(geometry.v_tail)
                join_error = max(
                    abs(q_left - q_right),
                    abs(q_prime_left - q_prime_right),
                    abs(tail_left_q - tail_right_q),
                    abs(tail_left_prime - tail_right_prime),
                )
                passed = (
                    minimum_x_derivative > 0.0
                    and minimum_y_derivative_magnitude > 0.0
                    and maximum_price_increase <= 2e-8
                    and maximum_symmetry_error <= 2e-10
                    and maximum_quote_order_error <= 2e-10
                    and maximum_roundtrip_error <= 2e-9
                    and maximum_split_error <= 2e-9
                    and maximum_offcenter_invariant_error <= 2e-9
                    and maximum_offcenter_roundtrip_error <= 2e-8
                    and maximum_offcenter_split_error <= 2e-8
                    and offcenter_solver_failures == 0
                    and join_error <= 2e-10
                )
                rows.append(
                    {
                        "peak_depth": peak_depth,
                        "fade_scale": fade_scale,
                        "bridge_start_fraction": bridge_fraction,
                        "passed": passed,
                        "minimum_x_log_derivative": minimum_x_derivative,
                        "minimum_negative_y_log_derivative": minimum_y_derivative_magnitude,
                        "maximum_outward_price_increase": maximum_price_increase,
                        "maximum_symmetry_error": maximum_symmetry_error,
                        "maximum_quote_order_error": maximum_quote_order_error,
                        "maximum_exact_in_out_duality_error": maximum_roundtrip_error,
                        "maximum_split_trade_error": maximum_split_error,
                        "maximum_offcenter_invariant_error": maximum_offcenter_invariant_error,
                        "maximum_offcenter_exact_in_out_duality_error": maximum_offcenter_roundtrip_error,
                        "maximum_offcenter_split_trade_error": maximum_offcenter_split_error,
                        "offcenter_solver_failures": offcenter_solver_failures,
                        "maximum_join_value_or_slope_error": join_error,
                    }
                )
    return rows


def summarize(
    execution: list[dict[str, object]],
    jumps: list[dict[str, object]],
    tails: list[dict[str, object]],
    recenter: list[dict[str, object]],
    path_lvr: list[dict[str, object]],
    restoring: list[dict[str, object]],
    pareto: list[dict[str, object]],
    service_constrained: list[dict[str, object]],
    solvers: list[dict[str, object]],
    bridge: list[dict[str, object]],
    safety: list[dict[str, object]],
) -> dict[str, object]:
    def rows_for(rows, **filters):
        return [
            row
            for row in rows
            if all(row[key] == value for key, value in filters.items())
        ]

    profile_results: dict[str, object] = {}
    for profile in PROFILES:
        profile_id = str(profile["profile"])
        curve_jump = rows_for(jumps, profile=profile_id, model="cryptoswap")
        dusk_jump = rows_for(jumps, profile=profile_id, model="dusk_c1")
        cpmm_jump = rows_for(jumps, profile=profile_id, model="cpmm")
        curve_execution = rows_for(execution, profile=profile_id, model="cryptoswap")
        dusk_execution = rows_for(execution, profile=profile_id, model="dusk_c1")
        cpmm_execution = rows_for(execution, profile=profile_id, model="cpmm")
        profile_results[profile_id] = {
            "stress_grid_mean_one_jump_loss_bps": {
                "cryptoswap": sum(float(row["one_jump_arbitrage_loss_bps_of_initial_tvl"]) for row in curve_jump)
                / len(curve_jump),
                "dusk_c1": sum(float(row["one_jump_arbitrage_loss_bps_of_initial_tvl"]) for row in dusk_jump)
                / len(dusk_jump),
                "cpmm": sum(float(row["one_jump_arbitrage_loss_bps_of_initial_tvl"]) for row in cpmm_jump)
                / len(cpmm_jump),
            },
            "stress_grid_mean_execution_premium_bps": {
                "cryptoswap": sum(float(row["average_execution_price_premium_bps"]) for row in curve_execution)
                / len(curve_execution),
                "dusk_c1": sum(float(row["average_execution_price_premium_bps"]) for row in dusk_execution)
                / len(dusk_execution),
                "cpmm": sum(float(row["average_execution_price_premium_bps"]) for row in cpmm_execution)
                / len(cpmm_execution),
            },
            "warning": "Unweighted stress-grid index; not expected economics or a scenario-weighted mean",
        }

    balanced_outward_quote_order_holds = all(
        float(row["maximum_quote_order_error"]) <= 2e-10 for row in safety
    )
    safety_passed = all(bool(row["passed"]) for row in safety)
    dusk_tail_rows = [
        row
        for row in tails
        if row["model"] == "dusk_c1" and row["curve_region"] == "tail"
    ]
    curve_supported_tail = [
        row
        for row in tails
        if row["model"] == "cryptoswap" and float(row["x_over_d"]) <= 200.0
    ]
    dusk_zero_impairment = [
        row
        for row in recenter
        if row["model"] == "dusk_c1"
        and row["reserve_basis"] == "native_post_arbitrage"
        and bool(row["same_exact_tail"])
    ]
    exact_zero_count = sum(
        abs(float(row["protected_q_impairment_bps"])) <= 1e-9
        for row in dusk_zero_impairment
    )
    curve_tail_solver = [
        row for row in solvers if row["model"] == "cryptoswap" and row["state"] == "tail"
    ]
    dusk_tail_solver = [
        row for row in solvers if row["model"] == "dusk_c1" and row["state"] == "tail"
    ]

    def one(rows, **filters):
        matches = rows_for(rows, **filters)
        if len(matches) != 1:
            raise RuntimeError(f"expected one row for {filters}, found {len(matches)}")
        return matches[0]

    decision_examples: dict[str, object] = {}
    for profile in PROFILES:
        profile_id = str(profile["profile"])
        execution_examples: dict[str, object] = {}
        for size in (10, 100, 1_000, 2_000):
            execution_examples[str(size)] = {
                model: float(
                    one(
                        execution,
                        profile=profile_id,
                        model=model,
                        trade_size_bps_of_initial_tvl=size,
                    )["average_execution_price_premium_bps"]
                )
                for model in ("cpmm", "cryptoswap", "dusk_c1")
            }
        jump_example = {
            model: float(
                one(
                    jumps,
                    profile=profile_id,
                    model=model,
                    external_price_multiple=2.0,
                )["one_jump_arbitrage_loss_bps_of_initial_tvl"]
            )
            for model in ("cpmm", "cryptoswap", "dusk_c1")
        }
        cpmm_loss = jump_example["cpmm"]
        curve_excess = jump_example["cryptoswap"] - cpmm_loss
        dusk_excess = jump_example["dusk_c1"] - cpmm_loss
        recenter_example = {
            model: float(
                one(
                    recenter,
                    profile=profile_id,
                    model=model,
                    reserve_basis="native_post_arbitrage",
                    external_price_multiple=2.0,
                    center_movement_fraction_of_log_gap=0.1,
                )["protected_q_impairment_bps"]
            )
            for model in ("cpmm", "cryptoswap", "dusk_c1")
        }
        restoring_example = {
            model: float(
                one(
                    restoring,
                    profile=profile_id,
                    model=model,
                    starting_price_multiple=2.0,
                    restoring_fraction_of_log_price_gap=1.0,
                )["output_difference_vs_same_reserve_cpmm_bps"]
            )
            for model in ("cpmm", "cryptoswap", "dusk_c1")
        }
        decision_examples[profile_id] = {
            "execution_premium_bps_by_trade_size": execution_examples,
            "one_jump_2x_loss_bps_initial_tvl": jump_example,
            "fraction_of_cryptoswap_excess_loss_removed_by_dusk": (
                1.0 - dusk_excess / curve_excess if curve_excess > 0.0 else 0.0
            ),
            "ten_percent_recenter_protected_q_impairment_bps": recenter_example,
            "full_restore_from_2x_output_difference_vs_same_reserve_cpmm_bps": restoring_example,
        }

    path_examples = {
        profile_id: {
            scenario: {
                model: float(
                    one(
                        path_lvr,
                        profile=profile_id,
                        model=model,
                        scenario=scenario,
                    )["cumulative_path_lvr_bps_of_initial_tvl"]
                )
                for model in ("cpmm", "cryptoswap", "dusk_c1")
            }
            for scenario in SYNTHETIC_PATHS
        }
        for profile_id in ("broad", "balanced", "aggressive")
    }

    return {
        "status": "complete" if safety_passed else "rejected",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "decision_scope": "Invariant economics and algorithmic work only; actual SBF CU remains unmeasured",
        "benchmark_configuration": {
            "profiles": list(PROFILES),
            "trade_sizes_bps_of_initial_tvl": list(TRADE_SIZES_BPS),
            "jump_price_multiples": list(JUMP_FACTORS),
            "tail_x_over_d": list(TAIL_X_OVER_D),
            "center_move_fractions_of_log_gap": list(CENTER_MOVE_FRACTIONS),
            "bridge_fractions_swept": list(BRIDGE_FRACTIONS),
            "selected_bridge_fraction": SELECTED_BRIDGE_FRACTION,
            "service_search": {
                "scope": "balanced-start outward exact-in at 10/100/1000/2000 bps of initial TVL",
                "peak_depth_min": SERVICE_PEAK_DEPTH_MIN,
                "peak_depth_max": SERVICE_PEAK_DEPTH_MAX,
                "peak_depth_log_points": SERVICE_PEAK_DEPTH_POINTS,
                "fade_scale_min": SERVICE_FADE_SCALE_MIN,
                "fade_scale_max": SERVICE_FADE_SCALE_MAX,
                "fade_scale_geometric_bisection_iterations": SERVICE_FADE_BISECTION_ITERATIONS,
                "feasibility_tolerance_bps": SERVICE_FEASIBILITY_TOLERANCE_BPS,
                "qualification": "bounded tested envelope, not an unbounded global optimum",
            },
            "synthetic_paths": {key: list(value) for key, value in SYNTHETIC_PATHS.items()},
            "random_seed": None,
            "network_required": False,
        },
        "parameter_identity": {
            "equation": "peak_depth = 2 * Curve A; fade_scale = Curve gamma",
            "matching_required": False,
            "reason": "The Dusk and CryptoSwap inner equations are algebraically identical",
        },
        "selected_protocol_bridge_start_fraction": SELECTED_BRIDGE_FRACTION,
        "bridge_selection_status": "provisional pending fixed-point SBF validation",
        "safety": {
            "tested_parameter_cells": len(safety),
            "failed_parameter_cells": sum(not bool(row["passed"]) for row in safety),
            "all_passed": safety_passed,
            "balanced_outward_quote_order_cpmm_le_dusk_le_cryptoswap": balanced_outward_quote_order_holds,
            "global_quote_order_claimed": False,
            "maximum_join_error": max(float(row["maximum_join_value_or_slope_error"]) for row in safety),
            "maximum_split_error": max(float(row["maximum_split_trade_error"]) for row in safety),
            "maximum_offcenter_split_error": max(
                float(row["maximum_offcenter_split_trade_error"]) for row in safety
            ),
            "maximum_offcenter_exact_in_out_error": max(
                float(row["maximum_offcenter_exact_in_out_duality_error"]) for row in safety
            ),
            "offcenter_solver_failures": sum(int(row["offcenter_solver_failures"]) for row in safety),
        },
        "profile_results": profile_results,
        "decision_examples": decision_examples,
        "path_lvr_examples": path_examples,
        "tail": {
            "dusk_exact_tail_local_exponent_min": min(float(row["local_exponent"]) for row in dusk_tail_rows),
            "dusk_exact_tail_local_exponent_max": max(float(row["local_exponent"]) for row in dusk_tail_rows),
            "dusk_tail_product_level": "xy = D^2 * (1 - fade_scale) / 4",
            "qualification": "Exact CPMM shape on a reduced product level, not the original balanced CPMM product",
            "cryptoswap_reachable_local_exponent_max_x_over_d_le_200": max(
                float(row["local_exponent"]) for row in curve_supported_tail
            ),
            "cryptoswap_x2y_is_asymptotic_not_reachable_default": True,
        },
        "recenter": {
            "dusk_same_tail_cases": len(dusk_zero_impairment),
            "dusk_same_tail_exact_zero_impairment_cases": exact_zero_count,
            "metric_scope": "Protected-Q impairment only; excludes subsequent arbitrage, fees, funding, and controller timing",
        },
        "restoring": {
            "minimum_dusk_output_difference_vs_same_reserve_cpmm_bps": min(
                float(row["output_difference_vs_same_reserve_cpmm_bps"])
                for row in restoring
                if row["model"] == "dusk_c1"
            ),
            "structural_tail_rebuild": "A full exact-tail restore to D=2 center withholds exactly fade_scale quote units",
        },
        "pareto": {
            "parameter_cells": len(pareto),
            "multi_size_frontier_counts": {
                model: sum(
                    bool(row["pareto_multi_size_execution_vs_2x_loss"])
                    for row in pareto
                    if row["model"] == model
                )
                for model in ("cryptoswap", "dusk_c1")
            },
            "service_constrained_curve_matches": service_constrained,
            "interpretation": "Within the sampled multi-size frontier, neither family dominates; Dusk adds deep-center/safe-tail points while CryptoSwap retains stronger medium/large execution points",
        },
        "solver_proxy": {
            "reference_curve_tail_mean_nested_iterations": sum(
                float(row["total_nonlinear_iterations_proxy"]) for row in curve_tail_solver
            )
            / len(curve_tail_solver),
            "reference_dusk_tail_mean_nested_iterations": sum(
                float(row["total_nonlinear_iterations_proxy"]) for row in dusk_tail_solver
            )
            / len(dusk_tail_solver),
            "sampled_dusk_same_tail_both_directions_and_quote_modes_all_exact_fast_path": all(
                bool(row["exact_tail_fast_path"]) for row in dusk_tail_solver
            ),
            "actual_compute_units": None,
            "warning": "Reference-algorithm counts cannot rank production CU",
        },
        "bridge_sensitivity": bridge,
        "recommendation": (
            "For Dusk's long-tail target, advance the finite-tail C1 candidate to a standalone Q48/u128 SBF prototype. "
            "Do not claim universal superiority or alter production until restoring-flow, fixed-point, and CU gates pass in Rust."
            if safety_passed and balanced_outward_quote_order_holds
            else "Reject the current Dusk C1 candidate before production work."
        ),
        "source_hashes": {
            "models.py": _sha256(ROOT / "models.py"),
            "benchmark.py": _sha256(ROOT / "benchmark.py"),
            "test_models.py": _sha256(ROOT / "test_models.py"),
            "validate_results.py": _sha256(ROOT / "validate_results.py"),
            "current_dusk_concentration_spec": _sha256(REPOSITORY / "programs/dusk/CONCENTRATION.md"),
            "current_dusk_concentrated_math": _sha256(REPOSITORY / "programs/dusk/src/math/concentrated.rs"),
            "legacy_curve_fixture": _sha256(
                REPOSITORY / "analysis/cryptoswap-vs-dh2/data/curve_v2_1_validation.json"
            ),
        },
    }


def main() -> None:
    RESULTS.mkdir(parents=True, exist_ok=True)
    execution = execution_rows()
    jumps = jump_rows()
    tails = tail_rows()
    recenter = recenter_rows()
    path_lvr = path_lvr_rows()
    restoring = restoring_rows()
    pareto = pareto_rows()
    service_constrained = service_constrained_rows()
    solvers = solver_rows()
    bridge = bridge_sensitivity_rows()
    safety = safety_rows()
    _write_csv(RESULTS / "execution.csv", execution)
    _write_csv(RESULTS / "jump_loss.csv", jumps)
    _write_csv(RESULTS / "tail_behavior.csv", tails)
    _write_csv(RESULTS / "recenter_impairment.csv", recenter)
    _write_csv(RESULTS / "path_lvr.csv", path_lvr)
    _write_csv(RESULTS / "restoring_quotes.csv", restoring)
    _write_csv(RESULTS / "pareto_sweep.csv", pareto)
    _write_csv(RESULTS / "service_constrained_matches.csv", service_constrained)
    _write_csv(RESULTS / "solver_proxy.csv", solvers)
    _write_csv(RESULTS / "bridge_sensitivity.csv", bridge)
    _write_csv(RESULTS / "safety_grid.csv", safety)
    summary = summarize(
        execution,
        jumps,
        tails,
        recenter,
        path_lvr,
        restoring,
        pareto,
        service_constrained,
        solvers,
        bridge,
        safety,
    )
    _write_json(RESULTS / "summary.json", summary)
    print(json.dumps({
        "status": summary["status"],
        "safety_cells": summary["safety"]["tested_parameter_cells"],
        "failed_cells": summary["safety"]["failed_parameter_cells"],
        "results": str(RESULTS),
    }, indent=2))


if __name__ == "__main__":
    main()

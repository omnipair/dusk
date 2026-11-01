#!/usr/bin/env python3
"""Illustrative expected-loss pricing for a Dusk SOL/USDC market.

The model is intentionally dependency-free.  It bootstraps sealed 12-hour
SOLUSDT return windows, applies Dusk's current CPMM liquidation valuation,
and compares a pair spread, a marginal action fee, and Shapley-allocated
position premiums.  It is a research model, not a production rate oracle.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import itertools
import json
import math
import random
import statistics
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable


HERE = Path(__file__).resolve().parent
DEFAULT_HISTORY = HERE.parent / "cryptoswap-vs-dh2" / "data" / "solusdt_sealed_native_windows.csv"
EXPECTED_HISTORY_SHA256 = "e7e6078f81eab5151fbd39343d6b67ba7644dda185fbebc4f729c72ce717fa05"

BPS = 10_000
LIQUIDATION_CF_BPS = 8_500
MAX_BORROW_CF_BPS = LIQUIDATION_CF_BPS * 9_500 // BPS
LIQUIDATION_HEALTH_FLOOR_BPS = math.ceil(BPS * BPS / LIQUIDATION_CF_BPS)
MIN_INCENTIVE_BPS = 100
MAX_INCENTIVE_BPS = 500
INSURANCE_FUNDING_BPS = 200
PRICE_SAFETY_PPM = 500
Q_HAIRCUT_PPM = 25


@dataclass(frozen=True)
class MarketConfig:
    initial_price_usdc_per_sol: float = 150.0
    base_reserve_sol: float = 1_000_000.0 / 150.0
    quote_reserve_usdc: float = 1_000_000.0
    position_collateral_sol: float = (1_000_000.0 / 150.0) * 0.05
    horizon_days: int = 7
    capital_return_apr: float = 0.15
    expected_shortfall_quantile: float = 0.99
    path_count: int = 50_000
    seed: int = 42


@dataclass(frozen=True)
class Segment:
    window_id: str
    stratum: str
    source_month: str
    rows: int
    final_log_return: float
    minimum_log_return: float
    final_return_pct: float
    minimum_from_start_pct: float


@dataclass(frozen=True)
class Position:
    name: str
    impact_ltv: float
    collateral_sol: float
    debt_usdc: float


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_segments(path: Path) -> tuple[list[Segment], dict[str, object]]:
    grouped: dict[tuple[str, str, str], list[tuple[int, float]]] = {}
    total_rows = 0
    score_rows = 0
    with path.open(newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        required = {"window_id", "stratum", "phase", "offset_minute", "close", "source_month"}
        missing = required.difference(reader.fieldnames or [])
        if missing:
            raise ValueError(f"history is missing columns: {sorted(missing)}")
        for row in reader:
            total_rows += 1
            if row["phase"] != "score":
                continue
            score_rows += 1
            key = (row["window_id"], row["stratum"], row["source_month"])
            grouped.setdefault(key, []).append((int(row["offset_minute"]), float(row["close"])))

    segments: list[Segment] = []
    for (window_id, stratum, source_month), observations in sorted(grouped.items()):
        observations.sort()
        closes = [value for _, value in observations]
        if len(closes) != 720:
            raise ValueError(f"{window_id} has {len(closes)} score rows; expected 720")
        if min(closes) <= 0:
            raise ValueError(f"{window_id} contains a non-positive price")
        start = closes[0]
        log_returns = [math.log(value / start) for value in closes]
        final_log = log_returns[-1]
        minimum_log = min(log_returns)
        segments.append(
            Segment(
                window_id=window_id,
                stratum=stratum,
                source_month=source_month,
                rows=len(closes),
                final_log_return=final_log,
                minimum_log_return=minimum_log,
                final_return_pct=100.0 * (math.exp(final_log) - 1.0),
                minimum_from_start_pct=100.0 * (math.exp(minimum_log) - 1.0),
            )
        )

    counts: dict[str, int] = {}
    for segment in segments:
        counts[segment.stratum] = counts.get(segment.stratum, 0) + 1
    checksum = sha256_file(path)
    diagnostics = {
        "path": str(path),
        "sha256": checksum,
        "expected_sha256": EXPECTED_HISTORY_SHA256,
        "checksum_matches": checksum == EXPECTED_HISTORY_SHA256,
        "total_rows": total_rows,
        "score_rows": score_rows,
        "segment_count": len(segments),
        "segments_by_stratum": counts,
    }
    return segments, diagnostics


def choose_stratum(rng: random.Random, mixture: dict[str, float]) -> str:
    draw = rng.random()
    cumulative = 0.0
    last = ""
    for name, weight in mixture.items():
        last = name
        cumulative += weight
        if draw <= cumulative:
            return name
    return last


def simulate_minimum_price_ratios(
    segments: Iterable[Segment],
    mixture: dict[str, float],
    config: MarketConfig,
    seed: int,
) -> list[float]:
    by_stratum: dict[str, list[Segment]] = {}
    for segment in segments:
        by_stratum.setdefault(segment.stratum, []).append(segment)
    if not math.isclose(sum(mixture.values()), 1.0, abs_tol=1e-12):
        raise ValueError(f"regime weights must sum to 1, got {sum(mixture.values())}")
    missing = set(mixture).difference(by_stratum)
    if missing:
        raise ValueError(f"missing history strata: {sorted(missing)}")

    rng = random.Random(seed)
    segment_count = math.ceil(config.horizon_days * 24 / 12)
    minima: list[float] = []
    for _ in range(config.path_count):
        level = 0.0
        minimum = 0.0
        for _ in range(segment_count):
            stratum = choose_stratum(rng, mixture)
            segment = rng.choice(by_stratum[stratum])
            minimum = min(minimum, level + segment.minimum_log_return)
            level += segment.final_log_return
        minima.append(math.exp(minimum))
    return minima


def cpmm_impact_value_usdc(
    collateral_sol: float,
    price_ratio: float,
    depth_factor: float,
    config: MarketConfig,
) -> float:
    """Dusk default-CPMM full-collateral exact-in value under conservative state."""
    safe_ratio = max(price_ratio * (1.0 - PRICE_SAFETY_PPM / 1_000_000.0), 1e-12)
    conservative_scale = depth_factor * (1.0 - Q_HAIRCUT_PPM / 1_000_000.0)
    base_reserve = conservative_scale * config.base_reserve_sol / math.sqrt(safe_ratio)
    quote_reserve = conservative_scale * config.quote_reserve_usdc * math.sqrt(safe_ratio)
    return collateral_sol * quote_reserve / (base_reserve + collateral_sol)


def build_positions(config: MarketConfig) -> list[Position]:
    initial_value = cpmm_impact_value_usdc(config.position_collateral_sol, 1.0, 1.0, config)
    return [
        Position("Low LTV", 0.50, config.position_collateral_sol, initial_value * 0.50),
        Position("Mid LTV", 0.65, config.position_collateral_sol, initial_value * 0.65),
        Position("High LTV", 0.80, config.position_collateral_sol, initial_value * 0.80),
    ]


def liquidation_penalty_bps(health_bps: int) -> int:
    shortfall = max(LIQUIDATION_HEALTH_FLOOR_BPS - health_bps, 0)
    max_for_config = min(LIQUIDATION_HEALTH_FLOOR_BPS - (BPS + 1), MAX_INCENTIVE_BPS)
    incentive = min(max(shortfall, MIN_INCENTIVE_BPS), max_for_config)
    max_total_penalty = LIQUIDATION_HEALTH_FLOOR_BPS - (BPS + 1)
    insurance_funding = min(INSURANCE_FUNDING_BPS, max(max_total_penalty - incentive, 0))
    return incentive + insurance_funding


def path_loss_usdc(
    minimum_price_ratio: float,
    depth_factor: float,
    positions: list[Position],
    mask: int,
    config: MarketConfig,
) -> tuple[float, int]:
    triggered: list[tuple[Position, float, int]] = []
    trigger_mask = 0
    for index, position in enumerate(positions):
        if not (mask & (1 << index)):
            continue
        value = cpmm_impact_value_usdc(position.collateral_sol, minimum_price_ratio, depth_factor, config)
        health_bps = math.floor(BPS * value / position.debt_usdc)
        if health_bps <= LIQUIDATION_HEALTH_FLOOR_BPS:
            trigger_mask |= 1 << index
            triggered.append((position, value, liquidation_penalty_bps(health_bps)))
    if not triggered:
        return 0.0, trigger_mask

    total_debt = sum(position.debt_usdc for position, _, _ in triggered)
    total_collateral = sum(position.collateral_sol for position, _, _ in triggered)
    weighted_penalty_bps = sum(
        position.debt_usdc * penalty for position, _, penalty in triggered
    ) / total_debt
    joint_execution_value = cpmm_impact_value_usdc(
        total_collateral, minimum_price_ratio, depth_factor, config
    )
    repay_capacity = joint_execution_value / (1.0 + weighted_penalty_bps / BPS)
    return max(total_debt - repay_capacity, 0.0), trigger_mask


def loss_metrics(losses: list[float], ead: float, config: MarketConfig) -> dict[str, float]:
    if not losses:
        return {
            "expected_loss_usdc": 0.0,
            "expected_shortfall_99_usdc": 0.0,
            "economic_capital_usdc": 0.0,
            "risk_cost_horizon_usdc": 0.0,
            "annual_risk_cost_usdc": 0.0,
            "risk_apr": 0.0,
            "loss_probability": 0.0,
        }
    expected_loss = statistics.fmean(losses)
    tail_count = max(1, math.ceil((1.0 - config.expected_shortfall_quantile) * len(losses)))
    expected_shortfall = statistics.fmean(sorted(losses, reverse=True)[:tail_count])
    economic_capital = max(expected_shortfall - expected_loss, 0.0)
    horizon_years = config.horizon_days / 365.0
    risk_cost_horizon = expected_loss + config.capital_return_apr * horizon_years * economic_capital
    annual_risk_cost = expected_loss / horizon_years + config.capital_return_apr * economic_capital
    return {
        "expected_loss_usdc": expected_loss,
        "expected_shortfall_99_usdc": expected_shortfall,
        "economic_capital_usdc": economic_capital,
        "risk_cost_horizon_usdc": risk_cost_horizon,
        "annual_risk_cost_usdc": annual_risk_cost,
        "risk_apr": annual_risk_cost / ead if ead else 0.0,
        "loss_probability": sum(loss > 0 for loss in losses) / len(losses),
    }


def coalition_metrics(
    minima: list[float],
    depth_factor: float,
    positions: list[Position],
    config: MarketConfig,
) -> tuple[dict[int, dict[str, float]], dict[int, float]]:
    metrics: dict[int, dict[str, float]] = {}
    trigger_probabilities = {index: 0.0 for index in range(len(positions))}
    full_mask = (1 << len(positions)) - 1
    full_trigger_counts = [0] * len(positions)
    for mask in range(1 << len(positions)):
        ead = sum(position.debt_usdc for index, position in enumerate(positions) if mask & (1 << index))
        losses: list[float] = []
        for minimum in minima:
            loss, trigger_mask = path_loss_usdc(minimum, depth_factor, positions, mask, config)
            losses.append(loss)
            if mask == full_mask:
                for index in range(len(positions)):
                    if trigger_mask & (1 << index):
                        full_trigger_counts[index] += 1
        metrics[mask] = loss_metrics(losses, ead, config)
    for index, count in enumerate(full_trigger_counts):
        trigger_probabilities[index] = count / len(minima)
    return metrics, trigger_probabilities


def shapley_allocations(
    coalition: dict[int, dict[str, float]], positions: list[Position]
) -> list[float]:
    n = len(positions)
    denominator = math.factorial(n)
    allocations: list[float] = []
    for index in range(n):
        allocation = 0.0
        for mask in range(1 << n):
            if mask & (1 << index):
                continue
            size = bin(mask).count("1")
            weight = math.factorial(size) * math.factorial(n - size - 1) / denominator
            with_position = mask | (1 << index)
            allocation += weight * (
                coalition[with_position]["annual_risk_cost_usdc"]
                - coalition[mask]["annual_risk_cost_usdc"]
            )
        allocations.append(allocation)
    return allocations


def scenario_analysis(
    scenario_name: str,
    mixture: dict[str, float],
    minima: list[float],
    depth_factor: float,
    positions: list[Position],
    config: MarketConfig,
) -> tuple[dict[str, object], list[dict[str, object]], list[dict[str, object]], dict[int, dict[str, float]]]:
    coalition, trigger_probabilities = coalition_metrics(minima, depth_factor, positions, config)
    full_mask = (1 << len(positions)) - 1
    full = coalition[full_mask]
    ead = sum(position.debt_usdc for position in positions)
    any_trigger_count = 0
    for minimum in minima:
        _, trigger_mask = path_loss_usdc(minimum, depth_factor, positions, full_mask, config)
        any_trigger_count += bool(trigger_mask)
    allocations = shapley_allocations(coalition, positions)

    low_mid_mask = 0b011
    marginal_annual_cost = (
        coalition[full_mask]["annual_risk_cost_usdc"]
        - coalition[low_mid_mask]["annual_risk_cost_usdc"]
    )
    high_debt = positions[2].debt_usdc
    horizon_years = config.horizon_days / 365.0
    marginal_fee = max(marginal_annual_cost * horizon_years / high_debt, 0.0)

    scenario_id = f"{scenario_name}_{round(depth_factor * 100):03d}pct_depth"
    scenario_row: dict[str, object] = {
        "scenario_id": scenario_id,
        "regime_mix": scenario_name,
        "depth_label": f"{round(depth_factor * 100)}%",
        "depth_factor": depth_factor,
        "calm_weight": mixture["calm"],
        "typical_weight": mixture["typical"],
        "stress_weight": mixture["stress"],
        "paths": len(minima),
        "horizon_days": config.horizon_days,
        "ead_usdc": ead,
        "probability_any_liquidation": any_trigger_count / len(minima),
        "probability_lender_shortfall": full["loss_probability"],
        "expected_loss_usdc": full["expected_loss_usdc"],
        "expected_loss_bps_of_ead": BPS * full["expected_loss_usdc"] / ead,
        "expected_shortfall_99_usdc": full["expected_shortfall_99_usdc"],
        "expected_shortfall_99_bps_of_ead": BPS * full["expected_shortfall_99_usdc"] / ead,
        "economic_capital_usdc": full["economic_capital_usdc"],
        "pair_risk_apr": full["risk_apr"],
        "pair_risk_apr_bps": BPS * full["risk_apr"],
        "marginal_high_ltv_fee": marginal_fee,
        "marginal_high_ltv_fee_bps": BPS * marginal_fee,
        "marginal_high_ltv_annualized_bps": BPS * marginal_annual_cost / high_debt,
        "minimum_path_price_ratio_p01": sorted(minima)[max(0, math.floor(0.01 * len(minima)) - 1)],
        "minimum_path_price_ratio_median": statistics.median(minima),
    }

    position_rows: list[dict[str, object]] = []
    for index, (position, allocation) in enumerate(zip(positions, allocations)):
        position_rows.append(
            {
                "scenario_id": scenario_id,
                "regime_mix": scenario_name,
                "depth_label": f"{round(depth_factor * 100)}%",
                "depth_factor": depth_factor,
                "position": position.name,
                "impact_ltv": position.impact_ltv,
                "impact_ltv_pct": 100.0 * position.impact_ltv,
                "collateral_sol": position.collateral_sol,
                "debt_usdc": position.debt_usdc,
                "liquidation_probability": trigger_probabilities[index],
                "allocated_annual_risk_cost_usdc": allocation,
                "position_risk_apr": allocation / position.debt_usdc,
                "position_risk_apr_bps": BPS * allocation / position.debt_usdc,
                "pair_risk_apr_bps": BPS * full["risk_apr"],
                "cross_subsidy_bps": BPS * (full["risk_apr"] - allocation / position.debt_usdc),
            }
        )

    structure_rows = [
        {
            "scenario_id": scenario_id,
            "pricing_structure": "Pair-average rolling spread",
            "quote": f"{100.0 * full['risk_apr']:.2f}% APR for every position",
            "quote_bps": BPS * full["risk_apr"],
            "prices": "Average expected shortfall and shared depth by debt side",
            "accounting_fit": "Fits today's one shared quote-debt index",
        },
        {
            "scenario_id": scenario_id,
            "pricing_structure": "Marginal risk-increasing action fee",
            "quote": f"{100.0 * marginal_fee:.2f}% upfront on the high-LTV debt",
            "quote_bps": BPS * marginal_fee,
            "prices": "Incremental portfolio loss from adding the high-LTV position",
            "accounting_fit": "Can be charged on borrow or collateral withdrawal",
        },
        {
            "scenario_id": scenario_id,
            "pricing_structure": "Position-level rolling premium",
            "quote": " / ".join(
                f"{position.name}: {100.0 * allocation / position.debt_usdc:.2f}%"
                for position, allocation in zip(positions, allocations)
            ),
            "quote_bps": None,
            "prices": "Shapley share of expected loss and joint depth externality",
            "accounting_fit": "Needs separate premium debt or position fee accrual",
        },
    ]
    return scenario_row, position_rows, structure_rows, coalition


def write_csv(path: Path, rows: list[dict[str, object]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not rows:
        raise ValueError(f"cannot write empty CSV: {path}")
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def run_analysis(history_path: Path, output_dir: Path, config: MarketConfig) -> dict[str, object]:
    segments, history_diagnostics = load_segments(history_path)
    positions = build_positions(config)
    mixtures = {
        "mostly_calm": {"calm": 0.80, "typical": 0.18, "stress": 0.02},
        "illustrative_base": {"calm": 0.55, "typical": 0.35, "stress": 0.10},
        "stress_heavy": {"calm": 0.25, "typical": 0.50, "stress": 0.25},
    }
    depth_factors = [1.00, 0.75, 0.50]
    minima_by_mix = {
        name: simulate_minimum_price_ratios(segments, mixture, config, config.seed + offset * 10_000)
        for offset, (name, mixture) in enumerate(mixtures.items())
    }

    scenario_rows: list[dict[str, object]] = []
    position_rows: list[dict[str, object]] = []
    structure_rows: list[dict[str, object]] = []
    coalitions: dict[str, dict[int, dict[str, float]]] = {}
    for name, mixture in mixtures.items():
        for depth_factor in depth_factors:
            scenario, positions_for_scenario, structures, coalition = scenario_analysis(
                name,
                mixture,
                minima_by_mix[name],
                depth_factor,
                positions,
                config,
            )
            scenario_rows.append(scenario)
            position_rows.extend(positions_for_scenario)
            structure_rows.extend(structures)
            coalitions[str(scenario["scenario_id"])] = coalition

    central_id = "illustrative_base_075pct_depth"
    central = next(row for row in scenario_rows if row["scenario_id"] == central_id)
    central_positions = [row for row in position_rows if row["scenario_id"] == central_id]

    repeat_minima = simulate_minimum_price_ratios(
        segments,
        mixtures["illustrative_base"],
        config,
        config.seed + 999_999,
    )
    repeat_row, _, _, _ = scenario_analysis(
        "illustrative_base_repeat",
        mixtures["illustrative_base"],
        repeat_minima,
        0.75,
        positions,
        config,
    )

    depth_monotonic = all(
        next(row for row in scenario_rows if row["regime_mix"] == mix and row["depth_factor"] == 0.50)[
            "pair_risk_apr_bps"
        ]
        >= next(row for row in scenario_rows if row["regime_mix"] == mix and row["depth_factor"] == 0.75)[
            "pair_risk_apr_bps"
        ]
        >= next(row for row in scenario_rows if row["regime_mix"] == mix and row["depth_factor"] == 1.00)[
            "pair_risk_apr_bps"
        ]
        for mix in mixtures
    )
    regime_monotonic = all(
        next(row for row in scenario_rows if row["regime_mix"] == "stress_heavy" and row["depth_factor"] == depth)[
            "pair_risk_apr_bps"
        ]
        >= next(row for row in scenario_rows if row["regime_mix"] == "illustrative_base" and row["depth_factor"] == depth)[
            "pair_risk_apr_bps"
        ]
        >= next(row for row in scenario_rows if row["regime_mix"] == "mostly_calm" and row["depth_factor"] == depth)[
            "pair_risk_apr_bps"
        ]
        for depth in depth_factors
    )
    shapley_identity_max_abs_usdc = max(
        abs(
            sum(
                row["allocated_annual_risk_cost_usdc"]
                for row in position_rows
                if row["scenario_id"] == scenario["scenario_id"]
            )
            - coalitions[str(scenario["scenario_id"])][0b111]["annual_risk_cost_usdc"]
        )
        for scenario in scenario_rows
    )
    repeat_difference_bps = abs(
        float(central["pair_risk_apr_bps"]) - float(repeat_row["pair_risk_apr_bps"])
    )
    repeat_relative_difference = repeat_difference_bps / max(float(central["pair_risk_apr_bps"]), 1e-12)
    validation = {
        "history_checksum_matches": history_diagnostics["checksum_matches"],
        "history_has_15_segments": history_diagnostics["segment_count"] == 15,
        "history_has_five_segments_per_stratum": history_diagnostics["segments_by_stratum"]
        == {"calm": 5, "stress": 5, "typical": 5},
        "max_impact_ltv_within_borrow_cap": max(position.impact_ltv for position in positions)
        <= MAX_BORROW_CF_BPS / BPS,
        "risk_apr_monotonic_with_depth_loss": depth_monotonic,
        "risk_apr_monotonic_with_stress_weight": regime_monotonic,
        "shapley_revenue_identity_max_abs_usdc": shapley_identity_max_abs_usdc,
        "shapley_revenue_identity_pass": shapley_identity_max_abs_usdc < 1e-8,
        "central_repeat_pair_apr_difference_bps": repeat_difference_bps,
        "central_repeat_pair_apr_relative_difference": repeat_relative_difference,
        "central_repeat_within_3pct_relative": repeat_relative_difference <= 0.03,
        "all_losses_bounded_by_ead": all(
            float(row["expected_shortfall_99_usdc"]) <= float(row["ead_usdc"]) + 1e-9
            for row in scenario_rows
        ),
    }
    validation["all_checks_pass"] = all(
        bool(value)
        for key, value in validation.items()
        if key.endswith("_matches")
        or key.endswith("_segments")
        or key.endswith("_stratum")
        or key.startswith("max_impact")
        or key.startswith("risk_apr")
        or key.endswith("_pass")
        or key.endswith("_3pct_relative")
        or key.startswith("all_losses")
    )

    segment_rows = [asdict(segment) for segment in segments]
    stratum_profile = []
    for stratum in ("calm", "typical", "stress"):
        selected = [segment for segment in segments if segment.stratum == stratum]
        stratum_profile.append(
            {
                "stratum": stratum,
                "windows": len(selected),
                "median_12h_return_pct": statistics.median(segment.final_return_pct for segment in selected),
                "worst_12h_minimum_from_start_pct": min(
                    segment.minimum_from_start_pct for segment in selected
                ),
                "median_12h_minimum_from_start_pct": statistics.median(
                    segment.minimum_from_start_pct for segment in selected
                ),
            }
        )

    generated_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    summary: dict[str, object] = {
        "generated_at": generated_at,
        "status": "illustrative_not_calibrated",
        "market": "SOL collateral / USDC debt (SOLUSDT used as the return proxy)",
        "model": "7-day historical segment bootstrap plus Dusk default-CPMM gross shortfall",
        "config": asdict(config),
        "protocol_constants": {
            "liquidation_cf_bps": LIQUIDATION_CF_BPS,
            "max_borrow_cf_bps": MAX_BORROW_CF_BPS,
            "liquidation_health_floor_bps": LIQUIDATION_HEALTH_FLOOR_BPS,
            "minimum_liquidation_incentive_bps": MIN_INCENTIVE_BPS,
            "maximum_liquidation_incentive_bps": MAX_INCENTIVE_BPS,
            "insurance_funding_bps": INSURANCE_FUNDING_BPS,
            "price_safety_ppm": PRICE_SAFETY_PPM,
            "q_haircut_ppm": Q_HAIRCUT_PPM,
        },
        "positions": [asdict(position) for position in positions],
        "history_diagnostics": history_diagnostics,
        "central_scenario": central,
        "central_position_pricing": central_positions,
        "monte_carlo_repeat": {
            "seed": config.seed + 999_999,
            "pair_risk_apr_bps": repeat_row["pair_risk_apr_bps"],
            "absolute_difference_bps": repeat_difference_bps,
        },
        "validation": validation,
        "interpretation": {
            "loss_definition": "Gross pool shortfall before drawing any pre-funded USDC insurance reserve.",
            "depth_definition": "A joint liquidation-resale proxy using one conservative Dusk CPMM curve; Dusk auctions transfer collateral to external liquidators rather than swapping it internally.",
            "recovery_timing": "Recovery is valued at the seven-day path minimum after a trigger. This is a deliberately conservative delayed-liquidation stress, not a calibration of Dusk's roughly 21-second auction-floor timing.",
            "prompt_liquidation": "At the liquidation boundary, collateral supports debt plus the configured penalty; lender loss arises from the post-trigger price/depth gap, not from liquidation probability alone.",
            "regime_weights": "Explicit policy scenarios, not frequencies estimated from the deliberately stratified history sample.",
            "fee_label": "The marginal action fee is a Dusk design inference, not a result claimed by the option-pricing papers.",
        },
    }

    output_dir.mkdir(parents=True, exist_ok=True)
    write_csv(output_dir / "segments.csv", segment_rows)
    write_csv(output_dir / "stratum_profile.csv", stratum_profile)
    write_csv(output_dir / "scenario_results.csv", scenario_rows)
    write_csv(output_dir / "position_pricing.csv", position_rows)
    write_csv(output_dir / "pricing_structures.csv", structure_rows)
    (output_dir / "model_summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    (output_dir / "validation.json").write_text(json.dumps(validation, indent=2) + "\n", encoding="utf-8")
    chart_map = {
        "scenario_results.csv": {
            "pair_risk_apr_bps": "Pair-wide expected-loss plus ES99 capital spread by regime mix and depth",
            "marginal_high_ltv_fee_bps": "Upfront marginal risk cost of adding the high-LTV position",
        },
        "position_pricing.csv": {
            "position_risk_apr_bps": "Shapley-allocated position risk APR; filter to the illustrative central scenario"
        },
    }
    (output_dir / "chart_map.json").write_text(json.dumps(chart_map, indent=2) + "\n", encoding="utf-8")
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--history", type=Path, default=DEFAULT_HISTORY)
    parser.add_argument("--output", type=Path, default=HERE / "results")
    parser.add_argument("--paths", type=int, default=50_000)
    parser.add_argument("--seed", type=int, default=42)
    args = parser.parse_args()
    config = MarketConfig(path_count=args.paths, seed=args.seed)
    summary = run_analysis(args.history.resolve(), args.output.resolve(), config)
    central = summary["central_scenario"]
    print(json.dumps({"central_scenario": central, "validation": summary["validation"]}, indent=2))


if __name__ == "__main__":
    main()

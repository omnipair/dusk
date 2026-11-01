#!/usr/bin/env python3
"""Deterministic exhaustive search for Dusk's fee and center controller.

This package deliberately leaves the DH2 invariant fixed.  It searches only
the controller and fee fields already exposed by :class:`ControlledConfig`.
Search output is diagnostic and lives outside the canonical native benchmark
matrix; a winning profile must still be promoted explicitly and survive the
canonical held-out replay before it can affect a product recommendation.

Full mode evaluates every initial profile over the complete synthetic matrix
before ranking.  Quick mode is only a pipeline smoke test and deliberately
uses successive filtering to keep its runtime small.
"""

from __future__ import annotations

import argparse
import csv
from dataclasses import asdict, dataclass, is_dataclass, replace
import hashlib
import json
import math
from pathlib import Path
import statistics
import tempfile
from typing import Mapping, Sequence

from dusk_policy import DynamicFeeConfig, NAD
from economics import ControlledConfig, default_controlled_config
from models import CurrentDh2
from native_benchmark import (
    ACCOUNTING_ERROR_LIMIT,
    FLOW_REGIMES,
    POOL_VALUE,
    PRIMARY_STEP_MINUTES,
    NativeProfile,
    _job,
    _matched_models,
    _percentile,
    _run_jobs,
)
from scenarios import synthetic_paths


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results" / "controller_search"
FULL_OUTPUT = RESULTS / "full"
QUICK_OUTPUT = RESULTS / "quick"
SEARCH_SCHEMA_VERSION = 2
SEARCH_SEEDS = (41, 7)
ROUTING_MODES = ("fixed", "elastic")
CHECKPOINT_BATCH_SIZE = 256


# Product-readiness staleness is a marginal tracking property.  The raw 1%
# quote and excess-over-centered probe remain replay diagnostics, while
# finite-size service continues to be enforced directly by selected execution
# cost.  Keeping this mapping local prevents a comparative Curve/DH2 gate from
# silently changing the Dusk product search.
SEARCH_SERVICE_GUARDRAILS = {
    "median_captured_volume_fraction": ("fraction_min", 0.50),
    "median_selected_execution_cost_bps": ("cost_max", 25.0),
    "p90_selected_execution_cost_bps": ("cost_max", 100.0),
    "median_marginal_tracking_stale_fraction": ("fraction_max", 0.75),
    "worst_regime_median_capture_fraction": ("fraction_min", 0.25),
    "worst_regime_p10_capture_fraction": ("fraction_min", 0.10),
    "worst_regime_median_marginal_tracking_stale_fraction": (
        "fraction_max",
        0.90,
    ),
    "worst_regime_p90_selected_execution_cost_bps": ("cost_max", 150.0),
}


@dataclass(frozen=True)
class ControllerTuple:
    ema_half_life_minutes: float
    adjustment_threshold_bps: float
    adjustment_step_bps: float
    min_adjustment_interval_minutes: int


@dataclass(frozen=True)
class FeeBundle:
    dynamic_cap_bps: int
    divergence_coefficient: float
    volatility_coefficient: float
    volatility_half_life_minutes: float


@dataclass(frozen=True)
class MatrixSpec:
    paths: tuple[str, ...]
    flows: tuple[str, ...]
    seeds: tuple[int, ...]

    @property
    def cells_per_profile(self) -> int:
        return (
            len(self.paths)
            * len(self.flows)
            * len(self.seeds)
            * len(ROUTING_MODES)
        )


# Ten deliberately comprehensible controller tuples.  The final tuple restores
# the slow prior-controller anchor.  Explicit tuples avoid nonsensical products
# of independently swept EMA, threshold, step, and interval values.  Threshold
# is always at least the step.
CONTROLLER_TUPLES = (
    ControllerTuple(1.0, 5.0, 1.0, 1),
    ControllerTuple(1.0, 10.0, 2.5, 1),
    ControllerTuple(3.0, 10.0, 2.5, 1),
    ControllerTuple(3.0, 25.0, 5.0, 1),
    ControllerTuple(3.0, 25.0, 5.0, 5),
    ControllerTuple(10.0, 25.0, 5.0, 1),
    ControllerTuple(10.0, 50.0, 10.0, 5),
    ControllerTuple(10.0, 100.0, 25.0, 5),
    ControllerTuple(30.0, 100.0, 25.0, 5),
    ControllerTuple(60.0, 200.0, 50.0, 5),
)


# The cap is the dynamic surcharge room above the fixed 5 bp base fee.  The
# first ten bundles preserve the original divergence-only and mixed policies.
# The final five add an explicit base-only control and a volatility-only ladder
# matched to the divergence-only cap ladder.  A zero cap is used only when both
# dynamic coefficients are zero, so no policy contains inert dynamic settings.
FEE_BUNDLES = (
    FeeBundle(25, 0.005, 0.000, 15.0),
    FeeBundle(50, 0.010, 0.000, 15.0),
    FeeBundle(100, 0.020, 0.000, 15.0),
    FeeBundle(200, 0.020, 0.000, 60.0),
    FeeBundle(25, 0.005, 0.005, 15.0),
    FeeBundle(50, 0.010, 0.010, 15.0),
    FeeBundle(100, 0.020, 0.010, 30.0),
    FeeBundle(50, 0.005, 0.020, 5.0),
    FeeBundle(50, 0.020, 0.005, 15.0),
    FeeBundle(200, 0.020, 0.020, 60.0),
    FeeBundle(0, 0.000, 0.000, 15.0),
    FeeBundle(25, 0.000, 0.005, 15.0),
    FeeBundle(50, 0.000, 0.010, 15.0),
    FeeBundle(100, 0.000, 0.020, 30.0),
    FeeBundle(200, 0.000, 0.020, 60.0),
)


FULL_MATRIX_SPEC = MatrixSpec(
    (
        "flat",
        "single_jump",
        "jump_reversal",
        "trend",
        "staircase",
        "mean_reversion",
        "alternating_chop",
        "volatility_cluster",
    ),
    tuple(str(flow["flow_regime"]) for flow in FLOW_REGIMES),
    SEARCH_SEEDS,
)


# Full mode has one exhaustive initial grid and an equally complete local pass.
# In particular, no profile is eliminated using a smaller scenario subset.
FULL_STAGE_SPECS = {
    "grid": FULL_MATRIX_SPEC,
    "local": FULL_MATRIX_SPEC,
}


# Smoke mode exercises all search transitions and every initial profile, but
# writes beneath a separate directory and is never decision-grade or canonical.
QUICK_STAGE_SPECS = {
    "stage1": MatrixSpec(("flat",), ("benign",), (41,)),
    "stage2": MatrixSpec(("flat",), ("benign",), SEARCH_SEEDS),
    "stage3": MatrixSpec(
        (
            "flat",
            "single_jump",
        ),
        ("benign",),
        SEARCH_SEEDS,
    ),
}


QUICK_SELECTION_COUNTS = {"stage1": 30, "stage2": 12, "stage3": 1, "local": 1}


ENGINE_FILES = (
    "dusk_controller_search.py",
    "cpmm_controller_search.py",
    "native_benchmark.py",
    "native_replay.py",
    "native_flow.py",
    "economics.py",
    "dusk_policy.py",
    "models.py",
    "scenarios.py",
    "results/parameter_matching.csv",
)


def _bps_to_fraction(value: float) -> float:
    return value / 10_000.0


def _coefficient_to_nad(value: float) -> int:
    return int(round(value * NAD))


def _build_config(
    controller: ControllerTuple,
    fee_bundle: FeeBundle,
) -> ControlledConfig:
    dynamic_enabled = (
        fee_bundle.divergence_coefficient > 0.0
        or fee_bundle.volatility_coefficient > 0.0
    )
    if fee_bundle.dynamic_cap_bps < 0:
        raise ValueError("dynamic fee cap must not be negative")
    if (fee_bundle.dynamic_cap_bps > 0) != dynamic_enabled:
        raise ValueError(
            "dynamic fee cap must be positive exactly when a dynamic "
            "coefficient is active"
        )
    if (
        fee_bundle.divergence_coefficient < 0.0
        or fee_bundle.volatility_coefficient < 0.0
    ):
        raise ValueError("dynamic fee coefficients must not be negative")
    base = replace(
        default_controlled_config(),
        solver_haircut_ppm=10.0,
        manager_fee_bps=500,
        protocol_fee_bps=0,
        center_ema_half_life_minutes=controller.ema_half_life_minutes,
        adjustment_threshold=_bps_to_fraction(
            controller.adjustment_threshold_bps
        ),
        adjustment_step=_bps_to_fraction(controller.adjustment_step_bps),
        min_adjustment_interval_minutes=(
            controller.min_adjustment_interval_minutes
        ),
    )
    if base.adjustment_threshold < base.adjustment_step:
        raise ValueError("adjustment threshold must not be smaller than step")
    base_rate = base.fee.base_fee_rate_nad
    fee = DynamicFeeConfig(
        base_fee_rate_nad=base_rate,
        max_fee_rate_nad=(
            base_rate + int(fee_bundle.dynamic_cap_bps * NAD // 10_000)
        ),
        divergence_coefficient_nad=_coefficient_to_nad(
            fee_bundle.divergence_coefficient
        ),
        volatility_coefficient_nad=_coefficient_to_nad(
            fee_bundle.volatility_coefficient
        ),
        volatility_half_life_ms=int(
            round(fee_bundle.volatility_half_life_minutes * 60_000)
        ),
        volatility_shock_cap_nad=NAD // 10,
        volatility_accumulator_cap_nad=NAD,
    )
    fee.validate()
    return replace(base, fee=fee)


def grid_profiles() -> tuple[NativeProfile, ...]:
    """Return the exact, stable 10 x 15 controller/fee search grid."""

    _, model = _matched_models()
    profiles = []
    for controller_index, controller in enumerate(CONTROLLER_TUPLES):
        for fee_index, fee_bundle in enumerate(FEE_BUNDLES):
            profile_id = f"grid_c{controller_index:02d}_f{fee_index:02d}"
            profiles.append(
                NativeProfile(
                    "dusk_native",
                    profile_id,
                    model,
                    _build_config(controller, fee_bundle),
                    (
                        f"EMA {controller.ema_half_life_minutes:g}m; "
                        f"threshold {controller.adjustment_threshold_bps:g}bp; "
                        f"step {controller.adjustment_step_bps:g}bp; "
                        f"interval {controller.min_adjustment_interval_minutes}m; "
                        f"dynamic cap {fee_bundle.dynamic_cap_bps}bp; "
                        f"div {fee_bundle.divergence_coefficient:g}; "
                        f"vol {fee_bundle.volatility_coefficient:g}; "
                        f"vol half-life {fee_bundle.volatility_half_life_minutes:g}m"
                    ),
                )
            )
    profile_ids = [profile.profile_id for profile in profiles]
    expected_profiles = len(CONTROLLER_TUPLES) * len(FEE_BUNDLES)
    if expected_profiles != 150:
        raise AssertionError("controller search definition must contain 150 profiles")
    if len(profiles) != expected_profiles or len(set(profile_ids)) != expected_profiles:
        raise AssertionError(
            f"controller search must contain exactly {expected_profiles} profiles"
        )
    return tuple(profiles)


def stage_specs(*, quick: bool) -> Mapping[str, MatrixSpec]:
    return QUICK_STAGE_SPECS if quick else FULL_STAGE_SPECS


def _flow_by_name(name: str) -> dict[str, object]:
    try:
        return dict(
            next(flow for flow in FLOW_REGIMES if flow["flow_regime"] == name)
        )
    except StopIteration as error:
        raise KeyError(f"unknown flow regime {name}") from error


def jobs_for_spec(
    profiles: Sequence[NativeProfile],
    spec: MatrixSpec,
    *,
    mode: str,
    stage: str,
) -> list[dict[str, object]]:
    paths = synthetic_paths(length=720)
    jobs = []
    for path_name in spec.paths:
        scoring_prices = paths[path_name]
        mature_prices = [scoring_prices[0]] * 1_440 + scoring_prices
        prices = mature_prices[::PRIMARY_STEP_MINUTES]
        for flow_name in spec.flows:
            flow = _flow_by_name(flow_name)
            for seed in spec.seeds:
                for routing_mode in ROUTING_MODES:
                    for profile in profiles:
                        jobs.append(
                            _job(
                                profile,
                                path_name=path_name,
                                path_class="synthetic",
                                prices=prices,
                                score_start_step=1_440 // PRIMARY_STEP_MINUTES,
                                step_minutes=PRIMARY_STEP_MINUTES,
                                flow=flow,
                                seed=seed,
                                routing_mode=routing_mode,
                                analysis_slice=(
                                    f"dusk_controller_search_{mode}_{stage}"
                                ),
                            )
                        )
    expected = len(profiles) * spec.cells_per_profile
    if len(jobs) != expected:
        raise AssertionError(f"{stage} has {len(jobs)} jobs; expected {expected}")
    return jobs


def _row_key(row: Mapping[str, object]) -> tuple[str, str, str, int, str]:
    return (
        str(row["profile_id"]),
        str(row["path"]),
        str(row["flow_regime"]),
        int(row["seed"]),
        str(row["routing_mode"]),
    )


def _job_key(job: Mapping[str, object]) -> tuple[str, str, str, int, str]:
    profile = job["profile"]
    flow = job["flow"]
    if not isinstance(profile, NativeProfile) or not isinstance(flow, dict):
        raise TypeError("invalid controller-search job")
    return (
        profile.profile_id,
        str(job["path_name"]),
        str(flow["flow_regime"]),
        int(job["seed"]),
        str(job["routing_mode"]),
    )


def _validate_matrix(
    rows: Sequence[Mapping[str, object]],
    jobs: Sequence[Mapping[str, object]],
) -> None:
    expected = [_job_key(job) for job in jobs]
    actual = [_row_key(row) for row in rows]
    if len(expected) != len(set(expected)):
        raise AssertionError("controller-search jobs contain duplicate cells")
    if len(actual) != len(set(actual)):
        raise AssertionError("controller-search rows contain duplicate cells")
    if set(expected) != set(actual):
        raise AssertionError(
            "controller-search matrix mismatch: "
            f"missing={len(set(expected) - set(actual))}, "
            f"extra={len(set(actual) - set(expected))}"
        )


_RAW_FIELD_ALIASES = {
    "capture_fraction": "captured_user_volume_fraction",
    "captured_volume_fraction": "captured_user_volume_fraction",
    "fee_free_stale_fraction": "fee_free_impact_stale_fraction",
}


def _guardrail_source(suffix: str) -> tuple[str, str, bool]:
    """Map a guardrail suffix to (statistic, replay field, per-flow)."""

    per_flow = suffix.startswith("worst_regime_")
    remainder = suffix[len("worst_regime_") :] if per_flow else suffix
    for statistic in ("median", "p10", "p90"):
        prefix = f"{statistic}_"
        if remainder.startswith(prefix):
            field = remainder[len(prefix) :]
            return statistic, _RAW_FIELD_ALIASES.get(field, field), per_flow
    raise ValueError(f"unsupported service guardrail aggregate {suffix}")


def _statistic(values: Sequence[float], statistic: str) -> float:
    if not values:
        return math.nan
    if statistic == "median":
        return statistics.median(values)
    if statistic == "p10":
        return _percentile(values, 0.10)
    if statistic == "p90":
        return _percentile(values, 0.90)
    raise ValueError(f"unknown statistic {statistic}")


def _aggregate_guardrail(
    route_rows: Sequence[Mapping[str, object]],
    suffix: str,
    kind: str,
) -> float:
    statistic, source_field, per_flow = _guardrail_source(suffix)
    if not per_flow:
        return _statistic(
            [float(row[source_field]) for row in route_rows], statistic
        )
    flow_values = []
    present_regimes = {str(row["flow_regime"]) for row in route_rows}
    for flow in FLOW_REGIMES:
        regime = str(flow["flow_regime"])
        if regime not in present_regimes:
            continue
        values = [
            float(row[source_field])
            for row in route_rows
            if str(row["flow_regime"]) == regime
        ]
        if not values:
            raise AssertionError(f"profile matrix lacks {regime} rows")
        flow_values.append(_statistic(values, statistic))
    if not flow_values:
        raise AssertionError("profile matrix has no flow-regime rows")
    return min(flow_values) if kind == "fraction_min" else max(flow_values)


def _required_search_service_relaxation(row: Mapping[str, object]) -> float:
    """Return Dusk's independent normalized readiness relaxation."""

    if (
        not math.isfinite(float(row["max_accounting_error"]))
        or float(row["max_accounting_error"]) > ACCOUNTING_ERROR_LIMIT
        or int(row["max_controller_external_price_reads"]) != 0
        or int(row.get("max_arbitrage_capped_event_count", 0)) != 0
        or int(row.get("max_arbitrage_numerical_failure_count", 0)) != 0
    ):
        return math.inf
    required = 0.0
    for route in ROUTING_MODES:
        for suffix, (kind, threshold) in SEARCH_SERVICE_GUARDRAILS.items():
            value = float(row[f"{route}_{suffix}"])
            if not math.isfinite(value):
                return math.inf
            if kind.startswith("fraction") and not 0.0 <= value <= 1.0:
                return math.inf
            if kind == "fraction_min":
                violation = max(0.0, (threshold - value) / threshold)
            elif kind == "fraction_max":
                violation = max(0.0, (value - threshold) / (1.0 - threshold))
            elif kind == "cost_max":
                violation = max(0.0, (value - threshold) / threshold)
            else:
                raise ValueError(f"unknown guardrail kind {kind}")
            required = max(required, violation)
    return required


def _failed_search_service_checks(
    row: Mapping[str, object],
    relaxation: float,
) -> list[str]:
    failed = []
    if (
        not math.isfinite(float(row["max_accounting_error"]))
        or float(row["max_accounting_error"]) > ACCOUNTING_ERROR_LIMIT
    ):
        failed.append("max_accounting_error")
    if int(row["max_controller_external_price_reads"]) != 0:
        failed.append("max_controller_external_price_reads")
    if int(row.get("max_arbitrage_capped_event_count", 0)) != 0:
        failed.append("max_arbitrage_capped_event_count")
    if int(row.get("max_arbitrage_numerical_failure_count", 0)) != 0:
        failed.append("max_arbitrage_numerical_failure_count")
    for route in ROUTING_MODES:
        for suffix, (kind, threshold) in SEARCH_SERVICE_GUARDRAILS.items():
            field = f"{route}_{suffix}"
            value = float(row[field])
            range_valid = math.isfinite(value) and (
                not kind.startswith("fraction") or 0.0 <= value <= 1.0
            )
            if kind == "fraction_min":
                relaxed_threshold = threshold * (1.0 - relaxation)
                passes = value >= relaxed_threshold
            elif kind == "fraction_max":
                relaxed_threshold = threshold + relaxation * (1.0 - threshold)
                passes = value <= relaxed_threshold
            elif kind == "cost_max":
                relaxed_threshold = threshold * (1.0 + relaxation)
                passes = value <= relaxed_threshold
            else:
                raise ValueError(f"unknown guardrail kind {kind}")
            if not range_valid or not passes:
                failed.append(
                    f"{field}:{value:.9g}:{kind}:{relaxed_threshold:.9g}"
                )
    return failed


def _profile_config_fields(profile: NativeProfile) -> dict[str, object]:
    if not isinstance(profile.config, ControlledConfig):
        raise TypeError("Dusk search profile requires ControlledConfig")
    if not isinstance(profile.model, CurrentDh2):
        raise TypeError("Dusk search profile requires CurrentDh2")
    fee = profile.config.fee
    return {
        "invariant_peak_depth": profile.model.peak_depth,
        "invariant_fade_scale": profile.model.fade_scale,
        "center_ema_half_life_minutes": (
            profile.config.center_ema_half_life_minutes
        ),
        "adjustment_threshold_bps": (
            profile.config.adjustment_threshold * 10_000.0
        ),
        "adjustment_step_bps": profile.config.adjustment_step * 10_000.0,
        "min_adjustment_interval_minutes": (
            profile.config.min_adjustment_interval_minutes
        ),
        "base_fee_bps": fee.base_fee_rate_nad / NAD * 10_000.0,
        "dynamic_cap_bps": (
            fee.max_fee_rate_nad - fee.base_fee_rate_nad
        )
        / NAD
        * 10_000.0,
        "divergence_coefficient": fee.divergence_coefficient_nad / NAD,
        "volatility_coefficient": fee.volatility_coefficient_nad / NAD,
        "volatility_half_life_minutes": fee.volatility_half_life_ms / 60_000.0,
        "volatility_shock_cap": fee.volatility_shock_cap_nad / NAD,
        "volatility_accumulator_cap": (
            fee.volatility_accumulator_cap_nad / NAD
        ),
        "manager_fee_bps": profile.config.manager_fee_bps,
        "protocol_fee_bps": profile.config.protocol_fee_bps,
        "solver_haircut_ppm": profile.config.solver_haircut_ppm,
    }


def summarize_profiles(
    rows: Sequence[Mapping[str, object]],
    profiles: Sequence[NativeProfile],
) -> list[dict[str, object]]:
    summaries = []
    source_fields = {
        _guardrail_source(suffix)[1]
        for suffix in SEARCH_SERVICE_GUARDRAILS
    }
    finite_fields = {
        "net_lp_return_after_protocol_fee_bps",
        "accounting_identity_error",
        *source_fields,
    }
    for profile in profiles:
        values = [row for row in rows if row["profile_id"] == profile.profile_id]
        if not values:
            raise AssertionError(f"profile {profile.profile_id} has no rows")
        all_metrics_finite = all(
            math.isfinite(float(row[field]))
            for row in values
            for field in finite_fields
        )
        max_accounting_error = max(
            abs(float(row["accounting_identity_error"])) for row in values
        )
        max_external_reads = max(
            int(row["controller_external_price_reads"]) for row in values
        )
        max_capped_events = max(
            int(row.get("arbitrage_capped_event_count", 0)) for row in values
        )
        max_numerical_failures = max(
            int(row.get("arbitrage_numerical_failure_count", 0))
            for row in values
        )
        hard_valid = (
            all_metrics_finite
            and max_accounting_error <= ACCOUNTING_ERROR_LIMIT
            and max_external_reads == 0
            and max_capped_events == 0
            and max_numerical_failures == 0
        )
        summary: dict[str, object] = {
            "system": profile.system,
            "profile_id": profile.profile_id,
            "description": profile.description,
            "training_cells": len(values),
            "hard_valid": hard_valid,
            "all_training_metrics_finite": all_metrics_finite,
            "max_accounting_error": max_accounting_error,
            "max_controller_external_price_reads": max_external_reads,
            "max_arbitrage_capped_event_count": max_capped_events,
            "max_arbitrage_numerical_failure_count": max_numerical_failures,
            **_profile_config_fields(profile),
        }
        route_utilities = []
        for route in ROUTING_MODES:
            route_rows = [
                row for row in values if str(row["routing_mode"]) == route
            ]
            if not route_rows:
                raise AssertionError(f"profile {profile.profile_id} lacks {route}")
            returns = [
                float(row["net_lp_return_after_protocol_fee_bps"])
                for row in route_rows
            ]
            route_median = statistics.median(returns)
            route_p10 = _percentile(returns, 0.10)
            utility = 0.8 * route_median + 0.2 * route_p10
            summary[f"{route}_median_net_lp_return_bps"] = route_median
            summary[f"{route}_p10_net_lp_return_bps"] = route_p10
            summary[f"{route}_selection_utility"] = utility
            route_utilities.append(utility)
            for suffix, (kind, _) in SEARCH_SERVICE_GUARDRAILS.items():
                summary[f"{route}_{suffix}"] = _aggregate_guardrail(
                    route_rows, suffix, kind
                )
        summary["minimum_route_selection_utility"] = min(route_utilities)
        summary["mean_route_selection_utility"] = statistics.mean(
            route_utilities
        )
        relaxation = (
            _required_search_service_relaxation(summary)
            if hard_valid
            else math.inf
        )
        summary["independent_dusk_service_relaxation"] = relaxation
        summary["service_relaxation_required"] = relaxation
        summary["original_service_envelope_pass"] = (
            hard_valid and relaxation <= 1e-12
        )
        failures = _failed_search_service_checks(summary, 0.0)
        summary["failed_original_service_checks"] = ";".join(failures)
        summary["original_service_check_failures"] = len(failures)
        summaries.append(summary)
    return summaries


def rank_summaries(
    summaries: Sequence[Mapping[str, object]],
) -> list[Mapping[str, object]]:
    """Rank finite, hard-valid Dusk rho first, then LP utility."""

    def eligible(row: Mapping[str, object]) -> bool:
        return bool(row["hard_valid"]) and math.isfinite(
            float(row["independent_dusk_service_relaxation"])
        )

    return sorted(
        summaries,
        key=lambda row: (
            not eligible(row),
            float(row["independent_dusk_service_relaxation"]),
            -float(row["minimum_route_selection_utility"]),
            -float(row["mean_route_selection_utility"]),
            str(row["profile_id"]),
        ),
    )


def select_profiles(
    summaries: Sequence[Mapping[str, object]],
    profiles: Sequence[NativeProfile],
    count: int,
) -> tuple[NativeProfile, ...]:
    ranked = [
        row
        for row in rank_summaries(summaries)
        if bool(row["hard_valid"])
        and math.isfinite(float(row["independent_dusk_service_relaxation"]))
    ]
    if len(ranked) < count:
        raise RuntimeError(
            f"only {len(ranked)} hard-valid profiles with finite independent "
            f"Dusk service relaxation available; need {count}"
        )
    by_id = {profile.profile_id: profile for profile in profiles}
    return tuple(by_id[str(row["profile_id"])] for row in ranked[:count])


def _profile_payload(profile: NativeProfile) -> dict[str, object]:
    if not is_dataclass(profile.model) or not is_dataclass(profile.config):
        raise TypeError("search profiles must use dataclass model/config")
    return {
        "system": profile.system,
        "profile_id": profile.profile_id,
        "model_type": type(profile.model).__name__,
        "model": asdict(profile.model),
        "config_type": type(profile.config).__name__,
        "config": asdict(profile.config),
        "description": profile.description,
    }


def stage_fingerprint(
    profiles: Sequence[NativeProfile],
    spec: MatrixSpec,
    *,
    mode: str,
    stage: str,
    upstream_fingerprint: str = "",
) -> str:
    paths = synthetic_paths(length=720)
    payload = {
        "schema_version": SEARCH_SCHEMA_VERSION,
        "mode": mode,
        "stage": stage,
        "upstream_fingerprint": upstream_fingerprint,
        "pool_value": POOL_VALUE,
        "step_minutes": PRIMARY_STEP_MINUTES,
        "spec": asdict(spec),
        "routing_modes": ROUTING_MODES,
        "flow_regimes": {
            name: _flow_by_name(name) for name in spec.flows
        },
        "path_hashes": {
            name: hashlib.sha256(
                json.dumps(
                    [float(value) for value in paths[name]],
                    separators=(",", ":"),
                    allow_nan=False,
                ).encode()
            ).hexdigest()
            for name in spec.paths
        },
        "profiles": [_profile_payload(profile) for profile in profiles],
        "search_service_guardrails": SEARCH_SERVICE_GUARDRAILS,
        "engine_hashes": {
            filename: hashlib.sha256((ROOT / filename).read_bytes()).hexdigest()
            for filename in ENGINE_FILES
        },
    }
    return hashlib.sha256(
        json.dumps(
            payload,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode()
    ).hexdigest()


def _write_csv(path: Path, rows: Sequence[Mapping[str, object]]) -> None:
    if not rows:
        raise ValueError(f"cannot write empty CSV {path}")
    fields = []
    seen = set()
    for row in rows:
        for field in row:
            if field not in seen:
                fields.append(field)
                seen.add(field)
    with path.open("w", newline="") as target:
        writer = csv.DictWriter(target, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            writer.writerow({field: row.get(field, "") for field in fields})


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _atomic_write_stage(
    stage_dir: Path,
    rows: Sequence[Mapping[str, object]],
    summaries: Sequence[Mapping[str, object]],
    manifest: Mapping[str, object],
) -> None:
    stage_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=f".{stage_dir.name}-", dir=stage_dir.parent
    ) as temporary:
        scratch = Path(temporary)
        rows_path = scratch / "rows.csv"
        profiles_path = scratch / "profiles.csv"
        manifest_path = scratch / "manifest.json"
        _write_csv(rows_path, rows)
        _write_csv(profiles_path, summaries)
        committed_manifest = {
            **manifest,
            "content_hashes": {
                "rows.csv": _sha256(rows_path),
                "profiles.csv": _sha256(profiles_path),
            },
        }
        manifest_path.write_text(
            json.dumps(committed_manifest, indent=2, allow_nan=False) + "\n"
        )
        rows_path.replace(stage_dir / "rows.csv")
        profiles_path.replace(stage_dir / "profiles.csv")
        # Manifest is the commit marker and is always installed last.
        manifest_path.replace(stage_dir / "manifest.json")


def _progress_manifest_path(stage_dir: Path) -> Path:
    return stage_dir / "progress.json"


def _load_progress(
    stage_dir: Path,
    fingerprint: str,
    jobs: Sequence[Mapping[str, object]],
    profiles: Sequence[NativeProfile],
) -> tuple[list[dict[str, object]], list[dict[str, object]]] | None:
    """Load atomically committed row batches for an incomplete stage."""

    manifest_path = _progress_manifest_path(stage_dir)
    if not manifest_path.exists():
        return None
    try:
        manifest = json.loads(manifest_path.read_text())
        if (
            manifest.get("status") != "controller_search_stage_in_progress"
            or manifest.get("fingerprint") != fingerprint
            or int(manifest.get("expected_rows", -1)) != len(jobs)
            or int(manifest.get("profiles", -1)) != len(profiles)
        ):
            return None
        batches = manifest.get("batches")
        if not isinstance(batches, list):
            return None
        rows: list[dict[str, object]] = []
        next_start = 0
        for batch in batches:
            if not isinstance(batch, dict):
                return None
            filename = str(batch.get("file", ""))
            if not filename or Path(filename).name != filename:
                return None
            start_row = int(batch.get("start_row", -1))
            end_row = int(batch.get("end_row", -1))
            if start_row != next_start or end_row <= start_row:
                return None
            path = stage_dir / filename
            if not path.is_file():
                return None
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            if digest != batch.get("sha256"):
                return None
            batch_rows = _read_csv(path)
            if len(batch_rows) != end_row - start_row:
                return None
            rows.extend(batch_rows)
            next_start = end_row
        if int(manifest.get("completed_rows", -1)) != len(rows):
            return None
        actual_keys = [_row_key(row) for row in rows]
        expected_keys = {_job_key(job) for job in jobs}
        if len(actual_keys) != len(set(actual_keys)):
            return None
        if not set(actual_keys).issubset(expected_keys):
            return None
        return rows, [dict(batch) for batch in batches]
    except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError):
        return None


def _atomic_append_progress(
    stage_dir: Path,
    fingerprint: str,
    batch_rows: Sequence[Mapping[str, object]],
    batches: Sequence[Mapping[str, object]],
    *,
    completed_rows: int,
    expected_rows: int,
    profile_count: int,
) -> list[dict[str, object]]:
    """Commit one immutable row shard, then atomically advance its manifest."""

    if not batch_rows:
        raise ValueError("cannot checkpoint an empty controller-search batch")
    stage_dir.mkdir(parents=True, exist_ok=True)
    start_row = completed_rows
    end_row = start_row + len(batch_rows)
    filename = (
        f"progress-{fingerprint[:16]}-{start_row:07d}-{end_row:07d}.csv"
    )
    destination = stage_dir / filename
    with tempfile.TemporaryDirectory(
        prefix=f".{stage_dir.name}-progress-", dir=stage_dir.parent
    ) as temporary:
        scratch = Path(temporary) / filename
        _write_csv(scratch, batch_rows)
        scratch.replace(destination)
    entry = {
        "file": filename,
        "start_row": start_row,
        "end_row": end_row,
        "rows": len(batch_rows),
        "sha256": hashlib.sha256(destination.read_bytes()).hexdigest(),
    }
    updated = [dict(batch) for batch in batches] + [entry]
    _atomic_write_json(
        _progress_manifest_path(stage_dir),
        {
            "status": "controller_search_stage_in_progress",
            "schema_version": SEARCH_SCHEMA_VERSION,
            "fingerprint": fingerprint,
            "expected_rows": expected_rows,
            "profiles": profile_count,
            "completed_rows": end_row,
            "checkpoint_batch_size": CHECKPOINT_BATCH_SIZE,
            "batches": updated,
        },
    )
    return updated


def _clear_progress(stage_dir: Path) -> None:
    """Remove only the row shards named by the committed progress manifest."""

    manifest_path = _progress_manifest_path(stage_dir)
    if not manifest_path.exists():
        return
    try:
        manifest = json.loads(manifest_path.read_text())
        batches = manifest.get("batches", [])
        if isinstance(batches, list):
            for batch in batches:
                if not isinstance(batch, dict):
                    continue
                filename = str(batch.get("file", ""))
                if filename and Path(filename).name == filename:
                    (stage_dir / filename).unlink(missing_ok=True)
    finally:
        manifest_path.unlink(missing_ok=True)


def _read_csv(path: Path) -> list[dict[str, object]]:
    with path.open() as source:
        return list(csv.DictReader(source))


def _load_stage(
    stage_dir: Path,
    fingerprint: str,
    jobs: Sequence[Mapping[str, object]],
    profiles: Sequence[NativeProfile],
    selection_count: int,
) -> tuple[list[dict[str, object]], list[dict[str, object]], tuple[NativeProfile, ...]] | None:
    manifest_path = stage_dir / "manifest.json"
    rows_path = stage_dir / "rows.csv"
    profiles_path = stage_dir / "profiles.csv"
    if not (manifest_path.exists() and rows_path.exists() and profiles_path.exists()):
        return None
    try:
        manifest = json.loads(manifest_path.read_text())
        content_hashes = manifest.get("content_hashes")
        if (
            manifest.get("status") != "controller_search_stage_complete"
            or manifest.get("fingerprint") != fingerprint
            or int(manifest.get("rows", -1)) != len(jobs)
            or int(manifest.get("profiles", -1)) != len(profiles)
            or not isinstance(content_hashes, dict)
            or content_hashes.get("rows.csv") != _sha256(rows_path)
            or content_hashes.get("profiles.csv") != _sha256(profiles_path)
        ):
            return None
        rows = _read_csv(rows_path)
        _validate_matrix(rows, jobs)
        summaries = summarize_profiles(rows, profiles)
        selected = select_profiles(summaries, profiles, selection_count)
        selected_ids = [profile.profile_id for profile in selected]
        if manifest.get("selected_profile_ids") != selected_ids:
            return None
        persisted_profile_ids = {
            str(row["profile_id"]) for row in _read_csv(profiles_path)
        }
        if persisted_profile_ids != {profile.profile_id for profile in profiles}:
            return None
        return rows, summaries, selected
    except (AssertionError, KeyError, TypeError, ValueError, OSError):
        return None


def run_stage(
    profiles: Sequence[NativeProfile],
    spec: MatrixSpec,
    *,
    output_root: Path,
    mode: str,
    stage: str,
    workers: int,
    selection_count: int,
    upstream_fingerprint: str = "",
    reusable_rows: Sequence[Mapping[str, object]] = (),
    force: bool = False,
) -> tuple[
    list[dict[str, object]],
    list[dict[str, object]],
    tuple[NativeProfile, ...],
    str,
]:
    jobs = jobs_for_spec(profiles, spec, mode=mode, stage=stage)
    fingerprint = stage_fingerprint(
        profiles,
        spec,
        mode=mode,
        stage=stage,
        upstream_fingerprint=upstream_fingerprint,
    )
    stage_dir = output_root / stage
    if not force:
        loaded = _load_stage(
            stage_dir,
            fingerprint,
            jobs,
            profiles,
            selection_count,
        )
        if loaded is not None:
            rows, summaries, selected = loaded
            print(f"{stage}: resumed {len(rows)} rows", flush=True)
            return rows, summaries, selected, fingerprint

    expected_keys = {_job_key(job) for job in jobs}
    reused = {
        _row_key(row): dict(row)
        for row in reusable_rows
        if _row_key(row) in expected_keys
    }
    progress_rows: list[dict[str, object]] = []
    progress_batches: list[dict[str, object]] = []
    if not force:
        progress = _load_progress(stage_dir, fingerprint, jobs, profiles)
        if progress is not None:
            progress_rows, progress_batches = progress
            print(
                f"{stage}: resumed {len(progress_rows)} in-progress rows",
                flush=True,
            )
    progress_by_key = {_row_key(row): dict(row) for row in progress_rows}
    overlap = set(reused).intersection(progress_by_key)
    if overlap:
        raise AssertionError(
            f"controller-search progress overlaps {len(overlap)} reused cells"
        )
    by_key = {**reused, **progress_by_key}
    missing_jobs = [job for job in jobs if _job_key(job) not in by_key]
    new_rows: list[dict[str, object]] = []
    for offset in range(0, len(missing_jobs), CHECKPOINT_BATCH_SIZE):
        batch_jobs = missing_jobs[offset : offset + CHECKPOINT_BATCH_SIZE]
        batch_rows = _run_jobs(
            batch_jobs,
            workers,
            f"controller search {stage} batch "
            f"{offset // CHECKPOINT_BATCH_SIZE + 1}/"
            f"{math.ceil(len(missing_jobs) / CHECKPOINT_BATCH_SIZE)}",
        )
        _validate_matrix(batch_rows, batch_jobs)
        normalized_batch = []
        for raw_row in batch_rows:
            row = dict(raw_row)
            row["analysis_slice"] = (
                f"dusk_controller_search_{mode}_{stage}"
            )
            normalized_batch.append(row)
        progress_batches = _atomic_append_progress(
            stage_dir,
            fingerprint,
            normalized_batch,
            progress_batches,
            completed_rows=len(progress_rows) + len(new_rows),
            expected_rows=len(jobs),
            profile_count=len(profiles),
        )
        new_rows.extend(normalized_batch)
        by_key.update({_row_key(row): row for row in normalized_batch})
    rows = []
    for job in jobs:
        row = dict(by_key[_job_key(job)])
        row["analysis_slice"] = f"dusk_controller_search_{mode}_{stage}"
        rows.append(row)
    _validate_matrix(rows, jobs)
    summaries = summarize_profiles(rows, profiles)
    selected = select_profiles(summaries, profiles, selection_count)
    selected_ids = {profile.profile_id for profile in selected}
    eligible_profiles = sum(
        bool(row["hard_valid"])
        and math.isfinite(float(row["independent_dusk_service_relaxation"]))
        for row in summaries
    )
    nonfinite_rho_profiles = sum(
        bool(row["hard_valid"])
        and not math.isfinite(
            float(row["independent_dusk_service_relaxation"])
        )
        for row in summaries
    )
    persisted_summaries = [
        {**row, "selected_for_next_stage": row["profile_id"] in selected_ids}
        for row in summaries
    ]
    manifest = {
        "status": "controller_search_stage_complete",
        "canonical_native_output": False,
        "mode": mode,
        "stage": stage,
        "schema_version": SEARCH_SCHEMA_VERSION,
        "fingerprint": fingerprint,
        "upstream_fingerprint": upstream_fingerprint,
        "rows": len(rows),
        "profiles": len(profiles),
        "cells_per_profile": spec.cells_per_profile,
        "reused_rows": len(reused),
        "resumed_progress_rows": len(progress_rows),
        "new_rows": len(new_rows),
        "checkpoint_batch_size": CHECKPOINT_BATCH_SIZE,
        "selection_eligible_profiles": eligible_profiles,
        "selection_excluded_nonfinite_rho_profiles": nonfinite_rho_profiles,
        "nonfinite_rho_selection_policy": "excluded",
        "selected_profile_ids": [profile.profile_id for profile in selected],
    }
    _atomic_write_stage(stage_dir, rows, persisted_summaries, manifest)
    _clear_progress(stage_dir)
    return rows, summaries, selected, fingerprint


_LOCAL_LEVELS: Mapping[str, tuple[float, ...]] = {
    "center_ema_half_life_minutes": (1.0, 3.0, 10.0, 30.0, 60.0),
    "adjustment_threshold_bps": (5.0, 10.0, 25.0, 50.0, 100.0, 200.0),
    "adjustment_step_bps": (1.0, 2.5, 5.0, 10.0, 25.0, 50.0),
    "min_adjustment_interval_minutes": (1.0, 5.0),
    "dynamic_cap_bps": (0.0, 25.0, 50.0, 100.0, 200.0),
    "divergence_coefficient": (0.0, 0.005, 0.010, 0.020),
    "volatility_coefficient": (0.0, 0.005, 0.010, 0.020),
    "volatility_half_life_minutes": (5.0, 15.0, 30.0, 60.0),
}


def _search_point(profile: NativeProfile) -> dict[str, float]:
    fields = _profile_config_fields(profile)
    return {field: float(fields[field]) for field in _LOCAL_LEVELS}


def _point_config(point: Mapping[str, float]) -> ControlledConfig:
    return _build_config(
        ControllerTuple(
            point["center_ema_half_life_minutes"],
            point["adjustment_threshold_bps"],
            point["adjustment_step_bps"],
            int(round(point["min_adjustment_interval_minutes"])),
        ),
        FeeBundle(
            int(round(point["dynamic_cap_bps"])),
            point["divergence_coefficient"],
            point["volatility_coefficient"],
            point["volatility_half_life_minutes"],
        ),
    )


def _valid_search_point(point: Mapping[str, float]) -> bool:
    if point["adjustment_threshold_bps"] < point["adjustment_step_bps"]:
        return False
    dynamic_enabled = (
        point["divergence_coefficient"] > 0.0
        or point["volatility_coefficient"] > 0.0
    )
    if (point["dynamic_cap_bps"] > 0.0) != dynamic_enabled:
        return False
    return True


def _value_slug(value: float) -> str:
    return f"{value:g}".replace("-", "m").replace(".", "p")


def local_neighborhood(winner: NativeProfile) -> tuple[NativeProfile, ...]:
    """Return winner plus every valid one-level, one-field perturbation."""

    if not isinstance(winner.model, CurrentDh2):
        raise TypeError("local search requires CurrentDh2")
    base_point = _search_point(winner)
    profiles = [winner]
    seen_configs = {
        json.dumps(asdict(winner.config), sort_keys=True, separators=(",", ":"))
    }
    for field, levels in _LOCAL_LEVELS.items():
        value = base_point[field]
        nearest = min(range(len(levels)), key=lambda index: abs(levels[index] - value))
        for direction, index in (("down", nearest - 1), ("up", nearest + 1)):
            if not 0 <= index < len(levels):
                continue
            point = dict(base_point)
            point[field] = levels[index]
            if not _valid_search_point(point):
                continue
            if field == "volatility_half_life_minutes" and (
                point["volatility_coefficient"] == 0.0
            ):
                continue
            config = _point_config(point)
            encoded = json.dumps(
                asdict(config), sort_keys=True, separators=(",", ":")
            )
            if encoded in seen_configs:
                continue
            seen_configs.add(encoded)
            profiles.append(
                NativeProfile(
                    "dusk_native",
                    (
                        f"local_{winner.profile_id}_{field}_{direction}_"
                        f"{_value_slug(levels[index])}"
                    ),
                    winner.model,
                    config,
                    f"one-step local perturbation of {winner.profile_id}: {field} {direction}",
                )
            )
    return tuple(profiles)


def _atomic_write_json(path: Path, payload: Mapping[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        prefix=f".{path.name}-",
        suffix=".tmp",
        dir=path.parent,
        delete=False,
    ) as target:
        temporary = Path(target.name)
        json.dump(payload, target, indent=2, allow_nan=False)
        target.write("\n")
    temporary.replace(path)


def run_search(*, workers: int, quick: bool, force: bool = False) -> dict[str, object]:
    if workers <= 0:
        raise ValueError("workers must be positive")
    mode = "quick" if quick else "full"
    output_root = QUICK_OUTPUT if quick else FULL_OUTPUT
    specs = stage_specs(quick=quick)
    all_profiles = grid_profiles()
    if quick:
        stage1_rows, _, top30, fingerprint1 = run_stage(
            all_profiles,
            specs["stage1"],
            output_root=output_root,
            mode=mode,
            stage="stage1",
            workers=workers,
            selection_count=QUICK_SELECTION_COUNTS["stage1"],
            force=force,
        )
        stage2_rows, _, top12, fingerprint2 = run_stage(
            top30,
            specs["stage2"],
            output_root=output_root,
            mode=mode,
            stage="stage2",
            workers=workers,
            selection_count=QUICK_SELECTION_COUNTS["stage2"],
            upstream_fingerprint=fingerprint1,
            reusable_rows=stage1_rows,
            force=force,
        )
        stage3_rows, _, grid_winner, fingerprint3 = run_stage(
            top12,
            specs["stage3"],
            output_root=output_root,
            mode=mode,
            stage="stage3",
            workers=workers,
            selection_count=QUICK_SELECTION_COUNTS["stage3"],
            upstream_fingerprint=fingerprint2,
            reusable_rows=stage2_rows,
            force=force,
        )
        neighborhood = local_neighborhood(grid_winner[0])
        local_rows, local_summaries, final_winner, fingerprint4 = run_stage(
            neighborhood,
            specs["stage3"],
            output_root=output_root,
            mode=mode,
            stage="local",
            workers=workers,
            selection_count=QUICK_SELECTION_COUNTS["local"],
            upstream_fingerprint=fingerprint3,
            reusable_rows=stage3_rows,
            force=force,
        )
        stage_counts = {
            "stage1": {
                "profiles": len(all_profiles),
                "cells_per_profile": specs["stage1"].cells_per_profile,
                "rows": len(stage1_rows),
                "selected": len(top30),
            },
            "stage2": {
                "profiles": len(top30),
                "cells_per_profile": specs["stage2"].cells_per_profile,
                "rows": len(stage2_rows),
                "selected": len(top12),
            },
            "stage3": {
                "profiles": len(top12),
                "cells_per_profile": specs["stage3"].cells_per_profile,
                "rows": len(stage3_rows),
                "selected": 1,
            },
            "local": {
                "profiles": len(neighborhood),
                "cells_per_profile": specs["stage3"].cells_per_profile,
                "rows": len(local_rows),
                "selected": 1,
            },
        }
        fingerprints = {
            "stage1": fingerprint1,
            "stage2": fingerprint2,
            "stage3": fingerprint3,
            "local": fingerprint4,
        }
    else:
        grid_rows, grid_summaries, grid_winner, grid_fingerprint = run_stage(
            all_profiles,
            specs["grid"],
            output_root=output_root,
            mode=mode,
            stage="grid",
            workers=workers,
            selection_count=1,
            force=force,
        )
        neighborhood = local_neighborhood(grid_winner[0])
        local_rows, local_summaries, local_best, local_fingerprint = run_stage(
            neighborhood,
            specs["local"],
            output_root=output_root,
            mode=mode,
            stage="local",
            workers=workers,
            selection_count=1,
            upstream_fingerprint=grid_fingerprint,
            reusable_rows=grid_rows,
            force=force,
        )
        stage_counts = {
            "grid": {
                "profiles": len(all_profiles),
                "cells_per_profile": specs["grid"].cells_per_profile,
                "rows": len(grid_rows),
                "selected": 1,
                "exhaustive": True,
            },
            "local": {
                "profiles": len(neighborhood),
                "cells_per_profile": specs["local"].cells_per_profile,
                "rows": len(local_rows),
                "selected": 1,
                "exhaustive": True,
            },
        }
        fingerprints = {
            "grid": grid_fingerprint,
            "local": local_fingerprint,
        }
        # The equal-budget Curve/DH2 decision lock is the exhaustive 150-point
        # grid winner.  The one-field local pass is retained as a robustness
        # diagnostic only; allowing it to replace the winner would give Dusk
        # an extra optimization budget that Curve does not receive.
        final_winner = grid_winner
        winner_summaries = grid_summaries
        local_best_summary = next(
            row
            for row in rank_summaries(local_summaries)
            if row["profile_id"] == local_best[0].profile_id
        )
    if quick:
        winner_summaries = local_summaries
        local_best = final_winner
        local_best_summary = next(
            row
            for row in rank_summaries(local_summaries)
            if row["profile_id"] == local_best[0].profile_id
        )
    ranked_winners = rank_summaries(winner_summaries)
    winner_summary = next(
        row
        for row in ranked_winners
        if row["profile_id"] == final_winner[0].profile_id
    )
    summary = {
        "status": "smoke_only" if quick else "controller_search_complete",
        "canonical_native_output": False,
        "mode": mode,
        "schema_version": SEARCH_SCHEMA_VERSION,
        "initial_profiles": len(all_profiles),
        "exact_profile_budget": len(all_profiles),
        "complete_matrix_evaluated_for_every_initial_profile": not quick,
        "decision_grade_search": not quick,
        "historical_reads": 0,
        "stage_counts": stage_counts,
        "fingerprints": fingerprints,
        "selection_rule": (
            "hard validity; minimum independent Dusk service relaxation; "
            "maximum worst-route fee-adjusted LP utility; maximum mean-route "
            "utility; stable profile id"
        ),
        "winner": {
            "profile_id": final_winner[0].profile_id,
            "description": final_winner[0].description,
            "hard_valid": bool(winner_summary["hard_valid"]),
            "independent_dusk_service_relaxation": float(
                winner_summary["independent_dusk_service_relaxation"]
            ),
            "original_service_envelope_pass": bool(
                winner_summary["original_service_envelope_pass"]
            ),
            "minimum_route_selection_utility": float(
                winner_summary["minimum_route_selection_utility"]
            ),
            "config": _profile_config_fields(final_winner[0]),
            "full_config": {
                "model": asdict(final_winner[0].model),
                "controller": asdict(final_winner[0].config),
            },
        },
        "local_diagnostic_best": {
            "profile_id": local_best[0].profile_id,
            "description": local_best[0].description,
            "hard_valid": bool(local_best_summary["hard_valid"]),
            "independent_dusk_service_relaxation": float(
                local_best_summary["independent_dusk_service_relaxation"]
            ),
            "minimum_route_selection_utility": float(
                local_best_summary["minimum_route_selection_utility"]
            ),
            "config": _profile_config_fields(local_best[0]),
            "selection_authority": "diagnostic_only",
        },
        "promotion_policy": (
            "promote the exhaustive 150-profile grid winner and adjacent "
            "already-grid-evaluated profiles only; the extra local pass is "
            "diagnostic and cannot replace the equal-budget winner"
        ),
    }
    _atomic_write_json(output_root / "summary.json", summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Search Dusk fee/controller profiles without changing DH2"
    )
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument(
        "--quick",
        action="store_true",
        help=(
            "run a smoke-only staged matrix beneath results/controller_search/quick"
        ),
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="ignore matching stage checkpoints and recompute every cell",
    )
    args = parser.parse_args()
    if args.workers <= 0:
        parser.error("--workers must be positive")
    print(
        json.dumps(
            run_search(workers=args.workers, quick=args.quick, force=args.force),
            indent=2,
        ),
        flush=True,
    )


if __name__ == "__main__":
    main()

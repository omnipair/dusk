#!/usr/bin/env python3
"""Decision-grade native Curve controller and fee parameter search.

The CryptoSwap invariant is fixed to the equal-execution match produced by
``_matched_models``.  Only the surrounding fee schedule and native controller
are searched.  The implementation is an independent benchmark harness around
the clean-room analytical model in this repository; it does not copy, vendor,
or execute Curve contract source code.

Full mode evaluates all 150 candidates over the same complete 128-cell
synthetic matrix used for Dusk controller selection.  Quick mode is a separate
two-cell pipeline smoke test and is never decision-grade.  No historical path
is read by either mode.
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

from curve_native import CurveNativeConfig
from models import CryptoSwap
from native_benchmark import (
    ACCOUNTING_ERROR_LIMIT,
    FLOW_REGIMES,
    POOL_VALUE,
    PRIMARY_STEP_MINUTES,
    SERVICE_GUARDRAILS,
    NativeProfile,
    _job,
    _matched_models,
    _percentile,
    _run_jobs,
)
from scenarios import synthetic_paths


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results" / "curve_controller_search"
FULL_OUTPUT = RESULTS / "full"
QUICK_OUTPUT = RESULTS / "quick"
SEARCH_SCHEMA_VERSION = 1
SEARCH_SEEDS = (41, 7)
ROUTING_MODES = ("fixed", "elastic")
CHECKPOINT_BATCH_SIZE = 256
PROFILE_BUDGET = 150


# Use the canonical native benchmark's exact product-service envelope.  A
# local copy prevents accidental mutation while preserving field-for-field
# equality with the Curve-vs-Dusk comparison.
SEARCH_SERVICE_GUARDRAILS = dict(SERVICE_GUARDRAILS)


@dataclass(frozen=True)
class ControllerTuple:
    policy_id: str
    ema_half_life_minutes: float
    min_adjustment_step: float
    allowed_extra_profit: float


@dataclass(frozen=True)
class FeeBundle:
    policy_id: str
    mid_fee: float
    out_fee: float
    fee_gamma: float


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


# Ten joint controller policies, not a Cartesian product of unrelated knobs.
# They span roughly logarithmic response, step, and profit-gate scales while
# retaining both benchmark anchors exactly:
#   * c00: the matched-model default policy;
#   * c01: the pinned v2.1 observed controller shape.
CONTROLLER_TUPLES = (
    ControllerTuple("matched_default", 30.0, 0.001, 0.000002),
    ControllerTuple("pinned_v21", 601.0 / 60.0, 0.000146, 0.000002),
    ControllerTuple("very_fast_open", 3.0, 0.0001, 0.0),
    ControllerTuple("fast_low_step", 10.0, 0.0005, 0.0000005),
    ControllerTuple("medium_low_step", 30.0, 0.0005, 0.000002),
    ControllerTuple("slow_default_step", 60.0, 0.001, 0.000002),
    ControllerTuple("medium_wide_step", 30.0, 0.0025, 0.000005),
    ControllerTuple("slow_wide_strict", 60.0, 0.0025, 0.000010),
    ControllerTuple("very_slow_wide", 120.0, 0.005, 0.000020),
    ControllerTuple("inertial_strict", 240.0, 0.010, 0.000050),
)


# Fifteen valid native fee shapes.  Fee levels are fractions (5 bp = 0.0005).
# f00 is the matched-model default and f01 is the pinned v2.1 observed shape.
# The remaining bundles isolate base-like schedules, progressively wider tails,
# and narrow/wide transition behavior at otherwise identical fee endpoints.
FEE_BUNDLES = (
    FeeBundle("matched_default", 0.0005, 0.0045, 0.01000),
    FeeBundle("pinned_v21", 0.0026, 0.0045, 0.00023),
    FeeBundle("base_like_flat", 0.0005, 0.0005, 0.01000),
    FeeBundle("base_like_soft_tail", 0.0005, 0.0015, 0.01000),
    FeeBundle("low_mid_low_tail", 0.0003, 0.0030, 0.01000),
    FeeBundle("default_mid_low_tail", 0.0005, 0.0030, 0.01000),
    FeeBundle("wide_tail", 0.0005, 0.0090, 0.01000),
    FeeBundle("extreme_tail", 0.0005, 0.0200, 0.01000),
    FeeBundle("default_narrow_transition", 0.0005, 0.0045, 0.00010),
    FeeBundle("default_medium_narrow", 0.0005, 0.0045, 0.00100),
    FeeBundle("default_wide_transition", 0.0005, 0.0045, 0.05000),
    FeeBundle("default_very_wide_transition", 0.0005, 0.0045, 0.10000),
    FeeBundle("wide_tail_narrow_transition", 0.0005, 0.0090, 0.00023),
    FeeBundle("wide_tail_wide_transition", 0.0005, 0.0090, 0.05000),
    FeeBundle("higher_mid_default_tail", 0.0015, 0.0045, 0.01000),
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


QUICK_MATRIX_SPEC = MatrixSpec(("flat",), ("benign",), (41,))


ENGINE_FILES = (
    "curve_controller_search.py",
    "native_benchmark.py",
    "native_replay.py",
    "native_flow.py",
    "economics.py",
    "curve_native.py",
    "models.py",
    "scenarios.py",
    "results/parameter_matching.csv",
)


def _validate_controller_tuple(controller: ControllerTuple) -> None:
    config = CurveNativeConfig(
        ema_half_life_minutes=controller.ema_half_life_minutes,
        min_adjustment_step=controller.min_adjustment_step,
        allowed_extra_profit=controller.allowed_extra_profit,
    )
    config.validate()


def _validate_fee_bundle(bundle: FeeBundle) -> None:
    values = (bundle.mid_fee, bundle.out_fee, bundle.fee_gamma)
    if not all(math.isfinite(value) for value in values):
        raise ValueError("Curve fee fields must be finite")
    if not 0.0 <= bundle.mid_fee <= bundle.out_fee < 1.0:
        raise ValueError("Curve fees must satisfy 0 <= mid_fee <= out_fee < 1")
    if bundle.fee_gamma <= 0.0:
        raise ValueError("Curve fee_gamma must be positive")


def _build_config(controller: ControllerTuple) -> CurveNativeConfig:
    _validate_controller_tuple(controller)
    return CurveNativeConfig(
        ema_half_life_minutes=controller.ema_half_life_minutes,
        min_adjustment_step=controller.min_adjustment_step,
        allowed_extra_profit=controller.allowed_extra_profit,
    )


def _build_model(invariant: CryptoSwap, bundle: FeeBundle) -> CryptoSwap:
    _validate_fee_bundle(bundle)
    return replace(
        invariant,
        mid_fee=bundle.mid_fee,
        out_fee=bundle.out_fee,
        fee_gamma=bundle.fee_gamma,
    )


def grid_profiles() -> tuple[NativeProfile, ...]:
    """Return the exact stable 10 x 15 Curve search grid."""

    invariant, _ = _matched_models("equal_execution")
    profiles = []
    for controller_index, controller in enumerate(CONTROLLER_TUPLES):
        config = _build_config(controller)
        for fee_index, fee_bundle in enumerate(FEE_BUNDLES):
            model = _build_model(invariant, fee_bundle)
            profile_id = (
                f"curve_grid_c{controller_index:02d}_f{fee_index:02d}"
            )
            profiles.append(
                NativeProfile(
                    "curve_native_v2_1",
                    profile_id,
                    model,
                    config,
                    (
                        f"controller={controller.policy_id}; "
                        f"EMA={controller.ema_half_life_minutes:g}m; "
                        f"step={controller.min_adjustment_step * 10_000:g}bp; "
                        f"allowance={controller.allowed_extra_profit * 1_000_000:g}ppm; "
                        f"fees={fee_bundle.policy_id}; "
                        f"mid={fee_bundle.mid_fee * 10_000:g}bp; "
                        f"out={fee_bundle.out_fee * 10_000:g}bp; "
                        f"fee_gamma={fee_bundle.fee_gamma:g}"
                    ),
                )
            )

    expected = len(CONTROLLER_TUPLES) * len(FEE_BUNDLES)
    ids = [profile.profile_id for profile in profiles]
    encodings = {
        json.dumps(
            {
                "model": asdict(profile.model),
                "config": asdict(profile.config),
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        for profile in profiles
    }
    if expected != PROFILE_BUDGET:
        raise AssertionError("Curve search definition must contain 150 profiles")
    if len(profiles) != PROFILE_BUDGET or len(set(ids)) != PROFILE_BUDGET:
        raise AssertionError("Curve search must contain exactly 150 unique ids")
    if len(encodings) != PROFILE_BUDGET:
        raise AssertionError("Curve search contains duplicate configurations")
    if any(
        not isinstance(profile.model, CryptoSwap)
        or profile.model.amplification != invariant.amplification
        or profile.model.gamma != invariant.gamma
        for profile in profiles
    ):
        raise AssertionError("Curve invariant changed inside controller search")
    return tuple(profiles)


def matrix_spec(*, quick: bool) -> MatrixSpec:
    return QUICK_MATRIX_SPEC if quick else FULL_MATRIX_SPEC


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
    """Build synthetic-only jobs for every profile/matrix cell."""

    paths = synthetic_paths(length=720)
    jobs = []
    for path_name in spec.paths:
        scoring_prices = paths[path_name]
        mature_prices = [scoring_prices[0]] * 1_440 + scoring_prices
        # Freezing once per path lets every profile share one immutable tuple.
        prices = tuple(mature_prices[::PRIMARY_STEP_MINUTES])
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
                                score_start_step=(
                                    1_440 // PRIMARY_STEP_MINUTES
                                ),
                                step_minutes=PRIMARY_STEP_MINUTES,
                                flow=flow,
                                seed=seed,
                                routing_mode=routing_mode,
                                analysis_slice=(
                                    f"curve_controller_search_{mode}_{stage}"
                                ),
                            )
                        )
    expected = len(profiles) * spec.cells_per_profile
    if len(jobs) != expected:
        raise AssertionError(f"{stage} has {len(jobs)} jobs; expected {expected}")
    if any(job["path_class"] != "synthetic" for job in jobs):
        raise AssertionError("Curve search must not create historical jobs")
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
        raise TypeError("invalid Curve controller-search job")
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
        raise AssertionError("Curve controller-search jobs contain duplicate cells")
    if len(actual) != len(set(actual)):
        raise AssertionError("Curve controller-search rows contain duplicate cells")
    if set(expected) != set(actual):
        raise AssertionError(
            "Curve controller-search matrix mismatch: "
            f"missing={len(set(expected) - set(actual))}, "
            f"extra={len(set(actual) - set(expected))}"
        )


_RAW_FIELD_ALIASES = {
    "capture_fraction": "captured_user_volume_fraction",
    "captured_volume_fraction": "captured_user_volume_fraction",
}


def _guardrail_source(suffix: str) -> tuple[str, str, bool]:
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
    present_regimes = {str(row["flow_regime"]) for row in route_rows}
    flow_values = []
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


def _required_curve_service_relaxation(row: Mapping[str, object]) -> float:
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


def _failed_curve_service_checks(
    row: Mapping[str, object], relaxation: float
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
    if not isinstance(profile.model, CryptoSwap):
        raise TypeError("Curve search profile requires CryptoSwap")
    if not isinstance(profile.config, CurveNativeConfig):
        raise TypeError("Curve search profile requires CurveNativeConfig")
    return {
        "invariant_amplification": profile.model.amplification,
        "invariant_gamma": profile.model.gamma,
        "mid_fee_bps": profile.model.mid_fee * 10_000.0,
        "out_fee_bps": profile.model.out_fee * 10_000.0,
        "fee_gamma": profile.model.fee_gamma,
        "ema_half_life_minutes": profile.config.ema_half_life_minutes,
        "min_adjustment_step_bps": (
            profile.config.min_adjustment_step * 10_000.0
        ),
        "allowed_extra_profit_ppm": (
            profile.config.allowed_extra_profit * 1_000_000.0
        ),
        "arb_min_profit_quote": profile.config.arb_min_profit_quote,
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
            "system": "curve_native_v2_1",
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
            _required_curve_service_relaxation(summary)
            if hard_valid
            else math.inf
        )
        summary["independent_curve_service_relaxation"] = relaxation
        summary["service_relaxation_required"] = relaxation
        summary["original_service_envelope_pass"] = (
            hard_valid and relaxation <= 1e-12
        )
        failures = _failed_curve_service_checks(summary, 0.0)
        summary["failed_original_service_checks"] = ";".join(failures)
        summary["original_service_check_failures"] = len(failures)
        summaries.append(summary)
    return summaries


def rank_summaries(
    summaries: Sequence[Mapping[str, object]],
) -> list[Mapping[str, object]]:
    """Rank hard validity, own Curve rho, then worst-route LP utility."""

    def eligible(row: Mapping[str, object]) -> bool:
        return bool(row["hard_valid"]) and math.isfinite(
            float(row["independent_curve_service_relaxation"])
        )

    return sorted(
        summaries,
        key=lambda row: (
            not eligible(row),
            float(row["independent_curve_service_relaxation"]),
            -float(row["minimum_route_selection_utility"]),
            -float(row["mean_route_selection_utility"]),
            str(row["profile_id"]),
        ),
    )


def select_profiles(
    summaries: Sequence[Mapping[str, object]],
    profiles: Sequence[NativeProfile],
    count: int = 1,
) -> tuple[NativeProfile, ...]:
    ranked = [
        row
        for row in rank_summaries(summaries)
        if bool(row["hard_valid"])
        and math.isfinite(
            float(row["independent_curve_service_relaxation"])
        )
    ]
    if len(ranked) < count:
        raise RuntimeError(
            f"only {len(ranked)} hard-valid Curve profiles with finite "
            f"service relaxation available; need {count}"
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
) -> str:
    paths = synthetic_paths(length=720)
    payload = {
        "schema_version": SEARCH_SCHEMA_VERSION,
        "mode": mode,
        "stage": stage,
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
        "checkpoint_batch_size": CHECKPOINT_BATCH_SIZE,
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


def _read_csv(path: Path) -> list[dict[str, object]]:
    with path.open() as source:
        return list(csv.DictReader(source))


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


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
        # The manifest is the atomic commit marker and is installed last.
        manifest_path.replace(stage_dir / "manifest.json")


def _progress_manifest_path(stage_dir: Path) -> Path:
    return stage_dir / "progress.json"


def _load_progress(
    stage_dir: Path,
    fingerprint: str,
    jobs: Sequence[Mapping[str, object]],
    profiles: Sequence[NativeProfile],
) -> tuple[list[dict[str, object]], list[dict[str, object]]] | None:
    manifest_path = _progress_manifest_path(stage_dir)
    if not manifest_path.exists():
        return None
    try:
        manifest = json.loads(manifest_path.read_text())
        if (
            manifest.get("status") != "curve_controller_search_in_progress"
            or manifest.get("fingerprint") != fingerprint
            or int(manifest.get("expected_rows", -1)) != len(jobs)
            or int(manifest.get("profiles", -1)) != len(profiles)
            or int(manifest.get("checkpoint_batch_size", -1))
            != CHECKPOINT_BATCH_SIZE
        ):
            return None
        batches = manifest.get("batches")
        if not isinstance(batches, list):
            return None
        rows: list[dict[str, object]] = []
        next_start = 0
        normalized_batches = []
        for batch in batches:
            if not isinstance(batch, dict):
                return None
            filename = str(batch.get("file", ""))
            if not filename or Path(filename).name != filename:
                return None
            start_row = int(batch.get("start_row", -1))
            end_row = int(batch.get("end_row", -1))
            if start_row != next_start or not start_row < end_row <= len(jobs):
                return None
            expected_batch_rows = min(
                CHECKPOINT_BATCH_SIZE, len(jobs) - start_row
            )
            if end_row - start_row != expected_batch_rows:
                return None
            path = stage_dir / filename
            if not path.is_file() or _sha256(path) != batch.get("sha256"):
                return None
            batch_rows = _read_csv(path)
            if (
                len(batch_rows) != end_row - start_row
                or int(batch.get("rows", -1)) != len(batch_rows)
            ):
                return None
            rows.extend(batch_rows)
            normalized_batches.append(dict(batch))
            next_start = end_row
        if int(manifest.get("completed_rows", -1)) != len(rows):
            return None
        actual_keys = [_row_key(row) for row in rows]
        expected_prefix = [_job_key(job) for job in jobs[: len(rows)]]
        if actual_keys != expected_prefix or len(actual_keys) != len(set(actual_keys)):
            return None
        return rows, normalized_batches
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
    if not batch_rows:
        raise ValueError("cannot checkpoint an empty Curve-search batch")
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
        "sha256": _sha256(destination),
    }
    updated = [dict(batch) for batch in batches] + [entry]
    _atomic_write_json(
        _progress_manifest_path(stage_dir),
        {
            "status": "curve_controller_search_in_progress",
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


def _load_stage(
    stage_dir: Path,
    fingerprint: str,
    jobs: Sequence[Mapping[str, object]],
    profiles: Sequence[NativeProfile],
) -> tuple[
    list[dict[str, object]],
    list[dict[str, object]],
    tuple[NativeProfile, ...],
] | None:
    manifest_path = stage_dir / "manifest.json"
    rows_path = stage_dir / "rows.csv"
    profiles_path = stage_dir / "profiles.csv"
    if not (manifest_path.exists() and rows_path.exists() and profiles_path.exists()):
        return None
    try:
        manifest = json.loads(manifest_path.read_text())
        hashes = manifest.get("content_hashes")
        if not isinstance(hashes, dict):
            return None
        if (
            manifest.get("status") != "curve_controller_search_stage_complete"
            or manifest.get("fingerprint") != fingerprint
            or int(manifest.get("rows", -1)) != len(jobs)
            or int(manifest.get("profiles", -1)) != len(profiles)
            or int(manifest.get("checkpoint_batch_size", -1))
            != CHECKPOINT_BATCH_SIZE
            or hashes.get("rows.csv") != _sha256(rows_path)
            or hashes.get("profiles.csv") != _sha256(profiles_path)
        ):
            return None
        rows = _read_csv(rows_path)
        _validate_matrix(rows, jobs)
        summaries = summarize_profiles(rows, profiles)
        selected = select_profiles(summaries, profiles)
        if manifest.get("selected_profile_ids") != [
            profile.profile_id for profile in selected
        ]:
            return None
        persisted_ids = {
            str(row["profile_id"]) for row in _read_csv(profiles_path)
        }
        if persisted_ids != {profile.profile_id for profile in profiles}:
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
    force: bool = False,
) -> tuple[
    list[dict[str, object]],
    list[dict[str, object]],
    tuple[NativeProfile, ...],
    str,
]:
    jobs = jobs_for_spec(profiles, spec, mode=mode, stage=stage)
    fingerprint = stage_fingerprint(
        profiles, spec, mode=mode, stage=stage
    )
    stage_dir = output_root / stage
    if not force:
        loaded = _load_stage(stage_dir, fingerprint, jobs, profiles)
        if loaded is not None:
            rows, summaries, selected = loaded
            print(f"{stage}: resumed {len(rows)} rows", flush=True)
            return rows, summaries, selected, fingerprint

    progress_rows: list[dict[str, object]] = []
    progress_batches: list[dict[str, object]] = []
    if force:
        _clear_progress(stage_dir)
    else:
        progress = _load_progress(stage_dir, fingerprint, jobs, profiles)
        if progress is None and _progress_manifest_path(stage_dir).exists():
            _clear_progress(stage_dir)
        elif progress is not None:
            progress_rows, progress_batches = progress
            print(
                f"{stage}: resumed {len(progress_rows)} in-progress rows",
                flush=True,
            )

    rows = list(progress_rows)
    missing_jobs = jobs[len(progress_rows) :]
    for offset in range(0, len(missing_jobs), CHECKPOINT_BATCH_SIZE):
        batch_jobs = missing_jobs[offset : offset + CHECKPOINT_BATCH_SIZE]
        batch_rows = _run_jobs(
            batch_jobs,
            workers,
            f"Curve controller search {stage} batch "
            f"{offset // CHECKPOINT_BATCH_SIZE + 1}/"
            f"{math.ceil(len(missing_jobs) / CHECKPOINT_BATCH_SIZE)}",
        )
        _validate_matrix(batch_rows, batch_jobs)
        normalized_batch = []
        for raw_row in batch_rows:
            row = dict(raw_row)
            row["analysis_slice"] = f"curve_controller_search_{mode}_{stage}"
            normalized_batch.append(row)
        progress_batches = _atomic_append_progress(
            stage_dir,
            fingerprint,
            normalized_batch,
            progress_batches,
            completed_rows=len(rows),
            expected_rows=len(jobs),
            profile_count=len(profiles),
        )
        rows.extend(normalized_batch)

    _validate_matrix(rows, jobs)
    summaries = summarize_profiles(rows, profiles)
    selected = select_profiles(summaries, profiles)
    selected_ids = {profile.profile_id for profile in selected}
    eligible_profiles = sum(
        bool(row["hard_valid"])
        and math.isfinite(
            float(row["independent_curve_service_relaxation"])
        )
        for row in summaries
    )
    nonfinite_rho_profiles = sum(
        bool(row["hard_valid"])
        and not math.isfinite(
            float(row["independent_curve_service_relaxation"])
        )
        for row in summaries
    )
    persisted_summaries = [
        {**row, "selected": row["profile_id"] in selected_ids}
        for row in summaries
    ]
    manifest = {
        "status": "curve_controller_search_stage_complete",
        "canonical_native_output": False,
        "mode": mode,
        "stage": stage,
        "schema_version": SEARCH_SCHEMA_VERSION,
        "fingerprint": fingerprint,
        "rows": len(rows),
        "profiles": len(profiles),
        "cells_per_profile": spec.cells_per_profile,
        "historical_reads": 0,
        "resumed_progress_rows": len(progress_rows),
        "new_rows": len(rows) - len(progress_rows),
        "checkpoint_batch_size": CHECKPOINT_BATCH_SIZE,
        "selection_eligible_profiles": eligible_profiles,
        "selection_excluded_nonfinite_rho_profiles": nonfinite_rho_profiles,
        "nonfinite_rho_selection_policy": "excluded",
        "selected_profile_ids": [profile.profile_id for profile in selected],
    }
    _atomic_write_stage(stage_dir, rows, persisted_summaries, manifest)
    _clear_progress(stage_dir)
    return rows, summaries, selected, fingerprint


def run_search(
    *, workers: int, quick: bool, force: bool = False
) -> dict[str, object]:
    if workers <= 0:
        raise ValueError("workers must be positive")
    mode = "quick" if quick else "full"
    output_root = QUICK_OUTPUT if quick else FULL_OUTPUT
    stage = "smoke" if quick else "grid"
    spec = matrix_spec(quick=quick)
    profiles = grid_profiles()
    rows, summaries, selected, fingerprint = run_stage(
        profiles,
        spec,
        output_root=output_root,
        mode=mode,
        stage=stage,
        workers=workers,
        force=force,
    )
    winner = selected[0]
    winner_summary = next(
        row for row in summaries if row["profile_id"] == winner.profile_id
    )
    summary = {
        "status": "smoke_only" if quick else "curve_controller_search_complete",
        "canonical_native_output": False,
        "mode": mode,
        "schema_version": SEARCH_SCHEMA_VERSION,
        "initial_profiles": len(profiles),
        "exact_profile_budget": PROFILE_BUDGET,
        "cells_per_profile": spec.cells_per_profile,
        "rows": len(rows),
        "complete_matrix_evaluated_for_every_profile": not quick,
        "decision_grade_search": not quick,
        "historical_reads": 0,
        "fingerprint": fingerprint,
        "checkpoint_batch_size": CHECKPOINT_BATCH_SIZE,
        "selection_rule": (
            "hard validity; minimum independent Curve service relaxation; "
            "maximum worst-route fee-adjusted LP utility; maximum mean-route "
            "utility; stable profile id"
        ),
        "winner": {
            "profile_id": winner.profile_id,
            "description": winner.description,
            "hard_valid": bool(winner_summary["hard_valid"]),
            "independent_curve_service_relaxation": float(
                winner_summary["independent_curve_service_relaxation"]
            ),
            "original_service_envelope_pass": bool(
                winner_summary["original_service_envelope_pass"]
            ),
            "minimum_route_selection_utility": float(
                winner_summary["minimum_route_selection_utility"]
            ),
            "mean_route_selection_utility": float(
                winner_summary["mean_route_selection_utility"]
            ),
            "full_config": {
                "model": asdict(winner.model),
                "controller": asdict(winner.config),
            },
            "display_fields": _profile_config_fields(winner),
        },
        "scope": (
            "synthetic pre-lock selection only; fixed equal-execution "
            "CryptoSwap invariant; no historical reads"
        ),
        "clean_room_notice": (
            "independent analytical benchmark implementation; no Curve source "
            "code is copied, vendored, or executed"
        ),
    }
    _atomic_write_json(output_root / "summary.json", summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(
        description=(
            "Search Curve native fees/controller with a fixed matched invariant"
        )
    )
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument(
        "--quick",
        action="store_true",
        help=(
            "run a smoke-only matrix beneath "
            "results/curve_controller_search/quick"
        ),
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="ignore matching completed output and recompute every cell",
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

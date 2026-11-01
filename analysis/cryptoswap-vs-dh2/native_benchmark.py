#!/usr/bin/env python3
"""Reproducible native Curve-vs-Dusk controller benchmark.

Controller profiles are selected on synthetic paths only.  The selected
profiles are then locked before the frozen historical windows are scored.
Every paired system cell receives the same externally denominated intent tape.
"""

from __future__ import annotations

import argparse
import csv
from concurrent.futures import ProcessPoolExecutor
from dataclasses import asdict, dataclass, is_dataclass, replace
import hashlib
import inspect
import json
import math
from pathlib import Path
import statistics
import tempfile
import time
from typing import Iterable, Sequence

from comparison import match_to_curve
import curve_native as curve_native_module
from curve_native import CurveNativeConfig, REFERENCE_COMMIT, REFERENCE_RELEASE
from dusk_policy import DynamicFeeConfig
import economics as economics_module
from economics import ControlledConfig, default_controlled_config
from models import CryptoSwap, CurrentDh2
from native_flow import generate_native_order_tape
from native_replay import (
    ARBITRAGE_CAP_COUNTER_SCOPE,
    RoutingConfig,
    simulate_curve_native_tape,
    simulate_dusk_native_tape,
)
from scenarios import native_historical_windows, synthetic_paths


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"
TRAINING_CHECKPOINT_DIR = RESULTS / "checkpoints" / "native_full_training"
POOL_VALUE = 2_000_000.0
PRIMARY_STEP_MINUTES = 5
PRIMARY_SEEDS = (7, 41)
ARRIVALS_PER_MINUTE = 0.4
VOLUME_SENSITIVITY_RATIOS = (0.05, 0.25, 1.0, 4.0)


def _controller_api_oracle_isolation() -> dict[str, object]:
    """Fail if a center/EMA controller boundary acquires an oracle argument."""

    boundaries = {
        "curve_roll_ema": curve_native_module._roll_ema_before_trade,
        "curve_recenter": curve_native_module.maybe_recenter,
        "dusk_roll_ema": economics_module._update_ema_before_trade,
        "dusk_recenter": economics_module.maybe_recenter,
    }
    forbidden = {"external_price", "reference_price", "oracle_price", "market_price"}
    signatures = {
        name: tuple(inspect.signature(function).parameters)
        for name, function in boundaries.items()
    }
    violations = {
        name: sorted(forbidden.intersection(parameters))
        for name, parameters in signatures.items()
        if forbidden.intersection(parameters)
    }
    return {
        "passes": not violations,
        "forbidden_parameter_names": sorted(forbidden),
        "signatures": {name: list(parameters) for name, parameters in signatures.items()},
        "violations": violations,
        "scope": (
            "runtime signature audit of the EMA and recenter controller APIs; "
            "external prices remain available only to flow, routing, arbitrage, "
            "and mark-to-market evaluation"
        ),
    }

FLOW_REGIMES = (
    {
        "flow_regime": "benign",
        "arb_latency_minutes": 5,
        "informed_share": 0.0,
        "informed_accuracy": 0.5,
        "daily_volume_pool_ratio": 0.25,
        "mean_arrivals_per_minute": 0.4,
    },
    {
        "flow_regime": "mixed",
        "arb_latency_minutes": 5,
        "informed_share": 0.20,
        "informed_accuracy": 0.80,
        "daily_volume_pool_ratio": 0.25,
        "mean_arrivals_per_minute": 0.4,
    },
    {
        "flow_regime": "toxic_delayed",
        "arb_latency_minutes": 30,
        "informed_share": 0.50,
        "informed_accuracy": 1.0,
        "daily_volume_pool_ratio": 0.25,
        "mean_arrivals_per_minute": 0.4,
    },
    {
        "flow_regime": "low_volume_stuck",
        "arb_latency_minutes": 30,
        "informed_share": 0.80,
        "informed_accuracy": 1.0,
        "daily_volume_pool_ratio": 0.05,
        # Low activity must reduce state-update frequency, not merely shrink
        # each order while leaving the EMA/recenter clock equally informed.
        "mean_arrivals_per_minute": 0.08,
    },
)


@dataclass(frozen=True)
class NativeProfile:
    system: str
    profile_id: str
    model: CryptoSwap | CurrentDh2
    config: CurveNativeConfig | ControlledConfig
    description: str


CANDIDATE_SYSTEMS = ("curve_native_v2_1", "dusk_native")
PROFILE_SELECTION_SYSTEMS = (*CANDIDATE_SYSTEMS, "dusk_cpmm")
PROMOTED_DUSK_PROFILES = ROOT / "data" / "dusk_controller_profiles.json"
PROMOTED_CURVE_PROFILES = ROOT / "data" / "curve_controller_profiles.json"
PROMOTED_CPMM_PROFILES = ROOT / "data" / "cpmm_controller_profiles.json"
PROMOTION_SCHEMA_VERSION = 2
PROMOTION_SOURCE_FIELDS = {
    "curve_native_v2_1": (
        "invariant_amplification",
        "invariant_gamma",
        "mid_fee_bps",
        "out_fee_bps",
        "fee_gamma",
        "ema_half_life_minutes",
        "min_adjustment_step_bps",
        "allowed_extra_profit_ppm",
        "arb_min_profit_quote",
    ),
    "dusk_native": (
        "invariant_peak_depth",
        "invariant_fade_scale",
        "center_ema_half_life_minutes",
        "adjustment_threshold_bps",
        "adjustment_step_bps",
        "min_adjustment_interval_minutes",
        "base_fee_bps",
        "dynamic_cap_bps",
        "divergence_coefficient",
        "volatility_coefficient",
        "volatility_half_life_minutes",
        "volatility_shock_cap",
        "volatility_accumulator_cap",
        "manager_fee_bps",
        "protocol_fee_bps",
        "solver_haircut_ppm",
    ),
    "dusk_cpmm": (
        "invariant_peak_depth",
        "invariant_fade_scale",
        "center_ema_half_life_minutes",
        "adjustment_threshold_bps",
        "adjustment_step_bps",
        "min_adjustment_interval_minutes",
        "base_fee_bps",
        "dynamic_cap_bps",
        "divergence_coefficient",
        "volatility_coefficient",
        "volatility_half_life_minutes",
        "volatility_shock_cap",
        "volatility_accumulator_cap",
        "manager_fee_bps",
        "protocol_fee_bps",
        "solver_haircut_ppm",
    ),
}


SERVICE_GUARDRAILS = {
    "median_captured_volume_fraction": ("fraction_min", 0.50),
    "median_selected_execution_cost_bps": ("cost_max", 25.0),
    "p90_selected_execution_cost_bps": ("cost_max", 100.0),
    # Staleness is a marginal-price tracking property.  Finite-size quote
    # impact and fees remain visible through execution-cost guardrails and raw
    # probe diagnostics; including them here would make a centered CPMM fail
    # by construction (its 1%-TVL self-impact is about 196 bps).
    "median_marginal_tracking_stale_fraction": ("fraction_max", 0.75),
    "worst_regime_median_capture_fraction": ("fraction_min", 0.25),
    "worst_regime_p10_capture_fraction": ("fraction_min", 0.10),
    "worst_regime_median_marginal_tracking_stale_fraction": (
        "fraction_max",
        0.90,
    ),
    "worst_regime_p90_selected_execution_cost_bps": ("cost_max", 150.0),
}
ACCOUNTING_ERROR_LIMIT = 1e-5
TRAINING_REPLAY_SCHEMA_VERSION = 5
TRAINING_REPLAY_ENGINE_FILES = (
    "native_benchmark.py",
    "native_replay.py",
    "native_flow.py",
    "economics.py",
    "dusk_policy.py",
    "curve_native.py",
    "models.py",
    "comparison.py",
    "scenarios.py",
)


def _percentile(values: Sequence[float], probability: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return math.nan
    position = (len(ordered) - 1) * probability
    low = math.floor(position)
    high = math.ceil(position)
    if low == high:
        return ordered[low]
    weight = position - low
    return ordered[low] * (1.0 - weight) + ordered[high] * weight


def _write_csv(path: Path, rows: Sequence[dict[str, object]]) -> None:
    if not rows:
        raise ValueError(f"no rows for {path}")
    fields: list[str] = []
    seen = set()
    for row in rows:
        for field in row:
            if field not in seen:
                fields.append(field)
                seen.add(field)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as target:
        writer = csv.DictWriter(target, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            writer.writerow({field: row.get(field, "") for field in fields})


def _write_native_outputs_atomically(
    output_dir: Path,
    datasets: dict[str, Sequence[dict[str, object]]],
    summary: dict[str, object],
) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=".native-stage-",
        dir=output_dir,
    ) as temporary:
        stage = Path(temporary)
        for filename, rows in datasets.items():
            _write_csv(stage / filename, rows)
        (stage / "native_summary.json").write_text(
            json.dumps(summary, indent=2) + "\n"
        )
        for staged in stage.iterdir():
            staged.replace(output_dir / staged.name)


def _training_replay_fingerprint(
    profiles: Sequence[NativeProfile],
) -> str:
    paths = synthetic_paths(length=720)
    path_hashes = {
        name: hashlib.sha256(
            json.dumps(
                [float(value) for value in prices],
                separators=(",", ":"),
                allow_nan=False,
            ).encode()
        ).hexdigest()
        for name, prices in sorted(paths.items())
    }
    profile_payload = []
    for profile in profiles:
        if not is_dataclass(profile.model) or not is_dataclass(profile.config):
            raise TypeError("training fingerprint requires dataclass model/config")
        profile_payload.append(
            {
                "system": profile.system,
                "profile_id": profile.profile_id,
                "model_type": type(profile.model).__name__,
                "model": asdict(profile.model),
                "config_type": type(profile.config).__name__,
                "config": asdict(profile.config),
                "description": profile.description,
            }
        )
    payload = {
        "training_replay_schema_version": TRAINING_REPLAY_SCHEMA_VERSION,
        "training_replay_engine_hashes": {
            filename: hashlib.sha256((ROOT / filename).read_bytes()).hexdigest()
            for filename in TRAINING_REPLAY_ENGINE_FILES
        },
        "curve_reference_commit": REFERENCE_COMMIT,
        "pool_value": POOL_VALUE,
        "primary_step_minutes": PRIMARY_STEP_MINUTES,
        "primary_seeds": list(PRIMARY_SEEDS),
        "arrivals_per_minute": ARRIVALS_PER_MINUTE,
        "flow_regimes": list(FLOW_REGIMES),
        "synthetic_path_hashes": path_hashes,
        "profiles": profile_payload,
    }
    encoded = json.dumps(
        payload,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode()
    return hashlib.sha256(encoded).hexdigest()


def _training_row_key(row: dict[str, object]) -> tuple[object, ...]:
    return (
        str(row["system"]),
        str(row["profile_id"]),
        str(row["path"]),
        str(row["flow_regime"]),
        int(row["seed"]),
        str(row["routing_mode"]),
    )


def _training_job_key(job: dict[str, object]) -> tuple[object, ...]:
    profile = job["profile"]
    if not isinstance(profile, NativeProfile):
        raise TypeError("training job profile has the wrong type")
    flow = job["flow"]
    if not isinstance(flow, dict):
        raise TypeError("training job flow has the wrong type")
    return (
        profile.system,
        profile.profile_id,
        str(job["path_name"]),
        str(flow["flow_regime"]),
        int(job["seed"]),
        str(job["routing_mode"]),
    )


def _validate_training_cross(
    rows: Sequence[dict[str, object]],
    profiles: Sequence[NativeProfile],
) -> None:
    expected_keys = [_training_job_key(job) for job in _training_jobs(profiles)]
    actual_keys = [_training_row_key(row) for row in rows]
    if len(expected_keys) != len(set(expected_keys)):
        raise AssertionError("training job matrix contains duplicate cell keys")
    if len(actual_keys) != len(set(actual_keys)):
        raise AssertionError("training checkpoint contains duplicate cell keys")
    if set(actual_keys) != set(expected_keys):
        missing = len(set(expected_keys) - set(actual_keys))
        extra = len(set(actual_keys) - set(expected_keys))
        raise AssertionError(
            f"training checkpoint cross mismatch: missing={missing}, extra={extra}"
        )


def _write_training_checkpoint(
    rows: Sequence[dict[str, object]],
    profiles: Sequence[NativeProfile],
    profile_summary: Sequence[dict[str, object]] | None = None,
) -> None:
    _validate_training_cross(rows, profiles)
    fingerprint = _training_replay_fingerprint(profiles)
    TRAINING_CHECKPOINT_DIR.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=".training-stage-", dir=TRAINING_CHECKPOINT_DIR
    ) as temporary:
        stage = Path(temporary)
        _write_csv(stage / "native_training_results.csv", rows)
        manifest = {
            "status": "training_complete",
            "rows": len(rows),
            "training_replay_fingerprint": fingerprint,
            "training_replay_schema_version": TRAINING_REPLAY_SCHEMA_VERSION,
            "profile_rows": 0 if profile_summary is None else len(profile_summary),
            "purpose": (
                "non-canonical deterministic checkpoint; final report requires "
                "the complete atomic native output matrix"
            ),
        }
        if profile_summary is not None:
            _write_csv(stage / "native_profile_diagnostics.csv", profile_summary)
        (stage / "checkpoint.json").write_text(
            json.dumps(manifest, indent=2) + "\n"
        )
        for staged in stage.iterdir():
            staged.replace(TRAINING_CHECKPOINT_DIR / staged.name)


def _read_training_checkpoint(
    expected_rows: int,
    profiles: Sequence[NativeProfile],
) -> list[dict[str, object]]:
    path = TRAINING_CHECKPOINT_DIR / "native_training_results.csv"
    manifest_path = TRAINING_CHECKPOINT_DIR / "checkpoint.json"
    if not path.exists():
        raise FileNotFoundError(f"training checkpoint does not exist: {path}")
    if not manifest_path.exists():
        raise FileNotFoundError(
            f"training checkpoint manifest does not exist: {manifest_path}"
        )
    manifest = json.loads(manifest_path.read_text())
    expected_fingerprint = _training_replay_fingerprint(profiles)
    if manifest.get("training_replay_fingerprint") != expected_fingerprint:
        raise AssertionError("training checkpoint replay fingerprint is stale")
    if int(manifest.get("training_replay_schema_version", -1)) != (
        TRAINING_REPLAY_SCHEMA_VERSION
    ):
        raise AssertionError("training checkpoint replay schema is stale")
    with path.open() as source:
        rows: list[dict[str, object]] = list(csv.DictReader(source))
    if len(rows) != expected_rows:
        raise AssertionError(
            f"training checkpoint has {len(rows)} rows; expected {expected_rows}"
        )
    _validate_training_cross(rows, profiles)
    return rows


def _required_service_relaxation(row: dict[str, object]) -> float:
    accounting_error = float(row["max_accounting_error"])
    external_reads = int(row["max_controller_external_price_reads"])
    capped_arbitrage_events = int(row.get("max_arbitrage_capped_event_count", 0))
    arbitrage_numerical_failures = int(
        row.get("max_arbitrage_numerical_failure_count", 0)
    )
    if (
        not math.isfinite(accounting_error)
        or accounting_error > ACCOUNTING_ERROR_LIMIT
        or external_reads != 0
        or capped_arbitrage_events != 0
        or arbitrage_numerical_failures != 0
    ):
        return math.inf
    required = 0.0
    for route in ("fixed", "elastic"):
        for suffix, (kind, threshold) in SERVICE_GUARDRAILS.items():
            value = float(row[f"{route}_{suffix}"])
            if not math.isfinite(value):
                return math.inf
            if kind.startswith("fraction") and not 0.0 <= value <= 1.0:
                return math.inf
            if kind == "fraction_min":
                violation = max(0.0, (threshold - value) / threshold)
            elif kind == "fraction_max":
                violation = max(0.0, (value - threshold) / (1.0 - threshold))
            else:
                violation = max(0.0, (value - threshold) / threshold)
            required = max(required, violation)
    return required


def _failed_service_checks(
    row: dict[str, object],
    relaxation: float,
) -> list[str]:
    failed = []
    if (
        not math.isfinite(float(row["max_accounting_error"]))
        or float(row["max_accounting_error"]) > ACCOUNTING_ERROR_LIMIT
    ):
        failed.append(
            f"max_accounting_error:{float(row['max_accounting_error']):.9g}:max:"
            f"{ACCOUNTING_ERROR_LIMIT:.9g}"
        )
    if int(row["max_controller_external_price_reads"]) != 0:
        failed.append(
            "max_controller_external_price_reads:"
            f"{int(row['max_controller_external_price_reads'])}:max:0"
        )
    if int(row.get("max_arbitrage_capped_event_count", 0)) != 0:
        failed.append(
            "max_all_period_arbitrage_capped_event_count:"
            f"{int(row.get('max_arbitrage_capped_event_count', 0))}:max:0"
        )
    if int(row.get("max_arbitrage_numerical_failure_count", 0)) != 0:
        failed.append(
            "max_all_period_arbitrage_numerical_failure_count:"
            f"{int(row.get('max_arbitrage_numerical_failure_count', 0))}:max:0"
        )
    for route in ("fixed", "elastic"):
        for suffix, (kind, threshold) in SERVICE_GUARDRAILS.items():
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
            else:
                relaxed_threshold = threshold * (1.0 + relaxation)
                passes = value <= relaxed_threshold
            if not range_valid or not passes:
                failed.append(
                    f"{field}:{value:.9g}:{kind}:{relaxed_threshold:.9g}"
                )
    return failed


def _training_service_deployment_ready(
    selected_training_rows: dict[str, dict[str, object]],
) -> bool:
    if set(selected_training_rows) != set(CANDIDATE_SYSTEMS):
        return False
    relaxations = {
        float(row["selection_group_service_relaxation"])
        for row in selected_training_rows.values()
    }
    return (
        len(relaxations) == 1
        and next(iter(relaxations)) <= 1e-12
        and all(
            bool(row["original_service_envelope_pass"])
            for row in selected_training_rows.values()
        )
    )


def _cpmm_product_training_ready(
    selected_training_row: dict[str, object],
) -> bool:
    return (
        selected_training_row.get("system") == "dusk_cpmm"
        and selected_training_row.get("selection_group") == "cpmm_product"
        and float(
            selected_training_row["selection_group_service_relaxation"]
        )
        <= 1e-12
        and bool(selected_training_row["original_service_envelope_pass"])
    )


def _dusk_product_training_ready(
    selected_training_row: dict[str, object],
) -> bool:
    """Evaluate Dusk independently from the comparative Curve/DH2 lock."""

    return (
        selected_training_row.get("system") == "dusk_native"
        and float(selected_training_row["service_relaxation_required"])
        <= 1e-12
        and bool(selected_training_row["original_service_envelope_pass"])
    )


def _matched_models(
    basis: str = "equal_execution",
) -> tuple[CryptoSwap, CurrentDh2]:
    with (RESULTS / "parameter_matching.csv").open() as source:
        rows = [
            row
            for row in csv.DictReader(source)
            if row["basis"] == basis
        ]
    curve_row = next(row for row in rows if row["model"] == "curve")
    dusk_row = next(row for row in rows if row["model"] == "current_dh2")
    return (
        CryptoSwap(
            amplification=float(curve_row["parameter_1"]),
            gamma=float(curve_row["parameter_2"]),
        ),
        CurrentDh2(
            peak_depth=float(dusk_row["parameter_1"]),
            fade_scale=float(dusk_row["parameter_2"]),
        ),
    )


def _curve_profiles(base: CryptoSwap) -> tuple[NativeProfile, ...]:
    balanced = CurveNativeConfig(
        ema_half_life_minutes=30.0,
        min_adjustment_step=0.001,
        allowed_extra_profit=0.000002,
    )
    specs = (
        ("balanced", base, balanced, "30m EMA; 10bp step; 2ppm allowance"),
        ("ema_fast", base, replace(balanced, ema_half_life_minutes=10.0), "10m EMA"),
        ("ema_slow", base, replace(balanced, ema_half_life_minutes=60.0), "60m EMA"),
        ("step_small", base, replace(balanced, min_adjustment_step=0.0005), "5bp step"),
        ("step_large", base, replace(balanced, min_adjustment_step=0.0025), "25bp step"),
        ("budget_strict", base, replace(balanced, allowed_extra_profit=0.00001), "10ppm allowance"),
        ("budget_open", base, replace(balanced, allowed_extra_profit=0.0), "zero allowance"),
        ("tail_fee_low", replace(base, out_fee=0.0030), balanced, "30bp tail fee"),
        (
            "tail_fee_high",
            replace(base, out_fee=0.0090, fee_gamma=0.005),
            balanced,
            "90bp tail fee; narrower transition",
        ),
        (
            "v21_deployed_controller_shape",
            replace(
                base,
                mid_fee=0.0026,
                out_fee=0.0045,
                fee_gamma=0.00023,
            ),
            CurveNativeConfig(
                ema_half_life_minutes=601.0 / 60.0,
                min_adjustment_step=0.000146,
                allowed_extra_profit=0.000002,
            ),
            "v2.1 fixture fee/controller shape; matched invariant parameters",
        ),
    )
    built_in = tuple(
        NativeProfile("curve_native_v2_1", profile_id, model, config, description)
        for profile_id, model, config, description in specs
    )
    promoted = _load_promoted_curve_profiles(base)
    return promoted if promoted else built_in


def _dusk_profiles(base: CurrentDh2) -> tuple[NativeProfile, ...]:
    balanced = replace(
        default_controlled_config(),
        solver_haircut_ppm=10.0,
        manager_fee_bps=500,
        protocol_fee_bps=0,
    )
    low_dynamic = replace(
        balanced.fee,
        max_fee_rate_nad=10_500_000,
        divergence_coefficient_nad=10_000_000,
        volatility_coefficient_nad=10_000_000,
    )
    high_dynamic = replace(
        balanced.fee,
        max_fee_rate_nad=50_500_000,
        divergence_coefficient_nad=50_000_000,
        volatility_coefficient_nad=50_000_000,
    )
    divergence_only = replace(
        balanced.fee,
        volatility_coefficient_nad=0,
    )
    volatility_only = replace(
        balanced.fee,
        divergence_coefficient_nad=0,
    )
    specs = (
        ("balanced", balanced, "30m EMA; 100bp threshold; 25bp step"),
        ("ema_fast", replace(balanced, center_ema_half_life_minutes=10.0), "10m EMA"),
        ("ema_slow", replace(balanced, center_ema_half_life_minutes=60.0), "60m EMA"),
        ("threshold_low", replace(balanced, adjustment_threshold=0.005), "50bp threshold"),
        ("threshold_high", replace(balanced, adjustment_threshold=0.020), "200bp threshold"),
        ("step_small", replace(balanced, adjustment_step=0.001), "10bp step"),
        ("step_large", replace(balanced, adjustment_step=0.005), "50bp step"),
        ("dynamic_low", replace(balanced, fee=low_dynamic), "half dynamic coefficients; 105bp cap"),
        ("dynamic_high", replace(balanced, fee=high_dynamic), "2.5x dynamic coefficients; 505bp cap"),
        (
            "divergence_only",
            replace(balanced, fee=divergence_only),
            "divergence surcharge only; 205bp cap",
        ),
        (
            "volatility_only",
            replace(balanced, fee=volatility_only),
            "volatility accumulator surcharge only; 205bp cap",
        ),
    )
    built_in = tuple(
        NativeProfile("dusk_native", profile_id, base, config, description)
        for profile_id, config, description in specs
    )
    promoted = _load_promoted_dusk_profiles(base, balanced)
    return promoted if promoted else built_in


def _sha256_path(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _load_promotion_payload(
    path: Path,
    *,
    system: str,
    expected_search_path: str,
    expected_status: str,
) -> tuple[dict[str, object], list[dict[str, object]]]:
    """Load a shortlist only when its full-search source is still exact."""

    if not path.exists():
        return {}, []
    payload = json.loads(path.read_text())
    if payload.get("schema_version") != PROMOTION_SCHEMA_VERSION:
        raise ValueError(f"unsupported promoted {system} profile schema")
    if payload.get("system") != system:
        raise ValueError(f"promoted profile system mismatch for {system}")
    unsigned = dict(payload)
    supplied_promotion_fingerprint = unsigned.pop("promotion_fingerprint", None)
    expected_promotion_fingerprint = hashlib.sha256(
        json.dumps(
            unsigned,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode()
    ).hexdigest()
    if supplied_promotion_fingerprint != expected_promotion_fingerprint:
        raise ValueError(f"promoted {system} snapshot fingerprint mismatch")
    source = payload.get("search_source")
    if not isinstance(source, dict):
        raise ValueError(f"promoted {system} profiles lack search provenance")
    if source.get("summary_path") != expected_search_path:
        raise ValueError(f"promoted {system} search path mismatch")
    source_path = (ROOT / expected_search_path).resolve()
    try:
        source_path.relative_to(ROOT.resolve())
    except ValueError as error:
        raise ValueError("promoted profile source escapes benchmark root") from error
    if not source_path.is_file():
        raise FileNotFoundError(f"missing promoted-profile source: {source_path}")
    if source.get("summary_sha256") != _sha256_path(source_path):
        raise ValueError(f"promoted {system} search source changed")
    summary = json.loads(source_path.read_text())
    if (
        summary.get("status") != expected_status
        or summary.get("mode") != "full"
        or summary.get("decision_grade_search") is not True
    ):
        raise ValueError(f"promoted {system} source is not a complete full search")
    winner = summary.get("winner")
    if not isinstance(winner, dict) or winner.get("hard_valid") is not True:
        raise ValueError(f"promoted {system} source lacks a hard-valid winner")
    winner_id = str(winner.get("profile_id", ""))
    if not winner_id or payload.get("winner_profile_id") != winner_id:
        raise ValueError(f"promoted {system} winner does not match its source")
    rows = payload.get("profiles")
    if not isinstance(rows, list) or not rows:
        raise ValueError(f"promoted {system} profile snapshot is empty")
    if sum(bool(row.get("selected_winner")) for row in rows if isinstance(row, dict)) != 1:
        raise ValueError(f"promoted {system} snapshot must mark one winner")
    marked = next(
        str(row.get("profile_id", ""))
        for row in rows
        if isinstance(row, dict) and row.get("selected_winner") is True
    )
    if marked != winner_id:
        raise ValueError(f"promoted {system} marked winner mismatch")

    summary_fingerprint = summary.get("fingerprint")
    if summary_fingerprint is None:
        fingerprints = summary.get("fingerprints")
        summary_fingerprint = (
            fingerprints.get("grid") if isinstance(fingerprints, dict) else None
        )
    if (
        not isinstance(summary_fingerprint, str)
        or len(summary_fingerprint) != 64
        or source.get("fingerprint") != summary_fingerprint
    ):
        raise ValueError(f"promoted {system} search fingerprint mismatch")
    # Import lazily to avoid a module cycle: the search modules themselves use
    # NativeProfile and replay helpers from this module.  Recomputing here
    # makes a promotion fail closed if any hashed engine file, matched
    # invariant, grid policy, scenario, or matrix definition changed after the
    # search was run.
    from promote_controller_profiles import current_search_fingerprint

    if current_search_fingerprint(system) != summary_fingerprint:
        raise ValueError(
            f"promoted {system} search is stale for the current engine; "
            "rerun the full search and promotion"
        )

    stage_manifest_relative = source.get("stage_manifest_path")
    profiles_relative = source.get("profiles_path")
    expected_parent = str(Path(expected_search_path).parent / "grid")
    if stage_manifest_relative != f"{expected_parent}/manifest.json":
        raise ValueError(f"promoted {system} stage-manifest path mismatch")
    if profiles_relative != f"{expected_parent}/profiles.csv":
        raise ValueError(f"promoted {system} profile-table path mismatch")
    stage_manifest_path = (ROOT / str(stage_manifest_relative)).resolve()
    profiles_path = (ROOT / str(profiles_relative)).resolve()
    for bound_path in (stage_manifest_path, profiles_path):
        try:
            bound_path.relative_to(ROOT.resolve())
        except ValueError as error:
            raise ValueError("promoted source path escapes benchmark root") from error
        if not bound_path.is_file():
            raise FileNotFoundError(f"missing promoted-profile source: {bound_path}")
    if source.get("stage_manifest_sha256") != _sha256_path(stage_manifest_path):
        raise ValueError(f"promoted {system} stage manifest changed")
    if source.get("profiles_sha256") != _sha256_path(profiles_path):
        raise ValueError(f"promoted {system} profile table changed")
    stage_manifest = json.loads(stage_manifest_path.read_text())
    rows_path = stage_manifest_path.parent / "rows.csv"
    content_hashes = stage_manifest.get("content_hashes")
    if (
        stage_manifest.get("status")
        not in {
            "controller_search_stage_complete",
            "curve_controller_search_stage_complete",
        }
        or
        stage_manifest.get("fingerprint") != summary_fingerprint
        or int(stage_manifest.get("profiles", -1)) != 150
        or int(stage_manifest.get("cells_per_profile", -1)) != 128
        or int(stage_manifest.get("rows", -1)) != 19_200
        or stage_manifest.get("selected_profile_ids") != [winner_id]
        or not isinstance(content_hashes, dict)
        or content_hashes.get("profiles.csv") != _sha256_path(profiles_path)
        or not rows_path.is_file()
        or content_hashes.get("rows.csv") != _sha256_path(rows_path)
    ):
        raise ValueError(f"promoted {system} stage matrix is not exact 150x128")
    with profiles_path.open() as source_rows:
        source_profile_rows = {
            str(row["profile_id"]): row for row in csv.DictReader(source_rows)
        }
    if len(source_profile_rows) != 150:
        raise ValueError(f"promoted {system} profile table must contain 150 rows")
    rho_field = (
        "independent_curve_service_relaxation"
        if system == "curve_native_v2_1"
        else "independent_dusk_service_relaxation"
    )

    def source_eligible(source_row: dict[str, str]) -> bool:
        return str(source_row.get("hard_valid", "")).strip().lower() == "true"

    try:
        ranked_source_rows = sorted(
            (
                source_row
                for source_row in source_profile_rows.values()
                if source_eligible(source_row)
                and math.isfinite(float(source_row[rho_field]))
                and math.isfinite(
                    float(source_row["minimum_route_selection_utility"])
                )
                and math.isfinite(float(source_row["mean_route_selection_utility"]))
            ),
            key=lambda source_row: (
                float(source_row[rho_field]),
                -float(source_row["minimum_route_selection_utility"]),
                -float(source_row["mean_route_selection_utility"]),
                str(source_row["profile_id"]),
            ),
        )[:5]
    except (KeyError, TypeError, ValueError) as error:
        raise ValueError(f"promoted {system} source ranking fields are invalid") from error
    if len(ranked_source_rows) != 5:
        raise ValueError(f"promoted {system} source lacks five ranked policies")
    expected_rank = {
        str(source_row["profile_id"]): rank
        for rank, source_row in enumerate(ranked_source_rows, start=1)
    }
    if len(rows) != 5 or {
        str(row.get("profile_id", "")) for row in rows if isinstance(row, dict)
    } != set(expected_rank):
        raise ValueError(
            f"promoted {system} snapshot is not the exact top-five policy shortlist"
        )
    source_fields = PROMOTION_SOURCE_FIELDS.get(system)
    if source_fields is None:
        raise KeyError(f"no promotion source-field contract for {system}")
    for row in rows:
        if not isinstance(row, dict):
            raise TypeError(f"promoted {system} profile must be an object")
        profile_id = str(row.get("profile_id", ""))
        source_row = source_profile_rows.get(profile_id)
        if source_row is None:
            raise ValueError(f"promoted {system} profile is absent from full grid")
        if int(row.get("source_rank", -1)) != expected_rank[profile_id]:
            raise ValueError(f"promoted {system} source rank changed")
        if str(row.get("description", "")) != str(source_row.get("description", "")):
            raise ValueError(f"promoted {system} description changed")
        row_digest = hashlib.sha256(
            json.dumps(
                source_row,
                sort_keys=True,
                separators=(",", ":"),
            ).encode()
        ).hexdigest()
        if row.get("source_profile_row_sha256") != row_digest:
            raise ValueError(f"promoted {system} profile row changed")
        for field in source_fields:
            try:
                promoted_value = float(row[field])
                source_value = float(source_row[field])
            except (KeyError, TypeError, ValueError, OverflowError) as error:
                raise ValueError(
                    f"promoted {system} field {field} is missing or non-numeric"
                ) from error
            if (
                not math.isfinite(promoted_value)
                or not math.isfinite(source_value)
                or promoted_value != source_value
            ):
                raise ValueError(
                    f"promoted {system} field {field} differs from its searched row"
                )
    return payload, rows


def _load_promoted_curve_profiles(
    base: CryptoSwap,
) -> tuple[NativeProfile, ...]:
    _, rows = _load_promotion_payload(
        PROMOTED_CURVE_PROFILES,
        system="curve_native_v2_1",
        expected_search_path="results/curve_controller_search/full/summary.json",
        expected_status="curve_controller_search_complete",
    )
    profiles: list[NativeProfile] = []
    seen_ids: set[str] = set()
    for row in rows:
        if not isinstance(row, dict):
            raise TypeError("promoted Curve profile must be an object")
        profile_id = str(row.get("profile_id", ""))
        description = str(row.get("description", ""))
        if (
            not profile_id.startswith("curve_grid_c")
            or profile_id in seen_ids
            or not description
        ):
            raise ValueError("invalid promoted Curve profile identity")
        seen_ids.add(profile_id)

        def finite(name: str) -> float:
            value = float(row[name])
            if not math.isfinite(value):
                raise ValueError(f"promoted Curve field {name} is not finite")
            return value

        amplification = finite("invariant_amplification")
        gamma = finite("invariant_gamma")
        if amplification != base.amplification or gamma != base.gamma:
            raise ValueError("promoted Curve invariant differs from matched base")
        model = replace(
            base,
            mid_fee=finite("mid_fee_bps") / 10_000.0,
            out_fee=finite("out_fee_bps") / 10_000.0,
            fee_gamma=finite("fee_gamma"),
        )
        config = CurveNativeConfig(
            ema_half_life_minutes=finite("ema_half_life_minutes"),
            min_adjustment_step=finite("min_adjustment_step_bps") / 10_000.0,
            allowed_extra_profit=finite("allowed_extra_profit_ppm") / 1_000_000.0,
            arb_min_profit_quote=finite("arb_min_profit_quote"),
        )
        config.validate()
        if not 0.0 <= model.mid_fee <= model.out_fee < 1.0 or model.fee_gamma <= 0.0:
            raise ValueError(f"invalid promoted Curve fees {profile_id}")
        if json.dumps(asdict(model), sort_keys=True) != json.dumps(
            row.get("model"), sort_keys=True
        ) or json.dumps(asdict(config), sort_keys=True) != json.dumps(
            row.get("config"), sort_keys=True
        ):
            raise ValueError(f"promoted Curve nested config mismatch {profile_id}")
        profiles.append(
            NativeProfile(
                "curve_native_v2_1",
                profile_id,
                model,
                config,
                description,
            )
        )
    return tuple(profiles)


def _load_promoted_dusk_profiles(
    base: CurrentDh2,
    balanced: ControlledConfig,
) -> tuple[NativeProfile, ...]:
    """Load the source-bound winner and top-ranked policy shortlist."""

    return _load_promoted_dusk_policy_profiles(
        base,
        balanced,
        path=PROMOTED_DUSK_PROFILES,
        system="dusk_native",
        expected_search_path="results/controller_search/full/summary.json",
        expected_status="controller_search_complete",
    )


def _load_promoted_dusk_policy_profiles(
    base: CurrentDh2,
    balanced: ControlledConfig,
    *,
    path: Path,
    system: str,
    expected_search_path: str,
    expected_status: str,
) -> tuple[NativeProfile, ...]:
    """Reconstruct a source-bound Dusk controller/fee shortlist."""

    _, rows = _load_promotion_payload(
        path,
        system=system,
        expected_search_path=expected_search_path,
        expected_status=expected_status,
    )
    profiles = []
    seen_ids = set()
    for row in rows:
        if not isinstance(row, dict):
            raise TypeError("promoted Dusk profile must be an object")
        profile_id = str(row.get("profile_id", ""))
        description = str(row.get("description", ""))
        if (
            not (
                profile_id.startswith("grid_c")
                or profile_id.startswith("local_grid_c")
            )
            or profile_id in seen_ids
            or not description
        ):
            raise ValueError("invalid promoted Dusk profile identity")
        seen_ids.add(profile_id)

        def finite(name: str) -> float:
            value = float(row[name])
            if not math.isfinite(value):
                raise ValueError(f"promoted Dusk field {name} is not finite")
            return value

        if (
            finite("invariant_peak_depth") != base.peak_depth
            or finite("invariant_fade_scale") != base.fade_scale
        ):
            raise ValueError(f"promoted {system} invariant differs from search base")
        ema = finite("center_ema_half_life_minutes")
        threshold_bps = finite("adjustment_threshold_bps")
        step_bps = finite("adjustment_step_bps")
        interval = int(row["min_adjustment_interval_minutes"])
        dynamic_cap_bps = finite("dynamic_cap_bps")
        divergence = finite("divergence_coefficient")
        volatility = finite("volatility_coefficient")
        volatility_half_life = finite("volatility_half_life_minutes")
        if not (
            ema > 0.0
            and threshold_bps >= step_bps > 0.0
            and interval > 0
            and 0.0 <= dynamic_cap_bps <= 10_000.0
            and divergence >= 0.0
            and volatility >= 0.0
            and volatility_half_life > 0.0
        ):
            raise ValueError(f"invalid promoted Dusk config {profile_id}")
        base_rate = balanced.fee.base_fee_rate_nad
        fee = DynamicFeeConfig(
            base_fee_rate_nad=base_rate,
            max_fee_rate_nad=base_rate
            + int(round(dynamic_cap_bps / 10_000.0 * 1_000_000_000)),
            divergence_coefficient_nad=int(round(divergence * 1_000_000_000)),
            volatility_coefficient_nad=int(round(volatility * 1_000_000_000)),
            volatility_half_life_ms=int(round(volatility_half_life * 60_000.0)),
            volatility_shock_cap_nad=100_000_000,
            volatility_accumulator_cap_nad=1_000_000_000,
        )
        fee.validate()
        config = replace(
            balanced,
            fee=fee,
            center_ema_half_life_minutes=ema,
            adjustment_threshold=threshold_bps / 10_000.0,
            adjustment_step=step_bps / 10_000.0,
            min_adjustment_interval_minutes=interval,
        )
        if json.dumps(asdict(base), sort_keys=True) != json.dumps(
            row.get("model"), sort_keys=True
        ) or json.dumps(asdict(config), sort_keys=True) != json.dumps(
            row.get("config"), sort_keys=True
        ):
            raise ValueError(f"promoted {system} nested config mismatch {profile_id}")
        profiles.append(
            NativeProfile(
                system,
                profile_id,
                base,
                config,
                description,
            )
        )
    return tuple(profiles)


def _cpmm_profiles(
    dusk_profiles: Sequence[NativeProfile],
) -> tuple[NativeProfile, ...]:
    """Build the independently selectable Dusk CPMM product family.

    CPMM receives the same controller/fee candidate family as concentrated
    DH2, but its profile is selected on synthetic training in its own right.
    This prevents the product comparator from inheriting a policy optimized for
    a different invariant.
    """

    cpmm = CurrentDh2(peak_depth=0.0, fade_scale=0.0)
    balanced = replace(
        default_controlled_config(),
        solver_haircut_ppm=10.0,
        manager_fee_bps=500,
        protocol_fee_bps=0,
    )
    promoted = _load_promoted_dusk_policy_profiles(
        cpmm,
        balanced,
        path=PROMOTED_CPMM_PROFILES,
        system="dusk_cpmm",
        expected_search_path="results/cpmm_controller_search/full/summary.json",
        expected_status="cpmm_controller_search_complete",
    )
    if promoted:
        return promoted
    profiles = []
    for profile in dusk_profiles:
        if profile.system != "dusk_native" or not isinstance(
            profile.config, ControlledConfig
        ):
            raise TypeError("CPMM profiles require Dusk controller profiles")
        profiles.append(
            NativeProfile(
                "dusk_cpmm",
                profile.profile_id,
                cpmm,
                profile.config,
                f"independent CPMM product candidate; {profile.description}",
            )
        )
    return tuple(profiles)


def _job(
    profile: NativeProfile,
    *,
    path_name: str,
    path_class: str,
    prices: Sequence[float],
    score_start_step: int,
    step_minutes: int,
    flow: dict[str, object],
    seed: int,
    routing_mode: str,
    pool_value: float = POOL_VALUE,
    source_month: str = "",
    stratum: str = "",
    analysis_slice: str,
    match_basis: str = "equal_execution",
) -> dict[str, object]:
    return {
        "profile": profile,
        "path_name": path_name,
        "path_class": path_class,
        "prices": tuple(prices),
        "score_start_step": score_start_step,
        "step_minutes": step_minutes,
        "flow": flow,
        "seed": seed,
        "routing_mode": routing_mode,
        "pool_value": pool_value,
        "source_month": source_month,
        "stratum": stratum,
        "analysis_slice": analysis_slice,
        "match_basis": match_basis,
    }


def _run_job(job: dict[str, object]) -> dict[str, object]:
    profile = job["profile"]
    if not isinstance(profile, NativeProfile):
        raise TypeError("invalid native profile")
    prices = job["prices"]
    if not isinstance(prices, tuple):
        raise TypeError("prices must be frozen before worker dispatch")
    flow = job["flow"]
    if not isinstance(flow, dict):
        raise TypeError("flow must be a dictionary")
    seed = int(job["seed"])
    step_minutes = int(job["step_minutes"])
    pool_value = float(job["pool_value"])
    tape = generate_native_order_tape(
        prices,
        pool_value=pool_value,
        daily_volume_pool_ratio=float(flow["daily_volume_pool_ratio"]),
        informed_share=float(flow["informed_share"]),
        informed_accuracy=float(flow["informed_accuracy"]),
        signal_horizon_minutes=5,
        mean_arrivals_per_minute=float(
            flow.get("mean_arrivals_per_minute", ARRIVALS_PER_MINUTE)
        ),
        seed=seed,
        step_minutes=step_minutes,
    )
    routing = RoutingConfig(
        mode=str(job["routing_mode"]),
        outside_depth_multiple=1.0,
        outside_fee_bps=5.0,
    )
    common = dict(
        path_name=str(job["path_name"]),
        prices=prices,
        tape=tape,
        score_start_index=int(job["score_start_step"]),
        arb_latency_minutes=int(flow["arb_latency_minutes"]),
        pool_value=pool_value,
        config=profile.config,
        routing=routing,
        step_minutes=step_minutes,
    )
    if profile.system == "curve_native_v2_1":
        if not isinstance(profile.model, CryptoSwap):
            raise TypeError("Curve profile requires CryptoSwap")
        result = simulate_curve_native_tape(profile.model, **common)
    elif profile.system in {
        "dusk_native",
        "dusk_cpmm",
        "dusk_cpmm_matched_policy",
    }:
        if not isinstance(profile.model, CurrentDh2):
            raise TypeError("Dusk profile requires CurrentDh2")
        result = simulate_dusk_native_tape(profile.model, **common)
        if profile.system in {"dusk_cpmm", "dusk_cpmm_matched_policy"}:
            result["system"] = profile.system
            result["model"] = "cpmm"
    else:
        raise ValueError(f"unknown system {profile.system}")
    result.update(
        {
            "profile_id": profile.profile_id,
            "profile_description": profile.description,
            "path_class": job["path_class"],
            "source_month": job["source_month"],
            "stratum": job["stratum"],
            "flow_regime": flow["flow_regime"],
            "informed_flow_share": flow["informed_share"],
            "informed_accuracy": flow["informed_accuracy"],
            "daily_volume_pool_ratio": flow["daily_volume_pool_ratio"],
            "mean_arrivals_per_minute": flow.get(
                "mean_arrivals_per_minute", ARRIVALS_PER_MINUTE
            ),
            "seed": seed,
            "analysis_slice": job["analysis_slice"],
            "match_basis": job["match_basis"],
        }
    )
    return result


def _run_jobs(
    jobs: Sequence[dict[str, object]],
    workers: int,
    label: str,
) -> list[dict[str, object]]:
    started = time.perf_counter()
    if workers <= 1:
        rows = [_run_job(job) for job in jobs]
    else:
        with ProcessPoolExecutor(max_workers=workers) as executor:
            rows = list(executor.map(_run_job, jobs, chunksize=1))
    elapsed = time.perf_counter() - started
    print(f"{label}: {len(rows)} rows in {elapsed:.1f}s", flush=True)
    return rows


def _profile_summary(
    rows: Sequence[dict[str, object]],
    profiles: Sequence[NativeProfile],
) -> tuple[
    list[dict[str, object]],
    dict[str, NativeProfile],
    list[str],
]:
    summaries: list[dict[str, object]] = []
    selected: dict[str, NativeProfile] = {}
    infeasible_systems: list[str] = []
    for system in PROFILE_SELECTION_SYSTEMS:
        system_profiles = [profile for profile in profiles if profile.system == system]
        for profile in system_profiles:
            values = [
                row
                for row in rows
                if row["system"] == system and row["profile_id"] == profile.profile_id
            ]
            if not values:
                raise AssertionError(f"profile {system}:{profile.profile_id} has no rows")
            elastic_values = [
                row for row in values if row["routing_mode"] == "elastic"
            ]
            finite_fields = (
                "net_lp_return_after_protocol_fee_bps",
                "captured_user_volume_fraction",
                "selected_execution_cost_bps",
                "depth_degradation_fraction",
                "marginal_tracking_stale_fraction",
                "accounting_identity_error",
            )
            all_training_metrics_finite = all(
                math.isfinite(float(row[field]))
                for row in values
                for field in finite_fields
            )
            route_metrics: dict[str, dict[str, float]] = {}
            for routing_mode in ("fixed", "elastic"):
                route_values = [
                    row for row in values if row["routing_mode"] == routing_mode
                ]
                if not route_values:
                    raise AssertionError(
                        f"profile {system}:{profile.profile_id} lacks {routing_mode} rows"
                    )
                route_returns = [
                    float(row["net_lp_return_after_protocol_fee_bps"])
                    for row in route_values
                ]
                flow_metrics = []
                for flow in FLOW_REGIMES:
                    regime = str(flow["flow_regime"])
                    regime_values = [
                        row
                        for row in route_values
                        if row["flow_regime"] == regime
                    ]
                    if not regime_values:
                        raise AssertionError(
                            f"profile {system}:{profile.profile_id} lacks "
                            f"{routing_mode}:{regime} rows"
                        )
                    flow_metrics.append(
                        {
                            "median_capture": statistics.median(
                                float(row["captured_user_volume_fraction"])
                                for row in regime_values
                            ),
                            "p10_capture": _percentile(
                                [
                                    float(row["captured_user_volume_fraction"])
                                    for row in regime_values
                                ],
                                0.10,
                            ),
                            "median_depth_degradation": statistics.median(
                                float(row["depth_degradation_fraction"])
                                for row in regime_values
                            ),
                            "median_marginal_tracking_stale": statistics.median(
                                float(row["marginal_tracking_stale_fraction"])
                                for row in regime_values
                            ),
                            "p90_selected_cost": _percentile(
                                [
                                    float(row["selected_execution_cost_bps"])
                                    for row in regime_values
                                ],
                                0.90,
                            ),
                        }
                    )
                route_median_return = statistics.median(route_returns)
                route_p10_return = _percentile(route_returns, 0.10)
                route_metrics[routing_mode] = {
                    "median_net_lp_return_bps": route_median_return,
                    "p10_net_lp_return_bps": route_p10_return,
                    "selection_utility": (
                        0.8 * route_median_return + 0.2 * route_p10_return
                    ),
                    "median_captured_volume_fraction": statistics.median(
                        float(row["captured_user_volume_fraction"])
                        for row in route_values
                    ),
                    "median_selected_execution_cost_bps": statistics.median(
                        float(row["selected_execution_cost_bps"])
                        for row in route_values
                    ),
                    "p90_selected_execution_cost_bps": _percentile(
                        [
                            float(row["selected_execution_cost_bps"])
                            for row in route_values
                        ],
                        0.90,
                    ),
                    "median_depth_degradation_fraction": statistics.median(
                        float(row["depth_degradation_fraction"])
                        for row in route_values
                    ),
                    "median_marginal_tracking_stale_fraction": statistics.median(
                        float(row["marginal_tracking_stale_fraction"])
                        for row in route_values
                    ),
                    "worst_regime_median_capture_fraction": min(
                        row["median_capture"] for row in flow_metrics
                    ),
                    "worst_regime_p10_capture_fraction": min(
                        row["p10_capture"] for row in flow_metrics
                    ),
                    "worst_regime_median_depth_degradation_fraction": max(
                        row["median_depth_degradation"] for row in flow_metrics
                    ),
                    "worst_regime_median_marginal_tracking_stale_fraction": max(
                        row["median_marginal_tracking_stale"]
                        for row in flow_metrics
                    ),
                    "worst_regime_p90_selected_execution_cost_bps": max(
                        row["p90_selected_cost"] for row in flow_metrics
                    ),
                }
            returns = [
                float(row["net_lp_return_after_protocol_fee_bps"])
                for row in elastic_values
            ]
            captures = [
                float(row["captured_user_volume_fraction"])
                for row in elastic_values
            ]
            depth_degradation = [
                float(row["depth_degradation_fraction"]) for row in values
            ]
            marginal_tracking_stale = [
                float(row["marginal_tracking_stale_fraction"])
                for row in values
            ]
            costs = [float(row["selected_execution_cost_bps"]) for row in values]
            errors = [abs(float(row["accounting_identity_error"])) for row in values]
            summary = {
                "system": system,
                "profile_id": profile.profile_id,
                "description": profile.description,
                "training_cells": len(values),
                "training_elastic_cells": len(elastic_values),
                "all_training_metrics_finite": all_training_metrics_finite,
                "max_controller_external_price_reads": max(
                    int(row["controller_external_price_reads"]) for row in values
                ),
                "max_arbitrage_capped_event_count": max(
                    int(row.get("arbitrage_capped_event_count", 0))
                    for row in values
                ),
                "max_arbitrage_numerical_failure_count": max(
                    int(row.get("arbitrage_numerical_failure_count", 0))
                    for row in values
                ),
                "median_net_lp_return_bps": statistics.median(returns),
                "p10_net_lp_return_bps": _percentile(returns, 0.10),
                "median_captured_volume_fraction": statistics.median(captures),
                "median_depth_degradation_fraction": statistics.median(
                    depth_degradation
                ),
                "median_marginal_tracking_stale_fraction": statistics.median(
                    marginal_tracking_stale
                ),
                "median_selected_execution_cost_bps": statistics.median(costs),
                "p90_selected_execution_cost_bps": _percentile(costs, 0.90),
                "max_accounting_error": max(errors),
                "minimum_route_selection_utility": min(
                    row["selection_utility"] for row in route_metrics.values()
                ),
                "mean_route_selection_utility": statistics.mean(
                    row["selection_utility"] for row in route_metrics.values()
                ),
                "minimum_route_median_capture_fraction": min(
                    row["median_captured_volume_fraction"]
                    for row in route_metrics.values()
                ),
                "maximum_route_p90_selected_execution_cost_bps": max(
                    row["p90_selected_execution_cost_bps"]
                    for row in route_metrics.values()
                ),
            }
            for routing_mode, metrics in route_metrics.items():
                for field, value in metrics.items():
                    summary[f"{routing_mode}_{field}"] = value
            summaries.append(summary)

    minimum_relaxation_by_system: dict[str, float] = {}
    for system in PROFILE_SELECTION_SYSTEMS:
        candidates = [row for row in summaries if row["system"] == system]
        for row in candidates:
            if not bool(row["all_training_metrics_finite"]):
                row["service_relaxation_required"] = math.inf
            else:
                row["service_relaxation_required"] = _required_service_relaxation(
                    row
                )
        finite_relaxations = [
            float(row["service_relaxation_required"])
            for row in candidates
            if math.isfinite(float(row["service_relaxation_required"]))
        ]
        if not finite_relaxations:
            infeasible_systems.append(system)
        else:
            minimum_relaxation_by_system[system] = min(finite_relaxations)
    if infeasible_systems:
        for row in summaries:
            row["eligible_for_lock"] = False
            row["selected_for_held_out"] = False
        return summaries, selected, infeasible_systems

    selection_groups = (
        ("concentrated_candidates", CANDIDATE_SYSTEMS),
        ("cpmm_product", ("dusk_cpmm",)),
    )
    for group_name, group_systems in selection_groups:
        group_relaxation = max(
            minimum_relaxation_by_system[system] for system in group_systems
        )
        group_rows = [
            row for row in summaries if row["system"] in group_systems
        ]
        for row in group_rows:
            row["selection_group"] = group_name
            row["selection_group_service_relaxation"] = group_relaxation
            row["original_service_envelope_pass"] = (
                float(row["service_relaxation_required"]) <= 1e-12
            )
            original_failures = _failed_service_checks(row, 0.0)
            relaxed_failures = _failed_service_checks(
                row, group_relaxation + 1e-12
            )
            row["failed_original_service_checks"] = ";".join(
                original_failures
            )
            row["original_service_check_failures"] = len(original_failures)
            row["failed_relaxed_service_checks"] = ";".join(
                relaxed_failures
            )
            row["relaxed_service_check_failures"] = len(relaxed_failures)

        for system in group_systems:
            system_profiles = [
                profile for profile in profiles if profile.system == system
            ]
            candidates = [
                row for row in summaries if row["system"] == system
            ]
            eligible = [
                row
                for row in candidates
                if math.isfinite(float(row["service_relaxation_required"]))
                and float(row["service_relaxation_required"])
                <= group_relaxation + 1e-12
                and int(row["relaxed_service_check_failures"]) == 0
            ]
            if not eligible:
                infeasible_systems.append(system)
                continue
            winner = sorted(
                eligible,
                key=lambda row: (
                    float(row["service_relaxation_required"]),
                    -float(row["minimum_route_selection_utility"]),
                    -float(row["mean_route_selection_utility"]),
                    -float(row["minimum_route_median_capture_fraction"]),
                    float(row["maximum_route_p90_selected_execution_cost_bps"]),
                    str(row["profile_id"]),
                ),
            )[0]
            for row in candidates:
                row["eligible_for_lock"] = row in eligible
                row["selected_for_held_out"] = row is winner
            selected[system] = next(
                profile
                for profile in system_profiles
                if profile.profile_id == winner["profile_id"]
            )
    return summaries, selected, infeasible_systems


def _training_jobs(profiles: Sequence[NativeProfile]) -> list[dict[str, object]]:
    jobs = []
    for name, scoring_prices in synthetic_paths(length=720).items():
        mature_prices = [scoring_prices[0]] * 1_440 + scoring_prices
        prices = mature_prices[::PRIMARY_STEP_MINUTES]
        for flow in FLOW_REGIMES:
            for seed in PRIMARY_SEEDS:
                for routing_mode in ("fixed", "elastic"):
                    for profile in profiles:
                        jobs.append(
                            _job(
                                profile,
                                path_name=name,
                                path_class="synthetic",
                                prices=prices,
                                score_start_step=1_440 // PRIMARY_STEP_MINUTES,
                                step_minutes=PRIMARY_STEP_MINUTES,
                                flow=dict(flow),
                                seed=seed,
                                routing_mode=routing_mode,
                                analysis_slice="synthetic_profile_selection",
                            )
                        )
    return jobs


def _quick_training_jobs(
    profiles: Sequence[NativeProfile],
) -> list[dict[str, object]]:
    return [
        job
        for job in _training_jobs(profiles)
        if job["path_name"] == "flat" and job["seed"] == 7
    ]


def _held_out_jobs(
    selected: dict[str, NativeProfile],
    *,
    quick: bool,
) -> list[dict[str, object]]:
    jobs = []
    windows = list(native_historical_windows().values())
    if quick:
        windows = windows[:3]
    for window in windows:
        prices = window.prices[::PRIMARY_STEP_MINUTES]
        score_start = window.burn_in_minutes // PRIMARY_STEP_MINUTES
        for flow in FLOW_REGIMES:
            for seed in PRIMARY_SEEDS:
                for routing_mode in ("fixed", "elastic"):
                    for system in CANDIDATE_SYSTEMS:
                        profile = selected[system]
                        jobs.append(
                            _job(
                                profile,
                                path_name=f"historical_{window.name}",
                                path_class="historical",
                                prices=prices,
                                score_start_step=score_start,
                                step_minutes=PRIMARY_STEP_MINUTES,
                                flow=dict(flow),
                                seed=seed,
                                routing_mode=routing_mode,
                                source_month=window.source_month,
                                stratum=window.stratum,
                                analysis_slice="held_out_primary",
                            )
                        )
    return jobs


def _cpmm_jobs(
    cpmm_selected: NativeProfile,
    *,
    quick: bool,
) -> list[dict[str, object]]:
    """Replay the independently selected CPMM product comparator."""

    if (
        cpmm_selected.system != "dusk_cpmm"
        or not isinstance(cpmm_selected.config, ControlledConfig)
        or not isinstance(cpmm_selected.model, CurrentDh2)
        or cpmm_selected.model.peak_depth != 0.0
        or cpmm_selected.model.fade_scale != 0.0
    ):
        raise TypeError("CPMM product replay requires a selected CPMM profile")
    windows = list(native_historical_windows().values())
    if quick:
        windows = windows[:3]
    jobs = []
    for window in windows:
        prices = window.prices[::PRIMARY_STEP_MINUTES]
        score_start = window.burn_in_minutes // PRIMARY_STEP_MINUTES
        for flow in FLOW_REGIMES:
            for seed in PRIMARY_SEEDS:
                for routing_mode in ("fixed", "elastic"):
                    jobs.append(
                        _job(
                            cpmm_selected,
                            path_name=f"historical_{window.name}",
                            path_class="historical",
                            prices=prices,
                            score_start_step=score_start,
                            step_minutes=PRIMARY_STEP_MINUTES,
                            flow=dict(flow),
                            seed=seed,
                            routing_mode=routing_mode,
                            source_month=window.source_month,
                            stratum=window.stratum,
                            analysis_slice="cpmm_product_fallback",
                        )
                    )
    return jobs


def _cpmm_matched_policy_jobs(
    dusk_selected: NativeProfile,
    *,
    quick: bool,
) -> list[dict[str, object]]:
    """Replay zero concentration under the selected current-DH2 policy.

    This arm is an invariant ablation: it deliberately preserves the fee and
    controller parameters selected for concentrated current DH2.  It is not
    the product comparator and therefore never participates in winner gates.
    """

    if (
        dusk_selected.system != "dusk_native"
        or not isinstance(dusk_selected.config, ControlledConfig)
    ):
        raise TypeError("matched-policy CPMM requires a selected Dusk profile")
    profile = NativeProfile(
        "dusk_cpmm_matched_policy",
        dusk_selected.profile_id,
        CurrentDh2(peak_depth=0.0, fade_scale=0.0),
        dusk_selected.config,
        (
            "matched-policy invariant ablation; selected current-DH2 fee and "
            "center controller preserved; only concentration disabled"
        ),
    )
    windows = list(native_historical_windows().values())
    if quick:
        windows = windows[:3]
    return [
        _job(
            profile,
            path_name=f"historical_{window.name}",
            path_class="historical",
            prices=window.prices[::PRIMARY_STEP_MINUTES],
            score_start_step=window.burn_in_minutes // PRIMARY_STEP_MINUTES,
            step_minutes=PRIMARY_STEP_MINUTES,
            flow=dict(flow),
            seed=seed,
            routing_mode=routing_mode,
            source_month=window.source_month,
            stratum=window.stratum,
            analysis_slice="cpmm_matched_policy_ablation",
        )
        for window in windows
        for flow in FLOW_REGIMES
        for seed in PRIMARY_SEEDS
        for routing_mode in ("fixed", "elastic")
    ]


def _counterfactual_jobs(
    selected: dict[str, NativeProfile],
    *,
    quick: bool,
) -> list[dict[str, object]]:
    curve = selected["curve_native_v2_1"]
    dusk = selected["dusk_native"]
    if not isinstance(curve.config, CurveNativeConfig) or not isinstance(
        dusk.config, ControlledConfig
    ):
        raise TypeError("selected profile configuration mismatch")
    disabled = (
        replace(
            curve,
            profile_id=f"{curve.profile_id}_controller_off",
            config=replace(curve.config, allowed_extra_profit=10.0),
            description=f"{curve.description}; center movement disabled",
        ),
        replace(
            dusk,
            profile_id=f"{dusk.profile_id}_controller_off",
            config=replace(
                dusk.config,
                recenter_enabled=False,
            ),
            description=f"{dusk.description}; center movement disabled",
        ),
    )
    windows = list(native_historical_windows().values())
    if quick:
        windows = windows[:3]
    return [
        _job(
            profile,
            path_name=f"historical_{window.name}",
            path_class="historical",
            prices=window.prices[::PRIMARY_STEP_MINUTES],
            score_start_step=window.burn_in_minutes // PRIMARY_STEP_MINUTES,
            step_minutes=PRIMARY_STEP_MINUTES,
            flow=dict(flow),
            seed=seed,
            routing_mode=routing_mode,
            source_month=window.source_month,
            stratum=window.stratum,
            analysis_slice="controller_off_counterfactual",
        )
        for window in windows
        for flow in FLOW_REGIMES
        for seed in PRIMARY_SEEDS
        for routing_mode in ("fixed", "elastic")
        for profile in disabled
    ]


def _sensitivity_jobs(
    profiles: Sequence[NativeProfile],
    selected: dict[str, NativeProfile],
    *,
    quick: bool,
) -> tuple[list[dict[str, object]], list[dict[str, object]], list[dict[str, object]]]:
    windows = [
        window
        for window in native_historical_windows().values()
        if window.source_month == "2025-04"
    ]
    if quick:
        windows = windows[:1]
    neighborhood = [
        _job(
            profile,
            path_name=f"historical_{window.name}",
            path_class="historical",
            prices=window.prices[::PRIMARY_STEP_MINUTES],
            score_start_step=window.burn_in_minutes // PRIMARY_STEP_MINUTES,
            step_minutes=PRIMARY_STEP_MINUTES,
            flow=dict(flow),
            seed=seed,
            routing_mode=routing_mode,
            source_month=window.source_month,
            stratum=window.stratum,
            analysis_slice="promoted_policy_shortlist",
        )
        for window in windows
        for flow in FLOW_REGIMES
        for seed in PRIMARY_SEEDS
        for routing_mode in ("fixed", "elastic")
        for profile in profiles
    ]
    scale = [
        _job(
            profile,
            path_name=f"historical_{window.name}",
            path_class="historical",
            prices=window.prices[::PRIMARY_STEP_MINUTES],
            score_start_step=window.burn_in_minutes // PRIMARY_STEP_MINUTES,
            step_minutes=PRIMARY_STEP_MINUTES,
            flow=dict(flow),
            seed=seed,
            routing_mode=routing_mode,
            pool_value=pool_value,
            source_month=window.source_month,
            stratum=window.stratum,
            analysis_slice="pool_scale_sensitivity",
        )
        for window in windows
        for flow in FLOW_REGIMES
        for seed in PRIMARY_SEEDS
        for routing_mode in ("fixed", "elastic")
        for system in CANDIDATE_SYSTEMS
        for profile in (selected[system],)
        for pool_value in (250_000.0, 2_000_000.0, 20_000_000.0)
    ]
    cadence = [
        _job(
            profile,
            path_name=f"historical_{window.name}",
            path_class="historical",
            prices=window.prices,
            score_start_step=window.burn_in_minutes,
            step_minutes=1,
            flow=dict(flow),
            seed=seed,
            routing_mode=routing_mode,
            source_month=window.source_month,
            stratum=window.stratum,
            analysis_slice="one_minute_cadence",
        )
        for window in windows
        for flow in FLOW_REGIMES
        for seed in PRIMARY_SEEDS
        for routing_mode in ("fixed", "elastic")
        for system in CANDIDATE_SYSTEMS
        for profile in (selected[system],)
    ]
    return neighborhood, scale, cadence


def _volume_sensitivity_jobs(
    selected: dict[str, NativeProfile],
    *,
    quick: bool,
) -> list[dict[str, object]]:
    """Replay locked profiles from thin through highly routed volume.

    Arrival frequency is held fixed so this arm isolates notional throughput;
    it does not reselect controllers and therefore reports transfer robustness,
    not globally optimal high-volume policies.
    """

    windows = [
        window
        for window in native_historical_windows().values()
        if window.source_month == "2025-04"
    ]
    if quick:
        windows = windows[:1]
    templates = tuple(
        flow
        for flow in FLOW_REGIMES
        if flow["flow_regime"] in {"benign", "toxic_delayed"}
    )
    profiles = tuple(selected[system] for system in PROFILE_SELECTION_SYSTEMS)
    jobs: list[dict[str, object]] = []
    for window in windows:
        for template in templates:
            for volume_ratio in VOLUME_SENSITIVITY_RATIOS:
                flow = dict(template)
                flow["flow_regime"] = (
                    f"{template['flow_regime']}_volume_{volume_ratio:g}x"
                )
                flow["daily_volume_pool_ratio"] = volume_ratio
                for seed in PRIMARY_SEEDS:
                    for routing_mode in ("fixed", "elastic"):
                        for profile in profiles:
                            jobs.append(
                                _job(
                                    profile,
                                    path_name=f"historical_{window.name}",
                                    path_class="historical",
                                    prices=window.prices[::PRIMARY_STEP_MINUTES],
                                    score_start_step=(
                                        window.burn_in_minutes
                                        // PRIMARY_STEP_MINUTES
                                    ),
                                    step_minutes=PRIMARY_STEP_MINUTES,
                                    flow=flow,
                                    seed=seed,
                                    routing_mode=routing_mode,
                                    source_month=window.source_month,
                                    stratum=window.stratum,
                                    analysis_slice="locked_profile_volume_sensitivity",
                                )
                            )
    return jobs


def _basis_sensitivity_jobs(
    selected: dict[str, NativeProfile],
    *,
    quick: bool,
) -> list[dict[str, object]]:
    windows = list(native_historical_windows().values())
    if quick:
        windows = windows[:3]
    jobs: list[dict[str, object]] = []
    basis_models = {
        "nearest_full_profile": _matched_models("nearest_full_profile"),
        "equal_jump_risk": _matched_models("equal_jump_risk"),
    }
    deployed_curve = CryptoSwap(amplification=10.0, gamma=0.000145)
    _, deployed_dusk, _, _ = match_to_curve(deployed_curve)
    basis_models["v21_deployed_invariant"] = (deployed_curve, deployed_dusk)
    for basis, (curve_model, dusk_model) in basis_models.items():
        profiles = (
            replace(
                selected["curve_native_v2_1"],
                profile_id=f"{selected['curve_native_v2_1'].profile_id}_{basis}",
                model=replace(
                    selected["curve_native_v2_1"].model,
                    amplification=curve_model.amplification,
                    gamma=curve_model.gamma,
                ),
            ),
            replace(
                selected["dusk_native"],
                profile_id=f"{selected['dusk_native'].profile_id}_{basis}",
                model=dusk_model,
            ),
        )
        for window in windows:
            for flow in FLOW_REGIMES:
                for seed in PRIMARY_SEEDS:
                    for routing_mode in ("fixed", "elastic"):
                        for profile in profiles:
                            jobs.append(
                                _job(
                                    profile,
                                    path_name=f"historical_{window.name}",
                                    path_class="historical",
                                    prices=window.prices[::PRIMARY_STEP_MINUTES],
                                    score_start_step=(
                                        window.burn_in_minutes // PRIMARY_STEP_MINUTES
                                    ),
                                    step_minutes=PRIMARY_STEP_MINUTES,
                                    flow=dict(flow),
                                    seed=seed,
                                    routing_mode=routing_mode,
                                    source_month=window.source_month,
                                    stratum=window.stratum,
                                    analysis_slice="matching_basis_sensitivity",
                                    match_basis=basis,
                                )
                            )
    return jobs


def _paired_native_rows(rows: Sequence[dict[str, object]]) -> list[dict[str, object]]:
    keys = (
        "path",
        "source_month",
        "stratum",
        "flow_regime",
        "daily_volume_pool_ratio",
        "seed",
        "routing_mode",
        "initial_pool_value",
        "step_minutes",
        "match_basis",
    )
    grouped: dict[tuple[object, ...], dict[str, dict[str, object]]] = {}
    for row in rows:
        grouped.setdefault(tuple(row[key] for key in keys), {})[str(row["system"])] = row
    output = []
    for key, systems in grouped.items():
        if set(systems) != {"curve_native_v2_1", "dusk_native"}:
            raise AssertionError(f"incomplete native pair {key}: {set(systems)}")
        curve = systems["curve_native_v2_1"]
        dusk = systems["dusk_native"]
        output.append(
            {
                **{field: value for field, value in zip(keys, key)},
                "dusk_minus_curve_net_lp_return_after_protocol_fee_bps": float(
                    dusk["net_lp_return_after_protocol_fee_bps"]
                )
                - float(curve["net_lp_return_after_protocol_fee_bps"]),
                "dusk_minus_curve_gross_net_lp_return_bps": float(
                    dusk["net_lp_return_vs_hold_bps"]
                )
                - float(curve["net_lp_return_vs_hold_bps"]),
                "dusk_minus_curve_net_lp_pnl_after_protocol_fee": float(
                    dusk["net_lp_pnl_after_protocol_fee"]
                )
                - float(curve["net_lp_pnl_after_protocol_fee"]),
                "dusk_minus_curve_selected_execution_cost_bps": float(
                    dusk["selected_execution_cost_bps"]
                )
                - float(curve["selected_execution_cost_bps"]),
                "dusk_minus_curve_captured_volume_fraction": float(
                    dusk["captured_user_volume_fraction"]
                )
                - float(curve["captured_user_volume_fraction"]),
                "dusk_minus_curve_depth_degradation_fraction": float(
                    dusk["depth_degradation_fraction"]
                )
                - float(curve["depth_degradation_fraction"]),
                "dusk_minus_curve_marginal_tracking_stale_fraction": float(
                    dusk["marginal_tracking_stale_fraction"]
                )
                - float(curve["marginal_tracking_stale_fraction"]),
                "dusk_minus_curve_realized_arb_profit_bps_tvl": (
                    float(dusk["realized_arb_profit"])
                    - float(curve["realized_arb_profit"])
                )
                / float(dusk["initial_pool_value"])
                * 10_000.0,
                "dusk_wins_lp_return_after_protocol_fee": float(
                    dusk["net_lp_return_after_protocol_fee_bps"]
                )
                > float(curve["net_lp_return_after_protocol_fee_bps"]),
            }
        )
    return output


def _dusk_fee_split_sensitivity_rows(
    rows: Sequence[dict[str, object]],
) -> list[dict[str, object]]:
    """Revalue Dusk LP ownership under alternate base-fee revenue shares.

    The split is accounting-only: it does not change quotes, reserves, the
    dynamic surcharge, or the controller path.  The configured 5% liability
    can therefore be scaled exactly without replaying the same tape.
    """

    keys = (
        "path",
        "source_month",
        "stratum",
        "flow_regime",
        "daily_volume_pool_ratio",
        "seed",
        "routing_mode",
    )
    grouped: dict[tuple[object, ...], dict[str, dict[str, object]]] = {}
    for row in rows:
        grouped.setdefault(tuple(row[key] for key in keys), {})[str(row["system"])] = row
    output: list[dict[str, object]] = []
    for key, systems in grouped.items():
        curve = systems["curve_native_v2_1"]
        dusk = systems["dusk_native"]
        configured_share_bps = int(dusk["manager_fee_bps"]) + int(
            dusk["protocol_fee_bps"]
        )
        if configured_share_bps <= 0:
            raise ValueError("fee-split sensitivity requires a positive base share")
        for total_share_bps in (0, 500, 2_500, 10_000):
            scale = total_share_bps / configured_share_bps
            net_final = float(dusk["final_lp_wealth"]) - scale * float(
                dusk["final_non_lp_fee_liability_value"]
            )
            net_hold = float(dusk["hold_value"]) - scale * float(
                dusk["starting_non_lp_fee_liability_value"]
            )
            dusk_return = (net_final - net_hold) / net_hold * 10_000.0
            curve_return = float(curve["net_lp_return_after_protocol_fee_bps"])
            output.append(
                {
                    **{field: value for field, value in zip(keys, key)},
                    "dusk_total_base_fee_revenue_share_bps": total_share_bps,
                    "dusk_net_lp_return_bps": dusk_return,
                    "curve_net_lp_return_after_admin_liability_bps": curve_return,
                    "dusk_minus_curve_net_lp_return_bps": (
                        dusk_return - curve_return
                    ),
                }
            )
    return output


def _tail_mean(values: Sequence[float], probability: float, *, lower: bool) -> float:
    ordered = sorted(values)
    count = max(1, math.ceil(len(ordered) * probability))
    tail = ordered[:count] if lower else ordered[-count:]
    return statistics.mean(tail)


def _neighborhood_candidate_win_fraction(
    rows: Sequence[dict[str, object]],
    selected: dict[str, NativeProfile],
    *,
    sign: float,
    return_field: str,
) -> tuple[float, float]:
    selected_curve = selected["curve_native_v2_1"].profile_id
    selected_dusk = selected["dusk_native"].profile_id
    grouping_keys = ("path", "flow_regime", "seed", "routing_mode")
    by_path: dict[tuple[object, ...], list[dict[str, object]]] = {}
    for row in rows:
        by_path.setdefault(tuple(row[key] for key in grouping_keys), []).append(row)
    deltas: list[float] = []
    for path_rows in by_path.values():
        curve_rows = [row for row in path_rows if row["system"] == "curve_native_v2_1"]
        dusk_rows = [row for row in path_rows if row["system"] == "dusk_native"]
        curve_selected = next(
            row for row in curve_rows if row["profile_id"] == selected_curve
        )
        dusk_selected = next(
            row for row in dusk_rows if row["profile_id"] == selected_dusk
        )
        deltas.extend(
            sign
            * (
                float(row[return_field])
                - float(curve_selected[return_field])
            )
            for row in dusk_rows
        )
        deltas.extend(
            sign
            * (
                float(dusk_selected[return_field])
                - float(row[return_field])
            )
            for row in curve_rows
        )
    return (
        sum(value > 0 for value in deltas) / len(deltas),
        statistics.median(deltas),
    )


def _flag_is_true(value: object) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() == "true"
    return bool(value)


def _eligible_neighborhood_rows(
    profile_rows: Sequence[dict[str, object]],
    neighborhood: Sequence[dict[str, object]],
    selected: dict[str, NativeProfile],
) -> tuple[list[dict[str, object]], dict[str, dict[str, int | bool]]]:
    """Keep only variants admitted by the common synthetic service envelope."""

    systems = ("curve_native_v2_1", "dusk_native")
    diagnostics: dict[tuple[str, str], dict[str, object]] = {}
    for row in profile_rows:
        if str(row["system"]) not in systems:
            continue
        key = (str(row["system"]), str(row["profile_id"]))
        if key in diagnostics:
            raise AssertionError(f"duplicate profile diagnostic {key}")
        diagnostics[key] = row
    expected_profiles = {
        system: {profile_id for row_system, profile_id in diagnostics if row_system == system}
        for system in systems
    }
    if any(not expected_profiles[system] for system in systems):
        raise AssertionError("missing profile diagnostics for policy-shortlist system")

    rows_by_profile: dict[tuple[str, str], list[dict[str, object]]] = {
        key: [] for key in diagnostics
    }
    for row in neighborhood:
        key = (str(row["system"]), str(row["profile_id"]))
        if key not in rows_by_profile:
            raise AssertionError(f"policy-shortlist row lacks profile diagnostic {key}")
        rows_by_profile[key].append(row)
    if any(not rows for rows in rows_by_profile.values()):
        missing = [key for key, rows in rows_by_profile.items() if not rows]
        raise AssertionError(f"policy-shortlist matrix misses profiles: {missing}")
    row_counts = {len(rows) for rows in rows_by_profile.values()}
    if len(row_counts) != 1:
        raise AssertionError(
            f"policy-shortlist profile matrices have unequal row counts: {row_counts}"
        )

    eligible_ids = {
        system: {
            profile_id
            for (row_system, profile_id), row in diagnostics.items()
            if row_system == system and _flag_is_true(row.get("eligible_for_lock"))
        }
        for system in systems
    }
    for system in systems:
        selected_id = selected[system].profile_id
        if selected_id not in eligible_ids[system]:
            raise AssertionError(
                f"selected profile {system}:{selected_id} is not shortlist eligible"
            )

    eligible = [
        row
        for row in neighborhood
        if str(row["profile_id"]) in eligible_ids[str(row["system"])]
    ]
    counts: dict[str, dict[str, int | bool]] = {}
    for system in systems:
        system_rows = [row for row in neighborhood if row["system"] == system]
        eligible_system_rows = [row for row in eligible if row["system"] == system]
        counts[system] = {
            "total_profiles": len(expected_profiles[system]),
            "eligible_profiles": len(eligible_ids[system]),
            "excluded_profiles": (
                len(expected_profiles[system]) - len(eligible_ids[system])
            ),
            "total_rows": len(system_rows),
            "eligible_rows": len(eligible_system_rows),
            "excluded_rows": len(system_rows) - len(eligible_system_rows),
            "selected_profile_eligible": True,
        }
    return eligible, counts


def _candidate_group_metrics(
    cells: Sequence[dict[str, object]],
    *,
    sign: float,
    return_delta_field: str,
    execution_delta_field: str = "dusk_minus_curve_selected_execution_cost_bps",
    capture_delta_field: str = "dusk_minus_curve_captured_volume_fraction",
    marginal_tracking_stale_delta_field: str = (
        "dusk_minus_curve_marginal_tracking_stale_fraction"
    ),
) -> dict[str, float]:
    """Summarize candidate-minus-other outcomes for one comparable group."""

    if not cells:
        raise ValueError("decision group cannot be empty")
    advantages = [sign * float(row[return_delta_field]) for row in cells]
    execution_regressions = [
        sign * float(row[execution_delta_field])
        for row in cells
    ]
    capture_advantages = [
        sign * float(row[capture_delta_field])
        for row in cells
    ]
    marginal_tracking_stale_regressions = [
        sign * float(row[marginal_tracking_stale_delta_field])
        for row in cells
    ]
    by_window: dict[str, list[float]] = {}
    for row, advantage in zip(cells, advantages):
        by_window.setdefault(str(row["path"]), []).append(advantage)
    window_medians = [statistics.median(values) for values in by_window.values()]
    return {
        "cells": float(len(cells)),
        "median_return_advantage_bps": statistics.median(advantages),
        "bottom_decile_mean_return_advantage_bps": _tail_mean(
            advantages, 0.10, lower=True
        ),
        "cell_win_fraction": sum(value > 0 for value in advantages) / len(advantages),
        "window_median_win_fraction": sum(value > 0 for value in window_medians)
        / len(window_medians),
        "p90_execution_regression_bps": _percentile(
            execution_regressions, 0.90
        ),
        "p10_capture_advantage_fraction": _percentile(capture_advantages, 0.10),
        "p90_marginal_tracking_stale_regression_fraction": _percentile(
            marginal_tracking_stale_regressions, 0.90
        ),
    }


def _candidate_vs_cpmm_rows(
    held_out: Sequence[dict[str, object]],
    cpmm: Sequence[dict[str, object]],
    candidate_system: str,
) -> list[dict[str, object]]:
    """Pair one selected concentrated system against the Dusk CPMM fallback."""

    keys = (
        "path",
        "source_month",
        "stratum",
        "flow_regime",
        "daily_volume_pool_ratio",
        "seed",
        "routing_mode",
        "initial_pool_value",
        "step_minutes",
        "match_basis",
    )

    def indexed(
        rows: Sequence[dict[str, object]],
        *,
        system: str,
    ) -> dict[tuple[object, ...], dict[str, object]]:
        output: dict[tuple[object, ...], dict[str, object]] = {}
        for row in rows:
            if str(row["system"]) != system:
                continue
            key = tuple(row[field] for field in keys)
            if key in output:
                raise AssertionError(f"duplicate {system} CPMM comparison cell {key}")
            output[key] = row
        return output

    candidates = indexed(held_out, system=candidate_system)
    controls = indexed(cpmm, system="dusk_cpmm")
    if not candidates or set(candidates) != set(controls):
        raise AssertionError(
            f"incomplete {candidate_system}-vs-CPMM matrix: "
            f"candidate={len(candidates)}, control={len(controls)}"
        )
    output = []
    for key, candidate in candidates.items():
        control = controls[key]
        output.append(
            {
                **{field: value for field, value in zip(keys, key)},
                "candidate_system": candidate_system,
                "candidate_minus_cpmm_net_lp_return_after_protocol_fee_bps": (
                    float(candidate["net_lp_return_after_protocol_fee_bps"])
                    - float(control["net_lp_return_after_protocol_fee_bps"])
                ),
                "candidate_minus_cpmm_gross_net_lp_return_bps": (
                    float(candidate["net_lp_return_vs_hold_bps"])
                    - float(control["net_lp_return_vs_hold_bps"])
                ),
                "candidate_minus_cpmm_selected_execution_cost_bps": (
                    float(candidate["selected_execution_cost_bps"])
                    - float(control["selected_execution_cost_bps"])
                ),
                "candidate_minus_cpmm_captured_volume_fraction": (
                    float(candidate["captured_user_volume_fraction"])
                    - float(control["captured_user_volume_fraction"])
                ),
                "candidate_minus_cpmm_depth_degradation_fraction": (
                    float(candidate["depth_degradation_fraction"])
                    - float(control["depth_degradation_fraction"])
                ),
                "candidate_minus_cpmm_marginal_tracking_stale_fraction": (
                    float(candidate["marginal_tracking_stale_fraction"])
                    - float(control["marginal_tracking_stale_fraction"])
                ),
            }
        )
    return output


def _service_tail_safe(groups: Iterable[dict[str, float]]) -> bool:
    return all(
        row["p90_execution_regression_bps"] <= 1.0
        and row["p10_capture_advantage_fraction"] >= -0.02
        and row["p90_marginal_tracking_stale_regression_fraction"] <= 0.05
        for row in groups
    )


def _cpmm_acceptance_predicates(
    route_metrics: dict[str, dict[str, float]],
    flow_route_metrics: dict[str, dict[str, float]],
) -> dict[str, bool]:
    """Apply the native universal thresholds to the product CPMM fallback."""

    return {
        "beats_dusk_cpmm_both_routes_median_advantage_at_least_1bp": all(
            row["median_return_advantage_bps"] >= 1.0
            for row in route_metrics.values()
        ),
        "beats_dusk_cpmm_both_routes_window_win_fraction_at_least_70pct": all(
            row["window_median_win_fraction"] >= 0.70
            for row in route_metrics.values()
        ),
        "beats_dusk_cpmm_both_routes_bottom_decile_mean_not_below_minus_1bp": all(
            row["bottom_decile_mean_return_advantage_bps"] >= -1.0
            for row in route_metrics.values()
        ),
        "beats_dusk_cpmm_both_routes_tail_service_regressions_within_limits": (
            _service_tail_safe(route_metrics.values())
        ),
        "beats_dusk_cpmm_every_flow_route_median_has_no_return_reversal": all(
            row["median_return_advantage_bps"] >= 0.0
            for row in flow_route_metrics.values()
        ),
        "beats_dusk_cpmm_every_flow_route_bottom_decile_not_below_minus_1bp": all(
            row["bottom_decile_mean_return_advantage_bps"] >= -1.0
            for row in flow_route_metrics.values()
        ),
        "beats_dusk_cpmm_every_flow_route_tail_service_within_limits": (
            _service_tail_safe(flow_route_metrics.values())
        ),
    }


def _native_summary(
    profile_rows: Sequence[dict[str, object]],
    training: Sequence[dict[str, object]],
    selected: dict[str, NativeProfile],
    held_out: Sequence[dict[str, object]],
    paired: Sequence[dict[str, object]],
    cpmm: Sequence[dict[str, object]],
    cpmm_matched_policy: Sequence[dict[str, object]],
    counterfactual: Sequence[dict[str, object]],
    neighborhood: Sequence[dict[str, object]],
    scale: Sequence[dict[str, object]],
    cadence: Sequence[dict[str, object]],
    volume_sensitivity: Sequence[dict[str, object]],
    basis_sensitivity: Sequence[dict[str, object]],
    fee_split_sensitivity: Sequence[dict[str, object]],
    runtime_seconds: float,
) -> dict[str, object]:
    controller_api_isolation = _controller_api_oracle_isolation()
    if controller_api_isolation["passes"] is not True:
        raise AssertionError(
            "controller API acquired an external/oracle price parameter: "
            f"{controller_api_isolation['violations']}"
        )
    by_route = {}
    for routing_mode in ("fixed", "elastic"):
        cells = [row for row in paired if row["routing_mode"] == routing_mode]
        deltas = [
            float(row["dusk_minus_curve_net_lp_return_after_protocol_fee_bps"])
            for row in cells
        ]
        gross_deltas = [
            float(row["dusk_minus_curve_gross_net_lp_return_bps"])
            for row in cells
        ]
        by_window: dict[str, list[float]] = {}
        for row in cells:
            by_window.setdefault(str(row["path"]), []).append(
                float(row["dusk_minus_curve_net_lp_return_after_protocol_fee_bps"])
            )
        window_medians = [statistics.median(values) for values in by_window.values()]
        by_route[routing_mode] = {
            "paired_cells": len(cells),
            "median_dusk_minus_curve_return_after_protocol_fee_bps": statistics.median(
                deltas
            ),
            "median_dusk_minus_curve_gross_return_bps": statistics.median(
                gross_deltas
            ),
            "p10_dusk_minus_curve_return_after_protocol_fee_bps": _percentile(
                deltas, 0.10
            ),
            "p90_dusk_minus_curve_return_after_protocol_fee_bps": _percentile(
                deltas, 0.90
            ),
            "cell_win_fraction": sum(value > 0 for value in deltas) / len(deltas),
            "window_median_win_fraction": sum(value > 0 for value in window_medians)
            / len(window_medians),
            "median_dusk_minus_curve_selected_execution_cost_bps": statistics.median(
                float(row["dusk_minus_curve_selected_execution_cost_bps"])
                for row in cells
            ),
            "median_dusk_minus_curve_captured_volume_fraction": statistics.median(
                float(row["dusk_minus_curve_captured_volume_fraction"])
                for row in cells
            ),
            "median_dusk_minus_curve_depth_degradation_fraction": statistics.median(
                float(row["dusk_minus_curve_depth_degradation_fraction"])
                for row in cells
            ),
            "median_dusk_minus_curve_marginal_tracking_stale_fraction": statistics.median(
                float(row["dusk_minus_curve_marginal_tracking_stale_fraction"])
                for row in cells
            ),
        }

    scale_pairs = _paired_native_rows(scale)
    cadence_pairs = _paired_native_rows(cadence)
    basis_pairs = _paired_native_rows(basis_sensitivity)
    volume_candidate_rows = [
        row for row in volume_sensitivity if row["system"] in CANDIDATE_SYSTEMS
    ]
    volume_cpmm_rows = [
        row for row in volume_sensitivity if row["system"] == "dusk_cpmm"
    ]
    volume_pairs = _paired_native_rows(volume_candidate_rows)
    basis_names = (
        "nearest_full_profile",
        "equal_jump_risk",
        "v21_deployed_invariant",
    )
    basis_summary: dict[str, dict[str, object]] = {}
    for basis in basis_names:
        basis_cells = [
            row
            for row in basis_pairs
            if row["match_basis"] == basis
        ]
        basis_summary[basis] = {
            "all_routes_fee_adjusted": _candidate_group_metrics(
                basis_cells,
                sign=1.0,
                return_delta_field=(
                    "dusk_minus_curve_net_lp_return_after_protocol_fee_bps"
                ),
            ),
            "all_routes_gross": _candidate_group_metrics(
                basis_cells,
                sign=1.0,
                return_delta_field="dusk_minus_curve_gross_net_lp_return_bps",
            ),
            "by_route_fee_adjusted": {
                routing_mode: _candidate_group_metrics(
                    [
                        row
                        for row in basis_cells
                        if row["routing_mode"] == routing_mode
                    ],
                    sign=1.0,
                    return_delta_field=(
                        "dusk_minus_curve_net_lp_return_after_protocol_fee_bps"
                    ),
                )
                for routing_mode in ("fixed", "elastic")
            },
            "by_route_gross": {
                routing_mode: _candidate_group_metrics(
                    [
                        row
                        for row in basis_cells
                        if row["routing_mode"] == routing_mode
                    ],
                    sign=1.0,
                    return_delta_field="dusk_minus_curve_gross_net_lp_return_bps",
                )
                for routing_mode in ("fixed", "elastic")
            },
        }

    selected_training_rows = {
        system: next(
            row
            for row in profile_rows
            if row["system"] == system
            and bool(row["selected_for_held_out"])
        )
        for system in PROFILE_SELECTION_SYSTEMS
    }
    selected_candidate_training_rows = {
        system: selected_training_rows[system] for system in CANDIDATE_SYSTEMS
    }
    selected_dusk_training_row = selected_training_rows["dusk_native"]
    selected_cpmm_training_row = selected_training_rows["dusk_cpmm"]
    common_training_service_relaxation = float(
        next(iter(selected_candidate_training_rows.values()))[
            "selection_group_service_relaxation"
        ]
    )
    cpmm_product_service_relaxation = float(
        selected_cpmm_training_row["selection_group_service_relaxation"]
    )
    dusk_product_service_relaxation = float(
        selected_dusk_training_row["service_relaxation_required"]
    )
    dusk_product_training_ready = _dusk_product_training_ready(
        selected_dusk_training_row
    )
    cpmm_product_training_ready = _cpmm_product_training_ready(
        selected_cpmm_training_row
    )
    eligible_neighborhood, neighborhood_eligibility = (
        _eligible_neighborhood_rows(profile_rows, neighborhood, selected)
    )
    cpmm_pairs_by_system = {
        system: _candidate_vs_cpmm_rows(held_out, cpmm, system)
        for system in ("curve_native_v2_1", "dusk_native")
    }
    replay_audit_rows = (
        *training,
        *held_out,
        *cpmm,
        *cpmm_matched_policy,
        *counterfactual,
        *neighborhood,
        *scale,
        *cadence,
        *volume_sensitivity,
        *basis_sensitivity,
    )
    all_period_arbitrage_cascades_converged = all(
        int(row.get("arbitrage_capped_event_count", 0)) == 0
        for row in replay_audit_rows
    )
    all_period_arbitrage_searches_numerically_valid = all(
        int(row.get("arbitrage_numerical_failure_count", 0)) == 0
        for row in replay_audit_rows
    )

    def candidate_decision(
        *,
        candidate_system: str,
        sign: float,
        return_delta_field: str,
        return_field: str,
        cpmm_return_delta_field: str,
    ) -> dict[str, object]:
        route_metrics = {
            routing_mode: _candidate_group_metrics(
                [row for row in paired if row["routing_mode"] == routing_mode],
                sign=sign,
                return_delta_field=return_delta_field,
            )
            for routing_mode in ("fixed", "elastic")
        }
        flow_route_metrics = {
            f"{regime}:{routing_mode}": _candidate_group_metrics(
                [
                    row
                    for row in paired
                    if row["flow_regime"] == regime
                    and row["routing_mode"] == routing_mode
                ],
                sign=sign,
                return_delta_field=return_delta_field,
            )
            for regime in (str(flow["flow_regime"]) for flow in FLOW_REGIMES)
            for routing_mode in ("fixed", "elastic")
        }
        cpmm_pairs = cpmm_pairs_by_system[candidate_system]
        cpmm_metric_kwargs = {
            "sign": 1.0,
            "return_delta_field": cpmm_return_delta_field,
            "execution_delta_field": (
                "candidate_minus_cpmm_selected_execution_cost_bps"
            ),
            "capture_delta_field": (
                "candidate_minus_cpmm_captured_volume_fraction"
            ),
            "marginal_tracking_stale_delta_field": (
                "candidate_minus_cpmm_marginal_tracking_stale_fraction"
            ),
        }
        cpmm_route_metrics = {
            routing_mode: _candidate_group_metrics(
                [
                    row
                    for row in cpmm_pairs
                    if row["routing_mode"] == routing_mode
                ],
                **cpmm_metric_kwargs,
            )
            for routing_mode in ("fixed", "elastic")
        }
        cpmm_flow_route_metrics = {
            f"{regime}:{routing_mode}": _candidate_group_metrics(
                [
                    row
                    for row in cpmm_pairs
                    if row["flow_regime"] == regime
                    and row["routing_mode"] == routing_mode
                ],
                **cpmm_metric_kwargs,
            )
            for regime in (str(flow["flow_regime"]) for flow in FLOW_REGIMES)
            for routing_mode in ("fixed", "elastic")
        }
        candidate_basis_metrics = {
            f"{basis}:{regime}:{routing_mode}": _candidate_group_metrics(
                [
                    row
                    for row in basis_pairs
                    if row["match_basis"] == basis
                    and row["flow_regime"] == regime
                    and row["routing_mode"] == routing_mode
                ],
                sign=sign,
                return_delta_field=return_delta_field,
            )
            for basis in basis_names
            for regime in (str(flow["flow_regime"]) for flow in FLOW_REGIMES)
            for routing_mode in ("fixed", "elastic")
        }
        volume_group_metrics = {
            f"{volume_ratio:g}:{template}:{routing_mode}": _candidate_group_metrics(
                [
                    row
                    for row in volume_pairs
                    if float(row["daily_volume_pool_ratio"]) == volume_ratio
                    and str(row["flow_regime"]).startswith(f"{template}_volume_")
                    and row["routing_mode"] == routing_mode
                ],
                sign=sign,
                return_delta_field=return_delta_field,
            )
            for volume_ratio in VOLUME_SENSITIVITY_RATIOS
            for template in ("benign", "toxic_delayed")
            for routing_mode in ("fixed", "elastic")
        }
        volume_cpmm_pairs = _candidate_vs_cpmm_rows(
            volume_candidate_rows,
            volume_cpmm_rows,
            candidate_system,
        )
        volume_cpmm_group_metrics = {
            f"{volume_ratio:g}:{template}:{routing_mode}": _candidate_group_metrics(
                [
                    row
                    for row in volume_cpmm_pairs
                    if float(row["daily_volume_pool_ratio"]) == volume_ratio
                    and str(row["flow_regime"]).startswith(f"{template}_volume_")
                    and row["routing_mode"] == routing_mode
                ],
                sign=1.0,
                return_delta_field=cpmm_return_delta_field,
                execution_delta_field=(
                    "candidate_minus_cpmm_selected_execution_cost_bps"
                ),
                capture_delta_field=(
                    "candidate_minus_cpmm_captured_volume_fraction"
                ),
                marginal_tracking_stale_delta_field=(
                    "candidate_minus_cpmm_marginal_tracking_stale_fraction"
                ),
            )
            for volume_ratio in VOLUME_SENSITIVITY_RATIOS
            for template in ("benign", "toxic_delayed")
            for routing_mode in ("fixed", "elastic")
        }
        neighborhood_win_fraction, neighborhood_median = (
            _neighborhood_candidate_win_fraction(
                eligible_neighborhood,
                selected,
                sign=sign,
                return_field=return_field,
            )
        )
        neighborhood_group_metrics = {
            f"{regime}:{routing_mode}": dict(
                zip(
                    ("win_fraction", "median_advantage_bps"),
                    _neighborhood_candidate_win_fraction(
                        [
                            row
                            for row in eligible_neighborhood
                            if row["flow_regime"] == regime
                            and row["routing_mode"] == routing_mode
                        ],
                        selected,
                        sign=sign,
                        return_field=return_field,
                    ),
                )
            )
            for regime in (str(flow["flow_regime"]) for flow in FLOW_REGIMES)
            for routing_mode in ("fixed", "elastic")
        }
        scale_group_metrics = {
            f"{int(pool_value)}:{regime}:{routing_mode}": _candidate_group_metrics(
                [
                    row
                    for row in scale_pairs
                    if float(row["initial_pool_value"]) == pool_value
                    and row["flow_regime"] == regime
                    and row["routing_mode"] == routing_mode
                ],
                sign=sign,
                return_delta_field=return_delta_field,
            )
            for pool_value in (250_000.0, 2_000_000.0, 20_000_000.0)
            for regime in (str(flow["flow_regime"]) for flow in FLOW_REGIMES)
            for routing_mode in ("fixed", "elastic")
        }
        cadence_group_metrics = {
            f"{regime}:{routing_mode}": _candidate_group_metrics(
                [
                    row
                    for row in cadence_pairs
                    if row["flow_regime"] == regime
                    and row["routing_mode"] == routing_mode
                ],
                sign=sign,
                return_delta_field=return_delta_field,
            )
            for regime in (str(flow["flow_regime"]) for flow in FLOW_REGIMES)
            for routing_mode in ("fixed", "elastic")
        }
        scale_advantages = [
            sign * float(row[return_delta_field]) for row in scale_pairs
        ]
        cadence_advantages = [
            sign * float(row[return_delta_field]) for row in cadence_pairs
        ]

        predicates = {
            "all_arbitrage_cascades_converged_before_hard_cap": (
                all_period_arbitrage_cascades_converged
            ),
            "all_arbitrage_searches_numerically_valid": (
                all_period_arbitrage_searches_numerically_valid
            ),
            "selected_profile_passes_original_training_service_envelope": bool(
                selected_training_rows[candidate_system][
                    "original_service_envelope_pass"
                ]
            ),
            "both_concentrated_profiles_pass_original_training_service_envelope": all(
                bool(row["original_service_envelope_pass"])
                for row in selected_candidate_training_rows.values()
            ),
            "training_service_is_deployment_ready": (
                _training_service_deployment_ready(
                    selected_candidate_training_rows
                )
            ),
            "common_training_service_relaxation_is_zero": (
                common_training_service_relaxation <= 1e-12
            ),
            "both_routes_median_advantage_at_least_1bp": all(
                row["median_return_advantage_bps"] >= 1.0
                for row in route_metrics.values()
            ),
            "both_routes_window_win_fraction_at_least_70pct": all(
                row["window_median_win_fraction"] >= 0.70
                for row in route_metrics.values()
            ),
            "both_routes_bottom_decile_mean_not_below_minus_1bp": all(
                row["bottom_decile_mean_return_advantage_bps"] >= -1.0
                for row in route_metrics.values()
            ),
            "both_routes_tail_service_regressions_within_limits": _service_tail_safe(
                route_metrics.values()
            ),
            "every_flow_route_median_has_no_return_reversal": all(
                row["median_return_advantage_bps"] >= 0.0
                for row in flow_route_metrics.values()
            ),
            "every_flow_route_bottom_decile_mean_not_below_minus_1bp": all(
                row["bottom_decile_mean_return_advantage_bps"] >= -1.0
                for row in flow_route_metrics.values()
            ),
            "every_flow_route_tail_service_regressions_within_limits": _service_tail_safe(
                flow_route_metrics.values()
            ),
            "neighborhood_win_fraction_at_least_60pct": neighborhood_win_fraction
            >= 0.60,
            "every_neighborhood_flow_route_has_no_reversal": all(
                row["median_advantage_bps"] >= 0.0
                and row["win_fraction"] >= 0.60
                for row in neighborhood_group_metrics.values()
            ),
            "pool_scale_win_fraction_at_least_two_thirds": (
                sum(value > 0 for value in scale_advantages)
                / len(scale_advantages)
                >= 2 / 3
            ),
            "every_scale_flow_route_has_no_reversal_or_tail_regression": all(
                row["median_return_advantage_bps"] >= 0.0
                and row["bottom_decile_mean_return_advantage_bps"] >= -1.0
                for row in scale_group_metrics.values()
            )
            and _service_tail_safe(scale_group_metrics.values()),
            "one_minute_cadence_win_fraction_at_least_two_thirds": (
                sum(value > 0 for value in cadence_advantages)
                / len(cadence_advantages)
                >= 2 / 3
            ),
            "every_one_minute_flow_route_has_no_reversal_or_tail_regression": all(
                row["median_return_advantage_bps"] >= 0.0
                and row["bottom_decile_mean_return_advantage_bps"] >= -1.0
                for row in cadence_group_metrics.values()
            )
            and _service_tail_safe(cadence_group_metrics.values()),
            "every_locked_profile_volume_flow_route_has_no_reversal_or_tail_regression": all(
                row["median_return_advantage_bps"] >= 0.0
                and row["bottom_decile_mean_return_advantage_bps"] >= -1.0
                for row in volume_group_metrics.values()
            )
            and _service_tail_safe(volume_group_metrics.values()),
            "beats_cpmm_every_locked_profile_volume_flow_route_without_reversal_or_tail_regression": all(
                row["median_return_advantage_bps"] >= 0.0
                and row["bottom_decile_mean_return_advantage_bps"] >= -1.0
                for row in volume_cpmm_group_metrics.values()
            )
            and _service_tail_safe(volume_cpmm_group_metrics.values()),
            **_cpmm_acceptance_predicates(
                cpmm_route_metrics,
                cpmm_flow_route_metrics,
            ),
        }
        return {
            "passes": all(predicates.values()),
            "predicates": predicates,
            "by_route": route_metrics,
            "by_flow_and_route": flow_route_metrics,
            "by_matching_basis_flow_and_route": candidate_basis_metrics,
            "by_neighborhood_flow_and_route": neighborhood_group_metrics,
            "by_scale_flow_and_route": scale_group_metrics,
            "by_one_minute_flow_and_route": cadence_group_metrics,
            "by_locked_profile_volume_flow_and_route": volume_group_metrics,
            "vs_dusk_cpmm_by_locked_profile_volume_flow_and_route": (
                volume_cpmm_group_metrics
            ),
            "vs_dusk_cpmm_by_route": cpmm_route_metrics,
            "vs_dusk_cpmm_by_flow_and_route": cpmm_flow_route_metrics,
            "neighborhood_win_fraction": neighborhood_win_fraction,
            "neighborhood_median_advantage_bps": neighborhood_median,
            "pool_scale_win_fraction": sum(
                value > 0 for value in scale_advantages
            )
            / len(scale_advantages),
            "one_minute_cadence_win_fraction": sum(
                value > 0 for value in cadence_advantages
            )
            / len(cadence_advantages),
            "matching_basis_sensitivity_selection_scope": (
                "locked equal-execution controllers; non-optimized diagnostic; "
                "excluded from the primary acceptance gate"
            ),
        }

    fee_adjusted_dusk = candidate_decision(
        candidate_system="dusk_native",
        sign=1.0,
        return_delta_field=(
            "dusk_minus_curve_net_lp_return_after_protocol_fee_bps"
        ),
        return_field="net_lp_return_after_protocol_fee_bps",
        cpmm_return_delta_field=(
            "candidate_minus_cpmm_net_lp_return_after_protocol_fee_bps"
        ),
    )
    fee_adjusted_curve = candidate_decision(
        candidate_system="curve_native_v2_1",
        sign=-1.0,
        return_delta_field=(
            "dusk_minus_curve_net_lp_return_after_protocol_fee_bps"
        ),
        return_field="net_lp_return_after_protocol_fee_bps",
        cpmm_return_delta_field=(
            "candidate_minus_cpmm_net_lp_return_after_protocol_fee_bps"
        ),
    )
    gross_dusk = candidate_decision(
        candidate_system="dusk_native",
        sign=1.0,
        return_delta_field="dusk_minus_curve_gross_net_lp_return_bps",
        return_field="net_lp_return_vs_hold_bps",
        cpmm_return_delta_field="candidate_minus_cpmm_gross_net_lp_return_bps",
    )
    gross_curve = candidate_decision(
        candidate_system="curve_native_v2_1",
        sign=-1.0,
        return_delta_field="dusk_minus_curve_gross_net_lp_return_bps",
        return_field="net_lp_return_vs_hold_bps",
        cpmm_return_delta_field="candidate_minus_cpmm_gross_net_lp_return_bps",
    )

    def winner(dusk: dict[str, object], curve: dict[str, object]) -> str | None:
        dusk_passes = bool(dusk["passes"])
        curve_passes = bool(curve["passes"])
        if dusk_passes and not curve_passes:
            return "dusk_current_dh2"
        if curve_passes and not dusk_passes:
            return "curve_twocrypto_v2_1"
        return None

    fee_adjusted_winner = winner(fee_adjusted_dusk, fee_adjusted_curve)
    primary_lp_deltas = [
        float(row["dusk_minus_curve_net_lp_return_after_protocol_fee_bps"])
        for row in paired
    ]
    primary_median_delta = statistics.median(primary_lp_deltas)
    route_medians = {
        route: float(
            metrics[
                "median_dusk_minus_curve_return_after_protocol_fee_bps"
            ]
        )
        for route, metrics in by_route.items()
    }
    descriptive_lp_leader = (
        "dusk_current_dh2"
        if primary_median_delta > 0.0
        else "curve_twocrypto_v2_1"
        if primary_median_delta < 0.0
        else None
    )
    route_materially_dusk = all(value >= 1.0 for value in route_medians.values())
    route_materially_curve = all(value <= -1.0 for value in route_medians.values())
    material_lp_leader = (
        "dusk_current_dh2"
        if primary_median_delta >= 1.0 and route_materially_dusk
        else "curve_twocrypto_v2_1"
        if primary_median_delta <= -1.0 and route_materially_curve
        else None
    )
    def gross_diagnostic(result: dict[str, object]) -> dict[str, object]:
        output = dict(result)
        output["meets_same_diagnostic_robustness_gate"] = bool(
            output.pop("passes")
        )
        return output

    gross_outcome = {
        "selection_basis": "fee_adjusted_synthetic_profile_lock",
        "universal_claim": False,
        "scope": (
            "gross outcome for the profiles selected on fee-adjusted synthetic "
            "return; diagnostic only and not a gross universal-winner claim"
        ),
        "dusk": gross_diagnostic(gross_dusk),
        "curve": gross_diagnostic(gross_curve),
    }

    fee_split_summary: dict[str, dict[str, float]] = {}
    for total_share_bps in (0, 500, 2_500, 10_000):
        for routing_mode in ("fixed", "elastic"):
            values = [
                float(row["dusk_minus_curve_net_lp_return_bps"])
                for row in fee_split_sensitivity
                if int(row["dusk_total_base_fee_revenue_share_bps"])
                == total_share_bps
                and row["routing_mode"] == routing_mode
            ]
            fee_split_summary[f"{total_share_bps}:{routing_mode}"] = {
                "cells": float(len(values)),
                "median_dusk_minus_curve_net_lp_return_bps": statistics.median(
                    values
                ),
                "p10_dusk_minus_curve_net_lp_return_bps": _percentile(values, 0.10),
                "cell_win_fraction": sum(value > 0 for value in values)
                / len(values),
            }

    held_lookup = {
        (
            row["path"],
            row["system"],
            row["flow_regime"],
            row["seed"],
            row["routing_mode"],
        ): row
        for row in held_out
    }
    controller_effects: dict[str, dict[str, list[float]]] = {
        "curve_native_v2_1": {
            "lp": [],
            "execution": [],
            "capture": [],
            "marginal_stale": [],
            "tracking": [],
        },
        "dusk_native": {
            "lp": [],
            "execution": [],
            "capture": [],
            "marginal_stale": [],
            "tracking": [],
        },
    }
    for off in counterfactual:
        on = held_lookup[
            (
                off["path"],
                off["system"],
                off["flow_regime"],
                off["seed"],
                off["routing_mode"],
            )
        ]
        effects = controller_effects[str(off["system"])]
        effects["lp"].append(
            float(on["net_lp_return_after_protocol_fee_bps"])
            - float(off["net_lp_return_after_protocol_fee_bps"])
        )
        effects["execution"].append(
            float(on["selected_execution_cost_bps"])
            - float(off["selected_execution_cost_bps"])
        )
        effects["capture"].append(
            float(on["captured_user_volume_fraction"])
            - float(off["captured_user_volume_fraction"])
        )
        effects["marginal_stale"].append(
            float(on["marginal_tracking_stale_fraction"])
            - float(off["marginal_tracking_stale_fraction"])
        )
        effects["tracking"].append(
            float(on["center_tracking_rmse_bps"])
            - float(off["center_tracking_rmse_bps"])
        )
    controller_summary = {
        system: {
            "median_controller_on_minus_off_lp_return_bps": statistics.median(
                values["lp"]
            ),
            "lp_win_fraction": sum(value > 0 for value in values["lp"])
            / len(values["lp"]),
            "median_controller_on_minus_off_execution_cost_bps": statistics.median(
                values["execution"]
            ),
            "median_controller_on_minus_off_capture_fraction": statistics.median(
                values["capture"]
            ),
            "median_controller_on_minus_off_marginal_tracking_stale_fraction": statistics.median(
                values["marginal_stale"]
            ),
            "median_controller_on_minus_off_tracking_rmse_bps": statistics.median(
                values["tracking"]
            ),
        }
        for system, values in controller_effects.items()
    }

    def cpmm_summary(
        controls: Sequence[dict[str, object]],
        *,
        comparison_scope: str,
        participates_in_winner_gate: bool,
    ) -> dict[str, object]:
        deltas = []
        execution_deltas = []
        capture_deltas = []
        marginal_stale_deltas = []
        for control in controls:
            concentrated = held_lookup[
                (
                    control["path"],
                    "dusk_native",
                    control["flow_regime"],
                    control["seed"],
                    control["routing_mode"],
                )
            ]
            deltas.append(
                float(concentrated["net_lp_return_after_protocol_fee_bps"])
                - float(control["net_lp_return_after_protocol_fee_bps"])
            )
            execution_deltas.append(
                float(concentrated["selected_execution_cost_bps"])
                - float(control["selected_execution_cost_bps"])
            )
            capture_deltas.append(
                float(concentrated["captured_user_volume_fraction"])
                - float(control["captured_user_volume_fraction"])
            )
            marginal_stale_deltas.append(
                float(concentrated["marginal_tracking_stale_fraction"])
                - float(control["marginal_tracking_stale_fraction"])
            )
        return {
            "comparison_scope": comparison_scope,
            "participates_in_winner_gate": participates_in_winner_gate,
            "control_system": str(controls[0]["system"]),
            "control_profile_id": str(controls[0]["profile_id"]),
            "median_current_dh2_minus_cpmm_lp_return_bps": statistics.median(
                deltas
            ),
            "current_dh2_win_fraction": sum(value > 0 for value in deltas)
            / len(deltas),
            "median_current_dh2_minus_cpmm_execution_cost_bps": statistics.median(
                execution_deltas
            ),
            "median_current_dh2_minus_cpmm_capture_fraction": statistics.median(
                capture_deltas
            ),
            "median_current_dh2_minus_cpmm_marginal_tracking_stale_fraction": statistics.median(
                marginal_stale_deltas
            ),
        }

    cpmm_product_summary = cpmm_summary(
        cpmm,
        comparison_scope=(
            "independently selected on synthetic training under the same "
            "route-service envelope; blind held-out product comparator"
        ),
        participates_in_winner_gate=True,
    )
    cpmm_product_summary["product_ready_on_training_service"] = (
        cpmm_product_training_ready
    )
    cpmm_product_summary["service_relaxation"] = (
        cpmm_product_service_relaxation
    )
    cpmm_matched_policy_summary = cpmm_summary(
        cpmm_matched_policy,
        comparison_scope=(
            "selected current-DH2 fee and controller held fixed; only "
            "peak_depth and fade_scale set to zero"
        ),
        participates_in_winner_gate=False,
    )

    profile_selection = {
        system: {
            "profile_id": profile.profile_id,
            "description": profile.description,
            "service_relaxation_required": float(
                selected_training_rows[system]["service_relaxation_required"]
            ),
            "original_service_envelope_pass": bool(
                selected_training_rows[system][
                    "original_service_envelope_pass"
                ]
            ),
            "minimum_route_selection_utility": float(
                selected_training_rows[system][
                    "minimum_route_selection_utility"
                ]
            ),
            "selection_group": str(
                selected_training_rows[system]["selection_group"]
            ),
            "selection_group_service_relaxation": float(
                selected_training_rows[system][
                    "selection_group_service_relaxation"
                ]
            ),
        }
        for system, profile in selected.items()
    }
    maximum_error = max(
        abs(float(row["accounting_identity_error"]))
        for row in replay_audit_rows
    )
    decision_metric_fields = (
        "net_lp_return_after_protocol_fee_bps",
        "net_lp_return_vs_hold_bps",
        "captured_user_volume_fraction",
        "selected_execution_cost_bps",
        "depth_degradation_fraction",
        "marginal_tracking_stale_fraction",
        "accounting_identity_error",
    )
    all_decision_metrics_finite = all(
        math.isfinite(float(row[field]))
        for row in replay_audit_rows
        for field in decision_metric_fields
    )
    return {
        "status": "complete",
        "reference": {
            "curve_release": REFERENCE_RELEASE,
            "curve_commit": REFERENCE_COMMIT,
            "implementation_scope": (
                "independent analytical benchmark reference; no Curve source code, "
                "tests, constants, comments, or implementation structure vendored"
            ),
        },
        "profile_selection": profile_selection,
        "profile_selection_policy": {
            "systems": list(CANDIDATE_SYSTEMS),
            "training_data": (
                "eight synthetic paths x four flow regimes x two seeds x fixed "
                "and elastic routing for Curve and concentrated Dusk; historical "
                "windows are not read; the CPMM lock is separate"
            ),
            "hard_gates": {
                "all_training_metrics_finite": True,
                "maximum_accounting_identity_error": ACCOUNTING_ERROR_LIMIT,
                "controller_external_price_reads": 0,
                "all_period_arbitrage_capped_event_count": 0,
                "all_period_arbitrage_numerical_failure_count": 0,
                "arbitrage_cap_counter_scope": ARBITRAGE_CAP_COUNTER_SCOPE,
            },
            "base_service_guardrails_by_route": {
                field: {"kind": kind, "threshold": threshold}
                for field, (kind, threshold) in SERVICE_GUARDRAILS.items()
            },
            "common_service_relaxation": common_training_service_relaxation,
            "minimum_required_relaxation_by_system": {
                system: min(
                    float(row["service_relaxation_required"])
                    for row in profile_rows
                    if row["system"] == system
                )
                for system in CANDIDATE_SYSTEMS
            },
            "relaxation_rule": (
                "minimum fraction t -> t*(1-rho); maximum bounded fraction "
                "t -> t+rho*(1-t); maximum cost t -> t*(1+rho); one common "
                "rho is the smallest value admitting at least one hard-valid "
                "profile from each concentrated candidate system; CPMM is "
                "selected under its own rho"
            ),
            "selection_utility": (
                "within each system, minimize that profile's own required "
                "service relaxation first; then maximize the minimum across "
                "routes of 0.8*median fee-adjusted LP return + 0.2*p10 return; "
                "tie-break by mean route utility, higher minimum route capture, "
                "lower maximum route p90 execution cost, profile id"
            ),
            "comparison_pair_ready_on_training_service": (
                _training_service_deployment_ready(
                    selected_candidate_training_rows
                )
            ),
        },
        "dusk_product_selection_policy": {
            "system": "dusk_native",
            "selected_profile_id": selected["dusk_native"].profile_id,
            "base_service_guardrails_by_route": {
                field: {"kind": kind, "threshold": threshold}
                for field, (kind, threshold) in SERVICE_GUARDRAILS.items()
            },
            "service_relaxation": dusk_product_service_relaxation,
            "selected_profile_original_service_envelope_pass": bool(
                selected_dusk_training_row["original_service_envelope_pass"]
            ),
            "product_ready_on_training_service": dusk_product_training_ready,
            "independent_from_common_curve_dh2_relaxation": True,
            "readiness_rule": (
                "Dusk is product-ready on synthetic training only when its "
                "own hard-valid selected profile passes every original fixed- "
                "and elastic-route service guardrail with rho=0. Curve's "
                "required relaxation cannot block or rescue this Dusk-only gate."
            ),
        },
        "cpmm_product_selection_policy": {
            "system": "dusk_cpmm",
            "training_data": (
                "the same eight synthetic paths x four flow regimes x two seeds "
                "x fixed and elastic routing; selected independently without "
                "entering the Curve/DH2 common relaxation or reading history"
            ),
            "hard_gates": {
                "all_training_metrics_finite": True,
                "maximum_accounting_identity_error": ACCOUNTING_ERROR_LIMIT,
                "controller_external_price_reads": 0,
                "all_period_arbitrage_capped_event_count": 0,
                "all_period_arbitrage_numerical_failure_count": 0,
                "arbitrage_cap_counter_scope": ARBITRAGE_CAP_COUNTER_SCOPE,
            },
            "base_service_guardrails_by_route": {
                field: {"kind": kind, "threshold": threshold}
                for field, (kind, threshold) in SERVICE_GUARDRAILS.items()
            },
            "service_relaxation": cpmm_product_service_relaxation,
            "minimum_required_relaxation_by_system": {
                "dusk_cpmm": min(
                    float(row["service_relaxation_required"])
                    for row in profile_rows
                    if row["system"] == "dusk_cpmm"
                )
            },
            "selected_profile_id": selected["dusk_cpmm"].profile_id,
            "selected_profile_original_service_envelope_pass": bool(
                selected_cpmm_training_row["original_service_envelope_pass"]
            ),
            "product_ready_on_training_service": cpmm_product_training_ready,
            "selection_utility": (
                "within the CPMM family's own minimum service relaxation, "
                "maximize the same worse-route fee-adjusted LP-return utility"
            ),
            "recommendation_rule": (
                "the independently selected CPMM may remain an economic "
                "comparator when relaxation is positive, but it cannot receive "
                "a ship or product-ready recommendation unless relaxation is "
                "zero and the selected profile passes the original envelope"
            ),
            "contaminates_concentrated_candidate_selection": False,
        },
        "held_out": by_route,
        "matching_basis_sensitivity": basis_summary,
        "matching_basis_sensitivity_scope": (
            "non-optimized locked-controller diagnostic; controllers and fees "
            "were selected on the primary equal-aggregate-execution invariant "
            "match and are not reselected on alternate bases"
        ),
        "locked_profile_volume_sensitivity": {
            "daily_volume_pool_ratios": list(VOLUME_SENSITIVITY_RATIOS),
            "scope": (
                "locked-profile transfer test at fixed arrival frequency; "
                "controllers are not reselected for each volume regime"
            ),
            "dusk_vs_curve_by_volume_flow_and_route": fee_adjusted_dusk[
                "by_locked_profile_volume_flow_and_route"
            ],
            "dusk_vs_cpmm_by_volume_flow_and_route": fee_adjusted_dusk[
                "vs_dusk_cpmm_by_locked_profile_volume_flow_and_route"
            ],
            "curve_vs_cpmm_by_volume_flow_and_route": fee_adjusted_curve[
                "vs_dusk_cpmm_by_locked_profile_volume_flow_and_route"
            ],
        },
        "dusk_base_fee_split_sensitivity": fee_split_summary,
        "controller_counterfactual": controller_summary,
        "cpmm_product_fallback": cpmm_product_summary,
        "cpmm_matched_policy_ablation": cpmm_matched_policy_summary,
        "decision": {
            "fee_adjusted_universal_native_winner": fee_adjusted_winner,
            "locked_profile_lp_return_leader_at_equal_invariant_execution_index": {
                "descriptive_leader": descriptive_lp_leader,
                "material_same_direction_both_routes_leader": material_lp_leader,
                "median_dusk_minus_curve_return_bps": primary_median_delta,
                "p10_dusk_minus_curve_return_bps": _percentile(
                    primary_lp_deltas, 0.10
                ),
                "p90_dusk_minus_curve_return_bps": _percentile(
                    primary_lp_deltas, 0.90
                ),
                "cell_win_fraction": sum(value > 0 for value in primary_lp_deltas)
                / len(primary_lp_deltas),
                "route_medians_dusk_minus_curve_bps": route_medians,
                "median_dusk_minus_curve_selected_execution_cost_bps": statistics.median(
                    float(row["dusk_minus_curve_selected_execution_cost_bps"])
                    for row in paired
                ),
                "median_dusk_minus_curve_capture_fraction": statistics.median(
                    float(row["dusk_minus_curve_captured_volume_fraction"])
                    for row in paired
                ),
                "median_dusk_minus_curve_marginal_tracking_stale_fraction": statistics.median(
                    float(
                        row[
                            "dusk_minus_curve_marginal_tracking_stale_fraction"
                        ]
                    )
                    for row in paired
                ),
                "scope": (
                    "LP-return-only result for locked profiles at the primary "
                    "equal aggregate invariant execution index; execution cost, "
                    "capture, and tracking remain separate; independent of ship "
                    "gates and not a global capital-efficiency or family optimum"
                ),
            },
            "rule": (
                "The selected Curve and concentrated-Dusk profiles must pass the "
                "original synthetic training service envelope before either can "
                "be called a universal winner, so their common relaxation must "
                "be zero. The independently selected CPMM uses a separate service "
                "relaxation and cannot alter that lock. A candidate must also pass independently in fixed and elastic "
                "routing: >=1 bp/12h median advantage, >=70% window wins, and a "
                "bottom-decile mean >= -1 bp. Every flow-by-route cell must avoid "
                "a median return reversal. Route, flow, and matching-basis tails "
                "must keep p90 execution regression <=1 bp, p10 capture advantage "
                ">=-2 percentage points, and p90 marginal-price tracking "
                "staleness regression <=5 percentage points. Finite-size raw "
                "quote costs remain separate depth/fee diagnostics. The result "
                "must also persist "
                "across source-ranked promoted policy shortlists, pool scales, one-minute "
                "cadence, and locked-profile 0.05x-to-4x daily-volume sensitivity. "
                "Alternate invariant matches are non-optimized diagnostics and do "
                "not enter this gate. In addition, a concentrated candidate must beat "
                "the Dusk CPMM product comparator under those same route and "
                "flow-by-route return and service thresholds."
            ),
            "neighborhood_service_eligibility": neighborhood_eligibility,
            "fee_adjusted": {
                "dusk": fee_adjusted_dusk,
                "curve": fee_adjusted_curve,
            },
            "gross_outcome_for_fee_adjusted_selected_profiles": gross_outcome,
            "fee_assumptions": {
                "dusk_manager_share_of_base_fee_bps": 500,
                "dusk_protocol_share_of_base_fee_bps": 0,
                "dusk_dynamic_surcharge_lp_share": 1.0,
                "curve_effective_admin_liability_fraction_of_gross_xcp_profit": 0.25,
            },
        },
        "validation": {
            "maximum_accounting_identity_error": maximum_error,
            "all_decision_metrics_finite": all_decision_metrics_finite,
            "controller_external_price_reads": max(
                int(row["controller_external_price_reads"])
                for row in replay_audit_rows
            ),
            "controller_api_oracle_isolation": controller_api_isolation,
            "arbitrage_capped_event_count": sum(
                int(row.get("arbitrage_capped_event_count", 0))
                for row in replay_audit_rows
            ),
            "scored_arbitrage_capped_event_count": sum(
                int(row.get("scored_arbitrage_capped_event_count", 0))
                for row in replay_audit_rows
            ),
            "arbitrage_numerical_failure_count": sum(
                int(row.get("arbitrage_numerical_failure_count", 0))
                for row in replay_audit_rows
            ),
            "scored_arbitrage_numerical_failure_count": sum(
                int(row.get("scored_arbitrage_numerical_failure_count", 0))
                for row in replay_audit_rows
            ),
            "arbitrage_cap_counter_scope": ARBITRAGE_CAP_COUNTER_SCOPE,
            "profile_rows": len(profile_rows),
            "held_out_rows": len(held_out),
            "paired_rows": len(paired),
            "cpmm_rows": len(cpmm),
            "cpmm_matched_policy_ablation_rows": len(cpmm_matched_policy),
            "counterfactual_rows": len(counterfactual),
            "neighborhood_rows": len(neighborhood),
            "scale_rows": len(scale),
            "cadence_rows": len(cadence),
            "volume_sensitivity_rows": len(volume_sensitivity),
            "basis_sensitivity_rows": len(basis_sensitivity),
            "fee_split_sensitivity_rows": len(fee_split_sensitivity),
            "runtime_seconds": runtime_seconds,
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--quick", action="store_true")
    parser.add_argument(
        "--reuse-training-checkpoint",
        action="store_true",
        help=(
            "reuse the validated non-canonical full-training checkpoint; "
            "incompatible with --quick"
        ),
    )
    args = parser.parse_args()
    if args.quick and args.reuse_training_checkpoint:
        parser.error("--reuse-training-checkpoint cannot be combined with --quick")
    started = time.perf_counter()
    output_dir = RESULTS / "quick" if args.quick else RESULTS
    output_dir.mkdir(parents=True, exist_ok=True)

    if not args.quick:
        missing_promotions = [
            str(path.relative_to(ROOT))
            for path in (
                PROMOTED_CURVE_PROFILES,
                PROMOTED_DUSK_PROFILES,
                PROMOTED_CPMM_PROFILES,
            )
            if not path.is_file()
        ]
        if missing_promotions:
            raise FileNotFoundError(
                "canonical native benchmark requires all source-bound full-search "
                "promotions; missing: " + ", ".join(missing_promotions)
            )

    curve, dusk = _matched_models()
    curve_profiles = _curve_profiles(curve)
    dusk_profiles = _dusk_profiles(dusk)
    candidate_profiles = (*curve_profiles, *dusk_profiles)
    cpmm_profiles = _cpmm_profiles(dusk_profiles)
    training_profiles = (*candidate_profiles, *cpmm_profiles)
    training_jobs = (
        _quick_training_jobs(training_profiles)
        if args.quick
        else _training_jobs(training_profiles)
    )
    expected_full_training = (
        8
        * len(FLOW_REGIMES)
        * len(PRIMARY_SEEDS)
        * 2
        * len(training_profiles)
    )
    if args.reuse_training_checkpoint:
        training = _read_training_checkpoint(
            expected_full_training, training_profiles
        )
        print(
            f"profile selection: reused {len(training)} checkpoint rows",
            flush=True,
        )
    else:
        training = _run_jobs(training_jobs, args.workers, "profile selection")
        if not args.quick:
            _write_training_checkpoint(training, training_profiles)
    profile_summary, selected, infeasible_systems = _profile_summary(
        training, training_profiles
    )
    if infeasible_systems:
        if not args.quick:
            _write_training_checkpoint(
                training, training_profiles, profile_summary
            )
        details = ", ".join(infeasible_systems)
        diagnostic = TRAINING_CHECKPOINT_DIR / "native_profile_diagnostics.csv"
        raise RuntimeError(
            "common service envelope is infeasible for: "
            f"{details}; inspect {diagnostic}"
        )

    held_out = _run_jobs(
        _held_out_jobs(selected, quick=args.quick),
        args.workers,
        "held-out native replay",
    )
    paired = _paired_native_rows(held_out)
    fee_split_sensitivity = _dusk_fee_split_sensitivity_rows(held_out)

    cpmm = _run_jobs(
        _cpmm_jobs(selected["dusk_cpmm"], quick=args.quick),
        args.workers,
        "independently selected CPMM product comparator",
    )
    cpmm_matched_policy = _run_jobs(
        _cpmm_matched_policy_jobs(
            selected["dusk_native"], quick=args.quick
        ),
        args.workers,
        "matched-policy CPMM invariant ablation",
    )

    counterfactual = _run_jobs(
        _counterfactual_jobs(selected, quick=args.quick),
        args.workers,
        "controller-off counterfactual",
    )

    neighborhood_jobs, scale_jobs, cadence_jobs = _sensitivity_jobs(
        candidate_profiles,
        selected,
        quick=args.quick,
    )
    neighborhood = _run_jobs(
        neighborhood_jobs,
        args.workers,
        "promoted policy shortlist sensitivity",
    )
    scale = _run_jobs(scale_jobs, args.workers, "pool scale sensitivity")
    cadence = _run_jobs(cadence_jobs, args.workers, "one-minute cadence")
    volume_sensitivity = _run_jobs(
        _volume_sensitivity_jobs(selected, quick=args.quick),
        args.workers,
        "locked-profile volume sensitivity",
    )
    basis_sensitivity = _run_jobs(
        _basis_sensitivity_jobs(selected, quick=args.quick),
        args.workers,
        "matching-basis sensitivity",
    )

    summary = _native_summary(
        profile_summary,
        training,
        selected,
        held_out,
        paired,
        cpmm,
        cpmm_matched_policy,
        counterfactual,
        neighborhood,
        scale,
        cadence,
        volume_sensitivity,
        basis_sensitivity,
        fee_split_sensitivity,
        time.perf_counter() - started,
    )
    maximum_accounting_error = float(
        summary["validation"]["maximum_accounting_identity_error"]
    )
    if (
        not math.isfinite(maximum_accounting_error)
        or maximum_accounting_error > 1e-5
    ):
        raise AssertionError(
            "native accounting audit failed: maximum identity error "
            f"{maximum_accounting_error:.6g} exceeds 1e-5"
        )
    if summary["validation"]["all_decision_metrics_finite"] is not True:
        raise AssertionError("native decision-metric audit contains non-finite values")
    if int(summary["validation"]["controller_external_price_reads"]) != 0:
        raise AssertionError(
            "native controller isolation failed: external reference price was read"
        )
    if int(summary["validation"]["arbitrage_capped_event_count"]) != 0:
        raise AssertionError(
            "native arbitrage cascade failed to converge during burn-in or "
            "scoring before its hard cap: "
            f"{summary['validation']['arbitrage_capped_event_count']} capped events"
        )
    if int(summary["validation"]["arbitrage_numerical_failure_count"]) != 0:
        raise AssertionError(
            "native arbitrage search encountered arithmetic, overflow, or "
            "solver failures during burn-in or scoring: "
            f"{summary['validation']['arbitrage_numerical_failure_count']} failures"
        )
    summary["status"] = "smoke_only" if args.quick else "complete"
    summary["validation"]["canonical_output"] = not args.quick
    expected_counts = (
        {
            "profile_rows": len(training_profiles),
            "held_out_rows": 96,
            "paired_rows": 48,
            "cpmm_rows": 48,
            "cpmm_matched_policy_ablation_rows": 48,
            "counterfactual_rows": 96,
            "neighborhood_rows": 16 * len(candidate_profiles),
            "scale_rows": 96,
            "cadence_rows": 32,
            "volume_sensitivity_rows": 96,
            "basis_sensitivity_rows": 288,
            "fee_split_sensitivity_rows": 192,
        }
        if args.quick
        else {
            "profile_rows": len(training_profiles),
            "held_out_rows": 480,
            "paired_rows": 240,
            "cpmm_rows": 240,
            "cpmm_matched_policy_ablation_rows": 240,
            "counterfactual_rows": 480,
            "neighborhood_rows": 48 * len(candidate_profiles),
            "scale_rows": 288,
            "cadence_rows": 96,
            "volume_sensitivity_rows": 288,
            "basis_sensitivity_rows": 1_440,
            "fee_split_sensitivity_rows": 960,
        }
    )
    for field, expected in expected_counts.items():
        actual = int(summary["validation"][field])
        if actual != expected:
            raise AssertionError(
                f"incomplete native matrix: {field}={actual}, expected {expected}"
            )
    expected_training = (
        len(training_profiles) * len(FLOW_REGIMES) * 2
        if args.quick
        else expected_full_training
    )
    if len(training) != expected_training:
        raise AssertionError(
            f"incomplete training matrix: {len(training)}, expected "
            f"{expected_training}"
        )
    summary["validation"]["training_rows"] = len(training)
    summary["validation"]["expected_counts"] = {
        **expected_counts,
        "training_rows": expected_training,
    }
    _write_native_outputs_atomically(
        output_dir,
        {
            "native_training_results.csv": training,
            "native_profile_selection.csv": profile_summary,
            "native_held_out.csv": held_out,
            "native_paired.csv": paired,
            "native_cpmm_control.csv": cpmm,
            "native_cpmm_matched_policy_ablation.csv": cpmm_matched_policy,
            "native_controller_counterfactual.csv": counterfactual,
            "native_neighborhood.csv": neighborhood,
            "native_scale_sensitivity.csv": scale,
            "native_cadence_sensitivity.csv": cadence,
            "native_volume_sensitivity.csv": volume_sensitivity,
            "native_matching_basis_sensitivity.csv": basis_sensitivity,
            "native_dusk_fee_split_sensitivity.csv": fee_split_sensitivity,
        },
        summary,
    )
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    main()

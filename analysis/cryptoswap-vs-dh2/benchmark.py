#!/usr/bin/env python3
"""Reproducible economic comparison of Curve, current DH2, and log-DH2."""

from __future__ import annotations

from concurrent.futures import ProcessPoolExecutor
import csv
import hashlib
import json
import math
from pathlib import Path
import statistics
import time
from typing import Iterable, Sequence

from comparison import (
    CandidateScore,
    ISO_FIRST_GRID_POINTS,
    ISO_FIRST_MAX_REFINEMENTS,
    ISO_FIRST_OBJECTIVE_CONVERGENCE_BPS,
    ISO_SECOND_AUDIT_SCAN_POINTS,
    ISO_SECOND_SCAN_POINTS,
    PROFILE_PRICE_RATIOS,
    PROFILE_TRADE_BPS,
    _iso_target_best,
    _log_grid,
    candidate_score,
    current_candidates,
    declared_parameter_bounds,
    curve_candidates,
    execution_impact_bps,
    execution_impact_bps_exact_out,
    execution_index_bps,
    jump_loss_index_bps,
    low_slippage_width_bps,
    match_to_curve,
    model_family,
    parameter_dict,
    pareto_frontier,
    proposed_candidates,
    service_distance,
    service_max_log_error,
    service_profile,
)
from economics import POOL_VALUE, simulate_path
from models import (
    Amm,
    Cpmm,
    CryptoSwap,
    CurrentDh2,
    HyperbolicSharpDh2,
    LogPotentialDh2,
    SharpDh2,
    scan_bracketed_roots,
    with_zero_fees,
)
from scenarios import historical_windows, synthetic_paths


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"
EXECUTION_STEP_MINUTES = 5
SEEDS = (7, 41)
FLOW_REGIMES = (
    {
        "flow_regime": "benign",
        "arb_latency_steps": 1,
        "informed_flow_share": 0.0,
        "daily_volume_pool_ratio": 0.25,
    },
    {
        "flow_regime": "toxic_delayed",
        "arb_latency_steps": 6,
        "informed_flow_share": 0.50,
        "daily_volume_pool_ratio": 0.25,
    },
)
SHARP_PEAK_DEPTHS = (50.0, 200.0, 323.0, 800.0, 2_000.0)
SHARP_FADE_SCALES = (0.001, 0.01, 0.1, 0.5)
SHARP_WITNESS = {
    "peak_depth": 323.0,
    "legacy_internal_amplification": 161.5,
    "fade_scale": 0.5,
    "x": 6.776596755,
    "y": 0.036891674,
}
MATCHING_FAMILY_ORDER = ("curve", "current_dh2", "proposed_dh2")
MATCHING_BASIS_ORDER = ("equal_execution", "equal_jump_risk")


def _run_matching_iso_task(
    task: tuple[str, str, float],
) -> tuple[str, str, Amm, dict[str, object]]:
    """Run one independent family/basis iso search in a worker process."""

    basis, family, target = task
    if basis == "equal_execution":
        metric = execution_index_bps
        objective = jump_loss_index_bps
    elif basis == "equal_jump_risk":
        metric = jump_loss_index_bps
        objective = execution_index_bps
    else:
        raise ValueError(f"unknown matching basis {basis}")

    if family == "curve":
        constructor = lambda first, second: CryptoSwap(  # noqa: E731
            amplification=first,
            gamma=second,
        )
        prototype: Amm = CryptoSwap()
    elif family == "current_dh2":
        constructor = lambda first, second: CurrentDh2(  # noqa: E731
            peak_depth=first,
            fade_scale=second,
        )
        prototype = CurrentDh2()
    elif family == "proposed_dh2":
        constructor = lambda first, second: LogPotentialDh2(  # noqa: E731
            depth_multiplier=first,
            half_width_bps=second,
        )
        prototype = LogPotentialDh2()
    else:
        raise ValueError(f"unknown matching family {family}")

    first_bounds, second_bounds = declared_parameter_bounds(prototype)
    first_values = _log_grid(
        first_bounds[0],
        first_bounds[1],
        ISO_FIRST_GRID_POINTS,
    )
    diagnostics: dict[str, object] = {}
    model = _iso_target_best(
        constructor,
        first_values,
        second_bounds,
        target,
        metric,
        objective,
        diagnostics=diagnostics,
    )
    return basis, family, model, diagnostics


def _parallel_matching_iso_models(
    target_execution: float,
    target_risk: float,
) -> tuple[
    tuple[Amm, Amm, Amm],
    tuple[Amm, Amm, Amm],
    dict[str, dict[str, object]],
    dict[str, dict[str, object]],
]:
    """Run the six independent iso searches and restore canonical ordering."""

    targets = {
        "equal_execution": target_execution,
        "equal_jump_risk": target_risk,
    }
    tasks = [
        (basis, family, targets[basis])
        for basis in MATCHING_BASIS_ORDER
        for family in MATCHING_FAMILY_ORDER
    ]
    with ProcessPoolExecutor(max_workers=len(tasks)) as executor:
        results = list(executor.map(_run_matching_iso_task, tasks))

    by_key: dict[tuple[str, str], tuple[Amm, dict[str, object]]] = {}
    for basis, family, model, diagnostics in results:
        key = (basis, family)
        if key in by_key:
            raise RuntimeError(f"duplicate matching result for {basis}/{family}")
        by_key[key] = model, diagnostics
    expected_keys = {
        (basis, family)
        for basis in MATCHING_BASIS_ORDER
        for family in MATCHING_FAMILY_ORDER
    }
    if set(by_key) != expected_keys:
        missing = sorted(expected_keys - set(by_key))
        unexpected = sorted(set(by_key) - expected_keys)
        raise RuntimeError(
            f"incomplete matching results: {missing=} {unexpected=}"
        )

    equal_execution = tuple(
        by_key[("equal_execution", family)][0]
        for family in MATCHING_FAMILY_ORDER
    )
    equal_risk = tuple(
        by_key[("equal_jump_risk", family)][0]
        for family in MATCHING_FAMILY_ORDER
    )
    equal_execution_diagnostics = {
        family: by_key[("equal_execution", family)][1]
        for family in MATCHING_FAMILY_ORDER
    }
    equal_risk_diagnostics = {
        family: by_key[("equal_jump_risk", family)][1]
        for family in MATCHING_FAMILY_ORDER
    }
    return (
        equal_execution,
        equal_risk,
        equal_execution_diagnostics,
        equal_risk_diagnostics,
    )


def write_csv(path: Path, rows: list[dict[str, object]]) -> None:
    if not rows:
        raise ValueError(f"no rows for {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as target:
        writer = csv.DictWriter(target, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def percentile(values: Sequence[float], probability: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return math.nan
    position = (len(ordered) - 1) * probability
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    weight = position - lower
    return ordered[lower] * (1.0 - weight) + ordered[upper] * weight


def parameter_label(model: Amm) -> str:
    if isinstance(model, CryptoSwap):
        return f"A={model.amplification:.8g};gamma={model.gamma:.8g}"
    if isinstance(model, SharpDh2):
        return (
            f"peak_depth={model.peak_depth:.8g};"
            f"fade_scale={model.fade_scale:.8g};fixed_exponent=2"
        )
    if isinstance(model, HyperbolicSharpDh2):
        return (
            f"peak_depth={model.peak_depth:.8g};"
            f"fade_scale={model.fade_scale:.8g};fixed_exponent=4;"
            "coordinate=hyperbolic"
        )
    if isinstance(model, CurrentDh2):
        return (
            f"peak_depth={model.peak_depth:.8g};"
            f"fade_scale={model.fade_scale:.8g}"
        )
    if isinstance(model, LogPotentialDh2):
        return (
            f"depth={model.depth_multiplier:.8g};"
            f"half_width_bps={model.half_width_bps:.8g}"
        )
    return "cpmm"


def score_without_width(model: Amm) -> CandidateScore:
    return CandidateScore(
        model=model,
        execution_index_bps=execution_index_bps(model),
        jump_loss_index_bps=jump_loss_index_bps(model),
        center_100bps_impact=max(
            execution_impact_bps(model, 1.0, 100, "x_to_y"),
            execution_impact_bps(model, 1.0, 100, "y_to_x"),
        ),
        low_slippage_width_bps=math.nan,
        tail_100bps_impact=max(
            execution_impact_bps(model, price, 100, direction)
            for price in (2.0 / 3.0, 1.5)
            for direction in ("x_to_y", "y_to_x")
        ),
    )


def build_decay_profile_rows() -> list[dict[str, object]]:
    """Compare only the protocol-fixed fade functions.

    ``normalized_imbalance`` is ``u / fade_scale``.  Both profiles therefore
    have the same center depth and the same operator knobs; the sharp profile
    has no third shape parameter.
    """
    rows = []
    for normalized_imbalance in (
        0.0,
        0.01,
        0.025,
        0.05,
        0.1,
        0.25,
        0.5,
        1.0,
        2.0,
        4.0,
        10.0,
        100.0,
    ):
        legacy_retention = 1.0 / (1.0 + normalized_imbalance)
        sharp_retention = 1.0 / (1.0 + normalized_imbalance**2)
        rows.append(
            {
                "normalized_imbalance_u_over_fade_scale": normalized_imbalance,
                "legacy_effective_depth_fraction": legacy_retention,
                "sharp_effective_depth_fraction": sharp_retention,
                "sharp_minus_legacy_fraction": sharp_retention - legacy_retention,
                "sharp_to_legacy_ratio": sharp_retention / legacy_retention,
                "fixed_shape_exponent": SharpDh2.SHAPE_EXPONENT,
                "interpretation": (
                    "same_center"
                    if normalized_imbalance == 0.0
                    else "sharp_retains_more_inside_shoulder"
                    if normalized_imbalance < 1.0
                    else "same_at_fade_scale"
                    if normalized_imbalance == 1.0
                    else "sharp_decays_faster_after_shoulder"
                ),
            }
        )
    return rows


def sharp_root_evidence_rows() -> list[dict[str, object]]:
    model = SharpDh2(
        peak_depth=float(SHARP_WITNESS["peak_depth"]),
        fade_scale=float(SHARP_WITNESS["fade_scale"]),
    )
    x = float(SHARP_WITNESS["x"])
    y = float(SHARP_WITNESS["y"])
    lower = 2.0 * math.sqrt(x * y)
    upper = x + y
    roots = scan_bracketed_roots(
        lambda invariant_d: model._residual(invariant_d, x, y),
        lower,
        upper,
        intervals=65_536,
    )
    return [
        {
            "candidate": "sharp_dh2",
            "formula": "A_eff=(peak_depth/2)/(1+(u/fade_scale)^2)",
            "fixed_shape_exponent": SharpDh2.SHAPE_EXPONENT,
            "peak_depth": model.peak_depth,
            "legacy_internal_amplification": model.legacy_amplification,
            "fade_scale": model.fade_scale,
            "reserve_x": x,
            "reserve_y": y,
            "search_lower_2sqrtxy": lower,
            "search_upper_x_plus_y": upper,
            "root_index": index,
            "root_count": len(roots),
            "invariant_d": root,
            "absolute_residual": abs(model._residual(root, x, y)),
            "decision": "hard_reject_multiple_invariant_roots",
            "arithmetic_scope": "analytical_float64_safety_witness_not_rust_validated",
        }
        for index, root in enumerate(roots, start=1)
    ]


def _metric_or_nan(metric, model: Amm, computed_status: str) -> tuple[float, str]:
    try:
        return float(metric(model)), computed_status
    except (ArithmeticError, OverflowError, ValueError, ZeroDivisionError) as error:
        return math.nan, f"solver_error:{type(error).__name__}"


def build_same_parameter_delta_rows() -> list[dict[str, object]]:
    """Sweep only peak depth and fade scale for legacy versus sharp DH2.

    The economic metrics are retained for diagnosis, but every sharp row is
    non-decision-grade after the family-level multiple-root counterexample.
    """
    rows: list[dict[str, object]] = []
    for peak_depth in SHARP_PEAK_DEPTHS:
        for fade_scale in SHARP_FADE_SCALES:
            legacy = CurrentDh2(
                peak_depth=peak_depth,
                fade_scale=fade_scale,
            )
            sharp = SharpDh2(
                peak_depth=peak_depth,
                fade_scale=fade_scale,
            )
            legacy_execution, legacy_execution_status = _metric_or_nan(
                execution_index_bps,
                legacy,
                "computed",
            )
            sharp_execution, sharp_execution_status = _metric_or_nan(
                execution_index_bps,
                sharp,
                "computed_but_non_decision_grade",
            )
            legacy_jump, legacy_jump_status = _metric_or_nan(
                jump_loss_index_bps,
                legacy,
                "computed",
            )
            sharp_jump, sharp_jump_status = _metric_or_nan(
                jump_loss_index_bps,
                sharp,
                "computed_but_non_decision_grade",
            )
            rows.append(
                {
                    "peak_depth": peak_depth,
                    "fade_scale": fade_scale,
                    "fixed_shape_exponent": SharpDh2.SHAPE_EXPONENT,
                    "legacy_internal_amplification": peak_depth / 2.0,
                    "legacy_execution_index_bps": legacy_execution,
                    "sharp_execution_index_bps": sharp_execution,
                    "sharp_minus_legacy_execution_bps": (
                        sharp_execution - legacy_execution
                    ),
                    "legacy_jump_loss_index_bps": legacy_jump,
                    "sharp_jump_loss_index_bps": sharp_jump,
                    "sharp_minus_legacy_jump_loss_bps": sharp_jump - legacy_jump,
                    "legacy_execution_status": legacy_execution_status,
                    "sharp_execution_status": sharp_execution_status,
                    "legacy_jump_status": legacy_jump_status,
                    "sharp_jump_status": sharp_jump_status,
                    "sharp_family_hard_rejected": True,
                    "rejection_reason": (
                        "multiple invariant roots at peak_depth=323, "
                        "fade_scale=0.5; see sharp_root_evidence.csv"
                    ),
                    "arithmetic_scope": (
                        "continuous analytical comparison; sharp DH2 is not Rust validated"
                    ),
                }
            )
    return rows


def classify_p4_economics(
    jump_loss_delta_fraction: float,
    execution_delta_fraction: float,
    *,
    tolerance: float = 1e-9,
) -> str:
    """Classify p4 only from the two like-for-like scalar objectives."""
    deltas = (jump_loss_delta_fraction, execution_delta_fraction)
    if all(delta >= -tolerance for delta in deltas) and any(
        delta > tolerance for delta in deltas
    ):
        return "reject_economically"
    if all(delta <= tolerance for delta in deltas) and any(
        delta < -tolerance for delta in deltas
    ):
        return "p4_improves_scalar_cross_sections"
    if all(abs(delta) <= tolerance for delta in deltas):
        return "scalar_cross_section_tie"
    return "mixed_economic_result"


def p4_production_decision(
    *,
    naive_multiple_roots: bool,
    p4_economic_decision: str,
    p4_max_scanned_roots: int,
    p4_rust_validated: bool,
) -> str:
    """Translate measured safety/economic evidence into a production status."""
    if naive_multiple_roots and p4_economic_decision == "reject_economically":
        return "keep_current_dh2_fade"
    if (
        p4_economic_decision == "p4_improves_scalar_cross_sections"
        and p4_max_scanned_roots == 1
        and not p4_rust_validated
    ):
        return "defer_p4_pending_fixed_point_validation"
    if (
        p4_economic_decision == "p4_improves_scalar_cross_sections"
        and p4_max_scanned_roots == 1
        and p4_rust_validated
    ):
        return "p4_candidate_requires_full_decision_analysis"
    return "no_replacement_decision"


def build_hyperbolic_p4_rows(
    legacy_equal_execution: CurrentDh2,
    legacy_equal_risk: CurrentDh2,
) -> tuple[list[dict[str, object]], HyperbolicSharpDh2]:
    """Compare p4 to the main search's selected current-DH2 iso points."""
    execution_target = execution_index_bps(legacy_equal_execution)
    p4_equal_execution = _iso_target_best(
        lambda peak_depth, fade_scale: HyperbolicSharpDh2(
            peak_depth=peak_depth,
            fade_scale=fade_scale,
        ),
        _log_grid(2.0, 2_000.0, 7),
        (1e-6, 0.5),
        execution_target,
        execution_index_bps,
        jump_loss_index_bps,
    )

    risk_target = jump_loss_index_bps(legacy_equal_risk)
    p4_equal_risk = _iso_target_best(
        lambda peak_depth, fade_scale: HyperbolicSharpDh2(
            peak_depth=peak_depth,
            fade_scale=fade_scale,
        ),
        _log_grid(2.0, 2_000.0, 7),
        (1e-6, 0.5),
        risk_target,
        jump_loss_index_bps,
        execution_index_bps,
    )

    p4_jump_delta_fraction = (
        jump_loss_index_bps(p4_equal_execution)
        / jump_loss_index_bps(legacy_equal_execution)
        - 1.0
    )
    p4_execution_delta_fraction = (
        execution_index_bps(p4_equal_risk)
        / execution_index_bps(legacy_equal_risk)
        - 1.0
    )
    p4_decision = classify_p4_economics(
        p4_jump_delta_fraction,
        p4_execution_delta_fraction,
    )

    rows: list[dict[str, object]] = []
    for basis, legacy, p4 in (
        (
            "equal_execution",
            legacy_equal_execution,
            p4_equal_execution,
        ),
        ("equal_jump_risk", legacy_equal_risk, p4_equal_risk),
    ):
        legacy_execution = execution_index_bps(legacy)
        p4_execution = execution_index_bps(p4)
        legacy_jump = jump_loss_index_bps(legacy)
        p4_jump = jump_loss_index_bps(p4)
        for family, model, execution, jump in (
            ("current_dh2", legacy, legacy_execution, legacy_jump),
            ("hyperbolic_p4_dh2", p4, p4_execution, p4_jump),
        ):
            rows.append(
                {
                    "basis": basis,
                    "model": family,
                    "peak_depth": model.peak_depth,
                    "fade_scale": model.fade_scale,
                    "fixed_shape_exponent": (
                        HyperbolicSharpDh2.SHAPE_EXPONENT
                        if family == "hyperbolic_p4_dh2"
                        else ""
                    ),
                    "fade_coordinate": (
                        "abs(x-y)/(2*sqrt(x*y))"
                        if family == "hyperbolic_p4_dh2"
                        else "((x-y)/D)^2"
                    ),
                    "execution_index_bps": execution,
                    "jump_loss_index_bps": jump,
                    "execution_delta_vs_legacy_bps": execution - legacy_execution,
                    "execution_delta_vs_legacy_fraction": (
                        execution / legacy_execution - 1.0
                    ),
                    "jump_loss_delta_vs_legacy_bps": jump - legacy_jump,
                    "jump_loss_delta_vs_legacy_fraction": jump / legacy_jump - 1.0,
                    "decision": (
                        "reference_baseline"
                        if family == "current_dh2"
                        else p4_decision
                    ),
                    "arithmetic_scope": (
                        "rust_fixture_validated_continuous_reference"
                        if family == "current_dh2"
                        else "analytical_float64_not_rust_validated"
                    ),
                }
            )
    return rows, p4_equal_execution


def build_hyperbolic_p4_root_scan_rows(
    model: HyperbolicSharpDh2,
) -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for ratio in (
        10 ** (-4.0 + index * 8.0 / 40.0)
        for index in range(41)
    ):
        x = math.sqrt(ratio)
        y = 1.0 / math.sqrt(ratio)
        roots = scan_bracketed_roots(
            lambda invariant_d: model._residual(invariant_d, x, y),
            2.0 * math.sqrt(x * y),
            x + y,
            intervals=4_096,
        )
        rows.append(
            {
                "model": "hyperbolic_p4_dh2",
                "peak_depth": model.peak_depth,
                "fade_scale": model.fade_scale,
                "reserve_ratio_x_over_y": ratio,
                "root_count": len(roots),
                "roots": ";".join(f"{root:.17g}" for root in roots),
                "scan_intervals": 4_096,
                "arithmetic_scope": "analytical_float64_not_rust_validated",
            }
        )
    return rows


def build_pareto_rows() -> tuple[list[dict[str, object]], list[CandidateScore]]:
    scores: list[CandidateScore] = []
    for model in [
        *curve_candidates(),
        *current_candidates(),
        *proposed_candidates(),
    ]:
        scores.append(score_without_width(model))
    combined = pareto_frontier(scores)
    combined_ids = {repr(score.model) for score in combined}
    family_ids: set[str] = set()
    for family in ("curve", "current_dh2", "proposed_dh2"):
        family_scores = [score for score in scores if model_family(score.model) == family]
        family_ids.update(repr(score.model) for score in pareto_frontier(family_scores))
    rows = []
    for score in scores:
        parameters = parameter_dict(score.model)
        rows.append(
            {
                "model": model_family(score.model),
                "parameter_1": parameters["parameter_1"],
                "parameter_2": parameters["parameter_2"],
                "execution_index_bps": score.execution_index_bps,
                "jump_loss_index_bps": score.jump_loss_index_bps,
                "center_100bps_impact": score.center_100bps_impact,
                "tail_100bps_impact": score.tail_100bps_impact,
                "family_pareto": repr(score.model) in family_ids,
                "combined_pareto": repr(score.model) in combined_ids,
            }
        )
    cpmm = score_without_width(Cpmm())
    rows.append(
        {
            "model": "cpmm",
            "parameter_1": 0.0,
            "parameter_2": 0.0,
            "execution_index_bps": cpmm.execution_index_bps,
            "jump_loss_index_bps": cpmm.jump_loss_index_bps,
            "center_100bps_impact": cpmm.center_100bps_impact,
            "tail_100bps_impact": cpmm.tail_100bps_impact,
            "family_pareto": True,
            "combined_pareto": True,
        }
    )
    return rows, combined


def build_matching_rows() -> tuple[
    list[dict[str, object]],
    tuple[Amm, Amm, Amm],
    tuple[Amm, Amm, Amm],
]:
    reference_curve = CryptoSwap(amplification=400.0, gamma=0.001)
    target_execution = execution_index_bps(reference_curve)
    target_risk = jump_loss_index_bps(reference_curve)
    (
        equal_execution,
        equal_risk,
        equal_execution_diagnostics,
        equal_risk_diagnostics,
    ) = _parallel_matching_iso_models(
        target_execution,
        target_risk,
    )
    nearest_curve, nearest_current, nearest_log, _ = match_to_curve(reference_curve)
    target_profile = service_profile(reference_curve)
    rows: list[dict[str, object]] = []

    for basis, models in (
        (
            "nearest_full_profile",
            (nearest_curve, nearest_current, nearest_log),
        ),
        ("equal_execution", equal_execution),
        ("equal_jump_risk", equal_risk),
    ):
        for model in models:
            parameters = parameter_dict(model)
            first_bounds, second_bounds = declared_parameter_bounds(model)
            family = model_family(model)
            iso_diagnostics = (
                equal_execution_diagnostics[family]
                if basis == "equal_execution"
                else equal_risk_diagnostics[family]
                if basis == "equal_jump_risk"
                else None
            )
            final_first_grid_points = int(
                iso_diagnostics["final_first_grid_points"]
                if iso_diagnostics is not None
                else (
                    (ISO_FIRST_GRID_POINTS - 1)
                    * (2**ISO_FIRST_MAX_REFINEMENTS)
                    + 1
                )
            )
            second_audit_points = (
                [
                    int(value)
                    for value in iso_diagnostics[
                        "second_axis_winner_audit_points"
                    ]
                ]
                if iso_diagnostics is not None
                else [ISO_SECOND_SCAN_POINTS, *ISO_SECOND_AUDIT_SCAN_POINTS]
            )
            first_cell = math.log(first_bounds[1] / first_bounds[0]) / (
                final_first_grid_points - 1
            )
            second_cell = math.log(second_bounds[1] / second_bounds[0]) / (
                second_audit_points[-1] - 1
            )
            near_first_boundary = min(
                abs(math.log(parameters["parameter_1"] / first_bounds[0])),
                abs(math.log(first_bounds[1] / parameters["parameter_1"])),
            ) <= first_cell + 1e-12
            near_second_boundary = min(
                abs(math.log(parameters["parameter_2"] / second_bounds[0])),
                abs(math.log(second_bounds[1] / parameters["parameter_2"])),
            ) <= second_cell + 1e-12
            execution = execution_index_bps(model)
            risk = jump_loss_index_bps(model)
            rows.append(
                {
                    "basis": basis,
                    "model": model_family(model),
                    "parameter_1": parameters["parameter_1"],
                    "parameter_2": parameters["parameter_2"],
                    "parameter_1_domain_min": first_bounds[0],
                    "parameter_1_domain_max": first_bounds[1],
                    "parameter_2_domain_min": second_bounds[0],
                    "parameter_2_domain_max": second_bounds[1],
                    "parameter_1_at_domain_boundary": math.isclose(
                        parameters["parameter_1"],
                        first_bounds[0],
                        rel_tol=1e-10,
                    )
                    or math.isclose(
                        parameters["parameter_1"],
                        first_bounds[1],
                        rel_tol=1e-10,
                    ),
                    "parameter_2_at_domain_boundary": math.isclose(
                        parameters["parameter_2"],
                        second_bounds[0],
                        rel_tol=1e-10,
                    )
                    or math.isclose(
                        parameters["parameter_2"],
                        second_bounds[1],
                        rel_tol=1e-10,
                    ),
                    "iso_root_method": (
                        "17->33->65 first-axis log-grid convergence; all "
                        "scan-exposed second-axis sign-change roots and tangent "
                        "checks at 129 points; selected winner audited at "
                        "257 and 513 points; 48-step root refinement; "
                        "non-finite islands isolated"
                        if basis in {"equal_execution", "equal_jump_risk"}
                        else "not applicable"
                    ),
                    "iso_search_converged": (
                        bool(iso_diagnostics["converged"])
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_first_grid_coarse_points": (
                        int(iso_diagnostics["coarse_first_grid_points"])
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_first_grid_final_points": (
                        int(iso_diagnostics["final_first_grid_points"])
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_first_objective_convergence_tolerance_bps": (
                        float(
                            iso_diagnostics[
                                "objective_convergence_tolerance_bps"
                            ]
                        )
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_first_last_objective_improvement_bps": (
                        float(iso_diagnostics["last_objective_improvement_bps"])
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_first_last_parameter_log_change": (
                        float(
                            iso_diagnostics[
                                "last_first_parameter_log_change"
                            ]
                        )
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_first_last_second_parameter_log_change": (
                        float(
                            iso_diagnostics[
                                "last_second_parameter_log_change"
                            ]
                        )
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_second_grid_points": (
                        ";".join(str(value) for value in second_audit_points)
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_second_winner_scan_exposed_root_counts": (
                        ";".join(
                            str(row["scan_exposed_root_count"])
                            for row in iso_diagnostics[
                                "second_axis_winner_levels"
                            ]
                        )
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_second_axis_converged": (
                        bool(iso_diagnostics["second_axis_converged"])
                        if iso_diagnostics is not None
                        else ""
                    ),
                    "iso_boundary_censored": (
                        basis in {"equal_execution", "equal_jump_risk"}
                        and (near_first_boundary or near_second_boundary)
                    ),
                    "parameter_label": parameter_label(model),
                    "execution_index_bps": execution,
                    "jump_loss_index_bps": risk,
                    "execution_target_error_fraction": execution / target_execution - 1.0,
                    "jump_risk_target_error_fraction": risk / target_risk - 1.0,
                    "full_profile_log_rmse": service_distance(model, target_profile),
                    "full_profile_max_multiplicative_error": math.exp(
                        service_max_log_error(model, target_profile)
                    )
                    - 1.0,
                    "center_100bps_impact": max(
                        execution_impact_bps(model, 1.0, 100, "x_to_y"),
                        execution_impact_bps(model, 1.0, 100, "y_to_x"),
                    ),
                    "low_slippage_width_bps": low_slippage_width_bps(model),
                }
            )
    return rows, equal_execution, equal_risk


def build_shape_rows(models: Iterable[Amm]) -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for model in models:
        for price in PROFILE_PRICE_RATIOS:
            for trade_bps in PROFILE_TRADE_BPS:
                for direction in ("x_to_y", "y_to_x"):
                    for order_type in ("exact_in", "exact_out"):
                        impact = (
                            execution_impact_bps(
                                model,
                                price,
                                trade_bps,
                                direction,
                            )
                            if order_type == "exact_in"
                            else execution_impact_bps_exact_out(
                                model,
                                price,
                                trade_bps,
                                direction,
                            )
                        )
                        rows.append(
                            {
                                "model": model_family(model),
                                "parameter_label": parameter_label(model),
                                "normalized_marginal_price": price,
                                "log_price_distance_bps": math.log(price) * 10_000.0,
                                "trade_bps_of_marked_tvl": trade_bps,
                                "direction": direction,
                                "order_type": order_type,
                                "execution_cost_bps": impact,
                            }
                        )
    return rows


def build_routing_rows(models: Sequence[Amm]) -> list[dict[str, object]]:
    rows = []
    for price in PROFILE_PRICE_RATIOS:
        for trade_bps in PROFILE_TRADE_BPS:
            for direction in ("x_to_y", "y_to_x"):
                costs = {
                    model_family(model): execution_impact_bps(
                        model,
                        price,
                        trade_bps,
                        direction,
                    )
                    for model in models
                }
                best = min(costs.values())
                winners = [
                    model
                    for model, cost in costs.items()
                    if cost <= best + 1e-9
                ]
                for model, cost in costs.items():
                    rows.append(
                        {
                            "normalized_marginal_price": price,
                            "trade_bps_of_marked_tvl": trade_bps,
                            "direction": direction,
                            "model": model,
                            "execution_cost_bps": cost,
                            "best_quote": model in winners,
                            "equal_intent_volume_share": (
                                1.0 / len(winners) if model in winners else 0.0
                            ),
                        }
                    )
    return rows


def run_path_matrix(models: Sequence[Amm]) -> list[dict[str, object]]:
    raw_paths = synthetic_paths()
    raw_paths.update(historical_windows())
    paths = {
        name: values[::EXECUTION_STEP_MINUTES]
        for name, values in raw_paths.items()
    }
    rows: list[dict[str, object]] = []
    for model in models:
        for path_name, prices in paths.items():
            for regime in FLOW_REGIMES:
                for seed in SEEDS:
                    row = simulate_path(
                        model,
                        path_name,
                        prices,
                        step_minutes=EXECUTION_STEP_MINUTES,
                        arb_latency_steps=int(regime["arb_latency_steps"]),
                        informed_share=float(regime["informed_flow_share"]),
                        daily_volume_pool_ratio=float(
                            regime["daily_volume_pool_ratio"]
                        ),
                        seed=seed,
                    )
                    row["comparison_basis"] = "equal_execution"
                    row["parameter_label"] = parameter_label(model)
                    row["flow_regime"] = regime["flow_regime"]
                    rows.append(row)
    return rows


PAIR_KEYS = (
    "path",
    "path_class",
    "flow_regime",
    "arb_latency_minutes",
    "informed_flow_share",
    "daily_volume_pool_ratio",
    "seed",
)


def paired_rows(path_rows: list[dict[str, object]]) -> list[dict[str, object]]:
    grouped: dict[tuple[object, ...], dict[str, dict[str, object]]] = {}
    for row in path_rows:
        key = tuple(row[field] for field in PAIR_KEYS)
        grouped.setdefault(key, {})[str(row["model"])] = row
    rows = []
    for key, models in grouped.items():
        if set(models) != {"curve", "current_dh2", "proposed_dh2"}:
            raise AssertionError(f"incomplete paired cell {key}: {set(models)}")
        curve = models["curve"]
        for competitor in ("current_dh2", "proposed_dh2"):
            other = models[competitor]
            row = {field: value for field, value in zip(PAIR_KEYS, key)}
            row.update(
                {
                    "competitor": competitor,
                    "curve_net_lp_pnl": curve["net_lp_pnl_vs_hold"],
                    "competitor_net_lp_pnl": other["net_lp_pnl_vs_hold"],
                    "paired_net_lp_delta": float(other["net_lp_pnl_vs_hold"])
                    - float(curve["net_lp_pnl_vs_hold"]),
                    "competitor_wins": float(other["net_lp_pnl_vs_hold"])
                    > float(curve["net_lp_pnl_vs_hold"]),
                    "curve_execution_fees": curve["gross_fee_value"],
                    "competitor_execution_fees": other["gross_fee_value"],
                    "curve_realized_arb_profit": curve["realized_arb_profit"],
                    "competitor_realized_arb_profit": other[
                        "realized_arb_profit"
                    ],
                }
            )
            rows.append(row)
    return rows


def build_regime_summary(pair_rows: list[dict[str, object]]) -> list[dict[str, object]]:
    rows = []
    group_fields = ("competitor", "path_class", "flow_regime")
    groups: dict[tuple[object, ...], list[dict[str, object]]] = {}
    for row in pair_rows:
        key = tuple(row[field] for field in group_fields)
        groups.setdefault(key, []).append(row)
    for key, values in sorted(groups.items(), key=lambda item: str(item[0])):
        deltas = [float(row["paired_net_lp_delta"]) for row in values]
        rows.append(
            {
                **{field: value for field, value in zip(group_fields, key)},
                "paired_cells": len(values),
                "median_paired_net_lp_delta": statistics.median(deltas),
                "p10_paired_net_lp_delta": percentile(deltas, 0.10),
                "p90_paired_net_lp_delta": percentile(deltas, 0.90),
                "win_fraction": sum(delta > 0 for delta in deltas) / len(deltas),
            }
        )
    return rows


def historical_window_win_fraction(
    pair_rows: list[dict[str, object]],
    competitor: str,
) -> float:
    by_path: dict[str, list[float]] = {}
    for row in pair_rows:
        if row["competitor"] == competitor and row["path_class"] == "historical":
            by_path.setdefault(str(row["path"]), []).append(
                float(row["paired_net_lp_delta"])
            )
    if not by_path:
        return math.nan
    wins = sum(statistics.median(values) > 0 for values in by_path.values())
    return wins / len(by_path)


def safety_checks(models: Sequence[Amm]) -> list[dict[str, object]]:
    rows = []
    for model in (Cpmm(), *models):
        failures = 0
        non_monotonic = 0
        maximum_invariant_error = 0.0
        maximum_roundtrip_gain = 0.0
        max_invariant_roots = 1
        root_evidence = ""
        start = time.perf_counter()
        if isinstance(model, SharpDh2):
            witness_rows = sharp_root_evidence_rows()
            max_invariant_roots = max(
                int(row["root_count"]) for row in witness_rows
            )
            root_evidence = "sharp_root_evidence.csv"
        elif isinstance(model, HyperbolicSharpDh2):
            p4_root_rows = build_hyperbolic_p4_root_scan_rows(model)
            max_invariant_roots = max(
                int(row["root_count"]) for row in p4_root_rows
            )
            root_evidence = "hyperbolic_p4_root_scan.csv"
        for ratio in [
            10 ** (-4.0 + index * 8.0 / 40.0)
            for index in range(41)
        ]:
            x = 1_000_000.0 * math.sqrt(ratio)
            y = 1_000_000.0 / math.sqrt(ratio)
            previous = 0.0
            for bps in (1, 10, 100, 500, 2_000):
                amount = (x + y) * bps / 10_000.0
                try:
                    quote = with_zero_fees(model).quote(
                        x,
                        y,
                        amount,
                        "x_to_y",
                        apply_fee=False,
                    )
                    if quote.amount_out <= previous:
                        non_monotonic += 1
                    previous = quote.amount_out
                    before = model.invariant(x, y)
                    after = model.invariant(
                        x + amount,
                        y - quote.amount_out,
                    )
                    maximum_invariant_error = max(
                        maximum_invariant_error,
                        abs(after / before - 1.0),
                    )
                except (ArithmeticError, OverflowError, ValueError):
                    failures += 1
        for bps in (1, 10, 100, 1_000):
            amount = 2_000_000.0 * bps / 10_000.0
            try:
                first = with_zero_fees(model).quote(
                    1_000_000.0,
                    1_000_000.0,
                    amount,
                    "x_to_y",
                    apply_fee=False,
                )
                floored = math.floor(first.amount_out * 1e9) / 1e9
                second = with_zero_fees(model).quote(
                    1_000_000.0 + amount,
                    1_000_000.0 - floored,
                    floored,
                    "y_to_x",
                    apply_fee=False,
                )
                returned = math.floor(second.amount_out * 1e9) / 1e9
                maximum_roundtrip_gain = max(
                    maximum_roundtrip_gain,
                    returned - amount,
                )
            except (ArithmeticError, OverflowError, ValueError):
                failures += 1
        hard_rejected = isinstance(model, SharpDh2) and max_invariant_roots > 1
        rows.append(
            {
                "model": model_family(model),
                "solver_failures": failures,
                "non_monotonic_quotes": non_monotonic,
                "max_relative_invariant_error": maximum_invariant_error,
                "max_zero_fee_roundtrip_gain_quote": maximum_roundtrip_gain,
                "max_invariant_roots": max_invariant_roots,
                "root_evidence": root_evidence,
                "hard_rejected": hard_rejected,
                "rejection_reason": (
                    "multiple invariant roots"
                    if hard_rejected
                    else ""
                ),
                "runtime_seconds": time.perf_counter() - start,
                "arithmetic_scope": (
                    "rust_fixed_point_fixture_validated_with_10ppm_quote_haircut"
                    if type(model) is CurrentDh2
                    else "analytical_experiment_not_rust_validated"
                    if isinstance(model, (SharpDh2, HyperbolicSharpDh2))
                    else "high_precision_analytical_reference"
                ),
            }
        )
    return rows


def summarize(
    matching_rows: list[dict[str, object]],
    pareto_rows: list[dict[str, object]],
    combined_frontier: list[CandidateScore],
    path_rows: list[dict[str, object]],
    pair_rows: list[dict[str, object]],
    routing_rows: list[dict[str, object]],
    safety_rows: list[dict[str, object]],
) -> dict[str, object]:
    equal_execution = [
        row for row in matching_rows if row["basis"] == "equal_execution"
    ]
    equal_risk = [
        row for row in matching_rows if row["basis"] == "equal_jump_risk"
    ]
    equal_execution_winner = min(
        equal_execution,
        key=lambda row: float(row["jump_loss_index_bps"]),
    )["model"]
    equal_risk_winner = min(
        equal_risk,
        key=lambda row: float(row["execution_index_bps"]),
    )["model"]

    paired_summary: dict[str, object] = {}
    for competitor in ("current_dh2", "proposed_dh2"):
        paired_summary[competitor] = {}
        for path_class in ("synthetic", "historical"):
            values = [
                row
                for row in pair_rows
                if row["competitor"] == competitor
                and row["path_class"] == path_class
            ]
            deltas = [float(row["paired_net_lp_delta"]) for row in values]
            class_summary = {
                "paired_cells": len(values),
                "median_paired_net_lp_delta": statistics.median(deltas),
                "p10_paired_net_lp_delta": percentile(deltas, 0.10),
                "p90_paired_net_lp_delta": percentile(deltas, 0.90),
                "win_fraction": sum(delta > 0 for delta in deltas) / len(deltas),
            }
            if path_class == "historical":
                class_summary["window_median_win_fraction"] = (
                    historical_window_win_fraction(pair_rows, competitor)
                )
            paired_summary[competitor][path_class] = class_summary

    model_path_summary: dict[str, object] = {}
    for family in ("curve", "current_dh2", "proposed_dh2"):
        model_path_summary[family] = {}
        for path_class in ("synthetic", "historical"):
            values = [
                float(row["net_lp_pnl_vs_hold"])
                for row in path_rows
                if row["model"] == family
                and row["path_class"] == path_class
            ]
            model_path_summary[family][path_class] = {
                "cells": len(values),
                "median_net_lp_pnl_vs_hold": statistics.median(values),
                "p10_net_lp_pnl_vs_hold": percentile(values, 0.10),
                "p90_net_lp_pnl_vs_hold": percentile(values, 0.90),
            }

    routing_summary = {}
    total_intents = len(routing_rows) / 3.0
    for family in ("curve", "current_dh2", "proposed_dh2"):
        rows = [row for row in routing_rows if row["model"] == family]
        routing_summary[family] = {
            "best_quote_intent_share": sum(
                float(row["equal_intent_volume_share"]) for row in rows
            )
            / total_intents,
            "median_execution_cost_bps": statistics.median(
                float(row["execution_cost_bps"]) for row in rows
            ),
        }

    frontier_counts = {
        family: sum(
            model_family(score.model) == family
            for score in combined_frontier
        )
        for family in ("curve", "current_dh2", "proposed_dh2")
    }
    all_families_on_frontier = all(value > 0 for value in frontier_counts.values())
    decision_grade_safety = [
        row for row in safety_rows if row["model"] != "sharp_dh2"
    ]
    safety_pass = all(
        int(row["solver_failures"]) == 0
        and int(row["non_monotonic_quotes"]) == 0
        and float(row["max_zero_fee_roundtrip_gain_quote"]) <= 1e-9
        and int(row["max_invariant_roots"]) == 1
        for row in decision_grade_safety
    )
    sharp_rejected = any(
        row["model"] == "sharp_dh2"
        and bool(row["hard_rejected"])
        and int(row["max_invariant_roots"]) > 1
        for row in safety_rows
    )

    reference_winner = (
        equal_execution_winner
        if equal_execution_winner == equal_risk_winner
        else "no_single_winner"
    )
    return {
        "status": "corrected_economic_comparison",
        "decision": {
            "universal_economic_winner": None,
            "material_reference_region_winner": None,
            "reason": (
                "All three curve families occupy part of the sampled "
                "execution-versus-jump-loss Pareto frontier, and the "
                "reference-point gaps are parameter-domain-conditioned."
                if all_families_on_frontier
                else "The frontier does not support one design across all risk regions."
            ),
            "reference_risk_region_numerical_winner": reference_winner,
            "equal_execution_lowest_jump_loss": equal_execution_winner,
            "equal_jump_risk_lowest_execution_cost": equal_risk_winner,
            "native_system_result_location": "results/native_summary.json",
            "native_system_scope_note": (
                "This summary covers the invariant-only and controlled-system "
                "layers. The independently audited fixed-liquidity native "
                "fee/controller comparison is reported in native_summary.json."
            ),
        },
        "matching": matching_rows,
        "frontier": {
            "concentrated_combined_points": len(combined_frontier),
            "family_point_counts": frontier_counts,
            "all_families_present": all_families_on_frontier,
            "scope": "Discrete declared parameter sweep; CPMM is reported separately.",
        },
        "controlled_paths": {
            "comparison_basis": "equal_execution",
            "models": model_path_summary,
            "paired_vs_curve": paired_summary,
            "scenario_weighting": (
                "Synthetic and historical classes are reported separately. "
                "Within each class every path, flow regime, and seed has equal weight."
            ),
        },
        "routing": routing_summary,
        "safety": {
            "common_reference_checks_pass": safety_pass,
            "sharp_dh2_hard_rejected": sharp_rejected,
            "sharp_dh2_rejection_reason": (
                "The fixed exponent-2 fade admits multiple invariant roots; "
                "see sharp_root_evidence.csv."
            ),
            "current_dh2_rust_fixture_validated": True,
            "current_dh2_rust_quote_haircut_ppm": 10,
            "implementation_readiness_affects_economic_ranking": False,
        },
        "metric_definitions": {
            "execution_index_bps": (
                "Geometric mean zero-fee exact-input execution cost across "
                "declared price states, order sizes, and both directions."
            ),
            "jump_loss_index_bps": (
                "Mean one-jump loss versus hold after profit-maximizing "
                "zero-fee arbitrage, in bps of initial TVL."
            ),
            "net_lp_pnl_vs_hold": (
                "Ending executable plus token-denominated claimable wealth "
                "minus the value of holding the initial tokens."
            ),
            "realized_arb_profit": (
                "Positive after-fee arbitrage surplus actually executed; it "
                "is not used as a synonym for all LVR."
            ),
            "recenter_impairment_value": (
                "Protected-Q capacity consumed by center changes; diagnostic "
                "only and not added again to loss versus hold."
            ),
        },
        "provenance": {
            "historical_snapshot_sha256": hashlib.sha256(
                (ROOT / "data" / "solusdt_1m.csv").read_bytes()
            ).hexdigest(),
            "historical_source_granularity_minutes": 1,
            "economic_execution_step_minutes": EXECUTION_STEP_MINUTES,
            "path_rows": len(path_rows),
            "paired_rows": len(pair_rows),
            "pareto_rows": len(pareto_rows),
            "routing_rows": len(routing_rows),
            "seeds": list(SEEDS),
            "parameter_domain": {
                "curve_paper_amplification_min": 0.1,
                "curve_paper_amplification_max": 1_000,
                "curve_gamma_min": 1e-8,
                "curve_gamma_max": 0.199,
                "curve_domain_source": (
                    "Twocrypto-NG v2.1 pinned parameter checks, converted "
                    "from raw ANN to the benchmark's paper-A normalization"
                ),
                "current_dh2_peak_depth_min": 2,
                "current_dh2_peak_depth_max": 2_000,
                "current_dh2_fade_scale_min": 1e-6,
                "current_dh2_fade_scale_max": 0.5,
                "cpmm_control": "peak_depth=0;fade_scale=0",
                "current_dh2_peak_depth_definition": (
                    "additional center marginal-depth multiplier; legacy "
                    "internal amplification equals peak_depth / 2"
                ),
                "proposed_dh2_depth_min": 3,
                "proposed_dh2_depth_max": 2_001,
                "proposed_dh2_half_width_bps_min": 1,
                "proposed_dh2_half_width_bps_max": 20_000,
                "proposed_dh2_domain_status": (
                    "experimental; center-depth range matched to Dusk's total "
                    "native-valid 1+peak_depth range; no protocol-valid log-DH2 domain"
                ),
                "boundary_note": (
                    "Any iso-service optimum at a declared boundary is "
                    "reported as domain-bounded, never as a global family "
                    "optimum. Curve and Dusk use their native-valid endpoint "
                    "domains; log-DH2 uses an explicit experimental domain."
                ),
                "iso_root_search": (
                    "17->33->65 globally interleaved first-parameter log grids "
                    "with a 0.001-bp objective convergence requirement; "
                    "129-point second-parameter log scan with every "
                    "scan-exposed sign-change root refined for 48 bisection "
                    "steps and same-sign tangent minima checked; selected "
                    "winner rescanned at 257 and 513 points with stable "
                    "objective, parameter, and scan-exposed root-count "
                    "requirements; non-finite islands not bridged; no "
                    "monotonicity assumption"
                ),
            },
        },
    }


def main() -> None:
    start = time.perf_counter()
    RESULTS.mkdir(parents=True, exist_ok=True)

    matching_rows, equal_execution, equal_risk = build_matching_rows()
    write_csv(RESULTS / "parameter_matching.csv", matching_rows)

    decay_profile_rows = build_decay_profile_rows()
    write_csv(RESULTS / "decay_profile.csv", decay_profile_rows)
    same_parameter_rows = build_same_parameter_delta_rows()
    write_csv(RESULTS / "same_parameter_deltas.csv", same_parameter_rows)
    root_evidence_rows = sharp_root_evidence_rows()
    if len(root_evidence_rows) != 3:
        raise AssertionError(
            "sharp-DH2 safety witness must reproduce exactly three invariant roots"
        )
    write_csv(RESULTS / "sharp_root_evidence.csv", root_evidence_rows)
    current_equal_execution = next(
        model for model in equal_execution if type(model) is CurrentDh2
    )
    current_equal_risk = next(
        model for model in equal_risk if type(model) is CurrentDh2
    )
    hyperbolic_p4_rows, hyperbolic_p4_safety_model = build_hyperbolic_p4_rows(
        current_equal_execution,
        current_equal_risk,
    )
    write_csv(RESULTS / "hyperbolic_p4_comparison.csv", hyperbolic_p4_rows)
    hyperbolic_p4_root_rows = build_hyperbolic_p4_root_scan_rows(
        hyperbolic_p4_safety_model
    )
    write_csv(
        RESULTS / "hyperbolic_p4_root_scan.csv",
        hyperbolic_p4_root_rows,
    )

    pareto_rows, combined_frontier = build_pareto_rows()
    write_csv(RESULTS / "pareto.csv", pareto_rows)

    shape_rows = build_shape_rows((Cpmm(), *equal_execution))
    write_csv(RESULTS / "shape.csv", shape_rows)

    routing_rows = build_routing_rows(equal_execution)
    write_csv(RESULTS / "routing.csv", routing_rows)

    path_rows = run_path_matrix(equal_execution)
    write_csv(RESULTS / "path_results.csv", path_rows)
    pairs = paired_rows(path_rows)
    write_csv(RESULTS / "paired_differences.csv", pairs)
    regime_rows = build_regime_summary(pairs)
    write_csv(RESULTS / "regime_summary.csv", regime_rows)

    safety_rows = safety_checks(
        (
            *equal_execution,
            SharpDh2(
                peak_depth=float(SHARP_WITNESS["peak_depth"]),
                fade_scale=float(SHARP_WITNESS["fade_scale"]),
            ),
            hyperbolic_p4_safety_model,
        )
    )
    write_csv(RESULTS / "safety.csv", safety_rows)

    summary = summarize(
        matching_rows,
        pareto_rows,
        combined_frontier,
        path_rows,
        pairs,
        routing_rows,
        safety_rows,
    )
    p4_equal_execution = {
        row["model"]: row
        for row in hyperbolic_p4_rows
        if row["basis"] == "equal_execution"
    }
    p4_equal_risk = {
        row["model"]: row
        for row in hyperbolic_p4_rows
        if row["basis"] == "equal_jump_risk"
    }
    p4_decisions = {
        str(row["decision"])
        for row in hyperbolic_p4_rows
        if row["model"] == "hyperbolic_p4_dh2"
    }
    if len(p4_decisions) != 1:
        raise RuntimeError("p4 iso-service rows disagree on economic decision")
    p4_economic_decision = next(iter(p4_decisions))
    p4_max_scanned_roots = max(
        int(row["root_count"]) for row in hyperbolic_p4_root_rows
    )
    naive_multiple_roots = len(root_evidence_rows) > 1
    p4_rust_validated = False
    production_decision = p4_production_decision(
        naive_multiple_roots=naive_multiple_roots,
        p4_economic_decision=p4_economic_decision,
        p4_max_scanned_roots=p4_max_scanned_roots,
        p4_rust_validated=p4_rust_validated,
    )
    summary["sharper_fade"] = {
        "naive_d_dependent_exponent_2": {
            "decision": (
                "hard_reject" if naive_multiple_roots else "no_hard_reject"
            ),
            "reason": (
                "multiple invariant roots"
                if naive_multiple_roots
                else "committed witness did not show multiple roots"
            ),
            "root_count_in_committed_witness": len(root_evidence_rows),
            "rust_validated": False,
        },
        "d_independent_hyperbolic_exponent_4": {
            "decision": p4_economic_decision,
            "invariant_roots_max_in_scanned_states": p4_max_scanned_roots,
            "jump_loss_delta_at_equal_execution_fraction": float(
                p4_equal_execution["hyperbolic_p4_dh2"][
                    "jump_loss_delta_vs_legacy_fraction"
                ]
            ),
            "execution_delta_at_equal_jump_risk_fraction": float(
                p4_equal_risk["hyperbolic_p4_dh2"][
                    "execution_delta_vs_legacy_fraction"
                ]
            ),
            "rust_validated": p4_rust_validated,
        },
        "production_decision": production_decision,
    }
    summary["provenance"]["runtime_seconds"] = time.perf_counter() - start
    summary["provenance"]["sharp_decay_rows"] = len(decay_profile_rows)
    summary["provenance"]["sharp_same_parameter_rows"] = len(
        same_parameter_rows
    )
    summary["provenance"]["sharp_root_evidence_rows"] = len(
        root_evidence_rows
    )
    summary["provenance"]["hyperbolic_p4_comparison_rows"] = len(
        hyperbolic_p4_rows
    )
    summary["provenance"]["hyperbolic_p4_root_scan_rows"] = len(
        hyperbolic_p4_root_rows
    )
    (RESULTS / "summary.json").write_text(
        json.dumps(summary, indent=2) + "\n"
    )
    (RESULTS / "scenario_weights.json").write_text(
        json.dumps(
            {
                "policy": (
                    "Do not pool synthetic and historical classes into one "
                    "winner statistic."
                ),
                "within_class": {
                    "path": "equal",
                    "flow_regime": "equal",
                    "seed": "equal",
                },
                "flow_regimes": list(FLOW_REGIMES),
                "seeds": list(SEEDS),
            },
            indent=2,
        )
        + "\n"
    )
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()

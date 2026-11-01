"""Fail-closed evidence contract for the final native decision report.

This module is deliberately outside the replay/search engine hash boundary.  It
does not select profiles, run a replay, or alter canonical benchmark outputs.
It verifies that the already-produced artifacts form one internally consistent
chain:

1. symmetric decision-grade Curve and Dusk searches plus an independent CPMM search;
2. explicit, source-bound promotion snapshots;
3. the canonical synthetic selection and development-history replay;
4. an immutable pre-history lock; and
5. a complete post-lock sealed replay.

The sealed economic check is confirmatory only.  A failed confirmation is a
valid report result; missing, stale, or inconsistent evidence is a build error.
"""

from __future__ import annotations

import csv
import json
import math
from pathlib import Path
import statistics
import tempfile
from typing import Any, Mapping, Sequence

from native_benchmark import PRIMARY_SEEDS
from promote_controller_profiles import current_search_fingerprint
from sealed_protocol import (
    LOCKED_SYSTEMS,
    LOCK_PATH,
    SEARCH_SOURCE_PATHS,
    SEALED_CSV_PATH,
    SEALED_MANIFEST_PATH,
    SEALED_METADATA_PATH,
    SEALED_MONTHS,
    SEALED_OUTPUT_DIR,
    SEALED_ROUTES,
    SEALED_STRATA,
    canonical_fingerprint,
    search_source_bindings,
    sha256_path,
    verify_postlock_hashes,
    verify_profile_lock,
)


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"
DATA = ROOT / "data"

FINAL_REPORT_AUDIT_PATH = RESULTS / "final_report_audit.json"
FINAL_REPORT_EVIDENCE_SCHEMA_VERSION = 1
PROMOTION_SCHEMA_VERSION = 2
EXPECTED_SEARCH_PROFILES = 150
EXPECTED_SEARCH_CELLS_PER_PROFILE = 128
EXPECTED_SEARCH_ROWS = 19_200
EXPECTED_SEALED_WINDOWS = 15
EXPECTED_SEALED_BASE_CELLS = 240
EXPECTED_SEALED_RESULT_ROWS = 720
EXPECTED_SEALED_PAIRED_ROWS = 720
ACCOUNTING_ERROR_LIMIT = 1e-5

PROMOTION_SPECS = {
    "curve_native_v2_1": {
        "path": "data/curve_controller_profiles.json",
        "search_name": "curve_controller_search",
    },
    "dusk_native": {
        "path": "data/dusk_controller_profiles.json",
        "search_name": "dusk_controller_search",
    },
    "dusk_cpmm": {
        "path": "data/cpmm_controller_profiles.json",
        "search_name": "cpmm_controller_search",
    },
}
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

SEALED_DECISION_FIELDS = (
    "net_lp_return_after_protocol_fee_bps",
    "net_lp_return_vs_hold_bps",
    "selected_execution_cost_bps",
    "captured_user_volume_fraction",
    "depth_degradation_fraction",
    "marginal_tracking_stale_fraction",
    "accounting_identity_error",
)
SEALED_PAIR_FIELDS = (
    "left_minus_right_net_lp_return_after_protocol_fee_bps",
    "left_minus_right_gross_net_lp_return_bps",
    "left_minus_right_selected_execution_cost_bps",
    "left_minus_right_captured_volume_fraction",
    "left_minus_right_depth_degradation_fraction",
    "left_minus_right_marginal_tracking_stale_fraction",
)
PAIR_SPECS = {
    "dusk_minus_curve": ("dusk_native", "curve_native_v2_1"),
    "dusk_minus_cpmm": ("dusk_native", "dusk_cpmm"),
    "curve_minus_cpmm": ("curve_native_v2_1", "dusk_cpmm"),
}
EXPECTED_FLOWS = (
    "benign",
    "mixed",
    "toxic_delayed",
    "low_volume_stuck",
)


def _read_json(path: Path, *, label: str) -> dict[str, Any]:
    if not path.exists():
        raise FileNotFoundError(f"missing {label}: {path}")
    try:
        payload = json.loads(path.read_text())
    except json.JSONDecodeError as error:
        raise ValueError(f"invalid {label} JSON: {path}") from error
    if not isinstance(payload, dict):
        raise TypeError(f"{label} must be a JSON object")
    return payload


def _read_csv(path: Path, *, label: str) -> list[dict[str, str]]:
    if not path.exists():
        raise FileNotFoundError(f"missing {label}: {path}")
    with path.open(newline="") as source:
        rows = list(csv.DictReader(source))
    if not rows:
        raise ValueError(f"{label} is empty: {path}")
    return rows


def _relative_path(root: Path, value: object, *, label: str) -> Path:
    text = str(value)
    if not text or Path(text).is_absolute() or ".." in Path(text).parts:
        raise ValueError(f"{label} must be a project-relative path")
    path = (root / text).resolve()
    try:
        path.relative_to(root.resolve())
    except ValueError as error:
        raise ValueError(f"{label} escapes the benchmark root") from error
    return path


def _require_sha256(value: object, *, label: str) -> str:
    digest = str(value)
    if len(digest) != 64 or any(
        character not in "0123456789abcdef" for character in digest
    ):
        raise ValueError(f"{label} must be a lowercase SHA-256 digest")
    return digest


def _require_hash(path: Path, expected: object, *, label: str) -> str:
    digest = _require_sha256(expected, label=label)
    actual = sha256_path(path)
    if actual != digest:
        raise ValueError(f"{label} mismatch: {path}")
    return actual


def _finite(value: object, *, label: str) -> float:
    try:
        parsed = float(value)
    except (TypeError, ValueError, OverflowError) as error:
        raise ValueError(f"{label} must be numeric") from error
    if not math.isfinite(parsed):
        raise ValueError(f"{label} must be finite")
    return parsed


def _close(actual: object, expected: object, *, label: str) -> None:
    left = _finite(actual, label=f"actual {label}")
    right = _finite(expected, label=f"expected {label}")
    if not math.isclose(left, right, rel_tol=1e-12, abs_tol=1e-12):
        raise ValueError(f"{label} mismatch: actual={left!r}, expected={right!r}")


def _promotion_fingerprint(payload: Mapping[str, Any]) -> str:
    unsigned = dict(payload)
    unsigned.pop("promotion_fingerprint", None)
    return canonical_fingerprint(unsigned)


def _search_fingerprint_for_promotion(
    binding: Mapping[str, Any], search_name: str
) -> str:
    fingerprint = binding.get("search_fingerprint")
    if search_name == "dusk_controller_search":
        if not isinstance(fingerprint, dict) or set(fingerprint) != {
            "grid",
            "local",
        }:
            raise ValueError("Dusk search binding lacks grid/local fingerprints")
        return _require_sha256(
            fingerprint["grid"], label="Dusk grid search fingerprint"
        )
    return _require_sha256(fingerprint, label=f"{search_name} search fingerprint")


def _validate_promotion(
    *,
    root: Path,
    system: str,
    spec: Mapping[str, str],
    binding: Mapping[str, Any],
    selected: Mapping[str, Any],
) -> dict[str, Any]:
    path = root / spec["path"]
    payload = _read_json(path, label=f"{system} promotion snapshot")
    if payload.get("schema_version") != PROMOTION_SCHEMA_VERSION:
        raise ValueError(f"unsupported {system} promotion schema")
    if payload.get("system") != system:
        raise ValueError(f"{system} promotion snapshot has the wrong system")
    source = payload.get("search_source")
    if not isinstance(source, dict):
        raise ValueError(f"{system} promotion snapshot lacks source provenance")

    search_name = spec["search_name"]
    expected_summary_path = SEARCH_SOURCE_PATHS[search_name]
    if source.get("summary_path") != expected_summary_path:
        raise ValueError(f"{system} promotion summary path mismatch")
    summary_path = _relative_path(
        root, source["summary_path"], label=f"{system} promotion summary"
    )
    summary_hash = _require_hash(
        summary_path,
        source.get("summary_sha256"),
        label=f"{system} promotion summary hash",
    )
    if summary_hash != binding.get("sha256"):
        raise ValueError(f"{system} promotion is bound to a stale search summary")

    expected_search_fingerprint = _search_fingerprint_for_promotion(
        binding, search_name
    )
    if source.get("fingerprint") != expected_search_fingerprint:
        raise ValueError(f"{system} promotion search fingerprint mismatch")
    if current_search_fingerprint(system) != expected_search_fingerprint:
        raise ValueError(
            f"{system} promotion refers to a search produced by stale engine, "
            "grid, matrix, or matched-invariant inputs"
        )
    winner = binding.get("winner")
    if not isinstance(winner, dict) or payload.get("winner_profile_id") != winner.get(
        "profile_id"
    ):
        raise ValueError(f"{system} promotion winner does not match the search")

    source_files = {}
    source_paths: dict[str, Path] = {}
    for field in ("stage_manifest", "profiles"):
        path_field = f"{field}_path"
        hash_field = f"{field}_sha256"
        source_path = _relative_path(
            root,
            source.get(path_field),
            label=f"{system} promotion {field}",
        )
        source_paths[field] = source_path
        source_files[field] = {
            "path": str(source.get(path_field)),
            "sha256": _require_hash(
                source_path,
                source.get(hash_field),
                label=f"{system} promotion {field} hash",
            ),
        }

    stage_manifest = _read_json(
        source_paths["stage_manifest"], label=f"{system} grid manifest"
    )
    rows_path = source_paths["stage_manifest"].parent / "rows.csv"
    content_hashes = stage_manifest.get("content_hashes")
    if (
        stage_manifest.get("fingerprint") != expected_search_fingerprint
        or int(stage_manifest.get("profiles", -1)) != EXPECTED_SEARCH_PROFILES
        or int(stage_manifest.get("cells_per_profile", -1))
        != EXPECTED_SEARCH_CELLS_PER_PROFILE
        or int(stage_manifest.get("rows", -1)) != EXPECTED_SEARCH_ROWS
        or stage_manifest.get("selected_profile_ids")
        != [str(payload.get("winner_profile_id", ""))]
        or not isinstance(content_hashes, dict)
        or content_hashes.get("profiles.csv")
        != sha256_path(source_paths["profiles"])
        or content_hashes.get("rows.csv") != sha256_path(rows_path)
    ):
        raise ValueError(f"{system} promotion grid content binding mismatch")
    source_profile_rows = {
        str(row["profile_id"]): row
        for row in _read_csv(
            source_paths["profiles"], label=f"{system} grid profiles"
        )
    }
    if len(source_profile_rows) != EXPECTED_SEARCH_PROFILES:
        raise ValueError(f"{system} source profile table is not complete")
    rho_field = (
        "independent_curve_service_relaxation"
        if system == "curve_native_v2_1"
        else "independent_dusk_service_relaxation"
    )
    try:
        ranked_source_rows = sorted(
            (
                source_row
                for source_row in source_profile_rows.values()
                if str(source_row.get("hard_valid", "")).strip().lower() == "true"
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
        raise ValueError(f"{system} source ranking fields are invalid") from error
    if len(ranked_source_rows) != 5:
        raise ValueError(f"{system} source lacks five ranked policies")
    expected_rank = {
        str(source_row["profile_id"]): rank
        for rank, source_row in enumerate(ranked_source_rows, start=1)
    }

    fingerprint = _require_sha256(
        payload.get("promotion_fingerprint"),
        label=f"{system} promotion fingerprint",
    )
    if fingerprint != _promotion_fingerprint(payload):
        raise ValueError(f"{system} promotion fingerprint mismatch")

    profiles = payload.get("profiles")
    if not isinstance(profiles, list) or len(profiles) != 5:
        raise ValueError(f"{system} promotion must contain five ranked policies")
    profile_by_id: dict[str, dict[str, Any]] = {}
    ranks: list[int] = []
    source_fields = PROMOTION_SOURCE_FIELDS[system]
    for row in profiles:
        if not isinstance(row, dict):
            raise TypeError(f"{system} promoted profile must be an object")
        profile_id = str(row.get("profile_id", ""))
        description = str(row.get("description", ""))
        if not profile_id or not description or profile_id in profile_by_id:
            raise ValueError(f"{system} promotion has an invalid profile identity")
        if not isinstance(row.get("model"), dict) or not row["model"]:
            raise ValueError(f"{system} promoted model config is missing")
        if not isinstance(row.get("config"), dict) or not row["config"]:
            raise ValueError(f"{system} promoted controller config is missing")
        try:
            rank = int(row["source_rank"])
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError(f"{system} promoted source rank is invalid") from error
        if rank <= 0:
            raise ValueError(f"{system} promoted source rank must be positive")
        if expected_rank.get(profile_id) != rank:
            raise ValueError(
                f"{system} promoted policy is not at its searched top-five rank"
            )
        ranks.append(rank)
        source_row = source_profile_rows.get(profile_id)
        if source_row is None:
            raise ValueError(f"{system} promoted profile is absent from its grid")
        if row.get("source_profile_row_sha256") != canonical_fingerprint(
            source_row
        ):
            raise ValueError(f"{system} promoted profile row hash mismatch")
        if description != str(source_row.get("description", "")):
            raise ValueError(f"{system} promoted description differs from its grid")
        for field in source_fields:
            promoted_value = _finite(
                row.get(field), label=f"{system} promoted {field}"
            )
            source_value = _finite(
                source_row.get(field), label=f"{system} source {field}"
            )
            if promoted_value != source_value:
                raise ValueError(
                    f"{system} promoted field {field} differs from its searched row"
                )
        profile_by_id[profile_id] = row
    if len(set(ranks)) != len(ranks) or sorted(ranks) != list(
        range(1, len(ranks) + 1)
    ):
        raise ValueError(f"{system} promoted source ranks must be contiguous")
    winner_id = str(payload["winner_profile_id"])
    if winner_id not in profile_by_id or int(profile_by_id[winner_id]["source_rank"]) != 1:
        raise ValueError(f"{system} search winner must be promoted at rank 1")

    selected_id = str(selected.get("profile_id", ""))
    selected_row = profile_by_id.get(selected_id)
    if selected_row is None:
        raise ValueError(
            f"canonical {system} profile {selected_id!r} is absent from its promotion"
        )
    if str(selected.get("description", "")) != str(selected_row["description"]):
        raise ValueError(f"canonical {system} profile description is stale")

    return {
        "system": system,
        "search": search_name,
        "search_profiles": EXPECTED_SEARCH_PROFILES,
        "search_cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
        "search_rows": EXPECTED_SEARCH_ROWS,
        "search_summary_path": expected_summary_path,
        "search_summary_sha256": summary_hash,
        "search_fingerprint": expected_search_fingerprint,
        "search_winner_profile_id": winner_id,
        "promoted_profiles": len(profiles),
        "promotion_path": spec["path"],
        "promotion_sha256": sha256_path(path),
        "promotion_fingerprint": fingerprint,
        "source_files": source_files,
        "canonical_locked_profile_id": selected_id,
        "canonical_locked_description": str(selected_row["description"]),
        "canonical_locked_source_rank": int(selected_row["source_rank"]),
        "canonical_locked_model": selected_row["model"],
        "canonical_locked_controller": selected_row["config"],
        "profile_ids": sorted(profile_by_id),
    }


def _validate_promotions_match_lock(
    search_promotions: Mapping[str, Any],
    lock: Mapping[str, Any],
) -> None:
    promotions = search_promotions.get("promotions")
    locked_profiles = lock.get("locked_profiles")
    if not isinstance(promotions, dict) or not isinstance(locked_profiles, dict):
        raise ValueError("promotion or sealed-profile inventory is incomplete")
    if set(promotions) != set(LOCKED_SYSTEMS) or set(locked_profiles) != set(
        LOCKED_SYSTEMS
    ):
        raise ValueError("promotion and sealed lock must contain the same three systems")
    for system in LOCKED_SYSTEMS:
        promoted = promotions[system]
        locked = locked_profiles[system]
        if not isinstance(promoted, dict) or not isinstance(locked, dict):
            raise TypeError(f"invalid promoted or locked profile for {system}")
        if promoted.get("canonical_locked_profile_id") != locked.get("profile_id"):
            raise ValueError(f"{system} promotion profile id differs from sealed lock")
        if promoted.get("canonical_locked_description") != locked.get("description"):
            raise ValueError(f"{system} promotion description differs from sealed lock")
        if canonical_fingerprint(promoted.get("canonical_locked_model")) != canonical_fingerprint(
            locked.get("model")
        ):
            raise ValueError(f"{system} promoted model differs from sealed lock")
        if canonical_fingerprint(
            promoted.get("canonical_locked_controller")
        ) != canonical_fingerprint(locked.get("config")):
            raise ValueError(f"{system} promoted controller differs from sealed lock")


def validate_search_and_promotions(
    native_summary: Mapping[str, Any],
    *,
    root: Path = ROOT,
    bindings: Mapping[str, Mapping[str, Any]] | None = None,
) -> dict[str, Any]:
    """Validate the symmetric searches and the explicit promotion boundary."""

    selection = native_summary.get("profile_selection")
    if not isinstance(selection, dict) or not set(LOCKED_SYSTEMS).issubset(selection):
        raise ValueError("canonical native summary lacks Curve, DH2, or CPMM locks")
    if bindings is None:
        paths = {
            name: root / relative for name, relative in SEARCH_SOURCE_PATHS.items()
        }
        bindings = search_source_bindings(paths=paths)
    if set(bindings) != set(SEARCH_SOURCE_PATHS):
        raise ValueError("final report requires Curve, DH2, and CPMM search bindings")

    promotions = {}
    for system, spec in PROMOTION_SPECS.items():
        binding = bindings[spec["search_name"]]
        matrix = binding.get("matrix")
        if matrix != {
            "profiles": EXPECTED_SEARCH_PROFILES,
            "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
            "rows": EXPECTED_SEARCH_ROWS,
        }:
            raise ValueError(f"{system} search is not the complete 150 x 128 matrix")
        promotions[system] = _validate_promotion(
            root=root,
            system=system,
            spec=spec,
            binding=binding,
            selected=selection[system],
        )

    return {
        "status": "passed",
        "symmetric_concentrated_search": True,
        "independent_cpmm_search": True,
        "profiles_per_family": EXPECTED_SEARCH_PROFILES,
        "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
        "rows_per_family": EXPECTED_SEARCH_ROWS,
        "promotions": promotions,
    }


def expected_native_counts(
    search_promotion_evidence: Mapping[str, Any],
) -> dict[str, int]:
    """Derive canonical matrix counts from the frozen promoted shortlists."""

    promotions = search_promotion_evidence.get("promotions")
    if not isinstance(promotions, dict):
        raise ValueError("promotion evidence is incomplete")
    curve_profiles = int(promotions["curve_native_v2_1"]["promoted_profiles"])
    dusk_profiles = int(promotions["dusk_native"]["promoted_profiles"])
    cpmm_profiles = int(promotions["dusk_cpmm"]["promoted_profiles"])
    if curve_profiles <= 0 or dusk_profiles <= 0 or cpmm_profiles <= 0:
        raise ValueError("canonical promoted profile counts must be positive")
    training_profiles = curve_profiles + dusk_profiles + cpmm_profiles
    concentrated_profiles = curve_profiles + dusk_profiles
    return {
        "profile_rows": training_profiles,
        "training_rows": 128 * training_profiles,
        "held_out_rows": 480,
        "paired_rows": 240,
        "cpmm_rows": 240,
        "cpmm_matched_policy_ablation_rows": 240,
        "counterfactual_rows": 480,
        "neighborhood_rows": 48 * concentrated_profiles,
        "scale_rows": 288,
        "cadence_rows": 96,
        "volume_sensitivity_rows": 288,
        "basis_sensitivity_rows": 1_440,
        "fee_split_sensitivity_rows": 960,
    }


def _percentile(values: Sequence[float], probability: float) -> float:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("cannot summarize an empty sealed metric")
    position = (len(ordered) - 1) * probability
    low = math.floor(position)
    high = math.ceil(position)
    if low == high:
        return ordered[low]
    weight = position - low
    return ordered[low] * (1.0 - weight) + ordered[high] * weight


def _tail_mean(values: Sequence[float], probability: float) -> float:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("cannot summarize an empty sealed tail")
    count = max(1, math.ceil(len(ordered) * probability))
    return statistics.mean(ordered[:count])


def _result_key(row: Mapping[str, Any]) -> tuple[Any, ...]:
    return (
        str(row["system"]),
        str(row["profile_id"]),
        str(row["path"]),
        str(row["flow_regime"]),
        int(row["seed"]),
        str(row["routing_mode"]),
    )


def _base_key(row: Mapping[str, Any]) -> tuple[Any, ...]:
    return (
        str(row["path"]),
        str(row["source_month"]),
        str(row["stratum"]),
        str(row["flow_regime"]),
        int(row["seed"]),
        str(row["routing_mode"]),
        float(row["initial_pool_value"]),
        int(row["step_minutes"]),
        str(row["match_basis"]),
    )


def _verify_paired_rows(
    result_rows: Sequence[Mapping[str, Any]],
    paired_rows: Sequence[Mapping[str, Any]],
) -> None:
    result_by_cell: dict[tuple[Any, ...], dict[str, Mapping[str, Any]]] = {}
    for row in result_rows:
        result_by_cell.setdefault(_base_key(row), {})[str(row["system"])] = row
    if len(result_by_cell) != EXPECTED_SEALED_BASE_CELLS or any(
        set(systems) != set(LOCKED_SYSTEMS) for systems in result_by_cell.values()
    ):
        raise ValueError("sealed result rows do not form the complete system cross")

    seen: set[tuple[Any, ...]] = set()
    for row in paired_rows:
        comparison = str(row["comparison"])
        if comparison not in PAIR_SPECS:
            raise ValueError(f"unknown sealed pair comparison: {comparison}")
        key = _base_key(row)
        identity = (*key, comparison)
        if identity in seen:
            raise ValueError("sealed paired output contains duplicate cells")
        seen.add(identity)
        left_name, right_name = PAIR_SPECS[comparison]
        if row.get("left_system") != left_name or row.get("right_system") != right_name:
            raise ValueError(f"sealed pair orientation mismatch: {comparison}")
        systems = result_by_cell.get(key)
        if systems is None:
            raise ValueError("sealed pair has no underlying result cell")
        left = systems[left_name]
        right = systems[right_name]
        expected = {
            "left_minus_right_net_lp_return_after_protocol_fee_bps": (
                float(left["net_lp_return_after_protocol_fee_bps"])
                - float(right["net_lp_return_after_protocol_fee_bps"])
            ),
            "left_minus_right_gross_net_lp_return_bps": (
                float(left["net_lp_return_vs_hold_bps"])
                - float(right["net_lp_return_vs_hold_bps"])
            ),
            "left_minus_right_selected_execution_cost_bps": (
                float(left["selected_execution_cost_bps"])
                - float(right["selected_execution_cost_bps"])
            ),
            "left_minus_right_captured_volume_fraction": (
                float(left["captured_user_volume_fraction"])
                - float(right["captured_user_volume_fraction"])
            ),
            "left_minus_right_depth_degradation_fraction": (
                float(left["depth_degradation_fraction"])
                - float(right["depth_degradation_fraction"])
            ),
            "left_minus_right_marginal_tracking_stale_fraction": (
                float(left["marginal_tracking_stale_fraction"])
                - float(right["marginal_tracking_stale_fraction"])
            ),
        }
        for field, value in expected.items():
            _close(row[field], value, label=f"sealed paired {comparison} {field}")
    if len(seen) != EXPECTED_SEALED_PAIRED_ROWS:
        raise ValueError("sealed paired output is incomplete")


def _group_metrics(
    rows: Sequence[Mapping[str, Any]],
    *,
    comparison: str,
    sign: float,
    route: str | None = None,
    flow: str | None = None,
) -> dict[str, float]:
    cells = [
        row
        for row in rows
        if row["comparison"] == comparison
        and (route is None or row["routing_mode"] == route)
        and (flow is None or row["flow_regime"] == flow)
    ]
    if not cells:
        raise ValueError("sealed confirmation group is empty")
    returns = [
        sign * float(row["left_minus_right_net_lp_return_after_protocol_fee_bps"])
        for row in cells
    ]
    execution = [
        sign * float(row["left_minus_right_selected_execution_cost_bps"])
        for row in cells
    ]
    capture = [
        sign * float(row["left_minus_right_captured_volume_fraction"])
        for row in cells
    ]
    stale = [
        sign * float(row["left_minus_right_marginal_tracking_stale_fraction"])
        for row in cells
    ]
    by_window: dict[str, list[float]] = {}
    for row, value in zip(cells, returns):
        by_window.setdefault(str(row["path"]), []).append(value)
    window_medians = [statistics.median(values) for values in by_window.values()]
    return {
        "cells": float(len(cells)),
        "median_return_advantage_bps": statistics.median(returns),
        "bottom_decile_mean_return_advantage_bps": _tail_mean(returns, 0.10),
        "cell_win_fraction": sum(value > 0.0 for value in returns) / len(returns),
        "window_median_win_fraction": (
            sum(value > 0.0 for value in window_medians) / len(window_medians)
        ),
        "p90_execution_regression_bps": _percentile(execution, 0.90),
        "p10_capture_advantage_fraction": _percentile(capture, 0.10),
        "p90_marginal_tracking_stale_regression_fraction": _percentile(
            stale, 0.90
        ),
    }


def _core_predicates(
    route_metrics: Mapping[str, Mapping[str, float]],
    flow_route_metrics: Mapping[str, Mapping[str, float]],
) -> dict[str, bool]:
    def service_safe(groups: Sequence[Mapping[str, float]]) -> bool:
        return all(
            row["p90_execution_regression_bps"] <= 1.0
            and row["p10_capture_advantage_fraction"] >= -0.02
            and row["p90_marginal_tracking_stale_regression_fraction"] <= 0.05
            for row in groups
        )

    route_rows = list(route_metrics.values())
    flow_rows = list(flow_route_metrics.values())
    return {
        "both_routes_median_advantage_at_least_1bp": all(
            row["median_return_advantage_bps"] >= 1.0 for row in route_rows
        ),
        "both_routes_window_win_fraction_at_least_70pct": all(
            row["window_median_win_fraction"] >= 0.70 for row in route_rows
        ),
        "both_routes_bottom_decile_mean_not_below_minus_1bp": all(
            row["bottom_decile_mean_return_advantage_bps"] >= -1.0
            for row in route_rows
        ),
        "both_routes_tail_service_regressions_within_limits": service_safe(
            route_rows
        ),
        "every_flow_route_median_has_no_return_reversal": all(
            row["median_return_advantage_bps"] >= 0.0 for row in flow_rows
        ),
        "every_flow_route_bottom_decile_not_below_minus_1bp": all(
            row["bottom_decile_mean_return_advantage_bps"] >= -1.0
            for row in flow_rows
        ),
        "every_flow_route_tail_service_within_limits": service_safe(flow_rows),
    }


def _candidate_core_confirmation(
    paired_rows: Sequence[Mapping[str, Any]], system: str
) -> dict[str, Any]:
    if system == "dusk_native":
        cross_comparison, cross_sign = "dusk_minus_curve", 1.0
        cpmm_comparison = "dusk_minus_cpmm"
    elif system == "curve_native_v2_1":
        cross_comparison, cross_sign = "dusk_minus_curve", -1.0
        cpmm_comparison = "curve_minus_cpmm"
    else:
        raise ValueError(f"unsupported confirmation candidate: {system}")

    def metrics(comparison: str, sign: float) -> tuple[dict[str, Any], dict[str, Any]]:
        by_route = {
            route: _group_metrics(
                paired_rows, comparison=comparison, sign=sign, route=route
            )
            for route in SEALED_ROUTES
        }
        by_flow_route = {
            f"{flow}:{route}": _group_metrics(
                paired_rows,
                comparison=comparison,
                sign=sign,
                flow=flow,
                route=route,
            )
            for flow in EXPECTED_FLOWS
            for route in SEALED_ROUTES
        }
        return by_route, by_flow_route

    cross_route, cross_flow = metrics(cross_comparison, cross_sign)
    cpmm_route, cpmm_flow = metrics(cpmm_comparison, 1.0)
    cross_predicates = _core_predicates(cross_route, cross_flow)
    cpmm_predicates = {
        f"beats_cpmm_{name}": value
        for name, value in _core_predicates(cpmm_route, cpmm_flow).items()
    }
    predicates = {**cross_predicates, **cpmm_predicates}
    return {
        "passes": all(predicates.values()),
        "predicates": predicates,
        "vs_other_by_route": cross_route,
        "vs_cpmm_by_route": cpmm_route,
    }


def _sealed_confirmation(
    native_summary: Mapping[str, Any],
    paired_rows: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    candidates = {
        system: _candidate_core_confirmation(paired_rows, system)
        for system in ("curve_native_v2_1", "dusk_native")
    }
    passing = [system for system, result in candidates.items() if result["passes"]]
    sealed_classification = passing[0] if len(passing) == 1 else None
    development_value = native_summary.get("decision", {}).get(
        "fee_adjusted_universal_native_winner"
    )
    development_classification = {
        "curve_twocrypto_v2_1": "curve_native_v2_1",
        "dusk_current_dh2": "dusk_native",
        None: None,
    }.get(development_value, "invalid")
    if development_classification == "invalid":
        raise ValueError("canonical development winner has an unknown value")
    confirmed = sealed_classification == development_classification
    return {
        "status": "pass" if confirmed else "fail",
        "confirmed": confirmed,
        "development_classification": development_classification,
        "sealed_core_classification": sealed_classification,
        "candidates": candidates,
        "rule": (
            "apply the pre-existing native route, flow-route, and CPMM return/"
            "service thresholds to the locked profiles; pass only when the sole "
            "sealed core winner, or neither, equals the development universal "
            "winner classification"
        ),
        "scope_limit": (
            "confirmatory locked-profile core only; the sealed matrix does not "
            "rerun neighborhood, scale, cadence, or matching-basis arms and "
            "cannot select, replace, or retune a profile"
        ),
    }


def _verify_summary_diagnostics(
    summary: Mapping[str, Any],
    result_rows: Sequence[Mapping[str, Any]],
    paired_rows: Sequence[Mapping[str, Any]],
) -> None:
    by_system = summary.get("locked_system_diagnostics")
    if not isinstance(by_system, dict) or set(by_system) != set(LOCKED_SYSTEMS):
        raise ValueError("sealed system diagnostics are incomplete")
    for system in LOCKED_SYSTEMS:
        rows = [row for row in result_rows if row["system"] == system]
        returns = [float(row["net_lp_return_after_protocol_fee_bps"]) for row in rows]
        costs = [float(row["selected_execution_cost_bps"]) for row in rows]
        captures = [float(row["captured_user_volume_fraction"]) for row in rows]
        stale = [float(row["marginal_tracking_stale_fraction"]) for row in rows]
        depth = [float(row["depth_degradation_fraction"]) for row in rows]
        expected = {
            "rows": len(rows),
            "median_net_lp_return_after_protocol_fee_bps": statistics.median(returns),
            "p10_net_lp_return_after_protocol_fee_bps": _percentile(returns, 0.10),
            "median_selected_execution_cost_bps": statistics.median(costs),
            "p90_selected_execution_cost_bps": _percentile(costs, 0.90),
            "median_captured_volume_fraction": statistics.median(captures),
            "median_marginal_tracking_stale_fraction": statistics.median(stale),
            "p90_marginal_tracking_stale_fraction": _percentile(stale, 0.90),
            "median_depth_degradation_fraction": statistics.median(depth),
        }
        actual = by_system[system]
        for field, value in expected.items():
            if field == "rows":
                if int(actual.get(field, -1)) != value:
                    raise ValueError(f"sealed {system} diagnostic row count mismatch")
            else:
                _close(actual.get(field), value, label=f"sealed {system} {field}")

    comparisons = summary.get("paired_diagnostics")
    if not isinstance(comparisons, dict) or set(comparisons) != set(PAIR_SPECS):
        raise ValueError("sealed paired diagnostics are incomplete")
    for comparison in PAIR_SPECS:
        rows = [row for row in paired_rows if row["comparison"] == comparison]
        returns = [
            float(row["left_minus_right_net_lp_return_after_protocol_fee_bps"])
            for row in rows
        ]
        execution = [
            float(row["left_minus_right_selected_execution_cost_bps"])
            for row in rows
        ]
        stale = [
            float(row["left_minus_right_marginal_tracking_stale_fraction"])
            for row in rows
        ]
        expected = {
            "rows": len(rows),
            "median_net_lp_return_delta_bps": statistics.median(returns),
            "p10_net_lp_return_delta_bps": _percentile(returns, 0.10),
            "left_win_fraction": sum(value > 0.0 for value in returns) / len(returns),
            "median_execution_cost_delta_bps": statistics.median(execution),
            "p90_marginal_tracking_stale_delta_fraction": _percentile(stale, 0.90),
        }
        actual = comparisons[comparison]
        for field, value in expected.items():
            if field == "rows":
                if int(actual.get(field, -1)) != value:
                    raise ValueError(f"sealed {comparison} diagnostic row count mismatch")
            else:
                _close(actual.get(field), value, label=f"sealed {comparison} {field}")


def _expected_sealed_matrix() -> dict[str, Any]:
    """Return the report-side contract for the canonical sealed replay matrix."""

    return {
        "months": list(SEALED_MONTHS),
        "strata": list(SEALED_STRATA),
        "flows": list(EXPECTED_FLOWS),
        "seeds": list(PRIMARY_SEEDS),
        "routes": list(SEALED_ROUTES),
        "windows": EXPECTED_SEALED_WINDOWS,
        "base_cells": EXPECTED_SEALED_BASE_CELLS,
        "result_rows": EXPECTED_SEALED_RESULT_ROWS,
        "paired_rows": EXPECTED_SEALED_PAIRED_ROWS,
    }


def validate_sealed_evidence(
    native_summary: Mapping[str, Any],
    *,
    root: Path = ROOT,
    verified_lock: Mapping[str, Any] | None = None,
    verified_input_hashes: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    """Validate sealed inputs, outputs, lock identity, and diagnostics."""

    lock_path = root / "data" / LOCK_PATH.name
    history_path = root / "data" / SEALED_CSV_PATH.name
    metadata_path = root / "data" / SEALED_METADATA_PATH.name
    manifest_path = root / "data" / SEALED_MANIFEST_PATH.name
    output_dir = root / "results" / "sealed_validation"
    results_path = output_dir / "sealed_native_results.csv"
    paired_path = output_dir / "sealed_native_paired.csv"
    summary_path = output_dir / "sealed_native_summary.json"

    lock = (
        dict(verified_lock)
        if verified_lock is not None
        else verify_profile_lock(lock_path, verify_environment=True)
    )
    input_hashes = (
        dict(verified_input_hashes)
        if verified_input_hashes is not None
        else verify_postlock_hashes(
            lock_path=lock_path,
            csv_path=history_path,
            metadata_path=metadata_path,
            manifest_path=manifest_path,
        )
    )
    sealed_summary = _read_json(summary_path, label="sealed validation summary")
    result_rows = _read_csv(results_path, label="sealed validation results")
    paired_rows = _read_csv(paired_path, label="sealed paired results")

    if sealed_summary.get("status") != "complete":
        raise ValueError("sealed validation is not complete")
    if sealed_summary.get("selection_performed") is not False or sealed_summary.get(
        "feeds_profile_selection"
    ) is not False:
        raise ValueError("sealed validation contaminated profile selection")
    if sealed_summary.get("winner_or_recommendation_emitted") is not False:
        raise ValueError("sealed replay improperly emitted a winner or recommendation")

    canonical_selection = native_summary.get("profile_selection")
    locked_profiles = lock.get("locked_profiles")
    if not isinstance(canonical_selection, dict) or not isinstance(
        locked_profiles, dict
    ):
        raise ValueError("canonical or sealed profile lock is incomplete")
    for system in LOCKED_SYSTEMS:
        if str(canonical_selection[system]["profile_id"]) != str(
            locked_profiles[system]["profile_id"]
        ):
            raise ValueError(f"sealed {system} profile differs from canonical selection")
        if str(canonical_selection[system]["description"]) != str(
            locked_profiles[system]["description"]
        ):
            raise ValueError(f"sealed {system} description differs from canonical selection")

    lock_summary = sealed_summary.get("profile_lock")
    if not isinstance(lock_summary, dict) or lock_summary.get("fingerprint") != lock.get(
        "profile_lock_fingerprint"
    ):
        raise ValueError("sealed summary profile-lock fingerprint mismatch")
    if lock_summary.get("locked_at_utc") != lock.get("locked_at_utc"):
        raise ValueError("sealed summary profile-lock timestamp mismatch")
    reported_profiles = lock_summary.get("profiles")
    if not isinstance(reported_profiles, dict) or set(reported_profiles) != set(
        LOCKED_SYSTEMS
    ):
        raise ValueError("sealed summary profile inventory is incomplete")
    for system in LOCKED_SYSTEMS:
        if reported_profiles[system] != {
            "profile_id": locked_profiles[system]["profile_id"],
            "description": locked_profiles[system]["description"],
        }:
            raise ValueError(f"sealed summary profile identity mismatch: {system}")

    if sealed_summary.get("input_hashes") != input_hashes:
        raise ValueError("sealed summary input hashes are stale")
    output_hashes = sealed_summary.get("output_hashes")
    if not isinstance(output_hashes, dict):
        raise ValueError("sealed summary output hashes are missing")
    _require_hash(
        results_path,
        output_hashes.get("sealed_native_results_sha256"),
        label="sealed result hash",
    )
    _require_hash(
        paired_path,
        output_hashes.get("sealed_native_paired_sha256"),
        label="sealed paired hash",
    )

    matrix = sealed_summary.get("matrix")
    expected_matrix = _expected_sealed_matrix()
    if matrix != expected_matrix:
        raise ValueError(f"sealed matrix mismatch: {matrix!r}")
    if len(result_rows) != EXPECTED_SEALED_RESULT_ROWS:
        raise ValueError("sealed result row count is incomplete")
    if len(paired_rows) != EXPECTED_SEALED_PAIRED_ROWS:
        raise ValueError("sealed paired row count is incomplete")
    if len({_result_key(row) for row in result_rows}) != len(result_rows):
        raise ValueError("sealed result output contains duplicate cells")
    if {row["analysis_slice"] for row in result_rows} != {
        "sealed_post_lock_validation"
    }:
        raise ValueError("sealed result output escaped its isolated slice")
    expected_ids = {
        system: str(locked_profiles[system]["profile_id"])
        for system in LOCKED_SYSTEMS
    }
    if {
        (row["system"], row["profile_id"]) for row in result_rows
    } != set(expected_ids.items()):
        raise ValueError("sealed result profiles differ from the immutable lock")
    if not all(
        field in row and math.isfinite(float(row[field]))
        for row in result_rows
        for field in SEALED_DECISION_FIELDS
    ):
        raise ValueError("sealed result metrics are missing or non-finite")
    if not all(
        field in row and math.isfinite(float(row[field]))
        for row in paired_rows
        for field in SEALED_PAIR_FIELDS
    ):
        raise ValueError("sealed paired metrics are missing or non-finite")
    _verify_paired_rows(result_rows, paired_rows)
    _verify_summary_diagnostics(sealed_summary, result_rows, paired_rows)

    maximum_error = max(
        abs(float(row["accounting_identity_error"])) for row in result_rows
    )
    external_reads = sum(
        int(row["controller_external_price_reads"]) for row in result_rows
    )
    capped = sum(int(row["arbitrage_capped_event_count"]) for row in result_rows)
    numerical = sum(
        int(row["arbitrage_numerical_failure_count"]) for row in result_rows
    )
    validation = sealed_summary.get("validation")
    if not isinstance(validation, dict):
        raise ValueError("sealed summary validation block is missing")
    expected_validation = {
        "expected_result_rows": EXPECTED_SEALED_RESULT_ROWS,
        "expected_paired_rows": EXPECTED_SEALED_PAIRED_ROWS,
        "all_decision_metrics_finite": True,
        "controller_external_price_reads": 0,
        "arbitrage_capped_event_count": 0,
        "arbitrage_numerical_failure_count": 0,
        "input_hashes_verified": True,
        "profile_lock_verified": True,
        "selection_isolation_verified": True,
    }
    for field, expected in expected_validation.items():
        if validation.get(field) != expected:
            raise ValueError(f"sealed validation field mismatch: {field}")
    _close(
        validation.get("maximum_accounting_identity_error"),
        maximum_error,
        label="sealed maximum accounting identity error",
    )
    if maximum_error > ACCOUNTING_ERROR_LIMIT:
        raise ValueError("sealed accounting identity error exceeds 1e-5")
    if external_reads != 0 or capped != 0 or numerical != 0:
        raise ValueError("sealed controller isolation or arbitrage convergence failed")

    route_outcomes = []
    development = native_summary.get("held_out")
    if not isinstance(development, dict):
        raise ValueError("canonical development-history route summary is missing")
    for route in SEALED_ROUTES:
        sealed_metrics = _group_metrics(
            paired_rows,
            comparison="dusk_minus_curve",
            sign=1.0,
            route=route,
        )
        development_metrics = development.get(route)
        if not isinstance(development_metrics, dict):
            raise ValueError(f"canonical development route is missing: {route}")
        route_outcomes.append(
            {
                "routing_mode": route,
                "development_median_dusk_minus_curve_lp_return_bps": float(
                    development_metrics[
                        "median_dusk_minus_curve_return_after_protocol_fee_bps"
                    ]
                ),
                "sealed_median_dusk_minus_curve_lp_return_bps": sealed_metrics[
                    "median_return_advantage_bps"
                ],
                "development_cell_win_fraction": float(
                    development_metrics["cell_win_fraction"]
                ),
                "sealed_cell_win_fraction": sealed_metrics["cell_win_fraction"],
                "development_window_win_fraction": float(
                    development_metrics["window_median_win_fraction"]
                ),
                "sealed_window_win_fraction": sealed_metrics[
                    "window_median_win_fraction"
                ],
                "sealed_bottom_decile_mean_return_bps": sealed_metrics[
                    "bottom_decile_mean_return_advantage_bps"
                ],
                "sealed_p90_execution_regression_bps": sealed_metrics[
                    "p90_execution_regression_bps"
                ],
                "sealed_p90_marginal_tracking_stale_regression_fraction": (
                    sealed_metrics[
                        "p90_marginal_tracking_stale_regression_fraction"
                    ]
                ),
            }
        )

    return {
        "status": "passed",
        "profile_lock_fingerprint": lock["profile_lock_fingerprint"],
        "locked_at_utc": lock["locked_at_utc"],
        "input_hashes": input_hashes,
        "output_hashes": {
            "sealed_native_summary_sha256": sha256_path(summary_path),
            "sealed_native_results_sha256": sha256_path(results_path),
            "sealed_native_paired_sha256": sha256_path(paired_path),
        },
        "matrix": expected_matrix,
        "route_outcomes": route_outcomes,
        "confirmation": _sealed_confirmation(native_summary, paired_rows),
        "locked_system_diagnostics": sealed_summary["locked_system_diagnostics"],
        "paired_diagnostics": sealed_summary["paired_diagnostics"],
        "interpretation_limit": sealed_summary.get("interpretation_limit"),
    }


def collect_final_report_evidence(
    native_summary: Mapping[str, Any],
    *,
    root: Path = ROOT,
    bindings: Mapping[str, Mapping[str, Any]] | None = None,
    verified_lock: Mapping[str, Any] | None = None,
    verified_input_hashes: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    search_promotions = validate_search_and_promotions(
        native_summary, root=root, bindings=bindings
    )
    lock = (
        dict(verified_lock)
        if verified_lock is not None
        else verify_profile_lock(
            root / "data" / LOCK_PATH.name,
            verify_environment=True,
        )
    )
    _validate_promotions_match_lock(search_promotions, lock)
    sealed = validate_sealed_evidence(
        native_summary,
        root=root,
        verified_lock=lock,
        verified_input_hashes=verified_input_hashes,
    )
    return {
        "schema_version": FINAL_REPORT_EVIDENCE_SCHEMA_VERSION,
        "status": "passed",
        "search_and_promotion": search_promotions,
        "sealed_validation": sealed,
    }


def audit_receipt(
    evidence: Mapping[str, Any],
    *,
    root: Path = ROOT,
) -> dict[str, Any]:
    native_summary_path = root / "results" / "native_summary.json"
    receipt: dict[str, Any] = {
        "schema_version": FINAL_REPORT_EVIDENCE_SCHEMA_VERSION,
        "status": "passed",
        "scope": "final_report_evidence",
        "native_summary_sha256": sha256_path(native_summary_path),
        "evidence": dict(evidence),
    }
    receipt["receipt_fingerprint"] = canonical_fingerprint(receipt)
    return receipt


def write_final_report_audit(
    native_summary: Mapping[str, Any],
    *,
    root: Path = ROOT,
) -> dict[str, Any]:
    evidence = collect_final_report_evidence(native_summary, root=root)
    receipt = audit_receipt(evidence, root=root)
    path = root / "results" / FINAL_REPORT_AUDIT_PATH.name
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w", dir=path.parent, prefix=".final-report-audit-", delete=False
    ) as target:
        target.write(json.dumps(receipt, indent=2, allow_nan=False) + "\n")
        temporary = Path(target.name)
    temporary.replace(path)
    return receipt


def require_final_report_audit(
    native_summary: Mapping[str, Any],
    *,
    root: Path = ROOT,
) -> dict[str, Any]:
    """Revalidate evidence and require a matching read-only audit receipt."""

    evidence = collect_final_report_evidence(native_summary, root=root)
    expected = audit_receipt(evidence, root=root)
    path = root / "results" / FINAL_REPORT_AUDIT_PATH.name
    actual = _read_json(path, label="final report audit receipt")
    if actual != expected:
        raise ValueError(
            "final report audit receipt is absent, stale, or inconsistent; run "
            "audit_native_outputs.py --final-report after sealed validation"
        )
    return evidence

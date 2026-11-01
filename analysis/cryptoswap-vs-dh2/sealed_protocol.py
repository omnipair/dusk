"""Fail-closed protocol bindings for post-lock native validation.

This module intentionally contains no price loading or benchmark selection.
It binds a pre-history profile lock to a mechanically declared future dataset
and to the exact replay engine that will later score that dataset.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any, Mapping


ROOT = Path(__file__).resolve().parent
DATA = ROOT / "data"
RESULTS = ROOT / "results"

LOCK_SCHEMA_VERSION = 3
SEALED_HISTORY_SCHEMA_VERSION = 1
SEALED_OUTPUT_SCHEMA_VERSION = 1

LOCK_PATH = DATA / "sealed_profile_lock.json"
SEALED_CSV_PATH = DATA / "solusdt_sealed_native_windows.csv"
SEALED_METADATA_PATH = DATA / "sealed_native_history_metadata.json"
SEALED_MANIFEST_PATH = DATA / "sealed_native_history_manifest.json"
SEALED_OUTPUT_DIR = RESULTS / "sealed_validation"

LOCKED_SYSTEMS = ("curve_native_v2_1", "dusk_native", "dusk_cpmm")
SEALED_STRATA = ("calm", "typical", "stress")
SEALED_ROUTES = ("fixed", "elastic")

SEARCH_SOURCE_PATHS = {
    "dusk_controller_search": "results/controller_search/full/summary.json",
    "curve_controller_search": (
        "results/curve_controller_search/full/summary.json"
    ),
    "cpmm_controller_search": (
        "results/cpmm_controller_search/full/summary.json"
    ),
}
EXPECTED_SEARCH_PROFILES = 150
EXPECTED_SEARCH_CELLS_PER_PROFILE = 128
EXPECTED_SEARCH_ROWS = (
    EXPECTED_SEARCH_PROFILES * EXPECTED_SEARCH_CELLS_PER_PROFILE
)

FIRST_SEALED_YEAR = 2025
FIRST_SEALED_MONTH = 5
SEALED_MONTH_STRIDE = 3
SEALED_MONTH_COUNT = 5

# Hash every executable dependency that can change sealed replay outcomes.
# Documentation and report generation are deliberately outside this boundary.
SEALED_ENGINE_FILES = (
    "native_benchmark.py",
    "native_replay.py",
    "native_flow.py",
    "economics.py",
    "dusk_policy.py",
    "curve_native.py",
    "models.py",
    "comparison.py",
    "scenarios.py",
    "sealed_protocol.py",
    "fetch_sealed_native_history.py",
    "native_sealed_validation.py",
    "cpmm_controller_search.py",
    "promote_controller_profiles.py",
)


def sealed_months() -> tuple[str, ...]:
    """Return five calendar months fixed mechanically at three-month steps."""

    months: list[str] = []
    zero_based = FIRST_SEALED_MONTH - 1
    for index in range(SEALED_MONTH_COUNT):
        absolute = zero_based + index * SEALED_MONTH_STRIDE
        year = FIRST_SEALED_YEAR + absolute // 12
        month = absolute % 12 + 1
        months.append(f"{year:04d}-{month:02d}")
    return tuple(months)


SEALED_MONTHS = sealed_months()


def sha256_path(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_fingerprint(payload: Mapping[str, Any]) -> str:
    encoded = json.dumps(
        payload,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode()
    return hashlib.sha256(encoded).hexdigest()


def profile_lock_fingerprint(payload: Mapping[str, Any]) -> str:
    unsigned = dict(payload)
    unsigned.pop("profile_lock_fingerprint", None)
    return canonical_fingerprint(unsigned)


def engine_hashes() -> dict[str, str]:
    return {
        filename: sha256_path(ROOT / filename)
        for filename in SEALED_ENGINE_FILES
    }


def _project_path(relative: object, *, expected: str) -> Path:
    if str(relative) != expected:
        raise ValueError(f"sealed path must be {expected}")
    candidate = (ROOT / str(relative)).resolve()
    try:
        candidate.relative_to(ROOT.resolve())
    except ValueError as error:
        raise ValueError("sealed path escapes benchmark root") from error
    return candidate


def _exact_int(value: object, *, expected: int, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value != expected:
        raise ValueError(f"{label} must be exactly {expected}")
    return value


def _positive_int(value: object, *, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def _sha256(value: object, *, label: str) -> str:
    digest = str(value)
    if len(digest) != 64 or any(
        character not in "0123456789abcdef" for character in digest
    ):
        raise ValueError(f"{label} must be a lowercase SHA-256 digest")
    return digest


def _winner_binding(
    winner: object,
    *,
    config_field: str,
    label: str,
) -> dict[str, object]:
    if not isinstance(winner, dict):
        raise ValueError(f"{label} winner must be an object")
    profile_id = str(winner.get("profile_id", ""))
    description = str(winner.get("description", ""))
    if not profile_id or not description:
        raise ValueError(f"{label} winner identity is incomplete")
    if winner.get("hard_valid") is not True:
        raise ValueError(f"{label} winner must pass hard validity")
    full_config = winner.get(config_field)
    if not isinstance(full_config, dict) or not full_config:
        raise ValueError(f"{label} winner full config is missing")
    if config_field == "full_config" and set(full_config) != {
        "model",
        "controller",
    }:
        raise ValueError(
            f"{label} winner full config must contain model and controller"
        )
    if config_field == "full_config" and any(
        not isinstance(full_config[field], dict) or not full_config[field]
        for field in ("model", "controller")
    ):
        raise ValueError(f"{label} winner model/controller config is empty")
    return {
        "profile_id": profile_id,
        "description": description,
        "full_config": full_config,
    }


def _validated_dusk_search_summary(
    summary: object,
    *,
    path: Path,
) -> dict[str, object]:
    label = "Dusk controller search"
    if not isinstance(summary, dict):
        raise ValueError(f"{label} summary must be an object")
    if summary.get("status") != "controller_search_complete":
        raise ValueError(f"{label} is not complete")
    if summary.get("mode") != "full":
        raise ValueError(f"{label} must use mode=full")
    if summary.get("decision_grade_search") is not True:
        raise ValueError(f"{label} must be decision-grade")
    if (
        summary.get("complete_matrix_evaluated_for_every_initial_profile")
        is not True
    ):
        raise ValueError(f"{label} did not complete every initial-profile cell")
    _exact_int(
        summary.get("initial_profiles"),
        expected=EXPECTED_SEARCH_PROFILES,
        label=f"{label} initial profile count",
    )

    stages = summary.get("stage_counts")
    if not isinstance(stages, dict) or set(stages) != {"grid", "local"}:
        raise ValueError(f"{label} must contain exactly grid and local stages")
    grid = stages["grid"]
    local = stages["local"]
    if not isinstance(grid, dict) or not isinstance(local, dict):
        raise ValueError(f"{label} stage counts must be objects")
    _exact_int(
        grid.get("profiles"),
        expected=EXPECTED_SEARCH_PROFILES,
        label=f"{label} grid profile count",
    )
    _exact_int(
        grid.get("cells_per_profile"),
        expected=EXPECTED_SEARCH_CELLS_PER_PROFILE,
        label=f"{label} grid cells per profile",
    )
    _exact_int(
        grid.get("rows"),
        expected=EXPECTED_SEARCH_ROWS,
        label=f"{label} grid row count",
    )
    _exact_int(
        grid.get("selected"), expected=1, label=f"{label} grid selection"
    )
    if grid.get("exhaustive") is not True:
        raise ValueError(f"{label} grid must be exhaustive")

    local_profiles = _positive_int(
        local.get("profiles"), label=f"{label} local profile count"
    )
    _exact_int(
        local.get("cells_per_profile"),
        expected=EXPECTED_SEARCH_CELLS_PER_PROFILE,
        label=f"{label} local cells per profile",
    )
    _exact_int(
        local.get("rows"),
        expected=local_profiles * EXPECTED_SEARCH_CELLS_PER_PROFILE,
        label=f"{label} local row count",
    )
    _exact_int(
        local.get("selected"), expected=1, label=f"{label} local selection"
    )
    if local.get("exhaustive") is not True:
        raise ValueError(f"{label} local matrix must be exhaustive")

    fingerprints = summary.get("fingerprints")
    if not isinstance(fingerprints, dict) or set(fingerprints) != {
        "grid",
        "local",
    }:
        raise ValueError(f"{label} fingerprints must bind grid and local")
    normalized_fingerprints = {
        stage: _sha256(
            fingerprints[stage], label=f"{label} {stage} fingerprint"
        )
        for stage in ("grid", "local")
    }
    return {
        "path": SEARCH_SOURCE_PATHS["dusk_controller_search"],
        "sha256": sha256_path(path),
        "status": "controller_search_complete",
        "mode": "full",
        "decision_grade_search": True,
        "complete_matrix": True,
        "matrix": {
            "profiles": EXPECTED_SEARCH_PROFILES,
            "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
            "rows": EXPECTED_SEARCH_ROWS,
        },
        "local_matrix": {
            "profiles": local_profiles,
            "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
            "rows": local_profiles * EXPECTED_SEARCH_CELLS_PER_PROFILE,
        },
        "search_fingerprint": normalized_fingerprints,
        "winner": _winner_binding(
            summary.get("winner"), config_field="config", label=label
        ),
    }


def _validated_curve_search_summary(
    summary: object,
    *,
    path: Path,
) -> dict[str, object]:
    label = "Curve controller search"
    if not isinstance(summary, dict):
        raise ValueError(f"{label} summary must be an object")
    if summary.get("status") != "curve_controller_search_complete":
        raise ValueError(f"{label} is not complete")
    if summary.get("mode") != "full":
        raise ValueError(f"{label} must use mode=full")
    if summary.get("decision_grade_search") is not True:
        raise ValueError(f"{label} must be decision-grade")
    if summary.get("complete_matrix_evaluated_for_every_profile") is not True:
        raise ValueError(f"{label} did not complete every profile cell")
    _exact_int(
        summary.get("initial_profiles"),
        expected=EXPECTED_SEARCH_PROFILES,
        label=f"{label} initial profile count",
    )
    _exact_int(
        summary.get("exact_profile_budget"),
        expected=EXPECTED_SEARCH_PROFILES,
        label=f"{label} profile budget",
    )
    _exact_int(
        summary.get("cells_per_profile"),
        expected=EXPECTED_SEARCH_CELLS_PER_PROFILE,
        label=f"{label} cells per profile",
    )
    _exact_int(
        summary.get("rows"),
        expected=EXPECTED_SEARCH_ROWS,
        label=f"{label} row count",
    )
    fingerprint = _sha256(
        summary.get("fingerprint"), label=f"{label} search fingerprint"
    )
    return {
        "path": SEARCH_SOURCE_PATHS["curve_controller_search"],
        "sha256": sha256_path(path),
        "status": "curve_controller_search_complete",
        "mode": "full",
        "decision_grade_search": True,
        "complete_matrix": True,
        "matrix": {
            "profiles": EXPECTED_SEARCH_PROFILES,
            "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
            "rows": EXPECTED_SEARCH_ROWS,
        },
        "search_fingerprint": fingerprint,
        "winner": _winner_binding(
            summary.get("winner"), config_field="full_config", label=label
        ),
    }


def _validated_cpmm_search_summary(
    summary: object,
    *,
    path: Path,
) -> dict[str, object]:
    label = "CPMM controller search"
    if not isinstance(summary, dict):
        raise ValueError(f"{label} summary must be an object")
    if summary.get("status") != "cpmm_controller_search_complete":
        raise ValueError(f"{label} is not complete")
    if summary.get("mode") != "full":
        raise ValueError(f"{label} must use mode=full")
    if summary.get("decision_grade_search") is not True:
        raise ValueError(f"{label} must be decision-grade")
    if summary.get("complete_matrix_evaluated_for_every_profile") is not True:
        raise ValueError(f"{label} did not complete every profile cell")
    for field, expected, suffix in (
        ("initial_profiles", EXPECTED_SEARCH_PROFILES, "profile count"),
        ("exact_profile_budget", EXPECTED_SEARCH_PROFILES, "profile budget"),
        (
            "cells_per_profile",
            EXPECTED_SEARCH_CELLS_PER_PROFILE,
            "cells per profile",
        ),
        ("rows", EXPECTED_SEARCH_ROWS, "row count"),
    ):
        _exact_int(summary.get(field), expected=expected, label=f"{label} {suffix}")
    fingerprint = _sha256(
        summary.get("fingerprint"), label=f"{label} search fingerprint"
    )
    winner = _winner_binding(
        summary.get("winner"), config_field="full_config", label=label
    )
    model = winner["full_config"].get("model")
    if not isinstance(model, dict) or (
        float(model.get("peak_depth", -1.0)) != 0.0
        or float(model.get("fade_scale", -1.0)) != 0.0
    ):
        raise ValueError("CPMM search winner is not exact zero concentration")
    return {
        "path": SEARCH_SOURCE_PATHS["cpmm_controller_search"],
        "sha256": sha256_path(path),
        "status": "cpmm_controller_search_complete",
        "mode": "full",
        "decision_grade_search": True,
        "complete_matrix": True,
        "matrix": {
            "profiles": EXPECTED_SEARCH_PROFILES,
            "cells_per_profile": EXPECTED_SEARCH_CELLS_PER_PROFILE,
            "rows": EXPECTED_SEARCH_ROWS,
        },
        "search_fingerprint": fingerprint,
        "winner": winner,
    }


def search_source_bindings(
    *,
    paths: Mapping[str, Path] | None = None,
) -> dict[str, dict[str, object]]:
    """Validate and bind all completed decision-grade search summaries."""

    supplied = dict(paths or {})
    if supplied and set(supplied) != set(SEARCH_SOURCE_PATHS):
        raise ValueError("search-source paths must contain exactly Curve, DH2, and CPMM")
    output: dict[str, dict[str, object]] = {}
    validators = {
        "dusk_controller_search": _validated_dusk_search_summary,
        "curve_controller_search": _validated_curve_search_summary,
        "cpmm_controller_search": _validated_cpmm_search_summary,
    }
    for name, relative in SEARCH_SOURCE_PATHS.items():
        path = supplied.get(name, ROOT / relative)
        try:
            actual_relative = path.resolve().relative_to(ROOT.resolve())
        except ValueError as error:
            raise ValueError(f"{name} summary must live under benchmark root") from error
        if str(actual_relative) != relative:
            raise ValueError(f"{name} summary path must be {relative}")
        if not path.exists():
            raise FileNotFoundError(f"missing decision-grade search summary: {path}")
        try:
            summary = json.loads(path.read_text())
        except json.JSONDecodeError as error:
            raise ValueError(f"invalid search summary JSON: {path}") from error
        output[name] = validators[name](summary, path=path)
    return output


def _validate_bound_search_sources(search_sources: object) -> None:
    if not isinstance(search_sources, dict) or set(search_sources) != set(
        SEARCH_SOURCE_PATHS
    ):
        raise ValueError("sealed lock must bind exactly the Curve, DH2, and CPMM searches")
    for name, relative in SEARCH_SOURCE_PATHS.items():
        source = search_sources[name]
        if not isinstance(source, dict):
            raise ValueError(f"invalid sealed search binding: {name}")
        if source.get("path") != relative:
            raise ValueError(f"sealed search path mismatch: {name}")
        _sha256(source.get("sha256"), label=f"{name} source hash")
        expected_status = {
            "dusk_controller_search": "controller_search_complete",
            "curve_controller_search": "curve_controller_search_complete",
            "cpmm_controller_search": "cpmm_controller_search_complete",
        }[name]
        if source.get("status") != expected_status:
            raise ValueError(f"sealed search is not complete: {name}")
        if source.get("mode") != "full":
            raise ValueError(f"sealed search is not full: {name}")
        if source.get("decision_grade_search") is not True:
            raise ValueError(f"sealed search is not decision-grade: {name}")
        if source.get("complete_matrix") is not True:
            raise ValueError(f"sealed search matrix is incomplete: {name}")
        matrix = source.get("matrix")
        if not isinstance(matrix, dict):
            raise ValueError(f"sealed search matrix binding is missing: {name}")
        _exact_int(
            matrix.get("profiles"),
            expected=EXPECTED_SEARCH_PROFILES,
            label=f"{name} bound profile count",
        )
        _exact_int(
            matrix.get("cells_per_profile"),
            expected=EXPECTED_SEARCH_CELLS_PER_PROFILE,
            label=f"{name} bound cells per profile",
        )
        _exact_int(
            matrix.get("rows"),
            expected=EXPECTED_SEARCH_ROWS,
            label=f"{name} bound row count",
        )
        winner = source.get("winner")
        if not isinstance(winner, dict):
            raise ValueError(f"sealed search winner is missing: {name}")
        if not str(winner.get("profile_id", "")) or not str(
            winner.get("description", "")
        ):
            raise ValueError(f"sealed search winner identity is incomplete: {name}")
        if not isinstance(winner.get("full_config"), dict) or not winner.get(
            "full_config"
        ):
            raise ValueError(f"sealed search winner config is missing: {name}")
        fingerprint = source.get("search_fingerprint")
        if name == "dusk_controller_search":
            if not isinstance(fingerprint, dict) or set(fingerprint) != {
                "grid",
                "local",
            }:
                raise ValueError("sealed Dusk search fingerprints are incomplete")
            for stage in ("grid", "local"):
                _sha256(fingerprint[stage], label=f"bound Dusk {stage} fingerprint")
            local = source.get("local_matrix")
            if not isinstance(local, dict):
                raise ValueError("sealed Dusk local matrix binding is missing")
            local_profiles = _positive_int(
                local.get("profiles"), label="bound Dusk local profile count"
            )
            _exact_int(
                local.get("cells_per_profile"),
                expected=EXPECTED_SEARCH_CELLS_PER_PROFILE,
                label="bound Dusk local cells per profile",
            )
            _exact_int(
                local.get("rows"),
                expected=local_profiles * EXPECTED_SEARCH_CELLS_PER_PROFILE,
                label="bound Dusk local row count",
            )
        else:
            _sha256(fingerprint, label=f"bound {name} search fingerprint")
            full_config = winner["full_config"]
            if set(full_config) != {"model", "controller"} or any(
                not isinstance(full_config[field], dict) or not full_config[field]
                for field in ("model", "controller")
            ):
                raise ValueError(
                    f"sealed {name} winner config must contain model and controller"
                )
            if name == "cpmm_controller_search":
                model = full_config["model"]
                if (
                    float(model.get("peak_depth", -1.0)) != 0.0
                    or float(model.get("fade_scale", -1.0)) != 0.0
                ):
                    raise ValueError("sealed CPMM winner is not exact zero concentration")


def verify_profile_lock(
    path: Path = LOCK_PATH,
    *,
    verify_environment: bool = True,
) -> dict[str, Any]:
    """Load and verify the immutable pre-history profile lock.

    Environment verification binds the lock to the canonical synthetic
    selection output and exact replay-engine files.  Consumers fail closed on
    any mismatch; there is no best-effort or automatic re-selection mode.
    """

    if not path.exists():
        raise FileNotFoundError(
            f"missing pre-history profile lock: {path}; materialize it before "
            "downloading sealed history"
        )
    payload = json.loads(path.read_text())
    if not isinstance(payload, dict):
        raise TypeError("sealed profile lock must be a JSON object")
    if payload.get("schema_version") != LOCK_SCHEMA_VERSION:
        raise ValueError("unsupported sealed profile-lock schema")
    expected_fingerprint = profile_lock_fingerprint(payload)
    if payload.get("profile_lock_fingerprint") != expected_fingerprint:
        raise ValueError("sealed profile-lock fingerprint mismatch")

    profiles = payload.get("locked_profiles")
    if not isinstance(profiles, dict) or set(profiles) != set(LOCKED_SYSTEMS):
        raise ValueError("sealed lock must contain exactly Curve, DH2, and CPMM")
    for system in LOCKED_SYSTEMS:
        row = profiles[system]
        if not isinstance(row, dict) or row.get("system") != system:
            raise ValueError(f"invalid locked profile for {system}")
        if not str(row.get("profile_id", "")) or not str(
            row.get("description", "")
        ):
            raise ValueError(f"missing locked profile identity for {system}")
        if not isinstance(row.get("model"), dict) or not isinstance(
            row.get("config"), dict
        ):
            raise ValueError(f"missing locked model/config for {system}")

    dataset = payload.get("dataset_protocol")
    if not isinstance(dataset, dict):
        raise ValueError("sealed lock is missing its dataset protocol")
    if tuple(dataset.get("source_months", ())) != SEALED_MONTHS:
        raise ValueError("sealed source-month protocol mismatch")
    if tuple(dataset.get("strata", ())) != SEALED_STRATA:
        raise ValueError("sealed stratum protocol mismatch")

    matrix = payload.get("matrix_protocol")
    if not isinstance(matrix, dict):
        raise ValueError("sealed lock is missing its replay matrix protocol")
    if tuple(matrix.get("systems", ())) != LOCKED_SYSTEMS:
        raise ValueError("sealed matrix system protocol mismatch")
    if tuple(matrix.get("routing_modes", ())) != SEALED_ROUTES:
        raise ValueError("sealed routing protocol mismatch")

    source = payload.get("selection_source")
    if not isinstance(source, dict):
        raise ValueError("sealed lock is missing selection provenance")
    source_path = _project_path(
        source.get("path"), expected="results/native_summary.json"
    )
    search_sources = payload.get("search_sources")
    _validate_bound_search_sources(search_sources)
    hashes = payload.get("engine_hashes")
    if not isinstance(hashes, dict) or set(hashes) != set(SEALED_ENGINE_FILES):
        raise ValueError("sealed replay-engine hash set mismatch")

    if verify_environment:
        if not source_path.exists():
            raise FileNotFoundError(f"missing lock selection source: {source_path}")
        if sha256_path(source_path) != source.get("sha256"):
            raise ValueError("canonical selection source changed after profile lock")
        current_search_sources = search_source_bindings()
        if search_sources != current_search_sources:
            changed = [
                name
                for name in SEARCH_SOURCE_PATHS
                if search_sources.get(name) != current_search_sources.get(name)
            ]
            raise ValueError(
                "decision-grade search source changed after profile lock: "
                + ", ".join(changed)
            )
        current_hashes = engine_hashes()
        mismatches = [
            name for name in SEALED_ENGINE_FILES if hashes.get(name) != current_hashes[name]
        ]
        if mismatches:
            raise ValueError(
                "sealed replay engine changed after profile lock: "
                + ", ".join(mismatches)
            )
    return payload


def verify_postlock_hashes(
    *,
    lock_path: Path,
    csv_path: Path,
    metadata_path: Path,
    manifest_path: Path,
) -> dict[str, str]:
    """Verify the post-lock CSV and metadata sidecar hashes."""

    for path in (lock_path, csv_path, metadata_path, manifest_path):
        if not path.exists():
            raise FileNotFoundError(f"missing sealed artifact: {path}")
    manifest = json.loads(manifest_path.read_text())
    if not isinstance(manifest, dict) or manifest.get("schema_version") != 1:
        raise ValueError("unsupported sealed history manifest")
    actual = {
        "profile_lock_sha256": sha256_path(lock_path),
        "history_csv_sha256": sha256_path(csv_path),
        "history_metadata_sha256": sha256_path(metadata_path),
    }
    for field, digest in actual.items():
        if manifest.get(field) != digest:
            raise ValueError(f"sealed artifact hash mismatch: {field}")
    metadata = json.loads(metadata_path.read_text())
    if metadata.get("sealed_profile_lock_sha256") != actual[
        "profile_lock_sha256"
    ]:
        raise ValueError("sealed metadata is bound to a different profile lock")
    if metadata.get("snapshot_sha256") != actual["history_csv_sha256"]:
        raise ValueError("sealed metadata CSV hash mismatch")
    return actual

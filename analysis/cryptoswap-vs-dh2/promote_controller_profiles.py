#!/usr/bin/env python3
"""Freeze equal-budget controller-search winners for canonical replay.

Each shortlist contains the authoritative 150-grid winner and the five
highest-ranked finite, hard-valid policies under the same own-rho-first rule.
Dusk's extra local pass is diagnostic and cannot enter this lock.  Every
promoted row is bound to the exact search summary, grid manifest, and complete
150-row profile table.
"""

from __future__ import annotations

import argparse
import csv
from dataclasses import asdict
import hashlib
import json
from pathlib import Path
import re
import tempfile
from typing import Mapping

from cpmm_controller_search import grid_profiles as cpmm_grid_profiles
from curve_controller_search import (
    FULL_MATRIX_SPEC as CURVE_FULL_MATRIX_SPEC,
    grid_profiles as curve_grid_profiles,
    stage_fingerprint as curve_stage_fingerprint,
)
from dusk_controller_search import (
    FULL_MATRIX_SPEC as DUSK_FULL_MATRIX_SPEC,
    grid_profiles as dusk_grid_profiles,
    stage_fingerprint as dusk_stage_fingerprint,
)


ROOT = Path(__file__).resolve().parent
SCHEMA_VERSION = 2

SPECS = (
    {
        "system": "dusk_native",
        "summary": "results/controller_search/full/summary.json",
        "status": "controller_search_complete",
        "complete_field": "complete_matrix_evaluated_for_every_initial_profile",
        "id_pattern": r"grid_c(\d{2})_f(\d{2})",
        "output": "data/dusk_controller_profiles.json",
        "fields": (
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
    },
    {
        "system": "curve_native_v2_1",
        "summary": "results/curve_controller_search/full/summary.json",
        "status": "curve_controller_search_complete",
        "complete_field": "complete_matrix_evaluated_for_every_profile",
        "id_pattern": r"curve_grid_c(\d{2})_f(\d{2})",
        "output": "data/curve_controller_profiles.json",
        "fields": (
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
    },
    {
        "system": "dusk_cpmm",
        "summary": "results/cpmm_controller_search/full/summary.json",
        "status": "cpmm_controller_search_complete",
        "complete_field": "complete_matrix_evaluated_for_every_profile",
        "id_pattern": r"grid_c(\d{2})_f(\d{2})",
        "output": "data/cpmm_controller_profiles.json",
        "fields": (
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
    },
)


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _row_digest(row: Mapping[str, str]) -> str:
    return hashlib.sha256(
        json.dumps(row, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def _fingerprint(summary: Mapping[str, object]) -> str:
    direct = summary.get("fingerprint")
    if isinstance(direct, str):
        return direct
    fingerprints = summary.get("fingerprints")
    if isinstance(fingerprints, dict) and isinstance(
        fingerprints.get("grid"), str
    ):
        return str(fingerprints["grid"])
    raise ValueError("search summary lacks a grid fingerprint")


def current_search_fingerprint(system: str) -> str:
    """Recompute the exact full-grid fingerprint from the current engine.

    This is intentionally stronger than checking that a stored summary and
    manifest agree with one another: any change to the search grid, matched
    invariant, scenario matrix, guardrails, or hashed engine files makes an
    old result ineligible for promotion and canonical replay.
    """

    if system == "curve_native_v2_1":
        return curve_stage_fingerprint(
            curve_grid_profiles(),
            CURVE_FULL_MATRIX_SPEC,
            mode="full",
            stage="grid",
        )
    if system == "dusk_native":
        return dusk_stage_fingerprint(
            dusk_grid_profiles(),
            DUSK_FULL_MATRIX_SPEC,
            mode="full",
            stage="grid",
        )
    if system == "dusk_cpmm":
        return dusk_stage_fingerprint(
            cpmm_grid_profiles(),
            DUSK_FULL_MATRIX_SPEC,
            mode="cpmm_full",
            stage="grid",
        )
    raise KeyError(f"unknown promoted search system: {system}")


def _grid_coordinate(profile_id: str, pattern: str) -> tuple[int, int]:
    match = re.fullmatch(pattern, profile_id)
    if match is None:
        raise ValueError(f"winner is not an authoritative grid profile: {profile_id}")
    return int(match.group(1)), int(match.group(2))


def _truthy(value: object) -> bool:
    return str(value).strip().lower() == "true"


def _finite(value: object) -> float:
    number = float(value)
    if number != number or number in {float("inf"), float("-inf")}:
        raise ValueError("non-finite ranking value")
    return number


def build_snapshot(spec: Mapping[str, object]) -> dict[str, object]:
    summary_relative = str(spec["summary"])
    summary_path = ROOT / summary_relative
    if not summary_path.is_file():
        raise FileNotFoundError(f"missing full search summary: {summary_path}")
    summary = json.loads(summary_path.read_text())
    if (
        summary.get("status") != spec["status"]
        or summary.get("mode") != "full"
        or summary.get("decision_grade_search") is not True
        or summary.get(str(spec["complete_field"])) is not True
        or int(summary.get("initial_profiles", -1)) != 150
    ):
        raise ValueError(f"{spec['system']} search is not complete and decision-grade")
    winner = summary.get("winner")
    if not isinstance(winner, dict) or winner.get("hard_valid") is not True:
        raise ValueError(f"{spec['system']} search lacks a hard-valid winner")
    winner_id = str(winner.get("profile_id", ""))
    _grid_coordinate(winner_id, str(spec["id_pattern"]))
    fingerprint = _fingerprint(summary)
    if len(fingerprint) != 64:
        raise ValueError("search fingerprint must be SHA-256 shaped")
    current_fingerprint = current_search_fingerprint(str(spec["system"]))
    if fingerprint != current_fingerprint:
        raise ValueError(
            f"{spec['system']} full-search output is stale for the current "
            "grid, matrix, matched invariant, or engine files; rerun the "
            "full search before promotion"
        )

    grid_dir = summary_path.parent / "grid"
    manifest_path = grid_dir / "manifest.json"
    profiles_path = grid_dir / "profiles.csv"
    if not manifest_path.is_file() or not profiles_path.is_file():
        raise FileNotFoundError("full search grid manifest/profile table is missing")
    manifest = json.loads(manifest_path.read_text())
    content_hashes = manifest.get("content_hashes")
    if (
        manifest.get("fingerprint") != fingerprint
        or int(manifest.get("profiles", -1)) != 150
        or int(manifest.get("cells_per_profile", -1)) != 128
        or int(manifest.get("rows", -1)) != 19_200
        or manifest.get("selected_profile_ids") != [winner_id]
        or not isinstance(content_hashes, dict)
        or content_hashes.get("profiles.csv") != _sha256(profiles_path)
        or content_hashes.get("rows.csv") != _sha256(grid_dir / "rows.csv")
    ):
        raise ValueError("full search grid manifest does not bind 150x128 winner")
    with profiles_path.open() as source:
        profile_rows = list(csv.DictReader(source))
    if len(profile_rows) != 150 or len(
        {row["profile_id"] for row in profile_rows}
    ) != 150:
        raise ValueError("full search profile table must contain 150 unique rows")

    rho_field = (
        "independent_curve_service_relaxation"
        if spec["system"] == "curve_native_v2_1"
        else "independent_dusk_service_relaxation"
    )
    eligible = []
    for source_row in profile_rows:
        _grid_coordinate(str(source_row["profile_id"]), str(spec["id_pattern"]))
        if not _truthy(source_row.get("hard_valid")):
            continue
        try:
            rho = _finite(source_row[rho_field])
            minimum_utility = _finite(
                source_row["minimum_route_selection_utility"]
            )
            mean_utility = _finite(source_row["mean_route_selection_utility"])
        except (KeyError, TypeError, ValueError):
            continue
        eligible.append((rho, -minimum_utility, -mean_utility, source_row))
    eligible.sort(key=lambda item: (*item[:3], str(item[3]["profile_id"])))
    ranked_source_rows = [item[3] for item in eligible[:5]]
    if not ranked_source_rows or ranked_source_rows[0]["profile_id"] != winner_id:
        raise ValueError("search winner disagrees with the persisted own-rho ranking")

    selected_rows = []
    profile_inventory = {
        profile.profile_id: profile
        for profile in {
            "curve_native_v2_1": curve_grid_profiles,
            "dusk_native": dusk_grid_profiles,
            "dusk_cpmm": cpmm_grid_profiles,
        }[str(spec["system"])]()
    }
    for source_rank, source_row in enumerate(ranked_source_rows, start=1):
        profile_id = str(source_row["profile_id"])
        profile = profile_inventory.get(profile_id)
        if profile is None:
            raise ValueError(f"profile {profile_id} is absent from executable grid")
        row: dict[str, object] = {
            "profile_id": profile_id,
            "description": str(source_row["description"]),
            "selected_winner": profile_id == winner_id,
            "source_rank": source_rank,
            "source_profile_row_sha256": _row_digest(source_row),
            "model": asdict(profile.model),
            "config": asdict(profile.config),
        }
        for field in spec["fields"]:
            if field not in source_row:
                raise ValueError(f"profile table lacks required field {field}")
            row[str(field)] = float(source_row[str(field)])
        # Integer controller fields remain exactly integral in the snapshot.
        if "min_adjustment_interval_minutes" in row:
            row["min_adjustment_interval_minutes"] = int(
                float(row["min_adjustment_interval_minutes"])
            )
        selected_rows.append(row)
    selected_rows.sort(
        key=lambda row: (
            int(row["source_rank"]),
        )
    )
    if not selected_rows or selected_rows[0]["profile_id"] != winner_id:
        raise AssertionError("winner is absent from promoted grid neighborhood")

    payload: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "system": spec["system"],
        "winner_profile_id": winner_id,
        "selection_authority": "exhaustive_150_profile_grid_only",
        "diagnostic_shortlist": (
            "top five finite hard-valid policies by own service rho, "
            "worst-route utility, mean-route utility, and stable id"
        ),
        "local_refinement_selection_authority": False,
        "search_source": {
            "summary_path": summary_relative,
            "summary_sha256": _sha256(summary_path),
            "stage_manifest_path": str(manifest_path.relative_to(ROOT)),
            "stage_manifest_sha256": _sha256(manifest_path),
            "profiles_path": str(profiles_path.relative_to(ROOT)),
            "profiles_sha256": _sha256(profiles_path),
            "fingerprint": fingerprint,
            "profiles": 150,
            "cells_per_profile": 128,
            "rows": 19_200,
            "historical_reads": 0,
        },
        "profiles": selected_rows,
    }
    payload["promotion_fingerprint"] = hashlib.sha256(
        json.dumps(
            payload,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode()
    ).hexdigest()
    return payload


def _atomic_write(path: Path, payload: Mapping[str, object]) -> None:
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


def promote(*, force: bool = False) -> list[Path]:
    destinations = [ROOT / str(spec["output"]) for spec in SPECS]
    existing = [path for path in destinations if path.exists()]
    if existing and not force:
        raise FileExistsError(
            "promotion snapshots already exist; pass --force only after rerunning "
            "all bound searches: " + ", ".join(map(str, existing))
        )
    snapshots = [build_snapshot(spec) for spec in SPECS]
    for path, snapshot in zip(destinations, snapshots):
        _atomic_write(path, snapshot)
    return destinations


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Promote source-bound Curve, DH2, and CPMM grid winners"
    )
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    for path in promote(force=args.force):
        print(path.relative_to(ROOT))


if __name__ == "__main__":
    main()

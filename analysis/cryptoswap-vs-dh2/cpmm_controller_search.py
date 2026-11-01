#!/usr/bin/env python3
"""Independent 150-profile controller/fee search for Dusk's CPMM fallback.

The concentrated DH2 and the zero-concentration fallback must not share a
policy merely because one was optimized first.  This search uses the exact
same Dusk controller/fee grid and 128-cell synthetic matrix as the DH2 search,
but fixes the invariant to ``peak_depth=0, fade_scale=0``.  It never reads
historical prices and has no post-grid refinement budget.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict
import json
from pathlib import Path
from typing import Mapping

from dusk_controller_search import (
    FULL_MATRIX_SPEC,
    QUICK_STAGE_SPECS,
    SEARCH_SCHEMA_VERSION,
    grid_profiles as dh2_grid_profiles,
    rank_summaries,
    run_stage,
)
from models import CurrentDh2
from native_benchmark import NativeProfile


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results" / "cpmm_controller_search"
FULL_OUTPUT = RESULTS / "full"
QUICK_OUTPUT = RESULTS / "quick"
PROFILE_BUDGET = 150
CPMM = CurrentDh2(peak_depth=0.0, fade_scale=0.0)


def grid_profiles() -> tuple[NativeProfile, ...]:
    profiles = tuple(
        NativeProfile(
            "dusk_cpmm",
            profile.profile_id,
            CPMM,
            profile.config,
            f"independent CPMM candidate; {profile.description}",
        )
        for profile in dh2_grid_profiles()
    )
    if len(profiles) != PROFILE_BUDGET:
        raise AssertionError("CPMM search requires exactly 150 profiles")
    if len({profile.profile_id for profile in profiles}) != PROFILE_BUDGET:
        raise AssertionError("CPMM search profile ids are not unique")
    if any(
        not isinstance(profile.model, CurrentDh2)
        or profile.model.peak_depth != 0.0
        or profile.model.fade_scale != 0.0
        for profile in profiles
    ):
        raise AssertionError("CPMM search changed the zero-depth invariant")
    return profiles


def _atomic_write_json(path: Path, payload: Mapping[str, object]) -> None:
    # The stage writer already uses atomic replacement.  Reuse a same-folder
    # temporary through the standard library without adding another state
    # format for this small terminal summary.
    import tempfile

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


def run_search(
    *, workers: int, quick: bool, force: bool = False
) -> dict[str, object]:
    if workers <= 0:
        raise ValueError("workers must be positive")
    profiles = grid_profiles()
    mode = "quick" if quick else "full"
    output_root = QUICK_OUTPUT if quick else FULL_OUTPUT
    spec = QUICK_STAGE_SPECS["stage1"] if quick else FULL_MATRIX_SPEC
    stage = "smoke" if quick else "grid"
    rows, summaries, selected, fingerprint = run_stage(
        profiles,
        spec,
        output_root=output_root,
        mode=f"cpmm_{mode}",
        stage=stage,
        workers=workers,
        selection_count=1,
        force=force,
    )
    winner = selected[0]
    winner_summary = next(
        row
        for row in rank_summaries(summaries)
        if row["profile_id"] == winner.profile_id
    )
    summary: dict[str, object] = {
        "status": "smoke_only" if quick else "cpmm_controller_search_complete",
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
        "selection_rule": (
            "hard validity; minimum independent Dusk service relaxation; "
            "maximum worst-route fee-adjusted LP utility; maximum mean-route "
            "utility; stable profile id"
        ),
        "winner": {
            "profile_id": winner.profile_id,
            "description": winner.description,
            "hard_valid": bool(winner_summary["hard_valid"]),
            "independent_cpmm_service_relaxation": float(
                winner_summary["independent_dusk_service_relaxation"]
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
        },
        "scope": (
            "synthetic pre-lock selection only; exact CPMM invariant; "
            "no historical reads; no local refinement"
        ),
    }
    _atomic_write_json(output_root / "summary.json", summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Search an independent Dusk CPMM controller/fee policy"
    )
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    if args.workers <= 0:
        parser.error("--workers must be positive")
    print(
        json.dumps(
            run_search(
                workers=args.workers,
                quick=args.quick,
                force=args.force,
            ),
            indent=2,
        ),
        flush=True,
    )


if __name__ == "__main__":
    main()

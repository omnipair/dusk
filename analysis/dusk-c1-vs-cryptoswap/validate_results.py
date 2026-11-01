#!/usr/bin/env python3
"""Independently reconcile the focused benchmark outputs and source hashes."""

from __future__ import annotations

import csv
import hashlib
import json
import math
from pathlib import Path
import tempfile


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"


def read_csv(name: str) -> list[dict[str, str]]:
    with (RESULTS / name).open(newline="") as source:
        return list(csv.DictReader(source))


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def close(left: float, right: float, tolerance: float = 1e-8) -> bool:
    return abs(left - right) <= tolerance * max(1.0, abs(left), abs(right))


def one(rows: list[dict[str, str]], **filters: object) -> dict[str, str]:
    matches = [
        row
        for row in rows
        if all(row[key] == str(value) for key, value in filters.items())
    ]
    if len(matches) != 1:
        raise AssertionError(f"expected one row for {filters}, found {len(matches)}")
    return matches[0]


def atomic_json(path: Path, payload: object) -> None:
    with tempfile.NamedTemporaryFile("w", dir=path.parent, delete=False) as temporary:
        json.dump(payload, temporary, indent=2, sort_keys=True)
        temporary.write("\n")
        temporary_path = Path(temporary.name)
    temporary_path.replace(path)


def main() -> None:
    summary = json.loads((RESULTS / "summary.json").read_text())
    datasets = {
        "execution.csv": read_csv("execution.csv"),
        "jump_loss.csv": read_csv("jump_loss.csv"),
        "tail_behavior.csv": read_csv("tail_behavior.csv"),
        "recenter_impairment.csv": read_csv("recenter_impairment.csv"),
        "path_lvr.csv": read_csv("path_lvr.csv"),
        "restoring_quotes.csv": read_csv("restoring_quotes.csv"),
        "pareto_sweep.csv": read_csv("pareto_sweep.csv"),
        "service_constrained_matches.csv": read_csv("service_constrained_matches.csv"),
        "solver_proxy.csv": read_csv("solver_proxy.csv"),
        "bridge_sensitivity.csv": read_csv("bridge_sensitivity.csv"),
        "safety_grid.csv": read_csv("safety_grid.csv"),
    }
    expected_rows = {
        "execution.csv": 108,
        "jump_loss.csv": 162,
        "tail_behavior.csv": 81,
        "recenter_impairment.csv": 540,
        "path_lvr.csv": 54,
        "restoring_quotes.csv": 180,
        "pareto_sweep.csv": 200,
        "service_constrained_matches.csv": 3,
        "solver_proxy.csv": 108,
        "bridge_sensitivity.csv": 9,
        "safety_grid.csv": 420,
    }
    checks: dict[str, object] = {}
    checks["row_counts"] = {
        name: {"expected": expected_rows[name], "actual": len(rows)}
        for name, rows in datasets.items()
    }
    assert all(len(datasets[name]) == expected for name, expected in expected_rows.items())

    assert summary["status"] == "complete"
    assert summary["safety"]["all_passed"] is True
    assert summary["safety"]["offcenter_solver_failures"] == 0
    checks["safety"] = summary["safety"]

    source_hashes = summary["source_hashes"]
    for name in ("models.py", "benchmark.py", "test_models.py", "validate_results.py"):
        assert source_hashes[name] == sha256(ROOT / name)
    checks["source_hashes_match"] = True

    jumps = datasets["jump_loss.csv"]
    for profile in ("broad", "balanced", "aggressive"):
        for model in ("cpmm", "cryptoswap", "dusk_c1"):
            for factor in (1.01, 1.02, 1.05, 1.1, 1.25, 1.5, 2.0, 4.0, 10.0):
                upward = one(
                    jumps,
                    profile=profile,
                    model=model,
                    external_price_multiple=factor,
                )
                downward = one(
                    jumps,
                    profile=profile,
                    model=model,
                    external_price_multiple=1.0 / factor,
                )
                assert close(
                    float(upward["one_jump_arbitrage_loss_bps_of_hold_value"]),
                    float(downward["one_jump_arbitrage_loss_bps_of_hold_value"]),
                    2e-8,
                )
    checks["reciprocal_jump_symmetry"] = True

    paths = datasets["path_lvr.csv"]
    for profile in ("broad", "balanced", "aggressive"):
        for model in ("cpmm", "cryptoswap", "dusk_c1"):
            single = one(paths, profile=profile, model=model, scenario="single_jump_2x")
            jump = one(jumps, profile=profile, model=model, external_price_multiple=2.0)
            assert close(
                float(single["cumulative_path_lvr_bps_of_initial_tvl"]),
                float(jump["one_jump_arbitrage_loss_bps_of_initial_tvl"]),
            )
            reversal = one(paths, profile=profile, model=model, scenario="jump_reversal_2x")
            assert abs(float(reversal["final_loss_versus_hold_bps_of_initial_tvl"])) <= 2e-8
            assert float(reversal["cumulative_path_lvr_bps_of_initial_tvl"]) > 0.0
    checks["path_lvr_reconciliation"] = True

    tail_rows = [
        row
        for row in datasets["tail_behavior.csv"]
        if row["model"] == "dusk_c1" and row["curve_region"] == "tail"
    ]
    assert tail_rows
    assert all(close(float(row["local_exponent"]), 1.0, 2e-10) for row in tail_rows)
    checks["exact_cpmm_tail_shape"] = True

    same_tail = [
        row
        for row in datasets["recenter_impairment.csv"]
        if row["model"] == "dusk_c1"
        and row["reserve_basis"] == "native_post_arbitrage"
        and row["same_exact_tail"] == "True"
    ]
    assert same_tail
    assert all(abs(float(row["protected_q_impairment_bps"])) <= 2e-8 for row in same_tail)
    checks["same_tail_zero_protected_q_impairment"] = len(same_tail)

    for row in datasets["service_constrained_matches.csv"]:
        for size in (10, 100, 1_000, 2_000):
            assert float(row[f"matched_curve_execution_premium_{size}bps_trade"]) <= float(
                row[f"dusk_execution_premium_{size}bps_trade"]
            ) + 1e-8
            assert float(row[f"limit_curve_execution_premium_{size}bps_trade"]) <= float(
                row[f"dusk_execution_premium_{size}bps_trade"]
            ) + 2e-7
        assert float(row["dusk_loss_reduction_vs_no_worse_execution_curve_bps"]) > 0.0
        assert float(row["dusk_loss_reduction_vs_limit_curve_bps"]) > 0.0
        assert float(row["search_peak_depth_min"]) == 2.0
        assert float(row["search_peak_depth_max"]) == 2_000.0
        assert int(row["search_peak_depth_points"]) == 31
        assert int(row["search_fade_bisection_iterations"]) == 35
        assert row["limit_status"].startswith("infimum")
    checks["service_constrained_matches_and_limit_valid"] = True

    numeric_failures: list[str] = []
    for name, rows in datasets.items():
        for row_number, row in enumerate(rows, start=2):
            for key, value in row.items():
                if value in ("True", "False", "", "None"):
                    continue
                try:
                    numeric = float(value)
                except ValueError:
                    continue
                if not math.isfinite(numeric):
                    numeric_failures.append(f"{name}:{row_number}:{key}")
    assert not numeric_failures
    checks["all_numeric_outputs_finite"] = True

    receipt = {
        "status": "passed",
        "checks": checks,
        "limitations": [
            "Analytical floating-point reference, not production Q48/u128 arithmetic",
            "No Solana SBF compute-unit measurement",
            "No dynamic fees, retained funding, hLP, lending, or controller replay",
            "Synthetic deterministic paths are stress cases, not an order-flow forecast",
        ],
    }
    atomic_json(RESULTS / "validation.json", receipt)
    print(json.dumps(receipt, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()

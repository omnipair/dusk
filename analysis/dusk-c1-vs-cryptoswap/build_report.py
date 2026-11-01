#!/usr/bin/env python3
"""Build the canonical analytical report artifact and Markdown companion."""

from __future__ import annotations

import csv
import json
from pathlib import Path
import tempfile


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"


MODEL_LABELS = {
    "cpmm": "CPMM control",
    "cryptoswap": "CryptoSwap",
    "dusk_c1": "Dusk finite-tail C1",
}
PROFILE_LABELS = {
    "broad": "Broad P=20, s=0.05",
    "balanced": "Balanced P=200, s=0.005",
    "aggressive": "Aggressive P=2,000, s=0.0005",
}
SCENARIO_LABELS = {
    "single_jump_2x": "Single 2× jump",
    "staircase_2x": "20-step trend to 2×",
    "staircase_4x": "40-step trend to 4×",
    "jump_reversal_2x": "2× jump then reversal",
    "alternating_chop_10pct": "Alternating ±10% chop",
    "volatility_cluster": "Volatility cluster",
}


def coerce(value: str):
    if value == "True":
        return True
    if value == "False":
        return False
    if value == "":
        return None
    try:
        return int(value)
    except ValueError:
        pass
    try:
        return float(value)
    except ValueError:
        return value


def read_csv(name: str) -> list[dict[str, object]]:
    with (RESULTS / name).open(newline="") as source:
        return [{key: coerce(value) for key, value in row.items()} for row in csv.DictReader(source)]


def atomic_json(path: Path, payload: object) -> None:
    with tempfile.NamedTemporaryFile("w", dir=path.parent, delete=False) as temporary:
        json.dump(payload, temporary, indent=2, ensure_ascii=False)
        temporary.write("\n")
        temporary_path = Path(temporary.name)
    temporary_path.replace(path)


def atomic_text(path: Path, text: str) -> None:
    with tempfile.NamedTemporaryFile("w", dir=path.parent, delete=False) as temporary:
        temporary.write(text)
        temporary_path = Path(temporary.name)
    temporary_path.replace(path)


def query(path: str, sql: str, description: str, *tables: str) -> dict[str, object]:
    return {
        "language": "sql",
        "engine": "artifact snapshot",
        "sql": sql,
        "description": description,
        "tables_used": list(tables),
    }


def source(label: str, path: str, sql: str, description: str, *tables: str) -> dict[str, object]:
    return {
        "label": label,
        "path": path,
        "query": query(path, sql, description, *tables),
    }


def main() -> None:
    summary = json.loads((RESULTS / "summary.json").read_text())
    validation = json.loads((RESULTS / "validation.json").read_text())
    execution = read_csv("execution.csv")
    jumps = read_csv("jump_loss.csv")
    paths = read_csv("path_lvr.csv")
    restoring = read_csv("restoring_quotes.csv")
    pareto = read_csv("pareto_sweep.csv")
    recenter = read_csv("recenter_impairment.csv")
    bridge = read_csv("bridge_sensitivity.csv")
    service = read_csv("service_constrained_matches.csv")
    solvers = read_csv("solver_proxy.csv")
    tail = read_csv("tail_behavior.csv")
    safety_grid = read_csv("safety_grid.csv")

    execution_balanced = [row for row in execution if row["profile"] == "balanced"]
    for row in execution_balanced:
        row["model_display"] = MODEL_LABELS[str(row["model"])]

    jump_balanced = [
        row
        for row in jumps
        if row["profile"] == "balanced" and row["direction"] == "up"
    ]
    for row in jump_balanced:
        row["model_display"] = MODEL_LABELS[str(row["model"])]

    path_balanced = [row for row in paths if row["profile"] == "balanced"]
    for row in path_balanced:
        row["model_display"] = MODEL_LABELS[str(row["model"])]
        row["scenario_display"] = SCENARIO_LABELS[str(row["scenario"])]

    restoring_full = [
        row
        for row in restoring
        if row["starting_price_multiple"] == 2.0
        and row["restoring_fraction_of_log_price_gap"] == 1.0
    ]
    for row in restoring_full:
        row["model_display"] = MODEL_LABELS[str(row["model"])]
        row["profile_display"] = PROFILE_LABELS[str(row["profile"])]

    pareto_chart = []
    for row in pareto:
        enriched = dict(row)
        enriched["model_display"] = MODEL_LABELS[str(row["model"])]
        enriched["frontier_display"] = "Frontier" if row["pareto_multi_size_execution_vs_2x_loss"] else "Interior"
        pareto_chart.append(enriched)

    recenter_balanced = [
        row
        for row in recenter
        if row["profile"] == "balanced"
        and row["reserve_basis"] == "native_post_arbitrage"
        and row["external_price_multiple"] == 2.0
    ]
    for row in recenter_balanced:
        row["model_display"] = MODEL_LABELS[str(row["model"])]
        row["center_move_percent"] = 100.0 * float(row["center_movement_fraction_of_log_gap"])

    key_profiles = []
    for profile in ("broad", "balanced", "aggressive"):
        values = summary["decision_examples"][profile]
        execution_values = values["execution_premium_bps_by_trade_size"]
        losses = values["one_jump_2x_loss_bps_initial_tvl"]
        restoring_values = values[
            "full_restore_from_2x_output_difference_vs_same_reserve_cpmm_bps"
        ]
        key_profiles.append(
            {
                "profile": PROFILE_LABELS[profile],
                "dusk_10bps_execution_premium": execution_values["10"]["dusk_c1"],
                "curve_10bps_execution_premium": execution_values["10"]["cryptoswap"],
                "dusk_100bps_execution_premium": execution_values["100"]["dusk_c1"],
                "curve_100bps_execution_premium": execution_values["100"]["cryptoswap"],
                "dusk_1000bps_execution_premium": execution_values["1000"]["dusk_c1"],
                "curve_1000bps_execution_premium": execution_values["1000"]["cryptoswap"],
                "cpmm_2x_loss": losses["cpmm"],
                "dusk_2x_loss": losses["dusk_c1"],
                "curve_2x_loss": losses["cryptoswap"],
                "curve_excess_loss_removed_percent": 100.0
                * values["fraction_of_cryptoswap_excess_loss_removed_by_dusk"],
                "dusk_full_restore_difference_vs_cpmm_bps": restoring_values["dusk_c1"],
            }
        )

    service_table = []
    service_by_profile = {str(row["dusk_profile"]): row for row in service}
    for row in service:
        service_table.append(
            {
                "dusk_profile": PROFILE_LABELS[str(row["dusk_profile"])],
                "dusk_2x_loss": row["dusk_one_jump_loss_2x_bps_initial_tvl"],
                "curve_2x_loss": row["matched_curve_one_jump_loss_2x_bps_initial_tvl"],
                "dusk_loss_reduction": row[
                    "dusk_loss_reduction_vs_no_worse_execution_curve_bps"
                ],
                "limit_curve_2x_loss": row[
                    "limit_curve_one_jump_loss_2x_bps_initial_tvl"
                ],
                "dusk_limit_loss_reduction": row[
                    "dusk_loss_reduction_vs_limit_curve_bps"
                ],
                "curve_configuration": (
                    f"P={row['matched_curve_peak_depth']:g}; "
                    f"A={row['matched_curve_A']:g}; γ={row['matched_curve_gamma']:g}"
                ),
                "limit_characterization": (
                    f"P·s²→{row['limit_concentration_budget_P_times_s_squared']:.6g}; "
                    f"active {row['limit_active_trade_size_bps']:g}-bps constraint"
                ),
                "eligible_peak_grid_points": row["feasible_peak_depth_grid_points"],
                "search_domain": (
                    f"P∈[{row['search_peak_depth_min']:g}, {row['search_peak_depth_max']:g}], "
                    f"s∈[{row['search_fade_scale_min']:g}, {row['search_fade_scale_max']:g}]"
                ),
            }
        )

    bridge_table = []
    for row in bridge:
        bridge_table.append(
            {
                "profile": PROFILE_LABELS[str(row["profile"])],
                "bridge_start_fraction": row["bridge_start_fraction"],
                "transition_width_v": row["transition_width_v"],
                "maximum_log_price_curvature": row["maximum_absolute_log_price_curvature"],
                "inner_join_curvature": row["inner_join_absolute_log_price_curvature"],
                "bridge_join_curvature": row["bridge_join_absolute_log_price_curvature"],
                "join_curvature_ratio": row["inner_join_curvature_ratio"],
            }
        )

    safety_table = [
        {
            "parameter_cells": summary["safety"]["tested_parameter_cells"],
            "failed_cells": summary["safety"]["failed_parameter_cells"],
            "offcenter_solver_failures": summary["safety"]["offcenter_solver_failures"],
            "max_join_error": summary["safety"]["maximum_join_error"],
            "max_offcenter_exact_in_out_error": summary["safety"][
                "maximum_offcenter_exact_in_out_error"
            ],
            "max_offcenter_split_error": summary["safety"]["maximum_offcenter_split_error"],
            "validation_status": validation["status"],
        }
    ]

    solver_table = [
        {
            **row,
            "model_display": MODEL_LABELS[str(row["model"])],
        }
        for row in solvers
        if row["profile"] == "balanced" and row["quote_mode"] == "exact_in"
    ]

    dataset_ledger = [
        {"dataset": name, "rows": len(rows), "purpose": purpose}
        for name, rows, purpose in (
            ("execution.csv", execution, "Balanced-start exact-input quote sweep"),
            ("jump_loss.csv", jumps, "Reciprocal one-jump adverse-selection stress"),
            ("path_lvr.csv", paths, "Synthetic cumulative path-LVR scenarios"),
            ("restoring_quotes.csv", restoring, "Restoring and boundary-crossing quotes"),
            ("pareto_sweep.csv", pareto, "Cross-family service/risk frontier"),
            ("recenter_impairment.csv", recenter, "Native and common-reserve protected-Q impairment"),
            ("bridge_sensitivity.csv", bridge, "Protocol-fixed bridge-width sensitivity"),
            ("solver_proxy.csv", solvers, "Reference algorithm work; not CU"),
            ("tail_behavior.csv", tail, "Marginal-price and local-exponent tail behavior"),
            ("service_constrained_matches.csv", service, "No-worse-execution cross-family matches"),
            ("safety_grid.csv", safety_grid, "Invariant, solver, join, symmetry, and split-trade safety"),
        )
    ]

    datasets = {
        "execution_balanced": execution_balanced,
        "jump_balanced": jump_balanced,
        "path_balanced": path_balanced,
        "restoring_full": restoring_full,
        "pareto": pareto_chart,
        "recenter_balanced": recenter_balanced,
        "key_profiles": key_profiles,
        "service_constrained": service_table,
        "bridge_sensitivity": bridge_table,
        "safety": safety_table,
        "solver_proxy": solver_table,
        "dataset_ledger": dataset_ledger,
    }

    sources = [
        {"id": "summary", "label": "Focused deterministic benchmark summary", "path": "results/summary.json"},
        {"id": "execution", "label": "Balanced-start execution sweep", "path": "results/execution.csv"},
        {"id": "jumps", "label": "Reciprocal one-jump stress sweep", "path": "results/jump_loss.csv"},
        {"id": "paths", "label": "Deterministic path-LVR replay", "path": "results/path_lvr.csv"},
        {"id": "restoring", "label": "Restoring quote sweep", "path": "results/restoring_quotes.csv"},
        {"id": "pareto", "label": "Cross-family parameter and Pareto sweep", "path": "results/pareto_sweep.csv"},
        {"id": "service", "label": "No-worse-execution CryptoSwap matches", "path": "results/service_constrained_matches.csv"},
        {"id": "recenter", "label": "Protected-Q recenter impairment sweep", "path": "results/recenter_impairment.csv"},
        {"id": "bridge", "label": "C1 bridge sensitivity sweep", "path": "results/bridge_sensitivity.csv"},
        {"id": "safety", "label": "420-cell invariant safety sweep", "path": "results/safety_grid.csv"},
        {"id": "validation", "label": "Independent result reconciliation receipt", "path": "results/validation.json"},
        {"id": "models", "label": "Clean-room analytical reference", "path": "models.py"},
        {"id": "notebook", "label": "Executed analytical notebook", "path": "analysis.ipynb"},
        {"id": "dusk-spec", "label": "Dusk and CryptoSwap source boundary", "path": "SOURCE_BOUNDARY.md"},
        {
            "id": "curve-docs",
            "label": "Curve: Understanding CryptoSwap",
            "href": "https://docs.curve.finance/protocol/pool/understanding-cryptoswap",
        },
        {
            "id": "curve-whitepaper",
            "label": "Curve CryptoSwap whitepaper",
            "href": "https://docs.curve.finance/assets/files/whitepaper_cryptoswap-de196d8f95aae3164a1226234d6d96ca.pdf",
        },
        {
            "id": "curve-license",
            "label": "Pinned restrictive Curve source license",
            "href": "https://github.com/curvefi/twocrypto-ng/blob/5cbe558902402e8fcb331463089db65fc56c11f9/LICENSE",
        },
    ]

    charts = [
        {
            "id": "execution-chart",
            "title": "Balanced-center execution premium by trade size",
            "description": "Same P=200, s=0.005 inner parameters; lower is better. Dusk and CryptoSwap are identical until Dusk enters finite convergence.",
            "showDescription": True,
            "dataset": "execution_balanced",
            "type": "line",
            "source": source(
                "Balanced-start execution sweep",
                "results/execution.csv",
                "SELECT model_display, trade_size_bps_of_initial_tvl, average_execution_price_premium_bps FROM execution_balanced ORDER BY model_display, trade_size_bps_of_initial_tvl",
                "Balanced profile, zero-fee exact-input average price premium.",
                "execution_balanced",
            ),
            "encodings": {
                "x": {"field": "trade_size_bps_of_initial_tvl", "type": "quantitative", "title": "Trade size (bps initial TVL)"},
                "y": {"field": "average_execution_price_premium_bps", "type": "quantitative", "title": "Average execution premium (bps)"},
                "color": {"field": "model_display", "type": "nominal"},
            },
        },
        {
            "id": "jump-chart",
            "title": "One-jump adverse-selection loss",
            "description": "Balanced profile, zero fees, immediate arbitrage; basis points of initial TVL. This is not expected LP return.",
            "showDescription": True,
            "dataset": "jump_balanced",
            "type": "line",
            "source": source(
                "Reciprocal one-jump stress sweep",
                "results/jump_loss.csv",
                "SELECT model_display, external_price_multiple, one_jump_arbitrage_loss_bps_of_initial_tvl FROM jump_balanced ORDER BY model_display, external_price_multiple",
                "Upward one-jump stress points for the balanced profile.",
                "jump_balanced",
            ),
            "encodings": {
                "x": {"field": "external_price_multiple", "type": "quantitative", "title": "External price multiple"},
                "y": {"field": "one_jump_arbitrage_loss_bps_of_initial_tvl", "type": "quantitative", "title": "Loss (bps initial TVL)"},
                "color": {"field": "model_display", "type": "nominal"},
            },
        },
        {
            "id": "pareto-chart",
            "title": "1,000-bps execution versus 2× jump loss",
            "description": "Two hundred parameter cells; lower-left is better. Both families occupy the sampled two-objective frontier at this trade size.",
            "showDescription": True,
            "dataset": "pareto",
            "type": "scatter",
            "source": source(
                "Cross-family parameter and Pareto sweep",
                "results/pareto_sweep.csv",
                "SELECT model_display, peak_depth, fade_scale_gamma, execution_premium_1000bps_trade, one_jump_loss_2x_bps_initial_tvl, pareto_multi_size_execution_vs_2x_loss FROM pareto ORDER BY model_display, execution_premium_1000bps_trade",
                "CryptoSwap and Dusk C1 parameter sweep with common execution/loss objectives.",
                "pareto",
            ),
            "encodings": {
                "x": {"field": "execution_premium_1000bps_trade", "type": "quantitative", "title": "1,000-bps trade premium (bps)"},
                "y": {"field": "one_jump_loss_2x_bps_initial_tvl", "type": "quantitative", "title": "2× jump loss (bps initial TVL)"},
                "color": {"field": "model_display", "type": "nominal"},
            },
        },
        {
            "id": "path-chart",
            "title": "Cumulative path LVR across deterministic stresses",
            "description": "Balanced profile, zero fees. Lower is better; scenario averages are not probabilities or forecasts.",
            "showDescription": True,
            "dataset": "path_balanced",
            "type": "bar",
            "source": source(
                "Deterministic path-LVR replay",
                "results/path_lvr.csv",
                "SELECT scenario_display, model_display, cumulative_path_lvr_bps_of_initial_tvl FROM path_balanced ORDER BY scenario_display, model_display",
                "Cumulative fee-free arbitrage loss for six deterministic price paths.",
                "path_balanced",
            ),
            "encodings": {
                "x": {"field": "scenario_display", "type": "nominal"},
                "y": {"field": "cumulative_path_lvr_bps_of_initial_tvl", "type": "quantitative", "title": "Cumulative path LVR (bps initial TVL)"},
                "color": {"field": "model_display", "type": "nominal"},
            },
            "options": {"grouping": "grouped"},
        },
        {
            "id": "restoring-chart",
            "title": "Full restoring quote from a native 2× state",
            "description": "Difference versus a CPMM initialized with the same physical reserves; negative means less output than that CPMM.",
            "showDescription": True,
            "dataset": "restoring_full",
            "type": "bar",
            "source": source(
                "Restoring quote sweep",
                "results/restoring_quotes.csv",
                "SELECT profile_display, model_display, output_difference_vs_same_reserve_cpmm_bps FROM restoring_full ORDER BY profile_display, model_display",
                "Full center-restoring quote from each model's own post-2× state.",
                "restoring_full",
            ),
            "encodings": {
                "x": {"field": "profile_display", "type": "nominal"},
                "y": {"field": "output_difference_vs_same_reserve_cpmm_bps", "type": "quantitative", "title": "Output difference vs same-reserve CPMM (bps)"},
                "color": {"field": "model_display", "type": "nominal"},
            },
            "options": {"grouping": "grouped"},
        },
        {
            "id": "recenter-chart",
            "title": "Protected-Q impairment for a center move after a 2× jump",
            "description": "Balanced profile on each model's native post-arbitrage reserves. This is controller funding demand, not total token PnL.",
            "showDescription": True,
            "dataset": "recenter_balanced",
            "type": "line",
            "source": source(
                "Protected-Q recenter impairment sweep",
                "results/recenter_impairment.csv",
                "SELECT model_display, center_move_percent, protected_q_impairment_bps FROM recenter_balanced ORDER BY model_display, center_move_percent",
                "Protected-Q change across candidate center moves after a 2× price state.",
                "recenter_balanced",
            ),
            "encodings": {
                "x": {"field": "center_move_percent", "type": "quantitative", "title": "Center movement (% log-price gap)"},
                "y": {"field": "protected_q_impairment_bps", "type": "quantitative", "title": "Protected-Q impairment (bps)"},
                "color": {"field": "model_display", "type": "nominal"},
            },
        },
    ]

    tables = [
        {
            "id": "key-profile-table",
            "title": "Decision cross-sections",
            "description": "Same-parameter profiles isolate the outer-tail choice; these rows are not scenario-weighted expected returns.",
            "showDescription": True,
            "dataset": "key_profiles",
            "source": source(
                "Focused deterministic benchmark summary",
                "results/summary.json",
                "SELECT * FROM key_profiles ORDER BY profile",
                "Selected execution, jump-loss, and restoring metrics for the three declared profiles.",
                "key_profiles",
            ),
            "columns": [
                {"field": "profile", "label": "Profile"},
                {"field": "dusk_10bps_execution_premium", "label": "Dusk 10-bps premium", "format": "number"},
                {"field": "dusk_100bps_execution_premium", "label": "Dusk 100-bps premium", "format": "number"},
                {"field": "curve_100bps_execution_premium", "label": "CryptoSwap 100-bps premium", "format": "number"},
                {"field": "dusk_1000bps_execution_premium", "label": "Dusk 1,000-bps premium", "format": "number"},
                {"field": "curve_1000bps_execution_premium", "label": "CryptoSwap 1,000-bps premium", "format": "number"},
                {"field": "dusk_2x_loss", "label": "Dusk 2× loss", "format": "number"},
                {"field": "curve_2x_loss", "label": "CryptoSwap 2× loss", "format": "number"},
                {"field": "curve_excess_loss_removed_percent", "label": "CryptoSwap excess removed", "format": "number"},
                {"field": "dusk_full_restore_difference_vs_cpmm_bps", "label": "Dusk restore vs CPMM", "format": "number"},
            ],
        },
        {
            "id": "service-table",
            "title": "Service-constrained CryptoSwap comparison",
            "description": "Bounded columns use 31 log-spaced peak points in [2, 2,000] plus 35-step fade bisection in [1e-7, 0.199]. Limit columns show the unattained P→∞, s→0 envelope. Constraints cover balanced-start outward exact-in only.",
            "showDescription": True,
            "dataset": "service_constrained",
            "source": source(
                "No-worse-execution CryptoSwap matches",
                "results/service_constrained_matches.csv",
                "SELECT * FROM service_constrained ORDER BY dusk_profile",
                "Bounded and singular-limit service-constrained CryptoSwap comparisons.",
                "service_constrained",
            ),
            "columns": [
                {"field": "dusk_profile", "label": "Dusk profile"},
                {"field": "dusk_2x_loss", "label": "Dusk 2× loss", "format": "number"},
                {"field": "curve_2x_loss", "label": "Best eligible CryptoSwap loss", "format": "number"},
                {"field": "dusk_loss_reduction", "label": "Dusk loss reduction", "format": "number"},
                {"field": "curve_configuration", "label": "Eligible CryptoSwap configuration"},
                {"field": "limit_curve_2x_loss", "label": "CryptoSwap singular-limit loss", "format": "number"},
                {"field": "dusk_limit_loss_reduction", "label": "Dusk reduction vs limit", "format": "number"},
                {"field": "limit_characterization", "label": "Singular-limit constraint"},
            ],
        },
        {
            "id": "bridge-table",
            "title": "Protocol-fixed bridge sensitivity",
            "description": "The bridge fraction is a design constant, not an operator parameter. C1 removes the price jump but leaves a curvature kink.",
            "showDescription": True,
            "dataset": "bridge_sensitivity",
            "source": source(
                "C1 bridge sensitivity sweep",
                "results/bridge_sensitivity.csv",
                "SELECT * FROM bridge_sensitivity ORDER BY profile, bridge_start_fraction",
                "Transition width and one-sided curvature across three protocol-fixed bridge candidates.",
                "bridge_sensitivity",
            ),
            "columns": [
                {"field": "profile", "label": "Profile"},
                {"field": "bridge_start_fraction", "label": "Bridge fraction", "format": "number"},
                {"field": "transition_width_v", "label": "Width in v", "format": "number"},
                {"field": "maximum_log_price_curvature", "label": "Max |log curvature|", "format": "number"},
                {"field": "inner_join_curvature", "label": "Inner-side curvature", "format": "number"},
                {"field": "bridge_join_curvature", "label": "Bridge-side curvature", "format": "number"},
                {"field": "join_curvature_ratio", "label": "Curvature ratio", "format": "number"},
            ],
        },
        {
            "id": "safety-table",
            "title": "Analytical safety and reconciliation gates",
            "dataset": "safety",
            "source": source(
                "Independent result reconciliation receipt",
                "results/validation.json",
                "SELECT * FROM safety",
                "Invariant, off-center solver, exact-in/out, split, and source-hash gates.",
                "safety",
            ),
            "columns": [
                {"field": "parameter_cells", "label": "Parameter cells", "format": "number"},
                {"field": "failed_cells", "label": "Failed cells", "format": "number"},
                {"field": "offcenter_solver_failures", "label": "Off-center solver failures", "format": "number"},
                {"field": "max_join_error", "label": "Maximum C1 join error", "format": "number"},
                {"field": "max_offcenter_exact_in_out_error", "label": "Max exact-in/out error", "format": "number"},
                {"field": "max_offcenter_split_error", "label": "Max split error", "format": "number"},
                {"field": "validation_status", "label": "Validation"},
            ],
        },
        {
            "id": "solver-table",
            "title": "Reference algorithm work by state",
            "description": "Balanced profile, exact-input only. These nested floating-point counts are directional engineering clues—not compute-unit measurements.",
            "showDescription": True,
            "dataset": "solver_proxy",
            "source": source(
                "Reference algorithm work",
                "results/solver_proxy.csv",
                "SELECT model_display, state, direction, total_nonlinear_iterations_proxy, exact_tail_fast_path FROM solver_proxy ORDER BY state, direction, model_display",
                "Curated reference solver paths for center, transition, same-tail, and tail-crossing cases.",
                "solver_proxy",
            ),
            "columns": [
                {"field": "model_display", "label": "Model"},
                {"field": "state", "label": "State"},
                {"field": "direction", "label": "Direction"},
                {"field": "total_nonlinear_iterations_proxy", "label": "Nested iterations", "format": "number"},
                {"field": "exact_tail_fast_path", "label": "Exact-tail fast path"},
            ],
        },
        {
            "id": "dataset-table",
            "title": "Reproducible dataset ledger",
            "dataset": "dataset_ledger",
            "source": source(
                "Focused benchmark package",
                "results/summary.json",
                "SELECT * FROM dataset_ledger ORDER BY dataset",
                "Row counts and analytical purpose for every report dataset.",
                "dataset_ledger",
            ),
            "columns": [
                {"field": "dataset", "label": "Dataset"},
                {"field": "rows", "label": "Rows", "format": "number"},
                {"field": "purpose", "label": "Purpose"},
            ],
        },
    ]

    balanced = summary["decision_examples"]["balanced"]
    aggressive = summary["decision_examples"]["aggressive"]
    blocks = [
        {
            "id": "title",
            "type": "markdown",
            "body": (
                "# Dusk finite-tail C1 vs CryptoSwap, with CPMM control\n\n"
                "> **Historical finite-C1 decision snapshot; not a current-runtime validation.** "
                "The current nested tail-and-band implementation is outside this report's measured system."
            ),
        },
        {
            "id": "verdict",
            "type": "markdown",
            "sourceId": "summary",
            "body": (
                "## Decision\n\n"
                "**For Dusk’s intended long-tail markets, keep the finite-tail C1 invariant as the production candidate instead of adopting CryptoSwap’s global tail unchanged.** "
                "It preserves the same CryptoSwap center exactly, but stops releasing concentrated inventory after a finite transition and becomes constant-product. That materially reduces stale-price and toxic-tail loss while retaining very deep tiny-trade quotes.\n\n"
                "This is a **prototype decision, not a ship decision**. CryptoSwap remains better for medium and large execution in many parameter regions, and the C1 candidate still needs Q48/u128, one-atom, SBF-CU, dynamic-fee, and controller validation."
            ),
        },
        {
            "id": "headline-numbers",
            "type": "markdown",
            "sourceId": "summary",
            "body": (
                "### Why that decision\n\n"
                f"At the balanced profile and a 2× jump, CPMM loses **{balanced['one_jump_2x_loss_bps_initial_tvl']['cpmm']:.2f} bps**, "
                f"CryptoSwap loses **{balanced['one_jump_2x_loss_bps_initial_tvl']['cryptoswap']:.2f} bps**, and Dusk C1 loses **{balanced['one_jump_2x_loss_bps_initial_tvl']['dusk_c1']:.2f} bps** of initial TVL. "
                f"Dusk removes **{100 * balanced['fraction_of_cryptoswap_excess_loss_removed_by_dusk']:.1f}%** of CryptoSwap’s loss above CPMM. "
                f"The aggressive profile removes **{100 * aggressive['fraction_of_cryptoswap_excess_loss_removed_by_dusk']:.1f}%**.\n\n"
                "Across the bounded service-constrained search, every selected Dusk profile still has lower 2× jump loss than the safest found CryptoSwap configuration whose balanced-start outward 10/100/1,000/2,000-bps quotes are all no worse. This is a result inside the tested parameter envelope—not a global or expected-profit dominance claim."
            ),
        },
        {"id": "key-profile-block", "type": "table", "tableId": "key-profile-table"},
        {
            "id": "same-inner",
            "type": "markdown",
            "sourceId": "models",
            "body": (
                "## The difference is not the center\n\n"
                "For two assets, the current Dusk inner equation and the published CryptoSwap equation are algebraically identical under:\n\n"
                "$$P_{\\text{Dusk}}=2A_{\\text{Curve}},\\qquad s_{\\text{Dusk}}=\\gamma_{\\text{Curve}}$$\n\n"
                "Therefore `peak_depth` and `fade_scale` already span the same central family. No empirical parameter matching is needed to isolate the tail. The designs diverge only after Dusk begins its protocol-fixed finite transition."
            ),
        },
        {"id": "execution-chart-block", "type": "chart", "chartId": "execution-chart"},
        {
            "id": "execution-reading",
            "type": "markdown",
            "sourceId": "execution",
            "body": (
                "Dusk and CryptoSwap overlap exactly for small trades that remain inside the common inner curve. Once a trade enters the finite convergence region, Dusk’s quote deteriorates toward CPMM sooner. "
                "That is the cost of refusing to expose LP inventory through CryptoSwap’s long global fade. In the balanced profile, the two designs both cost about 1.16 bps for a 100-bps-of-TVL trade, but Dusk costs 1,707 bps versus CryptoSwap’s 413 bps at a 1,000-bps trade."
            ),
        },
        {
            "id": "tail-mechanics",
            "type": "markdown",
            "sourceId": "models",
            "body": (
                "## What the finite tail changes\n\n"
                "Write imbalance as $v=|x-y|/(2\\sqrt{xy})$ and product ratio as $q(v)=4xy/D^2$. Dusk follows CryptoSwap until $q_0=1-\\beta s$, then uses a C1 bridge:\n\n"
                "$$q(z)=q_0+m_0L\\left(z-z^2+\\frac{z^3}{3}\\right),\\qquad L=\\frac{3(q_1-q_0)}{m_0},\\qquad q_1=1-s$$\n\n"
                "At the outer join, $q'=0$, so the level set becomes:\n\n"
                "$$xy=\\frac{D^2(1-s)}{4}$$\n\n"
                "This is an **exact CPMM shape on a reduced product level**—not the original balanced CPMM product. CryptoSwap instead keeps fading globally and approaches its stronger asymptote only at extreme imbalance."
            ),
        },
        {"id": "jump-chart-block", "type": "chart", "chartId": "jump-chart"},
        {
            "id": "jump-reading",
            "type": "markdown",
            "sourceId": "jumps",
            "body": (
                "The finite tail stays close to CPMM during large stale-price moves. It does not erase the inventory already released in the shoulder, so Dusk remains lossier than CPMM: the gap is small for the balanced and aggressive profiles but material for the broad profile around a 2× move. The one-jump panel is fee-free immediate adverse selection; it is not a forecast and not a substitute for full path LVR. Reciprocal downside jumps were validated on a hold-value basis."
            ),
        },
        {"id": "pareto-chart-block", "type": "chart", "chartId": "pareto-chart"},
        {
            "id": "pareto-reading",
            "type": "markdown",
            "sourceId": "pareto",
            "body": (
                "The fair conclusion is **specialization**, not universal dominance. At 100 bps, sampled Dusk configurations own the two-objective execution/loss frontier; at 1,000 bps, both families contribute frontier points. CryptoSwap offers stronger medium/large execution in many matched regions, while Dusk adds points that combine very deep micro-trade liquidity with near-CPMM tail protection—precisely the combination that matters for long-tail assets with thin external discovery and episodic toxic flow."
            ),
        },
        {"id": "service-table-block", "type": "table", "tableId": "service-table"},
        {
            "id": "service-limit-reading",
            "type": "markdown",
            "sourceId": "service",
            "body": (
                "The bounded match is not hiding a better ordinary CryptoSwap configuration outside the coarse chart grid. In CryptoSwap’s mathematical edge limit, "
                "$s\\to0$, $P\\to\\infty$, and $Ps^2$ remains finite; no finite parameter pair attains that envelope. Even there, Dusk’s 2× loss remains lower by "
                f"**{float(service_by_profile['broad']['dusk_loss_reduction_vs_limit_curve_bps']):.2f} bps** (broad), "
                f"**{float(service_by_profile['balanced']['dusk_loss_reduction_vs_limit_curve_bps']):.2f} bps** (balanced), and "
                f"**{float(service_by_profile['aggressive']['dusk_loss_reduction_vs_limit_curve_bps']):.2f} bps** (aggressive). "
                "This establishes a structural tail-safety difference under the four declared outward service constraints; it still does not establish expected LP profit, restoring service, or controller performance."
            ),
        },
        {
            "id": "path-section",
            "type": "markdown",
            "body": "## Path behavior separates LVR from loss versus hold",
        },
        {"id": "path-chart-block", "type": "chart", "chartId": "path-chart"},
        {
            "id": "path-reading",
            "type": "markdown",
            "sourceId": "paths",
            "body": (
                "A single jump’s loss versus the original inventory equals the arbitrage loss in that special case. Multi-step LVR is different: a 20-step trend to 2× creates much less immediate arbitrage profit than a one-block 2× jump, while a jump-and-reversal has positive cumulative LVR even though final loss versus hold returns to zero. "
                "Dusk C1 never exceeds CryptoSwap’s LVR in the tested paths. It is usually near or above CPMM and is 0.10 bp below CPMM in one aggressive chop case—a numerically tiny inversion rather than evidence of strict ordering. The broad profile remains exposed during chop; the aggressive profile is nearly CPMM-like outside its tiny center."
            ),
        },
        {
            "id": "restoring-section",
            "type": "markdown",
            "sourceId": "restoring",
            "body": (
                "## The real drawback: restoring flow rebuilds concentration\n\n"
                "C1 smoothing does not make restoring flow free. In an exact tail with $D=2$ and $q_t=1-s$:\n\n"
                "$$x=\\sqrt{q_t/p},\\qquad y=\\sqrt{q_tp}$$\n\n"
                "Restoring to center makes Dusk output $y-1$, while a CPMM initialized with the same physical reserves outputs $y-q_t$. Dusk therefore withholds exactly:\n\n"
                "$$1-q_t=s$$\n\n"
                "That withheld output rebuilds the product level from $1-s$ to 1. It is why restoring quotes can be worse than a same-reserve CPMM even though outward quotes from balance stay between CPMM and CryptoSwap."
            ),
        },
        {"id": "restoring-chart-block", "type": "chart", "chartId": "restoring-chart"},
        {
            "id": "restoring-reading",
            "type": "markdown",
            "sourceId": "restoring",
            "body": (
                "At a native 2× state, the Dusk restoring penalty versus the same-reserve CPMM is about 1.21% for the balanced profile and 0.12% for the aggressive profile, but 11.08% for the broad profile. "
                "This makes the broad $s=0.05$ profile a poor default for long-tail markets without stronger fee/recovery evidence. Restoring trades may have no divergence surcharge, but the invariant itself still performs this rebuild."
            ),
        },
        {
            "id": "recenter-section",
            "type": "markdown",
            "body": "## Why the tail helps Dusk’s lazy recenter controller",
        },
        {"id": "recenter-chart-block", "type": "chart", "chartId": "recenter-chart"},
        {
            "id": "recenter-reading",
            "type": "markdown",
            "sourceId": "recenter",
            "body": (
                "When both the old and candidate centers leave the same physical reserves on the same exact-CPMM tail, balanced-equivalent protected Q is center-independent. All 41 such tested Dusk cases have zero protected-Q impairment. "
                "The first move that re-enters the convergence or concentrated region is not free and still needs retained funding. This metric measures the controller’s protected-Q budget only; it excludes subsequent arbitrage, fee-anchor changes, and timing."
            ),
        },
        {
            "id": "c1-not-c2",
            "type": "markdown",
            "sourceId": "bridge",
            "body": (
                "## C1 solves the price jump, not the curvature kink\n\n"
                "Both $q$ and $q'$ match at each join, so reserves and marginal price are continuous. But $q''$ is not matched: local depth changes abruptly at the inner-to-bridge join. In the balanced $\\beta=0.5$ case, absolute log-price curvature moves from about 0.11 on the inner side to 4.82 on the bridge side. "
                "That is safer than the current C0 marginal-price gap, but it remains a router, rounding, and fixed-point test target."
            ),
        },
        {"id": "bridge-table-block", "type": "table", "tableId": "bridge-table"},
        {
            "id": "bridge-decision",
            "type": "markdown",
            "sourceId": "bridge",
            "body": (
                "Do not expose $\\beta$ to operators. The 0.75 candidate creates the sharpest curvature for negligible economic gain and should be dropped. Keep 0.25 and 0.5 for the Rust prototype; 0.25 is smoother and wider, while 0.5 preserves more execution before converging. Lock one only after one-atom and CU profiling."
            ),
        },
        {
            "id": "compute-section",
            "type": "markdown",
            "sourceId": "summary",
            "body": (
                "## Compute implication—not yet a compute result\n\n"
                "A same-side Dusk tail swap, in either direction and for exact-in or exact-out, is a direct constant-product calculation. A tail-crossing or convergence-region swap still needs the general curve solve. CryptoSwap never gains a finite exact-tail branch. "
                "This is a credible architectural advantage, but the floating-point nested-iteration counts are implementation-specific and cannot predict Solana CU. Actual CU remains unmeasured."
            ),
        },
        {"id": "solver-table-block", "type": "table", "tableId": "solver-table"},
        {
            "id": "safety-section",
            "type": "markdown",
            "body": "## Mathematical and reproducibility gates",
        },
        {"id": "safety-table-block", "type": "table", "tableId": "safety-table"},
        {
            "id": "scope-limitations",
            "type": "markdown",
            "sourceId": "validation",
            "body": (
                "The analytical reference passes all 420 parameter cells, including off-center restoring, cross-center, exact-in/out, split, symmetry, join, and extreme-tail checks. The clean-room CryptoSwap root was independently reconciled with high-precision residuals.\n\n"
                "What this report does **not** establish: fixed-point one-atom behavior, Solana CU, dynamic-fee profitability, retained-surcharge sufficiency, EMA/controller timing, hLP/lending interactions, historical-volume forecasts, or production readiness. Synthetic paths are deterministic stress cases, not probabilities."
            ),
        },
        {
            "id": "next-actions",
            "type": "markdown",
            "body": (
                "## Final action plan\n\n"
                "1. Build a standalone Q48/u128 Rust prototype of the finite-tail C1 curve; do not replace production math yet.\n"
                "2. Differential-test every quote against this reference within one raw atom, especially both C1 joins and full restoring crossings.\n"
                "3. Profile legacy SPL exact-in/out for center, convergence, same-tail outward/restoring, and tail-crossing paths. Keep the below-100k ordinary-swap target.\n"
                "4. Replay Dusk’s uncapped divergence fee, volatility accumulator, retained funding, and lazy controller on historical long-tail paths.\n"
                "5. Prototype the balanced and aggressive regions; do not use the broad $s=0.05$ profile as the default.\n"
                "6. Lock the protocol bridge constant and production parameter bounds only after the fixed-point, CU, and controller gates pass."
            ),
        },
        {
            "id": "methodology",
            "type": "markdown",
            "sourceId": "models",
            "body": (
                "## Methodology and source boundary\n\n"
                "The CryptoSwap comparator is an independent implementation of the published two-asset equations. No Curve source code, constants, comments, tests, or implementation structure are copied or vendored. The pinned restrictive source license remains an explicit source-boundary warning. "
                "All benchmark data are local deterministic snapshots; no network access is required."
            ),
        },
        {"id": "dataset-table-block", "type": "table", "tableId": "dataset-table"},
        {
            "id": "reproduction",
            "type": "markdown",
            "body": (
                "## Reproduce\n\n"
                "From `analysis/dusk-c1-vs-cryptoswap`:\n\n"
                "```bash\n"
                "PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 benchmark.py\n"
                "PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 -m unittest -v test_models.py\n"
                "PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 validate_results.py\n"
                "PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 execute_notebook.py\n"
                "python3 build_report.py\n"
                "```\n\n"
                "The packaged HTML command and validation receipt are recorded in `README.md`."
            ),
        },
    ]

    generated_at = summary["generated_at"]
    manifest = {
        "version": 1,
        "surface": "report",
        "title": "Dusk finite-tail C1 vs CryptoSwap, with CPMM control",
        "description": "Historical finite-C1 experiment, not a current-runtime validation. Invariant-level decision analysis for Dusk long-tail concentration.",
        "generatedAt": generated_at,
        "sources": sources,
        "charts": charts,
        "tables": tables,
        "blocks": blocks,
    }
    artifact = {
        "surface": "report",
        "manifest": manifest,
        "snapshot": {
            "version": 1,
            "status": "ready",
            "generatedAt": generated_at,
            "datasets": datasets,
        },
        "sources": sources,
    }
    atomic_json(ROOT / "artifact.json", artifact)

    markdown = "\n\n".join(
        block["body"] for block in blocks if block["type"] == "markdown"
    )
    markdown += (
        "\n\n## Durable artifacts\n\n"
        "- `artifact.json`: canonical report source\n"
        "- `report.html`: packaged portable report\n"
        "- `analysis.ipynb`: executed notebook companion\n"
        "- `results/validation.json`: independent reconciliation receipt\n"
    )
    atomic_text(ROOT / "DECISION_REPORT.md", markdown + "\n")
    print(json.dumps({"artifact": str(ROOT / "artifact.json"), "markdown": str(ROOT / "DECISION_REPORT.md")}, indent=2))


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Build the corrected technical decision report and reader artifact."""

from __future__ import annotations

import csv
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import statistics
import tempfile

from native_validation import (
    cpmm_product_selection_is_consistent,
    dusk_product_selection_is_consistent,
    native_decision_is_consistent,
)
from native_replay import ARBITRAGE_CAP_COUNTER_SCOPE
from final_report_evidence import (
    expected_native_counts,
    require_final_report_audit,
    validate_search_and_promotions,
)
from benchmark import classify_p4_economics, p4_production_decision


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"
RANKED_MODELS = ("curve", "current_dh2")
CPMM_CONTROL_MODEL = "cpmm"
DISPLAY_MODELS = (*RANKED_MODELS, CPMM_CONTROL_MODEL)
MODEL_LABELS = {
    "curve": "Curve CryptoSwap",
    "current_dh2": "Current DH2",
    "sharp_dh2": "Sharp DH2 (rejected)",
    "hyperbolic_p4_dh2": "Hyperbolic p4 DH2 (experiment)",
    "cpmm": "CPMM",
}
CHART_MODEL_LABELS = {
    "curve": "Curve",
    "current_dh2": "DH2",
    "cpmm": "CPMM",
}
NATIVE_SYSTEM_LABELS = {
    "curve_native_v2_1": "Curve Twocrypto-NG v2.1",
    "dusk_native": "Current DH2 + Dusk controller",
    "dusk_current_dh2": "Current DH2 + Dusk controller",
    "curve_twocrypto_v2_1": "Curve Twocrypto-NG v2.1",
    "dusk_cpmm": "Independently selected Dusk CPMM",
}
NATIVE_REQUIRED_KEYS = {
    "matching_basis_sensitivity",
    "matching_basis_sensitivity_scope",
    "locked_profile_volume_sensitivity",
    "dusk_base_fee_split_sensitivity",
    "controller_counterfactual",
    "cpmm_product_fallback",
    "cpmm_matched_policy_ablation",
    "profile_selection_policy",
    "cpmm_product_selection_policy",
    "dusk_product_selection_policy",
    "decision",
}
NATIVE_COUNT_FILES = {
    "profile_rows": "native_profile_selection.csv",
    "training_rows": "native_training_results.csv",
    "held_out_rows": "native_held_out.csv",
    "paired_rows": "native_paired.csv",
    "cpmm_rows": "native_cpmm_control.csv",
    "cpmm_matched_policy_ablation_rows": (
        "native_cpmm_matched_policy_ablation.csv"
    ),
    "counterfactual_rows": "native_controller_counterfactual.csv",
    "neighborhood_rows": "native_neighborhood.csv",
    "scale_rows": "native_scale_sensitivity.csv",
    "cadence_rows": "native_cadence_sensitivity.csv",
    "volume_sensitivity_rows": "native_volume_sensitivity.csv",
    "basis_sensitivity_rows": "native_matching_basis_sensitivity.csv",
    "fee_split_sensitivity_rows": "native_dusk_fee_split_sensitivity.csv",
}
NATIVE_AUDIT_FILES = (
    "native_training_results.csv",
    "native_held_out.csv",
    "native_cpmm_control.csv",
    "native_cpmm_matched_policy_ablation.csv",
    "native_controller_counterfactual.csv",
    "native_neighborhood.csv",
    "native_scale_sensitivity.csv",
    "native_cadence_sensitivity.csv",
    "native_volume_sensitivity.csv",
    "native_matching_basis_sensitivity.csv",
)
NATIVE_DECISION_METRIC_FIELDS = (
    "net_lp_return_after_protocol_fee_bps",
    "net_lp_return_vs_hold_bps",
    "captured_user_volume_fraction",
    "selected_execution_cost_bps",
    "depth_degradation_fraction",
    "marginal_tracking_stale_fraction",
    "accounting_identity_error",
)


def load_csv(name: str) -> list[dict[str, str]]:
    with (RESULTS / name).open() as source:
        return list(csv.DictReader(source))


def load_json(name: str) -> dict[str, object]:
    return json.loads((RESULTS / name).read_text())


def require_complete_native_summary() -> dict[str, object]:
    summary = load_json("native_summary.json")
    search_promotion = validate_search_and_promotions(summary, root=ROOT)
    canonical_counts = expected_native_counts(search_promotion)
    missing = sorted(NATIVE_REQUIRED_KEYS - set(summary))
    validation = summary.get("validation", {})
    canonical = (
        isinstance(validation, dict)
        and validation.get("canonical_output") is True
    )
    expected_counts = validation.get("expected_counts", {})
    declared_counts_match = canonical and all(
        int(validation.get(field, -1)) == int(expected)
        for field, expected in expected_counts.items()
    )
    expected_fields_match = expected_counts == canonical_counts
    files_match = expected_fields_match and all(
        len(load_csv(filename)) == int(expected_counts[field])
        for field, filename in NATIVE_COUNT_FILES.items()
    )
    counts_match = declared_counts_match and files_match
    replay_audit_rows = [
        row
        for filename in NATIVE_AUDIT_FILES
        for row in load_csv(filename)
    ]
    accounting_errors = [
        abs(float(row["accounting_identity_error"]))
        for row in replay_audit_rows
    ]
    actual_decision_metrics_finite = all(
        field in row and math.isfinite(float(row[field]))
        for row in replay_audit_rows
        for field in NATIVE_DECISION_METRIC_FIELDS
    )
    actual_maximum_accounting_error = max(accounting_errors)
    declared_maximum_accounting_error = float(
        validation.get("maximum_accounting_identity_error", float("nan"))
    )
    declared_external_reads = int(
        validation.get("controller_external_price_reads", -1)
    )
    controller_api_isolation = validation.get("controller_api_oracle_isolation")
    actual_external_reads = max(
        int(row["controller_external_price_reads"])
        for row in replay_audit_rows
    )
    actual_capped_arbitrage_events = sum(
        int(row.get("arbitrage_capped_event_count", 0))
        for row in replay_audit_rows
    )
    cap_counter_schema_valid = all(
        "arbitrage_capped_event_count" in row
        and "scored_arbitrage_capped_event_count" in row
        and "arbitrage_numerical_failure_count" in row
        and "scored_arbitrage_numerical_failure_count" in row
        and int(row["scored_arbitrage_capped_event_count"])
        <= int(row["arbitrage_capped_event_count"])
        and int(row["scored_arbitrage_numerical_failure_count"])
        <= int(row["arbitrage_numerical_failure_count"])
        for row in replay_audit_rows
    )
    actual_scored_capped_arbitrage_events = sum(
        int(row.get("scored_arbitrage_capped_event_count", 0))
        for row in replay_audit_rows
    )
    actual_arbitrage_numerical_failures = sum(
        int(row.get("arbitrage_numerical_failure_count", 0))
        for row in replay_audit_rows
    )
    actual_scored_arbitrage_numerical_failures = sum(
        int(row.get("scored_arbitrage_numerical_failure_count", 0))
        for row in replay_audit_rows
    )
    declared_capped_arbitrage_events = int(
        validation.get("arbitrage_capped_event_count", -1)
    )
    declared_scored_capped_arbitrage_events = int(
        validation.get("scored_arbitrage_capped_event_count", -1)
    )
    declared_arbitrage_numerical_failures = int(
        validation.get("arbitrage_numerical_failure_count", -1)
    )
    declared_scored_arbitrage_numerical_failures = int(
        validation.get("scored_arbitrage_numerical_failure_count", -1)
    )
    declared_cap_counter_scope = validation.get("arbitrage_cap_counter_scope")
    post_run_audit = validation.get("post_run_audit", {})
    expected_audit_rows = sum(
        canonical_counts[field]
        for field, filename in NATIVE_COUNT_FILES.items()
        if filename in NATIVE_AUDIT_FILES
    )
    selection_policy = summary.get("profile_selection_policy", {})
    decision = summary.get("decision", {})
    profile_selection = summary.get("profile_selection", {})
    expected_selection_systems = {
        "curve_native_v2_1",
        "dusk_native",
        "dusk_cpmm",
    }
    expected_candidate_systems = {"curve_native_v2_1", "dusk_native"}
    minimum_relaxations = (
        selection_policy.get("minimum_required_relaxation_by_system", {})
        if isinstance(selection_policy, dict)
        else {}
    )
    selection_systems = (
        selection_policy.get("systems")
        if isinstance(selection_policy, dict)
        else None
    )
    selection_roles_valid = (
        isinstance(profile_selection, dict)
        and set(profile_selection) == expected_selection_systems
        and all(isinstance(value, dict) for value in profile_selection.values())
        and isinstance(minimum_relaxations, dict)
        and set(minimum_relaxations) == expected_candidate_systems
        and isinstance(selection_systems, (list, tuple, set))
        and set(selection_systems) == expected_candidate_systems
    )
    cpmm_selection_policy = summary.get("cpmm_product_selection_policy", {})
    cpmm_selection_valid = (
        isinstance(cpmm_selection_policy, dict)
        and cpmm_product_selection_is_consistent(cpmm_selection_policy)
        and selection_roles_valid
        and cpmm_selection_policy.get("selected_profile_id")
        == profile_selection["dusk_cpmm"].get("profile_id")
    )
    cpmm_product = summary.get("cpmm_product_fallback", {})
    cpmm_ablation = summary.get("cpmm_matched_policy_ablation", {})
    cpmm_roles_valid = (
        cpmm_selection_valid
        and isinstance(cpmm_product, dict)
        and cpmm_product.get("participates_in_winner_gate") is True
        and cpmm_product.get("control_system") == "dusk_cpmm"
        and cpmm_product.get("control_profile_id")
        == profile_selection["dusk_cpmm"].get("profile_id")
        and cpmm_product.get("product_ready_on_training_service")
        is cpmm_selection_policy.get("product_ready_on_training_service")
        and math.isclose(
            float(cpmm_product.get("service_relaxation", math.nan)),
            float(cpmm_selection_policy.get("service_relaxation", math.nan)),
            rel_tol=0.0,
            abs_tol=1e-12,
        )
        and isinstance(cpmm_ablation, dict)
        and cpmm_ablation.get("participates_in_winner_gate") is False
        and cpmm_ablation.get("control_system")
        == "dusk_cpmm_matched_policy"
        and cpmm_ablation.get("control_profile_id")
        == profile_selection["dusk_native"].get("profile_id")
    )
    dusk_selection_policy = summary.get("dusk_product_selection_policy", {})
    dusk_selection_valid = (
        isinstance(dusk_selection_policy, dict)
        and dusk_product_selection_is_consistent(dusk_selection_policy)
        and selection_roles_valid
        and dusk_selection_policy.get("selected_profile_id")
        == profile_selection["dusk_native"].get("profile_id")
    )
    decision_consistent = native_decision_is_consistent(
        selection_policy,
        decision,
    )
    audits_pass = (
        all(math.isfinite(value) for value in accounting_errors)
        and actual_decision_metrics_finite
        and validation.get("all_decision_metrics_finite") is True
        and math.isfinite(declared_maximum_accounting_error)
        and actual_maximum_accounting_error <= 1e-5
        and declared_maximum_accounting_error <= 1e-5
        and actual_external_reads == 0
        and declared_external_reads == 0
        and isinstance(controller_api_isolation, dict)
        and controller_api_isolation.get("passes") is True
        and controller_api_isolation.get("violations") == {}
        and actual_capped_arbitrage_events == 0
        and declared_capped_arbitrage_events == 0
        and cap_counter_schema_valid
        and actual_scored_capped_arbitrage_events == 0
        and declared_scored_capped_arbitrage_events == 0
        and actual_arbitrage_numerical_failures == 0
        and declared_arbitrage_numerical_failures == 0
        and actual_scored_arbitrage_numerical_failures == 0
        and declared_scored_arbitrage_numerical_failures == 0
        and declared_cap_counter_scope == ARBITRAGE_CAP_COUNTER_SCOPE
        and isinstance(post_run_audit, dict)
        and post_run_audit.get("status") == "passed"
        and post_run_audit.get("scope") == "all_underlying_replay_matrices"
        and post_run_audit.get("controller_api_oracle_isolation")
        == controller_api_isolation
        and post_run_audit.get("arbitrage_cap_counter_scope")
        == ARBITRAGE_CAP_COUNTER_SCOPE
        and int(post_run_audit.get("arbitrage_capped_event_count", -1)) == 0
        and int(
            post_run_audit.get("scored_arbitrage_capped_event_count", -1)
        )
        == 0
        and int(post_run_audit.get("arbitrage_numerical_failure_count", -1))
        == 0
        and int(
            post_run_audit.get(
                "scored_arbitrage_numerical_failure_count",
                -1,
            )
        )
        == 0
        and int(post_run_audit.get("audited_rows", -1)) == expected_audit_rows
        and set(post_run_audit.get("audited_files", [])) == set(NATIVE_AUDIT_FILES)
        and selection_roles_valid
        and dusk_selection_valid
        and cpmm_selection_valid
        and cpmm_roles_valid
        and decision_consistent
    )
    if (
        summary.get("status") != "complete"
        or missing
        or not canonical
        or not counts_match
        or not audits_pass
    ):
        missing_text = ", ".join(missing) if missing else "none"
        raise RuntimeError(
            "canonical native benchmark is not complete; "
            f"status={summary.get('status')!r}, canonical={canonical}, "
            f"counts_match={counts_match}, audits_pass={audits_pass}, "
            f"missing={missing_text}. "
            "Run native_benchmark.py without --quick before building the report."
        )
    validation["maximum_accounting_identity_error"] = (
        actual_maximum_accounting_error
    )
    validation["controller_external_price_reads"] = actual_external_reads
    validation["arbitrage_capped_event_count"] = actual_capped_arbitrage_events
    validation["scored_arbitrage_capped_event_count"] = (
        actual_scored_capped_arbitrage_events
    )
    validation["arbitrage_numerical_failure_count"] = (
        actual_arbitrage_numerical_failures
    )
    validation["scored_arbitrage_numerical_failure_count"] = (
        actual_scored_arbitrage_numerical_failures
    )
    validation["arbitrage_cap_counter_scope"] = ARBITRAGE_CAP_COUNTER_SCOPE
    return summary


def money(value: float) -> str:
    sign = "−" if value < 0 else ""
    return f"{sign}${abs(value):,.0f}"


def percent(value: float, digits: int = 1) -> str:
    return f"{value * 100:.{digits}f}%"


def bps(value: float, digits: int = 2) -> str:
    return f"{value:.{digits}f} bps"


def signed_bps(value: float, digits: int = 2) -> str:
    return f"{value:+.{digits}f} bps"


def signed_pp(value: float, digits: int = 1) -> str:
    return f"{value * 100:+.{digits}f} pp"


def native_winner_text(winner: object) -> str:
    if winner is None:
        return "neither system"
    return NATIVE_SYSTEM_LABELS.get(str(winner), str(winner))


def format_profile(profile: dict[str, object]) -> str:
    return f"`{profile['profile_id']}` ({profile['description']})"


def compact_locked_config(row: dict[str, object]) -> str:
    """Render only the decision-useful fields from a fully bound profile."""

    system = str(row["system"])
    model = row["canonical_locked_model"]
    controller = row["canonical_locked_controller"]
    if not isinstance(model, dict) or not isinstance(controller, dict):
        raise RuntimeError("promoted locked profile lacks full model/controller config")
    if system == "curve_native_v2_1":
        return (
            f"A={float(model['amplification']):g}; gamma={float(model['gamma']):g}; "
            f"mid/out={float(model['mid_fee']) * 10_000:.1f}/"
            f"{float(model['out_fee']) * 10_000:.1f} bps; EMA="
            f"{float(controller['ema_half_life_minutes']):g}m; min step="
            f"{float(controller['min_adjustment_step']) * 10_000:.1f} bps"
        )
    fee = controller.get("fee")
    if not isinstance(fee, dict):
        raise RuntimeError("promoted Dusk profile lacks its full fee config")
    return (
        f"peak={float(model['peak_depth']):g}; fade={float(model['fade_scale']):g}; "
        f"EMA={float(controller['center_ema_half_life_minutes']):g}m; threshold/step="
        f"{float(controller['adjustment_threshold']) * 10_000:.1f}/"
        f"{float(controller['adjustment_step']) * 10_000:.1f} bps; dynamic cap="
        f"{(int(fee['max_fee_rate_nad']) - int(fee['base_fee_rate_nad'])) / 100_000:.1f} bps"
    )


def native_decision_sentence(
    *,
    winner: object,
    view: str,
) -> str:
    if winner is None:
        return (
            f"Under the {view} ledger, neither system clears every native "
            "acceptance rule at the primary equal-execution lock across routes, "
            "flows, the top-ranked policy shortlist, pool scales, volume levels, "
            "and one-minute cadence."
        )
    return (
        f"Under the {view} ledger, **{native_winner_text(winner)}** clears every "
        "declared native acceptance rule at the primary equal-execution lock; "
        "the other system does not."
    )


def native_route_markdown(rows: list[dict[str, object]]) -> str:
    by_route = {str(row["routing_mode"]): row for row in rows}
    lines = [
        "| Route | Dusk − Curve gross LP return (fee-adjusted-selected profiles) | Dusk − Curve fee-adjusted LP return | Selected-order cost delta | Captured-volume delta | Marginal-tracking stale delta |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for route in ("fixed", "elastic"):
        row = by_route[route]
        lines.append(
            "| "
            + " | ".join(
                (
                    route.title(),
                    signed_bps(float(row["gross_return_delta_bps"])),
                    signed_bps(float(row["fee_adjusted_return_delta_bps"])),
                    signed_bps(float(row["execution_cost_delta_bps"])),
                    signed_pp(float(row["capture_delta_fraction"])),
                    signed_pp(
                        float(row["marginal_tracking_stale_delta_fraction"])
                    ),
                )
            )
            + " |"
        )
    return "\n".join(lines)


def native_flow_markdown(rows: list[dict[str, object]]) -> str:
    lines = [
        "| Flow | Route | Median Dusk − Curve LP return | Bottom-decile mean | p90 selected-order cost regression | p10 capture advantage | p90 marginal-tracking stale regression |",
        "|---|---|---:|---:|---:|---:|---:|",
    ]
    for row in rows:
        lines.append(
            "| "
            + " | ".join(
                (
                    str(row["flow_regime"]).replace("_", " ").title(),
                    str(row["routing_mode"]).title(),
                    signed_bps(float(row["median_return_advantage_bps"])),
                    signed_bps(float(row["bottom_decile_mean_return_advantage_bps"])),
                    signed_bps(float(row["p90_execution_regression_bps"])),
                    signed_pp(float(row["p10_capture_advantage_fraction"])),
                    signed_pp(
                        float(
                            row[
                                "p90_marginal_tracking_stale_regression_fraction"
                            ]
                        )
                    ),
                )
            )
            + " |"
        )
    return "\n".join(lines)


def native_basis_markdown(rows: list[dict[str, object]]) -> str:
    lines = [
        "| Matching basis | Route | Median Dusk − Curve fee-adjusted LP return | Bottom-decile mean | p90 execution regression | p10 capture advantage |",
        "|---|---|---:|---:|---:|---:|",
    ]
    for row in rows:
        lines.append(
            "| "
            + " | ".join(
                (
                    str(row["matching_basis"]).replace("_", " ").title(),
                    str(row["routing_mode"]).title(),
                    signed_bps(float(row["median_return_advantage_bps"])),
                    signed_bps(float(row["bottom_decile_mean_return_advantage_bps"])),
                    signed_bps(float(row["p90_execution_regression_bps"])),
                    signed_pp(float(row["p10_capture_advantage_fraction"])),
                )
            )
            + " |"
        )
    return "\n".join(lines)


def native_volume_markdown(rows: list[dict[str, object]]) -> str:
    lines = [
        "| Comparison | Daily volume / pool | Median of flow-route medians | Worst bottom-decile mean | Worst p90 execution regression | Worst p10 capture advantage | Worst p90 marginal-stale regression |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for row in rows:
        lines.append(
            "| "
            + " | ".join(
                (
                    str(row["comparison"]),
                    percent(float(row["daily_volume_pool_ratio"]), 0),
                    signed_bps(float(row["median_return_advantage_bps"])),
                    signed_bps(
                        float(row["worst_bottom_decile_mean_return_advantage_bps"])
                    ),
                    signed_bps(float(row["worst_p90_execution_regression_bps"])),
                    signed_pp(float(row["worst_p10_capture_advantage_fraction"])),
                    signed_pp(
                        float(
                            row[
                                "worst_p90_marginal_tracking_stale_regression_fraction"
                            ]
                        )
                    ),
                )
            )
            + " |"
        )
    return "\n".join(lines)


def native_fee_split_markdown(rows: list[dict[str, object]]) -> str:
    lines = [
        "| Dusk base-fee revenue share | Route | Median Dusk − Curve LP return | p10 return delta | Dusk cell-win share |",
        "|---:|---|---:|---:|---:|",
    ]
    for row in rows:
        lines.append(
            "| "
            + " | ".join(
                (
                    f"{int(row['dusk_base_fee_revenue_share_bps']) / 100:.0f}%",
                    str(row["routing_mode"]).title(),
                    signed_bps(float(row["median_return_delta_bps"])),
                    signed_bps(float(row["p10_return_delta_bps"])),
                    percent(float(row["cell_win_fraction"])),
                )
            )
            + " |"
        )
    return "\n".join(lines)


def matching_row(
    rows: list[dict[str, str]],
    basis: str,
    model: str,
) -> dict[str, str]:
    return next(
        row
        for row in rows
        if row["basis"] == basis and row["model"] == model
    )


def csv_bool(value: str, *, field: str) -> bool:
    normalized = value.strip().lower()
    if normalized == "true":
        return True
    if normalized == "false":
        return False
    raise RuntimeError(f"{field} must be True or False, got {value!r}")


def domain_number(value: float) -> str:
    if value != 0.0 and abs(value) < 0.001:
        return f"{value:.0e}".replace("e-0", "e-").replace("e+0", "e+")
    if value.is_integer():
        return f"{int(value):,}"
    return f"{value:g}"


def displayed_pareto_rows(
    rows: list[dict[str, str]],
) -> list[dict[str, object]]:
    """Filter to the ranked pair plus CPMM and recompute their joint frontier."""
    selected: list[dict[str, object]] = [
        dict(row) for row in rows if row["model"] in DISPLAY_MODELS
    ]
    observed_models = {str(row["model"]) for row in selected}
    if observed_models != set(DISPLAY_MODELS):
        raise RuntimeError(
            "displayed Pareto input must contain Curve, current DH2, and CPMM; "
            f"observed {sorted(observed_models)!r}"
        )

    scores: list[tuple[float, float]] = []
    for row in selected:
        score = (
            float(row["execution_index_bps"]),
            float(row["jump_loss_index_bps"]),
        )
        if not all(math.isfinite(value) for value in score):
            raise RuntimeError("displayed Pareto input contains a non-finite score")
        scores.append(score)

    for index, row in enumerate(selected):
        execution, jump_loss = scores[index]
        row["combined_pareto"] = not any(
            other_execution <= execution
            and other_jump_loss <= jump_loss
            and (
                other_execution < execution
                or other_jump_loss < jump_loss
            )
            for other_index, (other_execution, other_jump_loss) in enumerate(
                scores
            )
            if other_index != index
        )
    return selected


def binary_routing_summary(
    rows: list[dict[str, str]],
) -> list[dict[str, object]]:
    """Re-run the static quote tournament for Curve versus current DH2 only."""
    intent_fields = (
        "normalized_marginal_price",
        "trade_bps_of_marked_tvl",
        "direction",
    )
    grouped: dict[tuple[str, str, str], dict[str, float]] = {}
    costs_by_model: dict[str, list[float]] = {
        model: [] for model in RANKED_MODELS
    }
    for row in rows:
        model = row["model"]
        if model not in RANKED_MODELS:
            continue
        key = tuple(row[field] for field in intent_fields)
        costs = grouped.setdefault(key, {})
        if model in costs:
            raise RuntimeError(
                f"duplicate {model} routing quote for intent {key!r}"
            )
        cost = float(row["execution_cost_bps"])
        if not math.isfinite(cost):
            raise RuntimeError("binary routing input contains a non-finite quote")
        costs[model] = cost
        costs_by_model[model].append(cost)

    if not grouped:
        raise RuntimeError("binary routing input contains no ranked quote intents")
    for key, costs in grouped.items():
        if set(costs) != set(RANKED_MODELS):
            raise RuntimeError(
                f"binary routing intent {key!r} does not contain both ranked models"
            )

    wins = {model: 0.0 for model in RANKED_MODELS}
    for costs in grouped.values():
        best = min(costs.values())
        winners = [
            model
            for model, cost in costs.items()
            if cost <= best + 1e-9
        ]
        for model in winners:
            wins[model] += 1.0 / len(winners)

    intent_count = len(grouped)
    summary = [
        {
            "model": MODEL_LABELS[model],
            "model_id": model,
            "best_quote_intent_share": wins[model] / intent_count,
            "median_execution_cost_bps": statistics.median(
                costs_by_model[model]
            ),
            "intent_count": intent_count,
        }
        for model in RANKED_MODELS
    ]
    if not math.isclose(
        sum(float(row["best_quote_intent_share"]) for row in summary),
        1.0,
        rel_tol=0.0,
        abs_tol=1e-12,
    ):
        raise RuntimeError("binary routing shares do not sum to one")
    return summary


def paired_path_summary(
    pairs: list[dict[str, str]],
) -> list[dict[str, object]]:
    display_labels = {
        "alternating_chop": "chop",
        "flat": "flat",
        "historical_2024-03": "Mar-24",
        "historical_2024-08": "Aug-24",
        "historical_2024-12": "Dec-24",
        "historical_2025-01": "Jan-25",
        "historical_2025-04": "Apr-25",
        "jump_reversal": "reversal",
        "mean_reversion": "mean rev",
        "single_jump": "jump",
        "staircase": "stairs",
        "trend": "trend",
        "volatility_cluster": "vol cluster",
    }
    grouped: dict[tuple[str, str, str], list[float]] = {}
    for row in pairs:
        if row["competitor"] not in RANKED_MODELS:
            continue
        key = (row["path"], row["path_class"], row["competitor"])
        grouped.setdefault(key, []).append(float(row["paired_net_lp_delta"]))
    output = []
    for (path, path_class, competitor), values in sorted(grouped.items()):
        output.append(
            {
                "path": display_labels.get(path, path.replace("historical_", "hist ")),
                "path_id": path,
                "path_class": path_class,
                "competitor": MODEL_LABELS[competitor],
                "competitor_id": competitor,
                "median_delta_vs_curve": statistics.median(values),
                "win_fraction_vs_curve": sum(value > 0 for value in values)
                / len(values),
                "paired_cells": len(values),
            }
        )
    return output


def build() -> tuple[dict, dict]:
    summary = json.loads((RESULTS / "summary.json").read_text())
    native = require_complete_native_summary()
    final_evidence = require_final_report_audit(native, root=ROOT)
    correction_path = RESULTS / "sealed_accounting_summary_correction.json"
    sealed_correction = (
        json.loads(correction_path.read_text()) if correction_path.is_file() else None
    )
    generated_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    search_promotion_evidence = final_evidence["search_and_promotion"]
    sealed_evidence = final_evidence["sealed_validation"]
    native_canonical_counts = expected_native_counts(
        search_promotion_evidence
    )
    matching = load_csv("parameter_matching.csv")
    matching_required_fields = {
        "basis",
        "model",
        "parameter_1",
        "parameter_2",
        "parameter_1_domain_min",
        "parameter_1_domain_max",
        "parameter_2_domain_min",
        "parameter_2_domain_max",
        "parameter_1_at_domain_boundary",
        "parameter_2_at_domain_boundary",
        "iso_boundary_censored",
    }
    if not matching:
        raise RuntimeError("parameter_matching.csv is empty")
    missing_matching_fields = sorted(
        matching_required_fields - set(matching[0])
    )
    if missing_matching_fields:
        raise RuntimeError(
            "parameter_matching.csv predates the domain-censoring contract; "
            "regenerate benchmark outputs. Missing fields: "
            + ", ".join(missing_matching_fields)
        )
    pareto = load_csv("pareto.csv")
    shape = load_csv("shape.csv")
    pairs = load_csv("paired_differences.csv")
    safety = load_csv("safety.csv")
    routing = load_csv("routing.csv")
    sharp_roots = load_csv("sharp_root_evidence.csv")
    same_parameter = load_csv("same_parameter_deltas.csv")
    hyperbolic_p4 = load_csv("hyperbolic_p4_comparison.csv")
    native_held_out = load_csv("native_held_out.csv")
    native_profile_selection = load_csv("native_profile_selection.csv")
    native_neighborhood = load_csv("native_neighborhood.csv")
    display_pareto = displayed_pareto_rows(pareto)

    equal_execution = {
        model: matching_row(matching, "equal_execution", model)
        for model in RANKED_MODELS
    }
    equal_risk = {
        model: matching_row(matching, "equal_jump_risk", model)
        for model in RANKED_MODELS
    }
    domain_fields = (
        "parameter_1_domain_min",
        "parameter_1_domain_max",
        "parameter_2_domain_min",
        "parameter_2_domain_max",
    )
    parameter_domains: dict[str, tuple[float, float, float, float]] = {}
    for model in RANKED_MODELS:
        execution_domain = tuple(
            float(equal_execution[model][field]) for field in domain_fields
        )
        risk_domain = tuple(
            float(equal_risk[model][field]) for field in domain_fields
        )
        if execution_domain != risk_domain:
            raise RuntimeError(
                f"{model} declares different parameter domains across iso bases"
            )
        if not (
            execution_domain[0] < execution_domain[1]
            and execution_domain[2] < execution_domain[3]
        ):
            raise RuntimeError(f"{model} has an invalid declared parameter domain")
        parameter_domains[model] = execution_domain

    expected_parameter_domains = {
        "curve": (0.1, 1_000.0, 1e-8, 0.199),
        "current_dh2": (2.0, 2_000.0, 1e-6, 0.5),
    }
    for model, expected_domain in expected_parameter_domains.items():
        if parameter_domains[model] != expected_domain:
            raise RuntimeError(
                f"{model} parameter domain {parameter_domains[model]!r} does "
                f"not match the report contract {expected_domain!r}"
            )

    def parameter_domain_text(model: str) -> str:
        first_min, first_max, second_min, second_max = parameter_domains[model]
        if model == "curve":
            return (
                f"paper A = {domain_number(first_min)}…{domain_number(first_max)}; "
                f"gamma = {domain_number(second_min)}…{domain_number(second_max)}"
            )
        if model == "current_dh2":
            return (
                f"peak_depth = {domain_number(first_min)}…{domain_number(first_max)}; "
                f"fade_scale = {domain_number(second_min)}…{domain_number(second_max)}"
            )
        raise RuntimeError(f"no report parameter-domain label for {model!r}")

    declared_domain_summary = "; ".join(
        f"**{MODEL_LABELS[model]}:** {parameter_domain_text(model)}"
        for model in RANKED_MODELS
    )
    current_pairs = summary["controlled_paths"]["paired_vs_curve"][
        "current_dh2"
    ]
    current_synthetic = current_pairs["synthetic"]
    current_historical = current_pairs["historical"]
    sharp_witness = next(
        row
        for row in same_parameter
        if float(row["peak_depth"]) == 323.0
        and float(row["fade_scale"]) == 0.5
    )
    p4_equal_execution = {
        row["model"]: row
        for row in hyperbolic_p4
        if row["basis"] == "equal_execution"
    }
    p4_equal_risk = {
        row["model"]: row
        for row in hyperbolic_p4
        if row["basis"] == "equal_jump_risk"
    }
    if set(p4_equal_execution) != {"current_dh2", "hyperbolic_p4_dh2"}:
        raise RuntimeError("incomplete p4 equal-execution comparison")
    if set(p4_equal_risk) != {"current_dh2", "hyperbolic_p4_dh2"}:
        raise RuntimeError("incomplete p4 equal-jump-risk comparison")
    p4_jump_delta_fraction = float(
        p4_equal_execution["hyperbolic_p4_dh2"][
            "jump_loss_delta_vs_legacy_fraction"
        ]
    )
    p4_execution_delta_fraction = float(
        p4_equal_risk["hyperbolic_p4_dh2"][
            "execution_delta_vs_legacy_fraction"
        ]
    )
    p4_economic_decision = classify_p4_economics(
        p4_jump_delta_fraction,
        p4_execution_delta_fraction,
    )
    if any(
        row["decision"] != p4_economic_decision
        for row in hyperbolic_p4
        if row["model"] == "hyperbolic_p4_dh2"
    ):
        raise RuntimeError("p4 CSV row decision disagrees with scalar deltas")
    p4_summary = summary["sharper_fade"][
        "d_independent_hyperbolic_exponent_4"
    ]
    if p4_summary["decision"] != p4_economic_decision:
        raise RuntimeError(
            "summary p4 economic decision disagrees with the paired scalar deltas"
        )
    for field, observed in (
        (
            "jump_loss_delta_at_equal_execution_fraction",
            p4_jump_delta_fraction,
        ),
        (
            "execution_delta_at_equal_jump_risk_fraction",
            p4_execution_delta_fraction,
        ),
    ):
        if not math.isclose(
            float(p4_summary[field]),
            observed,
            rel_tol=1e-12,
            abs_tol=1e-12,
        ):
            raise RuntimeError(f"summary p4 field {field} disagrees with CSV")
    p4_safety_row = next(
        row for row in safety if row["model"] == "hyperbolic_p4_dh2"
    )
    p4_max_scanned_roots = int(p4_safety_row["max_invariant_roots"])
    if int(p4_summary["invariant_roots_max_in_scanned_states"]) != p4_max_scanned_roots:
        raise RuntimeError("summary p4 root count disagrees with safety.csv")
    naive_summary = summary["sharper_fade"][
        "naive_d_dependent_exponent_2"
    ]
    naive_multiple_roots = len(sharp_roots) > 1
    expected_naive_decision = (
        "hard_reject" if naive_multiple_roots else "no_hard_reject"
    )
    if naive_summary["decision"] != expected_naive_decision:
        raise RuntimeError("summary sharp-DH2 decision disagrees with root evidence")
    expected_p4_production_decision = p4_production_decision(
        naive_multiple_roots=naive_multiple_roots,
        p4_economic_decision=p4_economic_decision,
        p4_max_scanned_roots=p4_max_scanned_roots,
        p4_rust_validated=bool(p4_summary["rust_validated"]),
    )
    reported_p4_production_decision = summary["sharper_fade"][
        "production_decision"
    ]
    if reported_p4_production_decision != expected_p4_production_decision:
        raise RuntimeError(
            "summary sharper-fade production decision disagrees with measured "
            "safety and scalar economics"
        )

    native_decision = native["decision"]
    fee_adjusted_winner = native_decision[
        "fee_adjusted_universal_native_winner"
    ]
    gross_outcome = native_decision[
        "gross_outcome_for_fee_adjusted_selected_profiles"
    ]
    lp_return_leader = native_decision[
        "locked_profile_lp_return_leader_at_equal_invariant_execution_index"
    ]
    neighborhood_eligibility = native_decision[
        "neighborhood_service_eligibility"
    ]
    fee_assumptions = native_decision["fee_assumptions"]
    selected_profiles = native["profile_selection"]
    selection_policy = native["profile_selection_policy"]
    dusk_selection_policy = native["dusk_product_selection_policy"]
    cpmm_selection_policy = native["cpmm_product_selection_policy"]
    common_service_relaxation = float(
        selection_policy["common_service_relaxation"]
    )
    original_service_pass = {
        system: bool(profile["original_service_envelope_pass"])
        for system, profile in selected_profiles.items()
    }
    service_selection_sentence = (
        "The original route-level training envelope passed for both concentrated "
        "profiles—Curve and Dusk; their comparative lock needed no common "
        "relaxation."
        if common_service_relaxation <= 1e-12
        else (
            "The original route-level training envelope was infeasible for the "
            "concentrated pair. The smallest common normalized widening was "
            f"{percent(common_service_relaxation)} (`rho = "
            f"{common_service_relaxation:.3f}`). Original-envelope pass: Curve "
            f"**{'yes' if original_service_pass['curve_native_v2_1'] else 'no'}**, "
            f"Dusk **{'yes' if original_service_pass['dusk_native'] else 'no'}**. "
            "Because `rho > 1`, bounded fraction guardrails cease to bind and "
            "cost ceilings are widened by more than 2×; this shared rho governs "
            "only the fair Curve-versus-DH2 profile lock, not Dusk product readiness."
        )
    )
    dusk_service_relaxation = float(dusk_selection_policy["service_relaxation"])
    dusk_product_ready = bool(
        dusk_selection_policy["product_ready_on_training_service"]
    )
    dusk_selection_sentence = (
        "The selected Dusk profile independently passes every original fixed- "
        "and elastic-route readiness guardrail at `rho = 0`; Curve's comparative "
        "rho does not affect that product result."
        if dusk_product_ready
        else (
            "The selected Dusk profile independently fails its original product-"
            "readiness envelope. Its own minimum normalized widening is "
            f"{percent(dusk_service_relaxation)} (`rho = "
            f"{dusk_service_relaxation:.3f}`). Curve's comparative rho neither "
            "blocks nor rescues this Dusk-only gate, so the profile is not "
            "product-ready on synthetic training."
        )
    )
    dusk_readiness_headline = (
        "**Dusk synthetic product-readiness gate: pass.** The selected DH2 "
        "profile clears its own original service envelope at `rho = 0`. This "
        "is necessary but not sufficient for a deployment recommendation; "
        "development-history economics, sealed confirmation, safety, and integration evidence still apply."
        if dusk_product_ready
        else (
            "**Dusk synthetic product-readiness gate: fail.** The selected DH2 "
            f"profile requires its own `rho = {dusk_service_relaxation:.3f}`; "
            "the common Curve–DH2 comparison relaxation cannot make it ready."
        )
    )
    dusk_next_action = (
        "preserve the selected Dusk profile's independent `rho = 0` service "
        "result while completing development-history, sealed, safety, and "
        "implementation gates"
        if dusk_product_ready
        else (
            "retune or redesign the Dusk fee/controller profile until its own "
            "fixed and elastic route-service envelope passes with `rho = 0`"
        )
    )
    cpmm_service_relaxation = float(
        cpmm_selection_policy["service_relaxation"]
    )
    cpmm_product_ready = bool(
        cpmm_selection_policy["product_ready_on_training_service"]
    )
    cpmm_selection_sentence = (
        "The independently selected CPMM passes its original route-service "
        "envelope and is product-ready on this training gate."
        if cpmm_product_ready
        else (
            "The independently selected CPMM also fails its original route-service "
            "envelope. Its separate normalized widening is "
            f"{percent(cpmm_service_relaxation)} (`rho = "
            f"{cpmm_service_relaxation:.3f}`). It remains an economic comparator, "
            "but it is not eligible for a ship or product-ready recommendation."
        )
    )
    guardrail_labels = {
        "median_captured_volume_fraction": "Median captured volume",
        "median_selected_execution_cost_bps": (
            "Median selected-order execution cost (finite-size service)"
        ),
        "p90_selected_execution_cost_bps": (
            "p90 selected-order execution cost (finite-size service)"
        ),
        "median_marginal_tracking_stale_fraction": (
            "Median marginal-price tracking stale time"
        ),
        "worst_regime_median_capture_fraction": "Worst-flow median capture",
        "worst_regime_p10_capture_fraction": "Worst-flow p10 capture",
        "worst_regime_median_marginal_tracking_stale_fraction": (
            "Worst-flow median marginal-price tracking stale time"
        ),
        "worst_regime_p90_selected_execution_cost_bps": (
            "Worst-flow p90 selected-order execution cost"
        ),
    }
    guardrail_lines = [
        "| Per-route synthetic guardrail | Original threshold |",
        "|---|---:|",
    ]
    for field, rule in selection_policy[
        "base_service_guardrails_by_route"
    ].items():
        kind = str(rule["kind"])
        threshold = float(rule["threshold"])
        comparator = "≥" if kind == "fraction_min" else "≤"
        rendered = (
            percent(threshold)
            if kind.startswith("fraction")
            else bps(threshold)
        )
        guardrail_lines.append(
            f"| {guardrail_labels.get(field, field)} | {comparator} {rendered} |"
        )
    guardrail_markdown = "\n".join(guardrail_lines)
    selected_profile_details = {
        row["system"]: row
        for row in native_profile_selection
        if row["selected_for_held_out"] == "True"
    }
    selected_profile_rows = {
        system: row["profile_id"]
        for system, row in selected_profile_details.items()
    }
    expected_profile_rows = {
        system: str(profile["profile_id"])
        for system, profile in selected_profiles.items()
    }
    if selected_profile_rows != expected_profile_rows:
        raise RuntimeError(
            "native profile-selection CSV disagrees with native_summary.json: "
            f"csv={selected_profile_rows}, summary={expected_profile_rows}"
        )
    selected_training_service_lines = [
        "| Selected training profile | Original pass | Elastic capture | Fixed p90 selected-order cost | Elastic p90 selected-order cost | Fixed marginal stale | Elastic marginal stale | Fixed depth degradation | Elastic depth degradation |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    selected_training_labels = {
        "curve_native_v2_1": "Curve",
        "dusk_native": "Current DH2",
        "dusk_cpmm": "Dusk CPMM",
    }
    for system in ("curve_native_v2_1", "dusk_native", "dusk_cpmm"):
        row = selected_profile_details[system]
        selected_training_service_lines.append(
            "| "
            + selected_training_labels[system]
            + " | "
            + ("yes" if row["original_service_envelope_pass"] == "True" else "no")
            + " | "
            + percent(float(row["elastic_median_captured_volume_fraction"]))
            + " | "
            + bps(float(row["fixed_p90_selected_execution_cost_bps"]))
            + " | "
            + bps(float(row["elastic_p90_selected_execution_cost_bps"]))
            + " | "
            + percent(
                float(row["fixed_median_marginal_tracking_stale_fraction"])
            )
            + " | "
            + percent(
                float(row["elastic_median_marginal_tracking_stale_fraction"])
            )
            + " | "
            + percent(float(row["fixed_median_depth_degradation_fraction"]))
            + " | "
            + percent(float(row["elastic_median_depth_degradation_fraction"]))
            + " |"
        )
    selected_training_service_markdown = "\n".join(
        selected_training_service_lines
    )
    selected_training_service_bullets = "\n".join(
        (
            f"- **{selected_training_labels[system]}:** original pass "
            f"**{'yes' if selected_profile_details[system]['original_service_envelope_pass'] == 'True' else 'no'}**; "
            f"elastic capture {percent(float(selected_profile_details[system]['elastic_median_captured_volume_fraction']))}; "
            f"p90 cost {bps(float(selected_profile_details[system]['fixed_p90_selected_execution_cost_bps']))} fixed / "
            f"{bps(float(selected_profile_details[system]['elastic_p90_selected_execution_cost_bps']))} elastic; "
            f"marginal-tracking stale {percent(float(selected_profile_details[system]['fixed_median_marginal_tracking_stale_fraction']))} fixed / "
            f"{percent(float(selected_profile_details[system]['elastic_median_marginal_tracking_stale_fraction']))} elastic; "
            f"1%-TVL depth degradation (diagnostic only) {percent(float(selected_profile_details[system]['fixed_median_depth_degradation_fraction']))} fixed / "
            f"{percent(float(selected_profile_details[system]['elastic_median_depth_degradation_fraction']))} elastic."
        )
        for system in ("curve_native_v2_1", "dusk_native", "dusk_cpmm")
    )
    actual_neighborhood_eligibility = {}
    for system in ("curve_native_v2_1", "dusk_native"):
        profile_ids = {
            row["profile_id"]
            for row in native_profile_selection
            if row["system"] == system
        }
        eligible_ids = {
            row["profile_id"]
            for row in native_profile_selection
            if row["system"] == system
            and row["eligible_for_lock"].lower() == "true"
        }
        system_rows = [
            row for row in native_neighborhood if row["system"] == system
        ]
        eligible_rows = [
            row for row in system_rows if row["profile_id"] in eligible_ids
        ]
        actual_neighborhood_eligibility[system] = {
            "total_profiles": len(profile_ids),
            "eligible_profiles": len(eligible_ids),
            "excluded_profiles": len(profile_ids) - len(eligible_ids),
            "total_rows": len(system_rows),
            "eligible_rows": len(eligible_rows),
            "excluded_rows": len(system_rows) - len(eligible_rows),
            "selected_profile_eligible": (
                expected_profile_rows[system] in eligible_ids
            ),
        }
    if actual_neighborhood_eligibility != neighborhood_eligibility:
        raise RuntimeError(
            "native neighborhood eligibility disagrees with profile diagnostics: "
            f"actual={actual_neighborhood_eligibility}, "
            f"summary={neighborhood_eligibility}"
        )
    native_routes = [
        {
            "routing_mode": routing_mode,
            "gross_return_delta_bps": float(
                native["held_out"][routing_mode][
                    "median_dusk_minus_curve_gross_return_bps"
                ]
            ),
            "fee_adjusted_return_delta_bps": float(
                native["held_out"][routing_mode][
                    "median_dusk_minus_curve_return_after_protocol_fee_bps"
                ]
            ),
            "p10_fee_adjusted_return_delta_bps": float(
                native["held_out"][routing_mode][
                    "p10_dusk_minus_curve_return_after_protocol_fee_bps"
                ]
            ),
            "p90_fee_adjusted_return_delta_bps": float(
                native["held_out"][routing_mode][
                    "p90_dusk_minus_curve_return_after_protocol_fee_bps"
                ]
            ),
            "cell_win_fraction": float(
                native["held_out"][routing_mode]["cell_win_fraction"]
            ),
            "window_median_win_fraction": float(
                native["held_out"][routing_mode][
                    "window_median_win_fraction"
                ]
            ),
            "execution_cost_delta_bps": float(
                native["held_out"][routing_mode][
                    "median_dusk_minus_curve_selected_execution_cost_bps"
                ]
            ),
            "capture_delta_fraction": float(
                native["held_out"][routing_mode][
                    "median_dusk_minus_curve_captured_volume_fraction"
                ]
            ),
            "depth_degradation_delta_fraction": float(
                native["held_out"][routing_mode][
                    "median_dusk_minus_curve_depth_degradation_fraction"
                ]
            ),
            "marginal_tracking_stale_delta_fraction": float(
                native["held_out"][routing_mode][
                    "median_dusk_minus_curve_marginal_tracking_stale_fraction"
                ]
            ),
            "paired_cells": int(
                native["held_out"][routing_mode]["paired_cells"]
            ),
        }
        for routing_mode in ("fixed", "elastic")
    ]
    route_by_mode = {
        str(row["routing_mode"]): row for row in native_routes
    }
    def route_leader_sentence(routing_mode: str) -> str:
        delta = float(
            route_by_mode[routing_mode]["fee_adjusted_return_delta_bps"]
        )
        if abs(delta) <= 1e-12:
            return (
                f"{routing_mode.title()} routing's median fee-adjusted result "
                "is tied to numerical precision."
            )
        return (
            f"{routing_mode.title()} routing's median fee-adjusted result "
            f"favors {'Dusk' if delta > 0 else 'Curve'} by "
            f"{abs(delta):.2f} bps per 12 hours."
        )

    native_descriptive_route_text = " ".join(
        route_leader_sentence(routing_mode)
        for routing_mode in ("fixed", "elastic")
    )
    lp_descriptive_leader_id = lp_return_leader["descriptive_leader"]
    lp_material_leader_id = lp_return_leader[
        "material_same_direction_both_routes_leader"
    ]
    lp_median_delta = float(
        lp_return_leader["median_dusk_minus_curve_return_bps"]
    )
    lp_route_medians = {
        str(route): float(value)
        for route, value in lp_return_leader[
            "route_medians_dusk_minus_curve_bps"
        ].items()
    }
    if lp_descriptive_leader_id is None:
        lp_return_leader_headline = (
            "The all-cell median LP-net return is tied to numerical precision."
        )
    else:
        lp_return_leader_headline = (
            "The locked-profile LP-return-only result at the primary "
            "equal-execution invariant match favors "
            f"**{native_winner_text(lp_descriptive_leader_id)}** by "
            f"**{abs(lp_median_delta):.2f} bps per 12 hours** at the all-cell median."
        )
    if lp_material_leader_id is None:
        lp_return_materiality_text = (
            "No system clears the stricter 1-bp, same-direction materiality "
            "threshold in both routes."
        )
    else:
        lp_return_materiality_text = (
            f"**{native_winner_text(lp_material_leader_id)}** also clears the "
            "stricter 1-bp, same-direction materiality threshold in both routes."
        )
    lp_return_leader_text = (
        f"{lp_return_leader_headline} {lp_return_materiality_text} "
        f"Fixed and elastic route medians are {signed_bps(lp_route_medians['fixed'])} "
        f"and {signed_bps(lp_route_medians['elastic'])}; positive values favor Dusk. "
        "This ranks LP return only. Execution cost, captured flow, tracking, and "
        "the stricter universal gate remain separate."
    )
    native_lp_leader_rows = [
        {
            "descriptive_leader": native_winner_text(lp_descriptive_leader_id),
            "material_both_routes_leader": native_winner_text(
                lp_material_leader_id
            ),
            "median_dusk_minus_curve_return_bps": lp_median_delta,
            "p10_dusk_minus_curve_return_bps": float(
                lp_return_leader["p10_dusk_minus_curve_return_bps"]
            ),
            "p90_dusk_minus_curve_return_bps": float(
                lp_return_leader["p90_dusk_minus_curve_return_bps"]
            ),
            "cell_win_fraction": float(lp_return_leader["cell_win_fraction"]),
            "fixed_route_median_dusk_minus_curve_bps": lp_route_medians["fixed"],
            "elastic_route_median_dusk_minus_curve_bps": lp_route_medians[
                "elastic"
            ],
            "median_dusk_minus_curve_selected_execution_cost_bps": float(
                lp_return_leader[
                    "median_dusk_minus_curve_selected_execution_cost_bps"
                ]
            ),
            "median_dusk_minus_curve_capture_fraction": float(
                lp_return_leader["median_dusk_minus_curve_capture_fraction"]
            ),
            "median_dusk_minus_curve_marginal_tracking_stale_fraction": float(
                lp_return_leader[
                    "median_dusk_minus_curve_marginal_tracking_stale_fraction"
                ]
            ),
        }
    ]
    gross_route_sentences = []
    for routing_mode in ("fixed", "elastic"):
        delta = float(route_by_mode[routing_mode]["gross_return_delta_bps"])
        if abs(delta) <= 1e-12:
            result = "is tied to numerical precision"
        else:
            result = (
                f"favors {'Dusk' if delta > 0 else 'Curve'} by "
                f"{abs(delta):.2f} bps per 12 hours"
            )
        gross_route_sentences.append(f"{routing_mode.title()} routing {result}")
    gross_outcome_text = (
        "For the same profiles selected on fee-adjusted synthetic return, the "
        "gross development-history outcome is diagnostic only: "
        + "; ".join(gross_route_sentences)
        + ". It is not a gross universal-winner claim."
    )
    neighborhood_eligibility_text = "; ".join(
        (
            f"{NATIVE_SYSTEM_LABELS[system]}: "
            f"{int(counts['eligible_profiles'])}/{int(counts['total_profiles'])} "
            "top-ranked policies and "
            f"{int(counts['eligible_rows'])}/{int(counts['total_rows'])} rows eligible"
        )
        for system, counts in neighborhood_eligibility.items()
    )
    native_route_chart = [
        {
            "routing_mode": row["routing_mode"],
            "economics_view": view,
            "economics_view_short": (
                "Gross" if field == "gross_return_delta_bps" else "LP net"
            ),
            "dusk_minus_curve_lp_return_bps": row[field],
            "paired_cells": row["paired_cells"],
            "execution_cost_delta_bps": row["execution_cost_delta_bps"],
            "capture_delta_fraction": row["capture_delta_fraction"],
            "marginal_tracking_stale_delta_fraction": row[
                "marginal_tracking_stale_delta_fraction"
            ],
            "depth_degradation_delta_fraction": row[
                "depth_degradation_delta_fraction"
            ],
        }
        for row in native_routes
        for view, field in (
            (
                "Gross outcome (fee-adjusted-selected profiles)",
                "gross_return_delta_bps",
            ),
            ("LP net after revenue liabilities", "fee_adjusted_return_delta_bps"),
        )
    ]
    native_flow_rows = []
    for key, metrics in native_decision["fee_adjusted"]["dusk"][
        "by_flow_and_route"
    ].items():
        flow_regime, routing_mode = str(key).rsplit(":", 1)
        native_flow_rows.append(
            {
                "flow_regime": flow_regime,
                "routing_mode": routing_mode,
                "flow_route": (
                    f"{flow_regime.replace('_', ' ')} · {routing_mode}"
                ),
                **{name: float(value) for name, value in metrics.items()},
            }
        )
    native_flow_rows.sort(
        key=lambda row: (str(row["flow_regime"]), str(row["routing_mode"]))
    )
    native_basis_rows = []
    for basis, views in native["matching_basis_sensitivity"].items():
        for routing_mode, metrics in views["by_route_fee_adjusted"].items():
            native_basis_rows.append(
                {
                    "matching_basis": basis,
                    "routing_mode": routing_mode,
                    **{name: float(value) for name, value in metrics.items()},
                }
            )
    native_basis_rows.sort(
        key=lambda row: (
            str(row["matching_basis"]),
            str(row["routing_mode"]),
        )
    )
    volume_sensitivity = native["locked_profile_volume_sensitivity"]
    native_volume_rows = []
    for field, comparison in (
        ("dusk_vs_curve_by_volume_flow_and_route", "Dusk − Curve"),
        ("dusk_vs_cpmm_by_volume_flow_and_route", "Dusk − CPMM"),
        ("curve_vs_cpmm_by_volume_flow_and_route", "Curve − CPMM"),
    ):
        grouped_volume_metrics: dict[float, list[dict[str, float]]] = {}
        for key, metrics in volume_sensitivity[field].items():
            volume_ratio_text, _flow_template, _routing_mode = str(key).split(":")
            grouped_volume_metrics.setdefault(float(volume_ratio_text), []).append(
                {name: float(value) for name, value in metrics.items()}
            )
        for volume_ratio, metrics_rows in grouped_volume_metrics.items():
            native_volume_rows.append(
                {
                    "comparison": comparison,
                    "daily_volume_pool_ratio": volume_ratio,
                    "daily_volume_display": percent(volume_ratio, 0),
                    "flow_route_cells": len(metrics_rows),
                    "median_return_advantage_bps": statistics.median(
                        row["median_return_advantage_bps"]
                        for row in metrics_rows
                    ),
                    "worst_bottom_decile_mean_return_advantage_bps": min(
                        row["bottom_decile_mean_return_advantage_bps"]
                        for row in metrics_rows
                    ),
                    "worst_p90_execution_regression_bps": max(
                        row["p90_execution_regression_bps"]
                        for row in metrics_rows
                    ),
                    "worst_p10_capture_advantage_fraction": min(
                        row["p10_capture_advantage_fraction"]
                        for row in metrics_rows
                    ),
                    "worst_p90_marginal_tracking_stale_regression_fraction": max(
                        row["p90_marginal_tracking_stale_regression_fraction"]
                        for row in metrics_rows
                    ),
                }
            )
    native_volume_rows.sort(
        key=lambda row: (
            str(row["comparison"]),
            float(row["daily_volume_pool_ratio"]),
        )
    )
    expected_volume_summary_rows = 3 * len(
        volume_sensitivity["daily_volume_pool_ratios"]
    )
    if (
        len(native_volume_rows) != expected_volume_summary_rows
        or any(int(row["flow_route_cells"]) != 4 for row in native_volume_rows)
    ):
        raise RuntimeError(
            "locked-profile volume sensitivity is incomplete: "
            f"expected {expected_volume_summary_rows} four-cell summaries, "
            f"observed {len(native_volume_rows)}"
        )
    native_fee_split_rows = []
    for key, metrics in native["dusk_base_fee_split_sensitivity"].items():
        share_bps, routing_mode = str(key).split(":", 1)
        native_fee_split_rows.append(
            {
                "dusk_base_fee_revenue_share_bps": int(share_bps),
                "base_fee_share": f"{int(share_bps) / 100:.0f}%",
                "routing_mode": routing_mode,
                "share_route": f"{int(share_bps) / 100:.0f}% · {routing_mode}",
                "median_return_delta_bps": float(
                    metrics["median_dusk_minus_curve_net_lp_return_bps"]
                ),
                "p10_return_delta_bps": float(
                    metrics["p10_dusk_minus_curve_net_lp_return_bps"]
                ),
                "cell_win_fraction": float(metrics["cell_win_fraction"]),
                "cells": int(metrics["cells"]),
            }
        )
    native_fee_split_rows.sort(
        key=lambda row: (
            int(row["dusk_base_fee_revenue_share_bps"]),
            str(row["routing_mode"]),
        )
    )
    native_controller_rows = [
        {
            "system": NATIVE_SYSTEM_LABELS[system],
            "system_id": system,
            **{name: float(value) for name, value in metrics.items()},
        }
        for system, metrics in native["controller_counterfactual"].items()
    ]
    native_controller_by_system = {
        str(row["system_id"]): row for row in native_controller_rows
    }
    cpmm_metric_fields = (
        "median_current_dh2_minus_cpmm_lp_return_bps",
        "current_dh2_win_fraction",
        "median_current_dh2_minus_cpmm_execution_cost_bps",
        "median_current_dh2_minus_cpmm_capture_fraction",
        "median_current_dh2_minus_cpmm_marginal_tracking_stale_fraction",
    )

    def cpmm_report_row(summary_key: str, comparison: str) -> dict[str, object]:
        values = native[summary_key]
        return {
            "comparison": comparison,
            "comparison_scope": (
                "independently selected on synthetic training under the same "
                "base route-service definitions but with a separate minimum "
                "relaxation; profile-blind development-history product comparator"
                if summary_key == "cpmm_product_fallback"
                else str(values["comparison_scope"])
            ),
            "control_profile_id": str(values["control_profile_id"]),
            "participates_in_winner_gate": bool(
                values["participates_in_winner_gate"]
            ),
            "selection_service_relaxation": (
                float(values["service_relaxation"])
                if summary_key == "cpmm_product_fallback"
                else None
            ),
            "product_recommendation_status": (
                "ready"
                if summary_key == "cpmm_product_fallback"
                and bool(values["product_ready_on_training_service"])
                else "not ready"
                if summary_key == "cpmm_product_fallback"
                else "not applicable"
            ),
            **{field: float(values[field]) for field in cpmm_metric_fields},
        }

    native_cpmm_rows = [
        cpmm_report_row(
            "cpmm_product_fallback",
            "Current DH2 minus independently selected CPMM",
        ),
        cpmm_report_row(
            "cpmm_matched_policy_ablation",
            "Current DH2 minus matched-policy CPMM",
        ),
    ]
    native_service_policy_rows = [
        {
            "policy": "Curve–DH2 comparative lock",
            "rho": common_service_relaxation,
            "original_envelope_pass": bool(
                selection_policy["comparison_pair_ready_on_training_service"]
            ),
            "selected_profile": "paired family lock",
            "product_readiness_role": (
                "Fair profile comparison only; cannot block or rescue Dusk"
            ),
        },
        {
            "policy": "Dusk product readiness",
            "rho": dusk_service_relaxation,
            "original_envelope_pass": dusk_product_ready,
            "selected_profile": str(
                dusk_selection_policy["selected_profile_id"]
            ),
            "product_readiness_role": (
                "Authoritative synthetic readiness gate for Dusk"
            ),
        },
        {
            "policy": "CPMM fallback qualification",
            "rho": cpmm_service_relaxation,
            "original_envelope_pass": cpmm_product_ready,
            "selected_profile": str(
                cpmm_selection_policy["selected_profile_id"]
            ),
            "product_readiness_role": (
                "Qualifies CPMM only; cannot change Curve–DH2 or Dusk readiness"
            ),
        },
    ]
    native_predicate_rows = []
    for economics_view, candidates in (
        ("Fee-adjusted LP universal gate", native_decision["fee_adjusted"]),
    ):
        for candidate, result in candidates.items():
            for predicate, passed in result["predicates"].items():
                native_predicate_rows.append(
                    {
                        "economics_view": economics_view,
                        "candidate": NATIVE_SYSTEM_LABELS[
                            "dusk_native"
                            if candidate == "dusk"
                            else "curve_native_v2_1"
                        ],
                        "predicate": predicate.replace(
                            "neighborhood", "top ranked policy shortlist"
                        ).replace("_", " "),
                        "passed": bool(passed),
                    }
                )
    native_history_windows = len(
        {row["path"] for row in native_held_out}
    )
    native_flow_regimes = sorted(
        {row["flow_regime"] for row in native_held_out}
    )
    native_arrival_rates = {
        regime: sorted(
            {
                float(row["mean_arrivals_per_minute"])
                for row in native_held_out
                if row["flow_regime"] == regime
            }
        )
        for regime in native_flow_regimes
    }
    if any(len(values) != 1 for values in native_arrival_rates.values()):
        raise RuntimeError("native flow regime has an ambiguous arrival rate")
    native_arrival_text = ", ".join(
        f"{regime}={values[0]:g}/min"
        for regime, values in native_arrival_rates.items()
    )
    fee_decision_text = native_decision_sentence(
        winner=fee_adjusted_winner,
        view="LP-net fee-adjusted",
    )
    curve_controller = native_controller_by_system["curve_native_v2_1"]
    dusk_controller = native_controller_by_system["dusk_native"]
    cpmm_product = native_cpmm_rows[0]
    cpmm_ablation = native_cpmm_rows[1]

    curve_jump = float(equal_execution["curve"]["jump_loss_index_bps"])
    current_jump = float(
        equal_execution["current_dh2"]["jump_loss_index_bps"]
    )
    curve_execution = float(
        equal_risk["curve"]["execution_index_bps"]
    )
    current_execution = float(
        equal_risk["current_dh2"]["execution_index_bps"]
    )
    model_order = RANKED_MODELS
    jump_values = {
        "curve": curve_jump,
        "current_dh2": current_jump,
    }
    execution_values = {
        "curve": curve_execution,
        "current_dh2": current_execution,
    }
    center_impact_values = {
        model: float(equal_execution[model]["center_100bps_impact"])
        for model in model_order
    }

    def scalar_leaders(values: dict[str, float]) -> tuple[list[str], float]:
        best = min(values.values())
        leaders = [
            model
            for model in model_order
            if math.isclose(values[model], best, rel_tol=0.0, abs_tol=1e-9)
        ]
        return leaders, best

    def leader_names(leaders: list[str]) -> str:
        labels = [MODEL_LABELS[model] for model in leaders]
        return " and ".join(labels)

    def scalar_bullets(values: dict[str, float]) -> str:
        leaders, best = scalar_leaders(values)
        lines = []
        for model in model_order:
            value = values[model]
            if model in leaders:
                comparison = "numerically lowest in this sampled cross-section"
            else:
                comparison = (
                    f"{percent(value / best - 1.0, 2)} above "
                    f"{leader_names(leaders)}"
                )
            lines.append(
                f"- {MODEL_LABELS[model]}: **{bps(value)}** — {comparison}"
            )
        return "\n".join(lines)

    jump_leaders, _ = scalar_leaders(jump_values)
    execution_leaders, _ = scalar_leaders(execution_values)
    center_leaders, _ = scalar_leaders(center_impact_values)

    def iso_censored(row: dict[str, str]) -> bool:
        return csv_bool(
            row["iso_boundary_censored"],
            field="iso_boundary_censored",
        )

    def iso_status_label(row: dict[str, str]) -> str:
        return (
            "Yes — boundary-censored"
            if iso_censored(row)
            else "No — interior selected point"
        )

    def scalar_winner_boundary_sentence(
        cross_section: str,
        leaders: list[str],
        rows: dict[str, dict[str, str]],
    ) -> str:
        statuses = []
        for model in leaders:
            status = (
                "**boundary-censored**"
                if iso_censored(rows[model])
                else "not boundary-censored"
            )
            statuses.append(
                f"**{MODEL_LABELS[model]}** is {status} within "
                f"`{parameter_domain_text(model)}`"
            )
        return f"{cross_section} scalar leader status: " + "; ".join(statuses) + "."

    scalar_boundary_summary = " ".join(
        (
            scalar_winner_boundary_sentence(
                "Equal-execution LP-loss",
                jump_leaders,
                equal_execution,
            ),
            scalar_winner_boundary_sentence(
                "Equal-jump-risk execution",
                execution_leaders,
                equal_risk,
            ),
        )
    )
    censored_selected_points = [
        f"{basis_label} / {MODEL_LABELS[model]}"
        for basis_label, rows in (
            ("equal execution", equal_execution),
            ("equal jump risk", equal_risk),
        )
        for model in model_order
        if iso_censored(rows[model])
    ]
    if censored_selected_points:
        iso_censoring_scope = (
            "The boundary-cell censor marks these selected scalar points: "
            + ", ".join(censored_selected_points)
            + ". Their precise gaps and apparent direction are conditional on "
            "the declared domains; they are not global family optima."
        )
    else:
        iso_censoring_scope = (
            "None of the selected scalar points is boundary-censored, but the "
            "cross-sections remain conditioned on the declared parameter domains "
            "and do not establish global family optima."
        )
    invariant_scalar_summary = (
        "At the equal-execution scalar cross-section, "
        f"**{leader_names(jump_leaders)}** has the numerically lowest one-jump "
        "LP loss. At the equal-jump-risk scalar cross-section, "
        f"**{leader_names(execution_leaders)}** has the numerically lowest "
        "execution index. These are boundary-conditioned results from the "
        "declared sampled domains—not Pareto results or global family optima. "
        f"{scalar_boundary_summary}"
    )
    jump_bullets = scalar_bullets(jump_values)
    execution_bullets = scalar_bullets(execution_values)
    center_distribution_text = (
        "The execution index is deliberately broad; the center row shows why one "
        "scalar is insufficient. At the 100-bps center probe, "
        f"**{leader_names(center_leaders)}** is numerically lowest in this "
        "boundary-conditioned equal-execution fit. The full wings and Pareto "
        "frontier remain separate comparisons."
    )
    frontier_counts = {
        model: sum(
            row["model"] == model and bool(row["combined_pareto"])
            for row in display_pareto
        )
        for model in model_order
    }
    cpmm_frontier_count = sum(
        row["model"] == CPMM_CONTROL_MODEL and bool(row["combined_pareto"])
        for row in display_pareto
    )
    frontier_members = [
        model for model in model_order if frontier_counts[model] > 0
    ]
    if len(frontier_members) == len(model_order):
        pareto_heading = (
            "The recomputed Curve–DH2 Pareto frontier contains both ranked families"
        )
    elif frontier_members:
        pareto_heading = (
            "The recomputed Curve–DH2 Pareto frontier contains "
            + leader_names(frontier_members)
        )
    else:
        raise RuntimeError("displayed Pareto frontier contains no ranked family")
    pareto_membership_summary = (
        ", ".join(
            f"{MODEL_LABELS[model]} has {frontier_counts[model]} sampled frontier points"
            for model in model_order
        )
        + f". The zero-concentration CPMM control has {cpmm_frontier_count} "
        "sampled frontier point"
        + ("" if cpmm_frontier_count == 1 else "s")
        + ". Frontier membership means only that a family has at least one "
        "non-dominated sampled configuration; point count is not a quality score. "
        "Scalar cross-section leadership does not prove Pareto or global dominance."
    )

    def pairwise_result(
        lhs: str,
        rhs: str,
        delta: float,
        *,
        path_class: str,
    ) -> str:
        if math.isclose(delta, 0.0, rel_tol=0.0, abs_tol=1e-9):
            return (
                f"{lhs} and {rhs} tie at the {path_class} paired-cell median"
            )
        leader = lhs if delta > 0.0 else rhs
        trailer = rhs if delta > 0.0 else lhs
        return (
            f"{leader} leads {trailer} by {money(abs(delta))} at the "
            f"{path_class} paired-cell median"
        )

    controlled_pairwise_sentences = [
        pairwise_result(
            "Current DH2",
            "the CryptoSwap invariant",
            float(current_synthetic["median_paired_net_lp_delta"]),
            path_class="synthetic",
        ),
        pairwise_result(
            "Current DH2",
            "the CryptoSwap invariant",
            float(current_historical["median_paired_net_lp_delta"]),
            path_class="historical",
        ),
    ]
    controlled_pairwise_summary = ". ".join(controlled_pairwise_sentences) + "."
    controlled_scope_text = (
        "These paired medians use the common controller; they are path-dependent "
        "and do not establish a Pareto or global invariant winner."
    )
    controlled_medians = (
        float(current_synthetic["median_paired_net_lp_delta"]),
        float(current_historical["median_paired_net_lp_delta"]),
    )
    if all(value > 0.0 for value in controlled_medians):
        dusk_controlled_decision = (
            "The common-controller paired medians favor current DH2 over the "
            "CryptoSwap invariant in both synthetic and historical classes."
        )
        dusk_decision_heading = "Controlled paths favor current DH2"
    elif all(value < 0.0 for value in controlled_medians):
        dusk_controlled_decision = (
            "The common-controller paired medians favor the CryptoSwap invariant "
            "over current DH2 in both synthetic and historical classes."
        )
        dusk_decision_heading = "Controlled paths favor the CryptoSwap invariant"
    else:
        dusk_controlled_decision = (
            "The common-controller Curve-versus-current-DH2 result changes by "
            "path class, so the controlled replay does not select one invariant."
        )
        dusk_decision_heading = "Controlled paths do not select one invariant"

    def p4_delta_sentence(
        metric: str,
        current_value: float,
        p4_value: float,
        delta_fraction: float,
    ) -> str:
        if math.isclose(delta_fraction, 0.0, rel_tol=0.0, abs_tol=1e-9):
            comparison = "effectively tied at this scalar cross-section"
        elif delta_fraction > 0.0:
            comparison = (
                f"{percent(abs(delta_fraction))} higher, favoring current DH2"
            )
        else:
            comparison = (
                f"{percent(abs(delta_fraction))} lower, favoring p4"
            )
        return (
            f"- {metric}: current DH2 **{bps(current_value)}**; p4 "
            f"**{bps(p4_value)}** — {comparison}."
        )

    p4_scalar_markdown = "\n".join(
        (
            p4_delta_sentence(
                "Jump loss at equal execution",
                float(
                    p4_equal_execution["current_dh2"][
                        "jump_loss_index_bps"
                    ]
                ),
                float(
                    p4_equal_execution["hyperbolic_p4_dh2"][
                        "jump_loss_index_bps"
                    ]
                ),
                p4_jump_delta_fraction,
            ),
            p4_delta_sentence(
                "Execution index at equal jump risk",
                float(
                    p4_equal_risk["current_dh2"]["execution_index_bps"]
                ),
                float(
                    p4_equal_risk["hyperbolic_p4_dh2"][
                        "execution_index_bps"
                    ]
                ),
                p4_execution_delta_fraction,
            ),
        )
    )
    if p4_max_scanned_roots == 1:
        p4_safety_text = (
            "The D-independent p4 repair has one invariant root in every scanned "
            "state, but this remains a sampled analytical-float check rather than "
            "a fixed-point proof."
        )
    else:
        p4_safety_text = (
            "The D-independent p4 repair reaches "
            f"{p4_max_scanned_roots} invariant roots in the sampled states and "
            "therefore fails the uniqueness gate."
        )
    if p4_economic_decision == "reject_economically":
        p4_decision_heading = "D-independent p4 repair: economically rejected"
        p4_economic_text = (
            "Both like-for-like scalar objectives favor current DH2, so the p4 "
            "repair is economically rejected in this experiment."
        )
    elif p4_economic_decision == "p4_improves_scalar_cross_sections":
        p4_decision_heading = (
            "D-independent p4 repair: scalar improvement, not production-ready"
        )
        p4_economic_text = (
            "Both like-for-like scalar objectives favor p4. That makes it a "
            "research candidate, not a production recommendation; its arithmetic "
            "is still analytical float unless separately validated."
        )
    elif p4_economic_decision == "scalar_cross_section_tie":
        p4_decision_heading = "D-independent p4 repair: no material scalar gain"
        p4_economic_text = (
            "The two like-for-like scalar objectives are tied within the declared "
            "tolerance, so they do not justify replacing current DH2."
        )
    else:
        p4_decision_heading = "D-independent p4 repair: mixed scalar result"
        p4_economic_text = (
            "The two like-for-like scalar objectives point in different directions, "
            "so this experiment does not support a replacement decision."
        )
    p4_production_text = (
        f"The evidence-derived production status is "
        f"`{reported_p4_production_decision}`."
    )
    center_quotes = []
    for model in DISPLAY_MODELS:
        for trade_bps in (10, 100, 500, 1_000):
            quote_rows = [
                row
                for row in shape
                if row["model"] == model
                and row["order_type"] == "exact_in"
                and abs(float(row["log_price_distance_bps"])) < 1e-9
                and int(row["trade_bps_of_marked_tvl"]) == trade_bps
            ]
            if len(quote_rows) != 2 or {
                row["direction"] for row in quote_rows
            } != {"x_to_y", "y_to_x"}:
                raise RuntimeError(
                    f"missing symmetric center quote rows for {model} at {trade_bps} bps"
                )
            costs = [float(row["execution_cost_bps"]) for row in quote_rows]
            execution_cost = statistics.mean(costs)
            input_value = 2_000_000.0 * trade_bps / 10_000.0
            directional_outputs = []
            output_by_direction = {}
            cost_by_direction = {}
            for row in quote_rows:
                cost_fraction = float(row["execution_cost_bps"]) / 10_000.0
                if row["direction"] == "x_to_y":
                    output_value = input_value * (1.0 - cost_fraction)
                else:
                    output_value = input_value / (1.0 + cost_fraction)
                directional_outputs.append(output_value)
                output_by_direction[str(row["direction"])] = output_value
                cost_by_direction[str(row["direction"])] = float(
                    row["execution_cost_bps"]
                )
            quoted_output_value = statistics.mean(directional_outputs)
            center_quotes.append(
                {
                    "model": MODEL_LABELS[model],
                    "chart_model": CHART_MODEL_LABELS[model],
                    "model_id": model,
                    "direction_count": len(quote_rows),
                    "directions": "x_to_y,y_to_x",
                    "trade_bps_of_tvl": trade_bps,
                    "input_value_usd": input_value,
                    "input_value_display": f"${input_value:,.0f}",
                    "execution_cost_bps": execution_cost,
                    "execution_cost_display": f"{execution_cost:.3f} bps",
                    "quoted_output_value_usd": quoted_output_value,
                    "quoted_output_value_display": f"${quoted_output_value:,.2f}",
                    "minimum_directional_output_value_usd": min(
                        directional_outputs
                    ),
                    "maximum_directional_output_value_usd": max(
                        directional_outputs
                    ),
                    "x_to_y_execution_cost_bps": cost_by_direction["x_to_y"],
                    "y_to_x_execution_cost_bps": cost_by_direction["y_to_x"],
                    "x_to_y_output_value_usd": output_by_direction["x_to_y"],
                    "y_to_x_output_value_usd": output_by_direction["y_to_x"],
                }
            )
    quote_lines = [
        "| Model | Exact-in size | Mean two-direction center cost | Quoted output value |",
        "|---|---:|---:|---:|",
    ]
    for row in center_quotes:
        quote_lines.append(
            f"| {row['model']} | ${row['input_value_usd']:,.0f} "
            f"({row['trade_bps_of_tvl']} bps TVL) | "
            f"{row['execution_cost_bps']:.3f} bps | "
            f"${row['quoted_output_value_usd']:,.2f} |"
        )
    center_quote_markdown = "\n".join(quote_lines)
    illustrative_sol_price = 150.0
    sol_quote_lines = [
        "| Model | Sell 133.333 SOL | Sell 20,000 USDC |",
        "|---|---:|---:|",
    ]
    for row in center_quotes:
        if int(row["trade_bps_of_tvl"]) != 100:
            continue
        sol_quote_lines.append(
            f"| {row['model']} | "
            f"{float(row['x_to_y_output_value_usd']):,.2f} USDC "
            f"({float(row['x_to_y_execution_cost_bps']):.3f} bps) | "
            f"{float(row['y_to_x_output_value_usd']) / illustrative_sol_price:,.6f} SOL "
            f"({float(row['y_to_x_execution_cost_bps']):.3f} bps) |"
        )
    sol_quote_markdown = "\n".join(sol_quote_lines)
    sol_quote_bullets = "\n".join(
        (
            f"- **{row['model']}:** sell 133.333 SOL for "
            f"{float(row['x_to_y_output_value_usd']):,.2f} USDC "
            f"({float(row['x_to_y_execution_cost_bps']):.3f} bps), or sell "
            f"20,000 USDC for "
            f"{float(row['y_to_x_output_value_usd']) / illustrative_sol_price:,.6f} SOL "
            f"({float(row['y_to_x_execution_cost_bps']):.3f} bps)."
        )
        for row in center_quotes
        if int(row["trade_bps_of_tvl"]) == 100
    )
    failed_native_predicates = [
        row for row in native_predicate_rows if not bool(row["passed"])
    ]
    if failed_native_predicates:
        failed_predicate_markdown = "\n".join(
            f"- **{row['economics_view']} / {row['candidate']}:** "
            f"{row['predicate']}"
            for row in failed_native_predicates
        )
    else:
        failed_predicate_markdown = "- None."
    canonical_count_markdown = "\n".join(
        f"| `{field}` | {int(expected):,} |"
        for field, expected in native_canonical_counts.items()
    )
    promoted = search_promotion_evidence["promotions"]
    search_lock_rows = []
    search_lock_lines = [
        "| Family | Full search | Search winner | Promoted | Canonical locked profile | Locked configuration |",
        "|---|---:|---|---:|---|---|",
    ]
    for system in ("curve_native_v2_1", "dusk_native", "dusk_cpmm"):
        row = promoted[system]
        report_row = {
            "family": NATIVE_SYSTEM_LABELS[system],
            "system": system,
            "search_profiles": int(row["search_profiles"]),
            "search_cells_per_profile": int(row["search_cells_per_profile"]),
            "search_rows": int(row["search_rows"]),
            "search_winner_profile_id": str(row["search_winner_profile_id"]),
            "promoted_profiles": int(row["promoted_profiles"]),
            "canonical_locked_profile_id": str(
                row["canonical_locked_profile_id"]
            ),
            "canonical_locked_source_rank": int(
                row["canonical_locked_source_rank"]
            ),
            "locked_configuration": compact_locked_config(row),
            "promotion_fingerprint": str(row["promotion_fingerprint"]),
        }
        search_lock_rows.append(report_row)
        search_lock_lines.append(
            f"| {report_row['family']} | "
            f"{report_row['search_profiles']} × "
            f"{report_row['search_cells_per_profile']} = "
            f"{report_row['search_rows']:,} | "
            f"`{report_row['search_winner_profile_id']}` | "
            f"{report_row['promoted_profiles']} | "
            f"`{report_row['canonical_locked_profile_id']}` "
            f"(source rank {report_row['canonical_locked_source_rank']}) | "
            f"{report_row['locked_configuration']} |"
        )
    search_lock_markdown = "\n".join(search_lock_lines)

    sealed_confirmation = sealed_evidence["confirmation"]
    sealed_confirmation_passed = bool(sealed_confirmation["confirmed"])
    sealed_confirmation_label = str(sealed_confirmation["status"]).upper()
    sealed_development_label = native_winner_text(
        native_decision["fee_adjusted_universal_native_winner"]
    )
    sealed_core_label = NATIVE_SYSTEM_LABELS.get(
        str(sealed_confirmation["sealed_core_classification"]),
        "neither system",
    )
    if sealed_confirmation["sealed_core_classification"] is None:
        sealed_core_label = "neither system"
    sealed_confirmation_text = (
        f"**Sealed confirmation: {sealed_confirmation_label}.** The locked "
        f"development classification was **{sealed_development_label}**; the "
        f"post-lock sealed core classification is **{sealed_core_label}**. "
        + (
            "They agree under the predeclared route, flow-route, and CPMM core thresholds."
            if sealed_confirmation_passed
            else (
                "They do not agree, so the development conclusion is not "
                "confirmed and must not be promoted to a production claim."
            )
        )
    )
    sealed_correction_text = ""
    if sealed_correction is not None:
        sealed_correction_text = (
            " During the evidence audit, the first sealed-summary producer was found "
            "to take the maximum signed accounting residual instead of the maximum "
            "absolute residual. The economic result and paired ledgers were unchanged; "
            f"their preserved hashes are `{sealed_correction['old_outputs']['sealed_native_results_sha256']}` "
            f"and `{sealed_correction['old_outputs']['sealed_native_paired_sha256']}`. "
            "The corrected producer is covered by a mixed-sign regression, and the "
            f"true maximum absolute residual is {float(sealed_correction['recomputed_maximum_absolute_accounting_identity_error']):.3e}, "
            f"below the {float(sealed_correction['accounting_error_limit']):.1e} limit."
        )
    sealed_route_rows = list(sealed_evidence["route_outcomes"])
    sealed_route_lines = [
        "| Route | Development Dusk − Curve LP return | Sealed Dusk − Curve LP return | Development cell wins | Sealed cell wins | Development window wins | Sealed window wins |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for row in sealed_route_rows:
        sealed_route_lines.append(
            f"| {str(row['routing_mode']).title()} | "
            f"{signed_bps(float(row['development_median_dusk_minus_curve_lp_return_bps']))} | "
            f"{signed_bps(float(row['sealed_median_dusk_minus_curve_lp_return_bps']))} | "
            f"{percent(float(row['development_cell_win_fraction']))} | "
            f"{percent(float(row['sealed_cell_win_fraction']))} | "
            f"{percent(float(row['development_window_win_fraction']))} | "
            f"{percent(float(row['sealed_window_win_fraction']))} |"
        )
    sealed_route_markdown = "\n".join(sealed_route_lines)

    report = f"""# Curve CryptoSwap vs current DH2, with CPMM control

> **Historical sealed DH2 snapshot; “current DH2” means current within this
> experiment, not the repository runtime.** This report does not rank or
> validate the present nested tail-and-band Dusk implementation.

## Executive summary

**The completed comparison has distinct scalar, controlled-path, and native answers.** {invariant_scalar_summary}

{controlled_pairwise_summary} {controlled_scope_text}

On the native-controller replay, {fee_decision_text[0].lower() + fee_decision_text[1:]}

{native_descriptive_route_text}

{lp_return_leader_text}

{sealed_confirmation_text}

{dusk_readiness_headline} The common Curve–DH2 rho remains a comparative profile-lock diagnostic, and the CPMM has its own fallback qualification. {dusk_controlled_decision} Dusk product readiness still comes only from the independent Dusk gate above.

{gross_outcome_text} Gross and fee-adjusted results answer different questions: the former describes the already fee-adjusted-selected profiles before protocol/manager revenue liabilities, while the latter is the ledger used to select profiles and test the universal claim.

## What is—and is not—being ranked

| Comparison level | What is held common | Supported conclusion |
|---|---|---|
| Invariant only | Same marginal-price states, marked TVL, trade-size/direction grid, zero fees, and no recentering; the jump test also starts from identical reserves | {invariant_scalar_summary} |
| Controlled system | Same order tape, internal EMA, Dusk fee policy, retained-surcharge budget, and funded center controller | {controlled_pairwise_summary} {controlled_scope_text} |
| Native system | Curve Twocrypto-NG v2.1 fixed-parameter fee/EMA/profit-gated price-scale controller versus current DH2 with Dusk divergence/volatility fees, retained surcharge, and funded recentering | {native_winner_text(fee_adjusted_winner).capitalize()} is the fee-adjusted universal winner under the primary equal-execution gate. The separate locked-profile LP-return leader states economic direction even when the strict universal label is null. Gross return is diagnostic only. |

The native result is a model result, not a live-volume forecast. It uses frozen historical prices and policy-blind modeled order tapes; it does not prove what a deployed pool will earn under future router flow.

At equal aggregate execution service:

{jump_bullets}

At equal one-jump LP risk:

{execution_bullets}

On paired 12-hour controlled-system paths, {controlled_pairwise_summary[0].lower() + controlled_pairwise_summary[1:]} {controlled_scope_text}

## The comparison now holds trader service or LP risk constant

Raw `A`, `gamma`, `peak_depth`, and `fade_scale` values are not comparable across invariants. The corrected benchmark uses two independent iso-comparisons:

1. **Equal execution:** identical geometric-mean exact-input quote cost across price states from 0.67× to 1.50× the center, 1–2,000-bps marked-TVL orders, and both directions.
2. **Equal jump risk:** identical mean loss versus hold after profit-maximizing zero-fee arbitrage over nine one-step price jumps.

The declared domains are read from `results/parameter_matching.csv`: {declared_domain_summary}. A boundary-censored scalar winner is only the best selected point inside that declared search and is never labeled a global family optimum.

The former “full profile match” could not match every wing: its nearest current-DH2 solution retained a worst-point impact error of {percent(float(matching_row(matching, 'nearest_full_profile', 'current_dh2')['full_profile_max_multiplicative_error']))}. The report therefore does not pretend the Curve and current-DH2 shapes are interchangeable.

| Basis | Curve | Current DH2 |
|---|---:|---:|
| Jump loss at equal execution | {curve_jump:.2f} bps | {current_jump:.2f} bps |
| Execution cost at equal jump risk | {curve_execution:.2f} bps | {current_execution:.2f} bps |
| Center impact at equal execution | {float(equal_execution['curve']['center_100bps_impact']):.3f} bps | {float(equal_execution['current_dh2']['center_100bps_impact']):.3f} bps |
| Equal-execution selection boundary-censored? | {iso_status_label(equal_execution['curve'])} | {iso_status_label(equal_execution['current_dh2'])} |
| Equal-jump-risk selection boundary-censored? | {iso_status_label(equal_risk['curve'])} | {iso_status_label(equal_risk['current_dh2'])} |

### Concrete center quotes

The following zero-fee exact-input examples use the selected equal-execution reference configurations and a balanced **$2.0M marked-TVL pool**. Cost and quoted output are computed independently in each token direction and then averaged; the inverse-price direction is not reconstructed by subtracting its cost. “Quoted output value” marks both input and output at the pre-trade center, so the difference from input is curve impact rather than a token-price forecast.

{center_quote_markdown}

At an illustrative **$150/SOL** center, the 100-bps-of-TVL row means either selling 133.333 SOL or 20,000 USDC:

{sol_quote_markdown}

{center_distribution_text}

## {pareto_heading}

{pareto_membership_summary}

This frontier is boundary- and grid-conditioned evidence, not a global proof over every continuous parameter value.

## Paired controlled-path economics are derived pair by pair

The primary path replay applies the same Dusk fee, internal EMA, retention gate, and funded center controller to each concentrated design. Each model receives the same externally denominated user order tape. Arbitrage sizes maximize actual after-fee profit and are skipped below the profit threshold.

| Competitor minus CryptoSwap invariant | Synthetic median | Synthetic wins | Historical median | Historical wins | Historical window-median wins |
|---|---:|---:|---:|---:|---:|
| Current DH2 | {money(current_synthetic['median_paired_net_lp_delta'])} | {percent(current_synthetic['win_fraction'])} | {money(current_historical['median_paired_net_lp_delta'])} | {percent(current_historical['win_fraction'])} | {percent(current_historical['window_median_win_fraction'])} |

{controlled_pairwise_summary} {controlled_scope_text}

## The corrected ledger removes the old double counting

LP wealth is:

```text
(executable base + claimable base) × external price
+ executable quote
+ claimable quote
```

Base fees leave executable reserves and remain token-denominated LP wealth. Only retained dynamic surcharge enters reserves and can create protected recentering budget. It is counted once, inside reserves.

The Dusk fee reference now mirrors the actual ordering:

- volatility decays before a trade and is updated from the successful AMM price shock;
- divergence charges only outward reserve movement;
- restorative flow pays no divergence surcharge;
- divergence has priority under the dynamic-fe cap;
- surcharge retention is binary;
- quote input, reserve credit, claimable fees, and retained principal reconcile exactly.

Protected liquidity is `Q = D / (2√center)` for the Curve and current-DH2 concentrated invariants. Changing the center with unchanged tokens has zero immediate marked-wealth cost. `Q` impairment is a controller-capacity diagnostic and is not subtracted a second time from LP return.

## Synthetic profile lock does not read development history

{service_selection_sentence}

{dusk_selection_sentence}

{cpmm_selection_sentence}

The same base guardrails apply independently to fixed and elastic routes. Hard gates—finite metrics, accounting error no greater than `1e-5`, and zero controller reads of the external price—never relax. For service only, Curve and concentrated Dusk use the smallest common normalized relaxation that admits at least one hard-valid profile from each candidate family. CPMM uses its own minimum relaxation and cannot change either concentrated profile or `comparison_pair_ready_on_training_service`. Dusk's readiness is computed separately from its selected profile's own required rho. Within every system, selection first minimizes the profile's own required rho, then maximizes worse-route LP-return utility among ties. Historical windows are not read.

The three rho values have different jobs: the **common Curve–DH2 rho** makes the comparative lock behaviorally fair; the **independent Dusk rho** alone decides Dusk's synthetic product-readiness status; and the **independent CPMM rho** qualifies the fallback comparator. Marginal-price tracking defines stale readiness. Selected-order execution cost is a finite-size service measure, while the raw 1%-TVL `depth_degradation_fraction` is shown only as a depth diagnostic and never relabeled as stale price.

{guardrail_markdown}

The selected rows show the original-envelope result and the separate depth diagnostic:

{selected_training_service_markdown}

## Symmetric concentrated searches and an independent CPMM search remove profile-budget asymmetry

Curve and concentrated Dusk each receive exactly **150 initial controller/fee profiles × 128 synthetic cells = 19,200 decision cells** before promotion. CPMM receives its own independent **150 × 128 = 19,200-cell** search under the same hard-validity-first, synthetic-only budget. The report accepts neither quick output nor a partially evaluated family.

Promotion is an explicit provenance boundary, not an implicit search rewrite. Each top-ranked policy shortlist is bound to its search summary, stage outputs, winner, and fingerprint. The canonical native selector may choose only a source-ranked promoted policy under the applicable service envelope; it cannot invent a profile outside that source-bound shortlist.

{search_lock_markdown}

The full configurations—not only display labels—are bound by the promotion snapshot, the canonical replay fingerprint, and the pre-history sealed lock. This is why the search winner and the canonical locked profile are shown separately.

## Native replay: the strict decision is generated from routes, tails, and robustness—not one median

{fee_decision_text}

{gross_outcome_text}

### Locked-profile LP-return leader at the primary equal-execution match

{lp_return_leader_text}

Positive deltas below favor Dusk. The **gross** column is an outcome for the profiles selected on fee-adjusted return; it compares LP wealth before the modeled external revenue liabilities and is not a separate universal ranking. The **fee-adjusted** column subtracts the liability accrued during the scored window while preserving liability already present at the score boundary. Execution-cost deltas are for the selected route: native venue for fixed routing and the cheaper of AMM or outside venue for elastic routing.

{native_route_markdown(native_routes)}

The selected native profiles are Curve {format_profile(selected_profiles['curve_native_v2_1'])}, Dusk {format_profile(selected_profiles['dusk_native'])}, and independently selected Dusk CPMM {format_profile(selected_profiles['dusk_cpmm'])}. {service_selection_sentence} {dusk_selection_sentence} {cpmm_selection_sentence} Within each system's eligible envelope, the lock first minimizes the profile's own required rho; only then does it maximize **worse-route** synthetic utility among ties. The utility is `0.8 × median fee-adjusted LP return + 0.2 × p10 return`, and all profiles are locked before development-history replay. Top-ranked-policy-shortlist robustness applies to the two concentrated candidates and excludes policies outside their common service envelope ({neighborhood_eligibility_text}). The table above uses {native_history_windows} profile-blind development historical windows, {len(native_flow_regimes)} modeled flow regimes, two seeds, and both routes. These windows were visible during benchmark development and are not the sealed confirmation set. The primary equal-execution acceptance rule requires both routes to pass independently and also checks bottom-decile returns, service tails, every flow-by-route cell, the eligible top-ranked policy shortlist, the independently selected Dusk CPMM product comparator, three pool scales, one-minute cadence, and locked-profile volume transfer. Alternate invariant matches are diagnostics only.

## Sealed post-lock history tests replication without reopening selection

{sealed_confirmation_text}

The development columns use the 15 profile-blind historical windows that were visible while the benchmark was being developed. The sealed columns use 15 new mechanically scheduled windows from May 2025 through May 2026, downloaded only after the Curve, DH2, and CPMM profiles, search sources, matrix, and replay-engine hashes were locked. Positive return deltas favor Dusk.

{sealed_route_markdown}

The binary confirmation applies the existing route, flow-route, and CPMM return/service thresholds to the locked profiles. It is intentionally narrower than the full native universal gate because the sealed matrix does not rerun the top-ranked policy shortlist, pool scales, volume levels, or one-minute cadence. Alternate matching bases are outside both gates and remain locked-controller diagnostics. A sealed **FAIL** blocks confirmation; it does not select a replacement, retune a parameter, or silently change `results/native_summary.json`.

## Flow-by-route tails prevent pooled medians from deciding the native winner

The next table shows Dusk-minus-Curve fee-adjusted outcomes at the decision grain. A positive return advantage helps Dusk; positive selected-order cost or marginal-tracking stale regression hurts Dusk. This prevents a benign or elastic-flow median from hiding failure under informed flow or fixed routing.

{native_flow_markdown(native_flow_rows)}

## Alternate invariant matches are locked-controller transfer diagnostics

Native results are repeated with the nearest full quote profile, equal one-jump LP risk, and the pinned v2.1 deployment invariant while keeping the controllers and fees selected on the primary equal-execution match. Those controllers are **not reoptimized** for an alternate basis. Reversals therefore diagnose transfer sensitivity but cannot grant, revoke, or reverse the universal label.

{native_basis_markdown(native_basis_rows)}

## Locked profiles are stress-tested from 5% to 400% daily volume

The next table holds each selected profile and its arrival frequency fixed while changing notional throughput. It is a transfer test, not a per-volume reoptimization. The primary equal-execution gate evaluates every underlying flow-route cell; this compact view shows the median of cell medians and the worst service/return tails at each volume.

{native_volume_markdown(native_volume_rows)}

## Revenue ownership can change LP ranking without changing a single quote

The primary fee-adjusted ledger gives Dusk's manager {float(fee_assumptions['dusk_manager_share_of_base_fee_bps']) / 100:.0f}% of the base fee, gives the Dusk dynamic surcharge entirely to LPs, and applies Curve's modeled effective admin liability of {percent(float(fee_assumptions['curve_effective_admin_liability_fraction_of_gross_xcp_profit']), 0)} of gross `xcp_profit` growth. Dusk's total base-fee share is then revalued from 0% to 100% without replaying trades because the split does not change quotes, reserves, surcharge retention, or controller decisions.

{native_fee_split_markdown(native_fee_split_rows)}

This sensitivity isolates ownership policy from AMM mechanism performance. It must not be used to claim that a revenue split itself improves quoting or reduces adverse selection.

## Recentring, the CPMM comparator, and concentration ablation are distinct

Enabling native center movement changed fee-adjusted LP return by a median {signed_bps(float(curve_controller['median_controller_on_minus_off_lp_return_bps']))} for Curve and {signed_bps(float(dusk_controller['median_controller_on_minus_off_lp_return_bps']))} for Dusk. The corresponding selected-execution-cost changes were {signed_bps(float(curve_controller['median_controller_on_minus_off_execution_cost_bps']))} and {signed_bps(float(dusk_controller['median_controller_on_minus_off_execution_cost_bps']))}; tracking-RMSE changes were {signed_bps(float(curve_controller['median_controller_on_minus_off_tracking_rmse_bps']))} and {signed_bps(float(dusk_controller['median_controller_on_minus_off_tracking_rmse_bps']))}. Fees, observations, and Dusk retention remain active in the off arm, so this isolates the economic consequence of suppressing center moves rather than disabling the whole controller. These are paired on the same development-history windows, flows, seeds, and fixed/elastic routes.

In a deterministic Curve regression witness, one same-slot event required **20 profitable fills and 19 price-scale recenters** before no profitable follow-up remained. It converged without a numerical failure. The benchmark therefore uses a Curve-local emergency ceiling of 64 fills while the shared DH2/CPMM ceiling remains 16. This witness shows that Curve's controller can reopen arbitrage after a fill; 20 is neither a claimed canonical-matrix maximum nor an on-chain execution count.

The **product comparator** selects its own Dusk fee/controller profile on the same synthetic training matrix but under a separate route-service relaxation, without reading development history or changing the Curve/DH2 lock. {cpmm_selection_sentence} Against that independently selected CPMM, current DH2 changed median LP return by {signed_bps(float(cpmm_product['median_current_dh2_minus_cpmm_lp_return_bps']))}, selected-order execution cost by {signed_bps(float(cpmm_product['median_current_dh2_minus_cpmm_execution_cost_bps']))}, captured volume by {signed_pp(float(cpmm_product['median_current_dh2_minus_cpmm_capture_fraction']))}, and marginal-tracking stale time by {signed_pp(float(cpmm_product['median_current_dh2_minus_cpmm_marginal_tracking_stale_fraction']))}. This comparison gates every universal concentrated-candidate claim even when the CPMM itself is not eligible for a product-ready recommendation.

The **matched-policy ablation** instead preserves the selected current-DH2 fee and center-controller parameters and changes only `peak_depth = fade_scale = 0`. Under that isolated concentration removal, current DH2 changed median LP return by {signed_bps(float(cpmm_ablation['median_current_dh2_minus_cpmm_lp_return_bps']))}, selected-order execution cost by {signed_bps(float(cpmm_ablation['median_current_dh2_minus_cpmm_execution_cost_bps']))}, captured volume by {signed_pp(float(cpmm_ablation['median_current_dh2_minus_cpmm_capture_fraction']))}, and marginal-tracking stale time by {signed_pp(float(cpmm_ablation['median_current_dh2_minus_cpmm_marginal_tracking_stale_fraction']))}. This arm diagnoses the marginal effect of concentration; it does not select the product comparator or enter winner predicates.

## What these results say about Dusk

**{dusk_decision_heading}.** {dusk_controlled_decision}

**Use the native decision gate for product-level claims.** {fee_decision_text} {native_descriptive_route_text} Separately, {invariant_scalar_summary[0].lower() + invariant_scalar_summary[1:]}

{dusk_readiness_headline} The locked-profile LP-return-only leader is **{native_winner_text(lp_descriptive_leader_id)}** at the primary equal-execution match; quote service and the stricter universal label remain separate. {sealed_confirmation_text} The comparative Curve–DH2 rho, Dusk product rho, and CPMM fallback rho must not be merged into one deployment label.

The supported next decision is:

1. keep the invariant-only, common-controller, and native-system conclusions separate;
2. retain current DH2 as the concentrated Dusk candidate and exact CPMM as its zero-concentration control;
3. {dusk_next_action};
4. then compare current DH2 with Curve at the chosen LP-risk and quote-surface target and rerun the native gate;
5. keep the measured locked-profile LP-return leader explicit and CPMM as the required zero-concentration comparator, without importing either comparator's rho into Dusk readiness.

## Sharper current-DH2 fade: hard rejected

The tested shape kept the same two public operator parameters—`peak_depth` and `fade_scale`—and fixed its exponent at two inside the protocol:

```text
legacy: A_eff = (peak_depth / 2) / (1 + u / fade_scale)
sharp:  A_eff = (peak_depth / 2) / (1 + (u / fade_scale)^2)
```

Both have identical center depth, and they cross at `u = fade_scale`. The sharp profile retains **more** depth before that shoulder and decays faster only after it. This is not an unqualified reduction in off-center liquidity.

The candidate is rejected before economic ranking because it violates invariant uniqueness. At `peak_depth = 323`, `fade_scale = 0.5`, `x = 6.776596755`, and `y = 0.036891674`, the same reserve state has **{len(sharp_roots)} valid invariant roots**: {', '.join(f"{float(row['invariant_d']):.9f}" for row in sharp_roots)}. A swap solver can therefore select different reserve branches for the same state.

The diagnostic same-parameter calculation at that point reports an execution-index change of {float(sharp_witness['sharp_minus_legacy_execution_bps']):+.2f} bps and a one-jump-loss change of {float(sharp_witness['sharp_minus_legacy_jump_loss_bps']):+.2f} bps. Those numbers are explicitly **non-decision-grade**: branch ambiguity invalidates the quote surface they summarize. The sharp profile is an analytical float reference only; it has not been Rust/fixed-point validated and should not be implemented.

A second experiment removed that feedback by measuring distance as `r = |x-y| / (2√xy)` and using the protocol-fixed profile `A_eff = (peak_depth / 2) / (1 + (r / fade_scale)^4)`. {p4_safety_text}

{p4_scalar_markdown}

The direct D-dependent sharper fade fails safety. {p4_economic_text} {p4_production_text}

The controlled-path result is not a product-readiness endorsement and does not establish either ranked invariant as globally superior. Current DH2's public invariant knobs are stated as `peak_depth` (additional center marginal-depth multiplier) and `fade_scale`; legacy internal `A = peak_depth / 2`. `peak_depth = 0` together with `fade_scale = 0` is exact CPMM. Fees, EMA half-life, adjustment threshold, and recenter velocity remain controller parameters, not invariant parameters.

## Scope and remaining uncertainty

The controlled-system frozen history contains five highest-range 720-minute SOLUSDT windows from one-minute Binance archives; USDT is a proxy for USDC. The native replay separately uses 15 development historical windows after burn-in and 15 post-lock sealed windows for confirmation. Synthetic and historical classes are never pooled into one winner statistic. Within a controlled-system class, paths, the benign/toxic-delayed flow regimes, and two seeds receive equal weight.

Historical prices are real; user and arbitrage arrivals are modeled. The quote-routing share is an equal-intent static tournament, not a Jupiter volume forecast. Real signed router orders, fills, latency, fees, and post-trade markouts could change the commercial conclusion.

The controlled replay uses one $2.0M pool size, one 25%-of-TVL daily volume level, one fee/EMA/recenter configuration, and two flow regimes. It does not establish controlled-system sensitivity across pool size, volume, or controller settings. The invariant economic ranking uses exact-input orders from 1 to 2,000 bps; exact-output remains a safety/shape diagnostic rather than a ranking weight. The native system separately tests locked-profile volume transfer from 5% to 400% of pool value per day.

The economic ranking excludes implementation readiness and IP. Current DH2's continuous Python equation is cross-validated against Rust; production exact-input quotes include a conservative 10-ppm haircut. That haircut is not charged against current DH2 in the pure-math ranking. The native Dusk replay does charge it and mirrors the Rust retention/recenter transition ordering, but EMA, Q, center, and candidate calculations remain analytical floating point rather than a multi-trade bit-exact Rust trace. The declared matching domains, read directly from `parameter_matching.csv`, are {declared_domain_summary}. CPMM remains a separate zero-concentration control. Curve's bounds come from pinned Twocrypto-NG v2.1 parameter checks converted to paper-A normalization.

The equal-execution result matches one geometric-mean impact index, not the full quote surface. {iso_censoring_scope}

The Curve native reference independently implements the published Twocrypto-NG v2.1 fixed-parameter swap, dynamic fee, EMA, `xcp_profit` gate, and price-scale update equations. It is validated against the pinned deployment fixture and published equations without copying or vending Curve source code. It does **not** reproduce liquidity-event hooks, parameter ramps, or the admin-fee claim that is triggered by a liquidity operation and time-gated to at most once per 24 hours; there is no automatic daily transaction in the model or the described protocol behavior. Curve's already-accrued liability is carried across the score boundary, and the v2.1 claim is represented as a 25%-of-gross-`xcp_profit` effective liability rather than as a simulated claim operation.

The native development replay covers {native_history_windows} non-overlapping 12-hour SOLUSDT scoring windows after 24-hour burn-in, four modeled flow regimes, two seeds, fixed and elastic routing, and 5-minute base cadence. The sealed replay repeats the locked-profile primary matrix on 15 post-lock windows. Arrival rates are `{native_arrival_text}`; the low-volume/stuck regime therefore reduces both total notional and the frequency of trade-driven controller observations. Primary-gate sensitivities add the top-ranked policy shortlist, three pool sizes, 1-minute cadence, 5%-to-400% locked-profile daily volume, center-movement-off, CPMM, and alternate Dusk base-fee ownership. Three alternate invariant matches remain non-optimized diagnostics. Historical prices are observed; arrivals, trader information, outside routing, and arbitrage behavior remain modeled assumptions. No external price is available to either controller.

## Reproduce

```bash
cd analysis/cryptoswap-vs-dh2
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 benchmark.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 dusk_controller_search.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 curve_controller_search.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 cpmm_controller_search.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 promote_controller_profiles.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_benchmark.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 audit_native_outputs.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_sealed_validation.py --materialize-lock
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 fetch_sealed_native_history.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_sealed_validation.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 audit_native_outputs.py --final-report
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 build_report.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 -m unittest discover -s tests -v
$NODE_BIN $REPORT_BUILDER --input artifact.json --output report.html
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 finalize_report_html.py report.html
```

The controlled historical snapshot SHA-256 is `{summary['provenance']['historical_snapshot_sha256']}`. The native window snapshot identity and pinned Curve release are recorded in `data/native_history_metadata.json` and `CURVE_REFERENCE.md`. Machine-readable results are in `results/`.
"""
    validation_report = f"""# Validation report

> **Historical sealed DH2 validation snapshot; not a current-runtime
> validation.** The package-level assumptions and locked profiles apply only to
> this experiment.

## Overall assessment: native economic comparison complete

The prior missing native-controller comparison is now resolved. The benchmark independently replays Curve Twocrypto-NG v2.1's fixed-parameter swap, dynamic fee, internal EMA, gross-`xcp_profit` gate, and price-scale update beside current DH2's Dusk-native divergence/volatility fees, retained surcharge, and funded center controller.

{fee_decision_text}

{gross_outcome_text}

{native_descriptive_route_text}

{lp_return_leader_text}

{dusk_readiness_headline}

Those are simulation results under the declared ledger and acceptance gates. They are **not** claims of full deployed-protocol equivalence or forecasts of Jupiter-routed volume.

## Validation status by comparison layer

| Comparison level | Status | Supported use |
|---|---|---|
| Invariant only | Ready | Compare quote shape and one-jump LP loss at matched aggregate execution or matched jump risk. |
| Controlled system | Ready with modeled-flow caveat | Isolate invariant shape under one common Dusk fee, internal EMA, retention, and center controller. |
| Native system | Ready as a fixed-liquidity economic replay | Compare the two modeled native fee/controller systems across the canonical historical and sensitivity matrices. |
| Full deployment equivalence | Not claimed | Liquidity-event hooks, parameter ramps, Curve's liquidity-operation-triggered and once-per-24-hours-gated claim operation, live router flow, and Dusk integration/compute are outside this economic ranking. |

The selected native profiles are Curve {format_profile(selected_profiles['curve_native_v2_1'])} and Dusk {format_profile(selected_profiles['dusk_native'])}. {service_selection_sentence} Inside that eligible comparative envelope, each system first minimizes its selected profile's own required rho, then maximizes worse-route fee-adjusted return utility among ties before development-history scoring. {dusk_selection_sentence} CPMM is locked separately under the same own-rho-first order as {format_profile(selected_profiles['dusk_cpmm'])}. {cpmm_selection_sentence}

The common Curve–DH2 rho controls only comparative profile matching. Dusk's own rho controls Dusk synthetic product readiness. CPMM's rho controls only its fallback qualification. `marginal_tracking_stale_fraction` is the readiness staleness metric; selected-order costs are finite-size service measurements, and `depth_degradation_fraction` is a raw 1%-TVL depth diagnostic rather than stale-price evidence.

{guardrail_markdown}

The selected rows show each original-envelope result and the separate depth diagnostic:

{selected_training_service_markdown}

## Symmetric concentrated-search, independent CPMM-search, and promotion integrity

Curve and concentrated Dusk each complete the same **150 × 128 = 19,200-cell** decision matrix. CPMM independently completes its own **150 × 128 = 19,200-cell** matrix. Quick output, partial matrices, stale search fingerprints, a promotion whose source files changed, a missing promoted search winner, or a canonical locked profile outside its top-ranked policy shortlist all fail the audit.

{search_lock_markdown}

## Canonical matrix and integrity gates

| Output count | Required and observed rows |
|---|---:|
{canonical_count_markdown}

- Historical score set: **{native_history_windows}** non-overlapping SOLUSDT windows, each with 24-hour burn-in and 12-hour scoring.
- Flow set: **{len(native_flow_regimes)}** regimes (`{', '.join(native_flow_regimes)}`), with arrival rates `{native_arrival_text}`, two seeds, and fixed plus elastic routing.
- Primary comparison basis: equal aggregate invariant execution; this is the only basis with controllers and fees selected for it and the only basis used by the universal gate.
- Locked-controller diagnostics: nearest full quote profile, equal one-jump risk, and the pinned v2.1 deployed invariant; none is reoptimized and none can reverse the universal label.
- Robustness: common-envelope-eligible top-ranked policy shortlists ({neighborhood_eligibility_text}), three pool sizes, one-minute cadence, 5%-to-400% locked-profile daily volume, center-movement-off, an independently selected Dusk CPMM product comparator, a same-policy zero-concentration ablation, and Dusk base-fee ownership from 0% through 100%.
- All decision metrics are finite across every underlying replay row: **{str(bool(native['validation']['all_decision_metrics_finite'])).lower()}**.
- Maximum native ledger reconciliation error: **{float(native['validation']['maximum_accounting_identity_error']):.3e}** quote-value units.
- Controller reads of the external reference price: **{int(native['validation']['controller_external_price_reads'])}**.
- Arbitrage events still profitable after the bounded same-slot cascade across burn-in and scoring: **{int(native['validation']['arbitrage_capped_event_count'])}**.
- Arbitrage-search arithmetic, overflow, or solver failures across burn-in and scoring: **{int(native['validation']['arbitrage_numerical_failure_count'])}**.
- Deterministic Curve cascade witness: **20 fills, 19 price-scale recenters, no profitable follow-up**; Curve emergency ceiling **64**, shared DH2/CPMM ceiling **16**.
- Frozen native history SHA-256: **`{json.loads((ROOT / 'data/native_history_metadata.json').read_text())['snapshot_sha256']}`**.

The report generator rejects smoke output, a non-canonical status, a missing matrix, any mismatched CSV row count, or a profile-selection mismatch.

## Post-lock sealed confirmation

{sealed_confirmation_text}

{sealed_route_markdown}

The sealed audit re-hashes the immutable profile lock, all three complete search summaries, the replay engine, history CSV and metadata, and both replay output ledgers. It reconstructs every paired delta from the 720 underlying result rows, recomputes the sealed summary diagnostics, and requires all 720 paired rows. The confirmation status can be **PASS** or **FAIL** without modifying the locked selection; evidence-integrity failure prevents report generation entirely.{sealed_correction_text}

## Economic and service evidence

### Development-history routing results

{native_route_markdown(native_routes)}

### Fee-adjusted flow-by-route tails

{native_flow_markdown(native_flow_rows)}

### Alternate invariant-match diagnostics with locked controllers

{native_basis_markdown(native_basis_rows)}

### Locked-profile daily-volume transfer

{native_volume_markdown(native_volume_rows)}

### Revenue-ownership sensitivity

{native_fee_split_markdown(native_fee_split_rows)}

## Acceptance predicates that did not pass

Every fee-adjusted predicate must pass before a candidate receives the universal-winner label. Gross diagnostics are not eligible for that label because profile selection used fee-adjusted return. A failed predicate means “no universal winner”—not an economic tie.

{failed_predicate_markdown}

## Corrections retained from the earlier analysis

- Raw `A`, `gamma`, `peak_depth`, and `fade_scale` are never compared directly; configurations are matched by observable quote service or jump risk.
- Arbitrage maximizes positive after-fee profit and is skipped when unprofitable.
- Claimable token fees remain LP wealth but are excluded from executable reserves; retained surcharge is counted once.
- Dusk's production 10-ppm output haircut is neutral to protected-Q recenter capacity and is not misclassified as retained surcharge.
- Curve's liability carries burn-in `xcp_profit_a` into the scoring window; Dusk fee ownership applies to base fees only, while dynamic surcharge remains LP-owned.
- Volatility is driven by successful AMM trade shocks, divergence surcharge is outward-only, and neither controller can see the external price.
- `realized_arbitrage_extraction_proxy_value` is only simulated realized arbitrage profit; it is not labeled as complete LVR.
- Paired differences are formed per identical window, flow, seed, and route before aggregation.

## Independent Curve reference and source boundary

The reference is pinned to Twocrypto-NG v2.1.0 commit `5cbe558902402e8fcb331463089db65fc56c11f9`. Bidirectional fixture quotes match the observed Ethereum deployment within one smallest six-decimal unit. Curve source code, tests, constants, comments, and implementation structure are neither copied nor vendored.

The replay covers fixed-parameter swaps, dynamic fees, EMA, the gross-profit gate, and price-scale moves. It does not simulate liquidity-event hooks, `A`/`gamma` ramps, or the admin-fee claim operation that a liquidity action may trigger and that is time-gated to at most once per 24 hours. The modeled 25%-of-gross-`xcp_profit` liability represents LP economics without pretending to reproduce that operation schedule; no automatic daily claim transaction is assumed.

## Decision-impacting limitations

1. Historical reference prices are observed; order arrivals, informed-flow accuracy, whole-order router elasticity, arbitrage priority, and arbitrage latency remain modeled. Elastic routing is a venue-choice tournament against a stateless outside CPMM, not optimal split routing.
2. SOLUSDT is used as a SOL/USDC market-path proxy. Results should be retested with signed production intents, fills, competing quotes, and post-trade markouts before making a volume claim.
3. This is a fixed-liquidity, exact-input economic replay. Exact-output flow, deposits, withdrawals, lending, leverage, liquidation, hLP operations, parameter ramps, fee/EMA manipulation, transaction ordering attacks, and bad debt are excluded rather than populated with invented assumptions.
4. A universal-winner gate is deliberately stricter than a median ranking. If neither candidate clears every tail and robustness predicate, the report says neither is universally dominant while still showing the actual economic deltas.
5. {iso_censoring_scope}

## Reproduce

```bash
cd analysis/cryptoswap-vs-dh2
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 benchmark.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 dusk_controller_search.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 curve_controller_search.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 cpmm_controller_search.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 promote_controller_profiles.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_benchmark.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 audit_native_outputs.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_sealed_validation.py --materialize-lock
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 fetch_sealed_native_history.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 native_sealed_validation.py --workers 4
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 audit_native_outputs.py --final-report
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 build_report.py
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 -m unittest discover -s tests -v
$NODE_BIN $REPORT_BUILDER --input artifact.json --output report.html
PYTHONPYCACHEPREFIX=/tmp/dusk-pycache python3 finalize_report_html.py report.html
```

The authoritative machine-readable decision is `results/native_summary.json`; `DECISION_REPORT.md`, this validation report, the notebook, and `artifact.json` derive from the same frozen outputs.
"""
    reference_comparison = []
    for model in RANKED_MODELS:
        reference_comparison.append(
            {
                "model": MODEL_LABELS[model],
                "model_id": model,
                "jump_loss_equal_execution_bps": float(
                    equal_execution[model]["jump_loss_index_bps"]
                ),
                "execution_equal_risk_bps": float(
                    equal_risk[model]["execution_index_bps"]
                ),
                "center_impact_equal_execution_bps": float(
                    equal_execution[model]["center_100bps_impact"]
                ),
                "equal_execution_parameters": equal_execution[model][
                    "parameter_label"
                ],
                "equal_risk_parameters": equal_risk[model]["parameter_label"],
                "declared_parameter_domain": parameter_domain_text(model),
                "parameter_1_domain_min": parameter_domains[model][0],
                "parameter_1_domain_max": parameter_domains[model][1],
                "parameter_2_domain_min": parameter_domains[model][2],
                "parameter_2_domain_max": parameter_domains[model][3],
                "equal_execution_boundary_censored": iso_censored(
                    equal_execution[model]
                ),
                "equal_risk_boundary_censored": iso_censored(equal_risk[model]),
                "equal_execution_parameter_1_at_boundary": csv_bool(
                    equal_execution[model]["parameter_1_at_domain_boundary"],
                    field="parameter_1_at_domain_boundary",
                ),
                "equal_execution_parameter_2_at_boundary": csv_bool(
                    equal_execution[model]["parameter_2_at_domain_boundary"],
                    field="parameter_2_at_domain_boundary",
                ),
                "equal_risk_parameter_1_at_boundary": csv_bool(
                    equal_risk[model]["parameter_1_at_domain_boundary"],
                    field="parameter_1_at_domain_boundary",
                ),
                "equal_risk_parameter_2_at_boundary": csv_bool(
                    equal_risk[model]["parameter_2_at_domain_boundary"],
                    field="parameter_2_at_domain_boundary",
                ),
            }
        )

    pareto_dataset = [
        {
            "model": MODEL_LABELS[row["model"]],
            "chart_model": CHART_MODEL_LABELS[row["model"]],
            "model_id": row["model"],
            "parameter_1": float(row["parameter_1"]),
            "parameter_2": float(row["parameter_2"]),
            "execution_index_bps": float(row["execution_index_bps"]),
            "jump_loss_index_bps": float(row["jump_loss_index_bps"]),
            "center_100bps_impact": float(row["center_100bps_impact"]),
            "tail_100bps_impact": float(row["tail_100bps_impact"]),
            "combined_pareto": bool(row["combined_pareto"]),
        }
        for row in display_pareto
    ]
    path_delta = paired_path_summary(pairs)
    path_delta_synthetic = [
        row for row in path_delta if row["path_class"] == "synthetic"
    ]
    path_delta_historical = [
        row for row in path_delta if row["path_class"] == "historical"
    ]
    safety_dataset = [
        {
            "model": MODEL_LABELS[row["model"]],
            "solver_failures": int(row["solver_failures"]),
            "non_monotonic_quotes": int(row["non_monotonic_quotes"]),
            "max_invariant_error": float(
                row["max_relative_invariant_error"]
            ),
            "max_invariant_error_display": (
                f"{float(row['max_relative_invariant_error']):.3e}"
            ),
            "roundtrip_gain": float(
                row["max_zero_fee_roundtrip_gain_quote"]
            ),
            "max_invariant_roots": int(row["max_invariant_roots"]),
            "hard_rejected": row["hard_rejected"] == "True",
            "rejection_reason": row["rejection_reason"],
            "arithmetic_scope": row["arithmetic_scope"],
        }
        for row in safety
        if row["model"] != "proposed_dh2"
    ]
    routing_summary = binary_routing_summary(routing)

    sources = [
        {
            "id": "benchmark",
            "label": "Corrected deterministic economic benchmark",
            "path": "results/summary.json",
        },
        {
            "id": "pareto-source",
            "label": "Invariant parameter sweep",
            "path": "results/pareto.csv",
        },
        {
            "id": "shape-source",
            "label": "Center quote and slippage sweep",
            "path": "results/shape.csv",
        },
        {
            "id": "paths-source",
            "label": "Paired controlled path replay",
            "path": "results/paired_differences.csv",
        },
        {
            "id": "history",
            "label": "Frozen SOLUSDT one-minute history",
            "path": "data/solusdt_1m.csv",
        },
        {
            "id": "sharp-root-evidence",
            "label": "Sharp-DH2 multiple-root safety witness",
            "path": "results/sharp_root_evidence.csv",
        },
        {
            "id": "hyperbolic-p4-comparison",
            "label": "D-independent p4 iso-service comparison",
            "path": "results/hyperbolic_p4_comparison.csv",
        },
        {
            "id": "native-summary",
            "label": "Canonical fixed-liquidity native decision summary",
            "path": "results/native_summary.json",
        },
        {
            "id": "dusk-controller-search",
            "label": "Full decision-grade Dusk controller search",
            "path": "results/controller_search/full/summary.json",
        },
        {
            "id": "curve-controller-search",
            "label": "Full decision-grade Curve controller search",
            "path": "results/curve_controller_search/full/summary.json",
        },
        {
            "id": "cpmm-controller-search",
            "label": "Independent decision-grade CPMM controller search",
            "path": "results/cpmm_controller_search/full/summary.json",
        },
        {
            "id": "controller-promotions",
            "label": "Source-bound Curve, Dusk, and CPMM promotion snapshots",
            "path": "results/final_report_audit.json",
        },
        {
            "id": "native-history",
            "label": "Frozen native SOLUSDT calm, typical, and stress windows",
            "path": "data/solusdt_native_windows.csv",
        },
        {
            "id": "native-history-metadata",
            "label": "Native history selection, hashes, and source ledger",
            "path": "data/native_history_metadata.json",
        },
        {
            "id": "native-held-out",
            "label": "Native development-history replay ledger",
            "path": "results/native_held_out.csv",
        },
        {
            "id": "native-paired",
            "label": "Paired native Curve-versus-Dusk outcomes",
            "path": "results/native_paired.csv",
        },
        {
            "id": "sealed-lock",
            "label": "Immutable pre-history Curve, DH2, and CPMM profile lock",
            "path": "data/sealed_profile_lock.json",
        },
        {
            "id": "sealed-history",
            "label": "Post-lock sealed SOLUSDT historical windows and metadata",
            "path": "data/sealed_native_history_manifest.json",
        },
        {
            "id": "sealed-summary",
            "label": "Complete post-lock sealed validation summary",
            "path": "results/sealed_validation/sealed_native_summary.json",
        },
        {
            "id": "sealed-paired",
            "label": "Recomputed post-lock sealed paired outcomes",
            "path": "results/sealed_validation/sealed_native_paired.csv",
        },
        {
            "id": "final-evidence-audit",
            "label": "Fail-closed final report evidence receipt",
            "path": "results/final_report_audit.json",
        },
        {
            "id": "native-basis",
            "label": "Native locked-controller matching-basis diagnostics",
            "path": "results/native_matching_basis_sensitivity.csv",
        },
        {
            "id": "native-volume",
            "label": "Native locked-profile daily-volume sensitivity",
            "path": "results/native_volume_sensitivity.csv",
        },
        {
            "id": "native-fee-split",
            "label": "Dusk base-fee ownership sensitivity",
            "path": "results/native_dusk_fee_split_sensitivity.csv",
        },
        {
            "id": "native-controller-off",
            "label": "Native center-movement-off counterfactual",
            "path": "results/native_controller_counterfactual.csv",
        },
        {
            "id": "curve-cascade-regression",
            "label": "Deterministic Curve same-slot cascade regression",
            "path": "tests/test_curve_native.py",
        },
        {
            "id": "sealed-accounting-correction",
            "label": "Sealed accounting-summary correction receipt",
            "path": "results/sealed_accounting_summary_correction.json",
        },
        {
            "id": "native-cpmm",
            "label": "Independently selected Dusk CPMM product comparator",
            "path": "results/native_cpmm_control.csv",
        },
        {
            "id": "native-cpmm-ablation",
            "label": "Matched-policy Dusk CPMM concentration ablation",
            "path": "results/native_cpmm_matched_policy_ablation.csv",
        },
        {
            "id": "curve-reference",
            "label": "Independent Curve v2.1 reference scope and fixture",
            "path": "CURVE_REFERENCE.md",
        },
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
            "id": "curve-pinned-reference",
            "label": "Pinned Twocrypto-NG v2.1.0 pool behavior",
            "href": "https://github.com/curvefi/twocrypto-ng/blob/5cbe558902402e8fcb331463089db65fc56c11f9/contracts/main/Twocrypto.vy",
        },
        {
            "id": "curve-license",
            "label": "Pinned Twocrypto-NG restrictive source license",
            "href": "https://github.com/curvefi/twocrypto-ng/blob/5cbe558902402e8fcb331463089db65fc56c11f9/LICENSE",
        },
    ]
    charts = [
        {
            "id": "pareto",
            "title": "Execution cost versus one-jump LP loss",
            "description": (
                f"Lower-left is better; {len(pareto_dataset)} displayed points "
                "cover Curve, current DH2, and the CPMM control, with frontier "
                "membership recomputed on that set."
            ),
            "showDescription": True,
            "dataset": "pareto",
            "type": "scatter",
            "source": {
                "label": "Invariant parameter sweep",
                "path": "results/pareto.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT model, chart_model, model_id, parameter_1, parameter_2, execution_index_bps, jump_loss_index_bps, center_100bps_impact, tail_100bps_impact, combined_pareto FROM pareto ORDER BY model, execution_index_bps",
                    "description": "Curve and current-DH2 parameter points plus the CPMM control; frontier membership is recomputed on this displayed set.",
                    "tables_used": ["pareto"],
                },
            },
            "encodings": {
                "x": {
                    "field": "execution_index_bps",
                    "type": "quantitative",
                    "title": "Execution index (bps)",
                },
                "y": {
                    "field": "jump_loss_index_bps",
                    "type": "quantitative",
                    "title": "One-jump loss (bps of TVL)",
                },
                "color": {"field": "chart_model", "type": "nominal"},
            },
        },
        {
            "id": "equal-service",
            "title": "One-jump LP loss at equal execution service",
            "description": "Mean loss over nine price jumps; lower is better.",
            "showDescription": True,
            "dataset": "reference_comparison",
            "type": "bar",
            "source": {
                "label": "Corrected deterministic economic benchmark",
                "path": "results/parameter_matching.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT model, model_id, jump_loss_equal_execution_bps, execution_equal_risk_bps, center_impact_equal_execution_bps, equal_execution_parameters, equal_risk_parameters, parameter_1_domain_min, parameter_1_domain_max, parameter_2_domain_min, parameter_2_domain_max, declared_parameter_domain, equal_execution_boundary_censored, equal_risk_boundary_censored FROM reference_comparison ORDER BY jump_loss_equal_execution_bps",
                    "description": "Selected boundary-conditioned equal-execution and equal-risk comparison points by invariant family; scalar leaders are not global family optima.",
                    "tables_used": ["reference_comparison"],
                },
            },
            "encodings": {
                "x": {"field": "model", "type": "nominal"},
                "y": {
                    "field": "jump_loss_equal_execution_bps",
                    "type": "quantitative",
                    "title": "Loss (bps of TVL)",
                },
            },
        },
        {
            "id": "center-slippage",
            "title": "Center exact-input slippage by trade size",
            "description": (
                "Selected equal-execution references and CPMM in a balanced "
                "$2.0M pool; zero fee, two-direction mean. Lower is better."
            ),
            "showDescription": True,
            "dataset": "center_quotes",
            "type": "line",
            "source": {
                "label": "Center quote and slippage sweep",
                "path": "results/shape.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT model, chart_model, model_id, direction_count, directions, trade_bps_of_tvl, input_value_usd, input_value_display, execution_cost_bps, execution_cost_display, quoted_output_value_usd, quoted_output_value_display, minimum_directional_output_value_usd, maximum_directional_output_value_usd FROM center_quotes ORDER BY model_id, trade_bps_of_tvl",
                    "description": "Two-direction mean exact-input execution cost at the concentration center.",
                    "tables_used": ["center_quotes"],
                },
            },
            "encodings": {
                "x": {
                    "field": "trade_bps_of_tvl",
                    "type": "quantitative",
                    "title": "Exact-input size (bps of TVL)",
                },
                "y": {
                    "field": "execution_cost_bps",
                    "type": "quantitative",
                    "title": "Execution cost (bps)",
                },
                "color": {"field": "chart_model", "type": "nominal"},
            },
        },
        {
            "id": "path-delta-synthetic",
            "title": "Synthetic-path median LP return difference versus Curve",
            "description": "Positive values favor the DH2 competitor; $2.0M pool over 12-hour paths.",
            "showDescription": True,
            "dataset": "path_delta_synthetic",
            "type": "bar",
            "source": {
                "label": "Paired controlled path replay",
                "path": "results/paired_differences.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT path, path_id, path_class, competitor, competitor_id, median_delta_vs_curve, win_fraction_vs_curve, paired_cells FROM path_delta_synthetic ORDER BY path_id, competitor",
                    "description": "Median paired LP return delta versus Curve for every synthetic path and DH2 competitor.",
                    "tables_used": ["path_delta_synthetic"],
                },
            },
            "encodings": {
                "x": {"field": "path", "type": "nominal"},
                "y": {
                    "field": "median_delta_vs_curve",
                    "type": "quantitative",
                    "title": "Paired delta (USD)",
                },
                "color": {"field": "competitor", "type": "nominal"},
            },
            "options": {"grouping": "grouped"},
        },
        {
            "id": "path-delta-historical",
            "title": "Historical-window median LP return difference versus Curve",
            "description": "Positive values favor the DH2 competitor; $2.0M pool over 12-hour paths.",
            "showDescription": True,
            "dataset": "path_delta_historical",
            "type": "bar",
            "source": {
                "label": "Paired controlled path replay",
                "path": "results/paired_differences.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT path, path_id, path_class, competitor, competitor_id, median_delta_vs_curve, win_fraction_vs_curve, paired_cells FROM path_delta_historical ORDER BY path_id, competitor",
                    "description": "Median paired LP return delta versus Curve for every historical window and DH2 competitor.",
                    "tables_used": ["path_delta_historical"],
                },
            },
            "encodings": {
                "x": {"field": "path", "type": "nominal"},
                "y": {
                    "field": "median_delta_vs_curve",
                    "type": "quantitative",
                    "title": "Paired delta (USD)",
                },
                "color": {"field": "competitor", "type": "nominal"},
            },
            "options": {"grouping": "grouped"},
        },
        {
            "id": "native-route-return",
            "title": "Native Dusk-minus-Curve LP return by routing mode",
            "description": (
                "Median paired 12-hour return delta in bps of starting LP "
                "value; positive favors Dusk."
            ),
            "showDescription": True,
            "dataset": "native_route_return",
            "type": "bar",
            "source": {
                "label": "Canonical fixed-liquidity native decision summary",
                "path": "results/native_summary.json",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT routing_mode, economics_view, economics_view_short, dusk_minus_curve_lp_return_bps, paired_cells, execution_cost_delta_bps, capture_delta_fraction, marginal_tracking_stale_delta_fraction, depth_degradation_delta_fraction FROM native_route_return ORDER BY routing_mode, economics_view",
                    "description": "Fee-adjusted paired native return medians plus the gross outcome for those fee-adjusted-selected profiles, by route.",
                    "tables_used": ["native_route_return"],
                },
            },
            "encodings": {
                "x": {"field": "routing_mode", "type": "nominal"},
                "y": {
                    "field": "dusk_minus_curve_lp_return_bps",
                    "type": "quantitative",
                    "title": "Dusk − Curve LP return (bps / 12h)",
                },
                "color": {"field": "economics_view_short", "type": "nominal"},
            },
            "options": {"grouping": "grouped"},
        },
        {
            "id": "native-flow-return",
            "title": "Native fee-adjusted LP return by flow and route",
            "description": (
                "Median Dusk-minus-Curve paired return; each flow-route cell "
                "is tested independently."
            ),
            "showDescription": True,
            "dataset": "native_flow_route",
            "type": "bar",
            "source": {
                "label": "Canonical fixed-liquidity native decision summary",
                "path": "results/native_summary.json",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT flow_route, flow_regime, routing_mode, median_return_advantage_bps, bottom_decile_mean_return_advantage_bps, p90_execution_regression_bps, p10_capture_advantage_fraction, p90_marginal_tracking_stale_regression_fraction FROM native_flow_route ORDER BY flow_regime, routing_mode",
                    "description": "Dusk-candidate fee-adjusted decision metrics at the required flow-by-route grain.",
                    "tables_used": ["native_flow_route"],
                },
            },
            "encodings": {
                "x": {"field": "flow_route", "type": "nominal"},
                "y": {
                    "field": "median_return_advantage_bps",
                    "type": "quantitative",
                    "title": "Dusk − Curve LP return (bps / 12h)",
                },
            },
        },
        {
            "id": "native-fee-split-return",
            "title": "Native LP return under alternate Dusk base-fee ownership",
            "description": (
                "Median Dusk-minus-Curve paired return; revenue ownership is "
                "revalued without changing the trade path."
            ),
            "showDescription": True,
            "dataset": "native_fee_split",
            "type": "bar",
            "source": {
                "label": "Dusk base-fee ownership sensitivity",
                "path": "results/native_dusk_fee_split_sensitivity.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT base_fee_share, share_route, dusk_base_fee_revenue_share_bps, routing_mode, median_return_delta_bps, p10_return_delta_bps, cell_win_fraction, cells FROM native_fee_split ORDER BY dusk_base_fee_revenue_share_bps, routing_mode",
                    "description": "LP return sensitivity to the fraction of Dusk base fees assigned outside LP ownership.",
                    "tables_used": ["native_fee_split"],
                },
            },
            "encodings": {
                "x": {"field": "base_fee_share", "type": "nominal"},
                "y": {
                    "field": "median_return_delta_bps",
                    "type": "quantitative",
                    "title": "Dusk − Curve LP return (bps / 12h)",
                },
                "color": {"field": "routing_mode", "type": "nominal"},
            },
            "options": {"grouping": "grouped"},
        },
    ]
    tables = [
        {
            "id": "search-lock-table",
            "title": "Symmetric concentrated searches, independent CPMM search, and canonical lock",
            "description": (
                "Curve and concentrated Dusk use symmetric budgets; CPMM receives "
                "an independent 150-profile × 128-cell synthetic search. "
                "the search winner and canonical locked shortlist member are "
                "shown separately."
            ),
            "showDescription": True,
            "dataset": "search_lock",
            "source": {
                "label": "Final report evidence audit",
                "path": "results/final_report_audit.json",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT family, search_profiles, search_cells_per_profile, search_rows, search_winner_profile_id, promoted_profiles, canonical_locked_profile_id, canonical_locked_source_rank, locked_configuration FROM search_lock ORDER BY family",
                    "description": "Symmetric concentrated-search budgets, an equally sized independent CPMM search, and source-bound canonical profile locks.",
                    "tables_used": ["search_lock"],
                },
            },
            "columns": [
                {"field": "family", "label": "Family"},
                {
                    "field": "search_rows",
                    "label": "Full search cells",
                    "format": "number",
                },
                {
                    "field": "search_winner_profile_id",
                    "label": "Search winner",
                },
                {
                    "field": "promoted_profiles",
                    "label": "Promoted",
                    "format": "number",
                },
                {
                    "field": "canonical_locked_profile_id",
                    "label": "Canonical lock",
                },
                {
                    "field": "locked_configuration",
                    "label": "Locked configuration",
                },
            ],
        },
        {
            "id": "sealed-route-table",
            "title": "Development-history versus sealed-history outcomes",
            "description": (
                "Median fee-adjusted Dusk-minus-Curve return and paired win "
                "fractions by route; positive return favors Dusk."
            ),
            "showDescription": True,
            "dataset": "sealed_route_comparison",
            "source": {
                "label": "Post-lock sealed evidence audit",
                "path": "results/final_report_audit.json",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT routing_mode, development_median_dusk_minus_curve_lp_return_bps, sealed_median_dusk_minus_curve_lp_return_bps, development_cell_win_fraction, sealed_cell_win_fraction, development_window_win_fraction, sealed_window_win_fraction, sealed_bottom_decile_mean_return_bps, sealed_p90_execution_regression_bps, sealed_p90_marginal_tracking_stale_regression_fraction FROM sealed_route_comparison ORDER BY routing_mode",
                    "description": "Locked-profile development and post-lock sealed route outcomes.",
                    "tables_used": ["sealed_route_comparison"],
                },
            },
            "columns": [
                {"field": "routing_mode", "label": "Route"},
                {
                    "field": "development_median_dusk_minus_curve_lp_return_bps",
                    "label": "Development return (bps)",
                    "format": "number",
                },
                {
                    "field": "sealed_median_dusk_minus_curve_lp_return_bps",
                    "label": "Sealed return (bps)",
                    "format": "number",
                },
                {
                    "field": "development_window_win_fraction",
                    "label": "Development window wins",
                    "format": "percent",
                },
                {
                    "field": "sealed_window_win_fraction",
                    "label": "Sealed window wins",
                    "format": "percent",
                },
            ],
        },
        {
            "id": "center-quote-table",
            "title": "Concrete center quote examples",
            "description": (
                "Zero-fee exact-input quotes for a balanced $2.0M marked-TVL "
                "pool; each direction is valued at the pre-trade center before averaging."
            ),
            "showDescription": True,
            "dataset": "center_quotes",
            "source": {
                "label": "Center quote and slippage sweep",
                "path": "results/shape.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT model, trade_bps_of_tvl, input_value_display, execution_cost_display, quoted_output_value_display FROM center_quotes ORDER BY trade_bps_of_tvl, execution_cost_bps",
                    "description": "Concrete quote values underlying the center-slippage chart.",
                    "tables_used": ["center_quotes"],
                },
            },
            "columns": [
                {"field": "model", "label": "Invariant"},
                {
                    "field": "trade_bps_of_tvl",
                    "label": "Size (bps TVL)",
                    "format": "number",
                },
                {
                    "field": "input_value_display",
                    "label": "Input value (USD)",
                },
                {
                    "field": "execution_cost_display",
                    "label": "Execution cost (bps)",
                },
                {
                    "field": "quoted_output_value_display",
                    "label": "Quoted output value (USD)",
                },
            ],
            "defaultSort": {
                "field": "trade_bps_of_tvl",
                "direction": "asc",
            },
        },
        {
            "id": "reference-table",
            "title": "Reference-region comparison",
            "description": "Equal-service and equal-risk scalar cross-sections, declared domains, and boundary-censor status; no row is a global-family optimum claim.",
            "showDescription": True,
            "dataset": "reference_comparison",
            "source": {
                "label": "Corrected deterministic economic benchmark",
                "path": "results/parameter_matching.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT model, jump_loss_equal_execution_bps, execution_equal_risk_bps, center_impact_equal_execution_bps, equal_execution_parameters, equal_risk_parameters, parameter_1_domain_min, parameter_1_domain_max, parameter_2_domain_min, parameter_2_domain_max, declared_parameter_domain, equal_execution_boundary_censored, equal_risk_boundary_censored FROM reference_comparison ORDER BY jump_loss_equal_execution_bps",
                    "description": "Exact selected scalar metrics, parameters, declared domains, and boundary-censor flags.",
                    "tables_used": ["reference_comparison"],
                },
            },
            "columns": [
                {"field": "model", "label": "Curve family"},
                {
                    "field": "jump_loss_equal_execution_bps",
                    "label": "Jump loss @ equal execution",
                    "format": "number",
                },
                {
                    "field": "execution_equal_risk_bps",
                    "label": "Execution @ equal risk",
                    "format": "number",
                },
                {
                    "field": "center_impact_equal_execution_bps",
                    "label": "Center impact",
                    "format": "number",
                },
                {
                    "field": "equal_execution_parameters",
                    "label": "Equal-execution parameters",
                },
                {
                    "field": "equal_execution_boundary_censored",
                    "label": "Equal-execution boundary-censored",
                },
                {
                    "field": "equal_risk_parameters",
                    "label": "Equal-risk parameters",
                },
                {
                    "field": "equal_risk_boundary_censored",
                    "label": "Equal-risk boundary-censored",
                },
                {
                    "field": "declared_parameter_domain",
                    "label": "Declared search domain",
                },
            ],
            "defaultSort": {
                "field": "jump_loss_equal_execution_bps",
                "direction": "asc",
            },
        },
        {
            "id": "routing-table",
            "title": "Binary Curve–DH2 equal-intent quote tournament",
            "description": "Best-quote share recomputed between Curve and current DH2 across the declared state/order grid; not a volume forecast.",
            "showDescription": True,
            "dataset": "routing_summary",
            "source": {
                "label": "Static equal-intent quote tournament",
                "path": "results/routing.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT model, best_quote_intent_share, median_execution_cost_bps, intent_count FROM routing_summary ORDER BY best_quote_intent_share DESC",
                    "description": "Curve-versus-current-DH2 best-quote intent share and median cost recomputed from raw execution costs.",
                    "tables_used": ["routing_summary"],
                },
            },
            "columns": [
                {"field": "model", "label": "Curve family"},
                {
                    "field": "best_quote_intent_share",
                    "label": "Best-quote share",
                    "format": "percent",
                },
                {
                    "field": "median_execution_cost_bps",
                    "label": "Median cost (bps)",
                    "format": "number",
                },
                {"field": "intent_count", "label": "Intent rows"},
            ],
            "defaultSort": {
                "field": "best_quote_intent_share",
                "direction": "desc",
            },
        },
        {
            "id": "safety-table",
            "title": "Common reference checks",
            "description": "Decision-grade checks plus the rejected sharp-DH2 uniqueness witness.",
            "showDescription": True,
            "dataset": "safety",
            "source": {
                "label": "Common numerical reference checks",
                "path": "results/safety.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT model, solver_failures, non_monotonic_quotes, max_invariant_error, max_invariant_error_display, roundtrip_gain, max_invariant_roots, hard_rejected, rejection_reason, arithmetic_scope FROM safety ORDER BY model",
                    "description": "Numerical safety results for the equal-execution references, CPMM, and the sharp-DH2 witness.",
                    "tables_used": ["safety"],
                },
            },
            "columns": [
                {"field": "model", "label": "Model"},
                {"field": "solver_failures", "label": "Solver failures"},
                {
                    "field": "non_monotonic_quotes",
                    "label": "Non-monotonic quotes",
                },
                {
                    "field": "max_invariant_error_display",
                    "label": "Max invariant error",
                },
                {
                    "field": "max_invariant_roots",
                    "label": "Invariant roots",
                },
                {"field": "hard_rejected", "label": "Hard rejected"},
                {"field": "arithmetic_scope", "label": "Arithmetic scope"},
            ],
            "defaultSort": {"field": "model", "direction": "asc"},
        },
        {
            "id": "native-service-policy-table",
            "title": "Three service-relaxation policies",
            "description": (
                "The comparative, Dusk-readiness, and CPMM-fallback rho values "
                "have separate authority. Only the Dusk row determines Dusk's "
                "synthetic product-readiness status."
            ),
            "showDescription": True,
            "dataset": "native_service_policy",
            "source": {
                "label": "Canonical native selection policies",
                "path": "results/native_summary.json",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT policy, rho, original_envelope_pass, selected_profile, product_readiness_role FROM native_service_policy ORDER BY policy",
                    "description": "Independent authority and relaxation for each native profile-selection policy.",
                    "tables_used": ["native_service_policy"],
                },
            },
            "columns": [
                {"field": "policy", "label": "Policy"},
                {"field": "rho", "label": "Required rho", "format": "percent"},
                {
                    "field": "original_envelope_pass",
                    "label": "rho = 0 pass",
                },
                {"field": "selected_profile", "label": "Selected profile"},
                {
                    "field": "product_readiness_role",
                    "label": "Authority",
                },
            ],
            "defaultSort": {"field": "policy", "direction": "asc"},
        },
        {
            "id": "native-route-table",
            "title": "Native route-level paired outcomes",
            "description": (
                "Medians across all development-history flow, seed, and window cells; "
                "positive return favors Dusk."
            ),
            "showDescription": True,
            "dataset": "native_route",
            "source": {
                "label": "Canonical fixed-liquidity native decision summary",
                "path": "results/native_summary.json",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT routing_mode, gross_return_delta_bps, fee_adjusted_return_delta_bps, p10_fee_adjusted_return_delta_bps, p90_fee_adjusted_return_delta_bps, cell_win_fraction, window_median_win_fraction, execution_cost_delta_bps, capture_delta_fraction, marginal_tracking_stale_delta_fraction, depth_degradation_delta_fraction, paired_cells FROM native_route ORDER BY routing_mode",
                    "description": "Route-level native paired outcome summary used in the report narrative.",
                    "tables_used": ["native_route"],
                },
            },
            "columns": [
                {"field": "routing_mode", "label": "Routing"},
                {
                    "field": "gross_return_delta_bps",
                    "label": "Gross return delta (bps)",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "fee_adjusted_return_delta_bps",
                    "label": "Fee-adjusted delta (bps)",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "p10_fee_adjusted_return_delta_bps",
                    "label": "p10 fee-adjusted delta",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "execution_cost_delta_bps",
                    "label": "Execution-cost delta",
                    "format": "number",
                },
                {
                    "field": "capture_delta_fraction",
                    "label": "Capture delta",
                    "format": "percent",
                    "movement": True,
                },
                {
                    "field": "marginal_tracking_stale_delta_fraction",
                    "label": "Marginal-tracking stale delta",
                    "format": "percent",
                },
                {
                    "field": "depth_degradation_delta_fraction",
                    "label": "1%-TVL depth degradation delta (diagnostic)",
                    "format": "percent",
                },
            ],
            "defaultSort": {"field": "routing_mode", "direction": "asc"},
        },
        {
            "id": "native-lp-leader-table",
            "title": "Locked-profile LP-return leader at primary equal execution",
            "description": (
                "Positive deltas favor Dusk. This table ranks LP return only and "
                "keeps execution, capture, tracking, and the universal gate separate."
            ),
            "showDescription": True,
            "dataset": "native_lp_leader",
            "source": {
                "label": "Canonical locked-profile LP-return result",
                "path": "results/native_summary.json",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT descriptive_leader, material_both_routes_leader, median_dusk_minus_curve_return_bps, p10_dusk_minus_curve_return_bps, p90_dusk_minus_curve_return_bps, cell_win_fraction, fixed_route_median_dusk_minus_curve_bps, elastic_route_median_dusk_minus_curve_bps, median_dusk_minus_curve_selected_execution_cost_bps, median_dusk_minus_curve_capture_fraction, median_dusk_minus_curve_marginal_tracking_stale_fraction FROM native_lp_leader",
                    "description": "LP-return-only leader and adjacent service diagnostics for the locked equal-execution profiles.",
                    "tables_used": ["native_lp_leader"],
                },
            },
            "columns": [
                {"field": "descriptive_leader", "label": "LP-return leader"},
                {
                    "field": "material_both_routes_leader",
                    "label": "Material both-routes leader",
                },
                {
                    "field": "median_dusk_minus_curve_return_bps",
                    "label": "Median return delta (bps)",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "p10_dusk_minus_curve_return_bps",
                    "label": "p10 return delta",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "p90_dusk_minus_curve_return_bps",
                    "label": "p90 return delta",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "cell_win_fraction",
                    "label": "Dusk cell wins",
                    "format": "percent",
                },
                {
                    "field": "fixed_route_median_dusk_minus_curve_bps",
                    "label": "Fixed-route median",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "elastic_route_median_dusk_minus_curve_bps",
                    "label": "Elastic-route median",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "median_dusk_minus_curve_selected_execution_cost_bps",
                    "label": "Execution-cost delta",
                    "format": "number",
                },
                {
                    "field": "median_dusk_minus_curve_capture_fraction",
                    "label": "Capture delta",
                    "format": "percent",
                    "movement": True,
                },
                {
                    "field": "median_dusk_minus_curve_marginal_tracking_stale_fraction",
                    "label": "Marginal-stale delta",
                    "format": "percent",
                },
            ],
        },
        {
            "id": "native-basis-table",
            "title": "Locked-controller alternate invariant-match diagnostics",
            "description": (
                "Controllers and fees remain those selected on equal execution; "
                "alternate bases are not optimized and cannot change the universal label."
            ),
            "showDescription": True,
            "dataset": "native_basis",
            "source": {
                "label": "Native locked-controller matching-basis diagnostics",
                "path": "results/native_matching_basis_sensitivity.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT matching_basis, routing_mode, median_return_advantage_bps, bottom_decile_mean_return_advantage_bps, cell_win_fraction, window_median_win_fraction, p90_execution_regression_bps, p10_capture_advantage_fraction, p90_marginal_tracking_stale_regression_fraction FROM native_basis ORDER BY matching_basis, routing_mode",
                    "description": "Non-optimized fee-adjusted Dusk-minus-Curve transfer diagnostics by alternate matching basis and route.",
                    "tables_used": ["native_basis"],
                },
            },
            "columns": [
                {"field": "matching_basis", "label": "Basis"},
                {"field": "routing_mode", "label": "Route"},
                {
                    "field": "median_return_advantage_bps",
                    "label": "Median return delta",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "bottom_decile_mean_return_advantage_bps",
                    "label": "Bottom-decile mean",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "p90_execution_regression_bps",
                    "label": "p90 execution regression",
                    "format": "number",
                },
                {
                    "field": "p10_capture_advantage_fraction",
                    "label": "p10 capture advantage",
                    "format": "percent",
                    "movement": True,
                },
                {
                    "field": "p90_marginal_tracking_stale_regression_fraction",
                    "label": "p90 marginal-stale regression",
                    "format": "percent",
                },
            ],
            "defaultSort": {
                "field": "matching_basis",
                "direction": "asc",
            },
        },
        {
            "id": "native-volume-table",
            "title": "Locked-profile transfer across daily-volume regimes",
            "description": (
                "Profiles and arrival frequency stay fixed from 5% to 400% daily "
                "volume/pool; the table summarizes four flow-route cells per row."
            ),
            "showDescription": True,
            "dataset": "native_volume",
            "source": {
                "label": "Native locked-profile daily-volume sensitivity",
                "path": "results/native_volume_sensitivity.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT comparison, daily_volume_display, daily_volume_pool_ratio, flow_route_cells, median_return_advantage_bps, worst_bottom_decile_mean_return_advantage_bps, worst_p90_execution_regression_bps, worst_p10_capture_advantage_fraction, worst_p90_marginal_tracking_stale_regression_fraction FROM native_volume ORDER BY comparison, daily_volume_pool_ratio",
                    "description": "Locked-profile return and service transfer across notional-throughput regimes without controller reselection.",
                    "tables_used": ["native_volume"],
                },
            },
            "columns": [
                {"field": "comparison", "label": "Comparison"},
                {
                    "field": "daily_volume_pool_ratio",
                    "label": "Daily volume / pool",
                    "format": "percent",
                },
                {
                    "field": "median_return_advantage_bps",
                    "label": "Median of cell medians",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "worst_bottom_decile_mean_return_advantage_bps",
                    "label": "Worst bottom-decile mean",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "worst_p90_execution_regression_bps",
                    "label": "Worst p90 execution regression",
                    "format": "number",
                },
                {
                    "field": "worst_p10_capture_advantage_fraction",
                    "label": "Worst p10 capture advantage",
                    "format": "percent",
                    "movement": True,
                },
                {
                    "field": "worst_p90_marginal_tracking_stale_regression_fraction",
                    "label": "Worst p90 marginal-stale regression",
                    "format": "percent",
                },
            ],
            "defaultSort": {
                "field": "daily_volume_pool_ratio",
                "direction": "asc",
            },
        },
        {
            "id": "native-controller-table",
            "title": "Native center movement on minus off effects",
            "description": (
                "Paired fixed-and-elastic-route medians; negative execution cost and "
                "tracking RMSE are improvements."
            ),
            "showDescription": True,
            "dataset": "native_controller",
            "source": {
                "label": "Native center-movement-off counterfactual",
                "path": "results/native_controller_counterfactual.csv",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT system, median_controller_on_minus_off_lp_return_bps, lp_win_fraction, median_controller_on_minus_off_execution_cost_bps, median_controller_on_minus_off_capture_fraction, median_controller_on_minus_off_marginal_tracking_stale_fraction, median_controller_on_minus_off_tracking_rmse_bps FROM native_controller ORDER BY system",
                    "description": "Paired effects of enabling each system's native center controller.",
                    "tables_used": ["native_controller"],
                },
            },
            "columns": [
                {"field": "system", "label": "System"},
                {
                    "field": "median_controller_on_minus_off_lp_return_bps",
                    "label": "LP return delta",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "median_controller_on_minus_off_execution_cost_bps",
                    "label": "Execution-cost delta",
                    "format": "number",
                },
                {
                    "field": "median_controller_on_minus_off_capture_fraction",
                    "label": "Capture delta",
                    "format": "percent",
                    "movement": True,
                },
                {
                    "field": "median_controller_on_minus_off_marginal_tracking_stale_fraction",
                    "label": "Marginal-tracking stale delta",
                    "format": "percent",
                },
                {
                    "field": "median_controller_on_minus_off_tracking_rmse_bps",
                    "label": "Tracking-RMSE delta",
                    "format": "number",
                },
            ],
            "defaultSort": {"field": "system", "direction": "asc"},
        },
        {
            "id": "native-cpmm-table",
            "title": "CPMM product comparator versus concentration ablation",
            "description": (
                "Paired development-history medians; every delta is current DH2 minus CPMM. "
                "Only the independently selected product row enters winner gates."
            ),
            "showDescription": True,
            "dataset": "native_cpmm",
            "source": {
                "label": "Canonical native CPMM comparison summaries",
                "path": "results/native_summary.json",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT comparison, control_profile_id, selection_service_relaxation, product_recommendation_status, participates_in_winner_gate, median_current_dh2_minus_cpmm_lp_return_bps, current_dh2_win_fraction, median_current_dh2_minus_cpmm_execution_cost_bps, median_current_dh2_minus_cpmm_capture_fraction, median_current_dh2_minus_cpmm_marginal_tracking_stale_fraction FROM native_cpmm",
                    "description": "Economic and service deltas for current DH2 against both the independently selected CPMM product comparator and the same-policy zero-concentration ablation.",
                    "tables_used": ["native_cpmm"],
                },
            },
            "columns": [
                {"field": "comparison", "label": "Comparison"},
                {"field": "control_profile_id", "label": "CPMM profile"},
                {
                    "field": "selection_service_relaxation",
                    "label": "CPMM service rho",
                    "format": "percent",
                },
                {
                    "field": "product_recommendation_status",
                    "label": "Product status",
                },
                {
                    "field": "participates_in_winner_gate",
                    "label": "Winner gate",
                },
                {
                    "field": "median_current_dh2_minus_cpmm_lp_return_bps",
                    "label": "LP return delta",
                    "format": "number",
                    "movement": True,
                },
                {
                    "field": "current_dh2_win_fraction",
                    "label": "Cell-win share",
                    "format": "percent",
                },
                {
                    "field": "median_current_dh2_minus_cpmm_execution_cost_bps",
                    "label": "Execution-cost delta",
                    "format": "number",
                },
                {
                    "field": "median_current_dh2_minus_cpmm_capture_fraction",
                    "label": "Capture delta",
                    "format": "percent",
                    "movement": True,
                },
                {
                    "field": "median_current_dh2_minus_cpmm_marginal_tracking_stale_fraction",
                    "label": "Marginal-tracking stale delta",
                    "format": "percent",
                },
            ],
        },
        {
            "id": "native-predicate-table",
            "title": "Native universal-winner acceptance predicates",
            "description": (
                "Every predicate must pass for one candidate to be called the "
                "fee-adjusted universal winner. Gross outcomes are diagnostic only."
            ),
            "showDescription": True,
            "dataset": "native_predicates",
            "source": {
                "label": "Canonical fixed-liquidity native decision summary",
                "path": "results/native_summary.json",
                "query": {
                    "language": "sql",
                    "engine": "artifact snapshot",
                    "sql": "SELECT economics_view, candidate, predicate, passed FROM native_predicates ORDER BY economics_view, candidate, predicate",
                    "description": "Complete pass/fail audit of the fee-adjusted native universal decision rule for both candidates.",
                    "tables_used": ["native_predicates"],
                },
            },
            "columns": [
                {"field": "economics_view", "label": "Economics view"},
                {"field": "candidate", "label": "Candidate"},
                {"field": "predicate", "label": "Required predicate"},
                {"field": "passed", "label": "Pass"},
            ],
            "defaultSort": {
                "field": "economics_view",
                "direction": "asc",
            },
        },
    ]
    blocks = [
        {
            "id": "title",
            "type": "markdown",
            "body": (
                "# Curve CryptoSwap vs current DH2, with CPMM control\n\n"
                "> **Historical sealed DH2 snapshot; not a current-runtime validation.**"
            ),
        },
        {
            "id": "summary",
            "type": "markdown",
            "sourceId": "benchmark",
            "body": (
                "## Executive summary\n\n"
                f"{invariant_scalar_summary} {dusk_controlled_decision}"
            ),
        },
        {
            "id": "summary-native",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## Native-controller result\n\n"
                f"{fee_decision_text} {gross_outcome_text} {native_descriptive_route_text} "
                f"{lp_return_leader_text} "
                f"{dusk_readiness_headline} "
                f"{sealed_confirmation_text} "
                f"{service_selection_sentence} {cpmm_selection_sentence} "
                "The result is a "
                "fixed-liquidity simulation under the declared routing, flow, accounting, "
                "and robustness gates—not a live-volume forecast."
            ),
        },
        {
            "id": "comparison-levels",
            "type": "markdown",
            "body": (
                "## Three comparison layers keep curve shape separate from controller economics\n\n"
                f"**Invariant only:** {invariant_scalar_summary}\n\n"
                "**Controlled system:** with the same Dusk fees, internal EMA, order tape, "
                f"retention, and center controller, {controlled_pairwise_summary[0].lower() + controlled_pairwise_summary[1:]} "
                f"{controlled_scope_text}\n\n"
                "**Native system:** each system keeps its own fee, EMA, profit budget, and "
                f"recenter controller. {fee_decision_text}"
            ),
        },
        {
            "id": "reference-text",
            "type": "markdown",
            "sourceId": "benchmark",
            "body": (
                "## Holding service constant makes the scalar comparison like-for-like\n\n"
                "The prior ranking rewarded designs that simply quoted worse or accepted less "
                "flow. These bars hold the broad execution index equal before comparing LP jump "
                f"loss. {invariant_scalar_summary}\n\n"
                f"Declared domains: {declared_domain_summary}. "
                f"{iso_censoring_scope}"
            ),
        },
        {"id": "equal-service-chart", "type": "chart", "chartId": "equal-service"},
        {"id": "reference-table-block", "type": "table", "tableId": "reference-table"},
        {
            "id": "center-quote-text",
            "type": "markdown",
            "sourceId": "shape-source",
            "body": (
                "## Concrete center quotes expose how the same broad service is distributed\n\n"
                "These are zero-fee exact-input quotes for a balanced $2.0M marked-TVL pool. "
                "Each direction's cost and output value are computed independently before the "
                "two-direction mean. Matching the aggregate execution index does not force identical "
                "center slippage: the invariants distribute liquidity differently across the curve.\n\n"
                "At an illustrative **$150/SOL** center, a 100-bps-of-TVL trade means "
                "selling either 133.333 SOL or 20,000 USDC:\n\n"
                f"{sol_quote_bullets}"
            ),
        },
        {
            "id": "center-slippage-chart-block",
            "type": "chart",
            "chartId": "center-slippage",
        },
        {
            "id": "center-quote-table-block",
            "type": "table",
            "tableId": "center-quote-table",
        },
        {
            "id": "pareto-text",
            "type": "markdown",
            "sourceId": "pareto-source",
            "body": (
                f"## {pareto_heading}\n\n"
                f"{pareto_membership_summary} The lower-left envelope is "
                "boundary- and grid-conditioned."
            ),
        },
        {"id": "pareto-chart", "type": "chart", "chartId": "pareto"},
        {
            "id": "paths-text",
            "type": "markdown",
            "sourceId": "paths-source",
            "body": (
                "## Paired controlled paths are derived pair by pair\n\n"
                f"{controlled_pairwise_summary} {controlled_scope_text}"
            ),
        },
        {
            "id": "path-chart-synthetic",
            "type": "chart",
            "chartId": "path-delta-synthetic",
        },
        {
            "id": "path-chart-historical",
            "type": "chart",
            "chartId": "path-delta-historical",
        },
        {
            "id": "routing-text",
            "type": "markdown",
            "sourceId": "benchmark",
            "body": (
                "## Quote specialization is not the same as economic dominance\n\n"
                "The static tournament shows which curve is cheapest at each declared state and "
                "order size. A high share means a design specializes in more grid cells; it does "
                "not forecast Jupiter volume because real intent sizes and state frequencies are unknown."
            ),
        },
        {"id": "routing-table-block", "type": "table", "tableId": "routing-table"},
        {
            "id": "ledger",
            "type": "markdown",
            "sourceId": "benchmark",
            "body": (
                "## Fees and recentering now reconcile\n\n"
                "Claimable fees remain token-denominated LP wealth but are not executable. Retained "
                "surcharge is counted once inside reserves. Divergence is outward-only, volatility "
                "comes from AMM trade shocks, and protected liquidity uses the center-normalized Q. "
                "Recenter impairment is a capacity diagnostic, not a second P&L subtraction."
            ),
        },
        {
            "id": "profile-lock-text",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## Synthetic profile lock does not read development history\n\n"
                f"{service_selection_sentence} {cpmm_selection_sentence}\n\n"
                f"{dusk_selection_sentence}\n\n"
                "Hard numerical, finite-metric, accounting, and no-oracle gates never "
                "relax. Service uses the smallest identical normalized widening that "
                "admits Curve and concentrated Dusk; CPMM uses a separate widening and "
                "cannot contaminate that lock. Dusk product readiness uses the selected "
                "Dusk profile's own rho and is independent of both comparison rho values. "
                "Within each envelope, selection first minimizes the profile's own "
                "required rho, then maximizes worse-route fee-adjusted return utility "
                "among ties. Historical windows are not read.\n\n"
                "Original per-route guardrails include median capture ≥50%, median "
                "selected-order cost ≤25 bps, p90 selected-order cost ≤100 bps, "
                "median marginal-tracking stale time ≤75%, and "
                "worst-flow service tails. The selected rows show each original-envelope result:\n\n"
                f"{selected_training_service_bullets}\n\n"
                "Selected-order costs are finite-size service measures. The 1%-TVL "
                "depth-degradation probe remains a raw diagnostic; it is not the stale-price gate."
            ),
        },
        {
            "id": "native-service-policy-table-block",
            "type": "table",
            "tableId": "native-service-policy-table",
        },
        {
            "id": "symmetric-search-text",
            "type": "markdown",
            "sourceId": "final-evidence-audit",
            "body": (
                "## Symmetric concentrated searches and an independent CPMM search bind every promoted policy\n\n"
                "Curve and concentrated Dusk each receive 150 initial profiles across "
                "the same 128 synthetic cells, for 19,200 decision cells per family. "
                "CPMM independently receives the same 150-by-128 budget. The "
                "audit rejects quick or partial output, stale source hashes, a "
                "missing promoted search winner, and any canonical profile outside "
                "its source-bound top-ranked policy shortlist. Search winner and "
                "canonical lock remain separate because the service envelope may "
                "select another source-ranked promoted policy."
            ),
        },
        {
            "id": "search-lock-table-block",
            "type": "table",
            "tableId": "search-lock-table",
        },
        {
            "id": "native-route-text",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## Fee-adjusted ranking keeps gross outcome diagnostic\n\n"
                f"{fee_decision_text} {gross_outcome_text} Positive bars favor Dusk. "
                "Gross return is a diagnostic for the fee-adjusted-selected profiles and is measured before external revenue liabilities; "
                "fee-adjusted return applies the declared Curve admin and Dusk manager/protocol "
                "ownership while preserving liabilities already present at the score boundary."
            ),
        },
        {
            "id": "native-route-chart-block",
            "type": "chart",
            "chartId": "native-route-return",
        },
        {
            "id": "native-route-table-block",
            "type": "table",
            "tableId": "native-route-table",
        },
        {
            "id": "native-lp-leader-text",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## LP-return leadership and universal dominance are separate questions\n\n"
                f"{lp_return_leader_text} The p10/p90 distribution and adjacent "
                "service deltas remain visible in the table, so a null universal "
                "winner cannot hide the measured LP-return direction."
            ),
        },
        {
            "id": "native-lp-leader-table-block",
            "type": "table",
            "tableId": "native-lp-leader-table",
        },
        {
            "id": "sealed-confirmation-text",
            "type": "markdown",
            "sourceId": "final-evidence-audit",
            "body": (
                "## Sealed post-lock history confirms or rejects replication without retuning\n\n"
                f"{sealed_confirmation_text} Development history was selection-blind "
                "but visible during benchmark development; sealed history was fetched "
                "only after profiles, searches, matrix, and replay engine were locked. "
                "The confirmation uses the existing route, flow-route, and CPMM core "
                "thresholds. It cannot change the selected profile."
            ),
        },
        {
            "id": "sealed-route-table-block",
            "type": "table",
            "tableId": "sealed-route-table",
        },
        *(
            [
                {
                    "id": "sealed-accounting-correction-text",
                    "type": "markdown",
                    "sourceId": "sealed-accounting-correction",
                    "body": (
                        "### Audit correction preserved without changing economics\n\n"
                        + sealed_correction_text.strip()
                    ),
                }
            ]
            if sealed_correction_text
            else []
        ),
        {
            "id": "native-flow-text",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## Route and flow tails—not a pooled median—control the native decision\n\n"
                "Each flow-by-route cell must avoid a median return reversal, and its return, "
                "selected-order execution, capture, and marginal-tracking stale tails must stay inside the declared limits. "
                "This chart shows the median fee-adjusted return delta; the decision also uses "
                "the richer tail fields preserved in the report dataset."
            ),
        },
        {
            "id": "native-flow-chart-block",
            "type": "chart",
            "chartId": "native-flow-return",
        },
        {
            "id": "native-basis-text",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## Alternate invariant matches are locked-controller diagnostics\n\n"
                "The native replay is repeated under nearest full quote-profile, equal one-jump "
                "risk, and pinned v2.1 deployment-invariant matching while retaining the "
                "controllers and fees selected on equal execution. These are non-optimized "
                "transfer diagnostics; a reversal here cannot grant, revoke, or reverse the "
                "primary universal label."
            ),
        },
        {
            "id": "native-basis-table-block",
            "type": "table",
            "tableId": "native-basis-table",
        },
        {
            "id": "native-volume-text",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## Locked profiles are tested from 5% to 400% daily volume\n\n"
                "Notional throughput changes while arrival frequency, profiles, fees, and "
                "controllers stay locked. This tests transfer rather than reoptimizing a "
                "different winner for each volume. The universal gate reads every underlying "
                "flow-route cell; the table reports compact medians and worst tails."
            ),
        },
        {
            "id": "native-volume-table-block",
            "type": "table",
            "tableId": "native-volume-table",
        },
        {
            "id": "native-fee-split-text",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## Revenue ownership can change LP ranking without changing quotes\n\n"
                f"The primary ledger assigns {float(fee_assumptions['dusk_manager_share_of_base_fee_bps']) / 100:.0f}% "
                "of Dusk base fees to the manager, keeps all Dusk dynamic surcharge with LPs, "
                f"and applies Curve's {percent(float(fee_assumptions['curve_effective_admin_liability_fraction_of_gross_xcp_profit']), 0)} "
                "effective gross-xcp-profit liability. The sensitivity revalues Dusk ownership "
                "without altering any trade, quote, reserve, or controller path."
            ),
        },
        {
            "id": "native-fee-split-chart-block",
            "type": "chart",
            "chartId": "native-fee-split-return",
        },
        {
            "id": "native-controller-text",
            "type": "markdown",
            "sourceId": "native-controller-off",
            "body": (
                "## Center-movement counterfactual isolates the value and cost of recentering\n\n"
                f"Enabling center movement changes median LP return by "
                f"{signed_bps(float(curve_controller['median_controller_on_minus_off_lp_return_bps']))} "
                f"for Curve and {signed_bps(float(dusk_controller['median_controller_on_minus_off_lp_return_bps']))} "
                "for Dusk across paired fixed/elastic route, flow, seed, and window cells. Execution, capture, marginal tracking, and "
                "tracking changes are reported alongside return so recentering is not credited "
                "for P&L while hiding a service regression."
            ),
        },
        {
            "id": "native-controller-table-block",
            "type": "table",
            "tableId": "native-controller-table",
        },
        {
            "id": "curve-cascade-regression-text",
            "type": "markdown",
            "sourceId": "curve-cascade-regression",
            "body": (
                "### Curve can reopen same-slot arbitrage after recentering\n\n"
                "A deterministic regression witness required **20 profitable fills and "
                "19 price-scale recenters** before no profitable follow-up remained. It "
                "converged without a numerical failure. The benchmark therefore uses a "
                "Curve-local emergency ceiling of 64 fills; the shared DH2/CPMM ceiling "
                "remains 16. This is a controller-mechanics witness, not a claimed "
                "canonical-matrix maximum or an on-chain execution count."
            ),
        },
        {
            "id": "native-cpmm-text",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## CPMM comparator and invariant ablation answer different questions\n\n"
                f"The product CPMM independently selects profile `{cpmm_product['control_profile_id']}` on synthetic training. Against it, current DH2 changes median LP return by "
                f"{signed_bps(float(cpmm_product['median_current_dh2_minus_cpmm_lp_return_bps']))} "
                f"and selected execution cost by "
                f"{signed_bps(float(cpmm_product['median_current_dh2_minus_cpmm_execution_cost_bps']))}. "
                f"{cpmm_selection_sentence} This comparison gates the universal claim. "
                "The matched-policy row preserves current DH2's selected fees/controller and zeros only `peak_depth` and `fade_scale`; it isolates concentration and remains diagnostic."
            ),
        },
        {
            "id": "native-cpmm-table-block",
            "type": "table",
            "tableId": "native-cpmm-table",
        },
        {
            "id": "native-rule-text",
            "type": "markdown",
            "sourceId": "native-summary",
            "body": (
                "## Universal means every primary equal-execution predicate passes\n\n"
                "The table preserves the complete fee-adjusted pass/fail audit for both systems. "
                "Gross outcomes do not receive a universal label. A failed predicate blocks the fee-adjusted universal label even when the "
                "headline median is positive. `No universal winner` therefore means insufficient "
                "dominance under the declared gate, not equal economic performance. Alternate "
                "invariant matches are reported separately and do not enter this gate."
            ),
        },
        {
            "id": "native-predicate-table-block",
            "type": "table",
            "tableId": "native-predicate-table",
        },
        {
            "id": "safety-text",
            "type": "markdown",
            "sourceId": "benchmark",
            "body": (
                "## Decision-grade references pass; the sharp fade is hard rejected\n\n"
                "Curve, current DH2, and CPMM pass the common numerical sweep. "
                "The fixed exponent-2 sharp-DH2 experiment fails a stronger uniqueness check: one "
                "reserve state has three invariant roots. Current DH2 is cross-validated against Rust; "
                "the rejected sharp experiment is analytical only."
            ),
        },
        {"id": "safety-table-block", "type": "table", "tableId": "safety-table"},
        {
            "id": "sharp-decision",
            "type": "markdown",
            "sourceId": "sharp-root-evidence",
            "body": (
                "## Fixed sharper fade: do not implement\n\n"
                "The experiment keeps only `peak_depth` and `fade_scale`; exponent 2 is protocol-fixed. "
                "It decays faster only after its shoulder, but the stronger D-dependence creates three "
                "valid invariant roots in the committed witness. That is a hard safety rejection before "
                "LP-return comparisons."
            ),
        },
        {
            "id": "hyperbolic-p4-decision",
            "type": "markdown",
            "sourceId": "hyperbolic-p4-comparison",
            "body": (
                f"## {p4_decision_heading}\n\n"
                f"{p4_safety_text}\n\n{p4_scalar_markdown}\n\n"
                f"{p4_economic_text} {p4_production_text}"
            ),
        },
        {
            "id": "decision",
            "type": "markdown",
            "body": (
                f"## Decision: {dusk_decision_heading.lower()}; keep native and scalar claims separate\n\n"
                f"{dusk_controlled_decision} {invariant_scalar_summary} "
                f"For the product-level replay, {fee_decision_text[0].lower() + fee_decision_text[1:]} "
                f"{dusk_readiness_headline} "
                "Retain exact CPMM as the zero-concentration comparator and keep the common "
                "comparison rho separate from Dusk's own product-readiness decision."
            ),
        },
        {
            "id": "limitations",
            "type": "markdown",
            "body": (
                "## Native conclusions remain simulation-conditioned\n\n"
                f"The native result uses {native_history_windows} development historical price windows plus 15 post-lock sealed windows, "
                "but user arrivals, information, outside routing, and arbitrage remain modeled. "
                f"The scalar cross-sections use these declared domains: {declared_domain_summary}. "
                f"{iso_censoring_scope} "
                "The independent Curve reference covers fixed-parameter swap, fee, EMA, profit gate, "
                "and price-scale updates; it does not simulate liquidity-event hooks, parameter ramps, "
                "or the liquidity-operation-triggered claim that is time-gated to at most once per "
                "24 hours; no automatic daily transaction is assumed. Signed routed orders and markouts are still "
                "required for a Jupiter-volume claim."
            ),
        },
    ]
    manifest = {
        "version": 1,
        "surface": "report",
        "title": "Curve CryptoSwap vs current DH2, with CPMM control",
        "description": (
            "Historical sealed DH2 experiment, not a current-runtime validation. "
            "Matched invariant-shape, common-controller, and native-controller "
            "economic comparison of Curve CryptoSwap and DH2, with "
            "CPMM retained as the zero-concentration control."
        ),
        "generatedAt": generated_at,
        "sources": sources,
        "charts": charts,
        "tables": tables,
        "blocks": blocks,
    }
    snapshot = {
        "version": 1,
        "status": "ready",
        "generatedAt": generated_at,
        "datasets": {
            "reference_comparison": reference_comparison,
            "center_quotes": center_quotes,
            "pareto": pareto_dataset,
            "path_delta": path_delta,
            "path_delta_synthetic": path_delta_synthetic,
            "path_delta_historical": path_delta_historical,
            "routing_summary": routing_summary,
            "safety": safety_dataset,
            "native_route": native_routes,
            "native_route_return": native_route_chart,
            "native_lp_leader": native_lp_leader_rows,
            "native_flow_route": native_flow_rows,
            "native_basis": native_basis_rows,
            "native_volume": native_volume_rows,
            "native_fee_split": native_fee_split_rows,
            "native_controller": native_controller_rows,
            "native_cpmm": native_cpmm_rows,
            "native_service_policy": native_service_policy_rows,
            "native_predicates": native_predicate_rows,
            "search_lock": search_lock_rows,
            "sealed_route_comparison": sealed_route_rows,
        },
    }
    artifact = {
        "surface": "report",
        "manifest": manifest,
        "snapshot": snapshot,
        "sources": sources,
    }
    payloads = {
        "DECISION_REPORT.md": report,
        "VALIDATION.md": validation_report,
        "artifact.json": json.dumps(artifact, indent=2) + "\n",
    }
    with tempfile.TemporaryDirectory(prefix=".report-stage-", dir=ROOT) as temporary:
        stage = Path(temporary)
        for filename, payload in payloads.items():
            (stage / filename).write_text(payload)
        for filename in payloads:
            (stage / filename).replace(ROOT / filename)
    return manifest, snapshot


if __name__ == "__main__":
    manifest, snapshot = build()
    print(f"wrote {ROOT / 'DECISION_REPORT.md'}")
    print(f"wrote {ROOT / 'VALIDATION.md'}")
    print(f"wrote {ROOT / 'artifact.json'}")
    print(f"datasets: {', '.join(snapshot['datasets'])}")

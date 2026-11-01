"""Pure validation helpers shared by native output audit and report build."""

from __future__ import annotations

import math


SERVICE_RELAXATION_TOLERANCE = 1e-12
CONCENTRATED_SELECTION_SYSTEMS = {
    "curve_native_v2_1",
    "dusk_native",
}
CPMM_PREDICATE_NAMES = {
    "beats_dusk_cpmm_both_routes_median_advantage_at_least_1bp",
    "beats_dusk_cpmm_both_routes_window_win_fraction_at_least_70pct",
    "beats_dusk_cpmm_both_routes_bottom_decile_mean_not_below_minus_1bp",
    "beats_dusk_cpmm_both_routes_tail_service_regressions_within_limits",
    "beats_dusk_cpmm_every_flow_route_median_has_no_return_reversal",
    "beats_dusk_cpmm_every_flow_route_bottom_decile_not_below_minus_1bp",
    "beats_dusk_cpmm_every_flow_route_tail_service_within_limits",
}
CPMM_FLOW_ROUTE_KEYS = {
    f"{flow}:{route}"
    for flow in ("benign", "mixed", "toxic_delayed", "low_volume_stuck")
    for route in ("fixed", "elastic")
}
CPMM_AGGREGATE_METRIC_FIELDS = {
    "median_return_advantage_bps",
    "window_median_win_fraction",
    "bottom_decile_mean_return_advantage_bps",
    "p90_execution_regression_bps",
    "p10_capture_advantage_fraction",
    "p90_marginal_tracking_stale_regression_fraction",
}


def _selection_policy_is_complete(selection_policy: dict[str, object]) -> bool:
    minimum_relaxations = selection_policy.get(
        "minimum_required_relaxation_by_system"
    )
    systems = selection_policy.get("systems")
    return (
        isinstance(minimum_relaxations, dict)
        and set(minimum_relaxations) == CONCENTRATED_SELECTION_SYSTEMS
        and isinstance(systems, (list, tuple, set))
        and set(systems) == CONCENTRATED_SELECTION_SYSTEMS
    )


def cpmm_product_selection_is_consistent(
    selection_policy: dict[str, object],
) -> bool:
    """Validate the independent CPMM lock without promoting it to ready."""

    minimum_relaxations = selection_policy.get(
        "minimum_required_relaxation_by_system"
    )
    try:
        relaxation = float(selection_policy["service_relaxation"])
        minimum = float(minimum_relaxations["dusk_cpmm"])
    except (KeyError, TypeError, ValueError, OverflowError):
        return False
    expected_ready = (
        math.isfinite(relaxation)
        and relaxation >= 0.0
        and abs(relaxation - minimum) <= SERVICE_RELAXATION_TOLERANCE
        and relaxation <= SERVICE_RELAXATION_TOLERANCE
        and selection_policy.get(
            "selected_profile_original_service_envelope_pass"
        )
        is True
    )
    return (
        selection_policy.get("system") == "dusk_cpmm"
        and isinstance(minimum_relaxations, dict)
        and set(minimum_relaxations) == {"dusk_cpmm"}
        and math.isfinite(minimum)
        and minimum >= 0.0
        and abs(relaxation - minimum) <= SERVICE_RELAXATION_TOLERANCE
        and isinstance(selection_policy.get("selected_profile_id"), str)
        and bool(selection_policy["selected_profile_id"])
        and isinstance(
            selection_policy.get(
                "selected_profile_original_service_envelope_pass"
            ),
            bool,
        )
        and selection_policy.get(
            "contaminates_concentrated_candidate_selection"
        )
        is False
        and selection_policy.get("product_ready_on_training_service")
        is expected_ready
    )


def dusk_product_selection_is_consistent(
    selection_policy: dict[str, object],
) -> bool:
    """Validate Dusk readiness independently of Curve's comparative rho."""

    try:
        relaxation = float(selection_policy["service_relaxation"])
    except (KeyError, TypeError, ValueError, OverflowError):
        return False
    expected_ready = (
        math.isfinite(relaxation)
        and 0.0 <= relaxation <= SERVICE_RELAXATION_TOLERANCE
        and selection_policy.get(
            "selected_profile_original_service_envelope_pass"
        )
        is True
    )
    return (
        selection_policy.get("system") == "dusk_native"
        and isinstance(selection_policy.get("selected_profile_id"), str)
        and bool(selection_policy["selected_profile_id"])
        and math.isfinite(relaxation)
        and relaxation >= 0.0
        and selection_policy.get(
            "independent_from_common_curve_dh2_relaxation"
        )
        is True
        and isinstance(
            selection_policy.get(
                "selected_profile_original_service_envelope_pass"
            ),
            bool,
        )
        and selection_policy.get("product_ready_on_training_service")
        is expected_ready
    )


def _deployment_ready(selection_policy: dict[str, object]) -> bool:
    """Require a three-family lock, readiness, and an unrelaxed envelope."""

    try:
        common_relaxation = float(selection_policy["common_service_relaxation"])
    except (KeyError, TypeError, ValueError):
        return False
    return (
        _selection_policy_is_complete(selection_policy)
        and selection_policy.get("comparison_pair_ready_on_training_service")
        is True
        and math.isfinite(common_relaxation)
        and 0.0 <= common_relaxation <= SERVICE_RELAXATION_TOLERANCE
    )


def expected_fee_adjusted_native_winner(
    selection_policy: dict[str, object],
    decision: dict[str, object],
) -> str | None:
    candidates = decision.get("fee_adjusted", {})
    if not isinstance(candidates, dict) or not _deployment_ready(selection_policy):
        return None
    dusk = candidates.get("dusk", {})
    curve = candidates.get("curve", {})
    dusk_passes = isinstance(dusk, dict) and bool(dusk.get("passes"))
    curve_passes = isinstance(curve, dict) and bool(curve.get("passes"))
    if dusk_passes and not curve_passes:
        return "dusk_current_dh2"
    if curve_passes and not dusk_passes:
        return "curve_twocrypto_v2_1"
    return None


def _gross_outcome_is_scoped(decision: dict[str, object]) -> bool:
    gross = decision.get("gross_outcome_for_fee_adjusted_selected_profiles")
    if not isinstance(gross, dict):
        return False
    if gross.get("selection_basis") != "fee_adjusted_synthetic_profile_lock":
        return False
    if gross.get("universal_claim") is not False:
        return False
    for candidate in ("dusk", "curve"):
        diagnostics = gross.get(candidate)
        if not isinstance(diagnostics, dict):
            return False
        if "passes" in diagnostics:
            return False
        if not isinstance(
            diagnostics.get("meets_same_diagnostic_robustness_gate"), bool
        ):
            return False
    return True


def _neighborhood_eligibility_is_valid(decision: dict[str, object]) -> bool:
    eligibility = decision.get("neighborhood_service_eligibility")
    if not isinstance(eligibility, dict):
        return False
    if set(eligibility) != {"curve_native_v2_1", "dusk_native"}:
        return False
    for counts in eligibility.values():
        if not isinstance(counts, dict):
            return False
        try:
            total_profiles = int(counts["total_profiles"])
            eligible_profiles = int(counts["eligible_profiles"])
            excluded_profiles = int(counts["excluded_profiles"])
            total_rows = int(counts["total_rows"])
            eligible_rows = int(counts["eligible_rows"])
            excluded_rows = int(counts["excluded_rows"])
        except (KeyError, TypeError, ValueError, OverflowError):
            return False
        if not (
            total_profiles > 0
            and 0 < eligible_profiles <= total_profiles
            and excluded_profiles == total_profiles - eligible_profiles
            and total_rows > 0
            and 0 < eligible_rows <= total_rows
            and excluded_rows == total_rows - eligible_rows
            and counts.get("selected_profile_eligible") is True
        ):
            return False
    return True


def _cpmm_gate_is_present(decision: dict[str, object]) -> bool:
    candidates = decision.get("fee_adjusted")
    if not isinstance(candidates, dict) or set(candidates) != {"dusk", "curve"}:
        return False
    for result in candidates.values():
        if not isinstance(result, dict):
            return False
        predicates = result.get("predicates")
        by_route = result.get("vs_dusk_cpmm_by_route")
        by_flow_route = result.get("vs_dusk_cpmm_by_flow_and_route")
        if (
            not isinstance(predicates, dict)
            or not CPMM_PREDICATE_NAMES.issubset(predicates)
            or not isinstance(by_route, dict)
            or set(by_route) != {"fixed", "elastic"}
            or not isinstance(by_flow_route, dict)
            or set(by_flow_route) != CPMM_FLOW_ROUTE_KEYS
        ):
            return False
        try:
            aggregate_rows = (*by_route.values(), *by_flow_route.values())
            if not all(
                isinstance(row, dict)
                and CPMM_AGGREGATE_METRIC_FIELDS.issubset(row)
                and all(
                    math.isfinite(float(row[field]))
                    for field in CPMM_AGGREGATE_METRIC_FIELDS
                )
                for row in aggregate_rows
            ):
                return False
            service_safe = lambda groups: all(
                float(row["p90_execution_regression_bps"]) <= 1.0
                and float(row["p10_capture_advantage_fraction"]) >= -0.02
                and float(
                    row["p90_marginal_tracking_stale_regression_fraction"]
                )
                <= 0.05
                for row in groups
            )
            expected = {
                "beats_dusk_cpmm_both_routes_median_advantage_at_least_1bp": all(
                    float(row["median_return_advantage_bps"]) >= 1.0
                    for row in by_route.values()
                ),
                "beats_dusk_cpmm_both_routes_window_win_fraction_at_least_70pct": all(
                    float(row["window_median_win_fraction"]) >= 0.70
                    for row in by_route.values()
                ),
                "beats_dusk_cpmm_both_routes_bottom_decile_mean_not_below_minus_1bp": all(
                    float(row["bottom_decile_mean_return_advantage_bps"]) >= -1.0
                    for row in by_route.values()
                ),
                "beats_dusk_cpmm_both_routes_tail_service_regressions_within_limits": service_safe(
                    by_route.values()
                ),
                "beats_dusk_cpmm_every_flow_route_median_has_no_return_reversal": all(
                    float(row["median_return_advantage_bps"]) >= 0.0
                    for row in by_flow_route.values()
                ),
                "beats_dusk_cpmm_every_flow_route_bottom_decile_not_below_minus_1bp": all(
                    float(row["bottom_decile_mean_return_advantage_bps"]) >= -1.0
                    for row in by_flow_route.values()
                ),
                "beats_dusk_cpmm_every_flow_route_tail_service_within_limits": service_safe(
                    by_flow_route.values()
                ),
            }
        except (KeyError, TypeError, ValueError, OverflowError):
            return False
        if any(
            not isinstance(predicates[name], bool)
            or predicates[name] is not expected[name]
            for name in CPMM_PREDICATE_NAMES
        ):
            return False
        if not isinstance(result.get("passes"), bool) or result["passes"] is not all(
            value is True for value in predicates.values()
        ):
            return False
    return True


def native_decision_is_consistent(
    selection_policy: dict[str, object],
    decision: dict[str, object],
) -> bool:
    fee_winner = expected_fee_adjusted_native_winner(
        selection_policy,
        decision,
    )
    removed_legacy_fields = {
        "universal_native_winner",
        "gross_mechanism_universal_winner",
        "gross_mechanism",
    }
    return (
        _selection_policy_is_complete(selection_policy)
        and "fee_adjusted_universal_native_winner" in decision
        and decision.get("fee_adjusted_universal_native_winner") == fee_winner
        and not removed_legacy_fields.intersection(decision)
        and _gross_outcome_is_scoped(decision)
        and _neighborhood_eligibility_is_valid(decision)
        and _cpmm_gate_is_present(decision)
    )

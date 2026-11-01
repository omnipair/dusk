#!/usr/bin/env python3
"""Fail-closed audit and canonicalization of native benchmark outputs."""

from __future__ import annotations

import argparse
import csv
import json
import math
from pathlib import Path
import tempfile

from native_validation import (
    cpmm_product_selection_is_consistent,
    dusk_product_selection_is_consistent,
    expected_fee_adjusted_native_winner,
    native_decision_is_consistent,
)
from native_replay import ARBITRAGE_CAP_COUNTER_SCOPE
from final_report_evidence import (
    expected_native_counts,
    validate_search_and_promotions,
    write_final_report_audit,
)


ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"
ACCOUNTING_ERROR_LIMIT = 1e-5
COUNT_FILES = {
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
AUDIT_FILES = tuple(
    filename
    for field, filename in COUNT_FILES.items()
    if field
    not in {"profile_rows", "paired_rows", "fee_split_sensitivity_rows"}
)
DECISION_METRIC_FIELDS = (
    "net_lp_return_after_protocol_fee_bps",
    "net_lp_return_vs_hold_bps",
    "captured_user_volume_fraction",
    "selected_execution_cost_bps",
    "depth_degradation_fraction",
    "marginal_tracking_stale_fraction",
    "accounting_identity_error",
)


def read_rows(filename: str) -> list[dict[str, str]]:
    with (RESULTS / filename).open() as source:
        return list(csv.DictReader(source))


def neighborhood_eligibility_from_rows(
    profile_rows: list[dict[str, str]],
    neighborhood_rows: list[dict[str, str]],
) -> dict[str, dict[str, int | bool]]:
    systems = ("curve_native_v2_1", "dusk_native")
    diagnostics = {
        (row["system"], row["profile_id"]): row for row in profile_rows
    }
    if len(diagnostics) != len(profile_rows):
        raise AssertionError("profile-selection output contains duplicate diagnostics")
    for row in neighborhood_rows:
        if (row["system"], row["profile_id"]) not in diagnostics:
            raise AssertionError("neighborhood row lacks profile diagnostic")
    output: dict[str, dict[str, int | bool]] = {}
    for system in systems:
        profiles = {
            profile_id
            for row_system, profile_id in diagnostics
            if row_system == system
        }
        eligible = {
            profile_id
            for (row_system, profile_id), row in diagnostics.items()
            if row_system == system
            and row.get("eligible_for_lock", "").lower() == "true"
        }
        system_rows = [row for row in neighborhood_rows if row["system"] == system]
        eligible_rows = [
            row for row in system_rows if row["profile_id"] in eligible
        ]
        selected = {
            row["profile_id"]
            for row in profile_rows
            if row["system"] == system
            and row.get("selected_for_held_out", "").lower() == "true"
        }
        if len(selected) != 1 or not selected.issubset(eligible):
            raise AssertionError("selected neighborhood profile is not service eligible")
        output[system] = {
            "total_profiles": len(profiles),
            "eligible_profiles": len(eligible),
            "excluded_profiles": len(profiles) - len(eligible),
            "total_rows": len(system_rows),
            "eligible_rows": len(eligible_rows),
            "excluded_rows": len(system_rows) - len(eligible_rows),
            "selected_profile_eligible": True,
        }
    return output


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--final-report",
        action="store_true",
        help=(
            "after the immutable lock and sealed replay exist, verify the full "
            "report evidence chain without modifying native_summary.json"
        ),
    )
    args = parser.parse_args()
    summary_path = RESULTS / "native_summary.json"
    summary = json.loads(summary_path.read_text())
    if args.final_report:
        post_run = summary.get("validation", {}).get("post_run_audit", {})
        if not isinstance(post_run, dict) or post_run.get("status") != "passed":
            raise AssertionError(
                "final report audit requires the completed pre-lock native audit"
            )
        receipt = write_final_report_audit(summary, root=ROOT)
        confirmation = receipt["evidence"]["sealed_validation"]["confirmation"]
        print(
            "final report evidence audit passed; sealed economic confirmation "
            f"{str(confirmation['status']).upper()} (selection remained locked)"
        )
        return

    search_promotion = validate_search_and_promotions(summary, root=ROOT)
    canonical_counts = expected_native_counts(search_promotion)
    validation = summary.get("validation", {})
    if summary.get("status") != "complete":
        raise AssertionError(f"native status is {summary.get('status')!r}, not complete")
    if validation.get("canonical_output") is not True:
        raise AssertionError("native output is not marked canonical")
    if validation.get("expected_counts") != canonical_counts:
        raise AssertionError(
            "declared native matrix differs from the canonical matrix: "
            f"{validation.get('expected_counts')!r}"
        )
    selection_policy = summary.get("profile_selection_policy")
    if not isinstance(selection_policy, dict):
        raise AssertionError("native summary lacks the route-safe profile-selection policy")
    common_relaxation = float(selection_policy.get("common_service_relaxation", math.nan))
    if not math.isfinite(common_relaxation) or common_relaxation < 0.0:
        raise AssertionError(
            f"invalid common service relaxation: {common_relaxation!r}"
        )
    decision = summary.get("decision", {})
    expected_fee_winner = expected_fee_adjusted_native_winner(
        selection_policy,
        decision,
    )
    decision_consistent = native_decision_is_consistent(
        selection_policy,
        decision,
    )
    if decision.get("fee_adjusted_universal_native_winner") != expected_fee_winner:
        raise AssertionError("fee-adjusted universal winner is inconsistent")
    if not decision_consistent:
        raise AssertionError("native decision schema or winner is inconsistent")

    rows_by_file = {filename: read_rows(filename) for filename in COUNT_FILES.values()}
    for field, filename in COUNT_FILES.items():
        actual = len(rows_by_file[filename])
        expected = canonical_counts[field]
        if actual != expected:
            raise AssertionError(
                f"{filename} has {actual:,} rows; expected {expected:,}"
            )
        declared = validation.get(field)
        if declared is not None and int(declared) != expected:
            raise AssertionError(
                f"summary validation {field}={declared}; expected {expected}"
            )

    profile_rows = rows_by_file["native_profile_selection.csv"]
    selected_rows = [
        row
        for row in profile_rows
        if row.get("selected_for_held_out", "").lower() == "true"
    ]
    expected_selection_systems = {
        "curve_native_v2_1",
        "dusk_native",
        "dusk_cpmm",
    }
    if (
        {row["system"] for row in selected_rows} != expected_selection_systems
        or len(selected_rows) != len(expected_selection_systems)
    ):
        raise AssertionError(
            "profile selection must lock exactly one Curve, concentrated-Dusk, "
            "and independently selected Dusk-CPMM profile"
        )
    selected_ids = {
        row["system"]: row["profile_id"] for row in selected_rows
    }
    summary_selection = summary.get("profile_selection")
    if (
        not isinstance(summary_selection, dict)
        or set(summary_selection) != expected_selection_systems
        or not all(
            isinstance(value, dict) for value in summary_selection.values()
        )
    ):
        raise AssertionError("native summary has an incomplete profile lock")
    if any(
        str(summary_selection[system].get("profile_id")) != profile_id
        for system, profile_id in selected_ids.items()
    ):
        raise AssertionError(
            "native summary profile lock disagrees with profile-selection CSV"
        )
    minimum_relaxations = selection_policy.get(
        "minimum_required_relaxation_by_system"
    )
    if not isinstance(minimum_relaxations, dict) or set(minimum_relaxations) != (
        {"curve_native_v2_1", "dusk_native"}
    ):
        raise AssertionError(
            "concentrated-candidate common service envelope is incomplete"
        )
    selection_systems = selection_policy.get("systems")
    if not isinstance(selection_systems, (list, tuple, set)) or set(
        selection_systems
    ) != {
        "curve_native_v2_1",
        "dusk_native",
    }:
        raise AssertionError(
            "CPMM must not enter the concentrated-candidate selection group"
        )
    cpmm_selection_policy = summary.get("cpmm_product_selection_policy")
    if not isinstance(
        cpmm_selection_policy, dict
    ) or not cpmm_product_selection_is_consistent(cpmm_selection_policy):
        raise AssertionError("independent CPMM selection policy is inconsistent")
    if cpmm_selection_policy.get("selected_profile_id") != selected_ids[
        "dusk_cpmm"
    ]:
        raise AssertionError("CPMM policy and selected profile disagree")
    dusk_selection_policy = summary.get("dusk_product_selection_policy")
    if not isinstance(
        dusk_selection_policy, dict
    ) or not dusk_product_selection_is_consistent(dusk_selection_policy):
        raise AssertionError("independent Dusk product policy is inconsistent")
    if dusk_selection_policy.get("selected_profile_id") != selected_ids[
        "dusk_native"
    ]:
        raise AssertionError("Dusk product policy and selected profile disagree")
    candidate_selected_rows = {
        row["system"]: row
        for row in selected_rows
        if row["system"] in {"curve_native_v2_1", "dusk_native"}
    }
    if any(
        row.get("selection_group") != "concentrated_candidates"
        or abs(
            float(row["selection_group_service_relaxation"])
            - common_relaxation
        )
        > 1e-12
        for row in candidate_selected_rows.values()
    ):
        raise AssertionError("concentrated profiles do not share their own rho")
    selected_cpmm_row = next(
        row for row in selected_rows if row["system"] == "dusk_cpmm"
    )
    selected_dusk_row = next(
        row for row in selected_rows if row["system"] == "dusk_native"
    )
    if (
        abs(
            float(selected_dusk_row["service_relaxation_required"])
            - float(dusk_selection_policy["service_relaxation"])
        )
        > 1e-12
        or (
            selected_dusk_row.get(
                "original_service_envelope_pass", ""
            ).lower()
            == "true"
        )
        is not dusk_selection_policy[
            "selected_profile_original_service_envelope_pass"
        ]
    ):
        raise AssertionError("Dusk product policy does not use its independent rho")
    if (
        selected_cpmm_row.get("selection_group") != "cpmm_product"
        or (
            selected_cpmm_row.get(
                "original_service_envelope_pass", ""
            ).lower()
            == "true"
        )
        is not cpmm_selection_policy[
            "selected_profile_original_service_envelope_pass"
        ]
        or abs(
            float(selected_cpmm_row["selection_group_service_relaxation"])
            - float(cpmm_selection_policy["service_relaxation"])
        )
        > 1e-12
    ):
        raise AssertionError("CPMM profile does not use its independent rho")

    cpmm_product = summary.get("cpmm_product_fallback")
    cpmm_ablation = summary.get("cpmm_matched_policy_ablation")
    if (
        not isinstance(cpmm_product, dict)
        or cpmm_product.get("participates_in_winner_gate") is not True
        or cpmm_product.get("control_system") != "dusk_cpmm"
        or cpmm_product.get("control_profile_id")
        != selected_ids["dusk_cpmm"]
        or cpmm_product.get("product_ready_on_training_service")
        is not cpmm_selection_policy["product_ready_on_training_service"]
        or abs(
            float(cpmm_product.get("service_relaxation", math.nan))
            - float(cpmm_selection_policy["service_relaxation"])
        )
        > 1e-12
    ):
        raise AssertionError(
            "CPMM product-fallback summary is not independently locked"
        )
    if (
        not isinstance(cpmm_ablation, dict)
        or cpmm_ablation.get("participates_in_winner_gate") is not False
        or cpmm_ablation.get("control_system")
        != "dusk_cpmm_matched_policy"
        or cpmm_ablation.get("control_profile_id")
        != selected_ids["dusk_native"]
    ):
        raise AssertionError("matched-policy CPMM ablation is mislabeled")
    product_rows = rows_by_file["native_cpmm_control.csv"]
    ablation_rows = rows_by_file[
        "native_cpmm_matched_policy_ablation.csv"
    ]
    if {
        (row["system"], row["profile_id"]) for row in product_rows
    } != {("dusk_cpmm", selected_ids["dusk_cpmm"])}:
        raise AssertionError("CPMM product replay does not use its selected profile")
    if {
        (row["system"], row["profile_id"]) for row in ablation_rows
    } != {("dusk_cpmm_matched_policy", selected_ids["dusk_native"])}:
        raise AssertionError("CPMM ablation does not use the selected Dusk policy")

    actual_neighborhood_eligibility = neighborhood_eligibility_from_rows(
        rows_by_file["native_profile_selection.csv"],
        rows_by_file["native_neighborhood.csv"],
    )
    if decision.get("neighborhood_service_eligibility") != (
        actual_neighborhood_eligibility
    ):
        raise AssertionError(
            "native neighborhood eligibility counts disagree with profile diagnostics"
        )

    audit_rows = [row for filename in AUDIT_FILES for row in rows_by_file[filename]]
    if not all(
        field in row and math.isfinite(float(row[field]))
        for row in audit_rows
        for field in DECISION_METRIC_FIELDS
    ):
        raise AssertionError(
            "native decision-metric audit contains missing or non-finite "
            "new-schema values; rerun native_benchmark.py"
        )
    errors = [abs(float(row["accounting_identity_error"])) for row in audit_rows]
    if not errors or not all(math.isfinite(value) for value in errors):
        raise AssertionError("native accounting audit contains no rows or non-finite values")
    maximum_error = max(errors)
    if maximum_error > ACCOUNTING_ERROR_LIMIT:
        raise AssertionError(
            f"maximum accounting identity error {maximum_error:.6g} exceeds "
            f"{ACCOUNTING_ERROR_LIMIT:.6g}"
        )
    maximum_external_reads = max(
        int(row["controller_external_price_reads"]) for row in audit_rows
    )
    if maximum_external_reads != 0:
        raise AssertionError(
            "controller isolation failed: external reference price read count "
            f"reached {maximum_external_reads}"
        )
    api_isolation = validation.get("controller_api_oracle_isolation")
    if (
        not isinstance(api_isolation, dict)
        or api_isolation.get("passes") is not True
        or api_isolation.get("violations") != {}
    ):
        raise AssertionError(
            "controller API isolation audit is missing or exposes an "
            "external/oracle price parameter"
        )
    cap_fields = {
        "arbitrage_capped_event_count",
        "scored_arbitrage_capped_event_count",
        "arbitrage_numerical_failure_count",
        "scored_arbitrage_numerical_failure_count",
    }
    if validation.get("arbitrage_cap_counter_scope") != ARBITRAGE_CAP_COUNTER_SCOPE:
        raise AssertionError(
            "native summary uses a stale scored-only arbitrage cap scope"
        )
    if not all(cap_fields.issubset(row) for row in audit_rows):
        raise AssertionError(
            "arbitrage cascade audit rows use a stale scored-only counter schema"
        )
    capped_arbitrage_events = sum(
        int(row["arbitrage_capped_event_count"]) for row in audit_rows
    )
    scored_capped_arbitrage_events = sum(
        int(row["scored_arbitrage_capped_event_count"]) for row in audit_rows
    )
    arbitrage_numerical_failures = sum(
        int(row["arbitrage_numerical_failure_count"]) for row in audit_rows
    )
    scored_arbitrage_numerical_failures = sum(
        int(row["scored_arbitrage_numerical_failure_count"])
        for row in audit_rows
    )
    if any(
        int(row["scored_arbitrage_capped_event_count"])
        > int(row["arbitrage_capped_event_count"])
        for row in audit_rows
    ):
        raise AssertionError("scored capped-event count exceeds all-period count")
    if any(
        int(row["scored_arbitrage_numerical_failure_count"])
        > int(row["arbitrage_numerical_failure_count"])
        for row in audit_rows
    ):
        raise AssertionError(
            "scored arbitrage numerical-failure count exceeds all-period count"
        )
    if capped_arbitrage_events != 0:
        raise AssertionError(
            "arbitrage cascade audit found unresolved hard-cap events during "
            "burn-in or scoring: "
            f"{capped_arbitrage_events}"
        )
    if arbitrage_numerical_failures != 0:
        raise AssertionError(
            "arbitrage search audit found arithmetic, overflow, or solver "
            "failures during burn-in or scoring: "
            f"{arbitrage_numerical_failures}"
        )

    for field, expected in canonical_counts.items():
        validation[field] = expected
    validation["maximum_accounting_identity_error"] = maximum_error
    validation["all_decision_metrics_finite"] = True
    validation["controller_external_price_reads"] = maximum_external_reads
    validation["arbitrage_capped_event_count"] = capped_arbitrage_events
    validation["scored_arbitrage_capped_event_count"] = (
        scored_capped_arbitrage_events
    )
    validation["arbitrage_numerical_failure_count"] = (
        arbitrage_numerical_failures
    )
    validation["scored_arbitrage_numerical_failure_count"] = (
        scored_arbitrage_numerical_failures
    )
    validation["arbitrage_cap_counter_scope"] = ARBITRAGE_CAP_COUNTER_SCOPE
    validation["post_run_audit"] = {
        "status": "passed",
        "script": "audit_native_outputs.py",
        "script_version": 3,
        "scope": "all_underlying_replay_matrices",
        "audited_files": list(AUDIT_FILES),
        "audited_rows": len(audit_rows),
        "accounting_error_limit": ACCOUNTING_ERROR_LIMIT,
        "controller_api_oracle_isolation": api_isolation,
        "arbitrage_capped_event_count": capped_arbitrage_events,
        "scored_arbitrage_capped_event_count": scored_capped_arbitrage_events,
        "arbitrage_numerical_failure_count": arbitrage_numerical_failures,
        "scored_arbitrage_numerical_failure_count": (
            scored_arbitrage_numerical_failures
        ),
        "arbitrage_cap_counter_scope": ARBITRAGE_CAP_COUNTER_SCOPE,
    }
    validation["search_promotion_audit"] = {
        "status": "passed",
        "symmetric_concentrated_search": True,
        "independent_cpmm_search": True,
        "profiles_per_family": search_promotion["profiles_per_family"],
        "cells_per_profile": search_promotion["cells_per_profile"],
        "rows_per_family": search_promotion["rows_per_family"],
        "promotions": {
            system: {
                "search_summary_sha256": row["search_summary_sha256"],
                "search_fingerprint": row["search_fingerprint"],
                "promotion_sha256": row["promotion_sha256"],
                "promotion_fingerprint": row["promotion_fingerprint"],
                "search_winner_profile_id": row["search_winner_profile_id"],
                "canonical_locked_profile_id": row[
                    "canonical_locked_profile_id"
                ],
                "promoted_profiles": row["promoted_profiles"],
            }
            for system, row in search_promotion["promotions"].items()
        },
    }

    payload = json.dumps(summary, indent=2) + "\n"
    with tempfile.NamedTemporaryFile(
        "w", dir=RESULTS, prefix=".native-summary-audit-", delete=False
    ) as target:
        target.write(payload)
        temporary_path = Path(target.name)
    temporary_path.replace(summary_path)
    print(
        "native post-run audit passed: "
        f"{len(audit_rows):,} replay rows, max accounting error "
        f"{maximum_error:.3e}, external price reads {maximum_external_reads}"
    )


if __name__ == "__main__":
    main()

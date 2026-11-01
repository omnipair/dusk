from __future__ import annotations

import sys
import json
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from native_validation import (
    CPMM_FLOW_ROUTE_KEYS,
    CPMM_PREDICATE_NAMES,
    cpmm_product_selection_is_consistent,
    dusk_product_selection_is_consistent,
    expected_fee_adjusted_native_winner,
    native_decision_is_consistent,
)


PROFILE_RELAXATIONS = {
    "curve_native_v2_1": 0.0,
    "dusk_native": 0.0,
}
PROFILE_SYSTEMS = ["curve_native_v2_1", "dusk_native"]


def fabricated_decision(
    *,
    fee_dusk_passes: bool = True,
    fee_curve_passes: bool = False,
    fee_winner: str | None = None,
) -> dict[str, object]:
    diagnostic = {
        "meets_same_diagnostic_robustness_gate": False,
        "predicates": {},
    }
    cpmm_metrics = {
        "median_return_advantage_bps": 1.0,
        "window_median_win_fraction": 0.70,
        "bottom_decile_mean_return_advantage_bps": -1.0,
        "p90_execution_regression_bps": 1.0,
        "p10_capture_advantage_fraction": -0.02,
        "p90_marginal_tracking_stale_regression_fraction": 0.05,
    }
    fee_candidate = lambda passes: {
        "passes": passes,
        "predicates": {
            **{name: True for name in CPMM_PREDICATE_NAMES},
            "other_gate": passes,
        },
        "vs_dusk_cpmm_by_route": {
            "fixed": dict(cpmm_metrics),
            "elastic": dict(cpmm_metrics),
        },
        "vs_dusk_cpmm_by_flow_and_route": {
            key: dict(cpmm_metrics) for key in CPMM_FLOW_ROUTE_KEYS
        },
    }
    return {
        "fee_adjusted": {
            "dusk": fee_candidate(fee_dusk_passes),
            "curve": fee_candidate(fee_curve_passes),
        },
        "fee_adjusted_universal_native_winner": fee_winner,
        "gross_outcome_for_fee_adjusted_selected_profiles": {
            "selection_basis": "fee_adjusted_synthetic_profile_lock",
            "universal_claim": False,
            "scope": "diagnostic only",
            "dusk": dict(diagnostic),
            "curve": dict(diagnostic),
        },
        "neighborhood_service_eligibility": {
            system: {
                "total_profiles": 2,
                "eligible_profiles": 1,
                "excluded_profiles": 1,
                "total_rows": 4,
                "eligible_rows": 2,
                "excluded_rows": 2,
                "selected_profile_eligible": True,
            }
            for system in ("curve_native_v2_1", "dusk_native")
        },
    }


class NativeDecisionValidationTests(unittest.TestCase):
    def test_positive_common_relaxation_forces_winner_to_none(self):
        policy = {
            "comparison_pair_ready_on_training_service": True,
            "common_service_relaxation": 0.01,
            "minimum_required_relaxation_by_system": PROFILE_RELAXATIONS,
            "systems": PROFILE_SYSTEMS,
        }
        decision = fabricated_decision(fee_winner="dusk_current_dh2")

        self.assertIsNone(expected_fee_adjusted_native_winner(policy, decision))
        self.assertFalse(native_decision_is_consistent(policy, decision))

        decision["fee_adjusted_universal_native_winner"] = None
        self.assertTrue(native_decision_is_consistent(policy, decision))

    def test_not_deployment_ready_forces_winner_to_none(self):
        policy = {
            "comparison_pair_ready_on_training_service": False,
            "common_service_relaxation": 0.0,
            "minimum_required_relaxation_by_system": PROFILE_RELAXATIONS,
            "systems": PROFILE_SYSTEMS,
        }
        decision = fabricated_decision(fee_winner="dusk_current_dh2")

        self.assertIsNone(expected_fee_adjusted_native_winner(policy, decision))
        self.assertFalse(native_decision_is_consistent(policy, decision))

        decision["fee_adjusted_universal_native_winner"] = None
        self.assertTrue(native_decision_is_consistent(policy, decision))

    def test_deployment_ready_maps_exactly_one_fee_adjusted_pass(self):
        policy = {
            "comparison_pair_ready_on_training_service": True,
            "common_service_relaxation": 0.0,
            "minimum_required_relaxation_by_system": PROFILE_RELAXATIONS,
            "systems": PROFILE_SYSTEMS,
        }
        decision = fabricated_decision(fee_winner="dusk_current_dh2")

        self.assertEqual(
            expected_fee_adjusted_native_winner(policy, decision),
            "dusk_current_dh2",
        )
        self.assertTrue(native_decision_is_consistent(policy, decision))

    def test_cpmm_cannot_contaminate_concentrated_profile_lock(self):
        policy = {
            "comparison_pair_ready_on_training_service": True,
            "common_service_relaxation": 0.0,
            "minimum_required_relaxation_by_system": {
                "curve_native_v2_1": 0.0,
                "dusk_native": 0.0,
                "dusk_cpmm": 1.0,
            },
            "systems": ["curve_native_v2_1", "dusk_native", "dusk_cpmm"],
        }
        decision = fabricated_decision(fee_winner="dusk_current_dh2")

        self.assertIsNone(expected_fee_adjusted_native_winner(policy, decision))
        self.assertFalse(native_decision_is_consistent(policy, decision))

        policy["minimum_required_relaxation_by_system"].pop("dusk_cpmm")
        policy["systems"].remove("dusk_cpmm")
        self.assertEqual(
            expected_fee_adjusted_native_winner(policy, decision),
            "dusk_current_dh2",
        )
        self.assertTrue(native_decision_is_consistent(policy, decision))

    def test_cpmm_positive_relaxation_is_comparable_but_not_product_ready(self):
        policy = {
            "system": "dusk_cpmm",
            "service_relaxation": 1.0,
            "minimum_required_relaxation_by_system": {"dusk_cpmm": 1.0},
            "selected_profile_id": "dynamic_high",
            "selected_profile_original_service_envelope_pass": False,
            "product_ready_on_training_service": False,
            "contaminates_concentrated_candidate_selection": False,
        }
        self.assertTrue(cpmm_product_selection_is_consistent(policy))
        policy["product_ready_on_training_service"] = True
        self.assertFalse(cpmm_product_selection_is_consistent(policy))

    def test_dusk_product_readiness_is_independent_of_curve_relaxation(self):
        policy = {
            "system": "dusk_native",
            "service_relaxation": 0.0,
            "selected_profile_id": "search_winner",
            "selected_profile_original_service_envelope_pass": True,
            "product_ready_on_training_service": True,
            "independent_from_common_curve_dh2_relaxation": True,
        }
        self.assertTrue(dusk_product_selection_is_consistent(policy))
        policy["service_relaxation"] = 0.01
        policy["selected_profile_original_service_envelope_pass"] = False
        policy["product_ready_on_training_service"] = False
        self.assertTrue(dusk_product_selection_is_consistent(policy))
        policy["product_ready_on_training_service"] = True
        self.assertFalse(dusk_product_selection_is_consistent(policy))

    def test_gross_outcome_cannot_be_labeled_universal(self):
        policy = {
            "comparison_pair_ready_on_training_service": True,
            "common_service_relaxation": 0.0,
            "minimum_required_relaxation_by_system": PROFILE_RELAXATIONS,
            "systems": PROFILE_SYSTEMS,
        }
        decision = fabricated_decision(fee_winner="dusk_current_dh2")
        decision["gross_mechanism_universal_winner"] = "curve_twocrypto_v2_1"
        self.assertFalse(native_decision_is_consistent(policy, decision))

        decision.pop("gross_mechanism_universal_winner")
        gross = decision["gross_outcome_for_fee_adjusted_selected_profiles"]
        self.assertIsInstance(gross, dict)
        gross["universal_claim"] = True
        self.assertFalse(native_decision_is_consistent(policy, decision))

    def test_missing_cpmm_gate_fails_closed(self):
        policy = {
            "comparison_pair_ready_on_training_service": True,
            "common_service_relaxation": 0.0,
            "minimum_required_relaxation_by_system": PROFILE_RELAXATIONS,
            "systems": PROFILE_SYSTEMS,
        }
        decision = fabricated_decision(fee_winner="dusk_current_dh2")
        decision["fee_adjusted"]["dusk"]["predicates"].pop(
            "beats_dusk_cpmm_both_routes_median_advantage_at_least_1bp"
        )
        self.assertFalse(native_decision_is_consistent(policy, decision))

    def test_nonfinite_cpmm_aggregate_fails_closed(self):
        policy = {
            "comparison_pair_ready_on_training_service": True,
            "common_service_relaxation": 0.0,
            "minimum_required_relaxation_by_system": PROFILE_RELAXATIONS,
            "systems": PROFILE_SYSTEMS,
        }
        decision = fabricated_decision(fee_winner="dusk_current_dh2")
        decision["fee_adjusted"]["dusk"]["vs_dusk_cpmm_by_route"][
            "fixed"
        ]["median_return_advantage_bps"] = float("inf")
        self.assertFalse(native_decision_is_consistent(policy, decision))

    def test_noncanonical_cpmm_flow_route_keys_fail_closed(self):
        policy = {
            "comparison_pair_ready_on_training_service": True,
            "common_service_relaxation": 0.0,
            "minimum_required_relaxation_by_system": PROFILE_RELAXATIONS,
            "systems": PROFILE_SYSTEMS,
        }
        decision = fabricated_decision(fee_winner="dusk_current_dh2")
        flow_routes = decision["fee_adjusted"]["dusk"][
            "vs_dusk_cpmm_by_flow_and_route"
        ]
        removed = flow_routes.pop("benign:fixed")
        flow_routes["invented:fixed"] = removed
        self.assertEqual(len(flow_routes), 8)
        self.assertFalse(native_decision_is_consistent(policy, decision))


class NativePresentationContractTests(unittest.TestCase):
    def test_notebook_revalidates_decision_before_display(self):
        notebook = json.loads((ROOT / "native_controller_analysis.ipynb").read_text())
        source = "\n".join(
            "".join(cell.get("source", [])) for cell in notebook["cells"]
        )
        self.assertIn(
            "native_decision_is_consistent",
            source,
        )
        self.assertIn("cpmm_product_selection_is_consistent", source)
        self.assertIn("dusk_product_selection_is_consistent", source)
        self.assertIn(
            "assert native_decision_is_consistent(summary['profile_selection_policy'], summary['decision'])",
            source,
        )
        self.assertIn(
            "assert cpmm_product_selection_is_consistent(summary['cpmm_product_selection_policy'])",
            source,
        )
        self.assertIn(
            "assert dusk_product_selection_is_consistent(summary['dusk_product_selection_policy'])",
            source,
        )
        self.assertIn("marginal_tracking_stale_fraction", source)
        self.assertIn("depth_degradation_fraction", source)
        self.assertNotIn("all_in_stale_fraction", source)
        self.assertNotIn("fee_free_impact_stale_fraction", source)

    def test_native_route_artifacts_cite_summary_and_center_movement(self):
        source = (ROOT / "build_report.py").read_text()
        chart = source[
            source.index('"id": "native-route-return"') :
            source.index('"id": "native-flow-return"')
        ]
        table = source[
            source.index('"id": "native-route-table"') :
            source.index('"id": "native-basis-table"')
        ]
        for block in (chart, table):
            self.assertIn('"path": "results/native_summary.json"', block)
            self.assertNotIn('"path": "results/native_paired.csv"', block)
        self.assertIn("Enabling center movement changes", source)
        self.assertNotIn("Enabling the native controller changes", source)

    def test_report_separates_readiness_from_service_diagnostics(self):
        source = (ROOT / "build_report.py").read_text()
        self.assertIn('native["dusk_product_selection_policy"]', source)
        self.assertIn("independent Dusk rho", source)
        self.assertIn("Marginal-price tracking defines stale readiness", source)
        self.assertIn("depth degradation (diagnostic only)", source)
        self.assertNotIn('"all_in_stale_fraction"', source)
        self.assertNotIn('"fee_free_impact_stale_fraction"', source)


if __name__ == "__main__":
    unittest.main()

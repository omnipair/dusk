import csv
import hashlib
import json
import math
import statistics
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from build_report import binary_routing_summary, displayed_pareto_rows


def read_rows(filename):
    with (ROOT / "results" / filename).open() as source:
        return list(csv.DictReader(source))


class OutputTests(unittest.TestCase):
    def test_report_projection_helpers_ignore_excluded_family(self):
        pareto_rows = [
            {
                "model": "curve",
                "execution_index_bps": "1",
                "jump_loss_index_bps": "2",
                "combined_pareto": "False",
            },
            {
                "model": "current_dh2",
                "execution_index_bps": "2",
                "jump_loss_index_bps": "1",
                "combined_pareto": "False",
            },
            {
                "model": "cpmm",
                "execution_index_bps": "3",
                "jump_loss_index_bps": "3",
                "combined_pareto": "True",
            },
            {
                "model": "proposed_dh2",
                "execution_index_bps": "0.1",
                "jump_loss_index_bps": "0.1",
                "combined_pareto": "True",
            },
        ]
        displayed = displayed_pareto_rows(pareto_rows)
        self.assertEqual(
            {row["model"] for row in displayed},
            {"curve", "current_dh2", "cpmm"},
        )
        self.assertEqual(
            {
                row["model"]
                for row in displayed
                if bool(row["combined_pareto"])
            },
            {"curve", "current_dh2"},
        )

        routing_rows = []
        for price, curve_cost, current_cost in (
            ("1.0", "1", "2"),
            ("1.1", "3", "2"),
        ):
            for model, cost in (
                ("curve", curve_cost),
                ("current_dh2", current_cost),
                ("proposed_dh2", "0.1"),
            ):
                routing_rows.append(
                    {
                        "normalized_marginal_price": price,
                        "trade_bps_of_marked_tvl": "100",
                        "direction": "x_to_y",
                        "model": model,
                        "execution_cost_bps": cost,
                    }
                )
        routing = {
            row["model_id"]: row
            for row in binary_routing_summary(routing_rows)
        }
        self.assertEqual(set(routing), {"curve", "current_dh2"})
        self.assertEqual(
            {
                model: float(row["best_quote_intent_share"])
                for model, row in routing.items()
            },
            {"curve": 0.5, "current_dh2": 0.5},
        )

    def test_frozen_history_matches_metadata(self):
        metadata = json.loads(
            (ROOT / "data" / "history_metadata.json").read_text()
        )
        payload = (ROOT / "data" / "solusdt_1m.csv").read_bytes()
        self.assertEqual(
            hashlib.sha256(payload).hexdigest(),
            metadata["snapshot_sha256"],
        )
        with (ROOT / "data" / "solusdt_1m.csv").open() as source:
            rows = list(csv.DictReader(source))
        self.assertEqual(len(rows), metadata["rows"])
        self.assertEqual(len(rows), 3_600)
        self.assertEqual(
            len({row["source_month"] for row in rows}),
            5,
        )

        native_metadata = json.loads(
            (ROOT / "data" / "native_history_metadata.json").read_text()
        )
        native_payload = (ROOT / "data" / "solusdt_native_windows.csv").read_bytes()
        self.assertEqual(
            hashlib.sha256(native_payload).hexdigest(),
            native_metadata["snapshot_sha256"],
        )
        self.assertEqual(len(native_metadata["windows"]), 15)
        self.assertEqual(
            {row["stratum"] for row in native_metadata["windows"]},
            {"calm", "typical", "stress"},
        )

    def test_result_matrix_is_complete(self):
        expected = {
            "parameter_matching.csv": 9,
            "pareto.csv": 240,
            # Four models x 11 price states x 12 trade sizes x two
            # directions x two order types.
            "shape.csv": 2_112,
            # Three concentrated models x 11 price states x 12 trade sizes x
            # two directions.
            "routing.csv": 792,
            "path_results.csv": 156,
            "paired_differences.csv": 104,
            "regime_summary.csv": 8,
            "safety.csv": 6,
            "decay_profile.csv": 12,
            "same_parameter_deltas.csv": 20,
            "sharp_root_evidence.csv": 3,
            "hyperbolic_p4_comparison.csv": 4,
            "hyperbolic_p4_root_scan.csv": 41,
        }
        for filename, expected_rows in expected.items():
            with self.subTest(filename=filename):
                self.assertEqual(len(read_rows(filename)), expected_rows)

    def test_accounting_identity_reconciles_every_path(self):
        errors = [
            abs(float(row["accounting_identity_error"]))
            for row in read_rows("path_results.csv")
        ]
        self.assertLess(max(errors), 1e-5)

    def test_report_ranks_curve_and_current_dh2_with_cpmm_control(self):
        report = (ROOT / "DECISION_REPORT.md").read_text()
        self.assertIn("Curve CryptoSwap vs current DH2, with CPMM control", report)
        self.assertIn("zero-concentration control", report)
        self.assertIn("Native system", report)
        self.assertIn("independent Dusk rho", report)
        self.assertIn("Marginal-price tracking defines stale readiness", report)
        self.assertNotIn("all-in stale time", report.lower())
        self.assertNotIn("fee-free stale time", report.lower())
        self.assertNotIn("Native Curve versus native Dusk remains unranked", report)

        validation = (ROOT / "VALIDATION.md").read_text()
        self.assertIn("native economic comparison complete", validation)
        self.assertNotIn("Native system | Not ready", validation)
        self.assertNotIn("Native Curve versus native Dusk remains unranked", validation)

        artifact = json.loads((ROOT / "artifact.json").read_text())
        forbidden = ("log-dh2", "log dh2", "log-potential", "proposed_dh2")
        for name, payload in (
            ("decision report", report),
            ("validation report", validation),
            ("artifact", json.dumps(artifact, sort_keys=True)),
        ):
            with self.subTest(name=name):
                normalized = payload.lower()
                for token in forbidden:
                    self.assertNotIn(token, normalized)

    def test_raw_excluded_model_rows_are_preserved(self):
        for filename, field in (
            ("parameter_matching.csv", "model"),
            ("pareto.csv", "model"),
            ("shape.csv", "model"),
            ("routing.csv", "model"),
            ("path_results.csv", "model"),
            ("paired_differences.csv", "competitor"),
            ("safety.csv", "model"),
        ):
            with self.subTest(filename=filename):
                self.assertIn(
                    "proposed_dh2",
                    {row[field] for row in read_rows(filename)},
                )

    def test_native_matrix_is_complete_and_decision_is_data_derived(self):
        summary = json.loads(
            (ROOT / "results" / "native_summary.json").read_text()
        )
        self.assertEqual(summary["status"], "complete")
        self.assertTrue(summary["validation"]["canonical_output"])
        expected = summary["validation"]["expected_counts"]
        self.assertEqual(
            expected,
            {
                # Each independently searched family promotes its exact
                # top-five shortlist: Curve + concentrated Dusk + CPMM.
                "profile_rows": 15,
                "held_out_rows": 480,
                "paired_rows": 240,
                "cpmm_rows": 240,
                "cpmm_matched_policy_ablation_rows": 240,
                "counterfactual_rows": 480,
                # Only the two concentrated five-profile shortlists enter the
                # 48-cell promoted-policy sensitivity.
                "neighborhood_rows": 480,
                "scale_rows": 288,
                "cadence_rows": 96,
                "volume_sensitivity_rows": 288,
                "basis_sensitivity_rows": 1_440,
                "fee_split_sensitivity_rows": 960,
                # Fifteen promoted profiles x the 128-cell training matrix.
                "training_rows": 1_920,
            },
        )
        files = {
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
        for field, filename in files.items():
            with self.subTest(field=field):
                self.assertEqual(len(read_rows(filename)), int(expected[field]))
                self.assertEqual(
                    int(summary["validation"][field]),
                    int(expected[field]),
                )
        audit = summary["validation"]["post_run_audit"]
        self.assertEqual(audit["status"], "passed")
        self.assertEqual(audit["scope"], "all_underlying_replay_matrices")
        self.assertEqual(
            audit["arbitrage_cap_counter_scope"],
            "all_periods_including_burn_in_and_score",
        )
        self.assertEqual(int(audit["arbitrage_capped_event_count"]), 0)
        self.assertEqual(int(audit["scored_arbitrage_capped_event_count"]), 0)
        self.assertEqual(int(audit["arbitrage_numerical_failure_count"]), 0)
        self.assertEqual(
            int(audit["scored_arbitrage_numerical_failure_count"]),
            0,
        )
        self.assertEqual(int(audit["audited_rows"]), 5_952)
        audit_rows = [
            row
            for filename in audit["audited_files"]
            for row in read_rows(filename)
        ]
        self.assertEqual(len(audit_rows), 5_952)
        decision_fields = (
            "net_lp_return_after_protocol_fee_bps",
            "net_lp_return_vs_hold_bps",
            "captured_user_volume_fraction",
            "selected_execution_cost_bps",
            "depth_degradation_fraction",
            "marginal_tracking_stale_fraction",
            "accounting_identity_error",
        )
        self.assertTrue(summary["validation"]["all_decision_metrics_finite"])
        self.assertTrue(
            all(
                math.isfinite(float(row[field]))
                for row in audit_rows
                for field in decision_fields
            )
        )
        self.assertLessEqual(
            max(abs(float(row["accounting_identity_error"])) for row in audit_rows),
            1e-5,
        )
        self.assertEqual(
            max(int(row["controller_external_price_reads"]) for row in audit_rows),
            0,
        )
        self.assertEqual(summary["validation"]["controller_external_price_reads"], 0)
        self.assertEqual(summary["validation"]["arbitrage_capped_event_count"], 0)
        self.assertEqual(
            summary["validation"]["scored_arbitrage_capped_event_count"],
            0,
        )
        self.assertEqual(
            summary["validation"]["arbitrage_numerical_failure_count"],
            0,
        )
        self.assertEqual(
            summary["validation"][
                "scored_arbitrage_numerical_failure_count"
            ],
            0,
        )
        self.assertEqual(
            summary["validation"]["arbitrage_cap_counter_scope"],
            "all_periods_including_burn_in_and_score",
        )
        self.assertEqual(
            sum(int(row["arbitrage_capped_event_count"]) for row in audit_rows),
            0,
        )
        self.assertEqual(
            sum(
                int(row["scored_arbitrage_capped_event_count"])
                for row in audit_rows
            ),
            0,
        )
        self.assertEqual(
            sum(
                int(row["arbitrage_numerical_failure_count"])
                for row in audit_rows
            ),
            0,
        )
        self.assertEqual(
            sum(
                int(row["scored_arbitrage_numerical_failure_count"])
                for row in audit_rows
            ),
            0,
        )
        self.assertEqual(
            {row["match_basis"] for row in read_rows("native_matching_basis_sensitivity.csv")},
            {
                "nearest_full_profile",
                "equal_jump_risk",
                "v21_deployed_invariant",
            },
        )
        paired = read_rows("native_paired.csv")
        self.assertEqual({row["routing_mode"] for row in paired}, {"fixed", "elastic"})
        self.assertEqual(
            {row["flow_regime"] for row in paired},
            {"benign", "mixed", "toxic_delayed", "low_volume_stuck"},
        )
        decision = summary["decision"]
        selection_policy = summary["profile_selection_policy"]
        self.assertGreaterEqual(
            float(selection_policy["common_service_relaxation"]),
            0.0,
        )
        self.assertEqual(
            set(selection_policy["minimum_required_relaxation_by_system"]),
            {"curve_native_v2_1", "dusk_native"},
        )
        self.assertEqual(
            set(selection_policy["systems"]),
            {"curve_native_v2_1", "dusk_native"},
        )
        selected_rows = [
            row
            for row in read_rows("native_profile_selection.csv")
            if row["selected_for_held_out"] == "True"
        ]
        self.assertEqual(len(selected_rows), 3)
        self.assertEqual(
            {row["system"] for row in selected_rows},
            {"curve_native_v2_1", "dusk_native", "dusk_cpmm"},
        )
        self.assertTrue(
            all(int(row["relaxed_service_check_failures"]) == 0 for row in selected_rows)
        )
        selected_ids = {
            row["system"]: row["profile_id"] for row in selected_rows
        }
        selected_by_system = {row["system"]: row for row in selected_rows}
        for system in ("curve_native_v2_1", "dusk_native"):
            self.assertEqual(
                selected_by_system[system]["selection_group"],
                "concentrated_candidates",
            )
            self.assertAlmostEqual(
                float(
                    selected_by_system[system][
                        "selection_group_service_relaxation"
                    ]
                ),
                float(selection_policy["common_service_relaxation"]),
            )
        dusk_policy = summary["dusk_product_selection_policy"]
        self.assertEqual(dusk_policy["system"], "dusk_native")
        self.assertEqual(
            dusk_policy["selected_profile_id"],
            selected_ids["dusk_native"],
        )
        self.assertIs(
            dusk_policy["independent_from_common_curve_dh2_relaxation"],
            True,
        )
        self.assertAlmostEqual(
            float(dusk_policy["service_relaxation"]),
            float(
                selected_by_system["dusk_native"][
                    "service_relaxation_required"
                ]
            ),
        )
        expected_dusk_ready = (
            float(dusk_policy["service_relaxation"]) <= 1e-12
            and bool(
                dusk_policy[
                    "selected_profile_original_service_envelope_pass"
                ]
            )
        )
        self.assertIs(
            dusk_policy["product_ready_on_training_service"],
            expected_dusk_ready,
        )
        cpmm_policy = summary["cpmm_product_selection_policy"]
        self.assertEqual(cpmm_policy["system"], "dusk_cpmm")
        self.assertEqual(
            set(cpmm_policy["minimum_required_relaxation_by_system"]),
            {"dusk_cpmm"},
        )
        self.assertIs(
            cpmm_policy["contaminates_concentrated_candidate_selection"],
            False,
        )
        self.assertEqual(
            selected_by_system["dusk_cpmm"]["selection_group"],
            "cpmm_product",
        )
        self.assertAlmostEqual(
            float(
                selected_by_system["dusk_cpmm"][
                    "selection_group_service_relaxation"
                ]
            ),
            float(cpmm_policy["service_relaxation"]),
        )
        self.assertIs(
            selected_by_system["dusk_cpmm"][
                "original_service_envelope_pass"
            ].lower()
            == "true",
            cpmm_policy[
                "selected_profile_original_service_envelope_pass"
            ],
        )
        expected_cpmm_ready = (
            float(cpmm_policy["service_relaxation"]) <= 1e-12
            and bool(
                cpmm_policy[
                    "selected_profile_original_service_envelope_pass"
                ]
            )
        )
        self.assertIs(
            cpmm_policy["product_ready_on_training_service"],
            expected_cpmm_ready,
        )
        self.assertEqual(set(summary["profile_selection"]), set(selected_ids))
        product = summary["cpmm_product_fallback"]
        ablation = summary["cpmm_matched_policy_ablation"]
        self.assertIs(product["participates_in_winner_gate"], True)
        self.assertEqual(product["control_system"], "dusk_cpmm")
        self.assertEqual(
            product["control_profile_id"], selected_ids["dusk_cpmm"]
        )
        self.assertIs(
            product["product_ready_on_training_service"],
            cpmm_policy["product_ready_on_training_service"],
        )
        self.assertAlmostEqual(
            float(product["service_relaxation"]),
            float(cpmm_policy["service_relaxation"]),
        )
        self.assertIs(ablation["participates_in_winner_gate"], False)
        self.assertEqual(
            ablation["control_system"], "dusk_cpmm_matched_policy"
        )
        self.assertEqual(
            ablation["control_profile_id"], selected_ids["dusk_native"]
        )
        self.assertEqual(
            {
                (row["system"], row["profile_id"])
                for row in read_rows("native_cpmm_control.csv")
            },
            {("dusk_cpmm", selected_ids["dusk_cpmm"])},
        )
        self.assertEqual(
            {
                (row["system"], row["profile_id"])
                for row in read_rows("native_cpmm_matched_policy_ablation.csv")
            },
            {("dusk_cpmm_matched_policy", selected_ids["dusk_native"])},
        )
        if not selection_policy["comparison_pair_ready_on_training_service"]:
            self.assertIsNone(decision["fee_adjusted_universal_native_winner"])
        self.assertIn(
            decision["fee_adjusted_universal_native_winner"],
            (None, "dusk_current_dh2", "curve_twocrypto_v2_1"),
        )
        self.assertNotIn("universal_native_winner", decision)
        self.assertNotIn("gross_mechanism_universal_winner", decision)
        gross = decision["gross_outcome_for_fee_adjusted_selected_profiles"]
        self.assertEqual(
            gross["selection_basis"],
            "fee_adjusted_synthetic_profile_lock",
        )
        self.assertIs(gross["universal_claim"], False)
        eligibility = decision["neighborhood_service_eligibility"]
        self.assertEqual(set(eligibility), {"curve_native_v2_1", "dusk_native"})
        for counts in eligibility.values():
            self.assertGreater(int(counts["eligible_profiles"]), 0)
            self.assertEqual(
                int(counts["total_profiles"]),
                int(counts["eligible_profiles"])
                + int(counts["excluded_profiles"]),
            )

    def test_synthetic_and_historical_weights_are_not_pooled(self):
        weights = json.loads(
            (ROOT / "results" / "scenario_weights.json").read_text()
        )
        self.assertIn("Do not pool", weights["policy"])
        rows = read_rows("path_results.csv")
        self.assertEqual(
            {row["path_class"] for row in rows},
            {"synthetic", "historical"},
        )
        summary = json.loads(
            (ROOT / "results" / "summary.json").read_text()
        )
        pairs = read_rows("paired_differences.csv")
        for competitor in ("current_dh2", "proposed_dh2"):
            reported = summary["controlled_paths"]["paired_vs_curve"][
                competitor
            ]
            self.assertNotIn("median_paired_net_lp_delta", reported)
            for path_class, expected_cells in (
                ("synthetic", 32),
                ("historical", 20),
            ):
                deltas = [
                    float(row["paired_net_lp_delta"])
                    for row in pairs
                    if row["competitor"] == competitor
                    and row["path_class"] == path_class
                ]
                self.assertEqual(len(deltas), expected_cells)
                self.assertEqual(
                    reported[path_class]["paired_cells"],
                    expected_cells,
                )
                self.assertAlmostEqual(
                    reported[path_class]["median_paired_net_lp_delta"],
                    statistics.median(deltas),
                )

    def test_current_dh2_matching_respects_configured_peak_depth_cap(self):
        rows = [
            row
            for row in read_rows("parameter_matching.csv")
            if row["model"] == "current_dh2"
        ]
        self.assertTrue(rows)
        self.assertLessEqual(
            max(float(row["parameter_1"]) for row in rows),
            2_000.0,
        )

    def test_sharp_profile_is_hard_rejected_with_root_evidence(self):
        roots = read_rows("sharp_root_evidence.csv")
        self.assertEqual(len(roots), 3)
        self.assertEqual({int(row["root_count"]) for row in roots}, {3})
        self.assertEqual(
            {row["decision"] for row in roots},
            {"hard_reject_multiple_invariant_roots"},
        )
        sharp_safety = next(
            row for row in read_rows("safety.csv") if row["model"] == "sharp_dh2"
        )
        self.assertEqual(int(sharp_safety["max_invariant_roots"]), 3)
        self.assertEqual(sharp_safety["hard_rejected"], "True")
        summary = json.loads((ROOT / "results" / "summary.json").read_text())
        self.assertTrue(summary["safety"]["sharp_dh2_hard_rejected"])
        report = (ROOT / "DECISION_REPORT.md").read_text()
        self.assertIn("Sharper current-DH2 fade: hard rejected", report)

    def test_hyperbolic_p4_is_root_safe_but_economically_rejected(self):
        root_rows = read_rows("hyperbolic_p4_root_scan.csv")
        self.assertEqual({int(row["root_count"]) for row in root_rows}, {1})
        rows = read_rows("hyperbolic_p4_comparison.csv")
        equal_execution = next(
            row
            for row in rows
            if row["basis"] == "equal_execution"
            and row["model"] == "hyperbolic_p4_dh2"
        )
        equal_risk = next(
            row
            for row in rows
            if row["basis"] == "equal_jump_risk"
            and row["model"] == "hyperbolic_p4_dh2"
        )
        self.assertGreater(
            float(equal_execution["jump_loss_delta_vs_legacy_fraction"]),
            0.05,
        )
        self.assertGreater(
            float(equal_risk["execution_delta_vs_legacy_fraction"]),
            0.05,
        )
        self.assertEqual(equal_execution["decision"], "reject_economically")

    def test_report_pareto_is_independently_recomputed_without_excluded_family(self):
        artifact = json.loads((ROOT / "artifact.json").read_text())
        actual_rows = artifact["snapshot"]["datasets"]["pareto"]
        visible_models = {"curve", "current_dh2", "cpmm"}
        raw_rows = [
            row
            for row in read_rows("pareto.csv")
            if row["model"] in visible_models
        ]
        self.assertEqual(
            {row["model_id"] for row in actual_rows},
            visible_models,
        )
        self.assertEqual(len(actual_rows), len(raw_rows))

        raw_scores = [
            (
                float(row["execution_index_bps"]),
                float(row["jump_loss_index_bps"]),
            )
            for row in raw_rows
        ]
        expected = {}
        for index, row in enumerate(raw_rows):
            execution, jump_loss = raw_scores[index]
            dominated = any(
                other_execution <= execution
                and other_jump_loss <= jump_loss
                and (
                    other_execution < execution
                    or other_jump_loss < jump_loss
                )
                for other_index, (other_execution, other_jump_loss) in enumerate(
                    raw_scores
                )
                if other_index != index
            )
            expected[
                (
                    row["model"],
                    float(row["parameter_1"]),
                    float(row["parameter_2"]),
                )
            ] = not dominated

        actual = {
            (
                row["model_id"],
                float(row["parameter_1"]),
                float(row["parameter_2"]),
            ): bool(row["combined_pareto"])
            for row in actual_rows
        }
        self.assertEqual(actual, expected)

    def test_report_routing_is_independently_recomputed_as_binary_tournament(self):
        artifact = json.loads((ROOT / "artifact.json").read_text())
        actual_rows = artifact["snapshot"]["datasets"]["routing_summary"]
        ranked_models = ("curve", "current_dh2")
        intent_fields = (
            "normalized_marginal_price",
            "trade_bps_of_marked_tvl",
            "direction",
        )
        grouped = {}
        costs_by_model = {model: [] for model in ranked_models}
        for row in read_rows("routing.csv"):
            model = row["model"]
            if model not in ranked_models:
                continue
            key = tuple(row[field] for field in intent_fields)
            grouped.setdefault(key, {})[model] = float(
                row["execution_cost_bps"]
            )
            costs_by_model[model].append(float(row["execution_cost_bps"]))
        self.assertTrue(grouped)
        self.assertTrue(
            all(set(costs) == set(ranked_models) for costs in grouped.values())
        )

        wins = {model: 0.0 for model in ranked_models}
        for costs in grouped.values():
            best = min(costs.values())
            winners = [
                model
                for model, cost in costs.items()
                if cost <= best + 1e-9
            ]
            for model in winners:
                wins[model] += 1.0 / len(winners)

        actual = {row["model_id"]: row for row in actual_rows}
        self.assertEqual(set(actual), set(ranked_models))
        for model in ranked_models:
            self.assertEqual(int(actual[model]["intent_count"]), len(grouped))
            self.assertAlmostEqual(
                float(actual[model]["best_quote_intent_share"]),
                wins[model] / len(grouped),
            )
            self.assertAlmostEqual(
                float(actual[model]["median_execution_cost_bps"]),
                statistics.median(costs_by_model[model]),
            )
        self.assertAlmostEqual(
            sum(
                float(row["best_quote_intent_share"])
                for row in actual_rows
            ),
            1.0,
        )

    def test_artifact_is_bounded_and_ready(self):
        artifact = json.loads((ROOT / "artifact.json").read_text())
        self.assertEqual(artifact["snapshot"]["status"], "ready")
        self.assertEqual(
            artifact["sources"],
            artifact["manifest"]["sources"],
        )
        self.assertLessEqual(
            len(artifact["snapshot"]["datasets"]),
            50,
        )
        for rows in artifact["snapshot"]["datasets"].values():
            self.assertLessEqual(len(rows), 2_000)
        source_ids = {
            source["id"] for source in artifact["manifest"]["sources"]
        }
        for block in artifact["manifest"]["blocks"]:
            if "sourceId" in block:
                self.assertIn(block["sourceId"], source_ids)
        datasets = artifact["snapshot"]["datasets"]
        for visual_kind in ("charts", "tables"):
            for visual in artifact["manifest"][visual_kind]:
                self.assertIn(visual["dataset"], datasets)
                rows = datasets[visual["dataset"]]
                self.assertTrue(rows)
                fields = set(rows[0])
                if visual_kind == "charts":
                    for encoding in visual["encodings"].values():
                        if "field" in encoding:
                            self.assertIn(encoding["field"], fields)
                else:
                    for column in visual["columns"]:
                        self.assertIn(column["field"], fields)
        self.assertTrue(
            any(
                block["type"] == "chart"
                for block in artifact["manifest"]["blocks"]
            )
        )
        self.assertIn("center_quotes", artifact["snapshot"]["datasets"])
        service_policies = artifact["snapshot"]["datasets"][
            "native_service_policy"
        ]
        self.assertEqual(len(service_policies), 3)
        dusk_policy = next(
            row
            for row in service_policies
            if row["policy"] == "Dusk product readiness"
        )
        self.assertIn("Authoritative", dusk_policy["product_readiness_role"])
        self.assertTrue(
            any(
                table["id"] == "native-service-policy-table"
                for table in artifact["manifest"]["tables"]
            )
        )
        center_quotes = artifact["snapshot"]["datasets"]["center_quotes"]
        self.assertEqual(len(center_quotes), 12)
        self.assertEqual(
            {row["model_id"] for row in center_quotes},
            {"curve", "current_dh2", "cpmm"},
        )
        cpmm_large = next(
            row
            for row in center_quotes
            if row["model_id"] == "cpmm"
            and int(row["trade_bps_of_tvl"]) == 1_000
        )
        self.assertEqual(int(cpmm_large["direction_count"]), 2)
        self.assertEqual(cpmm_large["directions"], "x_to_y,y_to_x")
        self.assertAlmostEqual(
            float(cpmm_large["quoted_output_value_usd"]),
            166_666.6666666667,
            places=6,
        )
        self.assertTrue(
            any(
                chart["id"] == "center-slippage"
                for chart in artifact["manifest"]["charts"]
            )
        )
        self.assertTrue(
            any(
                table["id"] == "center-quote-table"
                for table in artifact["manifest"]["tables"]
            )
        )
        first = artifact["manifest"]["blocks"][0]
        self.assertEqual(first["type"], "markdown")
        self.assertTrue(first["body"].startswith("# "))


if __name__ == "__main__":
    unittest.main()

import json
import sys
from dataclasses import replace
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import native_benchmark as benchmark
from native_benchmark import (
    FLOW_REGIMES,
    PRIMARY_SEEDS,
    SERVICE_GUARDRAILS,
    _basis_sensitivity_jobs,
    _candidate_vs_cpmm_rows,
    _cpmm_acceptance_predicates,
    _cpmm_jobs,
    _cpmm_matched_policy_jobs,
    _cpmm_product_training_ready,
    _cpmm_profiles,
    _counterfactual_jobs,
    _curve_profiles,
    _dusk_profiles,
    _eligible_neighborhood_rows,
    _failed_service_checks,
    _held_out_jobs,
    _matched_models,
    _quick_training_jobs,
    _profile_summary,
    _required_service_relaxation,
    _sensitivity_jobs,
    _training_jobs,
    _training_replay_fingerprint,
    _training_service_deployment_ready,
    _validate_training_cross,
)


class NativeBenchmarkMatrixTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        curve, dusk = _matched_models()
        cls.curve_profiles = _curve_profiles(curve)
        cls.dusk_profiles = _dusk_profiles(dusk)
        cls.cpmm_profiles = _cpmm_profiles(cls.dusk_profiles)
        cls.candidate_profiles = (*cls.curve_profiles, *cls.dusk_profiles)
        cls.profiles = (*cls.candidate_profiles, *cls.cpmm_profiles)
        cls.selected = {
            "curve_native_v2_1": cls.curve_profiles[0],
            "dusk_native": cls.dusk_profiles[0],
            "dusk_cpmm": cls.cpmm_profiles[0],
        }

    def assert_complete_policy_cross(self, jobs):
        self.assertEqual(
            {str(job["flow"]["flow_regime"]) for job in jobs},
            {str(flow["flow_regime"]) for flow in FLOW_REGIMES},
        )
        self.assertEqual({int(job["seed"]) for job in jobs}, set(PRIMARY_SEEDS))
        self.assertEqual(
            {str(job["routing_mode"]) for job in jobs},
            {"fixed", "elastic"},
        )

    def test_quick_robustness_slices_cover_every_flow_seed_and_route(self):
        neighborhood, scale, cadence = _sensitivity_jobs(
            self.candidate_profiles,
            self.selected,
            quick=True,
        )
        slices = {
            "cpmm": _cpmm_jobs(self.selected["dusk_cpmm"], quick=True),
            "cpmm_ablation": _cpmm_matched_policy_jobs(
                self.selected["dusk_native"], quick=True
            ),
            "counterfactual": _counterfactual_jobs(self.selected, quick=True),
            "neighborhood": neighborhood,
            "scale": scale,
            "cadence": cadence,
            "basis": _basis_sensitivity_jobs(self.selected, quick=True),
        }
        expected_counts = {
            "cpmm": 48,
            "cpmm_ablation": 48,
            "counterfactual": 96,
            "neighborhood": (
                len(self.candidate_profiles)
                * len(FLOW_REGIMES)
                * len(PRIMARY_SEEDS)
                * 2
            ),
            "scale": 96,
            "cadence": 32,
            "basis": 288,
        }
        for name, jobs in slices.items():
            with self.subTest(name=name):
                self.assertEqual(len(jobs), expected_counts[name])
                self.assert_complete_policy_cross(jobs)

        self.assertEqual(
            {str(job["match_basis"]) for job in slices["basis"]},
            {
                "nearest_full_profile",
                "equal_jump_risk",
                "v21_deployed_invariant",
            },
        )
        self.assertEqual(
            {
                job["profile"].system
                for job in _held_out_jobs(self.selected, quick=True)
            },
            {"curve_native_v2_1", "dusk_native"},
        )
        for name in ("scale", "cadence"):
            self.assertEqual(
                {job["profile"].system for job in slices[name]},
                {"curve_native_v2_1", "dusk_native"},
            )

    def test_quick_training_selection_is_route_safe(self):
        jobs = _quick_training_jobs(self.profiles)
        self.assertEqual(len(jobs), len(self.profiles) * len(FLOW_REGIMES) * 2)
        self.assertEqual({job["routing_mode"] for job in jobs}, {"fixed", "elastic"})
        self.assertEqual({job["path_name"] for job in jobs}, {"flat"})
        self.assertEqual({job["seed"] for job in jobs}, {7})

    def test_cpmm_relaxation_does_not_contaminate_candidate_lock(self):
        profiles = (
            self.curve_profiles[0],
            self.dusk_profiles[0],
            self.cpmm_profiles[0],
        )
        rows = []
        for profile in profiles:
            for flow in FLOW_REGIMES:
                for route in ("fixed", "elastic"):
                    capture = (
                        0.0
                        if profile.system == "dusk_cpmm" and route == "elastic"
                        else 1.0
                    )
                    rows.append(
                        {
                            "system": profile.system,
                            "profile_id": profile.profile_id,
                            "routing_mode": route,
                            "flow_regime": flow["flow_regime"],
                            "net_lp_return_after_protocol_fee_bps": 1.0,
                            "captured_user_volume_fraction": capture,
                            "selected_execution_cost_bps": 0.0,
                            "depth_degradation_fraction": 0.0,
                            "marginal_tracking_stale_fraction": 0.0,
                            "accounting_identity_error": 0.0,
                            "controller_external_price_reads": 0,
                            "arbitrage_capped_event_count": 0,
                            "arbitrage_numerical_failure_count": 0,
                        }
                    )
        summaries, selected, infeasible = _profile_summary(rows, profiles)
        self.assertEqual(infeasible, [])
        self.assertEqual(set(selected), set(benchmark.PROFILE_SELECTION_SYSTEMS))
        selected_rows = {
            row["system"]: row
            for row in summaries
            if row["selected_for_held_out"]
        }
        self.assertEqual(
            float(
                selected_rows["curve_native_v2_1"][
                    "selection_group_service_relaxation"
                ]
            ),
            0.0,
        )
        self.assertEqual(
            float(
                selected_rows["dusk_native"][
                    "selection_group_service_relaxation"
                ]
            ),
            0.0,
        )
        self.assertEqual(
            float(
                selected_rows["dusk_cpmm"][
                    "selection_group_service_relaxation"
                ]
            ),
            1.0,
        )
        self.assertTrue(
            _training_service_deployment_ready(
                {
                    system: selected_rows[system]
                    for system in benchmark.CANDIDATE_SYSTEMS
                }
            )
        )
        self.assertFalse(
            _cpmm_product_training_ready(selected_rows["dusk_cpmm"])
        )

    def test_profile_lock_minimizes_each_systems_own_relaxation_before_lp_utility(self):
        profiles = (
            self.curve_profiles[0],
            self.dusk_profiles[0],
            self.dusk_profiles[1],
            self.cpmm_profiles[0],
        )
        rows = []
        for profile in profiles:
            lp_return = (
                100.0
                if profile.system == "dusk_native"
                and profile.profile_id == self.dusk_profiles[1].profile_id
                else 1.0
            )
            for flow in FLOW_REGIMES:
                for route in ("fixed", "elastic"):
                    rows.append(
                        {
                            "system": profile.system,
                            "profile_id": profile.profile_id,
                            "routing_mode": route,
                            "flow_regime": flow["flow_regime"],
                            "net_lp_return_after_protocol_fee_bps": lp_return,
                            "captured_user_volume_fraction": 1.0,
                            "selected_execution_cost_bps": 0.0,
                            "depth_degradation_fraction": 0.0,
                            "marginal_tracking_stale_fraction": 0.0,
                            "accounting_identity_error": 0.0,
                            "controller_external_price_reads": 0,
                            "arbitrage_capped_event_count": 0,
                            "arbitrage_numerical_failure_count": 0,
                        }
                    )

        required_relaxation = {
            ("curve_native_v2_1", self.curve_profiles[0].profile_id): 0.5,
            ("dusk_native", self.dusk_profiles[0].profile_id): 0.0,
            ("dusk_native", self.dusk_profiles[1].profile_id): 0.4,
            ("dusk_cpmm", self.cpmm_profiles[0].profile_id): 0.0,
        }
        with mock.patch.object(
            benchmark,
            "_required_service_relaxation",
            side_effect=lambda row: required_relaxation[
                (str(row["system"]), str(row["profile_id"]))
            ],
        ):
            summaries, selected, infeasible = _profile_summary(rows, profiles)

        self.assertEqual(infeasible, [])
        self.assertEqual(
            selected["dusk_native"].profile_id,
            self.dusk_profiles[0].profile_id,
        )
        selected_row = next(
            row
            for row in summaries
            if row["system"] == "dusk_native"
            and row["selected_for_held_out"]
        )
        self.assertEqual(float(selected_row["service_relaxation_required"]), 0.0)
        self.assertLess(
            float(selected_row["minimum_route_selection_utility"]),
            100.0,
        )

    def test_low_volume_stuck_regime_also_reduces_trade_arrival_frequency(self):
        arrivals = {
            str(flow["flow_regime"]): float(flow["mean_arrivals_per_minute"])
            for flow in FLOW_REGIMES
        }
        self.assertLess(
            arrivals["low_volume_stuck"],
            min(
                value
                for regime, value in arrivals.items()
                if regime != "low_volume_stuck"
            ),
        )

    def test_service_relaxation_cannot_hide_one_bad_route(self):
        row = {
            "max_accounting_error": 0.0,
            "max_controller_external_price_reads": 0,
        }
        for route in ("fixed", "elastic"):
            for field, (kind, threshold) in SERVICE_GUARDRAILS.items():
                row[f"{route}_{field}"] = threshold
        row["fixed_median_selected_execution_cost_bps"] = 50.0
        self.assertAlmostEqual(_required_service_relaxation(row), 1.0)

    def test_service_relaxation_accepts_negative_cost_but_rejects_bad_fractions(self):
        row = {
            "max_accounting_error": 0.0,
            "max_controller_external_price_reads": 0,
        }
        for route in ("fixed", "elastic"):
            for field, (_, threshold) in SERVICE_GUARDRAILS.items():
                row[f"{route}_{field}"] = threshold
        row["fixed_median_selected_execution_cost_bps"] = -2.0
        row["elastic_p90_selected_execution_cost_bps"] = -0.25
        self.assertEqual(_required_service_relaxation(row), 0.0)
        self.assertEqual(_failed_service_checks(row, 0.0), [])

        row["fixed_median_captured_volume_fraction"] = -0.01
        self.assertEqual(_required_service_relaxation(row), float("inf"))
        self.assertTrue(_failed_service_checks(row, 0.0))
        row["fixed_median_captured_volume_fraction"] = 1.01
        self.assertEqual(_required_service_relaxation(row), float("inf"))
        self.assertTrue(_failed_service_checks(row, 0.0))

    def test_all_period_arbitrage_cap_is_a_fatal_profile_gate(self):
        row = {
            "max_accounting_error": 0.0,
            "max_controller_external_price_reads": 0,
            "max_arbitrage_capped_event_count": 1,
        }
        for route in ("fixed", "elastic"):
            for field, (_, threshold) in SERVICE_GUARDRAILS.items():
                row[f"{route}_{field}"] = threshold

        self.assertEqual(_required_service_relaxation(row), float("inf"))
        self.assertTrue(
            any(
                check.startswith(
                    "max_all_period_arbitrage_capped_event_count:1:max:0"
                )
                for check in _failed_service_checks(row, 0.0)
            )
        )

    def test_all_period_arbitrage_numerical_failure_is_a_fatal_profile_gate(self):
        row = {
            "max_accounting_error": 0.0,
            "max_controller_external_price_reads": 0,
            "max_arbitrage_capped_event_count": 0,
            "max_arbitrage_numerical_failure_count": 1,
        }
        for route in ("fixed", "elastic"):
            for field, (_, threshold) in SERVICE_GUARDRAILS.items():
                row[f"{route}_{field}"] = threshold

        self.assertEqual(_required_service_relaxation(row), float("inf"))
        self.assertTrue(
            any(
                check.startswith(
                    "max_all_period_arbitrage_numerical_failure_count:1:max:0"
                )
                for check in _failed_service_checks(row, 0.0)
            )
        )

    def test_cpmm_product_uses_its_independently_selected_profile(self):
        jobs = _cpmm_jobs(self.selected["dusk_cpmm"], quick=True)
        profiles = {job["profile"] for job in jobs}
        self.assertEqual(profiles, {self.selected["dusk_cpmm"]})
        profile = next(iter(profiles))
        self.assertEqual(profile.system, "dusk_cpmm")
        self.assertEqual(profile.profile_id, self.selected["dusk_cpmm"].profile_id)
        self.assertEqual(profile.model.peak_depth, 0.0)
        self.assertEqual(profile.model.fade_scale, 0.0)
        self.assertEqual(
            {job["analysis_slice"] for job in jobs},
            {"cpmm_product_fallback"},
        )

    def test_cpmm_profile_family_uses_independent_zero_depth_promotion(self):
        payload = json.loads(benchmark.PROMOTED_CPMM_PROFILES.read_text())
        self.assertEqual(
            [profile.profile_id for profile in self.cpmm_profiles],
            [str(row["profile_id"]) for row in payload["profiles"]],
        )
        self.assertEqual(
            len({profile.profile_id for profile in self.cpmm_profiles}),
            len(self.cpmm_profiles),
        )
        for cpmm in self.cpmm_profiles:
            self.assertEqual(cpmm.system, "dusk_cpmm")
            self.assertEqual(cpmm.model.peak_depth, 0.0)
            self.assertEqual(cpmm.model.fade_scale, 0.0)

    def test_cpmm_matched_policy_ablation_changes_only_concentration(self):
        jobs = _cpmm_matched_policy_jobs(
            self.selected["dusk_native"], quick=True
        )
        profiles = {job["profile"] for job in jobs}
        self.assertEqual(len(profiles), 1)
        profile = profiles.pop()
        self.assertEqual(profile.system, "dusk_cpmm_matched_policy")
        self.assertEqual(
            profile.profile_id, self.selected["dusk_native"].profile_id
        )
        self.assertEqual(profile.config, self.selected["dusk_native"].config)
        self.assertEqual(profile.model.peak_depth, 0.0)
        self.assertEqual(profile.model.fade_scale, 0.0)
        self.assertEqual(
            profile.config.adjustment_threshold,
            self.selected["dusk_native"].config.adjustment_threshold,
        )
        self.assertEqual(
            profile.config.adjustment_step,
            self.selected["dusk_native"].config.adjustment_step,
        )
        self.assertEqual(
            profile.config.min_adjustment_interval_minutes,
            self.selected["dusk_native"].config.min_adjustment_interval_minutes,
        )
        self.assertEqual(
            {job["analysis_slice"] for job in jobs},
            {"cpmm_matched_policy_ablation"},
        )

    def test_neighborhood_excludes_profiles_outside_common_envelope(self):
        chosen_profiles = tuple(
            profile
            for system, profiles in (
                ("curve_native_v2_1", self.curve_profiles),
                ("dusk_native", self.dusk_profiles),
            )
            for profile in (
                self.selected[system],
                next(
                    candidate
                    for candidate in profiles
                    if candidate.profile_id != self.selected[system].profile_id
                ),
            )
        )
        profile_rows = [
            {
                "system": profile.system,
                "profile_id": profile.profile_id,
                "eligible_for_lock": (
                    profile.profile_id
                    == self.selected[profile.system].profile_id
                ),
            }
            for profile in chosen_profiles
        ]
        neighborhood = [
            {
                "system": profile.system,
                "profile_id": profile.profile_id,
                "path": path,
            }
            for profile in chosen_profiles
            for path in ("a", "b")
        ]
        eligible, counts = _eligible_neighborhood_rows(
            profile_rows,
            neighborhood,
            self.selected,
        )
        self.assertEqual(
            {(row["system"], row["profile_id"]) for row in eligible},
            {
                (system, self.selected[system].profile_id)
                for system in ("curve_native_v2_1", "dusk_native")
            },
        )
        for system in ("curve_native_v2_1", "dusk_native"):
            self.assertEqual(counts[system]["eligible_profiles"], 1)
            self.assertEqual(counts[system]["excluded_profiles"], 1)
            self.assertEqual(counts[system]["eligible_rows"], 2)
            self.assertEqual(counts[system]["excluded_rows"], 2)

    def test_cpmm_gate_uses_route_and_flow_tail_thresholds(self):
        passing = {
            "median_return_advantage_bps": 1.0,
            "window_median_win_fraction": 0.70,
            "bottom_decile_mean_return_advantage_bps": -1.0,
            "p90_execution_regression_bps": 1.0,
            "p10_capture_advantage_fraction": -0.02,
            "p90_marginal_tracking_stale_regression_fraction": 0.05,
        }
        routes = {route: dict(passing) for route in ("fixed", "elastic")}
        flows = {"mixed:fixed": dict(passing), "mixed:elastic": dict(passing)}
        predicates = _cpmm_acceptance_predicates(routes, flows)
        self.assertTrue(all(predicates.values()))

        routes["fixed"]["median_return_advantage_bps"] = 0.99
        self.assertFalse(
            _cpmm_acceptance_predicates(routes, flows)[
                "beats_dusk_cpmm_both_routes_median_advantage_at_least_1bp"
            ]
        )
        routes["fixed"]["median_return_advantage_bps"] = 1.0
        flows["mixed:elastic"]["p90_execution_regression_bps"] = 1.01
        self.assertFalse(
            _cpmm_acceptance_predicates(routes, flows)[
                "beats_dusk_cpmm_every_flow_route_tail_service_within_limits"
            ]
        )

    def test_candidate_vs_cpmm_pairs_exact_cells(self):
        common = {
            "path": "historical_a",
            "source_month": "2025-01",
            "stratum": "calm",
            "flow_regime": "mixed",
            "daily_volume_pool_ratio": 0.25,
            "seed": 7,
            "routing_mode": "fixed",
            "initial_pool_value": 2_000_000.0,
            "step_minutes": 5,
            "match_basis": "equal_execution",
            "net_lp_return_after_protocol_fee_bps": 4.0,
            "net_lp_return_vs_hold_bps": 5.0,
            "selected_execution_cost_bps": 3.0,
            "captured_user_volume_fraction": 0.6,
            "depth_degradation_fraction": 0.2,
            "marginal_tracking_stale_fraction": 0.1,
        }
        candidate = {**common, "system": "dusk_native"}
        control = {
            **common,
            "system": "dusk_cpmm",
            "net_lp_return_after_protocol_fee_bps": 1.0,
            "net_lp_return_vs_hold_bps": 2.0,
            "selected_execution_cost_bps": 4.0,
            "captured_user_volume_fraction": 0.5,
            "depth_degradation_fraction": 0.3,
            "marginal_tracking_stale_fraction": 0.2,
        }
        paired = _candidate_vs_cpmm_rows(
            [candidate],
            [control],
            "dusk_native",
        )
        self.assertEqual(len(paired), 1)
        self.assertEqual(
            paired[0]["candidate_minus_cpmm_net_lp_return_after_protocol_fee_bps"],
            3.0,
        )
        self.assertEqual(
            paired[0]["candidate_minus_cpmm_selected_execution_cost_bps"],
            -1.0,
        )
        self.assertEqual(paired[0]["daily_volume_pool_ratio"], 0.25)

    def test_candidate_vs_cpmm_keeps_distinct_volume_cells(self):
        common = {
            "path": "historical_a",
            "source_month": "2025-01",
            "stratum": "calm",
            "flow_regime": "benign_volume",
            "seed": 7,
            "routing_mode": "fixed",
            "initial_pool_value": 2_000_000.0,
            "step_minutes": 5,
            "match_basis": "equal_execution",
            "net_lp_return_after_protocol_fee_bps": 4.0,
            "net_lp_return_vs_hold_bps": 5.0,
            "selected_execution_cost_bps": 3.0,
            "captured_user_volume_fraction": 0.6,
            "depth_degradation_fraction": 0.2,
            "marginal_tracking_stale_fraction": 0.1,
        }
        candidates = [
            {
                **common,
                "system": "dusk_native",
                "daily_volume_pool_ratio": ratio,
            }
            for ratio in (0.05, 1.0)
        ]
        controls = [
            {
                **common,
                "system": "dusk_cpmm",
                "daily_volume_pool_ratio": ratio,
                "net_lp_return_after_protocol_fee_bps": 1.0,
                "net_lp_return_vs_hold_bps": 2.0,
                "selected_execution_cost_bps": 4.0,
                "captured_user_volume_fraction": 0.5,
                "depth_degradation_fraction": 0.3,
                "marginal_tracking_stale_fraction": 0.2,
            }
            for ratio in (0.05, 1.0)
        ]

        paired = _candidate_vs_cpmm_rows(
            candidates,
            controls,
            "dusk_native",
        )

        self.assertEqual(len(paired), 2)
        self.assertEqual(
            {row["daily_volume_pool_ratio"] for row in paired},
            {0.05, 1.0},
        )

    def test_relaxed_opponent_blocks_deployment_ready_winner(self):
        selected = {
            "curve_native_v2_1": {
                "selection_group_service_relaxation": 0.2,
                "original_service_envelope_pass": True,
            },
            "dusk_native": {
                "selection_group_service_relaxation": 0.2,
                "original_service_envelope_pass": False,
            },
        }
        self.assertFalse(_training_service_deployment_ready(selected))
        selected["dusk_native"]["original_service_envelope_pass"] = True
        self.assertFalse(_training_service_deployment_ready(selected))
        for row in selected.values():
            row["selection_group_service_relaxation"] = 0.0
        self.assertTrue(_training_service_deployment_ready(selected))

    def test_training_checkpoint_binds_replay_fingerprint_and_exact_cross(self):
        jobs = _training_jobs(self.profiles)
        rows = []
        for job in jobs:
            profile = job["profile"]
            rows.append(
                {
                    "system": profile.system,
                    "profile_id": profile.profile_id,
                    "path": job["path_name"],
                    "flow_regime": job["flow"]["flow_regime"],
                    "seed": job["seed"],
                    "routing_mode": job["routing_mode"],
                }
            )
        _validate_training_cross(rows, self.profiles)
        duplicate = [dict(row) for row in rows]
        duplicate[-1] = dict(duplicate[0])
        with self.assertRaisesRegex(AssertionError, "duplicate"):
            _validate_training_cross(duplicate, self.profiles)

        mutated_profile = replace(
            self.curve_profiles[0],
            model=replace(
                self.curve_profiles[0].model,
                mid_fee=self.curve_profiles[0].model.mid_fee + 1e-6,
            ),
        )
        mutated_profiles = (mutated_profile, *self.profiles[1:])
        self.assertNotEqual(
            _training_replay_fingerprint(self.profiles),
            _training_replay_fingerprint(mutated_profiles),
        )
        with tempfile.TemporaryDirectory() as temporary:
            checkpoint = Path(temporary)
            original = benchmark.TRAINING_CHECKPOINT_DIR
            try:
                benchmark.TRAINING_CHECKPOINT_DIR = checkpoint
                benchmark._write_training_checkpoint(rows, self.profiles)
                self.assertEqual(
                    len(benchmark._read_training_checkpoint(len(rows), self.profiles)),
                    len(rows),
                )
                with self.assertRaisesRegex(AssertionError, "fingerprint is stale"):
                    benchmark._read_training_checkpoint(
                        len(rows), mutated_profiles
                    )
            finally:
                benchmark.TRAINING_CHECKPOINT_DIR = original

    def test_training_fingerprint_binds_engine_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            original_root = benchmark.ROOT
            original_files = benchmark.TRAINING_REPLAY_ENGINE_FILES
            try:
                benchmark.ROOT = Path(temporary)
                benchmark.TRAINING_REPLAY_ENGINE_FILES = ("engine.py",)
                engine = benchmark.ROOT / "engine.py"
                engine.write_text("ENGINE_REVISION = 1\n")
                first = _training_replay_fingerprint(self.profiles)
                engine.write_text("ENGINE_REVISION = 2\n")
                second = _training_replay_fingerprint(self.profiles)
                self.assertNotEqual(first, second)
            finally:
                benchmark.ROOT = original_root
                benchmark.TRAINING_REPLAY_ENGINE_FILES = original_files


if __name__ == "__main__":
    unittest.main()

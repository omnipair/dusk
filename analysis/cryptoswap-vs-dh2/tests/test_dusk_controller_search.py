import json
import hashlib
import sys
import tempfile
import unittest
from dataclasses import asdict, replace
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import dusk_controller_search as search
from native_benchmark import NativeProfile


def fake_row(job, *, selected_cost=10.0, marginal_stale=0.0):
    profile = job["profile"]
    flow = job["flow"]
    return {
        "system": "dusk_native",
        "model": "current_dh2",
        "profile_id": profile.profile_id,
        "profile_description": profile.description,
        "path": job["path_name"],
        "path_class": "synthetic",
        "flow_regime": flow["flow_regime"],
        "seed": job["seed"],
        "routing_mode": job["routing_mode"],
        "analysis_slice": job["analysis_slice"],
        "net_lp_return_after_protocol_fee_bps": 2.0,
        "captured_user_volume_fraction": 0.75,
        "selected_execution_cost_bps": selected_cost,
        "marginal_tracking_stale_fraction": marginal_stale,
        "accounting_identity_error": 0.0,
        "controller_external_price_reads": 0,
        "arbitrage_capped_event_count": 0,
        "arbitrage_numerical_failure_count": 0,
    }


class ControllerSearchTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profiles = search.grid_profiles()

    def test_grid_is_exact_stable_and_unique(self):
        self.assertEqual(len(search.CONTROLLER_TUPLES), 10)
        self.assertEqual(len(search.FEE_BUNDLES), 15)
        self.assertEqual(len(self.profiles), 150)
        self.assertEqual(self.profiles[0].profile_id, "grid_c00_f00")
        self.assertEqual(self.profiles[-1].profile_id, "grid_c09_f14")
        self.assertEqual(
            len({profile.profile_id for profile in self.profiles}),
            150,
        )
        self.assertEqual(
            len(
                {
                    json.dumps(
                        asdict(profile.config),
                        sort_keys=True,
                        separators=(",", ":"),
                    )
                    for profile in self.profiles
                }
            ),
            150,
        )

        current_divergence = self.profiles[3]
        self.assertEqual(current_divergence.profile_id, "grid_c00_f03")
        self.assertEqual(
            search._profile_config_fields(current_divergence)[
                "dynamic_cap_bps"
            ],
            200.0,
        )
        self.assertEqual(
            search._profile_config_fields(current_divergence)[
                "volatility_coefficient"
            ],
            0.0,
        )
        current_balanced = self.profiles[8 * 15 + 9]
        fields = search._profile_config_fields(current_balanced)
        self.assertEqual(fields["center_ema_half_life_minutes"], 30.0)
        self.assertEqual(fields["adjustment_threshold_bps"], 100.0)
        self.assertEqual(fields["adjustment_step_bps"], 25.0)
        self.assertEqual(fields["dynamic_cap_bps"], 200.0)
        self.assertEqual(fields["divergence_coefficient"], 0.02)
        self.assertEqual(fields["volatility_coefficient"], 0.02)

        policies = {"base_only": 0, "divergence_only": 0, "volatility_only": 0, "mixed": 0}
        for profile in self.profiles:
            fields = search._profile_config_fields(profile)
            divergence = float(fields["divergence_coefficient"])
            volatility = float(fields["volatility_coefficient"])
            if divergence == 0.0 and volatility == 0.0:
                policies["base_only"] += 1
            elif divergence > 0.0 and volatility == 0.0:
                policies["divergence_only"] += 1
            elif divergence == 0.0 and volatility > 0.0:
                policies["volatility_only"] += 1
            else:
                policies["mixed"] += 1
        self.assertEqual(
            policies,
            {
                "base_only": 10,
                "divergence_only": 40,
                "volatility_only": 40,
                "mixed": 60,
            },
        )

    def test_full_and_quick_stage_shapes_are_explicit(self):
        self.assertEqual(
            {
                stage: spec.cells_per_profile
                for stage, spec in search.FULL_STAGE_SPECS.items()
            },
            {"grid": 128, "local": 128},
        )
        self.assertEqual(
            {
                stage: spec.cells_per_profile
                for stage, spec in search.QUICK_STAGE_SPECS.items()
            },
            {"stage1": 2, "stage2": 4, "stage3": 8},
        )
        jobs = search.jobs_for_spec(
            self.profiles[:2],
            search.FULL_STAGE_SPECS["grid"],
            mode="full",
            stage="grid",
        )
        self.assertEqual(len(jobs), 256)
        self.assertEqual({job["seed"] for job in jobs}, {41, 7})
        self.assertEqual(
            {job["path_name"] for job in jobs},
            set(search.FULL_MATRIX_SPEC.paths),
        )
        self.assertEqual(
            {job["flow"]["flow_regime"] for job in jobs},
            {flow["flow_regime"] for flow in search.FLOW_REGIMES},
        )
        self.assertEqual(
            {job["routing_mode"] for job in jobs}, {"fixed", "elastic"}
        )

    def test_search_guardrails_use_marginal_tracking_not_raw_probe_staleness(self):
        self.assertIn(
            "median_marginal_tracking_stale_fraction",
            search.SEARCH_SERVICE_GUARDRAILS,
        )
        self.assertIn(
            "worst_regime_median_marginal_tracking_stale_fraction",
            search.SEARCH_SERVICE_GUARDRAILS,
        )
        self.assertNotIn(
            "median_fee_free_stale_fraction",
            search.SEARCH_SERVICE_GUARDRAILS,
        )
        self.assertNotIn(
            "median_all_in_stale_fraction",
            search.SEARCH_SERVICE_GUARDRAILS,
        )

    def test_summary_and_ranking_are_dusk_independent(self):
        profile = self.profiles[0]
        spec = search.MatrixSpec(
            ("flat",),
            ("benign", "toxic_delayed", "low_volume_stuck"),
            (41,),
        )
        jobs = search.jobs_for_spec(
            (profile,), spec, mode="test", stage="summary"
        )
        rows = [fake_row(job) for job in jobs]
        summary = search.summarize_profiles(rows, (profile,))[0]
        self.assertTrue(summary["hard_valid"])
        self.assertEqual(summary["independent_dusk_service_relaxation"], 0.0)
        self.assertTrue(summary["original_service_envelope_pass"])

        expensive = [fake_row(job, selected_cost=307.7) for job in jobs]
        expensive_summary = search.summarize_profiles(expensive, (profile,))[0]
        self.assertAlmostEqual(
            expensive_summary["independent_dusk_service_relaxation"],
            11.308,
            places=6,
        )

        candidates = [
            {
                "profile_id": "invalid",
                "hard_valid": False,
                "independent_dusk_service_relaxation": 0.0,
                "minimum_route_selection_utility": 100.0,
                "mean_route_selection_utility": 100.0,
            },
            {
                "profile_id": "infinite_rho",
                "hard_valid": True,
                "independent_dusk_service_relaxation": float("inf"),
                "minimum_route_selection_utility": 1_000.0,
                "mean_route_selection_utility": 1_000.0,
            },
            {
                "profile_id": "rho_high",
                "hard_valid": True,
                "independent_dusk_service_relaxation": 0.2,
                "minimum_route_selection_utility": 100.0,
                "mean_route_selection_utility": 100.0,
            },
            {
                "profile_id": "rho_zero_low_utility",
                "hard_valid": True,
                "independent_dusk_service_relaxation": 0.0,
                "minimum_route_selection_utility": 1.0,
                "mean_route_selection_utility": 2.0,
            },
            {
                "profile_id": "rho_zero_high_utility",
                "hard_valid": True,
                "independent_dusk_service_relaxation": 0.0,
                "minimum_route_selection_utility": 2.0,
                "mean_route_selection_utility": 2.0,
            },
        ]
        self.assertEqual(
            [row["profile_id"] for row in search.rank_summaries(candidates)],
            [
                "rho_zero_high_utility",
                "rho_zero_low_utility",
                "rho_high",
                "invalid",
                "infinite_rho",
            ],
        )

    def test_local_neighborhood_is_deterministic_and_one_field_at_a_time(self):
        winner = self.profiles[55]
        first = search.local_neighborhood(winner)
        second = search.local_neighborhood(winner)
        self.assertEqual(
            [profile.profile_id for profile in first],
            [profile.profile_id for profile in second],
        )
        self.assertEqual(first[0], winner)
        self.assertEqual(
            len({profile.profile_id for profile in first}), len(first)
        )
        base = search._search_point(winner)
        for neighbor in first[1:]:
            point = search._search_point(neighbor)
            changed = [field for field in base if point[field] != base[field]]
            self.assertEqual(len(changed), 1, neighbor.profile_id)
            self.assertGreaterEqual(
                point["adjustment_threshold_bps"],
                point["adjustment_step_bps"],
            )

    def test_stage_fingerprint_is_stable_and_mode_bound(self):
        spec = search.QUICK_STAGE_SPECS["stage1"]
        profiles = self.profiles[:2]
        first = search.stage_fingerprint(
            profiles, spec, mode="quick", stage="stage1"
        )
        second = search.stage_fingerprint(
            profiles, spec, mode="quick", stage="stage1"
        )
        full = search.stage_fingerprint(
            profiles, spec, mode="full", stage="stage1"
        )
        changed_profile = NativeProfile(
            profiles[0].system,
            profiles[0].profile_id,
            profiles[0].model,
            replace(
                profiles[0].config,
                center_ema_half_life_minutes=3.0,
            ),
            profiles[0].description,
        )
        changed = search.stage_fingerprint(
            (changed_profile, profiles[1]),
            spec,
            mode="quick",
            stage="stage1",
        )
        self.assertEqual(first, second)
        self.assertNotEqual(first, full)
        self.assertNotEqual(first, changed)

    def test_stage_output_is_atomic_resumable_and_noncanonical(self):
        profile = self.profiles[0]
        spec = search.MatrixSpec(("flat",), ("benign",), (41,))
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)

            def run_fake(jobs, workers, label):
                del workers, label
                return [fake_row(job) for job in jobs]

            with patch.object(search, "_run_jobs", side_effect=run_fake) as runner:
                rows, summaries, selected, fingerprint = search.run_stage(
                    (profile,),
                    spec,
                    output_root=output,
                    mode="quick",
                    stage="stage1",
                    workers=1,
                    selection_count=1,
                )
            self.assertEqual(runner.call_count, 1)
            self.assertEqual(len(rows), 2)
            self.assertEqual(len(summaries), 1)
            self.assertEqual(selected, (profile,))
            manifest = json.loads(
                (output / "stage1" / "manifest.json").read_text()
            )
            self.assertEqual(manifest["fingerprint"], fingerprint)
            self.assertFalse(manifest["canonical_native_output"])
            self.assertEqual(manifest["new_rows"], 2)
            for filename in ("rows.csv", "profiles.csv"):
                digest = hashlib.sha256(
                    (output / "stage1" / filename).read_bytes()
                ).hexdigest()
                self.assertEqual(manifest["content_hashes"][filename], digest)

            with patch.object(
                search,
                "_run_jobs",
                side_effect=AssertionError("resumed stage must not rerun"),
            ) as resumed_runner:
                resumed = search.run_stage(
                    (profile,),
                    spec,
                    output_root=output,
                    mode="quick",
                    stage="stage1",
                    workers=1,
                    selection_count=1,
                )
            self.assertEqual(resumed_runner.call_count, 0)
            self.assertEqual(len(resumed[0]), 2)
            self.assertEqual(resumed[3], fingerprint)

            rows_path = output / "stage1" / "rows.csv"
            rows_path.write_text(rows_path.read_text() + "\n")
            with patch.object(search, "_run_jobs", side_effect=run_fake) as rerun:
                regenerated = search.run_stage(
                    (profile,),
                    spec,
                    output_root=output,
                    mode="quick",
                    stage="stage1",
                    workers=1,
                    selection_count=1,
                )
            self.assertEqual(rerun.call_count, 1)
            self.assertEqual(len(regenerated[0]), 2)

    def test_in_progress_batches_resume_only_missing_cells(self):
        profile = self.profiles[0]
        spec = search.MatrixSpec(("flat", "single_jump"), ("benign",), (41,))
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            calls = 0

            def interrupt_after_first_batch(jobs, workers, label):
                nonlocal calls
                del workers, label
                calls += 1
                if calls == 2:
                    raise RuntimeError("simulated interruption")
                return [fake_row(job) for job in jobs]

            with patch.object(search, "CHECKPOINT_BATCH_SIZE", 2), patch.object(
                search, "_run_jobs", side_effect=interrupt_after_first_batch
            ):
                with self.assertRaisesRegex(RuntimeError, "simulated interruption"):
                    search.run_stage(
                        (profile,),
                        spec,
                        output_root=output,
                        mode="quick",
                        stage="crash_resume",
                        workers=1,
                        selection_count=1,
                    )

            progress = json.loads(
                (output / "crash_resume" / "progress.json").read_text()
            )
            self.assertEqual(progress["completed_rows"], 2)
            resumed_sizes = []

            def finish_missing(jobs, workers, label):
                del workers, label
                resumed_sizes.append(len(jobs))
                return [fake_row(job) for job in jobs]

            with patch.object(search, "CHECKPOINT_BATCH_SIZE", 2), patch.object(
                search, "_run_jobs", side_effect=finish_missing
            ):
                rows, _, selected, _ = search.run_stage(
                    (profile,),
                    spec,
                    output_root=output,
                    mode="quick",
                    stage="crash_resume",
                    workers=1,
                    selection_count=1,
                )
            self.assertEqual(resumed_sizes, [2])
            self.assertEqual(len(rows), 4)
            self.assertEqual(selected, (profile,))
            self.assertFalse(
                (output / "crash_resume" / "progress.json").exists()
            )

    def test_full_search_uses_one_exhaustive_grid_before_local_search(self):
        calls = []

        def fake_stage(profiles, spec, **kwargs):
            calls.append(
                (
                    kwargs["stage"],
                    len(profiles),
                    spec.cells_per_profile,
                    kwargs["selection_count"],
                )
            )
            selected = (profiles[0],)
            summaries = [
                {
                    "profile_id": profiles[0].profile_id,
                    "hard_valid": True,
                    "independent_dusk_service_relaxation": 0.0,
                    "original_service_envelope_pass": True,
                    "minimum_route_selection_utility": 1.0,
                    "mean_route_selection_utility": 1.0,
                }
            ]
            return (
                [{} for _ in range(len(profiles) * spec.cells_per_profile)],
                summaries,
                selected,
                f"{kwargs['stage']}-fingerprint",
            )

        with tempfile.TemporaryDirectory() as temporary, patch.object(
            search, "FULL_OUTPUT", Path(temporary)
        ), patch.object(search, "run_stage", side_effect=fake_stage):
            summary = search.run_search(workers=1, quick=False)

        self.assertEqual(calls[0], ("grid", 150, 128, 1))
        self.assertEqual(calls[1][0], "local")
        self.assertEqual(calls[1][2:], (128, 1))
        self.assertEqual(summary["status"], "controller_search_complete")
        self.assertTrue(summary["decision_grade_search"])
        self.assertTrue(
            summary["complete_matrix_evaluated_for_every_initial_profile"]
        )
        self.assertEqual(summary["stage_counts"]["grid"]["rows"], 19_200)
        self.assertEqual(set(summary["stage_counts"]), {"grid", "local"})

    def test_quick_and_full_outputs_cannot_be_confused(self):
        self.assertNotEqual(search.QUICK_OUTPUT, search.FULL_OUTPUT)
        self.assertEqual(search.QUICK_OUTPUT.name, "quick")
        self.assertEqual(search.FULL_OUTPUT.name, "full")


if __name__ == "__main__":
    unittest.main()

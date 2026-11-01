import hashlib
import json
import sys
import tempfile
import unittest
from collections import Counter
from dataclasses import asdict, replace
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import curve_controller_search as search
import native_benchmark
import scenarios
from native_benchmark import NativeProfile


def fake_row(job, *, selected_cost=10.0, marginal_stale=0.0, lp_return=2.0):
    profile = job["profile"]
    flow = job["flow"]
    return {
        "system": "curve_native_v2_1",
        "model": "curve",
        "profile_id": profile.profile_id,
        "profile_description": profile.description,
        "path": job["path_name"],
        "path_class": "synthetic",
        "flow_regime": flow["flow_regime"],
        "seed": job["seed"],
        "routing_mode": job["routing_mode"],
        "analysis_slice": job["analysis_slice"],
        "net_lp_return_after_protocol_fee_bps": lp_return,
        "captured_user_volume_fraction": 0.75,
        "selected_execution_cost_bps": selected_cost,
        "marginal_tracking_stale_fraction": marginal_stale,
        "accounting_identity_error": 0.0,
        "controller_external_price_reads": 0,
        "arbitrage_capped_event_count": 0,
        "arbitrage_numerical_failure_count": 0,
    }


class CurveControllerSearchTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profiles = search.grid_profiles()

    def test_grid_is_exact_unique_and_keeps_invariant_fixed(self):
        self.assertEqual(len(search.CONTROLLER_TUPLES), 10)
        self.assertEqual(len(search.FEE_BUNDLES), 15)
        self.assertEqual(search.PROFILE_BUDGET, 150)
        self.assertEqual(len(self.profiles), 150)
        self.assertEqual(
            self.profiles[0].profile_id, "curve_grid_c00_f00"
        )
        self.assertEqual(
            self.profiles[-1].profile_id, "curve_grid_c09_f14"
        )
        self.assertEqual(
            len({profile.profile_id for profile in self.profiles}), 150
        )
        self.assertEqual(
            len(
                {
                    json.dumps(
                        {
                            "model": asdict(profile.model),
                            "config": asdict(profile.config),
                        },
                        sort_keys=True,
                        separators=(",", ":"),
                    )
                    for profile in self.profiles
                }
            ),
            150,
        )

        invariant, _ = search._matched_models("equal_execution")
        self.assertTrue(
            all(
                profile.model.amplification == invariant.amplification
                and profile.model.gamma == invariant.gamma
                for profile in self.profiles
            )
        )
        invariant_shapes = {
            (profile.model.amplification, profile.model.gamma)
            for profile in self.profiles
        }
        self.assertEqual(len(invariant_shapes), 1)

    def test_grid_contains_default_pinned_and_fee_shape_anchors(self):
        default = self.profiles[0]
        self.assertEqual(search.CONTROLLER_TUPLES[0].policy_id, "matched_default")
        self.assertEqual(search.FEE_BUNDLES[0].policy_id, "matched_default")
        self.assertEqual(default.config.ema_half_life_minutes, 30.0)
        self.assertEqual(default.config.min_adjustment_step, 0.001)
        self.assertEqual(default.config.allowed_extra_profit, 0.000002)
        self.assertEqual(default.model.mid_fee, 0.0005)
        self.assertEqual(default.model.out_fee, 0.0045)
        self.assertEqual(default.model.fee_gamma, 0.01)

        pinned = self.profiles[1 * 15 + 1]
        self.assertEqual(pinned.profile_id, "curve_grid_c01_f01")
        self.assertAlmostEqual(
            pinned.config.ema_half_life_minutes, 601.0 / 60.0
        )
        self.assertEqual(pinned.config.min_adjustment_step, 0.000146)
        self.assertEqual(pinned.config.allowed_extra_profit, 0.000002)
        self.assertEqual(pinned.model.mid_fee, 0.0026)
        self.assertEqual(pinned.model.out_fee, 0.0045)
        self.assertEqual(pinned.model.fee_gamma, 0.00023)

        bundles = {bundle.policy_id: bundle for bundle in search.FEE_BUNDLES}
        self.assertEqual(
            bundles["base_like_flat"].mid_fee,
            bundles["base_like_flat"].out_fee,
        )
        self.assertGreater(
            bundles["wide_tail"].out_fee,
            bundles["matched_default"].out_fee,
        )
        self.assertLess(
            bundles["default_narrow_transition"].fee_gamma,
            bundles["matched_default"].fee_gamma,
        )
        self.assertGreater(
            bundles["default_wide_transition"].fee_gamma,
            bundles["matched_default"].fee_gamma,
        )
        self.assertTrue(
            all(
                0.0 <= bundle.mid_fee <= bundle.out_fee < 1.0
                and bundle.fee_gamma > 0.0
                for bundle in search.FEE_BUNDLES
            )
        )

    def test_full_matrix_has_all_128_cells_for_every_candidate(self):
        spec = search.FULL_MATRIX_SPEC
        self.assertEqual(spec.cells_per_profile, 128)
        self.assertEqual(len(spec.paths), 8)
        self.assertEqual(len(spec.flows), 4)
        self.assertEqual(set(spec.seeds), {7, 41})
        jobs = search.jobs_for_spec(
            self.profiles, spec, mode="full", stage="grid"
        )
        self.assertEqual(len(jobs), 150 * 128)
        counts = Counter(job["profile"].profile_id for job in jobs)
        self.assertEqual(set(counts.values()), {128})
        self.assertEqual(
            {job["path_name"] for job in jobs}, set(spec.paths)
        )
        self.assertEqual(
            {job["flow"]["flow_regime"] for job in jobs}, set(spec.flows)
        )
        self.assertEqual(
            {job["routing_mode"] for job in jobs}, {"fixed", "elastic"}
        )
        self.assertEqual({job["path_class"] for job in jobs}, {"synthetic"})

    def test_guardrails_and_own_rho_ranking_match_native_contract(self):
        self.assertEqual(
            search.SEARCH_SERVICE_GUARDRAILS,
            native_benchmark.SERVICE_GUARDRAILS,
        )
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
        self.assertEqual(summary["independent_curve_service_relaxation"], 0.0)
        self.assertTrue(summary["original_service_envelope_pass"])

        expensive = [fake_row(job, selected_cost=307.7) for job in jobs]
        expensive_summary = search.summarize_profiles(expensive, (profile,))[0]
        self.assertAlmostEqual(
            expensive_summary["independent_curve_service_relaxation"],
            11.308,
            places=6,
        )

        candidates = [
            {
                "profile_id": "invalid",
                "hard_valid": False,
                "independent_curve_service_relaxation": 0.0,
                "minimum_route_selection_utility": 1_000.0,
                "mean_route_selection_utility": 1_000.0,
            },
            {
                "profile_id": "infinite_rho",
                "hard_valid": True,
                "independent_curve_service_relaxation": float("inf"),
                "minimum_route_selection_utility": 1_000.0,
                "mean_route_selection_utility": 1_000.0,
            },
            {
                "profile_id": self.profiles[0].profile_id,
                "hard_valid": True,
                "independent_curve_service_relaxation": 0.2,
                "minimum_route_selection_utility": 100.0,
                "mean_route_selection_utility": 100.0,
            },
            {
                "profile_id": self.profiles[1].profile_id,
                "hard_valid": True,
                "independent_curve_service_relaxation": 0.0,
                "minimum_route_selection_utility": 1.0,
                "mean_route_selection_utility": 2.0,
            },
            {
                "profile_id": self.profiles[2].profile_id,
                "hard_valid": True,
                "independent_curve_service_relaxation": 0.0,
                "minimum_route_selection_utility": 2.0,
                "mean_route_selection_utility": 2.0,
            },
        ]
        ranked_ids = [
            row["profile_id"] for row in search.rank_summaries(candidates)
        ]
        self.assertEqual(
            ranked_ids[:3],
            [
                self.profiles[2].profile_id,
                self.profiles[1].profile_id,
                self.profiles[0].profile_id,
            ],
        )
        selected = search.select_profiles(candidates, self.profiles[:3])
        self.assertEqual(selected, (self.profiles[2],))
        only_infinite = [candidates[1]]
        with self.assertRaisesRegex(RuntimeError, "finite service relaxation"):
            search.select_profiles(only_infinite, (self.profiles[0],))

    def test_no_historical_path_provider_is_read(self):
        with patch.object(
            scenarios,
            "native_historical_windows",
            side_effect=AssertionError("history must not be read"),
        ), patch.object(
            native_benchmark,
            "native_historical_windows",
            side_effect=AssertionError("history must not be read"),
        ):
            jobs = search.jobs_for_spec(
                self.profiles[:2],
                search.QUICK_MATRIX_SPEC,
                mode="quick",
                stage="smoke",
            )
        self.assertEqual(len(jobs), 4)
        self.assertEqual({job["path_class"] for job in jobs}, {"synthetic"})
        self.assertFalse(
            any("history" in filename.lower() for filename in search.ENGINE_FILES)
        )

    def test_stage_is_content_hashed_atomic_and_resumable(self):
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
                    stage="smoke",
                    workers=1,
                )
            self.assertEqual(runner.call_count, 1)
            self.assertEqual(len(rows), 2)
            self.assertEqual(len(summaries), 1)
            self.assertEqual(selected, (profile,))
            manifest = json.loads(
                (output / "smoke" / "manifest.json").read_text()
            )
            self.assertEqual(manifest["fingerprint"], fingerprint)
            self.assertEqual(manifest["checkpoint_batch_size"], 256)
            self.assertFalse(manifest["canonical_native_output"])
            for filename in ("rows.csv", "profiles.csv"):
                digest = hashlib.sha256(
                    (output / "smoke" / filename).read_bytes()
                ).hexdigest()
                self.assertEqual(manifest["content_hashes"][filename], digest)

            with patch.object(
                search,
                "_run_jobs",
                side_effect=AssertionError("completed stage must resume"),
            ) as resumed_runner:
                resumed = search.run_stage(
                    (profile,),
                    spec,
                    output_root=output,
                    mode="quick",
                    stage="smoke",
                    workers=1,
                )
            self.assertEqual(resumed_runner.call_count, 0)
            self.assertEqual(len(resumed[0]), 2)
            self.assertEqual(resumed[3], fingerprint)

    def test_interrupted_256_cell_batches_resume_only_missing_suffix(self):
        profile = self.profiles[0]
        spec = search.MatrixSpec(
            ("flat", "single_jump"), ("benign",), (41,)
        )
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
                    )

            progress = json.loads(
                (output / "crash_resume" / "progress.json").read_text()
            )
            self.assertEqual(progress["completed_rows"], 2)
            self.assertEqual(progress["checkpoint_batch_size"], 2)
            batch = progress["batches"][0]
            batch_path = output / "crash_resume" / batch["file"]
            self.assertEqual(
                batch["sha256"], hashlib.sha256(batch_path.read_bytes()).hexdigest()
            )

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
                )
            self.assertEqual(resumed_sizes, [2])
            self.assertEqual(len(rows), 4)
            self.assertEqual(selected, (profile,))
            self.assertFalse(
                (output / "crash_resume" / "progress.json").exists()
            )

    def test_run_search_full_and_quick_control_flow_are_separate(self):
        calls = []

        def fake_stage(profiles, spec, **kwargs):
            calls.append(
                (
                    kwargs["mode"],
                    kwargs["stage"],
                    len(profiles),
                    spec.cells_per_profile,
                )
            )
            winner = profiles[0]
            summaries = [
                {
                    "profile_id": winner.profile_id,
                    "hard_valid": True,
                    "independent_curve_service_relaxation": 0.0,
                    "original_service_envelope_pass": True,
                    "minimum_route_selection_utility": 1.0,
                    "mean_route_selection_utility": 1.5,
                }
            ]
            return (
                [{} for _ in range(len(profiles) * spec.cells_per_profile)],
                summaries,
                (winner,),
                f"{kwargs['mode']}-{kwargs['stage']}-fingerprint",
            )

        with tempfile.TemporaryDirectory() as temporary, patch.object(
            search, "FULL_OUTPUT", Path(temporary) / "full"
        ), patch.object(
            search, "QUICK_OUTPUT", Path(temporary) / "quick"
        ), patch.object(search, "run_stage", side_effect=fake_stage):
            full = search.run_search(workers=1, quick=False)
            quick = search.run_search(workers=1, quick=True)

        self.assertEqual(calls[0], ("full", "grid", 150, 128))
        self.assertEqual(calls[1], ("quick", "smoke", 150, 2))
        self.assertEqual(full["status"], "curve_controller_search_complete")
        self.assertTrue(full["decision_grade_search"])
        self.assertTrue(full["complete_matrix_evaluated_for_every_profile"])
        self.assertEqual(full["rows"], 19_200)
        self.assertEqual(full["winner"]["profile_id"], self.profiles[0].profile_id)
        self.assertIn("model", full["winner"]["full_config"])
        self.assertIn("controller", full["winner"]["full_config"])
        self.assertEqual(quick["status"], "smoke_only")
        self.assertFalse(quick["decision_grade_search"])
        self.assertFalse(quick["complete_matrix_evaluated_for_every_profile"])
        self.assertNotEqual(search.QUICK_OUTPUT, search.FULL_OUTPUT)

    def test_fingerprint_binds_mode_profile_and_256_batch_contract(self):
        profiles = self.profiles[:2]
        spec = search.QUICK_MATRIX_SPEC
        first = search.stage_fingerprint(
            profiles, spec, mode="quick", stage="smoke"
        )
        second = search.stage_fingerprint(
            profiles, spec, mode="quick", stage="smoke"
        )
        full_mode = search.stage_fingerprint(
            profiles, spec, mode="full", stage="smoke"
        )
        changed_profile = NativeProfile(
            profiles[0].system,
            profiles[0].profile_id,
            profiles[0].model,
            replace(profiles[0].config, ema_half_life_minutes=31.0),
            profiles[0].description,
        )
        changed = search.stage_fingerprint(
            (changed_profile, profiles[1]),
            spec,
            mode="quick",
            stage="smoke",
        )
        with patch.object(search, "CHECKPOINT_BATCH_SIZE", 128):
            changed_batch = search.stage_fingerprint(
                profiles, spec, mode="quick", stage="smoke"
            )
        self.assertEqual(first, second)
        self.assertNotEqual(first, full_mode)
        self.assertNotEqual(first, changed)
        self.assertNotEqual(first, changed_batch)
        self.assertEqual(search.CHECKPOINT_BATCH_SIZE, 256)


if __name__ == "__main__":
    unittest.main()

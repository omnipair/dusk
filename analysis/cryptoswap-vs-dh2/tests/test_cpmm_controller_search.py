from __future__ import annotations

import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch

import cpmm_controller_search as search


class CpmmControllerSearchTests(unittest.TestCase):
    def test_grid_is_exactly_150_zero_depth_profiles(self):
        profiles = search.grid_profiles()
        self.assertEqual(len(profiles), 150)
        self.assertEqual(len({profile.profile_id for profile in profiles}), 150)
        self.assertEqual({profile.system for profile in profiles}, {"dusk_cpmm"})
        self.assertTrue(
            all(
                profile.model.peak_depth == 0.0
                and profile.model.fade_scale == 0.0
                for profile in profiles
            )
        )

    def test_full_matrix_is_128_cells_per_profile(self):
        self.assertEqual(search.FULL_MATRIX_SPEC.cells_per_profile, 128)
        self.assertEqual(150 * search.FULL_MATRIX_SPEC.cells_per_profile, 19_200)

    def test_full_summary_is_decision_grade_and_has_no_history(self):
        profiles = search.grid_profiles()
        winner = profiles[0]
        summary_row = {
            "profile_id": winner.profile_id,
            "hard_valid": True,
            "independent_dusk_service_relaxation": 0.0,
            "original_service_envelope_pass": True,
            "minimum_route_selection_utility": 1.0,
            "mean_route_selection_utility": 1.5,
        }

        def fake_stage(stage_profiles, spec, **kwargs):
            self.assertEqual(len(stage_profiles), 150)
            self.assertEqual(spec.cells_per_profile, 128)
            self.assertEqual(kwargs["stage"], "grid")
            return ([{}] * 19_200, [summary_row], (winner,), "a" * 64)

        with tempfile.TemporaryDirectory() as temporary, patch.object(
            search, "FULL_OUTPUT", Path(temporary)
        ), patch.object(search, "run_stage", side_effect=fake_stage):
            result = search.run_search(workers=1, quick=False)
        self.assertEqual(result["status"], "cpmm_controller_search_complete")
        self.assertTrue(result["decision_grade_search"])
        self.assertTrue(result["complete_matrix_evaluated_for_every_profile"])
        self.assertEqual(result["rows"], 19_200)
        self.assertEqual(result["historical_reads"], 0)
        self.assertEqual(result["winner"]["profile_id"], winner.profile_id)
        self.assertEqual(result["winner"]["full_config"]["model"]["peak_depth"], 0.0)

    def test_quick_output_is_separate_and_not_decision_grade(self):
        self.assertNotEqual(search.QUICK_OUTPUT, search.FULL_OUTPUT)
        profiles = search.grid_profiles()
        winner = profiles[0]
        summary_row = {
            "profile_id": winner.profile_id,
            "hard_valid": True,
            "independent_dusk_service_relaxation": 0.0,
            "original_service_envelope_pass": True,
            "minimum_route_selection_utility": 1.0,
            "mean_route_selection_utility": 1.0,
        }

        def fake_stage(stage_profiles, spec, **kwargs):
            return (
                [{}] * (len(stage_profiles) * spec.cells_per_profile),
                [summary_row],
                (winner,),
                "b" * 64,
            )

        with tempfile.TemporaryDirectory() as temporary, patch.object(
            search, "QUICK_OUTPUT", Path(temporary)
        ), patch.object(search, "run_stage", side_effect=fake_stage):
            result = search.run_search(workers=1, quick=True)
        self.assertEqual(result["status"], "smoke_only")
        self.assertFalse(result["decision_grade_search"])
        self.assertFalse(result["complete_matrix_evaluated_for_every_profile"])


if __name__ == "__main__":
    unittest.main()

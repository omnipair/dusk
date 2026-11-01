import math
from pathlib import Path
import sys
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from comparison import (
    CURVE_AMPLIFICATION_MAX,
    CURVE_AMPLIFICATION_MIN,
    CURVE_ANN_NORMALIZER,
    CURVE_GAMMA_MAX,
    CURVE_GAMMA_MIN,
    CURVE_MAX_ANN_RAW,
    CURVE_MIN_ANN_RAW,
    PROPOSED_DEPTH_MAX,
    PROPOSED_DEPTH_MIN,
    PROPOSED_HALF_WIDTH_BPS_MAX,
    PROPOSED_HALF_WIDTH_BPS_MIN,
    _iso_target_best,
    _refine_match,
    curve_candidates,
    execution_index_bps,
    jump_loss_index_bps,
    proposed_candidates,
)
from models import CryptoSwap, CurrentDh2


class IsoTargetSolverTests(unittest.TestCase):
    def test_curve_sweep_stays_inside_pinned_v21_parameter_domain(self):
        candidates = curve_candidates()
        self.assertEqual(len(candidates), 81)
        self.assertTrue(
            all(
                CURVE_AMPLIFICATION_MIN
                <= model.amplification
                <= CURVE_AMPLIFICATION_MAX
                and CURVE_GAMMA_MIN <= model.gamma <= CURVE_GAMMA_MAX
                for model in candidates
            )
        )
        self.assertEqual(CURVE_AMPLIFICATION_MIN, 0.1)
        self.assertEqual(CURVE_AMPLIFICATION_MAX, 1_000.0)
        self.assertEqual(
            CURVE_MIN_ANN_RAW / CURVE_ANN_NORMALIZER,
            CURVE_AMPLIFICATION_MIN,
        )
        self.assertEqual(
            CURVE_MAX_ANN_RAW / CURVE_ANN_NORMALIZER,
            CURVE_AMPLIFICATION_MAX,
        )
        self.assertEqual(CURVE_GAMMA_MIN, 1e-8)
        self.assertEqual(CURVE_GAMMA_MAX, 0.199)

    def test_log_sweep_uses_observable_center_depth_matched_experimental_domain(self):
        candidates = proposed_candidates()
        self.assertEqual(len(candidates), 81)
        self.assertEqual((PROPOSED_DEPTH_MIN, PROPOSED_DEPTH_MAX), (3.0, 2_001.0))
        self.assertEqual(
            (PROPOSED_HALF_WIDTH_BPS_MIN, PROPOSED_HALF_WIDTH_BPS_MAX),
            (1.0, 20_000.0),
        )
        self.assertTrue(
            all(
                PROPOSED_DEPTH_MIN
                <= model.depth_multiplier
                <= PROPOSED_DEPTH_MAX
                and PROPOSED_HALF_WIDTH_BPS_MIN
                <= model.half_width_bps
                <= PROPOSED_HALF_WIDTH_BPS_MAX
                for model in candidates
            )
        )

    def test_finds_both_roots_when_domain_endpoints_are_on_same_side(self):
        selected = _iso_target_best(
            lambda first, second: (first, second),
            (1.0,),
            (math.exp(-2.0), math.exp(2.0)),
            1.0,
            lambda model: math.log(model[1]) ** 2,
            lambda model: model[1],
        )
        self.assertAlmostEqual(selected[1], math.exp(-1.0), places=7)

    def test_objective_considers_every_sign_change_root(self):
        selected = _iso_target_best(
            lambda first, second: (first, second),
            (1.0,),
            (math.exp(-3.0), math.exp(3.0)),
            0.0,
            lambda model: (
                (math.log(model[1]) + 2.0)
                * (math.log(model[1]) - 0.5)
                * (math.log(model[1]) - 2.0)
            ),
            lambda model: abs(math.log(model[1]) - 0.5),
        )
        self.assertAlmostEqual(math.log(selected[1]), 0.5, places=7)

    def test_same_sign_tangent_root_is_recovered(self):
        selected = _iso_target_best(
            lambda first, second: (first, second),
            (1.0,),
            (math.exp(-2.0), math.exp(2.0)),
            0.0,
            lambda model: (math.log(model[1]) - 0.37) ** 2,
            lambda model: model[1],
        )
        self.assertAlmostEqual(math.log(selected[1]), 0.37, places=5)

    def test_nonfinite_island_is_never_used_as_a_root_bracket(self):
        def metric(model):
            value = math.log(model[1])
            return math.nan if abs(value) < 0.5 else value

        with self.assertRaisesRegex(ArithmeticError, "no finite iso-target root"):
            _iso_target_best(
                lambda first, second: (first, second),
                (1.0,),
                (math.exp(-2.0), math.exp(2.0)),
                0.0,
                metric,
                lambda model: model[1],
            )

    def test_endpoint_root_is_retained(self):
        selected = _iso_target_best(
            lambda first, second: (first, second),
            (1.0,),
            (math.exp(-2.0), math.exp(2.0)),
            -2.0,
            lambda model: math.log(model[1]),
            lambda model: model[1],
        )
        self.assertAlmostEqual(math.log(selected[1]), -2.0, places=7)

    def test_first_parameter_grid_is_refined_to_convergence(self):
        diagnostics = {}
        selected = _iso_target_best(
            lambda first, second: (first, second),
            (1.0, 4.0, 16.0),
            (1.0, 4.0),
            2.0,
            lambda model: model[1],
            lambda model: math.log(model[0] / 2.0) ** 2,
            diagnostics=diagnostics,
        )
        self.assertAlmostEqual(selected[0], 2.0, places=12)
        self.assertAlmostEqual(selected[1], 2.0, places=12)
        self.assertTrue(diagnostics["converged"])
        self.assertEqual(diagnostics["coarse_first_grid_points"], 3)
        self.assertEqual(diagnostics["final_first_grid_points"], 9)
        self.assertEqual(diagnostics["unique_first_values_evaluated"], 9)
        self.assertEqual(
            [row["first_grid_points"] for row in diagnostics["first_grid_levels"]],
            [3, 5, 9],
        )
        self.assertLessEqual(
            diagnostics["last_objective_improvement_bps"],
            diagnostics["objective_convergence_tolerance_bps"],
        )
        self.assertEqual(
            diagnostics["second_axis_winner_audit_points"],
            [129, 257, 513],
        )
        self.assertTrue(diagnostics["second_axis_converged"])
        self.assertEqual(
            [
                row["second_scan_points"]
                for row in diagnostics["second_axis_winner_levels"]
            ],
            [129, 257, 513],
        )

    def test_first_parameter_refinement_fails_closed_when_final_grid_moves(self):
        diagnostics = {}
        with self.assertRaisesRegex(
            ArithmeticError,
            "first-parameter iso-target optimum did not converge",
        ):
            _iso_target_best(
                lambda first, second: (first, second),
                (1.0, 16.0),
                (1.0, 4.0),
                2.0,
                lambda model: model[1],
                lambda model: math.log(model[0] / 2.0) ** 2,
                diagnostics=diagnostics,
            )
        self.assertFalse(diagnostics["converged"])
        self.assertEqual(diagnostics["final_first_grid_points"], 5)
        self.assertGreater(
            diagnostics["last_objective_improvement_bps"],
            diagnostics["objective_convergence_tolerance_bps"],
        )

    def test_second_parameter_winner_audit_fails_when_root_count_changes(self):
        diagnostics = {}
        with self.assertRaisesRegex(
            ArithmeticError,
            "second-parameter iso-target winner audit did not converge",
        ):
            _iso_target_best(
                lambda first, second: (first, second),
                (1.0,),
                (1.0, math.e),
                0.0,
                lambda model: math.sin(400.0 * math.pi * math.log(model[1])),
                lambda model: model[1],
                diagnostics=diagnostics,
            )
        self.assertFalse(diagnostics["second_axis_converged"])
        levels = diagnostics["second_axis_winner_levels"]
        self.assertEqual(
            [row["second_scan_points"] for row in levels],
            [129, 257, 513],
        )
        self.assertNotEqual(
            levels[-2]["scan_exposed_root_count"],
            levels[-1]["scan_exposed_root_count"],
        )

    def test_full_profile_refiner_resets_latent_coordinates_to_clamped_model(self):
        score_calls = 0

        def score(model, _target):
            nonlocal score_calls
            score_calls += 1
            return (
                (model.peak_depth / 2_000.0 - 1.0) ** 2
                + (model.fade_scale / 0.5 - 1.0) ** 2
            )

        def constructor(first, second):
            return CurrentDh2(
                peak_depth=min(max(first, 2.0), 2_000.0),
                fade_scale=min(max(second, 1e-6), 0.5),
            )

        with patch("comparison.service_max_log_error", side_effect=score):
            selected = _refine_match(
                constructor,
                (2_000.0, 0.5),
                (),
            )
        self.assertEqual(selected.peak_depth, 2_000.0)
        self.assertEqual(selected.fade_scale, 0.5)
        # One initial score plus at most four distinct clamped candidates per
        # refinement, rather than rescoring all nine latent shifts.
        self.assertLessEqual(score_calls, 25)

    def test_no_root_raises(self):
        with self.assertRaisesRegex(ArithmeticError, "no finite iso-target root"):
            _iso_target_best(
                lambda first, second: (first, second),
                (1.0,),
                (1.0, 10.0),
                -1.0,
                lambda model: model[1],
                lambda model: model[1],
            )

    def test_current_dh2_nonmonotone_regression_finds_low_fade_root(self):
        target = execution_index_bps(
            CryptoSwap(amplification=400.0, gamma=0.001)
        )
        selected = _iso_target_best(
            lambda first, second: CurrentDh2(
                peak_depth=first,
                fade_scale=second,
            ),
            (4_000.0,),
            (1e-6, 0.5),
            target,
            execution_index_bps,
            jump_loss_index_bps,
        )
        self.assertLess(selected.fade_scale, 1e-4)
        self.assertAlmostEqual(
            execution_index_bps(selected),
            target,
            delta=1e-5,
        )


if __name__ == "__main__":
    unittest.main()

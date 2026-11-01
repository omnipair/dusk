import math
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from models import (
    Cpmm,
    CryptoSwap,
    CurrentDh2,
    HyperbolicSharpDh2,
    LogPotentialDh2,
    SharpDh2,
    bisect_root,
    scan_bracketed_roots,
    with_zero_fees,
)
from comparison import execution_index_bps


class ModelTests(unittest.TestCase):
    def setUp(self):
        self.models = [Cpmm(), CryptoSwap(), CurrentDh2(), LogPotentialDh2()]

    def test_invariants_are_symmetric_and_homogeneous(self):
        for model in self.models:
            with self.subTest(model=model.name):
                value = model.invariant(1_200_000.0, 800_000.0)
                swapped = model.invariant(800_000.0, 1_200_000.0)
                scaled = model.invariant(12_000_000.0, 8_000_000.0)
                self.assertAlmostEqual(value, swapped, delta=value * 1e-10)
                self.assertAlmostEqual(scaled, value * 10, delta=value * 1e-8)

    def test_exact_input_preserves_invariant(self):
        for model in self.models:
            model = with_zero_fees(model)
            with self.subTest(model=model.name):
                x, y = 1_000_000.0, 1_000_000.0
                before = model.invariant(x, y)
                quote = model.quote(x, y, 10_000.0, "x_to_y", apply_fee=False)
                after = model.invariant(x + quote.amount_in, y - quote.amount_out)
                self.assertAlmostEqual(before, after, delta=before * 1e-9)

    def test_log_potential_is_exact_cpmm_outside_boundary(self):
        model = LogPotentialDh2()
        x, y = 1_300_000.0, 700_000.0
        invariant = model.invariant(x, y)
        ratio = invariant / math.sqrt(x * y)
        x2, y2 = 2_600_000.0, 350_000.0
        invariant2 = model.invariant(x2, y2)
        ratio2 = invariant2 / math.sqrt(x2 * y2)
        self.assertAlmostEqual(ratio, ratio2, places=12)

    def test_log_potential_center_depth(self):
        model = LogPotentialDh2(depth_multiplier=20.0)
        epsilon = 1e-4
        p_left = model.marginal_price_raw(1 + epsilon, 1 - epsilon)
        observed_log_slope = abs(math.log(p_left)) / abs(math.log((1 - epsilon) / (1 + epsilon)))
        self.assertLess(observed_log_slope, 0.06)
        self.assertGreater(observed_log_slope, 0.04)

    def test_quotes_are_positive_and_monotonic(self):
        for model in self.models:
            previous = 0.0
            for amount in (10.0, 100.0, 1_000.0, 10_000.0, 100_000.0):
                quote = model.quote(1_000_000.0, 1_000_000.0, amount, "x_to_y")
                self.assertGreater(quote.amount_out, previous)
                self.assertLess(quote.amount_out, 1_000_000.0)
                previous = quote.amount_out

    def test_known_invariant_quote_is_bit_identical(self):
        model = CurrentDh2()
        for x, y in (
            (1_000_000.0, 1_000_000.0),
            (1_200_000.0, 800_000.0),
            (700_000.0, 1_300_000.0),
        ):
            invariant = model.invariant(x, y)
            marginal = model.marginal_price_raw_with_invariant(
                x,
                y,
                invariant,
            )
            for direction in ("x_to_y", "y_to_x"):
                with self.subTest(x=x, y=y, direction=direction):
                    regular = model.quote(
                        x,
                        y,
                        12_345.678,
                        direction,
                        apply_fee=False,
                    )
                    reused = model.quote(
                        x,
                        y,
                        12_345.678,
                        direction,
                        apply_fee=False,
                        invariant_override=invariant,
                        marginal_before_override=marginal,
                    )
                    self.assertEqual(reused, regular)

    def test_bisection_stops_when_float_bracket_cannot_shrink(self):
        threshold = math.sqrt(2.0)
        calls = 0

        def step_function(value):
            nonlocal calls
            calls += 1
            return -1.0 if value <= threshold else 1.0

        root = bisect_root(step_function, 1.0, 2.0, iterations=1_000)
        self.assertLess(calls, 100)
        self.assertLessEqual(root, math.nextafter(threshold, math.inf))
        self.assertGreaterEqual(root, threshold)

    def test_current_dh2_uses_observable_peak_depth_parameterization(self):
        model = CurrentDh2(peak_depth=200.0, fade_scale=0.1)
        self.assertEqual(model.legacy_amplification, 100.0)
        self.assertEqual(model._effective_depth(1.0, 1.0, 2.0), 100.0)

    def test_current_dh2_zero_parameters_are_exact_cpmm(self):
        dh2 = CurrentDh2(peak_depth=0.0, fade_scale=0.0)
        cpmm = Cpmm()
        for x, y in ((1.0, 1.0), (2.0, 7.0), (1_000_000.0, 25_000.0)):
            with self.subTest(x=x, y=y):
                self.assertAlmostEqual(dh2.invariant(x, y), 2 * cpmm.invariant(x, y))
                self.assertAlmostEqual(
                    dh2.marginal_price_raw(x, y),
                    cpmm.marginal_price_raw(x, y),
                    delta=cpmm.marginal_price_raw(x, y) * 1e-8,
                )

    def test_current_dh2_rejects_half_enabled_concentration(self):
        with self.assertRaises(ValueError):
            CurrentDh2(peak_depth=0.0, fade_scale=0.1)
        with self.assertRaises(ValueError):
            CurrentDh2(peak_depth=200.0, fade_scale=0.0)

    def test_sharp_dh2_witness_has_three_invariant_roots(self):
        model = SharpDh2(peak_depth=323.0, fade_scale=0.5)
        x = 6.776596755
        y = 0.036891674
        roots = scan_bracketed_roots(
            lambda invariant_d: model._residual(invariant_d, x, y),
            2 * math.sqrt(x * y),
            x + y,
            intervals=65_536,
        )
        self.assertEqual(len(roots), 3)
        for actual, expected in zip(
            roots,
            (2.082136870, 2.486864718, 2.874788264),
        ):
            self.assertAlmostEqual(actual, expected, places=8)

    def test_hyperbolic_p4_full_fade_domain_has_robust_cpmm_endpoint(self):
        model = HyperbolicSharpDh2(
            peak_depth=2.0,
            fade_scale=1e-6,
        )
        self.assertTrue(math.isfinite(execution_index_bps(model)))


if __name__ == "__main__":
    unittest.main()

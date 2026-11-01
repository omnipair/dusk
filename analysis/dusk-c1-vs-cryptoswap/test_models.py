"""Deterministic mathematical tests for the focused invariant study."""

from __future__ import annotations

from decimal import Decimal, getcontext
import math
import random
import unittest

from models import Cpmm, CryptoSwap, CryptoSwapLimit, DuskC1, normalized_reserves_at_price


getcontext().prec = 70


PROFILES = ((20.0, 0.05), (200.0, 0.005), (2_000.0, 0.0005))


def decimal_inner_residual(peak_depth: float, fade_scale: float, q: float, v: float) -> Decimal:
    """Independent Decimal evaluation of the published normalized equation."""

    p = Decimal(str(peak_depth))
    s = Decimal(str(fade_scale))
    qd = Decimal(str(q))
    vd = Decimal(str(v))
    delta = Decimal(1) - qd
    weight = (s / (s + delta)) ** 2
    h = qd.sqrt() * (Decimal(1) + vd * vd).sqrt() - Decimal(1)
    return Decimal(2) * p * qd * weight * h + qd - Decimal(1)


class CryptoSwapReferenceTests(unittest.TestCase):
    def test_inner_root_satisfies_independent_decimal_residual(self) -> None:
        for peak_depth, fade_scale in PROFILES:
            model = CryptoSwap(peak_depth, fade_scale)
            for v in (1e-5, 1e-3, 0.01, 0.1, 1.0, 10.0, 100.0):
                q, _, _ = model.q_and_prime(v)
                residual = abs(decimal_inner_residual(peak_depth, fade_scale, q, v))
                self.assertLess(residual, Decimal("3e-11"), (peak_depth, fade_scale, v, residual))

    def test_analytic_marginal_matches_level_set_difference(self) -> None:
        for peak_depth, fade_scale in PROFILES:
            model = CryptoSwap(peak_depth, fade_scale)
            for signed_u in (-1.0, -0.1, 0.1, 1.0):
                x, y = model.coordinates(2.0, signed_u)
                amount = x * 1e-7
                quote = model.quote_exact_in(x, y, amount)
                finite_price = quote.amount_out / amount
                self.assertAlmostEqual(
                    finite_price / model.marginal_price(x, y),
                    1.0,
                    delta=2e-6,
                )

    def test_singular_limit_matches_finite_parameter_sequence(self) -> None:
        for budget in (1e-6, 3e-5, 5e-3):
            limit = CryptoSwapLimit(budget)
            fade_scale = 1e-7
            finite = CryptoSwap(budget / (fade_scale * fade_scale), fade_scale)
            for v in (0.01, 0.1, 1.0, 10.0):
                limit_q, _, _ = limit.q_and_prime(v)
                finite_q, _, _ = finite.q_and_prime(v)
                self.assertAlmostEqual(limit_q, finite_q, delta=2e-6)
                delta = 1.0 - limit_q
                h = math.sqrt(limit_q) * math.sqrt(1.0 + v * v) - 1.0
                self.assertLess(abs(2.0 * budget * limit_q * h - delta**3), 3e-11)


class DuskC1GeometryTests(unittest.TestCase):
    def test_inner_is_exactly_cryptoswap_before_transition(self) -> None:
        for peak_depth, fade_scale in PROFILES:
            curve = CryptoSwap(peak_depth, fade_scale)
            dusk = DuskC1(peak_depth, fade_scale)
            start = dusk.bridge_geometry().v_start
            for fraction in (0.0, 0.1, 0.5, 0.9, 0.999):
                v = start * fraction
                curve_q, curve_prime, _ = curve.q_and_prime(v)
                dusk_q, dusk_prime, _ = dusk.q_and_prime(v)
                self.assertAlmostEqual(dusk_q, curve_q, delta=2e-13)
                self.assertAlmostEqual(dusk_prime, curve_prime, delta=2e-12)

    def test_value_and_slope_are_continuous_at_both_joins(self) -> None:
        for peak_depth, fade_scale in PROFILES:
            for bridge_fraction in (0.25, 0.5, 0.75):
                dusk = DuskC1(peak_depth, fade_scale, bridge_fraction)
                geometry = dusk.bridge_geometry()
                curve = CryptoSwap(peak_depth, fade_scale)
                inner_q, inner_prime, _ = curve.q_and_prime(geometry.v_start)
                bridge_q, bridge_prime, _ = dusk.q_and_prime(geometry.v_start)
                tail_q, tail_prime, _ = dusk.q_and_prime(geometry.v_tail)
                self.assertAlmostEqual(inner_q, bridge_q, delta=2e-12)
                self.assertAlmostEqual(inner_prime, bridge_prime, delta=2e-11)
                self.assertAlmostEqual(tail_q, 1.0 - fade_scale, delta=2e-14)
                self.assertEqual(tail_prime, 0.0)

    def test_tail_is_exact_constant_product(self) -> None:
        for peak_depth, fade_scale in PROFILES:
            dusk = DuskC1(peak_depth, fade_scale)
            v = dusk.bridge_geometry().v_tail * 2.0
            x, y = dusk.coordinates(2.0, math.asinh(v))
            product = x * y
            self.assertAlmostEqual(product, 1.0 - fade_scale, delta=3e-13)
            quote = dusk.quote_exact_in(x, y, x * 0.1)
            self.assertTrue(quote.used_exact_tail)
            self.assertAlmostEqual(
                quote.reserve_in_after * quote.reserve_out_after,
                product,
                delta=3e-13,
            )

    def test_tail_fast_path_covers_both_directions_and_exact_out(self) -> None:
        dusk = DuskC1(200.0, 0.005)
        for signed_u in (-2.0, 2.0):
            x, y = dusk.coordinates(2.0, signed_u)
            exact_in = dusk.quote_exact_in(x, y, x * 1e-3)
            self.assertTrue(exact_in.used_exact_tail)
            exact_out = dusk.quote_exact_out(x, y, exact_in.amount_out)
            self.assertTrue(exact_out.used_exact_tail)
            self.assertAlmostEqual(exact_out.amount_in / exact_in.amount_in, 1.0, delta=2e-12)

    def test_full_tail_restore_rebuilds_exactly_fade_scale_product(self) -> None:
        cpmm = Cpmm()
        for peak_depth, fade_scale in PROFILES:
            dusk = DuskC1(peak_depth, fade_scale)
            x, y = dusk.coordinates(2.0, -2.0)
            self.assertEqual(dusk.region_for_v(dusk.imbalance(x, y)), "tail")
            amount = 1.0 - x
            dusk_out = dusk.quote_exact_in(x, y, amount).amount_out
            cpmm_out = cpmm.quote_exact_in(x, y, amount).amount_out
            self.assertAlmostEqual(cpmm_out - dusk_out, fade_scale, delta=3e-11)

    def test_transition_preserves_cpmm_output_dominance(self) -> None:
        """The quote stays above CPMM even where local curvature is steeper."""

        cpmm = Cpmm()
        for peak_depth, fade_scale in PROFILES:
            dusk = DuskC1(peak_depth, fade_scale)
            geometry = dusk.bridge_geometry()
            for fraction in (0.01, 0.25, 0.5, 0.75, 0.99):
                v = geometry.v_start + geometry.length * fraction
                x, y = dusk.coordinates(2.0, math.asinh(v))
                for input_fraction in (1e-5, 0.01, 0.25, 1.0):
                    amount = x * input_fraction
                    dusk_out = dusk.quote_exact_in(x, y, amount).amount_out
                    cpmm_out = cpmm.quote_exact_in(x, y, amount).amount_out
                    self.assertGreaterEqual(dusk_out + 2e-13, cpmm_out)


class QuoteAndPriceTests(unittest.TestCase):
    def test_extreme_restoring_solver_regressions(self) -> None:
        cases = (
            (CryptoSwap(2.0, 1e-7), -5.8232972464889325, 5.189223984608654e-06),
            (DuskC1(2.0, 1e-7), -4.565888544437991, 0.0009489260233913143),
        )
        for model, signed_u, amount in cases:
            x, y = model.coordinates(2.0, signed_u)
            quote = model.quote_exact_in(x, y, amount)
            self.assertGreater(quote.amount_out, 0.0)
            self.assertAlmostEqual(
                model.invariant(quote.reserve_in_after, quote.reserve_out_after) / 2.0,
                1.0,
                delta=2e-10,
            )

    def test_exact_in_out_and_split_path_independence(self) -> None:
        rng = random.Random(0xD05C)
        for peak_depth, fade_scale in PROFILES:
            for bridge_fraction in (0.25, 0.5, 0.75):
                model = DuskC1(peak_depth, fade_scale, bridge_fraction)
                for _ in range(20):
                    amount = 10 ** rng.uniform(-5.0, -0.05)
                    whole = model.quote_exact_in(1.0, 1.0, amount)
                    inverse = model.quote_exact_out(1.0, 1.0, whole.amount_out)
                    self.assertAlmostEqual(inverse.amount_in / amount, 1.0, delta=3e-9)
                    first = model.quote_exact_in(1.0, 1.0, amount / 2.0)
                    second = model.quote_exact_in(
                        first.reserve_in_after,
                        first.reserve_out_after,
                        amount / 2.0,
                    )
                    self.assertAlmostEqual(
                        (first.amount_out + second.amount_out) / whole.amount_out,
                        1.0,
                        delta=3e-9,
                    )

    def test_price_inverse_and_target_reconstruction(self) -> None:
        for model in (Cpmm(), CryptoSwap(200.0, 0.005), DuskC1(200.0, 0.005)):
            for target in (0.1, 0.5, 0.9, 1.1, 2.0, 10.0):
                x, y, _ = normalized_reserves_at_price(model, target)
                price = model.marginal_price(x, y)
                reverse = model.marginal_price(y, x)
                self.assertAlmostEqual(price / target, 1.0, delta=5e-11)
                self.assertAlmostEqual(price * reverse, 1.0, delta=5e-11)

    def test_balanced_execution_is_between_cpmm_and_cryptoswap(self) -> None:
        cpmm = Cpmm()
        for peak_depth, fade_scale in PROFILES:
            curve = CryptoSwap(peak_depth, fade_scale)
            dusk = DuskC1(peak_depth, fade_scale)
            for size_bps in (1, 10, 100, 1_000, 2_000, 10_000):
                amount = 2.0 * size_bps / 10_000.0
                cpmm_out = cpmm.quote_exact_in(1.0, 1.0, amount).amount_out
                dusk_out = dusk.quote_exact_in(1.0, 1.0, amount).amount_out
                curve_out = curve.quote_exact_in(1.0, 1.0, amount).amount_out
                self.assertLessEqual(cpmm_out, dusk_out + 2e-12)
                self.assertLessEqual(dusk_out, curve_out + 2e-12)


if __name__ == "__main__":
    unittest.main(verbosity=2)

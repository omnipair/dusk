"""Independent arithmetic and accounting checks for the paper-rule prototype."""

import math
import unittest
from decimal import Decimal, getcontext

from model import Pool, RULES


class ModelTests(unittest.TestCase):
    def make(self, rule, fee=0.0):
        return Pool(100.0, 5000.0, 500000.0, rule, fee)

    def test_fixed_reference_conditional_invariant(self):
        for rule in RULES:
            for reference in (83.0, 100.0, 117.0):
                for action, amount in (("buy", 10000.0), ("sell", 100.0)):
                    pool = self.make(rule)
                    before = pool.invariant(reference)
                    if action == "buy":
                        pool.buy_base_exact_in(amount, reference)
                    else:
                        pool.sell_base_exact_in(amount, reference)
                    self.assertAlmostEqual(before, pool.invariant(reference), delta=abs(before) * 1e-11)

    def test_exact_out_matches_exact_in(self):
        for rule in RULES:
            pool = self.make(rule, 0.0003)
            gross = pool.buy_base_exact_out(100.0, 108.0)
            twin = self.make(rule, 0.0003)
            out = twin.buy_base_exact_in(gross, 108.0)
            self.assertAlmostEqual(out, 100.0, delta=1e-8)
            self.assertAlmostEqual(pool.quote, twin.quote, delta=1e-7)

    def test_no_oracle_change_of_reserves(self):
        for rule in RULES:
            pool = self.make(rule)
            base, quote = pool.base, pool.quote
            p_before = pool.marginal_price(100.0)
            p_after = pool.marginal_price(110.0)
            self.assertAlmostEqual(p_after / p_before, 1.1 ** rule.lam, places=12)
            self.assertEqual((pool.base, pool.quote), (base, quote))

    def test_all_rules_same_price_and_matched_elasticity_at_launch(self):
        for rule in RULES:
            pool = self.make(rule)
            self.assertAlmostEqual(pool.marginal_price(100.0), 100.0)
            pool.quote *= 1.01
            self.assertAlmostEqual(pool.marginal_price(100.0), 100.0 * 1.01 ** rule.beta)

    def test_zero_fee_reverse_trade(self):
        for rule in RULES:
            pool = self.make(rule)
            base = pool.buy_base_exact_in(10000.0, 100.0)
            quote = pool.sell_base_exact_in(base, 100.0)
            self.assertAlmostEqual(quote, 10000.0, delta=1e-6)
            self.assertAlmostEqual(pool.base, 5000.0, delta=1e-8)
            self.assertAlmostEqual(pool.quote, 500000.0, delta=1e-6)

    def test_fee_increases_conditional_invariant(self):
        for rule in RULES:
            pool = self.make(rule, 0.0003)
            before = pool.invariant(108.0)
            pool.buy_base_exact_in(10000.0, 108.0)
            self.assertGreater(pool.invariant(108.0), before)

    def test_decimal_reference_for_pt(self):
        getcontext().prec = 60
        d = Decimal
        x, y, a, gross = d(500000), d(500000), d("1.1"), d(10000)
        exact_x_after = x * (y / (y + gross)) ** (1 / a)
        exact_base_out = (x - exact_x_after) / d(100)
        pool = self.make(RULES[0])
        self.assertAlmostEqual(pool.buy_base_exact_in(10000, 110), float(exact_base_out), delta=1e-9)

    def test_decimal_reference_for_half_blend(self):
        getcontext().prec = 60
        d = Decimal
        x, y, a, gross = d(500000), d(500000), d("1.1").sqrt(), d(10000)
        k = y.sqrt() + a * x.sqrt()
        exact_x_after = ((k - (y + gross).sqrt()) / a) ** 2
        exact_base_out = (x - exact_x_after) / d(100)
        pool = self.make(next(r for r in RULES if r.name == "GA_50_CPMM"))
        self.assertAlmostEqual(pool.buy_base_exact_in(10000, 110), float(exact_base_out), delta=1e-9)

    def test_base_neutral_cycle_conserves_lp_and_attacker_cash(self):
        for rule in RULES:
            pool = self.make(rule, 0.0003)
            before = pool.nav(100.0)
            spent = pool.buy_base_exact_out(100.0, 96.0)
            received = pool.sell_base_exact_in(100.0, 104.0)
            self.assertTrue(math.isfinite(received - spent))
            self.assertAlmostEqual(pool.base, 5000.0, delta=1e-8)
            self.assertAlmostEqual(before - pool.nav(100.0), received - spent, delta=1e-7)


if __name__ == "__main__":
    unittest.main()

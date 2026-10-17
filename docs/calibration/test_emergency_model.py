"""Economic invariants for the draft policy, not transaction-security tests."""

import math
import unittest

from emergency_model import Clock, Insurance, Pool, depth_amount, simulate, waterfall


class EmergencyPolicyTests(unittest.TestCase):
    def test_clock_requires_elapsed_time_and_a_real_health_recovery(self):
        clock = Clock()
        clock.observe(100, True, 30)
        for _ in range(20):
            clock.observe(100, True, 30)
        self.assertEqual(clock.age, 0)
        clock.observe(120, True, 30)
        # A tiny repayment that leaves reference health below MM changes none
        # of the clock inputs; it cannot reset confirmed distress.
        clock.observe(120, True, 30)
        self.assertEqual(clock.age, 20)
        clock.observe(125, False, 30)
        self.assertEqual(clock.age, 0)
        self.assertIsNone(clock.last)
        clock.observe(130, True, 30)
        self.assertEqual(clock.age, 0)

    def test_old_observation_does_not_backdate_confirmation(self):
        clock = Clock()
        clock.observe(100, True, 30)
        clock.observe(10_000, True, 30)
        self.assertEqual(clock.age, 0)
        clock.observe(10_010, True, 30)
        self.assertEqual(clock.age, 10)

    def test_insurance_covers_principal_without_manufacturing_interest(self):
        result = waterfall(1_000, 100, 800, 0, Insurance(2_000))
        self.assertEqual(result["principal_paid"], 800)
        self.assertEqual(result["insurance"], 200)
        self.assertEqual(result["loss"], 0)
        self.assertEqual(result["interest_paid"], 0)
        self.assertEqual(result["canceled_interest"], 100)

    def test_reward_and_loss_conserve_cash_with_empty_insurance(self):
        result = waterfall(5_000, 0, 4_496, 0.03, Insurance(0))
        self.assertAlmostEqual(result["reward"], 134.88)
        self.assertAlmostEqual(result["loss"], 638.88)
        self.assertAlmostEqual(result["principal_paid"] + result["loss"], 5_000)
        self.assertEqual(result["contribution"], 0)

    def test_draw_caps_survive_many_liquidations_and_new_credits(self):
        fund = Insurance(1_000)
        self.assertEqual(fund.cover(1_000), 200)
        self.assertEqual(fund.cover(1_000), 160)
        for _ in range(50):
            fund.cover(1_000)
        self.assertEqual(fund.drawn, 500)
        fund.credited += 200
        self.assertEqual(fund.cover(1_000), 100)
        self.assertEqual(fund.drawn, 600)

    def test_solvent_funding_cannot_create_a_new_shortfall(self):
        result = waterfall(1_000, 100, 1_150, 0.01, Insurance(0))
        self.assertEqual(result["loss"], 0)
        self.assertEqual(result["interest_paid"], 100)
        self.assertEqual(result["contribution"], 22)
        self.assertAlmostEqual(result["owner"], 16.5)

    def test_depth_bands_are_scale_invariant_and_continuous(self):
        for scale in (0.0004, 1, 2_500):
            base = depth_amount(15_000, 50_000, True) / 15_000
            actual = depth_amount(15_000 * scale, 50_000 * scale, True) / (15_000 * scale)
            self.assertAlmostEqual(actual, base)
        at_boundary = depth_amount(5_000, 50_000, True)
        after_boundary = depth_amount(5_000.01, 50_000, True)
        self.assertAlmostEqual(after_boundary - at_boundary, 0.002)

    def test_stack_split_neutrality_only_at_fixed_depth(self):
        total = depth_amount(30_000, 50_000, True)
        pieces = sum(depth_amount(end, 50_000, True) - depth_amount(end - 300, 50_000, True)
                     for end in range(300, 30_001, 300))
        self.assertAlmostEqual(pieces, total)
        independent = 100 * depth_amount(300, 50_000, True)
        self.assertLess(independent, total)

    def test_swap_and_writeoff_both_reduce_the_price(self):
        pool = Pool(50_000, 50_000, fee=0)
        before = pool.price
        output = pool.sell(8_000)
        after_sale = pool.price
        loss = 7_600 - output
        pool.y -= loss
        self.assertLess(pool.price, after_sale)
        self.assertLess(after_sale, before)
        self.assertAlmostEqual(pool.y, 42_400)

    def test_simulation_reports_admission_and_conserved_insurance(self):
        result = simulate()
        self.assertGreater(result["opened"], 0)
        self.assertGreater(result["original_debt"], 0)
        self.assertLessEqual(result["insurance_drawn"], (1_000 + result["insurance_funding"]) * 0.5 + 1e-6)
        self.assertGreaterEqual(result["remaining_debt"], 0)
        self.assertGreaterEqual(result["principal_loss"], 0)
        self.assertTrue(math.isfinite(result["final_pool_price"]))

    def test_unprofitable_cleanup_is_reported_as_unresolved(self):
        result = simulate(tvl=100, fixed_cost=5, path="gap_flat")
        self.assertGreater(result["remaining_debt"], 0)
        self.assertGreater(result["unresolved_eligible"], 0)
        self.assertEqual(result["terminal_full"], 0)


if __name__ == "__main__":
    unittest.main()

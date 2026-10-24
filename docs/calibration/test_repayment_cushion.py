import unittest

from flash_purchase_model import Terms, simulate
from repayment_cushion import solvent_amm_sale


class RepaymentCushionTests(unittest.TestCase):
    def sale(self, **changes):
        args = dict(principal=900, interest=50, output=960, equity=0.06,
                    maintenance=0.07, cushion_limit=0.02,
                    reward_max=0.01, insurance_rate=0.002)
        return solvent_amm_sale(**(args | changes))

    def test_thin_execution_never_makes_a_healthy_ema_position_eligible(self):
        for output in (949, 955, 960, 980):
            self.assertIsNone(self.sale(output=output, equity=0.08))

    def test_interest_and_all_allocations_are_funded_without_insurance(self):
        result = self.sale()
        self.assertEqual(result["principal_paid"], 900)
        self.assertEqual(result["interest_paid"], 50)
        self.assertEqual(result["insurance"], 0)
        self.assertEqual(result["loss"], 0)
        self.assertEqual(result["canceled_interest"], 0)
        self.assertGreater(result["owner"], 0)
        self.assertAlmostEqual(sum(result[k] for k in (
            "principal_paid", "interest_paid", "reward", "contribution", "owner")), 960)

    def test_repayment_must_cover_interest_even_if_principal_is_safe(self):
        self.assertIsNone(self.sale(output=949))

    def test_actual_execution_shortfall_cannot_use_an_earlier_good_quote(self):
        self.assertIsNotNone(self.sale(output=960))
        # Represents an additional execution cost or lower final spendable
        # credit. It is not a native Token-2022 or atomic rollback test.
        self.assertIsNone(self.sale(output=951))

    def test_insurance_funding_is_capped_by_post_reward_surplus(self):
        result = self.sale(output=952)
        self.assertGreater(result["contribution"], 0)
        self.assertLess(result["contribution"], 950 * 0.002)
        self.assertEqual(result["owner"], 0)
        self.assertEqual(result["loss"], 0)

    def test_window_is_bounded_above_and_below(self):
        self.assertIsNone(self.sale(output=990))
        self.assertIsNotNone(self.sale(output=960))
        self.assertIsNone(self.sale(output=949))

    def test_worse_quote_can_enable_full_close_at_unchanged_unhealthy_ema(self):
        self.assertIsNone(self.sale(output=990))
        self.assertIsNotNone(self.sale(output=960))
        # Counterfactual quote sensitivity, not an executed attack or profit proof.
        # Debt protection does not preserve the owner's earlier sale surplus.

    def test_zero_cushion_and_no_reward_cannot_fund_insurance(self):
        result = self.sale(output=950, equity=0.07, cushion_limit=0)
        self.assertEqual(result["owner"], 0)
        self.assertEqual(result["contribution"], 0)
        self.assertEqual(result["reward"], 0)

    def test_cushion_can_help_slow_paths_but_gap_still_needs_loss_resolution(self):
        terms = Terms(rates=(0.07, 0.12, 0.17), bands=(0.05, 0.15),
                      maximum_discount=0.05)
        baseline = simulate(terms=terms, pieces=10, path="slow")
        earlier = simulate(terms=terms, pieces=10, path="slow", cushion_limit=0.02)
        self.assertEqual(earlier["original_debt"], baseline["original_debt"])
        self.assertGreater(earlier["solvent_full"], 0)
        self.assertLess(earlier["insurance_drawn"], baseline["insurance_drawn"])
        self.assertEqual(earlier["remaining_debt"], 0)
        gap = simulate(terms=terms, pieces=10, path="gap_flat", cushion_limit=0.02)
        self.assertEqual(gap["solvent_full"], 0)
        self.assertGreater(gap["emergency_full"], 0)
        self.assertGreater(gap["loss"], 0)
        self.assertEqual(gap["remaining_debt"], 0)

    def test_partial_vs_full_first_is_explicit_sensitivity(self):
        terms = Terms(rates=(0.07, 0.12, 0.17), bands=(0.05, 0.15),
                      maximum_discount=0.05)
        for priority in (False, True):
            result = simulate(terms=terms, pieces=10, path="slow",
                              cushion_limit=0.02, solvent_first=priority)
            self.assertGreater(result["solvent_full"], 0)
            self.assertEqual(result["remaining_debt"], 0)
            self.assertEqual(result["solvent_first"], priority)


if __name__ == "__main__":
    unittest.main()

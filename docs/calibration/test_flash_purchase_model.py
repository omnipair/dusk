import unittest
from flash_purchase_model import Terms, Insurance, Pool, partial, principal_first, simulate


class FlashPurchaseTests(unittest.TestCase):
    def test_principal_first_cancels_interest_without_insuring_it(self):
        insurance = Insurance(1_000)
        result = principal_first(900, 50, 850, insurance, 0.002)
        self.assertEqual(result["principal_paid"], 850)
        self.assertEqual(result["insurance"], 50)
        self.assertEqual(result["interest_paid"], 0)
        self.assertEqual(result["canceled_interest"], 50)
        self.assertEqual(result["contribution"], 0)
        self.assertEqual(result["loss"], 0)

    def test_insurance_fee_cannot_create_a_full_sale_shortfall(self):
        result = principal_first(900, 50, 950.1, Insurance(1_000), 0.02)
        self.assertAlmostEqual(result["contribution"], 0.1)
        self.assertEqual(result["loss"], 0)
        self.assertEqual(result["owner"], 0)

    def test_partial_improves_health_and_buyer_keeps_only_slice_upside(self):
        fill = partial(1_000, 935, 50_000, 1, Pool(1_000_000, 1_000_000), None, Terms(), 0.01)
        self.assertIsNotNone(fill)
        self.assertLess(fill["sold"], 1_000)
        self.assertGreater(fill["health_after"], fill["health_before"])
        self.assertGreater(fill["profit"], 0)
        self.assertAlmostEqual(fill["repay"] + fill["contribution"], fill["payment"])

    def test_insolvent_position_cannot_be_repaired_by_underpaying_a_partial(self):
        self.assertIsNone(partial(1_000, 1_050, 50_000, 1, Pool(1e8, 1e8), None, Terms(), 0))

    def test_no_timer_in_equity_permission_or_health_discount(self):
        terms = Terms()
        self.assertGreater(terms.discount(0, 0.07), terms.discount(0.07, 0.07))
        self.assertEqual(terms.discount(-1, 0.07), terms.maximum_discount)
        result = simulate(path="fast")
        self.assertGreater(result["emergency_full"], 0)
        self.assertEqual(result["remaining_debt"], 0)


if __name__ == "__main__":
    unittest.main()

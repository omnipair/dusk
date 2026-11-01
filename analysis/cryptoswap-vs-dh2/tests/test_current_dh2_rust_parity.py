import csv
import math
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from models import CurrentDh2


NAD = 1_000_000_000
FIXTURE = ROOT / "data" / "current_dh2_rust_vectors.csv"


class CurrentDh2RustParityTests(unittest.TestCase):
    @staticmethod
    def fixture_rows():
        with FIXTURE.open(newline="") as source:
            return list(csv.DictReader(source))

    def test_continuous_invariant_lies_inside_rust_fixed_point_certificate(self):
        for row in self.fixture_rows():
            with self.subTest(vector=row["name"]):
                center = int(row["center_nad"]) / NAD
                base_common = int(row["base_nad"]) / NAD * center
                quote_common = int(row["quote_nad"]) / NAD
                model = CurrentDh2(
                    peak_depth=int(row["peak_depth_nad"]) / NAD,
                    fade_scale=int(row["fade_scale_nad"]) / NAD,
                )
                continuous_d = model.invariant(base_common, quote_common)
                rust_low = int(row["invariant_d_low"]) / NAD
                rust_high = int(row["invariant_d_high"]) / NAD
                rounding_tolerance = max(1e-9, rust_high * 2e-15)
                self.assertGreaterEqual(continuous_d, rust_low - rounding_tolerance)
                self.assertLessEqual(continuous_d, rust_high + rounding_tolerance)

    def test_marginal_prices_match_rust_fixed_point_vectors(self):
        for row in self.fixture_rows():
            with self.subTest(vector=row["name"]):
                center = int(row["center_nad"]) / NAD
                base_common = int(row["base_nad"]) / NAD * center
                quote_common = int(row["quote_nad"]) / NAD
                model = CurrentDh2(
                    peak_depth=int(row["peak_depth_nad"]) / NAD,
                    fade_scale=int(row["fade_scale_nad"]) / NAD,
                )
                analytical = center * model.marginal_price_raw(base_common, quote_common)
                rust = int(row["marginal_price_nad"]) / NAD
                error_ppm = abs(analytical - rust) / rust * 1_000_000
                self.assertLessEqual(error_ppm, 1.0)

    def test_rust_quotes_are_the_analytical_quote_minus_solver_haircut(self):
        for row in self.fixture_rows():
            with self.subTest(vector=row["name"]):
                center = int(row["center_nad"]) / NAD
                base_common = int(row["base_nad"]) / NAD * center
                quote_common = int(row["quote_nad"]) / NAD
                amount_in = int(row["amount_in_nad"]) / NAD
                model = CurrentDh2(
                    peak_depth=int(row["peak_depth_nad"]) / NAD,
                    fade_scale=int(row["fade_scale_nad"]) / NAD,
                )
                if row["direction"] == "base_to_quote":
                    analytical = model.quote(
                        base_common,
                        quote_common,
                        amount_in * center,
                        "x_to_y",
                        apply_fee=False,
                    ).amount_out
                else:
                    analytical = (
                        model.quote(
                            base_common,
                            quote_common,
                            amount_in,
                            "y_to_x",
                            apply_fee=False,
                        ).amount_out
                        / center
                    )
                rust = int(row["amount_out_nad"]) / NAD
                haircut_ppm = (analytical - rust) / analytical * 1_000_000
                self.assertLessEqual(rust, analytical)
                self.assertGreaterEqual(haircut_ppm, 9.9)
                self.assertLessEqual(haircut_ppm, 10.1)

    def test_balanced_equivalent_q_normalizes_center_with_square_root(self):
        row = next(row for row in self.fixture_rows() if row["name"] == "balanced_nonunit")
        center = int(row["center_nad"]) / NAD
        invariant_d = int(row["invariant_d_low"]) / NAD
        expected_q = int(row["balanced_equivalent_q_nad"]) / NAD

        q = invariant_d / (2 * math.sqrt(center))
        self.assertAlmostEqual(q, expected_q, delta=1 / NAD)
        self.assertNotAlmostEqual(q, invariant_d / 2, delta=1.0)


if __name__ == "__main__":
    unittest.main()

import math
import hashlib
import json
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from native_flow import generate_native_order_tape, orders_by_minute
from scenarios import native_historical_windows


class NativeFlowTests(unittest.TestCase):
    def test_common_tape_is_deterministic_and_reconciles_volume(self):
        prices = [150.0 * math.exp(0.001 * index) for index in range(720)]
        kwargs = dict(
            pool_value=2_000_000.0,
            daily_volume_pool_ratio=0.25,
            informed_share=0.2,
            informed_accuracy=0.8,
            signal_horizon_minutes=5,
            mean_arrivals_per_minute=0.4,
            seed=41,
        )
        first = generate_native_order_tape(prices, **kwargs)
        second = generate_native_order_tape(prices, **kwargs)
        self.assertEqual(first, second)
        target = kwargs["pool_value"] * kwargs["daily_volume_pool_ratio"] / 2
        self.assertAlmostEqual(
            sum(order.external_notional for order in first),
            target,
            delta=target * 1e-12,
        )
        self.assertLessEqual(
            max(order.external_notional for order in first),
            kwargs["pool_value"] * 0.2,
        )
        self.assertEqual(sum(map(len, orders_by_minute(first, len(prices)))), len(first))

    def test_native_history_has_fifteen_offline_burned_in_windows(self):
        windows = native_historical_windows()
        self.assertEqual(len(windows), 15)
        self.assertEqual(
            {window.stratum for window in windows.values()},
            {"calm", "typical", "stress"},
        )
        for window in windows.values():
            self.assertEqual(window.burn_in_minutes, 1_440)
            self.assertEqual(len(window.prices), 2_160)
            self.assertEqual(len(window.scoring_prices), 720)

        snapshot = ROOT / "data" / "solusdt_native_windows.csv"
        metadata = json.loads(
            (ROOT / "data" / "native_history_metadata.json").read_text()
        )
        self.assertEqual(
            hashlib.sha256(snapshot.read_bytes()).hexdigest(),
            metadata["snapshot_sha256"],
        )

    def test_flat_prices_have_no_informed_directional_signal(self):
        tape = generate_native_order_tape(
            [150.0] * 720,
            pool_value=2_000_000.0,
            daily_volume_pool_ratio=0.25,
            informed_share=1.0,
            informed_accuracy=1.0,
            signal_horizon_minutes=5,
            mean_arrivals_per_minute=0.4,
            seed=41,
        )
        directions = {order.direction for order in tape}
        self.assertEqual(directions, {"x_to_y", "y_to_x"})
        self.assertFalse(any(order.informed for order in tape))


if __name__ == "__main__":
    unittest.main()

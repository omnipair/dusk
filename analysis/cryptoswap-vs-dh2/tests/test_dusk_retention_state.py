import copy
from dataclasses import replace
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import economics as engine
from models import Cpmm, CurrentDh2


class DuskRetentionStateParityTests(unittest.TestCase):
    def setUp(self):
        self.model = CurrentDh2(peak_depth=200.0, fade_scale=0.1)
        self.base_config = engine.default_controlled_config()
        self.no_recenter = replace(self.base_config, recenter_enabled=False)

    def _state_with_stale_retained_endpoint(self):
        state = engine.initialize_pool(
            self.model,
            150.0,
            config=self.no_recenter,
        )
        # The first outward quote creates a non-zero forward target.  Because
        # the initial target is zero, that quote distributes its surcharge.
        first = engine.execute_trade(
            self.model,
            state,
            50_000.0,
            "y_to_x",
            150.0,
            150,
            self.no_recenter,
        )
        self.assertEqual(first.retained_surcharge_value, 0.0)
        self.assertGreater(state.retention_required_q, 0.0)
        self.assertTrue(state.retain_dynamic_surcharge)
        certified_target = (
            state.retention_required_q,
            state.retention_stop_q,
            state.retention_target_saturated,
        )

        # The second quote retains its dynamic surcharge.  Production updates
        # Q and the cap, but intentionally preserves the certified required and
        # stop amounts until a later decision point.
        second = engine.execute_trade(
            self.model,
            state,
            50_000.0,
            "y_to_x",
            150.0,
            300,
            self.no_recenter,
        )
        self.assertGreater(second.retained_surcharge_value, 0.0)
        return state, certified_target

    def test_retained_endpoint_marks_target_stale_and_preserves_target(self):
        state, certified_target = self._state_with_stale_retained_endpoint()

        self.assertTrue(state.retention_target_stale)
        self.assertTrue(state.retain_dynamic_surcharge)
        self.assertEqual(
            (
                state.retention_required_q,
                state.retention_stop_q,
                state.retention_target_saturated,
            ),
            certified_target,
        )
        self.assertAlmostEqual(
            state.retention_hard_cap_q,
            engine.PROTECTED_CAP * state.current_q,
            delta=1e-12 * state.current_q,
        )

    def test_funded_stale_probe_distributes_one_quote_then_rearms(self):
        state, certified_target = self._state_with_stale_retained_endpoint()
        state.protected_floor_q = (
            state.current_q - state.retention_stop_q - 1e-6
        )

        result = engine.execute_trade(
            self.model,
            state,
            50_000.0,
            "y_to_x",
            150.0,
            450,
            self.no_recenter,
        )

        self.assertGreater(result.dynamic_fee_value, 0.0)
        self.assertEqual(result.retained_surcharge_value, 0.0)
        self.assertEqual(
            result.assessment.distributed_surcharge_debit,
            result.assessment.dynamic_surcharge_debit,
        )
        self.assertTrue(state.retention_target_stale)
        self.assertTrue(state.retain_dynamic_surcharge)
        self.assertEqual(
            (
                state.retention_required_q,
                state.retention_stop_q,
                state.retention_target_saturated,
            ),
            certified_target,
        )

    def test_failed_trade_rolls_back_ema_and_stale_probe_release(self):
        state, _ = self._state_with_stale_retained_endpoint()
        state.protected_floor_q = (
            state.current_q - state.retention_stop_q - 1e-6
        )
        state.last_trade_price = state.center * 1.05
        before = copy.deepcopy(state)
        invalid = replace(
            self.no_recenter,
            solver_haircut_ppm=1_000_000.0,
        )

        with self.assertRaisesRegex(ValueError, "solver haircut"):
            engine.execute_trade(
                self.model,
                state,
                50_000.0,
                "y_to_x",
                150.0,
                600,
                invalid,
            )

        self.assertEqual(state, before)

    def test_stale_recenter_gate_uses_required_not_hysteresis_stop(self):
        state, certified_target = self._state_with_stale_retained_endpoint()
        state.ema = state.center * 1.10
        state.protected_floor_q = state.current_q - (
            state.retention_required_q * 0.99
        )
        counters_before = (
            state.requested_recenter_count,
            state.blocked_recenter_count,
        )
        center_before = state.center

        moved = engine.maybe_recenter(
            self.model,
            state,
            replace(
                self.base_config,
                min_adjustment_interval_minutes=0,
                adjustment_threshold=1e-6,
            ),
            600,
        )

        self.assertFalse(moved)
        self.assertEqual(state.center, center_before)
        self.assertEqual(
            (
                state.requested_recenter_count,
                state.blocked_recenter_count,
            ),
            counters_before,
        )
        self.assertTrue(state.retention_target_stale)
        self.assertEqual(
            (
                state.retention_required_q,
                state.retention_stop_q,
                state.retention_target_saturated,
            ),
            certified_target,
        )

        # Direct maintenance is allowed to value a candidate once the lower
        # required gate is funded, even though quote-fee release still waits
        # for the higher hysteresis stop.
        probe, _ = self._state_with_stale_retained_endpoint()
        probe.ema = probe.center * 1.10
        between = (
            probe.retention_required_q + probe.retention_stop_q
        ) / 2.0
        self.assertLess(between, probe.retention_stop_q)
        probe.protected_floor_q = probe.current_q - between
        requested_before = probe.requested_recenter_count

        engine.maybe_recenter(
            self.model,
            probe,
            replace(
                self.base_config,
                min_adjustment_interval_minutes=0,
                adjustment_threshold=1e-6,
            ),
            600,
        )

        self.assertEqual(
            probe.requested_recenter_count,
            requested_before + 1,
        )

    def test_successful_recenter_refreshes_from_realized_impairment(self):
        zero_dynamic = replace(
            self.base_config.fee,
            max_fee_rate_nad=self.base_config.fee.base_fee_rate_nad,
            divergence_coefficient_nad=0,
            volatility_coefficient_nad=0,
        )
        trade_config = replace(
            self.base_config,
            fee=zero_dynamic,
            recenter_enabled=False,
        )
        state = engine.initialize_pool(
            self.model,
            150.0,
            config=trade_config,
        )
        engine.execute_trade(
            self.model,
            state,
            400_000.0,
            "y_to_x",
            150.0,
            150,
            trade_config,
        )
        state.ema = state.last_trade_price
        recenter_config = replace(
            trade_config,
            recenter_enabled=True,
            min_adjustment_interval_minutes=0,
            adjustment_threshold=1e-6,
        )
        full_center, _, full_covered = engine._candidate_recenter(
            self.model,
            state,
            state.ema,
            recenter_config.adjustment_step,
        )
        state.protected_floor_q = state.current_q - full_covered - 1e-8
        old_q = state.current_q

        self.assertTrue(
            engine.maybe_recenter(
                self.model,
                state,
                recenter_config,
                150,
            )
        )

        self.assertAlmostEqual(state.center, full_center, delta=1e-12)
        realized = max(old_q - state.current_q, 0.0)
        hard_cap = engine.PROTECTED_CAP * state.current_q
        expected_required = min(
            engine.PROTECTED_COVERAGE * realized
            + engine.PROTECTED_GUARD * state.current_q,
            hard_cap,
        )
        expected_stop = min(
            expected_required
            + max(
                engine.PROTECTED_HYSTERESIS * expected_required,
                engine.PROTECTED_GUARD * state.current_q,
            ),
            hard_cap,
        )
        self.assertGreater(realized, 0.0)
        self.assertFalse(state.retention_target_stale)
        self.assertAlmostEqual(
            state.retention_required_q,
            expected_required,
            delta=max(expected_required * 1e-12, 1e-12),
        )
        self.assertAlmostEqual(
            state.retention_stop_q,
            expected_stop,
            delta=max(expected_stop * 1e-12, 1e-12),
        )

    def test_zero_impairment_target_has_no_guard_only_requirement(self):
        state = engine.initialize_pool(
            self.model,
            150.0,
            config=self.base_config,
        )
        engine.refresh_retention_target_from_impairment(state, 0.0)

        self.assertEqual(state.retention_required_q, 0.0)
        self.assertEqual(state.retention_stop_q, 0.0)
        self.assertFalse(state.retention_target_saturated)
        self.assertFalse(state.retention_target_stale)
        self.assertFalse(state.retain_dynamic_surcharge)

    def test_cpmm_anchor_moves_without_spending_protected_profit(self):
        model = Cpmm()
        config = replace(
            self.base_config,
            min_adjustment_interval_minutes=0,
            adjustment_threshold=1e-6,
        )
        state = engine.initialize_pool(model, 150.0, config=config)
        q_before = state.current_q
        floor_before = state.protected_floor_q
        state.ema = 180.0

        self.assertTrue(engine.maybe_recenter(model, state, config, 1))

        self.assertGreater(state.center, 150.0)
        self.assertEqual(state.current_q, q_before)
        self.assertEqual(state.protected_floor_q, floor_before)
        self.assertEqual(state.recenter_impairment_q, 0.0)
        self.assertEqual(state.retention_required_q, 0.0)
        self.assertEqual(state.retention_stop_q, 0.0)
        self.assertFalse(state.retention_target_stale)
        self.assertFalse(state.retain_dynamic_surcharge)


if __name__ == "__main__":
    unittest.main()

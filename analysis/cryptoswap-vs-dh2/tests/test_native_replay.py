import copy
import math
import inspect
from dataclasses import replace
import sys
import unittest
from unittest.mock import patch
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import curve_native as curve_engine
import economics as dusk_engine
from models import CryptoSwap, CurrentDh2
from native_flow import NativeOrder, generate_native_order_tape
from native_replay import (
    RoutingConfig,
    STATE_DEVIATION_THRESHOLD_BPS,
    _marginal_tracking_error_bps,
    _preview_curve_order,
    _preview_dusk_order,
    simulate_curve_native_tape,
    simulate_dusk_native_tape,
)
from curve_native import _roll_ema_before_trade as curve_roll_ema
from curve_native import maybe_recenter as curve_maybe_recenter
from economics import _update_ema_before_trade as dusk_update_ema
from economics import default_controlled_config
from economics import execute_trade, initialize_pool
from economics import maybe_recenter as dusk_maybe_recenter


class NativeReplayTests(unittest.TestCase):
    def setUp(self):
        self.prices = [150.0 * math.exp(0.0005 * index) for index in range(90)]
        self.tape = generate_native_order_tape(
            self.prices,
            pool_value=2_000_000.0,
            daily_volume_pool_ratio=0.25,
            informed_share=0.2,
            informed_accuracy=0.8,
            signal_horizon_minutes=5,
            mean_arrivals_per_minute=0.4,
            seed=7,
        )

    def test_mature_native_ledgers_reconcile_on_same_intents(self):
        curve = simulate_curve_native_tape(
            CryptoSwap(),
            "trend",
            self.prices,
            self.tape,
            score_start_index=30,
            arb_latency_minutes=5,
        )
        dusk = simulate_dusk_native_tape(
            CurrentDh2(),
            "trend",
            self.prices,
            self.tape,
            score_start_index=30,
            arb_latency_minutes=5,
        )
        for result in (curve, dusk):
            self.assertLess(abs(result["accounting_identity_error"]), 1e-5)
            self.assertEqual(result["controller_external_price_reads"], 0)
            self.assertEqual(
                result["attempted_user_orders"],
                result["executed_user_orders"]
                + result["rejected_user_orders"]
                + result["routed_out_orders"],
            )
            self.assertGreaterEqual(result["gross_fee_value"], 0.0)
            self.assertTrue(math.isfinite(result["selected_execution_cost_bps"]))
            self.assertEqual(result["arbitrage_capped_event_count"], 0)
            self.assertEqual(result["arbitrage_numerical_failure_count"], 0)
        self.assertGreaterEqual(curve["protocol_fee_liability_value"], 0.0)
        self.assertEqual(dusk["non_lp_fee_liability_value"], 0.0)

    def test_committing_dusk_preview_is_bit_identical_to_reexecution(self):
        model = CurrentDh2()
        config = default_controlled_config()
        for direction in ("x_to_y", "y_to_x"):
            with self.subTest(direction=direction):
                state = initialize_pool(model, 150.0, config=config)
                order = NativeOrder(
                    minute=1,
                    external_notional=25_000.0,
                    direction=direction,
                    informed=False,
                )
                candidate, preview_result, output_value = _preview_dusk_order(
                    model,
                    state,
                    order,
                    150.0,
                    150,
                    config,
                )
                replayed = copy.deepcopy(state)
                executed_result = execute_trade(
                    model,
                    replayed,
                    order.external_notional,
                    order.direction,
                    150.0,
                    150,
                    config,
                )
                self.assertEqual(candidate, replayed)
                self.assertEqual(preview_result, executed_result)
                expected_output_value = (
                    preview_result.output_amount
                    if direction == "x_to_y"
                    else preview_result.output_amount * 150.0
                )
                self.assertEqual(output_value, expected_output_value)

    def test_committing_curve_preview_is_bit_identical_to_reexecution(self):
        model = CryptoSwap()
        config = curve_engine.CurveNativeConfig()
        for direction in ("x_to_y", "y_to_x"):
            with self.subTest(direction=direction):
                state = curve_engine.initialize_curve_native_pool(
                    model,
                    150.0,
                    config=config,
                )
                order = NativeOrder(
                    minute=1,
                    external_notional=25_000.0,
                    direction=direction,
                    informed=False,
                )
                candidate, preview_result, output_value = _preview_curve_order(
                    model,
                    state,
                    order,
                    150.0,
                    150,
                    config,
                )
                replayed = copy.deepcopy(state)
                executed_result = curve_engine.execute_curve_native_order(
                    model,
                    replayed,
                    order.external_notional,
                    order.direction,
                    150.0,
                    150,
                    config,
                )
                self.assertEqual(candidate, replayed)
                self.assertEqual(preview_result, executed_result)
                expected_output_value = (
                    preview_result.trade.output_amount
                    if direction == "x_to_y"
                    else preview_result.trade.output_amount * 150.0
                )
                self.assertEqual(output_value, expected_output_value)

    def test_dusk_revenue_split_is_deducted_from_lp_net_return(self):
        result = simulate_dusk_native_tape(
            CurrentDh2(),
            "revenue_split",
            self.prices,
            self.tape,
            score_start_index=30,
            arb_latency_minutes=5,
            config=replace(default_controlled_config(), manager_fee_bps=500),
        )
        self.assertGreater(result["non_lp_fee_liability_value"], 0.0)
        self.assertLess(
            result["net_lp_pnl_after_protocol_fee"],
            result["net_lp_pnl_vs_hold"],
        )

    def test_elastic_router_can_decline_a_native_quote(self):
        routing = RoutingConfig(
            mode="elastic",
            outside_depth_multiple=5.0,
            outside_fee_bps=0.0,
        )
        for runner, model in (
            (simulate_curve_native_tape, CryptoSwap()),
            (simulate_dusk_native_tape, CurrentDh2()),
        ):
            result = runner(
                model,
                "trend",
                self.prices,
                self.tape,
                score_start_index=30,
                arb_latency_minutes=5,
                routing=routing,
            )
            self.assertGreater(result["routed_out_orders"], 0)
            self.assertLess(result["captured_user_volume_fraction"], 1.0)
            self.assertTrue(math.isfinite(result["selected_execution_cost_bps"]))
            self.assertEqual(result["rejected_user_orders"], 0)

    def test_native_dusk_charges_the_production_solver_haircut(self):
        result = simulate_dusk_native_tape(
            CurrentDh2(),
            "haircut",
            self.prices,
            self.tape,
            score_start_index=30,
            arb_latency_minutes=5,
            config=replace(default_controlled_config(), solver_haircut_ppm=10.0),
        )
        self.assertGreater(result["solver_haircut_value"], 0.0)

    def test_controller_boundaries_do_not_accept_external_prices(self):
        for controller in (
            curve_roll_ema,
            curve_maybe_recenter,
            dusk_update_ema,
            dusk_maybe_recenter,
        ):
            self.assertNotIn("external_price", inspect.signature(controller).parameters)

    def test_five_minute_burn_in_boundary_uses_step_index_explicitly(self):
        prices = [150.0] * 432
        tape = generate_native_order_tape(
            prices,
            pool_value=2_000_000.0,
            daily_volume_pool_ratio=0.05,
            informed_share=0.0,
            informed_accuracy=0.5,
            signal_horizon_minutes=5,
            mean_arrivals_per_minute=0.1,
            seed=7,
            step_minutes=5,
        )
        result = simulate_dusk_native_tape(
            CurrentDh2(),
            "burn_in_units",
            prices,
            tape,
            score_start_index=288,
            arb_latency_minutes=5,
            step_minutes=5,
        )
        self.assertEqual(result["score_start_minute"], 1_440)
        self.assertEqual(result["score_minutes"], 720)
        self.assertEqual(
            result["attempted_user_orders"],
            sum(order.minute >= 288 for order in tape),
        )

    def test_burn_in_cap_is_reported_by_all_period_counter(self):
        for engine, cap_name, runner, model, jump_price in (
            (
                curve_engine,
                "MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT",
                simulate_curve_native_tape,
                CryptoSwap(),
                180.0,
            ),
            (
                dusk_engine,
                "MAX_ARBITRAGE_FILLS_PER_EVENT",
                simulate_dusk_native_tape,
                CurrentDh2(),
                300.0,
            ),
        ):
            with self.subTest(engine=engine.__name__):
                prices = [150.0] * 5 + [jump_price] + [150.0] * 4
                original_cap = getattr(engine, cap_name)
                try:
                    setattr(engine, cap_name, 1)
                    result = runner(
                        model,
                        "forced_burn_in_cap",
                        prices,
                        [],
                        score_start_index=6,
                        arb_latency_minutes=5,
                        step_minutes=1,
                    )
                finally:
                    setattr(engine, cap_name, original_cap)

                self.assertEqual(result["arbitrage_capped_event_count"], 1)
                self.assertEqual(
                    result["scored_arbitrage_capped_event_count"],
                    0,
                )
                self.assertEqual(result["arbitrage_fill_count"], 0)

    def test_burn_in_arbitrage_numerical_failure_is_not_hidden(self):
        cases = (
            (
                curve_engine,
                "preview_curve_native_order",
                simulate_curve_native_tape,
                CryptoSwap(),
                180.0,
            ),
            (
                dusk_engine,
                "preview_trade",
                simulate_dusk_native_tape,
                CurrentDh2(),
                300.0,
            ),
        )
        for engine, preview_name, runner, model, jump_price in cases:
            for exception, expected_failure in (
                (ValueError("infeasible candidate domain"), False),
                (ArithmeticError("forced solver failure"), True),
            ):
                with self.subTest(
                    engine=engine.__name__,
                    exception=type(exception).__name__,
                ):
                    prices = [150.0] * 5 + [jump_price] + [150.0] * 4
                    with patch.object(engine, preview_name, side_effect=exception):
                        result = runner(
                            model,
                            "forced_burn_in_numerical_failure",
                            prices,
                            [],
                            score_start_index=6,
                            arb_latency_minutes=5,
                            step_minutes=1,
                        )
                    failures = result["arbitrage_numerical_failure_count"]
                    if expected_failure:
                        self.assertGreater(failures, 0)
                    else:
                        self.assertEqual(failures, 0)
                    self.assertEqual(
                        result["scored_arbitrage_numerical_failure_count"],
                        0,
                    )

    def test_arb_latency_must_align_with_sample_cadence(self):
        with self.assertRaisesRegex(ValueError, "multiple of step_minutes"):
            simulate_dusk_native_tape(
                CurrentDh2(),
                "latency_units",
                self.prices,
                self.tape,
                arb_latency_minutes=7,
                step_minutes=5,
            )

    def test_flat_centered_curves_have_zero_marginal_staleness(self):
        prices = [150.0] * 12
        results = (
            simulate_dusk_native_tape(
                CurrentDh2(peak_depth=0.0, fade_scale=0.0),
                "flat_cpmm",
                prices,
                [],
                arb_latency_minutes=1,
            ),
            simulate_dusk_native_tape(
                CurrentDh2(),
                "flat_dh2",
                prices,
                [],
                arb_latency_minutes=1,
            ),
            simulate_curve_native_tape(
                CryptoSwap(),
                "flat_curve",
                prices,
                [],
                arb_latency_minutes=1,
            ),
        )
        for result in results:
            self.assertEqual(result["marginal_tracking_stale_fraction"], 0.0)
            self.assertLess(result["marginal_tracking_max_bps"], 1e-6)
            self.assertNotIn("all_in_stale_fraction", result)
            self.assertNotIn("fee_free_impact_stale_fraction", result)
        self.assertAlmostEqual(
            results[0]["centered_fee_free_service_cost_bps"],
            196.0784313725,
            places=6,
        )
        self.assertGreater(
            results[0]["raw_fee_free_service_median_cost_bps"],
            STATE_DEVIATION_THRESHOLD_BPS,
        )

    def test_marginal_tracking_uses_log_bps_and_exact_boundary(self):
        at_boundary = _marginal_tracking_error_bps(
            math.exp(STATE_DEVIATION_THRESHOLD_BPS / 10_000.0),
            1.0,
            1.0,
        )
        self.assertAlmostEqual(
            at_boundary,
            STATE_DEVIATION_THRESHOLD_BPS,
            places=10,
        )
        self.assertEqual(
            _marginal_tracking_error_bps(2.0, 75.0, 150.0),
            0.0,
        )


if __name__ == "__main__":
    unittest.main()

import copy
import json
import math
import sys
import unittest
from unittest.mock import Mock, patch
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import curve_native as curve_engine
from curve_native import (
    CurveNativeConfig,
    MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT,
    _best_arbitrage,
    _roll_ema_before_trade,
    execute_curve_native_order,
    execute_curve_native_trade,
    execute_profitable_arbitrage,
    initialize_curve_native_pool,
    maybe_recenter,
    native_fee_rate,
    preview_curve_native_order,
    simulate_curve_native_path,
    state_price,
    total_wealth,
    xcp_at_scale,
    CurveNativeState,
    REFERENCE_COMMIT,
    REFERENCE_RELEASE,
)
from economics import (
    ARBITRAGE_GOLDEN_ITERATIONS,
    ARBITRAGE_SEARCH_GRID_COUNT,
    MAX_ARBITRAGE_FILLS_PER_EVENT,
    SLOTS_PER_MINUTE,
)
from models import CryptoSwap


class CurveNativePolicyTests(unittest.TestCase):
    def setUp(self):
        self.model = CryptoSwap(
            amplification=400.0,
            gamma=0.001,
            mid_fee=0.0005,
            out_fee=0.0045,
            fee_gamma=0.01,
        )
        self.config = CurveNativeConfig(
            ema_half_life_minutes=30.0,
            min_adjustment_step=0.001,
            allowed_extra_profit=0.000002,
            arb_min_profit_quote=0.10,
        )

    def test_initial_xcp_and_physical_reserve_normalization(self):
        state = initialize_curve_native_pool(self.model, 150.0)
        d = self.model.invariant(state.base * 150.0, state.quote)
        expected = d / (2.0 * math.sqrt(150.0))
        self.assertAlmostEqual(state.initial_xcp, expected, delta=expected * 1e-12)
        self.assertAlmostEqual(state.virtual_price, 1.0)
        self.assertAlmostEqual(state_price(self.model, state), 150.0, places=7)

    def test_fee_is_withheld_from_output_and_fully_retained(self):
        state = initialize_curve_native_pool(self.model, 150.0)
        base_before = state.base
        quote_before = state.quote
        invariant_before = self.model.invariant(state.base * 150.0, state.quote)
        trade = execute_curve_native_trade(
            self.model,
            state,
            50.0,
            "x_to_y",
            0,
            self.config,
        )
        self.assertAlmostEqual(state.base - base_before, 50.0, places=10)
        self.assertAlmostEqual(quote_before - state.quote, trade.output_amount, places=9)
        self.assertAlmostEqual(
            trade.raw_output,
            trade.output_amount + trade.retained_output_fee,
            places=9,
        )
        self.assertAlmostEqual(
            state.retained_quote_fees,
            trade.retained_output_fee,
            places=9,
        )
        invariant_after = self.model.invariant(state.base * state.price_scale, state.quote)
        self.assertGreater(invariant_after, invariant_before)
        self.assertGreater(state.virtual_price, 1.0)
        self.assertGreater(state.xcp_profit, 1.0)
        self.assertLess(trade.accounting_identity_error, 1e-7)

    def test_reverse_trade_retains_base_output_fee(self):
        state = initialize_curve_native_pool(self.model, 150.0)
        base_before = state.base
        quote_before = state.quote
        trade = execute_curve_native_trade(
            self.model,
            state,
            10_000.0,
            "y_to_x",
            0,
            self.config,
        )
        self.assertAlmostEqual(state.quote - quote_before, 10_000.0, places=8)
        self.assertAlmostEqual(base_before - state.base, trade.output_amount, places=9)
        self.assertAlmostEqual(state.retained_base_fees, trade.retained_output_fee, places=9)
        self.assertGreater(trade.retained_output_fee, 0.0)

    def test_v21_fee_weight_uses_balance_in_its_numerator(self):
        x = 1_400_000.0
        y = 600_000.0
        q = 4.0 * x * y / (x + y) ** 2
        weight = self.model.fee_gamma * q / (
            self.model.fee_gamma * q + 1.0 - q
        )
        expected = self.model.out_fee - weight * (
            self.model.out_fee - self.model.mid_fee
        )
        self.assertAlmostEqual(native_fee_rate(self.model, x, y), expected, places=15)
        self.assertAlmostEqual(
            native_fee_rate(self.model, 1_000_000.0, 1_000_000.0),
            self.model.mid_fee,
            places=15,
        )

    def test_ema_uses_prior_trade_state_and_two_x_upper_cap(self):
        state = initialize_curve_native_pool(self.model, 100.0)
        state.last_state_price = 1_000.0
        _roll_ema_before_trade(
            state,
            CurveNativeConfig(ema_half_life_minutes=10.0),
            10 * SLOTS_PER_MINUTE,
        )
        # One half-life: 50% old EMA plus 50% capped 2x observation.
        self.assertAlmostEqual(state.ema_price, 150.0, places=10)

    def test_internal_trade_has_no_external_price_input(self):
        state_a = initialize_curve_native_pool(self.model, 150.0)
        state_b = copy.deepcopy(state_a)
        trade_a = execute_curve_native_trade(
            self.model,
            state_a,
            25.0,
            "x_to_y",
            5 * SLOTS_PER_MINUTE,
            self.config,
        )
        trade_b = execute_curve_native_trade(
            self.model,
            state_b,
            25.0,
            "x_to_y",
            5 * SLOTS_PER_MINUTE,
            self.config,
        )
        self.assertEqual(trade_a, trade_b)
        self.assertEqual(state_a, state_b)

    def test_half_profit_gate_accepts_only_a_funded_center_move(self):
        state = initialize_curve_native_pool(self.model, 100.0)
        state.base *= 1.05
        state.quote *= 1.05
        state.virtual_price = xcp_at_scale(self.model, state) / state.initial_xcp
        state.xcp_profit = state.virtual_price
        state.ema_price = 101.0
        base_before = state.base
        quote_before = state.quote
        self.assertTrue(maybe_recenter(self.model, state, self.config))
        self.assertGreater(state.price_scale, 100.0)
        self.assertEqual(state.base, base_before)
        self.assertEqual(state.quote, quote_before)
        self.assertGreater(
            (state.virtual_price - self.config.allowed_extra_profit) ** 2,
            state.xcp_profit,
        )

        blocked = initialize_curve_native_pool(self.model, 100.0)
        blocked.base *= 1.30
        blocked.quote *= 1.05
        blocked.virtual_price = xcp_at_scale(self.model, blocked) / blocked.initial_xcp
        blocked.ema_price = 99.0
        candidate_vp = (
            xcp_at_scale(self.model, blocked, 99.8) / blocked.initial_xcp
        )
        pre_gate_squared = (
            blocked.virtual_price - self.config.allowed_extra_profit
        ) ** 2
        blocked.xcp_profit = (
            pre_gate_squared + candidate_vp**2
        ) / 2.0
        self.assertFalse(maybe_recenter(self.model, blocked, self.config))
        self.assertEqual(blocked.price_scale, 100.0)
        self.assertEqual(blocked.recenter_attempt_count, 1)

    def test_marked_order_accounting_is_exact(self):
        state = initialize_curve_native_pool(self.model, 150.0)
        wealth_before = total_wealth(state, 160.0)
        result = execute_curve_native_order(
            self.model,
            state,
            20_000.0,
            "x_to_y",
            160.0,
            0,
            self.config,
        )
        wealth_delta = total_wealth(state, 160.0) - wealth_before
        self.assertAlmostEqual(
            wealth_delta,
            -result.trader_profit_at_reference,
            delta=1e-7,
        )
        self.assertGreater(result.total_fee_value, 0.0)

    def test_arbitrage_is_profitable_and_never_forced(self):
        state = initialize_curve_native_pool(self.model, 150.0)
        before = copy.deepcopy(state)
        self.assertEqual(
            execute_profitable_arbitrage(self.model, state, 150.0, 0, self.config),
            0.0,
        )
        self.assertEqual(state, before)
        profit = execute_profitable_arbitrage(
            self.model,
            state,
            180.0,
            5 * SLOTS_PER_MINUTE,
            self.config,
        )
        self.assertGreater(profit, 0.0)
        self.assertAlmostEqual(state.realized_arb_profit, profit)

    def test_same_event_arbitrage_cascade_reconciles_and_rolls_ema_once(self):
        state = initialize_curve_native_pool(self.model, 150.0, config=self.config)
        # Equal-sided retained value funds native price-scale movement without
        # introducing an inventory skew before the arbitrage event.
        state.base *= 1.02
        state.quote *= 1.02
        state.virtual_price = xcp_at_scale(self.model, state) / state.initial_xcp
        state.xcp_profit = state.virtual_price
        state.ema_price = 165.0
        expected_after_one_roll = copy.deepcopy(state)
        _roll_ema_before_trade(expected_after_one_roll, self.config, 750)
        wealth_before = total_wealth(state, 165.0)
        flow_pnl_before = state.pool_flow_pnl
        realized_before = state.realized_arb_profit

        profit = execute_profitable_arbitrage(
            self.model,
            state,
            165.0,
            750,
            self.config,
        )

        self.assertGreaterEqual(state.arbitrage_fill_count, 2)
        self.assertGreaterEqual(state.recenter_count, 1)
        self.assertAlmostEqual(
            state.ema_price,
            expected_after_one_roll.ema_price,
            places=12,
        )
        self.assertAlmostEqual(
            state.realized_arb_profit - realized_before,
            profit,
            delta=max(profit * 1e-12, 1e-7),
        )
        self.assertAlmostEqual(
            total_wealth(state, 165.0) - wealth_before,
            -profit,
            delta=max(profit * 1e-12, 1e-7),
        )
        self.assertAlmostEqual(
            state.pool_flow_pnl - flow_pnl_before,
            -profit,
            delta=max(profit * 1e-12, 1e-7),
        )
        self.assertLess(
            state.arbitrage_fill_count,
            MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT,
        )
        self.assertIsNone(
            _best_arbitrage(
                self.model,
                state,
                165.0,
                750,
                self.config,
            )
        )
        self.assertEqual(state.arbitrage_capped_event_count, 0)

    def test_one_fill_search_matches_dense_dynamic_fee_kink_sample(self):
        model = CryptoSwap(
            amplification=400.0,
            gamma=0.001,
            mid_fee=0.0005,
            out_fee=0.02,
            fee_gamma=0.0001,
        )
        state = initialize_curve_native_pool(model, 150.0, config=self.config)
        execute_curve_native_order(
            model,
            state,
            250_000.0,
            "x_to_y",
            150.0,
            0,
            self.config,
        )
        external_price = 210.0
        current_slot = 750
        chosen = _best_arbitrage(
            model,
            state,
            external_price,
            current_slot,
            self.config,
        )
        self.assertIsNotNone(chosen)
        chosen_result = chosen[1]
        self.assertGreater(chosen_result.trade.fee_rate, model.out_fee * 0.90)
        self.assertLess(chosen_result.trade.fee_rate, model.out_fee)

        direction = (
            "x_to_y"
            if state_price(model, state) > external_price
            else "y_to_x"
        )
        maximum = min(
            total_wealth(state, external_price) * 0.60,
            (
                state.base * external_price
                if direction == "x_to_y"
                else state.quote
            )
            * 0.90,
        )
        minimum = max(maximum * 1e-8, 1e-6)
        dense_best = -math.inf
        dense_count = 4_097
        for index in range(dense_count):
            amount = minimum * math.exp(
                math.log(maximum / minimum) * index / (dense_count - 1)
            )
            try:
                _, result = preview_curve_native_order(
                    model,
                    state,
                    amount,
                    direction,
                    external_price,
                    current_slot,
                    self.config,
                )
            except (ArithmeticError, OverflowError, ValueError):
                continue
            dense_best = max(dense_best, result.trader_profit_at_reference)
        self.assertGreater(dense_best, 0.0)
        self.assertGreaterEqual(
            chosen_result.trader_profit_at_reference,
            dense_best * (1.0 - 1e-4),
        )

    def test_unresolved_profitable_fill_at_hard_cap_is_diagnosed(self):
        model = CryptoSwap(
            amplification=400.0,
            gamma=0.0001,
            mid_fee=0.0005,
            out_fee=0.0045,
            fee_gamma=0.01,
        )
        state = initialize_curve_native_pool(model, 150.0, config=self.config)
        state.base *= 1.02
        state.quote *= 1.02
        state.virtual_price = xcp_at_scale(model, state) / state.initial_xcp
        state.xcp_profit = state.virtual_price
        state.ema_price = 180.0

        original_cap = curve_engine.MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT
        try:
            curve_engine.MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT = 1
            execute_profitable_arbitrage(
                model,
                state,
                180.0,
                750,
                self.config,
            )
        finally:
            curve_engine.MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT = original_cap

        self.assertEqual(
            state.arbitrage_fill_count,
            1,
        )
        self.assertEqual(state.arbitrage_capped_event_count, 1)
        self.assertIsNotNone(
            _best_arbitrage(model, state, 180.0, 750, self.config)
        )

    def test_post_cap_probe_numerical_failure_is_diagnosed(self):
        state = initialize_curve_native_pool(self.model, 150.0, config=self.config)
        original_cap = curve_engine.MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT
        original_preview = curve_engine.preview_curve_native_order
        first_search_evaluations = (
            ARBITRAGE_SEARCH_GRID_COUNT
            + 2
            + ARBITRAGE_GOLDEN_ITERATIONS
        )
        calls = 0

        def fail_after_first_search(*args, **kwargs):
            nonlocal calls
            calls += 1
            if calls > first_search_evaluations:
                raise ArithmeticError("forced post-cap solver failure")
            return original_preview(*args, **kwargs)

        try:
            curve_engine.MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT = 1
            with patch.object(
                curve_engine,
                "preview_curve_native_order",
                side_effect=fail_after_first_search,
            ):
                execute_profitable_arbitrage(
                    self.model,
                    state,
                    180.0,
                    750,
                    self.config,
                )
        finally:
            curve_engine.MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT = original_cap

        self.assertEqual(state.arbitrage_fill_count, 1)
        self.assertEqual(state.arbitrage_capped_event_count, 0)
        self.assertGreater(state.arbitrage_numerical_failure_count, 0)

    def test_funded_recenter_cascade_converges_beyond_shared_cap(self):
        model = CryptoSwap(
            amplification=1000.0,
            gamma=0.00046123335046868,
            mid_fee=0.0005,
            out_fee=0.0005,
            fee_gamma=0.01,
        )
        config = CurveNativeConfig(
            ema_half_life_minutes=10.016666666666667,
            min_adjustment_step=0.000146,
            allowed_extra_profit=2e-6,
            arb_min_profit_quote=0.10,
        )
        state = CurveNativeState(
            base=7048.1633265415,
            quote=912416.9452444082,
            price_scale=154.31152366165495,
            ema_price=153.6318302779684,
            last_state_price=147.31103899729672,
            last_observation_slot=40500,
            initial_xcp=80381.5929865242,
            virtual_price=1.0010705563365256,
            xcp_profit=1.0009219162747602,
        )

        profit = execute_profitable_arbitrage(
            model,
            state,
            151.91,
            41250,
            config,
        )

        self.assertEqual(MAX_ARBITRAGE_FILLS_PER_EVENT, 16)
        self.assertEqual(MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT, 64)
        self.assertEqual(state.arbitrage_fill_count, 20)
        self.assertEqual(state.recenter_count, 19)
        self.assertGreater(
            state.arbitrage_fill_count,
            MAX_ARBITRAGE_FILLS_PER_EVENT,
        )
        self.assertLess(
            state.arbitrage_fill_count,
            MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT,
        )
        self.assertAlmostEqual(profit, 310.42756840927314, places=8)
        self.assertEqual(state.arbitrage_capped_event_count, 0)
        self.assertEqual(state.arbitrage_numerical_failure_count, 0)
        self.assertIsNone(
            _best_arbitrage(model, state, 151.91, 41250, config)
        )

    def test_curve_local_emergency_cap_still_probes_and_reports_exhaustion(self):
        state = initialize_curve_native_pool(self.model, 150.0, config=self.config)

        def always_profitable(
            _model,
            current_state,
            _external_price,
            _current_slot,
            _config,
            _numerical_failures=None,
        ):
            return (
                copy.deepcopy(current_state),
                Mock(trader_profit_at_reference=1.0),
            )

        with patch.object(
            curve_engine,
            "_best_arbitrage",
            side_effect=always_profitable,
        ) as search:
            profit = execute_profitable_arbitrage(
                self.model,
                state,
                180.0,
                750,
                self.config,
            )

        self.assertEqual(state.arbitrage_fill_count, 64)
        self.assertEqual(profit, 64.0)
        self.assertEqual(state.arbitrage_capped_event_count, 1)
        self.assertEqual(
            search.call_count,
            MAX_CURVE_ARBITRAGE_FILLS_PER_EVENT + 1,
        )

    def test_path_result_matches_shared_economic_ledger_fields(self):
        result = simulate_curve_native_path(
            self.model,
            "jump_reversal",
            [150.0, 165.0, 165.0, 150.0, 150.0],
            step_minutes=5,
            arb_latency_steps=1,
            informed_share=0.25,
            daily_volume_pool_ratio=0.5,
            seed=7,
            config=self.config,
        )
        required = {
            "net_lp_pnl_vs_hold",
            "gross_fee_value",
            "realized_arb_profit",
            "recenter_impairment_value",
            "stale_fraction",
            "center_tracking_error_bps",
            "accounting_identity_error",
            "virtual_price",
            "xcp_profit",
        }
        self.assertTrue(required.issubset(result))
        self.assertLess(abs(result["accounting_identity_error"]), 1e-5)
        self.assertGreaterEqual(result["gross_fee_value"], 0.0)
        self.assertGreater(result["xcp_profit"], 1.0)
        self.assertGreater(result["virtual_price"], 1.0)

    def test_pinned_v21_deployment_quote_fixture(self):
        fixture = json.loads(
            (ROOT / "data" / "curve_v2_1_validation.json").read_text()
        )
        self.assertEqual(fixture["reference_release"], REFERENCE_RELEASE)
        self.assertEqual(fixture["reference_commit"], REFERENCE_COMMIT)

        parameters = fixture["parameters"]
        chain_state = fixture["state"]
        balances = {row["symbol"]: row for row in fixture["tokens"]}
        model = CryptoSwap(
            amplification=float(parameters["paper_A"]),
            gamma=float(parameters["gamma"]),
            mid_fee=float(parameters["mid_fee"]),
            out_fee=float(parameters["out_fee"]),
            fee_gamma=float(parameters["fee_gamma"]),
        )
        controller = CurveNativeConfig(
            ema_half_life_minutes=parameters["ma_time_seconds"] / 60.0,
            min_adjustment_step=float(parameters["adjustment_step"]),
            allowed_extra_profit=float(parameters["allowed_extra_profit"]),
        )

        def deployed_state():
            price_scale = float(chain_state["price_scale"])
            state = CurveNativeState(
                base=int(balances["USDC"]["balance_raw"]) / 1e6,
                quote=int(balances["HLX"]["balance_raw"]) / 1e18,
                price_scale=price_scale,
                # The contract stores the cached oracle; the extrapolated
                # value is a read-only view projection at the fixture block.
                ema_price=float(chain_state["cached_price_oracle"]),
                last_state_price=float(chain_state["last_price"]),
                last_observation_slot=0,
                initial_xcp=1.0,
                virtual_price=float(chain_state["virtual_price"]),
                xcp_profit=float(chain_state["xcp_profit"]),
                xcp_profit_a=float(chain_state["xcp_profit_a"]),
            )
            state.initial_xcp = xcp_at_scale(model, state) / state.virtual_price
            return state

        state = deployed_state()
        modeled_d_raw = model.invariant(
            state.base * state.price_scale,
            state.quote,
        ) * 1e18
        expected_d_raw = int(chain_state["D_raw"])
        self.assertLess(
            abs(modeled_d_raw - expected_d_raw) / expected_d_raw,
            5e-10,
        )
        supply = int(chain_state["lp_total_supply_raw"]) / 1e18
        modeled_virtual_price = xcp_at_scale(model, state) / supply
        self.assertLess(
            abs(modeled_virtual_price - float(chain_state["virtual_price"])),
            5e-10,
        )

        for vector in fixture["quote_vectors"]:
            if vector["input_index"] == 0:
                direction = "y_to_x"
                gross_input = int(vector["input_raw"]) / 1e18
                output_decimals = 6
            else:
                direction = "x_to_y"
                gross_input = int(vector["input_raw"]) / 1e6
                output_decimals = 18
            trade = execute_curve_native_trade(
                model,
                deployed_state(),
                gross_input,
                direction,
                0,
                controller,
            )
            modeled_raw = trade.output_amount * 10**output_decimals
            expected_raw = int(vector["output_raw"])
            raw_error = abs(modeled_raw - expected_raw)
            # The 6-decimal output is only ~51k raw units, so its unavoidable
            # final integer rounding is a wider relative tolerance than the
            # continuous analytical model's sub-ppb error on the 18-decimal
            # vector.
            self.assertLessEqual(raw_error, max(1.0, expected_raw * 5e-10))


if __name__ == "__main__":
    unittest.main()

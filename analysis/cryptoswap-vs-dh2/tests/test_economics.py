import copy
import math
from dataclasses import replace
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from comparison import match_to_curve
from economics import (
    MAX_ARBITRAGE_FILLS_PER_EVENT,
    _best_arbitrage,
    _update_ema_before_trade,
    default_controlled_config,
    execute_trade,
    execute_profitable_arbitrage,
    initialize_pool,
    preview_trade,
    protected_q,
    simulate_path,
    spot_price,
    total_wealth,
)
from models import CurrentDh2


class EconomicEngineTests(unittest.TestCase):
    def test_protected_q_uses_center_normalization(self):
        model = CurrentDh2(peak_depth=200.0, fade_scale=0.1)
        state = initialize_pool(model, 2.5)
        invariant = model.invariant(state.base * state.center, state.quote)
        expected = invariant / (2.0 * math.sqrt(state.center))
        self.assertAlmostEqual(protected_q(model, state), expected, delta=expected * 1e-12)

    def test_flat_flow_earns_fees_and_reconciles(self):
        model = CurrentDh2(peak_depth=200.0, fade_scale=0.1)
        result = simulate_path(
            model,
            "flat",
            [150.0] * 12,
            step_minutes=5,
            arb_latency_steps=1,
            informed_share=0.0,
            daily_volume_pool_ratio=0.5,
            seed=7,
        )
        self.assertGreater(result["net_lp_pnl_vs_hold"], 0.0)
        self.assertGreater(result["gross_fee_value"], 0.0)
        self.assertLess(abs(result["accounting_identity_error"]), 1e-5)

    def test_arbitrage_is_profit_maximizing_and_never_forced_at_a_loss(self):
        curve, _, _, _ = match_to_curve()
        config = default_controlled_config()
        state = initialize_pool(curve, 150.0, config=config)
        before = total_wealth(state, 150.0)
        no_profit = execute_profitable_arbitrage(
            curve,
            state,
            150.0,
            0,
            config,
        )
        self.assertEqual(no_profit, 0.0)
        self.assertAlmostEqual(total_wealth(state, 150.0), before)

        profit = execute_profitable_arbitrage(
            curve,
            state,
            180.0,
            750,
            config,
        )
        self.assertGreater(profit, 0.0)
        self.assertGreaterEqual(state.realized_arb_profit, profit)

    def test_same_event_arbitrage_cascade_reconciles_and_rolls_ema_once(self):
        model = CurrentDh2(peak_depth=200.0, fade_scale=0.1)
        config = default_controlled_config()
        state = initialize_pool(model, 150.0, config=config)
        # Model an already-earned protected budget and a lagging controller.
        # This makes the first fill fund a center move and the new curve admit
        # a second independently profit-maximizing fill.
        state.protected_floor_q *= 0.99
        state.ema = 300.0
        expected_after_one_roll = copy.deepcopy(state)
        _update_ema_before_trade(expected_after_one_roll, config, 750)
        wealth_before = total_wealth(state, 300.0)
        flow_pnl_before = state.pool_flow_pnl
        realized_before = state.realized_arb_profit

        profit = execute_profitable_arbitrage(
            model,
            state,
            300.0,
            750,
            config,
        )

        self.assertGreaterEqual(state.arbitrage_fill_count, 2)
        self.assertGreaterEqual(state.recenter_count, 1)
        self.assertAlmostEqual(
            state.ema,
            expected_after_one_roll.ema,
            places=12,
        )
        self.assertAlmostEqual(
            state.realized_arb_profit - realized_before,
            profit,
            delta=max(profit * 1e-12, 1e-7),
        )
        self.assertAlmostEqual(
            total_wealth(state, 300.0) - wealth_before,
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
            MAX_ARBITRAGE_FILLS_PER_EVENT,
        )
        self.assertIsNone(
            _best_arbitrage(model, state, 300.0, 750, config)
        )
        self.assertEqual(state.arbitrage_capped_event_count, 0)

    def test_one_fill_search_matches_dense_dynamic_fee_kink_sample(self):
        model = CurrentDh2(peak_depth=200.0, fade_scale=0.1)
        config = default_controlled_config()
        state = initialize_pool(model, 150.0, config=config)
        execute_trade(
            model,
            state,
            250_000.0,
            "y_to_x",
            150.0,
            0,
            config,
        )
        # Put the volatility surcharge close to its cap while the reserve
        # path crosses the divergence-fee transition.
        state.volatility_accumulator_nad = 900_000_000
        external_price = 110.0
        current_slot = 750
        chosen = _best_arbitrage(
            model,
            state,
            external_price,
            current_slot,
            config,
        )
        self.assertIsNotNone(chosen)
        chosen_result = chosen[1]
        self.assertIsNotNone(chosen_result.assessment)
        self.assertGreater(
            chosen_result.assessment.total_fee_rate_nad,
            int(config.fee.max_fee_rate_nad * 0.90),
        )
        self.assertLess(
            chosen_result.assessment.total_fee_rate_nad,
            config.fee.max_fee_rate_nad,
        )

        direction = (
            "x_to_y"
            if spot_price(model, state) > external_price
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
                _, result = preview_trade(
                    model,
                    state,
                    amount,
                    direction,
                    external_price,
                    current_slot,
                    config,
                )
            except (ArithmeticError, OverflowError, ValueError):
                continue
            dense_best = max(dense_best, result.trader_profit_at_reference)
        self.assertGreater(dense_best, 0.0)
        self.assertGreaterEqual(
            chosen_result.trader_profit_at_reference,
            dense_best * (1.0 - 1e-4),
        )

    def test_solver_haircut_is_neutral_and_does_not_fund_recentering(self):
        model = CurrentDh2(peak_depth=200.0, fade_scale=0.1)
        base = default_controlled_config()
        config = replace(
            base,
            solver_haircut_ppm=10.0,
            fee=replace(
                base.fee,
                max_fee_rate_nad=base.fee.base_fee_rate_nad,
                divergence_coefficient_nad=0,
                volatility_coefficient_nad=0,
            ),
        )
        state = initialize_pool(model, 150.0, config=config)
        execute_trade(
            model,
            state,
            100_000.0,
            "y_to_x",
            150.0,
            150,
            config,
        )
        self.assertGreater(state.solver_haircut_value, 0.0)
        self.assertEqual(state.retained_surcharge_value, 0.0)
        self.assertAlmostEqual(
            state.current_q,
            state.protected_floor_q,
            delta=max(state.current_q * 2e-9, 1e-9),
        )

    def test_only_base_fee_is_split_away_from_lps(self):
        model = CurrentDh2(peak_depth=200.0, fade_scale=0.1)
        base = default_controlled_config()
        config = replace(base, manager_fee_bps=500, protocol_fee_bps=1_000)
        state = initialize_pool(model, 150.0, config=config)
        result = execute_trade(
            model,
            state,
            50_000.0,
            "y_to_x",
            150.0,
            150,
            config,
        )
        self.assertAlmostEqual(
            result.manager_fee_value + result.protocol_fee_value,
            result.base_fee_value * 0.15,
            delta=1e-7,
        )
        self.assertGreaterEqual(result.dynamic_fee_value, result.retained_surcharge_value)


if __name__ == "__main__":
    unittest.main()

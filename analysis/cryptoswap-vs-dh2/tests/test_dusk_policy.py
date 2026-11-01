import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from dusk_policy import (
    NAD,
    DynamicFeeConfig,
    DynamicFeePath,
    DynamicFeePreState,
    assess_swap_fee,
    decay_volatility_nad,
    fee_amount_ceil,
    outward_divergence_fee_potential_nad,
    preliminary_swap_inputs,
    quote_dynamic_fee,
    symmetric_ratio_distance_nad,
    volatility_after_assessed_swap,
    volatility_after_success_nad,
)


def price(integer):
    return integer * NAD


def default_config():
    return DynamicFeeConfig(
        base_fee_rate_nad=NAD // 1_000,
        max_fee_rate_nad=NAD // 20,
        divergence_coefficient_nad=NAD // 10,
        volatility_coefficient_nad=NAD // 20,
        volatility_half_life_ms=4_000,
        volatility_shock_cap_nad=NAD // 5,
        volatility_accumulator_cap_nad=NAD,
    )


def pre(volatility=0, slot=10):
    return DynamicFeePreState(
        center_price_nad=price(100),
        volatility_accumulator_nad=volatility,
        volatility_last_update_slot=slot,
    )


class DuskPolicyTests(unittest.TestCase):
    def test_symmetric_distance_and_actual_spot_shock_are_direction_independent(self):
        self.assertEqual(symmetric_ratio_distance_nad(price(200), price(100)), NAD)
        self.assertEqual(symmetric_ratio_distance_nad(price(100), price(200)), NAD)
        first = volatility_after_success_nad(
            0, price(100), price(200), NAD // 20, NAD // 10
        )
        second = volatility_after_success_nad(
            first, price(200), price(100), NAD // 20, NAD // 10
        )
        self.assertEqual(first, NAD // 20)
        self.assertEqual(second, NAD // 10)

    def test_outward_potential_is_restorative_free_and_telescopes(self):
        q0 = 1_000_000
        coefficient = NAD // 10
        cap = NAD // 20
        self.assertEqual(
            outward_divergence_fee_potential_nad(
                q0, 800_000, 900_000, coefficient, cap
            ),
            0,
        )
        crossing = outward_divergence_fee_potential_nad(
            q0, 800_000, 1_100_000, coefficient, cap
        )
        center_to_end = outward_divergence_fee_potential_nad(
            q0, q0, 1_100_000, coefficient, cap
        )
        first = center_to_end
        second = outward_divergence_fee_potential_nad(
            q0, 1_100_000, 1_200_000, coefficient, cap
        )
        whole = outward_divergence_fee_potential_nad(
            q0, q0, 1_200_000, coefficient, cap
        )
        self.assertEqual(crossing, center_to_end)
        self.assertGreater(first, 0)
        self.assertEqual(whole, first + second)

    def test_preliminary_pass_deducts_base_and_existing_volatility(self):
        config = default_config()
        state = pre(volatility=NAD // 5)
        gross = 1_000_000
        preliminary = preliminary_swap_inputs(
            config, state, gross, current_slot=10, retain_dynamic_surcharge=True
        )
        expected_base = fee_amount_ceil(gross, config.base_fee_rate_nad)
        expected_volatility_rate = NAD // 100
        expected_volatility = fee_amount_ceil(gross, expected_volatility_rate)
        self.assertEqual(preliminary.base_fee_amount, expected_base)
        self.assertEqual(
            preliminary.volatility_surcharge_amount, expected_volatility
        )
        self.assertEqual(
            preliminary.amount_in_for_quote,
            gross - expected_base - expected_volatility,
        )
        # Known dynamic fees are retained in reserves, but excluded from quote.
        self.assertEqual(preliminary.reserve_input_credit, gross - expected_base)

    def test_dynamic_hard_cap_prioritizes_divergence(self):
        config = DynamicFeeConfig(
            base_fee_rate_nad=NAD // 100,
            max_fee_rate_nad=NAD // 50,
            divergence_coefficient_nad=NAD,
            volatility_coefficient_nad=NAD,
            volatility_half_life_ms=4_000,
            volatility_shock_cap_nad=NAD // 5,
            volatility_accumulator_cap_nad=NAD,
        )
        quote = quote_dynamic_fee(
            config,
            pre(volatility=NAD, slot=0),
            DynamicFeePath(
                amount_in=1_000_000,
                start_price_nad=price(100),
                end_price_nad=price(200),
                current_slot=0,
                divergence_surcharge_amount=(1 << 64) - 1,
            ),
        )
        self.assertEqual(quote.total_fee_amount, 20_000)
        self.assertEqual(quote.divergence_surcharge_amount, 10_000)
        self.assertEqual(quote.volatility_surcharge_amount, 0)
        self.assertEqual(quote.total_rate_nad, config.max_fee_rate_nad)

    def test_binary_retention_reconciles_reserve_and_claimable_accounting(self):
        config = default_config()
        gross = 200_000
        arguments = dict(
            config=config,
            pre_state=pre(volatility=NAD // 5),
            reserve_credit=gross,
            current_slot=10,
            center_input_reserve=1_000_000,
            start_input_reserve=1_000_000,
        )
        retained = assess_swap_fee(
            **arguments, retain_dynamic_surcharge=True
        )
        distributed = assess_swap_fee(
            **arguments, retain_dynamic_surcharge=False
        )
        self.assertGreater(retained.dynamic_surcharge_debit, 0)
        self.assertEqual(
            retained.retained_surcharge, retained.dynamic_surcharge_debit
        )
        self.assertEqual(retained.claimable_fee_debit, retained.base_fee_debit)
        self.assertEqual(
            retained.reserve_input_credit, gross - retained.base_fee_debit
        )

        self.assertEqual(distributed.retained_surcharge, 0)
        self.assertEqual(
            distributed.claimable_fee_debit, distributed.total_fee_debit
        )
        self.assertEqual(
            distributed.reserve_input_credit, distributed.amount_in_for_quote
        )
        retained.validate_accounting()
        distributed.validate_accounting()

    def test_accumulator_decays_then_commits_actual_amm_spot_shock(self):
        config = default_config()
        one_half_life_later = 10 + config.volatility_half_life_ms // 400
        assessment = assess_swap_fee(
            config,
            pre(volatility=NAD),
            reserve_credit=100_000,
            current_slot=one_half_life_later,
            center_input_reserve=1_000_000,
            start_input_reserve=900_000,
            retain_dynamic_surcharge=True,
        )
        self.assertLessEqual(abs(assessment.decayed_volatility_nad - NAD // 2), 1_000)
        committed = volatility_after_assessed_swap(
            config, assessment, price(100), price(110)
        )
        expected_shock = symmetric_ratio_distance_nad(price(100), price(110))
        self.assertEqual(
            committed,
            min(
                assessment.decayed_volatility_nad + expected_shock,
                config.volatility_accumulator_cap_nad,
            ),
        )

    def test_integer_rounding_matches_on_chain_edges(self):
        self.assertEqual(fee_amount_ceil(1, 1), 1)
        decayed = decay_volatility_nad(NAD, 10, 20, 4_000)
        self.assertLessEqual(abs(decayed - NAD // 2), 1_000)
        with self.assertRaises(ValueError):
            decay_volatility_nad(NAD, 2, 1, 1_000)


if __name__ == "__main__":
    unittest.main()

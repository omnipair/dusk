use super::*;

fn price(integer: u64) -> u64 {
    integer * NAD
}

fn default_config() -> DynamicFeeConfig {
    DynamicFeeConfig {
        base_fee_rate_nad: NAD / 1_000,       // 10 bps
        divergence_coefficient_nad: NAD / 10, // 10% near-center coefficient
        volatility_coefficient_nad: NAD / 20, // 5% of volatility pressure near zero
        volatility_half_life_ms: 4_000,
        volatility_shock_cap_nad: NAD / 5,
        volatility_accumulator_cap_nad: NAD,
    }
}

fn pre(center: u64, volatility: u64, slot: u64) -> DynamicFeePreState {
    DynamicFeePreState {
        center_price_nad: center,
        volatility_accumulator_nad: volatility,
        volatility_last_update_slot: slot,
    }
}

fn path(amount: u64, start: u64, end: u64, slot: u64, divergence_surcharge_amount: u64) -> DynamicFeePath {
    DynamicFeePath {
        amount_in: amount,
        start_price_nad: start,
        end_price_nad: end,
        current_slot: slot,
        divergence_surcharge_amount,
    }
}

#[test]
fn symmetric_distance_is_direction_independent() {
    assert_eq!(
        symmetric_ratio_distance_nad(price(200), price(100)).unwrap(),
        NAD as u128
    );
    assert_eq!(
        symmetric_ratio_distance_nad(price(100), price(200)).unwrap(),
        NAD as u128
    );
}

#[test]
fn trend_charges_invariant_coordinate_divergence_potential() {
    let q0 = 1_000_000_u128;
    let outward_fee = outward_divergence_fee_potential_nad(q0, q0, q0 + 50_000, NAD / 10).unwrap() as u64;
    let quote = quote_dynamic_fee(
        default_config(),
        pre(price(100), 0, 10),
        path(50_000, price(100), price(110), 10, outward_fee),
    )
    .unwrap();

    assert!(outward_fee > 0);
    assert_eq!(quote.divergence_surcharge_amount, outward_fee);
    assert!(quote.divergence_rate_nad > 0);
    assert_eq!(quote.volatility_rate_nad, 0);
    assert_eq!(quote.post_success_volatility_nad, NAD / 10);
    assert!(quote.total_fee_amount < 50_000);
}

#[test]
fn restorative_and_center_crossing_coordinates_only_charge_outward_flow() {
    let q0 = 1_000_000_u128;
    let coefficient = NAD / 10;
    let restorative = outward_divergence_fee_potential_nad(q0, 800_000, 900_000, coefficient).unwrap();
    assert_eq!(restorative, 0);

    let crossing = outward_divergence_fee_potential_nad(q0, 800_000, 1_100_000, coefficient).unwrap();
    let center_to_end = outward_divergence_fee_potential_nad(q0, q0, 1_100_000, coefficient).unwrap();
    assert_eq!(crossing, center_to_end);

    let outward = outward_divergence_fee_potential_nad(q0, 1_100_000, 1_200_000, coefficient).unwrap();
    assert!(outward > 0);
}

#[test]
fn test_reference_mul_div_matches_wide_arithmetic_and_saturates_only_the_quotient() {
    let cases = [
        (u128::MAX, u64::MAX as u128, u128::MAX),
        (u128::MAX - 17, 1_u128 << 64, u128::MAX - 31),
        (1_u128 << 127, 3, 7),
    ];
    for (value, numerator, denominator) in cases {
        let (quotient, remainder, saturated) = mul_div_rem_saturating(value, numerator, denominator).unwrap();
        let product = U512::from(value) * U512::from(numerator);
        let reference_quotient = product / U512::from(denominator);
        let reference_remainder = product % U512::from(denominator);
        assert!(!saturated);
        assert_eq!(U512::from(quotient), reference_quotient);
        assert_eq!(U512::from(remainder), reference_remainder);
    }

    let (_, _, saturated) = mul_div_rem_saturating(u128::MAX, u128::MAX, 1).unwrap();
    assert!(saturated);
}

#[test]
fn test_reference_mul_div_fallback_is_bounded_by_the_q48_multiplier_width() {
    let numerator = 3 * Q48;
    let (quotient, _, saturated) =
        mul_div_rem_saturating(u128::MAX - 1, numerator, u128::MAX).unwrap();
    let iterations = LAST_MUL_DIV_FALLBACK_ITERATIONS.with(std::cell::Cell::get);

    assert!(!saturated);
    assert_eq!(quotient, numerator - 1);
    assert_eq!(iterations, u128::BITS - numerator.leading_zeros());
    assert!(iterations <= 50);
}

#[test]
fn analytical_reference_u64_coordinates_finish_gcd_well_inside_the_hard_bound() {
    // Consecutive Fibonacci numbers are the Euclidean algorithm's worst case.
    let center = 7_540_113_804_746_346_429_u128;
    let outward = 4_660_046_610_375_530_309_u128;
    let _ = outward_divergence_marginal_rate_nad(center, center + outward, NAD).unwrap();
    let iterations = LAST_GCD_ITERATIONS.with(std::cell::Cell::get);

    assert!(iterations > 80);
    assert!(iterations < 96);
    assert!(iterations < MAX_EUCLID_GCD_ITERATIONS);
}

#[test]
fn additive_potential_telescopes_across_split_monotonic_path() {
    let mut config = default_config();
    config.base_fee_rate_nad = 0;
    config.volatility_coefficient_nad = 0;

    let q0 = 1_000_000_u128;
    let whole_potential =
        outward_divergence_fee_potential_nad(q0, q0, 1_200_000, config.divergence_coefficient_nad).unwrap() as u64;
    let first_potential =
        outward_divergence_fee_potential_nad(q0, q0, 1_100_000, config.divergence_coefficient_nad).unwrap() as u64;
    let second_potential =
        outward_divergence_fee_potential_nad(q0, 1_100_000, 1_200_000, config.divergence_coefficient_nad).unwrap()
            as u64;
    assert_eq!(whole_potential, first_potential + second_potential);

    let whole = quote_dynamic_fee(
        config,
        pre(price(100), 0, 0),
        path(200_000, price(100), price(120), 0, whole_potential),
    )
    .unwrap();
    let first = quote_dynamic_fee(
        config,
        pre(price(100), 0, 0),
        path(100_000, price(100), price(110), 0, first_potential),
    )
    .unwrap();
    let second = quote_dynamic_fee(
        config,
        pre(price(100), 0, 0),
        path(100_000, price(110), price(120), 0, second_potential),
    )
    .unwrap();

    assert_eq!(whole.total_fee_amount, first.total_fee_amount + second.total_fee_amount);
}

#[test]
fn divergence_marginal_deterioration_is_monotonic() {
    let q0 = 1_000_000_u128;
    let step = 250_000_u128;
    let coefficient = NAD;
    let mut segment_fees = Vec::new();

    for segment in 0_u128..8 {
        let start = q0 + segment * step;
        let end = start + step;
        segment_fees.push(outward_divergence_fee_potential_nad(q0, start, end, coefficient).unwrap());
    }

    assert!(segment_fees.windows(2).all(|pair| pair[1] > pair[0]));
    assert!(segment_fees.last().copied().unwrap() > step);
}

#[test]
fn analytical_divergence_marginal_matches_the_convex_additive_potential() {
    let q0 = 1_000_000_000_000_u128;
    let coefficient = NAD / 10;
    let step = q0 / 1_000_000;
    let mut rates = Vec::new();

    for multiple in [0_u128, 1, 10, 100, 1_000, 100_000] {
        let start = q0 + multiple * q0 / 100;
        let rate = outward_divergence_marginal_rate_nad(q0, start, coefficient).unwrap();
        rates.push(rate);
        if start > q0 {
            let segment = outward_divergence_fee_potential_nad(q0, start, start + step, coefficient).unwrap();
            let observed_rate = segment * NAD as u128 / step;
            // The state potential is floored to raw reserve atoms at both
            // endpoints. Differencing can therefore lose up to two atoms
            // before it is rescaled into a marginal NAD rate.
            let potential_rounding_tolerance = 2 * NAD as u128 / step;
            let tolerance = (rate as u128 / 500).max(3) + potential_rounding_tolerance;
            assert!(observed_rate.abs_diff(rate as u128) <= tolerance);
        }
    }

    assert_eq!(rates[0], 0);
    assert!(rates.windows(2).all(|pair| pair[1] >= pair[0]));
    assert!(rates.last().copied().unwrap() > 100 * NAD);
}

#[test]
fn divergence_near_center_remains_quadratic_and_far_tail_is_unbounded() {
    let q0 = 1_000_000_000_000_u128;
    let coefficient = NAD / 10;
    let start_outward = q0 / 100;
    let step = q0 / 10_000;
    let fee =
        outward_divergence_fee_potential_nad(q0, q0 + start_outward, q0 + start_outward + step, coefficient).unwrap();
    let midpoint = start_outward + step / 2;
    let quadratic_estimate = 4_u128 * coefficient as u128 * midpoint * midpoint * step / (NAD as u128 * q0 * q0);
    assert!(fee.abs_diff(quadratic_estimate) * 100 < quadratic_estimate * 5);

    let far_start = q0 + 1_000 * q0;
    let far_end = far_start + q0;
    let far_fee = outward_divergence_fee_potential_nad(q0, far_start, far_end, coefficient).unwrap();
    assert!(far_fee > 100 * q0);
}

#[test]
fn volatility_pressure_is_monotonic_and_asymptotic() {
    let signals = [NAD as u128 / 100, NAD as u128, 100 * NAD as u128, u128::MAX];
    let rates: Vec<u64> = signals
        .iter()
        .map(|signal| asymptotic_scaled_rate_nad(*signal, NAD).unwrap())
        .collect();

    assert!(rates.windows(2).all(|pair| pair[1] > pair[0]));
    assert!(rates.iter().all(|rate| *rate < NAD));
    assert!(rates[3] >= NAD - 1);
}

#[test]
fn configured_maximum_volatility_pressure_has_the_documented_asymptote() {
    let rate = asymptotic_scaled_rate_nad(10 * NAD as u128, 100 * NAD).unwrap();
    assert_eq!(rate, 999_000_999);
    assert!(rate < NAD);
}

#[test]
fn chop_builds_volatility_and_charges_the_reversal() {
    let config = default_config();
    let outward = quote_dynamic_fee(
        config,
        pre(price(100), 0, 10),
        path(1_000_000, price(100), price(110), 10, 0),
    )
    .unwrap();
    assert_eq!(outward.volatility_rate_nad, 0);

    let reversal = quote_dynamic_fee(
        config,
        pre(price(100), outward.post_success_volatility_nad, 10),
        path(1_000_000, price(110), price(100), 10, 0),
    )
    .unwrap();
    assert_eq!(reversal.divergence_rate_nad, 0);
    assert!(reversal.volatility_rate_nad > 0);
    assert!(reversal.volatility_rate_nad < NAD / 200); // p/(1+p) is below its linear approximation.
    assert_eq!(reversal.post_success_volatility_nad, NAD / 5);
}

#[test]
fn volatility_decays_by_half_after_one_half_life() {
    let decayed = decay_volatility_nad(NAD, 10, 20, 4_000).unwrap();

    // The shared bounded exponential approximation is within one ppm here.
    assert!(decayed.abs_diff(NAD / 2) <= 1_000);
}

#[test]
fn shock_and_accumulator_caps_bound_repeated_moves() {
    let after_first = volatility_after_success_nad(0, price(100), price(200), NAD / 20, NAD / 10).unwrap();
    assert_eq!(after_first, NAD / 20);
    let after_second = volatility_after_success_nad(after_first, price(200), price(100), NAD / 20, NAD / 10).unwrap();
    assert_eq!(after_second, NAD / 10);
    let after_third = volatility_after_success_nad(after_second, price(100), price(200), NAD / 20, NAD / 10).unwrap();
    assert_eq!(after_third, NAD / 10);
}

#[test]
fn composed_fees_remain_below_one_without_an_economic_cap() {
    let mut config = default_config();
    config.base_fee_rate_nad = NAD / 10;
    config.volatility_coefficient_nad = NAD;
    config.volatility_shock_cap_nad = 0;
    config.volatility_accumulator_cap_nad = NAD;

    let quote = quote_dynamic_fee(
        config,
        pre(price(100), NAD, 0),
        path(1_000_000_000, price(100), price(100), 0, 2_000_000),
    )
    .unwrap();

    assert!(quote.base_fee_amount > 0);
    assert!(quote.volatility_surcharge_amount > 0);
    assert_eq!(quote.divergence_surcharge_amount, 2_000_000);
    assert!(quote.total_fee_amount < 1_000_000_000);
    assert!(quote.total_rate_nad < NAD);
    assert_eq!(
        quote.dynamic_surcharge_amount,
        quote.divergence_surcharge_amount + quote.volatility_surcharge_amount
    );
}

#[test]
fn composed_fee_components_preserve_positive_input_across_parameter_extremes() {
    let amount_in = 1_000_000_000_000_u64;
    for base_fee_rate_nad in [0_u64, 1, NAD / 100, NAD / 2, NAD - 1] {
        for volatility in [0_u64, NAD, 10 * NAD] {
            for volatility_coefficient_nad in [0_u64, NAD, 100 * NAD] {
                let config = DynamicFeeConfig {
                    base_fee_rate_nad,
                    divergence_coefficient_nad: 100 * NAD,
                    volatility_coefficient_nad,
                    volatility_half_life_ms: 4_000,
                    volatility_shock_cap_nad: 10 * NAD,
                    volatility_accumulator_cap_nad: 10 * NAD,
                };
                let without_divergence = quote_dynamic_fee(
                    config,
                    pre(price(100), volatility, 0),
                    path(amount_in, price(100), price(100), 0, 0),
                )
                .unwrap();
                let executable_before_divergence = amount_in - without_divergence.total_fee_amount;
                let divergence = executable_before_divergence.saturating_sub(1) / 2;
                let quote = quote_dynamic_fee(
                    config,
                    pre(price(100), volatility, 0),
                    path(amount_in, price(100), price(100), 0, divergence),
                )
                .unwrap();

                assert_eq!(
                    quote.dynamic_surcharge_amount,
                    quote.divergence_surcharge_amount + quote.volatility_surcharge_amount
                );
                assert_eq!(
                    quote.total_fee_amount,
                    quote.base_fee_amount + quote.dynamic_surcharge_amount
                );
                assert!(quote.total_fee_amount < amount_in);
                assert!(quote.total_rate_nad < NAD);
            }
        }
    }
}

#[test]
fn one_atom_quotes_either_preserve_input_or_fail_closed() {
    let mut config = default_config();
    config.base_fee_rate_nad = 0;
    config.divergence_coefficient_nad = 0;
    config.volatility_coefficient_nad = 0;
    let free = quote_dynamic_fee(config, pre(price(100), 0, 0), path(1, price(100), price(100), 0, 0)).unwrap();
    assert_eq!(free.total_fee_amount, 0);

    config.volatility_coefficient_nad = u64::MAX;
    config.volatility_accumulator_cap_nad = u64::MAX;
    let volatile = quote_dynamic_fee(
        config,
        pre(price(100), u64::MAX, 0),
        path(1, price(100), price(100), 0, 0),
    )
    .unwrap();
    assert_eq!(volatile.total_fee_amount, 0); // floor rounding leaves the atom executable.

    config.volatility_coefficient_nad = 0;
    config.divergence_coefficient_nad = NAD;
    assert!(quote_dynamic_fee(config, pre(price(100), 0, 0), path(1, price(100), price(101), 0, 1)).is_err());

    config.divergence_coefficient_nad = 0;
    config.base_fee_rate_nad = 1;
    assert!(quote_dynamic_fee(config, pre(price(100), 0, 0), path(1, price(100), price(100), 0, 0)).is_err());
}

#[test]
fn coefficient_signal_and_coordinate_extremes_saturate_or_fail_closed() {
    assert!(outward_divergence_fee_potential_nad(1, 1, u128::MAX, u64::MAX).is_err());
    let (saturated_fee, saturated) =
        outward_divergence_fee_raw_saturating(1, u64::MAX as u128 - 1, u64::MAX as u128, 9, u64::MAX).unwrap();
    assert_eq!(saturated_fee, u128::MAX);
    assert!(saturated);

    // If the balanced reserve lies beyond every representable token-account
    // balance, no u64 swap can cross it. This is wholly restorative flow and
    // must not be mistaken for an overflowing outward surcharge.
    let unreachable_center = (u64::MAX as u128 + 1) * NAD as u128;
    let (restorative_fee, restorative_saturated) = outward_divergence_fee_raw_saturating(
        unreachable_center,
        u64::MAX as u128 * NAD as u128,
        u64::MAX as u128 * NAD as u128,
        9,
        u64::MAX,
    )
    .unwrap();
    assert_eq!(restorative_fee, 0);
    assert!(!restorative_saturated);

    let rate = asymptotic_scaled_rate_nad(u128::MAX, u64::MAX).unwrap();
    assert!(rate < NAD);

    let mut config = default_config();
    config.base_fee_rate_nad = NAD - 1;
    config.volatility_coefficient_nad = u64::MAX;
    config.volatility_shock_cap_nad = 0;
    config.volatility_accumulator_cap_nad = u64::MAX;
    let quote = quote_dynamic_fee(
        config,
        pre(price(100), u64::MAX, 0),
        path(u64::MAX, price(100), price(100), 0, 0),
    )
    .unwrap();
    assert!(quote.total_fee_amount < u64::MAX);
    assert!(quote.total_rate_nad < NAD);
}

#[test]
fn fee_rounding_and_invalid_inputs_fail_checked() {
    assert_eq!(fee_amount_ceil(1, 1).unwrap(), 1);
    assert!(symmetric_ratio_distance_nad(0, NAD).is_err());
    assert!(decay_volatility_nad(NAD, 2, 1, 1_000).is_err());
    assert!(decay_volatility_nad(NAD, 0, u64::MAX, 1_000).is_err());

    let mut invalid = default_config();
    invalid.base_fee_rate_nad = NAD;
    assert!(quote_dynamic_fee(invalid, pre(price(100), 0, 0), path(100, price(100), price(100), 0, 0)).is_err());
}

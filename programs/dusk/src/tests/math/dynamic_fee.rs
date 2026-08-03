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
fn narrow_divergence_potential_is_bit_exact_and_wide_domain_falls_back() {
    let ordinary = [
        (1_u128, 1_u128, 1_u64),
        (50_000_000_000_000_u128, 100_000_000_000_000_u128, NAD / 10),
        (u64::MAX as u128, u64::MAX as u128, NAD),
    ];
    for (outward, center, coefficient) in ordinary {
        let narrow = divergence_state_potential_u256(outward, center, coefficient)
            .expect("ordinary domain must use U256");
        let reference = divergence_state_potential_u512_reference(outward, center, coefficient).unwrap();
        assert_eq!(narrow, reference);
        assert_eq!(
            divergence_state_potential_wide(outward, center, coefficient).unwrap(),
            reference
        );
    }

    let wide_outward = u128::MAX;
    let wide_center = u128::MAX - 1;
    assert!(divergence_state_potential_u256(wide_outward, wide_center, u64::MAX).is_none());
    assert_eq!(
        divergence_state_potential_wide(wide_outward, wide_center, u64::MAX).unwrap(),
        divergence_state_potential_u512_reference(wide_outward, wide_center, u64::MAX).unwrap()
    );

    for (outward, center, coefficient) in ordinary {
        let input = center + outward;
        assert_eq!(
            outward_divergence_marginal_rate_u256(center, input, coefficient)
                .expect("ordinary marginal domain must use U256"),
            outward_divergence_marginal_rate_u512(center, input, coefficient).unwrap()
        );
    }
    let wide_marginal_center = u128::MAX / 3;
    let wide_marginal_input = u128::MAX;
    assert!(outward_divergence_marginal_rate_u256(
        wide_marginal_center,
        wide_marginal_input,
        u64::MAX
    )
    .is_none());
    assert_eq!(
        outward_divergence_marginal_rate_nad(wide_marginal_center, wide_marginal_input, u64::MAX).unwrap(),
        outward_divergence_marginal_rate_u512(wide_marginal_center, wide_marginal_input, u64::MAX).unwrap()
    );

    // Mirrors the valid zero-decimal wide-CPMM SBF stress gate.
    assert!(divergence_state_potential_u256(
        5_000_000_000_000_000_000_000_000,
        10_000_000_000_000_000_000_000,
        100 * NAD,
    )
    .is_none());
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
    let center = u128::MAX / 2;
    assert!(outward_divergence_fee_potential_nad(center, center, u128::MAX, u64::MAX).is_err());
    let (saturated_fee, saturated) =
        outward_divergence_fee_raw_saturating(center, center, u128::MAX, 9, u64::MAX).unwrap();
    assert_eq!(saturated_fee, u128::MAX);
    assert!(saturated);

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

fn common_potential_at_quote_center(
    balanced_common_nad: u128,
    start_common_nad: u128,
    input_decimals: u8,
    coefficient_nad: u64,
) -> PreparedCommonDivergencePotential {
    prepare_common_divergence_potential(
        balanced_common_nad,
        start_common_nad,
        start_common_nad,
        NAD,
        input_decimals,
        coefficient_nad,
    )
    .unwrap()
}

#[test]
fn common_raw_state_potential_telescopes_for_every_supported_decimal_class() {
    let balanced = 1_000_000_u128 * NAD as u128;
    let coefficient = 100 * NAD;

    for decimals in [0_u8, 6, 9] {
        let token_scale = 10_u64.pow(decimals as u32);
        let start_raw = 1_000_000_u64 * token_scale;
        let total_raw = 100_000_u64 * token_scale;
        let start_nad = normalize_to_nad(start_raw as u128, decimals).unwrap();
        assert_eq!(start_nad, balanced);

        let whole = common_potential_at_quote_center(balanced, start_nad, decimals, coefficient)
            .fee_raw_saturating(total_raw)
            .unwrap();
        assert!(!whole.1);
        assert!(whole.0 > 0);

        for pieces in [2_u64, 10, 100] {
            let piece_raw = total_raw / pieces;
            let mut split_start_raw = start_raw;
            let mut split_sum = 0_u128;
            for _ in 0..pieces {
                let split_start_nad = normalize_to_nad(split_start_raw as u128, decimals).unwrap();
                let segment = common_potential_at_quote_center(
                    balanced,
                    split_start_nad,
                    decimals,
                    coefficient,
                )
                .fee_raw_saturating(piece_raw)
                .unwrap();
                assert!(!segment.1);
                split_sum += segment.0;
                split_start_raw += piece_raw;
            }
            assert_eq!(split_start_raw, start_raw + total_raw);
            assert_eq!(split_sum, whole.0, "decimals={decimals}, pieces={pieces}");
        }
    }
}

#[test]
fn prepared_common_potential_is_bit_exact_to_unfolded_reference() {
    for decimals in [0_u8, 6, 9] {
        let decimal_scale = 10_u128.pow((NAD_DECIMALS - decimals) as u32);
        for rate in [1_u64, NAD / 2, NAD, 2 * NAD, u64::MAX] {
            for coefficient in [0_u64, 1, NAD, 100 * NAD, u64::MAX] {
                let balanced = 1_000_000_000_000_u128;
                let coefficient_times_four = coefficient as u128 * 4;
                let denominator_rate_scale = rate as u128 * 3 * decimal_scale;
                for common in [balanced / 2, balanced, balanced + 1, 2 * balanced, MAX_COMMON_RESERVE] {
                    let optimized = common_raw_state_potential_wide_prepared(
                        common,
                        balanced,
                        coefficient_times_four,
                        denominator_rate_scale,
                    )
                    .unwrap();
                    let reference = common_raw_state_potential_u512_reference(
                        common,
                        balanced,
                        rate,
                        decimal_scale,
                        coefficient,
                    )
                    .unwrap();
                    assert_eq!(optimized, reference, "decimals={decimals}, rate={rate}, coefficient={coefficient}, common={common}");
                }
            }
        }
    }
}

#[test]
fn base_and_quote_common_coordinates_mirror_across_center_prices() {
    let balanced = 1_000_000_u128 * NAD as u128;
    let coefficient = 10 * NAD;

    for decimals in [0_u8, 6, 9] {
        let common_input = 100_000_u64 * 10_u64.pow(decimals as u32);
        for center in [NAD / 2, NAD, 2 * NAD] {
            let base_start = balanced * NAD as u128 / center as u128;
            let base_input = u64::try_from(common_input as u128 * NAD as u128 / center as u128).unwrap();
            let base = prepare_common_divergence_potential(
                balanced,
                base_start,
                balanced,
                center,
                decimals,
                coefficient,
            )
            .unwrap();
            let quote = common_potential_at_quote_center(balanced, balanced, decimals, coefficient);

            assert_eq!(
                base.endpoint_common_nad(base_input).unwrap(),
                quote.endpoint_common_nad(common_input).unwrap()
            );
            let base_fee = base.fee_raw_saturating(base_input).unwrap();
            let quote_fee = quote.fee_raw_saturating(common_input).unwrap();
            assert!(!base_fee.1 && !quote_fee.1);
            let base_fee_common = base_fee.0 * center as u128 / NAD as u128;
            assert!(
                base_fee_common.abs_diff(quote_fee.0) <= 1,
                "decimals={decimals}, center={center}"
            );
        }
    }
}

#[test]
fn absolute_common_conversion_preserves_fractional_center_price_carry() {
    let rate = 600_000_000_u64;
    let start_asset = 1_000_000_001_u128;
    let start_common = canonical_common_coordinate(start_asset, rate).unwrap();
    let prepared = prepare_common_divergence_potential(
        500_000_000,
        start_asset,
        start_common,
        rate,
        9,
        NAD,
    )
    .unwrap();

    assert_eq!(start_common, 600_000_000);
    assert_eq!(canonical_common_coordinate(1, rate).unwrap(), 0);
    assert_eq!(prepared.endpoint_common_nad(1).unwrap(), Some(600_000_001));
}

#[test]
fn common_potential_charges_only_the_outward_portion() {
    let balanced = 1_000_000_000_u128;
    let coefficient = 10 * NAD;
    let restoring = common_potential_at_quote_center(balanced, balanced - 100_000_000, 9, coefficient)
        .fee_raw_saturating(50_000_000)
        .unwrap();
    assert_eq!(restoring, (0, false));

    let crossing = common_potential_at_quote_center(balanced, balanced - 100_000_000, 9, coefficient)
        .fee_raw_saturating(150_000_000)
        .unwrap();
    let center_to_end = common_potential_at_quote_center(balanced, balanced, 9, coefficient)
        .fee_raw_saturating(50_000_000)
        .unwrap();
    assert_eq!(crossing, center_to_end);

    let outward = common_potential_at_quote_center(balanced, balanced + 100_000_000, 9, coefficient)
        .fee_raw_saturating(50_000_000)
        .unwrap();
    assert!(outward.0 > 0 && !outward.1);
}

#[test]
fn common_potential_rejects_unsupported_decimals_and_saturates_past_curve_domain() {
    assert!(prepare_common_divergence_potential(1, 1, 1, NAD, NAD_DECIMALS + 1, NAD).is_err());

    let prepared = prepare_common_divergence_potential(
        MAX_COMMON_RESERVE / 2,
        MAX_COMMON_RESERVE - 1,
        MAX_COMMON_RESERVE - 1,
        NAD,
        9,
        u64::MAX,
    )
    .unwrap();
    assert_eq!(prepared.endpoint_common_nad(2).unwrap(), None);
    assert_eq!(prepared.fee_raw_saturating(2).unwrap(), (u128::MAX, true));
}

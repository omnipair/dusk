use num_bigint::BigUint;
use proptest::prelude::*;

use super::*;

fn geometry() -> ConcentratedCurveGeometry {
    // Tail liquidity 1,000 and inner liquidity 4,000 over sqrt prices 10..20.
    ConcentratedCurveGeometry {
        inner_liquidity: 4_000,
        inner_base_amplification_offset: 200,
        inner_quote_amplification_offset: 40_000,
        lower_tail_base_inventory: 200,
        upper_tail_quote_inventory: 40_000,
        lower_boundary: ConcentratedCurvePoint {
            base_reserve: 300,
            quote_reserve: 10_000,
        },
        upper_boundary: ConcentratedCurvePoint {
            base_reserve: 50,
            quote_reserve: 60_000,
        },
        ..ConcentratedCurveGeometry::cpmm()
    }
}
#[test]
fn liquidity_range_constructor_matches_closed_form_geometry() {
    assert_eq!(
        ConcentratedCurveCache {
            math_revision: CONCENTRATED_CURVE_MATH_REVISION,
            peak_amplification_nad: 5 * NAD,
            core_half_width_bps: 1,
            fade_width_bps: 0,
            tail_liquidity: 1_000,
            concentrated_liquidity: 4_000,
            core_lower_sqrt_price_nad: 10 * NAD as u128,
            core_upper_sqrt_price_nad: 20 * NAD as u128,
            outer_lower_sqrt_price_nad: 10 * NAD as u128,
            outer_upper_sqrt_price_nad: 20 * NAD as u128,
        }
        .geometry()
        .unwrap(),
        geometry()
    );
}
#[test]
fn parameter_share_uses_existing_amplification_bound() {
    let max_amplification = 2_000 * NAD;
    ConcentratedCurveParameters {
        peak_amplification_nad: max_amplification,
        core_half_width_bps: 1,
        fade_width_bps: 0,
    }
    .validate(max_amplification)
    .unwrap();
    assert!(ConcentratedCurveParameters {
        peak_amplification_nad: max_amplification + 1,
        core_half_width_bps: 1,
        fade_width_bps: 0,
    }
    .validate(max_amplification)
    .is_err());

    let bounds = ConcentratedCurveParameters {
        peak_amplification_nad: 2 * NAD,
        core_half_width_bps: 10_000,
        fade_width_bps: 0,
    }
    .price_bounds_nad(200 * NAD)
    .unwrap()
    .unwrap();
    assert_eq!(bounds, (100 * NAD as u128, 400 * NAD as u128));
}

#[test]
fn centered_geometry_uses_closed_form_positive_liquidity() {
    let base = 1_000_000_000_000_u128;
    let quote = base * 200;
    let parameters = ConcentratedCurveParameters {
        peak_amplification_nad: 2 * NAD,
        core_half_width_bps: 10_000,
        fade_width_bps: 0,
    };
    let curve = prepare_centered_concentrated_geometry(base, quote, 200 * NAD, parameters).unwrap();
    let cache = prepare_centered_concentrated_cache(base, quote, 200 * NAD, parameters).unwrap();
    assert_eq!(cache.parameters(), parameters);
    assert_eq!(cache.geometry().unwrap(), curve);
    let center = cache.center_point(200 * NAD).unwrap();
    assert!(
        center.base_reserve.abs_diff(base) <= base / 1_000_000,
        "base={} expected={base}",
        center.base_reserve
    );
    assert!(
        center.quote_reserve.abs_diff(quote) <= quote / 1_000_000,
        "quote={} expected={quote}",
        center.quote_reserve
    );
    let (lower, upper) = curve.range_prices_nad().unwrap().unwrap();
    assert!(lower.abs_diff(100 * NAD as u128) <= 1_000, "lower={lower}");
    assert!(upper.abs_diff(400 * NAD as u128) <= 1_000, "upper={upper}");
    assert_eq!(curve.branch(ConcentratedCurvePoint { base_reserve: base, quote_reserve: quote }), ConcentratedCurveBranch::Inner);
}

#[test]
fn arbitrary_point_cache_reconstructs_each_branch_without_a_root_search() {
    let parameters = ConcentratedCurveParameters {
        peak_amplification_nad: 2 * NAD,
        core_half_width_bps: 10_000,
        fade_width_bps: 0,
    };
    let centered = prepare_centered_concentrated_cache(
        1_000_000_000_000,
        200_000_000_000_000,
        200 * NAD,
        parameters,
    )
    .unwrap();
    let geometry = centered.geometry().unwrap();
    let points = [
        geometry.point_at_price_nad(50 * NAD as u128, centered.tail_liquidity).unwrap(),
        geometry.point_at_price_nad(200 * NAD as u128, centered.tail_liquidity).unwrap(),
        geometry.point_at_price_nad(800 * NAD as u128, centered.tail_liquidity).unwrap(),
    ];
    for point in points {
        let rebuilt = prepare_concentrated_cache_at_point(
            point.base_reserve,
            point.quote_reserve,
            200 * NAD,
            parameters,
        )
        .unwrap();
        let rebuilt_geometry = rebuilt.geometry().unwrap();
        assert_eq!(rebuilt_geometry.branch(point), geometry.branch(point));
        assert!(
            rebuilt.tail_liquidity.abs_diff(centered.tail_liquidity) <= 64,
            "point={point:?} centered={centered:?} rebuilt={rebuilt:?}"
        );
        assert!(
            rebuilt.concentrated_liquidity.abs_diff(centered.concentrated_liquidity) <= 64,
            "point={point:?} centered={centered:?} rebuilt={rebuilt:?}"
        );
    }
}

#[test]
fn reserve_ratio_selects_the_exact_rebase_branch_before_solving() {
    for parameters in [
        ConcentratedCurveParameters {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 0,
        },
        ConcentratedCurveParameters {
            peak_amplification_nad: 20 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
        },
    ] {
        let centered = prepare_centered_concentrated_cache(
            1_000_000_000_000,
            1_000_000_000_000,
            NAD,
            parameters,
        )
        .unwrap();
        let shape = centered.geometry().unwrap();
        for target in [
            800_000_000_u128,
            950_000_000,
            990_000_000,
            NAD as u128,
            1_008_000_000,
            1_048_000_000,
            1_250_000_000,
        ] {
            let curve_point = shape.point_at_price_nad(target, centered.tail_liquidity).unwrap();
            for (base_fee, quote_fee) in [(1_u128, 0_u128), (0, 1), (100_000_000, 0), (0, 100_000_000)] {
                let point = ConcentratedCurvePoint {
                    base_reserve: curve_point.base_reserve.checked_add(base_fee).unwrap(),
                    quote_reserve: curve_point.quote_reserve.checked_add(quote_fee).unwrap(),
                };
                let selected = if shape.is_nested() {
                    let region = shape
                        .nested_boundaries
                        .iter()
                        .take(shape.nested_boundary_count as usize)
                        .position(|boundary| {
                            reserve_ratio_cmp(point, *boundary) != core::cmp::Ordering::Less
                        })
                        .unwrap_or(shape.nested_boundary_count as usize);
                    nested_branch_from_region(region)
                } else if reserve_ratio_cmp(point, shape.upper_boundary) != core::cmp::Ordering::Less {
                    ConcentratedCurveBranch::UpperTail
                } else if reserve_ratio_cmp(point, shape.lower_boundary) == core::cmp::Ordering::Greater {
                    ConcentratedCurveBranch::Inner
                } else {
                    ConcentratedCurveBranch::LowerTail
                };
                let rebuilt = prepare_concentrated_cache_at_point(
                    point.base_reserve,
                    point.quote_reserve,
                    NAD,
                    parameters,
                )
                .unwrap();
                assert_eq!(
                    rebuilt.geometry().unwrap().branch(point),
                    selected,
                    "parameters={parameters:?} target={target} base_fee={base_fee} quote_fee={quote_fee}"
                );
            }
        }
    }
}

#[test]
fn cpmm_mode_matches_cpmm_helpers_exactly() {
    let start = ConcentratedCurvePoint {
        base_reserve: 1_000_000,
        quote_reserve: 2_000_000,
    };
    let amount_in = 10_000;
    let quote = ConcentratedCurveGeometry::cpmm()
        .quote_exact_in(start, amount_in, ConcentratedCurveDirection::BaseToQuote)
        .unwrap();
    assert_eq!(
        quote.amount_out,
        cpmm_amount_out_nad(start.base_reserve, start.quote_reserve, amount_in).unwrap()
    );
    assert_eq!(quote.boundary_crossings, 0);
    assert_eq!(ConcentratedCurveGeometry::cpmm().range_prices_nad().unwrap(), None);
}

#[test]
fn range_and_spot_prices_are_direct_reserve_ratios() {
    let curve = geometry();
    let (lower, upper) = curve.range_prices_nad().unwrap().unwrap();
    assert_eq!(lower, 100 * NAD as u128);
    assert_eq!(upper, 400 * NAD as u128);
    assert_eq!(
        curve
            .spot_price_nad(ConcentratedCurvePoint {
                base_reserve: 200,
                quote_reserve: 22_500,
            })
            .unwrap(),
        156_250_000_000
    );
}

#[test]
fn reserve_at_price_inverse_covers_band_and_both_tails() {
    let parameters = ConcentratedCurveParameters {
        peak_amplification_nad: 2 * NAD,
        core_half_width_bps: 10_000,
        fade_width_bps: 0,
    };
    let cache = prepare_centered_concentrated_cache(
        1_000_000_000_000,
        200_000_000_000_000,
        200 * NAD,
        parameters,
    )
    .unwrap();
    let curve = cache.geometry().unwrap();
    for target in [50 * NAD as u128, 200 * NAD as u128, 800 * NAD as u128] {
        let point = curve.point_at_price_nad(target, cache.tail_liquidity).unwrap();
        let observed = curve.spot_price_nad(point).unwrap();
        assert!(observed.abs_diff(target) <= 2_000, "target={target} observed={observed}");
    }
}

#[test]
fn exact_in_crosses_both_boundaries_in_each_direction() {
    let curve = geometry();
    curve.validate().unwrap();

    let upper = ConcentratedCurvePoint {
        base_reserve: 40,
        quote_reserve: 65_000,
    };
    let down = curve
        .quote_exact_in(upper, 400, ConcentratedCurveDirection::BaseToQuote)
        .unwrap();
    assert_eq!(down.boundary_crossings, 2);
    assert_eq!(down.end_branch, ConcentratedCurveBranch::LowerTail);

    let up = curve
        .quote_exact_in(down.end, down.amount_out, ConcentratedCurveDirection::QuoteToBase)
        .unwrap();
    assert_eq!(up.boundary_crossings, 2);
    assert!(up.amount_out <= 400);
}

#[test]
fn exact_out_replays_with_no_less_than_requested_output() {
    let curve = geometry();
    let start = ConcentratedCurvePoint {
        base_reserve: 200,
        quote_reserve: 22_500,
    };
    let desired = 7_000;
    let exact_out = curve
        .quote_exact_out(start, desired, ConcentratedCurveDirection::BaseToQuote)
        .unwrap();
    let replay = curve
        .quote_exact_in(start, exact_out.amount_in, ConcentratedCurveDirection::BaseToQuote)
        .unwrap();
    assert!(replay.amount_out >= desired);
    let predecessor = curve
        .quote_exact_in(
            start,
            exact_out.amount_in - 1,
            ConcentratedCurveDirection::BaseToQuote,
        )
        .unwrap();
    assert!(predecessor.amount_out < desired);
}

#[test]
fn product_parameters_build_and_reconstruct_the_nested_shoulder() {
    let parameters = ConcentratedCurveParameters {
        peak_amplification_nad: 20 * NAD,
        core_half_width_bps: 100,
        fade_width_bps: 400,
    };
    let centered = prepare_centered_concentrated_cache(
        1_000_000_000_000,
        1_000_000_000_000,
        NAD,
        parameters,
    )
    .unwrap();
    let geometry = centered.geometry().unwrap();
    assert!(geometry.is_nested());
    assert_eq!(geometry.nested_boundary_count, 4);
    let center = centered.center_point(NAD).unwrap();
    assert!(center.base_reserve.abs_diff(1_000_000_000_000) <= 50_000);
    assert!(center.quote_reserve.abs_diff(1_000_000_000_000) <= 50_000);
    let peak = centered
        .tail_liquidity
        .checked_add(centered.concentrated_liquidity)
        .unwrap();
    assert!(peak.abs_diff(20_000_000_000_000) <= 1_000_000);

    for target in [
        900_000_000_u128,
        970_000_000,
        NAD as u128,
        1_020_000_000,
        1_100_000_000,
    ] {
        let point = geometry.point_at_price_nad(target, centered.tail_liquidity).unwrap();
        let rebuilt = prepare_concentrated_cache_at_point(point.base_reserve, point.quote_reserve, NAD, parameters).unwrap();
        assert_eq!(rebuilt.geometry().unwrap().branch(point), geometry.branch(point));
        assert!(
            rebuilt.tail_liquidity.abs_diff(centered.tail_liquidity) <= 100_000,
            "target={target} centered={centered:?} rebuilt={rebuilt:?}"
        );
    }

    let high = geometry
        .point_at_price_nad(1_100_000_000, centered.tail_liquidity)
        .unwrap();
    let amount_in = geometry.nested_boundaries[3]
        .base_reserve
        .checked_sub(high.base_reserve)
        .unwrap()
        .checked_add(center.base_reserve / 10)
        .unwrap();
    let quote = geometry
        .quote_exact_in(high, amount_in, ConcentratedCurveDirection::BaseToQuote)
        .unwrap();
    assert_eq!(quote.boundary_crossings, 4);
    assert_eq!(quote.end_branch, ConcentratedCurveBranch::LowerTail);

    let requested_out = quote.amount_out - 1;
    let exact_out = geometry
        .quote_exact_out(high, requested_out, ConcentratedCurveDirection::BaseToQuote)
        .unwrap();
    assert_eq!(exact_out.boundary_crossings, 4);
    let replay = geometry
        .quote_exact_in(high, exact_out.amount_in, ConcentratedCurveDirection::BaseToQuote)
        .unwrap();
    assert!(replay.amount_out >= requested_out);
    let predecessor = geometry
        .quote_exact_in(high, exact_out.amount_in - 1, ConcentratedCurveDirection::BaseToQuote)
        .unwrap();
    assert!(predecessor.amount_out < requested_out);

    let reverse = geometry
        .quote_exact_in(quote.end, quote.amount_out, ConcentratedCurveDirection::QuoteToBase)
        .unwrap();
    assert_eq!(reverse.boundary_crossings, 4);
    assert_eq!(reverse.end_branch, ConcentratedCurveBranch::UpperTail);
    assert!(reverse.amount_out <= amount_in);
}


#[test]
fn cached_liquidation_quotes_match_canonical_rounding_across_every_region() {
    // Small integer reserves exercise exact boundaries and the atoms on
    // either side, where canonical crossing dust matters most.
    let small = geometry();
    let start = ConcentratedCurvePoint { base_reserve: 100, quote_reserve: 30_000 };
    for direction in [ConcentratedCurveDirection::BaseToQuote, ConcentratedCurveDirection::QuoteToBase] {
        let path = small.prepare_quote_path(start, direction).unwrap();
        for input in 1..=35_001 {
            match (small.quote_exact_in(start, input, direction), path.amount_out(input)) {
                (Ok(q), Ok(output)) => assert_eq!(q.amount_out, output, "small direction={direction:?} input={input}"),
                (Err(_), Err(_)) => (),
                (a, b) => panic!("small quote/cache disagreement: {a:?} {b:?}"),
            }
        }
    }
    for shoulder in [0, 500] {
        for scale in [1_u128, 1_000, 1_000_000] {
            let base = 1_000_000_000_000 * scale;
            let quote = 2 * base;
            let curve = prepare_centered_concentrated_geometry(base, quote, 2 * NAD,
                ConcentratedCurveParameters { peak_amplification_nad: 3 * NAD,
                    core_half_width_bps: 250, fade_width_bps: shoulder }).unwrap();
            let center = ConcentratedCurvePoint { base_reserve: base, quote_reserve: quote };
            for direction in [ConcentratedCurveDirection::BaseToQuote, ConcentratedCurveDirection::QuoteToBase] {
                // Include shifted starts with integer reserve dust, rather than
                // assuming each boundary's ideal invariant point was reached.
                for shift in [0, base / 20, base] {
                    let start = if shift == 0 { center } else {
                        curve.quote_exact_in(center, shift, direction).unwrap().end
                    };
                    for quote_direction in [ConcentratedCurveDirection::BaseToQuote, ConcentratedCurveDirection::QuoteToBase] {
                        let path = curve.prepare_quote_path(start, quote_direction).unwrap();
                        for input in [1, 10, base / 100, base / 20, base / 10, base, base * 2] {
                            let expected = curve.quote_exact_in(start, input, quote_direction);
                            let actual = path.amount_out(input);
                            match (expected, actual) {
                                (Ok(q), Ok(output)) => assert_eq!(q.amount_out, output, "shoulder={shoulder} scale={scale} input={input}"),
                                (Err(_), Err(_)) => (),
                                (a, b) => panic!("quote/cache disagreement: {a:?} {b:?}"),
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn small_nested_deposits_allow_only_reserve_rounding_at_boundaries() {
    let parameters = ConcentratedCurveParameters {
        peak_amplification_nad: 3 * NAD,
        core_half_width_bps: 250,
        fade_width_bps: 500,
    };
    // Devnet pool 2qY1kxeJNHRMhYLZ72VkBSTcMYLNPyMmTUFcuz39hzzx:
    // 0.01 base (6 decimals) and 0.1 quote (9 decimals), normalized to 9.
    let cache = prepare_concentrated_cache_at_point(10_000_000, 100_000_000, 10 * NAD, parameters).unwrap();
    let curve = cache.geometry().unwrap();
    let boundary = curve.nested_boundaries[0];
    let left = curve.nested_effective_reserves(boundary, 0).unwrap();
    let right = curve.nested_effective_reserves(boundary, 1).unwrap();
    let left_price = mul_div_u128(left.1, NAD as u128, left.0).unwrap();
    let right_price = mul_div_u128(right.1, NAD as u128, right.0).unwrap();
    assert!(left_price.abs_diff(right_price) > 2, "must exercise the former false rejection");

    // Keep positive reserves and ordered boundaries, but introduce a real
    // price discontinuity in each adjoining region. Rounding cannot hide it.
    for region in 0..5 {
        let mut malformed = curve;
        malformed.nested_quote_constants[region].magnitude += 1_000_000;
        assert!(malformed.validate().is_err(), "region={region}");
    }
}

#[test]
fn small_nested_curves_cross_all_boundaries_without_round_trip_profit() {
    for amplification in [2, 3, 20] {
        for base in [1_000_000_u128, 10_000_000, 100_000_000, 1_000_000_000_000, 10_u128.pow(24)] {
            for price in [NAD / 10, NAD, 10 * NAD, 100 * NAD] {
                let parameters = ConcentratedCurveParameters {
                    peak_amplification_nad: amplification * NAD,
                    core_half_width_bps: 250,
                    fade_width_bps: 500,
                };
                let quote = mul_div_u128(base, price as u128, NAD as u128).unwrap();
                let cache = prepare_concentrated_cache_at_point(base, quote, price, parameters).unwrap();
                let curve = cache.geometry().unwrap();
                for direction in [ConcentratedCurveDirection::BaseToQuote, ConcentratedCurveDirection::QuoteToBase] {
                    let (start_price, target_price, reverse) = match direction {
                        ConcentratedCurveDirection::BaseToQuote => (price * 2, price / 2, ConcentratedCurveDirection::QuoteToBase),
                        ConcentratedCurveDirection::QuoteToBase => (price / 2, price * 2, ConcentratedCurveDirection::BaseToQuote),
                    };
                    let start = curve.point_at_price_nad(start_price as u128, cache.tail_liquidity).unwrap();
                    let target = curve.point_at_price_nad(target_price as u128, cache.tail_liquidity).unwrap();
                    let input = match direction {
                        ConcentratedCurveDirection::BaseToQuote => target.base_reserve - start.base_reserve,
                        ConcentratedCurveDirection::QuoteToBase => target.quote_reserve - start.quote_reserve,
                    };
                    let forward = curve.quote_exact_in(start, input, direction).unwrap();
                    assert_eq!(forward.boundary_crossings, 4);
                    let exact_out = curve.quote_exact_out(start, forward.amount_out, direction).unwrap();
                    let filled = curve.quote_exact_in(start, exact_out.amount_in, direction).unwrap();
                    assert!(filled.amount_out >= forward.amount_out);
                    let predecessor = curve.quote_exact_in(start, exact_out.amount_in - 1, direction).unwrap();
                    assert!(predecessor.amount_out < forward.amount_out);
                    let back = curve.quote_exact_in(forward.end, forward.amount_out, reverse).unwrap();
                    assert_eq!(back.boundary_crossings, 4);
                    assert!(back.amount_out <= input,
                        "amp={amplification} base={base} price={price} direction={direction:?}: {input} -> {}", back.amount_out);
                }
            }
        }
    }
}

// Nested boundary validation accepts the rounding left by construction and
// nothing more. Each boundary reserve sums one floored term per active layer
// (tail, shoulder, core), so with k active layers the exact value lies in
// [v, v + k). These properties pin that bound for every feasible curve shape
// and size, so a construction change that rounds differently fails here
// before it can reject a live market.
const NESTED_REGION_LAYERS: [usize; 5] = [1, 2, 3, 2, 1];

fn feasible_nested_parameters() -> impl Strategy<Value = ConcentratedCurveParameters> {
    (1_u16..=5_000, 1_u16..=10_000, 1_u32..1_000_000).prop_filter_map(
        "concentration must leave a nonzero tail",
        |(core_half_width_bps, fade_width_bps, fraction)| {
            // Peak amplification A is feasible while 1 - 1/A stays below the
            // mean inverse square-root width; sample A inside that bound.
            let inverse_root = |bps: f64| (1.0 + bps / 10_000.0).sqrt().recip();
            let mean_inverse = (inverse_root(f64::from(core_half_width_bps))
                + inverse_root(f64::from(core_half_width_bps) + f64::from(fade_width_bps)))
                / 2.0;
            let max_amplification = (1.0 - mean_inverse).recip().min(2_000.0);
            let amplification = 1.0 + f64::from(fraction) / 1_000_000.0 * (max_amplification - 1.0);
            let parameters = ConcentratedCurveParameters {
                peak_amplification_nad: (amplification * NAD as f64) as u64,
                core_half_width_bps,
                fade_width_bps,
            };
            parameters
                .validate(crate::state::MAX_AMM_AMPLIFICATION_NAD)
                .ok()
                .map(|()| parameters)
        },
    )
}

/// Feasible parameters, a center price from 10^-6 to about 10^6 quote per
/// base, and a reserve point at 0.6x to 1.6x that price whose smaller side
/// holds 10^5 to 10^24 normalized atoms. Smaller pools may legitimately
/// collapse and are generated separately.
fn nested_curve_inputs() -> impl Strategy<Value = (ConcentratedCurveParameters, u128, u128, u64)> {
    nested_curve_inputs_with_size_exponents(5..=23)
}

/// As `nested_curve_inputs`, with the smaller side holding 10^e to 10^(e+1)
/// atoms for each exponent e in the range.
fn nested_curve_inputs_with_size_exponents(
    size_exponents: core::ops::RangeInclusive<u32>,
) -> impl Strategy<Value = (ConcentratedCurveParameters, u128, u128, u64)> {
    (
        feasible_nested_parameters(),
        0_u32..=12,
        0_u128..1_000_000,
        size_exponents,
        0_u128..1_000_000,
        60_u128..=160,
    )
        .prop_map(
            |(parameters, price_exponent, price_jitter, size_exponent, size_jitter, offset_percent)| {
                let price_floor = NAD as u128 * 10_u128.pow(price_exponent) / 1_000_000;
                let center_price_nad = u64::try_from(price_floor + price_floor * price_jitter / 2_000_000).unwrap();
                let smaller_side = 10_u128.pow(size_exponent) * (1_000_000 + 9 * size_jitter) / 1_000_000;
                let point_price_nad = u128::from(center_price_nad) * offset_percent / 100;
                let (base, quote) = if point_price_nad >= NAD as u128 {
                    (smaller_side, mul_div_u128(smaller_side, point_price_nad, NAD as u128).unwrap())
                } else {
                    (mul_div_u128(smaller_side, NAD as u128, point_price_nad).unwrap(), smaller_side)
                };
                (parameters, base, quote, center_price_nad)
            },
        )
}

#[test]
fn dust_reserves_without_curve_liquidity_report_insufficient_liquidity() {
    // Every candidate branch floors the tail or concentrated liquidity of
    // these reserves to zero. They previously surfaced as BrokenInvariant.
    for (base, quote, center_price_nad, peak_amplification_nad, core_half_width_bps, fade_width_bps) in [
        (3, 3, 1_238_523_730, 1_183_490_052, 3_329, 6_123),
        (1_236, 174, 140_769_679, 1_000_601_010, 1_269, 4_054),
    ] {
        let parameters = ConcentratedCurveParameters {
            peak_amplification_nad,
            core_half_width_bps,
            fade_width_bps,
        };
        let result = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters);
        assert!(
            matches!(
                &result,
                Err(anchor_lang::error::Error::AnchorError(error))
                    if error.error_code_number == u32::from(ErrorCode::InsufficientLiquidity)
            ),
            "{result:?}"
        );
    }
}

#[test]
fn points_between_neighbouring_branch_solutions_reconstruct() {
    // Each point lies within rounding of a nested boundary, where the two
    // neighbouring branch solutions place it in each other's region. These
    // previously failed with BrokenInvariant, from dust to 10^24 atoms.
    let mut round_trips = 0;
    for (base, quote, center_price_nad, peak_amplification_nad, core_half_width_bps, fade_width_bps) in [
        (4, 694, 121_328_700_000, 2_276_128_474, 2_641, 6_132),
        (
            1_275_252_399_575_852,
            377_885_106_309_528_157,
            510_950_000_000,
            4_834_968_260,
            1_171,
            4_141,
        ),
        (
            1_261_115_701_243_678_306,
            492_217_401_632_643_200_888,
            628_290_000_000,
            1_269_688_543,
            4_536,
            1_817,
        ),
        (
            1_598_602_032_968_647_111_992_730,
            287_453_245_656_189_156_793_041_298,
            491_100_000_000,
            1_454_206_388,
            3_552,
            8_197,
        ),
    ] {
        let parameters = ConcentratedCurveParameters {
            peak_amplification_nad,
            core_half_width_bps,
            fade_width_bps,
        };
        let (cache, curve) =
            prepare_concentrated_cache_and_geometry_at_point(base, quote, center_price_nad, parameters).unwrap();
        assert_eq!(cache.geometry().unwrap(), curve);
        let point = ConcentratedCurvePoint {
            base_reserve: base,
            quote_reserve: quote,
        };
        curve.spot_price_nad(point).unwrap();
        // Dust outputs round to zero; round trips need a real pool.
        if base.min(quote) < 1_000_000 {
            continue;
        }
        for (direction, reverse, input_reserve) in [
            (ConcentratedCurveDirection::BaseToQuote, ConcentratedCurveDirection::QuoteToBase, base),
            (ConcentratedCurveDirection::QuoteToBase, ConcentratedCurveDirection::BaseToQuote, quote),
        ] {
            for input_ppm in [1_000, 100_000] {
                let input = mul_div_u128(input_reserve, input_ppm, 1_000_000).unwrap() + 1;
                let Ok(forward) = curve.quote_exact_in(point, input, direction) else {
                    continue;
                };
                prepare_concentrated_cache_at_point(
                    forward.end.base_reserve,
                    forward.end.quote_reserve,
                    center_price_nad,
                    parameters,
                )
                .unwrap();
                let back = curve.quote_exact_in(forward.end, forward.amount_out, reverse).unwrap();
                assert!(back.amount_out <= input, "{direction:?}: {input} -> {}", back.amount_out);
                round_trips += 1;
            }
        }
    }
    assert!(round_trips >= 8, "{round_trips}");
}

/// A point `offset` atoms of base along the boundary's reserve ratio, if both
/// reserves stay positive. Above 10^18 atoms the offset grows with the pool,
/// as the rounding band does.
fn point_near_boundary(boundary: ConcentratedCurvePoint, offset: i128) -> Option<ConcentratedCurvePoint> {
    let scale = (boundary.base_reserve.min(boundary.quote_reserve) / 10_u128.pow(18)).max(1);
    let base_step = offset.unsigned_abs() * scale;
    let quote_step = mul_div_u128(base_step, boundary.quote_reserve, boundary.base_reserve).unwrap();
    let point = if offset < 0 {
        ConcentratedCurvePoint {
            base_reserve: boundary.base_reserve.checked_sub(base_step)?,
            quote_reserve: boundary.quote_reserve + quote_step,
        }
    } else {
        ConcentratedCurvePoint {
            base_reserve: boundary.base_reserve + base_step,
            quote_reserve: boundary.quote_reserve.checked_sub(quote_step)?,
        }
    };
    (point.base_reserve > 0 && point.quote_reserve > 0).then_some(point)
}

fn is_error(result: &Result<impl core::fmt::Debug>, code: ErrorCode) -> bool {
    matches!(
        result,
        Err(anchor_lang::error::Error::AnchorError(error)) if error.error_code_number == u32::from(code)
    )
}

/// Active layer liquidities per region, outermost upper region first, in the
/// same order as `nested_boundaries`.
fn nested_region_liquidities(cache: ConcentratedCurveCache) -> [Vec<u128>; 5] {
    let core = cache.concentrated_liquidity.div_ceil(2);
    let shoulder = cache.concentrated_liquidity - core;
    let tail = cache.tail_liquidity;
    [
        vec![tail],
        vec![tail, shoulder],
        vec![tail, shoulder, core],
        vec![tail, shoulder],
        vec![tail],
    ]
}

/// How far a point lies from its own region's invariant, in atoms of the
/// larger effective reserve: |x * y - L^2| / max(x, y).
fn own_region_offset_atoms(
    cache: ConcentratedCurveCache,
    curve: ConcentratedCurveGeometry,
    point: ConcentratedCurvePoint,
) -> BigUint {
    let region = nested_region_from_branch(curve.branch(point));
    let (base, quote) = curve.nested_effective_reserves(point, region).unwrap();
    let liquidity: u128 = nested_region_liquidities(cache)[region].iter().sum();
    let product = BigUint::from(base) * BigUint::from(quote);
    let square = BigUint::from(liquidity) * BigUint::from(liquidity);
    let gap = if product >= square { product - square } else { square - product };
    gap / BigUint::from(base.max(quote))
}

/// Rounding band of a reconstructed point: a few floored atoms per active
/// layer, scaled up with the pool above the 10^18 solve shape.
fn reconstruction_band_atoms(cache: ConcentratedCurveCache) -> BigUint {
    let liquidity = cache.tail_liquidity + cache.concentrated_liquidity;
    BigUint::from(8_u8) * (BigUint::from(1_u8) + BigUint::from(liquidity / NESTED_SHAPE_SCALE))
}

/// Input that takes `start` exactly onto the next boundary in `direction`.
fn input_to_next_boundary(
    curve: ConcentratedCurveGeometry,
    start: ConcentratedCurvePoint,
    direction: ConcentratedCurveDirection,
) -> Option<u128> {
    let branch = curve.branch_for_direction(start, direction);
    let target = curve.next_boundary(start, branch, direction)?;
    let distance = match direction {
        ConcentratedCurveDirection::BaseToQuote => target.base_reserve.saturating_sub(start.base_reserve),
        ConcentratedCurveDirection::QuoteToBase => target.quote_reserve.saturating_sub(start.quote_reserve),
    };
    (distance > 0).then_some(distance)
}

#[test]
fn drained_upper_tail_round_trip_returns_at_most_its_input() {
    // The upper tail of this 10^18-atom pool holds 2.5e-5 of its liquidity.
    // The start sits on the upper-tail boundary and the forward swap drains
    // 99.5% of base. When the root solve halved the concentrated share in
    // NAD units, the start lay 3.6e9 quote atoms off its own region's curve
    // and the round trip returned 2 atoms more than its input.
    let parameters = ConcentratedCurveParameters {
        peak_amplification_nad: 6_326_237_625,
        core_half_width_bps: 1_648,
        fade_width_bps: 5_787,
    };
    let start = ConcentratedCurvePoint {
        base_reserve: 2_954_857_125_079_159_534,
        quote_reserve: 6_351_750_284_696_776_765,
    };
    let (cache, curve) =
        prepare_concentrated_cache_and_geometry_at_point(start.base_reserve, start.quote_reserve, 113_599, parameters)
            .unwrap();
    assert!(own_region_offset_atoms(cache, curve, start) <= reconstruction_band_atoms(cache));
    let input = 114_503_002_382_228_795;
    let forward = curve
        .quote_exact_in(start, input, ConcentratedCurveDirection::QuoteToBase)
        .unwrap();
    assert!(forward.amount_out * 100 > start.base_reserve * 99);
    let back = curve
        .quote_exact_in(forward.end, forward.amount_out, ConcentratedCurveDirection::BaseToQuote)
        .unwrap();
    assert!(back.amount_out <= input, "{input} -> {}", back.amount_out);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn feasible_nested_curves_pass_boundary_validation(
        (parameters, base, quote, center_price_nad) in nested_curve_inputs()
    ) {
        let prepared = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters);
        prop_assert!(prepared.is_ok(), "{parameters:?} base={base} quote={quote} center={center_price_nad}: {prepared:?}");
    }

    #[test]
    fn nested_boundary_rounding_stays_below_one_atom_per_active_layer(
        (parameters, base, quote, center_price_nad) in nested_curve_inputs()
    ) {
        let cache = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters).unwrap();
        let curve = cache.geometry().unwrap();
        let regions = nested_region_liquidities(cache);
        let sqrt_prices = [
            cache.outer_upper_sqrt_price_nad,
            cache.core_upper_sqrt_price_nad,
            cache.core_lower_sqrt_price_nad,
            cache.outer_lower_sqrt_price_nad,
        ];
        let scale = BigUint::from(NAD);
        for (boundary_index, boundary) in curve.nested_boundaries.into_iter().enumerate() {
            let sqrt_price = BigUint::from(sqrt_prices[boundary_index]);
            for region in [boundary_index, boundary_index + 1] {
                prop_assert_eq!(regions[region].len(), NESTED_REGION_LAYERS[region]);
                let layers = BigUint::from(regions[region].len());
                let liquidity: BigUint = regions[region].iter().map(|&value| BigUint::from(value)).sum();
                let (effective_base, effective_quote) = curve.nested_effective_reserves(boundary, region).unwrap();
                // Exact base is liquidity * NAD / sqrt_price; exact quote is
                // liquidity * sqrt_price / NAD. Both must lie in [v, v + k).
                let base_numerator = &liquidity * &scale;
                let effective_base = BigUint::from(effective_base);
                prop_assert!(&effective_base * &sqrt_price <= base_numerator);
                prop_assert!(base_numerator < (&effective_base + &layers) * &sqrt_price);
                let quote_numerator = &liquidity * &sqrt_price;
                let effective_quote = BigUint::from(effective_quote);
                prop_assert!(&effective_quote * &scale <= quote_numerator);
                prop_assert!(quote_numerator < (&effective_quote + &layers) * &scale);
            }
        }
    }

    #[test]
    fn nested_boundary_validation_rejects_one_percent_price_jumps(
        (parameters, base, quote, center_price_nad) in nested_curve_inputs()
    ) {
        let curve = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters)
            .unwrap()
            .geometry()
            .unwrap();
        for region in 0_usize..5 {
            let adjoining = [region.checked_sub(1), (region < 4).then_some(region)];
            let mut smallest = u128::MAX;
            let mut region_quote = 0;
            for boundary_index in adjoining.into_iter().flatten() {
                let boundary = curve.nested_boundaries[boundary_index];
                for side in [boundary_index, boundary_index + 1] {
                    let (effective_base, effective_quote) = curve.nested_effective_reserves(boundary, side).unwrap();
                    smallest = smallest.min(effective_base).min(effective_quote);
                    if side == region {
                        region_quote = effective_quote;
                    }
                }
            }
            // Below 10^4 atoms a 1% move is within a few atoms of rounding.
            if smallest < 10_000 {
                continue;
            }
            let mut malformed = curve;
            malformed.nested_quote_constants[region].magnitude += region_quote / 100;
            prop_assert!(malformed.validate().is_err(), "region={region} {parameters:?} base={base} quote={quote}");
        }
    }
    #[test]
    fn points_near_nested_boundaries_reconstruct(
        (parameters, base, quote, center_price_nad) in nested_curve_inputs(),
        offset in -16_i128..=16,
    ) {
        let curve = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters)
            .unwrap()
            .geometry()
            .unwrap();
        for boundary in curve.nested_boundaries {
            for point in [Some(boundary), point_near_boundary(boundary, offset)].into_iter().flatten() {
                let prepared = prepare_concentrated_cache_and_geometry_at_point(
                    point.base_reserve,
                    point.quote_reserve,
                    center_price_nad,
                    parameters,
                );
                prop_assert!(prepared.is_ok(), "{parameters:?} center={center_price_nad} {point:?}: {prepared:?}");
                let (_, local) = prepared.unwrap();
                prop_assert!(local.spot_price_nad_prevalidated(point).is_ok());
            }
        }
    }

    #[test]
    fn swaps_from_nested_boundary_points_reprepare_at_their_endpoints(
        (parameters, base, quote, center_price_nad) in nested_curve_inputs(),
        boundary_index in 0_usize..4,
        offset in -16_i128..=16,
        input_ppm in 1_u128..=100_000,
    ) {
        let curve = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters)
            .unwrap()
            .geometry()
            .unwrap();
        let Some(start) = point_near_boundary(curve.nested_boundaries[boundary_index], offset) else {
            return Ok(());
        };
        let (_, start_curve) = prepare_concentrated_cache_and_geometry_at_point(
            start.base_reserve,
            start.quote_reserve,
            center_price_nad,
            parameters,
        )
        .unwrap();
        for (direction, input_reserve) in [
            (ConcentratedCurveDirection::BaseToQuote, start.base_reserve),
            (ConcentratedCurveDirection::QuoteToBase, start.quote_reserve),
        ] {
            let input = mul_div_u128(input_reserve, input_ppm, 1_000_000).unwrap() + 1;
            // Outputs that round to zero are rejected before any transition.
            let Ok(forward) = start_curve.quote_exact_in(start, input, direction) else { continue };
            // Every transition re-prepares the curve at its endpoint. A swap
            // can drain one side to dust, which may only fail for its size.
            let end = prepare_concentrated_cache_at_point(
                forward.end.base_reserve,
                forward.end.quote_reserve,
                center_price_nad,
                parameters,
            );
            prop_assert!(
                end.is_ok()
                    || is_error(&end, ErrorCode::InsufficientLiquidity)
                    || is_error(&end, ErrorCode::InvalidMarketConfig),
                "{parameters:?} center={center_price_nad} {direction:?} end={:?}: {end:?}",
                forward.end
            );
        }
    }

    #[test]
    fn prepared_points_lie_on_their_own_region_within_the_rounding_band(
        (parameters, base, quote, center_price_nad) in nested_curve_inputs(),
        offset in -16_i128..=16,
    ) {
        let curve = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters)
            .unwrap()
            .geometry()
            .unwrap();
        let interior = ConcentratedCurvePoint { base_reserve: base, quote_reserve: quote };
        let near_boundaries = curve.nested_boundaries.map(|boundary| point_near_boundary(boundary, offset));
        for point in core::iter::once(Some(interior)).chain(near_boundaries).flatten() {
            let (cache, local) = prepare_concentrated_cache_and_geometry_at_point(
                point.base_reserve,
                point.quote_reserve,
                center_price_nad,
                parameters,
            )
            .unwrap();
            let offset_atoms = own_region_offset_atoms(cache, local, point);
            prop_assert!(
                offset_atoms <= reconstruction_band_atoms(cache),
                "{parameters:?} center={center_price_nad} {point:?}: {offset_atoms} atoms off"
            );
        }
    }

    #[test]
    fn round_trips_from_nested_boundary_points_return_at_most_their_input(
        (parameters, base, quote, center_price_nad) in nested_curve_inputs(),
        boundary_index in 0_usize..4,
        offset in -16_i128..=16,
        input_ppm in 1_u128..=100_000,
    ) {
        let curve = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters)
            .unwrap()
            .geometry()
            .unwrap();
        let Some(start) = point_near_boundary(curve.nested_boundaries[boundary_index], offset) else {
            return Ok(());
        };
        let (_, start_curve) = prepare_concentrated_cache_and_geometry_at_point(
            start.base_reserve,
            start.quote_reserve,
            center_price_nad,
            parameters,
        )
        .unwrap();
        for (direction, reverse, input_reserve) in [
            (ConcentratedCurveDirection::BaseToQuote, ConcentratedCurveDirection::QuoteToBase, start.base_reserve),
            (ConcentratedCurveDirection::QuoteToBase, ConcentratedCurveDirection::BaseToQuote, start.quote_reserve),
        ] {
            let input = mul_div_u128(input_reserve, input_ppm, 1_000_000).unwrap() + 1;
            let Ok(forward) = start_curve.quote_exact_in(start, input, direction) else { continue };
            // A return leg can still pay nothing when the forward output is dust.
            let Ok(back) = start_curve.quote_exact_in(forward.end, forward.amount_out, reverse) else { continue };
            prop_assert!(
                back.amount_out <= input,
                "{parameters:?} center={center_price_nad} {start:?} {direction:?}: {input} -> {} -> {}",
                forward.amount_out,
                back.amount_out
            );
        }
    }

    #[test]
    fn exact_in_output_never_falls_past_a_nested_boundary(
        (parameters, base, quote, center_price_nad) in nested_curve_inputs(),
        boundary_index in 0_usize..4,
        offset in -16_i128..=16,
        extra_exponent in 0_u32..=7,
        extra_digit in 1_u128..=9,
    ) {
        let curve = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters)
            .unwrap()
            .geometry()
            .unwrap();
        let Some(start) = point_near_boundary(curve.nested_boundaries[boundary_index], offset) else {
            return Ok(());
        };
        let (_, start_curve) = prepare_concentrated_cache_and_geometry_at_point(
            start.base_reserve,
            start.quote_reserve,
            center_price_nad,
            parameters,
        )
        .unwrap();
        for direction in [ConcentratedCurveDirection::BaseToQuote, ConcentratedCurveDirection::QuoteToBase] {
            let Some(crossing) = input_to_next_boundary(start_curve, start, direction) else { continue };
            let Ok(at_boundary) = start_curve.quote_exact_in(start, crossing, direction) else { continue };
            if at_boundary.amount_out == 0 {
                continue;
            }
            // A few atoms past the boundary can floor the last segment to
            // zero. The swap must still pay what the crossing paid.
            let input = crossing + extra_digit * 10_u128.pow(extra_exponent);
            let past = start_curve.quote_exact_in(start, input, direction);
            prop_assert!(
                matches!(&past, Ok(quote) if quote.amount_out >= at_boundary.amount_out),
                "{parameters:?} center={center_price_nad} {start:?} {direction:?} crossing={crossing} input={input}: {past:?}"
            );
            let path = start_curve.prepare_quote_path(start, direction).unwrap();
            prop_assert_eq!(path.amount_out(input).unwrap(), past.unwrap().amount_out);
        }
    }

    #[test]
    fn dust_nested_curves_fail_only_with_size_errors(
        (parameters, base, quote, center_price_nad) in nested_curve_inputs_with_size_exponents(0..=4)
    ) {
        let prepared = prepare_concentrated_cache_at_point(base, quote, center_price_nad, parameters);
        prop_assert!(
            prepared.is_ok()
                || is_error(&prepared, ErrorCode::InsufficientLiquidity)
                || is_error(&prepared, ErrorCode::InvalidMarketConfig),
            "{parameters:?} base={base} quote={quote} center={center_price_nad}: {prepared:?}"
        );
    }
}

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

use super::*;

fn geometry() -> ExplicitCurveGeometry {
    // Tail liquidity 1,000 and inner liquidity 4,000 over sqrt prices 10..20.
    ExplicitCurveGeometry {
        inner_liquidity: 4_000,
        inner_base_amplification_offset: 200,
        inner_quote_amplification_offset: 40_000,
        lower_tail_base_inventory: 200,
        upper_tail_quote_inventory: 40_000,
        lower_boundary: ExplicitCurvePoint {
            base_reserve: 300,
            quote_reserve: 10_000,
        },
        upper_boundary: ExplicitCurvePoint {
            base_reserve: 50,
            quote_reserve: 60_000,
        },
    }
}

#[test]
fn liquidity_range_constructor_matches_closed_form_geometry() {
    assert_eq!(
        ExplicitCurveGeometry::from_liquidity_range(1_000, 4_000, 10, 20, 1).unwrap(),
        geometry()
    );
}

#[test]
fn parameter_share_uses_existing_amplification_bound() {
    let max_amplification = 2_000 * NAD;
    let maximum_share = mul_div_u128(
        max_amplification as u128,
        NAD as u128,
        max_amplification as u128 + NAD as u128,
    )
    .unwrap() as u64;
    ExplicitCurveParameters {
        range_width_nad: 2 * NAD,
        concentrated_liquidity_share_nad: maximum_share,
    }
    .validate(max_amplification)
    .unwrap();
    assert!(ExplicitCurveParameters {
        range_width_nad: 2 * NAD,
        concentrated_liquidity_share_nad: maximum_share + 1,
    }
    .validate(max_amplification)
    .is_err());

    let bounds = ExplicitCurveParameters {
        range_width_nad: 2 * NAD,
        concentrated_liquidity_share_nad: NAD / 2,
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
    let parameters = ExplicitCurveParameters {
        range_width_nad: 2 * NAD,
        concentrated_liquidity_share_nad: 800_000_000,
    };
    let curve = prepare_centered_explicit_geometry(base, quote, 200 * NAD, parameters).unwrap();
    let cache = prepare_centered_explicit_cache(base, quote, 200 * NAD, parameters).unwrap();
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
    assert_eq!(curve.branch(ExplicitCurvePoint { base_reserve: base, quote_reserve: quote }), ExplicitCurveBranch::Inner);
}

#[test]
fn arbitrary_point_cache_reconstructs_each_branch_without_a_root_search() {
    let parameters = ExplicitCurveParameters {
        range_width_nad: 2 * NAD,
        concentrated_liquidity_share_nad: 800_000_000,
    };
    let centered = prepare_centered_explicit_cache(
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
        let rebuilt = prepare_explicit_cache_at_point(
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
    let start = ExplicitCurvePoint {
        base_reserve: 1_000_000,
        quote_reserve: 2_000_000,
    };
    let amount_in = 10_000;
    let quote = ExplicitCurveGeometry::cpmm()
        .quote_exact_in(start, amount_in, ExplicitCurveDirection::BaseToQuote)
        .unwrap();
    assert_eq!(
        quote.amount_out,
        cpmm_amount_out_nad(start.base_reserve, start.quote_reserve, amount_in).unwrap()
    );
    assert_eq!(quote.boundary_crossings, 0);
    assert_eq!(ExplicitCurveGeometry::cpmm().range_prices_nad().unwrap(), None);
}

#[test]
fn range_and_spot_prices_are_direct_reserve_ratios() {
    let curve = geometry();
    let (lower, upper) = curve.range_prices_nad().unwrap().unwrap();
    assert_eq!(lower, 100 * NAD as u128);
    assert_eq!(upper, 400 * NAD as u128);
    assert_eq!(
        curve
            .spot_price_nad(ExplicitCurvePoint {
                base_reserve: 200,
                quote_reserve: 22_500,
            })
            .unwrap(),
        156_250_000_000
    );
}

#[test]
fn reserve_at_price_inverse_covers_band_and_both_tails() {
    let parameters = ExplicitCurveParameters {
        range_width_nad: 2 * NAD,
        concentrated_liquidity_share_nad: 800_000_000,
    };
    let cache = prepare_centered_explicit_cache(
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

    let upper = ExplicitCurvePoint {
        base_reserve: 40,
        quote_reserve: 65_000,
    };
    let down = curve
        .quote_exact_in(upper, 400, ExplicitCurveDirection::BaseToQuote)
        .unwrap();
    assert_eq!(down.boundary_crossings, 2);
    assert_eq!(down.end_branch, ExplicitCurveBranch::LowerTail);

    let up = curve
        .quote_exact_in(down.end, down.amount_out, ExplicitCurveDirection::QuoteToBase)
        .unwrap();
    assert_eq!(up.boundary_crossings, 2);
    assert!(up.amount_out <= 400);
}

#[test]
fn exact_out_replays_with_no_less_than_requested_output() {
    let curve = geometry();
    let start = ExplicitCurvePoint {
        base_reserve: 200,
        quote_reserve: 22_500,
    };
    let desired = 7_000;
    let exact_out = curve
        .quote_exact_out(start, desired, ExplicitCurveDirection::BaseToQuote)
        .unwrap();
    let replay = curve
        .quote_exact_in(start, exact_out.amount_in, ExplicitCurveDirection::BaseToQuote)
        .unwrap();
    assert!(replay.amount_out >= desired);
    let predecessor = curve
        .quote_exact_in(
            start,
            exact_out.amount_in - 1,
            ExplicitCurveDirection::BaseToQuote,
        )
        .unwrap();
    assert!(predecessor.amount_out < desired);
}

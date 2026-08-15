use super::*;

#[test]
fn virtual_concentration_and_continuous_hlp_hedge_are_one_pass() {
    let scale = 1_000_000_000_u128;
    let state = IntegratedCurveState {
        ordinary_base: 200 * scale,
        ordinary_quote: 22_500 * scale,
        base_hlp_equity: 20 * scale,
        quote_hlp_equity: 2_250 * scale,
        base_hlp_quote_debt: 2_250 * scale,
        quote_hlp_base_debt: 20 * scale,
    };
    let concentrated = ExplicitCurveGeometry {
        inner_liquidity: 4_000 * scale,
        inner_base_amplification_offset: 200 * scale,
        inner_quote_amplification_offset: 40_000 * scale,
        lower_tail_base_inventory: 200 * scale,
        upper_tail_quote_inventory: 40_000 * scale,
        lower_boundary: ExplicitCurvePoint {
            base_reserve: 300 * scale,
            quote_reserve: 10_000 * scale,
        },
        upper_boundary: ExplicitCurvePoint {
            base_reserve: 50 * scale,
            quote_reserve: 60_000 * scale,
        },
    };
    let amount_in = scale;

    let plain = quote_integrated_exact_in(
        state,
        ExplicitCurveGeometry::cpmm(),
        amount_in,
        IntegratedSwapDirection::BaseToQuote,
    )
    .unwrap();
    let deep = quote_integrated_exact_in(
        state,
        concentrated,
        amount_in,
        IntegratedSwapDirection::BaseToQuote,
    )
    .unwrap();

    assert!(deep.amount_out > plain.amount_out);
    assert_eq!(deep.end.base_hlp_equity, state.base_hlp_equity);
    assert_eq!(deep.end.quote_hlp_equity, state.quote_hlp_equity);

    // Reconstructed opposite debts are the exact floor-valued opposite claims
    // implied by each fixed target equity at the endpoint reserve ratio.
    assert_eq!(
        deep.hlp.base_hlp_quote_debt,
        mul_div_u128(state.base_hlp_equity, deep.end.ordinary_quote, deep.end.ordinary_base,).unwrap()
    );
    assert_eq!(
        deep.hlp.quote_hlp_base_debt,
        mul_div_u128(state.quote_hlp_equity, deep.end.ordinary_base, deep.end.ordinary_quote,).unwrap()
    );

    // The amplified curve is conservative: effective K never decreases after
    // the floor-rounded exact-input output.
    let k0 = state
        .ordinary_base
        .checked_add(concentrated.inner_base_amplification_offset)
        .unwrap()
        .checked_mul(
            state
                .ordinary_quote
                .checked_add(concentrated.inner_quote_amplification_offset)
                .unwrap(),
        )
        .unwrap();
    let k1 = deep
        .end
        .ordinary_base
        .checked_add(concentrated.inner_base_amplification_offset)
        .unwrap()
        .checked_mul(
            deep.end
                .ordinary_quote
                .checked_add(concentrated.inner_quote_amplification_offset)
                .unwrap(),
        )
        .unwrap();
    assert!(k1 >= k0);
}

#[test]
fn total_reserves_decompose_without_counting_hlp_debt_twice() {
    let scale = 1_000_000_000_u128;
    let state = IntegratedCurveState::from_total_reserves(
        240 * scale,
        27_000 * scale,
        20 * scale,
        2_250 * scale,
    )
    .unwrap();

    assert_eq!(state.ordinary_base, 200 * scale);
    assert_eq!(state.ordinary_quote, 22_500 * scale);
    assert_eq!(state.base_hlp_quote_debt, 2_250 * scale);
    assert_eq!(state.quote_hlp_base_debt, 20 * scale);

    let quote = quote_integrated_exact_in(
        state,
        ExplicitCurveGeometry::cpmm(),
        scale,
        IntegratedSwapDirection::BaseToQuote,
    )
    .unwrap();
    assert!(quote.base_hlp_quote_debt_delta < 0);
    assert!(quote.quote_hlp_base_debt_delta > 0);
    assert_eq!(quote.end.base_hlp_quote_debt, quote.hlp.base_hlp_quote_debt);
    assert_eq!(quote.end.quote_hlp_base_debt, quote.hlp.quote_hlp_base_debt);
}

#[test]
fn frozen_gross_coordinate_fee_executes_only_the_net_path() {
    let state = IntegratedCurveState::from_total_reserves(240_000, 27_000_000, 20_000, 2_250_000).unwrap();
    let quote = quote_integrated_exact_in_with_frozen_fee(
        state,
        ExplicitCurveGeometry::cpmm(),
        10_000,
        100,
        IntegratedSwapDirection::BaseToQuote,
    )
    .unwrap();

    assert_eq!(quote.amount_in_after_fee, 9_900);
    assert_eq!(quote.total_fee, 100);
    assert_eq!(quote.executable.curve.amount_in, 9_900);
}

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

#[test]
fn compounded_fee_reconstructs_perfect_hedges_for_cpmm_and_concentration() {
    let state = IntegratedCurveState::from_total_reserves(240_000, 27_000_000, 20_000, 2_250_000).unwrap();
    let concentrated = ExplicitCurveGeometry {
        inner_liquidity: 4_000_000,
        inner_base_amplification_offset: 200_000,
        inner_quote_amplification_offset: 40_000_000,
        lower_tail_base_inventory: 200_000,
        upper_tail_quote_inventory: 40_000_000,
        lower_boundary: ExplicitCurvePoint {
            base_reserve: 300_000,
            quote_reserve: 10_000_000,
        },
        upper_boundary: ExplicitCurvePoint {
            base_reserve: 50_000,
            quote_reserve: 60_000_000,
        },
    };

    for geometry in [ExplicitCurveGeometry::cpmm(), concentrated] {
        let mut quote = quote_integrated_exact_in_with_frozen_fee(
            state,
            geometry,
            10_000,
            100,
            IntegratedSwapDirection::BaseToQuote,
        )
        .unwrap();
        let before = reconstruct_hlp_endpoint(quote.executable.end).unwrap();
        let base_equity_before = quote.executable.end.base_hlp_equity;
        let quote_equity_before = quote.executable.end.quote_hlp_equity;

        apply_compounded_ylp_fee(state, &mut quote, true, 1_000, 1_000, 200, 300).unwrap();

        assert_eq!(
            quote.executable.hlp.total_base - quote.executable.hlp.quote_hlp_base_debt,
            before.total_base - before.quote_hlp_base_debt + 1_000
        );
        assert_eq!(
            quote.executable.hlp.total_quote - quote.executable.hlp.base_hlp_quote_debt,
            before.total_quote - before.base_hlp_quote_debt
        );
        assert_eq!(quote.executable.end.base_hlp_equity, base_equity_before + 200);
        assert!(quote.executable.end.quote_hlp_equity > quote_equity_before);
        assert_eq!(
            quote.executable.hlp.base_hlp_quote_debt,
            mul_div_u128(
                quote.executable.end.base_hlp_equity,
                quote.executable.end.ordinary_quote,
                quote.executable.end.ordinary_base,
            )
            .unwrap()
        );
        assert_eq!(
            quote.executable.hlp.quote_hlp_base_debt,
            mul_div_u128(
                quote.executable.end.quote_hlp_equity,
                quote.executable.end.ordinary_base,
                quote.executable.end.ordinary_quote,
            )
            .unwrap()
        );
    }
}

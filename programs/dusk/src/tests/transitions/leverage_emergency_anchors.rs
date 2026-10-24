// Zero-fee native anchors for the independent economic model. These execute
// existing liquidation accounting; they do not install the candidate policy.
#[test]
fn margin_calibration_native_terminal_anchors() {
    println!("TERMINAL_ANCHOR,price_nad,base_decimals,quote_decimals,debt_side,collateral_reserve,debt_reserve,collateral_sold,debt,output,principal_loss,post_collateral_reserve,post_debt_reserve");
    let mut rows = 0;
    for price_nad in [NAD / 2_500, NAD, NAD * 2_500] {
        for (base_decimals, quote_decimals) in [(6u8, 9u8), (9, 6)] {
            let base_cash = u64::try_from(
                50_000u128 * 10u128.pow(base_decimals.into()) * u128::from(NAD) / u128::from(price_nad),
            )
            .unwrap();
            let quote_cash = 50_000 * 10u64.pow(quote_decimals.into());
            for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
                let mut market = test_market(base_cash, quote_cash);
                market.base_side.asset_decimals = base_decimals;
                market.quote_side.asset_decimals = quote_decimals;
                market.config.swap_fee_bps = 0;
                market.amm = AmmState::default();
                market.risk = Risk::default();
                market.prepare_amm_for_swap(1).unwrap();
                market.refresh_risk().unwrap();
                let collateral_asset = debt_asset.opposite();
                let collateral_reserve = market.curve_reserve(collateral_asset).unwrap();
                let debt_reserve = market.curve_reserve(debt_asset).unwrap();
                let collateral = collateral_reserve * 16 / 100;
                let value = reference_credit(&market, collateral_asset, collateral);
                let debt = value * 95 / 100;
                let mut position = seeded_position(&mut market, debt_asset, debt, collateral);
                let quote = market.quote_leverage_swap(collateral_asset, collateral, 1).unwrap();
                let expected = u128::from(debt_reserve) * u128::from(collateral)
                    / (u128::from(collateral_reserve) + u128::from(collateral));
                // Native curve price normalization may differ by a few atoms.
                assert!(u128::from(quote.amount_out).abs_diff(expected) <= 4);
                let prepared = prepared_leverage_swap(
                    &market,
                    quote,
                    SwapCashPolicy::Liquidate {
                        debt_asset,
                        debt_shares: position.debt_shares,
                        debt_principal: position.debt_principal,
                        insurance_credit: 0,
                    },
                );
                let receipt = market
                    .liquidate_leverage_position(
                        &mut position,
                        Some(prepared),
                        collateral,
                        full_fee_credit(&quote),
                        LeverageInsuranceDraw::default(),
                        0,
                        ProtocolAuctionSplit::default(),
                        1,
                    )
                    .unwrap();
                let loss = debt - quote.amount_out;
                assert_eq!(receipt.socialized_loss, loss);
                assert_eq!(receipt.interest_paid, 0);
                assert_eq!(position.debt_shares, 0);
                let post_collateral = market.curve_reserve(collateral_asset).unwrap();
                let post_debt = market.curve_reserve(debt_asset).unwrap();
                assert_eq!(post_collateral, collateral_reserve + collateral);
                assert_eq!(post_debt, debt_reserve - quote.amount_out - loss);
                market.assert_market_invariants().unwrap();
                println!("TERMINAL_ANCHOR,{price_nad},{base_decimals},{quote_decimals},{},{collateral_reserve},{debt_reserve},{collateral},{debt},{},{loss},{post_collateral},{post_debt}", debt_asset.code(), quote.amount_out);
                rows += 1;
            }
        }
    }
    assert_eq!(rows, 12);
}

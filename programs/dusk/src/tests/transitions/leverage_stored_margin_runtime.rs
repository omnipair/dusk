fn runtime_open(market: &mut Market, debt_asset: MarketAsset, margin: u64, slot: u64) -> LeveragePosition {
    let prepared = SwapRequest {
        current_slot: slot,
        current_unix_timestamp: 0,
        asset_in: debt_asset,
        reserve_credit: margin * 2,
        protocol_fee_bps: 0,
    }
    .prepare_with_cash_policy(
        market,
        SwapCashPolicy::Borrow {
            asset: debt_asset,
            amount: margin,
        },
    )
    .unwrap();
    let quote = prepared.leverage_quote();
    let mut position = empty_position();
    market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            debt_asset,
            margin,
            20_000,
            quote.amount_out,
            prepared,
            full_fee_credit(&quote),
            0,
            slot,
            255,
            0,
            ProtocolAuctionSplit::default(),
            LeverageCollateralFee::default(),
        )
        .unwrap();
    position
}

#[test]
fn runtime_terms_survive_other_traders_and_lp_activity_across_concentrated_hlp_markets() {
    for amp in [1, 4, 10] {
        for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
            for hlp in [false, true] {
                for controller in [false, true] {
                    let mut market = calibration_market(100_000, amp);
                    market.config.ema_half_life_ms = 60_000;
                    market.config.directional_ema_half_life_ms = 60_000;
                    market.config.curve_depth_ema_half_life_ms = 60_000;
                    if controller {
                        market.config.amm.adjustment_threshold_nad = NAD / 100;
                        market.config.amm.adjustment_step_nad = NAD / 1_000;
                        market.config.amm.min_adjustment_interval_slots = 1;
                    }
                    if hlp {
                        market.config.target_hlp_leverage_bps = 20_000;
                        for asset in [MarketAsset::Base, MarketAsset::Quote] {
                            market.deposit_single_sided(asset, 1000 * CALIBRATION_UNIT, 1).unwrap();
                        }
                        market.finalize_amm_transition_and_observe_risk(1).unwrap();
                    }
                    let asset = debt_asset.opposite();
                    let mut first = runtime_open(&mut market, debt_asset, 2000 * CALIBRATION_UNIT, 2);
                    let terms = first.margin_terms;
                    let mut second = runtime_open(&mut market, debt_asset, 1000 * CALIBRATION_UNIT, 2);
                    assert_ne!(first.owner, second.owner);
                    assert_eq!(first.margin_terms, terms);
                    let initial_exposure = first.collateral_amount + second.collateral_amount;
                    assert_eq!(market.leverage_collateral_exposure(asset), initial_exposure);

                    let shares = market.base_side.shares.ylp_supply / 100;
                    market.remove_liquidity(shares).unwrap();
                    market.finalize_amm_transition_and_observe_risk(2).unwrap();
                    assert_eq!(first.margin_terms, terms);
                    assert_eq!(
                        first.margin_terms.initial_bps(first.collateral_amount).unwrap(),
                        terms.initial_bps(first.collateral_amount).unwrap()
                    );

                    let slice = market.leverage_close_slice(&first, 2500).unwrap();
                    let prepared = SwapRequest {
                        current_slot: 3,
                        current_unix_timestamp: 0,
                        asset_in: asset,
                        reserve_credit: slice.collateral_amount,
                        protocol_fee_bps: 0,
                    }
                    .prepare_with_cash_policy(
                        &mut market,
                        SwapCashPolicy::Close {
                            debt_asset,
                            debt_shares: slice.debt_shares,
                            debt_principal: slice.debt_principal,
                        },
                    )
                    .unwrap();
                    let quote = prepared.leverage_quote();
                    market
                        .partial_close_leverage(
                            &mut first,
                            2500,
                            0,
                            prepared,
                            full_fee_credit(&quote),
                            0,
                            ProtocolAuctionSplit::default(),
                            3,
                            0,
                            LeverageCollateralFee::default(),
                        )
                        .unwrap();
                    assert_eq!(
                        market.leverage_collateral_exposure(asset),
                        initial_exposure - slice.collateral_amount
                    );
                    assert_eq!(first.margin_terms.reference_collateral, terms.reference_collateral);
                    let expected_retained = mul_div_ceil_u128(
                        terms.admission_equity_collateral_nad,
                        u128::from(first.collateral_amount),
                        u128::from(first.collateral_amount + slice.collateral_amount),
                    )
                    .unwrap();
                    assert_eq!(first.margin_terms.admission_equity_collateral_nad, expected_retained);

                    let repayment = market
                        .debt
                        .isolated_repayment_for_max(debt_asset, first.debt_shares, u64::MAX)
                        .unwrap()
                        .cash_repaid;
                    market.repay_leverage_debt(&mut first, repayment, 3).unwrap();
                    assert_eq!(market.leverage_collateral_exposure(asset), second.collateral_amount);
                    assert_eq!(first.margin_terms.admission_equity_collateral_nad, 0);
                    let repayment = market
                        .debt
                        .isolated_repayment_for_max(debt_asset, second.debt_shares, u64::MAX)
                        .unwrap()
                        .cash_repaid;
                    market.repay_leverage_debt(&mut second, repayment, 3).unwrap();
                    assert_eq!(market.leverage_collateral_exposure(asset), 0);
                    market.assert_market_invariants().unwrap();
                }
            }
        }
    }
}

#[test]
fn runtime_withdrawal_cannot_strip_a_retained_crowding_requirement() {
    let mut market = test_market(1_000_000_000, 1_000_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 12_000, 15_200);
    position.margin_terms = crate::state::LeverageMarginTerms::at_entry(50_000).unwrap();
    position.margin_terms.admission_equity_collateral_nad = 3000 * u128::from(NAD);
    assert!(
        position.margin_terms.initial_bps(15_200).unwrap()
            > position.margin_terms.maintenance_bps(15_200).unwrap() + 300
    );
    let result = market.remove_leverage_margin(&mut position, 300, 1, 0, LeverageCollateralFee::default());
    assert_eq!(result.unwrap_err(), error!(ErrorCode::LeverageInitialMarginTooLow));
    assert_eq!(position.debt_principal, 12000);
    market
        .remove_leverage_margin(&mut position, 100, 1, 0, LeverageCollateralFee::default())
        .unwrap();
    assert_eq!(
        position.margin_terms.admission_equity_collateral_nad,
        3000 * u128::from(NAD)
    );
    assert_eq!(market.leverage_collateral_exposure(MarketAsset::Quote), 15_200);
}

#[test]
fn runtime_large_solvent_position_is_no_longer_rejected_by_two_percent_unwind() {
    let mut market = calibration_market(100_000, 1);
    let position = runtime_open(&mut market, MarketAsset::Quote, 2500 * CALIBRATION_UNIT, 2);
    let quote = market
        .quote_leverage_swap(MarketAsset::Base, position.collateral_amount, 2)
        .unwrap();
    // This fixture uses equal decimals on both sides.
    let spot = (u128::from(position.collateral_amount)
        * u128::from(market.current_base_price_nad().unwrap()) / u128::from(NAD)) as u64;
    assert!(u128::from(spot - quote.amount_out) * 10_000 / u128::from(spot) > 200);
    assert_eq!(position.margin_terms.maintenance_rates_bps, [700, 1200, 1700]);
    assert!(
        position
            .margin_terms
            .maintenance_bps(position.collateral_amount)
            .unwrap()
            > 700
    );
}

#[test]
fn falling_potential_cannot_refund_a_retained_requirement() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 12000, 15200);
    position.margin_terms = crate::state::LeverageMarginTerms::at_entry(50000).unwrap();
    position.margin_terms.admission_equity_collateral_nad = 3000 * u128::from(NAD);
    let potential_after = market.leverage_admission_potential(MarketAsset::Quote).unwrap();
    market
        .retain_leverage_admission(&mut position, 0, potential_after + 1000 * u128::from(NAD), false)
        .unwrap();
    assert_eq!(position.margin_terms.reference_collateral, 50000);
    assert_eq!(
        position.margin_terms.admission_equity_collateral_nad,
        3000 * u128::from(NAD)
    );
}

#[test]
fn unhealthy_symmetric_ema_allows_liquidation_even_with_a_better_execution_quote() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1000, 1500);
    let quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 1)
        .unwrap();
    assert!(equity_bps(quote.amount_out, 1000).unwrap() > 700);
    let prepared = prepared_leverage_swap(
        &market,
        quote,
        SwapCashPolicy::Liquidate {
            debt_asset: MarketAsset::Base,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
            insurance_credit: 0,
        },
    );
    market.risk.quote_price_ema_nad = 700_000_000;
    assert!(market.leverage_reference_equity_bps(&position, 1500).unwrap() < 700);
    market
        .liquidate_leverage_position(
            &mut position,
            Some(prepared),
            quote.amount_in,
            full_fee_credit(&quote),
            LeverageInsuranceDraw::default(),
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();
    assert_eq!(position.debt_shares, 0);
    assert_eq!(market.leverage_collateral_exposure(MarketAsset::Quote), 0);
}

fn hlp_market() -> Market {
    initialize(Case {
        amp: 4,
        controller: true,
        hlp: true,
        debt_asset: MarketAsset::Quote,
        pieces: 1,
        path: 0,
        cushion: 200,
        withdrawal: 0,
    })
}

#[test]
fn native_hlp_first_deposit_prices_backing_before_receipt_mint() {
    for amp in [1, 4] {
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = initialize(Case {
                amp,
                controller: true,
                hlp: false,
                debt_asset: MarketAsset::Quote,
                pieces: 1,
                path: 0,
                cushion: 200,
                withdrawal: 0,
            });
            market.deposit_single_sided(asset, 10_000 * CALIBRATION_UNIT, 1).unwrap();
            let current_price = market.current_concentrated_spot_price_nad().unwrap().unwrap();
            let cached_price = match asset {
                MarketAsset::Base => market.base_hlp_vault.cached_settlement_price_nad,
                MarketAsset::Quote => market.quote_hlp_vault.cached_settlement_price_nad,
            };
            assert_eq!(cached_price, current_price as u128, "amp={amp} asset={asset:?}");
            // No market move occurred. A second entry must not trip the
            // divergence check against a transient pre-mint curve price.
            market.deposit_single_sided(asset, CALIBRATION_UNIT, 1).unwrap();
            market.assert_market_invariants().unwrap();
        }
    }
}

fn fund_recenter(market: &mut Market) {
    market
        .credit_protected_recenter_reserve(MarketAsset::Base, 1_000 * CALIBRATION_UNIT)
        .unwrap();
    market
        .credit_protected_recenter_reserve(MarketAsset::Quote, 2_000 * CALIBRATION_UNIT)
        .unwrap();
    market.amm.price_ema_nad = NAD * 9 / 10;
    assert!(market.advance_one_amm_controller_target(2).unwrap());
    assert_eq!(market.base_side.reserves.protected_recenter_reserve, 0);
    assert_eq!(market.quote_side.reserves.protected_recenter_reserve, 0);
}

#[test]
fn native_hlp_swap_after_funded_recenter() {
    let mut market = hlp_market();
    fund_recenter(&mut market);
    // Before the fix the quote inferred 10,333 / 10,667 tokens of hLP debt
    // from changed claims while the ledger still recorded 10,000 each.
    let state = market.integrated_curve_state_nad().unwrap();
    assert_eq!(state.base_hlp_quote_debt, 10_000 * NAD as u128);
    assert_eq!(state.quote_hlp_base_debt, state.base_hlp_quote_debt);
    // The funding proof and committed cache must use the point which will
    // actually be quoted, including hLP's share of the deployed inventory.
    assert_eq!(
        market.amm.concentrated_curve_cache,
        crate::transitions::amm::prepare_concentrated_cache_at_point(
            state.ordinary_base,
            state.ordinary_quote,
            market.current_curve_center_price_nad().unwrap(),
            market.config.amm.concentrated_curve_parameters().unwrap(),
        )
        .unwrap()
    );
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let (after, output) = sale(&market, asset, 100 * CALIBRATION_UNIT, 2).unwrap();
        assert!(output > 0);
        after.assert_market_invariants().unwrap();
    }
}

#[test]
fn native_hlp_start_and_settlement_conserve_recorded_debt_and_funding() {
    for mutation in 0..3 {
        for funding in [false, true] {
            for asset in [MarketAsset::Base, MarketAsset::Quote] {
                let mut market = hlp_market();
                match mutation {
                    0 => fund_recenter(&mut market),
                    1 => {
                        market.remove_liquidity(market.base_side.shares.ylp_supply / 4).unwrap();
                        market.finalize_amm_transition_and_observe_risk(2).unwrap();
                    }
                    _ => {
                        market
                            .credit_protected_recenter_reserve(MarketAsset::Quote, 2_000 * CALIBRATION_UNIT)
                            .unwrap();
                        market.release_protected_recenter_reserves().unwrap();
                        market.finalize_amm_transition_and_observe_risk(2).unwrap();
                    }
                }
                if funding {
                    market.debt.base_borrow_index_nad = 17 * NAD as u128 / 16;
                    market.debt.quote_borrow_index_nad = 33 * NAD as u128 / 32;
                    market.finalize_amm_transition_and_observe_risk(2).unwrap();
                }
                no_accrual(&mut market, 2);
                let mut prepared = SwapRequest {
                    current_slot: 2,
                    current_unix_timestamp: 0,
                    asset_in: asset,
                    reserve_credit: 100 * CALIBRATION_UNIT,
                    protocol_fee_bps: 0,
                }
                .prepare(&mut market)
                .unwrap();
                let state = market.integrated_curve_state_nad().unwrap();
                let reserves = market.curve_reserves_nad().unwrap();
                assert_eq!(
                    state.ordinary_base + state.base_hlp_equity + state.quote_hlp_base_debt,
                    reserves.base
                );
                assert_eq!(
                    state.ordinary_quote + state.quote_hlp_equity + state.base_hlp_quote_debt,
                    reserves.quote
                );
                let base_debt =
                    Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad)
                        .unwrap();
                let quote_debt =
                    Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad)
                        .unwrap();
                assert_eq!(
                    state.base_hlp_quote_debt,
                    market.normalize_amount(base_debt, 3).unwrap()
                );
                assert_eq!(
                    state.quote_hlp_base_debt,
                    market.normalize_amount(quote_debt, 3).unwrap()
                );
                let base_interest = base_debt as u64 - market.base_hlp_vault.debt_principal;
                let quote_interest = quote_debt as u64 - market.quote_hlp_vault.debt_principal;
                let before = market.clone();
                let quote = prepared.quote;
                let finalized = prepared
                    .finalize_state(&mut market, 2, 0, ProtocolAuctionSplit::default())
                    .unwrap();
                assert_eq!(finalized.base_rebalance.interest_paid, base_interest);
                assert_eq!(finalized.quote_rebalance.interest_paid, quote_interest);
                // Physical cash moves once for trader input/output, fee
                // compounding and each hLP's funding payment. Refinancing
                // synthetic debt cannot manufacture spendable tokens.
                for side in [MarketAsset::Base, MarketAsset::Quote] {
                    let interest = if side == MarketAsset::Base {
                        quote_interest
                    } else {
                        base_interest
                    };
                    let expected = before.side(side).reserves.cash_reserve as i128
                        + if side == asset {
                            quote.fee.amount_in_for_quote as i128
                        } else {
                            -(quote.gross_amount_out as i128)
                        }
                        + if side.code() == quote.fee.fee_asset {
                            quote.fee.compounded_fee_debit as i128
                        } else {
                            0
                        }
                        - interest as i128;
                    assert_eq!(market.side(side).reserves.cash_reserve as i128, expected);
                }
                market.assert_market_invariants().unwrap();
                assert_eq!(market.base_hlp_vault.residual_exposure, 0);
                assert_eq!(market.quote_hlp_vault.residual_exposure, 0);
            }
        }
    }
}

#[test]
fn native_hlp_still_rejects_unexplained_reserve_changes() {
    let mut market = hlp_market();
    fund_recenter(&mut market);
    no_accrual(&mut market, 2);
    let mut prepared = SwapRequest {
        current_slot: 2,
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 100 * CALIBRATION_UNIT,
        protocol_fee_bps: 0,
    }
    .prepare(&mut market)
    .unwrap();
    market.base_side.reserves.live_reserve += 4;
    let error = prepared
        .finalize_state(&mut market, 2, 0, ProtocolAuctionSplit::default())
        .unwrap_err();
    assert_eq!(error, error!(ErrorCode::BrokenInvariant));
}

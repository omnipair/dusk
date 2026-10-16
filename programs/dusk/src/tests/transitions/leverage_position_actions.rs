use super::*;

#[test]
fn reduce_to_one_x_releases_all_exposure_and_reborrowing_readmits_the_whole_position() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for concentrated in [false, true] {
            let scale = 1_000_000;
            let mut market = if concentrated { active_concentrated_hlp_market_with_decimals(6) }
                else { test_market(1_000_000 * scale, 1_000_000 * scale) };
            let mut position = seeded_position(&mut market, asset, 1_000 * scale, 10_000 * scale);
            let before_collateral = position.collateral_amount;
            let debit = 2_000 * scale;
            let mut prepared = prepare_leverage_swap_with_policy(&mut market, asset.opposite(), debit, 1,
                SwapCashPolicy::Decrease { debt_asset: asset, debt_shares: position.debt_shares, debt_principal: position.debt_principal });
            let quote = prepared.leverage_quote();
            let result = market.decrease_leverage(&mut position, debit, 0, 0, &mut prepared,
                full_fee_credit(&quote), 0, ProtocolAuctionSplit::default(), 1, 0, LeverageCollateralFee::default());
            assert_eq!(result.unwrap_err(), error!(ErrorCode::SlippageExceeded));
            assert_eq!(position.collateral_amount, before_collateral);
            let receipt = market.decrease_leverage(&mut position, debit, 1_000 * scale, u64::MAX, &mut prepared,
                full_fee_credit(&quote), 0, ProtocolAuctionSplit::default(), 1, 0, LeverageCollateralFee::default()).unwrap();
            assert_eq!(receipt.debt_amount, 0);
            assert_eq!(position.debt_shares, 0);
            assert_eq!(position.debt_principal, 0);
            assert!(receipt.owner_residual > 0);
            assert_eq!(position.collateral_amount, before_collateral - debit);
            assert_eq!(market.leverage_collateral_exposure(asset.opposite()), 0);
            assert_eq!(position.margin_terms.admission_equity_collateral_nad, 0);
            market.assert_market_invariants().unwrap();

            // Stale stored allowances cannot be resurrected from the 1x state.
            position.margin_terms.reference_collateral = u64::MAX;
            let prepared = prepare_leverage_swap_with_policy(&mut market, asset, 100 * scale, 2,
                SwapCashPolicy::Borrow { asset, amount: 100 * scale });
            let quote = prepared.leverage_quote();
            market.increase_leverage(&mut position, 100 * scale, 0, quote.amount_out, prepared,
                full_fee_credit(&quote), 0, ProtocolAuctionSplit::default(), 2, 0, LeverageCollateralFee::default()).unwrap();
            assert_eq!(market.leverage_collateral_exposure(asset.opposite()), position.collateral_amount);
            assert!(position.margin_terms.reference_collateral < u64::MAX);
            assert!(position.margin_terms.admission_equity_collateral_nad > 0);
            market.assert_market_invariants().unwrap();
        }
    }
}

#[test]
fn adding_to_position_swaps_only_wallet_credit_plus_new_borrowing() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for borrowed in [0, 100] {
            let mut market = test_market(1_000_000, 1_000_000);
            let mut position = seeded_position(&mut market, asset, 500, 2_000);
            let credit = 200;
            let prepared = prepare_leverage_swap_with_policy(&mut market, asset, credit + borrowed, 1,
                if borrowed == 0 { SwapCashPolicy::Spot } else { SwapCashPolicy::Borrow { asset, amount: borrowed } });
            let quote = prepared.leverage_quote();
            let receipt = market.increase_leverage(&mut position, borrowed, credit, quote.amount_out, prepared,
                full_fee_credit(&quote), 0, ProtocolAuctionSplit::default(), 1, 0, LeverageCollateralFee::default()).unwrap();
            assert_eq!(receipt.debt_amount, 500 + borrowed);
            assert_eq!(receipt.collateral_amount, 2_000 + quote.amount_out);
            assert_eq!(receipt.owner_residual, 0);
            assert_eq!(market.leverage_collateral_exposure(asset.opposite()), position.collateral_amount);
            market.assert_market_invariants().unwrap();
        }
    }
}

#[test]
fn repaid_positions_can_be_partially_and_fully_sold_without_readding_exposure() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = test_market(1_000_000, 1_000_000);
        let mut position = seeded_position(&mut market, asset, 500, 2_000);
        market.repay_leverage_debt(&mut position, 500, 1).unwrap();
        for close_bps in [5_000, 10_000] {
            let slice = market.leverage_close_slice(&position, close_bps).unwrap();
            assert_eq!(slice.debt_shares, 0);
            let prepared = prepare_leverage_swap_with_policy(&mut market, asset.opposite(), slice.collateral_amount, 2,
                SwapCashPolicy::Close { debt_asset: asset, debt_shares: 0, debt_principal: 0, shares_to_burn: 0 });
            let quote = prepared.leverage_quote();
            let receipt = if close_bps == 10_000 {
                market.close_leverage(&mut position, 0, prepared, full_fee_credit(&quote), 0,
                    ProtocolAuctionSplit::default(), 2, LeverageCollateralFee::default())
            } else {
                market.partial_close_leverage(&mut position, close_bps, 0, prepared, full_fee_credit(&quote), 0,
                    ProtocolAuctionSplit::default(), 2, 0, LeverageCollateralFee::default())
            }.unwrap();
            assert_eq!(receipt.debt_repaid, 0);
            assert_eq!(receipt.residual, quote.amount_out);
            assert_eq!(market.leverage_collateral_exposure(asset.opposite()), 0);
            market.assert_market_invariants().unwrap();
        }
        assert_eq!(position.collateral_amount, 0);
    }
}

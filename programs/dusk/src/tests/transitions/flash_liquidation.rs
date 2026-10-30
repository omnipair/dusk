use super::*;
use crate::state::{BorrowPosition, LeverageMarginTerms};
use crate::transitions::{flash_liquidation::LiquidationPositionRef, flash_settlement::LiquidationPositionMut};

#[test]
fn full_close_certificate_never_overrides_a_useful_integer_partial() {
    let mut certified = 0;
    let mut useful = 0;
    let mut emergency_certified = 0;
    for (swap_bps, token_bps, fee_mode) in [(0, 0, 0), (300, 0, 0), (300, 0, 1), (300, 0, 2), (100, 100, 0)] {
    let fee = LeverageCollateralFee::new((token_bps > 0).then_some(TransferFee {
        epoch: 0.into(), maximum_fee: 1_u64.into(), transfer_fee_basis_points: (token_bps as u16).into(),
    }));
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for reference in [100, 10_000] {
            for debt in 94..=99 {
                let market_template = test_market(50_000, 50_000);
                let mut market = market_template;
                market.config.swap_fee_bps = swap_bps;
                market.config.amm.swap_fee_collect_mode = fee_mode;
                let mut position = seeded_position(&mut market, asset, debt, 100);
                position.margin_terms.reference_collateral = reference;
                let p = LiquidationPositionRef::Leverage(&position);
                let health = market.flash_liquidation_health(&p, asset, fee).unwrap();
                if !health.emergency_allowed().unwrap() { continue; }
                let proof = market.proves_partial_bound(&p, asset, fee, fee, fee, 120, 0, None).unwrap();
                let mut found = false;
                let mut found_recovery = false;
                for size in 1..100 {
                    if let Ok(q) = market.quote_flash_liquidation(&p, asset, size, false, fee, fee, fee, 120, 0) {
                        assert!(health.improves(q.health_after).unwrap());
                        found = true;
                    }
                    if let Ok(prepared) = market.clone().prepare_emergency_liquidation(&p, asset, size, false, fee, fee, fee, 120, 0, 0) {
                        assert!(health.improves(prepared.quote.health_after).unwrap());
                        found = true;
                        found_recovery |= prepared.quote.health_after.recovered().unwrap();
                    }
                }
                if market.clone().prepare_emergency_liquidation(&p, asset, 100, true, fee, fee, fee, 120, 0, 0).is_ok() {
                    emergency_certified += 1;
                    assert!(!found_recovery, "full AMM sale bypassed recovery: {asset:?}, reference {reference}, debt {debt}");
                }
                if proof { certified += 1; assert!(!found, "false full-close permission: {asset:?}, reference {reference}, debt {debt}"); }
                if found { useful += 1; assert!(!proof); }
            }
        }
    }
    }
    assert!(certified > 0);
    assert!(useful > 0);
    assert!(emergency_certified > 0);
}

#[test]
fn emergency_sale_uses_principal_first_waterfall_and_clears_both_directions() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = test_market(5_000_000, 5_000_000);
        let mut position = indexed_position(&mut market, asset, 900_000);
        let remove = position.collateral_amount - 850_000;
        market.reduce_leverage_collateral(&mut position, remove).unwrap();
        market.insurance.credit(asset, 1_000_000, 0).unwrap();
        let mut prepared = market.prepare_emergency_liquidation(&LiquidationPositionRef::Leverage(&position), asset,
            850_000, true, LeverageCollateralFee::default(), LeverageCollateralFee::default(), LeverageCollateralFee::default(),
            0, 0, 0).unwrap();
        let q = prepared.quote;
        assert_eq!(q.rates.emergency_reward_bps, 100);
        assert_eq!(q.fee.total, 0);
        assert_eq!(q.loss.interest_paid, 0);
        assert_eq!(q.loss.interest_canceled, 450_000);
        assert!(q.loss.principal_written_off > 0);
        assert_eq!(q.swap_output, q.repayment + q.reward_debit);
        let finalized = market.apply_emergency_liquidation(&mut LiquidationPositionMut::Leverage(&mut position), asset, &mut prepared, 0, 0, ProtocolAuctionSplit::default()).unwrap().unwrap();
        assert_eq!(finalized.lifecycle.socialized_principal_loss, q.loss.principal_written_off);
        assert_eq!(position.debt_shares, 0);
        assert_eq!(position.debt_principal, 0);
        assert_eq!(position.collateral_amount, 0);
        assert_eq!(market.insurance.available(asset), 1_000_000 - q.insurance_debit);
        market.assert_virtual_reserve_invariant(asset).unwrap();
    }
}

#[test]
fn emergency_partial_improves_health_after_reward_and_charge() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = test_market(50_000_000, 50_000_000);
        let mut position = seeded_position(&mut market, asset, 960_000, 1_000_000);
        let mut prepared = market.prepare_emergency_liquidation(&LiquidationPositionRef::Leverage(&position), asset,
            100_000, false, LeverageCollateralFee::default(), LeverageCollateralFee::default(), LeverageCollateralFee::default(),
            0, 0, 0).unwrap();
        let q = prepared.quote;
        assert!(q.health_before.improves(q.health_after).unwrap());
        assert_eq!(q.fee.total, q.repayment / 100);
        assert_eq!(q.insurance_debit, 0);
        assert_eq!(q.loss.principal_written_off, 0);
        assert_eq!(q.swap_output, q.repayment + q.reward_debit + q.fee.total + q.owner_debit);
        market.apply_emergency_liquidation(&mut LiquidationPositionMut::Leverage(&mut position), asset, &mut prepared, 0, 0, ProtocolAuctionSplit::default()).unwrap();
        assert_eq!(position.collateral_amount, 900_000);
        assert_eq!(position.debt_amount(&market.debt).unwrap(), q.health_after.debt);
        market.assert_virtual_reserve_invariant(asset).unwrap();
    }
}

#[test]
fn internal_liquidation_nets_principal_for_both_debt_ledgers_without_wallet_cash() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for isolated in [false, true] {
            let mut market = test_market(10_000, 10_000);
            market.side_mut(asset).reserves.cash_reserve = 1_000;
            if isolated {
                match asset {
                    MarketAsset::Base => { market.debt.isolated_base_shares = 9_000; market.debt.isolated_base_principal = 9_000; }
                    MarketAsset::Quote => { market.debt.isolated_quote_shares = 9_000; market.debt.isolated_quote_principal = 9_000; }
                }
            } else {
                match asset {
                    MarketAsset::Base => { market.debt.fixed_base_shares = 9_000; market.debt.fixed_base_principal = 9_000; }
                    MarketAsset::Quote => { market.debt.fixed_quote_shares = 9_000; market.debt.fixed_quote_principal = 9_000; }
                }
            }
            market.assert_virtual_reserve_invariant(asset).unwrap();
            let policy = SwapCashPolicy::SettleLiquidation { debt_asset: asset, isolated, full: false,
                shares: 4_000, principal_removed: 4_000, debt_reduced: 4_000, repayment: 4_000, insurance_credit: 0 };
            // A wallet swap of this size would need 4,100 output atoms. Only
            // the 100-atom residual leaves custody during a netted liquidation.
            let transition = market.apply_leverage_lifecycle_transition(policy, asset.opposite(), 7_000, 4_100, 4_100).unwrap();
            assert_eq!(transition.clearance.principal_paid, 4_000);
            assert_eq!(transition.clearance.interest_paid, 0);
            assert_eq!(market.side(asset).reserves.cash_reserve, 900);
            assert_eq!(market.side(asset).reserves.live_reserve, 5_900);
            assert_eq!(transition.socialized_principal_loss, 0);
            market.assert_virtual_reserve_invariant(asset).unwrap();
        }
    }
}

#[test]
fn flash_partial_quote_recovers_health_and_leaves_unrelated_position_untouched() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = test_market(5_000_000, 5_000_000);
        let mut position = seeded_position(&mut market, asset, 940_000, 1_000_000);
        let unrelated = seeded_position(&mut market, asset, 20_000, 60_000);
        let before_other = unrelated.try_to_vec().unwrap();
        let q = market.quote_flash_liquidation(
            &LiquidationPositionRef::Leverage(&position), asset, 100_000, false,
            LeverageCollateralFee::default(), LeverageCollateralFee::default(), LeverageCollateralFee::default(), 0, 0,
        ).unwrap();
        assert_eq!(q.fee.total, 1_000);
        assert_eq!(q.fee.protocol, 200);
        assert!(q.health_before.improves(q.health_after).unwrap());
        assert!(!q.health_after.recovered().unwrap());
        let allocation = market.liquidation_debt_allocation(&LiquidationPositionRef::Leverage(&position), asset, q.debt_shares_to_burn).unwrap();
        let loss = market.settle_flash_recovery(&mut LiquidationPositionMut::Leverage(&mut position), asset, allocation, q.collateral_debit, allocation.debt, 0, 0, false, 0).unwrap();
        assert_eq!(loss.principal_written_off, 0);
        assert_eq!(loss.interest_canceled, 0);
        assert_eq!(position.collateral_amount, 1_000_000 - q.collateral_debit);
        assert_eq!(position.debt_amount(&market.debt).unwrap(), q.health_after.debt);
        assert_eq!(unrelated.try_to_vec().unwrap(), before_other);
        assert_eq!(market.leverage_collateral_exposure(asset.opposite()), position.collateral_amount + unrelated.collateral_amount);
    }
}

#[test]
fn full_external_recovery_spends_insurance_only_on_principal_and_cancels_interest() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = test_market(5_000_000, 5_000_000);
        let mut position = indexed_position(&mut market, asset, 900_000);
        let remove = position.collateral_amount - 850_000;
        market.reduce_leverage_collateral(&mut position, remove).unwrap();
        let before_curve = market.curve_reserve(asset).unwrap();
        market.insurance.credit(asset, 1_000_000, 1).unwrap();
        let allocation = market.liquidation_debt_allocation(&LiquidationPositionRef::Leverage(&position), asset, position.debt_shares).unwrap();
        assert_eq!((allocation.debt, allocation.principal), (1_350_000, 900_000));
        let loss = market.settle_flash_recovery(&mut LiquidationPositionMut::Leverage(&mut position), asset, allocation, 850_000, 400_000, 375_000, 375_000, true, 1).unwrap();
        assert_eq!(loss.principal_written_off, 125_000);
        assert_eq!(loss.interest_canceled, 450_000);
        assert_eq!(loss.interest_paid, 0);
        assert_eq!(position.debt_shares, 0);
        assert_eq!(position.debt_principal, 0);
        assert_eq!(position.collateral_amount, 0);
        assert_eq!(market.insurance.available(asset), 625_000);
        assert_eq!(market.insurance.draw_capacity(asset, 1).unwrap(), 125_000);
        assert_eq!(market.curve_reserve(asset).unwrap(), before_curve - 125_000);
    }
}

#[test]
fn full_borrow_recovery_preserves_the_other_debt_leg() {
    let mut market = test_market(5_000_000, 5_000_000);
    let mut position = crate::state::BorrowPosition::default();
    position.fixed_base_shares = 1_000;
    position.fixed_quote_shares = 500;
    position.fixed_base_principal = 1_000;
    position.fixed_quote_principal = 500;
    position.base_collateral = 2_000;
    position.quote_collateral = 800;
    position.base_liquidation_cf_bps = 9_000;
    position.quote_liquidation_cf_bps = 9_000;
    market.debt.fixed_base_shares = 1_000;
    market.debt.fixed_base_principal = 1_000;
    market.debt.fixed_quote_shares = 500;
    market.debt.fixed_quote_principal = 500;
    market.base_side.reserves.cash_reserve -= 1_000;
    market.quote_side.reserves.cash_reserve -= 500;
    let allocation = market.liquidation_debt_allocation(&LiquidationPositionRef::Borrow(&position), MarketAsset::Base, 1_000).unwrap();
    let loss = market.settle_flash_recovery(&mut LiquidationPositionMut::Borrow(&mut position), MarketAsset::Base, allocation, 800, 700, 0, 0, true, 1).unwrap();
    assert_eq!(loss.principal_written_off, 300);
    assert_eq!(position.fixed_base_shares, 0);
    assert_eq!(position.quote_collateral, 0);
    assert_eq!(position.fixed_quote_shares, 500);
    assert_eq!(position.base_collateral, 2_000);
    assert_eq!(position.quote_liquidation_cf_bps, 9_000);
}

#[test]
fn flash_quote_payment_covers_both_possible_aggregate_floor_phases() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = test_market(5_000_000, 5_000_000);
        let mut position = indexed_position(&mut market, asset, 60_001);
        let remove = position.collateral_amount - 95_000;
        market.reduce_leverage_collateral(&mut position, remove).unwrap();
        let q = market.quote_flash_liquidation(
            &LiquidationPositionRef::Leverage(&position), asset, 1_001, false,
            LeverageCollateralFee::default(), LeverageCollateralFee::default(), LeverageCollateralFee::default(), 0, 0,
        ).unwrap();
        for extra in 0..=1 {
            let mut changed = market.clone();
            match asset {
                MarketAsset::Base => changed.debt.isolated_base_shares += extra,
                MarketAsset::Quote => changed.debt.isolated_quote_shares += extra,
            }
            let allocation = changed.liquidation_debt_allocation(&LiquidationPositionRef::Leverage(&position), asset, q.debt_shares_to_burn).unwrap();
            assert!(allocation.debt <= q.repayment);
            assert!(q.repayment - allocation.debt <= 1);
            assert_eq!(allocation.remaining_debt, q.health_after.debt);
        }
    }
}

#[test]
fn external_flash_loss_preserves_concentrated_hlp_backing() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = active_concentrated_hlp_market();
        let mut position = seeded_position(&mut market, asset, 100_000, 50_000);
        let fee = LeverageCollateralFee::default();
        let q = market.quote_flash_liquidation(&LiquidationPositionRef::Leverage(&position), asset, 0, true,
            fee, fee, fee, 0, 1).unwrap();
        let allocation = market.liquidation_debt_allocation(&LiquidationPositionRef::Leverage(&position), asset, position.debt_shares).unwrap();
        let loss = market.settle_flash_recovery(&mut LiquidationPositionMut::Leverage(&mut position), asset, allocation, q.collateral_debit, q.repayment, 0, 0, true, 1).unwrap();
        assert!(loss.principal_written_off > 0);
        market.assert_virtual_reserve_invariant(asset).unwrap();
        market.assert_virtual_reserve_invariant(asset.opposite()).unwrap();
        market.prepare_amm_for_swap(2).unwrap();
        assert!(market.current_concentrated_curve_geometry().unwrap().is_some());
        for side in [MarketAsset::Base, MarketAsset::Quote] {
            let shares = if side == MarketAsset::Base { market.base_hlp_vault.hlp_supply } else { market.quote_hlp_vault.hlp_supply } / 10;
            market.withdraw_single_sided(side, shares).unwrap();
            market.assert_virtual_reserve_invariant(side).unwrap();
        }
    }
}

#[test]
fn positive_equity_full_close_proof_handles_raw_token_scale() {
    let fee = LeverageCollateralFee::default();
    for scale in [1, 1_000, 1_000_000] {
        let mut market = test_market(50_000 * scale, 50_000 * scale);
        let position = seeded_position(&mut market, MarketAsset::Quote, 995 * scale, 1_000 * scale);
        assert!(market.proves_partial_bound(&LiquidationPositionRef::Leverage(&position), MarketAsset::Quote,
            fee, fee, fee, 120, 0, None).unwrap(), "scale {scale}");
    }
}

#[test]
fn recovery_target_cannot_be_crossed_twice_by_larger_permitted_fills() {
    for cap in [0, 1, 10] {
        let token_fee = LeverageCollateralFee::new(Some(TransferFee {
            epoch: 0.into(), maximum_fee: cap.into(), transfer_fee_basis_points: 300_u16.into(),
        }));
        for debt in 92..100 {
            let mut market = test_market(50_000, 50_000);
            let position = seeded_position(&mut market, MarketAsset::Quote, debt, 100);
            let reference = LiquidationPositionRef::Leverage(&position);
            let fee = LeverageCollateralFee::default();
            let mut recovered_flash = None;
            let mut recovered_amm = None;
            for size in 1..100 {
                if let Ok(q) = market.quote_flash_liquidation(&reference, MarketAsset::Quote, size, false,
                    token_fee, token_fee, fee, 0, 0) {
                    if q.health_after.recovered().unwrap() {
                        assert!(recovered_flash.is_none(), "flash repeated target: cap {cap}, debt {debt}, first {recovered_flash:?}, size {size}");
                        recovered_flash = Some(size);
                    }
                }
                if let Ok(q) = market.clone().prepare_emergency_liquidation(&reference, MarketAsset::Quote, size, false,
                    token_fee, token_fee, fee, 0, 0, 0) {
                    if q.quote.health_after.recovered().unwrap() {
                        assert!(recovered_amm.is_none(), "AMM repeated target: cap {cap}, debt {debt}, first {recovered_amm:?}, size {size}");
                        recovered_amm = Some(size);
                    }
                }
            }
        }
    }
}

#[test]
fn useful_partials_have_no_economic_remaining_debt_floor() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = test_market(50_000_000, 50_000_000);
        let position = seeded_position(&mut market, asset, 960_000, 1_000_000);
        let fee = LeverageCollateralFee::default();
        let p = LiquidationPositionRef::Leverage(&position);
        let flash = market.quote_flash_liquidation(&p, asset, 100_000, false, fee, fee, fee, 0, 0).unwrap();
        assert_eq!(flash.repayment, 100_000);
        assert_eq!(flash.health_after.debt, 860_000);
        assert!(flash.health_before.improves(flash.health_after).unwrap());
        let amm = market.clone().prepare_emergency_liquidation(&p, asset, 100_000, false,
            fee, fee, fee, 0, 0, 0).unwrap().quote;
        assert!(amm.health_after.debt < 940_000);
        assert!(amm.health_before.improves(amm.health_after).unwrap());
        // Removing the minimum does not let a caller choose full liquidation
        // while a useful partial exists inside the emergency boundary.
        assert!(market.quote_flash_liquidation(&p, asset, 0, true, fee, fee, fee, 0, 0).is_err());
        assert!(market.clone().prepare_emergency_liquidation(&p, asset, 1_000_000, true,
            fee, fee, fee, 0, 0, 0).is_err());
    }
}

#[test]
fn health_only_partials_are_timestamp_independent_for_both_ledgers() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for borrowing in [false, true] {
            let mut market = test_market(50_000_000_000, 50_000_000_000);
            let mut leverage = seeded_position(&mut market, asset, 945_000_000, 1_000_000_000);
            leverage.margin_terms = LeverageMarginTerms::at_entry(100_000_000_000).unwrap();
            let mut borrow = BorrowPosition::default();
            if borrowing {
                match asset {
                    MarketAsset::Base => {
                        market.debt.fixed_base_shares = market.debt.isolated_base_shares;
                        market.debt.fixed_base_principal = market.debt.isolated_base_principal;
                        market.debt.isolated_base_shares = 0;
                        market.debt.isolated_base_principal = 0;
                        borrow.fixed_base_shares = 945_000_000;
                        borrow.quote_collateral = 1_000_000_000;
                        borrow.base_liquidation_cf_bps = 9_300;
                    }
                    MarketAsset::Quote => {
                        market.debt.fixed_quote_shares = market.debt.isolated_quote_shares;
                        market.debt.fixed_quote_principal = market.debt.isolated_quote_principal;
                        market.debt.isolated_quote_shares = 0;
                        market.debt.isolated_quote_principal = 0;
                        borrow.fixed_quote_shares = 945_000_000;
                        borrow.base_collateral = 1_000_000_000;
                        borrow.quote_liquidation_cf_bps = 9_300;
                    }
                }
            }
            let position = if borrowing { LiquidationPositionRef::Borrow(&borrow) }
                else { LiquidationPositionRef::Leverage(&leverage) };
            let fee = LeverageCollateralFee::default();
            let health = market.flash_liquidation_health(&position, asset, fee).unwrap();
            assert_eq!(health.maintenance_bps, 700);
            assert!(!health.emergency_allowed().unwrap());
            let early = market.quote_flash_liquidation(&position, asset, 100_000_000, false, fee, fee, fee, 0, 0).unwrap();
            let late = market.quote_flash_liquidation(&position, asset, 100_000_000, false, fee, fee, fee, 120, 0).unwrap();
            assert_eq!(early.rates.buyer_discount_bps, 146);
            assert_eq!(late.rates, early.rates);
            assert_eq!(late.collateral_debit, early.collateral_debit);
            assert_eq!(late.payment, early.payment);
            assert_eq!(late.health_after, early.health_after);
            for now in [i64::MIN, -1, 1, 60, 86_400, i64::MAX] {
                let quote = market.quote_flash_liquidation(&position, asset, 100_000_000, false, fee, fee, fee, now, 0).unwrap();
                assert_eq!(quote.rates, early.rates);
                assert_eq!(quote.collateral_debit, early.collateral_debit);
                assert_eq!(quote.payment, early.payment);
            }
            assert_eq!(late.repayment, 100_000_000);
            assert_eq!(late.fee.total, 1_000_000);
            assert_eq!(late.health_after.debt, 845_000_000);
            assert!(health.improves(late.health_after).unwrap());
            assert!(market.quote_flash_liquidation(&position, asset, 0, true, fee, fee, fee, 120, 0).is_err());
        }
    }
}

#[test]
fn affordable_flash_prevents_fee_heavy_market_from_blocking_every_liquidation() {
    // Original audit case: 3% equity and 3% AMM fee. The former fixed 5%
    // flash discount and fee-free full-close proof jointly blocked all paths.
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = test_market(50_000, 50_000);
        market.config.swap_fee_bps = 300;
        let mut position = seeded_position(&mut market, asset, 970, 1_000);
        position.margin_terms = LeverageMarginTerms::at_entry(50_000).unwrap();
        let p = LiquidationPositionRef::Leverage(&position);
        let fee = LeverageCollateralFee::default();
        let quote = market.quote_flash_liquidation(&p, asset, 100, false, fee, fee, fee, 120, 0).unwrap();
        assert!(quote.health_before.emergency_allowed().unwrap());
        assert!(quote.health_before.improves(quote.health_after).unwrap());
        assert!(quote.rates.buyer_discount_bps < 500);
        assert!(!market.proves_partial_bound(&p, asset, fee, fee, fee, 120, 0, None).unwrap());
        assert!(market.quote_flash_liquidation(&p, asset, 0, true, fee, fee, fee, 120, 0).is_err());
    }
}

#[test]
fn emergency_recovery_cap_uses_actual_fee_bearing_partials() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for fee_mode in [0, 1, 2] {
            let mut market = test_market(500_000, 500_000);
            market.config.swap_fee_bps = 100;
            market.config.amm.swap_fee_collect_mode = fee_mode;
            let mut position = seeded_position(&mut market, asset, 952, 1_000);
            position.margin_terms = LeverageMarginTerms::at_entry(50_000).unwrap();
            let p = LiquidationPositionRef::Leverage(&position);
            let fee = LeverageCollateralFee::default();
            let first = (1..1_000).find(|size| {
                market.clone().prepare_emergency_candidate(&p, asset, *size, false, fee, fee, fee, 120, 0, 0)
                    .is_ok_and(|candidate| candidate.quote.health_after.recovered().unwrap())
            }).expect("a fee-bearing recovery crossing exists");
            let quote = market.clone().prepare_emergency_liquidation(&p, asset, first, false, fee, fee, fee, 120, 0, 0).unwrap().quote;
            assert!(quote.health_after.recovered().unwrap());
            let full = market.clone().prepare_emergency_liquidation(&p, asset, position.collateral_amount, true, fee, fee, fee, 120, 0, 0);
            assert!(matches!(full, Err(anchor_lang::error::Error::AnchorError(e)) if e.error_code_number == u32::from(ErrorCode::LiquidationRepayTooLarge)));
            assert!(market.clone().prepare_emergency_liquidation(&p, asset, first + 10, false, fee, fee, fee, 120, 0, 0).is_err());
        }
    }
}

#[test]
fn emergency_fallback_can_close_despite_a_useful_flash_partial() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for isolated in [false, true] {
            let mut market = test_market(50_000, 50_000);
            market.config.swap_fee_bps = 300;
            let mut leverage = seeded_position(&mut market, asset, 970, 1_000);
            leverage.margin_terms = LeverageMarginTerms::at_entry(50_000).unwrap();
            let mut borrow = BorrowPosition::default();
            if !isolated {
                match asset {
                    MarketAsset::Base => {
                        market.debt.fixed_base_shares = market.debt.isolated_base_shares;
                        market.debt.fixed_base_principal = market.debt.isolated_base_principal;
                        market.debt.isolated_base_shares = 0;
                        market.debt.isolated_base_principal = 0;
                        borrow.fixed_base_shares = leverage.debt_shares;
                        borrow.fixed_base_principal = leverage.debt_principal as u64;
                        market.debt.leverage_quote_collateral -= leverage.collateral_amount;
                        borrow.quote_collateral = leverage.collateral_amount;
                    }
                    MarketAsset::Quote => {
                        market.debt.fixed_quote_shares = market.debt.isolated_quote_shares;
                        market.debt.fixed_quote_principal = market.debt.isolated_quote_principal;
                        market.debt.isolated_quote_shares = 0;
                        market.debt.isolated_quote_principal = 0;
                        borrow.fixed_quote_shares = leverage.debt_shares;
                        borrow.fixed_quote_principal = leverage.debt_principal as u64;
                        market.debt.leverage_base_collateral -= leverage.collateral_amount;
                        borrow.base_collateral = leverage.collateral_amount;
                    }
                }
                borrow.set_liquidation_cf_bps(asset, 9_300);
            }
            let reference = if isolated { LiquidationPositionRef::Leverage(&leverage) } else { LiquidationPositionRef::Borrow(&borrow) };
            let fee = LeverageCollateralFee::default();
            let partial = market.quote_flash_liquidation(&reference, asset, 100, false, fee, fee, fee, 120, 0).unwrap();
            assert!(partial.health_before.improves(partial.health_after).unwrap());
            assert!(market.quote_flash_liquidation(&reference, asset, 0, true, fee, fee, fee, 120, 0).is_err());
            let expected = SwapRequest { current_slot: 0, current_unix_timestamp: 120,
                asset_in: asset.opposite(), reserve_credit: 1_000, protocol_fee_bps: 0 }
                .prepare_with_cash_policy(&mut market.clone(), SwapCashPolicy::LiquidationQuote).unwrap().quote;
            let mut prepared = market.prepare_emergency_liquidation(&reference, asset, 1_000, true, fee, fee, fee, 120, 0, 0).unwrap();
            assert!(prepared.quote.health_before.collateral_value > prepared.quote.health_before.debt);
            assert_eq!(prepared.quote.swap_output, expected.amount_out);
            assert!(prepared.quote.loss.principal_written_off > 0);
            let mut position = if isolated { LiquidationPositionMut::Leverage(&mut leverage) } else { LiquidationPositionMut::Borrow(&mut borrow) };
            market.apply_emergency_liquidation(&mut position, asset, &mut prepared, 0, 0, ProtocolAuctionSplit::default()).unwrap();
            market.assert_virtual_reserve_invariant(asset).unwrap();
            market.assert_virtual_reserve_invariant(asset.opposite()).unwrap();
        }
    }
}

#[test]
fn concentrated_recovery_certificate_never_skips_a_fee_bearing_integer_recovery() {
    let mut certified = 0;
    let mut witnesses = 0;
    for amp in [1, 3] { for asset in [MarketAsset::Base, MarketAsset::Quote] {
    for mode in [0, 1, 2] { for taxed in [false, true] { for indexed in [false, true] {
        let mut market = test_market(50_000, 50_000);
        market.config.swap_fee_bps = if indexed { 25 } else { 300 };
        market.config.amm.swap_fee_collect_mode = mode;
        if amp > 1 {
            market.config.amm.set_concentrated_curve_parameters(ConcentratedCurveParameters {
                peak_amplification_nad: amp * NAD, core_half_width_bps: 250, fade_width_bps: 500,
            }).unwrap();
            market.amm = Default::default(); market.risk = Default::default();
            market.prepare_amm_for_swap(0).unwrap(); market.refresh_risk().unwrap();
        }
        // Another position and a fractional interest index exercise the
        // position-vs-aggregate one-atom floor allowance in the certificate.
        let _other = seeded_position(&mut market, asset, 7, 100);
        let mut position = seeded_position(&mut market, asset, if indexed { 800 } else { 970 }, 1_000);
        if indexed {
            let debt_before = market.debt.isolated_debt(asset).unwrap();
            match asset {
                MarketAsset::Base => market.debt.base_borrow_index_nad = 1_200_000_001,
                MarketAsset::Quote => market.debt.quote_borrow_index_nad = 1_200_000_001,
            }
            let accrued = market.debt.isolated_debt(asset).unwrap() - debt_before;
            market.side_mut(asset).reserves.live_reserve += u64::try_from(accrued).unwrap();
        }
        position.margin_terms = LeverageMarginTerms::at_entry(50_000).unwrap();
        let fee = LeverageCollateralFee::new(taxed.then_some(TransferFee {
            epoch: 0.into(), maximum_fee: 3_u64.into(), transfer_fee_basis_points: 100_u16.into(),
        }));
        let p = LiquidationPositionRef::Leverage(&position);
        let health = market.flash_liquidation_health(&p, asset, fee).unwrap();
        if !health.emergency_allowed().unwrap() { continue; }
        let proof = market.proves_partial_bound(&p, asset, fee, fee, fee, 120, 0, Some(1_000)).unwrap();
        let recovery = (1..1_000).any(|size| market.clone()
            .prepare_emergency_candidate(&p, asset, size, false, fee, fee, fee, 120, 0, 0)
            .is_ok_and(|candidate| candidate.quote.health_after.recovered().unwrap()));
        if proof {certified += 1;assert!(!recovery,"amp={amp} asset={asset:?} mode={mode} taxed={taxed} indexed={indexed}");}
        if recovery {witnesses += 1;assert!(!proof);}
    }}}}}
    assert!(certified > 0); assert!(witnesses > 0);
}

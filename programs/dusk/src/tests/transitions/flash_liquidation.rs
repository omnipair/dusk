use super::*;
use crate::transitions::{flash_liquidation::LiquidationPositionRef, flash_settlement::LiquidationPositionMut};

#[test]
fn full_close_certificate_never_overrides_a_useful_integer_partial() {
    let fee = LeverageCollateralFee::default();
    let mut certified = 0;
    let mut useful = 0;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for reference in [100, 10_000] {
            for debt in 94..=99 {
                let market_template = test_market(50_000, 50_000);
                let mut market = market_template;
                let mut position = seeded_position(&mut market, asset, debt, 100);
                position.margin_terms.reference_collateral = reference;
                position.distress.observe(true, 0, 0).unwrap();
                let p = LiquidationPositionRef::Leverage(&position);
                let health = market.flash_liquidation_health(&p, asset, fee).unwrap();
                if !health.emergency_allowed().unwrap() { continue; }
                let proof = market.proves_no_useful_partial(&p, asset, fee, fee, fee, 120, 0).unwrap();
                let mut found = false;
                for size in 1..100 {
                    if let Ok(q) = market.quote_flash_liquidation(&p, asset, size, false, fee, fee, fee, 120, 0) {
                        assert!(health.improves(q.health_after).unwrap());
                        found = true;
                    }
                    if let Ok(prepared) = market.clone().prepare_emergency_liquidation(&p, asset, size, false, fee, fee, fee, 120, 0, 0) {
                        assert!(health.improves(prepared.quote.health_after).unwrap());
                        found = true;
                    }
                }
                if proof { certified += 1; assert!(!found, "false full-close permission: {asset:?}, reference {reference}, debt {debt}"); }
                if found { useful += 1; assert!(!proof); }
            }
        }
    }
    assert!(certified > 0);
    assert!(useful > 0);
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
        let finalized = market.apply_emergency_liquidation(&mut LiquidationPositionMut::Leverage(&mut position), asset,
            &mut prepared, LeverageCollateralFee::default(), 0, 0, 0, ProtocolAuctionSplit::default()).unwrap().unwrap();
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
        market.apply_emergency_liquidation(&mut LiquidationPositionMut::Leverage(&mut position), asset,
            &mut prepared, LeverageCollateralFee::default(), 0, 0, 0, ProtocolAuctionSplit::default()).unwrap();
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
        let loss = market.settle_flash_recovery(
            &mut LiquidationPositionMut::Leverage(&mut position), asset, allocation, q.collateral_debit,
            allocation.debt, 0, 0, false, LeverageCollateralFee::default(), 0, 0,
        ).unwrap();
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
        let loss = market.settle_flash_recovery(
            &mut LiquidationPositionMut::Leverage(&mut position), asset, allocation, 850_000,
            400_000, 375_000, 375_000, true, LeverageCollateralFee::default(), 1, 1,
        ).unwrap();
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
    let loss = market.settle_flash_recovery(
        &mut LiquidationPositionMut::Borrow(&mut position), MarketAsset::Base, allocation, 800,
        700, 0, 0, true, LeverageCollateralFee::default(), 1, 1,
    ).unwrap();
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
        let loss = market.settle_flash_recovery(&mut LiquidationPositionMut::Leverage(&mut position), asset,
            allocation, q.collateral_debit, q.repayment, 0, 0, true, fee, 0, 1).unwrap();
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
        let mut position = seeded_position(&mut market, MarketAsset::Quote, 995 * scale, 1_000 * scale);
        position.distress.observe(true, 0, 0).unwrap();
        assert!(market.proves_no_useful_partial(&LiquidationPositionRef::Leverage(&position), MarketAsset::Quote,
            fee, fee, fee, 120, 0).unwrap(), "scale {scale}");
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
fn partial_invoice_resizes_for_dust_without_granting_full_close_permission() {
    let mut market = test_market(5_000_000, 5_000_000);
    let position = seeded_position(&mut market, MarketAsset::Quote, 960_000, 1_000_000);
    market.config.liquidation.minimum_quote_debt = 940_000;
    let fee = LeverageCollateralFee::default();
    let p = LiquidationPositionRef::Leverage(&position);
    let q = market.quote_flash_liquidation(&p, MarketAsset::Quote, 100_000, false, fee, fee, fee, 0, 0).unwrap();
    assert_eq!(q.health_after.debt, 940_000);
    assert_eq!(q.repayment, 20_000);
    assert!(q.health_before.improves(q.health_after).unwrap());
    market.config.liquidation.minimum_quote_debt = 960_000;
    assert!(market.quote_flash_liquidation(&p, MarketAsset::Quote, 0, true, fee, fee, fee, 0, 0).is_err());
}

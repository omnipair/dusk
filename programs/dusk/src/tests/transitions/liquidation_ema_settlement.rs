use crate::transitions::{flash_liquidation::LiquidationPositionRef as Ref, flash_settlement::LiquidationPositionMut as Mut};

fn market(amp: u64, hlp: bool) -> Market {
    let mut m = calibration_market(100_000, amp);
    m.config.ema_half_life_ms = 60_000;
    m.config.curve_depth_ema_half_life_ms = 60_000;
    m.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
    if hlp {
        for side in [MarketAsset::Base, MarketAsset::Quote] {
            m.deposit_single_sided(side, 10_000 * CALIBRATION_UNIT, 1).unwrap();
        }
        m.finalize_amm_transition_and_observe_risk(1).unwrap();
    }
    m
}

#[test]
fn ema_emergency_quote_ignores_same_slot_spot_and_observed_depth_in_both_directions() {
    for amp in [1, 4, 10] { for hlp in [false, true] { for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut m = market(amp, hlp);
        let p = seeded_position(&mut m, asset, 1_100_000, 1_000_000);
        let fee = LeverageCollateralFee::default();
        let quote = |m: &Market| m.clone().prepare_emergency_liquidation(&Ref::Leverage(&p), asset,
            p.collateral_amount, true, fee, fee, fee, 120, 1, 2_000).unwrap().quote;
        let before = quote(&m);
        let spot_before = m.current_concentrated_spot_price_nad().unwrap().unwrap();
        let mut trade = SwapRequest { current_slot: 1, current_unix_timestamp: 120, asset_in: asset.opposite(),
            reserve_credit: 5_000_000, protocol_fee_bps: 2_000 }.prepare(&mut m).unwrap();
        trade.finalize_state(&mut m, 1, 2_000, ProtocolAuctionSplit::default()).unwrap();
        assert_ne!(spot_before, m.current_concentrated_spot_price_nad().unwrap().unwrap());
        m.risk.observed_curve_depth_nad /= 2;
        let after = quote(&m);
        assert_eq!(before, after, "amp={amp} hlp={hlp} side={asset:?}");
        let mut executed = m.clone();
        let mut next = p.clone();
        let mut prepared = executed.prepare_emergency_liquidation(&Ref::Leverage(&next), asset,
            p.collateral_amount, true, fee, fee, fee, 120, 1, 2_000).unwrap();
        executed.apply_emergency_liquidation(&mut Mut::Leverage(&mut next), asset, &mut prepared,
            fee, 120, 1, 2_000, ProtocolAuctionSplit::default()).unwrap();
        executed.assert_market_invariants().unwrap();
        assert_eq!(next.collateral_amount, 0);
        assert_eq!(next.debt_shares, 0);
        m.risk.curve_depth_ema_nad /= 2;
        let shallower = quote(&m);
        assert!(shallower.swap_output < after.swap_output);
        assert_eq!(shallower.health_before, after.health_before);
    } } }
}

#[test]
fn ema_emergency_concentration_hlp_fees_and_real_cash_settle_without_virtual_reserves() {
    let mut count = 0;
    for amp in [1, 4, 10] { for hlp in [false, true] { for asset in [MarketAsset::Base, MarketAsset::Quote] {
    for fee_mode in [0, 1, 2] { for compounded in [0, 5_000] { for full in [false, true] {
        let mut m = market(amp, hlp);
        m.config.amm.swap_fee_collect_mode = fee_mode;
        m.config.amm.compounding_fee_bps = compounded;
        let mut p = seeded_position(&mut m, asset, if full { 1_100_000 } else { 960_000 }, 1_000_000);
        let token_fee = LeverageCollateralFee::new(Some(TransferFee {
            epoch: 0.into(), maximum_fee: 300_u64.into(), transfer_fee_basis_points: 50_u16.into(),
        }));
        m.insurance.credit(asset, 100_000, 1).unwrap();
        let sold = if full { p.collateral_amount } else { 50_000 };
        let mut prepared = m.prepare_emergency_liquidation(&Ref::Leverage(&p), asset, sold, full,
            token_fee, token_fee, token_fee, 120, 1, 2_000).unwrap();
        let q = prepared.quote;
        let fee = prepared.swap.as_ref().unwrap().quote.fee;
        let cash = [m.base_side.reserves.cash_reserve, m.quote_side.reserves.cash_reserve];
        let cache = m.amm.concentrated_curve_cache;
        let protected_before = m.amm.spendable_protected_profit_nad();
        let finalized = m.apply_emergency_liquidation(&mut Mut::Leverage(&mut p), asset, &mut prepared,
            token_fee, 120, 1, 2_000, ProtocolAuctionSplit::default()).unwrap().unwrap();
        assert_eq!(p.collateral_amount, 1_000_000 - sold);
        assert_eq!(finalized.lifecycle.socialized_principal_loss, q.loss.principal_written_off);
        let fee_on_input = fee.fee_asset == asset.opposite().code();
        let debt_output_fee = if fee_on_input { 0 } else { fee.claimable_fee_debit + fee.retained_surcharge };
        let expected_debt_cash = cash[asset.code() as usize] + q.loss.insurance_credit
            - q.reward_debit - q.owner_debit - q.insurance_fee_debit - q.revenue_fee_debit
            - q.loss.interest_paid - debt_output_fee;
        assert_eq!(m.side(asset).reserves.cash_reserve, expected_debt_cash);
        let collateral_fee = if fee_on_input { fee.claimable_fee_debit + fee.retained_surcharge } else { 0 };
        assert_eq!(m.side(asset.opposite()).reserves.cash_reserve,
            cash[asset.opposite().code() as usize] + q.collateral_credit - collateral_fee);
        assert_ne!(cache, m.amm.concentrated_curve_cache);
        assert_eq!(m.amm.center_price_nad, NAD);
        assert!(m.amm.spendable_protected_profit_nad() <= protected_before);
        assert_eq!(m.risk.cached_spot_base_price_nad, m.current_concentrated_spot_price_nad().unwrap().unwrap());
        m.assert_market_invariants().unwrap();
        // A prepared reference plan is consumable exactly once.
        assert!(m.apply_emergency_liquidation(&mut Mut::Leverage(&mut p), asset, &mut prepared,
            token_fee, 120, 1, 2_000, ProtocolAuctionSplit::default()).is_err());
        count += 1;
    } } } } } }
    assert_eq!(count, 144);
}

#[test]
fn ema_health_and_reference_survive_proportional_lp_withdrawal() {
    for amp in [1, 4, 10] {
        let mut m = market(amp, false);
        let p = seeded_position(&mut m, MarketAsset::Quote, 800_000, 1_000_000);
        let before = m.flash_liquidation_health(&Ref::Leverage(&p), MarketAsset::Quote, LeverageCollateralFee::default()).unwrap();
        let (reference, state) = m.ema_liquidation_curve(MarketAsset::Base, 1).unwrap();
        let shares = m.base_side.shares.ylp_supply / 4;
        m.remove_liquidity(shares).unwrap();
        m.finalize_amm_transition_and_observe_risk(1).unwrap();
        let after = m.flash_liquidation_health(&Ref::Leverage(&p), MarketAsset::Quote, LeverageCollateralFee::default()).unwrap();
        assert_eq!(before, after);
        assert!(!after.eligible().unwrap());
        let (next, next_state) = m.ema_liquidation_curve(MarketAsset::Base, 1).unwrap();
        assert_eq!(state, next_state);
        assert_eq!(reference.amm.concentrated_curve_cache, next.amm.concentrated_curve_cache);
        // Time incorporates the withdrawn depth without changing price/health.
        m.last_update_slot = 151;
        let risk = m.current_risk().unwrap();
        assert!(risk.curve_depth_ema_nad < m.risk.curve_depth_ema_nad);
        assert!(risk.curve_depth_ema_nad > m.risk.observed_curve_depth_nad);
        assert_eq!(after, m.flash_liquidation_health(&Ref::Leverage(&p), MarketAsset::Quote,
            LeverageCollateralFee::default()).unwrap());
    }
}

#[test]
fn ema_reference_uses_collateral_direction_even_when_price_emas_are_not_reciprocals() {
    for amp in [1, 4, 10] {
        let mut m = market(amp, false);
        m.risk.base_price_ema_nad = 2 * NAD;
        m.risk.quote_price_ema_nad = NAD;
        // Keep this fixture's deliberately non-reciprocal EMA values at the
        // same initialized snapshot slot used by the liquidation reference.
        m.risk.last_snapshot_slot = 1;
        for (collateral, price) in [(MarketAsset::Base, 2 * NAD), (MarketAsset::Quote, NAD)] {
            let (view, state) = m.ema_liquidation_curve(collateral, 1).unwrap();
            let point = crate::transitions::amm::ConcentratedCurvePoint {
                base_reserve: state.ordinary_base, quote_reserve: state.ordinary_quote,
            };
            let base_price = view.current_concentrated_curve_geometry().unwrap().unwrap().spot_price_nad_prevalidated(point).unwrap();
            let directional = if collateral == MarketAsset::Base { base_price } else { u128::from(NAD).pow(2) / base_price };
            assert!(directional.abs_diff(u128::from(price)) <= 2);
        }
    }
}

#[test]
fn ema_emergency_pays_accrued_hlp_funding_from_real_cash_and_cancels_unpaid_loan_interest() {
    for amp in [1, 4, 10] { for asset in [MarketAsset::Base, MarketAsset::Quote] { for advanced in [false, true] {
        let mut m = market(amp, true);
        let mut p = seeded_position(&mut m, asset, 1_100_000, 1_000_000);
        let slot = 1_000_001;
        m.accrue_interest_to_slot(slot).unwrap();
        m.observe_current_risk(slot).unwrap();
        let held_protocol = if advanced {
            let (gross, lp) = m.fund_public_interest(asset, u64::MAX, 2_000).unwrap();
            assert!(gross > 0);
            gross - lp
        } else { 0 };
        let fee = LeverageCollateralFee::default();
        let mut prepared = m.prepare_emergency_liquidation(&Ref::Leverage(&p), asset, p.collateral_amount,
            true, fee, fee, fee, 400_000, slot, 2_000).unwrap();
        let q = prepared.quote;
        let f = prepared.swap.as_ref().unwrap().quote.fee;
        let before = m.side(asset).reserves.cash_reserve;
        let result = m.apply_emergency_liquidation(&mut Mut::Leverage(&mut p), asset, &mut prepared,
            fee, 400_000, slot, 2_000, ProtocolAuctionSplit::default()).unwrap().unwrap();
        let funding = if asset == MarketAsset::Base {result.quote_rebalance.interest_paid} else {result.base_rebalance.interest_paid};
        assert!(funding > 0);
        assert!(q.loss.interest_canceled > 0);
        assert_eq!(q.loss.interest_paid, 0);
        let output_fee = if f.fee_asset == asset.code() { f.claimable_fee_debit + f.retained_surcharge } else {0};
        assert_eq!(m.side(asset).reserves.cash_reserve,
            before + held_protocol - q.reward_debit - q.owner_debit - q.insurance_fee_debit - q.revenue_fee_debit - output_fee - funding);
        assert_eq!(p.debt_shares, 0);
        m.assert_market_invariants().unwrap();
        assert_eq!(m.side_mut(asset).fees.public_interest_advance.take_payment(0).unwrap(), (0, 0));
        assert_eq!(m.side(asset).fees.public_interest_advance, crate::state::PublicInterestAdvance::default());
    } } }
}

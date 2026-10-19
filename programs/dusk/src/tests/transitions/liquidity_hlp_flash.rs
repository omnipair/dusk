use super::*;

fn market_for_flash(amplification: u64, target: MarketAsset, funding_bps: u128) -> Market {
    let mut m = Market::default();
    m.base_side.asset_decimals = 6;
    m.quote_side.asset_decimals = 6;
    m.debt.base_borrow_index_nad = u128::from(NAD);
    m.debt.quote_borrow_index_nad = u128::from(NAD);
    m.add_liquidity(1_000_000_000_000, 1_000_000_000_000).unwrap();
    m.deposit_single_sided(MarketAsset::Base, 20_000_000_000, 1).unwrap();
    m.deposit_single_sided(MarketAsset::Quote, 20_000_000_000, 1).unwrap();
    m.config.amm.peak_amplification_nad = amplification * NAD;
    m.config.amm.core_half_width_bps = if amplification == 1 { 0 } else { 100 };
    m.config.amm.fade_width_bps = if amplification == 1 { 0 } else { 400 };
    m.config.swap_fee_bps = 30;
    m.amm = AmmState::default();
    m.prepare_amm_for_swap(1).unwrap();
    m.risk.base_price_ema_nad = NAD;
    m.risk.quote_price_ema_nad = NAD;
    match target {
        MarketAsset::Base => m.debt.quote_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
        MarketAsset::Quote => m.debt.base_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
    }
    m
}

#[test]
fn flash_recovery_preserves_other_holders_and_restores_hedge() {
    for amp in [1, 4, 10] {
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            for funding_bps in [1250, 2500, 7500] {
                let mut m = market_for_flash(amp, target, funding_bps);
                let fee = LeverageCollateralFee::default();
                let q = m.quote_hlp_flash_recovery(target, fee, fee).unwrap();
                let before = m.clone();
                let cash = m.side(target).reserves.cash_reserve;
                m.begin_hlp_flash_recovery(q).unwrap();
                assert_eq!(m.side(target).reserves.cash_reserve, cash - q.collateral_debit);
                assert_eq!(m.curve_reserves_nad().unwrap(), before.curve_reserves_nad().unwrap());
                assert!(m.deposit_single_sided(target, 1_000_000, 1).is_err());
                assert!(m.withdraw_single_sided(target, 1_000_000).is_err());
                let prices = current_hlp_curve_prices(&m).unwrap();
                let other = target.opposite();
                let other_nav = engine::hlp_share_nav_nad_with_prices(&m, other, prices).unwrap();
                let own_nav = engine::hlp_share_nav_nad_with_prices(&m, target, prices).unwrap();
                let expected = own_nav + engine::asset_value_in_target_nad_with_prices(&m, prices, other, q.reserve_credit, target).unwrap()
                    - m.normalize_amount(u128::from(q.collateral_debit), m.side(target).asset_decimals).unwrap();
                let ordinary = m.base_side.shares.ylp_supply - m.base_hlp_vault.ylp_shares - m.quote_hlp_vault.ylp_shares;
                let ordinary_value = |m: &Market| [MarketAsset::Base, MarketAsset::Quote].into_iter().map(|asset| {
                    engine::asset_value_in_target_nad_with_prices(m, prices, asset, engine::ylp_live_underlying_amount(m, asset, ordinary).unwrap(), MarketAsset::Quote).unwrap()
                }).sum::<u128>();
                let ordinary_before = ordinary_value(&m);
                m.settle_hlp_flash_recovery(q, 1).unwrap_or_else(|e| panic!("amp={amp} target={target:?} funding={funding_bps}: {e}"));
                assert!(engine::hlp_share_nav_nad_with_prices(&m, other, prices).unwrap().abs_diff(other_nav) < 100_000);
                assert!(engine::hlp_share_nav_nad_with_prices(&m, target, prices).unwrap().abs_diff(expected) < 100_000);
                assert!(ordinary_value(&m).abs_diff(ordinary_before) < 100_000);
                for t in [MarketAsset::Base, MarketAsset::Quote] {
                    let v = if t == MarketAsset::Base { m.base_hlp_vault } else { m.quote_hlp_vault };
                    let claim = engine::ylp_live_underlying_amount(&m, t.opposite(), v.ylp_shares).unwrap();
                    let debt = Debt::shares_to_debt(v.debt_shares, m.debt.borrow_index(t.opposite())).unwrap();
                    assert!(debt.abs_diff(u128::from(claim)) <= 1);
                    assert_eq!(m.side(t).reserves.flash_receivable, 0);
                    assert_eq!(v.flash_collateral_debit, 0);
                }
                m.assert_market_invariants().unwrap();
            }
        }
    }
}

#[test]
fn terminal_flash_closes_debt_and_uses_insurance_only_for_principal() {
    for amp in [1, 4, 10] {
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            for principal_loss in [false, true] {
                let mut m = market_for_flash(amp, target, 15_000);
                if principal_loss {
                    // Model an existing pooled principal writeoff, which has
                    // already reduced both asset claims without returning cash.
                    for asset in [MarketAsset::Base, MarketAsset::Quote] {
                        m.side_mut(asset).reserves.cash_reserve -= 800_000_000_000;
                        m.side_mut(asset).reserves.live_reserve -= 800_000_000_000;
                    }
                    m.rebase_concentrated_curve_after_terminal_hlp_loss().unwrap();
                }
                m.insurance.principal_coverage_bps = 7_500;
                m.insurance.per_day_draw_bps = 5_000;
                m.insurance.credit(target.opposite(), 10_000_000_000, 1).unwrap();
                let fee = LeverageCollateralFee::default();
                let q = m.quote_hlp_flash_recovery(target, fee, fee).unwrap();
                assert!(q.full);
                let v = if target == MarketAsset::Base { m.base_hlp_vault } else { m.quote_hlp_vault };
                let borrowed = target.opposite();
                let debt = Debt::shares_to_debt(v.debt_shares, m.debt.borrow_index(borrowed)).unwrap() as u64;
                let opposite = engine::ylp_live_underlying_amount(&m, borrowed, v.ylp_shares).unwrap();
                let expected = LiquidationLossAllocation::quote_with_coverage(v.debt_principal, debt,
                    opposite + q.reserve_credit, 5_000_000_000, 7_500).unwrap();
                m.begin_hlp_flash_recovery(q).unwrap();
                let r = m.settle_terminal_hlp_flash(q, fee, u64::MAX, u64::MAX, 1)
                    .unwrap_or_else(|e| panic!("amp={amp} target={target:?} loss={principal_loss}: {e}"));
                assert_eq!(r.loss, expected);
                assert_eq!(r.insurance_debit, expected.insurance_credit);
                assert_eq!(r.loss.principal_repaid + r.loss.insurance_credit + r.loss.principal_written_off, v.debt_principal);
                assert_eq!(r.loss.interest_paid + r.loss.interest_canceled, debt - v.debt_principal);
                if !principal_loss { assert_eq!(r.insurance_debit, 0); }
                let closed = if target == MarketAsset::Base { m.base_hlp_vault } else { m.quote_hlp_vault };
                assert_eq!(closed.hlp_supply, v.hlp_supply);
                assert_eq!(closed.ylp_shares, 0);
                assert_eq!(closed.debt_shares, 0);
                assert_eq!(closed.flash_collateral_debit, 0);
                assert_eq!(m.side(target).reserves.flash_receivable, 0);
                m.assert_market_invariants().unwrap();
            }
        }
    }
}

#[test]
fn flash_receivables_compose_with_same_market_swaps_and_independent_recovery() {
    use crate::transitions::amm::SwapRequest;
    use crate::transitions::liquidity::SwapCashPolicy;
    use crate::transitions::LeverageSwapFeeCredit;
    for amp in [1, 4, 10] {
        let mut m = market_for_flash(amp, MarketAsset::Base, 2500);
        m.debt.base_borrow_index_nad = u128::from(NAD) * 5 / 4;
        let fee = LeverageCollateralFee::default();
        let a = m.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).unwrap();
        let b = m.quote_hlp_flash_recovery(MarketAsset::Quote, fee, fee).unwrap();
        m.begin_hlp_flash_recovery(a).unwrap();
        m.begin_hlp_flash_recovery(b).unwrap();
        let mut swap = SwapRequest { current_slot: 1, current_unix_timestamp: 0,
            asset_in: MarketAsset::Base, reserve_credit: 1_000_000_000, protocol_fee_bps: 0,
        }.prepare(&mut m).unwrap();
        assert_eq!(swap.quote.recovery.bonus_output, 0, "no second incentive for an active flash recovery");
        let quote = swap.leverage_quote();
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(&quote, swap.quote.fee.claimable_fee_debit).unwrap();
        swap.apply(&mut m, SwapCashPolicy::Spot, credit, 1, 0, ProtocolAuctionSplit::default(), None).unwrap();
        let cash_after_swap = m.base_side.reserves.cash_reserve;
        m.settle_hlp_flash_recovery(a, 1).unwrap();
        assert_eq!(m.quote_hlp_vault.flash_collateral_debit, b.collateral_debit);
        m.settle_hlp_flash_recovery(b, 1).unwrap();
        assert!(m.base_side.reserves.cash_reserve >= cash_after_swap);
        assert_eq!(m.base_side.reserves.flash_receivable, 0);
        assert_eq!(m.quote_side.reserves.flash_receivable, 0);
        m.assert_market_invariants().unwrap();
    }
}

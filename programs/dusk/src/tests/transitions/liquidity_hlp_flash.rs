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
    m.observe_current_risk(1).unwrap();
    match target {
        MarketAsset::Base => m.debt.quote_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
        MarketAsset::Quote => m.debt.base_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
    }
    m
}

#[test]
fn terminal_hlp_native_recovery_uses_ema_price_smoothed_depth_and_fees() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageSwapFeeCredit};

    fn trade(market: &mut Market, input: MarketAsset, amount: u64, slot: u64) -> u64 {
        let mut prepared = SwapRequest {
            current_slot: slot,
            current_unix_timestamp: 0,
            asset_in: input,
            reserve_credit: amount,
            protocol_fee_bps: 0,
        }
        .prepare(market)
        .unwrap();
        let output = prepared.quote.amount_out;
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(
            &prepared.leverage_quote(),
            prepared.quote.fee.claimable_fee_debit,
        )
        .unwrap();
        prepared
            .apply(
                market,
                SwapCashPolicy::Spot,
                credit,
                slot,
                0,
                ProtocolAuctionSplit::default(),
                None,
            )
            .unwrap();
        output
    }

    let recovery = |market: &Market, target, target_claim, borrowed_claim| {
        let output = SwapRequest {
            current_slot: 1,
            current_unix_timestamp: 0,
            asset_in: target,
            reserve_credit: target_claim,
            protocol_fee_bps: 2_000,
        }
        .quote_ema_liquidation(market)
        .unwrap()
        .amount_out;
        borrowed_claim + output
    };

    for amplification in [1, 4, 10] {
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = market_for_flash(amplification, target, 15_000);
            let target_claim = 10_000_000_000;
            let borrowed_claim = 3_000_000_000;
            let baseline = recovery(&market, target, target_claim, borrowed_claim);
            let spot_before = market.current_concentrated_spot_price_nad().unwrap().unwrap();

            trade(&mut market, target, 50_000_000_000, 1);
            assert_ne!(spot_before, market.current_concentrated_spot_price_nad().unwrap().unwrap());
            assert_eq!(
                recovery(&market, target, target_claim, borrowed_claim),
                baseline,
                "same-slot spot changed terminal recovery: amp={amplification} target={target:?}"
            );

            let mut shallower = market.clone();
            shallower.risk.curve_depth_ema_nad /= 2;
            assert!(
                recovery(&shallower, target, target_claim, borrowed_claim) < baseline,
                "smoothed depth did not affect terminal recovery: amp={amplification} target={target:?}"
            );

            let mut fee_free = market.clone();
            fee_free.config.swap_fee_bps = 0;
            assert!(
                recovery(&fee_free, target, target_claim, borrowed_claim) > baseline,
                "full swap fee was not charged: amp={amplification} target={target:?}"
            );

            let vault = match target {
                MarketAsset::Base => market.base_hlp_vault,
                MarketAsset::Quote => market.quote_hlp_vault,
            };
            let actual_target_claim = engine::ylp_live_underlying_amount(&market, target, vault.ylp_shares).unwrap();
            let actual_borrowed_claim =
                engine::ylp_live_underlying_amount(&market, target.opposite(), vault.ylp_shares).unwrap();
            let expected = recovery(&market, target, actual_target_claim, actual_borrowed_claim);
            let plan = market
                .clone()
                .prepare_terminal_hlp_waterfall(
                    target,
                    u64::MAX,
                    LeverageCollateralFee::default(),
                    1,
                    0,
                    2_000,
                )
                .unwrap();
            assert_eq!(plan.reference_recovery(), expected);
        }
    }
}

#[test]
fn terminal_hlp_eligibility_refreshes_ema_to_the_current_slot() {
    let target = MarketAsset::Base;
    let mut market = market_for_flash(4, target, 5_000);
    let vault = market.base_hlp_vault;
    let borrowed = target.opposite();
    let debt = u64::try_from(Debt::shares_to_debt(
        vault.debt_shares,
        market.debt.borrow_index(borrowed),
    )
    .unwrap())
    .unwrap();
    let target_claim = engine::ylp_live_underlying_amount(&market, target, vault.ylp_shares).unwrap();
    let borrowed_claim = engine::ylp_live_underlying_amount(&market, borrowed, vault.ylp_shares).unwrap();

    // Model an old low-price snapshot whose cached observation has already
    // recovered. Advancing the market clock must refresh the EMA used by both
    // terminal paths even when no separate risk-observation instruction ran.
    market.risk.base_price_ema_nad = NAD / 2;
    market.risk.quote_price_ema_nad = 2 * NAD;
    market.risk.directional_base_price_ema_nad = NAD / 2;
    market.risk.directional_quote_price_ema_nad = 2 * NAD;
    market.risk.cached_spot_base_price_nad = NAD;
    market.risk.cached_spot_quote_price_nad = NAD;
    market.risk.last_snapshot_slot = 1;
    market.config.ema_half_life_ms = crate::constants::MIN_HALF_LIFE_MS;
    market.last_update_slot = 100_000;
    market.advance_amm_clock(100_000).unwrap();

    let stale_target_value = market
        .normalize_amount(u128::from(target_claim), market.side(target).asset_decimals)
        .unwrap()
        * u128::from(NAD / 2)
        / u128::from(NAD);
    let stale_target_value = market
        .denormalize_amount_floor(stale_target_value, market.side(borrowed).asset_decimals)
        .unwrap();
    assert!(borrowed_claim + stale_target_value <= debt);

    let refreshed = market
        .hlp_ema_collateral_value_in_borrowed(target, target_claim, borrowed_claim, 100_000)
        .unwrap();
    assert!(refreshed > debt);
    let fee = LeverageCollateralFee::default();
    assert!(market
        .prepare_terminal_hlp_waterfall(target, u64::MAX, fee, 100_000, 0, 0)
        .is_err());
    assert!(!market.quote_hlp_flash_recovery(target, fee, fee).unwrap().full);
}

#[test]
fn solvent_vault_can_get_funded_rescue_before_critical_gap() {
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = market_for_flash(4, target, 100);
        let fee = LeverageCollateralFee::default();
        let early = market.quote_hlp_flash_recovery(target, fee, fee).unwrap();
        assert!(!early.full);
        assert!(early.payment > 0 && early.collateral_debit > 0);
        market.amm.frozen_hlp_mask = if target == MarketAsset::Base { 1 } else { 2 };
        let quote = market.quote_hlp_flash_recovery(target, fee, fee).unwrap();
        assert!(!quote.full);
        assert!(quote.payment > 0 && quote.collateral_debit > 0);

        let mut fully_backed = market_for_flash(4, target, 0);
        fully_backed.amm.frozen_hlp_mask = market.amm.frozen_hlp_mask;
        let quote = fully_backed.quote_hlp_flash_recovery(target, fee, fee).unwrap();
        assert!(!quote.full);
        assert!(quote.payment > 0 && quote.collateral_debit > 0);
        fully_backed.begin_hlp_flash_recovery(quote).unwrap();
        fully_backed.settle_hlp_flash_recovery(quote, 2).unwrap();
        assert_eq!(fully_backed.amm.frozen_hlp_mask, 0);
        fully_backed.assert_market_invariants().unwrap();
    }
}

#[test]
fn flash_recovery_preserves_other_holders_and_restores_hedge() {
    for amp in [1, 4, 10] {
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            for funding_bps in [50, 100, 500, 1250, 2500, 7500] {
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
                let expected = LiquidationLossAllocation::quote_with_coverage(v.debt_principal, debt,
                    q.borrowed_claim + q.reserve_credit, 5_000_000_000, 7_500).unwrap();
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
fn terminal_flash_composes_with_same_market_swap() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageSwapFeeCredit};

    for amp in [1, 4, 10] {
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = market_for_flash(amp, target, 15_000);
            let fee = LeverageCollateralFee::default();
            let quote = market.quote_hlp_flash_recovery(target, fee, fee).unwrap();
            assert!(quote.full);
            let vault = if target == MarketAsset::Base {
                market.base_hlp_vault
            } else {
                market.quote_hlp_vault
            };
            let debt = u64::try_from(Debt::shares_to_debt(
                vault.debt_shares,
                market.debt.borrow_index(target.opposite()),
            )
            .unwrap())
            .unwrap();
            let expected_loss = LiquidationLossAllocation::quote_with_coverage(
                vault.debt_principal,
                debt,
                quote.borrowed_claim + quote.reserve_credit,
                0,
                market.insurance.principal_coverage_bps,
            )
            .unwrap();
            market.begin_hlp_flash_recovery(quote).unwrap();
            assert!(market.hlp_frozen(target));

            // Model a liquidator routing part of the released collateral back
            // through Dusk before delivering the fixed borrowed-token payment.
            let mut swap = SwapRequest {
                current_slot: 1,
                current_unix_timestamp: 0,
                asset_in: target,
                reserve_credit: 1_000_000_000,
                protocol_fee_bps: 0,
            }
            .prepare(&mut market)
            .unwrap();
            let credit = LeverageSwapFeeCredit::from_total_actual_credit(
                &swap.leverage_quote(),
                swap.quote.fee.claimable_fee_debit,
            )
            .unwrap();
            swap.apply(
                &mut market,
                SwapCashPolicy::Spot,
                credit,
                1,
                0,
                ProtocolAuctionSplit::default(),
                None,
            )
            .unwrap();

            let settlement = market
                .settle_terminal_hlp_flash(quote, fee, u64::MAX, u64::MAX, 1)
                .unwrap_or_else(|error| panic!("amp={amp} target={target:?}: {error}"));
            assert_eq!(settlement.loss, expected_loss);
            assert_eq!(market.side(target).reserves.flash_receivable, 0);
            assert!(!market.hlp_frozen(target));
            market.assert_market_invariants().unwrap();
        }
    }
}

#[test]
fn terminal_flash_clears_frozen_vault_before_receipt_exit() {
    let mut market = market_for_flash(4, MarketAsset::Base, 15_000);
    market.freeze_negative_hlp_vaults().unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 1);
    let fee = LeverageCollateralFee::default();
    let quote = market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).unwrap();
    assert!(quote.full);
    let receipt_supply = market.base_hlp_vault.hlp_supply;
    market.begin_hlp_flash_recovery(quote).unwrap();
    market.settle_terminal_hlp_flash(quote, fee, u64::MAX, u64::MAX, 2).unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 0);
    let exit = market.withdraw_single_sided(MarketAsset::Base, receipt_supply).unwrap();
    assert_eq!(exit.target_amount_out, 0);
    assert_eq!(exit.hlp_supply, 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn solvent_recovery_of_frozen_vault_must_repay_debt_and_unfreeze() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageSwapFeeCredit};

    for amp in [1, 4, 10] {
    let mut market = market_for_flash(amp, MarketAsset::Base, 10_500);
    market.freeze_negative_hlp_vaults().unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 1);
    let mut trade = SwapRequest { current_slot: 2, current_unix_timestamp: 0,
        asset_in: MarketAsset::Quote, reserve_credit: if amp == 1 { 300_000_000_000 } else { 500_000_000_000 }, protocol_fee_bps: 0 }
        .prepare(&mut market).unwrap();
    let credit = LeverageSwapFeeCredit::from_total_actual_credit(&trade.leverage_quote(),
        trade.quote.fee.claimable_fee_debit).unwrap();
    trade.apply(&mut market, SwapCashPolicy::Spot, credit, 2, 0,
        ProtocolAuctionSplit::default(), None).unwrap();
    let fee = LeverageCollateralFee::default();
    let quote = market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee)
        .unwrap_or_else(|e| panic!("amp={amp}: {e}"));
    assert!(!quote.full);
    let prices = current_hlp_curve_prices(&market).unwrap();
    let ordinary_shares = market.base_side.shares.ylp_supply
        - market.base_hlp_vault.ylp_shares - market.quote_hlp_vault.ylp_shares;
    let ordinary_value = |m: &Market| [MarketAsset::Base, MarketAsset::Quote].into_iter().map(|asset| {
        engine::asset_value_in_target_nad_with_prices(m, prices, asset,
            engine::ylp_live_underlying_amount(m, asset, ordinary_shares).unwrap(), MarketAsset::Quote).unwrap()
    }).sum::<u128>();
    let ordinary_before = ordinary_value(&market);
    let healthy_nav_before = engine::hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap();
    let debt_before = Debt::shares_to_debt(market.base_hlp_vault.debt_shares,
        market.debt.quote_borrow_index_nad).unwrap();
    market.begin_hlp_flash_recovery(quote).unwrap();
    market.settle_hlp_flash_recovery(quote, 2).unwrap();
    assert!(ordinary_value(&market).abs_diff(ordinary_before) < 100_000);
    let healthy_nav_after = engine::hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap();
    assert!(healthy_nav_after.abs_diff(healthy_nav_before) < 100_000);
    let debt_after = Debt::shares_to_debt(market.base_hlp_vault.debt_shares,
        market.debt.quote_borrow_index_nad).unwrap();
    assert!(debt_after < debt_before);
    assert_eq!(market.amm.frozen_hlp_mask, 0);
    let exit = market.withdraw_single_sided(MarketAsset::Base, market.base_hlp_vault.hlp_supply / 10).unwrap();
    assert!(exit.target_amount_out > 0);
    market.assert_market_invariants().unwrap();
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

#[test]
fn spot_swap_carries_hlp_interest_when_cash_only_covers_trader_output() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageSwapFeeCredit};

    for amp in [1, 4] {
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            for compounding_bps in [0, 10_000] {
            let mut market = market_for_flash(amp, target, 100);
            market.config.amm.compounding_fee_bps = compounding_bps;
            let borrowed = target.opposite();
            let principal = 1_019_000_000_000_u64;
            match borrowed {
                MarketAsset::Base => {
                    market.debt.fixed_base_shares = u128::from(principal);
                    market.debt.fixed_base_principal = principal;
                }
                MarketAsset::Quote => {
                    market.debt.fixed_quote_shares = u128::from(principal);
                    market.debt.fixed_quote_principal = principal;
                }
            }
            let public_interest = market.unrealized_interest(borrowed).unwrap() as u64;
            market.side_mut(borrowed).reserves.cash_reserve -= principal;
            market.side_mut(borrowed).reserves.live_reserve += public_interest;
            market.assert_market_invariants().unwrap();
            let vault_before = if target == MarketAsset::Base { market.base_hlp_vault } else { market.quote_hlp_vault };
            let debt_before = Debt::shares_to_debt(vault_before.debt_shares, market.debt.borrow_index(borrowed)).unwrap();
            let interest_before = debt_before - u128::from(vault_before.debt_principal);
            let mut swap = SwapRequest {
                current_slot: 1,
                current_unix_timestamp: 0,
                asset_in: target,
                reserve_credit: 900_000_000,
                protocol_fee_bps: 0,
            }
            .prepare(&mut market)
            .unwrap_or_else(|e| panic!("amp={amp} target={target:?} prepare: {e}"));
            let quote = swap.leverage_quote();
            let credit = LeverageSwapFeeCredit::from_total_actual_credit(&quote, swap.quote.fee.claimable_fee_debit).unwrap();
            let output = swap.quote.gross_amount_out;
            let trader_output = swap.quote.amount_out;
            let cash_before = market.side(borrowed).reserves.cash_reserve;
            assert!(cash_before >= output);
            assert!(cash_before - output < interest_before as u64,
                "amp={amp} target={target:?} cash={cash_before} output={output} interest={interest_before}");
            let result = swap.apply(&mut market, SwapCashPolicy::Spot, credit, 1, 0,
                ProtocolAuctionSplit::default(), None)
                .unwrap_or_else(|e| panic!("amp={amp} target={target:?} apply: {e}"));
            let vault_after = if target == MarketAsset::Base { market.base_hlp_vault } else { market.quote_hlp_vault };
            let debt_after = Debt::shares_to_debt(vault_after.debt_shares, market.debt.borrow_index(borrowed)).unwrap();
            assert_eq!(debt_after - u128::from(vault_after.debt_principal), interest_before);
            assert_eq!(market.side(borrowed).reserves.cash_reserve, cash_before - output);
            assert_eq!(result.base_rebalance.interest_paid + result.quote_rebalance.interest_paid, 0);
            market.assert_market_invariants().unwrap();

            // A keeper can instead supply the missing token atomically while
            // the original cash shortage persists.
            let mut rescued = market.clone();
            let fee = LeverageCollateralFee::default();
            let rescue = rescued.quote_hlp_flash_recovery(target, fee, fee).unwrap();
            assert!(!rescue.full);
            rescued.begin_hlp_flash_recovery(rescue).unwrap();
            rescued.settle_hlp_flash_recovery(rescue, 2).unwrap();
            let rescued_vault = if target == MarketAsset::Base { rescued.base_hlp_vault } else { rescued.quote_hlp_vault };
            let rescued_debt = Debt::shares_to_debt(rescued_vault.debt_shares, rescued.debt.borrow_index(borrowed)).unwrap();
            assert_eq!(rescued_debt, u128::from(rescued_vault.debt_principal));
            rescued.assert_market_invariants().unwrap();

            // Crossing the cash threshold must not let a same-slot trader
            // extract target tokens by reversing the ordinary spot swap.
            let mut reversed = market.clone();
            let mut reverse = SwapRequest {
                current_slot: 1,
                current_unix_timestamp: 0,
                asset_in: borrowed,
                reserve_credit: trader_output,
                protocol_fee_bps: 0,
            }
            .prepare(&mut reversed).unwrap();
            let reverse_user_output = reverse.quote.amount_out;
            let reverse_quote = reverse.leverage_quote();
            let reverse_credit = LeverageSwapFeeCredit::from_total_actual_credit(
                &reverse_quote, reverse.quote.fee.claimable_fee_debit,
            ).unwrap();
            reverse.apply(&mut reversed, SwapCashPolicy::Spot, reverse_credit, 1, 0,
                ProtocolAuctionSplit::default(), None).unwrap();
            assert!(reverse_user_output <= 900_000_000,
                "amp={amp} target={target:?} compounding={compounding_bps} round trip output={reverse_user_output}");
            reversed.assert_market_invariants().unwrap();

            // A later trade replenishes the borrowed asset. Interest remains
            // indexed until a subsequent safe settlement pays it once.
            let mut replenish = SwapRequest {
                current_slot: 2,
                current_unix_timestamp: 0,
                asset_in: borrowed,
                reserve_credit: 1_000_000_000,
                protocol_fee_bps: 0,
            }
            .prepare(&mut market)
            .unwrap_or_else(|e| panic!("amp={amp} target={target:?} replenish prepare: {e}"));
            let replenish_quote = replenish.leverage_quote();
            let replenish_credit = LeverageSwapFeeCredit::from_total_actual_credit(
                &replenish_quote, replenish.quote.fee.claimable_fee_debit,
            ).unwrap();
            replenish.apply(&mut market, SwapCashPolicy::Spot, replenish_credit, 2, 0,
                ProtocolAuctionSplit::default(), None)
                .unwrap_or_else(|e| panic!("amp={amp} target={target:?} replenish apply: {e}"));
            market.assert_market_invariants().unwrap();

            // Inventory and writeoff flows rebuild the hLP endpoint from an
            // already materialized market instead of consuming the swap plan.
            let mut rebuilt = market.clone();
            let current = crate::transitions::liquidity::prepare_concentrated_hlp_transition_at_current_state(&rebuilt)
                .unwrap_or_else(|e| panic!("amp={amp} target={target:?} rebuild: {e}"));
            current.consume(&mut rebuilt)
                .unwrap_or_else(|e| panic!("amp={amp} target={target:?} rebuild consume: {e}"));
            rebuilt.assert_market_invariants().unwrap();

            let mut settle = SwapRequest {
                current_slot: 3,
                current_unix_timestamp: 0,
                asset_in: target,
                reserve_credit: 100_000_000,
                protocol_fee_bps: 0,
            }
            .prepare(&mut market)
            .unwrap_or_else(|e| panic!("amp={amp} target={target:?} settle prepare: {e}"));
            let settle_quote = settle.leverage_quote();
            let settle_credit = LeverageSwapFeeCredit::from_total_actual_credit(
                &settle_quote, settle.quote.fee.claimable_fee_debit,
            ).unwrap();
            let settled = settle.apply(&mut market, SwapCashPolicy::Spot, settle_credit, 3, 0,
                ProtocolAuctionSplit::default(), None)
                .unwrap_or_else(|e| panic!("amp={amp} target={target:?} settle apply: {e}"));
            assert!(settled.base_rebalance.interest_paid + settled.quote_rebalance.interest_paid > 0);
            let final_vault = if target == MarketAsset::Base { market.base_hlp_vault } else { market.quote_hlp_vault };
            let final_debt = Debt::shares_to_debt(final_vault.debt_shares, market.debt.borrow_index(borrowed)).unwrap();
            assert_eq!(final_debt, u128::from(final_vault.debt_principal));
            market.assert_market_invariants().unwrap();
            }
        }
    }
}

#[test]
fn cash_constrained_swap_carries_both_hlp_funding_liabilities() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageSwapFeeCredit};

    for amp in [1, 4] {
        let mut market = market_for_flash(amp, MarketAsset::Base, 100);
        market.debt.base_borrow_index_nad = u128::from(NAD) * 101 / 100;
        let principal = 1_019_000_000_000_u64;
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            match asset {
                MarketAsset::Base => {
                    market.debt.fixed_base_shares = u128::from(principal);
                    market.debt.fixed_base_principal = principal;
                }
                MarketAsset::Quote => {
                    market.debt.fixed_quote_shares = u128::from(principal);
                    market.debt.fixed_quote_principal = principal;
                }
            }
            let public_interest = market.unrealized_interest(asset).unwrap() as u64;
            market.side_mut(asset).reserves.cash_reserve -= principal;
            market.side_mut(asset).reserves.live_reserve += public_interest;
        }
        market.assert_market_invariants().unwrap();
        let due_before = [MarketAsset::Base, MarketAsset::Quote].map(|target| {
            let vault = if target == MarketAsset::Base { market.base_hlp_vault } else { market.quote_hlp_vault };
            Debt::shares_to_debt(vault.debt_shares, market.debt.borrow_index(target.opposite())).unwrap()
                - u128::from(vault.debt_principal)
        });
        let mut swap = SwapRequest {
            current_slot: 1,
            current_unix_timestamp: 0,
            asset_in: MarketAsset::Base,
            reserve_credit: 900_000_000,
            protocol_fee_bps: 0,
        }
        .prepare(&mut market).unwrap();
        let quote = swap.leverage_quote();
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(&quote, swap.quote.fee.claimable_fee_debit).unwrap();
        let result = swap.apply(&mut market, SwapCashPolicy::Spot, credit, 1, 0,
            ProtocolAuctionSplit::default(), None).unwrap();
        assert_eq!(result.base_rebalance.interest_paid, 0);
        assert_eq!(result.quote_rebalance.interest_paid, 0);
        for (i, target) in [MarketAsset::Base, MarketAsset::Quote].into_iter().enumerate() {
            let vault = if target == MarketAsset::Base { market.base_hlp_vault } else { market.quote_hlp_vault };
            let due = Debt::shares_to_debt(vault.debt_shares, market.debt.borrow_index(target.opposite())).unwrap()
                - u128::from(vault.debt_principal);
            assert_eq!(due, due_before[i]);
        }
        market.assert_market_invariants().unwrap();
    }
}

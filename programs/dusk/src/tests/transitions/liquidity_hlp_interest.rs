#[test]
fn h_lp_entry_accepts_full_value_public_interest_in_both_assets_and_curves() {
    use crate::state::{AmmConfig, AmmState};
    const UNIT: u64 = 1_000_000;
    let mut failures = Vec::new();
    for amplification in [1, 4] {
        for interest_asset in [MarketAsset::Base, MarketAsset::Quote] {
            for target in [MarketAsset::Base, MarketAsset::Quote] {
                let mut market = Market::default();
                market.base_side.asset_decimals = 6;
                market.quote_side.asset_decimals = 6;
                market.debt.base_borrow_index_nad = NAD as u128;
                market.debt.quote_borrow_index_nad = NAD as u128;
                market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
                match interest_asset {
                    MarketAsset::Base => {
                        market.debt.fixed_base_shares = u128::from(425_000 * UNIT);
                        market.debt.fixed_base_principal = 400_000 * UNIT;
                    }
                    MarketAsset::Quote => {
                        market.debt.fixed_quote_shares = u128::from(425_000 * UNIT);
                        market.debt.fixed_quote_principal = 400_000 * UNIT;
                    }
                }
                market.side_mut(interest_asset).reserves.cash_reserve -= 400_000 * UNIT;
                market.side_mut(interest_asset).reserves.live_reserve += 25_000 * UNIT;
                    market.sync_income_receivable(interest_asset, false, 0).unwrap();
                market.config.amm = AmmConfig {
                    peak_amplification_nad: amplification * NAD,
                    core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                    fade_width_bps: if amplification == 1 { 0 } else { 400 },
                    center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                    ..AmmConfig::default()
                };
                *market.amm = AmmState::default();
                market.risk.base_price_ema_nad = NAD;
                market.risk.quote_price_ema_nad = NAD;
                market.risk.directional_base_price_ema_nad = NAD;
                market.risk.directional_quote_price_ema_nad = NAD;
                market.prepare_amm_for_swap(1).unwrap();
                market.assert_market_invariants().unwrap();
                let before = market.clone();
                match market.deposit_single_sided(target, 20_000 * UNIT, 1) {
                    Ok(receipt) => {
                        assert!(receipt.hlp_amount > 0);
                        market.assert_market_invariants().unwrap();
                        assert_eq!(
                            market.unrealized_interest(interest_asset).unwrap(),
                            before.unrealized_interest(interest_asset).unwrap()
                        );
                    }
                    Err(error) => failures.push(format!(
                        "amp={amplification} interest={interest_asset:?} target={target:?}: {error}"
                    )),
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn full_value_hlp_swap_rehedges_without_losing_unpaid_interest() {
    use crate::state::{AmmConfig, AmmState};
    use crate::transitions::{amm::SwapRequest, LeverageSwapFeeCredit};
    const UNIT: u64 = 1_000_000;
    for (fee_bps, compounding_bps) in [(0, 0), (30, 0), (30, 10_000)] {
        for amplification in [1, 4] {
            for interest_asset in [MarketAsset::Base, MarketAsset::Quote] {
                for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
                    let mut market = Market::default();
                    market.base_side.asset_decimals = 6;
                    market.quote_side.asset_decimals = 6;
                    market.debt.base_borrow_index_nad = NAD as u128;
                    market.debt.quote_borrow_index_nad = NAD as u128;
                    market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
                    market
                        .deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1)
                        .unwrap();
                    market
                        .deposit_single_sided(MarketAsset::Quote, 20_000 * UNIT, 1)
                        .unwrap();
                    let plain_price = current_hlp_curve_prices(&market).unwrap();
                    match interest_asset {
                        MarketAsset::Base => {
                            market.debt.fixed_base_shares = 425_000 * u128::from(UNIT);
                            market.debt.fixed_base_principal = 400_000 * UNIT;
                        }
                        MarketAsset::Quote => {
                            market.debt.fixed_quote_shares = 425_000 * u128::from(UNIT);
                            market.debt.fixed_quote_principal = 400_000 * UNIT;
                        }
                    }
                    market.side_mut(interest_asset).reserves.cash_reserve -= 400_000 * UNIT;
                    market.side_mut(interest_asset).reserves.live_reserve += 25_000 * UNIT;
                    market.sync_income_receivable(interest_asset, false, 0).unwrap();
                    assert_eq!(plain_price, current_hlp_curve_prices(&market).unwrap());
                    market.config.swap_fee_bps = fee_bps;
                    market.config.amm = AmmConfig {
                        compounding_fee_bps: compounding_bps,
                        peak_amplification_nad: amplification * NAD,
                        core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                        fade_width_bps: if amplification == 1 { 0 } else { 400 },
                        center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                        ..AmmConfig::default()
                    };
                    market.risk.base_price_ema_nad = NAD;
                    market.risk.quote_price_ema_nad = NAD;
                    market.risk.directional_base_price_ema_nad = NAD;
                    market.risk.directional_quote_price_ema_nad = NAD;
                    *market.amm = AmmState::default();
                    market.prepare_amm_for_swap(1).unwrap();
                    let start = market.integrated_curve_state_nad().unwrap();
                    let eligible_supply = market.tranches.senior_reserve_shares(market.base_side.shares.ylp_supply).unwrap();
                    let eligible_shares = [market.base_hlp_vault.ylp_shares, market.quote_hlp_vault.ylp_shares];
                    let before_cash = (
                        market.base_side.reserves.cash_reserve,
                        market.quote_side.reserves.cash_reserve,
                    );
                    let amount_in = 10_000 * UNIT;
                    let mut prepared = SwapRequest {
                        current_slot: 1,
                        current_unix_timestamp: 0,
                        asset_in,
                        reserve_credit: amount_in,
                        protocol_fee_bps: 0,
                    }
                    .prepare(&mut market)
                    .unwrap();
                    let amm_quote = prepared.quote;
                    let quote = prepared.leverage_quote();
                    let credit =
                        LeverageSwapFeeCredit::from_total_actual_credit(&quote, prepared.quote.fee.claimable_fee_debit)
                            .unwrap();
                    prepared
                        .apply(
                            &mut market,
                            SwapCashPolicy::Spot,
                            credit,
                            1,
                            0,
                            ProtocolAuctionSplit::default(),
                            None,
                        )
                        .unwrap();
                    market.assert_market_invariants().unwrap();
                    let reconstructed_price = market.current_concentrated_spot_price_nad().unwrap().unwrap();
                    assert!(reconstructed_price.abs_diff(amm_quote.reserve_end_price_nad) <= 10,
                        "quoted endpoint differs from settled price: amp={amplification} interest={interest_asset:?} in={asset_in:?} fee={fee_bps} compound={compounding_bps}: quote={}, settled={reconstructed_price}", amm_quote.reserve_end_price_nad);
                    for (side, before) in [(MarketAsset::Base, before_cash.0), (MarketAsset::Quote, before_cash.1)] {
                        let trader_flow = if side == asset_in {
                            i128::from(amm_quote.fee.amount_in_for_quote)
                        } else {
                            -i128::from(amm_quote.gross_amount_out)
                        };
                        let compounded = if side.code() == amm_quote.fee.fee_asset {
                            amm_quote.fee.compounded_fee_debit
                        } else {
                            0
                        };
                        assert_eq!(
                            i128::from(market.side(side).reserves.cash_reserve),
                            i128::from(before) + trader_flow + i128::from(compounded)
                        );
                    }
                    assert_eq!(
                        market.unrealized_interest(interest_asset).unwrap(),
                        u128::from(25_000 * UNIT)
                    );
                    for (target, starting_nav) in [
                        (MarketAsset::Base, start.base_hlp_equity + start.hlp_unpaid[0][0]),
                        (MarketAsset::Quote, start.quote_hlp_equity + start.hlp_unpaid[1][1]),
                    ] {
                        let fee_share_nad = market
                            .normalize_amount(u128::from(amm_quote.fee.compounded_fee_debit.div_ceil(2)), 6)
                            .unwrap()
                            * u128::from(eligible_shares[target.code() as usize])
                            / u128::from(eligible_supply);
                        let fee_value_nad = if target.code() == amm_quote.fee.fee_asset {
                            fee_share_nad
                        } else if target == MarketAsset::Base {
                            fee_share_nad * u128::from(NAD) / u128::from(amm_quote.end_price_nad)
                        } else {
                            fee_share_nad * u128::from(amm_quote.end_price_nad) / u128::from(NAD)
                        };
                        let expected_nav = starting_nav + fee_value_nad;
                        let vault = match target {
                            MarketAsset::Base => &market.base_hlp_vault,
                            MarketAsset::Quote => &market.quote_hlp_vault,
                        };
                        let claim = hlp_full_underlying_amount(&market, target, target.opposite()).unwrap();
                        let debt = Debt::shares_to_debt(vault.debt_shares, market.debt.borrow_index(target.opposite()))
                            .unwrap();
                        assert!(u128::from(claim).abs_diff(debt) <= 1);
                        let nav =
                            hlp_share_nav_nad_with_prices(&market, target, current_hlp_curve_prices(&market).unwrap())
                                .unwrap();
                        assert!(nav.abs_diff(expected_nav) <= 10_000, "fee={fee_bps} compound={compounding_bps} amp={amplification} interest={interest_asset:?} in={asset_in:?} target={target:?}: before={expected_nav}, after={nav}");
                        let mut exit_market = market.clone();
                        let exit = exit_market.withdraw_single_sided(target, vault.hlp_supply / 2).unwrap();
                        assert!(exit.target_amount_out > 0);
                        exit_market.assert_market_invariants().unwrap();
                    }
                }
            }
        }
    }
}

#[test]
fn h_lp_positive_book_value_recovers_when_curve_only_nav_is_negative() {
    use crate::state::{AmmConfig, AmmState};
    const UNIT: u64 = 1_000_000;
    for amplification in [1, 4] {
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = Market::default();
            market.base_side.asset_decimals = 6;
            market.quote_side.asset_decimals = 6;
            market.debt.base_borrow_index_nad = NAD as u128;
            market.debt.quote_borrow_index_nad = NAD as u128;
            market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
            market.deposit_single_sided(target, 20_000 * UNIT, 1).unwrap();
            match target {
                MarketAsset::Base => {
                    market.debt.fixed_base_shares = 400_000 * u128::from(UNIT);
                    market.debt.fixed_base_principal = 400_000 * UNIT;
                    market.debt.base_borrow_index_nad = 20 * u128::from(NAD);
                    market.debt.quote_borrow_index_nad = 5 * u128::from(NAD);
                }
                MarketAsset::Quote => {
                    market.debt.fixed_quote_shares = 400_000 * u128::from(UNIT);
                    market.debt.fixed_quote_principal = 400_000 * UNIT;
                    market.debt.quote_borrow_index_nad = 20 * u128::from(NAD);
                    market.debt.base_borrow_index_nad = 5 * u128::from(NAD);
                }
            }
            market.side_mut(target).reserves.cash_reserve -= 400_000 * UNIT;
            market.side_mut(target).reserves.live_reserve += 7_600_000 * UNIT;
            market.sync_income_receivable(target, false, 0).unwrap();
            market.sync_income_receivable(target.opposite(), true, 0).unwrap();
            market.config.amm = AmmConfig {
                peak_amplification_nad: amplification * NAD,
                core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                fade_width_bps: if amplification == 1 { 0 } else { 400 },
                center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                ..AmmConfig::default()
            };
            *market.amm = AmmState::default();
            market.prepare_amm_for_swap(1).unwrap();
            let economic =
                hlp_share_nav_nad_with_prices(&market, target, current_hlp_curve_prices(&market).unwrap()).unwrap();
            assert!(economic > 0);
            let before_cash = market.side(target.opposite()).reserves.cash_reserve;
            let plan = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap();
            plan.consume(&mut market).unwrap();
            assert_eq!(
                before_cash - market.side(target.opposite()).reserves.cash_reserve,
                80_000 * UNIT
            );
            market.assert_market_invariants().unwrap();
            let after =
                hlp_share_nav_nad_with_prices(&market, target, current_hlp_curve_prices(&market).unwrap()).unwrap();
            assert!(
                after.abs_diff(economic) <= 10_000,
                "amp={amplification} target={target:?}: economic {economic} -> {after}"
            );
            let vault = match target {
                MarketAsset::Base => &market.base_hlp_vault,
                MarketAsset::Quote => &market.quote_hlp_vault,
            };
            let claim = hlp_full_underlying_amount(&market, target, target.opposite()).unwrap();
            let debt = Debt::shares_to_debt(vault.debt_shares, market.debt.borrow_index(target.opposite())).unwrap();
            assert!(u128::from(claim).abs_diff(debt) <= 1);
        }
    }
}

#[test]
fn public_repayment_and_writeoff_update_both_hlp_values_once() {
    use crate::state::{AmmConfig, AmmState, BorrowPosition};
    use crate::transitions::{
        flash_liquidation::LiquidationPositionRef, flash_settlement::LiquidationPositionMut,
    };
    const UNIT: u64 = 1_000_000;
    for amplification in [1, 4] {
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            for writeoff in [false, true] {
              for advance in [false, true] {
                let mut market = Market::default();
                market.base_side.asset_decimals = 6;
                market.quote_side.asset_decimals = 6;
                market.debt.base_borrow_index_nad = NAD as u128;
                market.debt.quote_borrow_index_nad = NAD as u128;
                market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
                for target in [MarketAsset::Base, MarketAsset::Quote] {
                    market.deposit_single_sided(target, 20_000 * UNIT, 1).unwrap();
                }
                let mut position = BorrowPosition {
                    auction_debt_asset: u8::MAX,
                    base_liquidation_cf_bps: 9_000,
                    quote_liquidation_cf_bps: 9_000,
                    ..BorrowPosition::default()
                };
                let shares = u128::from(425_000 * UNIT);
                match asset {
                    MarketAsset::Base => {
                        market.debt.fixed_base_shares = shares;
                        market.debt.fixed_base_principal = 400_000 * UNIT;
                        position.fixed_base_shares = shares;
                        position.fixed_base_principal = 400_000 * UNIT;
                        position.quote_collateral = 50_000 * UNIT;
                    }
                    MarketAsset::Quote => {
                        market.debt.fixed_quote_shares = shares;
                        market.debt.fixed_quote_principal = 400_000 * UNIT;
                        position.fixed_quote_shares = shares;
                        position.fixed_quote_principal = 400_000 * UNIT;
                        position.base_collateral = 50_000 * UNIT;
                    }
                }
                market.side_mut(asset).reserves.cash_reserve -= 400_000 * UNIT;
                market.side_mut(asset).reserves.live_reserve += 25_000 * UNIT;
                market.sync_income_receivable(asset, false, 0).unwrap();
                market.config.amm = AmmConfig {
                    peak_amplification_nad: amplification * NAD,
                    core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                    fade_width_bps: if amplification == 1 { 0 } else { 400 },
                    center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                    ..AmmConfig::default()
                };
                market.risk.base_price_ema_nad = NAD;
                market.risk.quote_price_ema_nad = NAD;
                market.risk.directional_base_price_ema_nad = NAD;
                market.risk.directional_quote_price_ema_nad = NAD;
                *market.amm = AmmState::default();
                market.prepare_amm_for_swap(1).unwrap();
                let before = market.clone();
                if advance {
                    let (_, credit) = market.fund_public_interest(asset, u64::MAX, 0).unwrap();
                    market.record_receivable_income_credit(asset, false, credit, 0, ProtocolAuctionSplit::default(), 0).unwrap();
                }
                let funded_growth_before = market.side(asset).fees.interest_growth_index_q64;
                let old_curve = market.curve_reserve(asset).unwrap();
                if writeoff {
                    let allocation = market
                        .liquidation_debt_allocation(&LiquidationPositionRef::Borrow(&position), asset, shares)
                        .unwrap();
                    let loss = market
                        .settle_flash_recovery(&mut LiquidationPositionMut::Borrow(&mut position), asset, allocation, 50_000 * UNIT, 375_000 * UNIT, 0, 0, true, 1)
                        .unwrap();
                    assert_eq!(loss.principal_written_off, 25_000 * UNIT);
                    assert_eq!(loss.interest_canceled, 25_000 * UNIT);
                    assert_eq!(market.side(asset).fees.interest_growth_index_q64, funded_growth_before);
                    let advanced_interest_loss = if advance { 25_000 * UNIT } else { 0 };
                    assert_eq!(market.curve_reserve(asset).unwrap(), old_curve - 25_000 * UNIT - advanced_interest_loss);
                    let ledger = &mut market.side_mut(asset).fees.public_interest_advance;
                    assert_eq!(ledger.take_payment(0).unwrap(), (0, 0));
                    assert_eq!(*ledger, crate::state::PublicInterestAdvance::default());
                } else {
                    let receipt = market.repay(&mut position, asset, 425_000 * UNIT).unwrap();
                    assert_eq!(receipt.interest_paid, 25_000 * UNIT);
                    assert_eq!(market.curve_reserve(asset).unwrap(), old_curve);
                    let (fresh, protocol) = market.side_mut(asset).fees.public_interest_advance.take_payment(receipt.interest_paid).unwrap();
                    assert_eq!(protocol, 0);
                    assert_eq!(fresh, if advance { 0 } else { receipt.interest_paid });
                    market.record_receivable_income_credit(asset, false, fresh, 0, ProtocolAuctionSplit::default(), 0).unwrap();
                    if advance { assert_eq!(market.side(asset).fees.interest_growth_index_q64, funded_growth_before); }
                    market.side(asset).fees.public_interest_advance.require_settled().unwrap();
                }
                assert_eq!(market.unrealized_interest(asset).unwrap(), 0);
                market.assert_market_invariants().unwrap();
                let eligibility = HlpYieldEligibility { ylp_supply: market.base_side.shares.ylp_supply,
                    base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
                    quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares };
                market.settle_new_hlp_public_interest(eligibility, [crate::transitions::LeverageCollateralFee::default(); 2], 1).unwrap();
                let prices = current_hlp_curve_prices(&market).unwrap();
                let navs = [
                    hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap(),
                    hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap(),
                ];
                let plan = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap();
                plan.consume(&mut market).unwrap();
                market.assert_market_invariants().unwrap();
                for (target, expected) in [(MarketAsset::Base, navs[0]), (MarketAsset::Quote, navs[1])] {
                    let nav =
                        hlp_share_nav_nad_with_prices(&market, target, current_hlp_curve_prices(&market).unwrap())
                            .unwrap();
                    assert!(
                        nav.abs_diff(expected) <= 10_000,
                        "amp={amplification} asset={asset:?} target={target:?} writeoff={writeoff} advance={advance}"
                    );
                    if !writeoff {
                        let vault = match target {
                            MarketAsset::Base => &market.base_hlp_vault,
                            MarketAsset::Quote => &market.quote_hlp_vault,
                        };
                        let (_, growth) = vault.yield_growth_indexes(asset);
                        let paid_claim = (u128::from(vault.hlp_supply) * growth) >> 64;
                        let funded_value =
                            asset_value_in_target_nad_with_prices(&market, prices, asset, paid_claim as u64, target)
                                .unwrap();
                        let original_nav = hlp_share_nav_nad_with_prices(&before, target, prices).unwrap();
                        assert!(nav.checked_add(funded_value).unwrap().abs_diff(original_nav) <= 10_000);
                    }
                }
            }
        }
    }
    }
}

#[test]
fn full_value_recovery_bonus_reports_the_settled_price() {
    use crate::state::{AmmConfig, AmmState};
    use crate::transitions::{amm::SwapRequest, LeverageSwapFeeCredit};
    const UNIT: u64 = 1_000_000;
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = Market::default();
        market.base_side.asset_decimals = 6;
        market.quote_side.asset_decimals = 6;
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
        market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
        market.deposit_single_sided(MarketAsset::Quote, 20_000 * UNIT, 1).unwrap();
        // Accrued public yield and a funding deficit coexist, on opposite
        // debt indexes. The recovery bonus changes the reconstructed shares.
        match target {
            MarketAsset::Base => {
                market.debt.fixed_base_shares = u128::from(425_000 * UNIT);
                market.debt.fixed_base_principal = 400_000 * UNIT;
                market.debt.quote_borrow_index_nad = 11 * u128::from(NAD) / 10;
            }
            MarketAsset::Quote => {
                market.debt.fixed_quote_shares = u128::from(425_000 * UNIT);
                market.debt.fixed_quote_principal = 400_000 * UNIT;
                market.debt.base_borrow_index_nad = 11 * u128::from(NAD) / 10;
            }
        }
        market.side_mut(target).reserves.cash_reserve -= 400_000 * UNIT;
        market.side_mut(target).reserves.live_reserve += 25_000 * UNIT;
        market.config.swap_fee_bps = 30;
        market.config.amm = AmmConfig {
            compounding_fee_bps: 10_000,
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
            center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
            ..AmmConfig::default()
        };
        *market.amm = AmmState::default();
        market.prepare_amm_for_swap(1).unwrap();
        let mut prepared = SwapRequest {
            current_slot: 1,
            current_unix_timestamp: 0,
            asset_in: target.opposite(),
            reserve_credit: 10_000 * UNIT,
            protocol_fee_bps: 0,
        }.prepare(&mut market).unwrap();
        let quote = prepared.quote;
        assert!(quote.recovery.bonus_output > 0);
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(
            &prepared.leverage_quote(), quote.fee.claimable_fee_debit,
        ).unwrap();
        prepared.apply(&mut market, SwapCashPolicy::Spot, credit, 1, 0, ProtocolAuctionSplit::default(), None).unwrap();
        market.assert_market_invariants().unwrap();
        let price = market.current_concentrated_spot_price_nad().unwrap().unwrap();
        assert!(price.abs_diff(quote.reserve_end_price_nad) <= 10, "{target:?}: {price} vs {}", quote.reserve_end_price_nad);
        assert!(price.abs_diff(market.risk.cached_spot_base_price_nad) <= 10);
    }
}


#[test]
fn accrued_interest_rehedging_does_not_pay_a_same_slot_round_trip() {
    use crate::state::{AmmConfig, AmmState};
    use crate::transitions::{amm::SwapRequest, LeverageSwapFeeCredit};
    const UNIT: u64 = 1_000_000;
    for (hlp_deposit, interest, initial) in [(20_000, 25_000, 1_000), (20_000, 25_000, 10_000), (200_000, 250_000, 100_000)] {
    for amplification in [1, 4] {
        for interest_asset in [MarketAsset::Base, MarketAsset::Quote] {
            for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
                let mut market = Market::default();
                market.base_side.asset_decimals = 6;
                market.quote_side.asset_decimals = 6;
                market.debt.base_borrow_index_nad = NAD as u128;
                market.debt.quote_borrow_index_nad = NAD as u128;
                market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
                market.deposit_single_sided(MarketAsset::Base, hlp_deposit * UNIT, 1).unwrap();
                market.deposit_single_sided(MarketAsset::Quote, hlp_deposit * UNIT, 1).unwrap();
                match interest_asset {
                    MarketAsset::Base => {
                        market.debt.fixed_base_shares = u128::from(400_000 + interest) * u128::from(UNIT);
                        market.debt.fixed_base_principal = 400_000 * UNIT;
                    }
                    MarketAsset::Quote => {
                        market.debt.fixed_quote_shares = u128::from(400_000 + interest) * u128::from(UNIT);
                        market.debt.fixed_quote_principal = 400_000 * UNIT;
                    }
                }
                market.side_mut(interest_asset).reserves.cash_reserve -= 400_000 * UNIT;
                market.side_mut(interest_asset).reserves.live_reserve += interest * UNIT;
                market.config.swap_fee_bps = 0;
                market.config.amm = AmmConfig {
                    peak_amplification_nad: amplification * NAD,
                    core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                    fade_width_bps: if amplification == 1 { 0 } else { 400 },
                    center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                    ..AmmConfig::default()
                };
                *market.amm = AmmState::default();
                market.risk.base_price_ema_nad = NAD;
                market.risk.quote_price_ema_nad = NAD;
                market.risk.directional_base_price_ema_nad = NAD;
                market.risk.directional_quote_price_ema_nad = NAD;
                market.prepare_amm_for_swap(1).unwrap();
                let initial = initial * UNIT;
                for cycle in 0..3 {
                let mut amount = initial;
                for input in [asset_in, asset_in.opposite()] {
                    let mut prepared = SwapRequest {
                        current_slot: 1,
                        current_unix_timestamp: 0,
                        asset_in: input,
                        reserve_credit: amount,
                        protocol_fee_bps: 0,
                    }.prepare(&mut market).unwrap();
                    amount = prepared.quote.amount_out;
                    let quote = prepared.leverage_quote();
                    let credit = LeverageSwapFeeCredit::from_total_actual_credit(
                        &quote, prepared.quote.fee.claimable_fee_debit,
                    ).unwrap();
                    prepared.apply(&mut market, SwapCashPolicy::Spot, credit, 1, 0,
                        ProtocolAuctionSplit::default(), None).unwrap();
                    market.assert_market_invariants().unwrap();
                }
                assert!(amount <= initial,
                    "round trip extracts reserve cash: cycle={cycle} amp={amplification} interest={interest_asset:?} in={asset_in:?}, input={initial}, output={amount}");
                }
            }
        }
    }
    }
}

#[test]
fn cash_funded_interest_preserves_raw_curve_and_reports_rehedged_value() {
    use crate::state::{AmmConfig, AmmState};
    const UNIT: u64 = 1_000_000;
    for amplification in [1, 4] {
        for interest_asset in [MarketAsset::Base, MarketAsset::Quote] {
          for unpaid_tokens in [25_000_u64, 250_000, 800_000] {
            let mut market = Market::default();
            market.base_side.asset_decimals = 6;
            market.quote_side.asset_decimals = 6;
            market.debt.base_borrow_index_nad = NAD as u128;
            market.debt.quote_borrow_index_nad = NAD as u128;
            market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
            market.deposit_single_sided(MarketAsset::Base, 200_000 * UNIT, 1).unwrap();
            market.deposit_single_sided(MarketAsset::Quote, 200_000 * UNIT, 1).unwrap();
            match interest_asset {
                MarketAsset::Base => {
                    market.debt.fixed_base_shares = (400_000 + u128::from(unpaid_tokens)) * u128::from(UNIT);
                    market.debt.fixed_base_principal = 400_000 * UNIT;
                }
                MarketAsset::Quote => {
                    market.debt.fixed_quote_shares = (400_000 + u128::from(unpaid_tokens)) * u128::from(UNIT);
                    market.debt.fixed_quote_principal = 400_000 * UNIT;
                }
            }
            market.side_mut(interest_asset).reserves.cash_reserve -= 400_000 * UNIT;
            market.side_mut(interest_asset).reserves.live_reserve += unpaid_tokens * UNIT;
            market.config.amm = AmmConfig {
                peak_amplification_nad: amplification * NAD,
                core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                fade_width_bps: if amplification == 1 { 0 } else { 400 },
                center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                ..AmmConfig::default()
            };
            *market.amm = AmmState::default();
            market.prepare_amm_for_swap(1).unwrap();
            let pre = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap();
            pre.consume(&mut market).unwrap();
            market.finalize_amm_transition_and_observe_risk(1).unwrap();
            let prices = current_hlp_curve_prices(&market).unwrap();
            let raw_before = market.curve_reserves_nad().unwrap();
            let nav_before = [
                hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap(),
                hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap(),
            ];
            let ordinary_before = market.base_side.shares.ylp_supply - market.base_hlp_vault.ylp_shares - market.quote_hlp_vault.ylp_shares;
            let (gross, lp_credit) = market.fund_public_interest(interest_asset, u64::MAX, 2_000).unwrap();
            assert!(gross > 0 && gross <= unpaid_tokens * UNIT);
            assert_eq!(lp_credit, gross - gross / 5);
            assert_eq!(market.curve_reserves_nad().unwrap(), raw_before);
            assert_eq!(current_hlp_curve_prices(&market).unwrap(), prices);
            market.record_receivable_income_credit(interest_asset, false, lp_credit, 0, ProtocolAuctionSplit::default(), 0).unwrap();
            market.checkpoint_hlp_yield_from_ylp(MarketAsset::Base).unwrap();
            market.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote).unwrap();
            let core_before_rehedge = [
                hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap(),
                hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap(),
            ];
            let post = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap_or_else(|e| panic!("amp={amplification} asset={interest_asset:?} unpaid={unpaid_tokens} gross={gross}: {e}"));
            post.consume(&mut market).unwrap();
            market.finalize_amm_transition_and_observe_risk(1).unwrap();
            market.assert_market_invariants().unwrap();
            let prices_after = current_hlp_curve_prices(&market).unwrap();
            let ordinary_after = market.base_side.shares.ylp_supply - market.base_hlp_vault.ylp_shares - market.quote_hlp_vault.ylp_shares;
            assert_eq!(ordinary_before, ordinary_after);
            for target in [MarketAsset::Base, MarketAsset::Quote] {
                let nav_after = hlp_share_nav_nad_with_prices(&market, target, prices_after).unwrap();
                let fixed_mark_nav = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
                assert!(fixed_mark_nav.abs_diff(core_before_rehedge[target.code() as usize]) <= 10_000,
                    "rehedge transferred holder value at fixed mark: amp={amplification} asset={interest_asset:?} target={target:?} unpaid={unpaid_tokens}");
                println!("ADVANCE_HLP unpaid={unpaid_tokens} gross={gross} amp={amplification} interest={interest_asset:?} target={target:?} before={} after={} price_before={} price_after={}",
                    nav_before[target.code() as usize], nav_after, prices.base_in_quote_nad(), prices_after.base_in_quote_nad());
            }
        }
    }
    }
}

#[test]
fn new_hlp_interest_pays_hedge_gap_before_publishing_yield() {
    use crate::state::{AmmConfig, AmmState};
    const UNIT: u64 = 1_000_000;
    for funding_bps in [0_u128, 100] {
    for (fee_bps, fee_max) in [(0_u16, u64::MAX), (50, u64::MAX), (250, 100_000)] {
    let fee = crate::transitions::LeverageCollateralFee::new(Some(spl_token_2022::extension::transfer_fee::TransferFee {
        epoch: 0.into(), transfer_fee_basis_points: fee_bps.into(), maximum_fee: fee_max.into() }));
    for amplification in [1, 4, 10] {
        for interest_asset in [MarketAsset::Base, MarketAsset::Quote] {
          for unpaid_tokens in [25_000_u64, 250_000, 800_000] {
            let mut market = Market::default();
            market.base_side.asset_decimals = 6;
            market.quote_side.asset_decimals = 6;
            market.debt.base_borrow_index_nad = NAD as u128;
            market.debt.quote_borrow_index_nad = NAD as u128;
            market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
            market.deposit_single_sided(MarketAsset::Base, 200_000 * UNIT, 1).unwrap();
            market.deposit_single_sided(MarketAsset::Quote, 200_000 * UNIT, 1).unwrap();
            match interest_asset {
                MarketAsset::Base => {
                    market.debt.fixed_base_shares = (400_000 + u128::from(unpaid_tokens)) * u128::from(UNIT);
                    market.debt.fixed_base_principal = 400_000 * UNIT;
                }
                MarketAsset::Quote => {
                    market.debt.fixed_quote_shares = (400_000 + u128::from(unpaid_tokens)) * u128::from(UNIT);
                    market.debt.fixed_quote_principal = 400_000 * UNIT;
                }
            }
            market.side_mut(interest_asset).reserves.cash_reserve -= 400_000 * UNIT;
            market.side_mut(interest_asset).reserves.live_reserve += unpaid_tokens * UNIT;
            market.sync_income_receivable(interest_asset, false, 0).unwrap();
            market.config.amm = AmmConfig {
                peak_amplification_nad: amplification * NAD,
                core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                fade_width_bps: if amplification == 1 { 0 } else { 400 },
                center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                ..AmmConfig::default()
            };
            *market.amm = AmmState::default();
            market.prepare_amm_for_swap(1).unwrap();
            let pre = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap();
            pre.consume(&mut market).unwrap();
            market.finalize_amm_transition_and_observe_risk(1).unwrap();
            // Accrue new funding costs after the initial hedge, without paying
            // them before the harvest being tested.
            let before_unpaid = market.public_unpaid_interest(interest_asset).unwrap();
            match interest_asset {
                MarketAsset::Base => market.debt.base_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
                MarketAsset::Quote => market.debt.quote_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
            }
            let unpaid = market.public_unpaid_interest(interest_asset).unwrap();
            market.side_mut(interest_asset).reserves.live_reserve += (unpaid - before_unpaid) as u64;
            market.sync_income_receivable(interest_asset, false, 0).unwrap();
            market.sync_income_receivable(interest_asset, true, 0).unwrap();
            // Previously funded yield is outside the spendable new interval.
            let supply = market.base_side.shares.ylp_supply;
            market.side_mut(interest_asset).record_interest_credit_with_supply(1_234 * UNIT, 0, ProtocolAuctionSplit::default(), 0, supply).unwrap();
            market.checkpoint_hlp_yield_from_ylp(MarketAsset::Base).unwrap();
            market.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote).unwrap();
            let protected = [market.hlp_yield_growth_indexes(MarketAsset::Base, interest_asset).1,
                market.hlp_yield_growth_indexes(MarketAsset::Quote, interest_asset).1];
            let prices = current_hlp_curve_prices(&market).unwrap();
            let raw_before = market.curve_reserves_nad().unwrap();
            let nav_before = [
                hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap(),
                hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap(),
            ];
            let ordinary_before = market.base_side.shares.ylp_supply - market.base_hlp_vault.ylp_shares - market.quote_hlp_vault.ylp_shares;
            let frozen_market = market.clone();
            let ordinary_value = |m: &Market| {
                [MarketAsset::Base, MarketAsset::Quote].into_iter().map(|asset| {
                    let claim = ylp_live_underlying_amount_from_values(m.curve_reserve(asset).unwrap(), m.base_side.shares.ylp_supply, ordinary_before).unwrap();
                    asset_value_in_target_nad_with_prices(m, prices, asset, claim, MarketAsset::Quote).unwrap()
                }).sum::<u128>()
            };
            let ordinary_nav_before = ordinary_value(&market);
            let (gross, lp_gross) = market.fund_public_interest(interest_asset, u64::MAX, 2_000).unwrap();
            assert!(gross > 0 && u128::from(gross) <= unpaid);
            assert_eq!(lp_gross, gross - gross / 5);
            let lp_credit = fee.unwind_credit(lp_gross).unwrap();
            assert_eq!(market.curve_reserves_nad().unwrap(), raw_before);
            assert_eq!(current_hlp_curve_prices(&market).unwrap(), prices);
            market.record_receivable_income_credit(interest_asset, false, lp_credit, 0, ProtocolAuctionSplit::default(), 0).unwrap();
            let eligibility = HlpYieldEligibility { ylp_supply: supply,
                base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
                quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares };
            let receipt = market.settle_new_hlp_public_interest(eligibility,
                [fee; 2], 1)
                .unwrap_or_else(|e| panic!("amp={amplification} asset={interest_asset:?} unpaid={unpaid_tokens}: {e}"));
            assert!(receipt.published.iter().flatten().sum::<u64>() <= lp_credit);
            market.assert_market_invariants().unwrap();
            let ordinary_after = market.base_side.shares.ylp_supply - market.base_hlp_vault.ylp_shares - market.quote_hlp_vault.ylp_shares;
            assert_eq!(ordinary_before, ordinary_after);

            assert!(ordinary_value(&market).abs_diff(ordinary_nav_before) < 100_000,
                "ordinary LPs funded hLP recapitalization: amp={amplification} unpaid={unpaid_tokens} funding={funding_bps} diff={}",
                ordinary_value(&market) as i128 - (ordinary_nav_before) as i128);
            for target in [MarketAsset::Base, MarketAsset::Quote] {
                let fixed_mark_nav = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
                let own_unpaid = frozen_market.hlp_unpaid_income(target, interest_asset).unwrap();
                let gross_claim = mul_div_u128(u128::from(gross), u128::from(own_unpaid), unpaid).unwrap() as u64;
                let lp_claim = mul_div_u128(u128::from(lp_credit), u128::from(own_unpaid), unpaid).unwrap() as u64;
                let fee_value = asset_value_in_target_nad_with_prices(&market, prices, interest_asset, gross_claim - lp_claim, target).unwrap();
                let yield_value = asset_value_in_target_nad_with_prices(&market, prices, interest_asset, receipt.published[target.code() as usize][interest_asset.code() as usize], target).unwrap();
                let t = target.code() as usize;
                assert!(market.hlp_yield_growth_indexes(target, interest_asset).1 >= protected[t]);
                let mut retained_fee_value = 0;
                for asset in [MarketAsset::Base, MarketAsset::Quote] {
                    let i = asset.code() as usize;
                    retained_fee_value += asset_value_in_target_nad_with_prices(&market, prices, asset,
                        receipt.reserve_debit[t][i] - receipt.reserve_credit[t][i], target).unwrap();
                }
                let expected = nav_before[t] - fee_value - retained_fee_value;
                assert!((fixed_mark_nav + yield_value).abs_diff(expected) < 100_000,
                    "value changed amp={amplification} asset={interest_asset:?} target={target:?} unpaid={unpaid_tokens}: before_net={expected} after_plus_yield={} difference={}", fixed_mark_nav+yield_value, (fixed_mark_nav+yield_value).abs_diff(expected));

            }
        }
    }
    }
    }
    }
}

#[test]
fn frozen_hlp_new_interest_is_harvestable_without_creating_unminted_ylp() {
    const UNIT: u64 = 1_000_000;
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        for income_asset in [MarketAsset::Base, MarketAsset::Quote] {
            for with_active_cohort in [false, true] {
                let mut market = Market::default();
                market.base_side.asset_decimals = 6;
                market.quote_side.asset_decimals = 6;
                market.debt.base_borrow_index_nad = u128::from(NAD);
                market.debt.quote_borrow_index_nad = u128::from(NAD);
                market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
                market.deposit_single_sided(target, 20_000 * UNIT, 1).unwrap();
                if with_active_cohort {
                    market.deposit_single_sided(target.opposite(), 10_000 * UNIT, 1).unwrap();
                }
                match target {
                    MarketAsset::Base => market.debt.quote_borrow_index_nad = 3 * u128::from(NAD),
                    MarketAsset::Quote => market.debt.base_borrow_index_nad = 3 * u128::from(NAD),
                }
                market.prepare_amm_for_swap(2).unwrap();
                market.freeze_negative_hlp_vaults().unwrap();
                assert!(market.hlp_frozen(target));
                assert!(!market.hlp_frozen(target.opposite()));
                market.checkpoint_hlp_yield_from_ylp(MarketAsset::Base).unwrap();
                market.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote).unwrap();
                let eligibility = HlpYieldEligibility {
                    ylp_supply: market.base_side.shares.ylp_supply,
                    base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
                    quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
                };
                let frozen_before = if target == MarketAsset::Base {
                    market.base_hlp_vault
                } else {
                    market.quote_hlp_vault
                };
                let price_before = market.current_concentrated_spot_price_nad().unwrap();
                let reserves = |m: &Market| [
                    (m.base_side.reserves.cash_reserve, m.base_side.reserves.live_reserve),
                    (m.quote_side.reserves.cash_reserve, m.quote_side.reserves.live_reserve),
                ];
                let reserves_before = reserves(&market);
                let funded_credit = 10_000 * UNIT;
                market.issue_income_receivable(income_asset, false, funded_credit).unwrap();
                market.clear_income_receivable(income_asset, false, funded_credit, funded_credit).unwrap();
                market.record_receivable_income_credit(income_asset, false, funded_credit, 0, ProtocolAuctionSplit::default(), 0).unwrap();
                let receipt = market.settle_new_hlp_public_interest(
                    eligibility, [crate::transitions::LeverageCollateralFee::default(); 2], 2,
                ).unwrap();
                let frozen_after = if target == MarketAsset::Base {
                    market.base_hlp_vault
                } else {
                    market.quote_hlp_vault
                };
                let t = target.code() as usize;
                let i = income_asset.code() as usize;
                assert_eq!(receipt.reserve_debit[t], [0, 0]);
                assert_eq!(receipt.reserve_credit[t], [0, 0]);
                let expected = mul_div_u128(
                    u128::from(funded_credit / 2), u128::from(frozen_before.ylp_shares), u128::from(market.tranches.senior_reserve_shares(eligibility.ylp_supply).unwrap()),
                ).unwrap() as u64;
                assert!(receipt.published[t][i] > 0);
                assert!(receipt.published[t][i].abs_diff(expected) <= 1);
                assert!(receipt.published.iter().flatten().sum::<u64>() <= funded_credit);
                assert_eq!(frozen_after.ylp_shares, frozen_before.ylp_shares);
                assert_eq!(frozen_after.debt_shares, frozen_before.debt_shares);
                assert_eq!(frozen_after.debt_principal, frozen_before.debt_principal);
                let minted = receipt.base_rebalance.ylp_mint_amount + receipt.quote_rebalance.ylp_mint_amount;
                let burned = receipt.base_rebalance.ylp_burn_amount + receipt.quote_rebalance.ylp_burn_amount;
                assert_eq!(market.base_side.shares.ylp_supply, eligibility.ylp_supply + minted - burned);
                assert_eq!(market.base_side.shares.ylp_supply - market.base_hlp_vault.ylp_shares - market.quote_hlp_vault.ylp_shares,
                    eligibility.non_hlp_ylp_supply().unwrap());
                if !with_active_cohort {
                    assert_eq!(reserves(&market), reserves_before);
                    assert_eq!(market.current_concentrated_spot_price_nad().unwrap(), price_before);
                }
                market.assert_market_invariants().unwrap();
            }
        }
    }
}


#[test]
fn tranche_hlp_nav_includes_only_its_allocated_unpaid_interest() {
    const UNIT: u64 = 1_000_000;
    let mut market = Market::default();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
    market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
    let prices = current_hlp_curve_prices(&market).unwrap();
    let before = hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap();
    // Model a closed accrual interval without replacing the native income
    // issuance: principal leaves cash, indexed interest enters live reserves.
    market.debt.fixed_base_principal = 400_000 * UNIT;
    market.debt.fixed_base_shares = 410_000 * UNIT as u128;
    market.base_side.reserves.cash_reserve -= 400_000 * UNIT;
    market.base_side.reserves.live_reserve += 10_000 * UNIT;
    market.sync_income_receivable(MarketAsset::Base, false, 0).unwrap();
    let income = market.income.get().unwrap();
    let own_unpaid = income.hlp[0][0].unpaid_value(&income.books[0]).unwrap();
    let expected = before + market.normalize_amount(own_unpaid as u128, 6).unwrap();
    let observed = hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap();
    assert!(observed.abs_diff(expected) <= 1_000, "hLP counts another tranche's receivable: expected {expected}, observed {observed}, own unpaid {own_unpaid}");
}


#[test]
fn tranche_hlp_rehedge_preserves_each_vaults_receivable_nav() {
    const UNIT: u64 = 1_000_000;
    for amplification in [1, 4] {
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = Market::default();
            market.base_side.asset_decimals = 6;
            market.quote_side.asset_decimals = 6;
            market.debt.base_borrow_index_nad = NAD as u128;
            market.debt.quote_borrow_index_nad = NAD as u128;
            market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
            market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
            market.deposit_single_sided(MarketAsset::Quote, 30_000 * UNIT, 1).unwrap();
            market.config.amm.peak_amplification_nad = amplification * NAD;
            market.config.amm.core_half_width_bps = if amplification == 1 { 0 } else { 100 };
            market.config.amm.fade_width_bps = if amplification == 1 { 0 } else { 400 };
            *market.amm = crate::state::AmmState::default();
            match asset {
                MarketAsset::Base => { market.debt.fixed_base_principal = 400_000 * UNIT; market.debt.fixed_base_shares = 425_000 * UNIT as u128; }
                MarketAsset::Quote => { market.debt.fixed_quote_principal = 400_000 * UNIT; market.debt.fixed_quote_shares = 425_000 * UNIT as u128; }
            }
            market.side_mut(asset).reserves.cash_reserve -= 400_000 * UNIT;
            market.side_mut(asset).reserves.live_reserve += 25_000 * UNIT;
            market.sync_income_receivable(asset, false, 0).unwrap();
            market.prepare_amm_for_swap(1).unwrap();
            let prices = current_hlp_curve_prices(&market).unwrap();
            let before = [MarketAsset::Base, MarketAsset::Quote].map(|target| hlp_share_nav_nad_with_prices(&market, target, prices).unwrap());
            let plan = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap();
            plan.consume(&mut market).unwrap();
            market.assert_market_invariants().unwrap();
            for target in [MarketAsset::Base, MarketAsset::Quote] {
                let after = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
                assert!(after.abs_diff(before[target.code() as usize]) <= 10_000, "NAV changed: amp={amplification}, income={asset:?}, target={target:?}, before={}, after={after}", before[target.code() as usize]);
                let opposite = hlp_full_underlying_amount(&market, target, target.opposite()).unwrap();
                let vault = if target == MarketAsset::Base { market.base_hlp_vault } else { market.quote_hlp_vault };
                let debt = Debt::shares_to_debt(vault.debt_shares, market.debt.borrow_index(target.opposite())).unwrap();
                assert!(debt.abs_diff(opposite as u128) <= 1);
            }
        }
    }
}

#[test]
fn tranche_hlp_exit_buys_only_departing_coupons_and_does_not_gift_next_depositor() {
    const UNIT: u64 = 1_000_000;
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = Market::default();
            market.base_side.asset_decimals = 6;
            market.quote_side.asset_decimals = 6;
            market.debt.base_borrow_index_nad = NAD as u128;
            market.debt.quote_borrow_index_nad = NAD as u128;
            market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
            market.deposit_single_sided(target, 20_000 * UNIT, 1).unwrap();
            match asset {
                MarketAsset::Base => { market.debt.fixed_base_principal = 400_000 * UNIT; market.debt.fixed_base_shares = 410_000 * UNIT as u128; }
                MarketAsset::Quote => { market.debt.fixed_quote_principal = 400_000 * UNIT; market.debt.fixed_quote_shares = 410_000 * UNIT as u128; }
            }
            market.side_mut(asset).reserves.cash_reserve -= 400_000 * UNIT;
            market.side_mut(asset).reserves.live_reserve += 10_000 * UNIT;
            market.sync_income_receivable(asset, false, 0).unwrap();
            market.prepare_amm_for_swap(1).unwrap();
            let static_units = market.base_side.shares.ylp_supply - if target == MarketAsset::Base { market.base_hlp_vault.ylp_shares } else { market.quote_hlp_vault.ylp_shares };
            let ordinary_value = |m: &Market, prices: HlpCurvePrices| [MarketAsset::Base, MarketAsset::Quote].into_iter().map(|a| {
                let amount = ylp_live_underlying_amount_from_values(m.curve_reserve(a).unwrap(), m.base_side.shares.ylp_supply, static_units).unwrap();
                asset_value_in_target_nad_with_prices(m, prices, a, amount, MarketAsset::Quote).unwrap()
            }).sum::<u128>();
            let initial_unpaid = market.hlp_unpaid_income(target, asset).unwrap();
            assert!(initial_unpaid > 0);
            for fraction in [2, 1] {
                let supply = if target == MarketAsset::Base { market.base_hlp_vault.hlp_supply } else { market.quote_hlp_vault.hlp_supply };
                let amount = supply / fraction;
                let prices = current_hlp_curve_prices(&market).unwrap();
                let before = ordinary_value(&market, prices);
                let quoted = market.quote_hlp_withdrawal(target, amount).unwrap();
                let receipt = market.withdraw_single_sided(target, amount).unwrap();
                assert_eq!(receipt.target_amount_out, quoted);
                market.assert_market_invariants().unwrap();
                // Closing an opposite-asset coupon uses the existing AMM quote.
                // Its price impact can benefit the remaining pool; it cannot dilute it.
                assert!(ordinary_value(&market, prices) + 10_000 >= before, "exit diluted ordinary capital: target={target:?} income={asset:?} fraction={fraction} before={before} after={} price={:?}", ordinary_value(&market, prices), prices);
            }
            let advanced = market.side(asset).fees.public_interest_advance.outstanding_gross;
            assert!(advanced.abs_diff(initial_unpaid) <= 1);
            assert_eq!(market.hlp_unpaid_income(target, asset).unwrap(), 0);
            market.deposit_single_sided(target, 20_000 * UNIT, 1).unwrap();
            assert_eq!(market.hlp_unpaid_income(target, asset).unwrap(), 0);
            let (_, credit) = market.fund_public_interest(asset, u64::MAX, 0).unwrap();
            assert_eq!(credit, 10_000 * UNIT - advanced);
            market.record_receivable_income_credit(asset, false, credit, 0, ProtocolAuctionSplit::default(), 0).unwrap();
            let eligibility = HlpYieldEligibility { ylp_supply: market.base_side.shares.ylp_supply,
                base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
                quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares };
            let settled = market.settle_new_hlp_public_interest(eligibility, [crate::transitions::LeverageCollateralFee::default(); 2], 1).unwrap();
            assert_eq!(settled.published[target.code() as usize], [0, 0]);
            assert_eq!(settled.reserve_credit[target.code() as usize], [0, 0]);
        }
    }
}

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
                market.config.amm = AmmConfig {
                    peak_amplification_nad: amplification * NAD,
                    core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                    fade_width_bps: if amplification == 1 { 0 } else { 400 },
                    center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                    ..AmmConfig::default()
                };
                market.amm = AmmState::default();
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
                    market.amm = AmmState::default();
                    market.prepare_amm_for_swap(1).unwrap();
                    let start = market.integrated_curve_state_nad().unwrap();
                    let eligible_supply = market.base_side.shares.ylp_supply;
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
                        (MarketAsset::Base, start.base_hlp_equity),
                        (MarketAsset::Quote, start.quote_hlp_equity),
                    ] {
                        let fee_share_nad = market
                            .normalize_amount(u128::from(amm_quote.fee.compounded_fee_debit), 6)
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
                        let claim = ylp_live_underlying_amount(&market, target.opposite(), vault.ylp_shares).unwrap();
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
            market.config.amm = AmmConfig {
                peak_amplification_nad: amplification * NAD,
                core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                fade_width_bps: if amplification == 1 { 0 } else { 400 },
                center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                ..AmmConfig::default()
            };
            market.amm = AmmState::default();
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
            let claim = ylp_live_underlying_amount(&market, target.opposite(), vault.ylp_shares).unwrap();
            let debt = Debt::shares_to_debt(vault.debt_shares, market.debt.borrow_index(target.opposite())).unwrap();
            assert!(u128::from(claim).abs_diff(debt) <= 1);
        }
    }
}

#[test]
fn public_repayment_and_writeoff_update_both_hlp_values_once() {
    use crate::state::{AmmConfig, AmmState, BorrowPosition};
    use crate::transitions::{
        flash_liquidation::LiquidationPositionRef, flash_settlement::LiquidationPositionMut, LeverageCollateralFee,
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
                market.amm = AmmState::default();
                market.prepare_amm_for_swap(1).unwrap();
                let before = market.clone();
                if advance {
                    let supply = market.side(asset).shares.ylp_supply;
                    let (_, credit) = market.fund_public_interest(asset, u64::MAX, 0).unwrap();
                    market.side_mut(asset).record_interest_credit_allocation(credit, 0, ProtocolAuctionSplit::default(), 0, supply).unwrap();
                }
                let funded_growth_before = market.side(asset).fees.interest_growth_index_q64;
                let old_curve = market.curve_reserve(asset).unwrap();
                if writeoff {
                    let allocation = market
                        .liquidation_debt_allocation(&LiquidationPositionRef::Borrow(&position), asset, shares)
                        .unwrap();
                    let loss = market
                        .settle_flash_recovery(
                            &mut LiquidationPositionMut::Borrow(&mut position),
                            asset,
                            allocation,
                            50_000 * UNIT,
                            375_000 * UNIT,
                            0,
                            0,
                            true,
                            LeverageCollateralFee::default(),
                            1,
                            1,
                        )
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
                    market.side_mut(asset).record_interest_credit(fresh, 0, ProtocolAuctionSplit::default(), 0).unwrap();
                    if advance { assert_eq!(market.side(asset).fees.interest_growth_index_q64, funded_growth_before); }
                    market.side(asset).fees.public_interest_advance.require_settled().unwrap();
                }
                assert_eq!(market.unrealized_interest(asset).unwrap(), 0);
                market.assert_market_invariants().unwrap();
                let prices = current_hlp_curve_prices(&market).unwrap();
                let navs = [
                    hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap(),
                    hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap(),
                ];
                for target in [MarketAsset::Base, MarketAsset::Quote] {
                    market.checkpoint_hlp_yield_from_ylp(target).unwrap();
                }
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
        market.amm = AmmState::default();
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
                market.amm = AmmState::default();
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
            market.amm = AmmState::default();
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
            let supply = market.base_side.shares.ylp_supply;
            let (gross, lp_credit) = market.fund_public_interest(interest_asset, u64::MAX, 2_000).unwrap();
            assert!(gross > 0 && gross <= unpaid_tokens * UNIT);
            assert_eq!(lp_credit, gross - gross / 5);
            assert_eq!(market.curve_reserves_nad().unwrap(), raw_before);
            assert_eq!(current_hlp_curve_prices(&market).unwrap(), prices);
            market.side_mut(interest_asset).record_interest_credit_allocation(lp_credit, 0, ProtocolAuctionSplit::default(), 0, supply).unwrap();
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
            market.config.amm = AmmConfig {
                peak_amplification_nad: amplification * NAD,
                core_half_width_bps: if amplification == 1 { 0 } else { 100 },
                fade_width_bps: if amplification == 1 { 0 } else { 400 },
                center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                ..AmmConfig::default()
            };
            market.amm = AmmState::default();
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
            // Previously funded yield is outside the spendable new interval.
            let supply = market.base_side.shares.ylp_supply;
            market.side_mut(interest_asset).record_interest_credit_allocation(1_234 * UNIT, 0, ProtocolAuctionSplit::default(), 0, supply).unwrap();
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
            let supply = market.base_side.shares.ylp_supply;
            let frozen_market = market.clone();
            let ordinary_value = |m: &Market| {
                [MarketAsset::Base, MarketAsset::Quote].into_iter().map(|asset| {
                    let claim = ylp_live_underlying_amount(m, asset, ordinary_before).unwrap();
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
            market.side_mut(interest_asset).record_interest_credit_allocation(lp_credit, 0, ProtocolAuctionSplit::default(), 0, supply).unwrap();
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
            let ordinary_gross = mul_div_u128(u128::from(gross), u128::from(ordinary_before), u128::from(supply)).unwrap() as u64;
            let ordinary_removed = asset_value_in_target_nad_with_prices(&market, prices, interest_asset, ordinary_gross, MarketAsset::Quote).unwrap();
            assert!(ordinary_value(&market).abs_diff(ordinary_nav_before - ordinary_removed) < 100_000,
                "ordinary LPs funded hLP recapitalization: amp={amplification} unpaid={unpaid_tokens} funding={funding_bps} diff={}",
                ordinary_value(&market) as i128 - (ordinary_nav_before - ordinary_removed) as i128);
            for target in [MarketAsset::Base, MarketAsset::Quote] {
                let fixed_mark_nav = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
                let vault_before = if target == MarketAsset::Base { frozen_market.base_hlp_vault } else { frozen_market.quote_hlp_vault };
                let gross_claim = mul_div_u128(u128::from(gross), u128::from(vault_before.ylp_shares), u128::from(supply)).unwrap() as u64;
                let lp_claim = mul_div_u128(u128::from(lp_credit), u128::from(vault_before.ylp_shares), u128::from(supply)).unwrap() as u64;
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
                market.side_mut(income_asset).record_interest_credit_allocation(
                    funded_credit, 0, ProtocolAuctionSplit::default(), 0, eligibility.ylp_supply,
                ).unwrap();
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
                    u128::from(funded_credit), u128::from(frozen_before.ylp_shares), u128::from(eligibility.ylp_supply),
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

// A frozen vault retains shares and indexed debt, but no longer participates
// in active hedge reconstruction. Both the analytic certificate and the
// materialized ownership must use the same funded frozen leg exactly once.
#[test]
fn frozen_hlp_reconstruction_cannot_fund_repeated_round_trips() {
    use crate::state::{AmmConfig, IrmConfig};
    use crate::transitions::{amm::SwapRequest, LeverageSwapFeeCredit};

    fn trade(m: &mut Market, asset: MarketAsset, input: u64) -> Result<u64> {
        let mut prepared = SwapRequest {
            current_slot: 2_500,
            current_unix_timestamp: 0,
            asset_in: asset,
            reserve_credit: input,
            protocol_fee_bps: 0,
        }.prepare(m)?;
        let out = prepared.quote.amount_out;
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(
            &prepared.leverage_quote(), prepared.quote.fee.claimable_fee_debit,
        )?;
        prepared.apply(m, SwapCashPolicy::Spot, credit, 2_500, 0,
            ProtocolAuctionSplit::default(), None)?;
        m.assert_market_invariants()?;
        Ok(out)
    }

    let raw = |n: u64, d: u8| n * 10_u64.pow(u32::from(d));
    for (base_decimals, quote_decimals) in [(9, 6), (6, 9), (2, 2), (0, 0)] {
      for amp in [1, 4] {
       for frozen_mask in [1, 2, 3] {
        for input in [MarketAsset::Base, MarketAsset::Quote] {
         let side = |d| MarketSide {
            asset_mint: Pubkey::new_unique(), hlp_mint: Pubkey::new_unique(),
            reserve_vault: Pubkey::new_unique(), collateral_vault: Pubkey::new_unique(),
            interest_vault: Pubkey::new_unique(), asset_decimals: d, ..MarketSide::default()
         };
         let config = MarketConfig {
            swap_fee_bps: 0, divergence_fee_share_cap_bps: 0, volatility_fee_share_cap_bps: 0,
            target_hlp_leverage_bps: 20_000, settlement_divergence_bps: 10_000,
            ema_half_life_ms: 60_000, directional_ema_half_life_ms: 60_000,
            curve_depth_ema_half_life_ms: 60_000,
            max_daily_borrow_bps: crate::state::DEFAULT_DAILY_BORROW_BPS,
            global_health_contribution_cap_bps: 15_000, borrow_market_health_floor_bps: 11_000,
            amm: AmmConfig { peak_amplification_nad: amp * NAD,
                core_half_width_bps: if amp > 1 { 1_000 } else { 0 },
                fade_width_bps: if amp > 1 { 4_000 } else { 0 },
                center_ema_half_life_ms: 60_000, volatility_half_life_ms: 60_000,
                ..AmmConfig::default() },
            irm: IrmConfig::default(), ..MarketConfig::default()
         };
         let mut market = Market::default();
         market.initialize(Pubkey::new_unique(), side(base_decimals), side(quote_decimals), config,
            Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(),
            [0;32], Pubkey::new_unique(), 60 * NAD, 0, 2_500, 255).unwrap();
         market.add_liquidity(raw(10_000,base_decimals),raw(600_000,quote_decimals)).unwrap();
         market.finalize_amm_transition_and_observe_risk(2_500).unwrap();
         market.deposit_single_sided(MarketAsset::Base,raw(1_000,base_decimals),1).unwrap();
         market.finalize_amm_transition_and_observe_risk(2_500).unwrap();
         market.deposit_single_sided(MarketAsset::Quote,raw(60_000,quote_decimals),1).unwrap();
         market.finalize_amm_transition_and_observe_risk(2_500).unwrap();
         if frozen_mask & 1 != 0 {market.debt.quote_borrow_index_nad *= 3;}
         if frozen_mask & 2 != 0 {market.debt.base_borrow_index_nad *= 3;}
         market.freeze_negative_hlp_vaults().unwrap();
         assert_eq!(market.amm.frozen_hlp_mask, frozen_mask);
         market.assert_market_invariants().unwrap();
         let base_frozen = market.base_hlp_vault;
         let quote_frozen = market.quote_hlp_vault;
         let offsets = market.frozen_curve_offsets().unwrap();
         let initial = if input == MarketAsset::Base {raw(4_200,base_decimals)} else {raw(252_000,quote_decimals)};
         for cycle in 0..32 {
            let sold=trade(&mut market,input,initial).unwrap_or_else(|e|
                panic!("first leg: decimals={base_decimals}/{quote_decimals} amp={amp} mask={frozen_mask} input={input:?} cycle={cycle}: {e}"));
            let bought=trade(&mut market,input.opposite(),sold).unwrap_or_else(|e|
                panic!("second leg: decimals={base_decimals}/{quote_decimals} amp={amp} mask={frozen_mask} input={input:?} cycle={cycle}: {e}"));
            assert!(bought <= initial,
                "frozen round trip gained {} atoms: decimals={base_decimals}/{quote_decimals} amp={amp} mask={frozen_mask} input={input:?} cycle={cycle}",bought.saturating_sub(initial));
            assert_eq!(market.amm.frozen_hlp_mask,frozen_mask);
            assert_eq!(market.frozen_curve_offsets().unwrap(), offsets);
            for (target, frozen, before) in [(MarketAsset::Base,frozen_mask&1!=0,base_frozen),(MarketAsset::Quote,frozen_mask&2!=0,quote_frozen)] {
                if frozen {
                    let after=if target == MarketAsset::Base {market.base_hlp_vault} else {market.quote_hlp_vault};
                    assert_eq!(after.ylp_shares,before.ylp_shares);
                    assert_eq!(after.debt_shares,before.debt_shares);
                    assert_eq!(after.debt_principal,before.debt_principal);
                    assert_eq!(after.base_hlp_live_reserve,before.base_hlp_live_reserve);
                    assert_eq!(after.quote_hlp_live_reserve,before.quote_hlp_live_reserve);
                }
            }
         }
        }
       }
      }
    }
}

/// Raw atoms of the six-decimal fixture tokens, in internal amount units.
fn fixture_atoms_nad(market: &Market, atoms: u128) -> u128 {
    market.normalize_amount(atoms, 6).unwrap()
}

#[test]
fn h_lp_entry_accepts_full_value_public_interest_in_both_assets_and_curves() {
    use crate::state::{AmmConfig, AmmState};
    const UNIT: u64 = 1_000_000;
    let mut failures = Vec::new();
    for amplification in [1, 4] {
        for interest_asset in [MarketAsset::Base, MarketAsset::Quote] {
            for target in [MarketAsset::Base, MarketAsset::Quote] {
                let mut market = Market::current_layout();
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
                market.side_mut(interest_asset).reserves.live_reserve += u128::from(25_000 * UNIT);
                market.recognize_interest(interest_asset).unwrap();
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
                            market.public_unpaid_interest(interest_asset).unwrap(),
                            before.public_unpaid_interest(interest_asset).unwrap()
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
                    let mut market = Market::current_layout();
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
                    market.side_mut(interest_asset).reserves.live_reserve += u128::from(25_000 * UNIT);
                    market.recognize_interest(interest_asset).unwrap();
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
                    let eligible_supply = market.tranches.senior_reserve_shares(market.base_side.shares.srlp_supply).unwrap();
                    let eligible_shares = [market.base_hlp_vault.srlp_shares, market.quote_hlp_vault.srlp_shares];
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
                        market.public_unpaid_interest(interest_asset).unwrap(),
                        u128::from(25_000 * UNIT)
                    );
                    for (target, starting_nav) in [
                        (MarketAsset::Base, start.base_hlp_equity),
                        (MarketAsset::Quote, start.quote_hlp_equity),
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
                        assert!(nav.abs_diff(expected_nav) <= fixture_atoms_nad(&market, 10), "fee={fee_bps} compound={compounding_bps} amp={amplification} interest={interest_asset:?} in={asset_in:?} target={target:?}: before={expected_nav}, after={nav}");
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
fn public_repayment_and_writeoff_update_both_hlp_values_once() {
    use crate::state::{AmmConfig, AmmState, BorrowPosition};
    use crate::transitions::{
        flash_liquidation::LiquidationPositionRef, flash_settlement::LiquidationPositionMut,
    };
    const UNIT: u64 = 1_000_000;
    for amplification in [1, 4] {
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            for writeoff in [false, true] {
                let mut market = Market::current_layout();
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
                market.side_mut(asset).reserves.live_reserve += u128::from(25_000 * UNIT);
                market.recognize_accrued_interest(asset).unwrap();
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
                let growth_before = market.side(asset).fees.interest_growth_index_q64;
                let credited_before = market.side(asset).fees.ledger.credited;
                let old_curve = market.curve_reserve(asset).unwrap();
                if writeoff {
                    let allocation = market
                        .liquidation_debt_allocation(&LiquidationPositionRef::Borrow(&position), asset, shares)
                        .unwrap();
                    let (loss, protocol_paid) = market
                        .settle_flash_recovery(&mut LiquidationPositionMut::Borrow(&mut position), asset, allocation, 50_000 * UNIT, 375_000 * UNIT, true, 1)
                        .unwrap();
                    // Principal first: the short recovery pays no interest.
                    assert_eq!(loss.interest_paid, 0);
                    assert_eq!(protocol_paid, 0);
                    assert_eq!(loss.principal_written_off, 25_000 * UNIT);
                    assert_eq!(loss.interest_canceled, 25_000 * UNIT);
                    // Credited interest stays credited; the written-off
                    // interest is a pool loss with the principal.
                    assert_eq!(market.side(asset).fees.interest_growth_index_q64, growth_before);
                    assert_eq!(market.side(asset).fees.ledger.credited, credited_before);
                    assert_eq!(market.curve_reserve(asset).unwrap(), old_curve - 50_000 * UNIT);
                } else {
                    let receipt = market.repay(&mut position, asset, 425_000 * UNIT).unwrap();
                    assert_eq!(receipt.interest_paid, 25_000 * UNIT);
                    assert_eq!(market.curve_reserve(asset).unwrap(), old_curve);
                    assert_eq!(market.side(asset).fees.interest_growth_index_q64, growth_before);
                    assert_eq!(market.side(asset).fees.ledger.credited, credited_before);
                }
                assert_eq!(market.public_unpaid_interest(asset).unwrap(), 0);
                assert_eq!(market.side(asset).fees.ledger.public_recognized, 0);
                market.assert_market_invariants().unwrap();
                let prices = current_hlp_curve_prices(&market).unwrap();
                let navs = [
                    hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap(),
                    hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap(),
                ];
                let plan = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap();
                plan.consume(&mut market, crate::transitions::ProtocolCash::Required).unwrap();
                market.assert_market_invariants().unwrap();
                for (target, expected) in [(MarketAsset::Base, navs[0]), (MarketAsset::Quote, navs[1])] {
                    let nav =
                        hlp_share_nav_nad_with_prices(&market, target, current_hlp_curve_prices(&market).unwrap())
                            .unwrap();
                    assert!(
                        nav.abs_diff(expected) <= fixture_atoms_nad(&market, 10),
                        "amp={amplification} asset={asset:?} target={target:?} writeoff={writeoff}"
                    );
                    if !writeoff {
                        // Repayment moves neither NAV nor the vault's credit.
                        let original_nav = hlp_share_nav_nad_with_prices(&before, target, prices).unwrap();
                        assert!(nav.abs_diff(original_nav) <= fixture_atoms_nad(&market, 10));
                        assert_eq!(
                            market.hlp_yield_growth_indexes(target, asset),
                            before.hlp_yield_growth_indexes(target, asset)
                        );
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
        let mut market = Market::current_layout();
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
        market.side_mut(target).reserves.live_reserve += u128::from(25_000 * UNIT);
        market.recognize_interest(MarketAsset::Base).unwrap();
        market.recognize_interest(MarketAsset::Quote).unwrap();
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
                let mut market = Market::current_layout();
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
                market.side_mut(interest_asset).reserves.live_reserve += u128::from(interest * UNIT);
                market.recognize_interest(interest_asset).unwrap();
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
fn accrued_interest_preserves_raw_curve_and_rehedged_value() {
    use crate::state::{AmmConfig, AmmState};
    const UNIT: u64 = 1_000_000;
    for amplification in [1, 4] {
        for interest_asset in [MarketAsset::Base, MarketAsset::Quote] {
          for unpaid_tokens in [25_000_u64, 250_000, 800_000] {
            let mut market = Market::current_layout();
            market.base_side.asset_decimals = 6;
            market.quote_side.asset_decimals = 6;
            market.debt.base_borrow_index_nad = NAD as u128;
            market.debt.quote_borrow_index_nad = NAD as u128;
            market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
            market.deposit_single_sided(MarketAsset::Base, 200_000 * UNIT, 1).unwrap();
            market.deposit_single_sided(MarketAsset::Quote, 200_000 * UNIT, 1).unwrap();
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
            pre.consume(&mut market, crate::transitions::ProtocolCash::Required).unwrap();
            market.finalize_amm_transition_and_observe_risk(1).unwrap();
            let prices = current_hlp_curve_prices(&market).unwrap();
            let raw_before = market.curve_reserves_nad().unwrap();
            let ordinary_before = market.base_side.shares.srlp_supply - market.base_hlp_vault.srlp_shares - market.quote_hlp_vault.srlp_shares;
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
            market.side_mut(interest_asset).reserves.live_reserve += u128::from(unpaid_tokens * UNIT);
            let credit = market.recognize_accrued_interest(interest_asset).unwrap();
            assert_eq!(credit.lp, unpaid_tokens * UNIT, "amp={amplification} asset={interest_asset:?} unpaid={unpaid_tokens}: {credit:?}");
            // The credit offsets the receivable it creates.
            assert_eq!(market.curve_reserves_nad().unwrap(), raw_before);
            assert_eq!(current_hlp_curve_prices(&market).unwrap(), prices);
            let core_before_rehedge = [
                hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap(),
                hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap(),
            ];
            let post = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap_or_else(|e| panic!("amp={amplification} asset={interest_asset:?} unpaid={unpaid_tokens}: {e}"));
            post.consume(&mut market, crate::transitions::ProtocolCash::Required).unwrap();
            market.finalize_amm_transition_and_observe_risk(1).unwrap();
            market.assert_market_invariants().unwrap();
            let ordinary_after = market.base_side.shares.srlp_supply - market.base_hlp_vault.srlp_shares - market.quote_hlp_vault.srlp_shares;
            assert_eq!(ordinary_before, ordinary_after);
            for target in [MarketAsset::Base, MarketAsset::Quote] {
                let fixed_mark_nav = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
                assert!(fixed_mark_nav.abs_diff(core_before_rehedge[target.code() as usize]) <= fixture_atoms_nad(&market, 10),
                    "rehedge transferred holder value at fixed mark: amp={amplification} asset={interest_asset:?} target={target:?} unpaid={unpaid_tokens}");
            }
        }
    }
    }
}

/// Property 8 of the accrued-interest ledger: an index advance accrues public
/// interest and the borrowing vault's funding together. Accrual nets the
/// vault's public credit against its funding, so its NAV moves by
/// `−funding accrued + netted`; the other vault's credit goes to its holder
/// index, not NAV; curve reserves and spot do not move.
fn assert_accrual_netting(funding_bps: u128, amplification: u64, interest_asset: MarketAsset, unpaid_tokens: u64) {
    use crate::state::{AmmConfig, AmmState};
    const UNIT: u64 = 1_000_000;
    let mut market = Market::current_layout();
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
    market.side_mut(interest_asset).reserves.live_reserve += u128::from(unpaid_tokens * UNIT);
    market.recognize_accrued_interest(interest_asset).unwrap();
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
    pre.consume(&mut market, crate::transitions::ProtocolCash::Required).unwrap();
    market.finalize_amm_transition_and_observe_risk(1).unwrap();
    market.assert_market_invariants().unwrap();
    let prices = current_hlp_curve_prices(&market).unwrap();
    let raw_before = market.curve_reserves_nad().unwrap();
    let nav_before = [
        hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap(),
        hlp_share_nav_nad_with_prices(&market, MarketAsset::Quote, prices).unwrap(),
    ];
    let funding_before = market.funding_unpaid_interest(interest_asset).unwrap();
    let public_before = market.public_unpaid_interest(interest_asset).unwrap();
    let other_index_before = market.hlp_yield_growth_indexes(interest_asset, interest_asset).1;
    // An index advance accrues public interest and the borrowing
    // vault's funding together.
    match interest_asset {
        MarketAsset::Base => market.debt.base_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
        MarketAsset::Quote => market.debt.quote_borrow_index_nad += u128::from(NAD) * funding_bps / 10_000,
    }
    let public_accrued = market.public_unpaid_interest(interest_asset).unwrap() - public_before;
    market.side_mut(interest_asset).reserves.live_reserve += public_accrued;
    let funding_accrued = market.funding_unpaid_interest(interest_asset).unwrap() - funding_before;
    let credit = market.recognize_accrued_interest(interest_asset).unwrap();
    assert!(u128::from(credit.netted) <= funding_accrued + funding_before);
    market.assert_interest_ledger(interest_asset).unwrap();
    assert_eq!(market.curve_reserves_nad().unwrap(), raw_before);
    assert_eq!(current_hlp_curve_prices(&market).unwrap(), prices);
    // The borrowing vault (the opposite target) loses its funding
    // accrual and regains what netting burned from its debt.
    let borrower = interest_asset.opposite();
    let nav_after = hlp_share_nav_nad_with_prices(&market, borrower, prices).unwrap();
    let change = |amount: u128| asset_value_in_target_nad_with_prices(&market, prices, interest_asset, amount as u64, borrower).unwrap();
    let expected = nav_before[borrower.code() as usize] + change(u128::from(credit.netted)) - change(funding_accrued);
    assert!(nav_after.abs_diff(expected) <= fixture_atoms_nad(&market, 10),
        "netting NAV: funding={funding_bps} amp={amplification} asset={interest_asset:?} unpaid={unpaid_tokens}: expected {expected}, observed {nav_after}");
    // The other vault's credit goes to its holder index, not NAV.
    let lender = interest_asset;
    let lender_nav = hlp_share_nav_nad_with_prices(&market, lender, prices).unwrap();
    assert!(lender_nav.abs_diff(nav_before[lender.code() as usize]) <= fixture_atoms_nad(&market, 10));
    if funding_bps > 0 {
        assert!(market.hlp_yield_growth_indexes(lender, interest_asset).1 > other_index_before);
    }
}

#[test]
fn accrual_netting_moves_only_the_borrowing_vault_nav_and_keeps_spot() {
    for funding_bps in [0_u128, 100, 2_000] {
        for amplification in [1, 4, 10] {
            for interest_asset in [MarketAsset::Base, MarketAsset::Quote] {
                for unpaid_tokens in [25_000_u64, 250_000] {
                    assert_accrual_netting(funding_bps, amplification, interest_asset, unpaid_tokens);
                }
            }
        }
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(32))]
    #[test]
    fn accrual_netting_moves_vault_nav_by_netted_minus_funding(
        funding_bps in 0..3_000u128,
        amplification in proptest::prop_oneof![proptest::strategy::Just(1u64), proptest::strategy::Just(4), proptest::strategy::Just(10)],
        quote_interest in proptest::prelude::any::<bool>(),
        unpaid_tokens in 1_000u64..400_000,
    ) {
        let interest_asset = if quote_interest { MarketAsset::Quote } else { MarketAsset::Base };
        assert_accrual_netting(funding_bps, amplification, interest_asset, unpaid_tokens);
    }
}

#[test]
fn frozen_hlp_new_interest_is_held_for_creditors_without_creating_unminted_srlp() {
    const UNIT: u64 = 1_000_000;
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        for income_asset in [MarketAsset::Base, MarketAsset::Quote] {
            for with_active_cohort in [false, true] {
                let mut market = Market::current_layout();
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
                market.recognize_interest(MarketAsset::Base).unwrap();
                market.recognize_interest(MarketAsset::Quote).unwrap();
                market.prepare_amm_for_swap(2).unwrap();
                market.freeze_negative_hlp_vaults().unwrap();
                assert!(market.hlp_frozen(target));
                assert!(!market.hlp_frozen(target.opposite()));
                let frozen_before = if target == MarketAsset::Base {
                    market.base_hlp_vault
                } else {
                    market.quote_hlp_vault
                };
                let supply_before = market.base_side.shares.srlp_supply;
                let price_before = market.current_concentrated_spot_price_nad().unwrap();
                let cash = |m: &Market| [m.base_side.reserves.cash_reserve, m.quote_side.reserves.cash_reserve];
                let cash_before = cash(&market);
                // Public interest accrues on a borrow the reserve already
                // funded: the live reserve gains the receivable.
                let index = market.debt.borrow_index(income_asset);
                let shares = Debt::debt_to_shares(10_000 * UNIT, index).unwrap();
                let accrued = Debt::shares_to_debt(shares, index).unwrap() as u64;
                match income_asset {
                    MarketAsset::Base => market.debt.fixed_base_shares += shares,
                    MarketAsset::Quote => market.debt.fixed_quote_shares += shares,
                }
                market.side_mut(income_asset).reserves.live_reserve += u128::from(accrued);
                market.recognize_interest(income_asset).unwrap();
                let frozen_after = if target == MarketAsset::Base {
                    market.base_hlp_vault
                } else {
                    market.quote_hlp_vault
                };
                // Nothing reaches the frozen vault's holders: its share is
                // held for its creditors (product decision, Ambiguity 8).
                assert_eq!(
                    frozen_after.yield_growth_indexes(income_asset).1,
                    frozen_before.yield_growth_indexes(income_asset).1
                );
                let held = |vault: &HlpVault| match income_asset {
                    MarketAsset::Base => vault.held_base_interest,
                    MarketAsset::Quote => vault.held_quote_interest,
                };
                let held = u128::from(held(&frozen_after) - held(&frozen_before));
                // The junior lane takes its half first, as in the base fixture.
                let expected = mul_div_u128(
                    u128::from(accrued / 2),
                    u128::from(frozen_before.srlp_shares),
                    u128::from(market.tranches.senior_reserve_shares(supply_before).unwrap()),
                )
                .unwrap();
                assert!(held > 0);
                assert!(held.abs_diff(expected) <= 1, "target={target:?} income={income_asset:?} active={with_active_cohort}: held={held} expected={expected} junior={}", market.tranches.junior_reserve_shares);
                assert_eq!(frozen_after.srlp_shares, frozen_before.srlp_shares);
                assert_eq!(frozen_after.debt_shares, frozen_before.debt_shares);
                assert_eq!(frozen_after.debt_principal, frozen_before.debt_principal);
                assert_eq!(market.base_side.shares.srlp_supply, supply_before);
                assert_eq!(cash(&market), cash_before);
                assert_eq!(market.current_concentrated_spot_price_nad().unwrap(), price_before);
                market.assert_market_invariants().unwrap();
            }
        }
    }
}

#[test]
fn hlp_nav_excludes_interest_credited_to_its_holder_index() {
    const UNIT: u64 = 1_000_000;
    let mut market = Market::current_layout();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
    market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
    let prices = current_hlp_curve_prices(&market).unwrap();
    let before = hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap();
    // A closed accrual interval: principal leaves cash, indexed interest
    // enters live reserves and is credited at once.
    market.debt.fixed_base_principal = 400_000 * UNIT;
    market.debt.fixed_base_shares = 410_000 * UNIT as u128;
    market.base_side.reserves.cash_reserve -= 400_000 * UNIT;
    market.base_side.reserves.live_reserve += u128::from(10_000 * UNIT);
    market.recognize_interest(MarketAsset::Base).unwrap();
    assert!(market.hlp_yield_growth_indexes(MarketAsset::Base, MarketAsset::Base).1 > 0);
    let observed = hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap();
    assert!(observed.abs_diff(before) <= 1_000, "hLP NAV counts credited interest: before {before}, observed {observed}");
}

#[test]
fn tranche_hlp_rehedge_preserves_each_vaults_receivable_nav() {
    const UNIT: u64 = 1_000_000;
    for amplification in [1, 4] {
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = Market::current_layout();
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
            market.side_mut(asset).reserves.live_reserve += u128::from(25_000 * UNIT);
            market.recognize_interest(asset).unwrap();
            market.prepare_amm_for_swap(1).unwrap();
            let prices = current_hlp_curve_prices(&market).unwrap();
            let before = [MarketAsset::Base, MarketAsset::Quote].map(|target| hlp_share_nav_nad_with_prices(&market, target, prices).unwrap());
            let plan = prepare_concentrated_hlp_transition_at_current_state(&market).unwrap();
            plan.consume(&mut market, crate::transitions::ProtocolCash::Required).unwrap();
            market.assert_market_invariants().unwrap();
            for target in [MarketAsset::Base, MarketAsset::Quote] {
                let after = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
                assert!(after.abs_diff(before[target.code() as usize]) <= fixture_atoms_nad(&market, 10), "NAV changed: amp={amplification}, income={asset:?}, target={target:?}, before={}, after={after}", before[target.code() as usize]);
                let opposite = hlp_full_underlying_amount(&market, target, target.opposite()).unwrap();
                let vault = if target == MarketAsset::Base { market.base_hlp_vault } else { market.quote_hlp_vault };
                let debt = Debt::shares_to_debt(vault.debt_shares, market.debt.borrow_index(target.opposite())).unwrap();
                assert!(debt.abs_diff(opposite as u128) <= 1);
            }
        }
    }
}

#[test]
fn hlp_exit_after_accrual_needs_no_coupon_conversion_and_does_not_dilute() {
    const UNIT: u64 = 1_000_000;
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = Market::current_layout();
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
            market.side_mut(asset).reserves.live_reserve += u128::from(10_000 * UNIT);
            market.recognize_interest(asset).unwrap();
            market.prepare_amm_for_swap(1).unwrap();
            let static_units = market.base_side.shares.srlp_supply - if target == MarketAsset::Base { market.base_hlp_vault.srlp_shares } else { market.quote_hlp_vault.srlp_shares };
            let ordinary_value = |m: &Market, prices: HlpCurvePrices| [MarketAsset::Base, MarketAsset::Quote].into_iter().map(|a| {
                let amount = srlp_live_underlying_amount_from_values(u128::from(m.curve_reserve(a).unwrap()), m.base_side.shares.srlp_supply, static_units).unwrap();
                asset_value_in_target_nad_with_prices(m, prices, a, amount, MarketAsset::Quote).unwrap()
            }).sum::<u128>();
            let vault_index = market.hlp_yield_growth_indexes(target, asset).1;
            assert!(vault_index > 0);
            let credited = market.side(asset).fees.ledger.credited;
            for fraction in [2, 1] {
                let supply = if target == MarketAsset::Base { market.base_hlp_vault.hlp_supply } else { market.quote_hlp_vault.hlp_supply };
                let amount = supply / fraction;
                let prices = current_hlp_curve_prices(&market).unwrap();
                let before = ordinary_value(&market, prices);
                let quoted = market.quote_hlp_withdrawal(target, amount).unwrap();
                let receipt = market.withdraw_single_sided(target, amount).unwrap();
                assert_eq!(receipt.target_amount_out, quoted);
                market.assert_market_invariants().unwrap();
                assert!(ordinary_value(&market, prices) + 10_000 >= before, "exit diluted ordinary capital: target={target:?} income={asset:?} fraction={fraction} before={before} after={} price={:?}", ordinary_value(&market, prices), prices);
            }
            // Exits leave credited interest with the departed holders' index.
            assert_eq!(market.hlp_yield_growth_indexes(target, asset).1, vault_index);
            assert_eq!(market.side(asset).fees.ledger.credited, credited);
            market.assert_interest_ledger(asset).unwrap();
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
            reserve_vault: Pubkey::new_unique(), collateral_vault: Pubkey::new_unique(), asset_decimals: d, ..MarketSide::default()
         };
         let config = MarketConfig {
            swap_fee_bps: crate::constants::MIN_CONCENTRATED_SWAP_FEE_BPS,
            divergence_fee_share_cap_bps: 0, volatility_fee_share_cap_bps: 0,
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
         let mut market = Market::current_layout();
         market.initialize(Pubkey::new_unique(), Box::new(side(base_decimals)), Box::new(side(quote_decimals)), config, Pubkey::new_unique(), Pubkey::new_unique(), [0;32], Pubkey::new_unique(), 60 * NAD, 0, 2_500, 255).unwrap();
         // Configuration admits a concentrated curve only with the minimum
         // base fee. Remove it after initialization so fee income cannot mask
         // a round trip funded by frozen-hLP reconstruction.
         market.config.swap_fee_bps = 0;
         market.add_liquidity(raw(10_000,base_decimals),raw(600_000,quote_decimals)).unwrap();
         market.finalize_amm_transition_and_observe_risk(2_500).unwrap();
         market.deposit_single_sided(MarketAsset::Base,raw(1_000,base_decimals),1).unwrap();
         market.finalize_amm_transition_and_observe_risk(2_500).unwrap();
         market.deposit_single_sided(MarketAsset::Quote,raw(60_000,quote_decimals),1).unwrap();
         market.finalize_amm_transition_and_observe_risk(2_500).unwrap();
         if frozen_mask & 1 != 0 {market.debt.quote_borrow_index_nad *= 3;}
         if frozen_mask & 2 != 0 {market.debt.base_borrow_index_nad *= 3;}
         market.recognize_interest(MarketAsset::Base).unwrap();
         market.recognize_interest(MarketAsset::Quote).unwrap();
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
                    assert_eq!(after.srlp_shares,before.srlp_shares);
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

#[test]
fn recovery_bonus_is_priced_at_the_executable_mark() {
    use crate::state::{AmmConfig, AmmState};
    use crate::transitions::amm::SwapRequest;
    const UNIT: u64 = 1_000_000;
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = Market::current_layout();
        market.base_side.asset_decimals = 6;
        market.quote_side.asset_decimals = 6;
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
        market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
        market.deposit_single_sided(MarketAsset::Quote, 20_000 * UNIT, 1).unwrap();
        // A funding deficit, as in full_value_recovery_bonus_reports_the_settled_price.
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
        market.side_mut(target).reserves.live_reserve += u128::from(25_000 * UNIT);
        market.recognize_interest(MarketAsset::Base).unwrap();
        market.recognize_interest(MarketAsset::Quote).unwrap();
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
        // The deficit set-up leaves the ordinary point where the reserve ratio
        // and the concentrated curve's marginal price disagree.
        let spot = market.current_concentrated_spot_price_nad().unwrap().unwrap();
        let prepared = SwapRequest {
            current_slot: 1,
            current_unix_timestamp: 0,
            asset_in: target.opposite(),
            reserve_credit: 10_000 * UNIT,
            protocol_fee_bps: 0,
        }
        .prepare(&mut market.clone())
        .unwrap();
        let recovery = prepared.quote.recovery;
        assert!(recovery.bonus_output > 0, "{target:?}: the vault must be under-funded");
        // The matched input valued at the executable mark, then d/(1-d).
        let fair = match target {
            MarketAsset::Base => u128::from(recovery.matched_input) * u128::from(NAD) / u128::from(spot),
            MarketAsset::Quote => u128::from(recovery.matched_input) * u128::from(spot) / u128::from(NAD),
        };
        let bound = fair * u128::from(recovery.discount_bps)
            / (u128::from(BPS_DENOMINATOR) - u128::from(recovery.discount_bps));
        assert!(
            u128::from(recovery.bonus_output) <= bound + 1,
            "{target:?}: bonus {} exceeds the mark-priced bound {bound}",
            recovery.bonus_output
        );
    }
}

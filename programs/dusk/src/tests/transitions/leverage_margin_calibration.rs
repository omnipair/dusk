use super::*;
use crate::math::leverage_margin::LeverageMarginSchedule;

const CALIBRATION_UNIT: u64 = 1_000;

mod stored_margin_runtime {
    use super::*;
    include!("leverage_stored_margin_runtime.rs");
}

mod emergency_anchors {
    use super::*;
    include!("leverage_emergency_anchors.rs");
}

mod stored_depth_schedules {
    use super::*;
    include!("leverage_stored_depth_calibration.rs");
}

mod stored_depth_paths {
    use super::*;
    include!("leverage_stored_depth_paths.rs");
}

mod concentrated_cushion {
    use super::*;
    include!("leverage_concentrated_cushion.rs");
}

fn calibration_market(total_liquidity: u64, amplification: u64) -> Market {
    let side_cash = total_liquidity * CALIBRATION_UNIT / 2;
    let mut market = test_market(side_cash, side_cash);
    market.base_side.asset_decimals = 3;
    market.quote_side.asset_decimals = 3;
    market.config.swap_fee_bps = 30;
    market.config.amm.peak_amplification_nad = amplification * NAD;
    if amplification > 1 {
        market.config.amm.core_half_width_bps = 100;
        market.config.amm.fade_width_bps = 400;
    }
    market.amm = AmmState::default();
    market.risk = Risk::default();
    market.prepare_amm_for_swap(1).unwrap();
    market.refresh_risk().unwrap();
    market
}

fn calibration_schedule() -> LeverageMarginSchedule {
    LeverageMarginSchedule {
        base_initial_bps: 1_000,
        free_exposure_bps: 2_000,
        exposure_slope_numerator: 1,
        exposure_slope_denominator: 10,
        maintenance_boundaries: [10_000_000, 30_000_000],
        maintenance_rates_bps: [700, 800, 1_000],
        entry_buffer_bps: 300,
        recovery_buffer_bps: 200,
    }
}

// Exercise native entry execution/accounting without selecting a runtime risk
// policy. Unlike inspecting mutations after a rejected open, this explicitly
// applies a successful prepared swap, then evaluates competing admission rules.
fn calibration_entry(
    market: &mut Market,
    debt_asset: MarketAsset,
    spend: u64,
    wallet_bps: u16,
    slot: u64,
) -> Result<LeveragePosition> {
    let margin = u64::try_from(ceil_div(u128::from(spend) * u128::from(wallet_bps), 10_000).unwrap()).unwrap();
    let borrowed = spend - margin;
    let policy = SwapCashPolicy::Borrow {
        asset: debt_asset,
        amount: borrowed,
    };
    let mut prepared = SwapRequest {
        current_slot: slot,
        current_unix_timestamp: 0,
        asset_in: debt_asset,
        reserve_credit: spend,
        protocol_fee_bps: 0,
    }
    .prepare_with_cash_policy(market, policy)?;
    let quote = prepared.leverage_quote();
    let finalized = prepared.apply(
        market,
        policy,
        full_fee_credit(&quote),
        slot,
        0,
        ProtocolAuctionSplit::default(),
        None,
    )?;
    let mut position = empty_position();
    position.initialize(
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::default(),
        0,
        debt_asset,
        quote.amount_out,
        margin,
        spend,
        borrowed,
        finalized.lifecycle.added_debt_shares,
        10_000 * spend / margin,
        0,
        slot,
        255,
    );
    position.open_curve_revision = market.curve_revision;
    // Historical calibration controls admission and maintenance outside the
    // runtime open path. Keep that fixture's old liquidation terms explicit.
    position.margin_terms = crate::state::LeverageMarginTerms::at_entry(market.leverage_reference_depth(debt_asset.opposite())?)?;
    position.margin_terms.maintenance_rates_bps = [700; 3];
    match debt_asset.opposite() {
        MarketAsset::Base => market.debt.leverage_base_collateral += position.collateral_amount,
        MarketAsset::Quote => market.debt.leverage_quote_collateral += position.collateral_amount,
    }
    market.assert_market_invariants()?;
    Ok(position)
}

fn reference_credit(market: &Market, asset: MarketAsset, amount: u64) -> u64 {
    let value = market
        .linear_liquidation_collateral_value_nad(asset, amount, &market.risk)
        .unwrap();
    market
        .denormalize_amount_floor(value, market.side(asset.opposite()).asset_decimals)
        .unwrap()
}

#[test]
fn margin_calibration_native_entry_report() {
    let schedule = calibration_schedule();
    println!("MARGIN_CALIBRATION,total_liquidity,amplification,debt_side,entry_spend,wallet_bps,reference_exposure,reference_liquidity,reference_health_bps,execution_health_bps,required_im_bps,candidate_admitted,old_admitted");
    let mut successful_native_entries = 0;
    for liquidity in [25_000, 100_000, 1_000_000] {
        for amplification in [1, 4] {
            for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
                for spend_dollars in [1_000, 5_000, 10_000, 25_000, 50_000] {
                    for wallet_bps in [1_000, 2_000, 3_334, 5_000] {
                        let mut market = calibration_market(liquidity, amplification);
                        let Ok(position) =
                            calibration_entry(&mut market, debt_asset, spend_dollars * CALIBRATION_UNIT, wallet_bps, 2)
                        else {
                            println!("MARGIN_CALIBRATION_REJECT,{liquidity},{amplification},{},{spend_dollars},{wallet_bps},native_execution", debt_asset.code());
                            continue;
                        };
                        successful_native_entries += 1;
                        let collateral_asset = debt_asset.opposite();
                        let debt = position.debt_amount(&market.debt).unwrap();
                        let notional = reference_credit(&market, collateral_asset, position.collateral_amount);
                        let collateral_cash = market.side(collateral_asset).reserves.cash_reserve;
                        let debt_cash = market.side(debt_asset).reserves.cash_reserve;
                        let liquidity_reference =
                            2 * reference_credit(&market, collateral_asset, collateral_cash).min(debt_cash);
                        let Ok(exit) = market.quote_leverage_swap(collateral_asset, position.collateral_amount, 2)
                        else {
                            println!("MARGIN_CALIBRATION_REJECT,{liquidity},{amplification},{},{spend_dollars},{wallet_bps},native_exit", debt_asset.code());
                            continue;
                        };
                        let reference_health = equity_bps(notional, debt).unwrap();
                        let execution_health = equity_bps(exit.amount_out, debt).unwrap();
                        let required = schedule
                            .initial_bps(
                                u128::from(notional),
                                u128::from(notional),
                                u128::from(liquidity_reference),
                            )
                            .unwrap();
                        let candidate_admitted =
                            reference_health >= u128::from(required) && execution_health >= u128::from(required);
                        let old_admitted = market
                            .require_position_initial_leverage_health(&position, 2, 0, LeverageCollateralFee::default())
                            .is_ok();
                        println!("MARGIN_CALIBRATION,{liquidity},{amplification},{},{spend_dollars},{wallet_bps},{notional},{liquidity_reference},{reference_health},{execution_health},{required},{candidate_admitted},{old_admitted}", debt_asset.code());
                        assert_eq!(
                            position.debt_amount(&market.debt).unwrap(),
                            spend_dollars * CALIBRATION_UNIT - position.margin_amount
                        );
                    }
                }
            }
        }
    }
    assert!(successful_native_entries > 100);
}

#[test]
fn native_plain_curve_matches_independent_reserve_depth_example() {
    let mut market = calibration_market(100_000, 1);
    market.config.swap_fee_bps = 0;
    for value in [10_000, 25_000, 50_000] {
        let amount = value * CALIBRATION_UNIT;
        let actual = market
            .quote_leverage_swap(MarketAsset::Base, amount, 2)
            .unwrap()
            .amount_out;
        let reserve = 50_000u128 * u128::from(CALIBRATION_UNIT);
        let expected = reserve * u128::from(amount) / (reserve + u128::from(amount));
        assert!(
            u128::from(actual).abs_diff(expected) <= 2,
            "value={value}, actual={actual}, expected={expected}"
        );
    }
}

#[test]
fn margin_calibration_partial_recovery_report() {
    // These are liquidation-state fixtures, not admission simulations. The
    // native reserve identity includes the seeded isolated debt. Use zero
    // keeper reward to find an optimistic bound on partial recovery; a reward
    // or additional transfer fee can only reduce available repayment here.
    println!("RECOVERY_CALIBRATION,curve_depth,amplification,reference_position,starting_margin_bps,target_bps,full_exit,original_debt,best_partial_reference_bps,first_feasible_sale_bps");
    for amplification in [1, 4] {
        for size in [1_000, 5_000, 10_000, 25_000] {
            for maintenance in [700u16, 1_000, 1_500, 2_000, 3_000] {
                let mut initial = calibration_market(100_000, amplification);
                let collateral = size * CALIBRATION_UNIT;
                let debt = collateral * u64::from(10_000 - maintenance) / 10_000;
                let position = seeded_position(&mut initial, MarketAsset::Quote, debt, collateral);
                let full_exit = initial
                    .quote_leverage_swap(MarketAsset::Base, collateral, 2)
                    .unwrap()
                    .amount_out;
                let target = u128::from(maintenance + 200);
                let mut best_reference_health = 0;
                let mut first_feasible_sale = None;
                for sold_bps in (100u64..10_000).step_by(100) {
                    let mut market = initial.clone();
                    let sold = collateral * sold_bps / 10_000;
                    let policy = SwapCashPolicy::Decrease {
                        debt_asset: MarketAsset::Quote,
                        debt_shares: position.debt_shares,
                        debt_principal: position.debt_principal,
                    };
                    let Ok(mut prepared) = (SwapRequest {
                        current_slot: 2,
                        current_unix_timestamp: 0,
                        asset_in: MarketAsset::Base,
                        reserve_credit: sold,
                        protocol_fee_bps: 0,
                    })
                    .prepare_with_cash_policy(&mut market, policy) else {
                        continue;
                    };
                    let quote = prepared.leverage_quote();
                    if quote.amount_out >= debt {
                        first_feasible_sale.get_or_insert(sold_bps);
                        break;
                    }
                    let Ok(finalized) = prepared.apply(
                        &mut market,
                        policy,
                        full_fee_credit(&quote),
                        2,
                        0,
                        ProtocolAuctionSplit::default(),
                        None,
                    ) else {
                        continue;
                    };
                    let remaining = collateral - sold;
                    let remaining_debt = u64::try_from(finalized.lifecycle.clearance.remaining_debt).unwrap();
                    let reference_value = reference_credit(&market, MarketAsset::Base, remaining);
                    let reference_health = equity_bps(reference_value, remaining_debt).unwrap();
                    best_reference_health = best_reference_health.max(reference_health);
                    let Ok(remaining_exit) = market.quote_leverage_swap(MarketAsset::Base, remaining, 2) else {
                        continue;
                    };
                    let execution_health = equity_bps(remaining_exit.amount_out, remaining_debt).unwrap();
                    if reference_health >= target && execution_health >= target {
                        first_feasible_sale.get_or_insert(sold_bps);
                    }
                    market.assert_market_invariants().unwrap();
                }
                println!("RECOVERY_CALIBRATION,100000,{amplification},{size},{maintenance},{target},{full_exit},{debt},{best_reference_health},{}", first_feasible_sale.map_or_else(|| "none".to_owned(), |value| value.to_string()));
                if amplification == 1 && size == 10_000 && maintenance == 700 {
                    assert!(full_exit < debt);
                    assert!(best_reference_health < target);
                    assert!(first_feasible_sale.is_none());
                }
            }
        }
    }
}

// A price path, not a manipulated account snapshot: open with native debt and
// swap accounting, then execute spot sales. No liquidation is executed on this
// path, so rows compare counterfactual intervention times on the same history.
#[test]
fn margin_calibration_emergency_price_path_report() {
    println!("EMERGENCY_CALIBRATION,amplification,debt_side,entry_spend,step_slots,threshold_bps,elapsed_slots,reference_value,debt,reference_health_bps,execution_health_bps,full_exit,proceeds_reward_25bps,loss_before_insurance");
    let mut crossings = 0;
    for amplification in [1, 4] {
        for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
            for spend in [1_000u64, 5_000, 10_000] {
                for step_slots in [25u64, 150] {
                    let mut market = calibration_market(100_000, amplification);
                    market.config.ema_half_life_ms = 60_000;
                    market.config.directional_ema_half_life_ms = 60_000;
                    market.config.curve_depth_ema_half_life_ms = 60_000;
                    let position =
                        calibration_entry(&mut market, debt_asset, spend * CALIBRATION_UNIT, 5_000, 2).unwrap();
                    let debt = position.debt_amount(&market.debt).unwrap();
                    let collateral_asset = debt_asset.opposite();
                    let entry_reference = reference_credit(&market, collateral_asset, position.collateral_amount);
                    let entry_exit = market
                        .quote_leverage_swap(collateral_asset, position.collateral_amount, 2)
                        .unwrap()
                        .amount_out;
                    let liquidity_reference = 2 * reference_credit(
                        &market,
                        collateral_asset,
                        market.side(collateral_asset).reserves.cash_reserve,
                    )
                    .min(market.side(debt_asset).reserves.cash_reserve);
                    let required_im = calibration_schedule()
                        .initial_bps(
                            entry_reference.into(),
                            entry_reference.into(),
                            liquidity_reference.into(),
                        )
                        .unwrap();
                    assert!(equity_bps(entry_reference, debt).unwrap() >= required_im.into());
                    assert!(equity_bps(entry_exit, debt).unwrap() >= required_im.into());
                    let mut recorded = [false; 6];
                    let mut crossing_losses = [0u64; 6];
                    for step in 1..=100u64 {
                        let slot = 2 + step * step_slots;
                        // Fixed 1% of the current collateral-side cash per step;
                        // 25/150 slots correspond to 10/60 modeled seconds.
                        let sale = market.side(collateral_asset).reserves.cash_reserve / 100;
                        let mut prepared = SwapRequest {
                            current_slot: slot,
                            current_unix_timestamp: (slot * 400 / 1_000) as i64,
                            asset_in: collateral_asset,
                            reserve_credit: sale,
                            protocol_fee_bps: 0,
                        }
                        .prepare_with_cash_policy(&mut market, SwapCashPolicy::Spot)
                        .unwrap();
                        let quote = prepared.leverage_quote();
                        prepared
                            .apply(
                                &mut market,
                                SwapCashPolicy::Spot,
                                full_fee_credit(&quote),
                                slot,
                                0,
                                ProtocolAuctionSplit::default(),
                                None,
                            )
                            .unwrap();
                        market.assert_market_invariants().unwrap();
                        let value = reference_credit(&market, collateral_asset, position.collateral_amount);
                        let reference_health = equity_bps(value, debt).unwrap();
                        let full_exit = market
                            .quote_leverage_swap_at_time(
                                collateral_asset,
                                position.collateral_amount,
                                slot,
                                (slot * 400 / 1_000) as i64,
                            )
                            .unwrap()
                            .amount_out;
                        let execution_health = equity_bps(full_exit, debt).unwrap();
                        for (index, threshold) in [2_000u128, 1_500, 700, 500, 300, 0].into_iter().enumerate() {
                            if !recorded[index] && reference_health <= threshold && execution_health <= threshold {
                                recorded[index] = true;
                                crossings += 1;
                                // Illustrative gross reward paid from execution
                                // proceeds before debt. Not a runtime policy or
                                // a claim that the recipient receives this net
                                // amount on transfer-fee tokens.
                                let reward = full_exit * 25 / 10_000;
                                let loss = debt.saturating_sub(full_exit - reward);
                                crossing_losses[index] = loss;
                                println!("EMERGENCY_CALIBRATION,{amplification},{},{spend},{step_slots},{threshold},{},{value},{debt},{reference_health},{execution_health},{full_exit},{reward},{loss}", debt_asset.code(), step * step_slots);
                            }
                        }
                        if recorded.into_iter().all(|value| value) {
                            break;
                        }
                    }
                    assert!(
                        recorded.into_iter().all(|value| value),
                        "path failed to reach a threshold"
                    );
                    assert!(crossing_losses.windows(2).all(|pair| pair[0] <= pair[1]));
                    if amplification == 1 && debt_asset == MarketAsset::Quote && spend == 10_000 {
                        if step_slots == 150 {
                            assert_eq!(crossing_losses[0], 0);
                            assert!(crossing_losses[2] > 500 * CALIBRATION_UNIT);
                        } else {
                            // Even a 20% reference trigger is late on the
                            // faster path. Earlier intervention is not a
                            // zero-loss promise while the EMA lags.
                            assert!(crossing_losses[0] > 500 * CALIBRATION_UNIT);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(crossings, 144);
}

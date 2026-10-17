// Native counterfactual trigger/quote paths. These do not execute the proposed
// flash instructions or count first-trigger quotes as realized recoveries.
use super::stored_depth_schedules::{depth_schedule, stored_cash_depth};

fn native_spot_sale(market: &mut Market, asset: MarketAsset, amount: u64, slot: u64) -> Result<()> {
    let mut prepared = SwapRequest {
        current_slot: slot,
        current_unix_timestamp: (slot * 400 / 1_000) as i64,
        asset_in: asset,
        reserve_credit: amount,
        protocol_fee_bps: 0,
    }
    .prepare_with_cash_policy(market, SwapCashPolicy::Spot)?;
    let quote = prepared.leverage_quote();
    prepared.apply(
        market,
        SwapCashPolicy::Spot,
        full_fee_credit(&quote),
        slot,
        0,
        ProtocolAuctionSplit::default(),
        None,
    )?;
    market.assert_market_invariants()
}

// Ordinary buyer keeps output above its bound payment. The native quote includes
// AMM fees; this experiment has legacy tokens and no routing/transfer fees.
fn profitable_partial_bps(
    market: &Market,
    position: &LeveragePosition,
    schedule: &LeverageMarginSchedule,
    slot: u64,
) -> u64 {
    let asset = position.collateral_asset().unwrap();
    let value = reference_credit(market, asset, position.collateral_amount);
    let debt = position.debt_amount(&market.debt).unwrap();
    if value <= debt {
        return 0;
    }
    let before = equity_bps(value, debt).unwrap();
    let mm = u128::from(
        schedule
            .effective_maintenance_bps(position.collateral_amount.into())
            .unwrap(),
    );
    let discount = 50 + 250 * mm.saturating_sub(before) / mm;
    for sold_bps in (100u64..10_000).step_by(100) {
        let sold = position.collateral_amount * sold_bps / 10_000;
        let sold_value = reference_credit(market, asset, sold);
        let payment = u64::try_from(ceil_div(u128::from(sold_value) * (10_000 - discount), 10_000).unwrap()).unwrap();
        let Ok(quote) = market.quote_leverage_swap_at_time(asset, sold, slot, (slot * 400 / 1_000) as i64) else {
            continue;
        };
        if quote.amount_out < payment + 50 {
            continue;
        } // $0.05 candidate cost at three decimals
        let fee = u64::try_from(ceil_div(u128::from(payment) * 20, 10_000).unwrap()).unwrap();
        let repayment = payment - fee;
        if repayment >= debt {
            continue;
        }
        let left = position.collateral_amount - sold;
        let after = equity_bps(reference_credit(market, asset, left), debt - repayment).unwrap();
        let after_mm = u128::from(schedule.effective_maintenance_bps(left.into()).unwrap());
        if after > before && after + mm > before + after_mm {
            return sold_bps;
        }
    }
    0
}

#[test]
fn margin_calibration_native_stored_depth_paths() {
    println!("STORED_DEPTH_PATH,amplification,debt_side,spend,step_slots,schedule,admitted,mm_bps,im_bps,threshold_fraction_bps,elapsed_slots,reference_value,debt,reference_health,full_exit,full_principal_shortfall_before_reward,profitable_partial_bps");
    let mut crossings = 0;
    for amplification in [1, 4] {
        for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
            for spend in [1_000u64, 5_000, 15_000] {
                for step_slots in [25u64, 150] {
                    let mut market = calibration_market(100_000, amplification);
                    market.config.ema_half_life_ms = 60_000;
                    market.config.directional_ema_half_life_ms = 60_000;
                    market.config.curve_depth_ema_half_life_ms = 60_000;
                    let position =
                        calibration_entry(&mut market, debt_asset, spend * CALIBRATION_UNIT, 5_000, 2).unwrap();
                    let asset = debt_asset.opposite();
                    let stored_depth = stored_cash_depth(&market, asset);
                    let entry_value = reference_credit(&market, asset, position.collateral_amount);
                    let debt = position.debt_amount(&market.debt).unwrap();
                    let entry_exit = market
                        .quote_leverage_swap(asset, position.collateral_amount, 2)
                        .unwrap()
                        .amount_out;
                    let schedules = [0, 1, 2, 3].map(|index| depth_schedule(index, stored_depth));
                    let margins = schedules.map(|schedule| {
                        let mm = schedule
                            .effective_maintenance_bps(position.collateral_amount.into())
                            .unwrap();
                        let im = schedule
                            .initial_bps(
                                position.collateral_amount.into(),
                                position.collateral_amount.into(),
                                stored_depth,
                            )
                            .unwrap();
                        let admitted = equity_bps(entry_value, debt)
                            .unwrap()
                            .min(equity_bps(entry_exit, debt).unwrap())
                            >= u128::from(im);
                        (mm, im, admitted)
                    });
                    let mut recorded = [[false; 4]; 4];
                    for step in 1..=250u64 {
                        let slot = 2 + step * step_slots;
                        let sale = market.side(asset).reserves.cash_reserve / 100;
                        native_spot_sale(&mut market, asset, sale, slot).unwrap();
                        let value = reference_credit(&market, asset, position.collateral_amount);
                        let h = equity_bps(value, debt).unwrap();
                        let exit = market
                            .quote_leverage_swap_at_time(
                                asset,
                                position.collateral_amount,
                                slot,
                                (slot * 400 / 1_000) as i64,
                            )
                            .unwrap()
                            .amount_out;
                        for (index, schedule) in schedules.iter().enumerate() {
                            let (mm, im, admitted) = margins[index];
                            for (stage, fraction) in [10_000u64, 2_500, 5_000, 7_500].into_iter().enumerate() {
                                let threshold = u64::from(mm) * fraction / 10_000;
                                if recorded[index][stage] || h > u128::from(threshold) {
                                    continue;
                                }
                                recorded[index][stage] = true;
                                let partial = profitable_partial_bps(&market, &position, schedule, slot);
                                let shortfall = debt.saturating_sub(exit);
                                println!("STORED_DEPTH_PATH,{amplification},{},{spend},{step_slots},{index},{admitted},{mm},{im},{fraction},{},{value},{debt},{h},{exit},{shortfall},{partial}", debt_asset.code(), step * step_slots);
                                crossings += 1;
                            }
                        }
                        if recorded.into_iter().flatten().all(|done| done) {
                            break;
                        }
                    }
                    assert!(
                        recorded.into_iter().flatten().all(|done| done),
                        "missing crossing: amp={amplification}, spend={spend}, debt={debt_asset:?}"
                    );
                }
            }
        }
    }
    assert_eq!(crossings, 384);
}

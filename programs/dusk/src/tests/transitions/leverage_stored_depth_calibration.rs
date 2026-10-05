// Native entry/quote comparison of size schedules, not runtime policy. Bands
// use collateral atoms relative to a stored post-entry cash depth. Both are
// repriced together, so changing the reference unit does not rebase the tier.
const DEPTH_SCHEDULES: [([u16; 3], [u128; 2]); 4] = [
    ([700, 800, 1_000], [2_000, 6_000]),
    ([700, 1_200, 3_000], [500, 1_500]),
    ([700, 2_000, 4_000], [500, 1_500]),
    ([700, 2_500, 4_500], [500, 1_500]),
];

pub(super) fn stored_cash_depth(market: &Market, collateral_asset: MarketAsset) -> u128 {
    let collateral_cash = market.side(collateral_asset).reserves.cash_reserve;
    let debt_cash = market.side(collateral_asset.opposite()).reserves.cash_reserve;
    let collateral_value = reference_credit(market, collateral_asset, collateral_cash);
    let cash_depth =
        u128::from(collateral_cash) * u128::from(collateral_value.min(debt_cash)) / u128::from(collateral_value);
    // Use the already maintained curve-depth EMA to cap newly observed depth.
    // Cash remains a separate bound: amplification/receivables cannot substitute
    // for physical inventory. This remains a candidate, not runtime admission.
    let cache = market.amm.concentrated_curve_cache;
    let current_depth = cache.tail_liquidity + cache.concentrated_liquidity;
    let depth_cap = market.risk.pessimistic_depth_nad().min(current_depth);
    cash_depth * depth_cap / current_depth
}

pub(super) fn depth_schedule(index: usize, stored_depth: u128) -> LeverageMarginSchedule {
    let (rates, boundaries) = DEPTH_SCHEDULES[index];
    let mut schedule = calibration_schedule();
    schedule.maintenance_rates_bps = rates;
    schedule.maintenance_boundaries = boundaries.map(|bps| stored_depth * bps / 10_000);
    schedule
}

#[test]
fn margin_calibration_native_stored_depth_schedules() {
    println!("STORED_DEPTH_ENTRY,liquidity,amplification,debt_side,spend,wallet_bps,schedule,collateral,stored_depth,mm_bps,im_bps,reference_health,execution_health,admitted");
    let mut cases = 0;
    let mut admitted = [0usize; 4];
    for liquidity in [25_000, 100_000, 1_000_000] {
        for amplification in [1, 4] {
            for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
                for spend_bps in [100u64, 500, 1_500, 3_000] {
                    for wallet_bps in [2_000, 5_000] {
                        let mut market = calibration_market(liquidity, amplification);
                        let spend = liquidity * CALIBRATION_UNIT * spend_bps / 10_000;
                        let Ok(position) = calibration_entry(&mut market, debt_asset, spend, wallet_bps, 2) else {
                            continue;
                        };
                        let collateral = position.collateral_amount;
                        let collateral_asset = debt_asset.opposite();
                        let value = reference_credit(&market, collateral_asset, collateral);
                        let stored_depth = stored_cash_depth(&market, collateral_asset);
                        let debt = position.debt_amount(&market.debt).unwrap();
                        let reference_health = equity_bps(value, debt).unwrap();
                        let Ok(exit) = market.quote_leverage_swap(collateral_asset, collateral, 2) else {
                            continue;
                        };
                        let execution_health = equity_bps(exit.amount_out, debt).unwrap();
                        for (index, total_admitted) in admitted.iter_mut().enumerate() {
                            let schedule = depth_schedule(index, stored_depth);
                            let mm = schedule.effective_maintenance_bps(u128::from(collateral)).unwrap();
                            let im = schedule
                                .initial_bps(u128::from(collateral), u128::from(collateral), stored_depth)
                                .unwrap();
                            let passes = reference_health >= u128::from(im) && execution_health >= u128::from(im);
                            *total_admitted += usize::from(passes);
                            assert!(im >= mm + 300);
                            if u128::from(collateral) <= schedule.maintenance_boundaries[0] {
                                assert_eq!(mm, 700);
                            }
                            println!("STORED_DEPTH_ENTRY,{liquidity},{amplification},{},{spend},{wallet_bps},{index},{collateral},{stored_depth},{mm},{im},{reference_health},{execution_health},{passes}", debt_asset.code());
                            cases += 1;
                        }
                        market.assert_market_invariants().unwrap();
                    }
                }
            }
        }
    }
    assert!(cases >= 300);
    assert!(admitted.iter().all(|n| *n > 0));
    assert!(admitted.windows(2).all(|pair| pair[0] >= pair[1]));
}

#[test]
fn stored_depth_candidate_rejects_instant_depth_inflation_and_does_not_rebase_on_withdrawal() {
    for amplification in [1, 4] {
        let mut market = calibration_market(100_000, amplification);
        let initial = stored_cash_depth(&market, MarketAsset::Base);
        let initial_value = reference_credit(&market, MarketAsset::Base, 1_000);
        let schedule = depth_schedule(2, initial);
        let position_size = initial / 5;
        let original_mm = schedule.effective_maintenance_bps(position_size).unwrap();
        let added = market
            .add_liquidity(50_000 * CALIBRATION_UNIT, 50_000 * CALIBRATION_UNIT)
            .unwrap();
        market.finalize_amm_transition_and_observe_risk(1).unwrap();
        let transient = stored_cash_depth(&market, MarketAsset::Base);
        assert!(
            transient.abs_diff(initial) <= 1,
            "immediate added depth inflated capacity: {initial} -> {transient}"
        );
        market.remove_liquidity(added.ylp_amount).unwrap();
        market.finalize_amm_transition_and_observe_risk(1).unwrap();
        let more_shares = market.base_side.shares.ylp_supply / 4;
        market.remove_liquidity(more_shares).unwrap();
        market.finalize_amm_transition_and_observe_risk(1).unwrap();
        let current = stored_cash_depth(&market, MarketAsset::Base);
        assert!(current < initial);
        assert_eq!(schedule.effective_maintenance_bps(position_size).unwrap(), original_mm);
        assert!(
            depth_schedule(2, current)
                .effective_maintenance_bps(position_size)
                .unwrap()
                > original_mm
        );
        // The same stored collateral and symmetric EMA have the same value.
        assert_eq!(reference_credit(&market, MarketAsset::Base, 1_000), initial_value);
        market.assert_market_invariants().unwrap();
    }
}

// Opt-in continuation of each eligible terminal position. Other positions keep
// their balances; this is a diagnostic continuation, not a new whole-market path.
// The zero-divergence variant changes only a scratch market, never production.
fn diagnose_unfilled(
    market: &Market,
    position: &LeveragePosition,
    case: Case,
    fee_bps: u16,
    wallet_bps: u16,
    external: bool,
    position_index: usize,
) {
    let fee = audit_fee(fee_bps);
    let side = case.debt_asset;
    let mark = match case.path {
        0 => NAD * 46 / 100,
        1 => NAD * 55 / 100,
        _ => NAD * 80 / 100,
    };
    let id = format!(
        "{}-{}-{}-{fee_bps}-{wallet_bps}-{}-{}-{external}-{position_index}",
        case.amp,
        case.hlp,
        side.code(),
        case.pieces,
        case.path
    );
    let before = market
        .flash_liquidation_health(&Ref::Leverage(position), side, fee)
        .unwrap();
    println!(
        "UNFILLED_STATE,{id},{},{},{},{},{},{},{}",
        position.collateral_amount,
        before.collateral_value,
        before.debt,
        before.maintenance_bps,
        before.emergency_allowed().unwrap(),
        0,
        price(market, side.opposite())
    );
    for base_only in [false, true] {
        for bps in [100, 10_000] {
            let mut trial = market.clone();
            if base_only {
                trial.config.amm.divergence_fee_coefficient_nad = 0;
            }
            let input = fee.unwind_credit(position.collateral_amount * bps / 10_000).unwrap();
            let swap = SwapRequest {
                current_slot: 1352,
                current_unix_timestamp: 540,
                asset_in: side.opposite(),
                reserve_credit: input,
                protocol_fee_bps: 0,
            }
            .prepare_with_cash_policy(&mut trial, SwapCashPolicy::LiquidationQuote);
            if let Ok(swap) = swap {
                println!(
                    "UNFILLED_COST,{id},{base_only},{bps},{input},{},{},{},{},{}",
                    swap.quote.amount_out,
                    swap.quote.fee.base_fee_rate_nad,
                    swap.quote.fee.divergence_fee_rate_nad,
                    swap.quote.fee.volatility_fee_rate_nad,
                    trial.amm.center_price_nad
                );
            }
        }
    }
    let sizes = [5_000, 2_000, 1_000, 500, 250, 100, 50, 25, 10, 5, 1];
    let status = |result: Result<()>| match result {
        Ok(()) => "accepted".to_owned(),
        Err(error) => error_name(error),
    };
    let flash_full = status(
        market
            .quote_flash_liquidation(
                &Ref::Leverage(position),
                side,
                before.debt,
                true,
                fee,
                fee,
                fee,
                540,
                1352,
            )
            .map(|_| ()),
    );
    let amm_full = status(
        market
            .clone()
            .prepare_emergency_liquidation(
                &Ref::Leverage(position),
                side,
                position.collateral_amount,
                true,
                fee,
                fee,
                fee,
                540,
                1352,
                0,
            )
            .map(|_| ()),
    );
    let mut flash_partials = 0;
    let mut amm_partials = 0;
    let mut maximum_amm_reward = 0;
    for bps in sizes {
        if market
            .quote_flash_liquidation(
                &Ref::Leverage(position),
                side,
                before.debt * bps / 10_000,
                false,
                fee,
                fee,
                fee,
                540,
                1352,
            )
            .is_ok()
        {
            flash_partials += 1;
        }
        if let Ok(candidate) = market.clone().prepare_emergency_liquidation(
            &Ref::Leverage(position),
            side,
            position.collateral_amount * bps / 10_000,
            false,
            fee,
            fee,
            fee,
            540,
            1352,
            0,
        ) {
            amm_partials += 1;
            maximum_amm_reward = maximum_amm_reward.max(candidate.quote.reward_credit);
        }
    }
    println!("UNFILLED_GATE,{id},{flash_full},{amm_full},{flash_partials},{amm_partials},{maximum_amm_reward}");
    for (name, steps, base_only) in [
        ("wider_grid", 0, false),
        ("extended", 40, false),
        ("zero_divergence", 40, true),
    ] {
        let mut market = market.clone();
        let mut position = position.clone();
        if base_only {
            market.config.amm.divergence_fee_coefficient_nad = 0;
        }
        let mut totals = AuditTotals::default();
        let mut first_fill_at = None;
        for step in 0..=steps {
            let now = 540 + step * 30;
            let slot = 1352 + step as u64 * 75;
            market.accrue_interest_to_slot(slot).unwrap();
            if step > 0 {
                stress_to(&mut market, side.opposite(), mark, slot).unwrap();
            }
            for _ in 0..100 {
                if position.debt_shares == 0 {
                    break;
                }
                if !audit_fill_sizes(
                    &mut market,
                    &mut position,
                    side,
                    fee,
                    now,
                    slot,
                    external,
                    mark,
                    &mut totals,
                    &sizes,
                )
                .unwrap()
                {
                    break;
                }
                first_fill_at.get_or_insert(now);
            }
            market.assert_market_invariants().unwrap();
            if position.debt_shares == 0 {
                break;
            }
        }
        let after = market
            .flash_liquidation_health(&Ref::Leverage(&position), side, fee)
            .unwrap();
        println!(
            "UNFILLED_CONTINUE,{id},{name},{},{},{},{},{},{},{},{},{},{}",
            after.debt,
            after.collateral_value,
            after.eligible().unwrap(),
            after.emergency_allowed().unwrap(),
            totals.flash,
            totals.emergency,
            totals.full,
            totals.writeoff,
            totals.fees,
            first_fill_at.unwrap_or(-1)
        );
    }
}

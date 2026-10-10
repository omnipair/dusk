// Current-admission, no-timer economic comparison. No production instructions
// or defaults are changed. Accrual is frozen: principal == debt throughout.
// Internal settlement offsets ONLY the principal actually repaid. It uses the
// existing native Close lifecycle for that debt slice, then the native terminal
// loss adapter. This is not an implementation of flash sessions or token CPIs.
use spl_token_2022::extension::transfer_fee::TransferFee;

#[derive(Clone, Copy, Debug)]
struct Scenario {
    case: Case,
    fee_bps: u16,
    emergency_first: bool,
    critical: u64,
}

#[derive(Default)]
struct Outcome {
    totals: Totals,
    transfer_cost: u64,
    buyer_profit: u64,
    sold_reference: u64,
    survivor_equity: u64,
    first_emergency_step: u64,
    first_emergency_health: u64,
    first_emergency_mm: u64,
    emergency_with_partial: u64,
    cash_rejections: u64,
    eligible_above_emergency: u64,
    eligible_above_emergency_debt: u64,
    largest_eligible_debt: u64,
    residual_shortfall: u64,
    residual_quote_failures: u64,
}

fn token_fee(bps: u16) -> LeverageCollateralFee {
    LeverageCollateralFee::new((bps > 0).then_some(TransferFee {
        epoch: 0.into(),
        maximum_fee: u64::MAX.into(),
        transfer_fee_basis_points: bps.into(),
    }))
}

fn open_current(market: &mut Market, case: Case, fee: LeverageCollateralFee) -> Result<LeveragePosition> {
    let spend = 15_000 * CALIBRATION_UNIT / case.pieces;
    let margin = spend / 2;
    let prepared = SwapRequest {
        current_slot: 2,
        current_unix_timestamp: 0,
        asset_in: case.debt_asset,
        reserve_credit: spend,
        protocol_fee_bps: 0,
    }
    .prepare_with_cash_policy(
        market,
        SwapCashPolicy::Borrow {
            asset: case.debt_asset,
            amount: margin,
        },
    )?;
    let quote = prepared.leverage_quote();
    let mut position = empty_position();
    market.open_leverage(
        &mut position,
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::default(),
        0,
        case.debt_asset,
        margin,
        20_000,
        fee.unwind_credit(quote.amount_out)?,
        prepared,
        full_fee_credit(&quote),
        0,
        2,
        255,
        0,
        ProtocolAuctionSplit::default(),
        fee,
    )?;
    Ok(position)
}

fn quote_sale(
    market: &Market,
    position: &LeveragePosition,
    sold: u64,
    slot: u64,
    fee: LeverageCollateralFee,
) -> Result<(Market, PreparedLeverageSwap)> {
    let mut trial = market.clone();
    no_accrual(&mut trial, slot);
    // This policy selects the existing integrated path instead of imposing the
    // ordinary spot swap's full-output cash debit. No loss is applied here.
    let prepared = SwapRequest {
        current_slot: slot,
        current_unix_timestamp: (slot * 400 / 1_000) as i64,
        asset_in: position.collateral_asset()?,
        reserve_credit: fee.unwind_credit(sold)?,
        protocol_fee_bps: 0,
    }
    .prepare_with_cash_policy(
        &mut trial,
        SwapCashPolicy::Liquidate {
            debt_asset: position.debt_asset()?,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
            insurance_credit: 0,
        },
    )?;
    Ok((trial, prepared))
}

fn health(market: &Market, position: &LeveragePosition, fee: LeverageCollateralFee) -> u64 {
    market
        .leverage_reference_equity_bps(position, fee.unwind_credit(position.collateral_amount).unwrap())
        .unwrap() as u64
}

fn purchase(market: &Market, position: &LeveragePosition, slot: u64, fee: LeverageCollateralFee) -> Option<Fill> {
    let debt = position.debt_amount(&market.debt).unwrap();
    let h = health(market, position, fee);
    let mm = u64::from(
        position
            .margin_terms
            .maintenance_bps(position.collateral_amount)
            .unwrap(),
    );
    let discount = 50 + 450 * mm.saturating_sub(h) / mm;
    let asset = position.collateral_asset().unwrap();
    let mut partial = None;
    for bps in (100..=10_000).step_by(100) {
        let sold = position.collateral_amount * bps / 10_000;
        if sold == 0 {
            continue;
        }
        let Ok((_, prepared)) = quote_sale(market, position, sold, slot, fee) else {
            continue;
        };
        let output = prepared.quote.amount_out;
        let value = reference_credit(market, asset, fee.unwind_credit(sold).unwrap());
        let payment = (u128::from(value) * u128::from(10_000 - discount)).div_ceil(10_000) as u64;
        // Payment is net debt credit. Only actual outward profit/contribution/
        // owner payments transfer debt tokens in this netted internal model.
        if output < payment || fee.unwind_credit(output - payment).unwrap() < 50 {
            continue;
        }
        let full = bps == 10_000;
        let contribution = if full {
            payment.saturating_sub(debt).min(debt * 20 / 10_000)
        } else {
            payment * 20 / 10_000
        };
        let repay = (payment - contribution).min(debt);
        if repay == 0 {
            continue;
        }
        let fill = Fill {
            kind: u8::from(full),
            sold,
            output,
            payment,
            contribution,
            reward: 0,
        };
        if full {
            return partial.or(Some(fill));
        }
        if repay >= debt {
            continue;
        }
        let left = position.collateral_amount - sold;
        let after_value = reference_credit(market, asset, fee.unwind_credit(left).unwrap());
        let after = equity_bps(after_value, debt - repay).unwrap() as u64;
        let after_mm = u64::from(position.margin_terms.maintenance_bps(left).unwrap());
        if after <= h || after + mm <= h + after_mm {
            continue;
        }
        partial = Some(fill);
        if after >= after_mm + 200 {
            return partial;
        }
    }
    partial
}

fn emergency(market: &Market, position: &LeveragePosition, slot: u64, scenario: Scenario) -> Option<Fill> {
    let fee = token_fee(scenario.fee_bps);
    let h = health(market, position, fee);
    let mm = u64::from(
        position
            .margin_terms
            .maintenance_bps(position.collateral_amount)
            .unwrap(),
    );
    // 6667 is the nearest basis-point representation of two thirds.
    if h * 10_000 > mm * scenario.critical {
        return None;
    }
    let (_, prepared) = quote_sale(market, position, position.collateral_amount, slot, fee).ok()?;
    let output = prepared.quote.amount_out;
    let reward = (u128::from(output) * u128::from(mm.saturating_sub(h)) * 100 / u128::from(mm) / 10_000) as u64;
    if fee.unwind_credit(reward).unwrap() < 50 {
        return None;
    }
    let debt = position.debt_amount(&market.debt).unwrap();
    let payment = output - reward;
    Some(Fill {
        kind: 3,
        sold: position.collateral_amount,
        output,
        payment,
        contribution: payment.saturating_sub(debt).min(debt * 20 / 10_000),
        reward,
    })
}

// Observe actual native full-sale quotes before insurance at the first eligible
// and emergency-permitted observations. A quote is neither execution permission
// nor proof of future recovery. Logging must not mutate either live account.
fn report_headroom(
    market: &Market,
    position: &LeveragePosition,
    scenario: Scenario,
    step: u64,
    position_index: usize,
    stage: &str,
) {
    let case = scenario.case;
    let slot = 3 + step * if case.path == 1 { 25 } else { 150 };
    let fee = token_fee(scenario.fee_bps);
    let h = health(market, position, fee);
    let mm = u64::from(
        position
            .margin_terms
            .maintenance_bps(position.collateral_amount)
            .unwrap(),
    );
    let debt = position.debt_amount(&market.debt).unwrap();
    let reference = reference_credit(
        market,
        position.collateral_asset().unwrap(),
        fee.unwind_credit(position.collateral_amount).unwrap(),
    );
    let (output, reward, contribution, net, quote_ok) =
        match quote_sale(market, position, position.collateral_amount, slot, fee) {
            Ok((_, prepared)) => {
                let output = prepared.quote.amount_out;
                let reward =
                    (u128::from(output) * u128::from(mm.saturating_sub(h)) * 100 / u128::from(mm) / 10_000) as u64;
                let payment = output - reward;
                let contribution = payment.saturating_sub(debt).min(debt * 20 / 10_000);
                (output, reward, contribution, payment - contribution, true)
            }
            Err(_) => (0, 0, 0, 0, false),
        };
    println!(
        "EMERGENCY_HEADROOM,{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        scenario.critical,
        case.amp,
        case.controller,
        case.hlp,
        case.debt_asset.code(),
        case.pieces,
        case.path,
        case.withdrawal,
        scenario.fee_bps,
        scenario.emergency_first,
        position_index,
        stage,
        step,
        h,
        mm,
        debt,
        reference,
        output,
        reward,
        contribution,
        format_args!("{net},{quote_ok}")
    );
}

fn settle_netted(
    market: &Market,
    position: &LeveragePosition,
    fill: Fill,
    slot: u64,
    fee: LeverageCollateralFee,
) -> Result<(Market, LeveragePosition, Paid)> {
    let (mut trial, mut prepared) = quote_sale(market, position, fill.sold, slot, fee)?;
    let quote = prepared.leverage_quote();
    require_eq!(quote.amount_out, fill.output, ErrorCode::BrokenInvariant);
    let mut next = position.clone();
    let debt_asset = next.debt_asset()?;
    let debt = next.debt_amount(&trial.debt)?;
    require_eq!(u128::from(debt), next.debt_principal, ErrorCode::BrokenInvariant);
    let repaid = (fill.payment - fill.contribution).min(debt);
    let slice = trial
        .debt
        .isolated_repayment_for_max(debt_asset, next.debt_shares, repaid)?;
    require_eq!(slice.cash_repaid, repaid, ErrorCode::BrokenInvariant);
    let policy = SwapCashPolicy::Close {
        debt_asset,
        debt_shares: slice.shares_to_burn,
        debt_principal: repaid.into(),
    };
    prepared.cash_policy = policy;
    let finalized = prepared.apply(
        &mut trial,
        policy,
        full_fee_credit(&quote),
        slot,
        0,
        ProtocolAuctionSplit::default(),
        None,
    )?;
    require_eq!(
        finalized.lifecycle.clearance.cash_repaid,
        repaid,
        ErrorCode::BrokenInvariant
    );
    next.debt_shares -= slice.shares_to_burn;
    next.debt_principal -= u128::from(repaid);
    let mut paid = Paid {
        repaid,
        owner: fee.unwind_credit(fill.payment.saturating_sub(fill.contribution + debt))?,
        reward: fee.unwind_credit(fill.reward)?,
        contribution: fee.unwind_credit(fill.contribution)?,
        ..Paid::default()
    };
    if paid.contribution > 0 {
        trial.insurance.credit(debt_asset, paid.contribution, slot)?;
    }
    if fill.kind > 0 && next.debt_shares > 0 {
        let remaining = next.debt_amount(&trial.debt)?;
        let spent = trial
            .insurance
            .draw_capacity(debt_asset, slot)?
            .min(fee.effective_gross_for_credit(remaining)?);
        let credit = fee.unwind_credit(spent)?;
        let receipt = trial.liquidate_leverage_position(
            &mut next,
            None,
            0,
            LeverageSwapFeeCredit::default(),
            LeverageInsuranceDraw { spent, credit },
            0,
            ProtocolAuctionSplit::default(),
            slot,
        )?;
        paid.insurance = receipt.insurance_drawn;
        paid.loss = receipt.principal_written_off;
    } else {
        trial.reduce_leverage_collateral(&mut next, fill.sold)?;
    }
    if fill.kind > 0 {
        require_eq!(next.collateral_amount, 0, ErrorCode::BrokenInvariant);
        require_eq!(next.debt_shares, 0, ErrorCode::BrokenInvariant);
        require_eq!(next.debt_principal, 0, ErrorCode::BrokenInvariant);
    }
    trial.assert_market_invariants()?;
    require_eq!(trial.base_hlp_vault.residual_exposure, 0, ErrorCode::BrokenInvariant);
    require_eq!(trial.quote_hlp_vault.residual_exposure, 0, ErrorCode::BrokenInvariant);
    Ok((trial, next, paid))
}

fn run(scenario: Scenario) -> Outcome {
    let case = scenario.case;
    let fee = token_fee(scenario.fee_bps);
    let asset = case.debt_asset.opposite();
    let mut market = initialize(case);
    let mut positions = Vec::new();
    let mut identities = Vec::new();
    let mut seen_eligible = std::collections::HashSet::new();
    let mut seen_emergency = std::collections::HashSet::new();
    let mut o = Outcome::default();
    for _ in 0..case.pieces {
        let mut trial = market.clone();
        no_accrual(&mut trial, 2);
        match open_current(&mut trial, case, fee) {
            Ok(position) => {
                o.totals.original_debt += position.debt_amount(&trial.debt).unwrap();
                o.totals.opened += 1;
                identities.push(position.owner);
                positions.push(position);
                market = trial;
            }
            Err(e) => {
                o.totals.blocked = format!("entry:{}", error_name(e));
                break;
            }
        }
    }
    let mut target = NAD;
    let mut last_slot = 3;
    for step in 0..=180 {
        if positions.is_empty() {
            break;
        }
        let slot = 3 + step * if case.path == 1 { 25 } else { 150 };
        last_slot = slot;
        no_accrual(&mut market, slot);
        if step == 2 && case.withdrawal > 0 {
            let mut trial = market.clone();
            let shares = trial.base_side.shares.ylp_supply * case.withdrawal / 10_000;
            if let Err(e) = trial
                .remove_liquidity(shares)
                .and_then(|_| trial.finalize_amm_transition_and_observe_risk(slot))
            {
                o.totals.blocked = format!("withdrawal:{}", error_name(e));
                break;
            }
            market = trial;
        }
        if step > 0 {
            target = match case.path {
                0 => target * 9950 / 10_000,
                1 => target * 9700 / 10_000,
                2 => NAD * 55 / 100,
                _ if step <= 60 => target * 9940 / 10_000,
                _ => (target * 10060 / 10_000).min(NAD),
            };
        }
        // Follow the same exogenous price target in both directions with real
        // countertrades. This supplies arbitrage capital after liquidation too.
        let result = if price(&market, asset) >= target {
            stress_to(&mut market, asset, target, slot)
        } else {
            stress_to(
                &mut market,
                asset.opposite(),
                (u128::from(NAD).pow(2) / u128::from(target)) as u64,
                slot,
            )
        };
        if let Err(e) = result {
            o.totals.blocked = format!("stress:{e}");
            break;
        }
        let mut survivors = Vec::new();
        for mut position in positions {
            for _ in 0..100 {
                if position.debt_shares == 0 {
                    break;
                }
                let h = health(&market, &position, fee);
                let mm = u64::from(
                    position
                        .margin_terms
                        .maintenance_bps(position.collateral_amount)
                        .unwrap(),
                );
                if h > mm {
                    break;
                }
                let index = identities.iter().position(|owner| *owner == position.owner).unwrap();
                if seen_eligible.insert(position.owner) {
                    report_headroom(&market, &position, scenario, step, index, "ordinary_eligible");
                }
                if h * 10_000 <= mm * scenario.critical && seen_emergency.insert(position.owner) {
                    report_headroom(&market, &position, scenario, step, index, "emergency_permitted");
                }
                let ordinary = purchase(&market, &position, slot, fee);
                let fallback = emergency(&market, &position, slot, scenario);
                let selected = if scenario.emergency_first {
                    fallback.or(ordinary)
                } else {
                    ordinary.or(fallback)
                };
                let Some(fill) = selected else {
                    break;
                };
                if fill.kind == 3 {
                    report_headroom(&market, &position, scenario, step, index, "emergency_selected");
                }
                let reference = reference_credit(&market, asset, fee.unwind_credit(fill.sold).unwrap());
                match settle_netted(&market, &position, fill, slot, fee) {
                    Ok((after, next, paid)) => {
                        if fill.kind == 3 {
                            if o.first_emergency_step == 0 {
                                o.first_emergency_step = step + 1;
                                o.first_emergency_health = h;
                                o.first_emergency_mm = mm;
                            }
                            o.emergency_with_partial += u64::from(ordinary.is_some_and(|f| f.kind == 0));
                        }
                        o.totals.partial += u64::from(fill.kind == 0);
                        o.totals.flash += u64::from(fill.kind == 1);
                        o.totals.emergency += u64::from(fill.kind == 3);
                        o.totals.repaid += paid.repaid;
                        o.totals.insurance += paid.insurance;
                        o.totals.loss += paid.loss;
                        o.totals.owner += paid.owner;
                        o.totals.reward += paid.reward;
                        o.totals.contribution += paid.contribution;
                        o.buyer_profit += fee.unwind_credit(fill.output - fill.payment - fill.reward).unwrap();
                        o.sold_reference += reference;
                        o.transfer_cost += fill.output
                            - paid.repaid
                            - paid.owner
                            - paid.reward
                            - paid.contribution
                            - fee.unwind_credit(fill.output - fill.payment - fill.reward).unwrap();
                        market = after;
                        position = next;
                    }
                    Err(e) => {
                        let name = error_name(e);
                        if name == "InsufficientLiquidity" {
                            o.cash_rejections += 1;
                            break;
                        }
                        panic!("unexpected settlement {scenario:?}: {name}");
                    }
                }
            }
            if position.debt_shares > 0 {
                survivors.push(position);
            }
        }
        positions = survivors;
        let exposure: u64 = positions.iter().map(|p| p.collateral_amount).sum();
        assert_eq!(market.leverage_collateral_exposure(asset), exposure);
        market.assert_market_invariants().unwrap();
    }
    for p in positions {
        let debt = p.debt_amount(&market.debt).unwrap();
        o.totals.remaining += debt;
        let value = reference_credit(&market, asset, fee.unwind_credit(p.collateral_amount).unwrap());
        o.survivor_equity += value.saturating_sub(debt);
        let h = health(&market, &p, fee);
        let mm = u64::from(p.margin_terms.maintenance_bps(p.collateral_amount).unwrap());
        if h <= mm {
            o.totals.eligible += 1;
            o.totals.eligible_debt += debt;
            o.largest_eligible_debt = o.largest_eligible_debt.max(debt);
            if h * 10_000 > mm * scenario.critical {
                o.eligible_above_emergency += 1;
                o.eligible_above_emergency_debt += debt;
            }
            match quote_sale(&market, &p, p.collateral_amount, last_slot, fee) {
                Ok((_, prepared)) => {
                    let output = prepared.quote.amount_out;
                    let reward = (u128::from(output) * u128::from(mm - h) * 100 / u128::from(mm) / 10_000) as u64;
                    o.residual_shortfall += debt.saturating_sub(output - reward);
                }
                Err(_) => o.residual_quote_failures += 1,
            }
        }
    }
    assert_eq!(
        o.totals.original_debt,
        o.totals.repaid + o.totals.insurance + o.totals.loss + o.totals.remaining
    );
    o
}

#[test]
fn native_emergency_threshold_comparison() {
    let full = std::env::var_os("DUSK_EMERGENCY_THRESHOLD_SWEEP").is_some();
    println!("EMERGENCY_HEADROOM,critical_bps,amplification,controller,hlp,debt_side,pieces,path,withdrawal_bps,transfer_fee_bps,emergency_first,position_index,stage,step,health_bps,mm_bps,debt,reference_value,output,reward,contribution,net_recovery,quote_ok");
    println!("EMERGENCY_THRESHOLD,critical_bps,amplification,controller,hlp,debt_side,pieces,path,withdrawal_bps,transfer_fee_bps,emergency_first,opened,original_debt,partial,flash,emergency,repaid,insurance,loss,owner,reward,contribution,buyer_profit,debt_transfer_cost,sold_reference,survivor_equity,remaining,eligible,eligible_debt,first_emergency_step,first_emergency_health,first_emergency_mm,emergency_with_partial,cash_rejections,eligible_above_emergency,eligible_above_emergency_debt,largest_eligible_debt,residual_shortfall,residual_quote_failures,blocked");
    let mut count = 0;
    for critical in [7500, 6667, 5000] {
        for amp in [1, 4, 10] {
            for controller in [false, true] {
                for hlp in [false, true] {
                    for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
                        for pieces in [1, 10] {
                            for path in [0, 1, 2, 3] {
                                // Stress transfers and withdrawals together in
                                // a separate matched slice, never mix admissions.
                                for (withdrawal, fee_bps) in [(0, 0), (2500, 100)] {
                                    for emergency_first in [false, true] {
                                        if !full
                                            && (amp != 4
                                                || !controller
                                                || !hlp
                                                || debt_asset != MarketAsset::Quote
                                                || pieces != 10
                                                || path != 0
                                                || withdrawal != 0
                                                || emergency_first)
                                        {
                                            continue;
                                        }
                                        let scenario = Scenario {
                                            case: Case {
                                                amp,
                                                controller,
                                                hlp,
                                                debt_asset,
                                                pieces,
                                                path,
                                                cushion: 0,
                                                withdrawal,
                                            },
                                            fee_bps,
                                            emergency_first,
                                            critical,
                                        };
                                        let o = run(scenario);
                                        let t = o.totals;
                                        println!("EMERGENCY_THRESHOLD,{critical},{amp},{controller},{hlp},{},{pieces},{path},{withdrawal},{fee_bps},{emergency_first},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",debt_asset.code(),t.opened,t.original_debt,t.partial,t.flash,t.emergency,t.repaid,t.insurance,t.loss,t.owner,t.reward,t.contribution,o.buyer_profit,o.transfer_cost,o.sold_reference,o.survivor_equity,t.remaining,t.eligible,t.eligible_debt,o.first_emergency_step,o.first_emergency_health,o.first_emergency_mm,o.emergency_with_partial,o.cash_rejections,o.eligible_above_emergency,o.eligible_above_emergency_debt,o.largest_eligible_debt,o.residual_shortfall,o.residual_quote_failures,t.blocked);
                                        count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(count, if full { 2304 } else { 3 });
}

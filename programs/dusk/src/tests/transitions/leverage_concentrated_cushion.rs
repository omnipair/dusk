// Native economic experiment, not new runtime policy. Quotes, trades, fees,
// recentering, EMA, repayment, write-off and hLP reconstruction use Dusk's
// integer engine. Selected-policy permission/reward adapters are test-only.
// Accrual is frozen for the sweep; separate existing tests cover funding and
// unpaid-interest mechanics. Token CPIs/flash sessions are not exercised here.
use super::stored_depth_schedules::stored_cash_depth;

mod settlement {
    use super::*;
    include!("leverage_cushion_settlement.rs");
}
use settlement::*;

#[derive(Clone, Copy, Debug)]
struct Case {
    amp: u64,
    controller: bool,
    hlp: bool,
    debt_asset: MarketAsset,
    pieces: u64,
    path: u8,
    cushion: u64,
    withdrawal: u64,
}

#[derive(Default)]
struct Totals {
    opened: u64,
    original_debt: u64,
    partial: u64,
    flash: u64,
    solvent: u64,
    emergency: u64,
    repaid: u64,
    insurance: u64,
    loss: u64,
    owner: u64,
    reward: u64,
    contribution: u64,
    remaining: u64,
    eligible: u64,
    tail_observations: u64,
    recentered: u64,
    deferred: u64,
    quote_failures: u64,
    last_quote_error: String,
    failed_fills: u64,
    blocked: String,
}

fn no_accrual(market: &mut Market, slot: u64) {
    market.debt.base_last_accrual_slot = slot;
    market.debt.quote_last_accrual_slot = slot;
}

fn initialize(case: Case) -> Market {
    // Fixed total wallet deposits across hLP variants: $100k. With hLP, $80k
    // starts as yLP and $10k per asset enters through native single-sided entry.
    let mut market = calibration_market(if case.hlp { 80_000 } else { 100_000 }, case.amp);
    market.config.ema_half_life_ms = 60_000;
    market.config.directional_ema_half_life_ms = 60_000;
    market.config.curve_depth_ema_half_life_ms = 60_000;
    market.config.amm.center_ema_half_life_ms = 60_000;
    if case.controller {
        market.config.amm.adjustment_threshold_nad = NAD / 100;
        market.config.amm.adjustment_step_nad = NAD / 1_000;
        market.config.amm.min_adjustment_interval_slots = 1;
        market.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
    }
    market.config.target_hlp_leverage_bps = 20_000;
    market.config.settlement_divergence_bps = 500;
    if case.hlp {
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            market
                .deposit_single_sided(asset, 10_000 * CALIBRATION_UNIT, 1)
                .unwrap();
        }
        market.finalize_amm_transition_and_observe_risk(1).unwrap();
    }
    market.config.amm.validate().unwrap();
    market.insurance.per_event_draw_bps = crate::constants::MAX_INSURANCE_DRAW_PER_EVENT_BPS;
    market.insurance.per_day_draw_bps = crate::constants::MAX_INSURANCE_DRAW_PER_DAY_BPS;
    market
        .insurance
        .credit(case.debt_asset, 1_000 * CALIBRATION_UNIT, 1)
        .unwrap();
    market.assert_market_invariants().unwrap();
    market
}

fn schedule(depth: u128) -> LeverageMarginSchedule {
    let mut terms = calibration_schedule();
    terms.maintenance_boundaries = [depth * 5 / 100, depth * 15 / 100];
    terms.maintenance_rates_bps = [700, 1_200, 1_700];
    terms
}

fn error_name(error: anchor_lang::error::Error) -> String {
    match error {
        anchor_lang::error::Error::AnchorError(e) => {
            if e.error_name == "BrokenInvariant" {
                format!("{}@{:?}", e.error_name, e.error_origin).replace(',', ";")
            } else {
                e.error_name
            }
        }
        other => format!("{other:?}").replace(',', ";"),
    }
}

fn price(market: &Market, collateral: MarketAsset) -> u64 {
    let p = market.current_concentrated_spot_price_nad().unwrap().unwrap();
    match collateral {
        MarketAsset::Base => p,
        MarketAsset::Quote => (u128::from(NAD).pow(2) / u128::from(p)) as u64,
    }
}

// Match a requested debt-per-collateral marginal price through native trades.
// The binary search uses actual post-transition prices, including hLP and fees.
// Failure to reach a target is surfaced; no reserves or price are overwritten.
fn stress_to(market: &mut Market, asset: MarketAsset, target: u64, slot: u64) -> std::result::Result<(), String> {
    if price(market, asset) <= target {
        market.observe_current_risk(slot).map_err(error_name)?;
        return Ok(());
    }
    let mut low = 1;
    let mut high = market.side(asset).reserves.cash_reserve.max(1);
    let mut best = None;
    let mut failure = "input_search_cap".to_owned();
    for _ in 0..32 {
        let mid = low + (high - low) / 2;
        match sale(market, asset, mid, slot) {
            Ok((trial, _)) => {
                if price(&trial, asset) <= target {
                    best = Some(trial);
                    high = mid;
                } else {
                    low = mid + 1;
                }
            }
            Err(e) => {
                let name = error_name(e);
                if name == "InsufficientOutputAmount" || name == "AmountZero" {
                    low = mid + 1;
                } else {
                    failure = name;
                    high = mid;
                }
            }
        }
        if low >= high {
            break;
        }
    }
    *market = best.ok_or_else(|| format!("target_unreached:{failure}"))?;
    Ok(())
}

fn one_case(case: Case) -> Totals {
    let mut market = initialize(case);
    let asset = case.debt_asset.opposite();
    let mut positions = Vec::new();
    let mut totals = Totals::default();
    for _ in 0..case.pieces {
        let mut trial = market.clone();
        no_accrual(&mut trial, 2);
        let position = match calibration_entry(
            &mut trial,
            case.debt_asset,
            15_000 * CALIBRATION_UNIT / case.pieces,
            5_000,
            2,
        ) {
            Ok(p) => p,
            Err(e) => {
                totals.blocked = format!("entry:{}", error_name(e));
                break;
            }
        };
        let depth = stored_cash_depth(&trial, asset);
        let terms = schedule(depth);
        let exposure = positions
            .iter()
            .map(|(p, _): &(LeveragePosition, LeverageMarginSchedule)| p.collateral_amount as u128)
            .sum::<u128>()
            + position.collateral_amount as u128;
        let required = terms
            .initial_bps(position.collateral_amount.into(), exposure, depth)
            .unwrap();
        let value = reference_credit(&trial, asset, position.collateral_amount);
        let debt = position.debt_amount(&trial.debt).unwrap();
        let exit = match sale(&trial, asset, position.collateral_amount, 2) {
            Ok((_, output)) => output,
            Err(e) => {
                totals.blocked = format!("entry_exit:{}", error_name(e));
                break;
            }
        };
        if equity_bps(value, debt).unwrap().min(equity_bps(exit, debt).unwrap()) < u128::from(required) {
            totals.blocked = "entry_margin".into();
            break;
        }
        totals.opened += 1;
        totals.original_debt += debt;
        positions.push((position, terms));
        market = trial;
    }
    // Restore the initial external mark using an ACTUAL native countertrade,
    // including its fees, inventory movements and hLP changes. Never overwrite
    // reserves or concentrate a CPMM by multiplying its available inventory.
    if let Err(e) = stress_to(&mut market, asset, NAD, 3) {
        totals.blocked = format!("entry_reprice:{e}");
    }
    let opening = NAD;
    let mut target = opening;
    let mut slot = 2;
    for step in 1..=180 {
        if positions.is_empty() {
            break;
        }
        slot = 2 + step * if case.path == 0 { 150 } else { 25 };
        no_accrual(&mut market, slot);
        if case.withdrawal > 0 && step == 2 {
            let mut trial = market.clone();
            let shares = trial.base_side.shares.ylp_supply * case.withdrawal / 10_000;
            if let Err(e) = trial
                .remove_liquidity(shares)
                .and_then(|_| trial.finalize_amm_transition_and_observe_risk(slot))
            {
                totals.blocked = format!("withdrawal:{}", error_name(e));
                break;
            }
            market = trial;
        }
        target = match case.path {
            0 => target * 9_950 / 10_000,
            1 => target * 9_700 / 10_000,
            _ => opening * 55 / 100,
        };
        let center_before = market.current_curve_center_price_nad().unwrap();
        if let Err(e) = stress_to(&mut market, asset, target, slot) {
            totals.blocked = format!("stress:{e}");
            break;
        }
        totals.recentered += u64::from(market.current_curve_center_price_nad().unwrap() != center_before);
        totals.deferred += u64::from(market.amm.deferred_controller_target.kind != 0);
        let branch = market
            .amm_preview_metrics(market.risk.cached_spot_base_price_nad)
            .unwrap()
            .concentrated_curve_branch;
        totals.tail_observations += u64::from(branch == 0 || branch == 4);
        let mut survivors = Vec::new();
        for (mut position, terms) in positions {
            // Multiple racing fills may execute at the same observation.
            for _ in 0..100 {
                if position.debt_shares == 0 {
                    break;
                }
                let debt = position.debt_amount(&market.debt).unwrap();
                let value = reference_credit(&market, asset, position.collateral_amount);
                let health = equity_bps(value, debt).unwrap() as u64;
                let mm = terms
                    .effective_maintenance_bps(position.collateral_amount.into())
                    .unwrap() as u64;
                if health > mm {
                    break;
                }
                let Some(fill) = choose_fill(&market, &position, &terms, case.cushion, slot) else {
                    if let Err(e) = sale(&market, asset, position.collateral_amount, slot) {
                        totals.quote_failures += 1;
                        totals.last_quote_error = error_name(e);
                    }
                    break;
                };
                match settle(&market, &position, fill, slot) {
                    Ok((after, next, paid)) => {
                        market = after;
                        position = next;
                        totals.partial += u64::from(fill.kind == 0);
                        totals.flash += u64::from(fill.kind == 1);
                        totals.solvent += u64::from(fill.kind == 2);
                        totals.emergency += u64::from(fill.kind == 3);
                        totals.repaid += paid.repaid;
                        totals.insurance += paid.insurance;
                        totals.loss += paid.loss;
                        totals.owner += paid.owner;
                        totals.reward += paid.reward;
                        totals.contribution += paid.contribution;
                    }
                    Err(e) => {
                        totals.failed_fills += 1;
                        totals.blocked = format!("settlement:{}", error_name(e));
                        break;
                    }
                }
            }
            if position.debt_shares > 0 {
                survivors.push((position, terms));
            }
        }
        positions = survivors;
        market.assert_market_invariants().unwrap();
        assert_eq!(market.base_hlp_vault.residual_exposure, 0);
        assert_eq!(market.quote_hlp_vault.residual_exposure, 0);
    }
    for (position, terms) in positions {
        let debt = position.debt_amount(&market.debt).unwrap();
        totals.remaining += debt;
        let health = equity_bps(reference_credit(&market, asset, position.collateral_amount), debt).unwrap();
        let mm = terms
            .effective_maintenance_bps(position.collateral_amount.into())
            .unwrap();
        totals.eligible += u64::from(health <= u128::from(mm));
    }
    assert_eq!(
        totals.original_debt,
        totals.repaid + totals.insurance + totals.loss + totals.remaining
    );
    assert!(slot >= 2);
    totals
}

#[test]
fn native_concentrated_cushion_report() {
    println!("NATIVE_CUSHION,amplification,controller,hlp,debt_side,pieces,path,cushion_bps,withdrawal_bps,opened,original_debt,partial,flash,solvent,emergency,repaid,insurance,loss,owner,reward,contribution,remaining,eligible,tail_observations,recentered,deferred,quote_failures,last_quote_error,failed_fills,blocked");
    let mut successes = [0; 3];
    let mut cases = 0;
    let full_sweep = std::env::var_os("DUSK_NATIVE_CUSHION_SWEEP").is_some();
    let selected = std::env::var("DUSK_NATIVE_CUSHION_CASE").ok();
    for amp in [1, 4, 10] {
        for controller in [false, true] {
            for hlp in [false, true] {
                for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
                    for pieces in [1, 10] {
                        for path in [0, 1, 2] {
                            for cushion in [0, 200] {
                                for withdrawal in [0, 2_500] {
                                    let key = format!(
                                        "{amp},{controller},{hlp},{},{pieces},{path},{cushion},{withdrawal}",
                                        debt_asset.code()
                                    );
                                    if let Some(ref selection) = selected {
                                        if *selection != key {
                                            continue;
                                        }
                                    } else if !full_sweep
                                        && (amp == 10
                                            || pieces != 10
                                            || path != 0
                                            || cushion != 200
                                            || withdrawal != 0
                                            || debt_asset != MarketAsset::Quote)
                                    {
                                        continue;
                                    }
                                    let case = Case {
                                        amp,
                                        controller,
                                        hlp,
                                        debt_asset,
                                        pieces,
                                        path,
                                        cushion,
                                        withdrawal,
                                    };
                                    let t = one_case(case);
                                    successes[0] += t.solvent;
                                    successes[1] += u64::from(t.loss > 0);
                                    successes[2] += u64::from(hlp && t.repaid > 0);
                                    println!("NATIVE_CUSHION,{amp},{controller},{hlp},{},{pieces},{path},{cushion},{withdrawal},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}", debt_asset.code(), t.opened,t.original_debt,t.partial,t.flash,t.solvent,t.emergency,t.repaid,t.insurance,t.loss,t.owner,t.reward,t.contribution,t.remaining,t.eligible,t.tail_observations,t.recentered,t.deferred,t.quote_failures,t.last_quote_error,t.failed_fills,t.blocked);
                                    cases += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        cases,
        if selected.is_some() {
            1
        } else if full_sweep {
            576
        } else {
            8
        }
    );
    if full_sweep && selected.is_none() {
        assert!(
            successes.iter().all(|n| *n > 0),
            "missing native coverage: {successes:?}"
        );
    }
}

#[test]
fn native_cushion_quote_is_not_a_spendable_cash_guarantee() {
    let mut market = calibration_market(100_000, 4);
    // Seed an explicitly cash-constrained state, not an admitted leveraged
    // entry. The borrow receivable remains in curve backing while cash leaves.
    let position = seeded_position(
        &mut market,
        MarketAsset::Quote,
        49_500 * CALIBRATION_UNIT,
        1_000 * CALIBRATION_UNIT,
    );
    let quote = market
        .quote_leverage_swap(MarketAsset::Base, position.collateral_amount, 1)
        .unwrap();
    assert!(quote.amount_out > market.quote_side.reserves.cash_reserve);
    let before = market.clone();
    let result = sale(&market, MarketAsset::Base, position.collateral_amount, 1);
    let Err(error) = result else {
        panic!("cash-constrained sale unexpectedly succeeded");
    };
    assert_eq!(error_name(error), "InsufficientLiquidity");
    // Failed hypothetical execution never commits its scratch state.
    assert_eq!(
        market.base_side.reserves.cash_reserve,
        before.base_side.reserves.cash_reserve
    );
    assert_eq!(
        market.quote_side.reserves.cash_reserve,
        before.quote_side.reserves.cash_reserve
    );
    assert_eq!(market.curve_revision, before.curve_revision);
}

#[test]
fn native_cushion_solvent_and_loss_settlements_preserve_active_hlp() {
    for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
        for insolvent in [false, true] {
            let case = Case {
                amp: 4,
                controller: true,
                hlp: true,
                debt_asset,
                pieces: 1,
                path: 0,
                cushion: 200,
                withdrawal: 0,
            };
            let mut market = initialize(case);
            let principal = if insolvent { 1_500 } else { 950 } * CALIBRATION_UNIT;
            let position = seeded_position(&mut market, debt_asset, principal, 1_000 * CALIBRATION_UNIT);
            let (_, output) = sale(&market, debt_asset.opposite(), position.collateral_amount, 2).unwrap();
            let reward = output / 100;
            let payment = output - reward;
            let contribution = payment.saturating_sub(principal).min(principal * 20 / 10_000);
            let fill = Fill {
                kind: if insolvent { 3 } else { 2 },
                sold: position.collateral_amount,
                output,
                payment,
                contribution,
                reward,
            };
            // Settlement/accounting anchor only; the seeded solvent case is
            // deliberately not a claim that the permission window was met.
            let (after, next, paid) = settle(&market, &position, fill, 2).unwrap();
            assert_eq!(next.debt_shares, 0);
            assert_eq!(next.collateral_amount, 0);
            assert_eq!(after.base_hlp_vault.residual_exposure, 0);
            assert_eq!(after.quote_hlp_vault.residual_exposure, 0);
            assert_eq!(principal, paid.repaid + paid.insurance + paid.loss);
            if insolvent {
                assert!(paid.insurance > 0);
                assert!(paid.loss > 0);
            } else {
                assert_eq!(paid.insurance, 0);
                assert_eq!(paid.loss, 0);
                assert!(paid.owner > 0);
            }
        }
    }
}

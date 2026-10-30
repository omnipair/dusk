// Audit-only counterfactual: no production implementation or policy selection.
use crate::transitions::liquidity::{current_hlp_signed_navs_with_prices, hlp_curve_prices_from_base_price_nad};

fn cmp_trade(m: &Market, a: MarketAsset, gross: u64, f: LeverageCollateralFee) -> Result<(Market, u64)> {
    let mut t = m.clone();
    let mut s = SwapRequest {
        current_slot: 2,
        current_unix_timestamp: 120,
        asset_in: a,
        reserve_credit: f.unwind_credit(gross)?,
        protocol_fee_bps: 2000,
    }
    .prepare(&mut t)?;
    let out = f.unwind_credit(s.quote.amount_out)?;
    s.finalize_state(&mut t, 2, 2000, ProtocolAuctionSplit::default())?;
    t.assert_market_invariants()?;
    Ok((t, out))
}
fn cmp_value(n: u64, a: MarketAsset, debt: MarketAsset, mark: u64) -> f64 {
    n as f64 / 1000.0 * if a == debt { 1.0 } else { mark as f64 / NAD as f64 }
}
fn cmp_hlp_fees(v: &HlpVault, asset: MarketAsset) -> f64 {
    let (swap, interest, unalloc) = match asset {
        MarketAsset::Base => (
            v.base_swap_fee_growth_index_q64,
            v.base_interest_growth_index_q64,
            v.unallocated_base_swap_fee_amount + v.unallocated_base_interest_amount,
        ),
        MarketAsset::Quote => (
            v.quote_swap_fee_growth_index_q64,
            v.quote_interest_growth_index_q64,
            v.unallocated_quote_swap_fee_amount + v.unallocated_quote_interest_amount,
        ),
    };
    (v.hlp_supply as f64 * (swap + interest) as f64 / ((1u128 << 64) as f64) + unalloc as f64) / 1000.0
}
// Mark principal with one common external price, not the manipulated spot.
// Fees are added once, assigned by the native hLP yield indexes.
fn cmp_nav(m: &Market, debt: MarketAsset, mark: u64) -> Result<[f64; 7]> {
    let mut t = m.clone();
    t.checkpoint_hlp_yield_from_ylp(MarketAsset::Base)?;
    t.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote)?;
    let p = if debt == MarketAsset::Quote {
        u128::from(mark)
    } else {
        u128::from(NAD).pow(2) / u128::from(mark)
    };
    let (base, quote) = current_hlp_signed_navs_with_prices(&t, hlp_curve_prices_from_base_price_nad(p)?)?;
    let bv = if debt == MarketAsset::Base {
        1.0
    } else {
        mark as f64 / NAD as f64
    };
    let qv = if debt == MarketAsset::Quote {
        1.0
    } else {
        mark as f64 / NAD as f64
    };
    let hbf = cmp_hlp_fees(&t.base_hlp_vault, MarketAsset::Base) * bv
        + cmp_hlp_fees(&t.base_hlp_vault, MarketAsset::Quote) * qv;
    let hqf = cmp_hlp_fees(&t.quote_hlp_vault, MarketAsset::Base) * bv
        + cmp_hlp_fees(&t.quote_hlp_vault, MarketAsset::Quote) * qv;
    let hs = t.base_hlp_vault.ylp_shares + t.quote_hlp_vault.ylp_shares;
    let fraction = (t.base_side.shares.ylp_supply - hs) as f64 / t.base_side.shares.ylp_supply as f64;
    let principal = cmp_value(t.curve_reserve(MarketAsset::Base)?, MarketAsset::Base, debt, mark)
        + cmp_value(t.curve_reserve(MarketAsset::Quote)?, MarketAsset::Quote, debt, mark);
    let mut lpfees = 0.0;
    let mut protocol = 0.0;
    let mut retained = 0.0;
    let mut backing = 0.0;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let s = t.side(asset);
        let f = &s.fees;
        lpfees += cmp_value(
            f.swap_fee_liability
                + f.interest_liability
                + f.unallocated_swap_fee_liability
                + f.unallocated_interest_liability,
            asset,
            debt,
            mark,
        );
        protocol += cmp_value(
            f.swap_protocol_fee_liability
                + f.interest_protocol_fee_liability
                + f.swap_buyback_fee_liability
                + f.interest_buyback_fee_liability,
            asset,
            debt,
            mark,
        );
        retained += cmp_value(s.reserves.protected_recenter_reserve, asset, debt, mark);
        backing += cmp_value(
            s.reserves.base_hlp_backing_inventory + s.reserves.quote_hlp_backing_inventory,
            asset,
            debt,
            mark,
        );
    }
    Ok([
        principal * fraction + lpfees - hbf - hqf,
        base as f64 / NAD as f64 * bv + hbf,
        quote as f64 / NAD as f64 * qv + hqf,
        protocol,
        retained,
        cmp_value(t.insurance.available(debt), debt, debt, mark),
        backing,
    ])
}
#[derive(Clone)]
struct CmpVenue {
    coll: u128,
    debt: u128,
}
impl CmpVenue {
    fn new(mark: u64) -> Self {
        Self {
            coll: 100_000_000u128 * u128::from(NAD) / u128::from(mark),
            debt: 100_000_000,
        }
    }
    fn buy_exact(&mut self, wallet_coll: u64, f: LeverageCollateralFee) -> Result<u64> {
        let out = u128::from(f.effective_gross_for_credit(wallet_coll)?);
        require!(out < self.coll, ErrorCode::InsufficientLiquidity);
        let credited = (self.debt * out * 10_000).div_ceil((self.coll - out) * 9_970);
        let gross = f.effective_gross_for_credit(credited as u64)?;
        self.debt += credited;
        self.coll -= out;
        Ok(gross)
    }
    fn sell(&mut self, gross: u64, f: LeverageCollateralFee) -> Result<u64> {
        let credited = u128::from(f.unwind_credit(gross)?);
        let out = self.debt * credited * 9_970 / (self.coll * 10_000 + credited * 9_970);
        self.coll += credited;
        self.debt -= out;
        f.unwind_credit(out as u64)
    }
}
fn cmp_arb(m: &Market, v: &CmpVenue, debt: MarketAsset, f: LeverageCollateralFee) -> Result<(Market, CmpVenue, f64)> {
    let mut best = (m.clone(), v.clone(), 0.0);
    // Two directions, includes no-trade; finite sampled best, not global optimum.
    for amount in (1..=100)
        .map(|i| i * 500 * CALIBRATION_UNIT)
        .chain([1, 10, 100, 1000, 10000, 100000])
    {
        if let Ok((t, out)) = cmp_trade(m, debt, amount, f) {
            let mut vv = v.clone();
            let profit = (vv.sell(out, f)? as f64 - amount as f64) / 1000.0;
            if profit > best.2 {
                best = (t, vv, profit);
            }
        }
        let mut vv = v.clone();
        if let (Ok(cost), Ok((t, out))) = (vv.buy_exact(amount, f), cmp_trade(m, debt.opposite(), amount, f)) {
            let profit = (out as f64 - cost as f64) / 1000.0;
            if profit > best.2 {
                best = (t, vv, profit);
            }
        }
    }
    Ok(best)
}
fn cmp_v1_inventory(
    m: &Market,
    p: &LeveragePosition,
    debt: MarketAsset,
    f: LeverageCollateralFee,
    bps: u64,
) -> Result<(Market, LeveragePosition, u64, u64, bool)> {
    let health = m.flash_liquidation_health(&Ref::Leverage(p), debt, f)?;
    require!(health.eligible()?, ErrorCode::PositionNotLiquidatable);
    // Preserve V2 linear-EMA eligibility/full-insolvency, not V1 impact health.
    let full = health.debt >= health.collateral_value;
    let a = m.liquidation_debt_allocation(
        &Ref::Leverage(p),
        debt,
        if full {
            p.debt_shares
        } else {
            (p.debt_shares * u128::from(bps)).div_ceil(10000)
        },
    )?;
    require_eq!(a.principal, a.debt, ErrorCode::InvalidArgument); // experiment has no accrued interest
    let collateral = debt.opposite();
    let ema = if collateral == MarketAsset::Base {
        m.risk.base_price_ema_nad
    } else {
        m.risk.quote_price_ema_nad
    };
    let x = u128::from(m.curve_reserve(collateral)?);
    let y = u128::from(m.curve_reserve(debt)?);
    // V1's constant-product same-k virtual reserves; this does not pretend
    // V1 used Dusk's concentration-aware curve.
    let xe = (x * y * u128::from(NAD) / u128::from(ema)).isqrt();
    let ye = (x * y * u128::from(ema) / u128::from(NAD)).isqrt();
    require!(u128::from(a.debt) < ye, ErrorCode::InsufficientLiquidity);
    let base = (u128::from(a.debt) * xe).div_ceil(ye - u128::from(a.debt)) as u64;
    let seized = if full {
        p.collateral_amount
    } else {
        (base * 10300).div_ceil(10000).min(p.collateral_amount)
    };
    require!(
        full || seized < p.collateral_amount,
        ErrorCode::LiquidationRepayTooLarge
    );
    let reward = (base * 50 / 10000).min(seized);
    let credit = f.unwind_credit(seized - reward)?;
    let mut t = m.clone();
    let mut q = p.clone();
    let policy = SwapCashPolicy::SettleLiquidation {
        debt_asset: debt,
        isolated: true,
        full,
        shares: a.shares,
        principal_removed: a.principal,
        debt_reduced: a.debt,
        repayment: a.debt,
        insurance_credit: 0,
    };
    // Hypothetical inventory exchange: cancel receivable, receive collateral.
    // Native ledger kernel ensures no fictional debt-token cash is introduced.
    t.apply_leverage_lifecycle_transition(policy, collateral, credit, a.debt, a.debt)?;
    t.apply_liquidation_position(&mut Mut::Leverage(&mut q), debt, a, seized, full)?;
    t.finalize_amm_socialized_loss_and_observe_risk(2)?;
    t.assert_market_invariants()?;
    let after = t.flash_liquidation_health(&Ref::Leverage(&q), debt, f)?;
    let improves = full || health.improves(after)?;
    require!(improves, ErrorCode::LiquidationRepayTooLarge);
    Ok((t, q, f.unwind_credit(reward)?, a.debt, improves))
}
#[test]
fn audit_ema_inventory_economic_comparison() {
    println!("EMA_CMP,amp,hlp,side,size,equity_bps,mark,tokenfee,withdrawal,front,method,status,repaid,remaining_debt,remaining_coll,reward_value,front_cost,front_output,backrun_profit,attack_profit,ylp,base_hlp,quote_hlp,protocol,protected,insurance,eligible_after,emergency_before,improves,residual_mark_gap,residual_route_gap,insurance_draw,writeoff,liq_fees,liq_full,backing,mm_bps");
    let mut constructed = 0;
    let mut skipped = 0;
    let mut rows = 0;
    for amp in [1, 4, 10] {
        for hlp in [false, true] {
            for debt in [MarketAsset::Base, MarketAsset::Quote] {
                for size in [1000u64, 15000] {
                    for eq in [600u64, 400] {
                        for mark in [NAD, NAD * 80 / 100] {
                            for tbps in [0u16, 50] {
                                for withdrawal in [0u64, 2500] {
                                    let case = Case {
                                        amp,
                                        controller: true,
                                        hlp,
                                        debt_asset: debt,
                                        pieces: 1,
                                        path: 0,
                                        cushion: 0,
                                        withdrawal,
                                    };
                                    let mut m = initialize(case);
                                    let fee = audit_fee(tbps);
                                    // Accounting-consistent distressed snapshot; not an admission/price-path proof.
                                    let mut p = seeded_position(
                                        &mut m,
                                        debt,
                                        size * CALIBRATION_UNIT * (10000 - eq) / 10000,
                                        size * CALIBRATION_UNIT,
                                    );
                                    p.margin_terms = crate::state::LeverageMarginTerms::at_entry(
                                        m.leverage_reference_depth(debt.opposite()).unwrap(),
                                    )
                                    .unwrap();
                                    no_accrual(&mut m, 2);
                                    if withdrawal > 0 {
                                        let ordinary = m.base_side.shares.ylp_supply
                                            - m.base_hlp_vault.ylp_shares
                                            - m.quote_hlp_vault.ylp_shares;
                                        if let Err(e) = m
                                            .remove_liquidity(ordinary * withdrawal / 10000)
                                            .and_then(|_| m.finalize_amm_transition_and_observe_risk(2))
                                        {
                                            skipped += 1;
                                            println!(
                                                "EMA_SKIP,{case:?},{size},{eq},{mark},{tbps},withdraw:{}",
                                                error_name(e)
                                            );
                                            continue;
                                        }
                                    }
                                    if let Err(e) = stress_to(&mut m, debt.opposite(), mark, 2) {
                                        skipped += 1;
                                        println!("EMA_SKIP,{case:?},{size},{eq},{mark},{tbps},stress:{e}");
                                        continue;
                                    }
                                    let health = m.flash_liquidation_health(&Ref::Leverage(&p), debt, fee).unwrap();
                                    assert!(health.eligible().unwrap());
                                    constructed += 1;
                                    for front in [0u64, 1000, 5000] {
                                        let (front_m, front_out) = if front == 0 {
                                            (m.clone(), 0)
                                        } else {
                                            match cmp_trade(&m, debt.opposite(), front * CALIBRATION_UNIT, fee) {
                                                Ok(x) => x,
                                                Err(e) => {
                                                    println!(
                                                        "EMA_FRONT_SKIP,{case:?},{size},{eq},{mark},{tbps},{front},{}",
                                                        error_name(e)
                                                    );
                                                    continue;
                                                }
                                            }
                                        };
                                        let mut venue = CmpVenue::new(mark);
                                        let front_cost =
                                            venue.buy_exact(front * CALIBRATION_UNIT, fee).unwrap() as f64 / 1000.0;
                                        for method in ["control", "v2", "v1_half", "v1_search"] {
                                            let mut t = front_m.clone();
                                            let mut next = p.clone();
                                            let mut reward = 0.0;
                                            let mut repaid = 0;
                                            let mut improves = false;
                                            let mut v = venue.clone();
                                            let mut totals = AuditTotals::default();
                                            let status = if method == "control" {
                                                "no_liquidation".to_owned()
                                            } else if method.starts_with("v1_") {
                                                let sizes: &[u64] = if method == "v1_half" {
                                                    &[5000]
                                                } else {
                                                    &[5000, 2500, 1000, 500, 100, 10, 1]
                                                };
                                                let mut status = "no_fill".to_owned();
                                                let mut best = f64::NEG_INFINITY;
                                                for &bps in sizes {
                                                    match cmp_v1_inventory(&front_m, &p, debt, fee, bps) {
                                                        Ok((mm, pp, r, d, good)) => {
                                                            let mut vv = venue.clone();
                                                            let rr = vv.sell(r, fee).unwrap() as f64 / 1000.0;
                                                            let (_, _, arb) = cmp_arb(&mm, &vv, debt, fee).unwrap();
                                                            if rr + arb > best {
                                                                best = rr + arb;
                                                                t = mm;
                                                                next = pp;
                                                                reward = rr;
                                                                repaid = d;
                                                                improves = good;
                                                                v = vv;
                                                                status = "ok".to_owned();
                                                            }
                                                        }
                                                        Err(e) => {
                                                            if !best.is_finite() {
                                                                status = error_name(e)
                                                            }
                                                        }
                                                    }
                                                }
                                                status
                                            } else {
                                                let mut best = f64::NEG_INFINITY;
                                                let mut status = "no_fill".to_owned();
                                                for flash in [true, false] {
                                                    for full in [false, true] {
                                                        for bps in [5000, 2500, 1000, 500, 100, 10, 1] {
                                                            if full && bps != 5000 {
                                                                continue;
                                                            }
                                                            let mut mm = front_m.clone();
                                                            let mut pp = p.clone();
                                                            let mut tt = AuditTotals::default();
                                                            match cmp_current_candidate(&mut mm,&mut pp,debt,fee,120,2,false,mark,&mut tt,&[bps],flash,full) {
                            Ok(true)=>{
                                let rr=tt.rewards as f64/1000.0;let (_,_,arb)=cmp_arb(&mm,&venue,debt,fee).unwrap();
                                if rr+arb>best {best=rr+arb;t=mm;next=pp;reward=rr;repaid=tt.repayment;improves=true;totals=tt;status="ok".to_owned();}
                            },Ok(false)=>{},Err(e)=>panic!("accepted candidate error: {case:?},{size},{eq},{mark},{tbps},{front},{flash},{full},{bps}: {}",error_name(e))
                        }
                                                        }
                                                    }
                                                }
                                                status
                                            };
                                            let (mut after, v_after, backprofit) = cmp_arb(&t, &v, debt, fee).unwrap();
                                            after.checkpoint_hlp_vaults().unwrap();
                                            after.assert_market_invariants().unwrap();
                                            let nav = cmp_nav(&after, debt, mark).unwrap();
                                            let profit = front_out as f64 / 1000.0 - front_cost + backprofit + reward;
                                            let h = after
                                                .flash_liquidation_health(&Ref::Leverage(&next), debt, fee)
                                                .unwrap();
                                            let residual_mark_gap = (h.debt as f64 / 1000.0
                                                - cmp_value(next.collateral_amount, debt.opposite(), debt, mark))
                                            .max(0.0);
                                            let mut liquidation_venue = v_after.clone();
                                            // Conservative external realization scenario: custody->wallet->venue,
                                            // venue output->wallet->debt vault; no additional insurance assumption.
                                            let residual_receipt = fee
                                                .unwind_credit(
                                                    liquidation_venue
                                                        .sell(fee.unwind_credit(next.collateral_amount).unwrap(), fee)
                                                        .unwrap(),
                                                )
                                                .unwrap();
                                            let residual_route_gap =
                                                h.debt.saturating_sub(residual_receipt) as f64 / 1000.0;
                                            println!("EMA_CMP,{amp},{hlp},{},{size},{eq},{mark},{tbps},{withdrawal},{front},{method},{status},{},{},{},{reward:.9},{front_cost:.9},{:.9},{backprofit:.9},{profit:.9},{:.9},{:.9},{:.9},{:.9},{:.9},{:.9},{},{},{},{residual_mark_gap:.9},{residual_route_gap:.9},{},{},{},{},{:.9},{}",
                    debt.code(),repaid as f64/1000.0,h.debt as f64/1000.0,next.collateral_amount as f64/1000.0,front_out as f64/1000.0,
                    nav[0],nav[1],nav[2],nav[3],nav[4],nav[5],h.eligible().unwrap(),health.emergency_allowed().unwrap(),improves,
                    totals.insurance as f64/1000.0,totals.writeoff as f64/1000.0,totals.fees as f64/1000.0,totals.full,nav[6],health.maintenance_bps);
                                            rows += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("EMA_COUNTS,constructed={constructed},skipped={skipped},rows={rows}");
    assert_eq!(constructed, 384);
    assert_eq!(skipped, 0);
    assert_eq!(rows, 4608);
}

// Audit copy of current native policy: parameterized candidate search, 20%
// protocol swap share, and captured pre-reconstruction liquidation-fee ownership.
fn cmp_current_candidate(
    m: &mut Market,
    p: &mut LeveragePosition,
    side: MarketAsset,
    f: LeverageCollateralFee,
    now: i64,
    slot: u64,
    external: bool,
    mark: u64,
    t: &mut AuditTotals,
    sizes: &[u64],
    try_flash: bool,
    which_full: bool,
) -> Result<bool> {
    let health = m.flash_liquidation_health(&Ref::Leverage(p), side, f)?;
    if !health.eligible()? {
        return Ok(false);
    }
    let debt = p.debt_amount(&m.debt)?;
    if try_flash {
        for full in [which_full] {
            for &bps in sizes {
                if full && bps != 5_000 {
                    continue;
                }
                let q = match m.quote_flash_liquidation(
                    &Ref::Leverage(p),
                    side,
                    debt * bps / 10_000,
                    full,
                    f,
                    f,
                    f,
                    now,
                    slot,
                ) {
                    Ok(q) => q,
                    Err(_) => continue,
                };
                let revenue = external_receipt(q.collateral_credit, mark, f)?;
                let required = f.effective_gross_for_credit(q.payment)?;
                let external_profitable = external && revenue >= required.saturating_add(50);
                let mut trial = m.clone();
                let mut next = p.clone();
                let allocation =
                    trial.liquidation_debt_allocation(&Ref::Leverage(&next), side, q.debt_shares_to_burn)?;
                let budget = q.repayment + q.fee.total + q.owner_surplus;
                let repaid = if full {
                    budget.min(allocation.debt)
                } else {
                    allocation.debt
                };
                let fee = LiquidationFeeAllocation::quote(
                    repaid,
                    (budget - repaid).min(q.fee.total),
                    trial.insurance.available(side),
                    trial.liquidation_insurance_target(side)?,
                );
                let cap = if full {
                    f.unwind_credit(trial.insurance.draw_capacity(side, slot)?)?
                } else {
                    0
                };
                let loss = LiquidationLossAllocation::quote_with_coverage(
                    allocation.principal.min(allocation.debt),
                    allocation.debt,
                    repaid,
                    cap,
                    trial.insurance.principal_coverage_bps,
                )?;
                let profit;
                let mut eligibility = HlpYieldEligibility {
                    ylp_supply: trial.base_side.shares.ylp_supply,
                    base_hlp_ylp_shares: trial.base_hlp_vault.ylp_shares,
                    quote_hlp_ylp_shares: trial.quote_hlp_vault.ylp_shares,
                };
                if external_profitable {
                    trial.settle_flash_recovery(&mut Mut::Leverage(&mut next), side, allocation, q.collateral_debit, repaid, f.effective_gross_for_credit(loss.insurance_credit)?, loss.insurance_credit, full, slot)?;
                    profit = revenue - required;
                } else {
                    let credit = f.unwind_credit(q.collateral_credit)?;
                    let mut swap = match (SwapRequest {
                        current_slot: slot,
                        current_unix_timestamp: now,
                        asset_in: side.opposite(),
                        reserve_credit: credit,
                        protocol_fee_bps: 2000,
                    })
                    .prepare_with_cash_policy(&mut trial, SwapCashPolicy::LiquidationQuote)
                    {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    eligibility = swap.interest_eligibility;
                    let owner = budget - repaid - fee.total;
                    let obligation = repaid
                        + f.effective_gross_for_credit(fee.insurance)?
                        + f.effective_gross_for_credit(fee.lp + fee.protocol)?
                        + f.effective_gross_for_credit(owner)?;
                    if swap.quote.amount_out < obligation {
                        continue;
                    }
                    profit = f.unwind_credit(swap.quote.amount_out - obligation)?;
                    if profit < 50 {
                        continue;
                    }
                    let policy = SwapCashPolicy::SettleLiquidation {
                        debt_asset: side,
                        isolated: true,
                        full,
                        shares: allocation.shares,
                        principal_removed: allocation.principal,
                        debt_reduced: allocation.debt,
                        repayment: repaid,
                        insurance_credit: loss.insurance_credit,
                    };
                    if swap.bind_liquidation(&trial, policy).is_err() {
                        continue;
                    }
                    let credit = full_fee_credit(&swap.leverage_quote());
                    swap.apply(
                        &mut trial,
                        policy,
                        credit,
                        slot,
                        2000,
                        ProtocolAuctionSplit::default(),
                        None,
                    )?;
                    if loss.insurance_credit > 0 {
                        trial.insurance.consume_draw(
                            side,
                            f.effective_gross_for_credit(loss.insurance_credit)?,
                            slot,
                        )?;
                    }
                    trial.apply_liquidation_position(
                        &mut Mut::Leverage(&mut next),
                        side,
                        allocation,
                        q.collateral_debit,
                        full,
                    )?;
                    trial.reconcile_liquidation_position(&mut Mut::Leverage(&mut next), side, allocation.remaining_debt)?;
                }
                cmp_publish_fee(&mut trial, side, fee, slot, eligibility)?;
                trial.assert_market_invariants()?;
                t.flash += 1;
                t.full += u64::from(full);
                t.repayment += repaid;
                t.insurance += loss.insurance_credit;
                t.writeoff += loss.principal_written_off;
                t.canceled += loss.interest_canceled;
                t.rewards += profit;
                t.fees += fee.total;
                *m = trial;
                *p = next;
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if !health.emergency_allowed()? {
        return Ok(false);
    }
    for full in [which_full] {
        for &bps in sizes {
            if full && bps != 5_000 {
                continue;
            }
            let mut trial = m.clone();
            let mut next = p.clone();
            let sold = if full {
                next.collateral_amount
            } else {
                next.collateral_amount * bps / 10_000
            };
            let mut prepared = match trial.prepare_emergency_liquidation(
                &Ref::Leverage(&next),
                side,
                sold,
                full,
                f,
                f,
                f,
                now,
                slot,
                2000,
            ) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let q = prepared.quote;
            let eligibility = prepared
                .swap
                .as_ref()
                .map(|s| s.interest_eligibility)
                .unwrap_or(HlpYieldEligibility {
                    ylp_supply: trial.base_side.shares.ylp_supply,
                    base_hlp_ylp_shares: trial.base_hlp_vault.ylp_shares,
                    quote_hlp_ylp_shares: trial.quote_hlp_vault.ylp_shares,
                });
            if q.reward_credit < 50 {
                continue;
            }
            trial.apply_emergency_liquidation(&mut Mut::Leverage(&mut next), side, &mut prepared, slot, 2000, ProtocolAuctionSplit::default())?;
            cmp_publish_fee(&mut trial, side, q.fee, slot, eligibility)?;
            trial.assert_market_invariants()?;
            t.emergency += 1;
            t.full += u64::from(full);
            t.repayment += q.repayment;
            t.insurance += q.loss.insurance_credit;
            t.writeoff += q.loss.principal_written_off;
            t.canceled += q.loss.interest_canceled;
            t.rewards += q.reward_credit;
            t.fees += q.fee.total;
            *m = trial;
            *p = next;
            return Ok(true);
        }
    }
    Ok(false)
}

fn cmp_publish_fee(
    m: &mut Market,
    side: MarketAsset,
    fee: LiquidationFeeAllocation,
    slot: u64,
    e: HlpYieldEligibility,
) -> Result<()> {
    if fee.insurance > 0 {
        m.insurance.credit(side, fee.insurance, slot)?;
    }
    m.side_mut(side).record_liquidation_fee_credit(
        fee.lp,
        fee.protocol,
        ProtocolAuctionSplit::default(),
        e.ylp_supply,
    )?;
    m.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, e.base_hlp_ylp_shares)?;
    m.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, e.quote_hlp_ylp_shares)?;
    Ok(())
}

#[test]
fn audit_ema_inventory_withdrawal_does_not_trigger_healthy_position() {
    for amp in [1, 4, 10] {
        for hlp in [false, true] {
            for debt in [MarketAsset::Base, MarketAsset::Quote] {
                let case = Case {
                    amp,
                    controller: true,
                    hlp,
                    debt_asset: debt,
                    pieces: 1,
                    path: 0,
                    cushion: 0,
                    withdrawal: 2500,
                };
                let mut m = initialize(case);
                let fee = audit_fee(0);
                let mut p = seeded_position(&mut m, debt, 800 * CALIBRATION_UNIT, 1000 * CALIBRATION_UNIT);
                p.margin_terms =
                    crate::state::LeverageMarginTerms::at_entry(m.leverage_reference_depth(debt.opposite()).unwrap())
                        .unwrap();
                no_accrual(&mut m, 2);
                let before = m.flash_liquidation_health(&Ref::Leverage(&p), debt, fee).unwrap();
                assert!(!before.eligible().unwrap());
                let ordinary =
                    m.base_side.shares.ylp_supply - m.base_hlp_vault.ylp_shares - m.quote_hlp_vault.ylp_shares;
                m.remove_liquidity(ordinary / 4).unwrap();
                m.finalize_amm_transition_and_observe_risk(2).unwrap();
                let after = m.flash_liquidation_health(&Ref::Leverage(&p), debt, fee).unwrap();
                assert_eq!(before.collateral_value, after.collateral_value);
                assert_eq!(before.debt, after.debt);
                assert_eq!(before.maintenance_bps, after.maintenance_bps);
                assert!(!after.eligible().unwrap());
                assert!(cmp_v1_inventory(&m, &p, debt, fee, 5000).is_err());
            }
        }
    }
}

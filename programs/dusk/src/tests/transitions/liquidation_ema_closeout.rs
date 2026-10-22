// Current native settlement, admitted positions and a finite external venue.
// Test-only economic evidence; no historical exploit or alternate settlement policy.
use super::*;

fn custody_resources(m: &Market) -> [u128; 2] {
    [MarketAsset::Base, MarketAsset::Quote].map(|a| {
        let s = m.side(a);
        u128::from(s.reserves.cash_reserve)
            + u128::from(s.fees.swap_fee_custody_balance)
            + u128::from(s.fees.interest_vault_balance)
            + u128::from(s.reserves.total_hlp_backing_inventory().unwrap())
            + u128::from(s.reserves.protected_recenter_reserve)
            + u128::from(m.insurance.available(a))
    })
}

thread_local! { static AUDIT_SLOT: std::cell::Cell<u64> = const { std::cell::Cell::new(1000) }; }
fn audit_slot() -> u64 {
    AUDIT_SLOT.get()
}
fn cmp_trade(m: &Market, a: MarketAsset, gross: u64, f: LeverageCollateralFee) -> Result<(Market, u64)> {
    let mut t = m.clone();
    let mut s = SwapRequest {
        current_slot: audit_slot(),
        current_unix_timestamp: 400,
        asset_in: a,
        reserve_credit: f.unwind_credit(gross)?,
        protocol_fee_bps: 2000,
    }
    .prepare(&mut t)?;
    let out = f.unwind_credit(s.quote.amount_out)?;
    s.finalize_state(&mut t, crate::clock::test_clock(audit_slot()), 2000, ProtocolAuctionSplit::default())?;
    t.assert_market_invariants()?;
    Ok((t, out))
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

// The old stress helper caps a shock at one side's cash. Extend only the
// test's exogenous price-path search; every candidate remains a native swap.
fn native_stress(m: &mut Market, asset: MarketAsset, target: u64, slot: u64) {
    let mut high = m.side(asset).reserves.cash_reserve.max(1);
    let mut upper = None;
    for _ in 0..10 {
        if let Ok((trial, _)) = sale(m, asset, high, slot) {
            if price(&trial, asset) <= target {
                upper = Some(trial);
                break;
            }
        }
        high = high.checked_mul(2).unwrap();
    }
    let mut best = upper.expect("native exogenous shock unavailable");
    let mut low = 1;
    while low < high {
        let mid = low + (high - low) / 2;
        match sale(m, asset, mid, slot) {
            Ok((trial, _)) if price(&trial, asset) <= target => {
                best = trial;
                high = mid;
            }
            _ => {
                low = mid + 1;
            }
        }
    }
    *m = best;
}

#[test]
fn ema_admitted_settlement_preserves_reference_and_resource_delta() {
    println!("EMA_NATIVE,amp,hlp,side,tokenfee,front,method,reference_price,reference_depth,output,repayment,writeoff,insurance,reward,front_cost,front_out,reverse_profit,profit,ylp,base_hlp,quote_hlp,protocol,protected,insurance_remaining,backing,remaining_debt,marked_custody");
    let mut cases = 0;
    for amp in [1, 4, 10] {
        for hlp in [false, true] {
            for side in [MarketAsset::Base, MarketAsset::Quote] {
                for tbps in [0, 50] {
                    let mut m = initialize(Case {
                        amp,
                        controller: true,
                        hlp,
                        debt_asset: side,
                        pieces: 1,
                        path: 0,
                        cushion: 0,
                        withdrawal: 0,
                    });
                    let f = audit_fee(tbps);
                    no_accrual(&mut m, 2);
                    let mut p = audit_open(&mut m, side, 15_000_000, 5000, f).unwrap();
                    let mark = NAD / 2;
                    no_accrual(&mut m, 3);
                    native_stress(&mut m, side.opposite(), mark, 3);
                    no_accrual(&mut m, 1000);
                    m.observe_current_risk(crate::clock::test_clock(1000)).unwrap();
                    let health = m.flash_liquidation_health(&Ref::Leverage(&p), side, f).unwrap();
                    assert!(
                        health.debt >= health.collateral_value,
                        "non-insolvent: {amp} {hlp} {side:?} {tbps}"
                    );
                    p.distress.observe(true, 0, 1000).unwrap();
                    let price = m.pessimistic_collateral_price_nad(side.opposite(), &m.current_risk().unwrap(), false);
                    let depth = m.current_risk().unwrap().curve_depth_ema_nad;
                    let mut baseline_quote = None;
                    let mut baseline_delta = None;
                    for front in [0, 1000, 5000] {
                        let mut venue = CmpVenue::new(mark);
                        let cost = venue.buy_exact(front * 1000, f).unwrap();
                        let (fm, out) = if front == 0 {
                            (m.clone(), 0)
                        } else {
                            cmp_trade(&m, side.opposite(), front * 1000, f).unwrap()
                        };
                        for liq in [false, true] {
                            let mut t = fm.clone();
                            let mut next = p.clone();
                            let mut output = 0;
                            let mut repayment = 0;
                            let mut writeoff = 0;
                            let mut insurance = 0;
                            let mut reward = 0;
                            if liq {
                                let mut prep = t
                                    .prepare_emergency_liquidation(
                                        &Ref::Leverage(&next),
                                        side,
                                        next.collateral_amount,
                                        true,
                                        f,
                                        f,
                                        f,
                                        400,
                                        1000,
                                        2000,
                                    )
                                    .unwrap();
                                let q = prep.quote;
                                if let Some(expected) = baseline_quote {
                                    assert_eq!(q, expected);
                                } else {
                                    baseline_quote = Some(q);
                                }
                                let resources_before = custody_resources(&t);
                                assert_eq!(q.reference_price_nad, price);
                                assert_eq!(q.reference_depth_nad, depth);
                                let ownership = prep.swap.as_ref().unwrap().interest_eligibility;
                                t.apply_emergency_liquidation(
                                    &mut Mut::Leverage(&mut next),
                                    side,
                                    &mut prep,
                                    f,
                                    400,
                                    1000,
                                    2000,
                                    ProtocolAuctionSplit::default(),
                                )
                                .unwrap();
                                cmp_publish_fee(&mut t, side, q.fee, crate::clock::test_clock(1000), ownership).unwrap();
                                assert_eq!(q.loss.interest_paid, 0);
                                assert_eq!(next.debt_shares, 0);
                                t.assert_market_invariants().unwrap();
                                let resources_after = custody_resources(&t);
                                let delta = std::array::from_fn::<_, 2, _>(|i| {
                                    resources_after[i] as i128 - resources_before[i] as i128
                                });
                                if let Some(expected) = baseline_delta {
                                    assert_eq!(delta, expected);
                                } else {
                                    baseline_delta = Some(delta);
                                }
                                output = q.swap_output;
                                repayment = q.repayment;
                                writeoff = q.loss.principal_written_off;
                                insurance = q.loss.insurance_credit;
                                reward = q.reward_credit;
                            }
                            let (end, _, reverse) = cmp_arb(&t, &venue, side, f).unwrap();
                            end.assert_market_invariants().unwrap();
                            let nav = cmp_nav(&end, side, mark).unwrap();
                            let physical = custody_resources(&end);
                            let physical_marked = physical[side.code() as usize] as f64 / 1000.0
                                + physical[side.opposite().code() as usize] as f64 / 1000.0 * mark as f64 / NAD as f64;
                            let profit = (out as f64 - cost as f64 + reward as f64) / 1000.0 + reverse;
                            println!("EMA_NATIVE,{amp},{hlp},{},{tbps},{front},{},{price},{depth},{output},{repayment},{writeoff},{insurance},{reward},{cost},{out},{reverse:.9},{profit:.9},{:.9},{:.9},{:.9},{:.9},{:.9},{:.9},{:.9},{},{physical_marked:.9}",side.code(),if liq{"emergency"}else{"control"},nav[0],nav[1],nav[2],nav[3],nav[4],nav[5],nav[6],next.debt_amount(&end.debt).unwrap());
                        }
                    }
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}

// Deterministic economic scenarios for the shared native liquidation policy.
// Stylized routes and sampled fill sizes are not a guarantee of profitable cleanup.
use super::*;
use crate::state::{LiquidationFeeAllocation, LiquidationLossAllocation};
use crate::transitions::{flash_liquidation::LiquidationPositionRef as Ref, flash_settlement::LiquidationPositionMut as Mut};

fn audit_fee(bps: u16) -> LeverageCollateralFee {
    LeverageCollateralFee::new((bps > 0).then_some(TransferFee {
        epoch: 0.into(), maximum_fee: u64::MAX.into(), transfer_fee_basis_points: bps.into(),
    }))
}

fn audit_open(m: &mut Market, side: MarketAsset, spend: u64, wallet_bps: u16, f: LeverageCollateralFee) -> Result<LeveragePosition> {
    let wallet = spend * u64::from(wallet_bps) / 10_000;
    let net = f.unwind_credit(wallet)?;
    let multiplier = 100_000_000 / u64::from(wallet_bps);
    let debt = leverage_debt_from_margin(net, multiplier)?;
    let prepared = SwapRequest { current_slot: 2, current_unix_timestamp: 0, asset_in: side,
        reserve_credit: debt + net, protocol_fee_bps: 0 }.prepare_with_cash_policy(m, SwapCashPolicy::Borrow { asset: side, amount: debt })?;
    let q = prepared.leverage_quote();
    let mut p = empty_position();
    m.open_leverage(&mut p, Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(),
        Pubkey::default(), 0, side, net, multiplier, f.unwind_credit(q.amount_out)?, prepared,
        full_fee_credit(&q), 0, 2, 255, 0, ProtocolAuctionSplit::default(), f)?;
    Ok(p)
}

#[derive(Default)]
struct AuditTotals { opened: u64, entry_debt: u64, flash: u64, emergency: u64, full: u64,
    repayment: u64, insurance: u64, writeoff: u64, canceled: u64, rewards: u64, fees: u64,
    eligible: u64, eligible_debt: u64, remaining: u64, failed: u64, admitted_error: String, path_error: String }

fn publish_audit_fee(m: &mut Market, side: MarketAsset, fee: LiquidationFeeAllocation, interest: u64, slot: u64) -> Result<()> {
    if fee.insurance>0 {m.insurance.credit(side, fee.insurance, slot)?;}
    let supply = m.base_side.shares.ylp_supply;
    m.side_mut(side).record_liquidation_fee_credit(fee.lp, fee.protocol, ProtocolAuctionSplit::default(), supply)?;
    m.side_mut(side).record_interest_credit(interest, 0, ProtocolAuctionSplit::default(), 0)?;
    m.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, m.base_hlp_vault.ylp_shares)?;
    m.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, m.quote_hlp_vault.ylp_shares)?;
    Ok(())
}

// External venue: a deterministic $100k debt-side CPMM depth at the exogenous
// path price, 30bps venue fee, plus both token transfer fees. This is an explicit
// liquidity assumption, not a Jupiter quote or evidence that a route exists.
fn external_receipt(credit: u64, mark: u64, f: LeverageCollateralFee) -> Result<u64> {
    let input = f.unwind_credit(credit)?;
    let fair = u128::from(input) * u128::from(mark) / u128::from(NAD);
    let depth = 100_000 * u128::from(CALIBRATION_UNIT);
    f.unwind_credit((fair * depth / (depth + fair) * 9_970 / 10_000) as u64)
}

fn audit_fill(m: &mut Market, p: &mut LeveragePosition, side: MarketAsset, f: LeverageCollateralFee,
    now: i64, slot: u64, external: bool, mark: u64, t: &mut AuditTotals) -> Result<bool> {
    audit_fill_sizes(m,p,side,f,now,slot,external,mark,t,&[5_000,2_000,1_000,500,100,10])
}

fn audit_fill_sizes(m: &mut Market, p: &mut LeveragePosition, side: MarketAsset, f: LeverageCollateralFee,
    now: i64, slot: u64, external: bool, mark: u64, t: &mut AuditTotals, sizes: &[u64]) -> Result<bool> {
    let health = m.flash_liquidation_health(&Ref::Leverage(p), side, f)?;
    p.distress.observe(health.eligible()?, now, slot)?;
    if !health.eligible()? { return Ok(false); }
    let debt = p.debt_amount(&m.debt)?;
    for full in [false, true] {
        for &bps in sizes {
            if full && bps != 5_000 { continue; }
            let q = match m.quote_flash_liquidation(&Ref::Leverage(p), side, debt * bps / 10_000,
                full, f, f, f, now, slot) { Ok(q) => q, Err(_) => continue };
            let revenue = external_receipt(q.collateral_credit, mark, f)?;
            let required = f.effective_gross_for_credit(q.payment)?;
            let external_profitable=external && revenue >= required.saturating_add(50);
            let mut trial = m.clone(); let mut next = p.clone();
            let allocation = trial.liquidation_debt_allocation(&Ref::Leverage(&next), side, q.debt_shares_to_burn)?;
            let budget = q.repayment + q.fee.total + q.owner_surplus;
            let repaid = if full { budget.min(allocation.debt) } else { allocation.debt };
            let fee = LiquidationFeeAllocation::quote(repaid, (budget-repaid).min(q.fee.total), trial.insurance.available(side), trial.liquidation_insurance_target(side)?);
            let cap = if full { f.unwind_credit(trial.insurance.draw_capacity(side,slot)?)? } else { 0 };
            let loss = LiquidationLossAllocation::quote_with_coverage(allocation.principal.min(allocation.debt), allocation.debt,
                repaid, cap, trial.insurance.principal_coverage_bps)?;
            let profit;
            if external_profitable {
                trial.settle_flash_recovery(&mut Mut::Leverage(&mut next), side, allocation, q.collateral_debit, repaid,
                    f.effective_gross_for_credit(loss.insurance_credit)?, loss.insurance_credit, full, f, now, slot)?;
                profit=revenue-required;
            } else {
                let credit=f.unwind_credit(q.collateral_credit)?;
                let mut swap=match (SwapRequest {current_slot:slot,current_unix_timestamp:now,asset_in:side.opposite(),reserve_credit:credit,protocol_fee_bps:0})
                    .prepare_with_cash_policy(&mut trial,SwapCashPolicy::LiquidationQuote) {Ok(s)=>s,Err(_)=>continue};
                let owner=budget-repaid-fee.total;
                let obligation=repaid+f.effective_gross_for_credit(fee.insurance)?+f.effective_gross_for_credit(fee.lp+fee.protocol)?+f.effective_gross_for_credit(owner)?;
                if swap.quote.amount_out<obligation {continue;}
                profit=f.unwind_credit(swap.quote.amount_out-obligation)?;
                if profit<50 {continue;}
                let policy=SwapCashPolicy::SettleLiquidation {debt_asset:side,isolated:true,full,shares:allocation.shares,
                    principal_removed:allocation.principal,debt_reduced:allocation.debt,repayment:repaid,insurance_credit:loss.insurance_credit};
                if swap.bind_liquidation(&trial,policy).is_err(){continue;}
                let credit=full_fee_credit(&swap.leverage_quote());
                swap.apply(&mut trial,policy,credit,slot,0,ProtocolAuctionSplit::default(),None)?;
                if loss.insurance_credit>0 {trial.insurance.consume_draw(side,f.effective_gross_for_credit(loss.insurance_credit)?,slot)?;}
                trial.apply_liquidation_position(&mut Mut::Leverage(&mut next),side,allocation,q.collateral_debit,full)?;
                trial.observe_liquidation_settlement(&mut Mut::Leverage(&mut next),side,allocation.remaining_debt,full,f,now,slot)?;
            }
            publish_audit_fee(&mut trial, side, fee, loss.interest_paid, slot)?;
            trial.assert_market_invariants()?;
            t.flash += 1; t.full += u64::from(full); t.repayment += repaid; t.insurance += loss.insurance_credit;
            t.writeoff += loss.principal_written_off; t.canceled += loss.interest_canceled; t.rewards += profit; t.fees += fee.total;
            *m=trial; *p=next; return Ok(true);
        }
    }
    if !health.emergency_allowed()? { return Ok(false); }
    for full in [false, true] {
        for &bps in sizes {
            if full && bps != 5_000 { continue; }
            let mut trial = m.clone(); let mut next = p.clone();
            let sold = if full { next.collateral_amount } else { next.collateral_amount * bps / 10_000 };
            let mut prepared = match trial.prepare_emergency_liquidation(&Ref::Leverage(&next), side, sold, full, f,f,f,now,slot,0) {
                Ok(p) => p, Err(_) => continue
            };
            let q=prepared.quote;
            if q.reward_credit < 50 { continue; }
            trial.apply_emergency_liquidation(&mut Mut::Leverage(&mut next), side, &mut prepared, f,now,slot,0,ProtocolAuctionSplit::default())?;
            publish_audit_fee(&mut trial,side,q.fee,q.loss.interest_paid,slot)?;
            trial.assert_market_invariants()?;
            t.emergency+=1;t.full+=u64::from(full);t.repayment+=q.repayment;t.insurance+=q.loss.insurance_credit;
            t.writeoff+=q.loss.principal_written_off;t.canceled+=q.loss.interest_canceled;t.rewards+=q.reward_credit;t.fees+=q.fee.total;
            *m=trial;*p=next;return Ok(true);
        }
    }
    Ok(false)
}

#[test]
fn final_economic_policy_paths() {
    println!("FINAL_ECON,amp,hlp,side,fee_bps,wallet_bps,pieces,path,external,withdrawal,opened,entry_debt,flash,emergency,full,repaid,insurance,writeoff,canceled,rewards,fees,remaining,eligible,eligible_debt,failed,entry_error,path_error");
    let mut count=0;
    for amp in [1,4,10] { for hlp in [false,true] { for side in [MarketAsset::Base,MarketAsset::Quote] {
    for fee_bps in [0,50] { for wallet_bps in [2_000,5_000] { for pieces in [1,3] {
    for path in [0,1,2] { for external in [false,true] {
        let withdrawal=if path==2 {2_500} else {0};
        let case=Case{amp,controller:true,hlp,debt_asset:side,pieces,path,cushion:0,withdrawal};
        let mut m=initialize(case); m.config.global_health_contribution_cap_bps=15_000;m.config.borrow_market_health_floor_bps=11_000;m.config.validate().unwrap(); let f=audit_fee(fee_bps); let mut ps=Vec::new(); let mut t=AuditTotals::default();
        for _ in 0..pieces {
            let mut trial=m.clone();
            match audit_open(&mut trial,side,15_000*CALIBRATION_UNIT/pieces,wallet_bps,f) {
                Ok(p)=>{t.opened+=1;t.entry_debt+=p.debt_amount(&trial.debt).unwrap();ps.push(p);m=trial;},
                Err(e)=>{t.admitted_error=error_name(e);break;}
            }
        }
        if !ps.is_empty() {
            for step in 1..=18u64 {
                let now=(step*30) as i64;let slot=2+step*75;
                if let Err(e)=m.accrue_interest_to_slot(slot) {t.path_error=error_name(e);break;}
                if step==2 && withdrawal>0 {
                    let mut trial=m.clone();let burn=trial.base_side.shares.ylp_supply*withdrawal/10_000;
                    match trial.remove_liquidity(burn).and_then(|_|trial.finalize_amm_transition_and_observe_risk(slot)) {
                        Ok(())=>m=trial,Err(e)=>{t.path_error=format!("lp:{}",error_name(e));break;}
                    }
                }
                let mark=match path {0=>NAD*(100-step*3)/100,1=>NAD*55/100,_=>NAD*80/100};
                // Existing stress helper uses real curve swaps and preserves the
                // accrued indices; its no_accrual only pins the already-accrued slot.
                if let Err(e)=stress_to(&mut m,side.opposite(),mark,slot) {t.path_error=e;break;}
                for p in &mut ps {
                    for _ in 0..12 {
                        if p.debt_shares==0 {break;}
                        match audit_fill(&mut m,p,side,f,now,slot,external,mark,&mut t) {
                            Ok(true)=>{},Ok(false)=>break,Err(e)=>{t.failed+=1;t.path_error=format!("settle:{}",error_name(e));break;}
                        }
                    }
                }
                if let Err(e)=m.assert_market_invariants() {t.path_error=format!("invariant:{}",error_name(e));break;}
                if ps.iter().all(|p|p.debt_shares==0) {break;}
            }
            for (position_index, p) in ps.iter().enumerate() { let h=m.flash_liquidation_health(&Ref::Leverage(p),side,f).unwrap();t.remaining+=h.debt;
                if h.eligible().unwrap(){
                    t.eligible+=1;t.eligible_debt+=h.debt;
                    // Regression for the original 19 critical terminal stalls.
                    // In these completed stress paths, the AMM recovery fallback
                    // must clear every emergency-eligible residual. This is not
                    // a guarantee for arbitrary liquidity or keeper economics.
                    if t.path_error.is_empty() {
                        assert!(!h.emergency_allowed().unwrap(), "critical residual: {case:?}, fee={fee_bps}, wallet={wallet_bps}, external={external}");
                    }
                    if t.path_error.is_empty() && std::env::var_os("DUSK_LIQUIDATION_DIAGNOSTICS").is_some() {
                        diagnose_unfilled(&m,p,case,fee_bps,wallet_bps,external,position_index);
                    }
                }
            }
        }
        assert_eq!(t.failed, 0, "accepted settlement failed: amp={amp}, hlp={hlp}, side={side:?}, fee={fee_bps}, path={path}: {}", t.path_error);
        assert!(!t.path_error.starts_with("invariant:"), "{}", t.path_error);
        println!("FINAL_ECON,{amp},{hlp},{},{fee_bps},{wallet_bps},{pieces},{path},{external},{withdrawal},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",side.code(),t.opened,t.entry_debt,t.flash,t.emergency,t.full,t.repayment,t.insurance,t.writeoff,t.canceled,t.rewards,t.fees,t.remaining,t.eligible,t.eligible_debt,t.failed,t.admitted_error,t.path_error);
        count+=1;
    }}}}}}}}
    assert_eq!(count,576);
}

include!("liquidation_unfilled_diagnostics.rs");

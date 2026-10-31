
pub fn research_state_to_array(s: crate::transitions::liquidity::IntegratedCurveState) -> [i128;17] {
 [s.ordinary_base as i128,s.ordinary_quote as i128,s.base_hlp_equity as i128,s.quote_hlp_equity as i128,
 s.base_hlp_quote_debt as i128,s.quote_hlp_base_debt as i128,s.base_hlp_quote_carry as i128,
 s.quote_hlp_base_carry as i128,s.ordinary_base_book_offset,s.ordinary_quote_book_offset,
 s.base_unpaid_interest as i128,s.quote_unpaid_interest as i128,s.base_atom_nad as i128,
 s.quote_atom_nad as i128,s.frozen_hlp_mask as i128,s.frozen_base_curve_offset_nad,s.frozen_quote_curve_offset_nad]
}
pub fn research_array_to_state(v:[i128;17])->crate::transitions::liquidity::IntegratedCurveState {
 use crate::transitions::liquidity::IntegratedCurveState;
 IntegratedCurveState{ordinary_base:v[0] as u128,ordinary_quote:v[1] as u128,
 base_hlp_equity:v[2] as u128,quote_hlp_equity:v[3] as u128,
 base_hlp_quote_debt:v[4] as u128,quote_hlp_base_debt:v[5] as u128,
 base_hlp_quote_carry:v[6] as u64,quote_hlp_base_carry:v[7] as u64,
 ordinary_base_book_offset:v[8],ordinary_quote_book_offset:v[9],
 base_unpaid_interest:v[10] as u128,quote_unpaid_interest:v[11] as u128,
 base_atom_nad:v[12] as u128,quote_atom_nad:v[13] as u128,frozen_hlp_mask:v[14] as u8,
 frozen_base_curve_offset_nad:v[15],frozen_quote_curve_offset_nad:v[16]}
}
pub fn research_kernel(v:[i128;17], amount:u128, direction:u8, amp:u64, center:u64)
 -> Result<([i128;17],[u128;2],[u128;4],u128)> {
 use crate::transitions::liquidity::*;
 use crate::transitions::amm::{prepare_concentrated_cache_at_point,ConcentratedCurveParameters};
 let s=research_array_to_state(v);
 let point=s.reconstructed_curve_point()?.ok_or(ErrorCode::InsufficientLiquidity)?;
 let (end,out)=if amount>0 {
  let cache=prepare_concentrated_cache_at_point(point.base_reserve,point.quote_reserve,center,
   ConcentratedCurveParameters{peak_amplification_nad:amp*NAD,core_half_width_bps:if amp>1{1000}else{0},fade_width_bps:if amp>1{4000}else{0}})?;
  let q=quote_integrated_exact_in(s,cache.geometry()?,amount,
   if direction==0{IntegratedSwapDirection::BaseToQuote}else{IntegratedSwapDirection::QuoteToBase})?;
  (q.end,q.amount_out)
 }else{(s,0)};
 let p=end.reconstructed_curve_point()?.ok_or(ErrorCode::InsufficientLiquidity)?;
 let e=reconstruct_hlp_endpoint(end)?;
 Ok((research_state_to_array(end),[p.base_reserve,p.quote_reserve],
 [e.total_base,e.total_quote,e.base_hlp_quote_debt,e.quote_hlp_base_debt],out))
}
pub fn research_make(decimals:[u8;2],kind:u8,amp:u64)->Result<Market>{
 use crate::state::{AmmConfig,IrmConfig};
 let side=|d|MarketSide{asset_mint:Pubkey::new_unique(),hlp_mint:Pubkey::new_unique(),
 reserve_vault:Pubkey::new_unique(),collateral_vault:Pubkey::new_unique(),
 interest_vault:Pubkey::new_unique(),asset_decimals:d,..MarketSide::default()};
 let cfg=MarketConfig{swap_fee_bps:0,divergence_fee_share_cap_bps:0,volatility_fee_share_cap_bps:0,
 target_hlp_leverage_bps:20000,settlement_divergence_bps:10000,ema_half_life_ms:60000,
 directional_ema_half_life_ms:60000,curve_depth_ema_half_life_ms:60000,max_daily_borrow_bps:crate::state::DEFAULT_DAILY_BORROW_BPS,
 global_health_contribution_cap_bps:15000,borrow_market_health_floor_bps:11000,
 amm:AmmConfig{peak_amplification_nad:amp*NAD,core_half_width_bps:if amp>1{1000}else{0},
 fade_width_bps:if amp>1{4000}else{0},center_ema_half_life_ms:60000,volatility_half_life_ms:60000,
 ..AmmConfig::default()},irm:IrmConfig::default(),..MarketConfig::default()};
 let mut m=Market::default();
 m.initialize(Pubkey::new_unique(),side(decimals[0]),side(decimals[1]),cfg,
 Pubkey::new_unique(),Pubkey::new_unique(),Pubkey::new_unique(),Pubkey::new_unique(),[0;32],
 Pubkey::new_unique(),60*NAD,0,2500,255)?;
 let raw=|n:u64,d:u8|n*10u64.pow(u32::from(d));
 m.add_liquidity(raw(10000,decimals[0]),raw(600000,decimals[1]))?;
 m.finalize_amm_transition_and_observe_risk(2500)?;
 m.deposit_single_sided(MarketAsset::Base,raw(1000,decimals[0]),1)?;
 m.finalize_amm_transition_and_observe_risk(2500)?;
 m.deposit_single_sided(MarketAsset::Quote,raw(60000,decimals[1]),1)?;
 m.finalize_amm_transition_and_observe_risk(2500)?;
 if (3..=5).contains(&kind) {
  // Controlled indexed-funding shock, without public debt: no invented cash.
  if kind==3 || kind==5 {m.debt.quote_borrow_index_nad*=3;}
  if kind==4 || kind==5 {m.debt.base_borrow_index_nad*=3;}
  m.freeze_negative_hlp_vaults()?;
 }else if kind>=1 {
  // Already-performing public principal, funded from reserve cash.
  for a in [MarketAsset::Base,MarketAsset::Quote]{
   let n=if kind==6 {m.side(a).reserves.cash_reserve - if a==MarketAsset::Base {raw(1,decimals[0])/1000}else{raw(60,decimals[1])/1000}}else{m.side(a).reserves.cash_reserve/3};
   let shares=Debt::debt_to_shares(n,m.debt.borrow_index(a))?;
   if a==MarketAsset::Base {m.debt.fixed_base_shares+=shares;m.debt.fixed_base_principal+=n;}
   else {m.debt.fixed_quote_shares+=shares;m.debt.fixed_quote_principal+=n;}
   m.side_mut(a).reserves.cash_reserve-=n;
  }
  m.accrue_interest_to_slot(2500+432000)?;
 }
 m.assert_market_invariants()?;
 Ok(m)
}
pub fn research_fixture(decimals:[u8;2],kind:u8,amp:u64)->Result<([i128;17],[u64;12])>{
 let m=research_make(decimals,kind,amp)?;
 Ok((research_read(&m,0,kind==2)?,research_meta(&m)?))
}
pub fn research_read(m:&Market,mark:u128,carry:bool)->Result<[i128;17]>{
 let mut s=if mark==0 {m.integrated_curve_state_nad()?} else {m.research_state_with_mark(mark)?};
 if carry {
  if s.frozen_hlp_mask&1==0{s.base_hlp_quote_carry=Debt::shares_to_debt(m.base_hlp_vault.debt_shares,m.debt.quote_borrow_index_nad)?.saturating_sub(m.base_hlp_vault.debt_principal as u128) as u64;}
  if s.frozen_hlp_mask&2==0{s.quote_hlp_base_carry=Debt::shares_to_debt(m.quote_hlp_vault.debt_shares,m.debt.base_borrow_index_nad)?.saturating_sub(m.quote_hlp_vault.debt_principal as u128) as u64;}
 }
 Ok(research_state_to_array(s))
}
pub fn research_meta(m:&Market)->Result<[u64;12]>{
 Ok([m.base_side.shares.ylp_supply,m.base_hlp_vault.ylp_shares,m.quote_hlp_vault.ylp_shares,
 m.base_side.reserves.cash_reserve,m.quote_side.reserves.cash_reserve,
 Debt::shares_to_debt(m.base_hlp_vault.debt_shares,m.debt.quote_borrow_index_nad)? as u64,
 Debt::shares_to_debt(m.quote_hlp_vault.debt_shares,m.debt.base_borrow_index_nad)? as u64,
 m.base_hlp_vault.debt_principal,m.quote_hlp_vault.debt_principal,m.amm.frozen_hlp_mask as u64,
 m.debt.base_borrow_index_nad as u64,m.debt.quote_borrow_index_nad as u64])
}
pub fn research_point(m:&Market)->Result<[u128;2]>{
 let p=m.ordinary_curve_point_nad()?;Ok([p.base_reserve,p.quote_reserve])
}
pub fn research_books(m:&Market)->Result<[i128;14]>{
 Ok([m.base_side.reserves.live_reserve as i128,m.quote_side.reserves.live_reserve as i128,
 m.base_side.fees.interest_vault_balance as i128,m.quote_side.fees.interest_vault_balance as i128,
 m.unrealized_interest(MarketAsset::Base)? as i128,m.unrealized_interest(MarketAsset::Quote)? as i128,
 m.base_hlp_vault.debt_shares as i128,m.quote_hlp_vault.debt_shares as i128,
 m.base_hlp_vault.quote_hlp_live_reserve as i128,m.quote_hlp_vault.base_hlp_live_reserve as i128,
 m.debt.fixed_base_debt()? as i128,m.debt.fixed_quote_debt()? as i128,
 m.frozen_curve_offsets()?.0,m.frozen_curve_offsets()?.1])
}
pub fn research_fork(m:&Market)->Result<Market>{clone_market(m)}
pub fn research_settle(m:&Market,v:[i128;17],direction:u8,input:u64,out:u64)->Result<Market>{
 use crate::transitions::liquidity::research_prepare_end;
 let mut n=clone_market(m)?;
 let plan=research_prepare_end(&n,research_array_to_state(v))?;
 let eligibility=HlpYieldEligibility{ylp_supply:n.base_side.shares.ylp_supply,
 base_hlp_ylp_shares:n.base_hlp_vault.ylp_shares,quote_hlp_ylp_shares:n.quote_hlp_vault.ylp_shares};
 let a=if direction==0{MarketAsset::Base}else{MarketAsset::Quote};
 n.side_mut(a).credit_reserve(input,true)?;
 n.side_mut(a.opposite()).debit_reserve(out,true)?;
 let (br,qr)=plan.consume(&mut n)?;
 for r in [br,qr]{if r.interest_paid>0{
 record_inline_hlp_interest_credit(&mut n,r.target_asset.opposite(),r.interest_paid,0,ProtocolAuctionSplit::default(),eligibility)?;
 }}
 // Preserve center/shape: no time advance or parameter/recenter transition in a swap.
 n.finalize_amm_transition(2500)?;
 n.assert_market_invariants()?;
 Ok(n)
}
pub fn research_advance(m:&Market,slot:u64)->Result<Market>{
 let mut n=clone_market(m)?;n.accrue_interest_to_slot(slot)?;n.freeze_negative_hlp_vaults()?;
 n.finalize_amm_transition(slot)?;n.assert_market_invariants()?;Ok(n)
}
pub fn research_recover(m:&Market,side:u8)->Result<(Market,[u64;3])>{
 let mut n=clone_market(m)?;
 let a=if side==0{MarketAsset::Base}else{MarketAsset::Quote};
 let q=n.quote_hlp_flash_recovery(a,LeverageCollateralFee::default(),LeverageCollateralFee::default())?;
 require!(!q.full,ErrorCode::HlpSettlementUnavailable);
 let eligibility=HlpYieldEligibility{ylp_supply:n.base_side.shares.ylp_supply,
 base_hlp_ylp_shares:n.base_hlp_vault.ylp_shares,quote_hlp_ylp_shares:n.quote_hlp_vault.ylp_shares};
 n.begin_hlp_flash_recovery(q)?;
 let (br,qr)=n.settle_hlp_flash_recovery(q,2500)?;
 for r in [br,qr]{if r.interest_paid>0{record_inline_hlp_interest_credit(&mut n,r.target_asset.opposite(),r.interest_paid,0,ProtocolAuctionSplit::default(),eligibility)?;}}
 n.assert_market_invariants()?;
 Ok((n,[q.payment,q.collateral_credit,q.discount_bps as u64]))
}
pub fn research_native(m:&Market,direction:u8,input:u64)->Result<(Market,u64)>{
 let slot=m.debt.base_last_accrual_slot.max(m.debt.quote_last_accrual_slot).max(m.last_update_slot).max(m.amm.last_observation_slot).max(m.risk.last_snapshot_slot);
 let clock=BenchmarkClock{slot,unix_timestamp:(slot*400/1000) as i64};
 let mut bm=BenchmarkMarket::from_market_state_at(Pubkey::new_unique(),clone_market(m)?,clock)?;
 let owner=Pubkey::new_unique();
 let mut b=BenchmarkHlpOwnedState::initialize(&bm,MarketAsset::Base,owner,owner)?;
 let mut q=BenchmarkHlpOwnedState::initialize(&bm,MarketAsset::Quote,owner,owner)?;
 let request=BenchmarkSwapRequest{asset_in:if direction==0{MarketAsset::Base}else{MarketAsset::Quote},reserve_credit:input,
 protocol_fee_bps:0,protocol_auction_split:ProtocolAuctionSplit::default()};
 let preview=bm.preview_swap(request)?;
 let tr=|n|BenchmarkTokenTransferOutcome{source_debit:n,destination_credit:n};
 let ex=bm.execute_swap_with_hlp(&mut b,&mut q,BenchmarkHlpAwareSwapRequest{
 swap:request,base_hlp_interest_transfer:tr(preview.base_rebalance.interest_paid),
 quote_hlp_interest_transfer:tr(preview.quote_rebalance.interest_paid),protocol_interest_fee_bps:0,
 protocol_auction_split:ProtocolAuctionSplit::default()})?;
 let n=bm.into_market();n.assert_market_invariants()?;Ok((n,ex.swap.quote.amount_out))
}
// Research switches. False by default; these are never applied to the workspace program.
pub static RESEARCH_FIX_FROZEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static RESEARCH_FIX_CARRY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn research_fixes(frozen:bool,carry:bool){
 RESEARCH_FIX_FROZEN.store(frozen,std::sync::atomic::Ordering::Relaxed);
 RESEARCH_FIX_CARRY.store(carry,std::sync::atomic::Ordering::Relaxed);
}

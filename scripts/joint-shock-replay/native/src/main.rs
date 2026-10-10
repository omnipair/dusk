use anchor_lang::prelude::Pubkey;
use dusk::{benchmark_api::*, constants::*, state::*};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io::Write};

type R<T> = Result<T, String>;
const P0: f64 = 200.;
const CAPITAL: f64 = 1_000_000.;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn clock(seconds: i64) -> BenchmarkClock {
    BenchmarkClock {
        slot: (seconds as u64) * 5 / 2,
        unix_timestamp: seconds,
    }
}
fn units(asset: MarketAsset) -> f64 {
    if asset == MarketAsset::Base {
        1e9
    } else {
        1e6
    }
}
fn value(amount: u128, asset: MarketAsset, p: f64) -> f64 {
    amount as f64 / units(asset) * if asset == MarketAsset::Base { p } else { 1. }
}
fn atoms(dollars: f64, asset: MarketAsset, p: f64) -> u64 {
    (dollars / if asset == MarketAsset::Base { p } else { 1. } * units(asset)).floor() as u64
}
fn transfer(x: u64) -> BenchmarkTokenTransferOutcome {
    BenchmarkTokenTransferOutcome {
        source_debit: x,
        destination_credit: x,
    }
}
fn f(v: &Value, k: &str, d: f64) -> f64 {
    v[k].as_f64().unwrap_or(d)
}
fn text<'a>(v: &'a Value, k: &str, d: &'a str) -> &'a str {
    v[k].as_str().unwrap_or(d)
}

struct Borrower {
    position: BenchmarkBorrowPosition,
    proceeds: u64,
    residual: u64,
    eligible_at: Option<i64>,
}
struct Sim {
    market: BenchmarkMarket,
    base_hlp: BenchmarkHlpOwnedState,
    quote_hlp: BenchmarkHlpOwnedState,
    borrowers: Vec<Borrower>,
    debt_asset: MarketAsset,
    start: i64,
    ylp_owned: u64,
    initial_ylp: u64,
    withdrawn: [u64; 2],
    events: Vec<Value>,
    trace: Vec<Value>,
    failures: BTreeMap<String, u64>,
    arb_wallet: [i128; 2],
    bounty: [u64; 2],
    initial_book: Value,
    accounting_checks: u64,
}

fn make(c: &Value) -> R<Sim> {
    let h = f(c, "hlp_each_usd", 100_000.);
    let ordinary = CAPITAL - 2. * h;
    let owner = Pubkey::new_unique();
    let amp = f(c, "amplification", 5.) as u64;
    let dynamic = text(c, "fee_profile", "mild") == "mild";
    let config = MarketConfig {
        swap_fee_bps: 3,
        divergence_fee_share_cap_bps: if dynamic { 20 } else { 0 },
        volatility_fee_share_cap_bps: if dynamic { 20 } else { 0 },
        target_hlp_leverage_bps: 20_000,
        settlement_divergence_bps: 10_000,
        ema_half_life_ms: 60_000,
        directional_ema_half_life_ms: 60_000,
        curve_depth_ema_half_life_ms: 60_000,
        max_daily_borrow_bps: DEFAULT_DAILY_BORROW_BPS,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 11_000,
        amm: AmmConfig {
            peak_amplification_nad: amp * NAD,
            core_half_width_bps: if amp > 1 { 100 } else { 0 },
            fade_width_bps: if amp > 1 { 400 } else { 0 },
            center_ema_half_life_ms: 60_000,
            volatility_half_life_ms: 60_000,
            adjustment_threshold_nad: NAD / 100,
            adjustment_step_nad: NAD / 1000,
            min_adjustment_interval_slots: 150,
            volatility_shock_cap_nad: NAD / 20,
            volatility_cap_nad: NAD / 10,
            divergence_fee_coefficient_nad: if dynamic { NAD / 10 } else { 0 },
            volatility_fee_coefficient_nad: if dynamic { NAD / 10 } else { 0 },
            ..AmmConfig::default()
        },
        irm: IrmConfig::default(),
        start_time: 0,
    };
    let side = |decimals| MarketSide {
        asset_mint: Pubkey::new_unique(),
        hlp_mint: Pubkey::new_unique(),
        reserve_vault: Pubkey::new_unique(),
        collateral_vault: Pubkey::new_unique(),
        interest_vault: Pubkey::new_unique(),
        asset_decimals: decimals,
        ..MarketSide::default()
    };
    let mut market = BenchmarkMarket::initialize_at(
        Pubkey::new_unique(),
        BenchmarkMarketInit {
            ylp_mint: Pubkey::new_unique(),
            base_side: side(9),
            quote_side: side(6),
            config,
            base_hlp_ylp_vault: Pubkey::new_unique(),
            quote_hlp_ylp_vault: Pubkey::new_unique(),
            base_insurance_vault: Pubkey::new_unique(),
            quote_insurance_vault: Pubkey::new_unique(),
            params_hash: [0; 32],
            initial_liquidity_authority: owner,
            bootstrap_price_nad: (P0 * NAD as f64) as u64,
            launch_fee_progress_offset: 0,
            bump: 255,
        },
        clock(1000),
    )
    .map_err(err)?;
    let ylp = market
        .execute_add_ylp(BenchmarkAddYlpRequest {
            clock: clock(1000),
            owner,
            base_reserve_credit: atoms(ordinary / 2., MarketAsset::Base, P0),
            quote_reserve_credit: atoms(ordinary / 2., MarketAsset::Quote, P0),
            min_ylp_amount: 1,
            global_reduce_only: false,
        })
        .map_err(err)?
        .receipt
        .ylp_amount;
    let mut base_hlp =
        BenchmarkHlpOwnedState::initialize(&market, MarketAsset::Base, Pubkey::new_unique(), owner)
            .map_err(err)?;
    let mut quote_hlp = BenchmarkHlpOwnedState::initialize(
        &market,
        MarketAsset::Quote,
        Pubkey::new_unique(),
        owner,
    )
    .map_err(err)?;
    if h > 0. {
        market
            .execute_hlp_entry(
                &mut base_hlp,
                BenchmarkHlpEntryRequest {
                    clock: clock(1000),
                    target_transfer: transfer(atoms(h, MarketAsset::Base, P0)),
                    min_hlp_amount: 1,
                    global_reduce_only: false,
                },
            )
            .map_err(err)?;
        market
            .execute_hlp_entry(
                &mut quote_hlp,
                BenchmarkHlpEntryRequest {
                    clock: clock(1000),
                    target_transfer: transfer(atoms(h, MarketAsset::Quote, P0)),
                    min_hlp_amount: 1,
                    global_reduce_only: false,
                },
            )
            .map_err(err)?;
    }
    let debt_asset = if text(c, "debt_asset", "quote") == "base" {
        MarketAsset::Base
    } else {
        MarketAsset::Quote
    };
    let mut borrowers = vec![];
    let mut now = 1000;
    let n = f(c, "borrowers", 10.) as usize;
    let principal = f(c, "borrow_usd", 200_000.);
    for _ in 0..n {
        if principal == 0. {
            break;
        }
        let amount = atoms(principal / n as f64, debt_asset, P0);
        let mut collateral = atoms(
            principal / n as f64 / f(c, "opening_ltv", 0.65),
            debt_asset.opposite(),
            P0,
        );
        let mut position = BenchmarkBorrowPosition::initialize(
            Pubkey::new_unique(),
            market.market_key().unwrap(),
            Pubkey::new_unique(),
            255,
        )
        .map_err(err)?;
        // Add enough external collateral to satisfy real native underwriting;
        // never inject debt or override a capacity guard.
        let desired_capacity = amount;
        let admissible = |coll: u64| -> R<bool> {
            let mut test_market = market.try_fork().map_err(err)?;
            let mut test_pos = position.try_fork().map_err(err)?;
            test_market
                .execute_deposit_collateral(
                    &mut test_pos,
                    BenchmarkDepositCollateralRequest {
                        clock: clock(now),
                        collateral_asset: debt_asset.opposite(),
                        collateral_credit: coll,
                    },
                )
                .map_err(err)?;
            let cap = test_market
                .preview_existing_borrow_capacity(&test_pos, debt_asset, clock(now), false)
                .map_err(err)?;
            Ok(cap
                .underwriting_max_additional
                .min(cap.global_health_floor_max_additional)
                >= desired_capacity)
        };
        if !admissible(collateral)? {
            let mut lo = collateral;
            let mut hi = collateral;
            for _ in 0..12 {
                hi = hi.checked_mul(2).ok_or("collateral overflow")?;
                if admissible(hi)? {
                    break;
                }
            }
            if !admissible(hi)? {
                let mut tm = market.try_fork().map_err(err)?;
                let mut tp = position.try_fork().map_err(err)?;
                tm.execute_deposit_collateral(
                    &mut tp,
                    BenchmarkDepositCollateralRequest {
                        clock: clock(now),
                        collateral_asset: debt_asset.opposite(),
                        collateral_credit: hi,
                    },
                )
                .map_err(err)?;
                return Err(format!(
                    "setup cannot underwrite borrower {} coll {} at {}: {:?}",
                    borrowers.len(),
                    hi,
                    now,
                    tm.preview_existing_borrow_capacity(&tp, debt_asset, clock(now), false)
                        .map_err(err)?
                ));
            }
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if admissible(mid)? {
                    hi = mid
                } else {
                    lo = mid
                }
            }
            collateral = hi;
        }
        market
            .execute_deposit_collateral(
                &mut position,
                BenchmarkDepositCollateralRequest {
                    clock: clock(now),
                    collateral_asset: debt_asset.opposite(),
                    collateral_credit: collateral,
                },
            )
            .map_err(err)?;
        let mut capacity = market
            .preview_existing_borrow_capacity(&position, debt_asset, clock(now), false)
            .map_err(err)?;
        let mut days = 0;
        while capacity.actionable_max_additional < amount
            && days < 8
            && capacity
                .limiting_constraints
                .contains(&BenchmarkBorrowCapacityConstraint::DailyBorrowBucket)
        {
            now += 86_400;
            days += 1;
            market.advance_to(clock(now)).map_err(err)?;
            capacity = market
                .preview_existing_borrow_capacity(&position, debt_asset, clock(now), false)
                .map_err(err)?;
        }
        if capacity.actionable_max_additional < amount {
            return Err(format!(
                "setup borrow rejected: wanted {amount}, capacity {} {:?}",
                capacity.actionable_max_additional, capacity.limiting_constraints
            ));
        }
        market
            .execute_borrow(
                &mut position,
                BenchmarkBorrowRequest {
                    clock: clock(now),
                    debt_asset,
                    borrow_amount: amount,
                    recipient_credit: amount,
                    min_recipient_credit: amount,
                    min_liquidation_cf_bps: 0,
                    global_reduce_only: false,
                    referral_partner: Pubkey::default(),
                    referral_interest_share_bps: 0,
                    referral_interest_share_cap_bps: 0,
                },
            )
            .map_err(err)?;
        borrowers.push(Borrower {
            position,
            proceeds: amount,
            residual: 0,
            eligible_at: None,
        });
    }
    let insurance = c["insurance_atoms"]
        .as_u64()
        .unwrap_or_else(|| atoms(f(c, "insurance_usd", 0.), debt_asset, P0));
    if insurance > 0 {
        market
            .market_mut()
            .insurance
            .credit(debt_asset, insurance, clock(now).slot)
            .map_err(err)?;
    }
    let mut s = Sim {
        market,
        base_hlp,
        quote_hlp,
        borrowers,
        debt_asset,
        start: now,
        ylp_owned: ylp,
        initial_ylp: ylp,
        withdrawn: [0; 2],
        events: vec![],
        trace: vec![],
        failures: BTreeMap::new(),
        arb_wallet: [0; 2],
        bounty: [0; 2],
        initial_book: Value::Null,
        accounting_checks: 0,
    };
    s.initial_book = s.book(P0)?;
    Ok(s)
}

impl Sim {
    fn book(&self, p: f64) -> R<Value> {
        let m = self.market.market();
        let supply = m.base_side.shares.ylp_supply as f64;
        let live = value(
            m.base_side.reserves.live_reserve as u128,
            MarketAsset::Base,
            p,
        ) + value(
            m.quote_side.reserves.live_reserve as u128,
            MarketAsset::Quote,
            p,
        );
        let raw_hlp = |v: &HlpVault, debt: MarketAsset| -> R<f64> {
            Ok(v.ylp_shares as f64 / supply * live
                - value(
                    Debt::shares_to_debt(v.debt_shares, m.debt.borrow_index(debt)).map_err(err)?,
                    debt,
                    p,
                ))
        };
        let bfees = self.market.stress_hlp_yield(&self.base_hlp).map_err(err)?;
        let qfees = self.market.stress_hlp_yield(&self.quote_hlp).map_err(err)?;
        let fee_value = |a: [u64; 2]| {
            value(a[0] as u128, MarketAsset::Base, p) + value(a[1] as u128, MarketAsset::Quote, p)
        };
        let rev = self.market.revenue_checkpoint();
        let total_lp_fee = fee_value([
            rev.base.lp_swap_fee_liability + rev.base.lp_interest_liability,
            rev.quote.lp_swap_fee_liability + rev.quote.lp_interest_liability,
        ]);
        let ordinary_principal =
            (supply - m.base_hlp_vault.ylp_shares as f64 - m.quote_hlp_vault.ylp_shares as f64)
                / supply
                * live;
        let mut borrowers = 0.;
        let mut remaining = 0.;
        let mut collateral = 0.;
        for b in &self.borrowers {
            let c = b.position.checkpoint(&self.market).map_err(err)?;
            let debt = if self.debt_asset == MarketAsset::Quote {
                c.fixed_quote_debt
            } else {
                c.fixed_base_debt
            };
            remaining += value(debt, self.debt_asset, p);
            let coll = value(c.base_collateral as u128, MarketAsset::Base, p)
                + value(c.quote_collateral as u128, MarketAsset::Quote, p);
            collateral += coll;
            borrowers += coll + value((b.proceeds + b.residual) as u128, self.debt_asset, p)
                - value(debt, self.debt_asset, p);
        }
        Ok(
            json!({"ylp_supply":m.base_side.shares.ylp_supply,"base_hlp_ylp_shares":m.base_hlp_vault.ylp_shares,"quote_hlp_ylp_shares":m.quote_hlp_vault.ylp_shares,"ordinary_principal_usd":ordinary_principal,"ordinary_total_usd":ordinary_principal+total_lp_fee-fee_value(bfees)-fee_value(qfees)+fee_value(self.withdrawn),
            "base_hlp_principal_usd":raw_hlp(&m.base_hlp_vault,MarketAsset::Quote)?,"quote_hlp_principal_usd":raw_hlp(&m.quote_hlp_vault,MarketAsset::Base)?,
            "base_hlp_total_usd":raw_hlp(&m.base_hlp_vault,MarketAsset::Quote)?+fee_value(bfees),"quote_hlp_total_usd":raw_hlp(&m.quote_hlp_vault,MarketAsset::Base)?+fee_value(qfees),
            "lp_fees_usd":total_lp_fee,"borrower_wealth_usd":borrowers,"remaining_debt_usd":remaining,"remaining_collateral_usd":collateral,
            "cash_base":m.base_side.reserves.cash_reserve,"cash_quote":m.quote_side.reserves.cash_reserve,
            "insurance_base":m.insurance.base_available,"insurance_quote":m.insurance.quote_available,
            "quote_utilization_bps":m.lending_utilization_bps(MarketAsset::Quote).map_err(err)?,"base_utilization_bps":m.lending_utilization_bps(MarketAsset::Base).map_err(err)?,
            "spot":self.market.curve_snapshot().map_err(err)?.spot_price_nad as f64/NAD as f64,
            "ema_base":m.risk.base_price_ema_nad as f64/NAD as f64}),
        )
    }
    fn fail(&mut self, kind: &str, e: String) {
        *self.failures.entry(format!("{kind}: {e}")).or_insert(0) += 1;
    }
    fn swap(&mut self, asset: MarketAsset, amount: u64) -> R<f64> {
        let req = BenchmarkSwapRequest {
            asset_in: asset,
            reserve_credit: amount,
            protocol_fee_bps: 2000,
            protocol_auction_split: ProtocolAuctionSplit::default(),
        };
        let preview = self.market.preview_swap(req).map_err(err)?;
        let result = self
            .market
            .execute_swap_with_hlp(
                &mut self.base_hlp,
                &mut self.quote_hlp,
                BenchmarkHlpAwareSwapRequest {
                    swap: req,
                    base_hlp_interest_transfer: transfer(preview.base_rebalance.interest_paid),
                    quote_hlp_interest_transfer: transfer(preview.quote_rebalance.interest_paid),
                    protocol_interest_fee_bps: 2000,
                    protocol_auction_split: ProtocolAuctionSplit::default(),
                },
            )
            .map_err(err)?;
        if preview != result.swap {
            return Err("preview/execution mismatch".into());
        }
        self.accounting_checks += 1;
        let i = if asset == MarketAsset::Base { 0 } else { 1 };
        self.arb_wallet[i] -= amount as i128;
        self.arb_wallet[1 - i] += result.swap.quote.amount_out as i128;
        Ok(result.swap.quote.amount_out as f64)
    }
    fn arb(&mut self, p: f64) -> R<bool> {
        let spot = self.market.curve_snapshot().map_err(err)?.spot_price_nad as f64 / NAD as f64;
        if (spot / p - 1.).abs() < 0.00030 {
            return Ok(false);
        }
        let asset = if spot > p {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let max = atoms(CAPITAL * 5., asset, p);
        let quote = |a: u64| -> f64 {
            if a == 0 {
                return 0.;
            }
            match self.market.preview_swap(BenchmarkSwapRequest {
                asset_in: asset,
                reserve_credit: a,
                protocol_fee_bps: 2000,
                protocol_auction_split: ProtocolAuctionSplit::default(),
            }) {
                Ok(q) => {
                    value(q.quote.amount_out as u128, asset.opposite(), p)
                        - value(a as u128, asset, p)
                }
                Err(_) => f64::NEG_INFINITY,
            }
        };
        let mut grid = vec![0];
        for i in 0..45 {
            grid.push(((max as f64) * (0.75f64).powi(44 - i)).max(1.) as u64);
        }
        let mut best = 0;
        let mut profit = 0.;
        for (i, &a) in grid.iter().enumerate() {
            let v = quote(a);
            if v > profit {
                best = i;
                profit = v;
            }
        }
        if best == 0 {
            let mut valid = 0;
            let mut example = None;
            for usd in [1., 100., 1000., 10000., 100000.] {
                let req = BenchmarkSwapRequest {
                    asset_in: asset,
                    reserve_credit: atoms(usd, asset, p),
                    protocol_fee_bps: 2000,
                    protocol_auction_split: ProtocolAuctionSplit::default(),
                };
                match self.market.preview_swap(req) {
                    Ok(_) => valid += 1,
                    Err(e) => {
                        example = Some(err(e));
                    }
                }
            }
            if valid == 0 {
                if let Some(e) = example {
                    self.fail("arb_unavailable", e);
                }
            }
            return Ok(false);
        }
        let mut lo = grid[best.saturating_sub(1)];
        let mut hi = grid[(best + 1).min(grid.len() - 1)];
        let mut amount = grid[best];
        for _ in 0..40 {
            if hi - lo < 3 {
                break;
            }
            let a = lo + (hi - lo) / 3;
            let b = hi - (hi - lo) / 3;
            let va = quote(a);
            let vb = quote(b);
            if va > profit {
                profit = va;
                amount = a;
            }
            if vb > profit {
                profit = vb;
                amount = b;
            }
            if va < vb {
                lo = a;
            } else {
                hi = b;
            }
        }
        if profit < 0.01 {
            return Ok(false);
        }
        self.swap(asset, amount)?;
        Ok(true)
    }
    fn withdraw(&mut self, fraction: f64, t: i64) -> R<()> {
        if fraction <= 0. {
            return Ok(());
        }
        let wanted = (self.initial_ylp as f64 * fraction).floor() as u64;
        let try_amount = |a: u64| -> R<BenchmarkMarketExecution<BenchmarkYlpLiquidityReceipt>> {
            let mut fork = self.market.try_fork().map_err(err)?;
            let m = fork.market();
            let supply = m.base_side.shares.ylp_supply as u128;
            let base = (m.base_side.reserves.live_reserve as u128 * a as u128 / supply) as u64;
            let quote = (m.quote_side.reserves.live_reserve as u128 * a as u128 / supply) as u64;
            fork.execute_remove_ylp(BenchmarkRemoveYlpRequest {
                clock: clock(t),
                ylp_amount: a,
                owner_ylp_balance_before: self.ylp_owned,
                base_recipient_credit: base,
                quote_recipient_credit: quote,
                min_base_recipient_credit: base,
                min_quote_recipient_credit: quote,
            })
            .map_err(err)
        };
        let full = try_amount(wanted);
        let mut filled = wanted;
        let full_error = full.as_ref().err().cloned();
        if full.is_err() {
            let mut lo = 0;
            let mut hi = wanted;
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if try_amount(mid).is_ok() {
                    lo = mid
                } else {
                    hi = mid
                }
            }
            filled = lo;
        }
        if filled > 0 {
            let preview = try_amount(filled)?;
            let rec = self
                .market
                .execute_remove_ylp(BenchmarkRemoveYlpRequest {
                    clock: clock(t),
                    ylp_amount: filled,
                    owner_ylp_balance_before: self.ylp_owned,
                    base_recipient_credit: preview.receipt.base_reserve_amount,
                    quote_recipient_credit: preview.receipt.quote_reserve_amount,
                    min_base_recipient_credit: 0,
                    min_quote_recipient_credit: 0,
                })
                .map_err(err)?;
            self.ylp_owned -= filled;
            self.withdrawn[0] += rec.receipt.base_reserve_amount;
            self.withdrawn[1] += rec.receipt.quote_reserve_amount;
        }
        self.events.push(json!({"kind":"withdrawal","t":t-self.start,"requested_fraction":fraction,"filled_fraction":filled as f64/self.initial_ylp as f64,"blocked_fraction":(wanted-filled) as f64/self.initial_ylp as f64,"full_error":full_error}));
        Ok(())
    }
    fn liquidations(&mut self, c: &Value, t: i64, p: f64) -> R<()> {
        let delay = f(c, "keeper_delay_s", 0.) as i64;
        for i in 0..self.borrowers.len() {
            let cp = self.borrowers[i]
                .position
                .checkpoint(&self.market)
                .map_err(err)?;
            let debt = if self.debt_asset == MarketAsset::Quote {
                cp.fixed_quote_debt
            } else {
                cp.fixed_base_debt
            };
            if debt == 0 {
                continue;
            }
            let eligible = self
                .market
                .market()
                .is_position_liquidatable(self.borrowers[i].position.position(), self.debt_asset)
                .map_err(err)?;
            if !eligible {
                continue;
            }
            let first = *self.borrowers[i].eligible_at.get_or_insert(t);
            if t - first < delay {
                continue;
            }
            if !self.borrowers[i]
                .position
                .position()
                .has_active_liquidation_auction()
            {
                match self.market.execute_start_liquidation_auction(&mut self.borrowers[i].position,BenchmarkStartLiquidationAuctionRequest{clock:clock(t),debt_asset:self.debt_asset}){
                    Ok(r)=>self.events.push(json!({"kind":"auction_start","borrower":i,"t":t-self.start,"floor":r.market.receipt.floor_price_nad})),
                    Err(e)=>{self.fail("auction",err(e));continue;}}
            }
            if t < self.borrowers[i]
                .position
                .first_liquidation_floor_unix_timestamp()
                .map_err(err)?
            {
                continue;
            }
            let before = self.borrowers[i]
                .position
                .checkpoint(&self.market)
                .map_err(err)?;
            let coll = if self.debt_asset == MarketAsset::Quote {
                before.base_collateral
            } else {
                before.quote_collateral
            };
            let bounty = (coll as u128 * LIQUIDATION_BACKSTOP_CALLER_BPS as u128 / 10_000) as u64;
            let req = BenchmarkLiquidationPlanRequest {
                clock: clock(t),
                debt_asset: self.debt_asset,
                phase: BenchmarkLiquidationPhase::Floor,
                max_repay_credit: 0,
                collateral_reserve_credit: coll - bounty,
                protocol_swap_fee_bps: 2000,
                protocol_auction_split: ProtocolAuctionSplit::default(),
            };
            match self.market.stress_floor_with_hlp(
                &mut self.borrowers[i].position,
                &mut self.base_hlp,
                &mut self.quote_hlp,
                req,
                2000,
            ) {
                Ok(r) => {
                    self.accounting_checks += 2;
                    let x = r.preview;
                    self.borrowers[i].residual += x.owner_residual;
                    let j = if self.debt_asset == MarketAsset::Quote {
                        0
                    } else {
                        1
                    };
                    self.bounty[j] += x.plan.caller_bounty;
                    let loss = x.native.socialized_loss;
                    let bshare = r.pre_loss_hlp_shares[0] as f64 / r.pre_loss_supply as f64;
                    let qshare = r.pre_loss_hlp_shares[1] as f64 / r.pre_loss_supply as f64;
                    self.events.push(json!({"kind":"floor","borrower":i,"t":t-self.start,"external_price":p,"debt_atoms":x.plan.terms.max_repay_amount,"swap_output_atoms":x.plan.swap_output,
                        "insurance_atoms":x.native.insurance_drawn,"socialized_atoms":loss,"shortfall_atoms":x.native.insurance_drawn+loss,
                        "ordinary_credit_loss_usd":value(loss as u128,self.debt_asset,p)*(1.-bshare-qshare),"base_hlp_credit_loss_usd":value(loss as u128,self.debt_asset,p)*bshare,
                        "quote_hlp_credit_loss_usd":value(loss as u128,self.debt_asset,p)*qshare,"owner_residual_atoms":x.owner_residual,
                        "bounty_atoms":x.plan.caller_bounty,"interest_paid_atoms":x.native.interest_paid}));
                }
                Err(e) => self.fail("floor", err(e)),
            }
        }
        Ok(())
    }
}

fn run(c: &Value) -> R<Value> {
    let mut s = make(c)?;
    let initial = s.initial_book.clone();
    let initial_positions:Vec<_>=s.borrowers.iter().map(|b|{let q=b.position.checkpoint(&s.market).unwrap();json!({"base_collateral":q.base_collateral,"quote_collateral":q.quote_collateral,"base_debt":q.fixed_base_debt.to_string(),"quote_debt":q.fixed_quote_debt.to_string(),"cf_base":q.base_liquidation_cf_bps,"cf_quote":q.quote_liquidation_cf_bps})}).collect();
    let horizon = f(c, "horizon_s", 3600.) as i64;
    let step = f(c, "step_s", 30.) as i64;
    let shock = f(c, "shock", -0.5);
    let mut last_p = P0;
    for elapsed in (0..=horizon).step_by(step as usize) {
        let t = s.start + elapsed;
        let extra = f(c, "continuation", 0.) * (elapsed as f64 / 900.).min(1.);
        let p = P0 * (1. + shock) * (1. + extra);
        last_p = p;
        s.market
            .advance_to(clock(t))
            .map_err(|e| format!("advance t={elapsed}: {e}"))?;
        if elapsed == f(c, "withdraw_at_s", 60.) as i64 {
            s.withdraw(f(c, "withdraw_fraction", 0.), t)?;
        }
        // Fresh external prices determine arbitrage demand, never native risk books.
        if let Err(e) = s.arb(p) {
            s.fail("arb_execute", e);
        }
        s.liquidations(c, t, p)?;
        if elapsed % 120 == 0 || elapsed == horizon {
            s.trace
                .push(json!({"t":elapsed,"external_price":p,"book":s.book(p)?}));
        }
    }
    let final_book = s.book(last_p)?;
    let final_positions:Vec<_>=s.borrowers.iter().map(|b|{let q=b.position.checkpoint(&s.market).unwrap();json!({"base_collateral":q.base_collateral,"quote_collateral":q.quote_collateral,"base_debt":q.fixed_base_debt.to_string(),"quote_debt":q.fixed_quote_debt.to_string()})}).collect();
    let floors: Vec<_> = s.events.iter().filter(|x| x["kind"] == "floor").collect();
    let sum = |k: &str| -> u64 { floors.iter().map(|x| x[k].as_u64().unwrap()).sum() };
    let sumf = |k: &str| -> f64 { floors.iter().map(|x| x[k].as_f64().unwrap()).sum() };
    let mut running = 0u128;
    let mut needed = 0u128;
    for event in &floors {
        let gap = event["shortfall_atoms"].as_u64().unwrap() as u128;
        needed =
            needed.max(running + (gap * 10_000).div_ceil(MAX_INSURANCE_DRAW_PER_EVENT_BPS as u128));
        running += gap;
    }
    needed = needed.max((running * 10_000).div_ceil(MAX_INSURANCE_DRAW_PER_DAY_BPS as u128));
    let h = f(c, "hlp_each_usd", 100_000.);
    let ordinary = CAPITAL - 2. * h;
    let ordinary_hold = ordinary / 2. * (1. + last_p / P0);
    let pnl = |key: &str| final_book[key].as_f64().unwrap() - initial[key].as_f64().unwrap();
    let arb = s.arb_wallet[0] as f64 / 1e9 * last_p + s.arb_wallet[1] as f64 / 1e6;
    Ok(
        json!({"id":c["id"],"config":c,"status":if s.failures.is_empty(){"complete"}else{"execution_blocked"},"initial":initial,"initial_positions":initial_positions,"final_positions":final_positions,"final":final_book,"price_final":last_p,
        "losses":{"socialized_atoms":sum("socialized_atoms"),"insurance_drawn_atoms":sum("insurance_atoms"),"shortfall_atoms":sum("shortfall_atoms"),
        "socialized_usd":value(sum("socialized_atoms") as u128,s.debt_asset,last_p),"insurance_drawn_usd":value(sum("insurance_atoms") as u128,s.debt_asset,last_p),
        "ordinary_credit_loss_usd":sumf("ordinary_credit_loss_usd"),"base_hlp_credit_loss_usd":sumf("base_hlp_credit_loss_usd"),"quote_hlp_credit_loss_usd":sumf("quote_hlp_credit_loss_usd")},
        "pnl":{"ordinary_usd":pnl("ordinary_total_usd"),"base_hlp_usd":pnl("base_hlp_total_usd"),"quote_hlp_usd":pnl("quote_hlp_total_usd"),"borrower_usd":pnl("borrower_wealth_usd"),
            "ordinary_vs_hold_usd":final_book["ordinary_total_usd"].as_f64().unwrap()-ordinary_hold,"base_hlp_vs_hold_usd":final_book["base_hlp_total_usd"].as_f64().unwrap()-h*last_p/P0,
            "quote_hlp_vs_hold_usd":final_book["quote_hlp_total_usd"].as_f64().unwrap()-h,"arbitrage_marked_profit_usd":arb},
        "path_implied_cap_reserve_atoms":needed.to_string(),"liquidated_positions":floors.len(),"events":s.events,"trace":s.trace,"failures":s.failures,"accounting_checks":s.accounting_checks,
        "withdrawn_base_atoms":s.withdrawn[0],"withdrawn_quote_atoms":s.withdrawn[1]}),
    )
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--regression-probes") {
        let mut out = Vec::new();
        for h in [25_000., 100_000.] {
            for debt in [0., 75_000.] {
                for age in [0, 30, 900, 86_400] {
                    for asset in [MarketAsset::Base, MarketAsset::Quote] {
                        let mut sim = make(&json!({"hlp_each_usd":h,"borrow_usd":debt})).unwrap();
                        sim.market.advance_to(clock(sim.start + age)).unwrap();
                        let gap = sim.market.stress_start_ledger_gap().unwrap();
                        let before = sim.market.stress_opposite_gaps().unwrap();
                        let result = sim.swap(asset, atoms(100., asset, P0));
                        let after = sim.market.stress_opposite_gaps().unwrap();
                        out.push(json!({"hlp_each_usd":h,"borrow_usd":debt,"age_s":age,"asset_in":format!("{asset:?}"),"start_gap_nad":gap,"opposite_before_atoms":before,"opposite_after_atoms":after,"ok":result.is_ok(),"error":result.err()}));
                    }
                }
            }
        }
        fs::write(&args[2], serde_json::to_string_pretty(&out).unwrap()).unwrap();
        return;
    }

    let input: Value = serde_json::from_str(&fs::read_to_string(&args[1]).unwrap()).unwrap();
    let mut file = fs::File::create(&args[2]).unwrap();
    for (i, c) in input.as_array().unwrap().iter().enumerate() {
        eprintln!("scenario {} {}", i, c["id"]);
        let out = match run(c) {
            Ok(x) => x,
            Err(e) => json!({"id":c["id"],"config":c,"status":"error","error":e}),
        };
        writeln!(file, "{}", out).unwrap();
        file.flush().unwrap();
    }
}

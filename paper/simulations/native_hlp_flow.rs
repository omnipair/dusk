use anchor_lang::prelude::Pubkey;
use dusk::{
    benchmark_api::*,
    constants::{MIN_HALF_LIFE_MS, NAD},
    state::*,
};
use serde_json::{json, Value};
const UNIT: u64 = 1_000_000_000;
fn snapshot(m: &BenchmarkMarket) -> Value {
    let c = m.checkpoint().unwrap();
    let supply = c.base.ylp_supply as f64;
    let price = m.curve_snapshot().unwrap().spot_price_nad as f64 / NAD as f64;
    let unit = UNIT as f64;
    let mut values = Vec::new();
    for (name, shares, debt_b, debt_q) in [
        (
            "ordinary",
            c.base.ylp_supply - c.base_hlp.ylp_shares - c.quote_hlp.ylp_shares,
            0.0,
            0.0,
        ),
        (
            "base_hlp",
            c.base_hlp.ylp_shares,
            0.0,
            Debt::shares_to_debt(c.base_hlp.debt_shares, c.quote.borrow_index_nad).unwrap() as f64
                / unit,
        ),
        (
            "quote_hlp",
            c.quote_hlp.ylp_shares,
            Debt::shares_to_debt(c.quote_hlp.debt_shares, c.base.borrow_index_nad).unwrap() as f64
                / unit,
            0.0,
        ),
    ] {
        let base = c.base.live_reserve as f64 / unit * shares as f64 / supply;
        let quote = c.quote.live_reserve as f64 / unit * shares as f64 / supply;
        values.push(json!({"name":name,"shares":shares,"base":base,"quote":quote,"debt_base":debt_b,"debt_quote":debt_q,"net_usd":(base-debt_b)*price+quote-debt_q,"gross_leverage":(base*price+quote)/((base-debt_b)*price+quote-debt_q)}));
    }
    json!({"price":price,"cash_base":c.base.cash_reserve as f64/unit,"cash_quote":c.quote.cash_reserve as f64/unit,"live_base":c.base.live_reserve as f64/unit,"live_quote":c.quote.live_reserve as f64/unit,"supply":c.base.ylp_supply,"holders":values})
}
fn run(amp: u64, fee: u16, compound: u16, with_hlp: bool) -> Value {
    let clock = BenchmarkClock {
        slot: 2500,
        unix_timestamp: 1000,
    };
    let side = || MarketSide {
        asset_mint: Pubkey::new_unique(),
        asset_decimals: 9,
        hlp_mint: Pubkey::new_unique(),
        reserve_vault: Pubkey::new_unique(),
        collateral_vault: Pubkey::new_unique(),
        interest_vault: Pubkey::new_unique(),
        ..MarketSide::default()
    };
    let owner = Pubkey::new_unique();
    let amm = AmmConfig {
        peak_amplification_nad: amp * NAD,
        core_half_width_bps: if amp == 1 { 0 } else { 100 },
        fade_width_bps: if amp == 1 { 0 } else { 400 },
        compounding_fee_bps: compound,
        ..AmmConfig::default()
    };
    let config = MarketConfig {
        swap_fee_bps: fee,
        divergence_fee_share_cap_bps: 0,
        volatility_fee_share_cap_bps: 0,
        target_hlp_leverage_bps: 20_000,
        settlement_divergence_bps: 10_000,
        ema_half_life_ms: MIN_HALF_LIFE_MS,
        directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
        curve_depth_ema_half_life_ms: MIN_HALF_LIFE_MS,
        max_daily_borrow_bps: DEFAULT_DAILY_BORROW_BPS,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 10_000,
        amm,
        irm: IrmConfig::default(),
        start_time: 0,
    };
    let mut m = BenchmarkMarket::initialize_at(
        Pubkey::new_unique(),
        BenchmarkMarketInit {
            ylp_mint: Pubkey::new_unique(),
            base_side: side(),
            quote_side: side(),
            config,
            base_hlp_ylp_vault: Pubkey::new_unique(),
            quote_hlp_ylp_vault: Pubkey::new_unique(),
            base_insurance_vault: Pubkey::new_unique(),
            quote_insurance_vault: Pubkey::new_unique(),
            params_hash: [0; 32],
            initial_liquidity_authority: owner,
            bootstrap_price_nad: NAD,
            launch_fee_progress_offset: 0,
            bump: 255,
        },
        clock,
    )
    .unwrap();
    m.execute_add_ylp(BenchmarkAddYlpRequest {
        clock,
        owner,
        base_reserve_credit: 100 * UNIT,
        quote_reserve_credit: 100 * UNIT,
        min_ylp_amount: 1,
        global_reduce_only: false,
    })
    .unwrap();
    let mut b = BenchmarkHlpOwnedState::initialize(&m, MarketAsset::Base, owner, owner).unwrap();
    let mut q = BenchmarkHlpOwnedState::initialize(&m, MarketAsset::Quote, owner, owner).unwrap();
    if with_hlp {
        for ext in [&mut b, &mut q] {
            m.execute_hlp_entry(
                ext,
                BenchmarkHlpEntryRequest {
                    clock,
                    target_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: 50 * UNIT,
                        destination_credit: 50 * UNIT,
                    },
                    min_hlp_amount: 1,
                    global_reduce_only: false,
                },
            )
            .unwrap();
        }
    }
    let before = snapshot(&m);
    // A fixed trade across configurations; no change in slot or external debt.
    let request = BenchmarkSwapRequest {
        asset_in: MarketAsset::Base,
        reserve_credit: UNIT * 200 / 9,
        protocol_fee_bps: 2_000,
        protocol_auction_split: ProtocolAuctionSplit::default(),
    };
    let preview = m.preview_swap(request).unwrap();
    let zero = BenchmarkTokenTransferOutcome {
        source_debit: 0,
        destination_credit: 0,
    };
    let result = m
        .execute_swap_with_hlp(
            &mut b,
            &mut q,
            BenchmarkHlpAwareSwapRequest {
                swap: request,
                base_hlp_interest_transfer: zero,
                quote_hlp_interest_transfer: zero,
                protocol_interest_fee_bps: 2_000,
                protocol_auction_split: ProtocolAuctionSplit::default(),
            },
        )
        .unwrap();
    assert_eq!(preview.quote, result.swap.quote);
    m.market().assert_market_invariants().unwrap();
    let after = snapshot(&m);
    let f = result.swap.quote.fee;
    let revenue = m.revenue_checkpoint();
    let p = after["price"].as_f64().unwrap();
    let delta_cash = (after["cash_base"].as_f64().unwrap() - before["cash_base"].as_f64().unwrap())
        * p
        + after["cash_quote"].as_f64().unwrap()
        - before["cash_quote"].as_f64().unwrap();
    let trader_profit = result.swap.quote.amount_out as f64 / UNIT as f64
        - request.reserve_credit as f64 / UNIT as f64 * p;
    let fee_usd = f.claimable_fee_debit as f64 / UNIT as f64 * p;
    assert!((delta_cash + trader_profit + fee_usd).abs() < 1e-6);
    json!({"amp":amp,"swap_fee_bps":fee,"compound_bps":compound,"with_hlp":with_hlp,"before":before,"after":after,"input":request.reserve_credit as f64/UNIT as f64,"output":result.swap.quote.amount_out as f64/UNIT as f64,"trader_gain_at_final_price":trader_profit,"cash_value_change_at_final_price":delta_cash,"fee":format!("{:?}",f),"base_rebalance":format!("{:?}",result.swap.base_rebalance),"quote_rebalance":format!("{:?}",result.swap.quote_rebalance),"revenue":format!("{:?}",revenue)})
}
fn main() {
    let mut rows = Vec::new();
    for amp in [1, 4] {
        for with_hlp in [false, true] {
            rows.push(run(amp, 0, 0, with_hlp));
        }
        for compound in [0, 10_000] {
            rows.push(run(amp, 30, compound, true));
        }
    }
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
}

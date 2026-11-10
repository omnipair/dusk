// Acceptance tests for the twelve-decimal amount scale. Real mint decimals and
// economic amounts run through the production swap, hLP, lending and parameter
// transitions, so every curve is rebuilt from rounded reserves as it is on
// chain. Base fees are zero, so a round trip that returns more than its input
// can only come from rounding.
use super::*;
use crate::{
    constants::{BPS_DENOMINATOR, DEFAULT_BORROW_APR_NAD, MARKET_LAYOUT_VERSION, MIN_CONCENTRATED_SWAP_FEE_BPS, MIN_HALF_LIFE_MS, NAD},
    state::{
        AmmConfig, BorrowPosition, Debt, MarketAsset, MarketConfig, MarketParameterUpdate, ProtocolAuctionSplit,
        DEFAULT_DAILY_BORROW_BPS, MAX_AMM_AMPLIFICATION_NAD, MIN_CONCENTRATION_RAMP_SLOTS,
    },
    transitions::amm::{ConcentratedCurveParameters, ConcentratedCurvePoint, SwapRequest},
};

#[derive(Clone, Copy, Debug)]
struct Pair {
    name: &'static str,
    base_decimals: u8,
    quote_decimals: u8,
    /// Whole quote tokens per whole base token.
    price: f64,
    /// Dollar value of one whole quote token.
    quote_usd: f64,
}

const fn pair(name: &'static str, base_decimals: u8, quote_decimals: u8, price: f64, quote_usd: f64) -> Pair {
    Pair { name, base_decimals, quote_decimals, price, quote_usd }
}

/// Mints at nine decimals or fewer, the range the twelve-decimal minimum is
/// meant to cover, including the zero- and two-decimal mints that receive
/// multipliers above 10^9. HV9 and MEME are the extreme-price shapes where
/// nine-decimal rounding returned the most atoms.
const PAIRS: [Pair; 9] = [
    pair("BTC/USDC", 8, 6, 100_000.0, 1.0),
    pair("SOL/USDC", 9, 6, 150.0, 1.0),
    pair("HV9/USDC", 9, 6, 100_000.0, 1.0),
    pair("MEME/SOL", 5, 9, 0.00002 / 150.0, 150.0),
    pair("SOL/BONK", 9, 5, 7_500_000.0, 0.00002),
    pair("BONK/USDC", 5, 6, 0.00002, 1.0),
    pair("USDC/USDT", 6, 6, 1.0, 1.0),
    pair("TICKET/USDC", 0, 6, 10.0, 1.0),
    pair("EUR2/USDC", 2, 6, 1.1, 1.0),
];

/// Mints whose own decimals already reach the minimum, so their scale is
/// unchanged by it.
const HIGH_DECIMAL_PAIRS: [Pair; 4] = [
    pair("WETH12/USDC", 12, 6, 3_000.0, 1.0),
    pair("WETH18/USDC", 18, 6, 3_000.0, 1.0),
    pair("SOL/WETH18", 9, 18, 0.05, 3_000.0),
    pair("X18/Y18", 18, 18, 1.0, 1.0),
];

#[derive(Clone, Copy, Debug)]
enum Shape {
    Cpmm,
    Moderate,
    TailFloor,
    Stable,
}

const SHAPES: [Shape; 4] = [Shape::Cpmm, Shape::Moderate, Shape::TailFloor, Shape::Stable];

/// Largest amplification the protocol admits for a core and fade width.
fn max_amplification(core_half_width_bps: u16, fade_width_bps: u16) -> u64 {
    let admits = |peak_amplification_nad| {
        ConcentratedCurveParameters { peak_amplification_nad, core_half_width_bps, fade_width_bps }
            .validate(MAX_AMM_AMPLIFICATION_NAD)
            .is_ok()
    };
    let (mut low, mut high) = (NAD + NAD / 10, MAX_AMM_AMPLIFICATION_NAD + 1);
    assert!(admits(low));
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if admits(middle) {
            low = middle;
        } else {
            high = middle;
        }
    }
    low
}

fn curve_parameters(shape: Shape) -> ConcentratedCurveParameters {
    let (peak_amplification_nad, core_half_width_bps, fade_width_bps) = match shape {
        Shape::Cpmm => (NAD, 0, 0),
        Shape::Moderate => (20 * NAD, 250, 500),
        // The narrowest common shape, concentrated until its tail holds the
        // protocol's minimum share of liquidity.
        Shape::TailFloor => (max_amplification(25, 50), 25, 50),
        Shape::Stable => (max_amplification(5, 10), 5, 10),
    };
    ConcentratedCurveParameters { peak_amplification_nad, core_half_width_bps, fade_width_bps }
}

fn market_config(shape: Shape) -> MarketConfig {
    let curve = curve_parameters(shape);
    MarketConfig {
        swap_fee_bps: if curve.is_cpmm() { 0 } else { MIN_CONCENTRATED_SWAP_FEE_BPS },
        target_hlp_leverage_bps: 2 * BPS_DENOMINATOR,
        settlement_divergence_bps: BPS_DENOMINATOR,
        ema_half_life_ms: MIN_HALF_LIFE_MS,
        directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
        curve_depth_ema_half_life_ms: MIN_HALF_LIFE_MS,
        max_daily_borrow_bps: DEFAULT_DAILY_BORROW_BPS,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: BPS_DENOMINATOR,
        amm: AmmConfig {
            peak_amplification_nad: curve.peak_amplification_nad,
            core_half_width_bps: curve.core_half_width_bps,
            fade_width_bps: curve.fade_width_bps,
            center_ema_half_life_ms: MIN_HALF_LIFE_MS,
            volatility_half_life_ms: MIN_HALF_LIFE_MS,
            adjustment_step_nad: NAD / 1_000,
            ..AmmConfig::default()
        },
        ..MarketConfig::default()
    }
}

/// Raw atoms of `tokens`, or `None` when a token account cannot hold them.
fn raw_amount(tokens: f64, decimals: u8) -> Option<u64> {
    let raw = tokens * 10_f64.powi(i32::from(decimals));
    (raw < u64::MAX as f64).then_some(raw as u64)
}

/// A market seeded through `add_liquidity` with `pool_usd` split evenly
/// between the sides. The configuration is one production accepts; the base
/// fee is then zeroed so only rounding can move value. `None` when the pool is
/// too small to seed.
fn seeded_market(pair: Pair, pool_usd: f64, shape: Shape) -> Option<Market> {
    let mut market = Market { version: MARKET_LAYOUT_VERSION, ..Market::current_layout() };
    market.base_side.asset_mint = Pubkey::new_unique();
    market.quote_side.asset_mint = Pubkey::new_unique();
    market.base_side.asset_decimals = pair.base_decimals;
    market.quote_side.asset_decimals = pair.quote_decimals;
    market.debt = Debt {
        base_borrow_index_nad: NAD as u128,
        quote_borrow_index_nad: NAD as u128,
        base_borrow_apr_nad: DEFAULT_BORROW_APR_NAD,
        quote_borrow_apr_nad: DEFAULT_BORROW_APR_NAD,
        ..Debt::default()
    };
    market.config = market_config(shape);
    market.config.validate().unwrap();
    market.config.swap_fee_bps = 0;
    let quote_tokens = pool_usd / 2.0 / pair.quote_usd;
    let base_raw = raw_amount(quote_tokens / pair.price, pair.base_decimals)?;
    let quote_raw = raw_amount(quote_tokens, pair.quote_decimals)?;
    if base_raw == 0 || quote_raw == 0 {
        return None;
    }
    market.add_liquidity(base_raw, quote_raw).ok()?;
    market.prepare_amm_for_swap(1).ok()?;
    Some(market)
}

fn swap(market: &mut Market, asset_in: MarketAsset, reserve_credit: u64, slot: u64) -> Result<u64> {
    let mut prepared = SwapRequest {
        current_slot: slot,
        current_unix_timestamp: 0,
        asset_in,
        reserve_credit,
        protocol_fee_bps: 0,
    }
    .prepare(market)?;
    prepared.finalize_state(market, slot, 0, ProtocolAuctionSplit::default())?;
    Ok(prepared.quote.amount_out)
}

fn error_name(error: &anchor_lang::error::Error) -> String {
    match error {
        anchor_lang::error::Error::AnchorError(error) => error.error_name.clone(),
        other => format!("{other:?}"),
    }
}

#[derive(Default, Debug)]
struct Tally {
    trips: u64,
    gains: u64,
    max_gain_atoms: u64,
    worst: String,
    overflow_errors: Vec<String>,
    other_errors: std::collections::BTreeMap<String, u64>,
}

impl Tally {
    fn record_error(&mut self, context: &str, error: &anchor_lang::error::Error) {
        let name = error_name(error);
        if name.contains("Overflow") {
            self.overflow_errors.push(format!("{context}: {name}"));
        } else {
            *self.other_errors.entry(name).or_default() += 1;
        }
    }

    fn record_trip(&mut self, context: &str, amount_in: u64, amount_back: u64) {
        self.trips += 1;
        if amount_back > amount_in {
            self.gains += 1;
            let gain = amount_back - amount_in;
            if gain > self.max_gain_atoms {
                self.max_gain_atoms = gain;
                self.worst = format!("{context}: in {amount_in}, back {amount_back}");
            }
        }
    }
}

/// One-way trades, as shares of the input reserve, that place the starting
/// price across the core, shoulders and tails.
const PRE_TRADE_SHARES: [f64; 6] = [0.001, 0.01, 0.05, 0.2, 0.5, 0.8];
/// Round-trip sizes in raw atoms, then as shares of the input reserve.
const TRADE_ATOMS: [u64; 6] = [1, 3, 10, 100, 1_000, 10_000];
const TRADE_SHARES: [f64; 5] = [1e-5, 1e-3, 1e-2, 0.1, 0.3];
const POOLS_USD: [f64; 5] = [0.1, 10.0, 10_000.0, 1e6, 1e8];

fn reserve_raw(market: &Market, asset: MarketAsset) -> u64 {
    market.side(asset).reserves.cash_reserve
}

fn atom_nad(market: &Market, asset: MarketAsset) -> u128 {
    market.normalize_amount(1, market.side(asset).asset_decimals).unwrap()
}

/// The raw input that moves the curve from its current point onto `target`,
/// and the asset paid in. Rounding leaves the result within an atom of it.
fn input_to_reach(market: &Market, target: ConcentratedCurvePoint) -> Option<(MarketAsset, u64)> {
    let current = market.ordinary_curve_point_nad().ok()?;
    let (asset, internal) = if target.base_reserve > current.base_reserve {
        (MarketAsset::Base, target.base_reserve - current.base_reserve)
    } else {
        (MarketAsset::Quote, target.quote_reserve.checked_sub(current.quote_reserve)?)
    };
    u64::try_from(internal / atom_nad(market, asset)).ok().map(|raw| (asset, raw))
}

/// Starting states reached through production swaps: the seeded center, the
/// price after one-way trades, and points within three atoms of every curve
/// boundary, where rebuilding the curve can place a point on either branch.
fn starting_markets(pair: Pair, pool_usd: f64, shape: Shape) -> Vec<(String, Market)> {
    let Some(seeded) = seeded_market(pair, pool_usd, shape) else {
        return vec![];
    };
    let label = format!("{} ${pool_usd} {shape:?}", pair.name);
    let mut starts = vec![(format!("{label} center"), seeded.clone())];
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for share in PRE_TRADE_SHARES {
            let mut market = seeded.clone();
            let amount = (reserve_raw(&market, asset) as f64 * share) as u64;
            if amount > 0 && swap(&mut market, asset, amount, 2).is_ok() {
                starts.push((format!("{label} pre={asset:?}x{share}"), market));
            }
        }
    }
    let geometry = seeded.current_concentrated_curve_geometry().unwrap().unwrap();
    let mut boundaries = vec![];
    if geometry.inner_liquidity > 0 {
        boundaries.extend([geometry.lower_boundary, geometry.upper_boundary]);
        boundaries.extend(geometry.nested_boundaries.iter().take(geometry.nested_boundary_count as usize));
    }
    for (index, boundary) in boundaries.into_iter().enumerate() {
        let Some((asset, raw)) = input_to_reach(&seeded, boundary) else {
            continue;
        };
        for offset in -3_i64..=3 {
            let amount = raw.saturating_add_signed(offset);
            let mut market = seeded.clone();
            if amount > 0 && swap(&mut market, asset, amount, 2).is_ok() {
                starts.push((format!("{label} boundary={index}{offset:+}"), market));
            }
        }
    }
    starts
}

fn trade_sizes(market: &Market, asset_in: MarketAsset) -> Vec<u64> {
    let reserve = reserve_raw(market, asset_in);
    let mut sizes: Vec<u64> = TRADE_ATOMS
        .into_iter()
        .chain(TRADE_SHARES.into_iter().map(|share| (reserve as f64 * share) as u64))
        .filter(|&size| size > 0 && size < reserve)
        .collect();
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

/// Sells a share of the input reserve and immediately buys back with the whole
/// proceeds, both in one slot, through the production swap transition.
fn round_trips(pairs: &[Pair]) -> Tally {
    let mut tally = Tally::default();
    for &pair in pairs {
        for shape in SHAPES {
            for pool_usd in POOLS_USD {
                for (label, start) in starting_markets(pair, pool_usd, shape) {
                    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
                        for amount in trade_sizes(&start, asset_in) {
                            let context = format!("{label} in={asset_in:?} amount={amount}");
                            let mut market = start.clone();
                            let out = match swap(&mut market, asset_in, amount, 3) {
                                Ok(0) => continue,
                                Ok(out) => out,
                                Err(error) => {
                                    tally.record_error(&context, &error);
                                    continue;
                                }
                            };
                            match swap(&mut market, asset_in.opposite(), out, 3) {
                                Ok(back) => {
                                    market.assert_market_invariants().unwrap();
                                    tally.record_trip(&context, amount, back);
                                }
                                Err(error) => tally.record_error(&context, &error),
                            }
                        }
                    }
                }
            }
        }
    }
    tally
}

#[test]
fn zero_fee_round_trips_return_at_most_their_input() {
    let tally = round_trips(&PAIRS);
    println!("ROUND_TRIPS {tally:?}");
    assert!(tally.trips > 1_000, "{tally:?}");
    assert!(tally.overflow_errors.is_empty(), "{:?}", tally.overflow_errors);
    assert_eq!(tally.gains, 0, "{}", tally.worst);
}

#[test]
fn markets_at_or_above_twelve_decimals_keep_their_scale() {
    for pair in HIGH_DECIMAL_PAIRS {
        let market = seeded_market(pair, 1.0, Shape::Moderate).unwrap();
        assert_eq!(market.amount_decimals().unwrap(), pair.base_decimals.max(pair.quote_decimals));
    }
    let tally = round_trips(&HIGH_DECIMAL_PAIRS);
    println!("HIGH_DECIMAL_ROUND_TRIPS {tally:?}");
    assert!(tally.trips > 500, "{tally:?}");
    assert!(tally.overflow_errors.is_empty(), "{:?}", tally.overflow_errors);
}

/// Repeats a trade and splits it into pieces. Neither may return more than
/// the trader put in.
#[test]
fn repeated_and_split_zero_fee_trades_return_at_most_their_input() {
    let mut tally = Tally::default();
    for &pair in &PAIRS {
        for shape in SHAPES {
            for pool_usd in [1.0, 10_000.0, 1e7] {
                for (label, start) in starting_markets(pair, pool_usd, shape) {
                    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
                        let amount = (reserve_raw(&start, asset_in) as f64 * 0.01) as u64;
                        if amount < 70 {
                            continue;
                        }
                        // Twenty consecutive round trips, each reinvesting
                        // everything the previous one returned.
                        let mut market = start.clone();
                        let mut holding = amount;
                        let mut slot = 3;
                        let mut failed = None;
                        for _ in 0..20 {
                            let out = swap(&mut market, asset_in, holding, slot)
                                .and_then(|out| swap(&mut market, asset_in.opposite(), out, slot));
                            match out {
                                Ok(back) => holding = back,
                                Err(error) => {
                                    failed = Some(error);
                                    break;
                                }
                            }
                            slot += 1;
                        }
                        let context = format!("{label} repeated in={asset_in:?}");
                        match failed {
                            Some(error) => tally.record_error(&context, &error),
                            None => tally.record_trip(&context, amount, holding),
                        }

                        // Ten sells, then buy back with the proceeds in seven.
                        let mut market = start.clone();
                        let context = format!("{label} split in={asset_in:?}");
                        let sold: Result<u64> = (0..10).try_fold(0_u64, |total, _| {
                            Ok(total + swap(&mut market, asset_in, amount / 10, 3)?)
                        });
                        let pieces = match sold {
                            Ok(proceeds) => {
                                let piece = proceeds / 7;
                                let last = proceeds - 6 * piece;
                                (0..7).try_fold(0_u64, |total, index| {
                                    let size = if index == 6 { last } else { piece };
                                    if size == 0 {
                                        return Ok(total);
                                    }
                                    Ok(total + swap(&mut market, asset_in.opposite(), size, 3)?)
                                })
                            }
                            Err(error) => Err(error),
                        };
                        match pieces {
                            Ok(back) => {
                                market.assert_market_invariants().unwrap();
                                tally.record_trip(&context, amount / 10 * 10, back);
                            }
                            Err(error) => tally.record_error(&context, &error),
                        }
                    }
                }
            }
        }
    }
    println!("REPEATED_AND_SPLIT {tally:?}");
    assert!(tally.trips > 200, "{tally:?}");
    assert!(tally.overflow_errors.is_empty(), "{:?}", tally.overflow_errors);
    assert_eq!(tally.gains, 0, "{}", tally.worst);
}

/// Dense starts around every curve boundary, where rebuilding the curve from
/// rounded reserves can place a point on either branch. The default run keeps
/// the suite fast; `DUSK_PRECISION_SWEEP=1` runs the full sweep.
#[test]
fn zero_fee_round_trips_beside_curve_boundaries_return_at_most_their_input() {
    let full = std::env::var_os("DUSK_PRECISION_SWEEP").is_some();
    let pairs: &[Pair] = if full { &PAIRS } else { &PAIRS[..5] };
    let pools: &[f64] = if full { &[0.1, 1.0, 10.0, 1_000.0, 100_000.0, 1e7] } else { &[0.1, 10.0, 100_000.0] };
    let offsets = if full { -16_i64..=16 } else { -6_i64..=6 };
    let atoms: &[u64] = if full { &[1, 2, 5, 10, 20, 50, 100, 200, 500, 1_000, 5_000, 50_000] } else { &[1, 10, 100, 1_000] };
    let mut tally = Tally::default();
    for &pair in pairs {
        for shape in [Shape::Moderate, Shape::TailFloor, Shape::Stable] {
            for &pool_usd in pools {
                let Some(seeded) = seeded_market(pair, pool_usd, shape) else {
                    continue;
                };
                let geometry = seeded.current_concentrated_curve_geometry().unwrap().unwrap();
                let mut boundaries = vec![geometry.lower_boundary, geometry.upper_boundary];
                boundaries.extend(geometry.nested_boundaries.iter().take(geometry.nested_boundary_count as usize));
                for (index, boundary) in boundaries.into_iter().enumerate() {
                    let Some((asset, raw)) = input_to_reach(&seeded, boundary) else {
                        continue;
                    };
                    for offset in offsets.clone() {
                        let mut start = seeded.clone();
                        let amount = raw.saturating_add_signed(offset);
                        if amount == 0 || swap(&mut start, asset, amount, 2).is_err() {
                            continue;
                        }
                        for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
                            for &size in atoms {
                                if size >= reserve_raw(&start, asset_in) {
                                    continue;
                                }
                                let context =
                                    format!("{} ${pool_usd} {shape:?} boundary={index}{offset:+} in={asset_in:?} amount={size}", pair.name);
                                let mut market = start.clone();
                                let out = match swap(&mut market, asset_in, size, 3) {
                                    Ok(0) => continue,
                                    Ok(out) => out,
                                    Err(error) => {
                                        tally.record_error(&context, &error);
                                        continue;
                                    }
                                };
                                match swap(&mut market, asset_in.opposite(), out, 3) {
                                    Ok(back) => tally.record_trip(&context, size, back),
                                    Err(error) => tally.record_error(&context, &error),
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("BOUNDARY_SWEEP full={full} {tally:?}");
    assert!(tally.trips > 5_000, "{tally:?}");
    assert!(tally.overflow_errors.is_empty(), "{:?}", tally.overflow_errors);
    assert_eq!(tally.gains, 0, "{}", tally.worst);
}

/// Runs one zero-fee round trip of every size from `start` within `slot`.
fn record_round_trips(tally: &mut Tally, label: &str, start: &Market, slot: u64) {
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        for amount in trade_sizes(start, asset_in) {
            let context = format!("{label} in={asset_in:?} amount={amount}");
            let mut market = start.clone();
            let out = match swap(&mut market, asset_in, amount, slot) {
                Ok(0) => continue,
                Ok(out) => out,
                Err(error) => {
                    tally.record_error(&context, &error);
                    continue;
                }
            };
            match swap(&mut market, asset_in.opposite(), out, slot) {
                Ok(back) => {
                    market.assert_market_invariants().unwrap();
                    tally.record_trip(&context, amount, back);
                }
                Err(error) => tally.record_error(&context, &error),
            }
        }
    }
}

/// With hLP vaults active on both sides, zero-fee round trips still return at
/// most their input.
#[test]
fn active_hlp_markets_keep_zero_fee_round_trips_bounded() {
    let mut tally = Tally::default();
    for pair in [PAIRS[0], PAIRS[1], PAIRS[4], PAIRS[7]] {
        for shape in [Shape::Cpmm, Shape::Moderate, Shape::TailFloor] {
            for pool_usd in [10_000.0, 1e8] {
                let Some(mut market) = seeded_market(pair, pool_usd, shape) else {
                    continue;
                };
                for asset in [MarketAsset::Base, MarketAsset::Quote] {
                    let amount = reserve_raw(&market, asset) / 20;
                    market.deposit_single_sided(asset, amount, 1).unwrap();
                }
                market.assert_market_invariants().unwrap();
                let label = format!("{} ${pool_usd} {shape:?} hLP", pair.name);
                record_round_trips(&mut tally, &label, &market, 3);
            }
        }
    }
    println!("HLP_ROUND_TRIPS {tally:?}");
    assert!(tally.trips > 300, "{tally:?}");
    assert!(tally.overflow_errors.is_empty(), "{:?}", tally.overflow_errors);
    assert_eq!(tally.gains, 0, "{}", tally.worst);
}

fn fixed_debt(market: &Market, position: &BorrowPosition, debt_asset: MarketAsset) -> u64 {
    let debt = match debt_asset {
        MarketAsset::Base => position.fixed_base_debt(&market.debt),
        MarketAsset::Quote => position.fixed_quote_debt(&market.debt),
    };
    u64::try_from(debt.unwrap()).unwrap()
}

/// Lending with up to tens of trillions of whole tokens on one side, where
/// plain products of two twelve-decimal amounts exceed u128. Two positions
/// each borrow half their previewed capacity, which leaves the daily limit
/// room for the second, then repay and withdraw.
#[test]
fn memecoin_scale_lending_borrows_repays_and_withdraws() {
    for (pair, pool_usd) in [(PAIRS[4], 1e8), (PAIRS[5], 1e8), (PAIRS[0], 1e8), (PAIRS[7], 1e7)] {
        for collateral_asset in [MarketAsset::Base, MarketAsset::Quote] {
            let debt_asset = collateral_asset.opposite();
            let mut market = seeded_market(pair, pool_usd, Shape::Moderate).unwrap();
            market.refresh_risk().unwrap();
            let mut positions = vec![];
            for share in [0.2, 0.1] {
                let collateral = (reserve_raw(&market, collateral_asset) as f64 * share) as u64;
                let mut position = BorrowPosition::default();
                position.clear_liquidation_auction();
                market.deposit_collateral(&mut position, collateral_asset, collateral).unwrap();
                let quote = market
                    .position_capacity_quote(&position, collateral_asset, None, true, 0, Default::default())
                    .unwrap();
                let borrowed = quote.max_borrow_amount.unwrap() / 2;
                assert!(borrowed > 0, "{} {collateral_asset:?}", pair.name);
                market.borrow(&mut position, debt_asset, borrowed, 0, 0).unwrap();
                market.assert_market_invariants().unwrap();
                positions.push(position);
            }
            for position in &mut positions {
                let half = fixed_debt(&market, position, debt_asset) / 2;
                market.repay(position, debt_asset, half).unwrap();
                let rest = fixed_debt(&market, position, debt_asset);
                market.repay(position, debt_asset, rest).unwrap();
                assert_eq!(fixed_debt(&market, position, debt_asset), 0);
                let collateral = position.collateral(collateral_asset);
                market.withdraw_collateral(position, collateral_asset, collateral, 0).unwrap();
                market.assert_market_invariants().unwrap();
            }
        }
    }
}

/// A concentration change applied through the timelocked ramp. Every applied
/// step rebuilds the curve from the current reserves; zero-fee round trips on
/// each intermediate shape return at most their input. Steps run at the
/// production minimum fee, which the fee floor requires for concentrated
/// shapes, and the round trips then zero it to isolate rounding.
#[test]
fn concentration_ramp_steps_keep_zero_fee_round_trips_bounded() {
    let mut tally = Tally::default();
    let mut applied_steps = 0;
    for &pair in &PAIRS {
        for pool_usd in [10.0, 10_000.0, 1e8] {
            // Each ramp stays within the tenfold change one ramp may make.
            let ramps = [
                (Shape::Cpmm, ConcentratedCurveParameters { peak_amplification_nad: 8 * NAD, core_half_width_bps: 250, fade_width_bps: 500 }),
                (Shape::Moderate, ConcentratedCurveParameters { peak_amplification_nad: 200 * NAD, core_half_width_bps: 25, fade_width_bps: 50 }),
            ];
            for (from, target) in ramps {
                let Some(mut market) = seeded_market(pair, pool_usd, from) else {
                    continue;
                };
                market.config.swap_fee_bps = MIN_CONCENTRATED_SWAP_FEE_BPS;
                market
                    .execute_parameter_updates(
                        &[MarketParameterUpdate::Concentration {
                            peak_amplification_nad: target.peak_amplification_nad,
                            core_half_width_bps: target.core_half_width_bps,
                            fade_width_bps: target.fade_width_bps,
                            ramp_duration_slots: MIN_CONCENTRATION_RAMP_SLOTS,
                        }],
                        2,
                    )
                    .unwrap();
                for step in 1..=4 {
                    let slot = 2 + MIN_CONCENTRATION_RAMP_SLOTS * step / 4;
                    if market.advance_concentration_ramp(slot).unwrap() {
                        applied_steps += 1;
                    }
                    market.assert_market_invariants().unwrap();
                    let mut zero_fee = market.clone();
                    zero_fee.config.swap_fee_bps = 0;
                    let label = format!("{} ${pool_usd} {from:?}->{target:?} step={step}", pair.name);
                    record_round_trips(&mut tally, &label, &zero_fee, slot);
                }
            }
        }
    }
    println!("RAMP_ROUND_TRIPS applied_steps={applied_steps} {tally:?}");
    assert!(applied_steps > 20, "{applied_steps}");
    assert!(tally.trips > 1_000, "{tally:?}");
    assert!(tally.overflow_errors.is_empty(), "{:?}", tally.overflow_errors);
    assert_eq!(tally.gains, 0, "{}", tally.worst);
}

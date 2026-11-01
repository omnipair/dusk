//! Exact visible-market eligibility and collective-window construction.
//!
//! Factual V1 reserves are held piecewise constant between canonical
//! operations and valued only with provider marks from the same UTC hour.
//! Missing marks are never forward-filled: they preserve raw eligibility, and
//! a miss inside the selected common window makes the market unavailable to
//! USD headlines and the TVL tail calculation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{ToPrimitive, Zero};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    audit::{
        visible_market_eligibility_with_policy, MarketEligibility, MarketEligibilityPolicy,
        MarketEvidence,
    },
    canonical::{ChainOrder, HistoricalPriceMarkV1, TokenAmount},
    replay::{CanonicalOperationV1, OperationPayloadV1},
    v1_reconciliation::V1ReplayLedgerRowV1,
    window::{
        chronological_splits, longest_common_window_ending_at_cutoff, ChronologicalSplitsV1,
        CommonWindowV1, HourlyMarketCoverageV1, HOUR_SECONDS,
    },
};

/// Version 2 scopes historical-TVL medians to the selected common window and
/// derives that window from raw eligibility before applying the TVL tail.
pub const UNIVERSE_SCHEMA_VERSION: u16 = 2;
const DAY_SECONDS: i64 = 86_400;
const USD_MICRO_SCALE: u64 = 1_000_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniverseWindowArtifactV1 {
    pub schema_version: u16,
    pub cutoff_block_time_unix_seconds: i64,
    pub historical_price_provider: String,
    pub canonical_operations_sha256: String,
    pub factual_ledger_sha256: String,
    pub historical_prices_sha256: String,
    pub market_eligibility: Vec<MarketEligibility>,
    pub hourly_market_coverage: Vec<HourlyMarketCoverageV1>,
    pub common_window: Option<CommonWindowV1>,
    pub chronological_splits: Option<ChronologicalSplitsV1>,
    pub blocking_reasons: BTreeSet<String>,
}

#[derive(Clone)]
struct OperationMeta {
    market: String,
    order: ChainOrder,
    block_time: i64,
    is_initialize: bool,
}

#[derive(Clone)]
struct ExactPrice {
    coefficient: BigInt,
    scale: BigInt,
}

#[derive(Default)]
struct HourAccumulator {
    active_seconds: u64,
    missing_price_seconds: u64,
    value_time_usd_micro_seconds: BigRational,
}

struct MarketAccumulator {
    token0: String,
    token1: String,
    token0_decimals: u8,
    token1_decimals: u8,
    reserve0: u64,
    reserve1: u64,
    last_time: i64,
    hours: BTreeMap<i64, HourAccumulator>,
}

#[derive(Debug)]
struct UniverseSelection {
    market_eligibility: Vec<MarketEligibility>,
    hourly_market_coverage: Vec<HourlyMarketCoverageV1>,
    common_window: Option<CommonWindowV1>,
}

#[allow(clippy::too_many_arguments)]
pub fn derive_universe_and_window(
    operations_path: &Path,
    factual_ledger_path: &Path,
    historical_prices_path: &Path,
    historical_price_provider: &str,
    visible_by_market: &BTreeMap<String, bool>,
    hard_gates_by_market: &BTreeMap<String, bool>,
    cutoff_block_time_unix_seconds: i64,
    eligibility_policy: MarketEligibilityPolicy,
    required_market_coverage_bps: u16,
    required_tvl_coverage_bps: u16,
    training_bps: u16,
    validation_bps: u16,
    holdout_bps: u16,
) -> Result<UniverseWindowArtifactV1> {
    if historical_price_provider.trim().is_empty() {
        bail!("historical price provider is empty");
    }
    let prices = load_prices(historical_prices_path, historical_price_provider)?;
    let (operations, swap_counts, swap_days) = load_operation_metadata(operations_path)?;
    let mut accumulators = BTreeMap::<String, MarketAccumulator>::new();

    for (line_index, line) in BufReader::new(
        File::open(factual_ledger_path)
            .with_context(|| format!("opening {}", factual_ledger_path.display()))?,
    )
    .lines()
    .enumerate()
    {
        let line = line.with_context(|| format!("reading factual row {}", line_index + 1))?;
        if line.trim().is_empty() {
            bail!("blank factual ledger row at line {}", line_index + 1);
        }
        let row: V1ReplayLedgerRowV1 = serde_json::from_str(&line)
            .with_context(|| format!("decoding factual row {}", line_index + 1))?;
        let meta = operations.get(&row.operation_id).with_context(|| {
            format!(
                "factual row {} has no canonical operation",
                row.operation_id
            )
        })?;
        if row.order != meta.order {
            bail!("factual row order differs from its canonical operation");
        }
        if !row.broadcast_global_config && meta.market != row.market {
            bail!("factual row market differs from its canonical operation");
        }
        if meta.block_time > cutoff_block_time_unix_seconds {
            bail!("factual operation block time exceeds the frozen cutoff");
        }
        let pair = &row.compact_checkpoint.pair;
        match accumulators.get_mut(&row.market) {
            Some(market) => {
                if meta.block_time < market.last_time {
                    bail!(
                        "authoritative block time regressed for market {}",
                        row.market
                    );
                }
                integrate_market(market, meta.block_time, &prices)?;
                if market.token0 != pair.token0.to_string()
                    || market.token1 != pair.token1.to_string()
                    || market.token0_decimals != pair.token0_decimals
                    || market.token1_decimals != pair.token1_decimals
                {
                    bail!("market token identity changed during factual replay");
                }
                market.reserve0 = pair.reserve0;
                market.reserve1 = pair.reserve1;
                market.last_time = meta.block_time;
            }
            None => {
                if !meta.is_initialize || row.broadcast_global_config {
                    bail!("market factual ledger begins before canonical initialization");
                }
                accumulators.insert(
                    row.market.clone(),
                    MarketAccumulator {
                        token0: pair.token0.to_string(),
                        token1: pair.token1.to_string(),
                        token0_decimals: pair.token0_decimals,
                        token1_decimals: pair.token1_decimals,
                        reserve0: pair.reserve0,
                        reserve1: pair.reserve1,
                        last_time: meta.block_time,
                        hours: BTreeMap::new(),
                    },
                );
            }
        }
    }

    let cutoff_exclusive = cutoff_block_time_unix_seconds
        .checked_add(1)
        .context("cutoff timestamp overflow")?;
    for market in accumulators.values_mut() {
        integrate_market(market, cutoff_exclusive, &prices)?;
    }

    let selection = select_universe_and_window(
        &accumulators,
        visible_by_market,
        hard_gates_by_market,
        &swap_counts,
        &swap_days,
        cutoff_block_time_unix_seconds,
        eligibility_policy,
        required_market_coverage_bps,
        required_tvl_coverage_bps,
    )?;
    let UniverseSelection {
        market_eligibility,
        hourly_market_coverage,
        common_window,
    } = selection;
    let chronological_splits = common_window
        .as_ref()
        .map(|window| chronological_splits(window, training_bps, validation_bps, holdout_bps))
        .transpose()?;
    let mut blocking_reasons = BTreeSet::new();
    if common_window.is_none() {
        blocking_reasons.insert("no_collective_common_window_ending_at_cutoff".to_owned());
    }
    for market in &market_eligibility {
        if market.eligible && !market.usd_headline_eligible {
            blocking_reasons.insert(format!(
                "historical_usd_tvl_unavailable:{}",
                market.pair_address
            ));
        }
    }

    Ok(UniverseWindowArtifactV1 {
        schema_version: UNIVERSE_SCHEMA_VERSION,
        cutoff_block_time_unix_seconds,
        historical_price_provider: historical_price_provider.to_owned(),
        canonical_operations_sha256: sha256_file(operations_path)?,
        factual_ledger_sha256: sha256_file(factual_ledger_path)?,
        historical_prices_sha256: sha256_file(historical_prices_path)?,
        market_eligibility,
        hourly_market_coverage,
        common_window,
        chronological_splits,
        blocking_reasons,
    })
}

#[allow(clippy::too_many_arguments)]
fn select_universe_and_window(
    accumulators: &BTreeMap<String, MarketAccumulator>,
    visible_by_market: &BTreeMap<String, bool>,
    hard_gates_by_market: &BTreeMap<String, bool>,
    swap_counts: &BTreeMap<String, u64>,
    swap_days: &BTreeMap<String, BTreeSet<i64>>,
    cutoff_block_time_unix_seconds: i64,
    eligibility_policy: MarketEligibilityPolicy,
    required_market_coverage_bps: u16,
    required_tvl_coverage_bps: u16,
) -> Result<UniverseSelection> {
    // Raw eligibility deliberately has no USD input. Markets with missing
    // historical marks therefore remain available for raw-token/invariant
    // analysis and participate in the configured collective coverage gate.
    let raw_evidence = market_evidence_for_window(
        accumulators,
        visible_by_market,
        hard_gates_by_market,
        swap_counts,
        swap_days,
        None,
    );
    let raw_eligibility = visible_market_eligibility_with_policy(&raw_evidence, eligibility_policy);
    let raw_eligible_markets = eligible_market_set(&raw_eligibility);
    let mut hourly_market_coverage = build_hourly_market_coverage(
        accumulators,
        hard_gates_by_market,
        &raw_eligible_markets,
        cutoff_block_time_unix_seconds,
    )?;
    let provisional_window = longest_common_window_ending_at_cutoff(
        &hourly_market_coverage,
        cutoff_block_time_unix_seconds,
        required_market_coverage_bps,
        required_tvl_coverage_bps,
    )?;
    let provisional_evidence = market_evidence_for_window(
        accumulators,
        visible_by_market,
        hard_gates_by_market,
        swap_counts,
        swap_days,
        provisional_window.as_ref(),
    );
    let provisional_eligibility =
        visible_market_eligibility_with_policy(&provisional_evidence, eligibility_policy);
    let provisional_eligible_markets = eligible_market_set(&provisional_eligibility);

    if provisional_eligible_markets == raw_eligible_markets {
        return Ok(UniverseSelection {
            market_eligibility: provisional_eligibility,
            hourly_market_coverage,
            common_window: provisional_window,
        });
    }

    // The p10 tail can only be known after the raw-eligible common window is
    // known. Refine the window exactly once using that tail, then prove the
    // resulting membership is stable. A second membership change fails closed
    // instead of emitting eligibility and window artifacts with different
    // populations.
    set_hourly_eligibility(&mut hourly_market_coverage, &provisional_eligible_markets);
    let common_window = longest_common_window_ending_at_cutoff(
        &hourly_market_coverage,
        cutoff_block_time_unix_seconds,
        required_market_coverage_bps,
        required_tvl_coverage_bps,
    )?;
    let refined_evidence = market_evidence_for_window(
        accumulators,
        visible_by_market,
        hard_gates_by_market,
        swap_counts,
        swap_days,
        common_window.as_ref(),
    );
    let market_eligibility =
        visible_market_eligibility_with_policy(&refined_evidence, eligibility_policy);
    let refined_eligible_markets = eligible_market_set(&market_eligibility);
    if refined_eligible_markets != provisional_eligible_markets {
        bail!(
            "historical-TVL p10 membership changed after the single bounded common-window refinement"
        );
    }

    Ok(UniverseSelection {
        market_eligibility,
        hourly_market_coverage,
        common_window,
    })
}

fn market_evidence_for_window(
    accumulators: &BTreeMap<String, MarketAccumulator>,
    visible_by_market: &BTreeMap<String, bool>,
    hard_gates_by_market: &BTreeMap<String, bool>,
    swap_counts: &BTreeMap<String, u64>,
    swap_days: &BTreeMap<String, BTreeSet<i64>>,
    common_window: Option<&CommonWindowV1>,
) -> Vec<MarketEvidence> {
    visible_by_market
        .iter()
        .map(|(pair, visible)| MarketEvidence {
            pair_address: pair.clone(),
            visible: *visible,
            hard_gates_pass: hard_gates_by_market.get(pair).copied().unwrap_or(false),
            swap_count: swap_counts.get(pair).copied().unwrap_or(0),
            active_swap_days: u32::try_from(swap_days.get(pair).map(BTreeSet::len).unwrap_or(0))
                .unwrap_or(u32::MAX),
            median_time_weighted_tvl_usd_micros: common_window.and_then(|window| {
                accumulators
                    .get(pair)
                    .and_then(|market| median_tvl_if_complete_in_window(market, window))
            }),
        })
        .collect()
}

fn eligible_market_set(markets: &[MarketEligibility]) -> BTreeSet<String> {
    markets
        .iter()
        .filter(|market| market.eligible)
        .map(|market| market.pair_address.clone())
        .collect()
}

fn set_hourly_eligibility(
    rows: &mut [HourlyMarketCoverageV1],
    eligible_markets: &BTreeSet<String>,
) {
    for row in rows {
        row.eligible = eligible_markets.contains(&row.pair);
    }
}

fn build_hourly_market_coverage(
    accumulators: &BTreeMap<String, MarketAccumulator>,
    hard_gates_by_market: &BTreeMap<String, bool>,
    eligible_markets: &BTreeSet<String>,
    cutoff_block_time_unix_seconds: i64,
) -> Result<Vec<HourlyMarketCoverageV1>> {
    let first_bucket = accumulators
        .values()
        .flat_map(|market| market.hours.keys())
        .copied()
        .min();
    let cutoff_bucket = cutoff_block_time_unix_seconds.div_euclid(HOUR_SECONDS) * HOUR_SECONDS;
    let mut output = Vec::new();
    if let Some(first_bucket) = first_bucket {
        let mut bucket = first_bucket;
        loop {
            for (pair, market) in accumulators {
                let hour = market.hours.get(&bucket);
                let active = hour.is_some_and(|hour| hour.active_seconds > 0);
                let historical_prices_complete = hour
                    .is_some_and(|hour| hour.active_seconds > 0 && hour.missing_price_seconds == 0);
                output.push(HourlyMarketCoverageV1 {
                    bucket_unix_seconds: bucket,
                    pair: pair.clone(),
                    eligible: eligible_markets.contains(pair),
                    ordered_events_complete: active
                        && hard_gates_by_market.get(pair).copied().unwrap_or(false),
                    historical_prices_complete,
                    factual_tvl_usd_micros: hour
                        .filter(|_| historical_prices_complete)
                        .and_then(hourly_average_tvl)
                        .unwrap_or_else(|| token(0)),
                });
            }
            if bucket == cutoff_bucket {
                break;
            }
            bucket = bucket
                .checked_add(HOUR_SECONDS)
                .context("hourly coverage bucket overflow")?;
            if bucket > cutoff_bucket {
                break;
            }
        }
    }
    output.sort_by(|left, right| {
        left.bucket_unix_seconds
            .cmp(&right.bucket_unix_seconds)
            .then(left.pair.cmp(&right.pair))
    });
    Ok(output)
}

fn load_operation_metadata(
    path: &Path,
) -> Result<(
    BTreeMap<String, OperationMeta>,
    BTreeMap<String, u64>,
    BTreeMap<String, BTreeSet<i64>>,
)> {
    let mut operations = BTreeMap::new();
    let mut swap_counts = BTreeMap::<String, u64>::new();
    let mut swap_days = BTreeMap::<String, BTreeSet<i64>>::new();
    let mut previous_order = None;
    for (line_index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            bail!("blank canonical operation at line {}", line_index + 1);
        }
        let operation: CanonicalOperationV1 = serde_json::from_str(&line)?;
        operation.validate()?;
        if previous_order.is_some_and(|order| order >= operation.order) {
            bail!("canonical operation order is not strictly increasing");
        }
        previous_order = Some(operation.order);
        if matches!(&operation.payload, OperationPayloadV1::SwapExactIn { .. }) {
            let count = swap_counts.entry(operation.pair.clone()).or_default();
            *count = count.checked_add(1).context("swap count overflow")?;
            swap_days
                .entry(operation.pair.clone())
                .or_default()
                .insert(operation.block_time_unix_seconds.div_euclid(DAY_SECONDS));
        }
        let meta = OperationMeta {
            market: operation.pair,
            order: operation.order,
            block_time: operation.block_time_unix_seconds,
            is_initialize: matches!(
                &operation.payload,
                OperationPayloadV1::InitializeMarket { .. }
            ),
        };
        if operations.insert(operation.operation_id, meta).is_some() {
            bail!("duplicate canonical operation ID");
        }
    }
    Ok((operations, swap_counts, swap_days))
}

fn load_prices(path: &Path, provider: &str) -> Result<BTreeMap<(String, i64), ExactPrice>> {
    let mut output = BTreeMap::new();
    for (line_index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            bail!("blank historical price row at line {}", line_index + 1);
        }
        let mark: HistoricalPriceMarkV1 = serde_json::from_str(&line)?;
        mark.validate()?;
        if mark.provider != provider {
            continue;
        }
        let (integer, fraction) = mark
            .price_usd
            .as_str()
            .split_once('.')
            .unwrap_or((mark.price_usd.as_str(), ""));
        let coefficient = BigInt::from_str(&format!("{integer}{fraction}"))?;
        if coefficient <= BigInt::zero() {
            bail!("historical price is not positive");
        }
        let price = ExactPrice {
            coefficient,
            scale: BigInt::from(10_u8).pow(u32::try_from(fraction.len())?),
        };
        if output
            .insert((mark.mint, mark.bucket_unix_seconds), price)
            .is_some()
        {
            bail!("duplicate provider price for one mint/hour");
        }
    }
    Ok(output)
}

fn integrate_market(
    market: &mut MarketAccumulator,
    end_time: i64,
    prices: &BTreeMap<(String, i64), ExactPrice>,
) -> Result<()> {
    let mut cursor = market.last_time;
    while cursor < end_time {
        let bucket = cursor.div_euclid(HOUR_SECONDS) * HOUR_SECONDS;
        let bucket_end = bucket
            .checked_add(HOUR_SECONDS)
            .context("hour boundary overflow")?;
        let segment_end = end_time.min(bucket_end);
        let seconds = u64::try_from(segment_end - cursor)?;
        let hour = market.hours.entry(bucket).or_default();
        hour.active_seconds = hour
            .active_seconds
            .checked_add(seconds)
            .context("active seconds overflow")?;
        match (
            prices.get(&(market.token0.clone(), bucket)),
            prices.get(&(market.token1.clone(), bucket)),
        ) {
            (Some(price0), Some(price1)) => {
                let value = value_atoms(market.reserve0, market.token0_decimals, price0)
                    + value_atoms(market.reserve1, market.token1_decimals, price1);
                hour.value_time_usd_micro_seconds +=
                    value * BigRational::from_integer(BigInt::from(seconds));
            }
            _ => {
                hour.missing_price_seconds = hour
                    .missing_price_seconds
                    .checked_add(seconds)
                    .context("missing price seconds overflow")?;
            }
        }
        cursor = segment_end;
    }
    Ok(())
}

fn value_atoms(atoms: u64, decimals: u8, price: &ExactPrice) -> BigRational {
    BigRational::new(
        BigInt::from(atoms) * &price.coefficient * BigInt::from(USD_MICRO_SCALE),
        BigInt::from(10_u8).pow(u32::from(decimals)) * &price.scale,
    )
}

fn hourly_average_tvl(hour: &HourAccumulator) -> Option<TokenAmount> {
    if hour.active_seconds == 0 || hour.missing_price_seconds != 0 {
        return None;
    }
    let average = &hour.value_time_usd_micro_seconds
        / BigRational::from_integer(BigInt::from(hour.active_seconds));
    let floored = average.to_integer().to_u128()?;
    Some(token(floored))
}

fn median_tvl_if_complete_in_window(
    market: &MarketAccumulator,
    window: &CommonWindowV1,
) -> Option<TokenAmount> {
    if window.hourly_coverage.is_empty() {
        return None;
    }
    let mut values = Vec::with_capacity(window.hourly_coverage.len());
    for collective_hour in &window.hourly_coverage {
        let hour = market.hours.get(&collective_hour.bucket_unix_seconds)?;
        if hour.active_seconds == 0 || hour.missing_price_seconds != 0 {
            return None;
        }
        let value = hourly_average_tvl(hour)?;
        values.push(value.as_str().parse::<u128>().ok()?);
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    let median = if values.len() % 2 == 0 {
        values[middle - 1].checked_add(values[middle])? / 2
    } else {
        values[middle]
    };
    Some(token(median))
}

fn token(value: u128) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u128 is a canonical token amount")
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(value: u64) -> ExactPrice {
        ExactPrice {
            coefficient: BigInt::from(value),
            scale: BigInt::from(1_u8),
        }
    }

    #[test]
    fn integration_splits_at_hour_boundaries_without_forward_fill() {
        let mut market = MarketAccumulator {
            token0: "a".to_owned(),
            token1: "b".to_owned(),
            token0_decimals: 0,
            token1_decimals: 0,
            reserve0: 2,
            reserve1: 3,
            last_time: 3_599,
            hours: BTreeMap::new(),
        };
        let prices = BTreeMap::from([
            (("a".to_owned(), 0), price(1)),
            (("b".to_owned(), 0), price(1)),
            (("a".to_owned(), 3_600), price(2)),
            (("b".to_owned(), 3_600), price(2)),
        ]);
        integrate_market(&mut market, 3_601, &prices).unwrap();
        assert_eq!(
            hourly_average_tvl(&market.hours[&0]).unwrap().as_str(),
            "5000000"
        );
        assert_eq!(
            hourly_average_tvl(&market.hours[&3_600]).unwrap().as_str(),
            "10000000"
        );
    }

    #[test]
    fn any_unpriced_active_second_blocks_usd_median() {
        let market = MarketAccumulator {
            token0: "a".to_owned(),
            token1: "b".to_owned(),
            token0_decimals: 0,
            token1_decimals: 0,
            reserve0: 1,
            reserve1: 1,
            last_time: 0,
            hours: BTreeMap::from([(
                0,
                HourAccumulator {
                    active_seconds: 10,
                    missing_price_seconds: 1,
                    value_time_usd_micro_seconds: BigRational::from_integer(BigInt::from(1)),
                },
            )]),
        };
        let coverage = [HourlyMarketCoverageV1 {
            bucket_unix_seconds: 0,
            pair: "pair".to_owned(),
            eligible: true,
            ordered_events_complete: true,
            historical_prices_complete: false,
            factual_tvl_usd_micros: token(0),
        }];
        let window = longest_common_window_ending_at_cutoff(&coverage, 10, 0, 0)
            .unwrap()
            .unwrap();
        assert!(median_tvl_if_complete_in_window(&market, &window).is_none());
    }

    fn complete_hour(tvl_usd_micros: u64) -> HourAccumulator {
        HourAccumulator {
            active_seconds: u64::try_from(HOUR_SECONDS).unwrap(),
            missing_price_seconds: 0,
            value_time_usd_micro_seconds: BigRational::from_integer(
                BigInt::from(tvl_usd_micros) * BigInt::from(HOUR_SECONDS),
            ),
        }
    }

    fn fixture_market(hours: BTreeMap<i64, HourAccumulator>) -> MarketAccumulator {
        MarketAccumulator {
            token0: "a".to_owned(),
            token1: "b".to_owned(),
            token0_decimals: 0,
            token1_decimals: 0,
            reserve0: 1,
            reserve1: 1,
            last_time: 0,
            hours,
        }
    }

    #[test]
    fn pre_window_missing_mark_does_not_block_window_scoped_usd_eligibility() {
        let mut accumulators = BTreeMap::new();
        let mut visible = BTreeMap::new();
        let mut hard_gates = BTreeMap::new();
        let mut swap_counts = BTreeMap::new();
        let mut swap_days = BTreeMap::new();
        for index in 0..5 {
            let pair = format!("pair-{index}");
            let first_hour = if index == 4 {
                HourAccumulator {
                    active_seconds: u64::try_from(HOUR_SECONDS).unwrap(),
                    missing_price_seconds: u64::try_from(HOUR_SECONDS).unwrap(),
                    value_time_usd_micro_seconds: BigRational::zero(),
                }
            } else {
                complete_hour(1_000_000 + index)
            };
            accumulators.insert(
                pair.clone(),
                fixture_market(BTreeMap::from([
                    (0, first_hour),
                    (HOUR_SECONDS, complete_hour(2_000_000 + index)),
                ])),
            );
            visible.insert(pair.clone(), true);
            hard_gates.insert(pair.clone(), true);
            swap_counts.insert(pair.clone(), 10);
            swap_days.insert(pair, BTreeSet::from([0, 1, 2]));
        }

        let selection = select_universe_and_window(
            &accumulators,
            &visible,
            &hard_gates,
            &swap_counts,
            &swap_days,
            2 * HOUR_SECONDS - 1,
            MarketEligibilityPolicy::default(),
            10_000,
            10_000,
        )
        .unwrap();

        let window = selection.common_window.unwrap();
        assert_eq!(window.start_unix_seconds, HOUR_SECONDS);
        let market = selection
            .market_eligibility
            .iter()
            .find(|market| market.pair_address == "pair-4")
            .unwrap();
        assert!(market.eligible);
        assert!(market.usd_headline_eligible);
        assert_eq!(
            market
                .median_time_weighted_tvl_usd_micros
                .as_ref()
                .unwrap()
                .as_str(),
            "2000004"
        );
        let pre_window = selection
            .hourly_market_coverage
            .iter()
            .find(|row| row.pair == "pair-4" && row.bucket_unix_seconds == 0)
            .unwrap();
        assert!(pre_window.eligible);
        assert!(!pre_window.historical_prices_complete);
    }

    #[test]
    fn p10_tail_refines_hourly_eligibility_once_and_stays_stable() {
        let mut accumulators = BTreeMap::new();
        let mut visible = BTreeMap::new();
        let mut hard_gates = BTreeMap::new();
        let mut swap_counts = BTreeMap::new();
        let mut swap_days = BTreeMap::new();
        for index in 0..10 {
            let pair = format!("pair-{index:02}");
            accumulators.insert(
                pair.clone(),
                fixture_market(BTreeMap::from([(
                    0,
                    complete_hour((index + 1) * 1_000_000),
                )])),
            );
            visible.insert(pair.clone(), true);
            hard_gates.insert(pair.clone(), true);
            swap_counts.insert(pair.clone(), 10);
            swap_days.insert(pair, BTreeSet::from([0, 1, 2]));
        }

        let selection = select_universe_and_window(
            &accumulators,
            &visible,
            &hard_gates,
            &swap_counts,
            &swap_days,
            HOUR_SECONDS - 1,
            MarketEligibilityPolicy::default(),
            9_000,
            9_500,
        )
        .unwrap();

        let lowest = selection
            .market_eligibility
            .iter()
            .find(|market| market.pair_address == "pair-00")
            .unwrap();
        assert!(!lowest.eligible);
        assert!(selection
            .hourly_market_coverage
            .iter()
            .filter(|row| row.pair == "pair-00")
            .all(|row| !row.eligible));
        assert_eq!(
            selection.common_window.as_ref().unwrap().start_unix_seconds,
            0
        );
    }

    #[test]
    fn second_tail_membership_change_fails_closed() {
        let mut accumulators = BTreeMap::new();
        let mut visible = BTreeMap::new();
        let mut hard_gates = BTreeMap::new();
        let mut swap_counts = BTreeMap::new();
        let mut swap_days = BTreeMap::new();
        for index in 0..10 {
            let pair = format!("pair-{index:02}");
            let first_hour = if index == 0 {
                HourAccumulator {
                    active_seconds: u64::try_from(HOUR_SECONDS).unwrap(),
                    missing_price_seconds: u64::try_from(HOUR_SECONDS).unwrap(),
                    value_time_usd_micro_seconds: BigRational::zero(),
                }
            } else {
                complete_hour((index + 1) * 1_000_000)
            };
            accumulators.insert(
                pair.clone(),
                fixture_market(BTreeMap::from([
                    (0, first_hour),
                    (HOUR_SECONDS, complete_hour((index + 1) * 1_000_000)),
                ])),
            );
            visible.insert(pair.clone(), true);
            hard_gates.insert(pair.clone(), true);
            swap_counts.insert(pair.clone(), 10);
            swap_days.insert(pair, BTreeSet::from([0, 1, 2]));
        }

        let error = select_universe_and_window(
            &accumulators,
            &visible,
            &hard_gates,
            &swap_counts,
            &swap_days,
            2 * HOUR_SECONDS - 1,
            MarketEligibilityPolicy::default(),
            10_000,
            10_000,
        )
        .unwrap_err();
        assert!(error.to_string().contains(
            "historical-TVL p10 membership changed after the single bounded common-window refinement"
        ));
    }
}

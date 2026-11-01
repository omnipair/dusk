//! Exact collective-window coverage and chronological split boundaries.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::TokenAmount;

pub const WINDOW_SCHEMA_VERSION: u16 = 1;
pub const HOUR_SECONDS: i64 = 3_600;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HourlyMarketCoverageV1 {
    pub bucket_unix_seconds: i64,
    pub pair: String,
    pub eligible: bool,
    pub ordered_events_complete: bool,
    pub historical_prices_complete: bool,
    pub factual_tvl_usd_micros: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HourlyCollectiveCoverageV1 {
    pub bucket_unix_seconds: i64,
    pub complete_markets: u64,
    pub eligible_markets: u64,
    pub complete_tvl_usd_micros: TokenAmount,
    pub factual_tvl_usd_micros: TokenAmount,
    pub market_coverage_bps: u16,
    pub tvl_coverage_bps: u16,
    pub passes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommonWindowV1 {
    pub schema_version: u16,
    pub start_unix_seconds: i64,
    pub end_unix_seconds_exclusive: i64,
    pub covered_hour_buckets: u64,
    pub required_market_coverage_bps: u16,
    pub required_tvl_coverage_bps: u16,
    pub hourly_coverage: Vec<HourlyCollectiveCoverageV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChronologicalSplitsV1 {
    pub schema_version: u16,
    pub training_start_unix_seconds: i64,
    pub training_end_unix_seconds_exclusive: i64,
    pub validation_start_unix_seconds: i64,
    pub validation_end_unix_seconds_exclusive: i64,
    pub holdout_start_unix_seconds: i64,
    pub holdout_end_unix_seconds_exclusive: i64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WindowError {
    #[error("coverage basis points must not exceed 10,000")]
    InvalidCoverageBps,
    #[error("coverage bucket is not aligned to one hour")]
    NonHourlyBucket,
    #[error("duplicate market coverage row in one hour")]
    DuplicateMarketHour,
    #[error("TVL amount exceeds u128")]
    TvlOverflow,
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
    #[error("common window is empty or invalid")]
    InvalidWindow,
    #[error("chronological split basis points must total 10,000 and be non-zero")]
    InvalidSplits,
}

fn parse_tvl(value: &TokenAmount) -> Result<u128, WindowError> {
    value
        .as_str()
        .parse::<u128>()
        .map_err(|_| WindowError::TvlOverflow)
}

fn token(value: u128) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u128 is canonical decimal")
}

fn coverage_bps(observed: u128, expected: u128) -> u16 {
    if expected == 0 {
        return 0;
    }
    observed
        .saturating_mul(10_000)
        .checked_div(expected)
        .unwrap_or_default()
        .min(10_000) as u16
}

/// Returns the longest hourly interval ending at the cutoff for which every
/// bucket meets both collective gates. A failure near the cutoff yields no
/// common window; an earlier failure truncates the start immediately after it.
pub fn longest_common_window_ending_at_cutoff(
    rows: &[HourlyMarketCoverageV1],
    cutoff_unix_seconds: i64,
    required_market_coverage_bps: u16,
    required_tvl_coverage_bps: u16,
) -> Result<Option<CommonWindowV1>, WindowError> {
    if required_market_coverage_bps > 10_000 || required_tvl_coverage_bps > 10_000 {
        return Err(WindowError::InvalidCoverageBps);
    }
    let cutoff_bucket = cutoff_unix_seconds.div_euclid(HOUR_SECONDS) * HOUR_SECONDS;
    let expected_markets: BTreeSet<&str> = rows
        .iter()
        .filter(|row| row.eligible)
        .map(|row| row.pair.as_str())
        .collect();
    if expected_markets.is_empty() {
        return Ok(None);
    }
    let mut by_bucket = BTreeMap::<i64, BTreeMap<&str, &HourlyMarketCoverageV1>>::new();
    for row in rows.iter().filter(|row| row.eligible) {
        if row.bucket_unix_seconds.rem_euclid(HOUR_SECONDS) != 0 {
            return Err(WindowError::NonHourlyBucket);
        }
        if by_bucket
            .entry(row.bucket_unix_seconds)
            .or_default()
            .insert(row.pair.as_str(), row)
            .is_some()
        {
            return Err(WindowError::DuplicateMarketHour);
        }
    }

    let mut accepted_reversed = Vec::new();
    let mut bucket = cutoff_bucket;
    loop {
        let Some(markets) = by_bucket.get(&bucket) else {
            break;
        };
        if markets.len() != expected_markets.len() {
            break;
        }
        let mut complete_markets = 0_u128;
        let mut complete_tvl = 0_u128;
        let mut factual_tvl = 0_u128;
        for pair in &expected_markets {
            let Some(row) = markets.get(pair) else {
                return Err(WindowError::ArithmeticOverflow);
            };
            let tvl = parse_tvl(&row.factual_tvl_usd_micros)?;
            factual_tvl = factual_tvl
                .checked_add(tvl)
                .ok_or(WindowError::ArithmeticOverflow)?;
            if row.ordered_events_complete {
                complete_markets = complete_markets.saturating_add(1);
            }
            if row.ordered_events_complete && row.historical_prices_complete {
                complete_tvl = complete_tvl
                    .checked_add(tvl)
                    .ok_or(WindowError::ArithmeticOverflow)?;
            }
        }
        let market_bps = coverage_bps(complete_markets, expected_markets.len() as u128);
        let tvl_bps = coverage_bps(complete_tvl, factual_tvl);
        let passes =
            market_bps >= required_market_coverage_bps && tvl_bps >= required_tvl_coverage_bps;
        if !passes {
            break;
        }
        accepted_reversed.push(HourlyCollectiveCoverageV1 {
            bucket_unix_seconds: bucket,
            complete_markets: complete_markets as u64,
            eligible_markets: expected_markets.len() as u64,
            complete_tvl_usd_micros: token(complete_tvl),
            factual_tvl_usd_micros: token(factual_tvl),
            market_coverage_bps: market_bps,
            tvl_coverage_bps: tvl_bps,
            passes,
        });
        let Some(previous) = bucket.checked_sub(HOUR_SECONDS) else {
            break;
        };
        bucket = previous;
    }
    if accepted_reversed.is_empty() {
        return Ok(None);
    }
    accepted_reversed.reverse();
    Ok(Some(CommonWindowV1 {
        schema_version: WINDOW_SCHEMA_VERSION,
        start_unix_seconds: accepted_reversed[0].bucket_unix_seconds,
        end_unix_seconds_exclusive: cutoff_unix_seconds
            .checked_add(1)
            .ok_or(WindowError::ArithmeticOverflow)?,
        covered_hour_buckets: accepted_reversed.len() as u64,
        required_market_coverage_bps,
        required_tvl_coverage_bps,
        hourly_coverage: accepted_reversed,
    }))
}

pub fn chronological_splits(
    window: &CommonWindowV1,
    training_bps: u16,
    validation_bps: u16,
    holdout_bps: u16,
) -> Result<ChronologicalSplitsV1, WindowError> {
    if training_bps == 0
        || validation_bps == 0
        || holdout_bps == 0
        || u32::from(training_bps) + u32::from(validation_bps) + u32::from(holdout_bps) != 10_000
        || window.start_unix_seconds >= window.end_unix_seconds_exclusive
    {
        return Err(WindowError::InvalidSplits);
    }
    let duration = window
        .end_unix_seconds_exclusive
        .checked_sub(window.start_unix_seconds)
        .ok_or(WindowError::ArithmeticOverflow)?;
    let training_duration = (i128::from(duration) * i128::from(training_bps)) / 10_000;
    let validation_duration = (i128::from(duration) * i128::from(validation_bps)) / 10_000;
    let training_end = i128::from(window.start_unix_seconds)
        .checked_add(training_duration)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(WindowError::ArithmeticOverflow)?;
    let validation_end = i128::from(training_end)
        .checked_add(validation_duration)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(WindowError::ArithmeticOverflow)?;
    if training_end <= window.start_unix_seconds
        || validation_end <= training_end
        || validation_end >= window.end_unix_seconds_exclusive
    {
        return Err(WindowError::InvalidSplits);
    }
    Ok(ChronologicalSplitsV1 {
        schema_version: WINDOW_SCHEMA_VERSION,
        training_start_unix_seconds: window.start_unix_seconds,
        training_end_unix_seconds_exclusive: training_end,
        validation_start_unix_seconds: training_end,
        validation_end_unix_seconds_exclusive: validation_end,
        holdout_start_unix_seconds: validation_end,
        holdout_end_unix_seconds_exclusive: window.end_unix_seconds_exclusive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(hour: i64, pair: &str, complete: bool, tvl: u128) -> HourlyMarketCoverageV1 {
        HourlyMarketCoverageV1 {
            bucket_unix_seconds: hour * HOUR_SECONDS,
            pair: pair.to_owned(),
            eligible: true,
            ordered_events_complete: complete,
            historical_prices_complete: complete,
            factual_tvl_usd_micros: token(tvl),
        }
    }

    #[test]
    fn last_failed_hour_truncates_the_common_window() {
        let mut rows = Vec::new();
        for hour in 0..=3 {
            rows.push(row(hour, "large", true, 95));
            rows.push(row(hour, "small", hour != 1, 5));
        }
        let window =
            longest_common_window_ending_at_cutoff(&rows, 3 * HOUR_SECONDS + 100, 5_000, 9_500)
                .unwrap()
                .unwrap();
        // Hour 1 still passes exactly 95% TVL, so the full history survives.
        assert_eq!(window.start_unix_seconds, 0);
        assert_eq!(window.covered_hour_buckets, 4);
        let none =
            longest_common_window_ending_at_cutoff(&rows, 3 * HOUR_SECONDS + 100, 10_000, 9_500)
                .unwrap()
                .unwrap();
        assert_eq!(none.start_unix_seconds, 2 * HOUR_SECONDS);
    }

    #[test]
    fn zero_tvl_gate_keeps_exact_raw_window_when_continuous_tvl_is_unavailable() {
        let rows = [HourlyMarketCoverageV1 {
            bucket_unix_seconds: 0,
            pair: "pair".to_owned(),
            eligible: true,
            ordered_events_complete: true,
            historical_prices_complete: false,
            factual_tvl_usd_micros: token(0),
        }];
        let window = longest_common_window_ending_at_cutoff(&rows, 10, 10_000, 0)
            .unwrap()
            .unwrap();
        assert_eq!(window.covered_hour_buckets, 1);
        assert_eq!(window.hourly_coverage[0].market_coverage_bps, 10_000);
        assert_eq!(window.hourly_coverage[0].tvl_coverage_bps, 0);
        assert!(longest_common_window_ending_at_cutoff(&rows, 10, 10_000, 1)
            .unwrap()
            .is_none());
    }

    #[test]
    fn splits_are_ordered_and_holdout_ends_at_cutoff() {
        let window = CommonWindowV1 {
            schema_version: 1,
            start_unix_seconds: 0,
            end_unix_seconds_exclusive: 10_000,
            covered_hour_buckets: 3,
            required_market_coverage_bps: 9_000,
            required_tvl_coverage_bps: 9_500,
            hourly_coverage: Vec::new(),
        };
        let split = chronological_splits(&window, 6_000, 2_000, 2_000).unwrap();
        assert_eq!(split.training_end_unix_seconds_exclusive, 6_000);
        assert_eq!(split.validation_end_unix_seconds_exclusive, 8_000);
        assert_eq!(split.holdout_end_unix_seconds_exclusive, 10_000);
    }
}

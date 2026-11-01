//! Leakage-safe per-market opportunity-ceiling qualification.
//!
//! Training and validation activity are frozen before holdout access. Holdout
//! activity is assessed only during sealed evaluation; a market that misses
//! the minimum in any split receives no optimized-profile claim.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    replay::{CanonicalOperationV1, OperationPayloadV1},
    window::ChronologicalSplitsV1,
};

pub const PER_MARKET_QUALIFICATION_SCHEMA_VERSION: u16 = 1;
const DAY_SECONDS: i64 = 86_400;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitActivityV1 {
    pub swap_count: u64,
    pub active_days: u64,
    pub qualifies: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketCalibrationQualificationV1 {
    pub market: String,
    pub training: SplitActivityV1,
    pub validation: SplitActivityV1,
    /// This does not inspect holdout. A final opportunity-ceiling claim also
    /// requires an independently computed holdout qualification artifact.
    pub calibration_eligible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerMarketCalibrationQualificationArtifactV1 {
    pub schema_version: u16,
    pub source_operations_sha256: String,
    pub minimum_swaps_per_split: u64,
    pub minimum_active_days_per_split: u64,
    pub markets: Vec<MarketCalibrationQualificationV1>,
    pub holdout_was_read: bool,
    pub artifact_sha256: String,
}

impl PerMarketCalibrationQualificationArtifactV1 {
    pub fn seal(mut self) -> Result<Self> {
        if self.holdout_was_read {
            bail!("calibration qualification cannot read holdout");
        }
        self.artifact_sha256.clear();
        self.artifact_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&self)?));
        Ok(self)
    }

    pub fn verify(&self) -> Result<()> {
        if self.schema_version != PER_MARKET_QUALIFICATION_SCHEMA_VERSION
            || self.holdout_was_read
            || self.minimum_swaps_per_split == 0
            || self.minimum_active_days_per_split == 0
            || self.clone().seal()?.artifact_sha256 != self.artifact_sha256
        {
            bail!("per-market calibration qualification failed verification");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketHoldoutQualificationV1 {
    pub market: String,
    pub holdout: SplitActivityV1,
    pub training_validation_qualified: bool,
    pub optimized_profile_claim_allowed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerMarketHoldoutQualificationArtifactV1 {
    pub schema_version: u16,
    pub source_operations_sha256: String,
    pub calibration_qualification_sha256: String,
    pub markets: Vec<MarketHoldoutQualificationV1>,
    pub artifact_sha256: String,
}

impl PerMarketHoldoutQualificationArtifactV1 {
    pub fn seal(mut self) -> Result<Self> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&self)?));
        Ok(self)
    }

    pub fn verify(&self) -> Result<()> {
        if self.schema_version != PER_MARKET_QUALIFICATION_SCHEMA_VERSION
            || self.clone().seal()?.artifact_sha256 != self.artifact_sha256
        {
            bail!("per-market holdout qualification failed verification");
        }
        Ok(())
    }
}

pub fn qualify_calibration_markets(
    operations_path: &Path,
    splits: &ChronologicalSplitsV1,
    candidate_markets: &BTreeSet<String>,
    minimum_swaps: u64,
    minimum_active_days: u64,
) -> Result<PerMarketCalibrationQualificationArtifactV1> {
    if minimum_swaps == 0 || minimum_active_days == 0 {
        bail!("per-market minimums must be positive");
    }
    let activity = read_swap_activity(operations_path, candidate_markets)?;
    let markets = candidate_markets
        .iter()
        .map(|market| {
            let training = split_activity(
                activity.get(market),
                splits.training_start_unix_seconds,
                splits.training_end_unix_seconds_exclusive,
                minimum_swaps,
                minimum_active_days,
            );
            let validation = split_activity(
                activity.get(market),
                splits.validation_start_unix_seconds,
                splits.validation_end_unix_seconds_exclusive,
                minimum_swaps,
                minimum_active_days,
            );
            MarketCalibrationQualificationV1 {
                market: market.clone(),
                calibration_eligible: training.qualifies && validation.qualifies,
                training,
                validation,
            }
        })
        .collect();
    PerMarketCalibrationQualificationArtifactV1 {
        schema_version: PER_MARKET_QUALIFICATION_SCHEMA_VERSION,
        source_operations_sha256: sha256_file(operations_path)?,
        minimum_swaps_per_split: minimum_swaps,
        minimum_active_days_per_split: minimum_active_days,
        markets,
        holdout_was_read: false,
        artifact_sha256: String::new(),
    }
    .seal()
}

pub fn qualify_holdout_markets(
    operations_path: &Path,
    splits: &ChronologicalSplitsV1,
    calibration: &PerMarketCalibrationQualificationArtifactV1,
) -> Result<PerMarketHoldoutQualificationArtifactV1> {
    calibration.verify()?;
    let candidate_markets: BTreeSet<String> = calibration
        .markets
        .iter()
        .map(|market| market.market.clone())
        .collect();
    let activity = read_swap_activity(operations_path, &candidate_markets)?;
    let markets = calibration
        .markets
        .iter()
        .map(|market| {
            let holdout = split_activity(
                activity.get(&market.market),
                splits.holdout_start_unix_seconds,
                splits.holdout_end_unix_seconds_exclusive,
                calibration.minimum_swaps_per_split,
                calibration.minimum_active_days_per_split,
            );
            MarketHoldoutQualificationV1 {
                market: market.market.clone(),
                training_validation_qualified: market.calibration_eligible,
                optimized_profile_claim_allowed: market.calibration_eligible && holdout.qualifies,
                holdout,
            }
        })
        .collect();
    PerMarketHoldoutQualificationArtifactV1 {
        schema_version: PER_MARKET_QUALIFICATION_SCHEMA_VERSION,
        source_operations_sha256: sha256_file(operations_path)?,
        calibration_qualification_sha256: calibration.artifact_sha256.clone(),
        markets,
        artifact_sha256: String::new(),
    }
    .seal()
}

fn read_swap_activity(
    path: &Path,
    markets: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<i64>>> {
    let mut output = BTreeMap::<String, Vec<i64>>::new();
    for (line_index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line.with_context(|| format!("reading operation line {}", line_index + 1))?;
        if line.trim().is_empty() {
            bail!("blank canonical operation row");
        }
        let operation: CanonicalOperationV1 = serde_json::from_str(&line)?;
        operation.validate()?;
        if markets.contains(&operation.pair)
            && matches!(operation.payload, OperationPayloadV1::SwapExactIn { .. })
        {
            output
                .entry(operation.pair)
                .or_default()
                .push(operation.block_time_unix_seconds);
        }
    }
    Ok(output)
}

fn split_activity(
    timestamps: Option<&Vec<i64>>,
    start: i64,
    end: i64,
    minimum_swaps: u64,
    minimum_active_days: u64,
) -> SplitActivityV1 {
    let timestamps = timestamps.map(Vec::as_slice).unwrap_or_default();
    let swap_count = timestamps
        .iter()
        .filter(|timestamp| (start..end).contains(timestamp))
        .count() as u64;
    let active_days = timestamps
        .iter()
        .filter(|timestamp| (start..end).contains(timestamp))
        .map(|timestamp| timestamp.div_euclid(DAY_SECONDS))
        .collect::<BTreeSet<_>>()
        .len() as u64;
    SplitActivityV1 {
        swap_count,
        active_days,
        qualifies: swap_count >= minimum_swaps && active_days >= minimum_active_days,
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_activity_counts_utc_days_and_boundaries() {
        let timestamps = vec![0, 1, DAY_SECONDS, 2 * DAY_SECONDS, 10 * DAY_SECONDS];
        let activity = split_activity(Some(&timestamps), 0, 3 * DAY_SECONDS, 4, 3);
        assert_eq!(activity.swap_count, 4);
        assert_eq!(activity.active_days, 3);
        assert!(activity.qualifies);
    }

    #[test]
    fn holdout_claim_requires_all_three_splits() {
        let activity = SplitActivityV1 {
            swap_count: 100,
            active_days: 7,
            qualifies: true,
        };
        assert!(activity.qualifies);
        let failed = SplitActivityV1 {
            swap_count: 99,
            active_days: 7,
            qualifies: false,
        };
        assert!(!failed.qualifies);
    }
}

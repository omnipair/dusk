//! Physical calibration-prefix sealing.
//!
//! Calibration opens only the training and validation prefixes produced here.
//! Holdout rows are not copied to a calibration-readable file; their exact
//! line bytes are committed in the self-hashed split manifest and the original
//! canonical dataset remains inaccessible to the calibration runner.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    canonical::HistoricalPriceMarkV1,
    replay::CanonicalOperationV1,
    transfer_fee::{TokenProgramKindV1, TransferFeeRegistryV1},
    v1_reconciliation::V1ReplayLedgerRowV1,
    window::ChronologicalSplitsV1,
};

pub const DATASET_SPLIT_SCHEMA_VERSION: u16 = 1;
const MANIFEST_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:dataset-split:v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetSegmentProofV1 {
    pub row_count: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileProofV1 {
    pub byte_length: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationPrefixV1 {
    pub operations_file_name: String,
    pub operations: DatasetSegmentProofV1,
    pub factual_ledger_file_name: String,
    pub factual_ledger: DatasetSegmentProofV1,
    pub historical_prices_file_name: String,
    pub historical_prices: DatasetSegmentProofV1,
    pub transfer_fee_registry_file_name: String,
    pub transfer_fee_registry: FileProofV1,
    pub max_slot_inclusive: u64,
    pub end_unix_seconds_exclusive: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChronologicalDatasetSplitManifestV1 {
    pub schema_version: u16,
    pub source_operations_sha256: String,
    pub source_factual_ledger_sha256: String,
    pub source_historical_prices_sha256: String,
    pub source_transfer_fee_registry_sha256: String,
    pub splits: ChronologicalSplitsV1,
    pub training_prefix: CalibrationPrefixV1,
    /// Includes training state followed by validation rows, so native replay
    /// reaches the validation boundary without importing an artificial state.
    pub validation_prefix: CalibrationPrefixV1,
    pub sealed_holdout_operations: DatasetSegmentProofV1,
    pub sealed_holdout_factual_ledger: DatasetSegmentProofV1,
    pub sealed_holdout_historical_prices: DatasetSegmentProofV1,
    pub sealed_holdout_transfer_fee_epochs: DatasetSegmentProofV1,
    pub holdout_opened_by_calibration: bool,
    pub manifest_sha256: String,
}

impl ChronologicalDatasetSplitManifestV1 {
    pub fn seal(mut self) -> Result<Self> {
        if self.holdout_opened_by_calibration {
            bail!("holdout cannot be marked opened before profile freeze");
        }
        self.manifest_sha256.clear();
        let bytes = serde_json::to_vec(&self)?;
        let mut digest = Sha256::new();
        digest.update(MANIFEST_DOMAIN);
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
        self.manifest_sha256 = hex::encode(digest.finalize());
        Ok(self)
    }

    pub fn verify(&self, directory: &Path) -> Result<()> {
        if self.schema_version != DATASET_SPLIT_SCHEMA_VERSION
            || self.holdout_opened_by_calibration
            || self.clone().seal()?.manifest_sha256 != self.manifest_sha256
        {
            bail!("chronological dataset split manifest is invalid");
        }
        for prefix in [&self.training_prefix, &self.validation_prefix] {
            verify_file(
                &directory.join(&prefix.operations_file_name),
                &prefix.operations,
            )?;
            verify_file(
                &directory.join(&prefix.factual_ledger_file_name),
                &prefix.factual_ledger,
            )?;
            verify_file(
                &directory.join(&prefix.historical_prices_file_name),
                &prefix.historical_prices,
            )?;
            verify_file_proof(
                &directory.join(&prefix.transfer_fee_registry_file_name),
                &prefix.transfer_fee_registry,
            )?;
        }
        Ok(())
    }
}

pub fn create_chronological_calibration_prefixes(
    source_operations: &Path,
    source_factual_ledger: &Path,
    source_historical_prices: &Path,
    source_transfer_fee_registry: &Path,
    output_directory: &Path,
    splits: &ChronologicalSplitsV1,
) -> Result<ChronologicalDatasetSplitManifestV1> {
    fs::create_dir_all(output_directory)?;
    let training_operations = output_directory.join("training-prefix-operations.jsonl");
    let validation_operations = output_directory.join("validation-prefix-operations.jsonl");
    let training_ledger = output_directory.join("training-prefix-v1-ledger.jsonl");
    let validation_ledger = output_directory.join("validation-prefix-v1-ledger.jsonl");
    let training_prices = output_directory.join("training-prefix-historical-prices.jsonl");
    let validation_prices = output_directory.join("validation-prefix-historical-prices.jsonl");
    let training_transfer_fees = output_directory.join("training-prefix-transfer-fees.json");
    let validation_transfer_fees = output_directory.join("validation-prefix-transfer-fees.json");
    for path in [
        &training_operations,
        &validation_operations,
        &training_ledger,
        &validation_ledger,
        &training_prices,
        &validation_prices,
        &training_transfer_fees,
        &validation_transfer_fees,
    ] {
        if path.exists()
            || partial_path(path).exists()
            || path.with_extension("json.partial").exists()
        {
            bail!(
                "refusing to overwrite chronological prefix {}",
                path.display()
            );
        }
    }

    let mut operation_times = BTreeMap::<String, i64>::new();
    let mut training_operation_writer = AtomicJsonlWriter::create(&training_operations)?;
    let mut validation_operation_writer = AtomicJsonlWriter::create(&validation_operations)?;
    let mut sealed_holdout_operations = SegmentHasher::default();
    let mut training_max_slot = None::<u64>;
    let mut validation_max_slot = None::<u64>;
    let mut previous_order = None;
    for (line_index, line) in BufReader::new(File::open(source_operations)?)
        .lines()
        .enumerate()
    {
        let line = line?;
        if line.trim().is_empty() {
            bail!("blank canonical operation at line {}", line_index + 1);
        }
        let operation: CanonicalOperationV1 = serde_json::from_str(&line)?;
        operation.validate()?;
        if previous_order.is_some_and(|order| order >= operation.order) {
            bail!("canonical operation source is not in strict order");
        }
        previous_order = Some(operation.order);
        if operation_times
            .insert(
                operation.operation_id.clone(),
                operation.block_time_unix_seconds,
            )
            .is_some()
        {
            bail!("duplicate canonical operation ID");
        }
        let bytes = canonical_line_bytes(&operation)?;
        if operation.block_time_unix_seconds < splits.training_end_unix_seconds_exclusive {
            training_operation_writer.write(&bytes)?;
            training_max_slot = Some(
                training_max_slot
                    .unwrap_or(operation.order.slot)
                    .max(operation.order.slot),
            );
        }
        if operation.block_time_unix_seconds < splits.validation_end_unix_seconds_exclusive {
            validation_operation_writer.write(&bytes)?;
            validation_max_slot = Some(
                validation_max_slot
                    .unwrap_or(operation.order.slot)
                    .max(operation.order.slot),
            );
        } else {
            sealed_holdout_operations.observe(&bytes)?;
        }
    }
    let training_operations_proof = training_operation_writer.commit()?;
    let validation_operations_proof = validation_operation_writer.commit()?;

    let mut training_ledger_writer = AtomicJsonlWriter::create(&training_ledger)?;
    let mut validation_ledger_writer = AtomicJsonlWriter::create(&validation_ledger)?;
    let mut sealed_holdout_ledger = SegmentHasher::default();
    for (line_index, line) in BufReader::new(File::open(source_factual_ledger)?)
        .lines()
        .enumerate()
    {
        let line = line?;
        if line.trim().is_empty() {
            bail!("blank factual ledger row at line {}", line_index + 1);
        }
        let row: V1ReplayLedgerRowV1 = serde_json::from_str(&line)?;
        let block_time = operation_times
            .get(&row.operation_id)
            .with_context(|| format!("factual row {} has no operation", row.operation_id))?;
        let bytes = canonical_line_bytes(&row)?;
        if *block_time < splits.training_end_unix_seconds_exclusive {
            training_ledger_writer.write(&bytes)?;
        }
        if *block_time < splits.validation_end_unix_seconds_exclusive {
            validation_ledger_writer.write(&bytes)?;
        } else {
            sealed_holdout_ledger.observe(&bytes)?;
        }
    }
    let training_ledger_proof = training_ledger_writer.commit()?;
    let validation_ledger_proof = validation_ledger_writer.commit()?;

    let mut training_price_writer = AtomicJsonlWriter::create(&training_prices)?;
    let mut validation_price_writer = AtomicJsonlWriter::create(&validation_prices)?;
    let mut sealed_holdout_prices = SegmentHasher::default();
    for (line_index, line) in BufReader::new(File::open(source_historical_prices)?)
        .lines()
        .enumerate()
    {
        let line = line?;
        if line.trim().is_empty() {
            bail!("blank historical price row at line {}", line_index + 1);
        }
        let mark: HistoricalPriceMarkV1 = serde_json::from_str(&line)?;
        mark.validate()?;
        let bytes = canonical_line_bytes(&mark)?;
        if mark.bucket_unix_seconds < splits.training_end_unix_seconds_exclusive {
            training_price_writer.write(&bytes)?;
        }
        if mark.bucket_unix_seconds < splits.validation_end_unix_seconds_exclusive {
            validation_price_writer.write(&bytes)?;
        } else {
            sealed_holdout_prices.observe(&bytes)?;
        }
    }
    let training_price_proof = training_price_writer.commit()?;
    let validation_price_proof = validation_price_writer.commit()?;

    let full_transfer_registry: TransferFeeRegistryV1 =
        serde_json::from_reader(BufReader::new(File::open(source_transfer_fee_registry)?))?;
    full_transfer_registry.clone().validate()?;
    let training_max_slot = training_max_slot.context("training prefix has no operation")?;
    let validation_max_slot = validation_max_slot.context("validation prefix has no operation")?;
    let training_registry = truncate_transfer_registry(&full_transfer_registry, training_max_slot);
    let validation_registry =
        truncate_transfer_registry(&full_transfer_registry, validation_max_slot);
    training_registry.clone().validate()?;
    validation_registry.clone().validate()?;
    write_json_atomic(&training_transfer_fees, &training_registry)?;
    write_json_atomic(&validation_transfer_fees, &validation_registry)?;
    let training_transfer_fee_proof = file_proof(&training_transfer_fees)?;
    let validation_transfer_fee_proof = file_proof(&validation_transfer_fees)?;
    let mut sealed_holdout_transfer_fee_epochs = SegmentHasher::default();
    for schedule in &full_transfer_registry.mints {
        if schedule.token_program == TokenProgramKindV1::LegacyToken {
            continue;
        }
        for epoch in &schedule.epochs {
            if epoch.source_slot > validation_max_slot {
                sealed_holdout_transfer_fee_epochs.observe(&canonical_line_bytes(epoch)?)?;
            }
        }
    }

    Ok(ChronologicalDatasetSplitManifestV1 {
        schema_version: DATASET_SPLIT_SCHEMA_VERSION,
        source_operations_sha256: sha256_file(source_operations)?,
        source_factual_ledger_sha256: sha256_file(source_factual_ledger)?,
        source_historical_prices_sha256: sha256_file(source_historical_prices)?,
        source_transfer_fee_registry_sha256: sha256_file(source_transfer_fee_registry)?,
        splits: splits.clone(),
        training_prefix: CalibrationPrefixV1 {
            operations_file_name: file_name(&training_operations)?,
            operations: training_operations_proof,
            factual_ledger_file_name: file_name(&training_ledger)?,
            factual_ledger: training_ledger_proof,
            historical_prices_file_name: file_name(&training_prices)?,
            historical_prices: training_price_proof,
            transfer_fee_registry_file_name: file_name(&training_transfer_fees)?,
            transfer_fee_registry: training_transfer_fee_proof,
            max_slot_inclusive: training_max_slot,
            end_unix_seconds_exclusive: splits.training_end_unix_seconds_exclusive,
        },
        validation_prefix: CalibrationPrefixV1 {
            operations_file_name: file_name(&validation_operations)?,
            operations: validation_operations_proof,
            factual_ledger_file_name: file_name(&validation_ledger)?,
            factual_ledger: validation_ledger_proof,
            historical_prices_file_name: file_name(&validation_prices)?,
            historical_prices: validation_price_proof,
            transfer_fee_registry_file_name: file_name(&validation_transfer_fees)?,
            transfer_fee_registry: validation_transfer_fee_proof,
            max_slot_inclusive: validation_max_slot,
            end_unix_seconds_exclusive: splits.validation_end_unix_seconds_exclusive,
        },
        sealed_holdout_operations: sealed_holdout_operations.finish(),
        sealed_holdout_factual_ledger: sealed_holdout_ledger.finish(),
        sealed_holdout_historical_prices: sealed_holdout_prices.finish(),
        sealed_holdout_transfer_fee_epochs: sealed_holdout_transfer_fee_epochs.finish(),
        holdout_opened_by_calibration: false,
        manifest_sha256: String::new(),
    }
    .seal()?)
}

#[derive(Default)]
struct SegmentHasher {
    digest: Sha256,
    rows: u64,
}

impl SegmentHasher {
    fn observe(&mut self, bytes: &[u8]) -> Result<()> {
        self.digest.update(bytes);
        self.rows = self.rows.checked_add(1).context("row count overflow")?;
        Ok(())
    }

    fn finish(self) -> DatasetSegmentProofV1 {
        DatasetSegmentProofV1 {
            row_count: self.rows,
            sha256: hex::encode(self.digest.finalize()),
        }
    }
}

struct AtomicJsonlWriter {
    writer: BufWriter<File>,
    final_path: PathBuf,
    partial_path: PathBuf,
    hasher: SegmentHasher,
}

impl AtomicJsonlWriter {
    fn create(path: &Path) -> Result<Self> {
        let partial_path = partial_path(path);
        Ok(Self {
            writer: BufWriter::new(File::create(&partial_path)?),
            final_path: path.to_path_buf(),
            partial_path,
            hasher: SegmentHasher::default(),
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.hasher.observe(bytes)
    }

    fn commit(mut self) -> Result<DatasetSegmentProofV1> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        drop(self.writer);
        fs::rename(&self.partial_path, &self.final_path)?;
        Ok(self.hasher.finish())
    }
}

fn canonical_line_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn verify_file(path: &Path, expected: &DatasetSegmentProofV1) -> Result<()> {
    let mut rows = 0_u64;
    for line in BufReader::new(File::open(path)?).lines() {
        line?;
        rows = rows.checked_add(1).context("row count overflow")?;
    }
    if rows != expected.row_count || sha256_file(path)? != expected.sha256 {
        bail!("calibration prefix differs from its sealed proof");
    }
    Ok(())
}

fn truncate_transfer_registry(
    full: &TransferFeeRegistryV1,
    max_slot_inclusive: u64,
) -> TransferFeeRegistryV1 {
    let mut truncated = full.clone();
    for schedule in &mut truncated.mints {
        if schedule.token_program != TokenProgramKindV1::LegacyToken {
            schedule
                .epochs
                .retain(|epoch| epoch.source_slot <= max_slot_inclusive);
        }
    }
    truncated
        .mints
        .retain(|schedule| !schedule.epochs.is_empty());
    truncated
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let partial = path.with_extension("json.partial");
    let mut file = File::create(&partial)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(partial, path)?;
    Ok(())
}

fn file_proof(path: &Path) -> Result<FileProofV1> {
    Ok(FileProofV1 {
        byte_length: fs::metadata(path)?.len(),
        sha256: sha256_file(path)?,
    })
}

fn verify_file_proof(path: &Path, expected: &FileProofV1) -> Result<()> {
    if fs::metadata(path)?.len() != expected.byte_length || sha256_file(path)? != expected.sha256 {
        bail!("calibration file differs from its sealed proof");
    }
    Ok(())
}

fn partial_path(path: &Path) -> PathBuf {
    path.with_extension("jsonl.partial")
}

fn file_name(path: &Path) -> Result<String> {
    Ok(path
        .file_name()
        .context("calibration prefix has no file name")?
        .to_string_lossy()
        .into_owned())
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
    use crate::{
        canonical::TokenAmount,
        transfer_fee::{EpochScheduleV1, MintTransferFeeScheduleV1, TransferFeeEpochV1},
    };

    fn fee_epoch(effective_epoch: u64, source_slot: u64) -> TransferFeeEpochV1 {
        TransferFeeEpochV1 {
            effective_epoch,
            transfer_fee_basis_points: 0,
            maximum_fee: TokenAmount::new("0".to_owned()).unwrap(),
            source_signature: format!("source-{source_slot}"),
            source_slot,
            decoded_instruction_sha256: "a".repeat(64),
        }
    }

    #[test]
    fn legacy_zero_fee_proof_is_timeless_but_token_2022_mutations_are_truncated() {
        let full = TransferFeeRegistryV1 {
            schema_version: 1,
            epoch_schedule: EpochScheduleV1 {
                slots_per_epoch: 432_000,
                leader_schedule_slot_offset: 432_000,
                warmup: false,
                first_normal_epoch: 0,
                first_normal_slot: 0,
                source_response_sha256: "b".repeat(64),
            },
            mints: vec![
                MintTransferFeeScheduleV1 {
                    schema_version: 1,
                    mint: "legacy".to_owned(),
                    token_program: TokenProgramKindV1::LegacyToken,
                    epochs: vec![fee_epoch(0, 1_000)],
                },
                MintTransferFeeScheduleV1 {
                    schema_version: 1,
                    mint: "token-2022".to_owned(),
                    token_program: TokenProgramKindV1::Token2022,
                    epochs: vec![fee_epoch(0, 0), fee_epoch(1, 1_000)],
                },
            ],
        };

        let truncated = truncate_transfer_registry(&full, 10);
        assert_eq!(truncated.mints[0].epochs, full.mints[0].epochs);
        assert_eq!(truncated.mints[1].epochs, vec![fee_epoch(0, 0)]);
    }

    #[test]
    fn manifest_rejects_holdout_open_flag() {
        let split = ChronologicalDatasetSplitManifestV1 {
            schema_version: DATASET_SPLIT_SCHEMA_VERSION,
            source_operations_sha256: "0".repeat(64),
            source_factual_ledger_sha256: "1".repeat(64),
            source_historical_prices_sha256: "2".repeat(64),
            source_transfer_fee_registry_sha256: "3".repeat(64),
            splits: ChronologicalSplitsV1 {
                schema_version: 1,
                training_start_unix_seconds: 0,
                training_end_unix_seconds_exclusive: 60,
                validation_start_unix_seconds: 60,
                validation_end_unix_seconds_exclusive: 80,
                holdout_start_unix_seconds: 80,
                holdout_end_unix_seconds_exclusive: 100,
            },
            training_prefix: CalibrationPrefixV1 {
                operations_file_name: "a".to_owned(),
                operations: DatasetSegmentProofV1 {
                    row_count: 0,
                    sha256: hex::encode(Sha256::digest([])),
                },
                factual_ledger_file_name: "b".to_owned(),
                factual_ledger: DatasetSegmentProofV1 {
                    row_count: 0,
                    sha256: hex::encode(Sha256::digest([])),
                },
                historical_prices_file_name: "prices-a".to_owned(),
                historical_prices: DatasetSegmentProofV1 {
                    row_count: 0,
                    sha256: hex::encode(Sha256::digest([])),
                },
                transfer_fee_registry_file_name: "fees-a".to_owned(),
                transfer_fee_registry: FileProofV1 {
                    byte_length: 0,
                    sha256: hex::encode(Sha256::digest([])),
                },
                max_slot_inclusive: 10,
                end_unix_seconds_exclusive: 60,
            },
            validation_prefix: CalibrationPrefixV1 {
                operations_file_name: "c".to_owned(),
                operations: DatasetSegmentProofV1 {
                    row_count: 0,
                    sha256: hex::encode(Sha256::digest([])),
                },
                factual_ledger_file_name: "d".to_owned(),
                factual_ledger: DatasetSegmentProofV1 {
                    row_count: 0,
                    sha256: hex::encode(Sha256::digest([])),
                },
                historical_prices_file_name: "prices-b".to_owned(),
                historical_prices: DatasetSegmentProofV1 {
                    row_count: 0,
                    sha256: hex::encode(Sha256::digest([])),
                },
                transfer_fee_registry_file_name: "fees-b".to_owned(),
                transfer_fee_registry: FileProofV1 {
                    byte_length: 0,
                    sha256: hex::encode(Sha256::digest([])),
                },
                max_slot_inclusive: 20,
                end_unix_seconds_exclusive: 80,
            },
            sealed_holdout_operations: DatasetSegmentProofV1 {
                row_count: 1,
                sha256: "2".repeat(64),
            },
            sealed_holdout_factual_ledger: DatasetSegmentProofV1 {
                row_count: 1,
                sha256: "3".repeat(64),
            },
            sealed_holdout_historical_prices: DatasetSegmentProofV1 {
                row_count: 1,
                sha256: "4".repeat(64),
            },
            sealed_holdout_transfer_fee_epochs: DatasetSegmentProofV1 {
                row_count: 1,
                sha256: "5".repeat(64),
            },
            holdout_opened_by_calibration: true,
            manifest_sha256: String::new(),
        };
        assert!(split.seal().is_err());
    }
}

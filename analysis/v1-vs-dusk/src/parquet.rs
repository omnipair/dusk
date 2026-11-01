//! Deterministic, declared-schema Parquet exports of benchmark JSONL artifacts.
//!
//! Canonical JSONL remains the source of truth. This module never infers Arrow
//! types from JSON values: each supported dataset has a fixed V1 schema and a
//! strict typed deserializer. Variant/nested payloads are stored as canonical
//! UTF-8 JSON alongside normalized identity/order columns, preserving every
//! `TokenAmount` and `SignedTokenAmount` as its exact decimal string.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use arrow_array::{
    builder::{
        BooleanBuilder, Int64Builder, StringBuilder, UInt16Builder, UInt32Builder, UInt64Builder,
        UInt8Builder,
    },
    ArrayRef, RecordBatch,
};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use parquet::{
    arrow::{arrow_reader::ParquetRecordBatchReaderBuilder, ArrowWriter},
    basic::{Compression, ZstdLevel},
    errors::ParquetError,
    file::properties::{EnabledStatistics, WriterProperties, WriterVersion},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    canonical::{
        CanonicalEventV1, ChainOrder, ConfigScope, ConfigVersionV1, HistoricalPriceMarkV1,
        CANONICAL_SCHEMA_VERSION,
    },
    counterfactual::{CounterfactualOperationRecordV1, COUNTERFACTUAL_SCHEMA_VERSION},
    metric_engine::{PairedOperationMetricRowV1, PAIRED_METRIC_SCHEMA_VERSION},
    replay::CanonicalOperationV1,
    v1_reconciliation::{V1ReplayLedgerRowV1, V1_RECONCILIATION_SCHEMA_VERSION},
};

pub const PARQUET_EXPORT_SCHEMA_VERSION: u16 = 1;
pub const PARQUET_ROW_GROUP_ROWS: usize = 4_096;
pub const PARQUET_WRITE_BATCH_ROWS: usize = 1_024;
pub const PARQUET_ZSTD_LEVEL: i32 = 3;
const PARQUET_CREATED_BY: &str = "omnipair-v1-vs-dusk-parquet-v1";
const SCHEMA_FINGERPRINT_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:parquet-schema:v1";
const MANIFEST_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:parquet-manifest:v1";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkParquetDatasetV1 {
    CanonicalOperations,
    CanonicalEvents,
    ConfigurationHistory,
    HistoricalPrices,
    V1ReplayLedger,
    CounterfactualReplayLedger,
    PairedMetricLedger,
}

impl BenchmarkParquetDatasetV1 {
    pub fn logical_name(self) -> &'static str {
        match self {
            Self::CanonicalOperations => "canonical_operations",
            Self::CanonicalEvents => "canonical_events",
            Self::ConfigurationHistory => "configuration_history",
            Self::HistoricalPrices => "historical_prices",
            Self::V1ReplayLedger => "v1_replay_ledger",
            Self::CounterfactualReplayLedger => "counterfactual_replay_ledger",
            Self::PairedMetricLedger => "paired_metric_ledger",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeterministicParquetSettingsV1 {
    pub writer_version: String,
    pub compression: String,
    pub compression_level: i32,
    pub row_group_rows: u64,
    pub write_batch_rows: u64,
    pub dictionary_enabled: bool,
    pub statistics: String,
    pub created_by: String,
}

impl Default for DeterministicParquetSettingsV1 {
    fn default() -> Self {
        Self {
            writer_version: "parquet_2_0".to_owned(),
            compression: "zstd".to_owned(),
            compression_level: PARQUET_ZSTD_LEVEL,
            row_group_rows: PARQUET_ROW_GROUP_ROWS as u64,
            write_batch_rows: PARQUET_WRITE_BATCH_ROWS as u64,
            dictionary_enabled: true,
            statistics: "chunk".to_owned(),
            created_by: PARQUET_CREATED_BY.to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParquetFileManifestV1 {
    pub schema_version: u16,
    pub dataset: BenchmarkParquetDatasetV1,
    pub logical_name: String,
    pub source_file_name: String,
    pub source_byte_length: u64,
    pub source_jsonl_sha256: String,
    pub parquet_file_name: String,
    pub parquet_byte_length: u64,
    pub parquet_sha256: String,
    pub row_count: u64,
    pub schema_fingerprint_sha256: String,
    pub settings: DeterministicParquetSettingsV1,
    pub manifest_sha256: String,
}

impl ParquetFileManifestV1 {
    fn seal(mut self) -> Result<Self, ParquetExportError> {
        self.manifest_sha256.clear();
        self.manifest_sha256 = self.compute_manifest_sha256()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), ParquetExportError> {
        if self.schema_version != PARQUET_EXPORT_SCHEMA_VERSION {
            return Err(ParquetExportError::Manifest(format!(
                "unsupported manifest schema version {}",
                self.schema_version
            )));
        }
        if self.logical_name.trim().is_empty()
            || self.source_file_name.trim().is_empty()
            || self.parquet_file_name.trim().is_empty()
        {
            return Err(ParquetExportError::Manifest(
                "manifest identity fields must be non-empty".to_owned(),
            ));
        }
        for (field, digest) in [
            ("source_jsonl_sha256", &self.source_jsonl_sha256),
            ("parquet_sha256", &self.parquet_sha256),
            ("schema_fingerprint_sha256", &self.schema_fingerprint_sha256),
            ("manifest_sha256", &self.manifest_sha256),
        ] {
            if !is_canonical_sha256(digest) {
                return Err(ParquetExportError::Manifest(format!(
                    "{field} is not a canonical SHA-256"
                )));
            }
        }
        if self.settings != DeterministicParquetSettingsV1::default() {
            return Err(ParquetExportError::Manifest(
                "writer settings differ from the deterministic V1 profile".to_owned(),
            ));
        }
        if self.schema_fingerprint_sha256 != declared_schema_fingerprint(self.dataset) {
            return Err(ParquetExportError::Manifest(
                "declared schema fingerprint mismatch".to_owned(),
            ));
        }
        if self.manifest_sha256 != self.compute_manifest_sha256()? {
            return Err(ParquetExportError::Manifest(
                "manifest self-hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    fn compute_manifest_sha256(&self) -> Result<String, ParquetExportError> {
        let mut unsigned = self.clone();
        unsigned.manifest_sha256.clear();
        let bytes = serde_json::to_vec(&unsigned)?;
        let mut digest = Sha256::new();
        digest.update(MANIFEST_HASH_DOMAIN);
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
        Ok(hex::encode(digest.finalize()))
    }
}

#[derive(Clone, Debug)]
pub struct ParquetExportSpecV1<'a> {
    pub dataset: BenchmarkParquetDatasetV1,
    pub logical_name: &'a str,
    pub source_jsonl: &'a Path,
    pub parquet_output: &'a Path,
    pub manifest_output: &'a Path,
}

#[derive(Debug, Error)]
pub enum ParquetExportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Arrow error: {0}")]
    Arrow(#[from] ArrowError),
    #[error("Parquet error: {0}")]
    Parquet(#[from] ParquetError),
    #[error("blank JSONL record at line {line}")]
    BlankLine { line: usize },
    #[error("invalid {dataset:?} JSONL record at line {line}: {detail}")]
    InvalidRecord {
        dataset: BenchmarkParquetDatasetV1,
        line: usize,
        detail: String,
    },
    #[error("schema drift in {dataset:?} JSONL record at line {line}")]
    SchemaDrift {
        dataset: BenchmarkParquetDatasetV1,
        line: usize,
    },
    #[error("{dataset:?} JSONL is not in strict declared row order at line {line}")]
    NonCanonicalOrder {
        dataset: BenchmarkParquetDatasetV1,
        line: usize,
    },
    #[error("declared row does not match its Parquet column schema")]
    InternalSchemaMismatch,
    #[error("source, Parquet, and manifest paths must be distinct")]
    PathCollision,
    #[error("output path has no file name: {0}")]
    MissingFileName(PathBuf),
    #[error("manifest validation failed: {0}")]
    Manifest(String),
    #[error("Parquet artifact does not match its manifest: {0}")]
    ArtifactMismatch(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColumnKind {
    Utf8,
    Boolean,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
}

impl ColumnKind {
    fn arrow_type(self) -> DataType {
        match self {
            Self::Utf8 => DataType::Utf8,
            Self::Boolean => DataType::Boolean,
            Self::Int64 => DataType::Int64,
            Self::UInt8 => DataType::UInt8,
            Self::UInt16 => DataType::UInt16,
            Self::UInt32 => DataType::UInt32,
            Self::UInt64 => DataType::UInt64,
        }
    }

    fn fingerprint_name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf8",
            Self::Boolean => "bool",
            Self::Int64 => "i64",
            Self::UInt8 => "u8",
            Self::UInt16 => "u16",
            Self::UInt32 => "u32",
            Self::UInt64 => "u64",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ColumnSpec {
    name: &'static str,
    kind: ColumnKind,
    nullable: bool,
}

const fn column(name: &'static str, kind: ColumnKind, nullable: bool) -> ColumnSpec {
    ColumnSpec {
        name,
        kind,
        nullable,
    }
}

const OPERATION_COLUMNS: &[ColumnSpec] = &[
    column("record_schema_version", ColumnKind::UInt16, false),
    column("operation_id", ColumnKind::Utf8, false),
    column("signature", ColumnKind::Utf8, false),
    column("pair", ColumnKind::Utf8, false),
    column("actor", ColumnKind::Utf8, true),
    column("slot", ColumnKind::UInt64, false),
    column("transaction_index", ColumnKind::UInt32, false),
    column("outer_instruction_index", ColumnKind::UInt32, false),
    column("inner_execution_index", ColumnKind::UInt32, true),
    column("event_sequence", ColumnKind::UInt32, false),
    column("block_time_unix_seconds", ColumnKind::Int64, false),
    column("decoder_version_id", ColumnKind::Utf8, false),
    column("payload_kind", ColumnKind::Utf8, false),
    column("payload_json", ColumnKind::Utf8, false),
    column("assertions_json", ColumnKind::Utf8, false),
    column("record_json", ColumnKind::Utf8, false),
];

const EVENT_COLUMNS: &[ColumnSpec] = &[
    column("record_schema_version", ColumnKind::UInt16, false),
    column("event_id", ColumnKind::Utf8, false),
    column("cluster", ColumnKind::Utf8, false),
    column("program_id", ColumnKind::Utf8, false),
    column("pair", ColumnKind::Utf8, true),
    column("actor", ColumnKind::Utf8, true),
    column("signature", ColumnKind::Utf8, false),
    column("slot", ColumnKind::UInt64, false),
    column("transaction_index", ColumnKind::UInt32, false),
    column("outer_instruction_index", ColumnKind::UInt32, false),
    column("inner_execution_index", ColumnKind::UInt32, true),
    column("event_sequence", ColumnKind::UInt32, false),
    column("execution_path_json", ColumnKind::Utf8, false),
    column("block_time_unix_seconds", ColumnKind::Int64, false),
    column("decoder_version_id", ColumnKind::Utf8, false),
    column("raw_payload_sha256", ColumnKind::Utf8, false),
    column("db_source_table", ColumnKind::Utf8, true),
    column("db_source_row_id", ColumnKind::Utf8, true),
    column("payload_kind", ColumnKind::Utf8, false),
    column("payload_json", ColumnKind::Utf8, false),
    column("record_json", ColumnKind::Utf8, false),
];

const CONFIG_COLUMNS: &[ColumnSpec] = &[
    column("record_schema_version", ColumnKind::UInt16, false),
    column("config_id", ColumnKind::Utf8, false),
    column("scope", ColumnKind::Utf8, false),
    column("entity_address", ColumnKind::Utf8, false),
    column("valid_from_slot", ColumnKind::UInt64, false),
    column("valid_from_transaction_index", ColumnKind::UInt32, false),
    column(
        "valid_from_outer_instruction_index",
        ColumnKind::UInt32,
        false,
    ),
    column("valid_from_inner_execution_index", ColumnKind::UInt32, true),
    column("valid_from_event_sequence", ColumnKind::UInt32, false),
    column("valid_to_slot", ColumnKind::UInt64, true),
    column("valid_to_transaction_index", ColumnKind::UInt32, true),
    column("valid_to_outer_instruction_index", ColumnKind::UInt32, true),
    column("valid_to_inner_execution_index", ColumnKind::UInt32, true),
    column("valid_to_event_sequence", ColumnKind::UInt32, true),
    column("source_signature", ColumnKind::Utf8, false),
    column("decoder_version_id", ColumnKind::Utf8, false),
    column("fields_json", ColumnKind::Utf8, false),
    column("record_json", ColumnKind::Utf8, false),
];

const PRICE_COLUMNS: &[ColumnSpec] = &[
    column("record_schema_version", ColumnKind::UInt16, false),
    column("mint", ColumnKind::Utf8, false),
    column("bucket_unix_seconds", ColumnKind::Int64, false),
    column("price_usd", ColumnKind::Utf8, false),
    column("token_decimals", ColumnKind::UInt8, true),
    column("provider", ColumnKind::Utf8, false),
    column("quality", ColumnKind::Utf8, false),
    column("record_json", ColumnKind::Utf8, false),
];

const LEDGER_COLUMNS: &[ColumnSpec] = &[
    column("record_schema_version", ColumnKind::UInt16, false),
    column("market", ColumnKind::Utf8, false),
    column("operation_id", ColumnKind::Utf8, false),
    column("slot", ColumnKind::UInt64, false),
    column("transaction_index", ColumnKind::UInt32, false),
    column("outer_instruction_index", ColumnKind::UInt32, false),
    column("inner_execution_index", ColumnKind::UInt32, true),
    column("event_sequence", ColumnKind::UInt32, false),
    column("broadcast_global_config", ColumnKind::Boolean, false),
    column("reconciliation_count", ColumnKind::UInt64, false),
    column("all_raw_unit_differences_zero", ColumnKind::Boolean, false),
    column("checkpoint_sha256", ColumnKind::Utf8, false),
    column("positions_sha256", ColumnKind::Utf8, false),
    column("reconciliations_sha256", ColumnKind::Utf8, false),
    column("factual_record_sha256", ColumnKind::Utf8, false),
    column("compact_checkpoint_json", ColumnKind::Utf8, false),
    column("conservation_json", ColumnKind::Utf8, false),
    column("revenue_delta_json", ColumnKind::Utf8, false),
    column("record_json", ColumnKind::Utf8, false),
];

// Synthetic arbitrage is emitted at a block boundary while retaining the
// canonical order of the historical operation that anchored it. Therefore
// the physical replay sequence is an explicit derived column rather than a
// fabricated instruction order.
const COUNTERFACTUAL_LEDGER_COLUMNS: &[ColumnSpec] = &[
    column("record_schema_version", ColumnKind::UInt16, false),
    column("replay_sequence", ColumnKind::UInt64, false),
    column("operation_id", ColumnKind::Utf8, false),
    column("market", ColumnKind::Utf8, false),
    column("slot", ColumnKind::UInt64, false),
    column("arm", ColumnKind::Utf8, false),
    column("synthetic_arbitrage", ColumnKind::Boolean, false),
    column("status", ColumnKind::Utf8, false),
    column("state_sha256", ColumnKind::Utf8, false),
    column("checkpoint_json", ColumnKind::Utf8, false),
    column("revenue_json", ColumnKind::Utf8, false),
    column("record_json", ColumnKind::Utf8, false),
];

const PAIRED_METRIC_COLUMNS: &[ColumnSpec] = &[
    column("record_schema_version", ColumnKind::UInt16, false),
    column("replay_sequence", ColumnKind::UInt64, false),
    column("profile_id", ColumnKind::Utf8, false),
    column("market", ColumnKind::Utf8, false),
    column("operation_id", ColumnKind::Utf8, false),
    column("slot", ColumnKind::UInt64, false),
    column("transaction_index", ColumnKind::UInt32, false),
    column("outer_instruction_index", ColumnKind::UInt32, false),
    column("inner_execution_index", ColumnKind::UInt32, true),
    column("event_sequence", ColumnKind::UInt32, false),
    column("block_time_unix_seconds", ColumnKind::Int64, false),
    column("arm", ColumnKind::Utf8, false),
    column("synthetic_arbitrage", ColumnKind::Boolean, false),
    column("dusk_status", ColumnKind::Utf8, false),
    column("has_swap", ColumnKind::Boolean, false),
    column("has_borrow", ColumnKind::Boolean, false),
    column("has_repay", ColumnKind::Boolean, false),
    column("has_safety", ColumnKind::Boolean, false),
    column("factual_record_sha256", ColumnKind::Utf8, false),
    column("dusk_state_sha256", ColumnKind::Utf8, false),
    column("swap_json", ColumnKind::Utf8, true),
    column("borrow_json", ColumnKind::Utf8, true),
    column("repay_json", ColumnKind::Utf8, true),
    column("safety_json", ColumnKind::Utf8, true),
    column("revenue_json", ColumnKind::Utf8, false),
    column("capital_after_json", ColumnKind::Utf8, false),
    column("record_json", ColumnKind::Utf8, false),
];

fn columns(dataset: BenchmarkParquetDatasetV1) -> &'static [ColumnSpec] {
    match dataset {
        BenchmarkParquetDatasetV1::CanonicalOperations => OPERATION_COLUMNS,
        BenchmarkParquetDatasetV1::CanonicalEvents => EVENT_COLUMNS,
        BenchmarkParquetDatasetV1::ConfigurationHistory => CONFIG_COLUMNS,
        BenchmarkParquetDatasetV1::HistoricalPrices => PRICE_COLUMNS,
        BenchmarkParquetDatasetV1::V1ReplayLedger => LEDGER_COLUMNS,
        BenchmarkParquetDatasetV1::CounterfactualReplayLedger => COUNTERFACTUAL_LEDGER_COLUMNS,
        BenchmarkParquetDatasetV1::PairedMetricLedger => PAIRED_METRIC_COLUMNS,
    }
}

pub fn declared_schema_fingerprint(dataset: BenchmarkParquetDatasetV1) -> String {
    let mut digest = Sha256::new();
    digest.update(SCHEMA_FINGERPRINT_DOMAIN);
    digest_field(&mut digest, dataset.logical_name().as_bytes());
    digest.update(PARQUET_EXPORT_SCHEMA_VERSION.to_be_bytes());
    for field in columns(dataset) {
        digest_field(&mut digest, field.name.as_bytes());
        digest_field(&mut digest, field.kind.fingerprint_name().as_bytes());
        digest.update([u8::from(field.nullable)]);
    }
    hex::encode(digest.finalize())
}

pub fn declared_arrow_schema(dataset: BenchmarkParquetDatasetV1) -> SchemaRef {
    Arc::new(Schema::new(
        columns(dataset)
            .iter()
            .map(|field| Field::new(field.name, field.kind.arrow_type(), field.nullable))
            .collect::<Vec<_>>(),
    ))
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SortAtom {
    I64(i64),
    U64(u64),
    Text(String),
}

#[derive(Clone, Debug)]
enum Cell {
    Utf8(Option<String>),
    Boolean(Option<bool>),
    Int64(Option<i64>),
    UInt8(Option<u8>),
    UInt16(Option<u16>),
    UInt32(Option<u32>),
    UInt64(Option<u64>),
}

impl Cell {
    fn is_null(&self) -> bool {
        match self {
            Self::Utf8(value) => value.is_none(),
            Self::Boolean(value) => value.is_none(),
            Self::Int64(value) => value.is_none(),
            Self::UInt8(value) => value.is_none(),
            Self::UInt16(value) => value.is_none(),
            Self::UInt32(value) => value.is_none(),
            Self::UInt64(value) => value.is_none(),
        }
    }

    fn kind(&self) -> ColumnKind {
        match self {
            Self::Utf8(_) => ColumnKind::Utf8,
            Self::Boolean(_) => ColumnKind::Boolean,
            Self::Int64(_) => ColumnKind::Int64,
            Self::UInt8(_) => ColumnKind::UInt8,
            Self::UInt16(_) => ColumnKind::UInt16,
            Self::UInt32(_) => ColumnKind::UInt32,
            Self::UInt64(_) => ColumnKind::UInt64,
        }
    }
}

#[derive(Clone, Debug)]
struct DeclaredRow {
    sort_key: Vec<SortAtom>,
    cells: Vec<Cell>,
}

/// Streams one already-canonical JSONL dataset into deterministic Parquet and
/// atomically commits both the Parquet file and its self-hashed sidecar.
pub fn export_jsonl_to_parquet(
    spec: &ParquetExportSpecV1<'_>,
) -> Result<ParquetFileManifestV1, ParquetExportError> {
    validate_distinct_paths(spec)?;
    if spec.logical_name.trim().is_empty() {
        return Err(ParquetExportError::Manifest(
            "logical_name must be non-empty".to_owned(),
        ));
    }
    let (source_byte_length, source_jsonl_sha256) = sha256_file(spec.source_jsonl)?;
    let schema = declared_arrow_schema(spec.dataset);
    let (parquet_file, parquet_guard) = PartialOutput::create(spec.parquet_output)?;
    let properties = deterministic_writer_properties()?;
    let mut writer = ArrowWriter::try_new(parquet_file, schema.clone(), Some(properties))?;
    let source = File::open(spec.source_jsonl)?;
    let mut pending = Vec::with_capacity(PARQUET_ROW_GROUP_ROWS);
    let mut previous_key: Option<Vec<SortAtom>> = None;
    let mut row_count = 0_u64;

    for (line_index, line) in BufReader::new(source).lines().enumerate() {
        let line_number = line_index + 1;
        let line = line?;
        if line.trim().is_empty() {
            return Err(ParquetExportError::BlankLine { line: line_number });
        }
        let row = parse_declared_row(spec.dataset, &line, line_number)?;
        if previous_key
            .as_ref()
            .is_some_and(|previous| previous >= &row.sort_key)
        {
            return Err(ParquetExportError::NonCanonicalOrder {
                dataset: spec.dataset,
                line: line_number,
            });
        }
        previous_key = Some(row.sort_key.clone());
        pending.push(row);
        row_count = row_count
            .checked_add(1)
            .ok_or_else(|| ParquetExportError::Manifest("Parquet row count overflow".to_owned()))?;
        if pending.len() == PARQUET_ROW_GROUP_ROWS {
            write_rows(&mut writer, schema.clone(), columns(spec.dataset), &pending)?;
            pending.clear();
        }
    }
    if !pending.is_empty() {
        write_rows(&mut writer, schema, columns(spec.dataset), &pending)?;
    }
    writer.finish()?;
    writer.inner().sync_all()?;
    drop(writer);

    let (parquet_byte_length, parquet_sha256) = sha256_file(parquet_guard.path())?;
    verify_parquet_shape(
        parquet_guard.path(),
        spec.dataset,
        row_count,
        &declared_arrow_schema(spec.dataset),
    )?;
    let manifest = ParquetFileManifestV1 {
        schema_version: PARQUET_EXPORT_SCHEMA_VERSION,
        dataset: spec.dataset,
        logical_name: spec.logical_name.to_owned(),
        source_file_name: file_name(spec.source_jsonl)?,
        source_byte_length,
        source_jsonl_sha256,
        parquet_file_name: file_name(spec.parquet_output)?,
        parquet_byte_length,
        parquet_sha256,
        row_count,
        schema_fingerprint_sha256: declared_schema_fingerprint(spec.dataset),
        settings: DeterministicParquetSettingsV1::default(),
        manifest_sha256: String::new(),
    }
    .seal()?;
    manifest.validate()?;

    parquet_guard.commit(spec.parquet_output)?;
    write_manifest_atomic(spec.manifest_output, &manifest)?;
    Ok(manifest)
}

/// Rechecks the self-hashed sidecar, Parquet bytes, row count, and declared
/// Arrow schema without consulting live data.
pub fn validate_parquet_artifact(
    parquet_path: &Path,
    manifest_path: &Path,
) -> Result<ParquetFileManifestV1, ParquetExportError> {
    let manifest_bytes = fs::read(manifest_path)?;
    let raw: JsonValue = serde_json::from_slice(&manifest_bytes)?;
    let manifest: ParquetFileManifestV1 = serde_json::from_value(raw.clone())?;
    if serde_json::to_value(&manifest)? != raw {
        return Err(ParquetExportError::Manifest(
            "manifest contains unknown, missing, or drifted fields".to_owned(),
        ));
    }
    manifest.validate()?;
    if file_name(parquet_path)? != manifest.parquet_file_name {
        return Err(ParquetExportError::ArtifactMismatch(
            "Parquet file name differs from manifest".to_owned(),
        ));
    }
    let (byte_length, sha256) = sha256_file(parquet_path)?;
    if byte_length != manifest.parquet_byte_length || sha256 != manifest.parquet_sha256 {
        return Err(ParquetExportError::ArtifactMismatch(
            "Parquet byte length or SHA-256 differs from manifest".to_owned(),
        ));
    }
    verify_parquet_shape(
        parquet_path,
        manifest.dataset,
        manifest.row_count,
        &declared_arrow_schema(manifest.dataset),
    )?;
    Ok(manifest)
}

fn deterministic_writer_properties() -> Result<WriterProperties, ParquetExportError> {
    Ok(WriterProperties::builder()
        .set_writer_version(WriterVersion::PARQUET_2_0)
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(PARQUET_ZSTD_LEVEL)?))
        .set_max_row_group_row_count(Some(PARQUET_ROW_GROUP_ROWS))
        .set_write_batch_size(PARQUET_WRITE_BATCH_ROWS)
        .set_dictionary_enabled(true)
        .set_statistics_enabled(EnabledStatistics::Chunk)
        .set_created_by(PARQUET_CREATED_BY.to_owned())
        .build())
}

fn verify_parquet_shape(
    path: &Path,
    dataset: BenchmarkParquetDatasetV1,
    expected_rows: u64,
    expected_schema: &SchemaRef,
) -> Result<(), ParquetExportError> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(path)?)?;
    let actual_rows =
        u64::try_from(builder.metadata().file_metadata().num_rows()).map_err(|_| {
            ParquetExportError::ArtifactMismatch("negative Parquet metadata row count".to_owned())
        })?;
    if actual_rows != expected_rows {
        return Err(ParquetExportError::ArtifactMismatch(format!(
            "{dataset:?} row count {actual_rows} differs from expected {expected_rows}"
        )));
    }
    if builder.schema().as_ref() != expected_schema.as_ref() {
        return Err(ParquetExportError::ArtifactMismatch(format!(
            "{dataset:?} Arrow schema differs from its declaration"
        )));
    }
    Ok(())
}

fn write_rows(
    writer: &mut ArrowWriter<File>,
    schema: SchemaRef,
    declared_columns: &[ColumnSpec],
    rows: &[DeclaredRow],
) -> Result<(), ParquetExportError> {
    let batch = rows_to_batch(schema, declared_columns, rows)?;
    writer.write(&batch)?;
    Ok(())
}

fn rows_to_batch(
    schema: SchemaRef,
    declared_columns: &[ColumnSpec],
    rows: &[DeclaredRow],
) -> Result<RecordBatch, ParquetExportError> {
    if rows.iter().any(|row| {
        row.cells.len() != declared_columns.len()
            || row.cells.iter().zip(declared_columns).any(|(cell, field)| {
                cell.kind() != field.kind || (!field.nullable && cell.is_null())
            })
    }) {
        return Err(ParquetExportError::InternalSchemaMismatch);
    }
    let mut arrays = Vec::<ArrayRef>::with_capacity(declared_columns.len());
    for (column_index, field) in declared_columns.iter().enumerate() {
        arrays.push(build_array(field.kind, rows, column_index)?);
    }
    Ok(RecordBatch::try_new(schema, arrays)?)
}

fn build_array(
    kind: ColumnKind,
    rows: &[DeclaredRow],
    column_index: usize,
) -> Result<ArrayRef, ParquetExportError> {
    macro_rules! primitive_array {
        ($variant:ident, $builder:ty) => {{
            let mut builder = <$builder>::with_capacity(rows.len());
            for row in rows {
                match &row.cells[column_index] {
                    Cell::$variant(Some(value)) => builder.append_value(*value),
                    Cell::$variant(None) => builder.append_null(),
                    _ => return Err(ParquetExportError::InternalSchemaMismatch),
                }
            }
            Arc::new(builder.finish()) as ArrayRef
        }};
    }
    Ok(match kind {
        ColumnKind::Utf8 => {
            let mut builder = StringBuilder::new();
            for row in rows {
                match &row.cells[column_index] {
                    Cell::Utf8(Some(value)) => builder.append_value(value),
                    Cell::Utf8(None) => builder.append_null(),
                    _ => return Err(ParquetExportError::InternalSchemaMismatch),
                }
            }
            Arc::new(builder.finish())
        }
        ColumnKind::Boolean => primitive_array!(Boolean, BooleanBuilder),
        ColumnKind::Int64 => primitive_array!(Int64, Int64Builder),
        ColumnKind::UInt8 => primitive_array!(UInt8, UInt8Builder),
        ColumnKind::UInt16 => primitive_array!(UInt16, UInt16Builder),
        ColumnKind::UInt32 => primitive_array!(UInt32, UInt32Builder),
        ColumnKind::UInt64 => primitive_array!(UInt64, UInt64Builder),
    })
}

fn parse_declared_row(
    dataset: BenchmarkParquetDatasetV1,
    line: &str,
    line_number: usize,
) -> Result<DeclaredRow, ParquetExportError> {
    match dataset {
        BenchmarkParquetDatasetV1::CanonicalOperations => {
            operation_row(strict_record(dataset, line, line_number)?, line_number)
        }
        BenchmarkParquetDatasetV1::CanonicalEvents => {
            event_row(strict_record(dataset, line, line_number)?, line_number)
        }
        BenchmarkParquetDatasetV1::ConfigurationHistory => {
            config_row(strict_record(dataset, line, line_number)?, line_number)
        }
        BenchmarkParquetDatasetV1::HistoricalPrices => {
            price_row(strict_record(dataset, line, line_number)?, line_number)
        }
        BenchmarkParquetDatasetV1::V1ReplayLedger => {
            ledger_row(strict_record(dataset, line, line_number)?, line_number)
        }
        BenchmarkParquetDatasetV1::CounterfactualReplayLedger => {
            counterfactual_row(strict_record(dataset, line, line_number)?, line_number)
        }
        BenchmarkParquetDatasetV1::PairedMetricLedger => {
            paired_metric_row(strict_record(dataset, line, line_number)?, line_number)
        }
    }
}

fn strict_record<T: DeserializeOwned + Serialize>(
    dataset: BenchmarkParquetDatasetV1,
    line: &str,
    line_number: usize,
) -> Result<T, ParquetExportError> {
    let raw: JsonValue =
        serde_json::from_str(line).map_err(|error| ParquetExportError::InvalidRecord {
            dataset,
            line: line_number,
            detail: error.to_string(),
        })?;
    let typed: T =
        serde_json::from_value(raw.clone()).map_err(|error| ParquetExportError::InvalidRecord {
            dataset,
            line: line_number,
            detail: error.to_string(),
        })?;
    if serde_json::to_value(&typed)? != raw {
        return Err(ParquetExportError::SchemaDrift {
            dataset,
            line: line_number,
        });
    }
    Ok(typed)
}

fn operation_row(
    operation: CanonicalOperationV1,
    line: usize,
) -> Result<DeclaredRow, ParquetExportError> {
    operation.validate().map_err(|error| {
        invalid_record(BenchmarkParquetDatasetV1::CanonicalOperations, line, error)
    })?;
    let (payload_kind, payload_json) = tagged_json(&operation.payload)?;
    let assertions_json = canonical_json(&operation.assertions)?;
    let record_json = canonical_json(&operation)?;
    let mut cells = vec![
        Cell::UInt16(Some(operation.schema_version)),
        Cell::Utf8(Some(operation.operation_id.clone())),
        Cell::Utf8(Some(operation.signature.clone())),
        Cell::Utf8(Some(operation.pair.clone())),
        Cell::Utf8(operation.actor.clone()),
    ];
    cells.extend(order_cells(operation.order));
    cells.extend([
        Cell::Int64(Some(operation.block_time_unix_seconds)),
        Cell::Utf8(Some(operation.decoder_version_id.clone())),
        Cell::Utf8(Some(payload_kind)),
        Cell::Utf8(Some(payload_json)),
        Cell::Utf8(Some(assertions_json)),
        Cell::Utf8(Some(record_json)),
    ]);
    Ok(DeclaredRow {
        sort_key: chain_sort_key(operation.order),
        cells,
    })
}

fn event_row(event: CanonicalEventV1, line: usize) -> Result<DeclaredRow, ParquetExportError> {
    if event.event_id.is_empty()
        || event.cluster.is_empty()
        || event.program_id.is_empty()
        || event.signature.is_empty()
        || event.decoder_version_id.is_empty()
    {
        return Err(invalid_record(
            BenchmarkParquetDatasetV1::CanonicalEvents,
            line,
            "event identity fields are empty",
        ));
    }
    let (payload_kind, payload_json) = tagged_json(&event.payload)?;
    let record_json = canonical_json(&event)?;
    let mut cells = vec![
        Cell::UInt16(Some(CANONICAL_SCHEMA_VERSION)),
        Cell::Utf8(Some(event.event_id.clone())),
        Cell::Utf8(Some(event.cluster.clone())),
        Cell::Utf8(Some(event.program_id.clone())),
        Cell::Utf8(event.pair.clone()),
        Cell::Utf8(event.actor.clone()),
        Cell::Utf8(Some(event.signature.clone())),
    ];
    cells.extend(order_cells(event.order));
    cells.extend([
        Cell::Utf8(Some(canonical_json(&event.execution_path)?)),
        Cell::Int64(Some(event.block_time_unix_seconds)),
        Cell::Utf8(Some(event.decoder_version_id.clone())),
        Cell::Utf8(Some(event.raw_payload_sha256.clone())),
        Cell::Utf8(event.db_source.as_ref().map(|source| source.table.clone())),
        Cell::Utf8(event.db_source.as_ref().map(|source| source.row_id.clone())),
        Cell::Utf8(Some(payload_kind)),
        Cell::Utf8(Some(payload_json)),
        Cell::Utf8(Some(record_json)),
    ]);
    Ok(DeclaredRow {
        sort_key: chain_sort_key(event.order),
        cells,
    })
}

fn config_row(config: ConfigVersionV1, line: usize) -> Result<DeclaredRow, ParquetExportError> {
    if config.config_id.is_empty()
        || config.entity_address.is_empty()
        || config.source_signature.is_empty()
        || config.decoder_version_id.is_empty()
        || config
            .valid_to_exclusive
            .is_some_and(|valid_to| valid_to <= config.valid_from)
    {
        return Err(invalid_record(
            BenchmarkParquetDatasetV1::ConfigurationHistory,
            line,
            "configuration identity or validity range is invalid",
        ));
    }
    let scope = enum_string(&config.scope)?;
    let record_json = canonical_json(&config)?;
    let mut cells = vec![
        Cell::UInt16(Some(CANONICAL_SCHEMA_VERSION)),
        Cell::Utf8(Some(config.config_id.clone())),
        Cell::Utf8(Some(scope.clone())),
        Cell::Utf8(Some(config.entity_address.clone())),
    ];
    cells.extend(order_cells(config.valid_from));
    cells.extend(optional_order_cells(config.valid_to_exclusive));
    cells.extend([
        Cell::Utf8(Some(config.source_signature.clone())),
        Cell::Utf8(Some(config.decoder_version_id.clone())),
        Cell::Utf8(Some(canonical_json(&config.fields)?)),
        Cell::Utf8(Some(record_json)),
    ]);
    let mut sort_key = chain_sort_key(config.valid_from);
    sort_key.extend([
        SortAtom::U64(config_scope_rank(&config.scope)),
        SortAtom::Text(config.entity_address.clone()),
        SortAtom::Text(config.config_id.clone()),
    ]);
    Ok(DeclaredRow { sort_key, cells })
}

fn price_row(price: HistoricalPriceMarkV1, line: usize) -> Result<DeclaredRow, ParquetExportError> {
    price.validate().map_err(|error| {
        invalid_record(BenchmarkParquetDatasetV1::HistoricalPrices, line, error)
    })?;
    let quality = enum_string(&price.quality)?;
    let cells = vec![
        Cell::UInt16(Some(CANONICAL_SCHEMA_VERSION)),
        Cell::Utf8(Some(price.mint.clone())),
        Cell::Int64(Some(price.bucket_unix_seconds)),
        Cell::Utf8(Some(price.price_usd.as_str().to_owned())),
        Cell::UInt8(price.token_decimals),
        Cell::Utf8(Some(price.provider.clone())),
        Cell::Utf8(Some(quality)),
        Cell::Utf8(Some(canonical_json(&price)?)),
    ];
    Ok(DeclaredRow {
        sort_key: vec![
            SortAtom::I64(price.bucket_unix_seconds),
            SortAtom::Text(price.mint),
            SortAtom::Text(price.provider),
        ],
        cells,
    })
}

fn ledger_row(row: V1ReplayLedgerRowV1, line: usize) -> Result<DeclaredRow, ParquetExportError> {
    if row.schema_version != V1_RECONCILIATION_SCHEMA_VERSION
        || row.market.is_empty()
        || row.operation_id.is_empty()
    {
        return Err(invalid_record(
            BenchmarkParquetDatasetV1::V1ReplayLedger,
            line,
            "ledger schema version or identity is invalid",
        ));
    }
    let mut cells = vec![
        Cell::UInt16(Some(row.schema_version)),
        Cell::Utf8(Some(row.market.clone())),
        Cell::Utf8(Some(row.operation_id.clone())),
    ];
    cells.extend(order_cells(row.order));
    cells.extend([
        Cell::Boolean(Some(row.broadcast_global_config)),
        Cell::UInt64(Some(row.reconciliation_count)),
        Cell::Boolean(Some(row.all_raw_unit_differences_zero)),
        Cell::Utf8(Some(row.compact_checkpoint.checkpoint_sha256.clone())),
        Cell::Utf8(Some(row.compact_checkpoint.positions_sha256.clone())),
        Cell::Utf8(Some(row.reconciliations_sha256.clone())),
        Cell::Utf8(Some(row.factual_record_sha256.clone())),
        Cell::Utf8(Some(canonical_json(&row.compact_checkpoint)?)),
        Cell::Utf8(Some(canonical_json(&row.conservation)?)),
        Cell::Utf8(Some(canonical_json(&row.revenue_delta)?)),
        Cell::Utf8(Some(canonical_json(&row)?)),
    ]);
    let mut sort_key = chain_sort_key(row.order);
    sort_key.extend([SortAtom::Text(row.market), SortAtom::Text(row.operation_id)]);
    Ok(DeclaredRow { sort_key, cells })
}

fn counterfactual_row(
    row: CounterfactualOperationRecordV1,
    line: usize,
) -> Result<DeclaredRow, ParquetExportError> {
    if row.schema_version != COUNTERFACTUAL_SCHEMA_VERSION
        || row.operation_id.is_empty()
        || row.market.is_empty()
        || !is_canonical_sha256(&row.state_sha256)
    {
        return Err(invalid_record(
            BenchmarkParquetDatasetV1::CounterfactualReplayLedger,
            line,
            "counterfactual schema version, identity, or state hash is invalid",
        ));
    }
    let replay_sequence = u64::try_from(line).map_err(|error| {
        invalid_record(
            BenchmarkParquetDatasetV1::CounterfactualReplayLedger,
            line,
            error,
        )
    })?;
    let cells = vec![
        Cell::UInt16(Some(row.schema_version)),
        Cell::UInt64(Some(replay_sequence)),
        Cell::Utf8(Some(row.operation_id.clone())),
        Cell::Utf8(Some(row.market.clone())),
        Cell::UInt64(Some(row.slot)),
        Cell::Utf8(Some(enum_string(&row.arm)?)),
        Cell::Boolean(Some(row.synthetic_arbitrage)),
        Cell::Utf8(Some(enum_string(&row.status)?)),
        Cell::Utf8(Some(row.state_sha256.clone())),
        Cell::Utf8(Some(canonical_json(&row.checkpoint)?)),
        Cell::Utf8(Some(canonical_json(&row.revenue)?)),
        Cell::Utf8(Some(canonical_json(&row)?)),
    ];
    Ok(DeclaredRow {
        sort_key: vec![SortAtom::U64(replay_sequence)],
        cells,
    })
}

fn paired_metric_row(
    row: PairedOperationMetricRowV1,
    line: usize,
) -> Result<DeclaredRow, ParquetExportError> {
    if row.schema_version != PAIRED_METRIC_SCHEMA_VERSION
        || row.profile_id.is_empty()
        || row.market.is_empty()
        || row.operation_id.is_empty()
        || !is_canonical_sha256(&row.factual_record_sha256)
        || !is_canonical_sha256(&row.dusk_state_sha256)
    {
        return Err(invalid_record(
            BenchmarkParquetDatasetV1::PairedMetricLedger,
            line,
            "paired metric schema version, identity, or state hash is invalid",
        ));
    }
    let replay_sequence = u64::try_from(line).map_err(|error| {
        invalid_record(BenchmarkParquetDatasetV1::PairedMetricLedger, line, error)
    })?;
    let mut cells = vec![
        Cell::UInt16(Some(row.schema_version)),
        Cell::UInt64(Some(replay_sequence)),
        Cell::Utf8(Some(row.profile_id.clone())),
        Cell::Utf8(Some(row.market.clone())),
        Cell::Utf8(Some(row.operation_id.clone())),
    ];
    cells.extend(order_cells(row.order));
    cells.extend([
        Cell::Int64(Some(row.block_time_unix_seconds)),
        Cell::Utf8(Some(enum_string(&row.arm)?)),
        Cell::Boolean(Some(row.synthetic_arbitrage)),
        Cell::Utf8(Some(enum_string(&row.dusk_status)?)),
        Cell::Boolean(Some(row.swap.is_some())),
        Cell::Boolean(Some(row.borrow.is_some())),
        Cell::Boolean(Some(row.repay.is_some())),
        Cell::Boolean(Some(row.safety.is_some())),
        Cell::Utf8(Some(row.factual_record_sha256.clone())),
        Cell::Utf8(Some(row.dusk_state_sha256.clone())),
        Cell::Utf8(row.swap.as_ref().map(canonical_json).transpose()?),
        Cell::Utf8(row.borrow.as_ref().map(canonical_json).transpose()?),
        Cell::Utf8(row.repay.as_ref().map(canonical_json).transpose()?),
        Cell::Utf8(row.safety.as_ref().map(canonical_json).transpose()?),
        Cell::Utf8(Some(canonical_json(&row.revenue)?)),
        Cell::Utf8(Some(canonical_json(&row.capital_after)?)),
        Cell::Utf8(Some(canonical_json(&row)?)),
    ]);
    Ok(DeclaredRow {
        sort_key: vec![SortAtom::U64(replay_sequence)],
        cells,
    })
}

fn order_cells(order: ChainOrder) -> [Cell; 5] {
    [
        Cell::UInt64(Some(order.slot)),
        Cell::UInt32(Some(order.transaction_index)),
        Cell::UInt32(Some(order.outer_instruction_index)),
        Cell::UInt32(order.inner_execution_index),
        Cell::UInt32(Some(order.event_sequence)),
    ]
}

fn optional_order_cells(order: Option<ChainOrder>) -> [Cell; 5] {
    match order {
        Some(order) => order_cells(order),
        None => [
            Cell::UInt64(None),
            Cell::UInt32(None),
            Cell::UInt32(None),
            Cell::UInt32(None),
            Cell::UInt32(None),
        ],
    }
}

fn chain_sort_key(order: ChainOrder) -> Vec<SortAtom> {
    vec![
        SortAtom::U64(order.slot),
        SortAtom::U64(u64::from(order.transaction_index)),
        SortAtom::U64(u64::from(order.outer_instruction_index)),
        SortAtom::U64(u64::from(order.inner_execution_index.is_some())),
        SortAtom::U64(u64::from(order.inner_execution_index.unwrap_or_default())),
        SortAtom::U64(u64::from(order.event_sequence)),
    ]
}

fn config_scope_rank(scope: &ConfigScope) -> u64 {
    match scope {
        ConfigScope::Pair | ConfigScope::ProtocolRevenue => 0,
        ConfigScope::RevenueRecipients => 1,
        ConfigScope::ReduceOnly => 2,
        ConfigScope::RateModel => 3,
        ConfigScope::ProgramBinary => 4,
    }
}

fn tagged_json<T: Serialize>(value: &T) -> Result<(String, String), ParquetExportError> {
    let json = serde_json::to_value(value)?;
    let kind = json
        .get("kind")
        .and_then(JsonValue::as_str)
        .ok_or(ParquetExportError::InternalSchemaMismatch)?
        .to_owned();
    Ok((kind, serde_json::to_string(&json)?))
}

fn enum_string<T: Serialize>(value: &T) -> Result<String, ParquetExportError> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or(ParquetExportError::InternalSchemaMismatch)
}

fn canonical_json<T: Serialize>(value: &T) -> Result<String, ParquetExportError> {
    Ok(serde_json::to_string(value)?)
}

fn invalid_record(
    dataset: BenchmarkParquetDatasetV1,
    line: usize,
    detail: impl std::fmt::Display,
) -> ParquetExportError {
    ParquetExportError::InvalidRecord {
        dataset,
        line,
        detail: detail.to_string(),
    }
}

fn validate_distinct_paths(spec: &ParquetExportSpecV1<'_>) -> Result<(), ParquetExportError> {
    if spec.source_jsonl == spec.parquet_output
        || spec.source_jsonl == spec.manifest_output
        || spec.parquet_output == spec.manifest_output
    {
        return Err(ParquetExportError::PathCollision);
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<(u64, String), ParquetExportError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.checked_add(read as u64).ok_or_else(|| {
            ParquetExportError::Manifest("artifact byte length overflow".to_owned())
        })?;
        digest.update(&buffer[..read]);
    }
    Ok((total, hex::encode(digest.finalize())))
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn file_name(path: &Path) -> Result<String, ParquetExportError> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| ParquetExportError::MissingFileName(path.to_owned()))
}

fn write_manifest_atomic(
    output: &Path,
    manifest: &ParquetFileManifestV1,
) -> Result<(), ParquetExportError> {
    let (mut file, guard) = PartialOutput::create(output)?;
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    guard.commit(output)
}

struct PartialOutput {
    path: PathBuf,
    committed: bool,
}

impl PartialOutput {
    fn create(target: &Path) -> Result<(File, Self), ParquetExportError> {
        let parent = parent_directory(target);
        let target_name = file_name(target)?;
        for _ in 0..32 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let partial = parent.join(format!(
                ".{target_name}.{}.{}.partial",
                std::process::id(),
                sequence
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial)
            {
                Ok(file) => {
                    return Ok((
                        file,
                        Self {
                            path: partial,
                            committed: false,
                        },
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(ParquetExportError::Manifest(
            "could not reserve an atomic partial output".to_owned(),
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn commit(mut self, target: &Path) -> Result<(), ParquetExportError> {
        fs::rename(&self.path, target)?;
        self.committed = true;
        File::open(parent_directory(target))?.sync_all()?;
        Ok(())
    }
}

fn parent_directory(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

impl Drop for PartialOutput {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::PathBuf};

    use arrow_array::{Array, StringArray};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    use super::*;
    use crate::{
        canonical::{ExactDecimal, HistoricalPriceQuality, TokenAmount},
        replay::{OperationPayloadV1, REPLAY_SCHEMA_VERSION},
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "v1-vs-dusk-parquet-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn amount(value: &str) -> TokenAmount {
        TokenAmount::new(value).unwrap()
    }

    fn operation(slot: u64, id: &str, amount_in: &str) -> CanonicalOperationV1 {
        CanonicalOperationV1 {
            schema_version: REPLAY_SCHEMA_VERSION,
            operation_id: id.to_owned(),
            signature: format!("signature-{id}"),
            pair: "pair-a".to_owned(),
            actor: Some("actor-a".to_owned()),
            order: ChainOrder {
                slot,
                transaction_index: 0,
                outer_instruction_index: 0,
                inner_execution_index: None,
                event_sequence: 0,
            },
            block_time_unix_seconds: 1_700_000_000 + slot as i64,
            decoder_version_id: "decoder-v1".to_owned(),
            payload: OperationPayloadV1::SwapExactIn {
                token0_in: true,
                gross_amount_in: amount(amount_in),
                vault_credit_in: amount(amount_in),
                min_amount_out: amount("1"),
                vault_debit_out: amount("2"),
                recipient_credit_out: amount("2"),
            },
            assertions: Vec::new(),
        }
    }

    fn write_jsonl<T: Serialize>(path: &Path, rows: &[T]) {
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        fs::write(path, bytes).unwrap();
    }

    fn export_paths(directory: &TestDirectory, stem: &str) -> (PathBuf, PathBuf, PathBuf) {
        (
            directory.0.join(format!("{stem}.jsonl")),
            directory.0.join(format!("{stem}.parquet")),
            directory.0.join(format!("{stem}.manifest.json")),
        )
    }

    #[test]
    fn operation_round_trip_preserves_large_token_amount_strings_and_is_deterministic() {
        let directory = TestDirectory::new();
        let (source, parquet, manifest) = export_paths(&directory, "operations-a");
        let rows = vec![
            operation(10, "op-10", "9007199254740993"),
            operation(11, "op-11", "340282366920938463463374607431768211455"),
        ];
        write_jsonl(&source, &rows);
        let exported = export_jsonl_to_parquet(&ParquetExportSpecV1 {
            dataset: BenchmarkParquetDatasetV1::CanonicalOperations,
            logical_name: "canonical-operations.jsonl",
            source_jsonl: &source,
            parquet_output: &parquet,
            manifest_output: &manifest,
        })
        .unwrap();
        assert_eq!(exported.row_count, 2);
        assert_eq!(
            validate_parquet_artifact(&parquet, &manifest).unwrap(),
            exported
        );

        let mut reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&parquet).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.next().unwrap().unwrap();
        let record_json = batch
            .column_by_name("record_json")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let round_trip: Vec<CanonicalOperationV1> = (0..record_json.len())
            .map(|index| serde_json::from_str(record_json.value(index)).unwrap())
            .collect();
        assert_eq!(round_trip, rows);
        let payload_json = batch
            .column_by_name("payload_json")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<JsonValue>(payload_json.value(0)).unwrap()["payload"]
                ["gross_amount_in"]
                .as_str(),
            Some("9007199254740993")
        );
        assert_eq!(
            batch
                .schema()
                .field_with_name("payload_json")
                .unwrap()
                .data_type(),
            &DataType::Utf8
        );

        let (_, parquet_b, manifest_b) = export_paths(&directory, "operations-b");
        export_jsonl_to_parquet(&ParquetExportSpecV1 {
            dataset: BenchmarkParquetDatasetV1::CanonicalOperations,
            logical_name: "canonical-operations.jsonl",
            source_jsonl: &source,
            parquet_output: &parquet_b,
            manifest_output: &manifest_b,
        })
        .unwrap();
        assert_eq!(
            sha256_file(&parquet).unwrap(),
            sha256_file(&parquet_b).unwrap()
        );
    }

    #[test]
    fn historical_price_round_trip_uses_declared_utf8_decimal() {
        let directory = TestDirectory::new();
        let (source, parquet, manifest) = export_paths(&directory, "prices");
        let prices = vec![HistoricalPriceMarkV1 {
            mint: "mint-a".to_owned(),
            bucket_unix_seconds: 1_699_999_200,
            price_usd: ExactDecimal::new("12345678901234567890.000001").unwrap(),
            token_decimals: Some(9),
            provider: "historical-provider".to_owned(),
            quality: HistoricalPriceQuality::Historical,
        }];
        write_jsonl(&source, &prices);
        export_jsonl_to_parquet(&ParquetExportSpecV1 {
            dataset: BenchmarkParquetDatasetV1::HistoricalPrices,
            logical_name: "historical-prices.jsonl",
            source_jsonl: &source,
            parquet_output: &parquet,
            manifest_output: &manifest,
        })
        .unwrap();
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(File::open(parquet).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.next().unwrap().unwrap();
        let price = batch
            .column_by_name("price_usd")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(price.value(0), "12345678901234567890.000001");
        assert_eq!(
            batch
                .schema()
                .field_with_name("price_usd")
                .unwrap()
                .data_type(),
            &DataType::Utf8
        );
    }

    #[test]
    fn unknown_fields_and_noncanonical_order_fail_closed_without_committed_output() {
        let directory = TestDirectory::new();
        let (source, parquet, manifest) = export_paths(&directory, "drift");
        let mut drifted = serde_json::to_value(operation(10, "op-10", "1")).unwrap();
        drifted
            .as_object_mut()
            .unwrap()
            .insert("future_field".to_owned(), JsonValue::Bool(true));
        write_jsonl(&source, &[drifted]);
        assert!(matches!(
            export_jsonl_to_parquet(&ParquetExportSpecV1 {
                dataset: BenchmarkParquetDatasetV1::CanonicalOperations,
                logical_name: "drift.jsonl",
                source_jsonl: &source,
                parquet_output: &parquet,
                manifest_output: &manifest,
            }),
            Err(ParquetExportError::SchemaDrift { line: 1, .. })
        ));
        assert!(!parquet.exists());
        assert!(!manifest.exists());

        write_jsonl(
            &source,
            &[operation(11, "op-11", "1"), operation(10, "op-10", "1")],
        );
        assert!(matches!(
            export_jsonl_to_parquet(&ParquetExportSpecV1 {
                dataset: BenchmarkParquetDatasetV1::CanonicalOperations,
                logical_name: "order.jsonl",
                source_jsonl: &source,
                parquet_output: &parquet,
                manifest_output: &manifest,
            }),
            Err(ParquetExportError::NonCanonicalOrder { line: 2, .. })
        ));
        assert!(!parquet.exists());
    }

    #[test]
    fn every_benchmark_dataset_has_a_versioned_declared_schema() {
        let datasets = [
            BenchmarkParquetDatasetV1::CanonicalOperations,
            BenchmarkParquetDatasetV1::CanonicalEvents,
            BenchmarkParquetDatasetV1::ConfigurationHistory,
            BenchmarkParquetDatasetV1::HistoricalPrices,
            BenchmarkParquetDatasetV1::V1ReplayLedger,
            BenchmarkParquetDatasetV1::CounterfactualReplayLedger,
            BenchmarkParquetDatasetV1::PairedMetricLedger,
        ];
        let mut fingerprints = BTreeMap::new();
        for dataset in datasets {
            let schema = declared_arrow_schema(dataset);
            assert_eq!(schema.field(0).name(), "record_schema_version");
            assert!(schema.field_with_name("record_json").is_ok());
            assert_eq!(declared_schema_fingerprint(dataset).len(), 64);
            assert!(fingerprints
                .insert(declared_schema_fingerprint(dataset), dataset)
                .is_none());
        }
        assert_eq!(
            declared_arrow_schema(BenchmarkParquetDatasetV1::V1ReplayLedger)
                .field_with_name("compact_checkpoint_json")
                .unwrap()
                .data_type(),
            &DataType::Utf8
        );
    }

    #[test]
    fn empty_jsonl_produces_a_valid_zero_row_artifact() {
        let directory = TestDirectory::new();
        let (source, parquet, manifest) = export_paths(&directory, "empty-prices");
        fs::write(&source, []).unwrap();

        let exported = export_jsonl_to_parquet(&ParquetExportSpecV1 {
            dataset: BenchmarkParquetDatasetV1::HistoricalPrices,
            logical_name: "empty-historical-prices.jsonl",
            source_jsonl: &source,
            parquet_output: &parquet,
            manifest_output: &manifest,
        })
        .unwrap();

        assert_eq!(exported.row_count, 0);
        assert_eq!(
            validate_parquet_artifact(&parquet, &manifest).unwrap(),
            exported
        );
    }
}

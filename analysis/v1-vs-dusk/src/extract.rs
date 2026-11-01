//! Read-only database extraction and archival Solana block enrichment.
//!
//! The PostgreSQL side intentionally exports slot/signature/raw values only.
//! `block_time` and transaction position are accepted exclusively from a
//! finalized archival `getBlock` response.

use crate::canonical::{
    sha256_hex, BlockArtifactChecksumV1, HistoricalPriceMarkV1, SanitizedSource,
    CANONICAL_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fmt::{self, Display, Formatter},
    future::Future,
    pin::Pin,
};
use tokio_postgres::{types::ToSql, Client, NoTls};

pub const MARKETS_SQL: &str = include_str!("../sql/markets.sql");
pub const EVENTS_SQL: &str = include_str!("../sql/events.sql");
pub const COVERAGE_SQL: &str = include_str!("../sql/coverage.sql");
pub const PRICES_SQL: &str = include_str!("../sql/prices.sql");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    MissingDatabaseUrl,
    DatabaseConnection,
    DatabaseTransaction,
    DatabaseQuery,
    DatabaseDecode,
    DatabaseDecodeAt { path: String, category: String },
    InvalidHistoricalPriceWindow,
    NonReadOnlyStatement,
    TransactionNotReadOnly,
    TransactionNotRepeatableRead,
    RpcBlockUnavailable(u64),
    RpcBlockTimeMissing(u64),
    RpcSlotMismatch { requested: u64, returned: u64 },
    SignatureMissing { slot: u64, signature: String },
    DuplicateSignature { slot: u64, signature: String },
    InvalidInstructionPath,
}

impl Display for ExtractError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDatabaseUrl => f.write_str("DATABASE_URL was not supplied at runtime"),
            Self::DatabaseConnection => {
                f.write_str("read-only PostgreSQL connection failed; connection details suppressed")
            }
            Self::DatabaseTransaction => {
                f.write_str("read-only PostgreSQL transaction failed; details suppressed")
            }
            Self::DatabaseQuery => {
                f.write_str("read-only PostgreSQL query failed; details suppressed")
            }
            Self::DatabaseDecode => {
                f.write_str("PostgreSQL export row was not valid canonical JSON")
            }
            Self::DatabaseDecodeAt { path, category } => write!(
                f,
                "PostgreSQL export row failed canonical decoding at {path} ({category})"
            ),
            Self::InvalidHistoricalPriceWindow => {
                f.write_str("historical price export window is invalid")
            }
            Self::NonReadOnlyStatement => {
                f.write_str("exporter rejected a statement that is not SELECT/WITH")
            }
            Self::TransactionNotReadOnly => f.write_str("PostgreSQL transaction is not read-only"),
            Self::TransactionNotRepeatableRead => {
                f.write_str("PostgreSQL transaction is not REPEATABLE READ")
            }
            Self::RpcBlockUnavailable(slot) => write!(f, "archival block {slot} is unavailable"),
            Self::RpcBlockTimeMissing(slot) => {
                write!(f, "archival block {slot} has no authoritative block time")
            }
            Self::RpcSlotMismatch {
                requested,
                returned,
            } => write!(
                f,
                "archival RPC returned slot {returned} for requested slot {requested}"
            ),
            Self::SignatureMissing { slot, signature } => {
                write!(
                    f,
                    "signature {signature} is absent from archival block {slot}"
                )
            }
            Self::DuplicateSignature { slot, signature } => {
                write!(
                    f,
                    "signature {signature} occurs more than once in block {slot}"
                )
            }
            Self::InvalidInstructionPath => f.write_str("invalid database instruction path"),
        }
    }
}

impl Error for ExtractError {}

fn strip_leading_sql_comments(mut sql: &str) -> &str {
    loop {
        sql = sql.trim_start();
        if let Some(comment) = sql.strip_prefix("--") {
            sql = comment
                .split_once('\n')
                .map_or("", |(_, remainder)| remainder);
            continue;
        }
        return sql;
    }
}

pub fn is_export_statement(sql: &str) -> bool {
    let statement = strip_leading_sql_comments(sql);
    statement
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("select"))
        || statement
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("with"))
}

/// A database session with no API that can execute mutations.
///
/// Production callers that require TLS should construct a `tokio_postgres::Client`
/// with their approved TLS connector and call `begin_with_client`. The convenience
/// runtime-env method is explicitly `NoTls` and is intended for local/tunnelled use.
pub struct ReadOnlyPgExporter {
    client: Client,
    finished: bool,
}

impl ReadOnlyPgExporter {
    pub async fn connect_from_runtime_env_no_tls() -> Result<(Self, SanitizedSource), ExtractError>
    {
        let database_url =
            env::var("DATABASE_URL").map_err(|_| ExtractError::MissingDatabaseUrl)?;
        let source =
            SanitizedSource::from_secret("postgresql", "runtime_DATABASE_URL", &database_url)
                .map_err(|_| ExtractError::DatabaseConnection)?;
        let exporter = Self::connect_no_tls(&database_url).await?;
        Ok((exporter, source))
    }

    pub async fn connect_no_tls(database_url: &str) -> Result<Self, ExtractError> {
        let (client, connection) = tokio_postgres::connect(database_url, NoTls)
            .await
            .map_err(|_| ExtractError::DatabaseConnection)?;
        // The connection task deliberately emits no error text because upstream
        // driver errors can contain endpoint or connection-string material.
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Self::begin_with_client(client).await
    }

    pub async fn begin_with_client(client: Client) -> Result<Self, ExtractError> {
        client
            .batch_execute(
                "BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;\
                 SET LOCAL statement_timeout = '120s';\
                 SET LOCAL idle_in_transaction_session_timeout = '300s';",
            )
            .await
            .map_err(|_| ExtractError::DatabaseTransaction)?;
        let exporter = Self {
            client,
            finished: false,
        };
        exporter.verify_transaction_mode().await?;
        Ok(exporter)
    }

    async fn verify_transaction_mode(&self) -> Result<(), ExtractError> {
        let read_only = self
            .client
            .query_one("SHOW transaction_read_only", &[])
            .await
            .map_err(|_| ExtractError::DatabaseTransaction)?
            .try_get::<_, String>(0)
            .map_err(|_| ExtractError::DatabaseDecode)?;
        if read_only != "on" {
            return Err(ExtractError::TransactionNotReadOnly);
        }

        let isolation = self
            .client
            .query_one("SHOW transaction_isolation", &[])
            .await
            .map_err(|_| ExtractError::DatabaseTransaction)?
            .try_get::<_, String>(0)
            .map_err(|_| ExtractError::DatabaseDecode)?;
        if !isolation.eq_ignore_ascii_case("repeatable read") {
            return Err(ExtractError::TransactionNotRepeatableRead);
        }
        Ok(())
    }

    /// Executes a static SELECT/WITH query whose first column is JSON text.
    pub async fn query_json_text(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<String>, ExtractError> {
        if !is_export_statement(sql) {
            return Err(ExtractError::NonReadOnlyStatement);
        }
        let rows = self
            .client
            .query(sql, params)
            .await
            .map_err(|_| ExtractError::DatabaseQuery)?;
        rows.into_iter()
            .map(|row| {
                row.try_get::<_, String>(0)
                    .map_err(|_| ExtractError::DatabaseDecode)
            })
            .collect()
    }

    pub async fn finish(mut self) -> Result<(), ExtractError> {
        self.client
            .batch_execute("COMMIT")
            .await
            .map_err(|_| ExtractError::DatabaseTransaction)?;
        self.finished = true;
        Ok(())
    }

    pub async fn abort(mut self) -> Result<(), ExtractError> {
        self.client
            .batch_execute("ROLLBACK")
            .await
            .map_err(|_| ExtractError::DatabaseTransaction)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for ReadOnlyPgExporter {
    fn drop(&mut self) {
        // Dropping the client closes an unfinished read-only transaction. There
        // is no data to roll back and no mutation can have occurred.
        let _ = self.finished;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DbEventSeed {
    pub source_table: String,
    pub source_id: u64,
    pub event_kind: String,
    pub pair: String,
    pub actor: Option<String>,
    pub signature: String,
    pub slot: u64,
    pub instruction_index: Option<u32>,
    pub instruction_path: Option<String>,
    pub payload: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportCheckpoint {
    pub schema_version: u16,
    pub query_name: String,
    pub last_slot: u64,
    pub last_source_table: String,
    pub last_source_id: u64,
    pub pages_completed: u64,
    pub rows_exported: u64,
    pub complete: bool,
}

impl ExportCheckpoint {
    pub fn new(query_name: impl Into<String>, slot_before_start: u64) -> Self {
        Self {
            schema_version: CANONICAL_SCHEMA_VERSION,
            query_name: query_name.into(),
            last_slot: slot_before_start,
            last_source_table: String::new(),
            last_source_id: 0,
            pages_completed: 0,
            rows_exported: 0,
            complete: false,
        }
    }

    pub fn advance(&mut self, rows: &[DbEventSeed], requested_limit: usize) {
        if let Some(last) = rows.last() {
            self.last_slot = last.slot;
            self.last_source_table.clone_from(&last.source_table);
            self.last_source_id = last.source_id;
        }
        self.pages_completed = self.pages_completed.saturating_add(1);
        self.rows_exported = self.rows_exported.saturating_add(rows.len() as u64);
        self.complete = rows.len() < requested_limit;
    }
}

pub async fn export_event_page(
    exporter: &ReadOnlyPgExporter,
    slot_start: u64,
    slot_end: u64,
    checkpoint: &mut ExportCheckpoint,
    limit: u32,
) -> Result<Vec<DbEventSeed>, ExtractError> {
    let slot_start = i64::try_from(slot_start).map_err(|_| ExtractError::DatabaseQuery)?;
    let slot_end = i64::try_from(slot_end).map_err(|_| ExtractError::DatabaseQuery)?;
    let cursor_slot =
        i64::try_from(checkpoint.last_slot).map_err(|_| ExtractError::DatabaseQuery)?;
    let cursor_source_id =
        i64::try_from(checkpoint.last_source_id).map_err(|_| ExtractError::DatabaseQuery)?;
    let limit_i64 = i64::from(limit.max(1));
    let json_rows = exporter
        .query_json_text(
            EVENTS_SQL,
            &[
                &slot_start,
                &slot_end,
                &cursor_slot,
                &checkpoint.last_source_table,
                &cursor_source_id,
                &limit_i64,
            ],
        )
        .await?;
    let rows: Vec<DbEventSeed> = json_rows
        .into_iter()
        .map(|row| {
            let mut deserializer = serde_json::Deserializer::from_str(&row);
            serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
                ExtractError::DatabaseDecodeAt {
                    path: error.path().to_string(),
                    category: format!("{:?}", error.inner().classify()).to_ascii_lowercase(),
                }
            })
        })
        .collect::<Result<_, _>>()?;
    checkpoint.advance(&rows, limit_i64 as usize);
    Ok(rows)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoricalPriceExportCheckpoint {
    pub schema_version: u16,
    pub last_bucket_unix_seconds: i64,
    pub last_mint: String,
    pub last_provider: String,
    pub pages_completed: u64,
    pub rows_exported: u64,
    pub complete: bool,
}

impl HistoricalPriceExportCheckpoint {
    pub fn new(window_start_unix_seconds: i64) -> Self {
        Self {
            schema_version: CANONICAL_SCHEMA_VERSION,
            last_bucket_unix_seconds: window_start_unix_seconds.saturating_sub(1),
            last_mint: String::new(),
            last_provider: String::new(),
            pages_completed: 0,
            rows_exported: 0,
            complete: false,
        }
    }

    pub fn advance(&mut self, rows: &[HistoricalPriceMarkV1], requested_limit: usize) {
        if let Some(last) = rows.last() {
            self.last_bucket_unix_seconds = last.bucket_unix_seconds;
            self.last_mint.clone_from(&last.mint);
            self.last_provider.clone_from(&last.provider);
        }
        self.pages_completed = self.pages_completed.saturating_add(1);
        self.rows_exported = self.rows_exported.saturating_add(rows.len() as u64);
        self.complete = rows.len() < requested_limit;
    }
}

/// Exports only exact, positive, hour-aligned `quality = historical` provider
/// marks. `bucket_unix_seconds` is provider observation time, never chain time.
pub async fn export_historical_price_page(
    exporter: &ReadOnlyPgExporter,
    window_start_unix_seconds: i64,
    window_end_unix_seconds_exclusive: i64,
    mint_allowlist_csv: &str,
    provider: &str,
    checkpoint: &mut HistoricalPriceExportCheckpoint,
    limit: u32,
) -> Result<Vec<HistoricalPriceMarkV1>, ExtractError> {
    if window_start_unix_seconds >= window_end_unix_seconds_exclusive {
        return Err(ExtractError::InvalidHistoricalPriceWindow);
    }
    if mint_allowlist_csv.is_empty() || provider.is_empty() {
        return Err(ExtractError::DatabaseQuery);
    }
    let limit_i64 = i64::from(limit.max(1));
    let json_rows = exporter
        .query_json_text(
            PRICES_SQL,
            &[
                &window_start_unix_seconds,
                &window_end_unix_seconds_exclusive,
                &checkpoint.last_bucket_unix_seconds,
                &checkpoint.last_mint,
                &checkpoint.last_provider,
                &limit_i64,
                &mint_allowlist_csv,
                &provider,
            ],
        )
        .await?;
    let rows: Vec<HistoricalPriceMarkV1> = json_rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let mut deserializer = serde_json::Deserializer::from_str(&row);
            let mark: HistoricalPriceMarkV1 = serde_path_to_error::deserialize(&mut deserializer)
                .map_err(|error| ExtractError::DatabaseDecodeAt {
                path: format!("prices[{index}].{}", error.path()),
                category: format!("{:?}", error.inner().classify()).to_ascii_lowercase(),
            })?;
            mark.validate()
                .map_err(|_| ExtractError::DatabaseDecodeAt {
                    path: format!("prices[{index}]"),
                    category: "invalid_historical_price_mark".to_owned(),
                })?;
            Ok(mark)
        })
        .collect::<Result<_, _>>()?;
    checkpoint.advance(&rows, limit_i64 as usize);
    Ok(rows)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GetBlockRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'static str,
    pub params: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GetTransactionRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'static str,
    pub params: JsonValue,
}

pub fn finalized_get_block_request(slot: u64, request_id: u64) -> GetBlockRequest {
    GetBlockRequest {
        jsonrpc: "2.0",
        id: request_id,
        method: "getBlock",
        params: json!([
            slot,
            {
                "commitment": "finalized",
                "encoding": "json",
                "transactionDetails": "full",
                "rewards": false,
                "maxSupportedTransactionVersion": 0
            }
        ]),
    }
}

pub fn finalized_get_block_signatures_request(slot: u64, request_id: u64) -> GetBlockRequest {
    GetBlockRequest {
        jsonrpc: "2.0",
        id: request_id,
        method: "getBlock",
        params: json!([
            slot,
            {
                "commitment": "finalized",
                "encoding": "json",
                "transactionDetails": "signatures",
                "rewards": false,
                "maxSupportedTransactionVersion": 0
            }
        ]),
    }
}

pub fn finalized_get_transaction_request(
    signature: &str,
    request_id: u64,
) -> GetTransactionRequest {
    GetTransactionRequest {
        jsonrpc: "2.0",
        id: request_id,
        method: "getTransaction",
        params: json!([
            signature,
            {
                "commitment": "finalized",
                "encoding": "json",
                "maxSupportedTransactionVersion": 0
            }
        ]),
    }
}

fn canonical_method_request_sha256(method: &str, params: &JsonValue) -> String {
    sha256_hex(
        &serde_json::to_vec(&json!({
            "method": method,
            "params": params,
        }))
        .expect("serializing JSON values is infallible"),
    )
}

pub fn ordered_signature_block_request_sha256(slot: u64) -> String {
    let request = finalized_get_block_signatures_request(slot, 0);
    canonical_method_request_sha256(request.method, &request.params)
}

pub fn independent_get_transaction_request_sha256(signature: &str) -> String {
    let request = finalized_get_transaction_request(signature, 0);
    canonical_method_request_sha256(request.method, &request.params)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchivedBlockProofBasisV1 {
    /// The legacy/full-payload path. Its ordered commitment is unchanged: it
    /// hashes `(transaction_index, signature, rpc_payload_sha256)` for every
    /// transaction returned by finalized `getBlock`.
    FullPayloadV1,
    /// A lightweight proof of the same block position facts. The ordered
    /// signature inventory comes from finalized `getBlock` with
    /// `transactionDetails=signatures`; target payloads are independently
    /// proven by `getTransaction`.
    OrderedSignaturesV2 {
        ordered_signatures: Vec<String>,
        canonical_request_sha256: String,
        raw_response_sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchivedTransactionProofBasisV1 {
    FullBlockPayloadV1,
    IndependentGetTransactionV1 {
        canonical_request_sha256: String,
        raw_response_sha256: String,
        normalization: IndependentGetTransactionNormalizationV1,
    },
}

/// The sole allowed semantic normalization between finalized `getBlock` full
/// transaction entries and finalized `getTransaction` results. Live provider
/// equivalence established that `meta.rewards` is the only differing path
/// because the full-block request explicitly sets `rewards=false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndependentGetTransactionNormalizationV1 {
    RemoveMetaRewardsV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedTransaction {
    pub signature: String,
    pub transaction_index: u32,
    pub succeeded: bool,
    /// SHA-256 of the canonical JSON transaction-plus-metadata object returned
    /// by archival RPC. It binds every decoded operation to its raw payload.
    pub rpc_payload_sha256: String,
    pub proof_basis: ArchivedTransactionProofBasisV1,
    /// Retained for version-aware instruction/event decoding by the integration layer.
    pub transaction: JsonValue,
    pub meta: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedBlock {
    pub slot: u64,
    pub block_time_unix_seconds: Option<i64>,
    pub blockhash: String,
    pub previous_blockhash: String,
    pub parent_slot: u64,
    /// Number of transactions in the finalized RPC block before the extractor
    /// retains only transactions relevant to the V1 program.
    pub full_transaction_count: u64,
    /// Proof-basis-specific ordered commitment for the complete finalized
    /// block. `FullPayloadV1` preserves the legacy ordered
    /// `(transaction_index, signature, rpc_payload_sha256)` hash exactly;
    /// `OrderedSignaturesV2` hashes the ordered `(transaction_index,
    /// signature)` inventory.
    pub full_transaction_inventory_sha256: String,
    pub proof_basis: ArchivedBlockProofBasisV1,
    pub transactions: Vec<ArchivedTransaction>,
}

pub const INDEPENDENT_GET_TRANSACTION_PROOF_SCHEMA_VERSION: u16 = 1;

/// Context retained until an independently fetched target transaction is
/// composed into its ordered-signature block proof.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndependentGetTransactionProofV1 {
    pub schema_version: u16,
    pub slot: u64,
    pub block_time_unix_seconds: Option<i64>,
    pub signature: String,
    pub succeeded: bool,
    pub rpc_payload_sha256: String,
    pub canonical_request_sha256: String,
    pub raw_response_sha256: String,
    pub normalization: IndependentGetTransactionNormalizationV1,
    pub transaction: JsonValue,
    pub meta: JsonValue,
}

pub fn checksum_archived_block_artifact(
    block: &ArchivedBlock,
    logical_name: impl Into<String>,
    bytes: &[u8],
) -> BlockArtifactChecksumV1 {
    BlockArtifactChecksumV1::from_bytes(
        block.slot,
        block.blockhash.clone(),
        block.parent_slot,
        block.previous_blockhash.clone(),
        block.full_transaction_count,
        logical_name,
        bytes,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcFailureKind {
    Transport,
    RateLimited,
    BlockUnavailable,
    InvalidResponse,
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_solana_signature(value: &str) -> bool {
    bs58::decode(value)
        .into_vec()
        .is_ok_and(|decoded| decoded.len() == 64)
}

pub fn canonical_transaction_payload_sha256(transaction: &JsonValue, meta: &JsonValue) -> String {
    sha256_hex(
        &serde_json::to_vec(&json!({
            "transaction": transaction,
            "meta": meta,
        }))
        .expect("serializing JSON values is infallible"),
    )
}

pub fn normalize_independent_get_transaction_meta(
    mut meta: JsonValue,
) -> Result<JsonValue, RpcFailureKind> {
    let object = meta
        .as_object_mut()
        .ok_or(RpcFailureKind::InvalidResponse)?;
    object.remove("rewards");
    Ok(meta)
}

fn is_normalized_independent_get_transaction_meta(meta: &JsonValue) -> bool {
    meta.as_object()
        .is_some_and(|object| !object.contains_key("rewards"))
}

pub fn full_payload_transaction_inventory_sha256(
    transactions: &[ArchivedTransaction],
) -> Result<String, RpcFailureKind> {
    let inventory = transactions
        .iter()
        .map(|transaction| {
            json!({
                "transaction_index": transaction.transaction_index,
                "signature": transaction.signature,
                "rpc_payload_sha256": transaction.rpc_payload_sha256,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&inventory)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|_| RpcFailureKind::InvalidResponse)
}

pub fn ordered_signature_inventory_sha256(
    ordered_signatures: &[String],
) -> Result<String, RpcFailureKind> {
    let inventory = ordered_signatures
        .iter()
        .enumerate()
        .map(|(transaction_index, signature)| {
            Ok(json!({
                "transaction_index": u32::try_from(transaction_index)
                    .map_err(|_| RpcFailureKind::InvalidResponse)?,
                "signature": signature,
            }))
        })
        .collect::<Result<Vec<_>, RpcFailureKind>>()?;
    serde_json::to_vec(&inventory)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|_| RpcFailureKind::InvalidResponse)
}

fn valid_transaction_status(meta: &JsonValue, succeeded: bool) -> bool {
    meta.get("err")
        .is_some_and(|error| succeeded == error.is_null())
}

impl IndependentGetTransactionProofV1 {
    pub fn validate(&self) -> Result<(), RpcFailureKind> {
        if self.schema_version != INDEPENDENT_GET_TRANSACTION_PROOF_SCHEMA_VERSION
            || self.normalization != IndependentGetTransactionNormalizationV1::RemoveMetaRewardsV1
            || !valid_solana_signature(&self.signature)
            || !valid_sha256(&self.rpc_payload_sha256)
            || !valid_sha256(&self.canonical_request_sha256)
            || !valid_sha256(&self.raw_response_sha256)
            || self.canonical_request_sha256
                != independent_get_transaction_request_sha256(&self.signature)
            || self.rpc_payload_sha256
                != canonical_transaction_payload_sha256(&self.transaction, &self.meta)
            || !is_normalized_independent_get_transaction_meta(&self.meta)
            || !valid_transaction_status(&self.meta, self.succeeded)
            || self
                .transaction
                .get("signatures")
                .and_then(JsonValue::as_array)
                .and_then(|signatures| signatures.first())
                .and_then(JsonValue::as_str)
                != Some(self.signature.as_str())
        {
            return Err(RpcFailureKind::InvalidResponse);
        }
        Ok(())
    }
}

impl ArchivedTransaction {
    pub fn validate_proof_basis(&self) -> Result<(), RpcFailureKind> {
        if !valid_solana_signature(&self.signature)
            || !valid_sha256(&self.rpc_payload_sha256)
            || !valid_transaction_status(&self.meta, self.succeeded)
            || self
                .transaction
                .get("signatures")
                .and_then(JsonValue::as_array)
                .and_then(|signatures| signatures.first())
                .and_then(JsonValue::as_str)
                != Some(self.signature.as_str())
        {
            return Err(RpcFailureKind::InvalidResponse);
        }
        match &self.proof_basis {
            ArchivedTransactionProofBasisV1::FullBlockPayloadV1 => Ok(()),
            ArchivedTransactionProofBasisV1::IndependentGetTransactionV1 {
                canonical_request_sha256,
                raw_response_sha256,
                normalization,
            } if valid_sha256(canonical_request_sha256)
                && valid_sha256(raw_response_sha256)
                && *normalization
                    == IndependentGetTransactionNormalizationV1::RemoveMetaRewardsV1
                && *canonical_request_sha256
                    == independent_get_transaction_request_sha256(&self.signature)
                && is_normalized_independent_get_transaction_meta(&self.meta)
                && self.rpc_payload_sha256
                    == canonical_transaction_payload_sha256(&self.transaction, &self.meta) =>
            {
                Ok(())
            }
            _ => Err(RpcFailureKind::InvalidResponse),
        }
    }
}

impl ArchivedBlock {
    pub fn validate_proof_basis(&self) -> Result<(), RpcFailureKind> {
        if self.blockhash.trim().is_empty()
            || self.previous_blockhash.trim().is_empty()
            || (self.slot != 0 && self.parent_slot >= self.slot)
            || !valid_sha256(&self.full_transaction_inventory_sha256)
            || self.full_transaction_count < self.transactions.len() as u64
        {
            return Err(RpcFailureKind::InvalidResponse);
        }
        let mut indexes = BTreeSet::new();
        let mut retained_signatures = BTreeSet::new();
        for transaction in &self.transactions {
            if u64::from(transaction.transaction_index) >= self.full_transaction_count
                || !indexes.insert(transaction.transaction_index)
                || !retained_signatures.insert(transaction.signature.clone())
            {
                return Err(RpcFailureKind::InvalidResponse);
            }
            transaction.validate_proof_basis()?;
        }

        match &self.proof_basis {
            ArchivedBlockProofBasisV1::FullPayloadV1 => {
                if self.transactions.iter().any(|transaction| {
                    transaction.proof_basis != ArchivedTransactionProofBasisV1::FullBlockPayloadV1
                }) {
                    return Err(RpcFailureKind::InvalidResponse);
                }
                if self.full_transaction_count == self.transactions.len() as u64
                    && self.full_transaction_inventory_sha256
                        != full_payload_transaction_inventory_sha256(&self.transactions)?
                {
                    return Err(RpcFailureKind::InvalidResponse);
                }
            }
            ArchivedBlockProofBasisV1::OrderedSignaturesV2 {
                ordered_signatures,
                canonical_request_sha256,
                raw_response_sha256,
            } => {
                if ordered_signatures.len() as u64 != self.full_transaction_count
                    || ordered_signatures.is_empty()
                    || !valid_sha256(canonical_request_sha256)
                    || !valid_sha256(raw_response_sha256)
                    || *canonical_request_sha256
                        != ordered_signature_block_request_sha256(self.slot)
                    || self.full_transaction_inventory_sha256
                        != ordered_signature_inventory_sha256(ordered_signatures)?
                {
                    return Err(RpcFailureKind::InvalidResponse);
                }
                let mut all_signatures = BTreeSet::new();
                if ordered_signatures.iter().any(|signature| {
                    !valid_solana_signature(signature) || !all_signatures.insert(signature.clone())
                }) {
                    return Err(RpcFailureKind::InvalidResponse);
                }
                for transaction in &self.transactions {
                    if !matches!(
                        transaction.proof_basis,
                        ArchivedTransactionProofBasisV1::IndependentGetTransactionV1 { .. }
                    ) || ordered_signatures
                        .get(transaction.transaction_index as usize)
                        .map(String::as_str)
                        != Some(transaction.signature.as_str())
                    {
                        return Err(RpcFailureKind::InvalidResponse);
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn compose_ordered_signature_block(
    mut signature_block: ArchivedBlock,
    expected_target_signatures: &BTreeSet<String>,
    transaction_proofs: Vec<IndependentGetTransactionProofV1>,
) -> Result<ArchivedBlock, RpcFailureKind> {
    signature_block.validate_proof_basis()?;
    let ArchivedBlockProofBasisV1::OrderedSignaturesV2 {
        ordered_signatures, ..
    } = &signature_block.proof_basis
    else {
        return Err(RpcFailureKind::InvalidResponse);
    };
    if !signature_block.transactions.is_empty()
        || expected_target_signatures.is_empty()
        || expected_target_signatures.len() != transaction_proofs.len()
        || expected_target_signatures
            .iter()
            .any(|signature| !valid_solana_signature(signature))
    {
        return Err(RpcFailureKind::InvalidResponse);
    }

    let ordered_indexes = ordered_signatures
        .iter()
        .enumerate()
        .map(|(index, signature)| {
            u32::try_from(index)
                .map(|index| (signature.as_str(), index))
                .map_err(|_| RpcFailureKind::InvalidResponse)
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut seen = BTreeSet::new();
    let mut transactions = Vec::with_capacity(transaction_proofs.len());
    for proof in transaction_proofs {
        proof.validate()?;
        if proof.slot != signature_block.slot
            || proof.block_time_unix_seconds != signature_block.block_time_unix_seconds
            || !expected_target_signatures.contains(&proof.signature)
            || !seen.insert(proof.signature.clone())
        {
            return Err(RpcFailureKind::InvalidResponse);
        }
        let transaction_index = *ordered_indexes
            .get(proof.signature.as_str())
            .ok_or(RpcFailureKind::InvalidResponse)?;
        transactions.push(ArchivedTransaction {
            signature: proof.signature,
            transaction_index,
            succeeded: proof.succeeded,
            rpc_payload_sha256: proof.rpc_payload_sha256,
            proof_basis: ArchivedTransactionProofBasisV1::IndependentGetTransactionV1 {
                canonical_request_sha256: proof.canonical_request_sha256,
                raw_response_sha256: proof.raw_response_sha256,
                normalization: proof.normalization,
            },
            transaction: proof.transaction,
            meta: proof.meta,
        });
    }
    if seen != *expected_target_signatures {
        return Err(RpcFailureKind::InvalidResponse);
    }
    transactions.sort_by_key(|transaction| transaction.transaction_index);
    signature_block.transactions = transactions;
    signature_block.validate_proof_basis()?;
    Ok(signature_block)
}

/// Proves that a lightweight composition contains the exact same target
/// transaction/order evidence as a complete full-payload block. The only
/// ignored response difference is top-level `meta.rewards`, matching the
/// persisted `RemoveMetaRewardsV1` normalization rule.
pub fn validate_full_vs_composed_equivalence(
    full_payload_block: &ArchivedBlock,
    composed_block: &ArchivedBlock,
    expected_target_signatures: &BTreeSet<String>,
) -> Result<(), RpcFailureKind> {
    full_payload_block.validate_proof_basis()?;
    composed_block.validate_proof_basis()?;
    if !matches!(
        full_payload_block.proof_basis,
        ArchivedBlockProofBasisV1::FullPayloadV1
    ) || !matches!(
        composed_block.proof_basis,
        ArchivedBlockProofBasisV1::OrderedSignaturesV2 { .. }
    ) || full_payload_block.full_transaction_count
        != full_payload_block.transactions.len() as u64
        || full_payload_block.slot != composed_block.slot
        || full_payload_block.block_time_unix_seconds != composed_block.block_time_unix_seconds
        || full_payload_block.blockhash != composed_block.blockhash
        || full_payload_block.previous_blockhash != composed_block.previous_blockhash
        || full_payload_block.parent_slot != composed_block.parent_slot
        || full_payload_block.full_transaction_count != composed_block.full_transaction_count
        || composed_block.transactions.len() != expected_target_signatures.len()
    {
        return Err(RpcFailureKind::InvalidResponse);
    }

    let full_by_signature = full_payload_block
        .transactions
        .iter()
        .map(|transaction| (transaction.signature.as_str(), transaction))
        .collect::<BTreeMap<_, _>>();
    let composed_by_signature = composed_block
        .transactions
        .iter()
        .map(|transaction| (transaction.signature.as_str(), transaction))
        .collect::<BTreeMap<_, _>>();
    for signature in expected_target_signatures {
        let full = full_by_signature
            .get(signature.as_str())
            .ok_or(RpcFailureKind::InvalidResponse)?;
        let composed = composed_by_signature
            .get(signature.as_str())
            .ok_or(RpcFailureKind::InvalidResponse)?;
        if full.signature != composed.signature
            || full.transaction_index != composed.transaction_index
            || full.succeeded != composed.succeeded
            || full.transaction != composed.transaction
            || normalize_independent_get_transaction_meta(full.meta.clone())? != composed.meta
        {
            return Err(RpcFailureKind::InvalidResponse);
        }
    }
    Ok(())
}

/// Transport errors are intentionally categorical so endpoint URLs and API keys
/// cannot leak through checkpoint files or logs.
pub trait ArchivalRpcTransport: Send + Sync {
    fn get_block<'a>(
        &'a self,
        request: GetBlockRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ArchivedBlock>, RpcFailureKind>> + Send + 'a>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcCheckpoint {
    pub schema_version: u16,
    pub completed_slots: BTreeSet<u64>,
    pub failures: BTreeMap<u64, RpcFailureKind>,
    pub requests_completed: u64,
}

impl Default for RpcCheckpoint {
    fn default() -> Self {
        Self {
            schema_version: CANONICAL_SCHEMA_VERSION,
            completed_slots: BTreeSet::new(),
            failures: BTreeMap::new(),
            requests_completed: 0,
        }
    }
}

/// Fetches at most `max_requests` unfinished slots and updates a serializable
/// checkpoint after every response. Re-running skips completed slots and retries
/// failures. The caller owns durable checkpoint writes.
pub async fn fetch_blocks_resumable<T: ArchivalRpcTransport>(
    transport: &T,
    slots: &[u64],
    checkpoint: &mut RpcCheckpoint,
    max_requests: usize,
) -> Vec<ArchivedBlock> {
    let mut blocks = Vec::new();
    let mut requested = 0usize;
    for slot in slots.iter().copied() {
        if requested >= max_requests || checkpoint.completed_slots.contains(&slot) {
            continue;
        }
        requested += 1;
        let request_id = checkpoint.requests_completed.saturating_add(1);
        checkpoint.requests_completed = request_id;
        match transport
            .get_block(finalized_get_block_request(slot, request_id))
            .await
        {
            Ok(Some(block)) if block.slot == slot => {
                checkpoint.completed_slots.insert(slot);
                checkpoint.failures.remove(&slot);
                blocks.push(block);
            }
            Ok(Some(_)) => {
                checkpoint
                    .failures
                    .insert(slot, RpcFailureKind::InvalidResponse);
            }
            Ok(None) => {
                checkpoint
                    .failures
                    .insert(slot, RpcFailureKind::BlockUnavailable);
            }
            Err(kind) => {
                checkpoint.failures.insert(slot, kind);
            }
        }
    }
    blocks
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrichedDbEventSeed {
    pub db: DbEventSeed,
    pub block_time_unix_seconds: i64,
    pub transaction_index: u32,
    pub parsed_instruction_path: Option<Vec<u32>>,
}

pub fn parse_instruction_path(path: &str) -> Result<Vec<u32>, ExtractError> {
    if path.is_empty() {
        return Err(ExtractError::InvalidInstructionPath);
    }
    path.split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(ExtractError::InvalidInstructionPath);
            }
            part.parse::<u32>()
                .map_err(|_| ExtractError::InvalidInstructionPath)
        })
        .collect()
}

/// Adds only facts available authoritatively from `getBlock`.
///
/// Instruction/event sequence for lending rows missing DB paths must still come
/// from the version-aware RPC decoder before constructing a canonical event.
pub fn enrich_db_event(
    seed: DbEventSeed,
    block: &ArchivedBlock,
) -> Result<EnrichedDbEventSeed, ExtractError> {
    if block.slot != seed.slot {
        return Err(ExtractError::RpcSlotMismatch {
            requested: seed.slot,
            returned: block.slot,
        });
    }
    let block_time_unix_seconds = block
        .block_time_unix_seconds
        .ok_or(ExtractError::RpcBlockTimeMissing(block.slot))?;
    let matches: Vec<&ArchivedTransaction> = block
        .transactions
        .iter()
        .filter(|transaction| transaction.signature == seed.signature)
        .collect();
    let transaction = match matches.as_slice() {
        [] => {
            return Err(ExtractError::SignatureMissing {
                slot: seed.slot,
                signature: seed.signature.clone(),
            })
        }
        [transaction] => *transaction,
        _ => {
            return Err(ExtractError::DuplicateSignature {
                slot: seed.slot,
                signature: seed.signature.clone(),
            })
        }
    };
    let parsed_instruction_path = seed
        .instruction_path
        .as_deref()
        .map(parse_instruction_path)
        .transpose()?;
    Ok(EnrichedDbEventSeed {
        db: seed,
        block_time_unix_seconds,
        transaction_index: transaction.transaction_index,
        parsed_instruction_path,
    })
}

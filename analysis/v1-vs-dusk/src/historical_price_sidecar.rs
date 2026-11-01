//! Proof-bound integration for an immutable historical-price provider sidecar.
//!
//! This module performs no network or database access. It accepts a sidecar
//! only when the config-pinned manifest and files, every raw provider response,
//! every per-mark provenance record, and the exact canonical source inputs all
//! validate. The merge rejects duplicates and produces a self-hashed receipt.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{bail, Context, Result};
use reqwest::Url;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::{
    canonical::{ExactDecimal, HistoricalPriceMarkV1},
    config::{HistoricalPriceSidecarConfig, HistoricalPriceSidecarPolicy},
};

pub const HISTORICAL_PRICE_SIDECAR_RECEIPT_SCHEMA_VERSION: u16 = 1;
const FETCH_MANIFEST_FILE_NAME: &str = "fetch-manifest.json";
const MARKS_FILE_NAME: &str = "historical-price-marks.jsonl";
const PROVENANCE_FILE_NAME: &str = "historical-price-mark-provenance.jsonl";
const UNRESOLVED_FILE_NAME: &str = "unresolved-price-keys.jsonl";
const RECEIPT_HASH_DOMAIN: &[u8] = b"v1-vs-dusk/historical-price-sidecar-merge-receipt/v1\0";
const EXPECTED_PURPOSE: &str = "proof_bound_read_only_historical_provider_sidecar";
const EXPECTED_CHAIN: &str = "solana";
const EXPECTED_ENDPOINT: &str = "/defi/history_price";
const EXPECTED_INTERVAL: &str = "1H";
const HOUR_SECONDS: i64 = 3_600;
static PARTIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct HistoricalPriceSidecarMergeSpec<'a> {
    /// Resolved sidecar root from `source.historical_price_sidecar.root`.
    pub sidecar_root: &'a Path,
    pub binding: &'a HistoricalPriceSidecarConfig,
    /// Exact canonical operations used to classify the missing mint-hours.
    pub canonical_operations_path: &'a Path,
    /// Database-exported historical marks before sidecar integration.
    pub original_historical_prices_path: &'a Path,
    /// Deterministically sorted database + sidecar marks.
    pub merged_historical_prices_output_path: &'a Path,
    /// Self-hashed receipt binding all source and output commitments.
    pub receipt_output_path: &'a Path,
    pub expected_provider: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalPriceSidecarMergeReceiptV1 {
    pub schema_version: u16,
    pub provider: String,
    pub source_manifest_file_sha256: String,
    pub source_provider_responses_manifest_sha256: String,
    pub source_marks_file_sha256: String,
    pub source_provenance_file_sha256: String,
    pub source_unresolved_file_sha256: String,
    pub source_classification_summary_sha256: String,
    pub source_missing_keys_sha256: String,
    pub canonical_operations_sha256: String,
    pub original_historical_prices_sha256: String,
    pub merged_historical_prices_sha256: String,
    pub provider_response_count: u64,
    pub canonical_operation_rows: u64,
    pub original_historical_price_rows: u64,
    pub sidecar_historical_price_rows: u64,
    pub merged_historical_price_rows: u64,
    pub recovered_operation_references: u64,
    pub current_price_fallback: bool,
    pub interpolation: bool,
    pub forward_fill: bool,
    pub receipt_sha256: String,
}

impl HistoricalPriceSidecarMergeReceiptV1 {
    fn seal(mut self) -> Result<Self> {
        self.receipt_sha256.clear();
        self.receipt_sha256 = self.compute_receipt_sha256()?;
        Ok(self)
    }

    pub fn verify_self_hash(&self) -> Result<()> {
        if !valid_sha256(&self.receipt_sha256) {
            bail!("historical-price sidecar receipt SHA-256 is not canonical");
        }
        if self.receipt_sha256 != self.compute_receipt_sha256()? {
            bail!("historical-price sidecar receipt self-hash mismatch");
        }
        Ok(())
    }

    fn compute_receipt_sha256(&self) -> Result<String> {
        let mut unsigned = self.clone();
        unsigned.receipt_sha256.clear();
        let bytes = serde_json::to_vec(&unsigned)?;
        let mut digest = Sha256::new();
        digest.update(RECEIPT_HASH_DOMAIN);
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
        Ok(hex::encode(digest.finalize()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PriceKey {
    bucket_unix_seconds: i64,
    mint: String,
}

impl PriceKey {
    fn new(mint: &str, bucket_unix_seconds: i64) -> Self {
        Self {
            bucket_unix_seconds,
            mint: mint.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileProofV1 {
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderResponseProofV1 {
    request_index: u64,
    path: String,
    sha256: String,
    ok: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FetchManifestSourceInputsV1 {
    classification_summary: FileProofV1,
    missing_price_keys: FileProofV1,
    sidecar_script: FileProofV1,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FetchRequestPlanV1 {
    chunk_hours: u64,
    complete_request_count: u64,
    executed_request_count: u64,
    complete: bool,
    timeout_ms: u64,
    retries: u64,
    delay_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FetchOutcomeV1 {
    processed_unique_keys: u64,
    recovered_unique_keys: u64,
    recovered_operation_references: u64,
    unresolved_unique_keys: u64,
    unresolved_operation_references: u64,
    unresolved_by_reason: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FetchArtifactsV1 {
    historical_price_marks: FileProofV1,
    historical_price_mark_provenance: FileProofV1,
    unresolved_price_keys: FileProofV1,
    provider_responses: Vec<ProviderResponseProofV1>,
    provider_responses_manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FetchManifestV1 {
    schema_version: u16,
    created_at: String,
    purpose: String,
    provider: String,
    chain: String,
    endpoint: String,
    interval: String,
    allow_current_fallback: bool,
    interpolation: bool,
    forward_fill: bool,
    source_inputs: FetchManifestSourceInputsV1,
    request_plan: FetchRequestPlanV1,
    outcome: FetchOutcomeV1,
    artifacts: FetchArtifactsV1,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderRequestV1 {
    method: String,
    url: String,
    provider: String,
    chain: String,
    interval: String,
    allow_current_fallback: bool,
    requested_bucket_count: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderAttemptV1 {
    started_at: String,
    completed_at: String,
    status: Option<u16>,
    ok: bool,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    response_body_sha256: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    body_text: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<JsonValue>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderResponseV1 {
    schema_version: u16,
    request_index: u64,
    request: ProviderRequestV1,
    attempts: Vec<ProviderAttemptV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MarkProvenanceV1 {
    schema_version: u16,
    mint: String,
    bucket_unix_seconds: i64,
    raw_provider_timestamp_unix_seconds: i64,
    price_usd: ExactDecimal,
    request_index: u64,
    provider_response_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MissingPriceKeyV1 {
    schema_version: u16,
    mint: String,
    bucket_unix_seconds: i64,
    token_decimals: u8,
    operation_references: u64,
    markets: Vec<String>,
    db_classification: String,
    db_row_count: u64,
}

#[derive(Clone, Debug)]
struct ParsedRequest {
    mint: String,
    from: i64,
    to_exclusive: i64,
}

#[derive(Clone, Debug)]
struct VerifiedResponse {
    request_index: u64,
    response_sha256: String,
    request: ParsedRequest,
    requested_bucket_count: u64,
    points: BTreeMap<i64, Vec<String>>,
}

/// Validates a config-pinned, read-only historical-price sidecar and merges it
/// into the exact database export used when the gap set was classified.
pub fn validate_and_merge_historical_price_sidecar(
    spec: &HistoricalPriceSidecarMergeSpec<'_>,
) -> Result<HistoricalPriceSidecarMergeReceiptV1> {
    validate_spec(spec)?;

    let manifest_path = spec.sidecar_root.join(FETCH_MANIFEST_FILE_NAME);
    let manifest_bytes = fs::read(&manifest_path)
        .with_context(|| format!("reading sidecar manifest {}", manifest_path.display()))?;
    let manifest_file_sha256 = sha256_bytes(&manifest_bytes);
    require_hash(
        "sidecar manifest file",
        &manifest_file_sha256,
        &spec.binding.manifest_file_sha256,
    )?;
    let raw_manifest: JsonValue = serde_json::from_slice(&manifest_bytes)
        .context("parsing historical-price sidecar manifest JSON")?;
    let manifest: FetchManifestV1 = serde_json::from_value(raw_manifest.clone())
        .context("decoding historical-price sidecar manifest schema")?;
    if serde_json::to_value(&manifest)? != raw_manifest {
        bail!("historical-price sidecar manifest contains drifted fields");
    }
    validate_manifest_policy(&manifest, spec)?;

    let response_manifest_value = raw_manifest
        .pointer("/artifacts/provider_responses")
        .context("sidecar manifest is missing provider response commitments")?;
    let response_manifest_sha256 = sha256_bytes(&stable_json_bytes(response_manifest_value)?);
    require_hash(
        "provider response manifest",
        &response_manifest_sha256,
        &manifest.artifacts.provider_responses_manifest_sha256,
    )?;

    let marks_path = spec.sidecar_root.join(MARKS_FILE_NAME);
    let provenance_path = spec.sidecar_root.join(PROVENANCE_FILE_NAME);
    let unresolved_path = spec.sidecar_root.join(UNRESOLVED_FILE_NAME);
    validate_declared_path(&manifest.artifacts.historical_price_marks.path, &marks_path)?;
    validate_declared_path(
        &manifest.artifacts.historical_price_mark_provenance.path,
        &provenance_path,
    )?;
    validate_declared_path(
        &manifest.artifacts.unresolved_price_keys.path,
        &unresolved_path,
    )?;

    let marks_sha256 = sha256_file(&marks_path)?;
    require_hash(
        "sidecar marks",
        &marks_sha256,
        &manifest.artifacts.historical_price_marks.sha256,
    )?;
    require_hash(
        "config-pinned sidecar marks",
        &marks_sha256,
        &spec.binding.historical_price_marks_file_sha256,
    )?;
    let provenance_sha256 = sha256_file(&provenance_path)?;
    require_hash(
        "sidecar provenance",
        &provenance_sha256,
        &manifest.artifacts.historical_price_mark_provenance.sha256,
    )?;
    require_hash(
        "config-pinned sidecar provenance",
        &provenance_sha256,
        &spec.binding.historical_price_mark_provenance_file_sha256,
    )?;
    let unresolved_sha256 = sha256_file(&unresolved_path)?;
    require_hash(
        "sidecar unresolved-key artifact",
        &unresolved_sha256,
        &manifest.artifacts.unresolved_price_keys.sha256,
    )?;
    if fs::metadata(&unresolved_path)?.len() != 0 {
        bail!("historical-price sidecar contains unresolved keys");
    }

    let classification_path = Path::new(&manifest.source_inputs.classification_summary.path);
    let missing_keys_path = Path::new(&manifest.source_inputs.missing_price_keys.path);
    let sidecar_script_path = Path::new(&manifest.source_inputs.sidecar_script.path);
    verify_declared_source_file(
        "classification summary",
        classification_path,
        &manifest.source_inputs.classification_summary.sha256,
    )?;
    verify_declared_source_file(
        "missing-price keys",
        missing_keys_path,
        &manifest.source_inputs.missing_price_keys.sha256,
    )?;
    verify_declared_source_file(
        "sidecar script",
        sidecar_script_path,
        &manifest.source_inputs.sidecar_script.sha256,
    )?;

    let classification: JsonValue = serde_json::from_slice(&fs::read(classification_path)?)
        .context("parsing historical-price classification summary")?;
    let canonical_operations_sha256 = sha256_file(spec.canonical_operations_path)?;
    let original_historical_prices_sha256 = sha256_file(spec.original_historical_prices_path)?;
    validate_classification_sources(
        &classification,
        spec,
        &canonical_operations_sha256,
        &original_historical_prices_sha256,
        &manifest,
    )?;

    let missing_keys: Vec<MissingPriceKeyV1> = read_strict_jsonl(missing_keys_path)?;
    let (expected_keys, recovered_operation_references) =
        validate_missing_keys(&missing_keys, spec.binding.expected_unique_keys)?;
    let sidecar_marks: Vec<HistoricalPriceMarkV1> = read_strict_jsonl(&marks_path)?;
    let sidecar_by_key = validate_sidecar_marks(
        &sidecar_marks,
        &missing_keys,
        &expected_keys,
        spec.expected_provider,
    )?;
    let provenance: Vec<MarkProvenanceV1> = read_strict_jsonl(&provenance_path)?;
    let responses = validate_provider_responses(spec.sidecar_root, &manifest)?;
    validate_provenance(&provenance, &sidecar_by_key, &responses, &expected_keys)?;

    validate_outcome(
        &manifest,
        expected_keys.len() as u64,
        recovered_operation_references,
        responses.len() as u64,
    )?;

    let original_marks: Vec<HistoricalPriceMarkV1> =
        read_strict_jsonl(spec.original_historical_prices_path)?;
    let original_by_key =
        validate_original_marks(&original_marks, spec.expected_provider, &expected_keys)?;
    let merged_bytes = merged_jsonl_bytes(&original_by_key, &sidecar_by_key)?;
    let merged_sha256 = sha256_bytes(&merged_bytes);
    let canonical_operation_rows = count_strict_nonblank_lines(spec.canonical_operations_path)?;

    let receipt = HistoricalPriceSidecarMergeReceiptV1 {
        schema_version: HISTORICAL_PRICE_SIDECAR_RECEIPT_SCHEMA_VERSION,
        provider: spec.expected_provider.to_owned(),
        source_manifest_file_sha256: manifest_file_sha256,
        source_provider_responses_manifest_sha256: response_manifest_sha256,
        source_marks_file_sha256: marks_sha256,
        source_provenance_file_sha256: provenance_sha256,
        source_unresolved_file_sha256: unresolved_sha256,
        source_classification_summary_sha256: manifest.source_inputs.classification_summary.sha256,
        source_missing_keys_sha256: manifest.source_inputs.missing_price_keys.sha256,
        canonical_operations_sha256,
        original_historical_prices_sha256,
        merged_historical_prices_sha256: merged_sha256,
        provider_response_count: responses.len() as u64,
        canonical_operation_rows,
        original_historical_price_rows: original_marks.len() as u64,
        sidecar_historical_price_rows: sidecar_marks.len() as u64,
        merged_historical_price_rows: (original_marks.len() + sidecar_marks.len()) as u64,
        recovered_operation_references,
        current_price_fallback: false,
        interpolation: false,
        forward_fill: false,
        receipt_sha256: String::new(),
    }
    .seal()?;
    receipt.verify_self_hash()?;

    let mut receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
    receipt_bytes.push(b'\n');
    write_outputs_atomically(spec, &merged_bytes, &receipt_bytes)?;
    require_hash(
        "committed merged historical prices",
        &sha256_file(spec.merged_historical_prices_output_path)?,
        &receipt.merged_historical_prices_sha256,
    )?;
    let committed_receipt: HistoricalPriceSidecarMergeReceiptV1 =
        serde_json::from_slice(&fs::read(spec.receipt_output_path)?)?;
    if committed_receipt != receipt {
        bail!("committed historical-price sidecar receipt differs from the sealed receipt");
    }
    committed_receipt.verify_self_hash()?;
    Ok(receipt)
}

fn validate_spec(spec: &HistoricalPriceSidecarMergeSpec<'_>) -> Result<()> {
    if spec.binding.policy != HistoricalPriceSidecarPolicy::RequireCompleteProofs {
        bail!("historical-price sidecar policy does not require complete proofs");
    }
    if spec.expected_provider.trim().is_empty() {
        bail!("historical-price sidecar expected provider is empty");
    }
    for (name, digest) in [
        ("manifest_file_sha256", &spec.binding.manifest_file_sha256),
        (
            "historical_price_marks_file_sha256",
            &spec.binding.historical_price_marks_file_sha256,
        ),
        (
            "historical_price_mark_provenance_file_sha256",
            &spec.binding.historical_price_mark_provenance_file_sha256,
        ),
    ] {
        if !valid_sha256(digest) {
            bail!("historical-price sidecar config {name} is not a canonical SHA-256");
        }
    }
    if spec.binding.expected_unique_keys == 0 {
        bail!("historical-price sidecar expected coverage is zero");
    }
    let paths = [
        spec.sidecar_root,
        spec.canonical_operations_path,
        spec.original_historical_prices_path,
        spec.merged_historical_prices_output_path,
        spec.receipt_output_path,
    ];
    for left in 0..paths.len() {
        for right in (left + 1)..paths.len() {
            if paths[left] == paths[right] {
                bail!("historical-price sidecar integration paths must be distinct");
            }
        }
    }
    Ok(())
}

fn validate_manifest_policy(
    manifest: &FetchManifestV1,
    spec: &HistoricalPriceSidecarMergeSpec<'_>,
) -> Result<()> {
    if manifest.schema_version != 1
        || manifest.purpose != EXPECTED_PURPOSE
        || manifest.provider != spec.expected_provider
        || manifest.chain != EXPECTED_CHAIN
        || manifest.endpoint != EXPECTED_ENDPOINT
        || manifest.interval != EXPECTED_INTERVAL
    {
        bail!("historical-price sidecar identity or provider policy mismatch");
    }
    if manifest.created_at.trim().is_empty() {
        bail!("historical-price sidecar creation timestamp is empty");
    }
    if manifest.allow_current_fallback || manifest.interpolation || manifest.forward_fill {
        bail!("historical-price sidecar enables a forbidden price fallback");
    }
    if !manifest.request_plan.complete
        || manifest.request_plan.complete_request_count == 0
        || manifest.request_plan.complete_request_count
            != manifest.request_plan.executed_request_count
        || manifest.request_plan.executed_request_count
            != manifest.artifacts.provider_responses.len() as u64
        || manifest.request_plan.chunk_hours == 0
        || manifest.request_plan.timeout_ms == 0
    {
        bail!("historical-price sidecar request plan is incomplete or inconsistent");
    }
    if !valid_sha256(&manifest.artifacts.provider_responses_manifest_sha256) {
        bail!("historical-price sidecar provider response manifest hash is invalid");
    }
    Ok(())
}

fn validate_classification_sources(
    classification: &JsonValue,
    spec: &HistoricalPriceSidecarMergeSpec<'_>,
    canonical_operations_sha256: &str,
    original_historical_prices_sha256: &str,
    manifest: &FetchManifestV1,
) -> Result<()> {
    require_json_u64(classification, "/schema_version", 1)?;
    require_json_string(
        classification,
        "/provider",
        spec.expected_provider,
        "classification provider",
    )?;
    require_json_string(
        classification,
        "/source_inputs/canonical_operations/sha256",
        canonical_operations_sha256,
        "classified canonical operations SHA-256",
    )?;
    require_json_string(
        classification,
        "/source_inputs/exported_historical_prices/sha256",
        original_historical_prices_sha256,
        "classified historical-price export SHA-256",
    )?;
    let declared_canonical_path = classification
        .pointer("/source_inputs/canonical_operations/path")
        .and_then(JsonValue::as_str)
        .context("classification summary lacks canonical operations path")?;
    require_logical_file_name(declared_canonical_path, "canonical-operations.jsonl")?;
    let declared_prices_path = classification
        .pointer("/source_inputs/exported_historical_prices/path")
        .and_then(JsonValue::as_str)
        .context("classification summary lacks historical-price export path")?;
    require_logical_file_name(declared_prices_path, "historical-prices.jsonl")?;
    if json_u64(classification, "/source_inputs/canonical_operations/rows")?
        != count_strict_nonblank_lines(spec.canonical_operations_path)?
    {
        bail!("classified canonical operation row count differs from the new extraction");
    }
    if json_u64(
        classification,
        "/source_inputs/exported_historical_prices/rows",
    )? != count_strict_nonblank_lines(spec.original_historical_prices_path)?
    {
        bail!("classified database price row count differs from the new extraction");
    }

    let missing_unique = json_u64(classification, "/audit_grain/missing_unique_mint_hour_keys")?;
    if missing_unique != spec.binding.expected_unique_keys
        || missing_unique != manifest.outcome.processed_unique_keys
    {
        bail!("classified missing-key coverage differs from the pinned sidecar coverage");
    }
    let expected_unique = json_u64(
        classification,
        "/audit_grain/expected_unique_mint_hour_keys",
    )?;
    let observed_unique = json_u64(
        classification,
        "/audit_grain/exported_expected_unique_mint_hour_keys",
    )?;
    if observed_unique.checked_add(missing_unique) != Some(expected_unique) {
        bail!("historical-price classification unique-key accounting does not conserve");
    }
    let missing_references = json_u64(classification, "/audit_grain/missing_operation_references")?;
    if missing_references != manifest.outcome.recovered_operation_references {
        bail!("historical-price classification reference coverage differs from sidecar outcome");
    }
    require_json_string(
        classification,
        "/artifacts/missing_price_keys/sha256",
        &manifest.source_inputs.missing_price_keys.sha256,
        "classification missing-key SHA-256",
    )?;
    Ok(())
}

fn validate_missing_keys(
    rows: &[MissingPriceKeyV1],
    expected_count: u64,
) -> Result<(BTreeSet<PriceKey>, u64)> {
    if rows.len() as u64 != expected_count {
        bail!("missing-price key row count differs from config-pinned coverage");
    }
    let mut keys = BTreeSet::new();
    let mut previous: Option<PriceKey> = None;
    let mut references = 0_u64;
    for row in rows {
        if row.schema_version != 1
            || row.mint.trim().is_empty()
            || row.bucket_unix_seconds.rem_euclid(HOUR_SECONDS) != 0
            || row.operation_references == 0
            || row.markets.is_empty()
            || row.markets.iter().any(|market| market.trim().is_empty())
            || row.db_classification == "configured_provider_historical_positive"
        {
            bail!("missing-price key row is invalid or was already historically covered");
        }
        let mut sorted_markets = row.markets.clone();
        sorted_markets.sort();
        sorted_markets.dedup();
        if sorted_markets != row.markets {
            bail!("missing-price key market list is not unique and sorted");
        }
        let key = PriceKey::new(&row.mint, row.bucket_unix_seconds);
        if previous.as_ref().is_some_and(|prior| prior >= &key) || !keys.insert(key.clone()) {
            bail!("missing-price keys are duplicated or not strictly sorted");
        }
        previous = Some(key);
        references = references
            .checked_add(row.operation_references)
            .context("missing-price operation reference count overflow")?;
    }
    Ok((keys, references))
}

fn validate_sidecar_marks(
    marks: &[HistoricalPriceMarkV1],
    missing_rows: &[MissingPriceKeyV1],
    expected_keys: &BTreeSet<PriceKey>,
    provider: &str,
) -> Result<BTreeMap<PriceKey, HistoricalPriceMarkV1>> {
    if marks.len() != missing_rows.len() || marks.len() != expected_keys.len() {
        bail!("historical-price sidecar marks do not exactly cover the missing-key set");
    }
    let missing_by_key: BTreeMap<PriceKey, &MissingPriceKeyV1> = missing_rows
        .iter()
        .map(|row| (PriceKey::new(&row.mint, row.bucket_unix_seconds), row))
        .collect();
    let mut result = BTreeMap::new();
    let mut previous: Option<PriceKey> = None;
    for mark in marks {
        mark.validate()
            .context("invalid historical-price sidecar mark")?;
        if mark.provider != provider {
            bail!("historical-price sidecar mark uses the wrong provider");
        }
        let key = PriceKey::new(&mark.mint, mark.bucket_unix_seconds);
        if previous.as_ref().is_some_and(|prior| prior >= &key)
            || result.insert(key.clone(), mark.clone()).is_some()
        {
            bail!("historical-price sidecar marks are duplicated or not strictly sorted");
        }
        let missing = missing_by_key
            .get(&key)
            .context("historical-price sidecar mark was not requested")?;
        if mark.token_decimals != Some(missing.token_decimals) {
            bail!("historical-price sidecar mark token decimals differ from canonical identity");
        }
        previous = Some(key);
    }
    if result.keys().cloned().collect::<BTreeSet<_>>() != *expected_keys {
        bail!("historical-price sidecar mark keys differ from the complete missing-key set");
    }
    Ok(result)
}

fn validate_provider_responses(
    sidecar_root: &Path,
    manifest: &FetchManifestV1,
) -> Result<BTreeMap<u64, VerifiedResponse>> {
    let response_root = sidecar_root.join("provider-responses");
    let mut responses = BTreeMap::new();
    for (position, proof) in manifest.artifacts.provider_responses.iter().enumerate() {
        if proof.request_index != position as u64 || !proof.ok || !valid_sha256(&proof.sha256) {
            bail!("provider response commitments are incomplete or not canonically ordered");
        }
        let declared = Path::new(&proof.path);
        let file_name = declared
            .file_name()
            .context("provider response commitment has no file name")?;
        let expected_path = response_root.join(file_name);
        validate_declared_path(&proof.path, &expected_path)?;
        require_hash(
            "raw provider response",
            &sha256_file(&expected_path)?,
            &proof.sha256,
        )?;
        let raw: JsonValue = serde_json::from_slice(&fs::read(&expected_path)?)
            .context("parsing raw historical-price provider response")?;
        let response: ProviderResponseV1 = serde_json::from_value(raw.clone())
            .context("decoding raw historical-price provider response schema")?;
        if response.schema_version != 1 || response.request_index != proof.request_index {
            bail!("raw provider response identity differs from its manifest commitment");
        }
        let parsed_request = validate_provider_request(&response.request, &manifest.provider)?;
        let last_attempt = response
            .attempts
            .last()
            .context("raw provider response has no attempts")?;
        if response.attempts[..response.attempts.len() - 1]
            .iter()
            .any(|attempt| attempt.ok)
        {
            bail!("provider response records attempts after a successful request");
        }
        let body = validate_successful_attempt(last_attempt)?;
        let points = extract_exact_provider_points(&body, &parsed_request.mint)?;
        let verified = VerifiedResponse {
            request_index: proof.request_index,
            response_sha256: proof.sha256.clone(),
            request: parsed_request,
            requested_bucket_count: response.request.requested_bucket_count,
            points,
        };
        if responses.insert(proof.request_index, verified).is_some() {
            bail!("duplicate provider response request index");
        }
    }
    Ok(responses)
}

fn validate_provider_request(request: &ProviderRequestV1, provider: &str) -> Result<ParsedRequest> {
    if request.method != "GET"
        || request.provider != provider
        || request.chain != EXPECTED_CHAIN
        || request.interval != EXPECTED_INTERVAL
        || request.allow_current_fallback
        || request.requested_bucket_count == 0
    {
        bail!("historical-price provider request violates the frozen request policy");
    }
    let url = Url::parse(&request.url).context("parsing historical-price provider request URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() != EXPECTED_ENDPOINT
    {
        bail!("historical-price provider request URL is unsafe or uses the wrong endpoint");
    }
    let mut query = BTreeMap::new();
    for (name, value) in url.query_pairs() {
        if query
            .insert(name.into_owned(), value.into_owned())
            .is_some()
        {
            bail!("historical-price provider request has duplicate query parameters");
        }
    }
    let expected_names = BTreeSet::from([
        "address".to_owned(),
        "address_type".to_owned(),
        "time_from".to_owned(),
        "time_to".to_owned(),
        "type".to_owned(),
        "ui_amount_mode".to_owned(),
    ]);
    if query.keys().cloned().collect::<BTreeSet<_>>() != expected_names
        || query.get("address_type").map(String::as_str) != Some("token")
        || query.get("type").map(String::as_str) != Some(EXPECTED_INTERVAL)
        || query.get("ui_amount_mode").map(String::as_str) != Some("raw")
    {
        bail!("historical-price provider request query violates historical-only policy");
    }
    let mint = query
        .get("address")
        .filter(|value| !value.trim().is_empty())
        .context("historical-price provider request address is empty")?
        .clone();
    let from = query
        .get("time_from")
        .context("historical-price provider request lacks time_from")?
        .parse::<i64>()
        .context("historical-price provider time_from is invalid")?;
    let to_exclusive = query
        .get("time_to")
        .context("historical-price provider request lacks time_to")?
        .parse::<i64>()
        .context("historical-price provider time_to is invalid")?;
    if from.rem_euclid(HOUR_SECONDS) != 0
        || to_exclusive.rem_euclid(HOUR_SECONDS) != 0
        || to_exclusive <= from
    {
        bail!("historical-price provider request range is invalid");
    }
    Ok(ParsedRequest {
        mint,
        from,
        to_exclusive,
    })
}

fn validate_successful_attempt(attempt: &ProviderAttemptV1) -> Result<JsonValue> {
    if attempt.started_at.trim().is_empty()
        || attempt.completed_at.trim().is_empty()
        || attempt.status != Some(200)
        || !attempt.ok
        || attempt.error.is_some()
    {
        bail!("final historical-price provider attempt did not succeed exactly");
    }
    let body_text = attempt
        .body_text
        .as_deref()
        .context("successful provider attempt lacks raw response bytes")?;
    let expected_body_sha256 = attempt
        .response_body_sha256
        .as_deref()
        .context("successful provider attempt lacks a raw body SHA-256")?;
    if !valid_sha256(expected_body_sha256) {
        bail!("provider raw response body SHA-256 is invalid");
    }
    require_hash(
        "provider raw response body",
        &sha256_bytes(body_text.as_bytes()),
        expected_body_sha256,
    )?;
    let parsed_body: JsonValue =
        serde_json::from_str(body_text).context("parsing provider raw response body")?;
    let recorded_body = attempt
        .body
        .as_ref()
        .context("successful provider attempt lacks parsed response body")?;
    // The raw text and its hash are authoritative. The duplicated parsed body
    // was serialized through JavaScript IEEE-754 numbers, so comparing its
    // arbitrary-precision JSON number lexemes would reject equivalent values.
    if parsed_body.get("success").and_then(JsonValue::as_bool) != Some(true)
        || recorded_body.get("success").and_then(JsonValue::as_bool) != Some(true)
    {
        bail!("provider raw or recorded response body is unsuccessful");
    }
    Ok(parsed_body)
}

fn extract_exact_provider_points(
    body: &JsonValue,
    request_mint: &str,
) -> Result<BTreeMap<i64, Vec<String>>> {
    let items = body
        .pointer("/data/items")
        .and_then(JsonValue::as_array)
        .context("historical-price provider body lacks data.items")?;
    let mut points: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for item in items {
        if item.get("address").and_then(JsonValue::as_str) != Some(request_mint) {
            bail!("historical-price provider point address differs from request mint");
        }
        let timestamp = item
            .get("unixTime")
            .and_then(JsonValue::as_i64)
            .context("historical-price provider point timestamp is invalid")?;
        if timestamp.rem_euclid(HOUR_SECONDS) != 0 {
            bail!("historical-price provider point is not an exact hourly observation");
        }
        let price = json_decimal_string(
            item.get("value")
                .context("historical-price provider point lacks a value")?,
        )?;
        points.entry(timestamp).or_default().push(price);
    }
    Ok(points)
}

fn validate_provenance(
    provenance: &[MarkProvenanceV1],
    sidecar_marks: &BTreeMap<PriceKey, HistoricalPriceMarkV1>,
    responses: &BTreeMap<u64, VerifiedResponse>,
    expected_keys: &BTreeSet<PriceKey>,
) -> Result<()> {
    if provenance.len() != sidecar_marks.len() || provenance.len() != expected_keys.len() {
        bail!("historical-price provenance does not exactly cover sidecar marks");
    }
    let mut observed_keys = BTreeSet::new();
    let mut previous: Option<PriceKey> = None;
    let mut provenance_per_request = BTreeMap::<u64, u64>::new();
    for row in provenance {
        if row.schema_version != 1
            || row.mint.trim().is_empty()
            || row.bucket_unix_seconds.rem_euclid(HOUR_SECONDS) != 0
            || row.raw_provider_timestamp_unix_seconds != row.bucket_unix_seconds
            || !valid_sha256(&row.provider_response_sha256)
        {
            bail!("historical-price mark provenance is invalid or not an exact hourly point");
        }
        let key = PriceKey::new(&row.mint, row.bucket_unix_seconds);
        if previous.as_ref().is_some_and(|prior| prior >= &key)
            || !observed_keys.insert(key.clone())
        {
            bail!("historical-price provenance is duplicated or not strictly sorted");
        }
        let mark = sidecar_marks
            .get(&key)
            .context("historical-price provenance has no matching sidecar mark")?;
        if row.price_usd != mark.price_usd {
            bail!("historical-price provenance value differs from sidecar mark");
        }
        let response = responses
            .get(&row.request_index)
            .context("historical-price provenance refers to an unknown request")?;
        if response.request_index != row.request_index
            || response.response_sha256 != row.provider_response_sha256
            || response.request.mint != row.mint
            || row.bucket_unix_seconds < response.request.from
            || row.bucket_unix_seconds >= response.request.to_exclusive
        {
            bail!("historical-price provenance request binding is inconsistent");
        }
        let raw_values = response
            .points
            .get(&row.raw_provider_timestamp_unix_seconds)
            .context("provenance exact timestamp is absent from raw provider response")?;
        if raw_values.len() != 1 || raw_values[0] != row.price_usd.as_str() {
            bail!("raw provider response does not prove exactly one matching historical mark");
        }
        *provenance_per_request.entry(row.request_index).or_default() += 1;
        previous = Some(key);
    }
    if observed_keys != *expected_keys {
        bail!("historical-price provenance keys differ from expected coverage");
    }
    for response in responses.values() {
        if provenance_per_request
            .get(&response.request_index)
            .copied()
            .unwrap_or(0)
            != response.requested_bucket_count
        {
            bail!("provider request bucket count differs from exact recovered provenance");
        }
    }
    Ok(())
}

fn validate_outcome(
    manifest: &FetchManifestV1,
    expected_keys: u64,
    recovered_operation_references: u64,
    response_count: u64,
) -> Result<()> {
    if manifest.outcome.processed_unique_keys != expected_keys
        || manifest.outcome.recovered_unique_keys != expected_keys
        || manifest.outcome.recovered_operation_references != recovered_operation_references
        || manifest.outcome.unresolved_unique_keys != 0
        || manifest.outcome.unresolved_operation_references != 0
        || !manifest.outcome.unresolved_by_reason.is_empty()
        || manifest.request_plan.executed_request_count != response_count
    {
        bail!("historical-price sidecar outcome is incomplete or inconsistent");
    }
    Ok(())
}

fn validate_original_marks(
    marks: &[HistoricalPriceMarkV1],
    provider: &str,
    sidecar_keys: &BTreeSet<PriceKey>,
) -> Result<BTreeMap<PriceKey, HistoricalPriceMarkV1>> {
    let mut result = BTreeMap::new();
    let mut previous: Option<PriceKey> = None;
    for mark in marks {
        mark.validate()
            .context("invalid database historical-price mark")?;
        if mark.provider != provider {
            bail!("database historical-price export uses the wrong provider");
        }
        let key = PriceKey::new(&mark.mint, mark.bucket_unix_seconds);
        if previous.as_ref().is_some_and(|prior| prior >= &key)
            || result.insert(key.clone(), mark.clone()).is_some()
        {
            bail!("database historical-price marks are duplicated or not strictly sorted");
        }
        if sidecar_keys.contains(&key) {
            bail!("sidecar historical-price mark conflicts with the database export");
        }
        previous = Some(key);
    }
    Ok(result)
}

fn merged_jsonl_bytes(
    original: &BTreeMap<PriceKey, HistoricalPriceMarkV1>,
    sidecar: &BTreeMap<PriceKey, HistoricalPriceMarkV1>,
) -> Result<Vec<u8>> {
    let mut merged = original.clone();
    for (key, mark) in sidecar {
        if merged.insert(key.clone(), mark.clone()).is_some() {
            bail!("duplicate historical-price mark during deterministic merge");
        }
    }
    let mut bytes = Vec::new();
    for mark in merged.values() {
        serde_json::to_writer(&mut bytes, mark)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn write_outputs_atomically(
    spec: &HistoricalPriceSidecarMergeSpec<'_>,
    merged_bytes: &[u8],
    receipt_bytes: &[u8],
) -> Result<()> {
    if spec.receipt_output_path.exists() {
        bail!("refusing to replace an existing historical-price sidecar receipt");
    }
    if spec.merged_historical_prices_output_path.exists()
        && spec.merged_historical_prices_output_path != spec.original_historical_prices_path
    {
        bail!("refusing to replace an unrelated merged historical-price output");
    }
    let merged_partial = write_partial(spec.merged_historical_prices_output_path, merged_bytes)?;
    let receipt_partial = match write_partial(spec.receipt_output_path, receipt_bytes) {
        Ok(path) => path,
        Err(error) => {
            let _ = fs::remove_file(&merged_partial);
            return Err(error);
        }
    };
    if let Err(error) = fs::rename(&merged_partial, spec.merged_historical_prices_output_path) {
        let _ = fs::remove_file(&merged_partial);
        let _ = fs::remove_file(&receipt_partial);
        return Err(error).context("committing merged historical-price output");
    }
    if let Err(error) = fs::rename(&receipt_partial, spec.receipt_output_path) {
        let _ = fs::remove_file(&receipt_partial);
        let _ = fs::remove_file(spec.merged_historical_prices_output_path);
        return Err(error).context("committing historical-price sidecar receipt");
    }
    Ok(())
}

fn write_partial(output_path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let parent = output_path
        .parent()
        .context("historical-price output path has no parent")?;
    fs::create_dir_all(parent)?;
    let file_name = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("historical-price output path has an invalid file name")?;
    let suffix = PARTIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let partial = parent.join(format!(
        ".{file_name}.partial-{}-{suffix}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .with_context(|| format!("creating partial output {}", partial.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&partial);
        return Err(error).context("writing historical-price partial output");
    }
    Ok(partial)
}

fn read_strict_jsonl<T>(path: &Path) -> Result<Vec<T>>
where
    T: DeserializeOwned + Serialize,
{
    let file = File::open(path).with_context(|| format!("opening JSONL {}", path.display()))?;
    let mut rows = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line_number = index + 1;
        let line = line.with_context(|| format!("reading JSONL line {line_number}"))?;
        if line.trim().is_empty() {
            bail!("blank JSONL row at line {line_number}");
        }
        let raw: JsonValue = serde_json::from_str(&line)
            .with_context(|| format!("parsing JSONL row at line {line_number}"))?;
        let row: T = serde_json::from_value(raw.clone())
            .with_context(|| format!("decoding JSONL row at line {line_number}"))?;
        if serde_json::to_value(&row)? != raw {
            bail!("JSONL row at line {line_number} contains unknown or drifted fields");
        }
        rows.push(row);
    }
    Ok(rows)
}

fn count_strict_nonblank_lines(path: &Path) -> Result<u64> {
    let file = File::open(path)?;
    let mut count = 0_u64;
    for (index, line) in BufReader::new(file).lines().enumerate() {
        if line?.trim().is_empty() {
            bail!("blank source row at line {}", index + 1);
        }
        count = count.checked_add(1).context("source row count overflow")?;
    }
    Ok(count)
}

fn verify_declared_source_file(name: &str, path: &Path, expected_sha256: &str) -> Result<()> {
    if !path.is_absolute() || !valid_sha256(expected_sha256) {
        bail!("historical-price sidecar {name} proof is invalid");
    }
    require_hash(name, &sha256_file(path)?, expected_sha256)
}

fn validate_declared_path(declared: &str, expected: &Path) -> Result<()> {
    let declared = fs::canonicalize(declared).with_context(|| {
        format!(
            "resolving declared sidecar proof path for {}",
            expected.display()
        )
    })?;
    let expected = fs::canonicalize(expected).with_context(|| {
        format!(
            "resolving expected sidecar proof path {}",
            expected.display()
        )
    })?;
    if declared != expected {
        bail!("sidecar proof path escapes or differs from its configured root");
    }
    Ok(())
}

fn require_logical_file_name(declared: &str, expected_file_name: &str) -> Result<()> {
    if Path::new(declared)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(expected_file_name)
    {
        bail!("historical-price classification logical source name mismatch");
    }
    Ok(())
}

fn require_hash(name: &str, actual: &str, expected: &str) -> Result<()> {
    if !valid_sha256(actual) || !valid_sha256(expected) || actual != expected {
        bail!("{name} SHA-256 mismatch");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    Ok(sha256_bytes(&fs::read(path)?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn stable_json_bytes(value: &JsonValue) -> Result<Vec<u8>> {
    fn write_stable(value: &JsonValue, output: &mut Vec<u8>) -> Result<()> {
        match value {
            JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::String(_) => {
                serde_json::to_writer(&mut *output, value)?;
            }
            JsonValue::Array(values) => {
                output.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    write_stable(value, output)?;
                }
                output.push(b']');
            }
            JsonValue::Object(values) => {
                output.push(b'{');
                let mut keys: Vec<_> = values.keys().collect();
                keys.sort();
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    serde_json::to_writer(&mut *output, key)?;
                    output.push(b':');
                    write_stable(&values[key], output)?;
                }
                output.push(b'}');
            }
        }
        Ok(())
    }

    let mut output = Vec::new();
    write_stable(value, &mut output)?;
    Ok(output)
}

fn json_decimal_string(value: &JsonValue) -> Result<String> {
    let raw = match value {
        JsonValue::Number(number) => number.to_string(),
        JsonValue::String(value) => value.clone(),
        _ => bail!("historical-price provider value is not numeric"),
    };
    let decimal = ExactDecimal::new(raw.clone())
        .context("historical-price provider value is not an exact decimal")?;
    if decimal.is_zero() {
        bail!("historical-price provider value is not positive");
    }
    Ok(raw)
}

fn json_u64(value: &JsonValue, pointer: &str) -> Result<u64> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_u64)
        .with_context(|| format!("historical-price proof lacks integer field {pointer}"))
}

fn require_json_u64(value: &JsonValue, pointer: &str, expected: u64) -> Result<()> {
    if json_u64(value, pointer)? != expected {
        bail!("historical-price proof field {pointer} differs from expected value");
    }
    Ok(())
}

fn require_json_string(value: &JsonValue, pointer: &str, expected: &str, name: &str) -> Result<()> {
    if value.pointer(pointer).and_then(JsonValue::as_str) != Some(expected) {
        bail!("{name} mismatch");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BenchmarkConfig;

    fn temporary_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "v1-vs-dusk-sidecar-{name}-{}-{}",
            std::process::id(),
            PARTIAL_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn receipt_fixture() -> HistoricalPriceSidecarMergeReceiptV1 {
        let digest = "a".repeat(64);
        HistoricalPriceSidecarMergeReceiptV1 {
            schema_version: HISTORICAL_PRICE_SIDECAR_RECEIPT_SCHEMA_VERSION,
            provider: "birdeye".to_owned(),
            source_manifest_file_sha256: digest.clone(),
            source_provider_responses_manifest_sha256: digest.clone(),
            source_marks_file_sha256: digest.clone(),
            source_provenance_file_sha256: digest.clone(),
            source_unresolved_file_sha256: digest.clone(),
            source_classification_summary_sha256: digest.clone(),
            source_missing_keys_sha256: digest.clone(),
            canonical_operations_sha256: digest.clone(),
            original_historical_prices_sha256: digest.clone(),
            merged_historical_prices_sha256: digest,
            provider_response_count: 1,
            canonical_operation_rows: 1,
            original_historical_price_rows: 1,
            sidecar_historical_price_rows: 1,
            merged_historical_price_rows: 2,
            recovered_operation_references: 1,
            current_price_fallback: false,
            interpolation: false,
            forward_fill: false,
            receipt_sha256: String::new(),
        }
    }

    #[test]
    fn receipt_self_hash_is_fail_closed() {
        let mut receipt = receipt_fixture().seal().unwrap();
        receipt.verify_self_hash().unwrap();
        receipt.sidecar_historical_price_rows += 1;
        assert!(receipt.verify_self_hash().is_err());
    }

    #[test]
    fn provider_request_rejects_nonhistorical_or_credential_query_drift() {
        let base = ProviderRequestV1 {
            method: "GET".to_owned(),
            url: "https://public-api.birdeye.so/defi/history_price?address=mint&address_type=token&type=1H&time_from=0&time_to=3600&ui_amount_mode=raw".to_owned(),
            provider: "birdeye".to_owned(),
            chain: "solana".to_owned(),
            interval: "1H".to_owned(),
            allow_current_fallback: false,
            requested_bucket_count: 1,
        };
        validate_provider_request(&base, "birdeye").unwrap();

        let mut current = base.clone();
        current.url = current.url.replace("history_price", "price");
        assert!(validate_provider_request(&current, "birdeye").is_err());

        let mut leaked = base;
        leaked.url.push_str("&api_key=secret");
        assert!(validate_provider_request(&leaked, "birdeye").is_err());
    }

    #[test]
    fn raw_body_hash_and_parsed_body_must_match() {
        let body_text = r#"{"data":{"items":[]},"success":true}"#;
        let body: JsonValue = serde_json::from_str(body_text).unwrap();
        let valid = ProviderAttemptV1 {
            started_at: "start".to_owned(),
            completed_at: "end".to_owned(),
            status: Some(200),
            ok: true,
            response_body_sha256: Some(sha256_bytes(body_text.as_bytes())),
            body_text: Some(body_text.to_owned()),
            body: Some(body),
            error: None,
        };
        validate_successful_attempt(&valid).unwrap();

        let mut tampered = valid;
        tampered.body_text = Some(r#"{"data":{"items":[1]},"success":true}"#.to_owned());
        assert!(validate_successful_attempt(&tampered).is_err());
    }

    #[test]
    fn merge_rejects_database_and_sidecar_key_collision() {
        let mark = HistoricalPriceMarkV1 {
            mint: "mint".to_owned(),
            bucket_unix_seconds: 0,
            price_usd: ExactDecimal::new("1").unwrap(),
            token_decimals: Some(6),
            provider: "birdeye".to_owned(),
            quality: crate::canonical::HistoricalPriceQuality::Historical,
        };
        let keys = BTreeSet::from([PriceKey::new("mint", 0)]);
        assert!(validate_original_marks(&[mark], "birdeye", &keys).is_err());
    }

    #[test]
    fn sealed_campaign_sidecar_validates_and_merges_when_present() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sidecar_root = root
            .join("results/runs/slot-440459364-04573f906223c06b/price-remediation/provider-fetch");
        if !sidecar_root.join(FETCH_MANIFEST_FILE_NAME).exists() {
            return;
        }
        let config = BenchmarkConfig::from_path(&root.join("benchmark.toml")).unwrap();
        let binding = config.source.historical_price_sidecar.as_ref().unwrap();
        let output_dir = temporary_path("sealed");
        fs::create_dir_all(&output_dir).unwrap();
        let merged = output_dir.join("historical-prices.jsonl");
        let receipt_path = output_dir.join("historical-price-sidecar-receipt.json");
        let canonical_operations =
            root.join("data/slot-440459364-04573f906223c06b/canonical-operations.jsonl");
        let original_prices =
            root.join("data/slot-440459364-04573f906223c06b/historical-prices.jsonl");
        let spec = HistoricalPriceSidecarMergeSpec {
            sidecar_root: &sidecar_root,
            binding,
            canonical_operations_path: &canonical_operations,
            original_historical_prices_path: &original_prices,
            merged_historical_prices_output_path: &merged,
            receipt_output_path: &receipt_path,
            expected_provider: "birdeye",
        };
        let receipt = validate_and_merge_historical_price_sidecar(&spec).unwrap();
        assert_eq!(receipt.provider_response_count, 60);
        assert_eq!(receipt.sidecar_historical_price_rows, 2_587);
        assert_eq!(receipt.merged_historical_price_rows, 22_071);
        assert_eq!(receipt.recovered_operation_references, 40_776);
        receipt.verify_self_hash().unwrap();
        fs::remove_dir_all(output_dir).unwrap();
    }
}

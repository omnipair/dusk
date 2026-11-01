//! Concrete data, replay, calibration, verification, and rendering stages.
//!
//! Stages may emit a diagnostic artifact when a quality gate fails, but they
//! never synthesize a downstream benchmark result around missing evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    future::Future,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
};

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;

use crate::{
    aggregate::{
        aggregate_candidate_score_for_markets, market_set_sha256, CandidateScoreEvidenceV1,
    },
    audit::{
        assess_phase_gates, evaluate_hard_gates_for_phase_with_certification,
        visible_market_eligibility_with_policy, CertificationStatus, CoverageCount,
        EligibilityReason, EvidenceTier as GateEvidenceTier, GateStatus, HardGateId, HardGateInput,
        HardGatePhase, HardGatePhaseInputV1, HardGatePhaseReportV1, HardGateReport,
        MarketEligibility, MarketEligibilityPolicy, MarketEvidence, PhaseGateDecisionV1,
        PhaseGatePolicyV1,
    },
    calibrate::{
        derive_protocol_interest_share_schedule, explore_profiles, refine_profiles,
        select_global_winner, CandidateScoreV1, DuskProfileV1, ProfileFreezeV1,
        ProtocolInterestEpochKeyV1, ProtocolInterestShareScheduleV1, ProtocolInterestSideWeightsV1,
        ProtocolInterestTrainingEpochV1, ProtocolInterestTrainingSampleV1,
        ProtocolInterestTrainingSourceV1, SearchDomainV1,
    },
    canonical::{
        sha256_hex, ArtifactChecksum, BlockArtifactChecksumV1, BlockArtifactManifestV1,
        CanonicalEventV1, ChainOrder, ConfigScope, ConfigVersionV1, HistoricalPriceMarkV1,
        SanitizedSource,
    },
    cohort::{
        select_cohort, CohortEdgeEvidenceV1, CohortEdgeUnavailableReasonV1,
        CohortFinalAuditQualificationV1, CohortMarketMetadataV1, CohortSelectionArtifactV1,
        CohortSelectionRequestV1, CohortSourceBindingsV1, CohortTailQualificationEvidenceV1,
        CohortTailQualificationUnavailableReasonV1,
    },
    config::{
        ArchivalBlockCacheConfig, ArchivalBlockCachePolicy, BenchmarkConfig,
        EvidenceTier as ConfigEvidenceTier, ScopeMode,
    },
    dataset_split::{
        create_chronological_calibration_prefixes, ChronologicalDatasetSplitManifestV1,
    },
    decode::{decode_archived_block, DecodedTransactionV1, DecoderRegistryV1},
    evaluation::{
        evaluate_profile_history_observed, evaluate_profile_history_observed_with_spot_probes,
        replay_profile_history_capture_state_hash_only, CounterfactualEvaluationSummaryV1,
    },
    extract::{
        checksum_archived_block_artifact, compose_ordered_signature_block, enrich_db_event,
        export_event_page, export_historical_price_page, finalized_get_block_request,
        finalized_get_block_signatures_request, finalized_get_transaction_request,
        normalize_independent_get_transaction_meta, validate_full_vs_composed_equivalence,
        ArchivalRpcTransport, ArchivedBlock, ArchivedBlockProofBasisV1,
        ArchivedTransactionProofBasisV1, DbEventSeed, EnrichedDbEventSeed, ExportCheckpoint,
        HistoricalPriceExportCheckpoint, ReadOnlyPgExporter, RpcCheckpoint, RpcFailureKind,
        COVERAGE_SQL, MARKETS_SQL, PRICES_SQL,
    },
    historical_price_sidecar::{
        validate_and_merge_historical_price_sidecar, HistoricalPriceSidecarMergeReceiptV1,
        HistoricalPriceSidecarMergeSpec,
    },
    leverage_campaign::{
        LeverageCampaignArtifactV1, LeverageCampaignBuilderV1, LeverageCampaignSpecV1,
        LeverageMintEligibilityEvidenceV1,
    },
    metric_engine::{PairedMetricLedgerSummaryV1, PairedMetricLedgerWriterV1},
    metrics::{
        BootstrapIntervalV1, CohortRepresentativenessEvidenceV1, MarketOpportunityCeilingV1,
        ResultManifestV1, ResultScopeV1, ScopeActivityCoverageV1, SensitivityEvidenceSummaryV1,
    },
    normalize::NormalizationStateV1,
    parquet::{
        export_jsonl_to_parquet, validate_parquet_artifact, BenchmarkParquetDatasetV1,
        ParquetExportSpecV1, ParquetFileManifestV1,
    },
    per_market::{
        qualify_calibration_markets, qualify_holdout_markets,
        PerMarketCalibrationQualificationArtifactV1,
    },
    reconcile::{canonical_event_kind, reconcile_slot_events, SourceReconciliationSummaryV1},
    replay::{CanonicalOperationV1, HlpScenario, OperationPayloadV1, ReplayArm},
    report::{write_and_render_reports, PortableBuilderConfigV1},
    result_manifest::{build_result_manifest, HoldoutMetricInputV1},
    rpc::{HttpArchivalRpc, ProgramSignatureV1, RpcResponseProofV1},
    sensitivity::{build_sensitivity_evidence, SensitivityArtifactV1, SensitivityRequestV1},
    spot_probe::SpotProbeSizingWindowV1,
    stages::{artifact_map, RunContext},
    terminal::{TerminalAccountSnapshotV1, TerminalSnapshotBindingV1},
    transfer_fee::TransferFeeRegistryV1,
    transfer_fee_extract::{
        extract_selected_scope_transfer_fee_provenance, extract_transfer_fee_provenance,
        MarketMintBindingV1, SelectedScopeTransferFeeExtractionRequestV1,
        TransferFeeExtractionRequestV1, TransferFeeProvenanceArtifactV1,
        DECISION_GRADE_LEGACY_MINT_PROOFS_JSON, DEFAULT_MINT_SIGNATURE_PAGE_LIMIT,
        SELECTED_OPERATION_TOKEN_PROGRAM_EVIDENCE_JSON, TRANSFER_FEE_EXACT_FALLBACK_DIR,
        TRANSFER_FEE_PROVENANCE_JSON, TRANSFER_FEE_REGISTRY_JSON,
    },
    universe::derive_universe_and_window,
    v1_reconciliation::{
        reconcile_history_to_ledger_with_terminal, V1ReconciliationSummaryV1,
        V1ReplayCertificationPolicyV1, V1ReplayLedgerRowV1,
    },
    v1_terminal_reconciliation::{
        reconcile_terminal_snapshot, TERMINAL_BINDING_JSON, TERMINAL_SNAPSHOT_JSON,
        V1_TERMINAL_RECONCILIATION_JSON,
    },
    window::ChronologicalSplitsV1,
};

const EXTRACTION_INVENTORY: &str = "extraction-inventory.json";
const COHORT_SELECTION_JSON: &str = "cohort-selection.json";
const COHORT_SELECTION_PREFIX_JSON: &str = "cohort-selection-prefix.json";
const COHORT_PREFIX_DB_EVENTS_JSONL: &str = "cohort-prefix-db-events.jsonl";
const COHORT_CATALOG_UNBOUND_DB_EVENTS_JSONL: &str = "cohort-catalog-unbound-db-events.jsonl";
const COHORT_MARKET_EVIDENCE_JSON: &str = "cohort-market-evidence.json";
const COHORT_MARKET_METADATA_JSON: &str = "cohort-market-metadata.json";
const COHORT_REPRESENTATIVENESS_JSON: &str = "cohort-representativeness.json";
const MARKETS_JSONL: &str = "markets.jsonl";
const DB_EVENTS_JSONL: &str = "db-events.jsonl";
const DB_EXPORT_CHECKPOINT: &str = "db-events-checkpoint.json";
const ENRICHED_DB_EVENTS_JSONL: &str = "enriched-db-events.jsonl";
const PROGRAM_SIGNATURES_JSONL: &str = "program-signatures.jsonl";
const SIGNATURE_CHECKPOINT: &str = "signature-checkpoint.json";
const LEGACY_SIGNATURE_CHECKPOINT: &str = "signature-checkpoint.legacy-v1.json";
const PROGRAM_SIGNATURE_PAGE_DIR: &str = "program-signature-pages";
const PROGRAM_SIGNATURE_CRAWL_SCHEMA_VERSION: u16 = 2;
const PROGRAM_SIGNATURE_PAGE_LIMIT: u16 = 1_000;
const PROGRAM_SIGNATURE_PAGE_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:program-signature-page:v2";
const PROGRAM_SIGNATURE_CHECKPOINT_HASH_DOMAIN: &[u8] =
    b"omnipair:v1-vs-dusk:program-signature-checkpoint:v2";
const RPC_CHECKPOINT: &str = "rpc-checkpoint.json";
const RPC_COMPOSITION_CALIBRATION_JSON: &str = "rpc-composition-calibration.json";
const RPC_COMPOSITION_CALIBRATION_SAMPLE_SIZE: usize = 128;
const BLOCK_ARTIFACTS_JSON: &str = "block-artifacts.json";
const ARCHIVAL_BLOCK_CACHE_IMPORT_JSON: &str = "archival-block-cache-import.json";
const DECODED_TRANSACTIONS_JSONL: &str = "decoded-transactions.jsonl";
const DECODED_INSTRUCTIONS_JSONL: &str = "decoded-instructions.jsonl";
const BLOCK_DECODE_FAILURES_JSONL: &str = "block-decode-failures.jsonl";
const TRANSACTION_DECODE_FAILURES_JSONL: &str = "transaction-decode-failures.jsonl";
const EXECUTION_PROVENANCE_JSONL: &str = "execution-provenance.jsonl";
const DECODER_REVISIONS_TOML: &str = "decoder-revisions.toml";
const HISTORICAL_PRICES_JSONL: &str = "historical-prices.jsonl";
const DATABASE_HISTORICAL_PRICES_JSONL: &str = "historical-prices-database.jsonl";
const HISTORICAL_PRICE_SIDECAR_RECEIPT_JSON: &str = "historical-price-sidecar-merge-receipt.json";
const CALIBRATION_PROFILE_REJECTIONS_JSON: &str = "calibration-profile-rejections.json";
const INVALID_MARKET_CONFIG_ERROR_CODE: u32 = 6_096;
const EXPLICIT_CURVE_SOURCE_SUFFIX: &str = "programs/dusk/src/math/explicit_curve.rs";
const PRICE_CHECKPOINT: &str = "historical-price-checkpoint.json";
const CANONICAL_OPERATIONS_JSONL: &str = "canonical-operations.jsonl";
const CANONICAL_EVENTS_JSONL: &str = "canonical-events.jsonl";
const CONFIG_HISTORY_JSONL: &str = "config-history.jsonl";
const PROTOCOL_INTEREST_SCHEDULE_JSON: &str = "protocol-interest-schedule.json";
const NORMALIZATION_GAPS_JSONL: &str = "normalization-gaps.jsonl";
const DB_RPC_RECONCILIATION_JSONL: &str = "db-rpc-reconciliation.jsonl";
const DB_RPC_RECONCILIATION_SUMMARY: &str = "db-rpc-reconciliation-summary.json";
const CONFIG_COVERAGE_SUMMARY: &str = "configuration-coverage-summary.json";
const PRICE_COVERAGE_SUMMARY: &str = "historical-price-coverage-summary.json";
const V1_REPLAY_LEDGER_JSONL: &str = "v1-replay-ledger.jsonl";
const V1_RECONCILIATION_JSON: &str = "v1-reconciliation.json";
const FINAL_PHASE_GATE_JSON: &str = "v1-final-phase-gate.json";
const CHRONOLOGICAL_SPLITS_JSON: &str = "chronological-splits.json";
const COMMON_WINDOW_JSON: &str = "common-window.json";
const UNIVERSE_WINDOW_JSON: &str = "universe-window.json";
const DATASET_SPLIT_MANIFEST_JSON: &str = "dataset-split-manifest.json";
const LEVERAGE_V1_OPEN_LOOP_CAP: u32 = 256;
const LEVERAGE_V1_UNWIND_LOOP_CAP: u32 = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFingerprintV1 {
    pub benchmark_run_source_sha256: String,
    pub dusk_source_sha256: String,
    pub indexer_decoder_source_sha256: String,
    pub v1_program_source_sha256: String,
    pub query_sha256: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableCoverageV1 {
    pub source_table: String,
    pub row_count: u64,
    pub distinct_transactions: u64,
    pub min_slot: Option<u64>,
    pub max_slot: Option<u64>,
    pub missing_identity: u64,
    pub missing_order: u64,
    pub missing_raw_amounts: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureCheckpointV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub program_id: String,
    pub provider_fingerprint_sha256: String,
    pub commitment: String,
    pub request_limit: u16,
    pub before: Option<String>,
    pub pages_completed: u64,
    pub rows_observed: u64,
    pub rows_at_or_before_cutoff: u64,
    pub explicit_empty_terminal_page: bool,
    pub certified_complete: bool,
    pub page_artifacts: Vec<ArtifactChecksum>,
    pub last_page_artifact_sha256: Option<String>,
    pub output_artifact: Option<ArtifactChecksum>,
    pub legacy_reference: Option<LegacySignatureReferenceV1>,
    pub failure_kind: Option<String>,
    pub artifact_sha256: String,
}

impl SignatureCheckpointV1 {
    fn new(
        run_id: &str,
        cutoff_slot: u64,
        program_id: &str,
        provider_fingerprint_sha256: &str,
        legacy_reference: Option<LegacySignatureReferenceV1>,
    ) -> Result<Self> {
        Self {
            schema_version: PROGRAM_SIGNATURE_CRAWL_SCHEMA_VERSION,
            run_id: run_id.to_owned(),
            cutoff_slot,
            program_id: program_id.to_owned(),
            provider_fingerprint_sha256: provider_fingerprint_sha256.to_owned(),
            commitment: "finalized".to_owned(),
            request_limit: PROGRAM_SIGNATURE_PAGE_LIMIT,
            before: None,
            pages_completed: 0,
            rows_observed: 0,
            rows_at_or_before_cutoff: 0,
            explicit_empty_terminal_page: false,
            certified_complete: false,
            page_artifacts: Vec::new(),
            last_page_artifact_sha256: None,
            output_artifact: legacy_reference
                .as_ref()
                .map(|reference| reference.output_artifact.clone()),
            legacy_reference,
            failure_kind: None,
            artifact_sha256: String::new(),
        }
        .seal()
    }

    fn seal(mut self) -> Result<Self> {
        self.artifact_sha256.clear();
        self.artifact_sha256 =
            signature_artifact_hash(PROGRAM_SIGNATURE_CHECKPOINT_HASH_DOMAIN, &self)?;
        Ok(self)
    }

    fn validate_identity(
        &self,
        run_id: &str,
        cutoff_slot: u64,
        program_id: &str,
        provider_fingerprint_sha256: &str,
    ) -> Result<()> {
        if self.schema_version != PROGRAM_SIGNATURE_CRAWL_SCHEMA_VERSION
            || self.run_id != run_id
            || self.cutoff_slot != cutoff_slot
            || self.program_id != program_id
            || self.provider_fingerprint_sha256 != provider_fingerprint_sha256
            || self.commitment != "finalized"
            || self.request_limit != PROGRAM_SIGNATURE_PAGE_LIMIT
            || !valid_signature_pubkey(&self.program_id)
            || !valid_signature_sha256(&self.provider_fingerprint_sha256)
            || !valid_signature_sha256(&self.artifact_sha256)
            || self.pages_completed != self.page_artifacts.len() as u64
            || self.page_artifacts.iter().any(|artifact| {
                artifact.logical_name.trim().is_empty() || !valid_signature_sha256(&artifact.sha256)
            })
            || self
                .last_page_artifact_sha256
                .as_ref()
                .is_some_and(|hash| !valid_signature_sha256(hash))
            || self
                .output_artifact
                .as_ref()
                .is_some_and(|artifact| !valid_signature_sha256(&artifact.sha256))
            || (self.certified_complete && !self.explicit_empty_terminal_page)
            || (self.certified_complete
                && (self.failure_kind.is_some() || self.output_artifact.is_none()))
        {
            bail!("invalid program-signature checkpoint identity");
        }
        if self.page_artifacts.is_empty() != self.last_page_artifact_sha256.is_none()
            || self.page_artifacts.last().map(|row| row.sha256.as_str())
                != self.last_page_artifact_sha256.as_deref()
        {
            bail!("program-signature checkpoint page checksum head is inconsistent");
        }
        if let Some(reference) = &self.legacy_reference {
            reference.validate()?;
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256
            != signature_artifact_hash(PROGRAM_SIGNATURE_CHECKPOINT_HASH_DOMAIN, &unsigned)?
        {
            bail!("program-signature checkpoint self-hash mismatch");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacySignatureReferenceV1 {
    pub checkpoint_artifact: ArtifactChecksum,
    pub output_artifact: ArtifactChecksum,
    pub claimed_before: Option<String>,
    pub claimed_pages_completed: u64,
    pub claimed_rows_observed: u64,
    pub claimed_complete: bool,
    pub output_rows: u64,
    pub output_unique_rows: u64,
    pub output_last_signature: Option<String>,
}

impl LegacySignatureReferenceV1 {
    fn validate(&self) -> Result<()> {
        for artifact in [&self.checkpoint_artifact, &self.output_artifact] {
            if artifact.logical_name.trim().is_empty() || !valid_signature_sha256(&artifact.sha256)
            {
                bail!("legacy program-signature reference checksum is invalid");
            }
        }
        if self
            .claimed_before
            .as_ref()
            .is_some_and(|signature| !valid_signature_value(signature))
            || self
                .output_last_signature
                .as_ref()
                .is_some_and(|signature| !valid_signature_value(signature))
            || self.output_unique_rows > self.output_rows
        {
            bail!("legacy program-signature reference inventory is invalid");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySignatureCheckpointV1 {
    before: Option<String>,
    pages_completed: u64,
    rows_observed: u64,
    complete: bool,
    failure_kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramSignaturePageArtifactV2 {
    schema_version: u16,
    run_id: String,
    cutoff_slot: u64,
    program_id: String,
    provider_fingerprint_sha256: String,
    commitment: String,
    page_index: u64,
    request_before: Option<String>,
    request_limit: u16,
    previous_page_artifact_sha256: Option<String>,
    raw_response_sha256: String,
    signatures: Vec<ProgramSignatureV1>,
    artifact_sha256: String,
}

impl ProgramSignaturePageArtifactV2 {
    fn seal(mut self) -> Result<Self> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = signature_artifact_hash(PROGRAM_SIGNATURE_PAGE_HASH_DOMAIN, &self)?;
        Ok(self)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != PROGRAM_SIGNATURE_CRAWL_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || !valid_signature_pubkey(&self.program_id)
            || !valid_signature_sha256(&self.provider_fingerprint_sha256)
            || self.commitment != "finalized"
            || self.request_limit != PROGRAM_SIGNATURE_PAGE_LIMIT
            || self.signatures.len() > usize::from(self.request_limit)
            || !valid_signature_sha256(&self.raw_response_sha256)
            || !valid_signature_sha256(&self.artifact_sha256)
            || self
                .request_before
                .as_ref()
                .is_some_and(|value| !valid_signature_value(value))
            || self
                .previous_page_artifact_sha256
                .as_ref()
                .is_some_and(|value| !valid_signature_sha256(value))
            || (self.page_index == 0
                && (self.request_before.is_some() || self.previous_page_artifact_sha256.is_some()))
            || (self.page_index > 0
                && (self.request_before.is_none() || self.previous_page_artifact_sha256.is_none()))
        {
            bail!("invalid program-signature page identity");
        }
        let mut seen = BTreeSet::new();
        let mut previous_slot = None;
        for row in &self.signatures {
            if !valid_signature_value(&row.signature)
                || !seen.insert(row.signature.clone())
                || previous_slot.is_some_and(|slot| row.slot > slot)
            {
                bail!("program-signature page is duplicated or not newest-first");
            }
            previous_slot = Some(row.slot);
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256
            != signature_artifact_hash(PROGRAM_SIGNATURE_PAGE_HASH_DOMAIN, &unsigned)?
        {
            bail!("program-signature page self-hash mismatch");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionInventoryV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub cutoff_block_time_unix_seconds: i64,
    pub program_id: String,
    pub scope_mode: ScopeMode,
    pub cohort_selection_sha256: String,
    pub cohort_representativeness_sha256: String,
    pub selected_markets: Vec<String>,
    pub selected_market_count: u64,
    pub earliest_successful_program_slot: Option<u64>,
    pub markets_exported: u64,
    pub database_event_rows: u64,
    pub database_distinct_signatures: u64,
    pub catalog_unbound_database_event_rows: u64,
    pub catalog_unbound_prefix_database_event_rows: u64,
    pub catalog_unbound_market_count: u64,
    pub catalog_unbound_prefix_market_count: u64,
    pub selected_database_event_rows: u64,
    pub selected_database_distinct_signatures: u64,
    pub program_signatures_observed: u64,
    pub successful_program_signatures_at_or_before_cutoff: u64,
    pub successful_program_signatures_without_database_rows: u64,
    pub targeted_successful_program_signatures: u64,
    pub signature_inventory_complete: bool,
    pub rpc_composition_calibration_blocks: u64,
    pub rpc_composition_calibration_sha256: String,
    pub expected_archival_blocks: u64,
    pub fetched_archival_blocks: u64,
    pub unresolved_archival_blocks: u64,
    pub archival_block_cache_import_sha256: Option<String>,
    pub archival_block_cache_blocks: u64,
    pub archival_block_cache_bytes: u64,
    pub archival_block_rpc_requests_avoided: u64,
    pub block_artifacts_checksummed: u64,
    pub successful_signatures_matched_in_blocks: u64,
    pub decoded_program_transactions: u64,
    pub decoded_state_changing_transactions: u64,
    pub decoded_program_instructions: u64,
    pub expected_state_changing_instructions: u64,
    pub decoded_state_changing_instructions: u64,
    pub unclassified_target_instructions: u64,
    pub transactions_with_incomplete_instruction_inventory: u64,
    pub transactions_with_incomplete_cpi_provenance: u64,
    pub unknown_program_instructions: u64,
    pub block_decode_failures: u64,
    pub transaction_decode_failures: u64,
    pub decode_failures: u64,
    pub decoder_registry_headline_verified: bool,
    pub canonical_operations: u64,
    pub canonical_events: u64,
    pub configuration_epochs: u64,
    pub normalization_gaps: u64,
    pub database_historical_price_marks: u64,
    pub historical_price_sidecar_marks: u64,
    pub historical_price_sidecar_receipt_sha256: Option<String>,
    pub historical_price_marks: u64,
    pub historical_price_window_start_unix_seconds: i64,
    pub historical_price_window_end_unix_seconds_exclusive: i64,
    pub database_events_enriched: u64,
    pub database_events_missing_authoritative_enrichment: u64,
    pub database_events_missing_instruction_path: u64,
    pub table_coverage: Vec<TableCoverageV1>,
    pub sources: Vec<SanitizedSource>,
    pub source_fingerprints: SourceFingerprintV1,
    pub artifacts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchivalBlockCacheImportReceiptV1 {
    schema_version: u16,
    run_id: String,
    cutoff_slot: u64,
    program_id: String,
    policy: ArchivalBlockCachePolicy,
    donor_manifest_file_sha256: String,
    donor_manifest_sha256: String,
    decoded_block_manifest_sha256: String,
    target_slots_sha256: String,
    target_signatures_sha256: String,
    validated_blocks: u64,
    validated_compressed_bytes: u64,
    composed_rpc_requests_avoided: u64,
    materialization: String,
    artifact_sha256: String,
}

impl ArchivalBlockCacheImportReceiptV1 {
    fn seal(mut self) -> Result<Self> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = sha256_hex(&serde_json::to_vec(&self)?);
        Ok(self)
    }

    fn verify(
        &self,
        run_id: &str,
        cutoff_slot: u64,
        program_id: &str,
        cache: &VerifiedArchivalBlockCacheV1,
        signatures_by_slot: &BTreeMap<u64, BTreeSet<String>>,
        decoded_manifest: &BlockArtifactManifestV1,
    ) -> Result<()> {
        let slots = signatures_by_slot.keys().copied().collect::<Vec<_>>();
        let expected_requests = signatures_by_slot.values().try_fold(0_u64, |total, rows| {
            let row_requests = u64::try_from(rows.len())?
                .checked_add(1)
                .context("archival cache per-block request count overflow")?;
            total
                .checked_add(row_requests)
                .context("archival cache avoided-request count overflow")
        })?;
        if self.schema_version != 1
            || self.run_id != run_id
            || self.cutoff_slot != cutoff_slot
            || self.program_id != program_id
            || self.policy != ArchivalBlockCachePolicy::RequireCompleteHardlink
            || self.donor_manifest_file_sha256 != cache.manifest_file_sha256
            || self.donor_manifest_sha256 != cache.manifest.manifest_sha256
            || self.decoded_block_manifest_sha256 != decoded_manifest.manifest_sha256
            || decoded_manifest.blocks != cache.manifest.blocks
            || self.target_slots_sha256 != sha256_hex(&serde_json::to_vec(&slots)?)
            || self.target_signatures_sha256 != sha256_hex(&serde_json::to_vec(signatures_by_slot)?)
            || self.validated_blocks != u64::try_from(slots.len())?
            || self.validated_compressed_bytes != cache.target_bytes(signatures_by_slot)?
            || self.composed_rpc_requests_avoided != expected_requests
            || self.materialization != "hard_link_exact_inode_v1"
            || !valid_signature_sha256(&self.artifact_sha256)
            || self.clone().seal()?.artifact_sha256 != self.artifact_sha256
        {
            bail!("archival block-cache import receipt failed identity verification");
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct VerifiedArchivalBlockCacheV1 {
    root: PathBuf,
    manifest_file_sha256: String,
    manifest: BlockArtifactManifestV1,
    blocks_by_slot: BTreeMap<u64, BlockArtifactChecksumV1>,
}

impl VerifiedArchivalBlockCacheV1 {
    fn open(
        config_root: &Path,
        current_data_dir: &Path,
        config: &ArchivalBlockCacheConfig,
    ) -> Result<Self> {
        if config.policy != ArchivalBlockCachePolicy::RequireCompleteHardlink {
            bail!("unsupported archival block-cache reuse policy");
        }
        let configured_root = Path::new(&config.root);
        let configured_root = if configured_root.is_absolute() {
            configured_root.to_path_buf()
        } else {
            config_root.join(configured_root)
        };
        let root = fs::canonicalize(&configured_root).with_context(|| {
            format!(
                "resolving archival block-cache root {}",
                configured_root.display()
            )
        })?;
        if !root.is_dir() {
            bail!("archival block-cache root is not a directory");
        }
        let current_data_dir = fs::canonicalize(current_data_dir).with_context(|| {
            format!(
                "resolving current data directory {}",
                current_data_dir.display()
            )
        })?;
        if root == current_data_dir
            || root.starts_with(&current_data_dir)
            || current_data_dir.starts_with(&root)
        {
            bail!("archival block-cache donor must be separate from the current run directory");
        }
        let manifest_path = root.join(BLOCK_ARTIFACTS_JSON);
        if !fs::symlink_metadata(&manifest_path)
            .with_context(|| format!("reading metadata for {}", manifest_path.display()))?
            .file_type()
            .is_file()
        {
            bail!("archival block-cache manifest must be a regular file");
        }
        let manifest_bytes = fs::read(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let manifest_file_sha256 = sha256_hex(&manifest_bytes);
        if manifest_file_sha256 != config.manifest_file_sha256 {
            bail!("archival block-cache manifest file checksum mismatch");
        }
        let manifest: BlockArtifactManifestV1 = serde_json::from_slice(&manifest_bytes)
            .context("decoding archival block-cache manifest")?;
        manifest
            .validate()
            .context("validating archival block-cache manifest")?;
        if manifest.manifest_sha256 != config.manifest_sha256 {
            bail!("archival block-cache internal manifest checksum mismatch");
        }
        let mut blocks_by_slot = BTreeMap::new();
        for block in &manifest.blocks {
            if block.artifact.logical_name != block_logical_name(block.slot)
                || block.artifact.byte_length == 0
                || !valid_signature_sha256(&block.artifact.sha256)
                || blocks_by_slot.insert(block.slot, block.clone()).is_some()
            {
                bail!("archival block-cache manifest contains an invalid block entry");
            }
        }
        Ok(Self {
            root,
            manifest_file_sha256,
            manifest,
            blocks_by_slot,
        })
    }

    fn preflight_complete(
        &self,
        signatures_by_slot: &BTreeMap<u64, BTreeSet<String>>,
    ) -> Result<()> {
        let manifest_slots = self.blocks_by_slot.keys().copied().collect::<BTreeSet<_>>();
        let target_slots = signatures_by_slot.keys().copied().collect::<BTreeSet<_>>();
        if manifest_slots != target_slots || target_slots.is_empty() {
            bail!("archival block-cache slot inventory differs from the current target scope");
        }
        for (slot, signatures) in signatures_by_slot {
            if signatures.is_empty() {
                bail!("archival block-cache target slot has no successful signature");
            }
            let source = self.source_path(*slot)?;
            self.validate_source_path(&source)?;
        }
        Ok(())
    }

    fn materialize_and_validate(
        &self,
        data_dir: &Path,
        slot: u64,
        expected_signatures: &BTreeSet<String>,
    ) -> Result<()> {
        let entry = self
            .blocks_by_slot
            .get(&slot)
            .context("archival block-cache is missing a required target slot")?;
        let source = self.source_path(slot)?;
        self.validate_source_path(&source)?;
        let destination = block_path(data_dir, slot);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        if !destination.exists() {
            fs::hard_link(&source, &destination).with_context(|| {
                format!(
                    "hard-linking required archival cache block {} into {} (copy fallback is disabled)",
                    source.display(),
                    destination.display()
                )
            })?;
        }
        if !fs::symlink_metadata(&destination)
            .with_context(|| format!("reading metadata for {}", destination.display()))?
            .file_type()
            .is_file()
        {
            bail!("materialized archival block-cache entry is not a regular file");
        }
        if !paths_share_file_identity(&source, &destination)? {
            bail!("materialized archival block-cache entry is not a hard link to the pinned donor");
        }
        let bytes = fs::read(&destination).with_context(|| {
            format!("reading materialized cache block {}", destination.display())
        })?;
        if !entry.artifact.matches_bytes(&bytes) {
            bail!("materialized archival block-cache byte commitment mismatch");
        }
        let block = decode_archived_block_cache(&bytes)
            .with_context(|| format!("decoding materialized cache block {slot}"))?;
        if entry.slot != block.slot
            || entry.blockhash != block.blockhash
            || entry.parent_slot != block.parent_slot
            || entry.previous_blockhash != block.previous_blockhash
            || entry.transaction_count != block.full_transaction_count
        {
            bail!("archival block-cache manifest header differs from block payload");
        }
        validate_composed_block(&block, slot, expected_signatures)
            .context("validating proof-bound archival cache block")
    }

    fn source_path(&self, slot: u64) -> Result<PathBuf> {
        let entry = self
            .blocks_by_slot
            .get(&slot)
            .context("archival block-cache manifest lacks target slot")?;
        if entry.artifact.logical_name != block_logical_name(slot) {
            bail!("archival block-cache logical path is non-canonical");
        }
        Ok(self.root.join(&entry.artifact.logical_name))
    }

    fn validate_source_path(&self, source: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(source)
            .with_context(|| format!("reading cache block metadata {}", source.display()))?;
        if !metadata.file_type().is_file() {
            bail!("archival block-cache entry is not a regular file");
        }
        let canonical = fs::canonicalize(source)
            .with_context(|| format!("resolving cache block {}", source.display()))?;
        if !canonical.starts_with(&self.root) {
            bail!("archival block-cache entry escapes the configured root");
        }
        Ok(())
    }

    fn validate_decoded_manifest(
        &self,
        data_dir: &Path,
        signatures_by_slot: &BTreeMap<u64, BTreeSet<String>>,
    ) -> Result<BlockArtifactManifestV1> {
        self.preflight_complete(signatures_by_slot)?;
        let path = data_dir.join(BLOCK_ARTIFACTS_JSON);
        let decoded_manifest: BlockArtifactManifestV1 = read_json(&path)
            .with_context(|| format!("reading decoded block manifest {}", path.display()))?;
        decoded_manifest
            .validate()
            .context("validating decoded block artifact manifest")?;
        if decoded_manifest.blocks != self.manifest.blocks {
            bail!("decoded block checksums differ from the pinned archival cache manifest");
        }
        Ok(decoded_manifest)
    }

    fn target_bytes(&self, signatures_by_slot: &BTreeMap<u64, BTreeSet<String>>) -> Result<u64> {
        signatures_by_slot.keys().try_fold(0_u64, |total, slot| {
            total
                .checked_add(
                    self.blocks_by_slot
                        .get(slot)
                        .context("archival block-cache manifest lacks target slot")?
                        .artifact
                        .byte_length,
                )
                .context("archival block-cache byte count overflow")
        })
    }

    fn receipt(
        &self,
        data_dir: &Path,
        run_id: &str,
        cutoff_slot: u64,
        program_id: &str,
        signatures_by_slot: &BTreeMap<u64, BTreeSet<String>>,
    ) -> Result<ArchivalBlockCacheImportReceiptV1> {
        let decoded_manifest = self.validate_decoded_manifest(data_dir, signatures_by_slot)?;
        for (slot, signatures) in signatures_by_slot {
            self.materialize_and_validate(data_dir, *slot, signatures)
                .with_context(|| {
                    format!(
                        "revalidating imported archival cache block after decode at slot {slot}"
                    )
                })?;
        }
        let slots = signatures_by_slot.keys().copied().collect::<Vec<_>>();
        let composed_rpc_requests_avoided =
            signatures_by_slot.values().try_fold(0_u64, |total, rows| {
                let row_requests = u64::try_from(rows.len())?
                    .checked_add(1)
                    .context("archival cache per-block request count overflow")?;
                total
                    .checked_add(row_requests)
                    .context("archival cache avoided-request count overflow")
            })?;
        let receipt = ArchivalBlockCacheImportReceiptV1 {
            schema_version: 1,
            run_id: run_id.to_owned(),
            cutoff_slot,
            program_id: program_id.to_owned(),
            policy: ArchivalBlockCachePolicy::RequireCompleteHardlink,
            donor_manifest_file_sha256: self.manifest_file_sha256.clone(),
            donor_manifest_sha256: self.manifest.manifest_sha256.clone(),
            decoded_block_manifest_sha256: decoded_manifest.manifest_sha256.clone(),
            target_slots_sha256: sha256_hex(&serde_json::to_vec(&slots)?),
            target_signatures_sha256: sha256_hex(&serde_json::to_vec(signatures_by_slot)?),
            validated_blocks: u64::try_from(slots.len())?,
            validated_compressed_bytes: self.target_bytes(signatures_by_slot)?,
            composed_rpc_requests_avoided,
            materialization: "hard_link_exact_inode_v1".to_owned(),
            artifact_sha256: String::new(),
        }
        .seal()?;
        receipt.verify(
            run_id,
            cutoff_slot,
            program_id,
            self,
            signatures_by_slot,
            &decoded_manifest,
        )?;
        Ok(receipt)
    }
}

#[cfg(unix)]
fn paths_share_file_identity(left: &Path, right: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let left = fs::metadata(left)?;
    let right = fs::metadata(right)?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(not(unix))]
fn paths_share_file_identity(_left: &Path, _right: &Path) -> Result<bool> {
    bail!("require_complete_hardlink is unsupported without stable file identity metadata")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcCompositionCalibrationRowV1 {
    slot: u64,
    target_signature_count: u64,
    target_signatures_sha256: String,
    full_payload_raw_response_sha256: String,
    full_payload_inventory_sha256: String,
    ordered_signatures_raw_response_sha256: String,
    ordered_signatures_inventory_sha256: String,
    independent_transaction_responses_sha256: String,
    full_normalized_target_sha256: String,
    composed_normalized_target_sha256: String,
    exact_match: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcCompositionCalibrationArtifactV1 {
    schema_version: u16,
    run_id: String,
    cutoff_slot: u64,
    provider_fingerprint_sha256: String,
    population_slot_count: u64,
    selection_method: String,
    normalization: String,
    rows: Vec<RpcCompositionCalibrationRowV1>,
    artifact_sha256: String,
}

impl RpcCompositionCalibrationArtifactV1 {
    fn seal(mut self) -> Result<Self> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = sha256_hex(&serde_json::to_vec(&self)?);
        Ok(self)
    }

    fn verify(
        &self,
        run_id: &str,
        cutoff_slot: u64,
        provider_fingerprint_sha256: &str,
        population_slots: &[u64],
    ) -> Result<()> {
        let expected_slots = evenly_spaced_calibration_slots(population_slots);
        if self.schema_version != 1
            || self.run_id != run_id
            || self.cutoff_slot != cutoff_slot
            || self.provider_fingerprint_sha256 != provider_fingerprint_sha256
            || self.population_slot_count != u64::try_from(population_slots.len())?
            || self.selection_method != "evenly_spaced_sorted_target_slots_v1"
            || self.normalization != "remove_meta_rewards_v1"
            || self.rows.len() != expected_slots.len()
            || self
                .rows
                .iter()
                .map(|row| row.slot)
                .ne(expected_slots.iter().copied())
            || self.rows.iter().any(|row| {
                row.target_signature_count == 0
                    || !row.exact_match
                    || !valid_signature_sha256(&row.target_signatures_sha256)
                    || !valid_signature_sha256(&row.full_payload_raw_response_sha256)
                    || !valid_signature_sha256(&row.full_payload_inventory_sha256)
                    || !valid_signature_sha256(&row.ordered_signatures_raw_response_sha256)
                    || !valid_signature_sha256(&row.ordered_signatures_inventory_sha256)
                    || !valid_signature_sha256(&row.independent_transaction_responses_sha256)
                    || !valid_signature_sha256(&row.full_normalized_target_sha256)
                    || row.full_normalized_target_sha256 != row.composed_normalized_target_sha256
            })
            || !valid_signature_sha256(&self.artifact_sha256)
            || self.clone().seal()?.artifact_sha256 != self.artifact_sha256
        {
            bail!("RPC composition calibration artifact is invalid");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodeFailureV1 {
    pub schema_version: u16,
    pub slot: u64,
    pub category: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataQualityArtifactV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub hard_gate_phase_input: HardGatePhaseInputV1,
    pub audit_phase_report: HardGatePhaseReportV1,
    pub audit_phase_decision: PhaseGateDecisionV1,
    pub cohort_selection_sha256: Option<String>,
    pub cohort_final_audit_qualification: Option<CohortFinalAuditQualificationV1>,
    pub gates: HardGateReport,
    pub market_eligibility: Vec<MarketEligibility>,
    pub visible_market_count: u64,
    pub eligible_raw_market_count: u64,
    pub eligible_usd_headline_market_count: u64,
    pub factual_swap_count: u64,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FinalPhaseGateArtifactV1 {
    schema_version: u16,
    run_id: String,
    cutoff_slot: u64,
    cohort_selection_sha256: Option<String>,
    phase_report: HardGatePhaseReportV1,
    decision: PhaseGateDecisionV1,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NaturalGrainCoverageSummaryV1 {
    pub schema_version: u16,
    pub coverage: CoverageCount,
    pub blocking_records: u64,
    pub by_market: BTreeMap<String, CoverageCount>,
    pub detail_counts: BTreeMap<String, u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CalibrationSplitV1 {
    Training,
    Validation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CalibrationArmEvidenceV1 {
    schema_version: u16,
    split: CalibrationSplitV1,
    profile_id: String,
    protocol_interest_schedule_sha256: String,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    counterfactual: CounterfactualEvaluationSummaryV1,
    paired_metrics: PairedMetricLedgerSummaryV1,
    score_evidence: CandidateScoreEvidenceV1,
    evidence_sha256: String,
}

impl CalibrationArmEvidenceV1 {
    fn seal(mut self) -> Result<Self> {
        self.evidence_sha256.clear();
        self.evidence_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&self)?));
        Ok(self)
    }

    fn verify(&self) -> Result<()> {
        if self.clone().seal()?.evidence_sha256 != self.evidence_sha256
            || self.profile_id != self.counterfactual.profile_id
            || self.profile_id != self.paired_metrics.profile_id
            || self.profile_id != self.score_evidence.profile_id
            || self.protocol_interest_schedule_sha256
                != self.counterfactual.protocol_interest_schedule_sha256
            || self.arm != self.counterfactual.arm
            || self.arm != self.score_evidence.arm
            || self.hlp_scenario != self.counterfactual.hlp_scenario
            || self.hlp_scenario != self.paired_metrics.hlp_scenario
            || self.hlp_scenario != self.score_evidence.hlp_scenario
            || self.paired_metrics.ledger_sha256 != self.score_evidence.paired_metric_ledger_sha256
        {
            bail!("calibration arm evidence failed identity or checksum verification");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CalibrationProfileEvidenceV1 {
    schema_version: u16,
    split: CalibrationSplitV1,
    profile_id: String,
    protocol_interest_schedule_sha256: String,
    hlp_scenario: HlpScenario,
    arms: Vec<CalibrationArmEvidenceV1>,
    combined_score: Option<CandidateScoreV1>,
    blocking_reasons: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CalibrationProfileRejectionV1 {
    schema_version: u16,
    split: CalibrationSplitV1,
    profile_id: String,
    protocol_interest_schedule_sha256: String,
    operations_sha256: String,
    factual_ledger_sha256: String,
    transfer_fee_registry_sha256: String,
    included_markets_sha256: String,
    included_market_count: u64,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    error_name: String,
    error_code_number: u32,
    error_message: String,
    source_file: String,
    source_line: u32,
    rejection_sha256: String,
}

impl CalibrationProfileRejectionV1 {
    fn seal(mut self) -> Result<Self> {
        self.rejection_sha256.clear();
        self.rejection_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&self)?));
        Ok(self)
    }

    fn verify(&self) -> Result<()> {
        if self.schema_version != 1
            || self.error_name != "InvalidMarketConfig"
            || self.error_code_number != INVALID_MARKET_CONFIG_ERROR_CODE
            || self.source_file != EXPLICIT_CURVE_SOURCE_SUFFIX
            || self.source_line == 0
            || self.profile_id.len() != 64
            || self.included_market_count == 0
            || self.clone().seal()?.rejection_sha256 != self.rejection_sha256
        {
            bail!("calibration profile rejection failed exact identity verification");
        }
        for digest in [
            &self.protocol_interest_schedule_sha256,
            &self.operations_sha256,
            &self.factual_ledger_sha256,
            &self.transfer_fee_registry_sha256,
            &self.included_markets_sha256,
            &self.rejection_sha256,
        ] {
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("calibration profile rejection contains an invalid SHA-256");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CalibrationProfileRejectionManifestV1 {
    schema_version: u16,
    rejections: Vec<CalibrationProfileRejectionV1>,
    manifest_sha256: String,
}

impl CalibrationProfileRejectionManifestV1 {
    fn seal(mut self) -> Result<Self> {
        self.rejections.sort_by(|left, right| {
            calibration_split_name(left.split)
                .cmp(calibration_split_name(right.split))
                .then(left.profile_id.cmp(&right.profile_id))
                .then(replay_arm_name(left.arm).cmp(replay_arm_name(right.arm)))
                .then(
                    hlp_scenario_name(left.hlp_scenario).cmp(hlp_scenario_name(right.hlp_scenario)),
                )
        });
        self.manifest_sha256.clear();
        self.manifest_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&self)?));
        Ok(self)
    }

    fn verify(&self) -> Result<()> {
        for rejection in &self.rejections {
            rejection.verify()?;
        }
        let mut identities = BTreeSet::new();
        for rejection in &self.rejections {
            if !identities.insert((
                calibration_split_name(rejection.split),
                rejection.profile_id.as_str(),
            )) {
                bail!("calibration rejection manifest repeats a profile and split");
            }
        }
        if self.schema_version != 1 || self.clone().seal()?.manifest_sha256 != self.manifest_sha256
        {
            bail!("calibration rejection manifest failed checksum verification");
        }
        Ok(())
    }
}

struct HoldoutEvaluationBundleV1 {
    profile_id: String,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    opportunity_ceiling_market: Option<String>,
    paired_path: PathBuf,
    paired_summary: PairedMetricLedgerSummaryV1,
    counterfactual_summary: Option<CounterfactualEvaluationSummaryV1>,
    evidence: CandidateScoreEvidenceV1,
}

impl NaturalGrainCoverageSummaryV1 {
    fn new() -> Self {
        Self {
            schema_version: 1,
            ..Self::default()
        }
    }

    fn record(&mut self, market: Option<&str>, observed: bool, detail: &str) {
        self.coverage.expected = self.coverage.expected.saturating_add(1);
        if observed {
            self.coverage.observed = self.coverage.observed.saturating_add(1);
        } else {
            self.blocking_records = self.blocking_records.saturating_add(1);
            *self.detail_counts.entry(detail.to_owned()).or_default() += 1;
        }
        if let Some(market) = market {
            let coverage = self.by_market.entry(market.to_owned()).or_default();
            coverage.expected = coverage.expected.saturating_add(1);
            if observed {
                coverage.observed = coverage.observed.saturating_add(1);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CohortSelectionPrefixArtifactV1 {
    schema_version: u16,
    run_id: String,
    cutoff_slot: u64,
    start_slot_inclusive: u64,
    end_slot_exclusive: u64,
    selection_training_bps: u16,
    db_event_rows: u64,
    db_event_seeds_sha256: String,
    catalog_unbound_prefix_db_event_rows: u64,
    catalog_unbound_prefix_market_counts: BTreeMap<String, u64>,
    catalog_unbound_all_window_db_event_rows: u64,
    catalog_unbound_all_window_market_counts: BTreeMap<String, u64>,
    catalog_unbound_db_event_seeds_sha256: String,
}

struct BuiltCohortV1 {
    artifact: CohortSelectionArtifactV1,
    artifact_path: PathBuf,
    prefix_path: PathBuf,
    catalog_unbound_path: PathBuf,
    catalog_unbound_all_window_db_event_rows: u64,
    catalog_unbound_prefix_db_event_rows: u64,
    catalog_unbound_market_count: u64,
    catalog_unbound_prefix_market_count: u64,
    prefix_descriptor_path: PathBuf,
    evidence_path: PathBuf,
    metadata_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CohortDatabaseEventPartitionV1 {
    CatalogBoundPrefix,
    CatalogUnboundAllWindow,
    CatalogBoundOutsidePrefix,
}

const fn cohort_database_event_partition(
    catalog_bound: bool,
    in_selection_prefix: bool,
) -> CohortDatabaseEventPartitionV1 {
    match (catalog_bound, in_selection_prefix) {
        (true, true) => CohortDatabaseEventPartitionV1::CatalogBoundPrefix,
        (false, _) => CohortDatabaseEventPartitionV1::CatalogUnboundAllWindow,
        (true, false) => CohortDatabaseEventPartitionV1::CatalogBoundOutsidePrefix,
    }
}

fn catalog_unbound_quarantine_allowed(config: &BenchmarkConfig) -> bool {
    config.scope.mode == ScopeMode::HybridPanel
        && config.scope.evidence_tier == ConfigEvidenceTier::DecisionGrade
        && config.scope.explicit_market_overrides.len() == config.scope.target_count
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CohortActivityCoverageV1 {
    visible_universe_rows: u64,
    selected_panel_rows: u64,
    selected_panel_coverage_bps: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CohortRepresentativenessV1 {
    schema_version: u16,
    run_id: String,
    cutoff_slot: u64,
    cohort_selection_sha256: String,
    swaps: CohortActivityCoverageV1,
    debt_actions: CohortActivityCoverageV1,
    liquidations: CohortActivityCoverageV1,
    artifact_sha256: String,
}

impl CohortRepresentativenessV1 {
    fn seal(mut self) -> Result<Self> {
        for coverage in [
            &mut self.swaps,
            &mut self.debt_actions,
            &mut self.liquidations,
        ] {
            if coverage.selected_panel_rows > coverage.visible_universe_rows {
                bail!("selected cohort activity exceeds visible-universe activity");
            }
            coverage.selected_panel_coverage_bps = if coverage.visible_universe_rows == 0 {
                0
            } else {
                u16::try_from(
                    (u128::from(coverage.selected_panel_rows) * 10_000)
                        / u128::from(coverage.visible_universe_rows),
                )?
            };
        }
        self.artifact_sha256.clear();
        self.artifact_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&self)?));
        Ok(self)
    }

    fn verify(&self) -> Result<()> {
        if self.schema_version != 1
            || self.run_id.trim().is_empty()
            || self.cohort_selection_sha256.len() != 64
            || self.clone().seal()?.artifact_sha256 != self.artifact_sha256
        {
            bail!("cohort representativeness artifact is invalid");
        }
        Ok(())
    }
}

fn build_pre_audit_cohort(
    context: &RunContext,
    config: &BenchmarkConfig,
    market_rows: &[String],
    market_mint_bindings: &[MarketMintBindingV1],
    db_event_path: &Path,
    program_signature_path: &Path,
    earliest_successful_program_slot: u64,
) -> Result<BuiltCohortV1> {
    let cutoff_exclusive = context
        .cutoff_slot()
        .checked_add(1)
        .context("cutoff slot cannot be represented as an exclusive bound")?;
    let span = cutoff_exclusive
        .checked_sub(earliest_successful_program_slot)
        .context("successful program history starts after the cutoff")?;
    let prefix_length = u64::try_from(
        (u128::from(span) * u128::from(config.scope.selection_training_bps)) / 10_000,
    )?
    .max(1);
    let selection_end_slot_exclusive = earliest_successful_program_slot
        .checked_add(prefix_length)
        .context("cohort selection prefix overflow")?
        .min(cutoff_exclusive);

    let mut market_evidence = Vec::with_capacity(market_rows.len());
    for row in market_rows {
        let value: JsonValue = serde_json::from_str(row)?;
        market_evidence.push(MarketEvidence {
            pair_address: value
                .get("pair_address")
                .and_then(JsonValue::as_str)
                .context("market catalog row is missing pair_address")?
                .to_owned(),
            visible: value
                .get("visible")
                .and_then(JsonValue::as_bool)
                .context("market catalog row is missing boolean visible")?,
            // This is deliberately a pre-audit projection. The cohort module
            // ignores this field and final RPC-backed qualification is mandatory.
            hard_gates_pass: false,
            swap_count: 0,
            active_swap_days: 0,
            median_time_weighted_tvl_usd_micros: None,
        });
    }
    market_evidence.sort_by(|left, right| left.pair_address.cmp(&right.pair_address));
    let evidence_path = context.data_dir().join(COHORT_MARKET_EVIDENCE_JSON);
    write_json(&evidence_path, &market_evidence)?;

    let mut market_metadata: Vec<CohortMarketMetadataV1> = market_mint_bindings
        .iter()
        .map(|binding| CohortMarketMetadataV1 {
            pair_address: binding.pair_address.clone(),
            token0_mint: binding.token0.clone(),
            token1_mint: binding.token1.clone(),
            lp_mint: binding.lp_mint.clone(),
            edge_evidence: CohortEdgeEvidenceV1::Unavailable {
                reason: CohortEdgeUnavailableReasonV1::MintAndConfigurationHistoryUnavailable,
            },
            tail_qualification_evidence: CohortTailQualificationEvidenceV1::Unavailable {
                reason:
                    CohortTailQualificationUnavailableReasonV1::AuthoritativeBlockTimeUnavailable,
            },
        })
        .collect();
    market_metadata.sort_by(|left, right| left.pair_address.cmp(&right.pair_address));
    let metadata_path = context.data_dir().join(COHORT_MARKET_METADATA_JSON);
    write_json(&metadata_path, &market_metadata)?;

    let prefix_path = context.data_dir().join(COHORT_PREFIX_DB_EVENTS_JSONL);
    let prefix_partial = partial_path(&prefix_path);
    let mut prefix_writer = BufWriter::new(File::create(&prefix_partial)?);
    let catalog_unbound_path = context
        .data_dir()
        .join(COHORT_CATALOG_UNBOUND_DB_EVENTS_JSONL);
    let catalog_unbound_partial = partial_path(&catalog_unbound_path);
    let mut catalog_unbound_writer = BufWriter::new(File::create(&catalog_unbound_partial)?);
    let catalog_pairs: BTreeSet<&str> = market_evidence
        .iter()
        .map(|market| market.pair_address.as_str())
        .collect();
    let mut catalog_unbound_all_window_market_counts = BTreeMap::<String, u64>::new();
    let mut catalog_unbound_prefix_market_counts = BTreeMap::<String, u64>::new();
    let mut prefix_events = Vec::<DbEventSeed>::new();
    for (line_index, line) in BufReader::new(File::open(db_event_path)?)
        .lines()
        .enumerate()
    {
        let line = line?;
        let event: DbEventSeed = serde_json::from_str(&line).with_context(|| {
            format!(
                "decoding {} line {} for sealed cohort prefix",
                db_event_path.display(),
                line_index + 1
            )
        })?;
        let in_selection_prefix = event.slot >= earliest_successful_program_slot
            && event.slot < selection_end_slot_exclusive;
        match cohort_database_event_partition(
            catalog_pairs.contains(event.pair.as_str()),
            in_selection_prefix,
        ) {
            CohortDatabaseEventPartitionV1::CatalogBoundPrefix => {
                write_json_line(&mut prefix_writer, &event)?;
                prefix_events.push(event);
            }
            CohortDatabaseEventPartitionV1::CatalogUnboundAllWindow => {
                write_json_line(&mut catalog_unbound_writer, &event)?;
                let count = catalog_unbound_all_window_market_counts
                    .entry(event.pair.clone())
                    .or_default();
                *count = count
                    .checked_add(1)
                    .context("catalog-unbound cohort event count overflow")?;
                if in_selection_prefix {
                    let prefix_count = catalog_unbound_prefix_market_counts
                        .entry(event.pair.clone())
                        .or_default();
                    *prefix_count = prefix_count
                        .checked_add(1)
                        .context("catalog-unbound cohort-prefix event count overflow")?;
                }
            }
            CohortDatabaseEventPartitionV1::CatalogBoundOutsidePrefix => {}
        }
    }
    prefix_writer.flush()?;
    catalog_unbound_writer.flush()?;
    fs::rename(&prefix_partial, &prefix_path)?;
    fs::rename(&catalog_unbound_partial, &catalog_unbound_path)?;
    if prefix_events.is_empty() {
        bail!("sealed cohort selection prefix contains no database events");
    }
    let catalog_unbound_all_window_db_event_rows = catalog_unbound_all_window_market_counts
        .values()
        .try_fold(0_u64, |sum, count| sum.checked_add(*count))
        .context("catalog-unbound cohort event total overflow")?;
    let catalog_unbound_prefix_db_event_rows = catalog_unbound_prefix_market_counts
        .values()
        .try_fold(0_u64, |sum, count| sum.checked_add(*count))
        .context("catalog-unbound cohort-prefix event total overflow")?;
    if catalog_unbound_all_window_db_event_rows > 0 && !catalog_unbound_quarantine_allowed(config) {
        bail!(
            "database history contains {catalog_unbound_all_window_db_event_rows} event rows whose markets are absent from the current catalog; quarantine is permitted only for a DecisionGrade hybrid panel completely fixed by explicit overrides"
        );
    }

    let prefix_descriptor_path = context.data_dir().join(COHORT_SELECTION_PREFIX_JSON);
    let prefix_descriptor = CohortSelectionPrefixArtifactV1 {
        schema_version: 3,
        run_id: context.run_id().to_owned(),
        cutoff_slot: context.cutoff_slot(),
        start_slot_inclusive: earliest_successful_program_slot,
        end_slot_exclusive: selection_end_slot_exclusive,
        selection_training_bps: config.scope.selection_training_bps,
        db_event_rows: u64::try_from(prefix_events.len())?,
        db_event_seeds_sha256: sha256_file(&prefix_path)?,
        catalog_unbound_prefix_db_event_rows,
        catalog_unbound_prefix_market_counts,
        catalog_unbound_all_window_db_event_rows,
        catalog_unbound_all_window_market_counts,
        catalog_unbound_db_event_seeds_sha256: sha256_file(&catalog_unbound_path)?,
    };
    write_json(&prefix_descriptor_path, &prefix_descriptor)?;

    let source_bindings = CohortSourceBindingsV1 {
        config_sha256: context.config_sha256().to_owned(),
        sealed_prefix_sha256: sha256_file(&prefix_descriptor_path)?,
        market_evidence_sha256: sha256_file(&evidence_path)?,
        db_event_seeds_sha256: prefix_descriptor.db_event_seeds_sha256.clone(),
        market_metadata_sha256: sha256_file(&metadata_path)?,
        global_signature_inventory_sha256: config
            .scope
            .keep_global_signature_inventory
            .then(|| sha256_file(program_signature_path))
            .transpose()?,
    };
    let artifact = select_cohort(CohortSelectionRequestV1 {
        run_id: context.run_id(),
        cutoff_slot: context.cutoff_slot(),
        selection_start_slot: earliest_successful_program_slot,
        selection_end_slot_exclusive,
        minimum_prefix_swaps_for_tail: config.universe.minimum_swaps,
        minimum_prefix_active_days_for_tail: u32::try_from(config.universe.minimum_active_days)?,
        scope: &config.scope,
        source_bindings: &source_bindings,
        market_evidence: &market_evidence,
        db_event_seeds: &prefix_events,
        market_metadata: &market_metadata,
    })
    .context("selecting the sealed pre-audit market cohort")?;
    artifact.verify_self_hash()?;
    artifact.extraction_targets()?;
    let artifact_path = context.run_dir().join(COHORT_SELECTION_JSON);
    write_json(&artifact_path, &artifact)?;
    Ok(BuiltCohortV1 {
        artifact,
        artifact_path,
        prefix_path,
        catalog_unbound_path,
        catalog_unbound_all_window_db_event_rows,
        catalog_unbound_prefix_db_event_rows,
        catalog_unbound_market_count: u64::try_from(
            prefix_descriptor
                .catalog_unbound_all_window_market_counts
                .len(),
        )?,
        catalog_unbound_prefix_market_count: u64::try_from(
            prefix_descriptor.catalog_unbound_prefix_market_counts.len(),
        )?,
        prefix_descriptor_path,
        evidence_path,
        metadata_path,
    })
}

fn retain_scoped_canonical_history(
    operations_path: &Path,
    events_path: &Path,
    selected_markets: &BTreeSet<String>,
) -> Result<(u64, u64)> {
    let operation_partial = partial_path(operations_path);
    let mut operation_writer = BufWriter::new(File::create(&operation_partial)?);
    let mut operation_count = 0_u64;
    visit_json_lines::<CanonicalOperationV1, _>(operations_path, |operation| {
        if operation.pair == "__global__" || selected_markets.contains(&operation.pair) {
            write_json_line(&mut operation_writer, &operation)?;
            operation_count = operation_count.saturating_add(1);
        }
        Ok(())
    })?;
    operation_writer.flush()?;
    fs::rename(&operation_partial, operations_path)?;

    let event_partial = partial_path(events_path);
    let mut event_writer = BufWriter::new(File::create(&event_partial)?);
    let mut event_count = 0_u64;
    visit_json_lines::<CanonicalEventV1, _>(events_path, |event| {
        if event
            .pair
            .as_ref()
            .is_none_or(|pair| selected_markets.contains(pair))
        {
            write_json_line(&mut event_writer, &event)?;
            event_count = event_count.saturating_add(1);
        }
        Ok(())
    })?;
    event_writer.flush()?;
    fs::rename(&event_partial, events_path)?;
    Ok((operation_count, event_count))
}

fn select_scoped_program_signatures(
    successful_program_signatures: &[ProgramSignatureV1],
    all_database_signatures: &BTreeSet<String>,
    selected_database_signatures: &BTreeSet<String>,
) -> Result<(Vec<ProgramSignatureV1>, u64)> {
    if selected_database_signatures.is_empty() {
        bail!("selected cohort has no database signature");
    }
    if !selected_database_signatures.is_subset(all_database_signatures) {
        bail!("selected database signature set is not a subset of the full database export");
    }
    let successful_signature_set: BTreeSet<&str> = successful_program_signatures
        .iter()
        .map(|row| row.signature.as_str())
        .collect();
    let missing_selected = selected_database_signatures
        .iter()
        .filter(|signature| !successful_signature_set.contains(signature.as_str()))
        .count();
    if missing_selected != 0 {
        bail!(
            "{missing_selected} selected-market database signatures are absent from the certified successful-program inventory"
        );
    }
    let program_only_count = successful_program_signatures
        .iter()
        .filter(|row| !all_database_signatures.contains(&row.signature))
        .count();
    let target_signatures: Vec<ProgramSignatureV1> = successful_program_signatures
        .iter()
        .filter(|row| {
            selected_database_signatures.contains(&row.signature)
                || !all_database_signatures.contains(&row.signature)
        })
        .cloned()
        .collect();
    if target_signatures.is_empty() {
        bail!("scoped extraction produced no target program signature");
    }
    Ok((target_signatures, u64::try_from(program_only_count)?))
}

#[cfg(test)]
mod scoped_extraction_tests {
    use super::*;

    fn shipped_config() -> BenchmarkConfig {
        BenchmarkConfig::from_path(&Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmark.toml"))
            .expect("shipped benchmark config")
    }

    fn signature(value: &str, slot: u64) -> ProgramSignatureV1 {
        ProgramSignatureV1 {
            signature: value.to_owned(),
            slot,
            succeeded: true,
            rpc_block_time_hint: None,
        }
    }

    #[test]
    fn selected_db_and_program_only_signatures_define_the_scoped_target() {
        let program = vec![
            signature("selected", 1),
            signature("nonselected", 2),
            signature("config-only", 3),
        ];
        let all_db = BTreeSet::from(["selected".to_owned(), "nonselected".to_owned()]);
        let selected_db = BTreeSet::from(["selected".to_owned()]);
        let (target, program_only) =
            select_scoped_program_signatures(&program, &all_db, &selected_db).unwrap();
        assert_eq!(program_only, 1);
        assert_eq!(
            target
                .iter()
                .map(|row| row.signature.as_str())
                .collect::<Vec<_>>(),
            ["selected", "config-only"]
        );
    }

    #[test]
    fn selected_db_signature_missing_from_program_inventory_fails_closed() {
        let program = vec![signature("other", 1)];
        let all_db = BTreeSet::from(["selected".to_owned()]);
        let selected_db = BTreeSet::from(["selected".to_owned()]);
        assert!(select_scoped_program_signatures(&program, &all_db, &selected_db).is_err());
    }

    #[test]
    fn catalog_unbound_quarantine_requires_the_exact_fixed_decision_grade_panel() {
        let exact = shipped_config();
        assert!(catalog_unbound_quarantine_allowed(&exact));

        let mut audit_grade = exact.clone();
        audit_grade.scope.evidence_tier = ConfigEvidenceTier::AuditGrade;
        assert!(!catalog_unbound_quarantine_allowed(&audit_grade));

        let mut dynamically_filled = exact.clone();
        dynamically_filled.scope.explicit_market_overrides.pop();
        assert!(!catalog_unbound_quarantine_allowed(&dynamically_filled));

        let mut full_universe = exact;
        full_universe.scope.mode = ScopeMode::FullUniverse;
        assert!(!catalog_unbound_quarantine_allowed(&full_universe));
    }

    #[test]
    fn catalog_unbound_rows_after_the_selection_prefix_are_still_quarantined() {
        assert_eq!(
            cohort_database_event_partition(false, true),
            CohortDatabaseEventPartitionV1::CatalogUnboundAllWindow
        );
        assert_eq!(
            cohort_database_event_partition(false, false),
            CohortDatabaseEventPartitionV1::CatalogUnboundAllWindow
        );
        assert_eq!(
            cohort_database_event_partition(true, false),
            CohortDatabaseEventPartitionV1::CatalogBoundOutsidePrefix
        );
    }
}

pub(crate) async fn extract(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    let program_id = context
        .runtime_value(&config.source.program_id_env)?
        .to_owned();
    let database_url = context
        .runtime_value(&config.source.database_url_env)?
        .to_owned();
    let rpc_url = context
        .runtime_value(&config.source.rpc_url_env)?
        .to_owned();
    let database_source =
        SanitizedSource::from_secret("postgresql", "configured_database_url", &database_url)?;
    let rpc_source = SanitizedSource::from_secret("solana_rpc", "configured_rpc_url", &rpc_url)?;
    let rpc = HttpArchivalRpc::new(rpc_url, config.source.rpc_max_attempts)
        .map_err(|kind| anyhow::anyhow!("invalid archival RPC configuration: {kind:?}"))?;
    let cutoff_block = rpc
        .get_block(finalized_get_block_request(context.cutoff_slot(), 1))
        .await
        .map_err(|kind| anyhow::anyhow!("archival cutoff block unavailable: {kind:?}"))?
        .context("archival cutoff block is unavailable")?;
    let cutoff_block_time = cutoff_block
        .block_time_unix_seconds
        .context("archival cutoff block lacks canonical block time")?;
    let (signature_checkpoint, successful_signatures) = extract_program_signatures(
        &rpc,
        context.run_id(),
        &program_id,
        context.cutoff_slot(),
        &rpc_source,
        context.data_dir(),
    )
    .await?;
    let signature_path = context.data_dir().join(PROGRAM_SIGNATURES_JSONL);
    let earliest_successful_program_slot =
        successful_signatures
            .iter()
            .map(|row| row.slot)
            .min()
            .context("program signature inventory contains no successful transaction")?;

    let exporter = ReadOnlyPgExporter::connect_no_tls(&database_url)
        .await
        .context("opening read-only repeatable-read PostgreSQL export")?;
    let cutoff_i64 =
        i64::try_from(context.cutoff_slot()).context("cutoff slot exceeds PostgreSQL bigint")?;
    let zero = 0_i64;

    let market_rows = exporter
        .query_json_text(MARKETS_SQL, &[])
        .await
        .context("exporting visible-market catalog")?;
    let market_path = context.data_dir().join(MARKETS_JSONL);
    write_raw_json_lines(&market_path, &market_rows)?;
    let market_mint_bindings = market_rows
        .iter()
        .map(|row| -> Result<MarketMintBindingV1> {
            let value: JsonValue = serde_json::from_str(row)?;
            let required = |field: &str| -> Result<String> {
                value
                    .get(field)
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
                    .with_context(|| format!("market catalog row is missing {field}"))
            };
            Ok(MarketMintBindingV1 {
                pair_address: required("pair_address")?,
                token0: required("token0")?,
                token1: required("token1")?,
                lp_mint: required("lp_mint")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let coverage_rows = exporter
        .query_json_text(COVERAGE_SQL, &[&zero, &cutoff_i64])
        .await
        .context("profiling source-table coverage")?;
    let table_coverage: Vec<TableCoverageV1> = coverage_rows
        .iter()
        .map(|row| serde_json::from_str(row).context("decoding table coverage"))
        .collect::<Result<_>>()?;
    let database_start = table_coverage
        .iter()
        .filter_map(|row| row.min_slot)
        .min()
        .unwrap_or(context.cutoff_slot());

    let db_event_path = context.data_dir().join(DB_EVENTS_JSONL);
    let db_event_partial = partial_path(&db_event_path);
    let mut db_writer = BufWriter::new(File::create(&db_event_partial)?);
    let mut db_checkpoint =
        ExportCheckpoint::new("canonical_events_v1", database_start.saturating_sub(1));
    let db_checkpoint_path = context.data_dir().join(DB_EXPORT_CHECKPOINT);
    let mut db_signatures = BTreeSet::new();
    while !db_checkpoint.complete {
        let page = export_event_page(
            &exporter,
            database_start,
            context.cutoff_slot(),
            &mut db_checkpoint,
            10_000,
        )
        .await
        .context("exporting raw database event page")?;
        for row in &page {
            serde_json::to_writer(&mut db_writer, row)?;
            db_writer.write_all(b"\n")?;
            db_signatures.insert(row.signature.clone());
        }
        if db_checkpoint.pages_completed % 10 == 0 || db_checkpoint.complete {
            eprintln!(
                "extract: database events exported={}",
                db_checkpoint.rows_exported
            );
        }
        db_writer.flush()?;
        write_json(&db_checkpoint_path, &db_checkpoint)?;
    }
    db_writer.flush()?;
    fs::rename(&db_event_partial, &db_event_path)?;

    let built_cohort = build_pre_audit_cohort(
        context,
        config,
        &market_rows,
        &market_mint_bindings,
        &db_event_path,
        &signature_path,
        earliest_successful_program_slot,
    )?;
    let extraction_targets = built_cohort.artifact.extraction_targets()?;
    let selected_markets: BTreeSet<String> = extraction_targets.markets.iter().cloned().collect();
    if selected_markets.is_empty() {
        bail!("pre-audit cohort selected no market");
    }
    let selected_market_bindings: Vec<MarketMintBindingV1> = market_mint_bindings
        .iter()
        .filter(|binding| selected_markets.contains(&binding.pair_address))
        .cloned()
        .collect();
    if selected_market_bindings.len() != selected_markets.len() {
        bail!("selected cohort is missing a market-to-mint binding");
    }

    let visible_markets = market_rows.iter().try_fold(
        BTreeSet::<String>::new(),
        |mut visible, row| -> Result<_> {
            let value: JsonValue = serde_json::from_str(row)?;
            if value
                .get("visible")
                .and_then(JsonValue::as_bool)
                .context("market catalog row is missing boolean visible")?
            {
                visible.insert(
                    value
                        .get("pair_address")
                        .and_then(JsonValue::as_str)
                        .context("market catalog row is missing pair_address")?
                        .to_owned(),
                );
            }
            Ok(visible)
        },
    )?;
    let mut selected_database_event_rows = 0_u64;
    let mut selected_db_signatures = BTreeSet::<String>::new();
    let mut representativeness = CohortRepresentativenessV1 {
        schema_version: 1,
        run_id: context.run_id().to_owned(),
        cutoff_slot: context.cutoff_slot(),
        cohort_selection_sha256: built_cohort.artifact.artifact_sha256.clone(),
        swaps: CohortActivityCoverageV1::default(),
        debt_actions: CohortActivityCoverageV1::default(),
        liquidations: CohortActivityCoverageV1::default(),
        artifact_sha256: String::new(),
    };
    visit_json_lines::<DbEventSeed, _>(&db_event_path, |event| {
        let coverage = match event.source_table.as_str() {
            "swaps" => Some(&mut representativeness.swaps),
            "adjust_debt_events" => Some(&mut representativeness.debt_actions),
            "user_position_liquidated_events" => Some(&mut representativeness.liquidations),
            _ => None,
        };
        if let Some(coverage) = coverage {
            if visible_markets.contains(&event.pair) {
                coverage.visible_universe_rows = coverage.visible_universe_rows.saturating_add(1);
            }
            if selected_markets.contains(&event.pair) {
                coverage.selected_panel_rows = coverage.selected_panel_rows.saturating_add(1);
            }
        }
        if selected_markets.contains(&event.pair) {
            selected_database_event_rows = selected_database_event_rows.saturating_add(1);
            selected_db_signatures.insert(event.signature.clone());
        }
        Ok(())
    })?;
    let representativeness = representativeness.seal()?;
    representativeness.verify()?;
    let representativeness_path = context.run_dir().join(COHORT_REPRESENTATIVENESS_JSON);
    write_json(&representativeness_path, &representativeness)?;
    let (target_signatures, successful_program_signatures_without_database_rows) =
        select_scoped_program_signatures(
            &successful_signatures,
            &db_signatures,
            &selected_db_signatures,
        )?;
    let target_slots: Vec<u64> = target_signatures
        .iter()
        .map(|signature| signature.slot)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let successful_by_slot = signatures_by_slot(&target_signatures);
    let archival_block_cache = config
        .source
        .archival_block_cache
        .as_ref()
        .map(|cache_config| {
            VerifiedArchivalBlockCacheV1::open(context.root(), context.data_dir(), cache_config)
        })
        .transpose()
        .context("opening configured archival block-cache donor")?;
    if let Some(cache) = &archival_block_cache {
        cache
            .preflight_complete(&successful_by_slot)
            .context("preflighting complete archival block-cache coverage")?;
    }
    if let Some(first_slot) = target_slots.first().copied() {
        extract_archival_blocks(
            &rpc,
            &[first_slot],
            &successful_by_slot,
            context.data_dir(),
            1,
            archival_block_cache.as_ref(),
        )
        .await
        .context("fetching earliest authoritative scoped program block")?;
    }
    let earliest_block_time = target_slots
        .first()
        .map(|slot| {
            read_archived_block(&block_path(context.data_dir(), *slot))?
                .block_time_unix_seconds
                .context("earliest scoped archival program block lacks canonical block time")
        })
        .transpose()?
        .unwrap_or(cutoff_block_time);
    let price_bucket_seconds = i64::try_from(config.window.price_bucket_seconds)?;
    let historical_price_window_start = earliest_block_time
        .div_euclid(price_bucket_seconds)
        .checked_mul(price_bucket_seconds)
        .context("historical price start overflow")?;
    let historical_price_window_end = cutoff_block_time
        .div_euclid(price_bucket_seconds)
        .checked_add(1)
        .and_then(|bucket| bucket.checked_mul(price_bucket_seconds))
        .context("historical price window overflow")?;
    let selected_price_mints: BTreeSet<String> = selected_market_bindings
        .iter()
        .flat_map(|binding| [binding.token0.clone(), binding.token1.clone()])
        .collect();
    if selected_price_mints.is_empty() {
        bail!("selected-market historical-price mint allowlist is empty");
    }
    let selected_mint_allowlist_csv = selected_price_mints
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let database_historical_price_path = context.data_dir().join(DATABASE_HISTORICAL_PRICES_JSONL);
    let historical_price_path = context.data_dir().join(HISTORICAL_PRICES_JSONL);
    let historical_price_partial = partial_path(&database_historical_price_path);
    let mut historical_price_writer = BufWriter::new(File::create(&historical_price_partial)?);
    let mut historical_price_checkpoint =
        HistoricalPriceExportCheckpoint::new(historical_price_window_start);
    let historical_price_checkpoint_path = context.data_dir().join(PRICE_CHECKPOINT);
    while !historical_price_checkpoint.complete {
        let page = export_historical_price_page(
            &exporter,
            historical_price_window_start,
            historical_price_window_end,
            &selected_mint_allowlist_csv,
            &config.source.historical_price_provider,
            &mut historical_price_checkpoint,
            10_000,
        )
        .await
        .context("exporting selected-market historical hourly USD marks")?;
        for mark in &page {
            write_json_line(&mut historical_price_writer, mark)?;
        }
        eprintln!(
            "extract: selected-market historical price marks exported={}",
            historical_price_checkpoint.rows_exported
        );
        historical_price_writer.flush()?;
        write_json(
            &historical_price_checkpoint_path,
            &historical_price_checkpoint,
        )?;
    }
    historical_price_writer.flush()?;
    fs::rename(&historical_price_partial, &database_historical_price_path)?;
    exporter
        .finish()
        .await
        .context("closing read-only PostgreSQL snapshot")?;

    let rpc_composition_calibration = calibrate_rpc_composition(
        &rpc,
        &target_slots,
        &successful_by_slot,
        context.data_dir(),
        context.run_id(),
        context.cutoff_slot(),
        &rpc_source.secret_fingerprint_sha256,
        config.source.rpc_concurrency,
        archival_block_cache.as_ref(),
    )
    .await
    .context("calibrating ordered-signature plus independent-transaction RPC composition")?;
    let rpc_composition_calibration_path =
        context.data_dir().join(RPC_COMPOSITION_CALIBRATION_JSON);
    let rpc_checkpoint = extract_archival_blocks(
        &rpc,
        &target_slots,
        &successful_by_slot,
        context.data_dir(),
        config.source.rpc_concurrency,
        archival_block_cache.as_ref(),
    )
    .await?;
    if archival_block_cache.is_some()
        && (!rpc_checkpoint.failures.is_empty()
            || rpc_checkpoint.completed_slots
                != target_slots.iter().copied().collect::<BTreeSet<_>>())
    {
        bail!("complete archival block-cache import did not materialize the exact target scope");
    }
    let decoder_registry_path = context.root().join(DECODER_REVISIONS_TOML);
    let decoder_registry: DecoderRegistryV1 = toml::from_str(
        &fs::read_to_string(&decoder_registry_path)
            .with_context(|| format!("reading {}", decoder_registry_path.display()))?,
    )
    .context("decoding versioned V1 decoder registry")?;
    if let Some(earliest) = successful_signatures.iter().map(|row| row.slot).min() {
        decoder_registry
            .validate_complete_range(earliest, context.cutoff_slot())
            .context("decoder registry does not cover the extracted lifecycle")?;
    }
    let mut decode_inventory = decode_archival_history(
        context.data_dir(),
        &rpc_checkpoint,
        &program_id,
        &decoder_registry,
        target_slots
            .first()
            .copied()
            .unwrap_or(context.cutoff_slot()),
        context.cutoff_slot(),
    )?;
    let canonical_operations_path = context.data_dir().join(CANONICAL_OPERATIONS_JSONL);
    let canonical_events_path = context.data_dir().join(CANONICAL_EVENTS_JSONL);
    let config_history_path = context.data_dir().join(CONFIG_HISTORY_JSONL);
    let normalization_gaps_path = context.data_dir().join(NORMALIZATION_GAPS_JSONL);
    let (scoped_operation_count, scoped_event_count) = retain_scoped_canonical_history(
        &canonical_operations_path,
        &canonical_events_path,
        &selected_markets,
    )?;
    decode_inventory.canonical_operations = scoped_operation_count;
    decode_inventory.canonical_events = scoped_event_count;

    let historical_price_sidecar_receipt_path = context
        .data_dir()
        .join(HISTORICAL_PRICE_SIDECAR_RECEIPT_JSON);
    let historical_price_sidecar_receipt: Option<HistoricalPriceSidecarMergeReceiptV1> =
        if let Some(binding) = &config.source.historical_price_sidecar {
            let configured_root = Path::new(&binding.root);
            let sidecar_root = if configured_root.is_absolute() {
                configured_root.to_path_buf()
            } else {
                context.root().join(configured_root)
            };
            Some(
                validate_and_merge_historical_price_sidecar(&HistoricalPriceSidecarMergeSpec {
                    sidecar_root: &sidecar_root,
                    binding,
                    canonical_operations_path: &canonical_operations_path,
                    original_historical_prices_path: &database_historical_price_path,
                    merged_historical_prices_output_path: &historical_price_path,
                    receipt_output_path: &historical_price_sidecar_receipt_path,
                    expected_provider: &config.source.historical_price_provider,
                })
                .context("validating and merging proof-bound historical-price sidecar")?,
            )
        } else {
            let partial = partial_path(&historical_price_path);
            fs::copy(&database_historical_price_path, &partial).with_context(|| {
                format!(
                    "copying database historical prices from {} to {}",
                    database_historical_price_path.display(),
                    partial.display()
                )
            })?;
            fs::rename(&partial, &historical_price_path)?;
            None
        };
    let decoded_transactions_path = context.data_dir().join(DECODED_TRANSACTIONS_JSONL);
    let execution_provenance_path = context.data_dir().join(EXECUTION_PROVENANCE_JSONL);
    let mut transfer_fee_support_artifacts = Vec::<(String, PathBuf)>::new();
    let (transfer_fee_provenance_path, transfer_fee_registry_path) = if config.scope.mode
        == ScopeMode::HybridPanel
    {
        let selected_output = extract_selected_scope_transfer_fee_provenance(
            &rpc,
            &SelectedScopeTransferFeeExtractionRequestV1 {
                run_id: context.run_id(),
                cutoff_slot: context.cutoff_slot(),
                provider: &rpc_source,
                evidence_tier: gate_evidence_tier(config.scope.mode, config.scope.evidence_tier)?,
                markets: &selected_market_bindings,
                output_dir: context.data_dir(),
                signature_page_limit: DEFAULT_MINT_SIGNATURE_PAGE_LIMIT,
                decoded_transactions_jsonl: &decoded_transactions_path,
                decoded_transactions_artifact_sha256: &sha256_file(&decoded_transactions_path)?,
                execution_provenance_jsonl: &execution_provenance_path,
                execution_provenance_artifact_sha256: &sha256_file(&execution_provenance_path)?,
            },
        )
        .await
        .context("extracting selected-scope mint ownership and transfer-fee evidence")?;
        transfer_fee_support_artifacts.push((
            SELECTED_OPERATION_TOKEN_PROGRAM_EVIDENCE_JSON.to_owned(),
            selected_output.operation_evidence_path,
        ));
        transfer_fee_support_artifacts.push((
            DECISION_GRADE_LEGACY_MINT_PROOFS_JSON.to_owned(),
            selected_output.legacy_proofs_path,
        ));
        if let Some(path) = selected_output.fallback_provenance_path {
            transfer_fee_support_artifacts.push((
                format!("{TRANSFER_FEE_EXACT_FALLBACK_DIR}/{TRANSFER_FEE_PROVENANCE_JSON}"),
                path,
            ));
        }
        (
            selected_output.provenance_path,
            selected_output.registry_path,
        )
    } else {
        let exact_output = extract_transfer_fee_provenance(
            &rpc,
            &TransferFeeExtractionRequestV1 {
                run_id: context.run_id(),
                cutoff_slot: context.cutoff_slot(),
                provider: &rpc_source,
                markets: &selected_market_bindings,
                output_dir: context.data_dir(),
                signature_page_limit: DEFAULT_MINT_SIGNATURE_PAGE_LIMIT,
            },
        )
        .await
        .context("extracting exact historical mint ownership and transfer fees")?;
        (exact_output.provenance_path, exact_output.registry_path)
    };

    let mut matched_signatures = BTreeSet::new();
    for slot in &rpc_checkpoint.completed_slots {
        let block_path = block_path(context.data_dir(), *slot);
        let block = read_archived_block(&block_path)?;
        if let Some(expected) = successful_by_slot.get(slot) {
            for transaction in &block.transactions {
                if transaction.succeeded && expected.contains(&transaction.signature) {
                    matched_signatures.insert(transaction.signature.clone());
                }
            }
        }
    }

    let enriched_path = context.data_dir().join(ENRICHED_DB_EVENTS_JSONL);
    let mut enriched_writer = BufWriter::new(File::create(&enriched_path)?);
    let mut enriched_count = 0_u64;
    let mut enrichment_failures = 0_u64;
    let mut missing_instruction_path = 0_u64;
    let mut cached_block_slot = None;
    let mut cached_block = None;
    let db_event_reader = BufReader::new(File::open(&db_event_path)?);
    for (line_index, line) in db_event_reader.lines().enumerate() {
        let line = line?;
        let event: DbEventSeed = serde_json::from_str(&line).with_context(|| {
            format!(
                "decoding {} line {} during archival enrichment",
                db_event_path.display(),
                line_index + 1
            )
        })?;
        if !selected_markets.contains(&event.pair) {
            continue;
        }
        if event.instruction_path.is_none() {
            missing_instruction_path = missing_instruction_path.saturating_add(1);
        }
        if cached_block_slot != Some(event.slot) {
            cached_block_slot = Some(event.slot);
            cached_block = block_path(context.data_dir(), event.slot)
                .exists()
                .then(|| read_archived_block(&block_path(context.data_dir(), event.slot)))
                .transpose()?;
        }
        let Some(block) = cached_block.as_ref() else {
            enrichment_failures = enrichment_failures.saturating_add(1);
            continue;
        };
        match enrich_db_event(event, block) {
            Ok(enriched) => {
                serde_json::to_writer(&mut enriched_writer, &enriched)?;
                enriched_writer.write_all(b"\n")?;
                enriched_count = enriched_count.saturating_add(1);
            }
            Err(_) => enrichment_failures = enrichment_failures.saturating_add(1),
        }
    }
    enriched_writer.flush()?;

    let archival_block_cache_import = archival_block_cache
        .as_ref()
        .map(|cache| {
            cache.receipt(
                context.data_dir(),
                context.run_id(),
                context.cutoff_slot(),
                &program_id,
                &successful_by_slot,
            )
        })
        .transpose()?;
    let archival_block_cache_import_path = archival_block_cache_import.as_ref().map(|receipt| {
        let path = context.data_dir().join(ARCHIVAL_BLOCK_CACHE_IMPORT_JSON);
        (path, receipt)
    });
    if let Some((path, receipt)) = &archival_block_cache_import_path {
        write_json(path, receipt)?;
    }

    let source_fingerprints = source_fingerprints(context)?;
    let signature_checkpoint_path = context.data_dir().join(SIGNATURE_CHECKPOINT);
    let rpc_checkpoint_path = context.data_dir().join(RPC_CHECKPOINT);
    let block_artifacts_path = context.data_dir().join(BLOCK_ARTIFACTS_JSON);
    let decoded_instructions_path = context.data_dir().join(DECODED_INSTRUCTIONS_JSONL);
    let block_decode_failures_path = context.data_dir().join(BLOCK_DECODE_FAILURES_JSONL);
    let transaction_decode_failures_path =
        context.data_dir().join(TRANSACTION_DECODE_FAILURES_JSONL);
    let parquet_dir = context.data_dir().join("parquet");
    fs::create_dir_all(&parquet_dir)?;
    let parquet_specs = [
        (
            BenchmarkParquetDatasetV1::CanonicalOperations,
            "canonical-operations",
            &canonical_operations_path,
        ),
        (
            BenchmarkParquetDatasetV1::CanonicalEvents,
            "canonical-events",
            &canonical_events_path,
        ),
        (
            BenchmarkParquetDatasetV1::ConfigurationHistory,
            "configuration-history",
            &config_history_path,
        ),
        (
            BenchmarkParquetDatasetV1::HistoricalPrices,
            "historical-prices",
            &historical_price_path,
        ),
    ];
    let mut parquet_artifacts = Vec::<(String, PathBuf)>::new();
    for (dataset, stem, source) in parquet_specs {
        let parquet_path = parquet_dir.join(format!("{stem}.parquet"));
        let manifest_path = parquet_dir.join(format!("{stem}.manifest.json"));
        ensure_parquet_artifact(&ParquetExportSpecV1 {
            dataset,
            logical_name: dataset.logical_name(),
            source_jsonl: source,
            parquet_output: &parquet_path,
            manifest_output: &manifest_path,
        })
        .with_context(|| format!("exporting declared {stem} Parquet dataset"))?;
        parquet_artifacts.push((format!("parquet/{stem}.parquet"), parquet_path));
        parquet_artifacts.push((format!("parquet/{stem}.manifest.json"), manifest_path));
    }
    let mut artifacts = artifact_map(&[
        (COHORT_SELECTION_JSON, &built_cohort.artifact_path),
        (
            COHORT_SELECTION_PREFIX_JSON,
            &built_cohort.prefix_descriptor_path,
        ),
        (COHORT_PREFIX_DB_EVENTS_JSONL, &built_cohort.prefix_path),
        (
            COHORT_CATALOG_UNBOUND_DB_EVENTS_JSONL,
            &built_cohort.catalog_unbound_path,
        ),
        (COHORT_MARKET_EVIDENCE_JSON, &built_cohort.evidence_path),
        (COHORT_MARKET_METADATA_JSON, &built_cohort.metadata_path),
        (COHORT_REPRESENTATIVENESS_JSON, &representativeness_path),
        (MARKETS_JSONL, &market_path),
        (DB_EVENTS_JSONL, &db_event_path),
        (DB_EXPORT_CHECKPOINT, &db_checkpoint_path),
        (ENRICHED_DB_EVENTS_JSONL, &enriched_path),
        (PROGRAM_SIGNATURES_JSONL, &signature_path),
        (SIGNATURE_CHECKPOINT, &signature_checkpoint_path),
        (RPC_CHECKPOINT, &rpc_checkpoint_path),
        (
            RPC_COMPOSITION_CALIBRATION_JSON,
            &rpc_composition_calibration_path,
        ),
        (BLOCK_ARTIFACTS_JSON, &block_artifacts_path),
        (DECODED_TRANSACTIONS_JSONL, &decoded_transactions_path),
        (DECODED_INSTRUCTIONS_JSONL, &decoded_instructions_path),
        (BLOCK_DECODE_FAILURES_JSONL, &block_decode_failures_path),
        (
            TRANSACTION_DECODE_FAILURES_JSONL,
            &transaction_decode_failures_path,
        ),
        (EXECUTION_PROVENANCE_JSONL, &execution_provenance_path),
        (DECODER_REVISIONS_TOML, &decoder_registry_path),
        (
            DATABASE_HISTORICAL_PRICES_JSONL,
            &database_historical_price_path,
        ),
        (HISTORICAL_PRICES_JSONL, &historical_price_path),
        (PRICE_CHECKPOINT, &historical_price_checkpoint_path),
        (CANONICAL_OPERATIONS_JSONL, &canonical_operations_path),
        (CANONICAL_EVENTS_JSONL, &canonical_events_path),
        (CONFIG_HISTORY_JSONL, &config_history_path),
        (NORMALIZATION_GAPS_JSONL, &normalization_gaps_path),
        (TRANSFER_FEE_PROVENANCE_JSON, &transfer_fee_provenance_path),
    ])?;
    if let Some(registry_path) = transfer_fee_registry_path.as_deref() {
        artifacts.insert(
            TRANSFER_FEE_REGISTRY_JSON.to_owned(),
            sha256_file(registry_path)?,
        );
    }
    if let Some((path, _)) = &archival_block_cache_import_path {
        artifacts.insert(
            ARCHIVAL_BLOCK_CACHE_IMPORT_JSON.to_owned(),
            sha256_file(path)?,
        );
    }
    if historical_price_sidecar_receipt.is_some() {
        artifacts.insert(
            HISTORICAL_PRICE_SIDECAR_RECEIPT_JSON.to_owned(),
            sha256_file(&historical_price_sidecar_receipt_path)?,
        );
    }
    for (name, path) in &transfer_fee_support_artifacts {
        artifacts.insert(name.clone(), sha256_file(path)?);
    }
    for (name, path) in &parquet_artifacts {
        artifacts.insert(name.clone(), sha256_file(path)?);
    }
    let inventory_path = context.run_dir().join(EXTRACTION_INVENTORY);
    let inventory = ExtractionInventoryV1 {
        schema_version: 7,
        run_id: context.run_id().to_owned(),
        cutoff_slot: context.cutoff_slot(),
        cutoff_block_time_unix_seconds: cutoff_block_time,
        program_id,
        scope_mode: config.scope.mode,
        cohort_selection_sha256: built_cohort.artifact.artifact_sha256.clone(),
        cohort_representativeness_sha256: representativeness.artifact_sha256.clone(),
        selected_markets: extraction_targets.markets.clone(),
        selected_market_count: u64::try_from(extraction_targets.markets.len())?,
        earliest_successful_program_slot: Some(earliest_successful_program_slot),
        markets_exported: market_rows.len() as u64,
        database_event_rows: db_checkpoint.rows_exported,
        database_distinct_signatures: db_signatures.len() as u64,
        catalog_unbound_database_event_rows: built_cohort.catalog_unbound_all_window_db_event_rows,
        catalog_unbound_prefix_database_event_rows: built_cohort
            .catalog_unbound_prefix_db_event_rows,
        catalog_unbound_market_count: built_cohort.catalog_unbound_market_count,
        catalog_unbound_prefix_market_count: built_cohort.catalog_unbound_prefix_market_count,
        selected_database_event_rows,
        selected_database_distinct_signatures: u64::try_from(selected_db_signatures.len())?,
        program_signatures_observed: signature_checkpoint.rows_observed,
        successful_program_signatures_at_or_before_cutoff: successful_signatures.len() as u64,
        successful_program_signatures_without_database_rows,
        targeted_successful_program_signatures: u64::try_from(target_signatures.len())?,
        signature_inventory_complete: signature_checkpoint.certified_complete,
        rpc_composition_calibration_blocks: u64::try_from(rpc_composition_calibration.rows.len())?,
        rpc_composition_calibration_sha256: rpc_composition_calibration.artifact_sha256,
        expected_archival_blocks: target_slots.len() as u64,
        fetched_archival_blocks: rpc_checkpoint.completed_slots.len() as u64,
        unresolved_archival_blocks: rpc_checkpoint.failures.len() as u64,
        archival_block_cache_import_sha256: archival_block_cache_import
            .as_ref()
            .map(|receipt| receipt.artifact_sha256.clone()),
        archival_block_cache_blocks: archival_block_cache_import
            .as_ref()
            .map_or(0, |receipt| receipt.validated_blocks),
        archival_block_cache_bytes: archival_block_cache_import
            .as_ref()
            .map_or(0, |receipt| receipt.validated_compressed_bytes),
        archival_block_rpc_requests_avoided: archival_block_cache_import
            .as_ref()
            .map_or(0, |receipt| receipt.composed_rpc_requests_avoided),
        block_artifacts_checksummed: decode_inventory.block_artifacts,
        successful_signatures_matched_in_blocks: matched_signatures.len() as u64,
        decoded_program_transactions: decode_inventory.program_transactions,
        decoded_state_changing_transactions: decode_inventory.state_changing_transactions,
        decoded_program_instructions: decode_inventory.program_instructions,
        expected_state_changing_instructions: decode_inventory.expected_state_changing_instructions,
        decoded_state_changing_instructions: decode_inventory.decoded_state_changing_instructions,
        unclassified_target_instructions: decode_inventory.unclassified_target_instructions,
        transactions_with_incomplete_instruction_inventory: decode_inventory
            .transactions_with_incomplete_instruction_inventory,
        transactions_with_incomplete_cpi_provenance: decode_inventory
            .transactions_with_incomplete_cpi_provenance,
        unknown_program_instructions: decode_inventory.unknown_program_instructions,
        block_decode_failures: decode_inventory.block_decode_failures,
        transaction_decode_failures: decode_inventory.transaction_decode_failures,
        decode_failures: decode_inventory
            .block_decode_failures
            .saturating_add(decode_inventory.transaction_decode_failures),
        decoder_registry_headline_verified: decoder_registry.is_headline_verified(),
        canonical_operations: decode_inventory.canonical_operations,
        canonical_events: decode_inventory.canonical_events,
        configuration_epochs: decode_inventory.configuration_epochs,
        normalization_gaps: decode_inventory.normalization_gaps,
        database_historical_price_marks: historical_price_checkpoint.rows_exported,
        historical_price_sidecar_marks: historical_price_sidecar_receipt
            .as_ref()
            .map_or(0, |receipt| receipt.sidecar_historical_price_rows),
        historical_price_sidecar_receipt_sha256: historical_price_sidecar_receipt
            .as_ref()
            .map(|receipt| receipt.receipt_sha256.clone()),
        historical_price_marks: historical_price_sidecar_receipt
            .as_ref()
            .map_or(historical_price_checkpoint.rows_exported, |receipt| {
                receipt.merged_historical_price_rows
            }),
        historical_price_window_start_unix_seconds: historical_price_window_start,
        historical_price_window_end_unix_seconds_exclusive: historical_price_window_end,
        database_events_enriched: enriched_count,
        database_events_missing_authoritative_enrichment: enrichment_failures,
        database_events_missing_instruction_path: missing_instruction_path,
        table_coverage,
        sources: vec![database_source, rpc_source],
        source_fingerprints,
        artifacts: artifacts.clone(),
    };
    write_json(&inventory_path, &inventory)?;
    artifacts.insert(
        EXTRACTION_INVENTORY.to_owned(),
        sha256_file(&inventory_path)?,
    );
    Ok(artifacts)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DecodeInventory {
    block_artifacts: u64,
    program_transactions: u64,
    state_changing_transactions: u64,
    program_instructions: u64,
    expected_state_changing_instructions: u64,
    decoded_state_changing_instructions: u64,
    unclassified_target_instructions: u64,
    transactions_with_incomplete_instruction_inventory: u64,
    transactions_with_incomplete_cpi_provenance: u64,
    unknown_program_instructions: u64,
    block_decode_failures: u64,
    transaction_decode_failures: u64,
    canonical_operations: u64,
    canonical_events: u64,
    configuration_epochs: u64,
    normalization_gaps: u64,
}

/// Operator-facing result for a deterministic local re-decode of an already
/// proof-bound block cache. This performs no database or RPC calls and is used
/// to iterate on historical decoder/normalizer exactness without refetching
/// archival evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedRedecodeSummaryV1 {
    pub schema_version: u16,
    pub completed_slots: u64,
    pub program_transactions: u64,
    pub expected_state_changing_instructions: u64,
    pub decoded_state_changing_instructions: u64,
    pub transaction_decode_failures: u64,
    pub canonical_operations: u64,
    pub canonical_events: u64,
    pub normalization_gaps: u64,
}

pub fn redecode_cached_history(
    data_dir: &Path,
    program_id: &str,
    decoder_registry_path: &Path,
) -> Result<CachedRedecodeSummaryV1> {
    let checkpoint_path = data_dir.join(RPC_CHECKPOINT);
    let checkpoint: RpcCheckpoint = serde_json::from_slice(
        &fs::read(&checkpoint_path)
            .with_context(|| format!("reading {}", checkpoint_path.display()))?,
    )
    .with_context(|| format!("decoding {}", checkpoint_path.display()))?;
    if checkpoint.completed_slots.is_empty() || !checkpoint.failures.is_empty() {
        bail!("cached re-decode requires a nonempty, gap-free RPC checkpoint");
    }
    let registry: DecoderRegistryV1 = toml::from_str(
        &fs::read_to_string(decoder_registry_path)
            .with_context(|| format!("reading {}", decoder_registry_path.display()))?,
    )
    .with_context(|| format!("decoding {}", decoder_registry_path.display()))?;
    let start = *checkpoint
        .completed_slots
        .first()
        .context("cached RPC checkpoint has no first slot")?;
    let end = *checkpoint
        .completed_slots
        .last()
        .context("cached RPC checkpoint has no last slot")?;
    registry
        .validate_complete_range(start, end)
        .context("decoder registry does not cover the cached slot range")?;
    let inventory =
        decode_archival_history(data_dir, &checkpoint, program_id, &registry, start, end)?;
    Ok(CachedRedecodeSummaryV1 {
        schema_version: 1,
        completed_slots: u64::try_from(checkpoint.completed_slots.len())?,
        program_transactions: inventory.program_transactions,
        expected_state_changing_instructions: inventory.expected_state_changing_instructions,
        decoded_state_changing_instructions: inventory.decoded_state_changing_instructions,
        transaction_decode_failures: inventory.transaction_decode_failures,
        canonical_operations: inventory.canonical_operations,
        canonical_events: inventory.canonical_events,
        normalization_gaps: inventory.normalization_gaps,
    })
}

fn decode_archival_history(
    data_dir: &Path,
    checkpoint: &RpcCheckpoint,
    program_id: &str,
    registry: &DecoderRegistryV1,
    finalized_slot_start: u64,
    finalized_slot_end: u64,
) -> Result<DecodeInventory> {
    let block_artifacts_path = data_dir.join(BLOCK_ARTIFACTS_JSON);
    let decoded_transactions_path = data_dir.join(DECODED_TRANSACTIONS_JSONL);
    let decoded_instructions_path = data_dir.join(DECODED_INSTRUCTIONS_JSONL);
    let block_failures_path = data_dir.join(BLOCK_DECODE_FAILURES_JSONL);
    let transaction_failures_path = data_dir.join(TRANSACTION_DECODE_FAILURES_JSONL);
    let execution_provenance_path = data_dir.join(EXECUTION_PROVENANCE_JSONL);
    let canonical_operations_path = data_dir.join(CANONICAL_OPERATIONS_JSONL);
    let canonical_events_path = data_dir.join(CANONICAL_EVENTS_JSONL);
    let config_history_path = data_dir.join(CONFIG_HISTORY_JSONL);
    let normalization_gaps_path = data_dir.join(NORMALIZATION_GAPS_JSONL);
    let transaction_partial = partial_path(&decoded_transactions_path);
    let instruction_partial = partial_path(&decoded_instructions_path);
    let block_failure_partial = partial_path(&block_failures_path);
    let transaction_failure_partial = partial_path(&transaction_failures_path);
    let execution_provenance_partial = partial_path(&execution_provenance_path);
    let operation_partial = partial_path(&canonical_operations_path);
    let event_partial = partial_path(&canonical_events_path);
    let config_partial = partial_path(&config_history_path);
    let normalization_gap_partial = partial_path(&normalization_gaps_path);
    let mut transaction_writer = BufWriter::new(File::create(&transaction_partial)?);
    let mut instruction_writer = BufWriter::new(File::create(&instruction_partial)?);
    let mut block_failure_writer = BufWriter::new(File::create(&block_failure_partial)?);
    let mut transaction_failure_writer =
        BufWriter::new(File::create(&transaction_failure_partial)?);
    let mut execution_provenance_writer =
        BufWriter::new(File::create(&execution_provenance_partial)?);
    let mut operation_writer = BufWriter::new(File::create(&operation_partial)?);
    let mut event_writer = BufWriter::new(File::create(&event_partial)?);
    let mut config_writer = BufWriter::new(File::create(&config_partial)?);
    let mut normalization_gap_writer = BufWriter::new(File::create(&normalization_gap_partial)?);
    let mut inventory = DecodeInventory::default();
    let mut block_checksums = Vec::with_capacity(checkpoint.completed_slots.len());
    let mut normalizer = NormalizationStateV1::new("mainnet-beta", program_id);

    for slot in &checkpoint.completed_slots {
        let path = block_path(data_dir, *slot);
        let bytes = fs::read(&path)
            .with_context(|| format!("reading archived block artifact {}", path.display()))?;
        let block = decode_archived_block_cache(&bytes)
            .with_context(|| format!("decoding archived block {}", slot))?;
        block_checksums.push(checksum_archived_block_artifact(
            &block,
            block_logical_name(*slot),
            &bytes,
        ));
        inventory.block_artifacts = inventory.block_artifacts.saturating_add(1);
        match decode_archived_block(&block, program_id, registry) {
            Ok(mut outcome) => {
                if !outcome.is_internally_consistent() {
                    bail!("block decoder produced an internally inconsistent outcome");
                }
                inventory.expected_state_changing_instructions = inventory
                    .expected_state_changing_instructions
                    .saturating_add(outcome.state_changing_instruction_coverage.expected);
                inventory.decoded_state_changing_instructions = inventory
                    .decoded_state_changing_instructions
                    .saturating_add(outcome.state_changing_instruction_coverage.decoded);
                inventory.unclassified_target_instructions =
                    inventory.unclassified_target_instructions.saturating_add(
                        outcome
                            .state_changing_instruction_coverage
                            .unclassified_target_instructions,
                    );
                inventory.transactions_with_incomplete_instruction_inventory = inventory
                    .transactions_with_incomplete_instruction_inventory
                    .saturating_add(
                        outcome
                            .state_changing_instruction_coverage
                            .transactions_with_incomplete_inventory,
                    );
                for failure in &outcome.transaction_failures {
                    write_json_line(&mut transaction_failure_writer, failure)?;
                }
                inventory.transaction_decode_failures = inventory
                    .transaction_decode_failures
                    .saturating_add(outcome.transaction_failures.len() as u64);

                let mut provenance_by_transaction = BTreeMap::new();
                for provenance in outcome.execution_provenance.drain(..) {
                    if !provenance.is_complete() {
                        inventory.transactions_with_incomplete_cpi_provenance = inventory
                            .transactions_with_incomplete_cpi_provenance
                            .saturating_add(1);
                    }
                    write_json_line(&mut execution_provenance_writer, &provenance)?;
                    provenance_by_transaction.insert(
                        (
                            provenance.slot,
                            provenance.transaction_index,
                            provenance.signature.clone(),
                        ),
                        provenance,
                    );
                }
                for transaction in outcome.transactions {
                    let provenance = provenance_by_transaction
                        .remove(&(
                            transaction.slot,
                            transaction.transaction_index,
                            transaction.signature.clone(),
                        ))
                        .context("decoded transaction lacks execution provenance")?;
                    record_decoded_transaction(
                        &transaction,
                        &mut transaction_writer,
                        &mut instruction_writer,
                        &mut inventory,
                    )?;
                    let batch = normalizer
                        .push_transaction_with_provenance(transaction, &provenance)
                        .context("normalizing decoded V1 transaction")?;
                    for operation in batch.operations {
                        write_json_line(&mut operation_writer, &operation)?;
                        inventory.canonical_operations =
                            inventory.canonical_operations.saturating_add(1);
                    }
                    for event in batch.events {
                        write_json_line(&mut event_writer, &event)?;
                        inventory.canonical_events = inventory.canonical_events.saturating_add(1);
                    }
                    for gap in batch.gaps {
                        write_json_line(&mut normalization_gap_writer, &gap)?;
                        inventory.normalization_gaps =
                            inventory.normalization_gaps.saturating_add(1);
                    }
                }
                if !provenance_by_transaction.is_empty() {
                    bail!("execution provenance has no decoded transaction");
                }
            }
            Err(error) => {
                write_json_line(
                    &mut block_failure_writer,
                    &DecodeFailureV1 {
                        schema_version: 1,
                        slot: *slot,
                        category: decode_error_category(&error),
                    },
                )?;
                inventory.block_decode_failures = inventory.block_decode_failures.saturating_add(1);
            }
        }
        if inventory.block_artifacts % 10_000 == 0 {
            eprintln!(
                "extract: archived blocks decoded={}",
                inventory.block_artifacts
            );
        }
    }
    let block_manifest =
        BlockArtifactManifestV1::new(finalized_slot_start, finalized_slot_end, block_checksums)
            .context("constructing block artifact manifest")?;
    write_json(&block_artifacts_path, &block_manifest)?;
    let mut configurations = normalizer.finish_configurations();
    configurations.extend(decoder_config_versions(registry));
    configurations.sort_by(|left, right| {
        left.valid_from
            .cmp(&right.valid_from)
            .then_with(|| {
                config_parquet_scope_rank(&left.scope).cmp(&config_parquet_scope_rank(&right.scope))
            })
            .then_with(|| left.entity_address.cmp(&right.entity_address))
            .then_with(|| left.config_id.cmp(&right.config_id))
    });
    for config in configurations {
        write_json_line(&mut config_writer, &config)?;
        inventory.configuration_epochs = inventory.configuration_epochs.saturating_add(1);
    }
    transaction_writer.flush()?;
    instruction_writer.flush()?;
    block_failure_writer.flush()?;
    transaction_failure_writer.flush()?;
    execution_provenance_writer.flush()?;
    operation_writer.flush()?;
    event_writer.flush()?;
    config_writer.flush()?;
    normalization_gap_writer.flush()?;
    fs::rename(transaction_partial, decoded_transactions_path)?;
    fs::rename(instruction_partial, decoded_instructions_path)?;
    fs::rename(block_failure_partial, block_failures_path)?;
    fs::rename(transaction_failure_partial, transaction_failures_path)?;
    fs::rename(execution_provenance_partial, execution_provenance_path)?;
    fs::rename(operation_partial, canonical_operations_path)?;
    fs::rename(event_partial, canonical_events_path)?;
    fs::rename(config_partial, config_history_path)?;
    fs::rename(normalization_gap_partial, normalization_gaps_path)?;
    Ok(inventory)
}

fn decoder_config_versions(registry: &DecoderRegistryV1) -> Vec<ConfigVersionV1> {
    registry
        .revisions
        .iter()
        .map(|revision| {
            let valid_from = ChainOrder {
                slot: revision.valid_from_slot,
                transaction_index: 0,
                outer_instruction_index: 0,
                inner_execution_index: None,
                event_sequence: 0,
            };
            let valid_to_exclusive = revision.valid_to_slot_exclusive.map(|slot| ChainOrder {
                slot,
                transaction_index: 0,
                outer_instruction_index: 0,
                inner_execution_index: None,
                event_sequence: 0,
            });
            let mut fields = BTreeMap::from([
                (
                    "source_git_commit".to_owned(),
                    revision.source_git_commit.clone(),
                ),
                ("idl_sha256".to_owned(), revision.idl_sha256.clone()),
                (
                    "candidate_program_binary_sha256".to_owned(),
                    revision.candidate_program_binary_sha256.clone(),
                ),
                ("provisional".to_owned(), revision.provisional.to_string()),
            ]);
            if let Some(deployed_sha256) = &revision.program_binary_sha256 {
                fields.insert("program_binary_sha256".to_owned(), deployed_sha256.clone());
            }
            ConfigVersionV1 {
                config_id: sha256_hex(
                    format!("program-binary:{}", revision.decoder_version_id).as_bytes(),
                ),
                scope: ConfigScope::ProgramBinary,
                entity_address: "__program_binary__".to_owned(),
                valid_from,
                valid_to_exclusive,
                source_signature: format!("registry:{}", revision.decoder_version_id),
                decoder_version_id: revision.decoder_version_id.clone(),
                fields,
            }
        })
        .collect()
}

fn config_parquet_scope_rank(scope: &ConfigScope) -> u8 {
    match scope {
        ConfigScope::Pair | ConfigScope::ProtocolRevenue => 0,
        ConfigScope::RevenueRecipients => 1,
        ConfigScope::ReduceOnly => 2,
        ConfigScope::RateModel => 3,
        ConfigScope::ProgramBinary => 4,
    }
}

fn record_decoded_transaction(
    transaction: &DecodedTransactionV1,
    transaction_writer: &mut BufWriter<File>,
    instruction_writer: &mut BufWriter<File>,
    inventory: &mut DecodeInventory,
) -> Result<()> {
    inventory.program_transactions = inventory.program_transactions.saturating_add(1);
    inventory.program_instructions = inventory
        .program_instructions
        .saturating_add(u64::from(transaction.program_instruction_count));
    inventory.unknown_program_instructions = inventory.unknown_program_instructions.saturating_add(
        u64::try_from(transaction.unknown_program_instruction_hashes.len())
            .context("unknown instruction count overflow")?,
    );
    if transaction.is_state_changing() {
        inventory.state_changing_transactions =
            inventory.state_changing_transactions.saturating_add(1);
    }
    for instruction in &transaction.instructions {
        write_json_line(instruction_writer, instruction)?;
    }
    write_json_line(transaction_writer, transaction)
}

fn decode_error_category(error: &crate::decode::DecodeError) -> String {
    use crate::decode::DecodeError;
    match error {
        DecodeError::SchemaVersion(_) => "schema_version",
        DecodeError::NoDecoder(_) => "no_decoder",
        DecodeError::OverlappingDecoders(_) => "overlapping_decoders",
        DecodeError::InvalidRange => "invalid_range",
        DecodeError::InvalidProgramId => "invalid_program_id",
        DecodeError::MissingBlockTime(_) => "missing_block_time",
        DecodeError::InvalidTransactionShape => "invalid_transaction_shape",
        DecodeError::ParsedTargetInstruction => "parsed_target_instruction",
        DecodeError::IndexOverflow => "index_overflow",
        DecodeError::AccountIndex => "account_index",
        DecodeError::InvalidBase58 => "invalid_base58",
        DecodeError::InvalidAccountKey => "invalid_account_key",
        DecodeError::DecodedSerialization => "decoded_serialization",
        DecodeError::InvalidTokenBalance => "invalid_token_balance",
        DecodeError::ExactTokenCpiEvidence(_) => "exact_token_cpi_evidence",
    }
    .to_owned()
}

trait ProgramSignatureHistoryRpc: Send + Sync {
    fn signature_page<'a>(
        &'a self,
        program_id: &'a str,
        before: Option<&'a str>,
        limit: u16,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RpcResponseProofV1<Vec<ProgramSignatureV1>>, RpcFailureKind>>
                + Send
                + 'a,
        >,
    >;
}

impl ProgramSignatureHistoryRpc for HttpArchivalRpc {
    fn signature_page<'a>(
        &'a self,
        program_id: &'a str,
        before: Option<&'a str>,
        limit: u16,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RpcResponseProofV1<Vec<ProgramSignatureV1>>, RpcFailureKind>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.signatures_for_address_page_with_proof(program_id, before, limit)
                .await
        })
    }
}

async fn extract_program_signatures<R: ProgramSignatureHistoryRpc>(
    rpc: &R,
    run_id: &str,
    program_id: &str,
    cutoff_slot: u64,
    provider: &SanitizedSource,
    data_dir: &Path,
) -> Result<(SignatureCheckpointV1, Vec<ProgramSignatureV1>)> {
    if run_id.trim().is_empty()
        || !valid_signature_pubkey(program_id)
        || !valid_signature_sha256(&provider.secret_fingerprint_sha256)
    {
        bail!("invalid program-signature crawl binding");
    }
    fs::create_dir_all(data_dir)?;
    let checkpoint_path = data_dir.join(SIGNATURE_CHECKPOINT);
    let output_path = data_dir.join(PROGRAM_SIGNATURES_JSONL);
    let page_dir = data_dir.join(PROGRAM_SIGNATURE_PAGE_DIR);
    fs::create_dir_all(&page_dir)?;
    let mut checkpoint =
        load_or_migrate_signature_checkpoint(run_id, program_id, cutoff_slot, provider, data_dir)?;
    checkpoint.validate_identity(
        run_id,
        cutoff_slot,
        program_id,
        &provider.secret_fingerprint_sha256,
    )?;
    let mut rows = validate_committed_signature_pages(&checkpoint, &page_dir)?;
    verify_signature_output_binding(&checkpoint, &output_path)?;

    if checkpoint.explicit_empty_terminal_page {
        if checkpoint.certified_complete {
            return Ok((checkpoint, successful_signature_rows(&rows, cutoff_slot)));
        }
        checkpoint = certify_signature_crawl(checkpoint, &rows, data_dir)?;
        return Ok((checkpoint, successful_signature_rows(&rows, cutoff_slot)));
    }

    let mut seen = rows
        .iter()
        .map(|row| row.signature.clone())
        .collect::<BTreeSet<_>>();
    let mut previous_slot = rows.last().map(|row| row.slot);
    loop {
        let page_index = checkpoint.pages_completed;
        let page_path = signature_page_path(&page_dir, page_index);
        let artifact = if page_path.exists() {
            let artifact: ProgramSignaturePageArtifactV2 = read_json(&page_path)?;
            artifact.validate()?;
            artifact
        } else {
            let response = match rpc
                .signature_page(
                    program_id,
                    checkpoint.before.as_deref(),
                    PROGRAM_SIGNATURE_PAGE_LIMIT,
                )
                .await
            {
                Ok(response) => response,
                Err(kind) => {
                    checkpoint.failure_kind = Some(format!("rpc_{kind:?}").to_ascii_lowercase());
                    materialize_uncertified_signature_rows(&mut checkpoint, &rows, &output_path)?;
                    checkpoint = checkpoint.seal()?;
                    write_json(&checkpoint_path, &checkpoint)?;
                    return Ok((checkpoint, successful_signature_rows(&rows, cutoff_slot)));
                }
            };
            let artifact = ProgramSignaturePageArtifactV2 {
                schema_version: PROGRAM_SIGNATURE_CRAWL_SCHEMA_VERSION,
                run_id: run_id.to_owned(),
                cutoff_slot,
                program_id: program_id.to_owned(),
                provider_fingerprint_sha256: provider.secret_fingerprint_sha256.clone(),
                commitment: "finalized".to_owned(),
                page_index,
                request_before: checkpoint.before.clone(),
                request_limit: PROGRAM_SIGNATURE_PAGE_LIMIT,
                previous_page_artifact_sha256: checkpoint.last_page_artifact_sha256.clone(),
                raw_response_sha256: response.raw_response_sha256,
                signatures: response.value,
                artifact_sha256: String::new(),
            }
            .seal()?;
            artifact.validate()?;
            write_json(&page_path, &artifact)?;
            artifact
        };
        validate_signature_page_binding(&artifact, &checkpoint)?;
        for row in &artifact.signatures {
            if previous_slot.is_some_and(|slot| row.slot > slot)
                || !seen.insert(row.signature.clone())
            {
                bail!("program-signature crawl regressed or duplicated across pages");
            }
            previous_slot = Some(row.slot);
            rows.push(row.clone());
        }
        let page_bytes = fs::read(&page_path)?;
        let page_checksum = ArtifactChecksum::from_bytes(
            format!("{PROGRAM_SIGNATURE_PAGE_DIR}/{page_index:08}.json"),
            &page_bytes,
        );
        checkpoint.pages_completed = checkpoint.pages_completed.saturating_add(1);
        checkpoint.rows_observed = checkpoint
            .rows_observed
            .saturating_add(artifact.signatures.len() as u64);
        checkpoint.rows_at_or_before_cutoff = checkpoint.rows_at_or_before_cutoff.saturating_add(
            artifact
                .signatures
                .iter()
                .filter(|row| row.slot <= cutoff_slot)
                .count() as u64,
        );
        if let Some(last) = artifact.signatures.last() {
            checkpoint.before = Some(last.signature.clone());
        }
        checkpoint.explicit_empty_terminal_page = artifact.signatures.is_empty();
        checkpoint.certified_complete = false;
        checkpoint.failure_kind = None;
        checkpoint.last_page_artifact_sha256 = Some(page_checksum.sha256.clone());
        checkpoint.page_artifacts.push(page_checksum);
        checkpoint = checkpoint.seal()?;
        write_json(&checkpoint_path, &checkpoint)?;
        if checkpoint.pages_completed % 25 == 0 || checkpoint.explicit_empty_terminal_page {
            eprintln!(
                "extract: program signatures observed={} explicit_empty_terminal={}",
                checkpoint.rows_observed, checkpoint.explicit_empty_terminal_page
            );
        }
        if checkpoint.explicit_empty_terminal_page {
            reject_signature_pages_after(&page_dir, page_index)?;
            checkpoint = certify_signature_crawl(checkpoint, &rows, data_dir)?;
            break;
        }
    }
    Ok((checkpoint, successful_signature_rows(&rows, cutoff_slot)))
}

fn load_or_migrate_signature_checkpoint(
    run_id: &str,
    program_id: &str,
    cutoff_slot: u64,
    provider: &SanitizedSource,
    data_dir: &Path,
) -> Result<SignatureCheckpointV1> {
    let checkpoint_path = data_dir.join(SIGNATURE_CHECKPOINT);
    let output_path = data_dir.join(PROGRAM_SIGNATURES_JSONL);
    if !checkpoint_path.exists() {
        if output_path.exists() {
            bail!("unbound program-signature JSONL exists without a checkpoint");
        }
        return SignatureCheckpointV1::new(
            run_id,
            cutoff_slot,
            program_id,
            &provider.secret_fingerprint_sha256,
            None,
        );
    }
    let checkpoint_bytes = fs::read(&checkpoint_path)?;
    let value: JsonValue = serde_json::from_slice(&checkpoint_bytes)?;
    if value.get("schema_version").is_some() {
        return Ok(serde_json::from_value(value)?);
    }
    let legacy: LegacySignatureCheckpointV1 = serde_json::from_value(value)?;
    if legacy
        .before
        .as_ref()
        .is_some_and(|value| !valid_signature_value(value))
        || (legacy.rows_observed > 0 && legacy.pages_completed == 0)
    {
        bail!("legacy program-signature checkpoint inventory is invalid");
    }
    let output_bytes = fs::read(&output_path)
        .context("legacy signature checkpoint exists without its JSONL reference")?;
    let output_rows = read_and_validate_signature_rows(&output_path, cutoff_slot)?;
    let output_last_signature = output_rows.last().map(|row| row.signature.clone());
    let output_row_count = read_json_lines::<ProgramSignatureV1>(&output_path)?.len() as u64;
    let backup_path = data_dir.join(LEGACY_SIGNATURE_CHECKPOINT);
    preserve_exact_bytes(&backup_path, &checkpoint_bytes)?;
    let reference = LegacySignatureReferenceV1 {
        checkpoint_artifact: ArtifactChecksum::from_bytes(
            LEGACY_SIGNATURE_CHECKPOINT,
            &checkpoint_bytes,
        ),
        output_artifact: ArtifactChecksum::from_bytes(PROGRAM_SIGNATURES_JSONL, &output_bytes),
        claimed_before: legacy.before,
        claimed_pages_completed: legacy.pages_completed,
        claimed_rows_observed: legacy.rows_observed,
        claimed_complete: legacy.complete,
        output_rows: output_row_count,
        output_unique_rows: output_rows.len() as u64,
        output_last_signature,
    };
    reference.validate()?;
    let checkpoint = SignatureCheckpointV1::new(
        run_id,
        cutoff_slot,
        program_id,
        &provider.secret_fingerprint_sha256,
        Some(reference),
    )?;
    write_json(&checkpoint_path, &checkpoint)?;
    Ok(checkpoint)
}

fn validate_committed_signature_pages(
    checkpoint: &SignatureCheckpointV1,
    page_dir: &Path,
) -> Result<Vec<ProgramSignatureV1>> {
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    let mut previous_slot = None;
    let mut before: Option<String> = None;
    let mut previous_artifact_sha256: Option<String> = None;
    let mut terminal = false;
    for (index, checksum) in checkpoint.page_artifacts.iter().enumerate() {
        let page_index = index as u64;
        let expected_name = format!("{PROGRAM_SIGNATURE_PAGE_DIR}/{page_index:08}.json");
        if checksum.logical_name != expected_name {
            bail!("program-signature checkpoint page name is not canonical");
        }
        let path = signature_page_path(page_dir, page_index);
        let bytes = fs::read(&path).with_context(|| {
            format!(
                "missing committed program-signature page {}",
                path.display()
            )
        })?;
        if !checksum.matches_bytes(&bytes) {
            bail!("committed program-signature page checksum mismatch");
        }
        let artifact: ProgramSignaturePageArtifactV2 = serde_json::from_slice(&bytes)?;
        artifact.validate()?;
        if artifact.run_id != checkpoint.run_id
            || artifact.cutoff_slot != checkpoint.cutoff_slot
            || artifact.program_id != checkpoint.program_id
            || artifact.provider_fingerprint_sha256 != checkpoint.provider_fingerprint_sha256
            || artifact.page_index != page_index
            || artifact.request_before != before
            || artifact.previous_page_artifact_sha256 != previous_artifact_sha256
        {
            bail!("committed program-signature page breaks the sealed request chain");
        }
        if terminal {
            bail!("program-signature page exists after the explicit empty terminal page");
        }
        for row in &artifact.signatures {
            if previous_slot.is_some_and(|slot| row.slot > slot)
                || !seen.insert(row.signature.clone())
            {
                bail!("committed program-signature pages regress or duplicate rows");
            }
            previous_slot = Some(row.slot);
            rows.push(row.clone());
        }
        if let Some(last) = artifact.signatures.last() {
            before = Some(last.signature.clone());
        } else {
            terminal = true;
        }
        previous_artifact_sha256 = Some(checksum.sha256.clone());
    }
    if checkpoint.before != before
        || checkpoint.rows_observed != rows.len() as u64
        || checkpoint.rows_at_or_before_cutoff
            != rows
                .iter()
                .filter(|row| row.slot <= checkpoint.cutoff_slot)
                .count() as u64
        || checkpoint.explicit_empty_terminal_page != terminal
    {
        bail!("program-signature checkpoint counters disagree with its page chain");
    }
    if terminal {
        reject_signature_pages_after(page_dir, checkpoint.pages_completed.saturating_sub(1))?;
    }
    Ok(rows)
}

fn validate_signature_page_binding(
    artifact: &ProgramSignaturePageArtifactV2,
    checkpoint: &SignatureCheckpointV1,
) -> Result<()> {
    if artifact.run_id != checkpoint.run_id
        || artifact.cutoff_slot != checkpoint.cutoff_slot
        || artifact.program_id != checkpoint.program_id
        || artifact.provider_fingerprint_sha256 != checkpoint.provider_fingerprint_sha256
        || artifact.commitment != checkpoint.commitment
        || artifact.request_limit != checkpoint.request_limit
        || artifact.page_index != checkpoint.pages_completed
        || artifact.request_before != checkpoint.before
        || artifact.previous_page_artifact_sha256 != checkpoint.last_page_artifact_sha256
    {
        bail!("program-signature page does not match its sealed request");
    }
    Ok(())
}

fn certify_signature_crawl(
    mut checkpoint: SignatureCheckpointV1,
    rows: &[ProgramSignatureV1],
    data_dir: &Path,
) -> Result<SignatureCheckpointV1> {
    if !checkpoint.explicit_empty_terminal_page {
        bail!("program-signature crawl cannot certify without an explicit empty page");
    }
    let output_path = data_dir.join(PROGRAM_SIGNATURES_JSONL);
    let filtered = rows
        .iter()
        .filter(|row| row.slot <= checkpoint.cutoff_slot)
        .cloned()
        .collect::<Vec<_>>();
    if let Some(reference) = &checkpoint.legacy_reference {
        verify_legacy_signature_reference(reference, data_dir)?;
        let legacy_rows = read_and_validate_signature_rows(&output_path, checkpoint.cutoff_slot)?;
        let agrees = legacy_rows == filtered
            && reference.output_unique_rows == filtered.len() as u64
            && reference.claimed_before == checkpoint.before;
        if !agrees {
            checkpoint.certified_complete = false;
            checkpoint.failure_kind = Some("legacy_reference_mismatch".to_owned());
            checkpoint.output_artifact = Some(reference.output_artifact.clone());
            checkpoint = checkpoint.seal()?;
            write_json(&data_dir.join(SIGNATURE_CHECKPOINT), &checkpoint)?;
            bail!(
                "independent finalized signature recrawl disagrees with the preserved legacy rows or cursor"
            );
        }
        checkpoint.output_artifact = Some(reference.output_artifact.clone());
    } else {
        let encoded = filtered
            .iter()
            .map(serde_json::to_string)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        write_raw_json_lines(&output_path, &encoded)?;
        checkpoint.output_artifact = Some(checksum_signature_path(
            &output_path,
            PROGRAM_SIGNATURES_JSONL,
        )?);
    }
    checkpoint.certified_complete = true;
    checkpoint.failure_kind = None;
    checkpoint = checkpoint.seal()?;
    checkpoint.validate_identity(
        &checkpoint.run_id,
        checkpoint.cutoff_slot,
        &checkpoint.program_id,
        &checkpoint.provider_fingerprint_sha256,
    )?;
    write_json(&data_dir.join(SIGNATURE_CHECKPOINT), &checkpoint)?;
    Ok(checkpoint)
}

fn materialize_uncertified_signature_rows(
    checkpoint: &mut SignatureCheckpointV1,
    rows: &[ProgramSignatureV1],
    output_path: &Path,
) -> Result<()> {
    if checkpoint.legacy_reference.is_some() {
        return Ok(());
    }
    let encoded = rows
        .iter()
        .filter(|row| row.slot <= checkpoint.cutoff_slot)
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    write_raw_json_lines(output_path, &encoded)?;
    checkpoint.output_artifact = Some(checksum_signature_path(
        output_path,
        PROGRAM_SIGNATURES_JSONL,
    )?);
    Ok(())
}

fn verify_signature_output_binding(
    checkpoint: &SignatureCheckpointV1,
    output_path: &Path,
) -> Result<()> {
    if let Some(expected) = &checkpoint.output_artifact {
        let bytes = fs::read(output_path)
            .context("program-signature checkpoint binds a missing JSONL artifact")?;
        if !expected.matches_bytes(&bytes) {
            bail!("program-signature JSONL checksum differs from its checkpoint");
        }
    } else if output_path.exists() {
        bail!("program-signature JSONL exists without a checkpoint checksum binding");
    }
    Ok(())
}

fn verify_legacy_signature_reference(
    reference: &LegacySignatureReferenceV1,
    data_dir: &Path,
) -> Result<()> {
    for (path, expected) in [
        (
            data_dir.join(LEGACY_SIGNATURE_CHECKPOINT),
            &reference.checkpoint_artifact,
        ),
        (
            data_dir.join(PROGRAM_SIGNATURES_JSONL),
            &reference.output_artifact,
        ),
    ] {
        let bytes = fs::read(&path)
            .with_context(|| format!("missing preserved legacy artifact {}", path.display()))?;
        if !expected.matches_bytes(&bytes) {
            bail!("preserved legacy program-signature artifact was modified");
        }
    }
    Ok(())
}

fn read_and_validate_signature_rows(
    path: &Path,
    cutoff_slot: u64,
) -> Result<Vec<ProgramSignatureV1>> {
    let mut unique = Vec::new();
    let mut seen = BTreeSet::new();
    let mut previous_slot = None;
    for row in read_json_lines::<ProgramSignatureV1>(path)? {
        if !valid_signature_value(&row.signature)
            || row.slot > cutoff_slot
            || previous_slot.is_some_and(|slot| row.slot > slot)
        {
            bail!("legacy program-signature JSONL is invalid or not newest-first");
        }
        previous_slot = Some(row.slot);
        if seen.insert(row.signature.clone()) {
            unique.push(row);
        }
    }
    Ok(unique)
}

fn successful_signature_rows(
    rows: &[ProgramSignatureV1],
    cutoff_slot: u64,
) -> Vec<ProgramSignatureV1> {
    let mut successful = rows
        .iter()
        .filter(|row| row.succeeded && row.slot <= cutoff_slot)
        .cloned()
        .collect::<Vec<_>>();
    successful.sort_by_key(|row| (row.slot, row.signature.clone()));
    successful
}

fn reject_signature_pages_after(page_dir: &Path, terminal_index: u64) -> Result<()> {
    for entry in fs::read_dir(page_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            bail!("unexpected directory in program-signature page chain");
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(index) = name
            .strip_suffix(".json")
            .and_then(|value| value.parse::<u64>().ok())
        else {
            bail!("unexpected file in program-signature page chain");
        };
        if index > terminal_index {
            bail!("program-signature page exists after explicit empty terminal page");
        }
    }
    Ok(())
}

fn signature_page_path(page_dir: &Path, page_index: u64) -> PathBuf {
    page_dir.join(format!("{page_index:08}.json"))
}

fn checksum_signature_path(path: &Path, logical_name: &str) -> Result<ArtifactChecksum> {
    Ok(ArtifactChecksum::from_bytes(logical_name, &fs::read(path)?))
}

fn preserve_exact_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        if fs::read(path)? != bytes {
            bail!("preserved legacy signature checkpoint differs from its source");
        }
        return Ok(());
    }
    let partial = partial_path(path);
    let mut file = File::create(&partial)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(partial, path)?;
    Ok(())
}

fn signature_artifact_hash<T: Serialize>(domain: &[u8], value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(hex::encode(digest.finalize()))
}

fn valid_signature_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_signature_value(value: &str) -> bool {
    bs58::decode(value)
        .into_vec()
        .is_ok_and(|bytes| bytes.len() == 64)
}

fn valid_signature_pubkey(value: &str) -> bool {
    bs58::decode(value)
        .into_vec()
        .is_ok_and(|bytes| bytes.len() == 32)
}

#[cfg(test)]
mod program_signature_crawl_tests {
    use std::sync::{atomic::AtomicU64, Arc, Mutex};

    use super::*;

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence =
                TEST_DIRECTORY_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "v1-vs-dusk-program-signatures-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    type PageResult =
        std::result::Result<RpcResponseProofV1<Vec<ProgramSignatureV1>>, RpcFailureKind>;

    #[derive(Clone, Default)]
    struct FakeSignatureRpc {
        pages: Arc<Mutex<BTreeMap<Option<String>, PageResult>>>,
        calls: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl FakeSignatureRpc {
        fn with_pages(pages: BTreeMap<Option<String>, PageResult>) -> Self {
            Self {
                pages: Arc::new(Mutex::new(pages)),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl ProgramSignatureHistoryRpc for FakeSignatureRpc {
        fn signature_page<'a>(
            &'a self,
            _program_id: &'a str,
            before: Option<&'a str>,
            _limit: u16,
        ) -> Pin<Box<dyn Future<Output = PageResult> + Send + 'a>> {
            Box::pin(async move {
                let key = before.map(str::to_owned);
                self.calls.lock().unwrap().push(key.clone());
                self.pages
                    .lock()
                    .unwrap()
                    .get(&key)
                    .cloned()
                    .unwrap_or(Err(RpcFailureKind::InvalidResponse))
            })
        }
    }

    fn signature(seed: u8) -> String {
        bs58::encode([seed; 64]).into_string()
    }

    fn program_id() -> String {
        bs58::encode([9_u8; 32]).into_string()
    }

    fn provider() -> SanitizedSource {
        SanitizedSource::from_secret("solana_rpc", "fixture_provider", "fixture-endpoint").unwrap()
    }

    fn row(seed: u8, slot: u64) -> ProgramSignatureV1 {
        ProgramSignatureV1 {
            signature: signature(seed),
            slot,
            succeeded: true,
            rpc_block_time_hint: Some(slot as i64),
        }
    }

    fn proof(seed: char, rows: Vec<ProgramSignatureV1>) -> PageResult {
        Ok(RpcResponseProofV1 {
            raw_response_sha256: seed.to_string().repeat(64),
            value: rows,
        })
    }

    fn pages_then_empty(rows: Vec<ProgramSignatureV1>) -> BTreeMap<Option<String>, PageResult> {
        let cursor = rows.last().unwrap().signature.clone();
        BTreeMap::from([
            (None, proof('a', rows)),
            (Some(cursor), proof('b', Vec::new())),
        ])
    }

    #[tokio::test]
    async fn short_page_requires_a_separate_explicit_empty_finalized_page() {
        let directory = TestDirectory::new();
        let rows = vec![row(1, 20), row(2, 10)];
        let rpc = FakeSignatureRpc::with_pages(pages_then_empty(rows.clone()));
        let checkpoint = extract_program_signatures(
            &rpc,
            "run-short-page",
            &program_id(),
            20,
            &provider(),
            &directory.0,
        )
        .await
        .unwrap()
        .0;
        assert!(checkpoint.certified_complete);
        assert!(checkpoint.explicit_empty_terminal_page);
        assert_eq!(checkpoint.pages_completed, 2);
        assert_eq!(checkpoint.rows_observed, 2);
        assert_eq!(rpc.calls.lock().unwrap().len(), 2);
        assert_eq!(rpc.calls.lock().unwrap()[0], None);
        assert_eq!(
            read_json_lines::<ProgramSignatureV1>(&directory.0.join(PROGRAM_SIGNATURES_JSONL))
                .unwrap(),
            rows
        );
    }

    #[tokio::test]
    async fn legacy_short_page_claim_is_preserved_and_independently_recrawled() {
        let directory = TestDirectory::new();
        let rows = vec![row(3, 20), row(4, 10)];
        let legacy = LegacySignatureCheckpointV1 {
            before: Some(rows[1].signature.clone()),
            pages_completed: 834,
            rows_observed: 2,
            complete: true,
            failure_kind: None,
        };
        write_json(&directory.0.join(SIGNATURE_CHECKPOINT), &legacy).unwrap();
        let encoded = rows
            .iter()
            .map(serde_json::to_string)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        write_raw_json_lines(&directory.0.join(PROGRAM_SIGNATURES_JSONL), &encoded).unwrap();
        let checkpoint_before = fs::read(directory.0.join(SIGNATURE_CHECKPOINT)).unwrap();
        let output_before = fs::read(directory.0.join(PROGRAM_SIGNATURES_JSONL)).unwrap();
        let rpc = FakeSignatureRpc::with_pages(pages_then_empty(rows));
        let checkpoint = extract_program_signatures(
            &rpc,
            "run-legacy",
            &program_id(),
            20,
            &provider(),
            &directory.0,
        )
        .await
        .unwrap()
        .0;
        assert!(checkpoint.certified_complete);
        assert!(checkpoint.legacy_reference.is_some());
        assert_eq!(rpc.calls.lock().unwrap()[0], None);
        assert_eq!(
            fs::read(directory.0.join(LEGACY_SIGNATURE_CHECKPOINT)).unwrap(),
            checkpoint_before
        );
        assert_eq!(
            fs::read(directory.0.join(PROGRAM_SIGNATURES_JSONL)).unwrap(),
            output_before
        );
    }

    #[tokio::test]
    async fn legacy_mismatch_blocks_without_rewriting_either_evidentiary_state() {
        let directory = TestDirectory::new();
        let legacy_rows = vec![row(5, 20), row(6, 10)];
        let legacy = LegacySignatureCheckpointV1 {
            before: Some(legacy_rows[1].signature.clone()),
            pages_completed: 1,
            rows_observed: 2,
            complete: true,
            failure_kind: None,
        };
        write_json(&directory.0.join(SIGNATURE_CHECKPOINT), &legacy).unwrap();
        let encoded = legacy_rows
            .iter()
            .map(serde_json::to_string)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        write_raw_json_lines(&directory.0.join(PROGRAM_SIGNATURES_JSONL), &encoded).unwrap();
        let output_before = fs::read(directory.0.join(PROGRAM_SIGNATURES_JSONL)).unwrap();
        let fresh_rows = vec![legacy_rows[0].clone(), row(7, 10)];
        let rpc = FakeSignatureRpc::with_pages(pages_then_empty(fresh_rows));
        let error = extract_program_signatures(
            &rpc,
            "run-mismatch",
            &program_id(),
            20,
            &provider(),
            &directory.0,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("disagrees"));
        assert_eq!(
            fs::read(directory.0.join(PROGRAM_SIGNATURES_JSONL)).unwrap(),
            output_before
        );
        let checkpoint: SignatureCheckpointV1 =
            read_json(&directory.0.join(SIGNATURE_CHECKPOINT)).unwrap();
        assert!(checkpoint.explicit_empty_terminal_page);
        assert!(!checkpoint.certified_complete);
        assert_eq!(
            checkpoint.failure_kind.as_deref(),
            Some("legacy_reference_mismatch")
        );
        assert!(signature_page_path(&directory.0.join(PROGRAM_SIGNATURE_PAGE_DIR), 0).exists());
    }

    #[tokio::test]
    async fn resume_uses_the_sealed_cursor_and_tampering_is_rejected() {
        let directory = TestDirectory::new();
        let rows = vec![row(8, 20), row(9, 10)];
        let cursor = rows[1].signature.clone();
        let first_rpc = FakeSignatureRpc::with_pages(BTreeMap::from([
            (None, proof('c', rows)),
            (Some(cursor.clone()), Err(RpcFailureKind::Transport)),
        ]));
        let partial = extract_program_signatures(
            &first_rpc,
            "run-resume",
            &program_id(),
            20,
            &provider(),
            &directory.0,
        )
        .await
        .unwrap()
        .0;
        assert!(!partial.certified_complete);
        assert_eq!(partial.pages_completed, 1);

        let resume_rpc =
            FakeSignatureRpc::with_pages(BTreeMap::from([(Some(cursor), proof('d', Vec::new()))]));
        let complete = extract_program_signatures(
            &resume_rpc,
            "run-resume",
            &program_id(),
            20,
            &provider(),
            &directory.0,
        )
        .await
        .unwrap()
        .0;
        assert!(complete.certified_complete);
        assert_eq!(resume_rpc.calls.lock().unwrap().len(), 1);

        let page_path = signature_page_path(&directory.0.join(PROGRAM_SIGNATURE_PAGE_DIR), 0);
        let mut bytes = fs::read(&page_path).unwrap();
        let position = bytes.iter().position(|byte| *byte == b'a').unwrap();
        bytes[position] = b'e';
        fs::write(&page_path, bytes).unwrap();
        let error = extract_program_signatures(
            &resume_rpc,
            "run-resume",
            &program_id(),
            20,
            &provider(),
            &directory.0,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
    }
}

fn evenly_spaced_calibration_slots(population_slots: &[u64]) -> Vec<u64> {
    if population_slots.len() <= RPC_COMPOSITION_CALIBRATION_SAMPLE_SIZE {
        return population_slots.to_vec();
    }
    let last_index = population_slots.len() - 1;
    (0..RPC_COMPOSITION_CALIBRATION_SAMPLE_SIZE)
        .map(|sample_index| {
            let index = (sample_index * last_index)
                / (RPC_COMPOSITION_CALIBRATION_SAMPLE_SIZE.saturating_sub(1));
            population_slots[index]
        })
        .collect()
}

fn normalized_target_projection_sha256(
    block: &ArchivedBlock,
    expected_signatures: &BTreeSet<String>,
) -> Result<String> {
    let mut transactions = block
        .transactions
        .iter()
        .filter(|transaction| expected_signatures.contains(&transaction.signature))
        .map(|transaction| {
            Ok(serde_json::json!({
                "transaction_index": transaction.transaction_index,
                "signature": transaction.signature,
                "succeeded": transaction.succeeded,
                "transaction": transaction.transaction,
                "meta": normalize_independent_get_transaction_meta(transaction.meta.clone())
                    .map_err(|kind| anyhow::anyhow!("invalid transaction metadata: {kind:?}"))?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    transactions.sort_by_key(|transaction| {
        transaction
            .get("transaction_index")
            .and_then(JsonValue::as_u64)
            .unwrap_or(u64::MAX)
    });
    if transactions.len() != expected_signatures.len() {
        bail!("calibration block is missing a target transaction");
    }
    Ok(sha256_hex(&serde_json::to_vec(&serde_json::json!({
        "slot": block.slot,
        "block_time_unix_seconds": block.block_time_unix_seconds,
        "blockhash": block.blockhash,
        "previous_blockhash": block.previous_blockhash,
        "parent_slot": block.parent_slot,
        "full_transaction_count": block.full_transaction_count,
        "transactions": transactions,
    }))?))
}

async fn calibrate_rpc_composition(
    rpc: &HttpArchivalRpc,
    population_slots: &[u64],
    signatures_by_slot: &BTreeMap<u64, BTreeSet<String>>,
    data_dir: &Path,
    run_id: &str,
    cutoff_slot: u64,
    provider_fingerprint_sha256: &str,
    composed_concurrency: usize,
    archival_block_cache: Option<&VerifiedArchivalBlockCacheV1>,
) -> Result<RpcCompositionCalibrationArtifactV1> {
    let sample_slots = evenly_spaced_calibration_slots(population_slots);
    if sample_slots.is_empty() {
        bail!("RPC composition calibration population is empty");
    }
    let sample_checkpoint = extract_archival_blocks(
        rpc,
        &sample_slots,
        signatures_by_slot,
        data_dir,
        composed_concurrency,
        archival_block_cache,
    )
    .await?;
    if sample_checkpoint
        .failures
        .keys()
        .any(|slot| sample_slots.binary_search(slot).is_ok())
    {
        bail!("RPC composition calibration could not fetch every composed sample block");
    }

    let artifact_path = data_dir.join(RPC_COMPOSITION_CALIBRATION_JSON);
    if artifact_path.exists() {
        let artifact: RpcCompositionCalibrationArtifactV1 = read_json(&artifact_path)?;
        artifact.verify(
            run_id,
            cutoff_slot,
            provider_fingerprint_sha256,
            population_slots,
        )?;
        return Ok(artifact);
    }

    let mut tasks = JoinSet::new();
    let mut pending = sample_slots.iter().copied().enumerate();
    let calibration_concurrency = composed_concurrency.clamp(1, 8);
    let mut rows = Vec::with_capacity(sample_slots.len());
    loop {
        while tasks.len() < calibration_concurrency {
            let Some((index, slot)) = pending.next() else {
                break;
            };
            let rpc = rpc.clone();
            let request_id = 9_000_000_u64
                .checked_add(u64::try_from(index)?)
                .context("calibration RPC request ID overflow")?;
            tasks.spawn(async move {
                (
                    slot,
                    rpc.finalized_block_with_proof(finalized_get_block_request(slot, request_id))
                        .await,
                )
            });
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        let (slot, response) = joined.context("full-block calibration task failed")?;
        let response = response.map_err(|kind| {
            anyhow::anyhow!("full-block calibration RPC failed for slot {slot}: {kind:?}")
        })?;
        let full_block = response
            .value
            .context("full-block calibration RPC returned no block")?;
        let expected = signatures_by_slot
            .get(&slot)
            .context("calibration slot lacks expected signatures")?;
        let composed_block = read_archived_block(&block_path(data_dir, slot))?;
        validate_full_vs_composed_equivalence(&full_block, &composed_block, expected).map_err(
            |kind| {
                anyhow::anyhow!(
                    "full-vs-composed archival evidence mismatch at slot {slot}: {kind:?}"
                )
            },
        )?;
        let (ordered_signatures_raw_response_sha256, independent_transaction_responses_sha256) =
            match &composed_block.proof_basis {
                ArchivedBlockProofBasisV1::OrderedSignaturesV2 {
                    raw_response_sha256,
                    ..
                } => {
                    let independent_responses = composed_block
                        .transactions
                        .iter()
                        .map(|transaction| match &transaction.proof_basis {
                            ArchivedTransactionProofBasisV1::IndependentGetTransactionV1 {
                                raw_response_sha256,
                                ..
                            } => Ok(serde_json::json!({
                                "transaction_index": transaction.transaction_index,
                                "signature": transaction.signature,
                                "raw_response_sha256": raw_response_sha256,
                            })),
                            ArchivedTransactionProofBasisV1::FullBlockPayloadV1 => {
                                bail!("composed calibration transaction uses a full-block proof")
                            }
                        })
                        .collect::<Result<Vec<_>>>()?;
                    (
                        raw_response_sha256.clone(),
                        sha256_hex(&serde_json::to_vec(&independent_responses)?),
                    )
                }
                ArchivedBlockProofBasisV1::FullPayloadV1 => {
                    bail!("composed calibration artifact uses a full-payload proof")
                }
            };
        let full_normalized_target_sha256 =
            normalized_target_projection_sha256(&full_block, expected)?;
        let composed_normalized_target_sha256 =
            normalized_target_projection_sha256(&composed_block, expected)?;
        let exact_match = full_normalized_target_sha256 == composed_normalized_target_sha256;
        rows.push(RpcCompositionCalibrationRowV1 {
            slot,
            target_signature_count: u64::try_from(expected.len())?,
            target_signatures_sha256: sha256_hex(&serde_json::to_vec(expected)?),
            full_payload_raw_response_sha256: response.raw_response_sha256,
            full_payload_inventory_sha256: full_block.full_transaction_inventory_sha256,
            ordered_signatures_raw_response_sha256,
            ordered_signatures_inventory_sha256: composed_block.full_transaction_inventory_sha256,
            independent_transaction_responses_sha256,
            full_normalized_target_sha256,
            composed_normalized_target_sha256,
            exact_match,
        });
    }
    rows.sort_by_key(|row| row.slot);
    let artifact = RpcCompositionCalibrationArtifactV1 {
        schema_version: 1,
        run_id: run_id.to_owned(),
        cutoff_slot,
        provider_fingerprint_sha256: provider_fingerprint_sha256.to_owned(),
        population_slot_count: u64::try_from(population_slots.len())?,
        selection_method: "evenly_spaced_sorted_target_slots_v1".to_owned(),
        normalization: "remove_meta_rewards_v1".to_owned(),
        rows,
        artifact_sha256: String::new(),
    }
    .seal()?;
    artifact.verify(
        run_id,
        cutoff_slot,
        provider_fingerprint_sha256,
        population_slots,
    )?;
    write_json(&artifact_path, &artifact)?;
    Ok(artifact)
}

#[cfg(test)]
mod rpc_composition_calibration_tests {
    use super::*;

    #[test]
    fn calibration_sample_is_evenly_spaced_and_includes_endpoints() {
        let population = (0_u64..1_000).collect::<Vec<_>>();
        let sample = evenly_spaced_calibration_slots(&population);
        assert_eq!(sample.len(), RPC_COMPOSITION_CALIBRATION_SAMPLE_SIZE);
        assert_eq!(sample.first(), Some(&0));
        assert_eq!(sample.last(), Some(&999));
        assert!(sample.windows(2).all(|window| window[0] < window[1]));
    }

    #[test]
    fn calibration_artifact_is_self_hashed_and_fail_closed() {
        let population = vec![10, 20, 30];
        let rows = population
            .iter()
            .map(|slot| RpcCompositionCalibrationRowV1 {
                slot: *slot,
                target_signature_count: 1,
                target_signatures_sha256: "a".repeat(64),
                full_payload_raw_response_sha256: "b".repeat(64),
                full_payload_inventory_sha256: "c".repeat(64),
                ordered_signatures_raw_response_sha256: "d".repeat(64),
                ordered_signatures_inventory_sha256: "e".repeat(64),
                independent_transaction_responses_sha256: "f".repeat(64),
                full_normalized_target_sha256: "1".repeat(64),
                composed_normalized_target_sha256: "1".repeat(64),
                exact_match: true,
            })
            .collect();
        let artifact = RpcCompositionCalibrationArtifactV1 {
            schema_version: 1,
            run_id: "run".to_owned(),
            cutoff_slot: 40,
            provider_fingerprint_sha256: "9".repeat(64),
            population_slot_count: 3,
            selection_method: "evenly_spaced_sorted_target_slots_v1".to_owned(),
            normalization: "remove_meta_rewards_v1".to_owned(),
            rows,
            artifact_sha256: String::new(),
        }
        .seal()
        .unwrap();
        artifact
            .verify("run", 40, &"9".repeat(64), &population)
            .unwrap();

        let mut tampered = artifact;
        tampered.rows[1].exact_match = false;
        assert!(tampered
            .verify("run", 40, &"9".repeat(64), &population)
            .is_err());
    }
}

async fn extract_archival_blocks(
    rpc: &HttpArchivalRpc,
    slots: &[u64],
    signatures_by_slot: &BTreeMap<u64, BTreeSet<String>>,
    data_dir: &Path,
    checkpoint_batch: usize,
    archival_block_cache: Option<&VerifiedArchivalBlockCacheV1>,
) -> Result<RpcCheckpoint> {
    let checkpoint_path = data_dir.join(RPC_CHECKPOINT);
    let blocks_dir = data_dir.join("blocks");
    fs::create_dir_all(&blocks_dir)?;
    let mut checkpoint = if checkpoint_path.exists() {
        read_json(&checkpoint_path)?
    } else {
        RpcCheckpoint::default()
    };
    let requested_slots: BTreeSet<u64> = slots.iter().copied().collect();
    checkpoint
        .completed_slots
        .retain(|slot| requested_slots.contains(slot));
    checkpoint
        .failures
        .retain(|slot, _| requested_slots.contains(slot));
    for slot in slots {
        checkpoint.completed_slots.remove(slot);
        let path = block_path(data_dir, *slot);
        let expected = signatures_by_slot
            .get(slot)
            .context("target slot has no expected program signatures")?;
        let validation = if let Some(cache) = archival_block_cache {
            cache.materialize_and_validate(data_dir, *slot, expected)
        } else {
            validate_resumable_block(&path, *slot, expected)
        };
        if validation.is_ok() {
            checkpoint.completed_slots.insert(*slot);
            checkpoint.failures.remove(slot);
        } else if archival_block_cache.is_some() {
            return Err(validation.expect_err("failed cache validation has an error"))
                .with_context(|| {
                    format!(
                        "required archival block-cache entry failed closed at slot {}",
                        slot
                    )
                });
        } else if path.exists() {
            checkpoint
                .failures
                .insert(*slot, RpcFailureKind::InvalidResponse);
        }
    }
    let concurrency = checkpoint_batch.max(1);
    let pending: Vec<u64> = slots
        .iter()
        .copied()
        .filter(|slot| !checkpoint.completed_slots.contains(slot))
        .collect();
    let mut pending = pending.into_iter();
    let mut tasks = JoinSet::new();
    let mut completed_since_checkpoint = 0_usize;
    let checkpoint_interval = concurrency.saturating_mul(16).max(256);
    loop {
        while tasks.len() < concurrency {
            let Some(slot) = pending.next() else {
                break;
            };
            let expected = signatures_by_slot
                .get(&slot)
                .context("target slot has no expected program signatures")?
                .clone();
            let request_id_start = checkpoint
                .requests_completed
                .checked_add(1)
                .context("archival RPC request counter overflow")?;
            let request_count = u64::try_from(expected.len())?
                .checked_add(1)
                .context("archival RPC request count overflow")?;
            checkpoint.requests_completed = checkpoint
                .requests_completed
                .checked_add(request_count)
                .context("archival RPC request counter overflow")?;
            spawn_composed_archival_block_task(
                &mut tasks,
                rpc.clone(),
                slot,
                expected,
                request_id_start,
            );
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        let (slot, response) = joined.context("archival block task failed")?;
        match response {
            Ok(Some(block)) if block.slot == slot => {
                let expected = signatures_by_slot
                    .get(&slot)
                    .context("target slot has no expected program signatures")?;
                if validate_composed_block(&block, slot, expected).is_err() {
                    checkpoint
                        .failures
                        .insert(slot, RpcFailureKind::InvalidResponse);
                } else {
                    write_archived_block(&block_path(data_dir, slot), &block)?;
                    checkpoint.completed_slots.insert(slot);
                    checkpoint.failures.remove(&slot);
                }
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
        completed_since_checkpoint = completed_since_checkpoint.saturating_add(1);
        if completed_since_checkpoint >= checkpoint_interval {
            write_json(&checkpoint_path, &checkpoint)?;
            eprintln!(
                "extract: composed archival blocks fetched={}/{} unresolved={} rpc_requests={}",
                checkpoint.completed_slots.len(),
                slots.len(),
                checkpoint.failures.len(),
                checkpoint.requests_completed
            );
            completed_since_checkpoint = 0;
        }
    }
    write_json(&checkpoint_path, &checkpoint)?;
    eprintln!(
        "extract: composed archival blocks fetched={}/{} unresolved={} rpc_requests={}",
        checkpoint.completed_slots.len(),
        slots.len(),
        checkpoint.failures.len(),
        checkpoint.requests_completed
    );
    Ok(checkpoint)
}

type ComposedArchivalBlockTask = (u64, Result<Option<ArchivedBlock>, RpcFailureKind>);

fn spawn_composed_archival_block_task(
    tasks: &mut JoinSet<ComposedArchivalBlockTask>,
    rpc: HttpArchivalRpc,
    slot: u64,
    expected_signatures: BTreeSet<String>,
    request_id_start: u64,
) {
    tasks.spawn(async move {
        let signature_block = match rpc
            .finalized_ordered_signature_block_with_proof(finalized_get_block_signatures_request(
                slot,
                request_id_start,
            ))
            .await
        {
            Ok(response) => match response.value {
                Some(block) => block,
                None => return (slot, Ok(None)),
            },
            Err(kind) => return (slot, Err(kind)),
        };
        let mut transaction_tasks = JoinSet::new();
        for (offset, signature) in expected_signatures.iter().cloned().enumerate() {
            let Some(request_id) = u64::try_from(offset)
                .ok()
                .and_then(|offset| request_id_start.checked_add(offset))
                .and_then(|request_id| request_id.checked_add(1))
            else {
                return (slot, Err(RpcFailureKind::InvalidResponse));
            };
            let rpc = rpc.clone();
            transaction_tasks.spawn(async move {
                rpc.finalized_independent_transaction_with_proof(finalized_get_transaction_request(
                    &signature, request_id,
                ))
                .await
                .map(|response| response.value)
            });
        }
        let mut transaction_proofs = Vec::with_capacity(expected_signatures.len());
        while let Some(joined) = transaction_tasks.join_next().await {
            match joined {
                Ok(Ok(Some(proof))) => transaction_proofs.push(proof),
                Ok(Ok(None)) => return (slot, Ok(None)),
                Ok(Err(kind)) => return (slot, Err(kind)),
                Err(_) => return (slot, Err(RpcFailureKind::Transport)),
            }
        }
        (
            slot,
            compose_ordered_signature_block(
                signature_block,
                &expected_signatures,
                transaction_proofs,
            )
            .map(Some),
        )
    });
}

fn validate_composed_block(
    block: &ArchivedBlock,
    expected_slot: u64,
    expected_signatures: &BTreeSet<String>,
) -> Result<()> {
    if block.slot != expected_slot
        || !matches!(
            &block.proof_basis,
            ArchivedBlockProofBasisV1::OrderedSignaturesV2 { .. }
        )
    {
        bail!("archived block is not an ordered-signature composition");
    }
    block
        .validate_proof_basis()
        .map_err(|kind| anyhow::anyhow!("invalid composed archived-block proof: {kind:?}"))?;
    let retained_signatures = block
        .transactions
        .iter()
        .map(|transaction| transaction.signature.clone())
        .collect::<BTreeSet<_>>();
    if block
        .transactions
        .iter()
        .any(|transaction| !transaction.succeeded)
        || &retained_signatures != expected_signatures
    {
        bail!("retained transaction inventory does not match successful signature crawl");
    }
    Ok(())
}

fn validate_resumable_block(
    path: &Path,
    expected_slot: u64,
    expected_signatures: &BTreeSet<String>,
) -> Result<()> {
    let block = read_archived_block(path)?;
    validate_composed_block(&block, expected_slot, expected_signatures)
}

fn signatures_by_slot(signatures: &[ProgramSignatureV1]) -> BTreeMap<u64, BTreeSet<String>> {
    let mut output = BTreeMap::<u64, BTreeSet<String>>::new();
    for signature in signatures {
        output
            .entry(signature.slot)
            .or_default()
            .insert(signature.signature.clone());
    }
    output
}

struct JsonlSlotCursor<T> {
    path: PathBuf,
    lines: std::io::Lines<BufReader<File>>,
    pending: Option<T>,
    line_number: usize,
}

impl<T: DeserializeOwned> JsonlSlotCursor<T> {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            lines: BufReader::new(File::open(path)?).lines(),
            pending: None,
            line_number: 0,
        })
    }

    fn next_value(&mut self) -> Result<Option<T>> {
        let Some(line) = self.lines.next() else {
            return Ok(None);
        };
        self.line_number = self.line_number.saturating_add(1);
        let line = line?;
        serde_json::from_str(&line)
            .with_context(|| format!("decoding {} line {}", self.path.display(), self.line_number))
            .map(Some)
    }

    fn next_slot_group(&mut self, slot_of: impl Fn(&T) -> u64) -> Result<Option<(u64, Vec<T>)>> {
        let first = match self.pending.take() {
            Some(value) => Some(value),
            None => self.next_value()?,
        };
        let Some(first) = first else {
            return Ok(None);
        };
        let slot = slot_of(&first);
        let mut group = vec![first];
        loop {
            let Some(next) = self.next_value()? else {
                break;
            };
            let next_slot = slot_of(&next);
            if next_slot < slot {
                bail!("JSONL slot order regressed in {}", self.path.display());
            }
            if next_slot == slot {
                group.push(next);
            } else {
                self.pending = Some(next);
                break;
            }
        }
        Ok(Some((slot, group)))
    }
}

fn reconcile_db_rpc_history(
    data_dir: &Path,
    run_dir: &Path,
    missing_enrichment: u64,
) -> Result<(SourceReconciliationSummaryV1, PathBuf, PathBuf)> {
    let mut database =
        JsonlSlotCursor::<EnrichedDbEventSeed>::open(&data_dir.join(ENRICHED_DB_EVENTS_JSONL))?;
    let mut rpc =
        JsonlSlotCursor::<CanonicalEventV1>::open(&data_dir.join(CANONICAL_EVENTS_JSONL))?;
    let records_path = run_dir.join(DB_RPC_RECONCILIATION_JSONL);
    let records_partial = partial_path(&records_path);
    let mut writer = BufWriter::new(File::create(&records_partial)?);
    let mut summary = SourceReconciliationSummaryV1::new();
    let mut db_group = database.next_slot_group(|event| event.db.slot)?;
    let mut rpc_group = rpc.next_slot_group(|event| event.order.slot)?;

    while db_group.is_some() || rpc_group.is_some() {
        let db_slot = db_group.as_ref().map(|(slot, _)| *slot);
        let rpc_slot = rpc_group.as_ref().map(|(slot, _)| *slot);
        let slot = match (db_slot, rpc_slot) {
            (Some(left), Some(right)) => left.min(right),
            (Some(slot), None) | (None, Some(slot)) => slot,
            (None, None) => break,
        };
        let db_events = if db_slot == Some(slot) {
            db_group.take().expect("slot checked").1
        } else {
            Vec::new()
        };
        let rpc_events = if rpc_slot == Some(slot) {
            rpc_group.take().expect("slot checked").1
        } else {
            Vec::new()
        };
        summary.database_rows = summary.database_rows.saturating_add(db_events.len() as u64);
        summary.rpc_events_in_db_covered_families =
            summary.rpc_events_in_db_covered_families.saturating_add(
                rpc_events
                    .iter()
                    .filter(|event| canonical_event_kind(&event.payload).is_some())
                    .count() as u64,
            );
        let market_by_rpc_id: BTreeMap<_, _> = rpc_events
            .iter()
            .filter_map(|event| {
                event
                    .pair
                    .as_deref()
                    .map(|pair| (event.event_id.as_str(), pair))
            })
            .collect();
        let market_by_db_ref: BTreeMap<_, _> = db_events
            .iter()
            .map(|event| {
                (
                    format!("{}:{}", event.db.source_table, event.db.source_id),
                    event.db.pair.as_str(),
                )
            })
            .collect();
        for record in reconcile_slot_events(&db_events, &rpc_events) {
            let market = record
                .rpc_event_id
                .as_deref()
                .and_then(|id| market_by_rpc_id.get(id).copied())
                .or_else(|| {
                    record
                        .db_event_ref
                        .as_deref()
                        .and_then(|id| market_by_db_ref.get(id).copied())
                });
            summary.record(market, record.classification);
            write_json_line(&mut writer, &record)?;
        }
        if db_group.is_none() {
            db_group = database.next_slot_group(|event| event.db.slot)?;
        }
        if rpc_group.is_none() {
            rpc_group = rpc.next_slot_group(|event| event.order.slot)?;
        }
    }
    // Rows that could not be enriched never reached the natural-grain join and
    // are explicit blockers rather than disappearing from the denominator.
    summary.database_rows = summary.database_rows.saturating_add(missing_enrichment);
    summary.expected_records = summary.expected_records.saturating_add(missing_enrichment);
    summary.blocking_records = summary.blocking_records.saturating_add(missing_enrichment);
    *summary
        .classifications
        .entry("rpc_missing_critical".to_owned())
        .or_default() += missing_enrichment;
    writer.flush()?;
    fs::rename(records_partial, &records_path)?;
    let summary_path = run_dir.join(DB_RPC_RECONCILIATION_SUMMARY);
    write_json(&summary_path, &summary)?;
    Ok((summary, records_path, summary_path))
}

fn canonical_order_coverage(path: &Path) -> Result<CoverageCount> {
    let mut previous = None;
    let mut event_ids = BTreeSet::new();
    let mut expected = 0_u64;
    let mut observed = 0_u64;
    visit_json_lines::<CanonicalEventV1, _>(path, |event| {
        expected = expected.saturating_add(1);
        let ordered = previous.is_none_or(|order| order < event.order);
        let unique = event_ids.insert(event.event_id.clone());
        let path_proven = !event.execution_path.is_empty();
        if ordered && unique && path_proven {
            observed = observed.saturating_add(1);
        }
        previous = Some(event.order);
        Ok(())
    })?;
    Ok(CoverageCount { observed, expected })
}

fn configuration_coverage(
    config_path: &Path,
    operations_path: &Path,
) -> Result<NaturalGrainCoverageSummaryV1> {
    let configurations: Vec<ConfigVersionV1> = read_json_lines(config_path)?;
    let mut summary = NaturalGrainCoverageSummaryV1::new();
    let mut by_key = BTreeMap::<(ConfigScope, String), Vec<ConfigVersionV1>>::new();
    let mut config_ids = BTreeSet::new();
    for configuration in configurations {
        let identity_valid = !configuration.config_id.is_empty()
            && !configuration.entity_address.is_empty()
            && !configuration.source_signature.is_empty()
            && !configuration.decoder_version_id.is_empty()
            && configuration
                .valid_to_exclusive
                .is_none_or(|end| configuration.valid_from < end)
            && config_ids.insert(configuration.config_id.clone());
        if !identity_valid {
            summary.record(None, false, "invalid_or_duplicate_config_epoch");
        }
        by_key
            .entry((
                configuration.scope.clone(),
                configuration.entity_address.clone(),
            ))
            .or_default()
            .push(configuration);
    }
    for epochs in by_key.values_mut() {
        epochs.sort_by_key(|epoch| epoch.valid_from);
        for adjacent in epochs.windows(2) {
            if adjacent[0].valid_to_exclusive != Some(adjacent[1].valid_from) {
                summary.record(None, false, "non_contiguous_config_epochs");
            }
        }
    }

    visit_json_lines::<CanonicalOperationV1, _>(operations_path, |operation| {
        let market = (operation.pair != "__global__").then_some(operation.pair.as_str());
        record_config_requirement(
            &mut summary,
            &by_key,
            &operation,
            ConfigScope::ProgramBinary,
            "__program_binary__",
            market,
        );

        if operation.pair == "__global__" {
            if let OperationPayloadV1::UpdateConfig { fields } = &operation.payload {
                match fields.get("instruction_kind").map(String::as_str) {
                    Some("init_futarchy_authority") => {
                        for scope in [
                            ConfigScope::ProtocolRevenue,
                            ConfigScope::RevenueRecipients,
                            ConfigScope::ReduceOnly,
                        ] {
                            record_config_requirement(
                                &mut summary,
                                &by_key,
                                &operation,
                                scope,
                                "__global__",
                                None,
                            );
                        }
                    }
                    Some("update_futarchy_authority" | "update_protocol_revenue") => {
                        record_config_requirement(
                            &mut summary,
                            &by_key,
                            &operation,
                            ConfigScope::ProtocolRevenue,
                            "__global__",
                            None,
                        );
                    }
                    Some("update_revenue_recipients") => {
                        record_config_requirement(
                            &mut summary,
                            &by_key,
                            &operation,
                            ConfigScope::RevenueRecipients,
                            "__global__",
                            None,
                        );
                    }
                    Some("set_global_reduce_only") => {
                        record_config_requirement(
                            &mut summary,
                            &by_key,
                            &operation,
                            ConfigScope::ReduceOnly,
                            "__global__",
                            None,
                        );
                    }
                    _ => summary.record(None, false, "unknown_global_config_operation"),
                }
            }
            return Ok(());
        }

        let pair_config =
            active_config(&by_key, ConfigScope::Pair, &operation.pair, operation.order);
        let pair_observed =
            pair_config.is_some_and(|config| config_fields_complete(config, &operation));
        summary.record(
            market,
            pair_observed,
            if pair_config.is_some() {
                "incomplete_pair_config"
            } else {
                "pair_config_not_unique"
            },
        );

        if let Some(rate_model) = pair_config.and_then(|config| config.fields.get("rate_model")) {
            record_config_requirement(
                &mut summary,
                &by_key,
                &operation,
                ConfigScope::RateModel,
                rate_model,
                market,
            );
        } else {
            summary.record(market, false, "rate_model_reference_missing");
        }
        for (scope, entity) in [
            (ConfigScope::ProtocolRevenue, "__global__"),
            (ConfigScope::ReduceOnly, "__global__"),
            (ConfigScope::ReduceOnly, operation.pair.as_str()),
        ] {
            record_config_requirement(&mut summary, &by_key, &operation, scope, entity, market);
        }
        if matches!(
            operation.payload,
            OperationPayloadV1::ClaimProtocolRevenue { .. }
        ) {
            record_config_requirement(
                &mut summary,
                &by_key,
                &operation,
                ConfigScope::RevenueRecipients,
                "__global__",
                market,
            );
        }
        Ok(())
    })?;
    Ok(summary)
}

fn active_config<'a>(
    by_key: &'a BTreeMap<(ConfigScope, String), Vec<ConfigVersionV1>>,
    scope: ConfigScope,
    entity: &str,
    order: ChainOrder,
) -> Option<&'a ConfigVersionV1> {
    let matches: Vec<_> = by_key
        .get(&(scope, entity.to_owned()))?
        .iter()
        .filter(|config| {
            config.valid_from <= order
                && config
                    .valid_to_exclusive
                    .is_none_or(|exclusive| order < exclusive)
        })
        .collect();
    (matches.len() == 1).then_some(matches[0])
}

fn record_config_requirement(
    summary: &mut NaturalGrainCoverageSummaryV1,
    by_key: &BTreeMap<(ConfigScope, String), Vec<ConfigVersionV1>>,
    operation: &CanonicalOperationV1,
    scope: ConfigScope,
    entity: &str,
    market: Option<&str>,
) {
    let config = active_config(by_key, scope, entity, operation.order);
    let observed = config.is_some_and(|config| config_fields_complete(config, operation));
    summary.record(
        market,
        observed,
        if config.is_some() {
            "active_config_fields_incomplete"
        } else {
            "active_config_not_unique"
        },
    );
}

fn config_fields_complete(
    configuration: &ConfigVersionV1,
    operation: &CanonicalOperationV1,
) -> bool {
    let required: &[&str] = match configuration.scope {
        ConfigScope::ProgramBinary => &[
            "source_git_commit",
            "idl_sha256",
            "candidate_program_binary_sha256",
            "provisional",
        ],
        ConfigScope::Pair => &[
            "token0",
            "token1",
            "token0_decimals",
            "token1_decimals",
            "rate_model",
            "swap_fee_bps",
            "half_life",
        ],
        ConfigScope::RateModel => &[
            "target_util_start_bps",
            "target_util_end_bps",
            "rate_half_life_ms",
            "min_rate_bps",
            "max_rate_bps",
            "initial_rate_bps",
        ],
        ConfigScope::ProtocolRevenue => &[
            "swap_bps",
            "interest_bps",
            "futarchy_treasury_bps",
            "buybacks_vault_bps",
            "team_treasury_bps",
        ],
        ConfigScope::RevenueRecipients => &["futarchy_treasury", "buybacks_vault", "team_treasury"],
        ConfigScope::ReduceOnly => &["reduce_only"],
    };
    if required.iter().any(|field| {
        configuration
            .fields
            .get(*field)
            .is_none_or(String::is_empty)
    }) {
        return false;
    }
    if configuration.scope == ConfigScope::ProgramBinary {
        let source_git_commit = configuration
            .fields
            .get("source_git_commit")
            .expect("required above");
        let idl = configuration
            .fields
            .get("idl_sha256")
            .expect("required above");
        let candidate_binary = configuration
            .fields
            .get("candidate_program_binary_sha256")
            .expect("required above");
        return configuration.decoder_version_id == operation.decoder_version_id
            && matches!(
                configuration.fields.get("provisional").map(String::as_str),
                Some("true" | "false")
            )
            && is_git_oid(source_git_commit)
            && is_sha256_hex(idl)
            && is_sha256_hex(candidate_binary)
            && configuration
                .fields
                .get("program_binary_sha256")
                .is_none_or(|deployed| is_sha256_hex(deployed));
    }
    if configuration.scope == ConfigScope::ReduceOnly {
        return matches!(
            configuration.fields.get("reduce_only").map(String::as_str),
            Some("true" | "false")
        );
    }
    true
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn historical_price_coverage(
    prices_path: &Path,
    operations_path: &Path,
    provider: &str,
) -> Result<NaturalGrainCoverageSummaryV1> {
    let mut summary = NaturalGrainCoverageSummaryV1::new();
    let mut prices = BTreeMap::<(i64, String), u32>::new();
    visit_json_lines::<HistoricalPriceMarkV1, _>(prices_path, |mark| {
        if mark.validate().is_err() || mark.provider != provider {
            summary.record(None, false, "invalid_or_wrong_provider_price_mark");
        } else {
            let count = prices
                .entry((mark.bucket_unix_seconds, mark.mint))
                .or_default();
            *count = count.saturating_add(1);
        }
        Ok(())
    })?;
    for count in prices.values() {
        if *count != 1 {
            summary.record(None, false, "ambiguous_duplicate_price_mark");
        }
    }

    let mut market_tokens = BTreeMap::<String, (String, String)>::new();
    visit_json_lines::<CanonicalOperationV1, _>(operations_path, |operation| {
        if let OperationPayloadV1::InitializeMarket { token0, token1, .. } = &operation.payload {
            match market_tokens.get(&operation.pair) {
                Some(existing) if existing != &(token0.clone(), token1.clone()) => {
                    summary.record(
                        Some(&operation.pair),
                        false,
                        "conflicting_market_token_identity",
                    );
                }
                _ => {
                    market_tokens.insert(operation.pair.clone(), (token0.clone(), token1.clone()));
                }
            }
        }
        if matches!(operation.payload, OperationPayloadV1::UpdateConfig { .. })
            || operation.pair == "__global__"
        {
            return Ok(());
        }
        let bucket = operation
            .block_time_unix_seconds
            .div_euclid(3_600)
            .checked_mul(3_600)
            .context("historical price bucket overflow")?;
        if let Some((token0, token1)) = market_tokens.get(&operation.pair) {
            for mint in [token0, token1] {
                let observed = prices
                    .get(&(bucket, mint.clone()))
                    .is_some_and(|count| *count == 1);
                summary.record(
                    Some(&operation.pair),
                    observed,
                    "historical_price_mark_missing",
                );
            }
        } else {
            // The two token lanes are still required even when a missing or
            // invalid initializer prevents resolving their mint addresses.
            summary.record(
                Some(&operation.pair),
                false,
                "market_token_identity_missing",
            );
            summary.record(
                Some(&operation.pair),
                false,
                "market_token_identity_missing",
            );
        }
        Ok(())
    })?;
    Ok(summary)
}

fn gate_evidence_tier(
    scope_mode: ScopeMode,
    configured: ConfigEvidenceTier,
) -> Result<GateEvidenceTier> {
    match (scope_mode, configured) {
        (ScopeMode::FullUniverse, ConfigEvidenceTier::DecisionGrade) => {
            bail!("full-universe scope requires audit-grade evidence")
        }
        (_, ConfigEvidenceTier::DecisionGrade) => Ok(GateEvidenceTier::DecisionGrade),
        (_, ConfigEvidenceTier::AuditGrade) => Ok(GateEvidenceTier::AuditGrade),
    }
}

fn v1_replay_certification_policy(
    scope_mode: ScopeMode,
    configured: ConfigEvidenceTier,
    has_verified_selected_scope_cohort: bool,
) -> Result<V1ReplayCertificationPolicyV1> {
    match gate_evidence_tier(scope_mode, configured)? {
        GateEvidenceTier::AuditGrade => Ok(V1ReplayCertificationPolicyV1::AuditGrade),
        GateEvidenceTier::DecisionGrade if has_verified_selected_scope_cohort => {
            Ok(V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope)
        }
        GateEvidenceTier::DecisionGrade => {
            bail!("decision-grade native replay requires a verified selected-scope cohort")
        }
    }
}

fn validated_cohort_selection(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<Option<CohortSelectionArtifactV1>> {
    if config.scope.mode == ScopeMode::FullUniverse {
        gate_evidence_tier(config.scope.mode, config.scope.evidence_tier)?;
        return Ok(None);
    }

    let path = context.run_dir().join(COHORT_SELECTION_JSON);
    if !path.exists() {
        bail!(
            "hybrid-panel evidence requires sealed cohort selection at {}",
            path.display()
        );
    }
    let artifact: CohortSelectionArtifactV1 = read_json(&path)?;
    artifact
        .verify_self_hash()
        .context("verifying sealed cohort selection")?;
    if artifact.run_id != context.run_id()
        || artifact.cutoff_slot != context.cutoff_slot()
        || artifact.mode != config.scope.mode
        || artifact.evidence_tier != config.scope.evidence_tier
        || artifact.source_bindings.config_sha256 != context.config_sha256()
        || !artifact.minimum_pre_audit_panel_met
    {
        bail!("cohort selection does not bind to the immutable run and configured evidence scope");
    }
    artifact
        .extraction_targets()
        .context("validating cohort extraction targets")?;
    Ok(Some(artifact))
}

fn validate_cohort_final_qualification(
    cohort: &CohortSelectionArtifactV1,
    qualification: &CohortFinalAuditQualificationV1,
) -> Result<()> {
    let accepted: BTreeSet<&str> = qualification
        .accepted_markets
        .iter()
        .map(String::as_str)
        .collect();
    let rejected: BTreeSet<&str> = qualification
        .rejected_markets
        .iter()
        .map(|market| market.pair_address.as_str())
        .collect();
    let selected: BTreeSet<&str> = cohort.selected_markets.iter().map(String::as_str).collect();
    let complete_partition = accepted.is_disjoint(&rejected)
        && accepted.union(&rejected).copied().collect::<BTreeSet<_>>() == selected;
    let expected_minimum = accepted.len() >= cohort.minimum_panel_markets;
    if accepted.len() != qualification.accepted_markets.len()
        || rejected.len() != qualification.rejected_markets.len()
        || !complete_partition
        || qualification.minimum_final_panel_met != expected_minimum
        || !qualification.minimum_final_panel_met
    {
        bail!("cohort final-audit qualification is incomplete or below its minimum panel size");
    }
    Ok(())
}

fn phase_gate_policy(
    config: &BenchmarkConfig,
    exact_selected_scope_replay_proven: bool,
) -> Result<PhaseGatePolicyV1> {
    Ok(PhaseGatePolicyV1 {
        evidence_tier: gate_evidence_tier(config.scope.mode, config.scope.evidence_tier)?,
        // Transfer-fee provenance is applied at market grain through
        // `market_ready`; a global mint gap must not poison unaffected markets.
        historical_transfer_fees_required: false,
        // This records a distinct USD decision without changing raw phase
        // eligibility. Universe construction still applies price evidence per
        // market and hourly bucket.
        historical_prices_required: true,
        exact_selected_scope_replay_proven,
    })
}

fn gate_passes(report: &HardGatePhaseReportV1, gate_id: HardGateId) -> bool {
    report
        .report
        .gates
        .iter()
        .find(|gate| gate.gate == gate_id)
        .is_some_and(|gate| gate.status == GateStatus::Pass)
}

fn selected_scope_replay_is_exact(
    cohort: Option<&CohortSelectionArtifactV1>,
    phase_report: &HardGatePhaseReportV1,
    reconciliation: &V1ReconciliationSummaryV1,
) -> bool {
    let Some(cohort) = cohort else {
        return false;
    };
    let replay_markets: BTreeSet<&str> = reconciliation
        .markets
        .iter()
        .map(|market| market.market.as_str())
        .collect();
    let selected_markets: BTreeSet<&str> =
        cohort.selected_markets.iter().map(String::as_str).collect();
    replay_markets == selected_markets
        && reconciliation.zero_raw_unit_difference
        && reconciliation.all_conservation_identities_hold
        && [
            HardGateId::TransactionInventory,
            HardGateId::StateChangingDecode,
            HardGateId::CanonicalOrder,
            HardGateId::AuthoritativeBlockTime,
            HardGateId::VersionedConfig,
            HardGateId::SourceAgreement,
            HardGateId::EconomicReplay,
        ]
        .into_iter()
        .all(|gate| gate_passes(phase_report, gate))
}

fn blocking_gate_codes(decision: &PhaseGateDecisionV1) -> String {
    decision
        .raw_blocking_gates
        .iter()
        .map(|gate| format!("{gate:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

struct ValidatedEvaluationScopeEvidenceV1 {
    replay_certification_policy: V1ReplayCertificationPolicyV1,
    cohort_selection_sha256: Option<String>,
    cohort_representativeness: Option<CohortRepresentativenessEvidenceV1>,
    final_phase_gate_sha256: String,
    final_phase_gate_decision: PhaseGateDecisionV1,
}

impl ValidatedEvaluationScopeEvidenceV1 {
    fn result_scope(
        &self,
        config: &BenchmarkConfig,
        selected_markets: BTreeSet<String>,
    ) -> Result<ResultScopeV1> {
        ResultScopeV1::new(
            config.scope.mode,
            config.scope.evidence_tier,
            selected_markets,
            self.cohort_selection_sha256.clone(),
            self.cohort_representativeness.clone(),
            self.final_phase_gate_sha256.clone(),
            self.final_phase_gate_decision.clone(),
        )
        .map_err(Into::into)
    }
}

fn validated_evaluation_scope_evidence(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<ValidatedEvaluationScopeEvidenceV1> {
    let cohort_selection = validated_cohort_selection(context, config)?;
    let expected_cohort_sha256 = cohort_selection
        .as_ref()
        .map(|cohort| cohort.artifact_sha256.clone());
    let final_gate: FinalPhaseGateArtifactV1 =
        read_json(&context.run_dir().join(FINAL_PHASE_GATE_JSON))
            .context("sealed evaluation requires the verified post-replay final-gate artifact")?;
    if final_gate.schema_version != 1
        || final_gate.run_id != context.run_id()
        || final_gate.cutoff_slot != context.cutoff_slot()
        || final_gate.cohort_selection_sha256 != expected_cohort_sha256
        || final_gate.phase_report.phase != HardGatePhase::PostReplayFinal
        || final_gate.decision.phase != HardGatePhase::PostReplayFinal
    {
        bail!("post-replay final-gate artifact belongs to another phase, run, or cohort");
    }

    let reconciliation: V1ReconciliationSummaryV1 =
        read_json(&context.run_dir().join(V1_RECONCILIATION_JSON))?;
    if reconciliation.run_id != context.run_id()
        || reconciliation.cutoff_slot != context.cutoff_slot()
    {
        bail!("V1 reconciliation belongs to another immutable run");
    }
    let exact_selected_scope_replay_proven = selected_scope_replay_is_exact(
        cohort_selection.as_ref(),
        &final_gate.phase_report,
        &reconciliation,
    );
    let expected_decision = assess_phase_gates(
        &final_gate.phase_report,
        phase_gate_policy(config, exact_selected_scope_replay_proven)?,
    )
    .context("revalidating the post-replay final gate before leverage evaluation")?;
    if final_gate.decision != expected_decision
        || !expected_decision.phase_eligible
        || matches!(
            expected_decision.raw_certification,
            CertificationStatus::Pending | CertificationStatus::Fail
        )
    {
        bail!("post-replay final gate does not authorize sealed leverage evaluation");
    }

    let replay_certification_policy = v1_replay_certification_policy(
        config.scope.mode,
        config.scope.evidence_tier,
        cohort_selection.is_some(),
    )?;

    let cohort_representativeness = if let Some(cohort) = cohort_selection.as_ref() {
        let path = context.run_dir().join(COHORT_REPRESENTATIVENESS_JSON);
        let artifact: CohortRepresentativenessV1 = read_json(&path)
            .context("selected-panel evaluation requires cohort representativeness evidence")?;
        artifact.verify()?;
        let inventory: ExtractionInventoryV1 =
            read_json(&context.run_dir().join(EXTRACTION_INVENTORY))?;
        if artifact.run_id != context.run_id()
            || artifact.cutoff_slot != context.cutoff_slot()
            || artifact.cohort_selection_sha256 != cohort.artifact_sha256
            || inventory.run_id != context.run_id()
            || inventory.cutoff_slot != context.cutoff_slot()
            || inventory.scope_mode != config.scope.mode
            || inventory.cohort_selection_sha256 != cohort.artifact_sha256
            || inventory.cohort_representativeness_sha256 != artifact.artifact_sha256
            || inventory.selected_markets != cohort.selected_markets
        {
            bail!("cohort representativeness does not bind to the selected extraction scope");
        }
        let coverage = |value: &CohortActivityCoverageV1| ScopeActivityCoverageV1 {
            visible_universe_rows: value.visible_universe_rows,
            selected_panel_rows: value.selected_panel_rows,
            selected_panel_coverage_bps: value.selected_panel_coverage_bps,
        };
        Some(CohortRepresentativenessEvidenceV1 {
            artifact_sha256: artifact.artifact_sha256,
            cohort_selection_sha256: artifact.cohort_selection_sha256,
            swaps: coverage(&artifact.swaps),
            debt_actions: coverage(&artifact.debt_actions),
            liquidations: coverage(&artifact.liquidations),
        })
    } else {
        None
    };

    Ok(ValidatedEvaluationScopeEvidenceV1 {
        replay_certification_policy,
        cohort_selection_sha256: expected_cohort_sha256,
        cohort_representativeness,
        final_phase_gate_sha256: sha256_file(&context.run_dir().join(FINAL_PHASE_GATE_JSON))?,
        final_phase_gate_decision: final_gate.decision,
    })
}

fn validate_result_scope_bindings(
    context: &RunContext,
    config: &BenchmarkConfig,
    manifest: &ResultManifestV1,
) -> Result<()> {
    if manifest.run_id != context.run_id() || manifest.cutoff_slot != context.cutoff_slot() {
        bail!("metrics manifest belongs to another immutable run");
    }
    let universe: crate::universe::UniverseWindowArtifactV1 =
        read_json(&context.run_dir().join(UNIVERSE_WINDOW_JSON))?;
    let selected_markets: BTreeSet<String> = universe
        .market_eligibility
        .iter()
        .filter(|market| market.usd_headline_eligible)
        .map(|market| market.pair_address.clone())
        .collect();
    let validated_scope = validated_evaluation_scope_evidence(context, config)?;
    let expected_scope = validated_scope.result_scope(config, selected_markets)?;
    if manifest.scope != expected_scope {
        bail!("metrics manifest scope differs from the verified cohort and final gate");
    }

    let mut required_sources = vec![context.run_dir().join(FINAL_PHASE_GATE_JSON)];
    if config.scope.mode == ScopeMode::HybridPanel {
        required_sources.extend([
            context.run_dir().join(COHORT_SELECTION_JSON),
            context.run_dir().join(COHORT_REPRESENTATIVENESS_JSON),
        ]);
    }
    for path in required_sources {
        let key = relative_source_key(context, &path)?;
        let expected = sha256_file(&path)?;
        if manifest.source_artifact_sha256.get(&key) != Some(&expected) {
            bail!("metrics manifest omits or misbinds scope source {key}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod phase_gate_pipeline_tests {
    use super::*;

    fn order(slot: u64) -> ChainOrder {
        ChainOrder {
            slot,
            transaction_index: 0,
            outer_instruction_index: 0,
            inner_execution_index: None,
            event_sequence: 0,
        }
    }

    fn program_binary_configuration(provisional: &str) -> ConfigVersionV1 {
        ConfigVersionV1 {
            config_id: "program-binary-config".to_owned(),
            scope: ConfigScope::ProgramBinary,
            entity_address: "__program_binary__".to_owned(),
            valid_from: order(1),
            valid_to_exclusive: None,
            source_signature: "registry:decoder-v1".to_owned(),
            decoder_version_id: "decoder-v1".to_owned(),
            fields: BTreeMap::from([
                ("source_git_commit".to_owned(), "c".repeat(40)),
                ("idl_sha256".to_owned(), "a".repeat(64)),
                ("candidate_program_binary_sha256".to_owned(), "b".repeat(64)),
                ("provisional".to_owned(), provisional.to_owned()),
            ]),
        }
    }

    fn operation_for_decoder(decoder_version_id: &str) -> CanonicalOperationV1 {
        CanonicalOperationV1 {
            schema_version: crate::replay::REPLAY_SCHEMA_VERSION,
            operation_id: "operation".to_owned(),
            signature: "signature".to_owned(),
            pair: "pair".to_owned(),
            actor: None,
            order: order(2),
            block_time_unix_seconds: 1,
            decoder_version_id: decoder_version_id.to_owned(),
            payload: OperationPayloadV1::ClockOnly,
            assertions: Vec::new(),
        }
    }

    #[test]
    fn evidence_tier_mapping_keeps_full_universe_audit_grade() {
        assert_eq!(
            gate_evidence_tier(ScopeMode::HybridPanel, ConfigEvidenceTier::DecisionGrade)
                .expect("hybrid decision grade"),
            GateEvidenceTier::DecisionGrade
        );
        assert_eq!(
            gate_evidence_tier(ScopeMode::FullUniverse, ConfigEvidenceTier::AuditGrade)
                .expect("full-universe audit grade"),
            GateEvidenceTier::AuditGrade
        );
        assert!(
            gate_evidence_tier(ScopeMode::FullUniverse, ConfigEvidenceTier::DecisionGrade).is_err()
        );
    }

    #[test]
    fn replay_policy_requires_explicit_verified_decision_grade_scope() {
        assert_eq!(
            v1_replay_certification_policy(
                ScopeMode::HybridPanel,
                ConfigEvidenceTier::DecisionGrade,
                true,
            )
            .expect("verified decision-grade selected scope"),
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope
        );
        assert!(v1_replay_certification_policy(
            ScopeMode::HybridPanel,
            ConfigEvidenceTier::DecisionGrade,
            false,
        )
        .is_err());
        assert_eq!(
            v1_replay_certification_policy(
                ScopeMode::FullUniverse,
                ConfigEvidenceTier::AuditGrade,
                false,
            )
            .expect("full-universe audit grade"),
            V1ReplayCertificationPolicyV1::AuditGrade
        );
    }

    #[test]
    fn provisional_program_binary_is_structurally_complete_not_certified() {
        let operation = operation_for_decoder("decoder-v1");
        let mut configuration = program_binary_configuration("true");

        assert!(config_fields_complete(&configuration, &operation));

        configuration
            .fields
            .insert("provisional".to_owned(), "not-a-bool".to_owned());
        assert!(!config_fields_complete(&configuration, &operation));

        configuration = program_binary_configuration("false");
        assert!(config_fields_complete(&configuration, &operation));

        configuration
            .fields
            .insert("idl_sha256".to_owned(), "unresolved".to_owned());
        assert!(!config_fields_complete(&configuration, &operation));
        configuration = program_binary_configuration("true");
        configuration.fields.insert(
            "program_binary_sha256".to_owned(),
            "not-a-deployed-hash".to_owned(),
        );
        assert!(!config_fields_complete(&configuration, &operation));
        assert!(!config_fields_complete(
            &program_binary_configuration("true"),
            &operation_for_decoder("another-decoder")
        ));
    }

    #[test]
    fn program_config_omits_an_unproven_deployed_binary_hash() {
        let registry = DecoderRegistryV1 {
            schema_version: crate::decode::DECODER_REGISTRY_SCHEMA_VERSION,
            revisions: vec![crate::decode::DecoderRevisionV1 {
                decoder_version_id: "decoder-v1".to_owned(),
                valid_from_slot: 1,
                valid_to_slot_exclusive: None,
                source_git_commit: "c".repeat(40),
                idl_sha256: "a".repeat(64),
                candidate_program_binary_sha256: "b".repeat(64),
                program_binary_sha256: None,
                provisional: true,
            }],
        };
        let configurations = decoder_config_versions(&registry);
        assert_eq!(configurations.len(), 1);
        let fields = &configurations[0].fields;
        assert_eq!(
            fields.get("candidate_program_binary_sha256"),
            Some(&"b".repeat(64))
        );
        assert!(!fields.contains_key("program_binary_sha256"));
        assert!(!fields.values().any(|value| value == "unresolved"));
    }
}

pub(crate) async fn audit(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    let inventory_path = context.run_dir().join(EXTRACTION_INVENTORY);
    let inventory: ExtractionInventoryV1 = read_json(&inventory_path)?;
    if inventory.run_id != context.run_id() || inventory.cutoff_slot != context.cutoff_slot() {
        bail!("extraction inventory belongs to a different immutable run");
    }
    let cohort_selection = validated_cohort_selection(context, config)?;
    let cohort_selection_sha256 = cohort_selection
        .as_ref()
        .map(|artifact| artifact.artifact_sha256.clone());
    let transfer_fee_artifact: TransferFeeProvenanceArtifactV1 =
        read_json(&context.data_dir().join(TRANSFER_FEE_PROVENANCE_JSON))?;
    transfer_fee_artifact
        .validate()
        .context("validating historical transfer-fee provenance")?;
    if transfer_fee_artifact.run_id != context.run_id()
        || transfer_fee_artifact.cutoff_slot != context.cutoff_slot()
    {
        bail!("transfer-fee provenance belongs to a different immutable run");
    }

    let expected_transactions = inventory
        .targeted_successful_program_signatures
        .saturating_add(u64::from(!inventory.signature_inventory_complete));
    let exact_transactions = inventory.successful_signatures_matched_in_blocks;
    let canonical_events_path = context.data_dir().join(CANONICAL_EVENTS_JSONL);
    let canonical_order = canonical_order_coverage(&canonical_events_path)?;
    let (source_reconciliation, reconciliation_path, reconciliation_summary_path) =
        reconcile_db_rpc_history(
            context.data_dir(),
            context.run_dir(),
            inventory.database_events_missing_authoritative_enrichment,
        )?;
    let canonical_operations_path = context.data_dir().join(CANONICAL_OPERATIONS_JSONL);
    let config_coverage = configuration_coverage(
        &context.data_dir().join(CONFIG_HISTORY_JSONL),
        &canonical_operations_path,
    )?;
    let config_coverage_path = context.run_dir().join(CONFIG_COVERAGE_SUMMARY);
    write_json(&config_coverage_path, &config_coverage)?;
    let price_coverage = historical_price_coverage(
        &context.data_dir().join(HISTORICAL_PRICES_JSONL),
        &canonical_operations_path,
        &config.source.historical_price_provider,
    )?;
    let price_coverage_path = context.run_dir().join(PRICE_COVERAGE_SUMMARY);
    write_json(&price_coverage_path, &price_coverage)?;
    let hard_gate_input = HardGateInput {
        transaction_inventory: CoverageCount {
            observed: exact_transactions,
            expected: expected_transactions,
        },
        state_changing_instruction_decode: CoverageCount {
            observed: inventory.decoded_state_changing_instructions,
            expected: inventory.expected_state_changing_instructions,
        },
        state_changing_decode_blockers: inventory
            .unclassified_target_instructions
            .saturating_add(inventory.transactions_with_incomplete_instruction_inventory)
            .saturating_add(inventory.block_decode_failures)
            .saturating_add(inventory.transaction_decode_failures)
            .saturating_add(inventory.transactions_with_incomplete_cpi_provenance),
        canonical_event_order: canonical_order,
        authoritative_block_time: canonical_order,
        versioned_config_coverage: config_coverage.coverage,
        historical_transfer_fee_coverage: CoverageCount {
            observed: transfer_fee_artifact.ready_mint_count(),
            expected: transfer_fee_artifact.expected_mint_count(),
        },
        transfer_fee_history_blockers: transfer_fee_artifact.blockers.len() as u64,
        db_rpc_raw_field_agreement: CoverageCount {
            observed: source_reconciliation.reconciled_records,
            expected: source_reconciliation.expected_records,
        },
        terminal_state_fields: CoverageCount {
            observed: 0,
            expected: inventory.selected_market_count,
        },
        economic_replay_events: CoverageCount {
            observed: 0,
            expected: inventory.canonical_operations,
        },
        unresolved_rpc_slots: inventory.unresolved_archival_blocks,
        reconciliations: Vec::new(),
        historical_price_coverage: Some(price_coverage.coverage),
    };
    let hard_gate_phase_input = HardGatePhaseInputV1 {
        extraction: hard_gate_input,
        decoder_revision_certification: CoverageCount {
            observed: u64::from(inventory.decoder_registry_headline_verified),
            expected: 1,
        },
    };
    let audit_phase_report = evaluate_hard_gates_for_phase_with_certification(
        &hard_gate_phase_input,
        HardGatePhase::ExtractionAudit,
    );
    let audit_phase_decision =
        assess_phase_gates(&audit_phase_report, phase_gate_policy(config, false)?)
            .context("assessing extraction/audit phase gates")?;
    let gates = audit_phase_report.report.clone();

    let market_rows: Vec<JsonValue> = read_json_lines(&context.data_dir().join(MARKETS_JSONL))?;
    let mut swaps = BTreeMap::<String, (u64, BTreeSet<i64>)>::new();
    let global_chain_gates_pass = inventory.signature_inventory_complete
        && inventory.unresolved_archival_blocks == 0
        && inventory.expected_state_changing_instructions
            == inventory.decoded_state_changing_instructions
        && inventory.unclassified_target_instructions == 0
        && inventory.transactions_with_incomplete_instruction_inventory == 0
        && inventory.transactions_with_incomplete_cpi_provenance == 0
        && inventory.unknown_program_instructions == 0
        && inventory.decode_failures == 0
        && inventory.normalization_gaps == 0;
    let global_non_mint_raw_gates_pass =
        global_chain_gates_pass && audit_phase_decision.raw_market_eligible;
    visit_json_lines::<EnrichedDbEventSeed, _>(
        &context.data_dir().join(ENRICHED_DB_EVENTS_JSONL),
        |record| {
            if record.db.event_kind == "swap" {
                let entry = swaps.entry(record.db.pair.clone()).or_default();
                entry.0 = entry.0.saturating_add(1);
                entry
                    .1
                    .insert(record.block_time_unix_seconds.div_euclid(86_400));
            }
            Ok(())
        },
    )?;
    let selected_scope: Option<BTreeSet<&str>> = cohort_selection
        .as_ref()
        .map(|cohort| cohort.selected_markets.iter().map(String::as_str).collect());
    let markets: Vec<MarketEvidence> = market_rows
        .iter()
        .map(|row| -> Result<Option<MarketEvidence>> {
            let pair = row
                .get("pair_address")
                .and_then(JsonValue::as_str)
                .context("market catalog row is missing pair_address")?
                .to_owned();
            if selected_scope
                .as_ref()
                .is_some_and(|selected| !selected.contains(pair.as_str()))
            {
                return Ok(None);
            }
            let visible = row
                .get("visible")
                .and_then(JsonValue::as_bool)
                .context("market catalog row is missing boolean visible")?;
            let (swap_count, active_days) = swaps
                .get(&pair)
                .map(|(count, days)| (*count, days.len() as u32))
                .unwrap_or_default();
            let mint_history_ready = transfer_fee_artifact.market_ready(&pair);
            Ok(Some(MarketEvidence {
                pair_address: pair,
                visible,
                // Until natural-grain per-market reconciliation is complete,
                // use the stricter global non-USD gates. This can defer a
                // market, but can never make one falsely eligible.
                hard_gates_pass: global_non_mint_raw_gates_pass && mint_history_ready,
                swap_count,
                active_swap_days: active_days,
                // Computed after exact V1 replay and historical hourly marks.
                median_time_weighted_tvl_usd_micros: None,
            }))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    let cohort_final_audit_qualification = cohort_selection
        .as_ref()
        .map(|cohort| cohort.qualify_after_final_audit(&markets))
        .transpose()
        .context("applying final RPC-backed cohort qualification")?;
    let cohort_final_panel_met = cohort_final_audit_qualification
        .as_ref()
        .is_none_or(|qualification| qualification.minimum_final_panel_met);
    let market_eligibility = visible_market_eligibility_with_policy(
        &markets,
        MarketEligibilityPolicy {
            minimum_swaps: config.universe.minimum_swaps,
            minimum_active_days: u32::try_from(config.universe.minimum_active_days)
                .context("minimum active days exceeds u32")?,
            tvl_percentile_bps: config.universe.median_tvl_percentile_bps,
        },
    );
    let visible_market_count = markets.iter().filter(|market| market.visible).count() as u64;
    let eligible_raw_market_count = market_eligibility
        .iter()
        .filter(|market| market.eligible)
        .count() as u64;
    let eligible_usd_headline_market_count = market_eligibility
        .iter()
        .filter(|market| market.eligible && market.usd_headline_eligible)
        .count() as u64;
    let factual_swap_count = markets.iter().map(|market| market.swap_count).sum();
    let mut limitations = vec![
        "Historical decoder revisions remain hard-gated until deployed program binaries are verified and dispatched to their exact implementations.".to_owned(),
        "Terminal account state and native V1 economic replay remain hard-gated until reconcile-v1 completes.".to_owned(),
        "Missing historical TVL marks preserve raw/invariant eligibility but exclude USD headlines.".to_owned(),
    ];
    if config.scope.mode == ScopeMode::HybridPanel {
        limitations.push(
            "DecisionGrade block targets are seeded from selected-market database signatures plus successful program signatures absent from every database event row. A transaction indexed only under a non-selected market could also contain a global side effect outside the scoped block inventory; only full-universe block replay rules out this mixed-transaction edge."
                .to_owned(),
        );
    }
    if inventory.catalog_unbound_database_event_rows > 0 {
        limitations.push(format!(
            "The full historical DB window contains {} event rows across {} market addresses absent from the current market catalog; {} rows across {} addresses occur in the sealed training prefix. Every unbound row and its checksum are retained in {}, they are excluded from visible-market cohort ranking, and this fixed-override DecisionGrade run makes no claim about those catalog-unbound markets.",
            inventory.catalog_unbound_database_event_rows,
            inventory.catalog_unbound_market_count,
            inventory.catalog_unbound_prefix_database_event_rows,
            inventory.catalog_unbound_prefix_market_count,
            COHORT_CATALOG_UNBOUND_DB_EVENTS_JSONL,
        ));
    }
    let artifact = DataQualityArtifactV1 {
        schema_version: 2,
        run_id: context.run_id().to_owned(),
        cutoff_slot: context.cutoff_slot(),
        hard_gate_phase_input,
        audit_phase_report,
        audit_phase_decision,
        cohort_selection_sha256,
        cohort_final_audit_qualification,
        gates,
        market_eligibility,
        visible_market_count,
        eligible_raw_market_count,
        eligible_usd_headline_market_count,
        factual_swap_count,
        limitations,
    };
    let path = context.run_dir().join("data-quality.json");
    write_json(&path, &artifact)?;
    if !cohort_final_panel_met {
        bail!("hybrid cohort falls below its minimum after final RPC-backed market audit");
    }
    artifact_map(&[
        ("data-quality.json", &path),
        (DB_RPC_RECONCILIATION_JSONL, &reconciliation_path),
        (DB_RPC_RECONCILIATION_SUMMARY, &reconciliation_summary_path),
        (CONFIG_COVERAGE_SUMMARY, &config_coverage_path),
        (PRICE_COVERAGE_SUMMARY, &price_coverage_path),
    ])
}

pub(crate) async fn reconcile_v1(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    let operations = context.data_dir().join("canonical-operations.jsonl");
    if !operations.exists() {
        bail!(
            "V1 reconciliation requires version-decoded canonical operations at {}; extraction coverage remains auditable but counterfactual evaluation is blocked",
            operations.display()
        );
    }
    let quality: DataQualityArtifactV1 = read_json(&context.run_dir().join("data-quality.json"))?;
    if quality.schema_version != 2
        || quality.run_id != context.run_id()
        || quality.cutoff_slot != context.cutoff_slot()
    {
        bail!("data-quality artifact belongs to a different immutable run");
    }
    let cohort_selection = validated_cohort_selection(context, config)?;
    let cohort_selection_sha256 = cohort_selection
        .as_ref()
        .map(|artifact| artifact.artifact_sha256.clone());
    if quality.cohort_selection_sha256 != cohort_selection_sha256 {
        bail!("data-quality artifact binds another cohort selection");
    }
    match (
        cohort_selection.as_ref(),
        quality.cohort_final_audit_qualification.as_ref(),
    ) {
        (Some(cohort), Some(qualification)) => {
            validate_cohort_final_qualification(cohort, qualification)?;
        }
        (None, None) if config.scope.mode == ScopeMode::FullUniverse => {}
        _ => bail!("data-quality artifact has inconsistent cohort final-audit evidence"),
    }
    let expected_audit_phase_report = evaluate_hard_gates_for_phase_with_certification(
        &quality.hard_gate_phase_input,
        HardGatePhase::ExtractionAudit,
    );
    let expected_audit_phase_decision = assess_phase_gates(
        &expected_audit_phase_report,
        phase_gate_policy(config, false)?,
    )
    .context("revalidating extraction/audit phase gates")?;
    if quality.audit_phase_report != expected_audit_phase_report
        || quality.gates != expected_audit_phase_report.report
        || quality.audit_phase_decision != expected_audit_phase_decision
    {
        bail!("data-quality artifact phase-gate evidence is internally inconsistent");
    }
    if !expected_audit_phase_decision.phase_eligible {
        bail!(
            "native V1 replay prerequisites failed: {}",
            blocking_gate_codes(&expected_audit_phase_decision)
        );
    }

    let replay_certification_policy = v1_replay_certification_policy(
        config.scope.mode,
        config.scope.evidence_tier,
        cohort_selection.is_some(),
    )?;

    let ledger = context.run_dir().join(V1_REPLAY_LEDGER_JSONL);
    let replay = reconcile_history_to_ledger_with_terminal(
        &operations,
        &context.data_dir().join(CONFIG_HISTORY_JSONL),
        &ledger,
        context.run_id(),
        context.cutoff_slot(),
        replay_certification_policy,
    )?;
    let mut reconciliation = replay.summary;
    let ledger_sha256 = sha256_file(&ledger)?;
    if reconciliation.ledger_sha256 != ledger_sha256 {
        bail!("factual V1 ledger checksum differs from its streaming commitment");
    }
    if !reconciliation.zero_raw_unit_difference || !reconciliation.all_conservation_identities_hold
    {
        bail!("native V1 replay failed raw-unit reconciliation or conservation");
    }

    let parquet_dir = context.data_dir().join("parquet");
    fs::create_dir_all(&parquet_dir)?;
    let ledger_parquet = parquet_dir.join("v1-replay-ledger.parquet");
    let ledger_parquet_manifest = parquet_dir.join("v1-replay-ledger.manifest.json");
    ensure_parquet_artifact(&ParquetExportSpecV1 {
        dataset: BenchmarkParquetDatasetV1::V1ReplayLedger,
        logical_name: BenchmarkParquetDatasetV1::V1ReplayLedger.logical_name(),
        source_jsonl: &ledger,
        parquet_output: &ledger_parquet,
        manifest_output: &ledger_parquet_manifest,
    })
    .context("exporting declared V1 replay ledger Parquet dataset")?;

    // An exact transition replay is necessary but not sufficient. The binding
    // is a separate input so a self-consistent snapshot from another run,
    // cutoff, program, provider, or incomplete account list cannot substitute.
    let terminal_binding_path = context.data_dir().join(TERMINAL_BINDING_JSON);
    let terminal_snapshot_path = context.data_dir().join(TERMINAL_SNAPSHOT_JSON);
    let terminal_reconciliation = match (
        terminal_binding_path.exists(),
        terminal_snapshot_path.exists(),
    ) {
        (true, true) => {
            let terminal_binding: TerminalSnapshotBindingV1 = read_json(&terminal_binding_path)?;
            let terminal_snapshot: TerminalAccountSnapshotV1 = read_json(&terminal_snapshot_path)?;
            let terminal_reconciliation = reconcile_terminal_snapshot(
                context.run_id(),
                context.cutoff_slot(),
                &terminal_binding,
                &terminal_snapshot,
                &replay.terminal_checkpoints,
            )
            .context("reconciling exact-cutoff V1 terminal accounts")?;
            terminal_reconciliation.validate()?;
            reconciliation.terminal_state_verified =
                terminal_reconciliation.terminal_state_verified;
            if !reconciliation.terminal_state_verified {
                bail!("exact-cutoff V1 terminal reconciliation did not verify equality");
            }
            Some(terminal_reconciliation)
        }
        (false, false) => None,
        _ => bail!(
            "terminal binding and snapshot must either both exist or both be absent: {}, {}",
            terminal_binding_path.display(),
            terminal_snapshot_path.display()
        ),
    };
    let terminal_reconciliation_path = terminal_reconciliation.as_ref().map(|artifact| {
        let path = context.run_dir().join(V1_TERMINAL_RECONCILIATION_JSON);
        (path, artifact)
    });
    if let Some((path, artifact)) = &terminal_reconciliation_path {
        write_json(path, artifact)?;
    }
    let reconciliation_path = context.run_dir().join(V1_RECONCILIATION_JSON);
    write_json(&reconciliation_path, &reconciliation)?;

    let mut final_phase_input = quality.hard_gate_phase_input.clone();
    if reconciliation.canonical_operation_count
        != final_phase_input.extraction.economic_replay_events.expected
    {
        bail!("native V1 replay operation count differs from audited extraction coverage");
    }
    final_phase_input.extraction.economic_replay_events = CoverageCount {
        observed: reconciliation.canonical_operation_count,
        expected: reconciliation.canonical_operation_count,
    };
    final_phase_input.extraction.terminal_state_fields =
        terminal_reconciliation.as_ref().map_or_else(
            || CoverageCount {
                observed: 0,
                expected: reconciliation.market_count.max(1),
            },
            |terminal| CoverageCount {
                observed: terminal.raw_unit_comparison_count,
                expected: terminal.raw_unit_comparison_count,
            },
        );
    let final_phase_report = evaluate_hard_gates_for_phase_with_certification(
        &final_phase_input,
        HardGatePhase::PostReplayFinal,
    );
    let exact_selected_scope_replay_proven = selected_scope_replay_is_exact(
        cohort_selection.as_ref(),
        &final_phase_report,
        &reconciliation,
    );
    let final_phase_decision = assess_phase_gates(
        &final_phase_report,
        phase_gate_policy(config, exact_selected_scope_replay_proven)?,
    )
    .context("assessing post-replay final gates")?;
    let final_phase_gate_path = context.run_dir().join(FINAL_PHASE_GATE_JSON);
    write_json(
        &final_phase_gate_path,
        &FinalPhaseGateArtifactV1 {
            schema_version: 1,
            run_id: context.run_id().to_owned(),
            cutoff_slot: context.cutoff_slot(),
            cohort_selection_sha256: cohort_selection_sha256.clone(),
            phase_report: final_phase_report,
            decision: final_phase_decision.clone(),
        },
    )?;
    if !final_phase_decision.phase_eligible
        || matches!(
            final_phase_decision.raw_certification,
            CertificationStatus::Pending | CertificationStatus::Fail
        )
    {
        bail!(
            "post-replay final gates block universe/calibration: {}",
            blocking_gate_codes(&final_phase_decision)
        );
    }

    let extraction_inventory: ExtractionInventoryV1 =
        read_json(&context.run_dir().join(EXTRACTION_INVENTORY))?;
    let market_rows: Vec<JsonValue> = read_json_lines(&context.data_dir().join(MARKETS_JSONL))?;
    let visible_by_market: BTreeMap<String, bool> = market_rows
        .into_iter()
        .map(|row| {
            let pair = row
                .get("pair_address")
                .and_then(JsonValue::as_str)
                .context("market catalog row lacks pair_address")?;
            let visible = row
                .get("visible")
                .and_then(JsonValue::as_bool)
                .context("market catalog row lacks boolean visible")?;
            Ok((pair.to_owned(), visible))
        })
        .collect::<Result<_>>()?;
    let hard_gates_by_market: BTreeMap<String, bool> = quality
        .market_eligibility
        .iter()
        .map(|market| {
            (
                market.pair_address.clone(),
                market.reason != EligibilityReason::HardGatesFailed,
            )
        })
        .collect();
    let universe = derive_universe_and_window(
        &operations,
        &ledger,
        &context.data_dir().join(HISTORICAL_PRICES_JSONL),
        &config.source.historical_price_provider,
        &visible_by_market,
        &hard_gates_by_market,
        extraction_inventory.cutoff_block_time_unix_seconds,
        MarketEligibilityPolicy {
            minimum_swaps: config.universe.minimum_swaps,
            minimum_active_days: u32::try_from(config.universe.minimum_active_days)?,
            tvl_percentile_bps: config.universe.median_tvl_percentile_bps,
        },
        config.window.common_market_fraction_bps,
        config.window.common_tvl_fraction_bps,
        config.calibration.training_bps,
        config.calibration.validation_bps,
        config.calibration.holdout_bps,
    )?;
    let universe_path = context.run_dir().join(UNIVERSE_WINDOW_JSON);
    write_json(&universe_path, &universe)?;
    let common_window = universe
        .common_window
        .as_ref()
        .context("exact historical evidence has no collective common window ending at cutoff")?;
    let splits = universe
        .chronological_splits
        .as_ref()
        .context("collective common window cannot be split chronologically")?;
    let common_window_path = context.run_dir().join(COMMON_WINDOW_JSON);
    let splits_path = context.run_dir().join(CHRONOLOGICAL_SPLITS_JSON);
    write_json(&common_window_path, common_window)?;
    write_json(&splits_path, splits)?;

    let calibration_dir = context.run_dir().join("calibration-inputs");
    let split_manifest_path = context.run_dir().join(DATASET_SPLIT_MANIFEST_JSON);
    let split_manifest = if split_manifest_path.exists() {
        let manifest: ChronologicalDatasetSplitManifestV1 = read_json(&split_manifest_path)?;
        manifest.verify(&calibration_dir)?;
        manifest
    } else {
        let manifest = create_chronological_calibration_prefixes(
            &operations,
            &ledger,
            &context.data_dir().join(HISTORICAL_PRICES_JSONL),
            &context.data_dir().join(TRANSFER_FEE_REGISTRY_JSON),
            &calibration_dir,
            splits,
        )?;
        write_json(&split_manifest_path, &manifest)?;
        manifest
    };
    if split_manifest.splits != *splits
        || split_manifest.source_operations_sha256 != sha256_file(&operations)?
        || split_manifest.source_factual_ledger_sha256 != sha256_file(&ledger)?
    {
        bail!("chronological calibration prefixes do not bind to current replay inputs");
    }

    let mut artifacts = vec![
        (V1_REPLAY_LEDGER_JSONL, ledger.as_path()),
        ("v1-replay-ledger.parquet", ledger_parquet.as_path()),
        (
            "v1-replay-ledger.manifest.json",
            ledger_parquet_manifest.as_path(),
        ),
        (V1_RECONCILIATION_JSON, reconciliation_path.as_path()),
        (FINAL_PHASE_GATE_JSON, final_phase_gate_path.as_path()),
        (UNIVERSE_WINDOW_JSON, universe_path.as_path()),
        (COMMON_WINDOW_JSON, common_window_path.as_path()),
        (CHRONOLOGICAL_SPLITS_JSON, splits_path.as_path()),
        (DATASET_SPLIT_MANIFEST_JSON, split_manifest_path.as_path()),
    ];
    if let Some((terminal_path, _)) = &terminal_reconciliation_path {
        artifacts.push((V1_TERMINAL_RECONCILIATION_JSON, terminal_path.as_path()));
    }
    artifact_map(&artifacts)
}

#[derive(Clone)]
struct TrainingProtocolEpochV1 {
    config_epoch_id: String,
    valid_from: ChainOrder,
    valid_to_exclusive: Option<ChainOrder>,
    interest_bps: u16,
}

fn build_training_protocol_interest_schedule(
    calibration_directory: &Path,
    manifest: &ChronologicalDatasetSplitManifestV1,
) -> Result<ProtocolInterestShareScheduleV1> {
    let operations_path =
        calibration_directory.join(&manifest.training_prefix.operations_file_name);
    let factual_ledger_path =
        calibration_directory.join(&manifest.training_prefix.factual_ledger_file_name);
    let operations: Vec<CanonicalOperationV1> = read_json_lines(&operations_path)?;
    let ledger: Vec<V1ReplayLedgerRowV1> = read_json_lines(&factual_ledger_path)?;

    let mut protocol_epochs = Vec::<TrainingProtocolEpochV1>::new();
    let mut current_interest_bps = None::<u16>;
    let mut operation_metadata = BTreeMap::<String, (ChainOrder, i64)>::new();
    let mut market_initializations = BTreeMap::<String, ChainOrder>::new();
    for operation in &operations {
        operation_metadata.insert(
            operation.operation_id.clone(),
            (operation.order, operation.block_time_unix_seconds),
        );
        if matches!(
            operation.payload,
            OperationPayloadV1::InitializeMarket { .. }
        ) {
            if market_initializations
                .insert(operation.pair.clone(), operation.order)
                .is_some()
            {
                bail!("training prefix initializes one market more than once");
            }
        }
        let OperationPayloadV1::UpdateConfig { fields } = &operation.payload else {
            continue;
        };
        let kind = fields
            .get("instruction_kind")
            .map(String::as_str)
            .context("training config operation lacks instruction_kind")?;
        if !matches!(
            kind,
            "init_futarchy_authority" | "update_futarchy_authority" | "update_protocol_revenue"
        ) {
            continue;
        }
        if let Some(raw) = fields
            .get("args.interest_bps")
            .filter(|value| value.as_str() != "null")
        {
            current_interest_bps = Some(
                raw.parse::<u16>()
                    .context("invalid training V1 protocol-interest bps")?,
            );
        }
        let interest_bps = current_interest_bps
            .context("protocol-revenue epoch precedes V1 interest-bps initialization")?;
        if let Some(previous) = protocol_epochs.last_mut() {
            previous.valid_to_exclusive = Some(operation.order);
        }
        protocol_epochs.push(TrainingProtocolEpochV1 {
            config_epoch_id: sha256_hex(
                format!(
                    "{}:{:?}:{}",
                    operation.operation_id,
                    ConfigScope::ProtocolRevenue,
                    "__global__"
                )
                .as_bytes(),
            ),
            valid_from: operation.order,
            valid_to_exclusive: None,
            interest_bps,
        });
    }
    if protocol_epochs.is_empty() || market_initializations.is_empty() {
        bail!("training prefix lacks protocol-revenue epochs or initialized markets");
    }

    let mut epochs = BTreeMap::<(String, String), ProtocolInterestTrainingEpochV1>::new();
    for (market, initialized_at) in &market_initializations {
        let first = protocol_epochs
            .iter()
            .position(|epoch| {
                epoch.valid_from <= *initialized_at
                    && epoch
                        .valid_to_exclusive
                        .is_none_or(|exclusive| *initialized_at < exclusive)
            })
            .with_context(|| {
                format!("market {market} initialized without an active protocol-revenue epoch")
            })?;
        for epoch in &protocol_epochs[first..] {
            epochs.insert(
                (market.clone(), epoch.config_epoch_id.clone()),
                ProtocolInterestTrainingEpochV1 {
                    key: ProtocolInterestEpochKeyV1 {
                        market: market.clone(),
                        config_epoch_id: epoch.config_epoch_id.clone(),
                    },
                    valid_from: epoch.valid_from,
                    valid_to_exclusive: epoch.valid_to_exclusive,
                    v1_interest_surcharge_bps: epoch.interest_bps,
                    samples: Vec::new(),
                },
            );
        }
    }

    for row in ledger {
        let (order, block_time) = operation_metadata
            .get(&row.operation_id)
            .copied()
            .with_context(|| {
                format!(
                    "training factual row {} has no canonical operation",
                    row.operation_id
                )
            })?;
        if order != row.order {
            bail!("training factual row order differs from canonical operation");
        }
        let active = protocol_epochs
            .iter()
            .find(|epoch| {
                epoch.valid_from <= row.order
                    && epoch
                        .valid_to_exclusive
                        .is_none_or(|exclusive| row.order < exclusive)
            })
            .context("training factual row has no active protocol-revenue epoch")?;
        let training_epoch = epochs
            .get_mut(&(row.market.clone(), active.config_epoch_id.clone()))
            .context("training factual row precedes its market initialization")?;
        if block_time >= manifest.splits.training_start_unix_seconds {
            if let Some(sample) = ProtocolInterestTrainingSampleV1::from_revenue_delta(
                row.operation_id,
                row.order,
                block_time,
                &row.revenue_delta,
            )? {
                training_epoch.samples.push(sample);
            }
        }
    }

    derive_protocol_interest_share_schedule(
        ProtocolInterestTrainingSourceV1 {
            operations_sha256: manifest.training_prefix.operations.sha256.clone(),
            factual_ledger_sha256: manifest.training_prefix.factual_ledger.sha256.clone(),
            training_start_unix_seconds: manifest.splits.training_start_unix_seconds,
            training_end_unix_seconds_exclusive: manifest
                .splits
                .training_end_unix_seconds_exclusive,
        },
        ProtocolInterestSideWeightsV1::default(),
        epochs.into_values().collect(),
    )
    .map_err(Into::into)
}

pub(crate) async fn calibrate(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    let calibration_dir = context.run_dir().join("calibration-inputs");
    let split_manifest_path = context.run_dir().join(DATASET_SPLIT_MANIFEST_JSON);
    let split_manifest: ChronologicalDatasetSplitManifestV1 = read_json(&split_manifest_path)?;
    split_manifest.verify(&calibration_dir)?;
    if split_manifest.holdout_opened_by_calibration {
        bail!("calibration split manifest indicates holdout access before freeze");
    }
    let protocol_interest_schedule =
        build_training_protocol_interest_schedule(&calibration_dir, &split_manifest)?;
    let protocol_interest_schedule_path = context.run_dir().join(PROTOCOL_INTEREST_SCHEDULE_JSON);
    write_json(
        &protocol_interest_schedule_path,
        &protocol_interest_schedule,
    )?;
    let universe: crate::universe::UniverseWindowArtifactV1 =
        read_json(&context.run_dir().join(UNIVERSE_WINDOW_JSON))?;
    let headline_markets: BTreeSet<String> = universe
        .market_eligibility
        .iter()
        .filter(|market| market.usd_headline_eligible)
        .map(|market| market.pair_address.clone())
        .collect();
    if headline_markets.is_empty() {
        bail!("no USD-headline-eligible market exists for global calibration");
    }

    let cache_dir = context.run_dir().join("calibration-cache");
    fs::create_dir_all(&cache_dir)?;
    let arms = enabled_replay_arms(config)?;
    let hlp_scenario = calibration_hlp_scenario(config)?;
    let domain = SearchDomainV1::default();
    let initial_profiles = explore_profiles(
        config.calibration.exploration_points,
        config.calibration.seed,
        domain,
    );
    let mut profiles_by_id: BTreeMap<String, DuskProfileV1> = initial_profiles
        .into_iter()
        .map(|profile| (profile.profile_id.clone(), profile))
        .collect();
    let mut training_by_id = BTreeMap::<String, CandidateScoreV1>::new();
    let initial_ids: Vec<String> = profiles_by_id.keys().cloned().collect();
    // Freeze the low-discrepancy base set before any market-specific local
    // refinement adds profiles to the shared, immutable candidate catalog.
    let mut per_market_base_profile_ids = initial_ids.clone();
    for (index, profile_id) in initial_ids.iter().enumerate() {
        let profile = profiles_by_id
            .get(profile_id)
            .expect("profile ID came from map");
        let evidence = score_calibration_profile(
            &calibration_dir,
            &cache_dir,
            &split_manifest,
            profile,
            &protocol_interest_schedule,
            CalibrationSplitV1::Training,
            &arms,
            hlp_scenario,
            &config.source.historical_price_provider,
            &headline_markets,
        )?;
        if let Some(evidence) = evidence {
            if let Some(score) = evidence.combined_score {
                training_by_id.insert(profile_id.clone(), score);
            }
        }
        if (index + 1) % 10 == 0 || index + 1 == initial_ids.len() {
            eprintln!(
                "calibrate: training initial profiles {}/{}",
                index + 1,
                initial_ids.len()
            );
        }
    }

    let first_shortlist_ids = calibration_shortlist(
        &training_by_id.values().cloned().collect::<Vec<_>>(),
        config.calibration.pareto_shortlist,
    );
    if first_shortlist_ids.is_empty() {
        bail!(
            "no training profile produced complete safety, quote, revenue, and capacity evidence"
        );
    }
    let refinement_seeds: Vec<DuskProfileV1> = first_shortlist_ids
        .iter()
        .filter_map(|profile_id| profiles_by_id.get(profile_id).cloned())
        .collect();
    let refined = refine_profiles(
        &refinement_seeds,
        config.calibration.local_refinement_rounds,
        domain,
    );
    let refined_ids: Vec<String> = refined
        .into_iter()
        .filter_map(|profile| {
            let profile_id = profile.profile_id.clone();
            profiles_by_id
                .insert(profile_id.clone(), profile)
                .is_none()
                .then_some(profile_id)
        })
        .collect();
    for (index, profile_id) in refined_ids.iter().enumerate() {
        let profile = profiles_by_id
            .get(profile_id)
            .expect("refined profile ID came from map");
        let evidence = score_calibration_profile(
            &calibration_dir,
            &cache_dir,
            &split_manifest,
            profile,
            &protocol_interest_schedule,
            CalibrationSplitV1::Training,
            &arms,
            hlp_scenario,
            &config.source.historical_price_provider,
            &headline_markets,
        )?;
        if let Some(evidence) = evidence {
            if let Some(score) = evidence.combined_score {
                training_by_id.insert(profile_id.clone(), score);
            }
        }
        if (index + 1) % 10 == 0 || index + 1 == refined_ids.len() {
            eprintln!(
                "calibrate: training refined profiles {}/{}",
                index + 1,
                refined_ids.len()
            );
        }
    }
    per_market_base_profile_ids.extend(refined_ids);
    per_market_base_profile_ids.sort();
    per_market_base_profile_ids.dedup();

    let validation_ids = calibration_shortlist(
        &training_by_id.values().cloned().collect::<Vec<_>>(),
        config.calibration.pareto_shortlist,
    );
    let mut validation_by_id = BTreeMap::<String, CandidateScoreV1>::new();
    for (index, profile_id) in validation_ids.iter().enumerate() {
        let profile = profiles_by_id
            .get(profile_id)
            .context("shortlisted profile is absent from deterministic candidate set")?;
        let evidence = score_calibration_profile(
            &calibration_dir,
            &cache_dir,
            &split_manifest,
            profile,
            &protocol_interest_schedule,
            CalibrationSplitV1::Validation,
            &arms,
            hlp_scenario,
            &config.source.historical_price_provider,
            &headline_markets,
        )?;
        if let Some(evidence) = evidence {
            if let Some(score) = evidence.combined_score {
                validation_by_id.insert(profile_id.clone(), score);
            }
        }
        eprintln!(
            "calibrate: validation profiles {}/{}",
            index + 1,
            validation_ids.len()
        );
    }

    let training_path = context.run_dir().join("training-scores.json");
    let validation_path = context.run_dir().join("validation-scores.json");
    let training_scores: Vec<CandidateScoreV1> = training_by_id.values().cloned().collect();
    let validation_scores: Vec<CandidateScoreV1> = validation_by_id.values().cloned().collect();
    write_json(&training_path, &training_scores)?;
    write_json(&validation_path, &validation_scores)?;

    // The validation prefix contains training+validation and physically omits
    // holdout rows. Qualification therefore cannot leak the sealed final 20%.
    let per_market_qualification = qualify_calibration_markets(
        &calibration_dir.join(&split_manifest.validation_prefix.operations_file_name),
        &split_manifest.splits,
        &headline_markets,
        config.calibration.minimum_optimized_swaps_per_split,
        config.calibration.minimum_optimized_active_days_per_split,
    )?;
    per_market_qualification.verify()?;
    let per_market_qualification_path = context
        .run_dir()
        .join("per-market-calibration-qualification.json");
    write_json(&per_market_qualification_path, &per_market_qualification)?;
    let computed_per_market = if config.scope.per_market_opportunity_ceiling {
        Some(calibrate_per_market_profiles(
            &calibration_dir,
            &cache_dir,
            &split_manifest,
            &per_market_qualification,
            &per_market_base_profile_ids,
            &mut profiles_by_id,
            &protocol_interest_schedule,
            &arms,
            hlp_scenario,
            &config.source.historical_price_provider,
            config.calibration.pareto_shortlist,
            config.calibration.local_refinement_rounds,
            domain,
            &headline_markets,
        )?)
    } else {
        None
    };
    let (per_market_training, per_market_validation, per_market_selected, per_market_pareto) =
        apply_per_market_opportunity_ceiling_policy(
            config.scope.per_market_opportunity_ceiling,
            computed_per_market,
        )?;
    let per_market_training_path = context.run_dir().join("per-market-training-scores.json");
    let per_market_validation_path = context.run_dir().join("per-market-validation-scores.json");
    write_json(&per_market_training_path, &per_market_training)?;
    write_json(&per_market_validation_path, &per_market_validation)?;
    let rejection_manifest = collect_calibration_profile_rejections(&cache_dir)?;
    let rejection_manifest_path = context.run_dir().join(CALIBRATION_PROFILE_REJECTIONS_JSON);
    write_json(&rejection_manifest_path, &rejection_manifest)?;
    let joint_scores: Vec<CandidateScoreV1> = training_by_id
        .values()
        .filter(|training| training.passes_gates())
        .filter_map(|training| validation_by_id.get(&training.profile_id).cloned())
        .collect();
    let winner = select_global_winner(&joint_scores);
    let selected_profile_id = winner.map(|score| score.profile_id.clone());
    let pareto_profile_ids = pareto_frontier_ids(&validation_scores);
    let freeze = ProfileFreezeV1 {
        schema_version: crate::calibrate::PROFILE_FREEZE_SCHEMA_VERSION,
        dataset_id: sha256_file(&context.run_dir().join(EXTRACTION_INVENTORY))?,
        config_sha256: context.config_sha256().to_owned(),
        dusk_source_sha256: source_fingerprints(context)?.dusk_source_sha256,
        protocol_interest_schedule_sha256: protocol_interest_schedule.schedule_sha256.clone(),
        hlp_scenario,
        selected_profile_id,
        pareto_profile_ids,
        training_metrics_sha256: sha256_file(&training_path)?,
        validation_metrics_sha256: sha256_file(&validation_path)?,
        per_market_qualification_sha256: sha256_file(&per_market_qualification_path)?,
        per_market_training_metrics_sha256: sha256_file(&per_market_training_path)?,
        per_market_validation_metrics_sha256: sha256_file(&per_market_validation_path)?,
        per_market_selected_profile_ids: per_market_selected,
        per_market_pareto_profile_ids: per_market_pareto,
        holdout_read_before_freeze: false,
        freeze_sha256: String::new(),
    }
    .seal()?;
    let profiles_path = context.run_dir().join("candidate-profiles.json");
    let freeze_path = context.run_dir().join("profile-freeze.json");
    let profiles: Vec<DuskProfileV1> = profiles_by_id.into_values().collect();
    write_json(&profiles_path, &profiles)?;
    write_json(&freeze_path, &freeze)?;
    artifact_map(&[
        ("candidate-profiles.json", &profiles_path),
        ("training-scores.json", &training_path),
        ("validation-scores.json", &validation_path),
        (
            "per-market-calibration-qualification.json",
            &per_market_qualification_path,
        ),
        ("per-market-training-scores.json", &per_market_training_path),
        (
            "per-market-validation-scores.json",
            &per_market_validation_path,
        ),
        (
            CALIBRATION_PROFILE_REJECTIONS_JSON,
            &rejection_manifest_path,
        ),
        ("profile-freeze.json", &freeze_path),
        (
            PROTOCOL_INTEREST_SCHEDULE_JSON,
            &protocol_interest_schedule_path,
        ),
        (DATASET_SPLIT_MANIFEST_JSON, &split_manifest_path),
    ])
}

fn calibration_rejection_cache_path(
    cache_directory: &Path,
    split: CalibrationSplitV1,
    profile_id: &str,
    hlp_scenario: HlpScenario,
) -> PathBuf {
    cache_directory.join(format!(
        "{}-{}-{}.rejection.json",
        calibration_split_name(split),
        profile_id,
        hlp_scenario_name(hlp_scenario),
    ))
}

#[allow(clippy::too_many_arguments)]
fn classify_profile_geometry_rejection(
    error: &anyhow::Error,
    split: CalibrationSplitV1,
    profile_id: &str,
    protocol_interest_schedule_sha256: &str,
    operations_sha256: &str,
    factual_ledger_sha256: &str,
    transfer_fee_registry_sha256: &str,
    included_markets_sha256: &str,
    included_market_count: u64,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
) -> Result<Option<CalibrationProfileRejectionV1>> {
    for cause in error.chain() {
        let Some(anchor_error) = cause.downcast_ref::<anchor_lang::error::Error>() else {
            continue;
        };
        let anchor_lang::error::Error::AnchorError(anchor_error) = anchor_error else {
            continue;
        };
        let Some(anchor_lang::error::ErrorOrigin::Source(source)) = &anchor_error.error_origin
        else {
            continue;
        };
        if anchor_error.error_code_number != INVALID_MARKET_CONFIG_ERROR_CODE
            || anchor_error.error_name != "InvalidMarketConfig"
            || !source.filename.ends_with(EXPLICIT_CURVE_SOURCE_SUFFIX)
        {
            continue;
        }
        return CalibrationProfileRejectionV1 {
            schema_version: 1,
            split,
            profile_id: profile_id.to_owned(),
            protocol_interest_schedule_sha256: protocol_interest_schedule_sha256.to_owned(),
            operations_sha256: operations_sha256.to_owned(),
            factual_ledger_sha256: factual_ledger_sha256.to_owned(),
            transfer_fee_registry_sha256: transfer_fee_registry_sha256.to_owned(),
            included_markets_sha256: included_markets_sha256.to_owned(),
            included_market_count,
            arm,
            hlp_scenario,
            error_name: anchor_error.error_name.clone(),
            error_code_number: anchor_error.error_code_number,
            error_message: anchor_error.error_msg.clone(),
            source_file: EXPLICIT_CURVE_SOURCE_SUFFIX.to_owned(),
            source_line: source.line,
            rejection_sha256: String::new(),
        }
        .seal()
        .map(Some);
    }
    Ok(None)
}

fn remove_owned_calibration_scratch(counterfactual_path: &Path, paired_path: &Path) -> Result<()> {
    for path in [
        counterfactual_path.to_path_buf(),
        counterfactual_path.with_extension("jsonl.partial"),
        paired_path.to_path_buf(),
        paired_path.with_extension("jsonl.partial"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("removing owned calibration scratch {}", path.display())
                })
            }
        }
    }
    Ok(())
}

fn collect_calibration_profile_rejections(
    cache_directory: &Path,
) -> Result<CalibrationProfileRejectionManifestV1> {
    let mut rejections = Vec::new();
    for entry in fs::read_dir(cache_directory)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file()
            || !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".rejection.json"))
        {
            continue;
        }
        let rejection: CalibrationProfileRejectionV1 = read_json(&path)?;
        rejection.verify()?;
        rejections.push(rejection);
    }
    let manifest = CalibrationProfileRejectionManifestV1 {
        schema_version: 1,
        rejections,
        manifest_sha256: String::new(),
    }
    .seal()?;
    manifest.verify()?;
    Ok(manifest)
}

fn score_calibration_profile(
    calibration_directory: &Path,
    cache_directory: &Path,
    manifest: &ChronologicalDatasetSplitManifestV1,
    profile: &DuskProfileV1,
    protocol_interest_schedule: &ProtocolInterestShareScheduleV1,
    split: CalibrationSplitV1,
    arms: &[ReplayArm],
    hlp_scenario: HlpScenario,
    historical_price_provider: &str,
    headline_markets: &BTreeSet<String>,
) -> Result<Option<CalibrationProfileEvidenceV1>> {
    protocol_interest_schedule.verify()?;
    if protocol_interest_schedule.source.operations_sha256
        != manifest.training_prefix.operations.sha256
        || protocol_interest_schedule.source.factual_ledger_sha256
            != manifest.training_prefix.factual_ledger.sha256
        || protocol_interest_schedule
            .source
            .training_start_unix_seconds
            != manifest.splits.training_start_unix_seconds
        || protocol_interest_schedule
            .source
            .training_end_unix_seconds_exclusive
            != manifest.splits.training_end_unix_seconds_exclusive
    {
        bail!("protocol-interest schedule belongs to another training prefix");
    }
    let profile_path = cache_directory.join(format!(
        "{}-{}-{}.profile.json",
        calibration_split_name(split),
        profile.profile_id,
        hlp_scenario_name(hlp_scenario),
    ));
    if profile_path.exists() {
        let cached: CalibrationProfileEvidenceV1 = read_json(&profile_path)?;
        if cached.profile_id != profile.profile_id
            || cached.split != split
            || cached.hlp_scenario != hlp_scenario
            || cached.protocol_interest_schedule_sha256
                != protocol_interest_schedule.schedule_sha256
            || cached.arms.len() != arms.len()
        {
            bail!("cached calibration profile evidence has the wrong identity");
        }
        for arm in &cached.arms {
            arm.verify()?;
            if arm.score_evidence.included_markets_sha256 != market_set_sha256(headline_markets)
                || arm.score_evidence.included_market_count
                    != u64::try_from(headline_markets.len())?
            {
                bail!("cached calibration profile uses another eligible-market set");
            }
        }
        let (combined_score, blocking_reasons) = combine_calibration_arms(&cached.arms)?;
        if combined_score != cached.combined_score || blocking_reasons != cached.blocking_reasons {
            bail!("cached calibration profile aggregation differs from arm evidence");
        }
        return Ok(Some(cached));
    }

    let prefix = match split {
        CalibrationSplitV1::Training => &manifest.training_prefix,
        CalibrationSplitV1::Validation => &manifest.validation_prefix,
    };
    let transfer_registry: TransferFeeRegistryV1 =
        read_json(&calibration_directory.join(&prefix.transfer_fee_registry_file_name))?;
    let transfer_registry = transfer_registry.validate()?;
    let transfer_fee_registry_sha256 =
        sha256_file(&calibration_directory.join(&prefix.transfer_fee_registry_file_name))?;
    let included_markets_sha256 = market_set_sha256(headline_markets);
    let included_market_count = u64::try_from(headline_markets.len())?;
    let rejection_path =
        calibration_rejection_cache_path(cache_directory, split, &profile.profile_id, hlp_scenario);
    if rejection_path.exists() {
        let rejection: CalibrationProfileRejectionV1 = read_json(&rejection_path)?;
        rejection.verify()?;
        if rejection.split != split
            || rejection.profile_id != profile.profile_id
            || rejection.protocol_interest_schedule_sha256
                != protocol_interest_schedule.schedule_sha256
            || rejection.operations_sha256 != prefix.operations.sha256
            || rejection.factual_ledger_sha256 != prefix.factual_ledger.sha256
            || rejection.transfer_fee_registry_sha256 != transfer_fee_registry_sha256
            || rejection.included_markets_sha256 != included_markets_sha256
            || rejection.included_market_count != included_market_count
            || rejection.hlp_scenario != hlp_scenario
            || !arms.contains(&rejection.arm)
        {
            bail!("cached calibration rejection has the wrong immutable identity");
        }
        return Ok(None);
    }
    let (window_start, window_end) = match split {
        CalibrationSplitV1::Training => (
            manifest.splits.training_start_unix_seconds,
            manifest.splits.training_end_unix_seconds_exclusive,
        ),
        CalibrationSplitV1::Validation => (
            manifest.splits.validation_start_unix_seconds,
            manifest.splits.validation_end_unix_seconds_exclusive,
        ),
    };
    let scratch_directory = cache_directory.join("scratch");
    fs::create_dir_all(&scratch_directory)?;
    let mut arm_evidence = Vec::with_capacity(arms.len());
    for arm in arms {
        let arm_cache_path = cache_directory.join(format!(
            "{}-{}-{}-{}.arm.json",
            calibration_split_name(split),
            profile.profile_id,
            replay_arm_name(*arm),
            hlp_scenario_name(hlp_scenario),
        ));
        let evidence = if arm_cache_path.exists() {
            let cached: CalibrationArmEvidenceV1 = read_json(&arm_cache_path)?;
            cached.verify()?;
            if cached.split != split
                || cached.profile_id != profile.profile_id
                || cached.arm != *arm
                || cached.hlp_scenario != hlp_scenario
                || cached.protocol_interest_schedule_sha256
                    != protocol_interest_schedule.schedule_sha256
                || cached.score_evidence.included_markets_sha256
                    != market_set_sha256(headline_markets)
                || cached.score_evidence.included_market_count
                    != u64::try_from(headline_markets.len())?
            {
                bail!("cached calibration arm evidence has the wrong identity");
            }
            cached
        } else {
            let tag = format!(
                "{}-{}-{}-{}",
                calibration_split_name(split),
                profile.profile_id,
                replay_arm_name(*arm),
                hlp_scenario_name(hlp_scenario),
            );
            let counterfactual_path = scratch_directory.join(format!("counterfactual-{tag}.jsonl"));
            let paired_path = scratch_directory.join(format!("paired-{tag}.jsonl"));
            if counterfactual_path.exists()
                || paired_path.exists()
                || counterfactual_path.with_extension("jsonl.partial").exists()
                || paired_path.with_extension("jsonl.partial").exists()
            {
                bail!("incomplete calibration scratch exists for {tag}");
            }
            let mut metric_writer = PairedMetricLedgerWriterV1::create(
                &paired_path,
                &profile.profile_id,
                hlp_scenario,
            )?;
            let counterfactual = match evaluate_profile_history_observed(
                &calibration_directory.join(&prefix.operations_file_name),
                &calibration_directory.join(&prefix.factual_ledger_file_name),
                &counterfactual_path,
                profile,
                protocol_interest_schedule,
                *arm,
                hlp_scenario,
                &transfer_registry,
                Some(&mut metric_writer),
            ) {
                Ok(counterfactual) => counterfactual,
                Err(error) => {
                    drop(metric_writer);
                    remove_owned_calibration_scratch(&counterfactual_path, &paired_path)?;
                    if let Some(rejection) = classify_profile_geometry_rejection(
                        &error,
                        split,
                        &profile.profile_id,
                        &protocol_interest_schedule.schedule_sha256,
                        &prefix.operations.sha256,
                        &prefix.factual_ledger.sha256,
                        &transfer_fee_registry_sha256,
                        &included_markets_sha256,
                        included_market_count,
                        *arm,
                        hlp_scenario,
                    )? {
                        rejection.verify()?;
                        write_json(&rejection_path, &rejection)?;
                        eprintln!(
                            "calibrate: rejected profile {} for exact Dusk curve geometry",
                            profile.profile_id
                        );
                        return Ok(None);
                    }
                    return Err(error);
                }
            };
            let paired_metrics = metric_writer.commit()?;
            if counterfactual.records_sha256 != sha256_file(&counterfactual_path)?
                || paired_metrics.ledger_sha256 != sha256_file(&paired_path)?
            {
                bail!("calibration replay commitment differs from committed scratch bytes");
            }
            let mut score_evidence = aggregate_candidate_score_for_markets(
                &paired_path,
                &paired_metrics,
                &calibration_directory.join(&prefix.historical_prices_file_name),
                historical_price_provider,
                &profile.profile_id,
                *arm,
                window_start,
                window_end,
                Some(headline_markets),
            )?;
            if !counterfactual.complete_for_scoring {
                score_evidence
                    .blocking_reasons
                    .insert("counterfactual_replay_deferred_external_state".to_owned());
                score_evidence.candidate_score = None;
            }
            let evidence = CalibrationArmEvidenceV1 {
                schema_version: 2,
                split,
                profile_id: profile.profile_id.clone(),
                protocol_interest_schedule_sha256: protocol_interest_schedule
                    .schedule_sha256
                    .clone(),
                arm: *arm,
                hlp_scenario,
                counterfactual,
                paired_metrics,
                score_evidence,
                evidence_sha256: String::new(),
            }
            .seal()?;
            evidence.verify()?;
            write_json(&arm_cache_path, &evidence)?;
            fs::remove_file(&counterfactual_path).with_context(|| {
                format!(
                    "removing owned calibration scratch {}",
                    counterfactual_path.display()
                )
            })?;
            fs::remove_file(&paired_path).with_context(|| {
                format!(
                    "removing owned calibration scratch {}",
                    paired_path.display()
                )
            })?;
            evidence
        };
        arm_evidence.push(evidence);
    }
    let (combined_score, blocking_reasons) = combine_calibration_arms(&arm_evidence)?;
    let evidence = CalibrationProfileEvidenceV1 {
        schema_version: 2,
        split,
        profile_id: profile.profile_id.clone(),
        protocol_interest_schedule_sha256: protocol_interest_schedule.schedule_sha256.clone(),
        hlp_scenario,
        arms: arm_evidence,
        combined_score,
        blocking_reasons,
    };
    write_json(&profile_path, &evidence)?;
    Ok(Some(evidence))
}

type PerMarketScoreTableV1 = BTreeMap<String, Vec<CandidateScoreV1>>;
type PerMarketCalibrationOutputsV1 = (
    PerMarketScoreTableV1,
    PerMarketScoreTableV1,
    BTreeMap<String, String>,
    BTreeMap<String, Vec<String>>,
);

/// Enforces the scope flag at the boundary before per-market artifacts and the
/// profile freeze are written. `None` is the only accepted disabled input, so
/// a stale or accidentally computed optimized-profile claim fails closed.
fn apply_per_market_opportunity_ceiling_policy(
    enabled: bool,
    computed: Option<PerMarketCalibrationOutputsV1>,
) -> Result<PerMarketCalibrationOutputsV1> {
    match (enabled, computed) {
        (true, Some(outputs)) => Ok(outputs),
        (true, None) => bail!("enabled per-market opportunity ceiling has no calibration output"),
        (false, None) => Ok((
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        )),
        (false, Some(_)) => {
            bail!("disabled per-market opportunity ceiling received calibration output")
        }
    }
}

#[cfg(test)]
mod calibration_profile_rejection_tests {
    use anchor_lang::error::{AnchorError, Error as AnchorProgramError, ErrorOrigin, Source};

    use super::*;

    fn feature_on_reference_profile() -> DuskProfileV1 {
        use dusk::constants::{MIN_HALF_LIFE_MS, NAD};

        let mut profile = DuskProfileV1 {
            schema_version: crate::calibrate::PROFILE_SCHEMA_VERSION,
            profile_id: String::new(),
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
            center_ema_half_life_ms: MIN_HALF_LIFE_MS,
            adjustment_threshold_nad: NAD / 100,
            adjustment_step_nad: NAD / 1_000,
            min_adjustment_interval_slots: 1,
            divergence_fee_share_cap_bps: 2_000,
            volatility_fee_share_cap_bps: 2_000,
            divergence_fee_coefficient_nad: 10 * NAD,
            volatility_fee_coefficient_nad: NAD / 10,
            volatility_half_life_ms: MIN_HALF_LIFE_MS,
            volatility_shock_cap_nad: NAD / 10,
            volatility_accumulator_cap_nad: NAD,
            compounding_fee_bps: 10_000,
            target_utilization_bps: dusk::state::DEFAULT_IRM_TARGET_UTILIZATION_BPS,
            irm_curve_steepness_nad: dusk::state::DEFAULT_IRM_CURVE_STEEPNESS_NAD,
            irm_adjustment_speed_per_year: dusk::state::DEFAULT_IRM_ADJUSTMENT_SPEED_PER_YEAR,
        };
        profile.recompute_id();
        profile
    }

    fn anchor_failure(code: u32, source_file: &'static str) -> anyhow::Error {
        anyhow::Error::new(AnchorProgramError::AnchorError(Box::new(AnchorError {
            error_name: "InvalidMarketConfig".to_owned(),
            error_code_number: code,
            error_msg: "Invalid market config".to_owned(),
            error_origin: Some(ErrorOrigin::Source(Source {
                filename: source_file,
                line: 1137,
            })),
            compared_values: None,
        })))
        .context("counterfactual replay")
    }

    fn classify(error: &anyhow::Error) -> Option<CalibrationProfileRejectionV1> {
        classify_profile_geometry_rejection(
            error,
            CalibrationSplitV1::Training,
            &"a".repeat(64),
            &"b".repeat(64),
            &"c".repeat(64),
            &"d".repeat(64),
            &"e".repeat(64),
            &"f".repeat(64),
            5,
            ReplayArm::FixedFlow,
            HlpScenario::MaximumFeasible,
        )
        .unwrap()
    }

    #[test]
    fn only_exact_explicit_curve_geometry_failure_is_a_profile_rejection() {
        let rejection = classify(&anchor_failure(
            INVALID_MARKET_CONFIG_ERROR_CODE,
            "/checkout/programs/dusk/src/math/explicit_curve.rs",
        ))
        .expect("exact geometry failure is rejectable");
        rejection.verify().unwrap();
        assert_eq!(
            rejection.error_code_number,
            INVALID_MARKET_CONFIG_ERROR_CODE
        );
        assert_eq!(rejection.source_file, EXPLICIT_CURVE_SOURCE_SUFFIX);

        assert!(classify(&anchor_failure(
            INVALID_MARKET_CONFIG_ERROR_CODE + 1,
            "/checkout/programs/dusk/src/math/explicit_curve.rs",
        ))
        .is_none());
        assert!(classify(&anchor_failure(
            INVALID_MARKET_CONFIG_ERROR_CODE,
            "/checkout/programs/dusk/src/state/market.rs",
        ))
        .is_none());
    }

    #[test]
    #[ignore = "requires an explicit sealed benchmark run directory"]
    fn diagnose_live_executable_profile_score() {
        let run_directory = PathBuf::from(std::env::var("DUSK_DIAG_RUN_DIR").unwrap());
        let cache_directory = PathBuf::from(std::env::var("DUSK_DIAG_CACHE_DIR").unwrap());
        fs::create_dir_all(&cache_directory).unwrap();
        let calibration_directory = run_directory.join("calibration-inputs");
        let manifest: ChronologicalDatasetSplitManifestV1 =
            read_json(&run_directory.join(DATASET_SPLIT_MANIFEST_JSON)).unwrap();
        let schedule: ProtocolInterestShareScheduleV1 =
            read_json(&run_directory.join(PROTOCOL_INTEREST_SCHEDULE_JSON)).unwrap();
        let universe: crate::universe::UniverseWindowArtifactV1 =
            read_json(&run_directory.join(UNIVERSE_WINDOW_JSON)).unwrap();
        let mut headline_markets = universe
            .market_eligibility
            .iter()
            .filter(|market| market.usd_headline_eligible)
            .map(|market| market.pair_address.clone())
            .collect::<BTreeSet<_>>();
        if let Ok(requested) = std::env::var("DUSK_DIAG_MARKETS") {
            let requested = requested
                .split(',')
                .map(str::trim)
                .filter(|market| !market.is_empty())
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
            assert!(!requested.is_empty(), "DUSK_DIAG_MARKETS is empty");
            assert!(
                requested.is_subset(&headline_markets),
                "DUSK_DIAG_MARKETS contains a market outside the sealed headline set"
            );
            headline_markets = requested;
        }
        let profile = match std::env::var("DUSK_DIAG_PROFILE").as_deref() {
            Ok("program-defaults") => DuskProfileV1::program_defaults(),
            Ok("feature-on-reference") => feature_on_reference_profile(),
            Ok(other) => panic!("unsupported DUSK_DIAG_PROFILE {other}"),
            Err(_) => explore_profiles(32, 20_260_820, SearchDomainV1::default())
                .into_iter()
                .find(|profile| {
                    profile.profile_id
                        == "d36b6a5465308a79cb68c7fa815eca2ef41ad688d31cfe6610817cdf390e0d3b"
                })
                .unwrap(),
        };
        let evidence = score_calibration_profile(
            &calibration_directory,
            &cache_directory,
            &manifest,
            &profile,
            &schedule,
            CalibrationSplitV1::Training,
            &[ReplayArm::FixedFlow, ReplayArm::Intent],
            HlpScenario::MaximumFeasible,
            "birdeye",
            &headline_markets,
        )
        .unwrap()
        .expect("known executable profile must not be rejected");
        eprintln!(
            "live profile {} score available={} blocking={:?}",
            evidence.profile_id,
            evidence.combined_score.is_some(),
            evidence.blocking_reasons
        );
    }
}

#[cfg(test)]
mod per_market_calibration_policy_tests {
    use super::*;

    fn claimed_outputs() -> PerMarketCalibrationOutputsV1 {
        (
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::from([("market".to_owned(), "optimized-profile".to_owned())]),
            BTreeMap::from([("market".to_owned(), vec!["optimized-profile".to_owned()])]),
        )
    }

    #[test]
    fn disabled_ceiling_emits_empty_artifacts_and_rejects_any_computed_claim() {
        let (training, validation, selected, pareto) =
            apply_per_market_opportunity_ceiling_policy(false, None).unwrap();
        assert!(training.is_empty());
        assert!(validation.is_empty());
        assert!(selected.is_empty());
        assert!(pareto.is_empty());

        let error = apply_per_market_opportunity_ceiling_policy(false, Some(claimed_outputs()))
            .expect_err("disabled policy must reject optimized-profile output");
        assert_eq!(
            error.to_string(),
            "disabled per-market opportunity ceiling received calibration output"
        );
    }

    #[test]
    fn enabled_ceiling_preserves_full_universe_outputs() {
        let outputs =
            apply_per_market_opportunity_ceiling_policy(true, Some(claimed_outputs())).unwrap();
        assert_eq!(
            outputs.2.get("market").map(String::as_str),
            Some("optimized-profile")
        );
        assert_eq!(outputs.3["market"], vec!["optimized-profile".to_owned()]);
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn calibrate_per_market_profiles(
    calibration_directory: &Path,
    cache_directory: &Path,
    manifest: &ChronologicalDatasetSplitManifestV1,
    qualification: &PerMarketCalibrationQualificationArtifactV1,
    base_profile_ids: &[String],
    profiles_by_id: &mut BTreeMap<String, DuskProfileV1>,
    protocol_interest_schedule: &ProtocolInterestShareScheduleV1,
    arms: &[ReplayArm],
    hlp_scenario: HlpScenario,
    historical_price_provider: &str,
    pareto_shortlist: usize,
    local_refinement_rounds: usize,
    domain: SearchDomainV1,
    headline_markets: &BTreeSet<String>,
) -> Result<PerMarketCalibrationOutputsV1> {
    qualification.verify()?;
    if headline_markets.is_empty() {
        bail!("per-market calibration requires the global headline market set");
    }
    let eligible: Vec<String> = qualification
        .markets
        .iter()
        .filter(|market| market.calibration_eligible)
        .map(|market| market.market.clone())
        .collect();
    if eligible
        .iter()
        .any(|market| !headline_markets.contains(market))
    {
        bail!("per-market qualification contains a non-headline market");
    }

    // One profile replay produces independently gated evidence for every
    // market. Keep only this projected score table here: creating a singleton
    // market cache would repeat the same state transitions once per market.
    let mut training = eligible
        .iter()
        .map(|market| (market.clone(), BTreeMap::<String, CandidateScoreV1>::new()))
        .collect::<BTreeMap<_, _>>();
    let mut evaluated_training_profiles = BTreeSet::new();
    for (index, profile_id) in base_profile_ids.iter().enumerate() {
        let profile = profiles_by_id
            .get(profile_id)
            .context("base profile disappeared during per-market calibration")?;
        let evidence = score_calibration_profile(
            calibration_directory,
            cache_directory,
            manifest,
            profile,
            protocol_interest_schedule,
            CalibrationSplitV1::Training,
            arms,
            hlp_scenario,
            historical_price_provider,
            headline_markets,
        )?;
        if let Some(evidence) = evidence {
            project_profile_scores_by_market(&evidence, &eligible, &mut training)?;
        }
        evaluated_training_profiles.insert(profile_id.clone());
        if (index + 1) % 100 == 0 || index + 1 == base_profile_ids.len() {
            eprintln!(
                "calibrate: projected per-market base evidence {}/{}",
                index + 1,
                base_profile_ids.len()
            );
        }
    }

    // Generate each market's training-only local neighborhood, union profile
    // IDs across markets, and replay every unique profile globally once.
    let mut unique_refined_ids = BTreeSet::new();
    for market in &eligible {
        let scores = training
            .get(market)
            .expect("eligible market initialized above");
        let shortlist = calibration_shortlist(
            &scores.values().cloned().collect::<Vec<_>>(),
            pareto_shortlist,
        );
        let seeds: Vec<DuskProfileV1> = shortlist
            .iter()
            .filter_map(|profile_id| profiles_by_id.get(profile_id).cloned())
            .collect();
        for profile in refine_profiles(&seeds, local_refinement_rounds, domain) {
            let profile_id = profile.profile_id.clone();
            profiles_by_id.entry(profile_id.clone()).or_insert(profile);
            if !evaluated_training_profiles.contains(&profile_id) {
                unique_refined_ids.insert(profile_id);
            }
        }
    }
    let unique_refined_ids: Vec<String> = unique_refined_ids.into_iter().collect();
    for (index, profile_id) in unique_refined_ids.iter().enumerate() {
        let profile = profiles_by_id
            .get(profile_id)
            .context("refined per-market profile disappeared")?;
        let evidence = score_calibration_profile(
            calibration_directory,
            cache_directory,
            manifest,
            profile,
            protocol_interest_schedule,
            CalibrationSplitV1::Training,
            arms,
            hlp_scenario,
            historical_price_provider,
            headline_markets,
        )?;
        if let Some(evidence) = evidence {
            project_profile_scores_by_market(&evidence, &eligible, &mut training)?;
        }
        evaluated_training_profiles.insert(profile_id.clone());
        if (index + 1) % 100 == 0 || index + 1 == unique_refined_ids.len() {
            eprintln!(
                "calibrate: unique per-market refined replays {}/{}",
                index + 1,
                unique_refined_ids.len()
            );
        }
    }

    let validation_ids_by_market: BTreeMap<String, BTreeSet<String>> = eligible
        .iter()
        .map(|market| {
            let scores = training
                .get(market)
                .expect("eligible market initialized above");
            (
                market.clone(),
                calibration_shortlist(
                    &scores.values().cloned().collect::<Vec<_>>(),
                    pareto_shortlist,
                )
                .into_iter()
                .collect(),
            )
        })
        .collect();
    let unique_validation_ids: Vec<String> = validation_ids_by_market
        .values()
        .flat_map(|ids| ids.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut validation = eligible
        .iter()
        .map(|market| (market.clone(), BTreeMap::<String, CandidateScoreV1>::new()))
        .collect::<BTreeMap<_, _>>();
    for (index, profile_id) in unique_validation_ids.iter().enumerate() {
        let profile = profiles_by_id
            .get(profile_id)
            .context("shortlisted per-market profile disappeared")?;
        let evidence = score_calibration_profile(
            calibration_directory,
            cache_directory,
            manifest,
            profile,
            protocol_interest_schedule,
            CalibrationSplitV1::Validation,
            arms,
            hlp_scenario,
            historical_price_provider,
            headline_markets,
        )?;
        let Some(evidence) = evidence else {
            continue;
        };
        for market in &eligible {
            if !validation_ids_by_market
                .get(market)
                .expect("eligible market validation set exists")
                .contains(profile_id)
            {
                continue;
            }
            if let Some(score) = combine_calibration_arms_for_market(&evidence.arms, market)? {
                validation
                    .get_mut(market)
                    .expect("eligible market initialized above")
                    .insert(profile_id.clone(), score);
            }
        }
        if (index + 1) % 100 == 0 || index + 1 == unique_validation_ids.len() {
            eprintln!(
                "calibrate: unique per-market validation replays {}/{}",
                index + 1,
                unique_validation_ids.len()
            );
        }
    }

    let mut all_training = BTreeMap::new();
    let mut all_validation = BTreeMap::new();
    let mut selected = BTreeMap::new();
    let mut pareto = BTreeMap::new();
    for market in eligible {
        let training_by_id = training.remove(&market).expect("training market exists");
        let validation_by_id = validation
            .remove(&market)
            .expect("validation market exists");
        let joint_scores: Vec<CandidateScoreV1> = training_by_id
            .values()
            .filter(|score| score.passes_gates())
            .filter_map(|score| validation_by_id.get(&score.profile_id).cloned())
            .collect();
        if let Some(winner) = select_global_winner(&joint_scores) {
            selected.insert(market.clone(), winner.profile_id.clone());
        }
        pareto.insert(
            market.clone(),
            pareto_frontier_ids(&validation_by_id.values().cloned().collect::<Vec<_>>()),
        );
        all_training.insert(market.clone(), training_by_id.into_values().collect());
        all_validation.insert(market, validation_by_id.into_values().collect());
    }
    Ok((all_training, all_validation, selected, pareto))
}

fn project_profile_scores_by_market(
    evidence: &CalibrationProfileEvidenceV1,
    eligible_markets: &[String],
    target: &mut BTreeMap<String, BTreeMap<String, CandidateScoreV1>>,
) -> Result<()> {
    for market in eligible_markets {
        if let Some(score) = combine_calibration_arms_for_market(&evidence.arms, market)? {
            target
                .get_mut(market)
                .context("projected per-market score target is missing")?
                .insert(evidence.profile_id.clone(), score);
        }
    }
    Ok(())
}

fn combine_calibration_arms_for_market(
    arms: &[CalibrationArmEvidenceV1],
    market: &str,
) -> Result<Option<CandidateScoreV1>> {
    if arms.is_empty() {
        bail!("per-market calibration profile has no replay arm evidence");
    }
    let profile_id = &arms[0].profile_id;
    let hlp_scenario = arms[0].hlp_scenario;
    let mut scores = Vec::with_capacity(arms.len());
    for arm in arms {
        arm.verify()?;
        if &arm.profile_id != profile_id || arm.hlp_scenario != hlp_scenario {
            bail!("per-market calibration arms have inconsistent identities");
        }
        let market_evidence = arm
            .score_evidence
            .markets
            .iter()
            .find(|row| row.market == market)
            .with_context(|| format!("global replay evidence omits eligible market {market}"))?;
        if !market_evidence.blocking_reasons.is_empty() {
            return Ok(None);
        }
        match market_evidence.candidate_score.as_ref() {
            Some(score)
                if score.profile_id == *profile_id && score.hlp_scenario == hlp_scenario =>
            {
                scores.push(score)
            }
            Some(_) => bail!("per-market candidate score has the wrong identity"),
            None => return Ok(None),
        }
    }
    Ok(Some(combine_candidate_scores(
        profile_id,
        hlp_scenario,
        &scores,
    )))
}

fn combine_calibration_arms(
    arms: &[CalibrationArmEvidenceV1],
) -> Result<(Option<CandidateScoreV1>, BTreeSet<String>)> {
    if arms.is_empty() {
        bail!("calibration profile has no replay arm evidence");
    }
    let profile_id = &arms[0].profile_id;
    let hlp_scenario = arms[0].hlp_scenario;
    let mut blocking = BTreeSet::new();
    let mut scores = Vec::with_capacity(arms.len());
    for arm in arms {
        arm.verify()?;
        if &arm.profile_id != profile_id || arm.hlp_scenario != hlp_scenario {
            bail!("calibration arms belong to different profiles or hLP scenarios");
        }
        blocking.extend(arm.score_evidence.blocking_reasons.iter().cloned());
        match &arm.score_evidence.candidate_score {
            Some(score) => scores.push(score),
            None => {
                blocking.insert(format!(
                    "{}_arm_has_no_complete_candidate_score",
                    replay_arm_name(arm.arm)
                ));
            }
        }
    }
    if !blocking.is_empty() || scores.len() != arms.len() {
        return Ok((None, blocking));
    }
    Ok((
        Some(combine_candidate_scores(profile_id, hlp_scenario, &scores)),
        blocking,
    ))
}

fn combine_candidate_scores(
    profile_id: &str,
    hlp_scenario: HlpScenario,
    scores: &[&CandidateScoreV1],
) -> CandidateScoreV1 {
    debug_assert!(!scores.is_empty());
    let max_i128 = |field: fn(&CandidateScoreV1) -> i128| {
        scores
            .iter()
            .map(|score| field(score))
            .max()
            .expect("nonempty")
    };
    let max_i64 = |field: fn(&CandidateScoreV1) -> i64| {
        scores
            .iter()
            .map(|score| field(score))
            .max()
            .expect("nonempty")
    };
    let min_i64 = |field: fn(&CandidateScoreV1) -> i64| {
        scores
            .iter()
            .map(|score| field(score))
            .min()
            .expect("nonempty")
    };
    CandidateScoreV1 {
        profile_id: profile_id.to_owned(),
        hlp_scenario,
        bad_debt_delta_usd_micro: max_i128(|score| score.bad_debt_delta_usd_micro),
        tail_liquidation_shortfall_delta_usd_micro: max_i128(|score| {
            score.tail_liquidation_shortfall_delta_usd_micro
        }),
        volume_weighted_execution_cost_delta_ppb: max_i64(|score| {
            score.volume_weighted_execution_cost_delta_ppb
        }),
        p95_slippage_delta_ppb: max_i64(|score| score.p95_slippage_delta_ppb),
        capital_weighted_stakeholder_revenue_uplift_bps: min_i64(|score| {
            score.capital_weighted_stakeholder_revenue_uplift_bps
        }),
        equal_market_stakeholder_revenue_uplift_bps: min_i64(|score| {
            score.equal_market_stakeholder_revenue_uplift_bps
        }),
        quote_quality_uplift_ppb: min_i64(|score| score.quote_quality_uplift_ppb),
        borrow_capacity_uplift_bps: min_i64(|score| score.borrow_capacity_uplift_bps),
    }
}

fn calibration_shortlist(scores: &[CandidateScoreV1], maximum: usize) -> Vec<String> {
    if scores.is_empty() || maximum == 0 {
        return Vec::new();
    }
    let frontier: BTreeSet<String> = pareto_frontier_ids(scores).into_iter().collect();
    let mut candidates: Vec<&CandidateScoreV1> = scores
        .iter()
        .filter(|score| frontier.contains(&score.profile_id))
        .collect();
    candidates.sort_by(|left, right| {
        let left_weak = left
            .capital_weighted_stakeholder_revenue_uplift_bps
            .min(left.equal_market_stakeholder_revenue_uplift_bps);
        let right_weak = right
            .capital_weighted_stakeholder_revenue_uplift_bps
            .min(right.equal_market_stakeholder_revenue_uplift_bps);
        right_weak
            .cmp(&left_weak)
            .then(
                right
                    .quote_quality_uplift_ppb
                    .cmp(&left.quote_quality_uplift_ppb),
            )
            .then(
                right
                    .borrow_capacity_uplift_bps
                    .cmp(&left.borrow_capacity_uplift_bps),
            )
            .then(left.profile_id.cmp(&right.profile_id))
    });
    candidates
        .into_iter()
        .take(maximum)
        .map(|score| score.profile_id.clone())
        .collect()
}

const fn calibration_split_name(split: CalibrationSplitV1) -> &'static str {
    match split {
        CalibrationSplitV1::Training => "training",
        CalibrationSplitV1::Validation => "validation",
    }
}

pub(crate) async fn evaluate(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    let freeze_path = context.run_dir().join("profile-freeze.json");
    let freeze: ProfileFreezeV1 = read_json(&freeze_path)?;
    freeze.verify()?;
    let protocol_interest_schedule_path = context.run_dir().join(PROTOCOL_INTEREST_SCHEDULE_JSON);
    let protocol_interest_schedule: ProtocolInterestShareScheduleV1 =
        read_json(&protocol_interest_schedule_path)?;
    protocol_interest_schedule.verify()?;
    if freeze.config_sha256 != context.config_sha256()
        || freeze.dusk_source_sha256 != source_fingerprints(context)?.dusk_source_sha256
        || freeze.dataset_id != sha256_file(&context.run_dir().join(EXTRACTION_INVENTORY))?
        || freeze.hlp_scenario != calibration_hlp_scenario(config)?
        || freeze.protocol_interest_schedule_sha256 != protocol_interest_schedule.schedule_sha256
    {
        bail!("profile freeze does not bind to this dataset, config, and Dusk source");
    }
    let validated_scope = validated_evaluation_scope_evidence(context, config)?;
    let v1_replay_certification_policy = validated_scope.replay_certification_policy;

    let splits_path = context.run_dir().join(CHRONOLOGICAL_SPLITS_JSON);
    let splits: ChronologicalSplitsV1 = read_json(&splits_path).with_context(|| {
        format!(
            "sealed holdout evaluation requires {}",
            splits_path.display()
        )
    })?;
    let universe: crate::universe::UniverseWindowArtifactV1 =
        read_json(&context.run_dir().join(UNIVERSE_WINDOW_JSON))?;
    let headline_markets: BTreeSet<String> = universe
        .market_eligibility
        .iter()
        .filter(|market| market.usd_headline_eligible)
        .map(|market| market.pair_address.clone())
        .collect();
    if headline_markets.is_empty() {
        bail!("sealed evaluation has no USD-headline-eligible market");
    }
    let result_scope = validated_scope.result_scope(config, headline_markets.clone())?;
    let profile_path = context.run_dir().join("candidate-profiles.json");
    let profiles: Vec<DuskProfileV1> = read_json(&profile_path)?;
    let by_id: BTreeMap<&str, &DuskProfileV1> = profiles
        .iter()
        .map(|profile| (profile.profile_id.as_str(), profile))
        .collect();
    let evaluation_ids: Vec<&str> = match freeze.selected_profile_id.as_deref() {
        Some(profile_id) => vec![profile_id],
        None => freeze
            .pareto_profile_ids
            .iter()
            .map(String::as_str)
            .collect(),
    };
    if evaluation_ids.is_empty() {
        bail!("profile freeze contains neither a global winner nor a Pareto frontier");
    }

    let transfer_registry: TransferFeeRegistryV1 =
        read_json(&context.data_dir().join(TRANSFER_FEE_REGISTRY_JSON))
            .context("sealed evaluation requires the exact historical transfer-fee registry")?;
    let transfer_registry = transfer_registry.validate()?;
    let transfer_fee_provenance: TransferFeeProvenanceArtifactV1 =
        read_json(&context.data_dir().join(TRANSFER_FEE_PROVENANCE_JSON))
            .context("sealed evaluation requires the exact historical mint-extension provenance")?;
    transfer_fee_provenance
        .validate()
        .context("validating sealed historical mint-extension provenance")?;
    if transfer_fee_provenance.run_id != context.run_id()
        || transfer_fee_provenance.cutoff_slot != context.cutoff_slot()
    {
        bail!("mint-extension provenance belongs to another immutable run");
    }
    let leverage_mint_eligibility: Vec<LeverageMintEligibilityEvidenceV1> = transfer_fee_provenance
        .leverage_mint_eligibility_projections()?
        .into_iter()
        .map(|projection| LeverageMintEligibilityEvidenceV1 {
            mint: projection.mint,
            decoded_mint_sha256: projection.decoded_mint_sha256,
            native_leverage_eligible: projection.native_leverage_eligible,
        })
        .collect();
    let operations_path = context.data_dir().join(CANONICAL_OPERATIONS_JSONL);
    let factual_ledger_path = context.run_dir().join(V1_REPLAY_LEDGER_JSONL);
    let prices_path = context.data_dir().join(HISTORICAL_PRICES_JSONL);
    let spot_probe_window = SpotProbeSizingWindowV1 {
        start_unix_seconds: splits.holdout_start_unix_seconds,
        end_unix_seconds_exclusive: splits.holdout_end_unix_seconds_exclusive,
        standardized_trade_nav_bps: config.replay.standardized_trade_nav_bps.clone(),
    };
    spot_probe_window.validate()?;
    let mut artifacts = Vec::<(String, PathBuf)>::new();
    let mut evidence_rows = Vec::<CandidateScoreEvidenceV1>::new();
    let mut bundles = Vec::<HoldoutEvaluationBundleV1>::new();

    for profile_id in evaluation_ids {
        let profile = by_id
            .get(profile_id)
            .copied()
            .with_context(|| format!("frozen profile {profile_id} is absent from candidate set"))?;
        if profile.profile_id.len() != 64
            || !profile
                .profile_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("frozen profile ID is not a canonical SHA-256");
        }
        for arm in enabled_replay_arms(config)? {
            for hlp_scenario in enabled_hlp_scenarios(config)? {
                let tag = format!(
                    "{}-{}-{}",
                    profile.profile_id,
                    replay_arm_name(arm),
                    hlp_scenario_name(hlp_scenario),
                );
                let counterfactual_path = context
                    .run_dir()
                    .join(format!("counterfactual-{tag}.jsonl"));
                let counterfactual_summary_path = context
                    .run_dir()
                    .join(format!("counterfactual-{tag}.summary.json"));
                let paired_path = context
                    .run_dir()
                    .join(format!("paired-metrics-{tag}.jsonl"));
                let paired_summary_path = context
                    .run_dir()
                    .join(format!("paired-metrics-{tag}.summary.json"));
                let evidence_path = context
                    .run_dir()
                    .join(format!("candidate-evidence-{tag}.json"));

                let (evidence, paired_summary, counterfactual_summary) = if counterfactual_path
                    .exists()
                    || counterfactual_summary_path.exists()
                    || paired_path.exists()
                    || paired_summary_path.exists()
                    || evidence_path.exists()
                {
                    if !counterfactual_path.exists()
                        || !counterfactual_summary_path.exists()
                        || !paired_path.exists()
                        || !paired_summary_path.exists()
                        || !evidence_path.exists()
                    {
                        bail!("partial frozen evaluation artifact set exists for {tag}");
                    }
                    let counterfactual_summary: CounterfactualEvaluationSummaryV1 =
                        read_json(&counterfactual_summary_path)?;
                    let paired_summary: PairedMetricLedgerSummaryV1 =
                        read_json(&paired_summary_path)?;
                    if counterfactual_summary.profile_id != profile.profile_id
                        || counterfactual_summary.protocol_interest_schedule_sha256
                            != protocol_interest_schedule.schedule_sha256
                        || counterfactual_summary.arm != arm
                        || counterfactual_summary.hlp_scenario != hlp_scenario
                        || counterfactual_summary.records_sha256
                            != sha256_file(&counterfactual_path)?
                        || paired_summary.profile_id != profile.profile_id
                        || paired_summary.hlp_scenario != hlp_scenario
                        || paired_summary.ledger_sha256 != sha256_file(&paired_path)?
                        || !counterfactual_summary.spot_probes_enabled
                        || counterfactual_summary.spot_probe_sizing_window.as_ref()
                            != Some(&spot_probe_window)
                        || !paired_summary.spot_probes_enabled
                        || paired_summary.spot_probe_sizing_window.as_ref()
                            != Some(&spot_probe_window)
                    {
                        bail!(
                            "existing frozen evaluation artifact failed hash or identity binding"
                        );
                    }
                    let evidence: CandidateScoreEvidenceV1 = read_json(&evidence_path)?;
                    if evidence.profile_id != profile.profile_id
                        || evidence.arm != arm
                        || evidence.hlp_scenario != hlp_scenario
                        || evidence.paired_metric_ledger_sha256 != paired_summary.ledger_sha256
                        || evidence.included_markets_sha256 != market_set_sha256(&headline_markets)
                        || evidence.included_market_count != u64::try_from(headline_markets.len())?
                    {
                        bail!("existing candidate evidence failed replay binding");
                    }
                    let mut recomputed = aggregate_candidate_score_for_markets(
                        &paired_path,
                        &paired_summary,
                        &prices_path,
                        &config.source.historical_price_provider,
                        &profile.profile_id,
                        arm,
                        splits.holdout_start_unix_seconds,
                        splits.holdout_end_unix_seconds_exclusive,
                        Some(&headline_markets),
                    )?;
                    if !counterfactual_summary.complete_for_scoring {
                        recomputed
                            .blocking_reasons
                            .insert("counterfactual_replay_deferred_external_state".to_owned());
                        recomputed.candidate_score = None;
                    }
                    if evidence != recomputed {
                        bail!("existing candidate evidence differs from its paired ledger");
                    }
                    (evidence, paired_summary, counterfactual_summary)
                } else {
                    let mut metric_writer =
                        PairedMetricLedgerWriterV1::create_with_spot_probe_window(
                            &paired_path,
                            &profile.profile_id,
                            hlp_scenario,
                            Some(spot_probe_window.clone()),
                        )?;
                    let counterfactual_summary =
                        evaluate_profile_history_observed_with_spot_probes(
                            &operations_path,
                            &factual_ledger_path,
                            &counterfactual_path,
                            profile,
                            &protocol_interest_schedule,
                            arm,
                            hlp_scenario,
                            &transfer_registry,
                            Some(&spot_probe_window),
                            Some(&mut metric_writer),
                        )?;
                    let paired_summary = metric_writer.commit()?;
                    if counterfactual_summary.records_sha256 != sha256_file(&counterfactual_path)?
                        || paired_summary.ledger_sha256 != sha256_file(&paired_path)?
                    {
                        bail!("frozen replay streaming commitment differs from committed bytes");
                    }
                    let mut evidence = aggregate_candidate_score_for_markets(
                        &paired_path,
                        &paired_summary,
                        &prices_path,
                        &config.source.historical_price_provider,
                        &profile.profile_id,
                        arm,
                        splits.holdout_start_unix_seconds,
                        splits.holdout_end_unix_seconds_exclusive,
                        Some(&headline_markets),
                    )?;
                    if !counterfactual_summary.complete_for_scoring {
                        evidence
                            .blocking_reasons
                            .insert("counterfactual_replay_deferred_external_state".to_owned());
                        evidence.candidate_score = None;
                    }
                    write_json(&counterfactual_summary_path, &counterfactual_summary)?;
                    write_json(&paired_summary_path, &paired_summary)?;
                    write_json(&evidence_path, &evidence)?;
                    (evidence, paired_summary, counterfactual_summary)
                };

                let parquet_dir = context.data_dir().join("parquet");
                fs::create_dir_all(&parquet_dir)?;
                let counterfactual_parquet =
                    parquet_dir.join(format!("counterfactual-{tag}.parquet"));
                let counterfactual_parquet_manifest =
                    parquet_dir.join(format!("counterfactual-{tag}.manifest.json"));
                ensure_parquet_artifact(&ParquetExportSpecV1 {
                    dataset: BenchmarkParquetDatasetV1::CounterfactualReplayLedger,
                    logical_name: BenchmarkParquetDatasetV1::CounterfactualReplayLedger
                        .logical_name(),
                    source_jsonl: &counterfactual_path,
                    parquet_output: &counterfactual_parquet,
                    manifest_output: &counterfactual_parquet_manifest,
                })?;
                let paired_parquet = parquet_dir.join(format!("paired-metrics-{tag}.parquet"));
                let paired_parquet_manifest =
                    parquet_dir.join(format!("paired-metrics-{tag}.manifest.json"));
                ensure_parquet_artifact(&ParquetExportSpecV1 {
                    dataset: BenchmarkParquetDatasetV1::PairedMetricLedger,
                    logical_name: BenchmarkParquetDatasetV1::PairedMetricLedger.logical_name(),
                    source_jsonl: &paired_path,
                    parquet_output: &paired_parquet,
                    manifest_output: &paired_parquet_manifest,
                })?;

                bundles.push(HoldoutEvaluationBundleV1 {
                    profile_id: profile.profile_id.clone(),
                    arm,
                    hlp_scenario,
                    opportunity_ceiling_market: None,
                    paired_path: paired_path.clone(),
                    paired_summary,
                    counterfactual_summary: Some(counterfactual_summary),
                    evidence: evidence.clone(),
                });
                artifacts.extend([
                    (format!("counterfactual-{tag}.jsonl"), counterfactual_path),
                    (
                        format!("counterfactual-{tag}.summary.json"),
                        counterfactual_summary_path,
                    ),
                    (format!("paired-metrics-{tag}.jsonl"), paired_path),
                    (
                        format!("paired-metrics-{tag}.summary.json"),
                        paired_summary_path,
                    ),
                    (format!("candidate-evidence-{tag}.json"), evidence_path),
                    (
                        format!("counterfactual-{tag}.parquet"),
                        counterfactual_parquet,
                    ),
                    (
                        format!("counterfactual-{tag}.manifest.json"),
                        counterfactual_parquet_manifest,
                    ),
                    (format!("paired-metrics-{tag}.parquet"), paired_parquet),
                    (
                        format!("paired-metrics-{tag}.manifest.json"),
                        paired_parquet_manifest,
                    ),
                ]);
                evidence_rows.push(evidence);
            }
        }
    }

    let calibration_qualification_path = context
        .run_dir()
        .join("per-market-calibration-qualification.json");
    if sha256_file(&calibration_qualification_path)? != freeze.per_market_qualification_sha256 {
        bail!("per-market qualification does not bind to the frozen profile artifact");
    }
    let calibration_qualification: PerMarketCalibrationQualificationArtifactV1 =
        read_json(&calibration_qualification_path)?;
    calibration_qualification.verify()?;
    let holdout_qualification =
        qualify_holdout_markets(&operations_path, &splits, &calibration_qualification)?;
    holdout_qualification.verify()?;
    let holdout_qualification_path = context
        .run_dir()
        .join("per-market-holdout-qualification.json");
    write_json(&holdout_qualification_path, &holdout_qualification)?;
    artifacts.push((
        "per-market-holdout-qualification.json".to_owned(),
        holdout_qualification_path.clone(),
    ));

    let calibration_by_market: BTreeMap<&str, _> = calibration_qualification
        .markets
        .iter()
        .map(|market| (market.market.as_str(), market))
        .collect();
    let mut opportunity_claims = Vec::<MarketOpportunityCeilingV1>::new();
    let mut opportunity_assignments = BTreeMap::<String, Vec<String>>::new();
    for holdout in &holdout_qualification.markets {
        let calibration = calibration_by_market
            .get(holdout.market.as_str())
            .context("holdout qualification has no calibration row")?;
        let frozen_profile = freeze
            .per_market_selected_profile_ids
            .get(&holdout.market)
            .cloned();
        let claim_available = holdout.optimized_profile_claim_allowed && frozen_profile.is_some();
        let reason = if claim_available {
            None
        } else if !holdout.training_validation_qualified {
            Some("training_or_validation_activity_below_100_swaps_or_7_days".to_owned())
        } else if !holdout.holdout.qualifies {
            Some("holdout_activity_below_100_swaps_or_7_days".to_owned())
        } else {
            Some("no_training_validation_profile_passed_all_gates".to_owned())
        };
        if let Some(profile_id) = frozen_profile.as_ref().filter(|_| claim_available) {
            opportunity_assignments
                .entry(profile_id.clone())
                .or_default()
                .push(holdout.market.clone());
        }
        opportunity_claims.push(MarketOpportunityCeilingV1 {
            market: holdout.market.clone(),
            training_swap_count: calibration.training.swap_count,
            training_active_days: calibration.training.active_days,
            validation_swap_count: calibration.validation.swap_count,
            validation_active_days: calibration.validation.active_days,
            holdout_swap_count: holdout.holdout.swap_count,
            holdout_active_days: holdout.holdout.active_days,
            selected_profile_id: frozen_profile.filter(|_| claim_available),
            claim_available,
            unavailable_reason: reason,
        });
    }

    let mut opportunity_holdout_passes: BTreeMap<String, bool> = opportunity_assignments
        .values()
        .flatten()
        .map(|market| (market.clone(), true))
        .collect();
    for (profile_id, markets) in opportunity_assignments {
        let profile = by_id.get(profile_id.as_str()).copied().with_context(|| {
            format!("per-market frozen profile {profile_id} is absent from candidate set")
        })?;
        for arm in enabled_replay_arms(config)? {
            let reusable = bundles
                .iter()
                .find(|bundle| {
                    bundle.opportunity_ceiling_market.is_none()
                        && bundle.profile_id == profile_id
                        && bundle.arm == arm
                        && bundle.hlp_scenario == freeze.hlp_scenario
                })
                .map(|bundle| (bundle.paired_path.clone(), bundle.paired_summary.clone()));
            let (paired_path, paired_summary) = match reusable {
                Some(bundle) => bundle,
                None => {
                    let replay = evaluate_additional_holdout_replay(
                        context,
                        profile,
                        &protocol_interest_schedule,
                        arm,
                        freeze.hlp_scenario,
                        &operations_path,
                        &factual_ledger_path,
                        &transfer_registry,
                        &spot_probe_window,
                    )?;
                    artifacts.extend(replay.artifacts);
                    (replay.paired_path, replay.paired_summary)
                }
            };
            for market in markets.iter() {
                let market_set = BTreeSet::from([market.clone()]);
                let evidence = aggregate_candidate_score_for_markets(
                    &paired_path,
                    &paired_summary,
                    &prices_path,
                    &config.source.historical_price_provider,
                    &profile.profile_id,
                    arm,
                    splits.holdout_start_unix_seconds,
                    splits.holdout_end_unix_seconds_exclusive,
                    Some(&market_set),
                )?;
                let evidence_name = format!(
                    "opportunity-evidence-{}-{}-{}.json",
                    sha256_hex(market.as_bytes()),
                    profile.profile_id,
                    replay_arm_name(arm),
                );
                let evidence_path = context.run_dir().join(&evidence_name);
                write_json(&evidence_path, &evidence)?;
                artifacts.push((evidence_name, evidence_path));
                evidence_rows.push(evidence.clone());
                let passes_holdout = evidence
                    .candidate_score
                    .as_ref()
                    .is_some_and(CandidateScoreV1::passes_gates)
                    && evidence.blocking_reasons.is_empty();
                if !passes_holdout {
                    opportunity_holdout_passes.insert(market.clone(), false);
                }
                if passes_holdout {
                    bundles.push(HoldoutEvaluationBundleV1 {
                        profile_id: profile.profile_id.clone(),
                        arm,
                        hlp_scenario: freeze.hlp_scenario,
                        opportunity_ceiling_market: Some(market.clone()),
                        paired_path: paired_path.clone(),
                        paired_summary: paired_summary.clone(),
                        counterfactual_summary: None,
                        evidence,
                    });
                }
            }
        }
    }
    for (market, passes) in opportunity_holdout_passes {
        if passes {
            continue;
        }
        bundles.retain(|bundle| bundle.opportunity_ceiling_market.as_deref() != Some(&market));
        if let Some(claim) = opportunity_claims
            .iter_mut()
            .find(|claim| claim.market == market)
        {
            claim.claim_available = false;
            claim.selected_profile_id = None;
            claim.unavailable_reason =
                Some("selected_profile_failed_one_or_more_sealed_holdout_arms".to_owned());
        }
    }

    let evidence_index_path = context.run_dir().join("holdout-evidence.json");
    write_json(&evidence_index_path, &evidence_rows)?;
    artifacts.push((
        "holdout-evidence.json".to_owned(),
        evidence_index_path.clone(),
    ));

    let historical_prices_sha256 = sha256_file(&prices_path)?;
    let mut bootstrap_intervals = Vec::<BootstrapIntervalV1>::new();
    let mut sensitivity_evidence = Vec::<SensitivityEvidenceSummaryV1>::new();
    for bundle in bundles
        .iter()
        .filter(|bundle| bundle.opportunity_ceiling_market.is_none())
    {
        let tag = format!(
            "{}-{}-{}",
            bundle.profile_id,
            replay_arm_name(bundle.arm),
            hlp_scenario_name(bundle.hlp_scenario),
        );
        let sensitivity_name = format!("sensitivity-{tag}.json");
        let sensitivity_path = context.run_dir().join(&sensitivity_name);
        let sensitivity = if sensitivity_path.exists() {
            let artifact: SensitivityArtifactV1 = read_json(&sensitivity_path)?;
            artifact.verify_self_hash()?;
            if artifact.profile_id != bundle.profile_id
                || artifact.arm != bundle.arm
                || artifact.hlp_scenario != bundle.hlp_scenario
                || artifact.paired_metric_ledger_sha256 != bundle.paired_summary.ledger_sha256
                || artifact.historical_prices_sha256 != historical_prices_sha256
                || artifact.included_markets_sha256 != market_set_sha256(&headline_markets)
            {
                bail!("existing sensitivity artifact has the wrong frozen identity");
            }
            artifact
        } else {
            let artifact = build_sensitivity_evidence(&SensitivityRequestV1 {
                run_id: context.run_id(),
                profile_id: &bundle.profile_id,
                arm: bundle.arm,
                hlp_scenario: bundle.hlp_scenario,
                holdout_start_unix_seconds: splits.holdout_start_unix_seconds,
                holdout_end_unix_seconds_exclusive: splits.holdout_end_unix_seconds_exclusive,
                paired_metric_ledger_path: &bundle.paired_path,
                paired_summary: &bundle.paired_summary,
                historical_prices_path: &prices_path,
                historical_prices_sha256: &historical_prices_sha256,
                historical_price_provider: &config.source.historical_price_provider,
                included_markets: Some(&headline_markets),
                bootstrap_seed: config.calibration.seed,
                bootstrap_samples: config.calibration.bootstrap_samples,
            })?;
            write_json(&sensitivity_path, &artifact)?;
            artifact
        };
        bootstrap_intervals.extend(reporting_bootstrap_intervals(&sensitivity));
        sensitivity_evidence.push(SensitivityEvidenceSummaryV1 {
            profile_id: sensitivity.profile_id.clone(),
            replay_arm: sensitivity.arm,
            hlp_scenario: sensitivity.hlp_scenario,
            sensitivity_artifact_sha256: sensitivity.artifact_sha256.clone(),
            included_markets_sha256: sensitivity.included_markets_sha256.clone(),
            sampled_block_sequence_sha256: sensitivity
                .paired_bootstrap
                .sampled_block_sequence_sha256
                .clone(),
            bootstrap_samples: sensitivity.paired_bootstrap.samples,
            top_market_rankings: sensitivity.top_market_rankings.clone(),
            top_market_exclusions: sensitivity.top_market_exclusions.clone(),
            top_market_exclusion_unsupported_reason: sensitivity
                .top_market_exclusion_unsupported_reason
                .clone(),
        });
        artifacts.push((sensitivity_name, sensitivity_path));
    }

    let leverage_campaign_rows = evaluate_leverage_campaigns(
        context,
        &freeze,
        &splits,
        &profiles,
        &headline_markets,
        &bundles,
        &operations_path,
        &factual_ledger_path,
        &transfer_registry,
        &protocol_interest_schedule,
        &spot_probe_window,
        &leverage_mint_eligibility,
        v1_replay_certification_policy,
    )?;
    let leverage_campaigns: Vec<LeverageCampaignArtifactV1> = leverage_campaign_rows
        .iter()
        .map(|(_, _, artifact)| artifact.clone())
        .collect();
    artifacts.extend(
        leverage_campaign_rows
            .into_iter()
            .map(|(name, path, _)| (name, path)),
    );

    let scope_denominator_markets: BTreeSet<String> = universe
        .market_eligibility
        .iter()
        .filter(|market| market.reason != EligibilityReason::NotVisible)
        .map(|market| market.pair_address.clone())
        .collect();
    let denominator_bundle = bundles
        .iter()
        .find(|bundle| {
            bundle.opportunity_ceiling_market.is_none()
                && bundle.hlp_scenario == freeze.hlp_scenario
        })
        .context("no shared-profile headline replay exists for the scope denominator")?;
    let visible_denominator = aggregate_candidate_score_for_markets(
        &denominator_bundle.paired_path,
        &denominator_bundle.paired_summary,
        &prices_path,
        &config.source.historical_price_provider,
        &denominator_bundle.profile_id,
        denominator_bundle.arm,
        splits.holdout_start_unix_seconds,
        splits.holdout_end_unix_seconds_exclusive,
        Some(&scope_denominator_markets),
    )?;
    if !visible_denominator.missing_price_keys.is_empty()
        || visible_denominator.blocking_reasons.iter().any(|reason| {
            reason == "missing_historical_usd_marks"
                || reason.starts_with("included_market_has_no_replay_rows:")
        })
    {
        bail!("scope USD denominator is incomplete; collective headlines are blocked");
    }
    let visible_denominator_path = context
        .run_dir()
        .join("visible-market-denominator-evidence.json");
    write_json(&visible_denominator_path, &visible_denominator)?;
    artifacts.push((
        "visible-market-denominator-evidence.json".to_owned(),
        visible_denominator_path,
    ));

    let reconciliation_path = context.run_dir().join(V1_RECONCILIATION_JSON);
    let v1_reconciliation: V1ReconciliationSummaryV1 = read_json(&reconciliation_path)?;
    let inventory_path = context.run_dir().join(EXTRACTION_INVENTORY);
    let inventory: ExtractionInventoryV1 = read_json(&inventory_path)?;
    let exact_transaction_fetch_denominator = match config.scope.mode {
        ScopeMode::HybridPanel => inventory.targeted_successful_program_signatures,
        ScopeMode::FullUniverse => inventory.successful_program_signatures_at_or_before_cutoff,
    };
    let exact_transaction_fetch_bps = exact_coverage_bps(
        inventory.successful_signatures_matched_in_blocks,
        exact_transaction_fetch_denominator,
    )?;
    let exact_decode_bps = exact_coverage_bps(
        inventory.decoded_state_changing_instructions,
        inventory.expected_state_changing_instructions,
    )?;
    let mut source_artifact_sha256 = BTreeMap::new();
    for (_, path) in &artifacts {
        source_artifact_sha256.insert(relative_source_key(context, path)?, sha256_file(path)?);
    }
    let data_quality_path = context.run_dir().join("data-quality.json");
    let universe_window_path = context.run_dir().join(UNIVERSE_WINDOW_JSON);
    let mut additional_sources = vec![
        inventory_path,
        data_quality_path,
        operations_path.clone(),
        context.data_dir().join(CANONICAL_EVENTS_JSONL),
        context.data_dir().join(CONFIG_HISTORY_JSONL),
        prices_path.clone(),
        context.data_dir().join(TRANSFER_FEE_REGISTRY_JSON),
        context.data_dir().join(TRANSFER_FEE_PROVENANCE_JSON),
        factual_ledger_path.clone(),
        reconciliation_path,
        context.run_dir().join(FINAL_PHASE_GATE_JSON),
        context.run_dir().join(DATASET_SPLIT_MANIFEST_JSON),
        context.run_dir().join("candidate-profiles.json"),
        context.run_dir().join("training-scores.json"),
        context.run_dir().join("validation-scores.json"),
        context.run_dir().join("per-market-training-scores.json"),
        context.run_dir().join("per-market-validation-scores.json"),
        calibration_qualification_path,
        holdout_qualification_path.clone(),
        freeze_path.clone(),
        protocol_interest_schedule_path.clone(),
        universe_window_path.clone(),
        splits_path,
    ];
    let terminal_reconciliation_path = context.run_dir().join(V1_TERMINAL_RECONCILIATION_JSON);
    if terminal_reconciliation_path.exists() {
        additional_sources.push(terminal_reconciliation_path);
    }
    if config.scope.mode == ScopeMode::HybridPanel {
        additional_sources.extend([
            context.run_dir().join(COHORT_SELECTION_JSON),
            context.run_dir().join(COHORT_REPRESENTATIVENESS_JSON),
        ]);
    }
    for path in additional_sources {
        source_artifact_sha256.insert(relative_source_key(context, &path)?, sha256_file(&path)?);
    }
    let holdout_inputs: Vec<HoldoutMetricInputV1<'_>> = bundles
        .iter()
        .map(|bundle| HoldoutMetricInputV1 {
            evidence: &bundle.evidence,
            paired_ledger_path: &bundle.paired_path,
            paired_summary: &bundle.paired_summary,
            hlp_scenario: bundle.hlp_scenario,
            opportunity_ceiling_market: bundle.opportunity_ceiling_market.as_deref(),
        })
        .collect();
    let headline_profiles_pass_holdout = bundles
        .iter()
        .filter(|bundle| {
            bundle.opportunity_ceiling_market.is_none()
                && bundle.hlp_scenario == freeze.hlp_scenario
        })
        .all(|bundle| {
            bundle
                .evidence
                .candidate_score
                .as_ref()
                .is_some_and(CandidateScoreV1::passes_gates)
                && bundle.evidence.blocking_reasons.is_empty()
        });
    let holdout_winner = freeze
        .selected_profile_id
        .clone()
        .filter(|_| headline_profiles_pass_holdout);
    let profile_scope_limitation = match (
        config.scope.mode,
        config.scope.per_market_opportunity_ceiling,
    ) {
        (ScopeMode::HybridPanel, false) => "Per-market optimized profiles are disabled; the selected-panel result evaluates only the one protocol-wide shared profile or Pareto set.",
        (ScopeMode::HybridPanel, true) => "Per-market optimized profiles are separately labeled opportunity ceilings and never substitute for the selected-panel shared profile.",
        (ScopeMode::FullUniverse, false) => "Per-market optimized profiles are disabled; the headline result evaluates only the one protocol-wide shared profile or Pareto set.",
        (ScopeMode::FullUniverse, true) => "Per-market optimized profiles are opportunity ceilings and never substitute for the global headline policy.",
    };
    let mut limitations = vec![
        "Historical counterfactual, not a forecast of future demand or routing.".to_owned(),
        "Fixed-flow isolates mechanics; intent replay and synthetic arbitrage are separately labeled.".to_owned(),
        "Maximum-feasible hLP is an upper bound on a frozen common-pro-rata allocation lattice with no additional external capital.".to_owned(),
        profile_scope_limitation.to_owned(),
        "Quote-driven extra volume is excluded because historical routing intent is unavailable.".to_owned(),
        "Token-2022 transfer-fee history must be complete; TransferHook or unsupported extension history blocks affected markets.".to_owned(),
        "Leverage opening and full unwind are exact at the same captured checkpoint; a realized future Dusk breach path is reported as unavailable unless a separately owned future counterfactual state branch exists.".to_owned(),
        "Inventory PnL and net LP return remain unavailable until passive LP ownership, protected/recenter inventory, hLP/yLP backing, and revenue liabilities are captured in one exact ownership ledger; no zero is substituted.".to_owned(),
    ];
    if config.scope.mode == ScopeMode::HybridPanel {
        limitations.insert(
            1,
            "Selected-panel block targets are selected-market DB signatures plus successful program signatures absent from every DB row. A transaction indexed only under a non-selected market could also carry a global side effect and remain outside scoped blocks; only a block replay spanning the complete visible market universe rules out this mixed-transaction edge."
                .to_owned(),
        );
    }
    let manifest = build_result_manifest(
        context.run_id(),
        &freeze.dataset_id,
        &freeze.freeze_sha256,
        &freeze.dusk_source_sha256,
        context.cutoff_slot(),
        &universe,
        result_scope,
        &holdout_inputs,
        &visible_denominator,
        &v1_reconciliation,
        exact_transaction_fetch_bps,
        exact_decode_bps,
        config.universe.headline_swap_coverage_bps,
        config.universe.headline_lending_coverage_bps,
        freeze.hlp_scenario,
        holdout_winner,
        freeze.pareto_profile_ids.clone(),
        freeze.per_market_qualification_sha256.clone(),
        sha256_file(&holdout_qualification_path)?,
        opportunity_claims,
        sensitivity_evidence,
        sha256_file(&evidence_index_path)?,
        sha256_file(&universe_window_path)?,
        source_artifact_sha256,
        &leverage_campaigns,
        bootstrap_intervals,
        limitations,
    )?;
    validate_result_scope_bindings(context, config, &manifest)?;
    let metrics_path = context.run_dir().join("metrics-manifest.json");
    write_json(&metrics_path, &manifest)?;
    let verified: ResultManifestV1 = read_json(&metrics_path)?;
    verified.verify()?;
    if verified.profile_freeze_sha256 != freeze.freeze_sha256 {
        bail!("holdout metrics do not bind to the frozen profile");
    }
    artifacts.push(("metrics-manifest.json".to_owned(), metrics_path));
    let borrowed: Vec<(&str, &Path)> = artifacts
        .iter()
        .map(|(name, path)| (name.as_str(), path.as_path()))
        .collect();
    artifact_map(&borrowed)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_leverage_campaigns(
    context: &RunContext,
    freeze: &ProfileFreezeV1,
    splits: &ChronologicalSplitsV1,
    profiles: &[DuskProfileV1],
    headline_markets: &BTreeSet<String>,
    global_bundles: &[HoldoutEvaluationBundleV1],
    operations_path: &Path,
    factual_ledger_path: &Path,
    transfer_registry: &crate::transfer_fee::ValidatedTransferFeeRegistryV1,
    protocol_interest_schedule: &ProtocolInterestShareScheduleV1,
    spot_probe_window: &SpotProbeSizingWindowV1,
    mint_eligibility: &[LeverageMintEligibilityEvidenceV1],
    v1_replay_certification_policy: V1ReplayCertificationPolicyV1,
) -> Result<Vec<(String, PathBuf, LeverageCampaignArtifactV1)>> {
    let configurations_path = context.data_dir().join(CONFIG_HISTORY_JSONL);
    let transfer_registry_path = context.data_dir().join(TRANSFER_FEE_REGISTRY_JSON);
    let operations_sha256 = sha256_file(operations_path)?;
    let factual_ledger_sha256 = sha256_file(factual_ledger_path)?;
    let configurations_sha256 = sha256_file(&configurations_path)?;
    let transfer_fee_registry_sha256 = sha256_file(&transfer_registry_path)?;
    let profile_ids: BTreeSet<String> = global_bundles
        .iter()
        .filter(|bundle| bundle.opportunity_ceiling_market.is_none())
        .map(|bundle| bundle.profile_id.clone())
        .collect();
    if profile_ids.is_empty() {
        bail!("leverage campaign has no global holdout profile");
    }

    let mut output = Vec::with_capacity(profile_ids.len());
    for profile_id in profile_ids {
        let profile = profiles
            .iter()
            .find(|profile| profile.profile_id == profile_id)
            .context("leverage campaign profile is absent from the frozen candidate set")?;
        let spec = LeverageCampaignSpecV1 {
            schema_version: crate::leverage_campaign::LEVERAGE_CAMPAIGN_SCHEMA_VERSION,
            profile_id: profile_id.clone(),
            profile_freeze_sha256: freeze.freeze_sha256.clone(),
            operations_sha256: operations_sha256.clone(),
            factual_ledger_sha256: factual_ledger_sha256.clone(),
            configurations_sha256: configurations_sha256.clone(),
            transfer_fee_registry_sha256: transfer_fee_registry_sha256.clone(),
            v1_replay_certification_policy,
            holdout_start_unix_seconds: splits.holdout_start_unix_seconds,
            holdout_end_unix_seconds_exclusive: splits.holdout_end_unix_seconds_exclusive,
            cutoff_slot: context.cutoff_slot(),
            usd_headline_markets: headline_markets.clone(),
            mint_eligibility: mint_eligibility.to_vec(),
            v1_open_loop_iteration_cap: LEVERAGE_V1_OPEN_LOOP_CAP,
            v1_unwind_iteration_cap: LEVERAGE_V1_UNWIND_LOOP_CAP,
            spec_sha256: String::new(),
        }
        .seal()?;
        let name = format!("leverage-campaign-{profile_id}.json");
        let path = context.run_dir().join(&name);
        let artifact = if path.exists() {
            let artifact: LeverageCampaignArtifactV1 = read_json(&path)?;
            artifact.verify()?;
            if artifact.spec != spec {
                bail!("existing leverage campaign belongs to another frozen input set");
            }
            artifact
        } else {
            let mut campaign =
                LeverageCampaignBuilderV1::from_canonical_history(operations_path, spec)?;
            campaign.capture_v1_history(operations_path, &configurations_path)?;
            let mut passes: Vec<_> = global_bundles
                .iter()
                .filter(|bundle| {
                    bundle.opportunity_ceiling_market.is_none() && bundle.profile_id == profile_id
                })
                .collect();
            passes.sort_by_key(|bundle| {
                (
                    replay_arm_name(bundle.arm),
                    hlp_scenario_name(bundle.hlp_scenario),
                )
            });
            for bundle in passes {
                let expected = bundle
                    .counterfactual_summary
                    .as_ref()
                    .context("global holdout bundle omits its counterfactual replay summary")?;
                let mut capture = campaign.begin_dusk_pass(bundle.arm, bundle.hlp_scenario)?;
                let replayed = replay_profile_history_capture_state_hash_only(
                    operations_path,
                    factual_ledger_path,
                    profile,
                    protocol_interest_schedule,
                    bundle.arm,
                    bundle.hlp_scenario,
                    transfer_registry,
                    Some(spot_probe_window),
                    expected.counterfactual_record_count,
                    &expected.records_sha256,
                    &mut capture,
                )?;
                if &replayed != expected {
                    bail!("hash-only leverage capture replay differs from sealed holdout summary");
                }
                capture.finish(&replayed)?;
            }
            let artifact = campaign.finalize(transfer_registry)?;
            write_json(&path, &artifact)?;
            artifact
        };
        output.push((name, path, artifact));
    }
    Ok(output)
}

struct AdditionalHoldoutReplayV1 {
    paired_path: PathBuf,
    paired_summary: PairedMetricLedgerSummaryV1,
    artifacts: Vec<(String, PathBuf)>,
}

fn exact_coverage_bps(observed: u64, expected: u64) -> Result<u16> {
    if expected == 0 || observed > expected {
        bail!("exact coverage has an invalid natural-grain denominator");
    }
    let bps = u128::from(observed)
        .checked_mul(10_000)
        .context("coverage multiplication overflow")?
        / u128::from(expected);
    u16::try_from(bps).context("coverage basis points exceed u16")
}

fn relative_source_key(context: &RunContext, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(context.root()).with_context(|| {
        format!(
            "source artifact {} is outside benchmark root {}",
            path.display(),
            context.root().display()
        )
    })?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("source artifact path is not a safe benchmark-relative path");
    }
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn reporting_bootstrap_intervals(artifact: &SensitivityArtifactV1) -> Vec<BootstrapIntervalV1> {
    let arm = match artifact.arm {
        ReplayArm::FixedFlow => "fixed_flow",
        ReplayArm::Intent => "intent",
    };
    let scenario = match artifact.hlp_scenario {
        HlpScenario::NoHlp => "no_hlp",
        HlpScenario::MaximumFeasible => "maximum_feasible_hlp",
    };
    let prefix = format!(
        "aggregate.profile.{}.arm.{arm}.scenario.{scenario}",
        artifact.profile_id
    );
    let interval =
        |metric_id: String, unit: &str, value: &crate::sensitivity::BootstrapIntervalV1| {
            BootstrapIntervalV1 {
                metric_id,
                block_days: 7,
                samples: artifact.paired_bootstrap.samples,
                lower_025: value.lower_2_5_percent.reported_signed_integer.clone(),
                median: value.median.reported_signed_integer.clone(),
                upper_975: value.upper_97_5_percent.reported_signed_integer.clone(),
                unit: unit.to_owned(),
            }
        };
    let mut output = vec![interval(
        format!("{prefix}.stakeholder_revenue.user_flow_usd_micro"),
        "usd_micro",
        &artifact
            .paired_bootstrap
            .stakeholder_user_flow_revenue_delta_usd_micro,
    )];
    output.push(interval(
        format!("{prefix}.stakeholder_revenue.synthetic_arbitrage_usd_micro"),
        "usd_micro",
        &artifact
            .paired_bootstrap
            .dusk_synthetic_arbitrage_revenue_usd_micro,
    ));
    if let Some(execution) = &artifact.paired_bootstrap.quoted_execution_cost_delta_ppb {
        output.push(interval(
            format!("{prefix}.spot.volume_weighted_execution_cost_delta_ppb"),
            "ppb",
            execution,
        ));
    }
    output
}

fn evaluate_additional_holdout_replay(
    context: &RunContext,
    profile: &DuskProfileV1,
    protocol_interest_schedule: &ProtocolInterestShareScheduleV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    operations_path: &Path,
    factual_ledger_path: &Path,
    transfer_registry: &crate::transfer_fee::ValidatedTransferFeeRegistryV1,
    spot_probe_window: &SpotProbeSizingWindowV1,
) -> Result<AdditionalHoldoutReplayV1> {
    if profile.profile_id.len() != 64
        || !profile
            .profile_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("opportunity-ceiling profile ID is not canonical");
    }
    let tag = format!(
        "opportunity-{}-{}-{}",
        profile.profile_id,
        replay_arm_name(arm),
        hlp_scenario_name(hlp_scenario),
    );
    let counterfactual_path = context
        .run_dir()
        .join(format!("counterfactual-{tag}.jsonl"));
    let counterfactual_summary_path = context
        .run_dir()
        .join(format!("counterfactual-{tag}.summary.json"));
    let paired_path = context
        .run_dir()
        .join(format!("paired-metrics-{tag}.jsonl"));
    let paired_summary_path = context
        .run_dir()
        .join(format!("paired-metrics-{tag}.summary.json"));
    let any_json = counterfactual_path.exists()
        || counterfactual_summary_path.exists()
        || paired_path.exists()
        || paired_summary_path.exists();
    let all_json = counterfactual_path.exists()
        && counterfactual_summary_path.exists()
        && paired_path.exists()
        && paired_summary_path.exists();
    if any_json && !all_json {
        bail!("partial opportunity-ceiling replay artifact set exists for {tag}");
    }
    let paired_summary = if all_json {
        let counterfactual: CounterfactualEvaluationSummaryV1 =
            read_json(&counterfactual_summary_path)?;
        let paired: PairedMetricLedgerSummaryV1 = read_json(&paired_summary_path)?;
        if counterfactual.profile_id != profile.profile_id
            || counterfactual.protocol_interest_schedule_sha256
                != protocol_interest_schedule.schedule_sha256
            || counterfactual.arm != arm
            || counterfactual.hlp_scenario != hlp_scenario
            || counterfactual.records_sha256 != sha256_file(&counterfactual_path)?
            || paired.profile_id != profile.profile_id
            || paired.hlp_scenario != hlp_scenario
            || paired.ledger_sha256 != sha256_file(&paired_path)?
            || !counterfactual.spot_probes_enabled
            || counterfactual.spot_probe_sizing_window.as_ref() != Some(spot_probe_window)
            || !paired.spot_probes_enabled
            || paired.spot_probe_sizing_window.as_ref() != Some(spot_probe_window)
        {
            bail!("opportunity-ceiling replay failed identity verification");
        }
        paired
    } else {
        let mut metric_writer = PairedMetricLedgerWriterV1::create_with_spot_probe_window(
            &paired_path,
            &profile.profile_id,
            hlp_scenario,
            Some(spot_probe_window.clone()),
        )?;
        let counterfactual = evaluate_profile_history_observed_with_spot_probes(
            operations_path,
            factual_ledger_path,
            &counterfactual_path,
            profile,
            protocol_interest_schedule,
            arm,
            hlp_scenario,
            transfer_registry,
            Some(spot_probe_window),
            Some(&mut metric_writer),
        )?;
        let paired = metric_writer.commit()?;
        if counterfactual.records_sha256 != sha256_file(&counterfactual_path)?
            || paired.ledger_sha256 != sha256_file(&paired_path)?
        {
            bail!("opportunity-ceiling replay commitment differs from bytes");
        }
        write_json(&counterfactual_summary_path, &counterfactual)?;
        write_json(&paired_summary_path, &paired)?;
        paired
    };

    let parquet_dir = context.data_dir().join("parquet");
    fs::create_dir_all(&parquet_dir)?;
    let counterfactual_parquet = parquet_dir.join(format!("counterfactual-{tag}.parquet"));
    let counterfactual_parquet_manifest =
        parquet_dir.join(format!("counterfactual-{tag}.manifest.json"));
    ensure_parquet_artifact(&ParquetExportSpecV1 {
        dataset: BenchmarkParquetDatasetV1::CounterfactualReplayLedger,
        logical_name: BenchmarkParquetDatasetV1::CounterfactualReplayLedger.logical_name(),
        source_jsonl: &counterfactual_path,
        parquet_output: &counterfactual_parquet,
        manifest_output: &counterfactual_parquet_manifest,
    })?;
    let paired_parquet = parquet_dir.join(format!("paired-metrics-{tag}.parquet"));
    let paired_parquet_manifest = parquet_dir.join(format!("paired-metrics-{tag}.manifest.json"));
    ensure_parquet_artifact(&ParquetExportSpecV1 {
        dataset: BenchmarkParquetDatasetV1::PairedMetricLedger,
        logical_name: BenchmarkParquetDatasetV1::PairedMetricLedger.logical_name(),
        source_jsonl: &paired_path,
        parquet_output: &paired_parquet,
        manifest_output: &paired_parquet_manifest,
    })?;
    Ok(AdditionalHoldoutReplayV1 {
        paired_path: paired_path.clone(),
        paired_summary,
        artifacts: vec![
            (format!("counterfactual-{tag}.jsonl"), counterfactual_path),
            (
                format!("counterfactual-{tag}.summary.json"),
                counterfactual_summary_path,
            ),
            (format!("paired-metrics-{tag}.jsonl"), paired_path),
            (
                format!("paired-metrics-{tag}.summary.json"),
                paired_summary_path,
            ),
            (
                format!("counterfactual-{tag}.parquet"),
                counterfactual_parquet,
            ),
            (
                format!("counterfactual-{tag}.manifest.json"),
                counterfactual_parquet_manifest,
            ),
            (format!("paired-metrics-{tag}.parquet"), paired_parquet),
            (
                format!("paired-metrics-{tag}.manifest.json"),
                paired_parquet_manifest,
            ),
        ],
    })
}

fn enabled_replay_arms(config: &BenchmarkConfig) -> Result<Vec<ReplayArm>> {
    let mut arms = Vec::new();
    if config.replay.fixed_flow {
        arms.push(ReplayArm::FixedFlow);
    }
    if config.replay.intent_with_arbitrage {
        arms.push(ReplayArm::Intent);
    }
    if arms.is_empty() {
        bail!("benchmark config enables no replay arm");
    }
    Ok(arms)
}

fn enabled_hlp_scenarios(config: &BenchmarkConfig) -> Result<Vec<HlpScenario>> {
    let mut scenarios = Vec::new();
    if config.replay.maximum_feasible_hlp {
        scenarios.push(HlpScenario::MaximumFeasible);
    }
    if config.replay.no_hlp_control {
        scenarios.push(HlpScenario::NoHlp);
    }
    if scenarios.is_empty() {
        bail!("benchmark config enables no hLP scenario");
    }
    Ok(scenarios)
}

fn calibration_hlp_scenario(config: &BenchmarkConfig) -> Result<HlpScenario> {
    if config.replay.maximum_feasible_hlp {
        Ok(HlpScenario::MaximumFeasible)
    } else {
        bail!("headline calibration requires the maximum-feasible hLP scenario")
    }
}

const fn replay_arm_name(arm: ReplayArm) -> &'static str {
    match arm {
        ReplayArm::FixedFlow => "fixed-flow",
        ReplayArm::Intent => "intent",
    }
}

const fn hlp_scenario_name(scenario: HlpScenario) -> &'static str {
    match scenario {
        HlpScenario::NoHlp => "no-hlp",
        HlpScenario::MaximumFeasible => "maximum-feasible-hlp",
    }
}

pub(crate) async fn verify(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    let metrics_path = context.run_dir().join("metrics-manifest.json");
    let manifest: ResultManifestV1 = read_json(&metrics_path)?;
    manifest.verify()?;
    validate_result_scope_bindings(context, config, &manifest)?;
    let mut verified_sources = 0_u64;
    for (relative, expected_sha256) in &manifest.source_artifact_sha256 {
        let relative = Path::new(relative);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )
            })
        {
            bail!("metrics manifest contains an unsafe source-artifact path");
        }
        let path = context.root().join(relative);
        if sha256_file(&path)? != *expected_sha256 {
            bail!(
                "source artifact {} changed after metrics sealing",
                path.display()
            );
        }
        verified_sources = verified_sources
            .checked_add(1)
            .context("verified source count overflow")?;
    }
    let notebook_path = context.root().join("notebooks/benchmark.ipynb");
    if !notebook_path.exists() {
        bail!("source-backed notebook is absent");
    }
    let notebook_runner = context.root().join("scripts/execute_notebook.py");
    if !notebook_runner.exists() {
        bail!("stdlib notebook verifier/executor is absent");
    }
    let executed_notebook_path = context.run_dir().join("benchmark.executed.ipynb");
    let notebook_output = Command::new(&config.output.python_executable)
        .arg(&notebook_runner)
        .arg("--manifest")
        .arg(&metrics_path)
        .arg("--notebook")
        .arg(&notebook_path)
        .arg("--output")
        .arg(&executed_notebook_path)
        .output()
        .context("executing the source-backed benchmark notebook")?;
    if !notebook_output.status.success() {
        bail!(
            "notebook verification/execution failed: {}",
            String::from_utf8_lossy(&notebook_output.stderr)
        );
    }
    if !executed_notebook_path.exists() || fs::metadata(&executed_notebook_path)?.len() == 0 {
        bail!("notebook executor produced no verified output");
    }
    let verification_path = context.run_dir().join("verification.json");
    let verification = serde_json::json!({
        "schema_version": 1,
        "run_id": context.run_id(),
        "metrics_manifest_sha256": sha256_file(&metrics_path)?,
        "notebook_sha256": sha256_file(&notebook_path)?,
        "notebook_runner_sha256": sha256_file(&notebook_runner)?,
        "executed_notebook_sha256": sha256_file(&executed_notebook_path)?,
        "verified_source_artifact_count": verified_sources,
        "all_machine_readable_identities_pass": true
    });
    write_json(&verification_path, &verification)?;
    artifact_map(&[
        ("verification.json", &verification_path),
        ("benchmark.executed.ipynb", &executed_notebook_path),
    ])
}

pub(crate) async fn render(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    let verification: JsonValue = read_json(&context.run_dir().join("verification.json"))?;
    if verification
        .get("all_machine_readable_identities_pass")
        .and_then(JsonValue::as_bool)
        != Some(true)
    {
        bail!("reports cannot render before machine-readable verification passes");
    }
    let metrics_path = context.run_dir().join("metrics-manifest.json");
    let metrics_sha256 = sha256_file(&metrics_path)?;
    if verification
        .get("metrics_manifest_sha256")
        .and_then(JsonValue::as_str)
        != Some(metrics_sha256.as_str())
    {
        bail!("verification receipt does not bind to the current metrics manifest");
    }
    let manifest: ResultManifestV1 = read_json(&metrics_path)?;
    manifest.verify()?;
    validate_result_scope_bindings(context, config, &manifest)?;
    let builder_value =
        std::env::var(&config.output.portable_report_builder_env).with_context(|| {
            format!(
            "render requires environment variable {} to name the portable Data Analytics builder",
            config.output.portable_report_builder_env
        )
        })?;
    let mut builder_path = PathBuf::from(builder_value);
    if builder_path.is_relative() {
        builder_path = context.root().join(builder_path);
    }
    let rendered = write_and_render_reports(
        &manifest,
        context.report_dir(),
        &PortableBuilderConfigV1 {
            node_executable: std::ffi::OsStr::new(&config.output.node_executable),
            builder_script: &builder_path,
        },
    )?;
    let mut paths = Vec::<(String, PathBuf)>::new();
    for report in rendered.reports {
        report.receipt.verify()?;
        let slug = report.audience.slug();
        paths.extend([
            (format!("{slug}.html"), report.html_path),
            (format!("{slug}-render-receipt.json"), report.receipt_path),
            (
                format!("{slug}-artifact.json"),
                context.report_dir().join(slug).join("artifact.json"),
            ),
            (
                format!("{slug}-provenance.sql"),
                context.report_dir().join(slug).join("provenance.sql"),
            ),
        ]);
    }
    let borrowed: Vec<(&str, &Path)> = paths
        .iter()
        .map(|(name, path)| (name.as_str(), path.as_path()))
        .collect();
    artifact_map(&borrowed)
}

fn pareto_frontier_ids(scores: &[CandidateScoreV1]) -> Vec<String> {
    let mut ids: Vec<String> = scores
        .iter()
        .filter(|candidate| {
            !scores.iter().any(|other| {
                other.profile_id != candidate.profile_id
                    && other.bad_debt_delta_usd_micro <= candidate.bad_debt_delta_usd_micro
                    && other.tail_liquidation_shortfall_delta_usd_micro
                        <= candidate.tail_liquidation_shortfall_delta_usd_micro
                    && other.volume_weighted_execution_cost_delta_ppb
                        <= candidate.volume_weighted_execution_cost_delta_ppb
                    && other.p95_slippage_delta_ppb <= candidate.p95_slippage_delta_ppb
                    && other.capital_weighted_stakeholder_revenue_uplift_bps
                        >= candidate.capital_weighted_stakeholder_revenue_uplift_bps
                    && other.equal_market_stakeholder_revenue_uplift_bps
                        >= candidate.equal_market_stakeholder_revenue_uplift_bps
                    && (other.bad_debt_delta_usd_micro < candidate.bad_debt_delta_usd_micro
                        || other.tail_liquidation_shortfall_delta_usd_micro
                            < candidate.tail_liquidation_shortfall_delta_usd_micro
                        || other.volume_weighted_execution_cost_delta_ppb
                            < candidate.volume_weighted_execution_cost_delta_ppb
                        || other.p95_slippage_delta_ppb < candidate.p95_slippage_delta_ppb
                        || other.capital_weighted_stakeholder_revenue_uplift_bps
                            > candidate.capital_weighted_stakeholder_revenue_uplift_bps
                        || other.equal_market_stakeholder_revenue_uplift_bps
                            > candidate.equal_market_stakeholder_revenue_uplift_bps)
            })
        })
        .map(|score| score.profile_id.clone())
        .collect();
    ids.sort();
    ids
}

fn source_fingerprints(context: &RunContext) -> Result<SourceFingerprintV1> {
    let dusk_program = context.root().join("../../programs/dusk");
    let indexer_decoder = context
        .root()
        .join("../../../omnipair-indexer/indexer/decoders/omnipair_decoder");
    let v1_program = context
        .root()
        .join("../../../omnipair-rs/programs/omnipair");
    let query_sha256 = BTreeMap::from([
        ("markets.sql".to_owned(), sha256_hex(MARKETS_SQL.as_bytes())),
        (
            "events.sql".to_owned(),
            sha256_hex(crate::extract::EVENTS_SQL.as_bytes()),
        ),
        (
            "coverage.sql".to_owned(),
            sha256_hex(COVERAGE_SQL.as_bytes()),
        ),
        ("prices.sql".to_owned(), sha256_hex(PRICES_SQL.as_bytes())),
        (
            DECODER_REVISIONS_TOML.to_owned(),
            sha256_file(&context.root().join(DECODER_REVISIONS_TOML))?,
        ),
    ]);
    Ok(SourceFingerprintV1 {
        benchmark_run_source_sha256: context.source_identity_sha256().to_owned(),
        dusk_source_sha256: hash_tree(&dusk_program)?,
        indexer_decoder_source_sha256: hash_tree(&indexer_decoder)?,
        v1_program_source_sha256: hash_tree(&v1_program)?,
        query_sha256,
    })
}

fn hash_tree(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut digest = Sha256::new();
    for relative in files {
        let path = root.join(&relative);
        let name = relative.to_string_lossy();
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        let bytes = fs::read(path)?;
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    Ok(hex::encode(digest.finalize()))
}

fn collect_files(root: &Path, current: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(current)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, output)?;
        } else if path.is_file() {
            output.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

fn block_path(data_dir: &Path, slot: u64) -> PathBuf {
    data_dir
        .join("blocks")
        .join((slot / ARCHIVED_BLOCK_SHARD_SLOTS).to_string())
        .join(format!("{slot}.json.zst"))
}

const ARCHIVED_BLOCK_ZSTD_LEVEL: i32 = 3;
const ARCHIVED_BLOCK_SHARD_SLOTS: u64 = 1_000_000;

fn block_logical_name(slot: u64) -> String {
    format!(
        "blocks/{}/{}.json.zst",
        slot / ARCHIVED_BLOCK_SHARD_SLOTS,
        slot
    )
}

fn write_archived_block(path: &Path, block: &ArchivedBlock) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = partial_path(path);
    let file = File::create(&partial)?;
    let mut encoder = zstd::stream::write::Encoder::new(file, ARCHIVED_BLOCK_ZSTD_LEVEL)?;
    serde_json::to_writer(&mut encoder, block)?;
    let file = encoder.finish()?;
    file.sync_all()?;
    fs::rename(partial, path)?;
    Ok(())
}

fn read_archived_block(path: &Path) -> Result<ArchivedBlock> {
    let bytes = fs::read(path)?;
    decode_archived_block_cache(&bytes).with_context(|| format!("decoding {}", path.display()))
}

fn decode_archived_block_cache(bytes: &[u8]) -> Result<ArchivedBlock> {
    let decoder = zstd::stream::read::Decoder::new(bytes)?;
    serde_json::from_reader(decoder).context("decoding compressed archived block JSON")
}

#[cfg(test)]
mod archived_block_cache_tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn archived_block_cache_is_atomic_zstd_and_round_trips() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "v1-vs-dusk-block-cache-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("create test directory");
        let path = directory.join("42.json.zst");
        let block = ArchivedBlock {
            slot: 42,
            block_time_unix_seconds: Some(1_700_000_000),
            blockhash: "blockhash".to_owned(),
            previous_blockhash: "previous".to_owned(),
            parent_slot: 41,
            full_transaction_count: 0,
            full_transaction_inventory_sha256: "a".repeat(64),
            proof_basis: ArchivedBlockProofBasisV1::FullPayloadV1,
            transactions: Vec::new(),
        };

        write_archived_block(&path, &block).expect("write compressed block");
        assert!(!partial_path(&path).exists());
        let bytes = fs::read(&path).expect("read compressed bytes");
        assert_eq!(&bytes[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
        assert_eq!(read_archived_block(&path).expect("read block"), block);

        fs::remove_file(path).expect("remove test block");
        fs::remove_dir(directory).expect("remove test directory");
    }
}

#[cfg(test)]
mod verified_archival_block_cache_tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::extract::{
        canonical_transaction_payload_sha256, independent_get_transaction_request_sha256,
        ordered_signature_block_request_sha256, ordered_signature_inventory_sha256,
        IndependentGetTransactionNormalizationV1,
    };

    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "v1-vs-dusk-verified-cache-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create cache test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn signature(seed: u8) -> String {
        bs58::encode([seed; 64]).into_string()
    }

    fn fixture_block(slot: u64, signature: &str) -> ArchivedBlock {
        let transaction = serde_json::json!({"signatures": [signature]});
        let meta = serde_json::json!({"err": null});
        ArchivedBlock {
            slot,
            block_time_unix_seconds: Some(1_700_000_000),
            blockhash: format!("blockhash-{slot}"),
            previous_blockhash: format!("blockhash-{}", slot.saturating_sub(1)),
            parent_slot: slot.saturating_sub(1),
            full_transaction_count: 1,
            full_transaction_inventory_sha256: ordered_signature_inventory_sha256(&[
                signature.to_owned()
            ])
            .unwrap(),
            proof_basis: ArchivedBlockProofBasisV1::OrderedSignaturesV2 {
                ordered_signatures: vec![signature.to_owned()],
                canonical_request_sha256: ordered_signature_block_request_sha256(slot),
                raw_response_sha256: "a".repeat(64),
            },
            transactions: vec![crate::extract::ArchivedTransaction {
                signature: signature.to_owned(),
                transaction_index: 0,
                succeeded: true,
                rpc_payload_sha256: canonical_transaction_payload_sha256(&transaction, &meta),
                proof_basis: ArchivedTransactionProofBasisV1::IndependentGetTransactionV1 {
                    canonical_request_sha256: independent_get_transaction_request_sha256(signature),
                    raw_response_sha256: "b".repeat(64),
                    normalization: IndependentGetTransactionNormalizationV1::RemoveMetaRewardsV1,
                },
                transaction,
                meta,
            }],
        }
    }

    fn write_donor(
        parent: &Path,
        slot: u64,
        signature: &str,
    ) -> (PathBuf, ArchivalBlockCacheConfig, Vec<u8>) {
        let donor = parent.join("donor");
        fs::create_dir_all(&donor).unwrap();
        let block = fixture_block(slot, signature);
        let block_path = block_path(&donor, slot);
        write_archived_block(&block_path, &block).unwrap();
        let bytes = fs::read(&block_path).unwrap();
        let manifest = BlockArtifactManifestV1::new(
            slot,
            slot,
            vec![checksum_archived_block_artifact(
                &block,
                block_logical_name(slot),
                &bytes,
            )],
        )
        .unwrap();
        let manifest_sha256 = manifest.manifest_sha256.clone();
        let manifest_path = donor.join(BLOCK_ARTIFACTS_JSON);
        write_json(&manifest_path, &manifest).unwrap();
        let config = ArchivalBlockCacheConfig {
            root: donor.to_string_lossy().into_owned(),
            manifest_file_sha256: sha256_file(&manifest_path).unwrap(),
            manifest_sha256,
            policy: ArchivalBlockCachePolicy::RequireCompleteHardlink,
        };
        (donor, config, bytes)
    }

    #[test]
    fn exact_cache_is_hardlinked_idempotently_and_receipt_is_run_bound() {
        let directory = TestDirectory::new();
        let slot = 42;
        let target_signature = signature(7);
        let (donor, config, donor_bytes) = write_donor(&directory.0, slot, &target_signature);
        let destination = directory.0.join("current");
        fs::create_dir_all(&destination).unwrap();
        let signatures = BTreeMap::from([(slot, BTreeSet::from([target_signature]))]);
        let cache =
            VerifiedArchivalBlockCacheV1::open(&directory.0, &destination, &config).unwrap();
        cache.preflight_complete(&signatures).unwrap();
        cache
            .materialize_and_validate(&destination, slot, signatures.get(&slot).unwrap())
            .unwrap();
        cache
            .materialize_and_validate(&destination, slot, signatures.get(&slot).unwrap())
            .unwrap();

        let destination_path = block_path(&destination, slot);
        assert_eq!(fs::read(&destination_path).unwrap(), donor_bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::metadata(block_path(&donor, slot)).unwrap().ino(),
                fs::metadata(&destination_path).unwrap().ino()
            );
        }

        let decoded_manifest =
            BlockArtifactManifestV1::new(slot, 100, cache.manifest.blocks.clone()).unwrap();
        write_json(&destination.join(BLOCK_ARTIFACTS_JSON), &decoded_manifest).unwrap();
        let receipt = cache
            .receipt(&destination, "current-run", 100, "program", &signatures)
            .unwrap();
        receipt
            .verify(
                "current-run",
                100,
                "program",
                &cache,
                &signatures,
                &decoded_manifest,
            )
            .unwrap();
        let mut wrong_run = receipt;
        wrong_run.run_id = "another-run".to_owned();
        assert!(wrong_run
            .verify(
                "current-run",
                100,
                "program",
                &cache,
                &signatures,
                &decoded_manifest,
            )
            .is_err());

        let mut mismatched_entry = cache.manifest.blocks[0].clone();
        mismatched_entry.artifact.sha256 = "0".repeat(64);
        let mismatched_manifest =
            BlockArtifactManifestV1::new(slot, 100, vec![mismatched_entry]).unwrap();
        write_json(
            &destination.join(BLOCK_ARTIFACTS_JSON),
            &mismatched_manifest,
        )
        .unwrap();
        assert!(cache
            .receipt(&destination, "current-run", 100, "program", &signatures)
            .is_err());
    }

    #[test]
    fn cache_manifest_and_current_signature_scope_fail_closed() {
        let directory = TestDirectory::new();
        let slot = 77;
        let target_signature = signature(8);
        let (_, mut config, _) = write_donor(&directory.0, slot, &target_signature);
        let destination = directory.0.join("current");
        fs::create_dir_all(&destination).unwrap();

        config.manifest_file_sha256 = "0".repeat(64);
        assert!(VerifiedArchivalBlockCacheV1::open(&directory.0, &destination, &config).is_err());

        config.manifest_file_sha256 =
            sha256_file(&PathBuf::from(&config.root).join(BLOCK_ARTIFACTS_JSON)).unwrap();
        let cache =
            VerifiedArchivalBlockCacheV1::open(&directory.0, &destination, &config).unwrap();
        let wrong_scope = BTreeMap::from([(slot + 1, BTreeSet::from([signature(9)]))]);
        assert!(cache.preflight_complete(&wrong_scope).is_err());

        let wrong_signatures = BTreeSet::from([signature(10)]);
        assert!(cache
            .materialize_and_validate(&destination, slot, &wrong_signatures)
            .is_err());
    }

    #[test]
    fn detached_or_conflicting_destination_is_rejected_and_atomic_replacement_preserves_donor() {
        let directory = TestDirectory::new();
        let slot = 99;
        let target_signature = signature(11);
        let (donor, config, donor_bytes) = write_donor(&directory.0, slot, &target_signature);
        let destination = directory.0.join("current");
        fs::create_dir_all(block_path(&destination, slot).parent().unwrap()).unwrap();
        fs::write(block_path(&destination, slot), &donor_bytes).unwrap();
        let cache =
            VerifiedArchivalBlockCacheV1::open(&directory.0, &destination, &config).unwrap();
        let expected = BTreeSet::from([target_signature]);
        assert!(cache
            .materialize_and_validate(&destination, slot, &expected)
            .is_err());

        fs::write(block_path(&destination, slot), b"not-the-donor").unwrap();
        assert!(cache
            .materialize_and_validate(&destination, slot, &expected)
            .is_err());

        fs::remove_file(block_path(&destination, slot)).unwrap();
        cache
            .materialize_and_validate(&destination, slot, &expected)
            .unwrap();
        let replacement = fixture_block(slot, &signature(12));
        write_archived_block(&block_path(&destination, slot), &replacement).unwrap();
        assert_eq!(fs::read(block_path(&donor, slot)).unwrap(), donor_bytes);
        assert_ne!(
            fs::read(block_path(&destination, slot)).unwrap(),
            donor_bytes
        );
    }

    #[test]
    #[ignore = "operator smoke test scans the configured multi-gigabyte donor cache"]
    fn shipped_donor_bytes_and_proofs_match_the_pinned_manifest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = BenchmarkConfig::from_path(&root.join("benchmark.toml")).unwrap();
        let cache_config = config.source.archival_block_cache.as_ref().unwrap();
        let directory = TestDirectory::new();
        let cache = VerifiedArchivalBlockCacheV1::open(root, &directory.0, cache_config).unwrap();
        let mut bytes_verified = 0_u64;
        for (slot, entry) in &cache.blocks_by_slot {
            let path = cache.source_path(*slot).unwrap();
            let bytes = fs::read(path).unwrap();
            assert!(entry.artifact.matches_bytes(&bytes));
            let block = decode_archived_block_cache(&bytes).unwrap();
            assert_eq!(entry.blockhash, block.blockhash);
            assert_eq!(entry.parent_slot, block.parent_slot);
            assert_eq!(entry.previous_blockhash, block.previous_blockhash);
            assert_eq!(entry.transaction_count, block.full_transaction_count);
            let signatures = block
                .transactions
                .iter()
                .map(|transaction| transaction.signature.clone())
                .collect::<BTreeSet<_>>();
            assert!(!signatures.is_empty());
            validate_composed_block(&block, *slot, &signatures).unwrap();
            bytes_verified = bytes_verified.checked_add(bytes.len() as u64).unwrap();
        }
        assert_eq!(cache.blocks_by_slot.len(), cache.manifest.blocks.len());
        assert_eq!(
            bytes_verified,
            cache
                .manifest
                .blocks
                .iter()
                .map(|block| block.artifact.byte_length)
                .sum::<u64>()
        );
    }
}

fn partial_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".partial");
    PathBuf::from(name)
}

fn write_raw_json_lines(path: &Path, rows: &[String]) -> Result<()> {
    let partial = partial_path(path);
    let mut writer = BufWriter::new(File::create(&partial)?);
    for row in rows {
        let _: JsonValue = serde_json::from_str(row).context("validating database JSON export")?;
        writer.write_all(row.as_bytes())?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    fs::rename(partial, path)?;
    Ok(())
}

fn read_json_lines<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let reader = BufReader::new(File::open(path)?);
    reader
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let line = line?;
            serde_json::from_str(&line)
                .with_context(|| format!("decoding {} line {}", path.display(), index + 1))
        })
        .collect()
}

fn visit_json_lines<T: DeserializeOwned, F>(path: &Path, mut visit: F) -> Result<()>
where
    F: FnMut(T) -> Result<()>,
{
    let reader = BufReader::new(File::open(path)?);
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        let value = serde_json::from_str(&line)
            .with_context(|| format!("decoding {} line {}", path.display(), index + 1))?;
        visit(value)?;
    }
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = partial_path(path);
    let mut writer = BufWriter::new(File::create(&partial)?);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    fs::rename(partial, path)?;
    Ok(())
}

fn write_json_line<T: Serialize>(writer: &mut BufWriter<File>, value: &T) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(BufReader::new(File::open(path)?))
        .with_context(|| format!("decoding {}", path.display()))
}

fn ensure_parquet_artifact(spec: &ParquetExportSpecV1<'_>) -> Result<ParquetFileManifestV1> {
    match (spec.parquet_output.exists(), spec.manifest_output.exists()) {
        (false, false) => Ok(export_jsonl_to_parquet(spec)?),
        (true, true) => {
            let manifest = validate_parquet_artifact(spec.parquet_output, spec.manifest_output)?;
            if manifest.dataset != spec.dataset
                || manifest.logical_name != spec.logical_name
                || manifest.source_jsonl_sha256 != sha256_file(spec.source_jsonl)?
            {
                bail!(
                    "existing Parquet artifact {} does not bind to its current declared source",
                    spec.parquet_output.display()
                );
            }
            Ok(manifest)
        }
        _ => bail!(
            "Parquet artifact and manifest must both exist or both be absent: {}, {}",
            spec.parquet_output.display(),
            spec.manifest_output.display()
        ),
    }
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

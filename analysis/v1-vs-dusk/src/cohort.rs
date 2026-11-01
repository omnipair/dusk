//! Deterministic, training-only market cohort selection.
//!
//! The selector consumes an event export that is already sealed to the slot
//! prefix declared in the request. Any row outside that prefix is rejected,
//! rather than filtered, so validation or holdout activity cannot influence a
//! cohort accidentally. `MarketEvidence`'s full-window quantitative fields are
//! deliberately ignored: only its cutoff visibility flag defines the pre-audit
//! candidate population, and every ranking statistic is rebuilt from the
//! sealed DB rows. Final RPC reconciliation and audit gates remain mandatory.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use num_bigint::BigInt;
use num_traits::Zero;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::{
    audit::MarketEvidence,
    config::{EvidenceTier, ScopeConfig, ScopeMode},
    extract::DbEventSeed,
};

pub const COHORT_SELECTION_SCHEMA_VERSION: u16 = 1;
const ARTIFACT_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:cohort-selection:v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortSourceBindingsV1 {
    /// Hash of the frozen v2 benchmark configuration.
    pub config_sha256: String,
    /// Hash of the split/window artifact that proves the selection slot prefix.
    pub sealed_prefix_sha256: String,
    /// Hash of the prefix-scoped pre-audit market visibility projection.
    pub market_evidence_sha256: String,
    /// Hash of the DB event export containing exactly the sealed prefix rows.
    pub db_event_seeds_sha256: String,
    /// Hash of the market-to-mint and edge-evidence artifact.
    pub market_metadata_sha256: String,
    /// Required when the scope keeps the independent global program signature
    /// inventory. That inventory is a completeness proof, not a ranking input.
    pub global_signature_inventory_sha256: Option<String>,
}

impl CohortSourceBindingsV1 {
    fn validate(&self, keep_global_signature_inventory: bool) -> Result<(), CohortError> {
        for (name, hash) in [
            ("config", self.config_sha256.as_str()),
            ("sealed prefix", self.sealed_prefix_sha256.as_str()),
            ("market evidence", self.market_evidence_sha256.as_str()),
            ("DB event seeds", self.db_event_seeds_sha256.as_str()),
            ("market metadata", self.market_metadata_sha256.as_str()),
        ] {
            if !valid_sha256(hash) {
                return Err(CohortError::InvalidSourceHash(name));
            }
        }
        match (
            keep_global_signature_inventory,
            self.global_signature_inventory_sha256.as_deref(),
        ) {
            (true, Some(hash)) if valid_sha256(hash) => Ok(()),
            (true, _) => Err(CohortError::InvalidSourceHash("global signature inventory")),
            (false, Some(hash)) if !valid_sha256(hash) => {
                Err(CohortError::InvalidSourceHash("global signature inventory"))
            }
            (false, _) => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortEdgeUnavailableReasonV1 {
    MintHistoryUnavailable,
    ConfigurationHistoryUnavailable,
    MintAndConfigurationHistoryUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum CohortEdgeEvidenceV1 {
    Exact {
        token_2022_mint_present: bool,
        configuration_edge_present: bool,
    },
    Unavailable {
        reason: CohortEdgeUnavailableReasonV1,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortTailQualificationUnavailableReasonV1 {
    AuthoritativeBlockTimeUnavailable,
    PrefixEventCoverageUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum CohortTailQualificationEvidenceV1 {
    /// Counts are scoped to this request's sealed prefix. Active days must be
    /// derived from authoritative RPC block time and revalidated by the
    /// extraction/audit stage before headline use.
    Exact {
        prefix_swap_count: u64,
        prefix_active_swap_days: u32,
    },
    Unavailable {
        reason: CohortTailQualificationUnavailableReasonV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortMarketMetadataV1 {
    pub pair_address: String,
    pub token0_mint: String,
    pub token1_mint: String,
    pub lp_mint: String,
    pub edge_evidence: CohortEdgeEvidenceV1,
    pub tail_qualification_evidence: CohortTailQualificationEvidenceV1,
}

impl CohortMarketMetadataV1 {
    fn validate(&self) -> Result<(), CohortError> {
        if self.pair_address.trim().is_empty() {
            return Err(CohortError::InvalidMarketMetadata(
                "empty pair address".to_owned(),
            ));
        }
        let mut mints = BTreeSet::new();
        for mint in [&self.token0_mint, &self.token1_mint, &self.lp_mint] {
            Pubkey::from_str(mint).map_err(|_| {
                CohortError::InvalidMarketMetadata(format!(
                    "market {} contains an invalid mint",
                    self.pair_address
                ))
            })?;
            if !mints.insert(mint) {
                return Err(CohortError::InvalidMarketMetadata(format!(
                    "market {} repeats a mint",
                    self.pair_address
                )));
            }
        }
        if let CohortTailQualificationEvidenceV1::Exact {
            prefix_swap_count,
            prefix_active_swap_days,
        } = &self.tail_qualification_evidence
        {
            if u64::from(*prefix_active_swap_days) > *prefix_swap_count {
                return Err(CohortError::InvalidMarketMetadata(format!(
                    "market {} has more prefix active days than swaps",
                    self.pair_address
                )));
            }
        }
        Ok(())
    }

    fn mints(&self) -> [&str; 3] {
        [&self.token0_mint, &self.token1_mint, &self.lp_mint]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortArchetypeV1 {
    SpotActivity,
    LendingToken0,
    LendingToken1,
    Liquidation,
    LowActivityPreAuditTail,
    Token2022OrConfigurationEdge,
}

const ARCHETYPES: [CohortArchetypeV1; 6] = [
    CohortArchetypeV1::SpotActivity,
    CohortArchetypeV1::LendingToken0,
    CohortArchetypeV1::LendingToken1,
    CohortArchetypeV1::Liquidation,
    CohortArchetypeV1::LowActivityPreAuditTail,
    CohortArchetypeV1::Token2022OrConfigurationEdge,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortFallbackReasonV1 {
    HigherRankedCandidateAlreadySelected,
    HigherRankedCandidateMissingMintBinding,
    HigherRankedCandidateUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortUnavailableReasonV1 {
    TargetCountReached,
    NoVisiblePreAuditMarket,
    NoPrefixSpotActivity,
    NoPrefixToken0Lending,
    NoPrefixToken1Lending,
    NoPrefixLiquidation,
    NoPrefixBaseQualifiedTailMarket,
    TailQualificationEvidenceUnavailable,
    NoToken2022OrConfigurationEdge,
    EdgeEvidenceUnavailable,
    MissingMintBinding,
    NoUniqueCandidate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CohortArchetypeStatusV1 {
    Selected {
        pair_address: String,
        /// Zero is the primary ranked candidate. A positive value is a
        /// deterministic fallback after an earlier candidate was unavailable.
        rank: u32,
        fallback_reason: Option<CohortFallbackReasonV1>,
    },
    Unavailable {
        reason: CohortUnavailableReasonV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortArchetypeOutcomeV1 {
    pub archetype: CohortArchetypeV1,
    pub outcome: CohortArchetypeStatusV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortBlockingReasonV1 {
    MinimumPreAuditPanelMarketsNotMet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortSelectionStageV1 {
    PreAudit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortSelectionArtifactV1 {
    pub schema_version: u16,
    pub run_id: String,
    /// Inclusive finalized cutoff for the entire benchmark run.
    pub cutoff_slot: u64,
    pub selection_stage: CohortSelectionStageV1,
    /// Always true. The cohort is an extraction accelerator, not evidence that
    /// decoder, terminal, DB-to-RPC, or market audit gates passed.
    pub final_audit_required: bool,
    pub mode: ScopeMode,
    /// Identity only: this module never changes gates based on the tier.
    pub evidence_tier: EvidenceTier,
    pub selection_training_bps: u16,
    pub selection_start_slot: u64,
    pub selection_end_slot_exclusive: u64,
    pub minimum_prefix_swaps_for_tail: u64,
    pub minimum_prefix_active_days_for_tail: u32,
    pub target_count: usize,
    pub minimum_panel_markets: usize,
    pub keep_global_signature_inventory: bool,
    pub per_market_opportunity_ceiling: bool,
    pub explicit_market_overrides: Vec<String>,
    pub source_bindings: CohortSourceBindingsV1,
    pub archetype_outcomes: Vec<CohortArchetypeOutcomeV1>,
    /// Canonically sorted full-history extraction targets.
    pub selected_markets: Vec<String>,
    /// Prefix-only evidence used to reproduce selection. These are not the
    /// full-lifecycle signatures or slots for replay extraction.
    pub selection_prefix_signatures: Vec<String>,
    pub selection_prefix_slots: Vec<u64>,
    pub selected_mints: Vec<String>,
    pub minimum_pre_audit_panel_met: bool,
    pub blocking_reasons: Vec<CohortBlockingReasonV1>,
    pub artifact_sha256: String,
}

/// User-facing name for the current versioned contract.
pub type CohortSelectionArtifact = CohortSelectionArtifactV1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CohortExtractionTargetsV1 {
    pub markets: Vec<String>,
    pub mints: Vec<String>,
    pub keep_global_signature_inventory: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CohortSelectionPrefixEvidenceV1 {
    pub signatures: Vec<String>,
    pub slots: Vec<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortFinalAuditRejectionReasonV1 {
    MissingFinalAuditEvidence,
    NotVisible,
    HardGatesFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortFinalAuditRejectionV1 {
    pub pair_address: String,
    pub reason: CohortFinalAuditRejectionReasonV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortFinalAuditQualificationV1 {
    pub accepted_markets: Vec<String>,
    pub rejected_markets: Vec<CohortFinalAuditRejectionV1>,
    pub minimum_final_panel_met: bool,
}

impl CohortSelectionArtifactV1 {
    fn seal(mut self) -> Result<Self, CohortError> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = hash_serialized(&self)?;
        self.verify_self_hash()?;
        Ok(self)
    }

    pub fn verify_self_hash(&self) -> Result<(), CohortError> {
        if self.schema_version != COHORT_SELECTION_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || self.selection_stage != CohortSelectionStageV1::PreAudit
            || !self.final_audit_required
            || self.selection_start_slot >= self.selection_end_slot_exclusive
            || self.selection_end_slot_exclusive.saturating_sub(1) > self.cutoff_slot
            || self.selection_training_bps == 0
            || self.selection_training_bps > 10_000
            || self.minimum_prefix_swaps_for_tail == 0
            || self.minimum_prefix_active_days_for_tail == 0
            || self.minimum_panel_markets == 0
            || !valid_sha256(&self.artifact_sha256)
        {
            return Err(CohortError::InvalidArtifactIdentity);
        }
        self.source_bindings
            .validate(self.keep_global_signature_inventory)?;
        ensure_sorted_unique_nonempty("selected markets", &self.selected_markets)?;
        ensure_sorted_unique_nonempty(
            "selection-prefix signatures",
            &self.selection_prefix_signatures,
        )?;
        ensure_sorted_unique("selection-prefix slots", &self.selection_prefix_slots)?;
        ensure_sorted_unique_nonempty("selected mints", &self.selected_mints)?;
        ensure_sorted_unique_nonempty(
            "explicit market overrides",
            &self.explicit_market_overrides,
        )?;
        if self.selection_prefix_slots.iter().any(|slot| {
            *slot < self.selection_start_slot || *slot >= self.selection_end_slot_exclusive
        }) {
            return Err(CohortError::InvalidArtifactIdentity);
        }
        if self
            .explicit_market_overrides
            .iter()
            .any(|market| self.selected_markets.binary_search(market).is_err())
        {
            return Err(CohortError::InvalidArtifactIdentity);
        }

        match self.mode {
            ScopeMode::HybridPanel => {
                if !(5..=6).contains(&self.target_count)
                    || self.minimum_panel_markets > self.target_count
                    || self.selected_markets.len() > self.target_count
                    || self.archetype_outcomes.len() != ARCHETYPES.len()
                    || self
                        .archetype_outcomes
                        .iter()
                        .zip(ARCHETYPES)
                        .any(|(outcome, archetype)| outcome.archetype != archetype)
                {
                    return Err(CohortError::InvalidArtifactIdentity);
                }
                let mut outcome_markets = BTreeSet::new();
                for outcome in &self.archetype_outcomes {
                    if let CohortArchetypeStatusV1::Selected {
                        pair_address,
                        rank,
                        fallback_reason,
                    } = &outcome.outcome
                    {
                        if self.selected_markets.binary_search(pair_address).is_err()
                            || !outcome_markets.insert(pair_address)
                            || (*rank == 0 && fallback_reason.is_some())
                            || (*rank > 0 && fallback_reason.is_none())
                        {
                            return Err(CohortError::InvalidArtifactIdentity);
                        }
                    }
                }
                let expected_minimum = self.selected_markets.len() >= self.minimum_panel_markets;
                let expected_blockers = if expected_minimum {
                    Vec::new()
                } else {
                    vec![CohortBlockingReasonV1::MinimumPreAuditPanelMarketsNotMet]
                };
                if self.minimum_pre_audit_panel_met != expected_minimum
                    || self.blocking_reasons != expected_blockers
                {
                    return Err(CohortError::InvalidArtifactIdentity);
                }
            }
            ScopeMode::FullUniverse => {
                if !self.archetype_outcomes.is_empty()
                    || !self.explicit_market_overrides.is_empty()
                    || !self.minimum_pre_audit_panel_met
                    || !self.blocking_reasons.is_empty()
                {
                    return Err(CohortError::InvalidArtifactIdentity);
                }
            }
        }

        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256 != hash_serialized(&unsigned)? {
            return Err(CohortError::SelfHashMismatch);
        }
        Ok(())
    }

    /// Returns targeting sets only when the versioned artifact is intact and
    /// the pre-audit hybrid panel meets its configured minimum size. Final
    /// audit requalification remains mandatory for every returned market.
    pub fn extraction_targets(&self) -> Result<CohortExtractionTargetsV1, CohortError> {
        self.verify_self_hash()?;
        if !self.minimum_pre_audit_panel_met {
            return Err(CohortError::MinimumPreAuditPanelNotMet);
        }
        Ok(CohortExtractionTargetsV1 {
            markets: self.selected_markets.clone(),
            mints: self.selected_mints.clone(),
            keep_global_signature_inventory: self.keep_global_signature_inventory,
        })
    }

    /// Returns only the sealed-prefix evidence used to reproduce cohort
    /// selection. Callers must not treat this as the full replay history.
    pub fn selection_prefix_evidence(
        &self,
    ) -> Result<CohortSelectionPrefixEvidenceV1, CohortError> {
        self.verify_self_hash()?;
        Ok(CohortSelectionPrefixEvidenceV1 {
            signatures: self.selection_prefix_signatures.clone(),
            slots: self.selection_prefix_slots.clone(),
        })
    }

    /// Applies the later RPC/reconciliation-backed market audit without
    /// changing the sealed selection artifact. The caller must persist and
    /// hash this qualification in the downstream audit manifest.
    pub fn qualify_after_final_audit(
        &self,
        final_market_evidence: &[MarketEvidence],
    ) -> Result<CohortFinalAuditQualificationV1, CohortError> {
        self.verify_self_hash()?;
        let by_pair = market_evidence_by_pair(final_market_evidence)?;
        let mut accepted_markets = Vec::new();
        let mut rejected_markets = Vec::new();
        for pair in &self.selected_markets {
            let reason = match by_pair.get(pair) {
                None => Some(CohortFinalAuditRejectionReasonV1::MissingFinalAuditEvidence),
                Some(evidence) if !evidence.visible => {
                    Some(CohortFinalAuditRejectionReasonV1::NotVisible)
                }
                Some(evidence) if !evidence.hard_gates_pass => {
                    Some(CohortFinalAuditRejectionReasonV1::HardGatesFailed)
                }
                Some(_) => None,
            };
            if let Some(reason) = reason {
                rejected_markets.push(CohortFinalAuditRejectionV1 {
                    pair_address: pair.clone(),
                    reason,
                });
            } else {
                accepted_markets.push(pair.clone());
            }
        }
        let minimum_final_panel_met = self.mode == ScopeMode::FullUniverse
            || accepted_markets.len() >= self.minimum_panel_markets;
        Ok(CohortFinalAuditQualificationV1 {
            accepted_markets,
            rejected_markets,
            minimum_final_panel_met,
        })
    }
}

#[derive(Clone, Debug)]
pub struct CohortSelectionRequestV1<'a> {
    pub run_id: &'a str,
    pub cutoff_slot: u64,
    pub selection_start_slot: u64,
    pub selection_end_slot_exclusive: u64,
    /// Frozen from the configured universe policy. The tail archetype is not
    /// pre-audit qualified without both minimums and exact prefix-scoped evidence.
    pub minimum_prefix_swaps_for_tail: u64,
    pub minimum_prefix_active_days_for_tail: u32,
    pub scope: &'a ScopeConfig,
    pub source_bindings: &'a CohortSourceBindingsV1,
    pub market_evidence: &'a [MarketEvidence],
    /// Must contain exactly sealed-prefix rows. Later rows are an error, even
    /// though ignoring them would produce the same rankings.
    pub db_event_seeds: &'a [DbEventSeed],
    pub market_metadata: &'a [CohortMarketMetadataV1],
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CohortError {
    #[error("invalid cohort source hash: {0}")]
    InvalidSourceHash(&'static str),
    #[error("invalid cohort request: {0}")]
    InvalidRequest(String),
    #[error("duplicate market evidence for {0}")]
    DuplicateMarketEvidence(String),
    #[error("duplicate market metadata for {0}")]
    DuplicateMarketMetadata(String),
    #[error("invalid market metadata: {0}")]
    InvalidMarketMetadata(String),
    #[error("event seed is outside the sealed selection prefix: {0}")]
    EventOutsideSelectionPrefix(String),
    #[error("event seed references unknown market: {0}")]
    UnknownEventMarket(String),
    #[error("duplicate event seed identity: {0}")]
    DuplicateEventSeed(String),
    #[error("invalid event seed: {0}")]
    InvalidEventSeed(String),
    #[error("explicit market override is unknown: {0}")]
    UnknownOverride(String),
    #[error("explicit market override is not visible at the cutoff: {0}")]
    InvisibleOverride(String),
    #[error("selected market has no exact mint binding: {0}")]
    MissingMintBinding(String),
    #[error("cohort artifact identity is invalid")]
    InvalidArtifactIdentity,
    #[error("cohort artifact self-hash mismatch")]
    SelfHashMismatch,
    #[error("pre-audit hybrid cohort does not meet minimum panel size")]
    MinimumPreAuditPanelNotMet,
    #[error("cohort artifact serialization failed")]
    Serialization,
}

#[derive(Default)]
struct PrefixActivity {
    swaps: u64,
    debt0: u64,
    debt1: u64,
    liquidations: u64,
}

pub fn select_cohort(
    request: CohortSelectionRequestV1<'_>,
) -> Result<CohortSelectionArtifactV1, CohortError> {
    validate_request(&request)?;

    let evidence = market_evidence_by_pair(request.market_evidence)?;
    let metadata = market_metadata_by_pair(request.market_metadata, &evidence)?;
    let activity = prefix_activity(request.db_event_seeds, &evidence, &request)?;
    let pre_audit_candidates: BTreeSet<String> = evidence
        .iter()
        .filter(|(_, market)| market.visible)
        .map(|(pair, _)| pair.clone())
        .collect();
    let tail_qualified = tail_qualified_markets(
        &pre_audit_candidates,
        &activity,
        &metadata,
        request.minimum_prefix_swaps_for_tail,
        request.minimum_prefix_active_days_for_tail,
    )?;

    let mut selected = BTreeSet::new();
    let mut explicit_market_overrides = request.scope.explicit_market_overrides.clone();
    explicit_market_overrides.sort();
    for pair in &explicit_market_overrides {
        let market = evidence
            .get(pair)
            .ok_or_else(|| CohortError::UnknownOverride(pair.clone()))?;
        if !market.visible {
            return Err(CohortError::InvisibleOverride(pair.clone()));
        }
        if !metadata.contains_key(pair) {
            return Err(CohortError::MissingMintBinding(pair.clone()));
        }
        selected.insert(pair.clone());
    }

    let mut archetype_outcomes = Vec::new();
    if request.scope.mode == ScopeMode::FullUniverse {
        for pair in &pre_audit_candidates {
            if !metadata.contains_key(pair) {
                return Err(CohortError::MissingMintBinding(pair.clone()));
            }
            selected.insert(pair.clone());
        }
    } else {
        for archetype in ARCHETYPES {
            let ranked = ranked_candidates(
                archetype,
                &pre_audit_candidates,
                &tail_qualified,
                &activity,
                &metadata,
            );
            let no_candidate_reason =
                unavailable_reason(archetype, &pre_audit_candidates, &tail_qualified, &metadata);
            let outcome = select_for_archetype(
                archetype,
                ranked,
                no_candidate_reason,
                request.scope.target_count,
                &mut selected,
                &metadata,
            );
            archetype_outcomes.push(CohortArchetypeOutcomeV1 { archetype, outcome });
        }
    }

    let mut selection_prefix_signatures = BTreeSet::new();
    let mut selection_prefix_slots = BTreeSet::new();
    for event in request.db_event_seeds {
        if selected.contains(&event.pair) {
            selection_prefix_signatures.insert(event.signature.clone());
            selection_prefix_slots.insert(event.slot);
        }
    }
    let mut selected_mints = BTreeSet::new();
    for pair in &selected {
        let binding = metadata
            .get(pair)
            .ok_or_else(|| CohortError::MissingMintBinding(pair.clone()))?;
        selected_mints.extend(binding.mints().into_iter().map(str::to_owned));
    }

    let minimum_pre_audit_panel_met = request.scope.mode == ScopeMode::FullUniverse
        || selected.len() >= request.scope.minimum_panel_markets;
    let blocking_reasons = if minimum_pre_audit_panel_met {
        Vec::new()
    } else {
        vec![CohortBlockingReasonV1::MinimumPreAuditPanelMarketsNotMet]
    };
    CohortSelectionArtifactV1 {
        schema_version: COHORT_SELECTION_SCHEMA_VERSION,
        run_id: request.run_id.to_owned(),
        cutoff_slot: request.cutoff_slot,
        selection_stage: CohortSelectionStageV1::PreAudit,
        final_audit_required: true,
        mode: request.scope.mode,
        evidence_tier: request.scope.evidence_tier,
        selection_training_bps: request.scope.selection_training_bps,
        selection_start_slot: request.selection_start_slot,
        selection_end_slot_exclusive: request.selection_end_slot_exclusive,
        minimum_prefix_swaps_for_tail: request.minimum_prefix_swaps_for_tail,
        minimum_prefix_active_days_for_tail: request.minimum_prefix_active_days_for_tail,
        target_count: request.scope.target_count,
        minimum_panel_markets: request.scope.minimum_panel_markets,
        keep_global_signature_inventory: request.scope.keep_global_signature_inventory,
        per_market_opportunity_ceiling: request.scope.per_market_opportunity_ceiling,
        explicit_market_overrides,
        source_bindings: request.source_bindings.clone(),
        archetype_outcomes,
        selected_markets: selected.into_iter().collect(),
        selection_prefix_signatures: selection_prefix_signatures.into_iter().collect(),
        selection_prefix_slots: selection_prefix_slots.into_iter().collect(),
        selected_mints: selected_mints.into_iter().collect(),
        minimum_pre_audit_panel_met,
        blocking_reasons,
        artifact_sha256: String::new(),
    }
    .seal()
}

fn validate_request(request: &CohortSelectionRequestV1<'_>) -> Result<(), CohortError> {
    if request.run_id.trim().is_empty() {
        return Err(CohortError::InvalidRequest("run_id is empty".to_owned()));
    }
    if request.selection_start_slot >= request.selection_end_slot_exclusive
        || request.selection_end_slot_exclusive.saturating_sub(1) > request.cutoff_slot
    {
        return Err(CohortError::InvalidRequest(
            "selection prefix is empty or exceeds cutoff".to_owned(),
        ));
    }
    if request.scope.selection_training_bps == 0
        || request.scope.selection_training_bps > 10_000
        || request.scope.minimum_panel_markets == 0
        || !request.scope.keep_global_signature_inventory
        || request.minimum_prefix_swaps_for_tail == 0
        || request.minimum_prefix_active_days_for_tail == 0
    {
        return Err(CohortError::InvalidRequest(
            "scope is not valid for sealed cohort selection".to_owned(),
        ));
    }
    match request.scope.mode {
        ScopeMode::HybridPanel => {
            if !(5..=6).contains(&request.scope.target_count)
                || request.scope.minimum_panel_markets > request.scope.target_count
                || request.scope.explicit_market_overrides.len() > request.scope.target_count
            {
                return Err(CohortError::InvalidRequest(
                    "hybrid-panel scope is invalid".to_owned(),
                ));
            }
        }
        ScopeMode::FullUniverse => {
            if !request.scope.explicit_market_overrides.is_empty() {
                return Err(CohortError::InvalidRequest(
                    "full-universe scope cannot contain explicit overrides".to_owned(),
                ));
            }
        }
    }
    let override_count = request
        .scope
        .explicit_market_overrides
        .iter()
        .collect::<BTreeSet<_>>()
        .len();
    if override_count != request.scope.explicit_market_overrides.len()
        || request
            .scope
            .explicit_market_overrides
            .iter()
            .any(|pair| pair.trim().is_empty())
    {
        return Err(CohortError::InvalidRequest(
            "explicit overrides must be unique and non-empty".to_owned(),
        ));
    }
    request
        .source_bindings
        .validate(request.scope.keep_global_signature_inventory)
}

fn market_evidence_by_pair<'a>(
    markets: &'a [MarketEvidence],
) -> Result<BTreeMap<String, &'a MarketEvidence>, CohortError> {
    let mut by_pair = BTreeMap::new();
    for market in markets {
        if market.pair_address.trim().is_empty() {
            return Err(CohortError::InvalidRequest(
                "market evidence contains an empty pair address".to_owned(),
            ));
        }
        if by_pair
            .insert(market.pair_address.clone(), market)
            .is_some()
        {
            return Err(CohortError::DuplicateMarketEvidence(
                market.pair_address.clone(),
            ));
        }
    }
    Ok(by_pair)
}

fn market_metadata_by_pair<'a>(
    rows: &'a [CohortMarketMetadataV1],
    evidence: &BTreeMap<String, &'a MarketEvidence>,
) -> Result<BTreeMap<String, &'a CohortMarketMetadataV1>, CohortError> {
    let mut by_pair = BTreeMap::new();
    for row in rows {
        row.validate()?;
        if !evidence.contains_key(&row.pair_address) {
            return Err(CohortError::InvalidMarketMetadata(format!(
                "metadata references unknown market {}",
                row.pair_address
            )));
        }
        if by_pair.insert(row.pair_address.clone(), row).is_some() {
            return Err(CohortError::DuplicateMarketMetadata(
                row.pair_address.clone(),
            ));
        }
    }
    Ok(by_pair)
}

fn prefix_activity(
    events: &[DbEventSeed],
    evidence: &BTreeMap<String, &MarketEvidence>,
    request: &CohortSelectionRequestV1<'_>,
) -> Result<BTreeMap<String, PrefixActivity>, CohortError> {
    let mut activity = BTreeMap::<String, PrefixActivity>::new();
    let mut identities = BTreeSet::new();
    for event in events {
        if event.slot < request.selection_start_slot
            || event.slot >= request.selection_end_slot_exclusive
            || event.slot > request.cutoff_slot
        {
            return Err(CohortError::EventOutsideSelectionPrefix(format!(
                "{}:{}:{}",
                event.source_table, event.source_id, event.slot
            )));
        }
        if event.source_table.trim().is_empty()
            || event.pair.trim().is_empty()
            || event.signature.trim().is_empty()
        {
            return Err(CohortError::InvalidEventSeed(format!(
                "{}:{}",
                event.source_table, event.source_id
            )));
        }
        if !evidence.contains_key(&event.pair) {
            return Err(CohortError::UnknownEventMarket(event.pair.clone()));
        }
        if !identities.insert((event.source_table.clone(), event.source_id)) {
            return Err(CohortError::DuplicateEventSeed(format!(
                "{}:{}",
                event.source_table, event.source_id
            )));
        }
        let row = activity.entry(event.pair.clone()).or_default();
        match event.source_table.as_str() {
            "swaps" => row.swaps = checked_increment(row.swaps)?,
            "adjust_debt_events" => {
                if payload_amount_positive(event, "amount0")? {
                    row.debt0 = checked_increment(row.debt0)?;
                }
                if payload_amount_positive(event, "amount1")? {
                    row.debt1 = checked_increment(row.debt1)?;
                }
            }
            "user_position_liquidated_events" => {
                row.liquidations = checked_increment(row.liquidations)?;
            }
            _ => {}
        }
    }
    Ok(activity)
}

fn payload_amount_positive(event: &DbEventSeed, field: &str) -> Result<bool, CohortError> {
    let value = event
        .payload
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            CohortError::InvalidEventSeed(format!(
                "{}:{} has no string {field}",
                event.source_table, event.source_id
            ))
        })?;
    let parsed = BigInt::from_str(value).map_err(|_| {
        CohortError::InvalidEventSeed(format!(
            "{}:{} has invalid integer {field}",
            event.source_table, event.source_id
        ))
    })?;
    Ok(parsed > BigInt::zero())
}

fn checked_increment(value: u64) -> Result<u64, CohortError> {
    value
        .checked_add(1)
        .ok_or_else(|| CohortError::InvalidRequest("activity counter overflow".to_owned()))
}

fn tail_qualified_markets(
    pre_audit_candidates: &BTreeSet<String>,
    activity: &BTreeMap<String, PrefixActivity>,
    metadata: &BTreeMap<String, &CohortMarketMetadataV1>,
    minimum_swaps: u64,
    minimum_active_days: u32,
) -> Result<BTreeSet<String>, CohortError> {
    let mut output = BTreeSet::new();
    for (pair, row) in metadata {
        let CohortTailQualificationEvidenceV1::Exact {
            prefix_swap_count,
            prefix_active_swap_days,
        } = &row.tail_qualification_evidence
        else {
            continue;
        };
        let observed_swaps = activity.get(pair).map_or(0, |counts| counts.swaps);
        if *prefix_swap_count != observed_swaps {
            return Err(CohortError::InvalidMarketMetadata(format!(
                "market {pair} tail swap proof disagrees with sealed DB rows"
            )));
        }
        if pre_audit_candidates.contains(pair)
            && *prefix_swap_count >= minimum_swaps
            && *prefix_active_swap_days >= minimum_active_days
        {
            output.insert(pair.clone());
        }
    }
    Ok(output)
}

fn ranked_candidates(
    archetype: CohortArchetypeV1,
    pre_audit_candidates: &BTreeSet<String>,
    tail_qualified: &BTreeSet<String>,
    activity: &BTreeMap<String, PrefixActivity>,
    metadata: &BTreeMap<String, &CohortMarketMetadataV1>,
) -> Vec<String> {
    let mut ranked: Vec<(String, u64)> = pre_audit_candidates
        .iter()
        .filter_map(|pair| {
            let counts = activity.get(pair);
            let score = match archetype {
                CohortArchetypeV1::SpotActivity => counts.map_or(0, |row| row.swaps),
                CohortArchetypeV1::LendingToken0 => counts.map_or(0, |row| row.debt0),
                CohortArchetypeV1::LendingToken1 => counts.map_or(0, |row| row.debt1),
                CohortArchetypeV1::Liquidation => counts.map_or(0, |row| row.liquidations),
                CohortArchetypeV1::LowActivityPreAuditTail => {
                    if tail_qualified.contains(pair) {
                        counts.map_or(0, |row| row.swaps)
                    } else {
                        0
                    }
                }
                CohortArchetypeV1::Token2022OrConfigurationEdge => metadata
                    .get(pair)
                    .and_then(|row| match &row.edge_evidence {
                        CohortEdgeEvidenceV1::Exact {
                            token_2022_mint_present,
                            configuration_edge_present,
                        } => Some(
                            u64::from(*token_2022_mint_present)
                                + u64::from(*configuration_edge_present),
                        ),
                        CohortEdgeEvidenceV1::Unavailable { .. } => None,
                    })
                    .unwrap_or(0),
            };
            (score > 0).then(|| (pair.clone(), score))
        })
        .collect();
    if archetype == CohortArchetypeV1::LowActivityPreAuditTail {
        ranked.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
    } else {
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    }
    ranked.into_iter().map(|(pair, _)| pair).collect()
}

fn unavailable_reason(
    archetype: CohortArchetypeV1,
    pre_audit_candidates: &BTreeSet<String>,
    tail_qualified: &BTreeSet<String>,
    metadata: &BTreeMap<String, &CohortMarketMetadataV1>,
) -> CohortUnavailableReasonV1 {
    if pre_audit_candidates.is_empty() {
        return CohortUnavailableReasonV1::NoVisiblePreAuditMarket;
    }
    match archetype {
        CohortArchetypeV1::SpotActivity => CohortUnavailableReasonV1::NoPrefixSpotActivity,
        CohortArchetypeV1::LendingToken0 => CohortUnavailableReasonV1::NoPrefixToken0Lending,
        CohortArchetypeV1::LendingToken1 => CohortUnavailableReasonV1::NoPrefixToken1Lending,
        CohortArchetypeV1::Liquidation => CohortUnavailableReasonV1::NoPrefixLiquidation,
        CohortArchetypeV1::LowActivityPreAuditTail => {
            let any_metadata_missing = pre_audit_candidates
                .iter()
                .any(|pair| !metadata.contains_key(pair));
            let any_tail_unavailable = pre_audit_candidates.iter().any(|pair| {
                metadata.get(pair).is_some_and(|row| {
                    matches!(
                        &row.tail_qualification_evidence,
                        CohortTailQualificationEvidenceV1::Unavailable { .. }
                    )
                })
            });
            if any_metadata_missing {
                CohortUnavailableReasonV1::MissingMintBinding
            } else if any_tail_unavailable && tail_qualified.is_empty() {
                CohortUnavailableReasonV1::TailQualificationEvidenceUnavailable
            } else {
                CohortUnavailableReasonV1::NoPrefixBaseQualifiedTailMarket
            }
        }
        CohortArchetypeV1::Token2022OrConfigurationEdge => {
            let any_metadata_missing = pre_audit_candidates
                .iter()
                .any(|pair| !metadata.contains_key(pair));
            let any_edge_unavailable = pre_audit_candidates.iter().any(|pair| {
                metadata.get(pair).is_some_and(|row| {
                    matches!(&row.edge_evidence, CohortEdgeEvidenceV1::Unavailable { .. })
                })
            });
            if any_metadata_missing {
                CohortUnavailableReasonV1::MissingMintBinding
            } else if any_edge_unavailable {
                CohortUnavailableReasonV1::EdgeEvidenceUnavailable
            } else {
                CohortUnavailableReasonV1::NoToken2022OrConfigurationEdge
            }
        }
    }
}

fn select_for_archetype(
    _archetype: CohortArchetypeV1,
    ranked: Vec<String>,
    no_candidate_reason: CohortUnavailableReasonV1,
    target_count: usize,
    selected: &mut BTreeSet<String>,
    metadata: &BTreeMap<String, &CohortMarketMetadataV1>,
) -> CohortArchetypeStatusV1 {
    if selected.len() >= target_count {
        return CohortArchetypeStatusV1::Unavailable {
            reason: CohortUnavailableReasonV1::TargetCountReached,
        };
    }
    if ranked.is_empty() {
        return CohortArchetypeStatusV1::Unavailable {
            reason: no_candidate_reason,
        };
    }

    let mut skipped_selected = false;
    let mut skipped_metadata = false;
    for (rank, pair) in ranked.iter().enumerate() {
        if selected.contains(pair) {
            skipped_selected = true;
            continue;
        }
        if !metadata.contains_key(pair) {
            skipped_metadata = true;
            continue;
        }
        selected.insert(pair.clone());
        let fallback_reason = if rank == 0 {
            None
        } else if skipped_selected && skipped_metadata {
            Some(CohortFallbackReasonV1::HigherRankedCandidateUnavailable)
        } else if skipped_metadata {
            Some(CohortFallbackReasonV1::HigherRankedCandidateMissingMintBinding)
        } else {
            Some(CohortFallbackReasonV1::HigherRankedCandidateAlreadySelected)
        };
        return CohortArchetypeStatusV1::Selected {
            pair_address: pair.clone(),
            rank: u32::try_from(rank).unwrap_or(u32::MAX),
            fallback_reason,
        };
    }

    CohortArchetypeStatusV1::Unavailable {
        reason: if skipped_metadata && !skipped_selected {
            CohortUnavailableReasonV1::MissingMintBinding
        } else {
            CohortUnavailableReasonV1::NoUniqueCandidate
        },
    }
}

fn hash_serialized<T: Serialize>(value: &T) -> Result<String, CohortError> {
    let bytes = serde_json::to_vec(value).map_err(|_| CohortError::Serialization)?;
    let mut digest = Sha256::new();
    digest.update(ARTIFACT_HASH_DOMAIN);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(hex::encode(digest.finalize()))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn ensure_sorted_unique_nonempty(name: &str, values: &[String]) -> Result<(), CohortError> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(CohortError::InvalidRequest(format!(
            "{name} contains an empty value"
        )));
    }
    ensure_sorted_unique(name, values)
}

fn ensure_sorted_unique<T: Ord>(name: &str, values: &[T]) -> Result<(), CohortError> {
    if values.windows(2).any(|window| window[0] >= window[1]) {
        return Err(CohortError::InvalidRequest(format!(
            "{name} is not strictly sorted and unique"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::canonical::TokenAmount;

    fn hash(byte: char) -> String {
        byte.to_string().repeat(64)
    }

    fn scope(mode: ScopeMode) -> ScopeConfig {
        ScopeConfig {
            mode,
            evidence_tier: EvidenceTier::DecisionGrade,
            target_count: 6,
            minimum_panel_markets: 5,
            selection_training_bps: 6_000,
            keep_global_signature_inventory: true,
            per_market_opportunity_ceiling: mode == ScopeMode::FullUniverse,
            explicit_market_overrides: Vec::new(),
        }
    }

    fn sources() -> CohortSourceBindingsV1 {
        CohortSourceBindingsV1 {
            config_sha256: hash('a'),
            sealed_prefix_sha256: hash('b'),
            market_evidence_sha256: hash('c'),
            db_event_seeds_sha256: hash('d'),
            market_metadata_sha256: hash('e'),
            global_signature_inventory_sha256: Some(hash('f')),
        }
    }

    fn mint(seed: u8) -> String {
        Pubkey::new_from_array([seed; 32]).to_string()
    }

    fn evidence(pair: &str) -> MarketEvidence {
        MarketEvidence {
            pair_address: pair.to_owned(),
            visible: true,
            hard_gates_pass: true,
            swap_count: 999_999,
            active_swap_days: 999,
            median_time_weighted_tvl_usd_micros: Some(TokenAmount::new("999999").unwrap()),
        }
    }

    fn metadata(
        pair: &str,
        seed: u8,
        edge: CohortEdgeEvidenceV1,
        prefix_swap_count: u64,
        prefix_active_swap_days: u32,
    ) -> CohortMarketMetadataV1 {
        CohortMarketMetadataV1 {
            pair_address: pair.to_owned(),
            token0_mint: mint(seed),
            token1_mint: mint(seed + 32),
            lp_mint: mint(seed + 64),
            edge_evidence: edge,
            tail_qualification_evidence: CohortTailQualificationEvidenceV1::Exact {
                prefix_swap_count,
                prefix_active_swap_days,
            },
        }
    }

    fn event(
        table: &str,
        id: u64,
        pair: &str,
        slot: u64,
        payload: serde_json::Value,
    ) -> DbEventSeed {
        DbEventSeed {
            source_table: table.to_owned(),
            source_id: id,
            event_kind: table.to_owned(),
            pair: pair.to_owned(),
            actor: None,
            signature: format!("signature-{table}-{id}"),
            slot,
            instruction_index: None,
            instruction_path: None,
            payload,
        }
    }

    fn fixture() -> (
        Vec<MarketEvidence>,
        Vec<CohortMarketMetadataV1>,
        Vec<DbEventSeed>,
    ) {
        let pairs = [
            "market-a", "market-b", "market-c", "market-d", "market-e", "market-f",
        ];
        let markets = pairs.iter().map(|pair| evidence(pair)).collect();
        let metadata = pairs
            .iter()
            .enumerate()
            .map(|(index, pair)| {
                metadata(
                    pair,
                    u8::try_from(index + 1).unwrap(),
                    if *pair == "market-f" {
                        CohortEdgeEvidenceV1::Exact {
                            token_2022_mint_present: true,
                            configuration_edge_present: false,
                        }
                    } else {
                        CohortEdgeEvidenceV1::Exact {
                            token_2022_mint_present: false,
                            configuration_edge_present: false,
                        }
                    },
                    match *pair {
                        "market-a" => 20,
                        "market-e" => 10,
                        _ => 0,
                    },
                    if matches!(*pair, "market-a" | "market-e") {
                        3
                    } else {
                        0
                    },
                )
            })
            .collect();

        let mut events = Vec::new();
        let mut id = 1_u64;
        for _ in 0..20 {
            events.push(event("swaps", id, "market-a", 110 + id, json!({})));
            id += 1;
        }
        for _ in 0..4 {
            events.push(event(
                "adjust_debt_events",
                id,
                "market-b",
                120 + id,
                json!({"amount0":"1","amount1":"0"}),
            ));
            id += 1;
        }
        for _ in 0..4 {
            events.push(event(
                "adjust_debt_events",
                id,
                "market-c",
                130 + id,
                json!({"amount0":"0","amount1":"1"}),
            ));
            id += 1;
        }
        for _ in 0..3 {
            events.push(event(
                "user_position_liquidated_events",
                id,
                "market-d",
                140 + id,
                json!({}),
            ));
            id += 1;
        }
        for offset in 0..10 {
            events.push(event("swaps", id, "market-e", 175 + offset, json!({})));
            id += 1;
        }
        (markets, metadata, events)
    }

    fn build(
        mode: ScopeMode,
        markets: &[MarketEvidence],
        metadata: &[CohortMarketMetadataV1],
        events: &[DbEventSeed],
    ) -> Result<CohortSelectionArtifactV1, CohortError> {
        let scope = scope(mode);
        let sources = sources();
        select_cohort(CohortSelectionRequestV1 {
            run_id: "fixture-run",
            cutoff_slot: 300,
            selection_start_slot: 100,
            selection_end_slot_exclusive: 200,
            minimum_prefix_swaps_for_tail: 10,
            minimum_prefix_active_days_for_tail: 3,
            scope: &scope,
            source_bindings: &sources,
            market_evidence: markets,
            db_event_seeds: events,
            market_metadata: metadata,
        })
    }

    #[test]
    fn hybrid_panel_selects_six_unique_archetypes_deterministically() {
        let (markets, metadata, events) = fixture();
        let first = build(ScopeMode::HybridPanel, &markets, &metadata, &events).unwrap();
        let mut reversed_markets = markets.clone();
        reversed_markets.reverse();
        let mut reversed_metadata = metadata.clone();
        reversed_metadata.reverse();
        let mut reversed_events = events.clone();
        reversed_events.reverse();
        let second = build(
            ScopeMode::HybridPanel,
            &reversed_markets,
            &reversed_metadata,
            &reversed_events,
        )
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.selected_markets.len(), 6);
        assert!(first.minimum_pre_audit_panel_met);
        assert!(first.blocking_reasons.is_empty());
        assert_eq!(first.archetype_outcomes.len(), 6);
        for outcome in &first.archetype_outcomes {
            assert!(matches!(
                outcome.outcome,
                CohortArchetypeStatusV1::Selected { .. }
            ));
        }
        first.extraction_targets().unwrap();
    }

    #[test]
    fn hybrid_panel_retains_the_separately_labeled_opportunity_ceiling() {
        let (markets, metadata, events) = fixture();
        let mut scope = scope(ScopeMode::HybridPanel);
        scope.per_market_opportunity_ceiling = true;
        let sources = sources();
        let artifact = select_cohort(CohortSelectionRequestV1 {
            run_id: "fixture-run",
            cutoff_slot: 300,
            selection_start_slot: 100,
            selection_end_slot_exclusive: 200,
            minimum_prefix_swaps_for_tail: 10,
            minimum_prefix_active_days_for_tail: 3,
            scope: &scope,
            source_bindings: &sources,
            market_evidence: &markets,
            db_event_seeds: &events,
            market_metadata: &metadata,
        })
        .unwrap();

        assert!(artifact.per_market_opportunity_ceiling);
        artifact.verify_self_hash().unwrap();
    }

    #[test]
    fn full_window_counts_usd_and_final_audit_flags_do_not_affect_selection() {
        let (markets, metadata, events) = fixture();
        let first = build(ScopeMode::HybridPanel, &markets, &metadata, &events).unwrap();
        let mut changed = markets;
        for (index, market) in changed.iter_mut().enumerate() {
            market.swap_count = index as u64;
            market.active_swap_days = index as u32;
            market.median_time_weighted_tvl_usd_micros = None;
            market.hard_gates_pass = index % 2 == 0;
        }
        let second = build(ScopeMode::HybridPanel, &changed, &metadata, &events).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn final_audit_helper_filters_without_rewriting_selection() {
        let (mut markets, metadata, events) = fixture();
        let artifact = build(ScopeMode::HybridPanel, &markets, &metadata, &events).unwrap();
        markets[0].hard_gates_pass = false;
        markets[1].hard_gates_pass = false;
        let qualification = artifact.qualify_after_final_audit(&markets).unwrap();
        assert!(!qualification.minimum_final_panel_met);
        assert_eq!(qualification.accepted_markets.len(), 4);
        assert_eq!(
            qualification.rejected_markets,
            vec![
                CohortFinalAuditRejectionV1 {
                    pair_address: "market-a".to_owned(),
                    reason: CohortFinalAuditRejectionReasonV1::HardGatesFailed,
                },
                CohortFinalAuditRejectionV1 {
                    pair_address: "market-b".to_owned(),
                    reason: CohortFinalAuditRejectionReasonV1::HardGatesFailed,
                },
            ]
        );
        assert_eq!(artifact.selected_markets.len(), 6);
    }

    #[test]
    fn validation_or_holdout_event_is_rejected_not_ignored() {
        let (markets, metadata, mut events) = fixture();
        events.push(event("swaps", 10_000, "market-f", 200, json!({})));
        assert!(matches!(
            build(ScopeMode::HybridPanel, &markets, &metadata, &events),
            Err(CohortError::EventOutsideSelectionPrefix(_))
        ));
    }

    #[test]
    fn repayments_do_not_count_as_observed_borrows() {
        let (markets, metadata, mut events) = fixture();
        for offset in 0..20_u64 {
            events.push(event(
                "adjust_debt_events",
                50_000 + offset,
                "market-e",
                175 + (offset % 10),
                json!({"amount0":"-999","amount1":"0"}),
            ));
        }
        let artifact = build(ScopeMode::HybridPanel, &markets, &metadata, &events).unwrap();
        let token0 = artifact
            .archetype_outcomes
            .iter()
            .find(|row| row.archetype == CohortArchetypeV1::LendingToken0)
            .unwrap();
        assert!(matches!(
            &token0.outcome,
            CohortArchetypeStatusV1::Selected { pair_address, .. }
                if pair_address == "market-b"
        ));
    }

    #[test]
    fn equal_scores_tie_break_by_pair_address() {
        let (markets, mut metadata, mut events) = fixture();
        metadata
            .iter_mut()
            .find(|row| row.pair_address == "market-f")
            .unwrap()
            .tail_qualification_evidence = CohortTailQualificationEvidenceV1::Exact {
            prefix_swap_count: 20,
            prefix_active_swap_days: 3,
        };
        for offset in 0..20_u64 {
            events.push(event(
                "swaps",
                60_000 + offset,
                "market-f",
                175 + (offset % 10),
                json!({}),
            ));
        }
        let artifact = build(ScopeMode::HybridPanel, &markets, &metadata, &events).unwrap();
        let spot = artifact
            .archetype_outcomes
            .iter()
            .find(|row| row.archetype == CohortArchetypeV1::SpotActivity)
            .unwrap();
        assert!(matches!(
            &spot.outcome,
            CohortArchetypeStatusV1::Selected { pair_address, .. }
                if pair_address == "market-a"
        ));
    }

    #[test]
    fn unavailable_active_day_proof_is_typed_not_called_tail_qualified() {
        let (markets, mut metadata, events) = fixture();
        for row in &mut metadata {
            row.tail_qualification_evidence = CohortTailQualificationEvidenceV1::Unavailable {
                reason:
                    CohortTailQualificationUnavailableReasonV1::AuthoritativeBlockTimeUnavailable,
            };
        }
        let artifact = build(ScopeMode::HybridPanel, &markets, &metadata, &events).unwrap();
        let tail = artifact
            .archetype_outcomes
            .iter()
            .find(|row| row.archetype == CohortArchetypeV1::LowActivityPreAuditTail)
            .unwrap();
        assert_eq!(
            tail.outcome,
            CohortArchetypeStatusV1::Unavailable {
                reason: CohortUnavailableReasonV1::TailQualificationEvidenceUnavailable,
            }
        );
    }

    #[test]
    fn absent_edge_proof_is_typed_and_can_leave_a_five_market_panel() {
        let (markets, mut metadata, events) = fixture();
        metadata.last_mut().unwrap().edge_evidence = CohortEdgeEvidenceV1::Unavailable {
            reason: CohortEdgeUnavailableReasonV1::MintHistoryUnavailable,
        };
        let artifact = build(ScopeMode::HybridPanel, &markets, &metadata, &events).unwrap();
        assert_eq!(artifact.selected_markets.len(), 5);
        assert!(artifact.minimum_pre_audit_panel_met);
        assert_eq!(
            artifact.archetype_outcomes.last().unwrap().outcome,
            CohortArchetypeStatusV1::Unavailable {
                reason: CohortUnavailableReasonV1::EdgeEvidenceUnavailable,
            }
        );
    }

    #[test]
    fn full_universe_mode_selects_every_visible_market() {
        let (mut markets, metadata, events) = fixture();
        markets[5].visible = false;
        let artifact = build(ScopeMode::FullUniverse, &markets, &metadata, &events).unwrap();
        assert_eq!(artifact.selected_markets.len(), 5);
        assert!(!artifact.selected_markets.contains(&"market-f".to_owned()));
        assert!(artifact.archetype_outcomes.is_empty());
        assert!(artifact.per_market_opportunity_ceiling);
    }

    #[test]
    fn self_hash_detects_target_tampering() {
        let (markets, metadata, events) = fixture();
        let mut artifact = build(ScopeMode::HybridPanel, &markets, &metadata, &events).unwrap();
        artifact.selection_prefix_slots.push(199);
        assert!(artifact.verify_self_hash().is_err());
    }
}

//! Deterministic source reconciliation, hard quality gates, and market eligibility.

use crate::canonical::TokenAmount;
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::BTreeMap};

pub const MIN_ELIGIBLE_SWAPS: u64 = 10;
pub const MIN_ELIGIBLE_ACTIVE_DAYS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketEligibilityPolicy {
    pub minimum_swaps: u64,
    pub minimum_active_days: u32,
    pub tvl_percentile_bps: u16,
}

impl Default for MarketEligibilityPolicy {
    fn default() -> Self {
        Self {
            minimum_swaps: MIN_ELIGIBLE_SWAPS,
            minimum_active_days: MIN_ELIGIBLE_ACTIVE_DAYS,
            tvl_percentile_bps: 1_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationClassification {
    Exact,
    /// The indexer intentionally omitted or collapsed a row; decoded RPC is truth.
    DbMissingRpcCanonical,
    /// A DB instruction path was missing but finalized RPC recovered exact order.
    OrderingRecoveredFromRpc,
    /// The DB uniqueness constraint collapsed repeated same-type events in one tx.
    DbCollapsedSameTransaction,
    /// An additional DB row is field-identical to an already matched RPC event.
    /// RPC remains the canonical event inventory, so this is recorded but does
    /// not block acceptance.
    DbDuplicate,
    FieldMismatch,
    RpcMissingCritical,
    UnsupportedStateChangingInstruction,
    DecoderVersionMissing,
}

impl ReconciliationClassification {
    pub fn blocks_acceptance(self) -> bool {
        matches!(
            self,
            Self::FieldMismatch
                | Self::RpcMissingCritical
                | Self::UnsupportedStateChangingInstruction
                | Self::DecoderVersionMissing
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationRecord {
    pub signature: String,
    pub db_event_ref: Option<String>,
    pub rpc_event_id: Option<String>,
    pub classification: ReconciliationClassification,
    pub differing_fields: Vec<String>,
    /// A categorical code only; never store driver errors, URLs, or credentials.
    pub detail_code: Option<String>,
}

pub fn reconcile_fields(
    signature: impl Into<String>,
    db_event_ref: Option<String>,
    rpc_event_id: Option<String>,
    db_fields: Option<&BTreeMap<String, String>>,
    rpc_fields: Option<&BTreeMap<String, String>>,
) -> ReconciliationRecord {
    let signature = signature.into();
    match (db_fields, rpc_fields) {
        (None, Some(_)) => ReconciliationRecord {
            signature,
            db_event_ref,
            rpc_event_id,
            classification: ReconciliationClassification::DbMissingRpcCanonical,
            differing_fields: Vec::new(),
            detail_code: Some("db_row_absent".to_owned()),
        },
        (Some(_), None) | (None, None) => ReconciliationRecord {
            signature,
            db_event_ref,
            rpc_event_id,
            classification: ReconciliationClassification::RpcMissingCritical,
            differing_fields: Vec::new(),
            detail_code: Some("rpc_event_absent".to_owned()),
        },
        (Some(db), Some(rpc)) => {
            let differing_fields: Vec<String> = db
                .keys()
                .chain(rpc.keys())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .filter(|field| db.get(*field) != rpc.get(*field))
                .cloned()
                .collect();
            ReconciliationRecord {
                signature,
                db_event_ref,
                rpc_event_id,
                classification: if differing_fields.is_empty() {
                    ReconciliationClassification::Exact
                } else {
                    ReconciliationClassification::FieldMismatch
                },
                differing_fields,
                detail_code: None,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageCount {
    pub observed: u64,
    pub expected: u64,
}

impl CoverageCount {
    pub fn complete(self) -> bool {
        self.observed == self.expected
    }

    pub fn basis_points(self) -> Option<u16> {
        if self.expected == 0 {
            return None;
        }
        let bps = (u128::from(self.observed).saturating_mul(10_000)) / u128::from(self.expected);
        Some(bps.min(10_000) as u16)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardGateInput {
    pub transaction_inventory: CoverageCount,
    pub state_changing_instruction_decode: CoverageCount,
    /// Blocking decode/provenance failures that do not have instruction-grain
    /// denominators (for example an unreadable transaction inventory).
    pub state_changing_decode_blockers: u64,
    pub canonical_event_order: CoverageCount,
    pub authoritative_block_time: CoverageCount,
    pub versioned_config_coverage: CoverageCount,
    /// Mint-owner and fee schedule histories proven from complete finalized
    /// address crawls, at mint grain.
    pub historical_transfer_fee_coverage: CoverageCount,
    pub transfer_fee_history_blockers: u64,
    pub db_rpc_raw_field_agreement: CoverageCount,
    pub terminal_state_fields: CoverageCount,
    pub economic_replay_events: CoverageCount,
    pub unresolved_rpc_slots: u64,
    pub reconciliations: Vec<ReconciliationRecord>,
    /// `None` means USD results are not requested. When requested, all included
    /// revenue-bearing events require a historical (not current/estimated) price.
    pub historical_price_coverage: Option<CoverageCount>,
}

/// Phase-aware input extends the existing extraction contract without making
/// legacy audit callers fabricate decoder-revision certification evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardGatePhaseInputV1 {
    pub extraction: HardGateInput,
    /// Historical deployed program revisions whose binaries/IDLs are proven and
    /// bound to exactly one active decoder implementation.
    pub decoder_revision_certification: CoverageCount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardGateId {
    TransactionInventory,
    StateChangingDecode,
    DecoderRevisionCertification,
    CanonicalOrder,
    AuthoritativeBlockTime,
    VersionedConfig,
    HistoricalTransferFees,
    SourceAgreement,
    TerminalState,
    EconomicReplay,
    HistoricalPrices,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Pass,
    Fail,
    /// The evidence is produced by a later stage and has not been evaluated.
    Pending,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateResult {
    pub gate: HardGateId,
    pub status: GateStatus,
    pub observed: Option<u64>,
    pub expected: Option<u64>,
    pub blocking_records: u64,
    pub detail_code: String,
}

impl GateResult {
    fn coverage(gate: HardGateId, coverage: CoverageCount, detail_code: &str) -> Self {
        Self {
            gate,
            status: if coverage.complete() {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            observed: Some(coverage.observed),
            expected: Some(coverage.expected),
            blocking_records: coverage.expected.saturating_sub(coverage.observed),
            detail_code: detail_code.to_owned(),
        }
    }

    fn pending(gate: HardGateId, coverage: CoverageCount, detail_code: &str) -> Self {
        Self {
            gate,
            status: GateStatus::Pending,
            observed: Some(coverage.observed),
            expected: Some(coverage.expected),
            blocking_records: 0,
            detail_code: detail_code.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardGateReport {
    /// Acceptance of the unscoped prerequisites for the phase in which this
    /// report was produced. Historical prices and transfer-fee histories are
    /// deliberately excluded because they gate USD evidence and affected
    /// markets respectively.
    pub accepted: bool,
    pub gates: Vec<GateResult>,
}

pub const HARD_GATE_PHASE_REPORT_SCHEMA_VERSION: u16 = 1;

/// The stage whose evidence is allowed to participate in a hard-gate decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardGatePhase {
    /// Extraction and source audit have completed. Terminal reconciliation and
    /// native economic replay are future evidence and must remain pending.
    ExtractionAudit,
    /// Native factual replay and exact-cutoff terminal reconciliation have run.
    PostReplayFinal,
}

/// A phase-bound report prevents a caller from treating an audit-stage pending
/// result as final certification evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardGatePhaseReportV1 {
    pub schema_version: u16,
    pub phase: HardGatePhase,
    pub report: HardGateReport,
}

/// The claim strength requested by a consumer of the benchmark evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTier {
    /// Exact selected-scope replay may be used for an internal decision while
    /// explicitly retaining decoder-revision/terminal certification gaps as
    /// limitations. State-changing operation decode coverage remains exact.
    DecisionGrade,
    /// Every applicable final hard gate must pass.
    AuditGrade,
}

/// Market-specific requirements which cannot be inferred from a global gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseGatePolicyV1 {
    pub evidence_tier: EvidenceTier,
    /// True only for a market whose mint/program history requires this evidence.
    pub historical_transfer_fees_required: bool,
    /// True only when this market is requested in USD headline evidence.
    pub historical_prices_required: bool,
    /// An explicit caller assertion that the selected scope has exact DB/RPC
    /// agreement and native economic replay. It never changes an underlying
    /// gate status; it only permits the two named DecisionGrade limitations.
    pub exact_selected_scope_replay_proven: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificationStatus {
    Pending,
    Pass,
    Limited,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateLimitationV1 {
    pub gate: HardGateId,
    pub underlying_status: GateStatus,
    pub detail_code: String,
}

/// Phase- and scope-correct progression decision. The original gate results are
/// retained in `HardGatePhaseReportV1`; permitted limitations are additive and
/// never rewrite a failure into a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseGateDecisionV1 {
    pub phase: HardGatePhase,
    pub evidence_tier: EvidenceTier,
    pub phase_eligible: bool,
    pub raw_market_eligible: bool,
    pub usd_headline_eligible: bool,
    pub raw_certification: CertificationStatus,
    pub usd_certification: CertificationStatus,
    pub raw_blocking_gates: Vec<HardGateId>,
    pub usd_blocking_gates: Vec<HardGateId>,
    pub limitations: Vec<GateLimitationV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PhaseGateError {
    #[error("unsupported hard-gate phase report schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("hard-gate phase report contains duplicate gate {0:?}")]
    DuplicateGate(HardGateId),
    #[error("hard-gate phase report is missing gate {0:?}")]
    MissingGate(HardGateId),
}

pub fn evaluate_hard_gates(input: &HardGateInput) -> HardGateReport {
    evaluate_hard_gates_for_phase(input, HardGatePhase::ExtractionAudit).report
}

pub fn evaluate_hard_gates_for_phase(
    input: &HardGateInput,
    phase: HardGatePhase,
) -> HardGatePhaseReportV1 {
    evaluate_hard_gates_for_phase_with_certification(
        &HardGatePhaseInputV1 {
            extraction: input.clone(),
            // The legacy audit API has no revision proof. Preserve that fact as
            // pending during audit and fail closed if used as final evidence.
            decoder_revision_certification: CoverageCount {
                observed: 0,
                expected: 1,
            },
        },
        phase,
    )
}

pub fn evaluate_hard_gates_for_phase_with_certification(
    phase_input: &HardGatePhaseInputV1,
    phase: HardGatePhase,
) -> HardGatePhaseReportV1 {
    let input = &phase_input.extraction;
    let mut inventory = GateResult::coverage(
        HardGateId::TransactionInventory,
        input.transaction_inventory,
        "all_successful_state_changing_transactions_fetched",
    );
    if input.unresolved_rpc_slots > 0 {
        inventory.status = GateStatus::Fail;
        inventory.blocking_records = inventory
            .blocking_records
            .saturating_add(input.unresolved_rpc_slots);
        inventory.detail_code = "unresolved_archival_rpc_slots".to_owned();
    }

    let mut state_changing_decode = GateResult::coverage(
        HardGateId::StateChangingDecode,
        input.state_changing_instruction_decode,
        "all_state_changing_instructions_version_decoded",
    );
    if input.state_changing_decode_blockers > 0 {
        state_changing_decode.status = GateStatus::Fail;
        state_changing_decode.blocking_records = state_changing_decode
            .blocking_records
            .saturating_add(input.state_changing_decode_blockers);
        state_changing_decode.detail_code = "decode_or_cpi_provenance_blockers_present".to_owned();
    }

    let decoder_revision_certification = match phase {
        HardGatePhase::ExtractionAudit
            if !phase_input.decoder_revision_certification.complete() =>
        {
            GateResult::pending(
                HardGateId::DecoderRevisionCertification,
                phase_input.decoder_revision_certification,
                "awaiting_historical_decoder_revision_certification",
            )
        }
        _ => GateResult::coverage(
            HardGateId::DecoderRevisionCertification,
            phase_input.decoder_revision_certification,
            "every_historical_program_revision_has_one_verified_decoder",
        ),
    };

    let blocking_reconciliations = input
        .reconciliations
        .iter()
        .filter(|record| record.classification.blocks_acceptance())
        .count() as u64;
    let mut transfer_fees = GateResult::coverage(
        HardGateId::HistoricalTransferFees,
        input.historical_transfer_fee_coverage,
        "every_market_mint_has_complete_owner_and_transfer_fee_history",
    );
    if input.transfer_fee_history_blockers > 0 {
        transfer_fees.status = GateStatus::Fail;
        transfer_fees.blocking_records = transfer_fees
            .blocking_records
            .max(input.transfer_fee_history_blockers);
        transfer_fees.detail_code = "mint_history_gaps_or_unsupported_hooks".to_owned();
    }
    let mut source_agreement = GateResult::coverage(
        HardGateId::SourceAgreement,
        input.db_rpc_raw_field_agreement,
        "db_rpc_raw_fields_exact_or_documented_db_omission",
    );
    if blocking_reconciliations > 0 {
        source_agreement.status = GateStatus::Fail;
        source_agreement.blocking_records = source_agreement
            .blocking_records
            .saturating_add(blocking_reconciliations);
        source_agreement.detail_code = "blocking_db_rpc_field_mismatches".to_owned();
    }

    let price_gate = input.historical_price_coverage.map_or(
        GateResult {
            gate: HardGateId::HistoricalPrices,
            status: GateStatus::NotApplicable,
            observed: None,
            expected: None,
            blocking_records: 0,
            detail_code: "usd_headline_metrics_not_requested".to_owned(),
        },
        |coverage| {
            GateResult::coverage(
                HardGateId::HistoricalPrices,
                coverage,
                "all_revenue_events_have_historical_prices",
            )
        },
    );

    let terminal_state = match phase {
        HardGatePhase::ExtractionAudit => GateResult::pending(
            HardGateId::TerminalState,
            input.terminal_state_fields,
            "awaiting_post_replay_terminal_reconciliation",
        ),
        HardGatePhase::PostReplayFinal => GateResult::coverage(
            HardGateId::TerminalState,
            input.terminal_state_fields,
            "terminal_raw_state_matches_on_chain_exactly",
        ),
    };
    let economic_replay = match phase {
        HardGatePhase::ExtractionAudit => GateResult::pending(
            HardGateId::EconomicReplay,
            input.economic_replay_events,
            "awaiting_native_economic_replay",
        ),
        HardGatePhase::PostReplayFinal => GateResult::coverage(
            HardGateId::EconomicReplay,
            input.economic_replay_events,
            "swaps_interest_and_lp_supply_replay_exactly",
        ),
    };

    let gates = vec![
        inventory,
        state_changing_decode,
        decoder_revision_certification,
        GateResult::coverage(
            HardGateId::CanonicalOrder,
            input.canonical_event_order,
            "all_events_have_block_transaction_instruction_order",
        ),
        GateResult::coverage(
            HardGateId::AuthoritativeBlockTime,
            input.authoritative_block_time,
            "all_events_use_rpc_block_time",
        ),
        GateResult::coverage(
            HardGateId::VersionedConfig,
            input.versioned_config_coverage,
            "every_event_has_exactly_one_active_config_version",
        ),
        transfer_fees,
        source_agreement,
        terminal_state,
        economic_replay,
        price_gate,
    ];
    let accepted = gates.iter().all(|gate| {
        matches!(
            gate.gate,
            HardGateId::HistoricalTransferFees | HardGateId::HistoricalPrices
        ) || gate.status != GateStatus::Fail
    });
    HardGatePhaseReportV1 {
        schema_version: HARD_GATE_PHASE_REPORT_SCHEMA_VERSION,
        phase,
        report: HardGateReport { accepted, gates },
    }
}

const ALL_HARD_GATES: [HardGateId; 11] = [
    HardGateId::TransactionInventory,
    HardGateId::StateChangingDecode,
    HardGateId::DecoderRevisionCertification,
    HardGateId::CanonicalOrder,
    HardGateId::AuthoritativeBlockTime,
    HardGateId::VersionedConfig,
    HardGateId::HistoricalTransferFees,
    HardGateId::SourceAgreement,
    HardGateId::TerminalState,
    HardGateId::EconomicReplay,
    HardGateId::HistoricalPrices,
];

/// Applies a claim tier and market scope without weakening the recorded gates.
/// Audit-stage terminal/replay evidence is excluded from extraction eligibility;
/// final-stage economic replay is mandatory for both evidence tiers.
pub fn assess_phase_gates(
    phase_report: &HardGatePhaseReportV1,
    policy: PhaseGatePolicyV1,
) -> Result<PhaseGateDecisionV1, PhaseGateError> {
    if phase_report.schema_version != HARD_GATE_PHASE_REPORT_SCHEMA_VERSION {
        return Err(PhaseGateError::UnsupportedSchemaVersion(
            phase_report.schema_version,
        ));
    }

    let mut by_gate = BTreeMap::new();
    for gate in &phase_report.report.gates {
        if by_gate.insert(gate.gate, gate).is_some() {
            return Err(PhaseGateError::DuplicateGate(gate.gate));
        }
    }
    for gate in ALL_HARD_GATES {
        if !by_gate.contains_key(&gate) {
            return Err(PhaseGateError::MissingGate(gate));
        }
    }

    let mut raw_blocking_gates = Vec::new();
    let mut limitations = Vec::new();
    for gate_id in ALL_HARD_GATES {
        if gate_id == HardGateId::HistoricalPrices
            || (gate_id == HardGateId::HistoricalTransferFees
                && !policy.historical_transfer_fees_required)
            || (phase_report.phase == HardGatePhase::ExtractionAudit
                && matches!(
                    gate_id,
                    HardGateId::DecoderRevisionCertification
                        | HardGateId::TerminalState
                        | HardGateId::EconomicReplay
                ))
        {
            continue;
        }

        let gate = by_gate[&gate_id];
        if gate.status == GateStatus::Pass {
            continue;
        }
        let permitted_decision_grade_limitation = phase_report.phase
            == HardGatePhase::PostReplayFinal
            && policy.evidence_tier == EvidenceTier::DecisionGrade
            && policy.exact_selected_scope_replay_proven
            && matches!(
                gate_id,
                HardGateId::DecoderRevisionCertification | HardGateId::TerminalState
            );
        if permitted_decision_grade_limitation {
            limitations.push(GateLimitationV1 {
                gate: gate_id,
                underlying_status: gate.status,
                detail_code: gate.detail_code.clone(),
            });
        } else {
            raw_blocking_gates.push(gate_id);
        }
    }

    let raw_market_eligible = raw_blocking_gates.is_empty();
    let price = by_gate[&HardGateId::HistoricalPrices];
    let price_blocks_usd = policy.historical_prices_required && price.status != GateStatus::Pass;
    let mut usd_blocking_gates = raw_blocking_gates.clone();
    if price_blocks_usd {
        usd_blocking_gates.push(HardGateId::HistoricalPrices);
    }
    let usd_headline_eligible = raw_market_eligible && !price_blocks_usd;

    let raw_certification = certification_status(
        phase_report.phase,
        raw_market_eligible,
        !limitations.is_empty(),
    );
    let usd_certification = certification_status(
        phase_report.phase,
        usd_headline_eligible,
        !limitations.is_empty(),
    );

    Ok(PhaseGateDecisionV1 {
        phase: phase_report.phase,
        evidence_tier: policy.evidence_tier,
        phase_eligible: raw_market_eligible,
        raw_market_eligible,
        usd_headline_eligible,
        raw_certification,
        usd_certification,
        raw_blocking_gates,
        usd_blocking_gates,
        limitations,
    })
}

fn certification_status(
    phase: HardGatePhase,
    eligible: bool,
    has_limitations: bool,
) -> CertificationStatus {
    if !eligible {
        CertificationStatus::Fail
    } else if phase == HardGatePhase::ExtractionAudit {
        CertificationStatus::Pending
    } else if has_limitations {
        CertificationStatus::Limited
    } else {
        CertificationStatus::Pass
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawStateMismatch {
    pub field: String,
    pub replayed: Option<String>,
    pub on_chain: Option<String>,
}

pub fn compare_terminal_state(
    replayed: &BTreeMap<String, String>,
    on_chain: &BTreeMap<String, String>,
) -> Vec<RawStateMismatch> {
    replayed
        .keys()
        .chain(on_chain.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|field| {
            let replayed_value = replayed.get(field);
            let on_chain_value = on_chain.get(field);
            (replayed_value != on_chain_value).then(|| RawStateMismatch {
                field: field.clone(),
                replayed: replayed_value.cloned(),
                on_chain: on_chain_value.cloned(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketEvidence {
    pub pair_address: String,
    pub visible: bool,
    pub hard_gates_pass: bool,
    pub swap_count: u64,
    /// Counted from authoritative RPC block time, never a DB timestamp.
    pub active_swap_days: u32,
    /// Median of interval-TW TVL marks using prices observed at each historical
    /// interval, in integer USD micro-units. `None` means the USD/TVL marks are
    /// unavailable; a current price must never be substituted.
    pub median_time_weighted_tvl_usd_micros: Option<TokenAmount>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EligibilityReason {
    Eligible,
    NotVisible,
    HardGatesFailed,
    FewerThanTenSwaps,
    FewerThanThreeActiveDays,
    BelowVisibleMarketTvlP10,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsdHeadlineEligibilityReason {
    Eligible,
    RawMetricsIneligible,
    HistoricalTvlUnavailable,
    BelowVisibleMarketTvlP10,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketEligibility {
    pub pair_address: String,
    /// Eligibility for raw-token and invariant metrics.
    pub eligible: bool,
    pub reason: EligibilityReason,
    /// USD headlines additionally require historical TVL marks and the p10 tail
    /// rule. Missing USD evidence never makes raw/invariant metrics ineligible.
    pub usd_headline_eligible: bool,
    pub usd_headline_reason: UsdHeadlineEligibilityReason,
    pub median_time_weighted_tvl_usd_micros: Option<TokenAmount>,
    pub visible_market_p10_tvl_usd_micros: Option<TokenAmount>,
}

fn compare_decimal_amounts(left: &TokenAmount, right: &TokenAmount) -> Ordering {
    left.as_str()
        .len()
        .cmp(&right.as_str().len())
        .then_with(|| left.as_str().cmp(right.as_str()))
}

/// Applies raw-data requirements, then the historical-TVL p10 tail rule.
///
/// The p10 population contains visible, hard-gate-passing markets that meet the
/// ten-swap/three-day minima and have historical USD TVL evidence. With fewer
/// than ten such markets no tail is removed. Otherwise the cutoff is the first
/// value after `floor(n / 10)` sorted values; values strictly below it are omitted
/// from the benchmark and ties are retained. Markets missing historical USD/TVL
/// marks cannot be ranked, so they remain eligible for raw/invariant metrics but
/// not USD headlines.
pub fn visible_market_eligibility(markets: &[MarketEvidence]) -> Vec<MarketEligibility> {
    visible_market_eligibility_with_policy(markets, MarketEligibilityPolicy::default())
}

pub fn visible_market_eligibility_with_policy(
    markets: &[MarketEvidence],
    policy: MarketEligibilityPolicy,
) -> Vec<MarketEligibility> {
    let mut output: Vec<MarketEligibility> = markets
        .iter()
        .map(|market| {
            let reason = if !market.visible {
                EligibilityReason::NotVisible
            } else if !market.hard_gates_pass {
                EligibilityReason::HardGatesFailed
            } else if market.swap_count < policy.minimum_swaps {
                EligibilityReason::FewerThanTenSwaps
            } else if market.active_swap_days < policy.minimum_active_days {
                EligibilityReason::FewerThanThreeActiveDays
            } else {
                EligibilityReason::Eligible
            };
            let eligible = reason == EligibilityReason::Eligible;
            let (usd_headline_eligible, usd_headline_reason) = if !eligible {
                (false, UsdHeadlineEligibilityReason::RawMetricsIneligible)
            } else if market.median_time_weighted_tvl_usd_micros.is_none()
                && policy.tvl_percentile_bps > 0
            {
                (
                    false,
                    UsdHeadlineEligibilityReason::HistoricalTvlUnavailable,
                )
            } else {
                (true, UsdHeadlineEligibilityReason::Eligible)
            };
            MarketEligibility {
                pair_address: market.pair_address.clone(),
                eligible,
                reason,
                usd_headline_eligible,
                usd_headline_reason,
                median_time_weighted_tvl_usd_micros: market
                    .median_time_weighted_tvl_usd_micros
                    .clone(),
                visible_market_p10_tvl_usd_micros: None,
            }
        })
        .collect();

    let mut preliminary_tvl: Vec<TokenAmount> = output
        .iter()
        .filter(|market| market.usd_headline_eligible)
        .filter_map(|market| market.median_time_weighted_tvl_usd_micros.clone())
        .collect();
    preliminary_tvl.sort_by(compare_decimal_amounts);
    let bottom_count = preliminary_tvl
        .len()
        .saturating_mul(usize::from(policy.tvl_percentile_bps))
        / 10_000;
    let cutoff = (bottom_count > 0 && bottom_count < preliminary_tvl.len())
        .then(|| preliminary_tvl[bottom_count].clone());

    if let Some(cutoff) = &cutoff {
        for market in output
            .iter_mut()
            .filter(|market| market.usd_headline_eligible)
        {
            market.visible_market_p10_tvl_usd_micros = Some(cutoff.clone());
            if market
                .median_time_weighted_tvl_usd_micros
                .as_ref()
                .is_some_and(|tvl| compare_decimal_amounts(tvl, cutoff) == Ordering::Less)
            {
                market.eligible = false;
                market.reason = EligibilityReason::BelowVisibleMarketTvlP10;
                market.usd_headline_eligible = false;
                market.usd_headline_reason = UsdHeadlineEligibilityReason::BelowVisibleMarketTvlP10;
            }
        }
    }

    output.sort_by(|left, right| left.pair_address.cmp(&right.pair_address));
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> CoverageCount {
        CoverageCount {
            observed: 2,
            expected: 2,
        }
    }

    fn pending() -> CoverageCount {
        CoverageCount {
            observed: 0,
            expected: 2,
        }
    }

    fn input() -> HardGateInput {
        HardGateInput {
            transaction_inventory: complete(),
            state_changing_instruction_decode: complete(),
            state_changing_decode_blockers: 0,
            canonical_event_order: complete(),
            authoritative_block_time: complete(),
            versioned_config_coverage: complete(),
            historical_transfer_fee_coverage: complete(),
            transfer_fee_history_blockers: 0,
            db_rpc_raw_field_agreement: complete(),
            terminal_state_fields: pending(),
            economic_replay_events: pending(),
            unresolved_rpc_slots: 0,
            reconciliations: Vec::new(),
            historical_price_coverage: Some(complete()),
        }
    }

    fn evaluate(
        input: &HardGateInput,
        decoder_revision_certification: CoverageCount,
        phase: HardGatePhase,
    ) -> HardGatePhaseReportV1 {
        evaluate_hard_gates_for_phase_with_certification(
            &HardGatePhaseInputV1 {
                extraction: input.clone(),
                decoder_revision_certification,
            },
            phase,
        )
    }

    fn evaluate_certified(input: &HardGateInput, phase: HardGatePhase) -> HardGatePhaseReportV1 {
        evaluate(input, complete(), phase)
    }

    fn policy(tier: EvidenceTier) -> PhaseGatePolicyV1 {
        PhaseGatePolicyV1 {
            evidence_tier: tier,
            historical_transfer_fees_required: true,
            historical_prices_required: true,
            exact_selected_scope_replay_proven: false,
        }
    }

    fn gate(report: &HardGatePhaseReportV1, id: HardGateId) -> &GateResult {
        report
            .report
            .gates
            .iter()
            .find(|gate| gate.gate == id)
            .expect("gate exists")
    }

    #[test]
    fn perfect_extraction_keeps_future_evidence_pending_and_market_eligible() {
        // This is the compatibility entry point used by the current audit
        // stage, which does not yet supply revision-certification coverage.
        let phase_report = HardGatePhaseReportV1 {
            schema_version: HARD_GATE_PHASE_REPORT_SCHEMA_VERSION,
            phase: HardGatePhase::ExtractionAudit,
            report: evaluate_hard_gates(&input()),
        };

        assert_eq!(
            gate(&phase_report, HardGateId::DecoderRevisionCertification).status,
            GateStatus::Pending
        );
        assert_eq!(
            gate(&phase_report, HardGateId::TerminalState).status,
            GateStatus::Pending
        );
        assert_eq!(
            gate(&phase_report, HardGateId::EconomicReplay).status,
            GateStatus::Pending
        );
        assert!(phase_report.report.accepted);

        let decision = assess_phase_gates(&phase_report, policy(EvidenceTier::AuditGrade))
            .expect("well-formed report");
        assert!(decision.phase_eligible);
        assert!(decision.raw_market_eligible);
        assert!(decision.usd_headline_eligible);
        assert_eq!(decision.raw_certification, CertificationStatus::Pending);
        assert!(decision.raw_blocking_gates.is_empty());
    }

    #[test]
    fn final_certification_rejects_unevaluated_terminal_and_economic_evidence() {
        let mut phase_report = evaluate_certified(&input(), HardGatePhase::PostReplayFinal);
        // A malformed integration must not turn pending final evidence into a
        // pass merely because it uses the explicit Pending representation.
        for gate in &mut phase_report.report.gates {
            if matches!(
                gate.gate,
                HardGateId::TerminalState | HardGateId::EconomicReplay
            ) {
                gate.status = GateStatus::Pending;
            }
        }

        let decision = assess_phase_gates(&phase_report, policy(EvidenceTier::AuditGrade))
            .expect("well-formed report");
        assert!(!decision.phase_eligible);
        assert_eq!(decision.raw_certification, CertificationStatus::Fail);
        assert_eq!(
            decision.raw_blocking_gates,
            vec![HardGateId::TerminalState, HardGateId::EconomicReplay]
        );
    }

    #[test]
    fn reconcile_upgrades_terminal_and_replay_to_auditable_passes() {
        let mut final_input = input();
        final_input.terminal_state_fields = complete();
        final_input.economic_replay_events = complete();
        let phase_report = evaluate_certified(&final_input, HardGatePhase::PostReplayFinal);
        let decision = assess_phase_gates(&phase_report, policy(EvidenceTier::AuditGrade))
            .expect("well-formed report");

        assert!(decision.phase_eligible);
        assert_eq!(decision.raw_certification, CertificationStatus::Pass);
        assert_eq!(decision.usd_certification, CertificationStatus::Pass);
        assert!(decision.limitations.is_empty());
    }

    #[test]
    fn historical_prices_gate_usd_only() {
        let mut no_prices = input();
        no_prices.historical_price_coverage = Some(pending());
        let phase_report = evaluate_certified(&no_prices, HardGatePhase::ExtractionAudit);
        let decision = assess_phase_gates(&phase_report, policy(EvidenceTier::AuditGrade))
            .expect("well-formed report");

        assert!(decision.raw_market_eligible);
        assert!(!decision.usd_headline_eligible);
        assert!(decision.raw_blocking_gates.is_empty());
        assert_eq!(
            decision.usd_blocking_gates,
            vec![HardGateId::HistoricalPrices]
        );
    }

    #[test]
    fn zero_tvl_percentile_explicitly_disables_continuous_tvl_headline_gate() {
        let markets = [MarketEvidence {
            pair_address: "pair".to_owned(),
            visible: true,
            hard_gates_pass: true,
            swap_count: 10,
            active_swap_days: 3,
            median_time_weighted_tvl_usd_micros: None,
        }];
        let eligibility = visible_market_eligibility_with_policy(
            &markets,
            MarketEligibilityPolicy {
                minimum_swaps: 10,
                minimum_active_days: 3,
                tvl_percentile_bps: 0,
            },
        );
        assert!(eligibility[0].eligible);
        assert!(eligibility[0].usd_headline_eligible);
        assert_eq!(
            eligibility[0].usd_headline_reason,
            UsdHeadlineEligibilityReason::Eligible
        );
        assert!(eligibility[0].median_time_weighted_tvl_usd_micros.is_none());
    }

    #[test]
    fn transfer_fee_history_gates_only_affected_markets() {
        let mut missing_fees = input();
        missing_fees.historical_transfer_fee_coverage = pending();
        let phase_report = evaluate_certified(&missing_fees, HardGatePhase::ExtractionAudit);

        let mut unaffected_policy = policy(EvidenceTier::AuditGrade);
        unaffected_policy.historical_transfer_fees_required = false;
        let unaffected =
            assess_phase_gates(&phase_report, unaffected_policy).expect("well-formed report");
        assert!(unaffected.raw_market_eligible);

        let affected = assess_phase_gates(&phase_report, policy(EvidenceTier::AuditGrade))
            .expect("well-formed report");
        assert!(!affected.raw_market_eligible);
        assert_eq!(
            affected.raw_blocking_gates,
            vec![HardGateId::HistoricalTransferFees]
        );
    }

    #[test]
    fn decision_grade_retains_decoder_and_terminal_failures_as_limitations() {
        let mut selected_scope = input();
        selected_scope.terminal_state_fields = pending();
        selected_scope.economic_replay_events = complete();
        let phase_report = evaluate(&selected_scope, pending(), HardGatePhase::PostReplayFinal);

        let mut decision_policy = policy(EvidenceTier::DecisionGrade);
        decision_policy.exact_selected_scope_replay_proven = true;
        let decision =
            assess_phase_gates(&phase_report, decision_policy).expect("well-formed report");
        assert!(decision.raw_market_eligible);
        assert_eq!(decision.raw_certification, CertificationStatus::Limited);
        assert_eq!(
            decision
                .limitations
                .iter()
                .map(|limitation| limitation.gate)
                .collect::<Vec<_>>(),
            vec![
                HardGateId::DecoderRevisionCertification,
                HardGateId::TerminalState
            ]
        );
        assert!(decision
            .limitations
            .iter()
            .all(|limitation| limitation.underlying_status == GateStatus::Fail));

        let audit = assess_phase_gates(&phase_report, policy(EvidenceTier::AuditGrade))
            .expect("well-formed report");
        assert!(!audit.raw_market_eligible);
        assert_eq!(audit.raw_certification, CertificationStatus::Fail);
    }

    #[test]
    fn decision_grade_never_relaxes_operation_decode_economic_replay_or_unproven_scope() {
        let phase_report = evaluate_certified(&input(), HardGatePhase::PostReplayFinal);
        let mut decision_policy = policy(EvidenceTier::DecisionGrade);
        decision_policy.exact_selected_scope_replay_proven = true;
        let missing_replay =
            assess_phase_gates(&phase_report, decision_policy).expect("well-formed report");
        assert_eq!(missing_replay.raw_certification, CertificationStatus::Fail);
        assert!(missing_replay
            .raw_blocking_gates
            .contains(&HardGateId::EconomicReplay));

        let mut selected_scope = input();
        selected_scope.terminal_state_fields = complete();
        selected_scope.economic_replay_events = complete();
        let final_report = evaluate(&selected_scope, pending(), HardGatePhase::PostReplayFinal);
        let unproven = assess_phase_gates(&final_report, policy(EvidenceTier::DecisionGrade))
            .expect("well-formed report");
        assert!(!unproven.raw_market_eligible);
        assert!(unproven.limitations.is_empty());

        let mut undecoded_operation = input();
        undecoded_operation.state_changing_instruction_decode = pending();
        undecoded_operation.terminal_state_fields = complete();
        undecoded_operation.economic_replay_events = complete();
        let undecoded_report =
            evaluate_certified(&undecoded_operation, HardGatePhase::PostReplayFinal);
        let mut exact_scope_policy = policy(EvidenceTier::DecisionGrade);
        exact_scope_policy.exact_selected_scope_replay_proven = true;
        let undecoded =
            assess_phase_gates(&undecoded_report, exact_scope_policy).expect("well-formed report");
        assert!(!undecoded.raw_market_eligible);
        assert!(undecoded
            .raw_blocking_gates
            .contains(&HardGateId::StateChangingDecode));
    }
}

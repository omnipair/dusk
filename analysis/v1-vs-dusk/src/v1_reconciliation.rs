//! Streaming, compact persistence for the exact factual V1 replay.
//!
//! [`V1FactualReplay`] deliberately returns a full in-memory checkpoint after
//! every operation. Persisting that object verbatim would repeat every open
//! position on every row and make a complete-history run unnecessarily large.
//! This module keeps the exact transition in memory, then persists all pair,
//! rate, authority, custody, and revenue fields plus a deterministic position
//! set commitment and the touched position. The full factual record and its
//! field reconciliations are separately hash-bound on every row.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use omnipair::{
    constants::{DEFAULT_INITIAL_RATE_BPS, FUTARCHY_AUTHORITY_SEED_PREFIX, VERSION},
    state::FutarchyAuthority,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;

use crate::{
    canonical::{ChainOrder, ConfigScope, ConfigVersionV1},
    decode::HISTORICAL_DECODER_DISPATCH_VERIFIED,
    factual_replay::{
        V1BehaviorObservationV1, V1ConservationIdentityV1, V1DecisionGradeCorrectionEvidenceV1,
        V1DecisionGradeRetainedEventLimitationV1, V1FactualOperationRecordV1, V1FactualReplay,
        V1RevenueDeltaV1,
    },
    pubkey_encoding::parse_schema_pubkey,
    replay::{CanonicalOperationV1, OperationPayloadV1},
    v1_engine::{
        V1AuthorityCheckpointV1, V1CanonicalAuthorityInitializationV1,
        V1CanonicalInitializationInputV1, V1CollateralFactorTransitionPolicyV1,
        V1CustodyCheckpointV1, V1NativeCheckpointV1, V1NativeEngine, V1PairCheckpointV1,
        V1PositionCheckpointV1, V1PostStateCorrectionPolicyV1, V1RateModelAddressPolicyV1,
        V1RateModelCheckpointV1, V1RevenueCheckpointV1,
    },
};

pub const V1_RECONCILIATION_SCHEMA_VERSION: u16 = 5;
pub const V1_OBSERVED_TRAVERSAL_SCHEMA_VERSION: u16 = 1;
pub const GLOBAL_CONFIG_ENTITY: &str = "__global__";

/// Explicit decoder-certification policy for native factual replay.
///
/// Decision-grade replay may use a structurally complete, matching revision
/// while its deployment certification remains provisional. The caller must
/// subsequently prove exact selected-scope replay and retain the separate
/// decoder-certification limitation. Audit-grade replay never permits that
/// provisional revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V1ReplayCertificationPolicyV1 {
    AuditGrade,
    DecisionGradeExactSelectedScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CompactCheckpointV1 {
    pub pair: V1PairCheckpointV1,
    pub rate_model: V1RateModelCheckpointV1,
    pub authority: V1AuthorityCheckpointV1,
    pub custody: V1CustodyCheckpointV1,
    pub revenue: V1RevenueCheckpointV1,
    pub open_position_count: u64,
    pub positions_sha256: String,
    pub touched_position: Option<V1PositionCheckpointV1>,
    pub checkpoint_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1ReplayLedgerRowV1 {
    pub schema_version: u16,
    pub market: String,
    pub operation_id: String,
    pub order: ChainOrder,
    pub broadcast_global_config: bool,
    pub compact_checkpoint: V1CompactCheckpointV1,
    pub reconciliation_count: u64,
    pub reconciliations_sha256: String,
    pub all_raw_unit_differences_zero: bool,
    pub conservation: Vec<V1ConservationIdentityV1>,
    pub revenue_delta: V1RevenueDeltaV1,
    pub corrections: Vec<V1DecisionGradeCorrectionEvidenceV1>,
    pub retained_event_limitations: Vec<V1DecisionGradeRetainedEventLimitationV1>,
    pub behavior: Option<V1BehaviorObservationV1>,
    pub factual_record_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1MarketReplaySummaryV1 {
    pub market: String,
    pub initialized_at: ChainOrder,
    pub replayed_transitions: u64,
    pub raw_unit_comparisons: u64,
    pub conservation_checks: u64,
    pub terminal_checkpoint_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1ReconciliationSummaryV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub canonical_operation_count: u64,
    pub replayed_transition_count: u64,
    pub authority_seed_operation_count: u64,
    pub broadcast_global_config_applications: u64,
    pub market_count: u64,
    pub raw_unit_comparison_count: u64,
    pub nonzero_raw_unit_difference_count: u64,
    pub conservation_check_count: u64,
    pub conservation_failure_count: u64,
    pub zero_raw_unit_difference: bool,
    pub all_conservation_identities_hold: bool,
    /// Set only after exact-cutoff account bytes have independently matched
    /// every terminal replay field. A normal current-state RPC response is not
    /// sufficient.
    pub terminal_state_verified: bool,
    pub markets: Vec<V1MarketReplaySummaryV1>,
    pub ledger_sha256: String,
}

/// Full replay result retained in memory for exact-cutoff account
/// reconciliation. `terminal_checkpoints` contains wallet-level positions and
/// must only be persisted inside the gitignored immutable run directory.
pub struct V1ReconciliationRunV1 {
    pub summary: V1ReconciliationSummaryV1,
    pub terminal_checkpoints: BTreeMap<Pubkey, V1NativeCheckpointV1>,
}

/// Private full-state traversal hook. Observers receive the native engine,
/// including every open position and exact custody, immediately before and
/// after each transition. Implementations must retain only bounded in-memory
/// forks and report-safe hashes.
pub trait V1HistoricalStateObserverV1 {
    fn observe_pre_operation(
        &mut self,
        operation: &CanonicalOperationV1,
        market: &str,
        engine: Option<&V1NativeEngine>,
        broadcast_global_config: bool,
    ) -> Result<()>;

    fn observe_post_operation(
        &mut self,
        operation: &CanonicalOperationV1,
        market: &str,
        engine: &V1NativeEngine,
        broadcast_global_config: bool,
    ) -> Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1ObservedTraversalSummaryV1 {
    pub schema_version: u16,
    pub cutoff_slot: u64,
    pub canonical_operation_count: u64,
    pub replayed_transition_count: u64,
    pub market_count: u64,
    pub terminal_state_set_sha256: String,
}

#[derive(Serialize)]
struct CanonicalCheckpointCommitmentV1<'a> {
    schema_version: u16,
    engine_id: &'a str,
    program_revision: crate::v1_engine::V1ProgramRevisionV1,
    pair_key: String,
    pair: &'a V1PairCheckpointV1,
    rate_model: &'a V1RateModelCheckpointV1,
    authority: &'a V1AuthorityCheckpointV1,
    positions: Vec<(String, &'a V1PositionCheckpointV1)>,
    custody: &'a V1CustodyCheckpointV1,
    revenue: &'a V1RevenueCheckpointV1,
}

fn canonical_checkpoint_commitment(
    checkpoint: &V1NativeCheckpointV1,
) -> CanonicalCheckpointCommitmentV1<'_> {
    CanonicalCheckpointCommitmentV1 {
        schema_version: checkpoint.schema_version,
        engine_id: &checkpoint.engine_id,
        program_revision: checkpoint.program_revision,
        pair_key: checkpoint.pair_key.to_string(),
        pair: &checkpoint.pair,
        rate_model: &checkpoint.rate_model,
        authority: &checkpoint.authority,
        positions: checkpoint
            .positions
            .iter()
            .map(|(key, position)| (key.to_string(), position))
            .collect(),
        custody: &checkpoint.custody,
        revenue: &checkpoint.revenue,
    }
}

/// Deterministic JSON commitment that represents public keys as explicit
/// strings rather than JSON map keys. This remains valid when positions are
/// nonempty; directly serializing `BTreeMap<Pubkey, _>` is not portable across
/// serde human-readable map-key implementations.
pub fn canonical_terminal_checkpoint_sha256(checkpoint: &V1NativeCheckpointV1) -> Result<String> {
    let commitment = canonical_checkpoint_commitment(checkpoint);
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &commitment,
    )?)))
}

#[derive(Serialize)]
struct CanonicalFactualRecordCommitmentV1<'a> {
    schema_version: u16,
    operation_id: &'a str,
    order: ChainOrder,
    checkpoint_before: Option<CanonicalCheckpointCommitmentV1<'a>>,
    checkpoint_after: CanonicalCheckpointCommitmentV1<'a>,
    reconciliations: &'a [crate::factual_replay::V1RawUnitReconciliationV1],
    conservation: &'a [V1ConservationIdentityV1],
    revenue_delta: &'a V1RevenueDeltaV1,
    corrections: &'a [V1DecisionGradeCorrectionEvidenceV1],
    retained_event_limitations: &'a [V1DecisionGradeRetainedEventLimitationV1],
    behavior: &'a Option<V1BehaviorObservationV1>,
}

fn canonical_factual_record_sha256(record: &V1FactualOperationRecordV1) -> Result<String> {
    let commitment = CanonicalFactualRecordCommitmentV1 {
        schema_version: record.schema_version,
        operation_id: &record.operation_id,
        order: record.order,
        checkpoint_before: record
            .checkpoint_before
            .as_ref()
            .map(canonical_checkpoint_commitment),
        checkpoint_after: canonical_checkpoint_commitment(&record.checkpoint_after),
        reconciliations: &record.reconciliations,
        conservation: &record.conservation,
        revenue_delta: &record.revenue_delta,
        corrections: &record.corrections,
        retained_event_limitations: &record.retained_event_limitations,
        behavior: &record.behavior,
    };
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &commitment,
    )?)))
}

fn canonical_positions_sha256(
    positions: &BTreeMap<Pubkey, V1PositionCheckpointV1>,
) -> Result<String> {
    let positions: Vec<_> = positions
        .iter()
        .map(|(key, position)| (key.to_string(), position))
        .collect();
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&positions)?)))
}

struct MarketReplayState {
    replay: V1FactualReplay,
    initialized_at: ChainOrder,
    replayed_transitions: u64,
    raw_unit_comparisons: u64,
    conservation_checks: u64,
    terminal_checkpoint_sha256: String,
}

/// Replays a globally ordered canonical JSONL stream and writes a compact,
/// deterministic ledger atomically. The caller must independently verify the
/// decoder/config coverage and exact-cutoff terminal accounts before accepting
/// this summary for a headline benchmark.
pub fn reconcile_history_to_ledger(
    operations_path: &Path,
    configurations_path: &Path,
    ledger_path: &Path,
    run_id: &str,
    cutoff_slot: u64,
    certification_policy: V1ReplayCertificationPolicyV1,
) -> Result<V1ReconciliationSummaryV1> {
    Ok(reconcile_history_to_ledger_with_terminal(
        operations_path,
        configurations_path,
        ledger_path,
        run_id,
        cutoff_slot,
        certification_policy,
    )?
    .summary)
}

/// Replays history and retains each market's full terminal checkpoint for an
/// independent exact-slot account comparison. The compatibility wrapper above
/// intentionally discards these private position details.
pub fn reconcile_history_to_ledger_with_terminal(
    operations_path: &Path,
    configurations_path: &Path,
    ledger_path: &Path,
    run_id: &str,
    cutoff_slot: u64,
    certification_policy: V1ReplayCertificationPolicyV1,
) -> Result<V1ReconciliationRunV1> {
    let configurations: Vec<ConfigVersionV1> = read_json_lines(configurations_path)?;
    let config_index = ConfigIndex::new(configurations)?;
    let input = File::open(operations_path)
        .with_context(|| format!("opening {}", operations_path.display()))?;
    let partial = ledger_path.with_extension("jsonl.partial");
    let mut writer = BufWriter::new(
        File::create(&partial).with_context(|| format!("creating {}", partial.display()))?,
    );
    let mut ledger_digest = Sha256::new();
    let mut markets = BTreeMap::<String, MarketReplayState>::new();
    let mut previous_order = None;
    let mut canonical_operation_count = 0_u64;
    let mut replayed_transition_count = 0_u64;
    let mut authority_seed_operation_count = 0_u64;
    let mut broadcast_global_config_applications = 0_u64;
    let mut raw_unit_comparison_count = 0_u64;
    let mut nonzero_raw_unit_difference_count = 0_u64;
    let mut conservation_check_count = 0_u64;
    let mut conservation_failure_count = 0_u64;

    for (line_index, line) in BufReader::new(input).lines().enumerate() {
        let line = line.with_context(|| {
            format!(
                "reading canonical operation line {} from {}",
                line_index + 1,
                operations_path.display()
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let operation: CanonicalOperationV1 = serde_json::from_str(&line)
            .with_context(|| format!("decoding canonical operation line {}", line_index + 1))?;
        operation.validate()?;
        if operation.order.slot > cutoff_slot {
            bail!(
                "operation {} at slot {} exceeds frozen cutoff {}",
                operation.operation_id,
                operation.order.slot,
                cutoff_slot
            );
        }
        if previous_order.is_some_and(|previous| previous >= operation.order) {
            bail!("canonical operations are not in strict chain order");
        }
        previous_order = Some(operation.order);
        canonical_operation_count = canonical_operation_count
            .checked_add(1)
            .context("canonical operation count overflow")?;

        match &operation.payload {
            OperationPayloadV1::InitializeMarket { .. } => {
                if markets.contains_key(&operation.pair) {
                    bail!("market {} was initialized more than once", operation.pair);
                }
                let initialization = canonical_initialization_input_indexed(
                    &operation,
                    &config_index,
                    certification_policy,
                )?;
                let (replay, record) =
                    V1FactualReplay::from_canonical_initialization(&operation, initialization)?;
                let row = compact_row(&operation, &record, false)?;
                write_ledger_row(&mut writer, &mut ledger_digest, &row)?;
                accumulate_record(
                    &record,
                    &mut raw_unit_comparison_count,
                    &mut nonzero_raw_unit_difference_count,
                    &mut conservation_check_count,
                    &mut conservation_failure_count,
                )?;
                replayed_transition_count = replayed_transition_count
                    .checked_add(1)
                    .context("replayed transition count overflow")?;
                markets.insert(
                    operation.pair.clone(),
                    MarketReplayState {
                        replay,
                        initialized_at: operation.order,
                        replayed_transitions: 1,
                        raw_unit_comparisons: u64::try_from(record.reconciliations.len())?,
                        conservation_checks: u64::try_from(record.conservation.len())?,
                        terminal_checkpoint_sha256: row
                            .compact_checkpoint
                            .checkpoint_sha256
                            .clone(),
                    },
                );
            }
            OperationPayloadV1::UpdateConfig { fields }
                if fields.get("instruction_kind").map(String::as_str)
                    == Some("init_futarchy_authority") =>
            {
                if !markets.is_empty() {
                    bail!("futarchy authority initialization followed market initialization");
                }
                authority_seed_operation_count = authority_seed_operation_count
                    .checked_add(1)
                    .context("authority seed operation count overflow")?;
            }
            OperationPayloadV1::UpdateConfig { fields }
                if is_global_config(fields.get("instruction_kind").map(String::as_str)) =>
            {
                if markets.is_empty() {
                    // The selected replay can begin after the global authority
                    // was initialized but before its first selected market.
                    // ConfigIndex retains those exact global epochs; a later
                    // market initialization resolves the active snapshot at
                    // its own ChainOrder. There is no per-market transition
                    // (and therefore no ledger row) to emit yet.
                    continue;
                }
                for (market, state) in &mut markets {
                    let record = state.replay.apply_operation(&operation)?;
                    let row = compact_row(&operation, &record, true)?;
                    if &row.market != market {
                        bail!("global replay checkpoint market identity changed");
                    }
                    write_ledger_row(&mut writer, &mut ledger_digest, &row)?;
                    accumulate_record(
                        &record,
                        &mut raw_unit_comparison_count,
                        &mut nonzero_raw_unit_difference_count,
                        &mut conservation_check_count,
                        &mut conservation_failure_count,
                    )?;
                    update_market_summary(state, &record, &row)?;
                    replayed_transition_count = replayed_transition_count
                        .checked_add(1)
                        .context("replayed transition count overflow")?;
                    broadcast_global_config_applications = broadcast_global_config_applications
                        .checked_add(1)
                        .context("global config application count overflow")?;
                }
            }
            _ => {
                let state = markets.get_mut(&operation.pair).with_context(|| {
                    format!(
                        "operation {} references market {} before exact initialization",
                        operation.operation_id, operation.pair
                    )
                })?;
                let record = state.replay.apply_operation(&operation)?;
                let row = compact_row(&operation, &record, false)?;
                write_ledger_row(&mut writer, &mut ledger_digest, &row)?;
                accumulate_record(
                    &record,
                    &mut raw_unit_comparison_count,
                    &mut nonzero_raw_unit_difference_count,
                    &mut conservation_check_count,
                    &mut conservation_failure_count,
                )?;
                update_market_summary(state, &record, &row)?;
                replayed_transition_count = replayed_transition_count
                    .checked_add(1)
                    .context("replayed transition count overflow")?;
            }
        }
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    fs::rename(&partial, ledger_path).with_context(|| {
        format!(
            "committing factual V1 replay ledger {}",
            ledger_path.display()
        )
    })?;

    let terminal_checkpoints: BTreeMap<_, _> = markets
        .values()
        .map(|state| {
            let checkpoint = state.replay.engine().checkpoint();
            (checkpoint.pair_key, checkpoint)
        })
        .collect();
    let mut market_summaries: Vec<_> = markets
        .iter()
        .map(|(market, state)| V1MarketReplaySummaryV1 {
            market: market.clone(),
            initialized_at: state.initialized_at,
            replayed_transitions: state.replayed_transitions,
            raw_unit_comparisons: state.raw_unit_comparisons,
            conservation_checks: state.conservation_checks,
            terminal_checkpoint_sha256: state.terminal_checkpoint_sha256.clone(),
        })
        .collect();
    market_summaries.sort_by(|left, right| left.market.cmp(&right.market));
    if terminal_checkpoints.len() != market_summaries.len() {
        bail!("terminal checkpoint market keys are not unique");
    }
    Ok(V1ReconciliationRunV1 {
        summary: V1ReconciliationSummaryV1 {
            schema_version: V1_RECONCILIATION_SCHEMA_VERSION,
            run_id: run_id.to_owned(),
            cutoff_slot,
            canonical_operation_count,
            replayed_transition_count,
            authority_seed_operation_count,
            broadcast_global_config_applications,
            market_count: u64::try_from(market_summaries.len())?,
            raw_unit_comparison_count,
            nonzero_raw_unit_difference_count,
            conservation_check_count,
            conservation_failure_count,
            zero_raw_unit_difference: nonzero_raw_unit_difference_count == 0,
            all_conservation_identities_hold: conservation_failure_count == 0,
            terminal_state_verified: false,
            markets: market_summaries,
            ledger_sha256: hex::encode(ledger_digest.finalize()),
        },
        terminal_checkpoints,
    })
}

/// Performs one exact factual traversal without writing another ledger and
/// exposes native pre/post states to a bounded private observer. This is the
/// only supported way for a later campaign to capture untargeted positions;
/// compact reconciliation rows are intentionally insufficient.
pub fn traverse_v1_history_observed(
    operations_path: &Path,
    configurations_path: &Path,
    cutoff_slot: u64,
    certification_policy: V1ReplayCertificationPolicyV1,
    observer: &mut dyn V1HistoricalStateObserverV1,
) -> Result<V1ObservedTraversalSummaryV1> {
    let configurations: Vec<ConfigVersionV1> = read_json_lines(configurations_path)?;
    let config_index = ConfigIndex::new(configurations)?;
    let input = File::open(operations_path)
        .with_context(|| format!("opening {}", operations_path.display()))?;
    let mut markets = BTreeMap::<String, V1FactualReplay>::new();
    let mut previous_order = None;
    let mut canonical_operation_count = 0_u64;
    let mut replayed_transition_count = 0_u64;

    for (line_index, line) in BufReader::new(input).lines().enumerate() {
        let line = line.with_context(|| {
            format!(
                "reading canonical operation line {} from {}",
                line_index + 1,
                operations_path.display()
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let operation: CanonicalOperationV1 = serde_json::from_str(&line)
            .with_context(|| format!("decoding canonical operation line {}", line_index + 1))?;
        operation.validate()?;
        if operation.order.slot > cutoff_slot {
            bail!(
                "operation {} at slot {} exceeds frozen cutoff {}",
                operation.operation_id,
                operation.order.slot,
                cutoff_slot
            );
        }
        if previous_order.is_some_and(|previous| previous >= operation.order) {
            bail!("canonical operations are not in strict chain order");
        }
        previous_order = Some(operation.order);
        canonical_operation_count = canonical_operation_count
            .checked_add(1)
            .context("canonical operation count overflow")?;

        match &operation.payload {
            OperationPayloadV1::InitializeMarket { .. } => {
                if markets.contains_key(&operation.pair) {
                    bail!("market {} was initialized more than once", operation.pair);
                }
                observer.observe_pre_operation(&operation, &operation.pair, None, false)?;
                let initialization = canonical_initialization_input_indexed(
                    &operation,
                    &config_index,
                    certification_policy,
                )?;
                let (replay, _) =
                    V1FactualReplay::from_canonical_initialization(&operation, initialization)?;
                observer.observe_post_operation(
                    &operation,
                    &operation.pair,
                    replay.engine(),
                    false,
                )?;
                markets.insert(operation.pair.clone(), replay);
                replayed_transition_count = replayed_transition_count
                    .checked_add(1)
                    .context("replayed transition count overflow")?;
            }
            OperationPayloadV1::UpdateConfig { fields }
                if fields.get("instruction_kind").map(String::as_str)
                    == Some("init_futarchy_authority") =>
            {
                if !markets.is_empty() {
                    bail!("futarchy authority initialization followed market initialization");
                }
            }
            OperationPayloadV1::UpdateConfig { fields }
                if is_global_config(fields.get("instruction_kind").map(String::as_str)) =>
            {
                if markets.is_empty() {
                    // See reconcile_history_to_ledger_with_terminal: global
                    // epochs are staged in ConfigIndex until a selected market
                    // is initialized, so an observer has no market state to
                    // receive for this operation.
                    continue;
                }
                for (market, replay) in &mut markets {
                    observer.observe_pre_operation(
                        &operation,
                        market,
                        Some(replay.engine()),
                        true,
                    )?;
                    replay.apply_operation(&operation)?;
                    observer.observe_post_operation(&operation, market, replay.engine(), true)?;
                    replayed_transition_count = replayed_transition_count
                        .checked_add(1)
                        .context("replayed transition count overflow")?;
                }
            }
            _ => {
                let replay = markets.get_mut(&operation.pair).with_context(|| {
                    format!(
                        "operation {} references market {} before exact initialization",
                        operation.operation_id, operation.pair
                    )
                })?;
                observer.observe_pre_operation(
                    &operation,
                    &operation.pair,
                    Some(replay.engine()),
                    false,
                )?;
                replay.apply_operation(&operation)?;
                observer.observe_post_operation(
                    &operation,
                    &operation.pair,
                    replay.engine(),
                    false,
                )?;
                replayed_transition_count = replayed_transition_count
                    .checked_add(1)
                    .context("replayed transition count overflow")?;
            }
        }
    }

    let terminal_rows: Vec<_> = markets
        .iter()
        .map(|(market, replay)| {
            canonical_terminal_checkpoint_sha256(&replay.engine().checkpoint())
                .map(|hash| (market, hash))
        })
        .collect::<Result<_>>()?;
    let terminal_state_set_sha256 =
        hex::encode(Sha256::digest(serde_json::to_vec(&terminal_rows)?));
    Ok(V1ObservedTraversalSummaryV1 {
        schema_version: V1_OBSERVED_TRAVERSAL_SCHEMA_VERSION,
        cutoff_slot,
        canonical_operation_count,
        replayed_transition_count,
        market_count: u64::try_from(markets.len())?,
        terminal_state_set_sha256,
    })
}

pub(crate) fn is_global_config(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            "set_global_reduce_only"
                | "update_futarchy_authority"
                | "update_protocol_revenue"
                | "update_revenue_recipients"
        )
    )
}

fn update_market_summary(
    state: &mut MarketReplayState,
    record: &V1FactualOperationRecordV1,
    row: &V1ReplayLedgerRowV1,
) -> Result<()> {
    state.replayed_transitions = state
        .replayed_transitions
        .checked_add(1)
        .context("market transition count overflow")?;
    state.raw_unit_comparisons = state
        .raw_unit_comparisons
        .checked_add(u64::try_from(record.reconciliations.len())?)
        .context("market reconciliation count overflow")?;
    state.conservation_checks = state
        .conservation_checks
        .checked_add(u64::try_from(record.conservation.len())?)
        .context("market conservation count overflow")?;
    state.terminal_checkpoint_sha256 = row.compact_checkpoint.checkpoint_sha256.clone();
    Ok(())
}

fn accumulate_record(
    record: &V1FactualOperationRecordV1,
    raw_count: &mut u64,
    nonzero_count: &mut u64,
    conservation_count: &mut u64,
    conservation_failure_count: &mut u64,
) -> Result<()> {
    *raw_count = raw_count
        .checked_add(u64::try_from(record.reconciliations.len())?)
        .context("raw reconciliation count overflow")?;
    *nonzero_count = nonzero_count
        .checked_add(u64::try_from(
            record
                .reconciliations
                .iter()
                .filter(|item| item.raw_unit_difference != "0")
                .count(),
        )?)
        .context("nonzero reconciliation count overflow")?;
    *conservation_count = conservation_count
        .checked_add(u64::try_from(record.conservation.len())?)
        .context("conservation count overflow")?;
    *conservation_failure_count = conservation_failure_count
        .checked_add(u64::try_from(
            record
                .conservation
                .iter()
                .filter(|item| !item.holds)
                .count(),
        )?)
        .context("conservation failure count overflow")?;
    Ok(())
}

fn compact_row(
    operation: &CanonicalOperationV1,
    record: &V1FactualOperationRecordV1,
    broadcast_global_config: bool,
) -> Result<V1ReplayLedgerRowV1> {
    let checkpoint = &record.checkpoint_after;
    let positions_sha256 = canonical_positions_sha256(&checkpoint.positions)?;
    let touched_position = touched_position_key(&operation.payload)
        .map(|key| Pubkey::from_str(key).context("decoding touched position public key"))
        .transpose()?
        .and_then(|key| checkpoint.positions.get(&key).cloned());
    let checkpoint_sha256 = canonical_terminal_checkpoint_sha256(checkpoint)?;
    let reconciliations_sha256 =
        hex::encode(Sha256::digest(serde_json::to_vec(&record.reconciliations)?));
    let factual_record_sha256 = canonical_factual_record_sha256(record)?;
    Ok(V1ReplayLedgerRowV1 {
        schema_version: V1_RECONCILIATION_SCHEMA_VERSION,
        market: checkpoint.pair_key.to_string(),
        operation_id: operation.operation_id.clone(),
        order: operation.order,
        broadcast_global_config,
        compact_checkpoint: V1CompactCheckpointV1 {
            pair: checkpoint.pair.clone(),
            rate_model: checkpoint.rate_model.clone(),
            authority: checkpoint.authority.clone(),
            custody: checkpoint.custody,
            revenue: checkpoint.revenue,
            open_position_count: u64::try_from(checkpoint.positions.len())?,
            positions_sha256,
            touched_position,
            checkpoint_sha256,
        },
        reconciliation_count: u64::try_from(record.reconciliations.len())?,
        reconciliations_sha256,
        all_raw_unit_differences_zero: record
            .reconciliations
            .iter()
            .all(|item| item.raw_unit_difference == "0"),
        conservation: record.conservation.clone(),
        revenue_delta: record.revenue_delta,
        corrections: record.corrections.clone(),
        retained_event_limitations: record.retained_event_limitations.clone(),
        behavior: record.behavior.clone(),
        factual_record_sha256,
    })
}

fn write_ledger_row(
    writer: &mut BufWriter<File>,
    digest: &mut Sha256,
    row: &V1ReplayLedgerRowV1,
) -> Result<()> {
    let bytes = serde_json::to_vec(row)?;
    digest.update(&bytes);
    digest.update(b"\n");
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn touched_position_key(payload: &OperationPayloadV1) -> Option<&str> {
    match payload {
        OperationPayloadV1::CollateralDelta { position, .. }
        | OperationPayloadV1::Borrow { position, .. }
        | OperationPayloadV1::Repay { position, .. }
        | OperationPayloadV1::Liquidate { position, .. } => Some(position),
        _ => None,
    }
}

struct ConfigIndex {
    by_key: BTreeMap<(ConfigScope, String), Vec<ConfigVersionV1>>,
}

impl ConfigIndex {
    fn new(configurations: Vec<ConfigVersionV1>) -> Result<Self> {
        let mut by_key = BTreeMap::<(ConfigScope, String), Vec<ConfigVersionV1>>::new();
        let mut ids = BTreeSet::new();
        for configuration in configurations {
            if !ids.insert(configuration.config_id.clone()) {
                bail!("duplicate configuration id {}", configuration.config_id);
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
            if epochs.windows(2).any(|epochs| {
                epochs[0].valid_to_exclusive != Some(epochs[1].valid_from)
                    || epochs[0].valid_from >= epochs[1].valid_from
            }) {
                bail!("configuration epochs are overlapping or non-contiguous");
            }
        }
        Ok(Self { by_key })
    }

    fn active(
        &self,
        scope: ConfigScope,
        entity: &str,
        order: ChainOrder,
    ) -> Result<&ConfigVersionV1> {
        let matches: Vec<_> = self
            .by_key
            .get(&(scope.clone(), entity.to_owned()))
            .into_iter()
            .flatten()
            .filter(|epoch| {
                epoch.valid_from <= order
                    && epoch
                        .valid_to_exclusive
                        .is_none_or(|exclusive| order < exclusive)
            })
            .collect();
        if matches.len() != 1 {
            bail!(
                "expected exactly one active {:?} configuration for {} at {:?}; found {}",
                scope,
                entity,
                order,
                matches.len()
            );
        }
        Ok(matches[0])
    }
}

pub fn canonical_initialization_input(
    operation: &CanonicalOperationV1,
    configurations: &[ConfigVersionV1],
    certification_policy: V1ReplayCertificationPolicyV1,
) -> Result<V1CanonicalInitializationInputV1> {
    let index = ConfigIndex::new(configurations.to_vec())?;
    canonical_initialization_input_indexed(operation, &index, certification_policy)
}

fn canonical_initialization_input_indexed(
    operation: &CanonicalOperationV1,
    configurations: &ConfigIndex,
    certification_policy: V1ReplayCertificationPolicyV1,
) -> Result<V1CanonicalInitializationInputV1> {
    let OperationPayloadV1::InitializeMarket {
        token0,
        token1,
        token0_decimals,
        token1_decimals,
        gross_amount0,
        gross_amount1,
        vault_credit0,
        vault_credit1,
        minted_lp,
        min_liquidity_out,
        fields,
    } = &operation.payload
    else {
        bail!("canonical initialization input requires initialize_market");
    };
    let protocol = configurations.active(
        ConfigScope::ProtocolRevenue,
        GLOBAL_CONFIG_ENTITY,
        operation.order,
    )?;
    let recipients = configurations.active(
        ConfigScope::RevenueRecipients,
        GLOBAL_CONFIG_ENTITY,
        operation.order,
    )?;
    let reduce_only = configurations.active(
        ConfigScope::ReduceOnly,
        GLOBAL_CONFIG_ENTITY,
        operation.order,
    )?;
    let program = configurations.active(
        ConfigScope::ProgramBinary,
        "__program_binary__",
        operation.order,
    )?;
    let provisional = match program.fields.get("provisional").map(String::as_str) {
        Some("true") => true,
        Some("false") => false,
        _ => bail!("program-binary configuration has invalid provisional metadata"),
    };
    let candidate_program_binary_sha256 = program
        .fields
        .get("candidate_program_binary_sha256")
        .filter(|hash| valid_sha256(hash));
    let deployed_program_binary_sha256 = program.fields.get("program_binary_sha256");
    let structurally_bound = program.decoder_version_id == operation.decoder_version_id
        && program
            .fields
            .get("source_git_commit")
            .is_some_and(|value| valid_git_oid(value))
        && program
            .fields
            .get("idl_sha256")
            .is_some_and(|hash| valid_sha256(hash))
        && candidate_program_binary_sha256.is_some()
        && deployed_program_binary_sha256.is_none_or(|hash| valid_sha256(hash));
    if !structurally_bound {
        bail!(
            "initialize operation is not bound to a matching decoder/candidate-program configuration"
        );
    }
    if certification_policy == V1ReplayCertificationPolicyV1::AuditGrade {
        if provisional {
            bail!("audit-grade native replay requires a nonprovisional decoder revision");
        }
        if deployed_program_binary_sha256 != candidate_program_binary_sha256 {
            bail!(
                "audit-grade native replay requires the exact deployed ELF hash to match the candidate ELF hash"
            );
        }
        if !HISTORICAL_DECODER_DISPATCH_VERIFIED {
            bail!("audit-grade native replay requires verified historical decoder dispatch");
        }
    }
    let (authority_pda, authority_bump) =
        Pubkey::find_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX], &omnipair::ID);
    let _ = authority_pda;
    let pair_version = required_u8(fields, "version")?;
    if pair_version != VERSION {
        bail!("linked V1 source supports pair version {VERSION}, not {pair_version}");
    }
    Ok(V1CanonicalInitializationInputV1 {
        program_version: pair_version,
        pair_account_version: pair_version,
        slot: operation.order.slot,
        pair_key: required_pubkey_text("pair", &operation.pair)?,
        token0: required_pubkey_text("token0", token0)?,
        token1: required_pubkey_text("token1", token1)?,
        lp_mint: required_pubkey(fields, "lp_mint")?,
        rate_model_key: required_pubkey(fields, "rate_model")?,
        rate_model_address_policy: if provisional {
            V1RateModelAddressPolicyV1::HistoricalExplicitInitAccount
        } else {
            V1RateModelAddressPolicyV1::PairDerivedPda
        },
        collateral_factor_transition_policy: if provisional {
            V1CollateralFactorTransitionPolicyV1::EventWitnessedCollateralFactorTransition
        } else {
            V1CollateralFactorTransitionPolicyV1::SourceNative
        },
        post_state_correction_policy: if provisional {
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        } else {
            V1PostStateCorrectionPolicyV1::SourceNative
        },
        token0_decimals: *token0_decimals,
        token1_decimals: *token1_decimals,
        swap_fee_bps: required_u16(fields, "swap_fee_bps")?,
        half_life_ms: required_u64(fields, "half_life")?,
        fixed_cf_bps: optional_u16(fields, "instruction.args.fixed_cf_bps")?,
        target_util_start_bps_arg: optional_u64(fields, "instruction.args.target_util_start_bps")?,
        target_util_end_bps_arg: optional_u64(fields, "instruction.args.target_util_end_bps")?,
        rate_half_life_ms_arg: optional_u64(fields, "instruction.args.rate_half_life_ms")?,
        min_rate_bps_arg: optional_u64(fields, "instruction.args.min_rate_bps")?,
        max_rate_bps_arg: optional_u64(fields, "instruction.args.max_rate_bps")?,
        initial_rate_bps: optional_u64(fields, "instruction.args.initial_rate_bps")?
            .unwrap_or(DEFAULT_INITIAL_RATE_BPS),
        params_hash: required_hash32(fields, "params_hash")?,
        gross_amount0: amount_u64("gross_amount0", gross_amount0.as_str())?,
        gross_amount1: amount_u64("gross_amount1", gross_amount1.as_str())?,
        vault_credit0: amount_u64("vault_credit0", vault_credit0.as_str())?,
        vault_credit1: amount_u64("vault_credit1", vault_credit1.as_str())?,
        minted_lp: amount_u64("minted_lp", minted_lp.as_str())?,
        min_liquidity_out: amount_u64("min_liquidity_out", min_liquidity_out.as_str())?,
        authority: V1CanonicalAuthorityInitializationV1 {
            version: FutarchyAuthority::CURRENT_VERSION,
            authority: required_config_pubkey(&protocol.fields, "authority")?,
            swap_bps: required_u16(&protocol.fields, "swap_bps")?,
            interest_bps: required_u16(&protocol.fields, "interest_bps")?,
            futarchy_treasury: required_config_pubkey(&recipients.fields, "futarchy_treasury")?,
            buybacks_vault: required_config_pubkey(&recipients.fields, "buybacks_vault")?,
            team_treasury: required_config_pubkey(&recipients.fields, "team_treasury")?,
            futarchy_treasury_bps: required_u16(&protocol.fields, "futarchy_treasury_bps")?,
            buybacks_vault_bps: required_u16(&protocol.fields, "buybacks_vault_bps")?,
            team_treasury_bps: required_u16(&protocol.fields, "team_treasury_bps")?,
            global_reduce_only: required_bool(&reduce_only.fields, "reduce_only")?,
            bump: authority_bump,
        },
    })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn required_text<'a>(fields: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    fields
        .get(key)
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .with_context(|| format!("missing canonical field {key}"))
}

fn required_u64(fields: &BTreeMap<String, String>, key: &str) -> Result<u64> {
    required_text(fields, key)?
        .parse::<u64>()
        .with_context(|| format!("invalid u64 canonical field {key}"))
}

fn required_u16(fields: &BTreeMap<String, String>, key: &str) -> Result<u16> {
    required_text(fields, key)?
        .parse::<u16>()
        .with_context(|| format!("invalid u16 canonical field {key}"))
}

fn required_u8(fields: &BTreeMap<String, String>, key: &str) -> Result<u8> {
    required_text(fields, key)?
        .parse::<u8>()
        .with_context(|| format!("invalid u8 canonical field {key}"))
}

fn optional_u64(fields: &BTreeMap<String, String>, key: &str) -> Result<Option<u64>> {
    fields
        .get(key)
        .filter(|value| value.as_str() != "null")
        .map(|value| {
            value
                .parse::<u64>()
                .with_context(|| format!("invalid optional u64 canonical field {key}"))
        })
        .transpose()
}

fn optional_u16(fields: &BTreeMap<String, String>, key: &str) -> Result<Option<u16>> {
    fields
        .get(key)
        .filter(|value| value.as_str() != "null")
        .map(|value| {
            value
                .parse::<u16>()
                .with_context(|| format!("invalid optional u16 canonical field {key}"))
        })
        .transpose()
}

fn required_bool(fields: &BTreeMap<String, String>, key: &str) -> Result<bool> {
    required_text(fields, key)?
        .parse::<bool>()
        .with_context(|| format!("invalid boolean canonical field {key}"))
}

fn required_pubkey(fields: &BTreeMap<String, String>, key: &str) -> Result<Pubkey> {
    required_pubkey_text(key, required_text(fields, key)?)
}

fn required_config_pubkey(fields: &BTreeMap<String, String>, key: &str) -> Result<Pubkey> {
    let value = required_text(fields, key)?;
    parse_schema_pubkey(value).with_context(|| format!("invalid public key canonical field {key}"))
}

fn required_pubkey_text(key: &str, value: &str) -> Result<Pubkey> {
    Pubkey::from_str(value).with_context(|| format!("invalid public key canonical field {key}"))
}

fn required_hash32(fields: &BTreeMap<String, String>, key: &str) -> Result<[u8; 32]> {
    let values: Vec<u8> = serde_json::from_str(required_text(fields, key)?)
        .with_context(|| format!("invalid byte-array canonical field {key}"))?;
    values.try_into().map_err(|values: Vec<u8>| {
        anyhow::anyhow!("canonical field {key} has {} bytes", values.len())
    })
}

fn amount_u64(field: &str, value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .with_context(|| format!("canonical amount {field} exceeds native u64"))
}

fn read_json_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    BufReader::new(file)
        .lines()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Ok(line) if line.trim().is_empty() => None,
            Ok(line) => Some(
                serde_json::from_str(&line)
                    .with_context(|| format!("decoding {} line {}", path.display(), index + 1)),
            ),
            Err(error) => Some(Err(error.into())),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{CanonicalEventV1, EventPayloadV1, TokenAmount};
    use omnipair::constants::{
        DEFAULT_MAX_RATE_BPS, DEFAULT_MIN_RATE_BPS, DEFAULT_RATE_HALF_LIFE_MS, PAIR_SEED_PREFIX,
        RATE_MODEL_SEED_PREFIX, TARGET_UTIL_END_BPS, TARGET_UTIL_START_BPS,
    };
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn token(value: u64) -> TokenAmount {
        TokenAmount::new(value.to_string()).unwrap()
    }

    fn order(slot: u64, event_sequence: u32) -> ChainOrder {
        ChainOrder {
            slot,
            transaction_index: 0,
            outer_instruction_index: 0,
            inner_execution_index: None,
            event_sequence,
        }
    }

    fn params_hash(
        version: u8,
        swap_fee_bps: u16,
        half_life: u64,
        fixed_cf_bps: Option<u16>,
    ) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(version.to_le_bytes());
        digest.update(swap_fee_bps.to_le_bytes());
        digest.update(half_life.to_le_bytes());
        digest.update(fixed_cf_bps.unwrap_or(0).to_le_bytes());
        digest.update(0_u64.to_le_bytes());
        digest.update(0_u64.to_le_bytes());
        digest.update(0_u64.to_le_bytes());
        digest.update(0_u64.to_le_bytes());
        digest.update(0_u64.to_le_bytes());
        digest.finalize().into()
    }

    fn epoch(
        scope: ConfigScope,
        entity: &str,
        fields: BTreeMap<String, String>,
    ) -> ConfigVersionV1 {
        ConfigVersionV1 {
            config_id: format!("{scope:?}:{entity}"),
            scope,
            entity_address: entity.to_owned(),
            valid_from: order(0, 0),
            valid_to_exclusive: None,
            source_signature: "fixture".to_owned(),
            decoder_version_id: "verified-v1".to_owned(),
            fields,
        }
    }

    fn fixture() -> (CanonicalOperationV1, Vec<ConfigVersionV1>) {
        let token0 = Pubkey::new_from_array([1; 32]);
        let token1 = Pubkey::new_from_array([2; 32]);
        let lp_mint = Pubkey::new_from_array([3; 32]);
        let swap_fee_bps = 30;
        let half_life = 60_000;
        let fixed_cf_bps = None;
        let params_hash = params_hash(VERSION, swap_fee_bps, half_life, fixed_cf_bps);
        let pair = Pubkey::find_program_address(
            &[
                PAIR_SEED_PREFIX,
                token0.as_ref(),
                token1.as_ref(),
                params_hash.as_ref(),
            ],
            &omnipair::ID,
        )
        .0;
        let rate_model =
            Pubkey::find_program_address(&[RATE_MODEL_SEED_PREFIX, pair.as_ref()], &omnipair::ID).0;
        let mut fields = BTreeMap::from([
            ("token0".to_owned(), token0.to_string()),
            ("token1".to_owned(), token1.to_string()),
            ("lp_mint".to_owned(), lp_mint.to_string()),
            ("token0_decimals".to_owned(), "6".to_owned()),
            ("token1_decimals".to_owned(), "6".to_owned()),
            ("rate_model".to_owned(), rate_model.to_string()),
            ("swap_fee_bps".to_owned(), swap_fee_bps.to_string()),
            ("half_life".to_owned(), half_life.to_string()),
            (
                "target_util_start_bps".to_owned(),
                TARGET_UTIL_START_BPS.to_string(),
            ),
            (
                "target_util_end_bps".to_owned(),
                TARGET_UTIL_END_BPS.to_string(),
            ),
            (
                "rate_half_life_ms".to_owned(),
                DEFAULT_RATE_HALF_LIFE_MS.to_string(),
            ),
            ("min_rate_bps".to_owned(), DEFAULT_MIN_RATE_BPS.to_string()),
            ("max_rate_bps".to_owned(), DEFAULT_MAX_RATE_BPS.to_string()),
            ("params_hash".to_owned(), format!("{:?}", params_hash)),
            ("version".to_owned(), VERSION.to_string()),
            (
                "instruction.args.fixed_cf_bps".to_owned(),
                "null".to_owned(),
            ),
            (
                "instruction.args.target_util_start_bps".to_owned(),
                "null".to_owned(),
            ),
            (
                "instruction.args.target_util_end_bps".to_owned(),
                "null".to_owned(),
            ),
            (
                "instruction.args.rate_half_life_ms".to_owned(),
                "null".to_owned(),
            ),
            (
                "instruction.args.min_rate_bps".to_owned(),
                "null".to_owned(),
            ),
            (
                "instruction.args.max_rate_bps".to_owned(),
                "null".to_owned(),
            ),
            (
                "instruction.args.initial_rate_bps".to_owned(),
                "null".to_owned(),
            ),
            ("instruction.args.version".to_owned(), VERSION.to_string()),
        ]);
        fields.insert("event_kind".to_owned(), "pair_created".to_owned());
        let assertion = CanonicalEventV1::new(
            "mainnet-beta".to_owned(),
            omnipair::ID.to_string(),
            Some(pair.to_string()),
            None,
            "signature".to_owned(),
            order(100, 1),
            vec![0, 0],
            1_700_000_000,
            "verified-v1".to_owned(),
            b"pair-created",
            None,
            EventPayloadV1::Config(fields.clone()),
        );
        let operation = CanonicalOperationV1 {
            schema_version: 1,
            operation_id: "initialize".to_owned(),
            signature: "signature".to_owned(),
            pair: pair.to_string(),
            actor: Some(Pubkey::new_from_array([9; 32]).to_string()),
            order: order(100, 0),
            block_time_unix_seconds: 1_700_000_000,
            decoder_version_id: "verified-v1".to_owned(),
            payload: OperationPayloadV1::InitializeMarket {
                token0: token0.to_string(),
                token1: token1.to_string(),
                token0_decimals: 6,
                token1_decimals: 6,
                gross_amount0: token(1_000_000),
                gross_amount1: token(1_000_000),
                vault_credit0: token(999_900),
                vault_credit1: token(1_000_000),
                minted_lp: token(999_000),
                min_liquidity_out: token(900_000),
                fields,
            },
            assertions: vec![assertion],
        };
        let authority = Pubkey::new_from_array([10; 32]);
        let treasury = Pubkey::new_from_array([11; 32]);
        let buybacks = Pubkey::new_from_array([12; 32]);
        let team = Pubkey::new_from_array([13; 32]);
        let configurations = vec![
            epoch(
                ConfigScope::ProgramBinary,
                "__program_binary__",
                BTreeMap::from([
                    ("source_git_commit".to_owned(), "c".repeat(40)),
                    ("idl_sha256".to_owned(), "a".repeat(64)),
                    ("candidate_program_binary_sha256".to_owned(), "b".repeat(64)),
                    ("program_binary_sha256".to_owned(), "b".repeat(64)),
                    ("provisional".to_owned(), "false".to_owned()),
                ]),
            ),
            epoch(
                ConfigScope::ProtocolRevenue,
                GLOBAL_CONFIG_ENTITY,
                BTreeMap::from([
                    ("authority".to_owned(), authority.to_string()),
                    ("swap_bps".to_owned(), "2000".to_owned()),
                    ("interest_bps".to_owned(), "1000".to_owned()),
                    ("futarchy_treasury_bps".to_owned(), "4000".to_owned()),
                    ("buybacks_vault_bps".to_owned(), "3000".to_owned()),
                    ("team_treasury_bps".to_owned(), "3000".to_owned()),
                ]),
            ),
            epoch(
                ConfigScope::RevenueRecipients,
                GLOBAL_CONFIG_ENTITY,
                BTreeMap::from([
                    ("futarchy_treasury".to_owned(), treasury.to_string()),
                    ("buybacks_vault".to_owned(), buybacks.to_string()),
                    ("team_treasury".to_owned(), team.to_string()),
                ]),
            ),
            epoch(
                ConfigScope::ReduceOnly,
                GLOBAL_CONFIG_ENTITY,
                BTreeMap::from([("reduce_only".to_owned(), "false".to_owned())]),
            ),
        ];
        (operation, configurations)
    }

    fn global_config_operation(
        slot: u64,
        kind: &str,
        reduce_only: Option<bool>,
    ) -> CanonicalOperationV1 {
        let mut fields = BTreeMap::from([("instruction_kind".to_owned(), kind.to_owned())]);
        if let Some(reduce_only) = reduce_only {
            fields.insert("args.reduce_only".to_owned(), reduce_only.to_string());
        }
        CanonicalOperationV1 {
            schema_version: 1,
            operation_id: format!("{kind}-{slot}"),
            signature: format!("{kind}-signature-{slot}"),
            pair: GLOBAL_CONFIG_ENTITY.to_owned(),
            actor: None,
            order: order(slot, 0),
            block_time_unix_seconds: 1_700_000_000 + i64::try_from(slot).unwrap(),
            decoder_version_id: "verified-v1".to_owned(),
            payload: OperationPayloadV1::UpdateConfig { fields },
            assertions: Vec::new(),
        }
    }

    fn set_reduce_only_epochs(
        configurations: &mut Vec<ConfigVersionV1>,
        transitions: &[(ChainOrder, bool)],
    ) {
        configurations.retain(|epoch| epoch.scope != ConfigScope::ReduceOnly);
        let mut epochs = Vec::with_capacity(transitions.len() + 1);
        epochs.push((order(0, 0), false));
        epochs.extend_from_slice(transitions);
        for (index, (valid_from, reduce_only)) in epochs.iter().enumerate() {
            configurations.push(ConfigVersionV1 {
                config_id: format!("reduce-only-{index}"),
                scope: ConfigScope::ReduceOnly,
                entity_address: GLOBAL_CONFIG_ENTITY.to_owned(),
                valid_from: *valid_from,
                valid_to_exclusive: epochs.get(index + 1).map(|(order, _)| *order),
                source_signature: format!("reduce-only-source-{index}"),
                decoder_version_id: "verified-v1".to_owned(),
                fields: BTreeMap::from([("reduce_only".to_owned(), reduce_only.to_string())]),
            });
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "dusk-v1-reconciliation-{label}-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn write_fixture_json_lines<T: Serialize>(path: &Path, rows: &[T]) {
        let mut writer = BufWriter::new(File::create(path).unwrap());
        for row in rows {
            serde_json::to_writer(&mut writer, row).unwrap();
            writer.write_all(b"\n").unwrap();
        }
        writer.flush().unwrap();
    }

    #[derive(Default)]
    struct InitializationObserver {
        initialized_authorities: Vec<V1AuthorityCheckpointV1>,
    }

    impl V1HistoricalStateObserverV1 for InitializationObserver {
        fn observe_pre_operation(
            &mut self,
            operation: &CanonicalOperationV1,
            _market: &str,
            engine: Option<&V1NativeEngine>,
            broadcast_global_config: bool,
        ) -> Result<()> {
            if matches!(
                operation.payload,
                OperationPayloadV1::InitializeMarket { .. }
            ) {
                assert!(engine.is_none());
                assert!(!broadcast_global_config);
            }
            Ok(())
        }

        fn observe_post_operation(
            &mut self,
            operation: &CanonicalOperationV1,
            _market: &str,
            engine: &V1NativeEngine,
            broadcast_global_config: bool,
        ) -> Result<()> {
            if matches!(
                operation.payload,
                OperationPayloadV1::InitializeMarket { .. }
            ) {
                assert!(!broadcast_global_config);
                self.initialized_authorities
                    .push(engine.checkpoint().authority);
            }
            Ok(())
        }
    }

    #[test]
    fn pre_market_global_update_is_staged_for_later_initialization() {
        let (initialization, mut configurations) = fixture();
        let authority_seed = global_config_operation(70, "init_futarchy_authority", None);
        let reduce_only = global_config_operation(80, "set_global_reduce_only", Some(true));
        set_reduce_only_epochs(&mut configurations, &[(reduce_only.order, true)]);
        let operations = vec![authority_seed, reduce_only, initialization];
        let directory = TestDirectory::new("pre-market-global-update");
        let operations_path = directory.path("operations.jsonl");
        let configurations_path = directory.path("configurations.jsonl");
        let ledger_path = directory.path("ledger.jsonl");
        write_fixture_json_lines(&operations_path, &operations);
        write_fixture_json_lines(&configurations_path, &configurations);

        let run = reconcile_history_to_ledger_with_terminal(
            &operations_path,
            &configurations_path,
            &ledger_path,
            "fixture-run",
            100,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .unwrap();

        assert_eq!(run.summary.canonical_operation_count, 3);
        assert_eq!(run.summary.replayed_transition_count, 1);
        assert_eq!(run.summary.authority_seed_operation_count, 1);
        assert_eq!(run.summary.broadcast_global_config_applications, 0);
        assert_eq!(
            read_json_lines::<V1ReplayLedgerRowV1>(&ledger_path)
                .unwrap()
                .len(),
            1
        );
        assert!(
            run.terminal_checkpoints
                .values()
                .next()
                .unwrap()
                .authority
                .global_reduce_only
        );
    }

    #[test]
    fn pre_market_true_then_false_uses_final_epoch_in_observed_initialization() {
        let (initialization, mut configurations) = fixture();
        let authority_seed = global_config_operation(70, "init_futarchy_authority", None);
        let reduce_only_true = global_config_operation(80, "set_global_reduce_only", Some(true));
        let reduce_only_false = global_config_operation(90, "set_global_reduce_only", Some(false));
        set_reduce_only_epochs(
            &mut configurations,
            &[
                (reduce_only_true.order, true),
                (reduce_only_false.order, false),
            ],
        );
        let operations = vec![
            authority_seed,
            reduce_only_true,
            reduce_only_false,
            initialization,
        ];
        let directory = TestDirectory::new("pre-market-global-update-sequence");
        let operations_path = directory.path("operations.jsonl");
        let configurations_path = directory.path("configurations.jsonl");
        write_fixture_json_lines(&operations_path, &operations);
        write_fixture_json_lines(&configurations_path, &configurations);
        let mut observer = InitializationObserver::default();

        let summary = traverse_v1_history_observed(
            &operations_path,
            &configurations_path,
            100,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
            &mut observer,
        )
        .unwrap();

        assert_eq!(summary.canonical_operation_count, 4);
        assert_eq!(summary.replayed_transition_count, 1);
        assert_eq!(summary.market_count, 1);
        assert_eq!(observer.initialized_authorities.len(), 1);
        assert!(!observer.initialized_authorities[0].global_reduce_only);
    }

    #[test]
    fn decision_grade_canonical_join_resolves_explicit_null_to_revision_default() {
        let (operation, configurations) = fixture();
        let input = canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .unwrap();
        assert_eq!(input.initial_rate_bps, DEFAULT_INITIAL_RATE_BPS);
        assert_eq!(input.fixed_cf_bps, None);
        assert_eq!(
            input.rate_model_address_policy,
            V1RateModelAddressPolicyV1::PairDerivedPda
        );
        assert_eq!(
            input.collateral_factor_transition_policy,
            V1CollateralFactorTransitionPolicyV1::SourceNative
        );
        assert_eq!(
            input.post_state_correction_policy,
            V1PostStateCorrectionPolicyV1::SourceNative
        );
        let (_, record) =
            V1FactualReplay::from_canonical_initialization(&operation, input).unwrap();
        assert!(record
            .reconciliations
            .iter()
            .all(|item| item.raw_unit_difference == "0"));
        assert!(record.conservation.iter().all(|item| item.holds));
    }

    #[test]
    fn factual_record_commitment_stringifies_nonempty_position_map_keys() {
        let (operation, configurations) = fixture();
        let input = canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .unwrap();
        let checkpoint_after = V1NativeEngine::from_canonical_initialization(input)
            .unwrap()
            .checkpoint();
        let mut record = V1FactualOperationRecordV1 {
            schema_version: crate::factual_replay::FACTUAL_REPLAY_SCHEMA_VERSION,
            operation_id: operation.operation_id.clone(),
            order: operation.order,
            checkpoint_before: None,
            checkpoint_after,
            reconciliations: Vec::new(),
            conservation: Vec::new(),
            revenue_delta: V1RevenueDeltaV1::default(),
            corrections: Vec::new(),
            retained_event_limitations: Vec::new(),
            behavior: None,
        };
        let position_key = Pubkey::new_from_array([77; 32]);
        record.checkpoint_after.positions.insert(
            position_key,
            V1PositionCheckpointV1 {
                owner: Pubkey::new_from_array([78; 32]),
                pair: record.checkpoint_after.pair_key,
                collateral0_liquidation_cf_bps: 8_500,
                collateral1_liquidation_cf_bps: 0,
                collateral0: 500_000_000,
                collateral1: 0,
                debt0_shares: 0,
                debt1_shares: 0,
                bump: 255,
            },
        );

        let committed = canonical_factual_record_sha256(&record).unwrap();
        assert_eq!(committed.len(), 64);
        let ledger_row = compact_row(&operation, &record, false).unwrap();
        assert_eq!(ledger_row.factual_record_sha256, committed);

        record
            .checkpoint_after
            .positions
            .get_mut(&position_key)
            .unwrap()
            .collateral0 += 1;
        assert_ne!(canonical_factual_record_sha256(&record).unwrap(), committed);
    }

    #[test]
    fn canonical_join_preserves_legacy_config_pubkey_bytes_exactly() {
        fn legacy_pubkey(pubkey: Pubkey) -> String {
            serde_json::to_string(
                &pubkey
                    .to_bytes()
                    .iter()
                    .map(u8::to_string)
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        }

        let (operation, mut configurations) = fixture();
        let mut expected = BTreeMap::new();
        for epoch in &mut configurations {
            let keys: &[&str] = match epoch.scope {
                ConfigScope::ProtocolRevenue => &["authority"],
                ConfigScope::RevenueRecipients => {
                    &["futarchy_treasury", "buybacks_vault", "team_treasury"]
                }
                _ => continue,
            };
            for key in keys {
                let pubkey = Pubkey::from_str(&epoch.fields[*key]).unwrap();
                expected.insert((*key).to_owned(), pubkey);
                epoch
                    .fields
                    .insert((*key).to_owned(), legacy_pubkey(pubkey));
            }
        }

        let input = canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .unwrap();
        assert_eq!(input.authority.authority, expected["authority"]);
        assert_eq!(
            input.authority.futarchy_treasury,
            expected["futarchy_treasury"]
        );
        assert_eq!(input.authority.buybacks_vault, expected["buybacks_vault"]);
        assert_eq!(input.authority.team_treasury, expected["team_treasury"]);

        configurations
            .iter_mut()
            .find(|epoch| epoch.scope == ConfigScope::ProtocolRevenue)
            .unwrap()
            .fields
            .insert("authority".to_owned(), "[\"1\"]".to_owned());
        assert!(canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .is_err());
    }

    #[test]
    fn provisional_binary_requires_explicit_decision_grade_policy() {
        let (operation, mut configurations) = fixture();
        let program = configurations
            .iter_mut()
            .find(|epoch| epoch.scope == ConfigScope::ProgramBinary)
            .unwrap();
        program
            .fields
            .insert("provisional".to_owned(), "true".to_owned());
        program.fields.remove("program_binary_sha256");
        assert!(canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::AuditGrade,
        )
        .is_err());
        let input = canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .unwrap();
        assert_eq!(
            input.rate_model_address_policy,
            V1RateModelAddressPolicyV1::HistoricalExplicitInitAccount
        );
        assert_eq!(
            input.collateral_factor_transition_policy,
            V1CollateralFactorTransitionPolicyV1::EventWitnessedCollateralFactorTransition
        );
        assert_eq!(
            input.post_state_correction_policy,
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        );
    }

    #[test]
    fn decision_grade_still_requires_matching_hashed_revision_metadata() {
        let (operation, mut configurations) = fixture();
        let program = configurations
            .iter_mut()
            .find(|epoch| epoch.scope == ConfigScope::ProgramBinary)
            .unwrap();
        program
            .fields
            .insert("provisional".to_owned(), "true".to_owned());
        program.fields.insert(
            "candidate_program_binary_sha256".to_owned(),
            "unresolved".to_owned(),
        );
        assert!(canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .is_err());

        let (operation, mut configurations) = fixture();
        let program = configurations
            .iter_mut()
            .find(|epoch| epoch.scope == ConfigScope::ProgramBinary)
            .unwrap();
        program
            .fields
            .insert("provisional".to_owned(), "true".to_owned());
        program.decoder_version_id = "another-decoder".to_owned();
        assert!(canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .is_err());
    }

    #[test]
    fn deployed_hash_is_optional_but_never_accepts_a_sentinel() {
        let (operation, mut configurations) = fixture();
        configurations
            .iter_mut()
            .find(|epoch| epoch.scope == ConfigScope::ProgramBinary)
            .unwrap()
            .fields
            .remove("program_binary_sha256");
        assert!(canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .is_ok());

        configurations
            .iter_mut()
            .find(|epoch| epoch.scope == ConfigScope::ProgramBinary)
            .unwrap()
            .fields
            .insert("program_binary_sha256".to_owned(), "unresolved".to_owned());
        assert!(canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
        )
        .is_err());
    }

    #[test]
    fn audit_grade_fails_closed_until_historical_dispatch_is_real() {
        let (operation, configurations) = fixture();
        assert!(!HISTORICAL_DECODER_DISPATCH_VERIFIED);
        let error = canonical_initialization_input(
            &operation,
            &configurations,
            V1ReplayCertificationPolicyV1::AuditGrade,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("verified historical decoder dispatch"));
    }
}

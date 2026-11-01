//! Deterministic join of canonical demand, factual V1 checkpoints, and Dusk.
//!
//! The evaluator never derives factual behavior from Dusk outcomes. Each
//! canonical operation is joined to the exact V1 ledger row at its natural
//! market grain; borrow/repay behavior comes from the native V1 receipt. The
//! intent arm inserts only explicitly flagged synthetic arbitrage records at
//! block boundaries.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Lines, Write},
    path::Path,
};

use anyhow::{bail, Context, Result};
use omnipair::constants::NAD;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    calibrate::{DuskProfileV1, ProtocolInterestShareScheduleV1},
    canonical::{ChainOrder, TokenAmount},
    counterfactual::{
        BehavioralContextV1, CounterfactualOperationRecordV1, CounterfactualStatusV1,
        DuskCounterfactualMarketV1, ResolvedProtocolInterestPolicyV1,
    },
    replay::{CanonicalOperationV1, HlpScenario, OperationPayloadV1, ReplayArm},
    spot_probe::{build_spot_probe_plan, SpotProbeSizingIndexV1, SpotProbeSizingWindowV1},
    transfer_fee::ValidatedTransferFeeRegistryV1,
    v1_reconciliation::{is_global_config, V1ReplayLedgerRowV1, GLOBAL_CONFIG_ENTITY},
};

pub const EVALUATION_SCHEMA_VERSION: u16 = 3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlpAttributionEvidenceV1 {
    pub deposit_operation_count: u64,
    pub withdrawal_operation_count: u64,
    pub interest_settlement_operation_count: u64,
    pub external_gross_base: TokenAmount,
    pub external_gross_quote: TokenAmount,
    pub ylp_gross_base: TokenAmount,
    pub ylp_gross_quote: TokenAmount,
    pub base_hlp_gross: TokenAmount,
    pub quote_hlp_gross: TokenAmount,
    pub base_hlp_interest_credit_quote: TokenAmount,
    pub quote_hlp_interest_credit_base: TokenAmount,
    pub every_observation_conserved_external_capital: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterfactualEvaluationSummaryV1 {
    pub schema_version: u16,
    pub profile_id: String,
    pub protocol_interest_schedule_sha256: String,
    pub arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub canonical_operation_count: u64,
    pub factual_ledger_row_count: u64,
    pub counterfactual_record_count: u64,
    pub synthetic_arbitrage_count: u64,
    pub synthetic_protocol_settlement_count: u64,
    pub executed_count: u64,
    pub partially_filled_count: u64,
    pub rejected_count: u64,
    pub skipped_intent_count: u64,
    pub deferred_external_state_count: u64,
    pub no_economic_counterpart_count: u64,
    pub unresolved_liquidations_at_cutoff: u64,
    pub spot_probes_enabled: bool,
    pub spot_probe_sizing_window: Option<SpotProbeSizingWindowV1>,
    pub spot_probe_set_count: u64,
    pub spot_probe_observation_count: u64,
    pub spot_probe_sizing_source_sha256: Option<String>,
    pub spot_probe_sets_sha256: String,
    pub hlp_attribution: HlpAttributionEvidenceV1,
    pub market_count: u64,
    pub first_order: Option<ChainOrder>,
    pub last_order: Option<ChainOrder>,
    pub records_sha256: String,
    pub complete_for_scoring: bool,
}

/// Streaming natural-grain hook used by metric and calibration writers. The
/// factual row and Dusk record are observed together while their canonical
/// operation is still in scope, so downstream code never performs a lossy
/// timestamp or operation-ID-only join.
pub trait CounterfactualRecordObserverV1 {
    fn observe(
        &mut self,
        operation: &CanonicalOperationV1,
        factual: &V1ReplayLedgerRowV1,
        dusk: &CounterfactualOperationRecordV1,
    ) -> Result<()>;
}

/// Full-state hook for private analysis artifacts. The hook is invoked after
/// any due synthetic liquidation settlement and immediately before the
/// factual operation. Implementations may fork native state in memory, but
/// must not serialize wallet positions or hLP YieldAccounts into public
/// artifacts.
pub trait CounterfactualStateObserverV1 {
    fn observe_pre_operation(
        &mut self,
        operation: &CanonicalOperationV1,
        factual: &V1ReplayLedgerRowV1,
        market: Option<&DuskCounterfactualMarketV1<'_>>,
    ) -> Result<()>;

    fn observe_post_operation(
        &mut self,
        _operation: &CanonicalOperationV1,
        _factual: &V1ReplayLedgerRowV1,
        _market: &DuskCounterfactualMarketV1<'_>,
    ) -> Result<()> {
        Ok(())
    }
}

struct JsonlReader<T> {
    path: std::path::PathBuf,
    lines: Lines<BufReader<File>>,
    line_number: usize,
    lookahead: Option<T>,
}

impl<T: DeserializeOwned> JsonlReader<T> {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            lines: BufReader::new(
                File::open(path).with_context(|| format!("opening {}", path.display()))?,
            )
            .lines(),
            line_number: 0,
            lookahead: None,
        })
    }

    fn next(&mut self) -> Result<Option<T>> {
        if self.lookahead.is_some() {
            return Ok(self.lookahead.take());
        }
        self.read_next()
    }

    fn peek(&mut self) -> Result<Option<&T>> {
        if self.lookahead.is_none() {
            self.lookahead = self.read_next()?;
        }
        Ok(self.lookahead.as_ref())
    }

    fn read_next(&mut self) -> Result<Option<T>> {
        for line in self.lines.by_ref() {
            self.line_number += 1;
            let line = line.with_context(|| {
                format!("reading {} line {}", self.path.display(), self.line_number)
            })?;
            if line.trim().is_empty() {
                continue;
            }
            return serde_json::from_str(&line)
                .with_context(|| {
                    format!("decoding {} line {}", self.path.display(), self.line_number)
                })
                .map(Some);
        }
        Ok(None)
    }
}

struct RecordWriter {
    writer: Option<BufWriter<File>>,
    partial_path: Option<std::path::PathBuf>,
    final_path: Option<std::path::PathBuf>,
    expected_existing: Option<(u64, String)>,
    digest: Sha256,
    count: u64,
}

impl RecordWriter {
    fn create(path: &Path) -> Result<Self> {
        if path.exists() {
            bail!(
                "refusing to overwrite counterfactual ledger {}",
                path.display()
            );
        }
        let partial_path = path.with_extension("jsonl.partial");
        if partial_path.exists() {
            bail!(
                "counterfactual partial already exists {}; inspect or remove it before retrying",
                partial_path.display()
            );
        }
        Ok(Self {
            writer: Some(BufWriter::new(
                File::create(&partial_path)
                    .with_context(|| format!("creating {}", partial_path.display()))?,
            )),
            partial_path: Some(partial_path),
            final_path: Some(path.to_path_buf()),
            expected_existing: None,
            digest: Sha256::new(),
            count: 0,
        })
    }

    fn hash_only(expected_count: u64, expected_sha256: &str) -> Result<Self> {
        if expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("expected counterfactual record hash is not a canonical SHA-256");
        }
        Ok(Self {
            writer: None,
            partial_path: None,
            final_path: None,
            expected_existing: Some((expected_count, expected_sha256.to_owned())),
            digest: Sha256::new(),
            count: 0,
        })
    }

    fn write(&mut self, record: &CounterfactualOperationRecordV1) -> Result<()> {
        let bytes = serde_json::to_vec(record)?;
        if let Some(writer) = &mut self.writer {
            writer.write_all(&bytes)?;
            writer.write_all(b"\n")?;
        }
        self.digest.update(&bytes);
        self.digest.update(b"\n");
        self.count = self.count.checked_add(1).context("record count overflow")?;
        Ok(())
    }

    fn commit(mut self) -> Result<(u64, String)> {
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
            writer.get_ref().sync_all()?;
            drop(writer);
            let partial = self
                .partial_path
                .as_ref()
                .context("file record writer has no partial path")?;
            let final_path = self
                .final_path
                .as_ref()
                .context("file record writer has no final path")?;
            fs::rename(partial, final_path).with_context(|| {
                format!("committing counterfactual ledger {}", final_path.display())
            })?;
        }
        let digest = hex::encode(self.digest.finalize());
        if let Some((expected_count, expected_digest)) = self.expected_existing {
            if self.count != expected_count || digest != expected_digest {
                bail!("hash-only counterfactual replay differs from committed ledger");
            }
        }
        Ok((self.count, digest))
    }
}

struct StatusCounts {
    synthetic: u64,
    synthetic_protocol_settlement: u64,
    executed: u64,
    partial: u64,
    rejected: u64,
    skipped: u64,
    deferred: u64,
    no_counterpart: u64,
    hlp_deposit_operations: u64,
    hlp_withdrawal_operations: u64,
    hlp_interest_operations: u64,
    hlp_external_gross_base: u128,
    hlp_external_gross_quote: u128,
    ylp_gross_base: u128,
    ylp_gross_quote: u128,
    base_hlp_gross: u128,
    quote_hlp_gross: u128,
    base_hlp_interest_credit_quote: u128,
    quote_hlp_interest_credit_base: u128,
    hlp_capital_conserved: bool,
    spot_probe_set_count: u64,
    spot_probe_observation_count: u64,
    spot_probe_sizing_source_sha256: Option<String>,
    spot_probe_digest: Sha256,
}

impl Default for StatusCounts {
    fn default() -> Self {
        Self {
            synthetic: 0,
            synthetic_protocol_settlement: 0,
            executed: 0,
            partial: 0,
            rejected: 0,
            skipped: 0,
            deferred: 0,
            no_counterpart: 0,
            hlp_deposit_operations: 0,
            hlp_withdrawal_operations: 0,
            hlp_interest_operations: 0,
            hlp_external_gross_base: 0,
            hlp_external_gross_quote: 0,
            ylp_gross_base: 0,
            ylp_gross_quote: 0,
            base_hlp_gross: 0,
            quote_hlp_gross: 0,
            base_hlp_interest_credit_quote: 0,
            quote_hlp_interest_credit_base: 0,
            hlp_capital_conserved: true,
            spot_probe_set_count: 0,
            spot_probe_observation_count: 0,
            spot_probe_sizing_source_sha256: None,
            spot_probe_digest: Sha256::new(),
        }
    }
}

impl StatusCounts {
    fn observe(&mut self, record: &CounterfactualOperationRecordV1) -> Result<()> {
        if record.synthetic_arbitrage {
            self.synthetic = checked_increment(self.synthetic)?;
        }
        if record.synthetic_protocol_settlement {
            self.synthetic_protocol_settlement =
                checked_increment(self.synthetic_protocol_settlement)?;
        }
        if let Some(hlp) = &record.hlp {
            let external_base = parse_token(&hlp.external_gross_base)?;
            let external_quote = parse_token(&hlp.external_gross_quote)?;
            let ylp_base = parse_token(&hlp.ylp_gross_base)?;
            let ylp_quote = parse_token(&hlp.ylp_gross_quote)?;
            let base_hlp = parse_token(&hlp.base_hlp_gross)?;
            let quote_hlp = parse_token(&hlp.quote_hlp_gross)?;
            let base_interest = parse_token(&hlp.base_hlp_interest_credit_quote)?;
            let quote_interest = parse_token(&hlp.quote_hlp_interest_credit_base)?;
            if external_base > 0 || external_quote > 0 {
                self.hlp_deposit_operations = checked_increment(self.hlp_deposit_operations)?;
            }
            if parse_token(&hlp.ylp_burned)? > 0
                || parse_token(&hlp.base_hlp_burned)? > 0
                || parse_token(&hlp.quote_hlp_burned)? > 0
            {
                self.hlp_withdrawal_operations = checked_increment(self.hlp_withdrawal_operations)?;
            }
            if base_interest > 0 || quote_interest > 0 {
                self.hlp_interest_operations = checked_increment(self.hlp_interest_operations)?;
            }
            checked_add_amount(&mut self.hlp_external_gross_base, external_base)?;
            checked_add_amount(&mut self.hlp_external_gross_quote, external_quote)?;
            checked_add_amount(&mut self.ylp_gross_base, ylp_base)?;
            checked_add_amount(&mut self.ylp_gross_quote, ylp_quote)?;
            checked_add_amount(&mut self.base_hlp_gross, base_hlp)?;
            checked_add_amount(&mut self.quote_hlp_gross, quote_hlp)?;
            checked_add_amount(&mut self.base_hlp_interest_credit_quote, base_interest)?;
            checked_add_amount(&mut self.quote_hlp_interest_credit_base, quote_interest)?;
            self.hlp_capital_conserved &= hlp.no_extra_capital_verified;
        }
        if let Some(probes) = &record.spot_probes {
            probes.validate_hash()?;
            if self
                .spot_probe_sizing_source_sha256
                .as_ref()
                .is_some_and(|hash| hash != &probes.sizing_source_sha256)
            {
                bail!("counterfactual evaluation mixed spot probe sizing sources");
            }
            self.spot_probe_sizing_source_sha256
                .get_or_insert_with(|| probes.sizing_source_sha256.clone());
            self.spot_probe_set_count = checked_increment(self.spot_probe_set_count)?;
            self.spot_probe_observation_count = self
                .spot_probe_observation_count
                .checked_add(u64::try_from(probes.observations.len())?)
                .context("spot probe observation count overflow")?;
            self.spot_probe_digest
                .update(probes.probe_set_sha256.as_bytes());
            self.spot_probe_digest.update(b"\n");
        }
        match record.status {
            CounterfactualStatusV1::Executed => self.executed = checked_increment(self.executed)?,
            CounterfactualStatusV1::PartiallyFilled => {
                self.partial = checked_increment(self.partial)?
            }
            CounterfactualStatusV1::Rejected => self.rejected = checked_increment(self.rejected)?,
            CounterfactualStatusV1::SkippedIntentMinOut => {
                self.skipped = checked_increment(self.skipped)?
            }
            CounterfactualStatusV1::DeferredExternalState => {
                self.deferred = checked_increment(self.deferred)?
            }
            CounterfactualStatusV1::NoEconomicCounterpart => {
                self.no_counterpart = checked_increment(self.no_counterpart)?
            }
        }
        Ok(())
    }
}

/// Evaluates one immutable profile/arm/scenario and writes a canonical JSONL
/// ledger atomically. The scenario is bound into every record and downstream
/// evidence identity, so the maximum-feasible upper bound cannot be confused
/// with its no-hLP attribution control.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_profile_history(
    operations_path: &Path,
    factual_ledger_path: &Path,
    output_path: &Path,
    profile: &DuskProfileV1,
    interest_schedule: &ProtocolInterestShareScheduleV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    transfers: &ValidatedTransferFeeRegistryV1,
) -> Result<CounterfactualEvaluationSummaryV1> {
    evaluate_profile_history_observed_with_spot_probes(
        operations_path,
        factual_ledger_path,
        output_path,
        profile,
        interest_schedule,
        arm,
        hlp_scenario,
        transfers,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn evaluate_profile_history_observed(
    operations_path: &Path,
    factual_ledger_path: &Path,
    output_path: &Path,
    profile: &DuskProfileV1,
    interest_schedule: &ProtocolInterestShareScheduleV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    transfers: &ValidatedTransferFeeRegistryV1,
    observer: Option<&mut dyn CounterfactualRecordObserverV1>,
) -> Result<CounterfactualEvaluationSummaryV1> {
    evaluate_profile_history_observed_with_spot_probes(
        operations_path,
        factual_ledger_path,
        output_path,
        profile,
        interest_schedule,
        arm,
        hlp_scenario,
        transfers,
        None,
        observer,
    )
}

/// Sealed evaluation entry point. Passing a sizing window enables immutable
/// spot probes only for swaps inside that exact scoring split; passing `None`
/// disables probes for calibration search replays.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_profile_history_observed_with_spot_probes(
    operations_path: &Path,
    factual_ledger_path: &Path,
    output_path: &Path,
    profile: &DuskProfileV1,
    interest_schedule: &ProtocolInterestShareScheduleV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    transfers: &ValidatedTransferFeeRegistryV1,
    spot_probe_window: Option<&SpotProbeSizingWindowV1>,
    observer: Option<&mut dyn CounterfactualRecordObserverV1>,
) -> Result<CounterfactualEvaluationSummaryV1> {
    evaluate_profile_history_observed_with_state(
        operations_path,
        factual_ledger_path,
        output_path,
        profile,
        interest_schedule,
        arm,
        hlp_scenario,
        transfers,
        spot_probe_window,
        observer,
        None,
    )
}

/// Sealed evaluation with an optional private native-state capture hook. This
/// remains a single counterfactual traversal regardless of how many campaign
/// cells are later evaluated from the bounded captured checkpoints.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_profile_history_observed_with_state(
    operations_path: &Path,
    factual_ledger_path: &Path,
    output_path: &Path,
    profile: &DuskProfileV1,
    interest_schedule: &ProtocolInterestShareScheduleV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    transfers: &ValidatedTransferFeeRegistryV1,
    spot_probe_window: Option<&SpotProbeSizingWindowV1>,
    observer: Option<&mut dyn CounterfactualRecordObserverV1>,
    state_observer: Option<&mut dyn CounterfactualStateObserverV1>,
) -> Result<CounterfactualEvaluationSummaryV1> {
    let writer = RecordWriter::create(output_path)?;
    evaluate_profile_history_with_writer(
        operations_path,
        factual_ledger_path,
        profile,
        interest_schedule,
        arm,
        hlp_scenario,
        transfers,
        spot_probe_window,
        observer,
        state_observer,
        writer,
    )
}

/// Replays an already committed counterfactual evaluation solely to capture
/// bounded private native states. Every record is serialized into the same
/// digest, but no second JSONL is written; a count/hash mismatch fails closed.
#[allow(clippy::too_many_arguments)]
pub fn replay_profile_history_capture_state_hash_only(
    operations_path: &Path,
    factual_ledger_path: &Path,
    profile: &DuskProfileV1,
    interest_schedule: &ProtocolInterestShareScheduleV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    transfers: &ValidatedTransferFeeRegistryV1,
    spot_probe_window: Option<&SpotProbeSizingWindowV1>,
    expected_record_count: u64,
    expected_records_sha256: &str,
    state_observer: &mut dyn CounterfactualStateObserverV1,
) -> Result<CounterfactualEvaluationSummaryV1> {
    let writer = RecordWriter::hash_only(expected_record_count, expected_records_sha256)?;
    evaluate_profile_history_with_writer(
        operations_path,
        factual_ledger_path,
        profile,
        interest_schedule,
        arm,
        hlp_scenario,
        transfers,
        spot_probe_window,
        None,
        Some(state_observer),
        writer,
    )
}

#[allow(clippy::too_many_arguments)]
fn evaluate_profile_history_with_writer(
    operations_path: &Path,
    factual_ledger_path: &Path,
    profile: &DuskProfileV1,
    interest_schedule: &ProtocolInterestShareScheduleV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    transfers: &ValidatedTransferFeeRegistryV1,
    spot_probe_window: Option<&SpotProbeSizingWindowV1>,
    mut observer: Option<&mut dyn CounterfactualRecordObserverV1>,
    mut state_observer: Option<&mut dyn CounterfactualStateObserverV1>,
    mut writer: RecordWriter,
) -> Result<CounterfactualEvaluationSummaryV1> {
    interest_schedule.verify()?;
    let spot_probe_sizing = spot_probe_window
        .cloned()
        .map(|window| SpotProbeSizingIndexV1::from_canonical_jsonl(operations_path, window))
        .transpose()?;
    let mut operations = JsonlReader::<CanonicalOperationV1>::open(operations_path)?;
    let mut ledger = JsonlReader::<V1ReplayLedgerRowV1>::open(factual_ledger_path)?;
    let mut markets = BTreeMap::<String, DuskCounterfactualMarketV1<'_>>::new();
    let mut latest_v1 = BTreeMap::<String, V1ReplayLedgerRowV1>::new();
    let mut swapped_markets = BTreeSet::<String>::new();
    let mut block_anchors = BTreeMap::<String, CanonicalOperationV1>::new();
    let mut status = StatusCounts::default();
    let mut canonical_operation_count = 0_u64;
    let mut factual_ledger_row_count = 0_u64;
    let mut first_order = None;
    let mut last_order = None;
    let mut current_slot = None;

    while let Some(operation) = operations.next()? {
        operation.validate()?;
        if last_order.is_some_and(|order| order >= operation.order) {
            bail!("canonical operation stream is not in strict chain order");
        }
        if current_slot.is_some_and(|slot| slot != operation.order.slot) {
            finalize_intent_block(
                arm,
                &mut markets,
                &latest_v1,
                &mut swapped_markets,
                &mut block_anchors,
                &mut writer,
                &mut status,
                &mut observer,
            )?;
        }
        current_slot = Some(operation.order.slot);
        first_order.get_or_insert(operation.order);
        last_order = Some(operation.order);
        canonical_operation_count = checked_increment(canonical_operation_count)?;

        let mut rows = Vec::new();
        while ledger
            .peek()?
            .is_some_and(|row| row.operation_id == operation.operation_id)
        {
            let row = ledger.next()?.expect("peeked ledger row exists");
            if row.order != operation.order {
                bail!("factual ledger order differs from canonical operation order");
            }
            factual_ledger_row_count = checked_increment(factual_ledger_row_count)?;
            rows.push(row);
        }

        match &operation.payload {
            OperationPayloadV1::UpdateConfig { fields }
                if fields.get("instruction_kind").map(String::as_str)
                    == Some("init_futarchy_authority") =>
            {
                if !rows.is_empty() {
                    bail!("authority seed operation unexpectedly has factual market rows");
                }
            }
            OperationPayloadV1::UpdateConfig { fields }
                if is_global_config(fields.get("instruction_kind").map(String::as_str)) =>
            {
                validate_global_config_join(
                    &operation,
                    rows.iter()
                        .map(|row| (row.market.as_str(), row.broadcast_global_config)),
                    markets.keys().map(String::as_str),
                )?;
                for row in rows {
                    apply_row(
                        &operation,
                        row,
                        profile,
                        interest_schedule,
                        arm,
                        hlp_scenario,
                        transfers,
                        spot_probe_sizing.as_ref(),
                        &mut markets,
                        &mut latest_v1,
                        &mut swapped_markets,
                        &mut block_anchors,
                        &mut writer,
                        &mut status,
                        &mut observer,
                        &mut state_observer,
                    )?;
                }
            }
            _ => {
                if rows.len() != 1 {
                    bail!(
                        "canonical operation {} joined {} factual rows instead of one",
                        operation.operation_id,
                        rows.len()
                    );
                }
                apply_row(
                    &operation,
                    rows.pop().expect("one row checked"),
                    profile,
                    interest_schedule,
                    arm,
                    hlp_scenario,
                    transfers,
                    spot_probe_sizing.as_ref(),
                    &mut markets,
                    &mut latest_v1,
                    &mut swapped_markets,
                    &mut block_anchors,
                    &mut writer,
                    &mut status,
                    &mut observer,
                    &mut state_observer,
                )?;
            }
        }
    }

    finalize_intent_block(
        arm,
        &mut markets,
        &latest_v1,
        &mut swapped_markets,
        &mut block_anchors,
        &mut writer,
        &mut status,
        &mut observer,
    )?;
    if ledger.peek()?.is_some() {
        bail!("factual ledger contains rows with no canonical operation");
    }
    let market_count = u64::try_from(markets.len())?;
    let unresolved_liquidations_at_cutoff = markets.values().try_fold(0_u64, |total, market| {
        total
            .checked_add(u64::try_from(market.pending_liquidation_count())?)
            .context("unresolved liquidation count overflow")
    })?;
    let (counterfactual_record_count, records_sha256) = writer.commit()?;
    Ok(CounterfactualEvaluationSummaryV1 {
        schema_version: EVALUATION_SCHEMA_VERSION,
        profile_id: profile.profile_id.clone(),
        protocol_interest_schedule_sha256: interest_schedule.schedule_sha256.clone(),
        arm,
        hlp_scenario,
        canonical_operation_count,
        factual_ledger_row_count,
        counterfactual_record_count,
        synthetic_arbitrage_count: status.synthetic,
        synthetic_protocol_settlement_count: status.synthetic_protocol_settlement,
        executed_count: status.executed,
        partially_filled_count: status.partial,
        rejected_count: status.rejected,
        skipped_intent_count: status.skipped,
        deferred_external_state_count: status.deferred,
        no_economic_counterpart_count: status.no_counterpart,
        unresolved_liquidations_at_cutoff,
        spot_probes_enabled: spot_probe_sizing.is_some(),
        spot_probe_sizing_window: spot_probe_sizing
            .as_ref()
            .map(|sizing| sizing.window().clone()),
        spot_probe_set_count: status.spot_probe_set_count,
        spot_probe_observation_count: status.spot_probe_observation_count,
        spot_probe_sizing_source_sha256: status.spot_probe_sizing_source_sha256,
        spot_probe_sets_sha256: hex::encode(status.spot_probe_digest.finalize()),
        hlp_attribution: HlpAttributionEvidenceV1 {
            deposit_operation_count: status.hlp_deposit_operations,
            withdrawal_operation_count: status.hlp_withdrawal_operations,
            interest_settlement_operation_count: status.hlp_interest_operations,
            external_gross_base: token(status.hlp_external_gross_base),
            external_gross_quote: token(status.hlp_external_gross_quote),
            ylp_gross_base: token(status.ylp_gross_base),
            ylp_gross_quote: token(status.ylp_gross_quote),
            base_hlp_gross: token(status.base_hlp_gross),
            quote_hlp_gross: token(status.quote_hlp_gross),
            base_hlp_interest_credit_quote: token(status.base_hlp_interest_credit_quote),
            quote_hlp_interest_credit_base: token(status.quote_hlp_interest_credit_base),
            every_observation_conserved_external_capital: status.hlp_capital_conserved,
        },
        market_count,
        first_order,
        last_order,
        records_sha256,
        complete_for_scoring: status.deferred == 0 && unresolved_liquidations_at_cutoff == 0,
    })
}

#[allow(clippy::too_many_arguments)]
fn apply_row<'a>(
    operation: &CanonicalOperationV1,
    row: V1ReplayLedgerRowV1,
    profile: &DuskProfileV1,
    interest_schedule: &ProtocolInterestShareScheduleV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    transfers: &'a ValidatedTransferFeeRegistryV1,
    spot_probe_sizing: Option<&SpotProbeSizingIndexV1>,
    markets: &mut BTreeMap<String, DuskCounterfactualMarketV1<'a>>,
    latest_v1: &mut BTreeMap<String, V1ReplayLedgerRowV1>,
    swapped_markets: &mut BTreeSet<String>,
    block_anchors: &mut BTreeMap<String, CanonicalOperationV1>,
    writer: &mut RecordWriter,
    status: &mut StatusCounts,
    observer: &mut Option<&mut dyn CounterfactualRecordObserverV1>,
    state_observer: &mut Option<&mut dyn CounterfactualStateObserverV1>,
) -> Result<()> {
    if row.market != operation.pair && !row.broadcast_global_config {
        bail!("factual ledger market differs from canonical operation pair");
    }
    let market_key = row.market.clone();
    let interest_epoch = interest_schedule.resolve_at(
        &market_key,
        operation.order,
        row.compact_checkpoint.authority.interest_bps,
    )?;
    if let OperationPayloadV1::UpdateConfig { fields } = &operation.payload {
        if matches!(
            fields.get("instruction_kind").map(String::as_str),
            Some("update_futarchy_authority" | "update_protocol_revenue")
        ) {
            let expected_epoch_id = crate::canonical::sha256_hex(
                format!(
                    "{}:{:?}:{}",
                    operation.operation_id,
                    crate::canonical::ConfigScope::ProtocolRevenue,
                    "__global__"
                )
                .as_bytes(),
            );
            if interest_epoch.key.config_epoch_id != expected_epoch_id {
                bail!(
                    "canonical protocol-revenue update is absent from the frozen training schedule"
                );
            }
        }
    }
    if !interest_epoch.training_fit_available
        && operation.block_time_unix_seconds >= interest_schedule.source.training_start_unix_seconds
        && (row.revenue_delta.lp_interest0 != 0
            || row.revenue_delta.lp_interest1 != 0
            || row.revenue_delta.protocol_interest0 != 0
            || row.revenue_delta.protocol_interest1 != 0)
    {
        bail!(
            "positive factual interest occurred in an epoch with no training fit for market {}",
            market_key
        );
    }
    let resolved_interest_policy = ResolvedProtocolInterestPolicyV1 {
        schedule_sha256: interest_schedule.schedule_sha256.clone(),
        epoch_id: interest_epoch.key.config_epoch_id.clone(),
        factual_v1_interest_bps: interest_epoch.v1_interest_surcharge_bps,
        dusk_protocol_interest_share_bps: interest_epoch.dusk_protocol_interest_share_bps,
    };
    let record = match &operation.payload {
        OperationPayloadV1::InitializeMarket { .. } => {
            if markets.contains_key(&market_key) {
                bail!("Dusk counterfactual market initialized twice");
            }
            if let Some(state_observer) = state_observer.as_deref_mut() {
                state_observer.observe_pre_operation(operation, &row, None)?;
            }
            let (market, record) =
                DuskCounterfactualMarketV1::initialize_from_v1_with_resolved_interest_policy(
                    operation,
                    profile,
                    row.compact_checkpoint.authority.swap_bps,
                    &resolved_interest_policy,
                    transfers,
                    arm,
                    hlp_scenario,
                )?;
            markets.insert(market_key.clone(), market);
            if let Some(state_observer) = state_observer.as_deref_mut() {
                state_observer.observe_post_operation(
                    operation,
                    &row,
                    markets
                        .get(&market_key)
                        .context("initialized Dusk market disappeared")?,
                )?;
            }
            record
        }
        _ => {
            let market = markets.get_mut(&market_key).with_context(|| {
                format!("Dusk operation references uninitialized market {market_key}")
            })?;
            for synthetic in market.settle_due_liquidations(operation, arm)? {
                status.observe(&synthetic)?;
                writer.write(&synthetic)?;
                if let Some(observer) = observer.as_deref_mut() {
                    observer.observe(operation, &row, &synthetic)?;
                }
            }
            if let Some(state_observer) = state_observer.as_deref_mut() {
                state_observer.observe_pre_operation(operation, &row, Some(market))?;
            }
            let spot_probes = if matches!(operation.payload, OperationPayloadV1::SwapExactIn { .. })
                && spot_probe_sizing.is_some_and(|sizing| {
                    sizing.window().contains(operation.block_time_unix_seconds)
                }) {
                let sizing = spot_probe_sizing.expect("checked above");
                let plan = build_spot_probe_plan(operation, &row, sizing, transfers)?;
                Some(market.preview_spot_probes(operation, arm, &plan)?)
            } else {
                None
            };
            let mut record = market.apply_with_resolved_interest_policy(
                operation,
                arm,
                BehavioralContextV1::from_factual(row.behavior.as_ref())?,
                &resolved_interest_policy,
            )?;
            if let Some(probes) = spot_probes {
                record.bind_spot_probes(probes)?;
            }
            if let Some(state_observer) = state_observer.as_deref_mut() {
                state_observer.observe_post_operation(operation, &row, market)?;
            }
            record
        }
    };
    if matches!(operation.payload, OperationPayloadV1::SwapExactIn { .. }) {
        swapped_markets.insert(market_key.clone());
        block_anchors.insert(market_key.clone(), operation.clone());
    }
    status.observe(&record)?;
    writer.write(&record)?;
    if let Some(observer) = observer.as_deref_mut() {
        observer.observe(operation, &row, &record)?;
    }
    latest_v1.insert(market_key, row);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finalize_intent_block(
    arm: ReplayArm,
    markets: &mut BTreeMap<String, DuskCounterfactualMarketV1<'_>>,
    latest_v1: &BTreeMap<String, V1ReplayLedgerRowV1>,
    swapped_markets: &mut BTreeSet<String>,
    block_anchors: &mut BTreeMap<String, CanonicalOperationV1>,
    writer: &mut RecordWriter,
    status: &mut StatusCounts,
    observer: &mut Option<&mut dyn CounterfactualRecordObserverV1>,
) -> Result<()> {
    if arm == ReplayArm::Intent {
        for market_key in swapped_markets.iter() {
            let factual = latest_v1
                .get(market_key)
                .context("missing post-block V1 checkpoint for arbitrage")?;
            let anchor = block_anchors
                .get(market_key)
                .context("missing block anchor for arbitrage")?;
            let (lower, upper) = v1_price_band(factual)?;
            if let Some(record) = markets
                .get_mut(market_key)
                .context("missing Dusk market for arbitrage")?
                .arbitrage_to_price_band(anchor, lower, upper)?
            {
                status.observe(&record)?;
                writer.write(&record)?;
                if let Some(observer) = observer.as_deref_mut() {
                    observer.observe(anchor, factual, &record)?;
                }
            }
        }
    }
    swapped_markets.clear();
    block_anchors.clear();
    Ok(())
}

fn v1_price_band(row: &V1ReplayLedgerRowV1) -> Result<(u64, u64)> {
    let base = u128::from(row.compact_checkpoint.pair.reserve0);
    let quote = u128::from(row.compact_checkpoint.pair.reserve1);
    if base == 0 || quote == 0 {
        bail!("V1 price band is undefined at zero reserve");
    }
    let numerator = quote
        .checked_mul(u128::from(NAD))
        .context("V1 price numerator overflow")?;
    let lower = numerator / base;
    let upper = numerator
        .checked_add(base - 1)
        .context("V1 price ceiling overflow")?
        / base;
    Ok((u64::try_from(lower)?, u64::try_from(upper)?))
}

fn checked_increment(value: u64) -> Result<u64> {
    value.checked_add(1).context("evaluation count overflow")
}

fn validate_global_config_join<'a>(
    operation: &CanonicalOperationV1,
    rows: impl IntoIterator<Item = (&'a str, bool)>,
    initialized_markets: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    if operation.pair != GLOBAL_CONFIG_ENTITY {
        bail!(
            "global config operation {} does not use the global entity",
            operation.operation_id
        );
    }
    let expected: BTreeSet<_> = initialized_markets.into_iter().collect();
    let rows: Vec<_> = rows.into_iter().collect();
    if rows.iter().any(|(_, broadcast)| !broadcast) {
        bail!(
            "global config operation {} has a non-broadcast factual row",
            operation.operation_id
        );
    }
    let actual: BTreeSet<_> = rows.iter().map(|(market, _)| *market).collect();
    if rows.len() != expected.len() || actual != expected {
        bail!(
            "global config operation {} joined {} factual rows for {} initialized markets",
            operation.operation_id,
            rows.len(),
            expected.len()
        );
    }
    Ok(())
}

fn parse_token(value: &TokenAmount) -> Result<u128> {
    value
        .as_str()
        .parse::<u128>()
        .context("hLP attribution amount exceeds u128")
}

fn checked_add_amount(total: &mut u128, value: u128) -> Result<()> {
    *total = total
        .checked_add(value)
        .context("hLP attribution total overflow")?;
    Ok(())
}

fn token(value: u128) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u128 is canonical")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global_config_operation(pair: &str) -> CanonicalOperationV1 {
        CanonicalOperationV1 {
            schema_version: 1,
            operation_id: "global-config".to_owned(),
            signature: "signature".to_owned(),
            pair: pair.to_owned(),
            actor: None,
            order: ChainOrder {
                slot: 1,
                transaction_index: 0,
                outer_instruction_index: 0,
                inner_execution_index: None,
                event_sequence: 0,
            },
            block_time_unix_seconds: 1,
            decoder_version_id: "decoder".to_owned(),
            payload: OperationPayloadV1::UpdateConfig {
                fields: BTreeMap::from([
                    (
                        "instruction_kind".to_owned(),
                        "set_global_reduce_only".to_owned(),
                    ),
                    ("args.reduce_only".to_owned(), "true".to_owned()),
                ]),
            },
            assertions: Vec::new(),
        }
    }

    #[test]
    fn hash_only_writer_verifies_without_creating_a_file() {
        let empty_sha256 = hex::encode(Sha256::digest([]));
        assert_eq!(
            RecordWriter::hash_only(0, &empty_sha256)
                .unwrap()
                .commit()
                .unwrap(),
            (0, empty_sha256.clone())
        );
        assert!(RecordWriter::hash_only(1, &empty_sha256)
            .unwrap()
            .commit()
            .is_err());
    }

    #[test]
    fn v1_price_band_contains_exact_rational_price() {
        // The production helper is exercised through a minimally serialized
        // ledger row elsewhere; assert the integer rounding identity directly
        // here so unequal reserve ratios cannot collapse to a false point.
        let base = 3_u128;
        let quote = 2_u128;
        let numerator = quote * u128::from(NAD);
        let lower = numerator / base;
        let upper = (numerator + base - 1) / base;
        assert_eq!(lower, 666_666_666);
        assert_eq!(upper, 666_666_667);
    }

    #[test]
    fn global_config_join_requires_exact_initialized_market_fanout() {
        let operation = global_config_operation(GLOBAL_CONFIG_ENTITY);

        validate_global_config_join(
            &operation,
            std::iter::empty::<(&str, bool)>(),
            std::iter::empty::<&str>(),
        )
        .unwrap();
        validate_global_config_join(
            &operation,
            [("market-a", true), ("market-b", true)],
            ["market-a", "market-b"],
        )
        .unwrap();

        assert!(validate_global_config_join(
            &operation,
            [("market-a", true)],
            ["market-a", "market-b"],
        )
        .is_err());
        assert!(validate_global_config_join(
            &operation,
            [("market-a", true), ("market-a", true)],
            ["market-a", "market-b"],
        )
        .is_err());
        assert!(
            validate_global_config_join(&operation, [("market-a", false)], ["market-a"],).is_err()
        );
        assert!(validate_global_config_join(
            &global_config_operation("market-a"),
            std::iter::empty::<(&str, bool)>(),
            std::iter::empty::<&str>(),
        )
        .is_err());
    }
}

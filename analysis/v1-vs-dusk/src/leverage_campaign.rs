//! Hash-bound leverage campaign over bounded full native checkpoints.
//!
//! Public artifacts contain only market/order identities, native state
//! commitments, exact integer metrics, and typed availability. Complete V1
//! position maps and Dusk hLP YieldAccounts remain in memory inside the
//! builder and have no serialization implementation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use dusk::{
    benchmark_api::BenchmarkClock,
    constants::{BPS_DENOMINATOR, LEVERAGE_MAX_MULTIPLIER_BPS},
    state::MarketAsset,
};
use omnipair::constants::NAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;

use crate::{
    canonical::{ChainOrder, TokenAmount},
    counterfactual::{DuskCounterfactualMarketV1, DuskLeveragePrivateSnapshotV1},
    evaluation::{
        CounterfactualEvaluationSummaryV1, CounterfactualStateObserverV1, EVALUATION_SCHEMA_VERSION,
    },
    leverage::{
        benchmark_leverage_exact, AdversePathAvailabilityV1, AdversePriceMarkV1,
        LeverageBenchmarkError, LeverageBenchmarkInputV1, LeverageBenchmarkResultV1,
        LeverageDirectionV1, SourcedRealizedMaeV1, V1LoopClockV1, V1UnwindClockV1,
        LEVERAGE_BENCHMARK_SCHEMA_VERSION,
    },
    replay::{CanonicalOperationV1, HlpScenario, OperationPayloadV1, ReplayArm},
    transfer_fee::ValidatedTransferFeeRegistryV1,
    v1_engine::V1NativeEngine,
    v1_reconciliation::{
        canonical_terminal_checkpoint_sha256, traverse_v1_history_observed,
        V1HistoricalStateObserverV1, V1ObservedTraversalSummaryV1, V1ReplayCertificationPolicyV1,
        V1ReplayLedgerRowV1, V1_OBSERVED_TRAVERSAL_SCHEMA_VERSION,
    },
};

pub const LEVERAGE_CAMPAIGN_SCHEMA_VERSION: u16 = 2;
const SPEC_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:leverage-campaign-spec:v2";
const PLAN_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:leverage-campaign-plan:v1";
const CELL_ID_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:leverage-cell-id:v1";
const CELL_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:leverage-cell:v1";
const ARTIFACT_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:leverage-campaign-artifact:v2";
const PATH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:leverage-realized-path:v1";
const PRIVATE_CAPTURE_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:leverage-private-captures:v1";
pub const STANDARDIZED_MARGIN_NAV_BPS: [u16; 3] = [10, 100, 500];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeverageCheckpointKindV1 {
    First,
    Median,
    Last,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeverageUnwindHorizonV1 {
    /// Open and full unwind use the same exact pre-operation market state and
    /// same-slot clocks. No later liquidity is invented.
    ImmediateSameCheckpoint,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeverageMintEligibilityEvidenceV1 {
    pub mint: String,
    pub decoded_mint_sha256: String,
    pub native_leverage_eligible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeverageCampaignSpecV1 {
    pub schema_version: u16,
    pub profile_id: String,
    pub profile_freeze_sha256: String,
    pub operations_sha256: String,
    pub factual_ledger_sha256: String,
    pub configurations_sha256: String,
    pub transfer_fee_registry_sha256: String,
    /// Hash-bound factual replay policy. Decision-grade is permitted only when
    /// the pipeline has already verified the selected cohort and final gate.
    pub v1_replay_certification_policy: V1ReplayCertificationPolicyV1,
    pub holdout_start_unix_seconds: i64,
    pub holdout_end_unix_seconds_exclusive: i64,
    pub cutoff_slot: u64,
    pub usd_headline_markets: BTreeSet<String>,
    pub mint_eligibility: Vec<LeverageMintEligibilityEvidenceV1>,
    pub v1_open_loop_iteration_cap: u32,
    pub v1_unwind_iteration_cap: u32,
    pub spec_sha256: String,
}

impl LeverageCampaignSpecV1 {
    pub fn seal(mut self) -> Result<Self> {
        self.spec_sha256.clear();
        self.validate_fields()?;
        self.spec_sha256 = domain_hash(SPEC_DOMAIN, &self)?;
        Ok(self)
    }

    pub fn verify(&self) -> Result<()> {
        self.validate_fields()?;
        let mut expected = self.clone();
        expected.spec_sha256.clear();
        if domain_hash(SPEC_DOMAIN, &expected)? != self.spec_sha256 {
            bail!("leverage campaign spec hash mismatch");
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<()> {
        if self.schema_version != LEVERAGE_CAMPAIGN_SCHEMA_VERSION
            || !valid_sha256(&self.profile_id)
            || !valid_sha256(&self.profile_freeze_sha256)
            || !valid_sha256(&self.operations_sha256)
            || !valid_sha256(&self.factual_ledger_sha256)
            || !valid_sha256(&self.configurations_sha256)
            || !valid_sha256(&self.transfer_fee_registry_sha256)
            || self.holdout_start_unix_seconds >= self.holdout_end_unix_seconds_exclusive
            || self.usd_headline_markets.is_empty()
            || self.v1_open_loop_iteration_cap == 0
            || self.v1_unwind_iteration_cap == 0
        {
            bail!("invalid leverage campaign spec");
        }
        for market in &self.usd_headline_markets {
            Pubkey::from_str(market).context("invalid USD-headline market key")?;
        }
        let mut mints = BTreeSet::new();
        for evidence in &self.mint_eligibility {
            Pubkey::from_str(&evidence.mint).context("invalid leverage mint key")?;
            if !valid_sha256(&evidence.decoded_mint_sha256) || !mints.insert(evidence.mint.as_str())
            {
                bail!("invalid or duplicate leverage mint eligibility evidence");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeverageCampaignSelectionV1 {
    pub market: String,
    pub checkpoint_kind: LeverageCheckpointKindV1,
    pub operation_id: String,
    pub order: ChainOrder,
    pub block_time_unix_seconds: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeverageCampaignReplayEvidenceV1 {
    pub arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub evaluation_schema_version: u16,
    pub records_sha256: String,
    pub complete_for_scoring: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizedAdverseExcursionEvidenceV1 {
    pub mark_count: u64,
    pub first_slot: u64,
    pub last_slot: u64,
    pub entry_collateral_price_nad: TokenAmount,
    pub worst_collateral_price_nad: TokenAmount,
    pub adverse_excursion_bps: TokenAmount,
    pub path_end_unix_seconds_exclusive: i64,
    pub source_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeverageCampaignBlockerV1 {
    NoHoldoutOperation,
    MissingFullV1PreOperationState,
    MissingDuskPreOperationState,
    MissingMintEligibilityEvidence,
    NativeCollateralMintIneligible,
    ZeroOrOverflowingStartingMargin,
    FactualMarginalPricePathUnavailable,
    ExactDuskFuturePositionBranchUnavailable,
    V1OpenLoopIterationCapReached,
    V1UnwindIterationCapReached,
    DuskFullUnwindUnavailable,
    NativeDuskTransitionRejected,
    NativeV1TransitionRejected,
    HistoricalTransferFeeResolutionRejected,
    NativeBenchmarkInputRejected,
    NativeBenchmarkArithmeticRejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeverageCampaignMarketStatusV1 {
    pub market: String,
    pub factual_lending_activity: bool,
    pub selected_holdout_state_count: u64,
    pub blocker: Option<LeverageCampaignBlockerV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeverageCampaignCellV1 {
    pub schema_version: u16,
    pub cell_id: String,
    pub market: String,
    pub checkpoint_kind: LeverageCheckpointKindV1,
    pub operation_id: String,
    pub order: ChainOrder,
    pub block_time_unix_seconds: i64,
    pub profile_id: String,
    pub arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub direction: LeverageDirectionV1,
    pub starting_margin_nav_bps: u16,
    pub factual_accounted_pool_nav_in_debt_atoms: Option<TokenAmount>,
    pub starting_margin_gross: Option<TokenAmount>,
    pub v1_pre_state_sha256: Option<String>,
    pub dusk_pre_state_sha256: Option<String>,
    pub realized_adverse_excursion: Option<RealizedAdverseExcursionEvidenceV1>,
    pub dusk_adverse_path_availability: AdversePathAvailabilityV1,
    pub unwind_horizon: LeverageUnwindHorizonV1,
    pub core_benchmark_available: bool,
    pub complete_for_headline_path_claim: bool,
    pub blockers: Vec<LeverageCampaignBlockerV1>,
    pub benchmark: Option<LeverageBenchmarkResultV1>,
    pub cell_sha256: String,
}

impl LeverageCampaignCellV1 {
    fn seal(mut self) -> Result<Self> {
        self.cell_sha256.clear();
        self.cell_sha256 = domain_hash(CELL_DOMAIN, &self)?;
        Ok(self)
    }

    fn verify(&self) -> Result<()> {
        if self.schema_version != LEVERAGE_CAMPAIGN_SCHEMA_VERSION
            || !valid_sha256(&self.cell_id)
            || !valid_sha256(&self.cell_sha256)
            || self.profile_id.len() != 64
            || !STANDARDIZED_MARGIN_NAV_BPS.contains(&self.starting_margin_nav_bps)
            || self.core_benchmark_available != self.benchmark.is_some()
        {
            bail!("invalid leverage campaign cell");
        }
        if let Some(benchmark) = &self.benchmark {
            if benchmark.schema_version != LEVERAGE_BENCHMARK_SCHEMA_VERSION
                || benchmark.benchmark_id != self.cell_id
                || benchmark.direction != self.direction
                || benchmark.hlp_scenario != self.hlp_scenario
            {
                bail!("leverage campaign cell benchmark identity mismatch");
            }
        }
        let mut expected = self.clone();
        expected.cell_sha256.clear();
        if domain_hash(CELL_DOMAIN, &expected)? != self.cell_sha256 {
            bail!("leverage campaign cell hash mismatch");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeverageCampaignArtifactV1 {
    pub schema_version: u16,
    pub spec: LeverageCampaignSpecV1,
    pub plan_sha256: String,
    pub selection_policy: String,
    pub standardized_margin_nav_bps: Vec<u16>,
    pub factual_lending_markets: Vec<String>,
    pub non_lending_usd_headline_markets: Vec<String>,
    pub market_status: Vec<LeverageCampaignMarketStatusV1>,
    pub selections: Vec<LeverageCampaignSelectionV1>,
    pub v1_traversal: V1ObservedTraversalSummaryV1,
    pub dusk_replays: Vec<LeverageCampaignReplayEvidenceV1>,
    pub private_capture_set_sha256: String,
    pub cells: Vec<LeverageCampaignCellV1>,
    pub all_core_cells_available: bool,
    pub all_realized_dusk_paths_complete: bool,
    pub artifact_sha256: String,
}

impl LeverageCampaignArtifactV1 {
    fn seal(mut self) -> Result<Self> {
        self.artifact_sha256.clear();
        for cell in &self.cells {
            cell.verify()?;
        }
        self.artifact_sha256 = domain_hash(ARTIFACT_DOMAIN, &self)?;
        Ok(self)
    }

    pub fn verify(&self) -> Result<()> {
        if self.schema_version != LEVERAGE_CAMPAIGN_SCHEMA_VERSION
            || self.standardized_margin_nav_bps != STANDARDIZED_MARGIN_NAV_BPS
            || !valid_sha256(&self.plan_sha256)
            || !valid_sha256(&self.private_capture_set_sha256)
            || !valid_sha256(&self.artifact_sha256)
            || self.v1_traversal.schema_version != V1_OBSERVED_TRAVERSAL_SCHEMA_VERSION
        {
            bail!("invalid leverage campaign artifact");
        }
        self.spec.verify()?;
        let mut ids = BTreeSet::new();
        for cell in &self.cells {
            cell.verify()?;
            if !ids.insert(cell.cell_id.as_str()) {
                bail!("duplicate leverage campaign cell ID");
            }
        }
        if self.all_core_cells_available
            != self.cells.iter().all(|cell| cell.core_benchmark_available)
            || self.all_realized_dusk_paths_complete
                != self.cells.iter().all(|cell| {
                    cell.dusk_adverse_path_availability == AdversePathAvailabilityV1::Complete
                })
        {
            bail!("leverage campaign aggregate availability mismatch");
        }
        let mut expected = self.clone();
        expected.artifact_sha256.clear();
        if domain_hash(ARTIFACT_DOMAIN, &expected)? != self.artifact_sha256 {
            bail!("leverage campaign artifact hash mismatch");
        }
        Ok(())
    }
}

#[derive(Clone)]
struct OperationIdentityV1 {
    operation_id: String,
    order: ChainOrder,
    block_time_unix_seconds: i64,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum PriceMarkPhaseV1 {
    PreOperation,
    PostOperation,
}

#[derive(Clone, Serialize)]
struct RawFactualPriceMarkV1 {
    order: ChainOrder,
    phase: PriceMarkPhaseV1,
    reserve0: u64,
    reserve1: u64,
}

struct V1PrivateCaptureV1 {
    engine: V1NativeEngine,
    state_sha256: String,
}

/// Mutable in-memory collector. It is intentionally not serializable.
pub struct LeverageCampaignBuilderV1 {
    spec: LeverageCampaignSpecV1,
    selections: Vec<LeverageCampaignSelectionV1>,
    plan_sha256: String,
    lending_markets: Vec<String>,
    non_lending_markets: Vec<String>,
    market_status: Vec<LeverageCampaignMarketStatusV1>,
    target_orders: BTreeMap<String, BTreeSet<ChainOrder>>,
    v1_states: BTreeMap<String, V1PrivateCaptureV1>,
    factual_price_paths: BTreeMap<String, Vec<RawFactualPriceMarkV1>>,
    dusk_states: BTreeMap<String, DuskLeveragePrivateSnapshotV1>,
    v1_traversal: Option<V1ObservedTraversalSummaryV1>,
    dusk_replays: BTreeMap<String, LeverageCampaignReplayEvidenceV1>,
}

impl LeverageCampaignBuilderV1 {
    pub fn from_canonical_history(
        operations_path: &Path,
        spec: LeverageCampaignSpecV1,
    ) -> Result<Self> {
        spec.verify()?;
        if sha256_file(operations_path)? != spec.operations_sha256 {
            bail!("leverage campaign operation source hash mismatch");
        }
        let input = File::open(operations_path)
            .with_context(|| format!("opening {}", operations_path.display()))?;
        let mut previous_order = None;
        let mut holdout = BTreeMap::<String, Vec<OperationIdentityV1>>::new();
        let mut lending = BTreeSet::<String>::new();
        for (line_index, line) in BufReader::new(input).lines().enumerate() {
            let line =
                line.with_context(|| format!("reading operation line {}", line_index + 1))?;
            if line.trim().is_empty() {
                continue;
            }
            let operation: CanonicalOperationV1 = serde_json::from_str(&line)
                .with_context(|| format!("decoding operation line {}", line_index + 1))?;
            operation.validate()?;
            if previous_order.is_some_and(|order| order >= operation.order) {
                bail!("canonical operation stream is not in strict order");
            }
            previous_order = Some(operation.order);
            if operation.block_time_unix_seconds < spec.holdout_end_unix_seconds_exclusive
                && spec.usd_headline_markets.contains(&operation.pair)
                && matches!(operation.payload, OperationPayloadV1::Borrow { .. })
            {
                lending.insert(operation.pair.clone());
            }
            if spec.usd_headline_markets.contains(&operation.pair)
                && operation.block_time_unix_seconds >= spec.holdout_start_unix_seconds
                && operation.block_time_unix_seconds < spec.holdout_end_unix_seconds_exclusive
            {
                holdout
                    .entry(operation.pair.clone())
                    .or_default()
                    .push(OperationIdentityV1 {
                        operation_id: operation.operation_id.clone(),
                        order: operation.order,
                        block_time_unix_seconds: operation.block_time_unix_seconds,
                    });
            }
        }

        let lending_markets: Vec<_> = lending.iter().cloned().collect();
        let non_lending_markets: Vec<_> = spec
            .usd_headline_markets
            .difference(&lending)
            .cloned()
            .collect();
        let mut selections = Vec::new();
        let mut target_orders = BTreeMap::<String, BTreeSet<ChainOrder>>::new();
        let mut market_status = Vec::new();
        for market in &lending_markets {
            let operations = holdout.get(market).map(Vec::as_slice).unwrap_or_default();
            let selected = select_first_median_last(operations);
            for (checkpoint_kind, operation) in selected {
                target_orders
                    .entry(market.clone())
                    .or_default()
                    .insert(operation.order);
                selections.push(LeverageCampaignSelectionV1 {
                    market: market.clone(),
                    checkpoint_kind,
                    operation_id: operation.operation_id.clone(),
                    order: operation.order,
                    block_time_unix_seconds: operation.block_time_unix_seconds,
                });
            }
            market_status.push(LeverageCampaignMarketStatusV1 {
                market: market.clone(),
                factual_lending_activity: true,
                selected_holdout_state_count: if operations.is_empty() { 0 } else { 3 },
                blocker: operations
                    .is_empty()
                    .then_some(LeverageCampaignBlockerV1::NoHoldoutOperation),
            });
        }
        for market in &non_lending_markets {
            market_status.push(LeverageCampaignMarketStatusV1 {
                market: market.clone(),
                factual_lending_activity: false,
                selected_holdout_state_count: 0,
                blocker: None,
            });
        }
        selections.sort_by(|left, right| {
            left.market
                .cmp(&right.market)
                .then(left.checkpoint_kind.cmp(&right.checkpoint_kind))
        });
        market_status.sort_by(|left, right| left.market.cmp(&right.market));
        let plan_sha256 = domain_hash(
            PLAN_DOMAIN,
            &(spec.spec_sha256.as_str(), &lending_markets, &selections),
        )?;
        Ok(Self {
            spec,
            selections,
            plan_sha256,
            lending_markets,
            non_lending_markets,
            market_status,
            target_orders,
            v1_states: BTreeMap::new(),
            factual_price_paths: BTreeMap::new(),
            dusk_states: BTreeMap::new(),
            v1_traversal: None,
            dusk_replays: BTreeMap::new(),
        })
    }

    pub fn capture_v1_history(
        &mut self,
        operations_path: &Path,
        configurations_path: &Path,
    ) -> Result<&V1ObservedTraversalSummaryV1> {
        if self.v1_traversal.is_some() {
            bail!("V1 leverage campaign traversal already captured");
        }
        if sha256_file(operations_path)? != self.spec.operations_sha256
            || sha256_file(configurations_path)? != self.spec.configurations_sha256
        {
            bail!("V1 leverage campaign traversal source hash mismatch");
        }
        let summary = traverse_v1_history_observed(
            operations_path,
            configurations_path,
            self.spec.cutoff_slot,
            self.spec.v1_replay_certification_policy,
            self,
        )?;
        self.v1_traversal = Some(summary);
        Ok(self.v1_traversal.as_ref().expect("inserted above"))
    }

    pub fn begin_dusk_pass(
        &mut self,
        arm: ReplayArm,
        hlp_scenario: HlpScenario,
    ) -> Result<LeverageDuskCapturePassV1<'_>> {
        let key = pass_key(arm, hlp_scenario);
        if self.dusk_replays.contains_key(&key) {
            bail!("Dusk leverage campaign pass was already captured");
        }
        Ok(LeverageDuskCapturePassV1 {
            builder: self,
            arm,
            hlp_scenario,
        })
    }

    pub fn finalize(
        self,
        transfers: &ValidatedTransferFeeRegistryV1,
    ) -> Result<LeverageCampaignArtifactV1> {
        let v1_traversal = self
            .v1_traversal
            .clone()
            .context("leverage campaign has no full V1 traversal")?;
        let mint_evidence: BTreeMap<_, _> = self
            .spec
            .mint_eligibility
            .iter()
            .map(|evidence| (evidence.mint.as_str(), evidence))
            .collect();
        let loop_count = usize::try_from(self.spec.v1_open_loop_iteration_cap)?;
        let unwind_count = usize::try_from(self.spec.v1_unwind_iteration_cap)?;
        let mut cells = Vec::new();
        let mut private_commitments = Vec::<(String, String, String)>::new();

        for selection in &self.selections {
            let state_key = target_key(&selection.market, selection.order);
            let v1 = self.v1_states.get(&state_key);
            if let Some(v1) = v1 {
                private_commitments.push((
                    format!("v1:{state_key}"),
                    v1.state_sha256.clone(),
                    "full_native_engine".to_owned(),
                ));
            }
            for arm in [ReplayArm::FixedFlow, ReplayArm::Intent] {
                for hlp_scenario in [HlpScenario::NoHlp, HlpScenario::MaximumFeasible] {
                    let dusk_key =
                        dusk_target_key(&selection.market, selection.order, arm, hlp_scenario);
                    let dusk = self.dusk_states.get(&dusk_key);
                    if let Some(dusk) = dusk {
                        private_commitments.push((
                            format!("dusk:{dusk_key}"),
                            dusk.state_sha256.clone(),
                            "market_positions_hlp_owned".to_owned(),
                        ));
                    }
                    for direction in [
                        LeverageDirectionV1::LongBase,
                        LeverageDirectionV1::LongQuote,
                    ] {
                        let path = self.factual_price_paths.get(&state_key).and_then(|marks| {
                            build_realized_path(
                                selection,
                                direction,
                                marks,
                                self.spec.holdout_end_unix_seconds_exclusive,
                            )
                            .ok()
                        });
                        for margin_bps in STANDARDIZED_MARGIN_NAV_BPS {
                            cells.push(build_cell(
                                &self.spec,
                                selection,
                                arm,
                                hlp_scenario,
                                direction,
                                margin_bps,
                                v1,
                                dusk,
                                path.as_ref(),
                                &mint_evidence,
                                transfers,
                                loop_count,
                                unwind_count,
                            )?);
                        }
                    }
                }
            }
        }
        cells.sort_by(|left, right| left.cell_id.cmp(&right.cell_id));
        private_commitments.sort();
        private_commitments.dedup();
        let private_capture_set_sha256 = domain_hash(PRIVATE_CAPTURE_DOMAIN, &private_commitments)?;
        let mut dusk_replays: Vec<_> = self.dusk_replays.into_values().collect();
        dusk_replays.sort_by_key(|row| (arm_code(row.arm), scenario_code(row.hlp_scenario)));
        let artifact = LeverageCampaignArtifactV1 {
            schema_version: LEVERAGE_CAMPAIGN_SCHEMA_VERSION,
            spec: self.spec,
            plan_sha256: self.plan_sha256,
            selection_policy:
                "first, lower-median index floor((n-1)/2), and last canonical holdout operation pre-state"
                    .to_owned(),
            standardized_margin_nav_bps: STANDARDIZED_MARGIN_NAV_BPS.to_vec(),
            factual_lending_markets: self.lending_markets,
            non_lending_usd_headline_markets: self.non_lending_markets,
            market_status: self.market_status,
            selections: self.selections,
            v1_traversal,
            dusk_replays,
            private_capture_set_sha256,
            all_core_cells_available: cells.iter().all(|cell| cell.core_benchmark_available),
            all_realized_dusk_paths_complete: cells.iter().all(|cell| {
                cell.dusk_adverse_path_availability == AdversePathAvailabilityV1::Complete
            }),
            cells,
            artifact_sha256: String::new(),
        }
        .seal()?;
        artifact.verify()?;
        Ok(artifact)
    }

    fn target_orders_at_or_before(
        &self,
        market: &str,
        order: ChainOrder,
    ) -> impl Iterator<Item = ChainOrder> + '_ {
        self.target_orders
            .get(market)
            .into_iter()
            .flat_map(move |orders| orders.range(..=order).copied())
    }

    fn observe_price(
        &mut self,
        operation: &CanonicalOperationV1,
        market: &str,
        engine: &V1NativeEngine,
        phase: PriceMarkPhaseV1,
    ) -> Result<()> {
        if operation.block_time_unix_seconds < self.spec.holdout_start_unix_seconds
            || operation.block_time_unix_seconds >= self.spec.holdout_end_unix_seconds_exclusive
        {
            return Ok(());
        }
        let targets: Vec<_> = self
            .target_orders_at_or_before(market, operation.order)
            .collect();
        let pair = engine.pair();
        for target in targets {
            self.factual_price_paths
                .entry(target_key(market, target))
                .or_default()
                .push(RawFactualPriceMarkV1 {
                    order: operation.order,
                    phase,
                    reserve0: pair.reserve0,
                    reserve1: pair.reserve1,
                });
        }
        Ok(())
    }
}

impl V1HistoricalStateObserverV1 for LeverageCampaignBuilderV1 {
    fn observe_pre_operation(
        &mut self,
        operation: &CanonicalOperationV1,
        market: &str,
        engine: Option<&V1NativeEngine>,
        _broadcast_global_config: bool,
    ) -> Result<()> {
        if self
            .target_orders
            .get(market)
            .is_some_and(|orders| orders.contains(&operation.order))
        {
            if let Some(engine) = engine {
                let state_sha256 = canonical_terminal_checkpoint_sha256(&engine.checkpoint())?;
                let key = target_key(market, operation.order);
                let capture = V1PrivateCaptureV1 {
                    engine: engine.try_fork(),
                    state_sha256,
                };
                if self.v1_states.insert(key, capture).is_some() {
                    bail!("duplicate full V1 campaign capture");
                }
            }
        }
        if let Some(engine) = engine {
            self.observe_price(operation, market, engine, PriceMarkPhaseV1::PreOperation)?;
        }
        Ok(())
    }

    fn observe_post_operation(
        &mut self,
        operation: &CanonicalOperationV1,
        market: &str,
        engine: &V1NativeEngine,
        _broadcast_global_config: bool,
    ) -> Result<()> {
        self.observe_price(operation, market, engine, PriceMarkPhaseV1::PostOperation)
    }
}

pub struct LeverageDuskCapturePassV1<'a> {
    builder: &'a mut LeverageCampaignBuilderV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
}

impl LeverageDuskCapturePassV1<'_> {
    pub fn finish(self, summary: &CounterfactualEvaluationSummaryV1) -> Result<()> {
        if summary.schema_version != EVALUATION_SCHEMA_VERSION
            || summary.profile_id != self.builder.spec.profile_id
            || summary.arm != self.arm
            || summary.hlp_scenario != self.hlp_scenario
            || !valid_sha256(&summary.records_sha256)
        {
            bail!("Dusk leverage capture summary identity mismatch");
        }
        let key = pass_key(self.arm, self.hlp_scenario);
        if self
            .builder
            .dusk_replays
            .insert(
                key,
                LeverageCampaignReplayEvidenceV1 {
                    arm: self.arm,
                    hlp_scenario: self.hlp_scenario,
                    evaluation_schema_version: summary.schema_version,
                    records_sha256: summary.records_sha256.clone(),
                    complete_for_scoring: summary.complete_for_scoring,
                },
            )
            .is_some()
        {
            bail!("duplicate Dusk leverage capture summary");
        }
        Ok(())
    }
}

impl CounterfactualStateObserverV1 for LeverageDuskCapturePassV1<'_> {
    fn observe_pre_operation(
        &mut self,
        operation: &CanonicalOperationV1,
        factual: &V1ReplayLedgerRowV1,
        market: Option<&DuskCounterfactualMarketV1<'_>>,
    ) -> Result<()> {
        if !self
            .builder
            .target_orders
            .get(&factual.market)
            .is_some_and(|orders| orders.contains(&operation.order))
        {
            return Ok(());
        }
        let market = market.context("selected Dusk pre-operation state is uninitialized")?;
        if market.profile_id() != self.builder.spec.profile_id {
            bail!("Dusk leverage capture profile mismatch");
        }
        let snapshot = market.capture_leverage_private_snapshot()?;
        if snapshot.hlp_scenario != self.hlp_scenario {
            bail!("Dusk leverage capture hLP scenario mismatch");
        }
        let key = dusk_target_key(
            &factual.market,
            operation.order,
            self.arm,
            self.hlp_scenario,
        );
        if self.builder.dusk_states.insert(key, snapshot).is_some() {
            bail!("duplicate Dusk leverage campaign capture");
        }
        Ok(())
    }
}

struct BuiltRealizedPathV1 {
    marks: Vec<AdversePriceMarkV1>,
    evidence: RealizedAdverseExcursionEvidenceV1,
}

#[allow(clippy::too_many_arguments)]
fn build_cell(
    spec: &LeverageCampaignSpecV1,
    selection: &LeverageCampaignSelectionV1,
    arm: ReplayArm,
    hlp_scenario: HlpScenario,
    direction: LeverageDirectionV1,
    margin_bps: u16,
    v1: Option<&V1PrivateCaptureV1>,
    dusk: Option<&DuskLeveragePrivateSnapshotV1>,
    path: Option<&BuiltRealizedPathV1>,
    mint_evidence: &BTreeMap<&str, &LeverageMintEligibilityEvidenceV1>,
    transfers: &ValidatedTransferFeeRegistryV1,
    loop_count: usize,
    unwind_count: usize,
) -> Result<LeverageCampaignCellV1> {
    let mut blockers = Vec::new();
    let v1_state_sha256 = v1.map(|value| value.state_sha256.clone());
    let dusk_state_sha256 = dusk.map(|value| value.state_sha256.clone());
    if v1.is_none() {
        blockers.push(LeverageCampaignBlockerV1::MissingFullV1PreOperationState);
    }
    if dusk.is_none() {
        blockers.push(LeverageCampaignBlockerV1::MissingDuskPreOperationState);
    }
    if path.is_none() {
        blockers.push(LeverageCampaignBlockerV1::FactualMarginalPricePathUnavailable);
    }
    blockers.push(LeverageCampaignBlockerV1::ExactDuskFuturePositionBranchUnavailable);

    let (pool_nav, margin) = v1
        .and_then(|capture| {
            factual_nav_and_margin(capture.engine.pair(), direction, margin_bps).ok()
        })
        .map_or((None, None), |(nav, margin)| (Some(nav), Some(margin)));
    if margin.is_none_or(|value| value == 0) {
        blockers.push(LeverageCampaignBlockerV1::ZeroOrOverflowingStartingMargin);
    }
    let collateral_mint = v1.map(|capture| match direction {
        LeverageDirectionV1::LongBase => capture.engine.pair().token0.to_string(),
        LeverageDirectionV1::LongQuote => capture.engine.pair().token1.to_string(),
    });
    let eligibility = collateral_mint
        .as_deref()
        .and_then(|mint| mint_evidence.get(mint).copied());
    match eligibility {
        None => blockers.push(LeverageCampaignBlockerV1::MissingMintEligibilityEvidence),
        Some(evidence) if !evidence.native_leverage_eligible => {
            blockers.push(LeverageCampaignBlockerV1::NativeCollateralMintIneligible)
        }
        Some(_) => {}
    }

    let identity = serde_json::json!({
        "spec_sha256": spec.spec_sha256,
        "market": selection.market,
        "checkpoint_kind": selection.checkpoint_kind,
        "operation_id": selection.operation_id,
        "order": selection.order,
        "profile_id": spec.profile_id,
        "arm": arm,
        "hlp_scenario": hlp_scenario,
        "direction": direction,
        "starting_margin_nav_bps": margin_bps,
        "v1_pre_state_sha256": v1_state_sha256,
        "dusk_pre_state_sha256": dusk_state_sha256,
        "realized_path_sha256": path.map(|value| value.evidence.source_sha256.as_str()),
    });
    let cell_id = domain_hash(CELL_ID_DOMAIN, &identity)?;
    let benchmark = match (v1, dusk, path, margin, eligibility) {
        (Some(v1), Some(dusk), Some(path), Some(margin), Some(eligibility))
            if margin > 0 && eligibility.native_leverage_eligible =>
        {
            let debt_asset = match direction {
                LeverageDirectionV1::LongBase => MarketAsset::Quote,
                LeverageDirectionV1::LongQuote => MarketAsset::Base,
            };
            let token_balances = dusk.token_balances(debt_asset, margin)?;
            let loop_clocks = vec![
                V1LoopClockV1 {
                    borrow_slot: selection.order.slot,
                    swap_slot: selection.order.slot,
                    deposit_slot: selection.order.slot,
                };
                loop_count
            ];
            let unwind_clocks = vec![
                V1UnwindClockV1 {
                    withdraw_slot: selection.order.slot,
                    swap_slot: selection.order.slot,
                    repay_slot: selection.order.slot,
                };
                unwind_count
            ];
            let owner = deterministic_pubkey("owner", &cell_id)?;
            let position_id = deterministic_pubkey("position", &cell_id)?;
            let realized_mae = SourcedRealizedMaeV1 {
                adverse_excursion_bps: path
                    .evidence
                    .adverse_excursion_bps
                    .as_str()
                    .parse()
                    .context("realized MAE exceeds u64")?,
                source_sha256: path.evidence.source_sha256.clone(),
            };
            let clock = BenchmarkClock {
                slot: selection.order.slot,
                unix_timestamp: selection.block_time_unix_seconds,
            };
            let request = LeverageBenchmarkInputV1 {
                benchmark_id: cell_id.clone(),
                direction,
                owner,
                position_id,
                starting_margin_gross: margin,
                dusk_multiplier_bps: LEVERAGE_MAX_MULTIPLIER_BPS,
                dusk_open_clock: clock,
                dusk_close_clock: clock,
                dusk_token_balances: token_balances,
                dusk_policy: dusk.policy(eligibility.native_leverage_eligible),
                hlp_scenario,
                dusk_base_hlp: dusk.base_hlp.as_ref(),
                dusk_quote_hlp: dusk.quote_hlp.as_ref(),
                collateral_mint_decode_sha256: eligibility.decoded_mint_sha256.clone(),
                v1_initial_swap_slot: selection.order.slot,
                v1_initial_deposit_slot: selection.order.slot,
                v1_loop_clocks: &loop_clocks,
                v1_max_loop_iterations: loop_count,
                v1_unwind_clocks: &unwind_clocks,
                v1_max_unwind_iterations: unwind_count,
                adverse_path: &path.marks,
                realized_mae: Some(realized_mae),
            };
            match benchmark_leverage_exact(&dusk.market, &v1.engine, transfers, request) {
                Ok(result) => Some(result),
                Err(error) => {
                    blockers.push(blocker_for_error(&error));
                    None
                }
            }
        }
        _ => None,
    };
    if let Some(result) = &benchmark {
        if result.v1.opening.loop_iteration_limit_reached {
            blockers.push(LeverageCampaignBlockerV1::V1OpenLoopIterationCapReached);
        }
        if result.v1.unwind.limiting_reason == crate::leverage::UnwindLimitV1::IterationLimit {
            blockers.push(LeverageCampaignBlockerV1::V1UnwindIterationCapReached);
        }
        if !result.dusk.unwind.executable_full_unwind {
            blockers.push(LeverageCampaignBlockerV1::DuskFullUnwindUnavailable);
        }
    }
    blockers.sort_by_key(blocker_code);
    blockers.dedup();
    LeverageCampaignCellV1 {
        schema_version: LEVERAGE_CAMPAIGN_SCHEMA_VERSION,
        cell_id,
        market: selection.market.clone(),
        checkpoint_kind: selection.checkpoint_kind,
        operation_id: selection.operation_id.clone(),
        order: selection.order,
        block_time_unix_seconds: selection.block_time_unix_seconds,
        profile_id: spec.profile_id.clone(),
        arm,
        hlp_scenario,
        direction,
        starting_margin_nav_bps: margin_bps,
        factual_accounted_pool_nav_in_debt_atoms: pool_nav.map(token),
        starting_margin_gross: margin.map(token),
        v1_pre_state_sha256: v1_state_sha256,
        dusk_pre_state_sha256: dusk_state_sha256,
        realized_adverse_excursion: path.map(|value| value.evidence.clone()),
        dusk_adverse_path_availability:
            AdversePathAvailabilityV1::MissingExactDuskFutureCounterfactualState,
        unwind_horizon: LeverageUnwindHorizonV1::ImmediateSameCheckpoint,
        core_benchmark_available: benchmark.is_some(),
        complete_for_headline_path_claim: false,
        blockers,
        benchmark,
        cell_sha256: String::new(),
    }
    .seal()
}

fn build_realized_path(
    selection: &LeverageCampaignSelectionV1,
    direction: LeverageDirectionV1,
    raw: &[RawFactualPriceMarkV1],
    path_end_unix_seconds_exclusive: i64,
) -> Result<BuiltRealizedPathV1> {
    let mut marks = Vec::with_capacity(raw.len());
    let mut source_rows = Vec::with_capacity(raw.len());
    for mark in raw {
        let (debt, collateral) = match direction {
            LeverageDirectionV1::LongBase => (mark.reserve1, mark.reserve0),
            LeverageDirectionV1::LongQuote => (mark.reserve0, mark.reserve1),
        };
        if debt == 0 || collateral == 0 {
            bail!("zero reserve in factual leverage price path");
        }
        let price = u64::try_from(
            u128::from(debt)
                .checked_mul(u128::from(NAD))
                .and_then(|value| value.checked_div(u128::from(collateral)))
                .context("factual leverage price overflow")?,
        )?;
        if price == 0 {
            bail!("zero factual leverage marginal price");
        }
        marks.push(AdversePriceMarkV1 {
            slot: mark.order.slot,
            collateral_price_nad: price,
            dusk_closeout_value: None,
        });
        source_rows.push((mark, price));
    }
    let entry = marks
        .first()
        .context("empty factual leverage price path")?
        .collateral_price_nad;
    let worst = marks
        .iter()
        .map(|mark| mark.collateral_price_nad)
        .min()
        .context("empty factual leverage price path")?;
    let adverse_excursion_bps = u64::try_from(
        u128::from(entry.saturating_sub(worst))
            .checked_mul(u128::from(BPS_DENOMINATOR))
            .and_then(|value| value.checked_div(u128::from(entry)))
            .context("factual leverage adverse excursion overflow")?,
    )?;
    let source_sha256 = domain_hash(
        PATH_DOMAIN,
        &(
            &selection.market,
            selection.order,
            direction,
            path_end_unix_seconds_exclusive,
            &source_rows,
        ),
    )?;
    Ok(BuiltRealizedPathV1 {
        evidence: RealizedAdverseExcursionEvidenceV1 {
            mark_count: u64::try_from(marks.len())?,
            first_slot: marks.first().expect("nonempty above").slot,
            last_slot: marks.last().expect("nonempty above").slot,
            entry_collateral_price_nad: token(entry),
            worst_collateral_price_nad: token(worst),
            adverse_excursion_bps: token(adverse_excursion_bps),
            path_end_unix_seconds_exclusive,
            source_sha256,
        },
        marks,
    })
}

fn factual_nav_and_margin(
    pair: &omnipair::state::Pair,
    direction: LeverageDirectionV1,
    margin_bps: u16,
) -> Result<(u64, u64)> {
    let debt_reserve = match direction {
        LeverageDirectionV1::LongBase => pair.reserve1,
        LeverageDirectionV1::LongQuote => pair.reserve0,
    };
    factual_nav_and_margin_from_debt_reserve(debt_reserve, margin_bps)
}

fn factual_nav_and_margin_from_debt_reserve(
    debt_reserve: u64,
    margin_bps: u16,
) -> Result<(u64, u64)> {
    // At the factual CPMM marginal price, the opposite reserve is worth
    // exactly the debt-side reserve. NAV in debt atoms is therefore 2R.
    let nav = u128::from(debt_reserve)
        .checked_mul(2)
        .context("factual accounted NAV overflow")?;
    let margin = nav
        .checked_mul(u128::from(margin_bps))
        .and_then(|value| value.checked_div(u128::from(BPS_DENOMINATOR)))
        .context("standardized margin overflow")?;
    Ok((u64::try_from(nav)?, u64::try_from(margin)?))
}

fn select_first_median_last(
    operations: &[OperationIdentityV1],
) -> Vec<(LeverageCheckpointKindV1, &OperationIdentityV1)> {
    if operations.is_empty() {
        return Vec::new();
    }
    vec![
        (LeverageCheckpointKindV1::First, &operations[0]),
        (
            LeverageCheckpointKindV1::Median,
            &operations[(operations.len() - 1) / 2],
        ),
        (
            LeverageCheckpointKindV1::Last,
            &operations[operations.len() - 1],
        ),
    ]
}

fn blocker_for_error(error: &LeverageBenchmarkError) -> LeverageCampaignBlockerV1 {
    match error {
        LeverageBenchmarkError::DuskNative(_) => {
            LeverageCampaignBlockerV1::NativeDuskTransitionRejected
        }
        LeverageBenchmarkError::V1(_) => LeverageCampaignBlockerV1::NativeV1TransitionRejected,
        LeverageBenchmarkError::TransferFee(_) => {
            LeverageCampaignBlockerV1::HistoricalTransferFeeResolutionRejected
        }
        LeverageBenchmarkError::Arithmetic(_) | LeverageBenchmarkError::IntegerString => {
            LeverageCampaignBlockerV1::NativeBenchmarkArithmeticRejected
        }
        LeverageBenchmarkError::InvalidInput(_)
        | LeverageBenchmarkError::MarketIdentityMismatch
        | LeverageBenchmarkError::MissingClock { .. }
        | LeverageBenchmarkError::UnsupportedDuskCollateralMint => {
            LeverageCampaignBlockerV1::NativeBenchmarkInputRejected
        }
    }
}

fn blocker_code(blocker: &LeverageCampaignBlockerV1) -> u8 {
    match blocker {
        LeverageCampaignBlockerV1::NoHoldoutOperation => 0,
        LeverageCampaignBlockerV1::MissingFullV1PreOperationState => 1,
        LeverageCampaignBlockerV1::MissingDuskPreOperationState => 2,
        LeverageCampaignBlockerV1::MissingMintEligibilityEvidence => 3,
        LeverageCampaignBlockerV1::NativeCollateralMintIneligible => 4,
        LeverageCampaignBlockerV1::ZeroOrOverflowingStartingMargin => 5,
        LeverageCampaignBlockerV1::FactualMarginalPricePathUnavailable => 6,
        LeverageCampaignBlockerV1::ExactDuskFuturePositionBranchUnavailable => 7,
        LeverageCampaignBlockerV1::V1OpenLoopIterationCapReached => 8,
        LeverageCampaignBlockerV1::V1UnwindIterationCapReached => 9,
        LeverageCampaignBlockerV1::DuskFullUnwindUnavailable => 10,
        LeverageCampaignBlockerV1::NativeDuskTransitionRejected => 11,
        LeverageCampaignBlockerV1::NativeV1TransitionRejected => 12,
        LeverageCampaignBlockerV1::HistoricalTransferFeeResolutionRejected => 13,
        LeverageCampaignBlockerV1::NativeBenchmarkInputRejected => 14,
        LeverageCampaignBlockerV1::NativeBenchmarkArithmeticRejected => 15,
    }
}

fn deterministic_pubkey(label: &str, cell_id: &str) -> Result<Pubkey> {
    let digest = Sha256::digest(format!("leverage-campaign:{label}:{cell_id}").as_bytes());
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&digest);
    let key = Pubkey::new_from_array(bytes);
    if key == Pubkey::default() {
        bail!("deterministic leverage campaign key is zero");
    }
    Ok(key)
}

fn target_key(market: &str, order: ChainOrder) -> String {
    format!(
        "{}:{}:{}:{}:{:?}:{}",
        market,
        order.slot,
        order.transaction_index,
        order.outer_instruction_index,
        order.inner_execution_index,
        order.event_sequence
    )
}

fn dusk_target_key(
    market: &str,
    order: ChainOrder,
    arm: ReplayArm,
    scenario: HlpScenario,
) -> String {
    format!(
        "{}:{}:{}",
        target_key(market, order),
        arm_code(arm),
        scenario_code(scenario)
    )
}

fn pass_key(arm: ReplayArm, scenario: HlpScenario) -> String {
    format!("{}:{}", arm_code(arm), scenario_code(scenario))
}

const fn arm_code(arm: ReplayArm) -> u8 {
    match arm {
        ReplayArm::FixedFlow => 0,
        ReplayArm::Intent => 1,
    }
}

const fn scenario_code(scenario: HlpScenario) -> u8 {
    match scenario {
        HlpScenario::NoHlp => 0,
        HlpScenario::MaximumFeasible => 1,
    }
}

fn token(value: u64) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u64 is canonical")
}

fn domain_hash<T: Serialize>(domain: &[u8], value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(hex::encode(digest.finalize()))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("reading {}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
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

    fn identity(slot: u64) -> OperationIdentityV1 {
        OperationIdentityV1 {
            operation_id: format!("operation-{slot}"),
            order: order(slot),
            block_time_unix_seconds: slot as i64,
        }
    }

    fn campaign_spec(policy: V1ReplayCertificationPolicyV1) -> LeverageCampaignSpecV1 {
        LeverageCampaignSpecV1 {
            schema_version: LEVERAGE_CAMPAIGN_SCHEMA_VERSION,
            profile_id: "1".repeat(64),
            profile_freeze_sha256: "2".repeat(64),
            operations_sha256: "3".repeat(64),
            factual_ledger_sha256: "4".repeat(64),
            configurations_sha256: "5".repeat(64),
            transfer_fee_registry_sha256: "6".repeat(64),
            v1_replay_certification_policy: policy,
            holdout_start_unix_seconds: 10,
            holdout_end_unix_seconds_exclusive: 20,
            cutoff_slot: 100,
            usd_headline_markets: BTreeSet::from([Pubkey::new_from_array([7; 32]).to_string()]),
            mint_eligibility: Vec::new(),
            v1_open_loop_iteration_cap: 8,
            v1_unwind_iteration_cap: 8,
            spec_sha256: String::new(),
        }
    }

    #[test]
    fn selection_is_first_lower_median_last_and_preserves_ties() {
        let operations = [identity(10), identity(20), identity(30), identity(40)];
        let selected = select_first_median_last(&operations);
        assert_eq!(selected[0].1.order, order(10));
        assert_eq!(selected[1].1.order, order(20));
        assert_eq!(selected[2].1.order, order(40));

        let singleton = [identity(99)];
        let selected = select_first_median_last(&singleton);
        assert_eq!(selected.len(), 3);
        assert!(selected.iter().all(|(_, row)| row.order == order(99)));
    }

    #[test]
    fn direction_specific_nav_and_path_are_exact() {
        assert_eq!(
            factual_nav_and_margin_from_debt_reserve(2_000, 100).unwrap(),
            (4_000, 40)
        );
        assert_eq!(
            factual_nav_and_margin_from_debt_reserve(1_000, 500).unwrap(),
            (2_000, 100)
        );

        let selection = LeverageCampaignSelectionV1 {
            market: Pubkey::new_unique().to_string(),
            checkpoint_kind: LeverageCheckpointKindV1::First,
            operation_id: "operation".to_owned(),
            order: order(10),
            block_time_unix_seconds: 10,
        };
        let raw = vec![
            RawFactualPriceMarkV1 {
                order: order(10),
                phase: PriceMarkPhaseV1::PreOperation,
                reserve0: 1_000,
                reserve1: 2_000,
            },
            RawFactualPriceMarkV1 {
                order: order(20),
                phase: PriceMarkPhaseV1::PostOperation,
                reserve0: 1_000,
                reserve1: 1_500,
            },
        ];
        let path =
            build_realized_path(&selection, LeverageDirectionV1::LongBase, &raw, 30).unwrap();
        assert_eq!(path.evidence.adverse_excursion_bps.as_str(), "2500");
        assert!(path
            .marks
            .iter()
            .all(|mark| mark.dusk_closeout_value.is_none()));
    }

    #[test]
    fn typed_future_path_unavailability_is_not_a_no_breach_claim() {
        let blocker = LeverageCampaignBlockerV1::ExactDuskFuturePositionBranchUnavailable;
        assert_eq!(blocker_code(&blocker), 7);
        assert_ne!(
            AdversePathAvailabilityV1::MissingExactDuskFutureCounterfactualState,
            AdversePathAvailabilityV1::Complete
        );
    }

    #[test]
    fn domain_hashes_bind_scenario_and_margin() {
        let base = serde_json::json!({
            "scenario": HlpScenario::NoHlp,
            "margin_bps": 10,
        });
        let scenario = serde_json::json!({
            "scenario": HlpScenario::MaximumFeasible,
            "margin_bps": 10,
        });
        let margin = serde_json::json!({
            "scenario": HlpScenario::NoHlp,
            "margin_bps": 100,
        });
        let base = domain_hash(CELL_ID_DOMAIN, &base).unwrap();
        assert_ne!(base, domain_hash(CELL_ID_DOMAIN, &scenario).unwrap());
        assert_ne!(base, domain_hash(CELL_ID_DOMAIN, &margin).unwrap());
    }

    #[test]
    fn campaign_spec_hash_binds_required_replay_certification_policy() {
        let audit = campaign_spec(V1ReplayCertificationPolicyV1::AuditGrade)
            .seal()
            .unwrap();
        let decision =
            campaign_spec(V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope)
                .seal()
                .unwrap();
        audit.verify().unwrap();
        decision.verify().unwrap();
        assert_ne!(audit.spec_sha256, decision.spec_sha256);

        let mut missing_policy = serde_json::to_value(decision).unwrap();
        missing_policy
            .as_object_mut()
            .unwrap()
            .remove("v1_replay_certification_policy");
        assert!(serde_json::from_value::<LeverageCampaignSpecV1>(missing_policy).is_err());
    }

    #[test]
    fn sealed_cell_rejects_post_hash_mutation() {
        let mut cell = LeverageCampaignCellV1 {
            schema_version: LEVERAGE_CAMPAIGN_SCHEMA_VERSION,
            cell_id: "1".repeat(64),
            market: Pubkey::new_unique().to_string(),
            checkpoint_kind: LeverageCheckpointKindV1::Median,
            operation_id: "operation".to_owned(),
            order: order(10),
            block_time_unix_seconds: 10,
            profile_id: "2".repeat(64),
            arm: ReplayArm::FixedFlow,
            hlp_scenario: HlpScenario::NoHlp,
            direction: LeverageDirectionV1::LongBase,
            starting_margin_nav_bps: 100,
            factual_accounted_pool_nav_in_debt_atoms: Some(token(1_000)),
            starting_margin_gross: Some(token(10)),
            v1_pre_state_sha256: Some("3".repeat(64)),
            dusk_pre_state_sha256: Some("4".repeat(64)),
            realized_adverse_excursion: None,
            dusk_adverse_path_availability:
                AdversePathAvailabilityV1::MissingExactDuskFutureCounterfactualState,
            unwind_horizon: LeverageUnwindHorizonV1::ImmediateSameCheckpoint,
            core_benchmark_available: false,
            complete_for_headline_path_claim: false,
            blockers: vec![LeverageCampaignBlockerV1::ExactDuskFuturePositionBranchUnavailable],
            benchmark: None,
            cell_sha256: String::new(),
        }
        .seal()
        .unwrap();
        cell.verify().unwrap();
        cell.starting_margin_nav_bps = 500;
        assert!(cell.verify().is_err());
    }
}

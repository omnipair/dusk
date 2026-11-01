//! Exact USD aggregation and deterministic candidate-score evidence.
//!
//! Historical marks are joined by mint and UTC hour. Missing marks remain an
//! explicit blocker; current or estimated prices cannot enter this module.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{Signed, ToPrimitive, Zero};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    calibrate::CandidateScoreV1,
    canonical::{HistoricalPriceMarkV1, SignedTokenAmount, TokenAmount},
    counterfactual::{CounterfactualStatusV1, DuskBorrowCapacityConstraintV1},
    metric_engine::{
        revenue_origin_from_flags, CapitalCheckpointV1, PairedMetricLedgerSummaryV1,
        PairedOperationMetricRowV1, RevenueAccountingClassV1, RevenueLaneV1, RevenueOriginV1,
        RevenueSideDeltaV1, RevenueVenueV1,
    },
    metrics::MetricAvailability,
    replay::{HlpScenario, ReplayArm},
    spot_probe::{quote_improvement_ppb, SpotProbeDirectionV1, SpotProbeSizeKindV1},
};

pub const AGGREGATE_SCHEMA_VERSION: u16 = 5;
const HOUR_SECONDS: i64 = 3_600;
const USD_MICRO_SCALE: u128 = 1_000_000;
const MIN_ANNUALIZED_WINDOW_SECONDS: u64 = 30 * 24 * 60 * 60;
const YEAR_SECONDS: u64 = 365 * 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactRationalV1 {
    pub numerator: SignedTokenAmount,
    pub denominator: TokenAmount,
}

impl ExactRationalV1 {
    fn from_ratio(value: &BigRational) -> Result<Self> {
        if value.denom().is_negative() || value.denom().is_zero() {
            bail!("exact rational denominator is not positive");
        }
        Ok(Self {
            numerator: SignedTokenAmount::new(value.numer().to_string())?,
            denominator: TokenAmount::new(value.denom().to_string())?,
        })
    }
}

/// Exact rational with explicit availability. An unavailable value never
/// carries a partial numerator assembled from the subset of rows that happened
/// to have historical prices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableExactRationalV1 {
    pub availability: MetricAvailability,
    pub value: Option<ExactRationalV1>,
}

impl AvailableExactRationalV1 {
    fn available(value: &BigRational) -> Result<Self> {
        Ok(Self {
            availability: MetricAvailability::Available,
            value: Some(ExactRationalV1::from_ratio(value)?),
        })
    }

    fn unavailable(availability: MetricAvailability) -> Self {
        debug_assert!(availability != MetricAvailability::Available);
        Self {
            availability,
            value: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevenueUsdLaneTotalV1 {
    pub venue: RevenueVenueV1,
    pub origin: RevenueOriginV1,
    pub lane: RevenueLaneV1,
    pub accounting_class: RevenueAccountingClassV1,
    pub usd_micro: AvailableExactRationalV1,
    pub missing_price_keys: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevenueBeneficiaryV1 {
    AllStakeholders,
    ExternalLp,
    Hlp,
    Protocol,
}

/// Stable component identifier prevents multiple returns for the same
/// venue/origin/beneficiary from becoming ambiguous downstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevenueReturnComponentV1 {
    LpSwap,
    LpPublicInterestAccrued,
    ExternalLpCombinedEconomic,
    LpPublicInterestCashMemo,
    ProtocolPublicInterestCashMemo,
    NetHlpCommonCapitalContribution,
    ProtocolEconomic,
    AllStakeholderEconomic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevenueDenominatorBasisV1 {
    /// Common comparison denominator, not evidence of actual Dusk ownership or
    /// capital allocation.
    FactualV1AccountedLpCapitalTime,
    ExactDuskExternalLpOwnerClaimCapitalTime,
    ExactDuskHlpCapitalTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevenueReturnEvidenceV1 {
    pub venue: RevenueVenueV1,
    pub origin: RevenueOriginV1,
    pub beneficiary: RevenueBeneficiaryV1,
    pub component: RevenueReturnComponentV1,
    pub denominator_basis: RevenueDenominatorBasisV1,
    pub revenue_usd_micro: AvailableExactRationalV1,
    pub absolute_return_bps_exact: AvailableExactRationalV1,
    pub apr_bps_exact: AvailableExactRationalV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketCandidateEvidenceV1 {
    pub market: String,
    pub missing_price_keys: BTreeSet<String>,
    pub blocking_reasons: BTreeSet<String>,
    pub stakeholder_revenue_v1_usd_micro: AvailableExactRationalV1,
    pub stakeholder_revenue_dusk_user_usd_micro: AvailableExactRationalV1,
    pub stakeholder_revenue_dusk_synthetic_arbitrage_usd_micro: AvailableExactRationalV1,
    pub stakeholder_revenue_dusk_synthetic_protocol_settlement_usd_micro: AvailableExactRationalV1,
    pub factual_capital_time_usd_micro_seconds: ExactRationalV1,
    pub volume_weighted_execution_cost_delta_ppb_exact: Option<ExactRationalV1>,
    pub p95_slippage_delta_ppb: Option<SignedTokenAmount>,
    pub v1_actionable_headroom_matched_observation_sum_usd_micro: ExactRationalV1,
    pub v1_gross_headroom_matched_observation_sum_usd_micro: ExactRationalV1,
    pub dusk_actionable_headroom_matched_observation_sum_usd_micro: ExactRationalV1,
    pub dusk_capacity_decomposition: DuskBorrowCapacityAggregateV1,
    pub projected_total_debt_capacity: BorrowProjectedTotalDebtAggregateV1,
    pub v1_debt_increase_notional_usd_micro: ExactRationalV1,
    pub dusk_debt_increase_notional_usd_micro: ExactRationalV1,
    pub dusk_unmet_borrow_notional_usd_micro: ExactRationalV1,
    pub v1_bad_debt_usd_micro: ExactRationalV1,
    pub dusk_bad_debt_usd_micro: ExactRationalV1,
    pub bad_debt_delta_usd_micro_exact: ExactRationalV1,
    pub tail_liquidation_shortfall_delta_usd_micro: Option<SignedTokenAmount>,
    pub borrow_capacity_uplift_bps_exact: Option<ExactRationalV1>,
    pub revenue_uplift_on_factual_capital_bps: Option<SignedTokenAmount>,
    pub swap_count: u64,
    pub borrow_count: u64,
    pub liquidation_count: u64,
    pub spot_probes: Vec<SpotProbeAggregateCellV1>,
    pub revenue_yield: RevenueYieldEvidenceV1,
    /// Independently gated score for this market. This is derived from the
    /// same global replay bytes; calibration must not rerun a profile merely
    /// to obtain one market's score.
    pub candidate_score: Option<CandidateScoreV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskBorrowCapacityConstraintAggregateV1 {
    pub constraint: DuskBorrowCapacityConstraintV1,
    /// Number of currently-underwritten matched borrow pre-states where this
    /// constraint tied the exact actionable raw-request bound.
    pub limiting_matched_observation_count: u64,
    /// Gross-to-actionable raw-headroom loss attributed to this tied
    /// constraint across the same currently-underwritten population.
    /// Ties are intentionally non-additive: the same exact loss appears for
    /// every simultaneously binding constraint.
    pub tied_additional_headroom_loss_usd_micro: ExactRationalV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskBorrowCapacityAggregateV1 {
    pub matched_borrow_prestate_observation_count: u64,
    pub underwriting_comparable_observation_count: u64,
    pub headroom_ratio_observation_count: u64,
    pub gross_additional_headroom_matched_observation_sum_usd_micro: ExactRationalV1,
    pub actionable_additional_headroom_matched_observation_sum_usd_micro: ExactRationalV1,
    pub gross_to_actionable_additional_headroom_loss_usd_micro: ExactRationalV1,
    /// Exact distribution of actionable / gross raw additional headroom in
    /// bps. Underwriting-unsatisfied and zero-gross rows are excluded and
    /// counted separately.
    pub actionable_to_gross_additional_headroom_bps_p50_exact: Option<ExactRationalV1>,
    pub actionable_to_gross_additional_headroom_bps_p95_exact: Option<ExactRationalV1>,
    pub zero_gross_headroom_observation_count: u64,
    pub current_underwriting_unsatisfied_observation_count: u64,
    pub current_global_health_floor_unsatisfied_observation_count: u64,
    pub constraints: Vec<DuskBorrowCapacityConstraintAggregateV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BorrowProjectedTotalDebtAggregateV1 {
    /// Same currently-underwritten matched borrow-prestate population used by
    /// raw-headroom sums; these are risk/LTV levels, never efficiency inputs.
    pub matched_observation_count: u64,
    pub v1_gross_max_debt_observation_sum_usd_micro: ExactRationalV1,
    pub v1_actionable_max_debt_observation_sum_usd_micro: ExactRationalV1,
    pub dusk_gross_max_debt_observation_sum_usd_micro: ExactRationalV1,
    pub dusk_actionable_max_debt_observation_sum_usd_micro: ExactRationalV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotProbeAggregateCellV1 {
    pub size_kind: SpotProbeSizeKindV1,
    pub direction: SpotProbeDirectionV1,
    pub observation_count: u64,
    pub v1_executable_count: u64,
    pub dusk_executable_count: u64,
    pub paired_executable_count: u64,
    pub mean_execution_cost_delta_ppb_exact: Option<ExactRationalV1>,
    pub mean_gross_quote_improvement_ppb_exact: Option<ExactRationalV1>,
    pub mean_net_quote_improvement_ppb_exact: Option<ExactRationalV1>,
    pub mean_slippage_delta_ppb_exact: Option<ExactRationalV1>,
    pub mean_cpmm_equivalent_depth_delta_exact: Option<ExactRationalV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryPnlUnsupportedReasonV1 {
    PassiveLpOwnershipNavNotCaptured,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevenueYieldEvidenceV1 {
    pub window_seconds: u64,
    pub lane_totals: Vec<RevenueUsdLaneTotalV1>,
    pub common_factual_capital_time_usd_micro_seconds: AvailableExactRationalV1,
    pub common_factual_capital_returns: Vec<RevenueReturnEvidenceV1>,
    pub actual_dusk_external_lp_apy_bps_exact: AvailableExactRationalV1,
    pub actual_dusk_hlp_apy_bps_exact: AvailableExactRationalV1,
    pub inventory_pnl_exact: Option<ExactRationalV1>,
    pub inventory_pnl_unsupported_reason: InventoryPnlUnsupportedReasonV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateScoreEvidenceV1 {
    pub schema_version: u16,
    pub profile_id: String,
    pub arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub window_start_unix_seconds: i64,
    pub window_end_unix_seconds_exclusive: i64,
    pub paired_metric_ledger_sha256: String,
    pub spot_probes_enabled: bool,
    pub spot_probe_sizing_source_sha256: Option<String>,
    pub spot_probe_sets_sha256: String,
    /// Domain-separated commitment to the exact market population used by
    /// this score. This prevents a cached score from surviving an eligibility
    /// or common-window population change while the replay bytes stay fixed.
    pub included_markets_sha256: String,
    pub included_market_count: u64,
    pub historical_price_provider: String,
    pub historical_price_rows: u64,
    pub missing_price_keys: BTreeSet<String>,
    pub blocking_reasons: BTreeSet<String>,
    pub stakeholder_revenue_v1_usd_micro: AvailableExactRationalV1,
    pub stakeholder_revenue_dusk_user_usd_micro: AvailableExactRationalV1,
    pub stakeholder_revenue_dusk_synthetic_arbitrage_usd_micro: AvailableExactRationalV1,
    pub stakeholder_revenue_dusk_synthetic_protocol_settlement_usd_micro: AvailableExactRationalV1,
    pub factual_capital_time_usd_micro_seconds: ExactRationalV1,
    pub v1_actionable_headroom_matched_observation_sum_usd_micro: ExactRationalV1,
    pub v1_gross_headroom_matched_observation_sum_usd_micro: ExactRationalV1,
    pub dusk_actionable_headroom_matched_observation_sum_usd_micro: ExactRationalV1,
    pub dusk_capacity_decomposition: DuskBorrowCapacityAggregateV1,
    pub projected_total_debt_capacity: BorrowProjectedTotalDebtAggregateV1,
    pub factual_swap_input_notional_usd_micro: ExactRationalV1,
    pub v1_debt_increase_notional_usd_micro: ExactRationalV1,
    pub dusk_debt_increase_notional_usd_micro: ExactRationalV1,
    pub dusk_unmet_borrow_notional_usd_micro: ExactRationalV1,
    pub v1_bad_debt_usd_micro: ExactRationalV1,
    pub dusk_bad_debt_usd_micro: ExactRationalV1,
    pub volume_weighted_execution_cost_delta_ppb_exact: Option<ExactRationalV1>,
    pub p95_slippage_delta_ppb: Option<SignedTokenAmount>,
    pub capital_weighted_stakeholder_revenue_uplift_bps_exact: Option<ExactRationalV1>,
    pub equal_market_stakeholder_revenue_uplift_bps: Option<SignedTokenAmount>,
    pub borrow_capacity_uplift_bps_exact: Option<ExactRationalV1>,
    pub bad_debt_delta_usd_micro_exact: Option<ExactRationalV1>,
    pub tail_liquidation_shortfall_delta_usd_micro: Option<SignedTokenAmount>,
    pub spot_probes: Vec<SpotProbeAggregateCellV1>,
    pub revenue_yield: RevenueYieldEvidenceV1,
    pub markets: Vec<MarketCandidateEvidenceV1>,
    /// Present only when every gate input is complete and representable in the
    /// frozen calibration schema.
    pub candidate_score: Option<CandidateScoreV1>,
}

struct HistoricalPriceIndex {
    provider: String,
    rows: u64,
    marks: BTreeMap<(String, i64), DecimalPrice>,
}

#[derive(Clone)]
struct DecimalPrice {
    coefficient: BigInt,
    scale: BigInt,
}

impl HistoricalPriceIndex {
    fn from_jsonl(path: &Path, provider: &str) -> Result<Self> {
        if provider.trim().is_empty() {
            bail!("historical price provider is empty");
        }
        let mut marks = BTreeMap::new();
        let mut rows = 0_u64;
        for (index, line) in
            BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?)
                .lines()
                .enumerate()
        {
            let line = line.with_context(|| format!("reading price line {}", index + 1))?;
            if line.trim().is_empty() {
                continue;
            }
            let mark: HistoricalPriceMarkV1 = serde_json::from_str(&line)
                .with_context(|| format!("decoding price line {}", index + 1))?;
            mark.validate()?;
            if mark.provider != provider {
                continue;
            }
            let key = (mark.mint.clone(), mark.bucket_unix_seconds);
            let price = DecimalPrice::parse(mark.price_usd.as_str())?;
            if marks.insert(key, price).is_some() {
                bail!("duplicate historical provider mark for one mint/hour");
            }
            rows = rows
                .checked_add(1)
                .context("historical price row count overflow")?;
        }
        if marks.is_empty() {
            bail!("no historical marks exist for configured provider {provider}");
        }
        Ok(Self {
            provider: provider.to_owned(),
            rows,
            marks,
        })
    }

    fn value_atoms(
        &self,
        mint: &str,
        decimals: u8,
        unix_seconds: i64,
        atoms: u128,
        missing: &mut BTreeSet<String>,
    ) -> Result<Option<BigRational>> {
        // Zero contributes exactly zero and does not require a price mark.
        // This is essential for lane-level completeness: a structurally
        // present but unused lane must not manufacture a missing-price gate.
        if atoms == 0 {
            return Ok(Some(BigRational::zero()));
        }
        let bucket = unix_seconds.div_euclid(HOUR_SECONDS) * HOUR_SECONDS;
        let Some(price) = self.marks.get(&(mint.to_owned(), bucket)) else {
            missing.insert(format!("{mint}@{bucket}"));
            return Ok(None);
        };
        let atom_scale = pow10_big(decimals);
        Ok(Some(BigRational::new(
            BigInt::from(atoms) * &price.coefficient * BigInt::from(USD_MICRO_SCALE),
            atom_scale * &price.scale,
        )))
    }
}

#[derive(Clone, Default)]
struct RevenueLaneAccumulator {
    value_usd_micro: BigRational,
    missing_price_keys: BTreeSet<String>,
}

impl DecimalPrice {
    fn parse(value: &str) -> Result<Self> {
        let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
        let digits = format!("{integer}{fraction}");
        let coefficient = BigInt::from_str(&digits).context("invalid exact historical price")?;
        if coefficient <= BigInt::zero() {
            bail!("historical price is not positive");
        }
        Ok(Self {
            coefficient,
            scale: BigInt::from(10_u8).pow(u32::try_from(fraction.len())?),
        })
    }
}

#[derive(Default)]
struct AggregateTotals {
    weighted_execution_cost_numerator: BigRational,
    execution_weight: BigRational,
    slippage_deltas: Vec<i128>,
    revenue_lanes:
        BTreeMap<(RevenueVenueV1, RevenueOriginV1, RevenueLaneV1), RevenueLaneAccumulator>,
    factual_capital_time: BigRational,
    factual_capital_missing_price_keys: BTreeSet<String>,
    v1_borrow_capacity: BigRational,
    v1_gross_borrow_capacity: BigRational,
    dusk_borrow_capacity: BigRational,
    dusk_gross_borrow_capacity: BigRational,
    dusk_gross_to_actionable_capacity_loss: BigRational,
    v1_gross_projected_total_debt: BigRational,
    v1_actionable_projected_total_debt: BigRational,
    dusk_gross_projected_total_debt: BigRational,
    dusk_actionable_projected_total_debt: BigRational,
    dusk_actionable_to_gross_bps: Vec<BigRational>,
    dusk_zero_gross_capacity_observations: u64,
    dusk_current_underwriting_unsatisfied_observations: u64,
    dusk_current_global_health_floor_unsatisfied_observations: u64,
    dusk_limiting_constraint_counts: BTreeMap<DuskBorrowCapacityConstraintV1, u64>,
    dusk_tied_constraint_capacity_loss: BTreeMap<DuskBorrowCapacityConstraintV1, BigRational>,
    v1_borrowed_notional: BigRational,
    dusk_borrowed_notional: BigRational,
    dusk_unmet_borrow_notional: BigRational,
    v1_bad_debt: BigRational,
    dusk_bad_debt: BigRational,
    liquidations: BTreeMap<String, (BigRational, BigRational)>,
    spot_probes: BTreeMap<(SpotProbeSizeKindV1, SpotProbeDirectionV1), SpotProbeAccumulator>,
}

#[derive(Default)]
struct SpotProbeAccumulator {
    observation_count: u64,
    v1_executable_count: u64,
    dusk_executable_count: u64,
    paired_executable_count: u64,
    execution_cost_delta_sum: BigRational,
    gross_quote_improvement_sum: BigRational,
    net_quote_improvement_sum: BigRational,
    slippage_delta_sum: BigRational,
    depth_delta_sum: BigRational,
    depth_pair_count: u64,
}

struct MarketAggregateState {
    token0: String,
    token1: String,
    token0_decimals: u8,
    token1_decimals: u8,
    last_time: i64,
    capital: CapitalCheckpointV1,
    totals: AggregateTotals,
    swap_count: u64,
    borrow_count: u64,
    liquidation_count: u64,
    seeded_before_window: bool,
    missing_price_keys: BTreeSet<String>,
    blocking_reasons: BTreeSet<String>,
}

/// Aggregates one paired metric ledger over an explicit chronological split.
/// The caller supplies the already-verified ledger summary so the score is
/// hash-bound to the replay artifact rather than a filename.
#[allow(clippy::too_many_arguments)]
pub fn aggregate_candidate_score(
    paired_metrics_path: &Path,
    paired_summary: &PairedMetricLedgerSummaryV1,
    historical_prices_path: &Path,
    historical_price_provider: &str,
    expected_profile_id: &str,
    expected_arm: ReplayArm,
    window_start_unix_seconds: i64,
    window_end_unix_seconds_exclusive: i64,
) -> Result<CandidateScoreEvidenceV1> {
    aggregate_candidate_score_for_markets(
        paired_metrics_path,
        paired_summary,
        historical_prices_path,
        historical_price_provider,
        expected_profile_id,
        expected_arm,
        window_start_unix_seconds,
        window_end_unix_seconds_exclusive,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn aggregate_candidate_score_for_markets(
    paired_metrics_path: &Path,
    paired_summary: &PairedMetricLedgerSummaryV1,
    historical_prices_path: &Path,
    historical_price_provider: &str,
    expected_profile_id: &str,
    expected_arm: ReplayArm,
    window_start_unix_seconds: i64,
    window_end_unix_seconds_exclusive: i64,
    included_markets: Option<&BTreeSet<String>>,
) -> Result<CandidateScoreEvidenceV1> {
    if window_start_unix_seconds >= window_end_unix_seconds_exclusive {
        bail!("candidate aggregation window is empty");
    }
    if paired_summary.profile_id != expected_profile_id {
        bail!("paired metric summary belongs to another profile");
    }
    if paired_summary.schema_version != crate::metric_engine::PAIRED_METRIC_SCHEMA_VERSION {
        bail!("paired metric summary has an unsupported schema version");
    }
    if paired_summary.spot_probes_enabled
        && paired_summary
            .spot_probe_sizing_window
            .as_ref()
            .is_none_or(|window| {
                window.start_unix_seconds != window_start_unix_seconds
                    || window.end_unix_seconds_exclusive != window_end_unix_seconds_exclusive
            })
    {
        bail!("spot probe sizing window differs from the candidate scoring window");
    }
    let prices =
        HistoricalPriceIndex::from_jsonl(historical_prices_path, historical_price_provider)?;
    let mut missing = BTreeSet::new();
    let mut blocking = BTreeSet::new();
    let mut markets = BTreeMap::<String, MarketAggregateState>::new();
    let expected_included_markets = included_markets.cloned();
    let mut observed_rows = 0_u64;
    let mut observed_probe_rows = 0_u64;
    let mut observed_probe_count = 0_u64;
    let mut observed_probe_sizing_source = None::<String>;
    let mut observed_probe_digest = Sha256::new();

    for (index, line) in BufReader::new(
        File::open(paired_metrics_path)
            .with_context(|| format!("opening {}", paired_metrics_path.display()))?,
    )
    .lines()
    .enumerate()
    {
        let line = line.with_context(|| format!("reading paired metric line {}", index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let row: PairedOperationMetricRowV1 = serde_json::from_str(&line)
            .with_context(|| format!("decoding paired metric line {}", index + 1))?;
        if row.profile_id != expected_profile_id
            || row.arm != expected_arm
            || row.hlp_scenario != paired_summary.hlp_scenario
        {
            bail!("paired metric row belongs to another profile, replay arm, or hLP scenario");
        }
        observed_rows = observed_rows
            .checked_add(1)
            .context("paired row count overflow")?;
        if let Some(probes) = &row.spot_probes {
            probes.validate_hash()?;
            if probes.profile_id != row.profile_id
                || probes.arm != row.arm
                || probes.hlp_scenario != row.hlp_scenario
                || probes.operation_id != row.operation_id
                || probes.market != row.market
                || probes.slot != row.order.slot
                || probes.factual_record_sha256 != row.factual_record_sha256
            {
                bail!("paired spot probe identity differs from its metric row");
            }
            if observed_probe_sizing_source
                .as_ref()
                .is_some_and(|source| source != &probes.sizing_source_sha256)
            {
                bail!("paired metric rows mix spot probe sizing sources");
            }
            observed_probe_sizing_source.get_or_insert_with(|| probes.sizing_source_sha256.clone());
            observed_probe_rows = observed_probe_rows
                .checked_add(1)
                .context("spot probe row count overflow")?;
            observed_probe_count = observed_probe_count
                .checked_add(u64::try_from(probes.observations.len())?)
                .context("spot probe observation count overflow")?;
            observed_probe_digest.update(probes.probe_set_sha256.as_bytes());
            observed_probe_digest.update(b"\n");
        }
        if included_markets.is_some_and(|included| !included.contains(&row.market)) {
            continue;
        }
        let state = markets
            .entry(row.market.clone())
            .or_insert_with(|| MarketAggregateState {
                token0: row.token0.clone(),
                token1: row.token1.clone(),
                token0_decimals: row.token0_decimals,
                token1_decimals: row.token1_decimals,
                last_time: row.block_time_unix_seconds,
                capital: row.capital_after.clone(),
                totals: AggregateTotals::default(),
                swap_count: 0,
                borrow_count: 0,
                liquidation_count: 0,
                seeded_before_window: row.block_time_unix_seconds <= window_start_unix_seconds,
                missing_price_keys: BTreeSet::new(),
                blocking_reasons: BTreeSet::new(),
            });
        if state.token0 != row.token0
            || state.token1 != row.token1
            || state.token0_decimals != row.token0_decimals
            || state.token1_decimals != row.token1_decimals
        {
            bail!("paired metric token identity changed within one market");
        }
        if row.block_time_unix_seconds < state.last_time {
            bail!("paired metric block time regressed within one market");
        }
        integrate_capital_until(
            state,
            row.block_time_unix_seconds,
            window_start_unix_seconds,
            window_end_unix_seconds_exclusive,
            &prices,
        )?;
        if row.block_time_unix_seconds <= window_start_unix_seconds {
            state.seeded_before_window = true;
        }
        if (window_start_unix_seconds..window_end_unix_seconds_exclusive)
            .contains(&row.block_time_unix_seconds)
        {
            observe_window_row(state, &row, &prices)?;
        }
        state.capital = row.capital_after;
        state.last_time = row.block_time_unix_seconds;
    }
    if observed_rows != paired_summary.row_count {
        bail!("paired metric summary row count differs from the ledger");
    }
    if observed_probe_rows != paired_summary.spot_probe_rows
        || observed_probe_count != paired_summary.spot_probe_observation_count
        || observed_probe_sizing_source != paired_summary.spot_probe_sizing_source_sha256
        || hex::encode(observed_probe_digest.finalize()) != paired_summary.spot_probe_sets_sha256
    {
        bail!("paired metric spot probe summary differs from the ledger");
    }

    if let Some(expected) = &expected_included_markets {
        let observed: BTreeSet<String> = markets.keys().cloned().collect();
        for missing_market in expected.difference(&observed) {
            blocking.insert(format!(
                "included_market_has_no_replay_rows:{missing_market}"
            ));
        }
    }

    for state in markets.values_mut() {
        if !state.seeded_before_window {
            state
                .blocking_reasons
                .insert("capital_state_missing_at_window_start".to_owned());
        }
        integrate_capital_until(
            state,
            window_end_unix_seconds_exclusive,
            window_start_unix_seconds,
            window_end_unix_seconds_exclusive,
            &prices,
        )?;
        if !state.missing_price_keys.is_empty() {
            state
                .blocking_reasons
                .insert("missing_historical_usd_marks".to_owned());
        }
        missing.extend(state.missing_price_keys.iter().cloned());
        blocking.extend(state.blocking_reasons.iter().cloned());
    }
    if !missing.is_empty() {
        blocking.insert("missing_historical_usd_marks".to_owned());
    }

    let duration = BigRational::from_integer(BigInt::from(
        window_end_unix_seconds_exclusive - window_start_unix_seconds,
    ));
    let mut aggregate = AggregateTotals::default();
    let mut market_evidence = Vec::with_capacity(markets.len());
    let mut equal_market_uplifts = Vec::new();
    for (market, mut state) in markets {
        add_totals(&mut aggregate, &state.totals)?;
        let market_v1_revenue = revenue_component_value(
            &state.totals,
            RevenueVenueV1::V1,
            RevenueOriginV1::FactualUserFlow,
            RevenueReturnComponentV1::AllStakeholderEconomic,
        )?;
        let market_dusk_user_revenue = revenue_component_value(
            &state.totals,
            RevenueVenueV1::Dusk,
            RevenueOriginV1::FactualUserFlow,
            RevenueReturnComponentV1::AllStakeholderEconomic,
        )?;
        let market_dusk_arb_revenue = revenue_component_value(
            &state.totals,
            RevenueVenueV1::Dusk,
            RevenueOriginV1::SyntheticArbitrage,
            RevenueReturnComponentV1::AllStakeholderEconomic,
        )?;
        let market_dusk_settlement_revenue = revenue_component_value(
            &state.totals,
            RevenueVenueV1::Dusk,
            RevenueOriginV1::SyntheticProtocolSettlement,
            RevenueReturnComponentV1::AllStakeholderEconomic,
        )?;
        let uplift = match (
            available_rational(&market_v1_revenue)?,
            available_rational(&market_dusk_user_revenue)?,
        ) {
            (Some(v1), Some(dusk))
                if state.totals.factual_capital_missing_price_keys.is_empty() =>
            {
                revenue_uplift_bps(&v1, &dusk, &state.totals.factual_capital_time, &duration)
            }
            _ => None,
        };
        if let Some(value) = &uplift {
            equal_market_uplifts.push(rational_to_i128_trunc(value)?);
        }
        let market_execution_cost = nonzero_ratio(
            &state.totals.weighted_execution_cost_numerator,
            &state.totals.execution_weight,
        );
        let market_p95 = empirical_quantile(&mut state.totals.slippage_deltas, 95, 100);
        let market_spot_probes = spot_probe_evidence(&state.totals.spot_probes)?;
        let market_revenue_yield = revenue_yield_evidence(
            &state.totals,
            window_end_unix_seconds_exclusive - window_start_unix_seconds,
        )?;
        let market_capacity_decomposition =
            dusk_borrow_capacity_evidence(&mut state.totals, state.borrow_count)?;
        let market_projected_total_debt_capacity = projected_total_debt_capacity_evidence(
            &state.totals,
            market_capacity_decomposition.underwriting_comparable_observation_count,
        )?;
        let market_borrow_uplift = borrow_capacity_uplift(&state.totals);
        let market_bad_debt_delta = &state.totals.dusk_bad_debt - &state.totals.v1_bad_debt;
        let market_tail_delta = if state
            .blocking_reasons
            .contains("dusk_liquidation_replay_incomplete")
        {
            None
        } else {
            liquidation_tail_delta(&state.totals.liquidations, state.liquidation_count)?
        };
        let mut market_blocking = state.blocking_reasons.clone();
        add_candidate_component_blockers(
            &mut market_blocking,
            market_execution_cost.is_none(),
            uplift.is_none(),
            uplift.is_none(),
            market_borrow_uplift.is_none(),
            market_p95.is_none(),
            market_tail_delta.is_none(),
        );
        let market_candidate_score = candidate_score_from_components(
            expected_profile_id,
            paired_summary.hlp_scenario,
            &market_bad_debt_delta,
            market_tail_delta,
            market_execution_cost.as_ref(),
            market_p95,
            uplift.as_ref(),
            uplift.as_ref(),
            market_borrow_uplift.as_ref(),
            &market_blocking,
        )?;
        market_evidence.push(MarketCandidateEvidenceV1 {
            market,
            missing_price_keys: state.missing_price_keys,
            blocking_reasons: market_blocking,
            stakeholder_revenue_v1_usd_micro: market_v1_revenue,
            stakeholder_revenue_dusk_user_usd_micro: market_dusk_user_revenue,
            stakeholder_revenue_dusk_synthetic_arbitrage_usd_micro: market_dusk_arb_revenue,
            stakeholder_revenue_dusk_synthetic_protocol_settlement_usd_micro:
                market_dusk_settlement_revenue,
            factual_capital_time_usd_micro_seconds: ExactRationalV1::from_ratio(
                &state.totals.factual_capital_time,
            )?,
            volume_weighted_execution_cost_delta_ppb_exact: market_execution_cost
                .as_ref()
                .map(ExactRationalV1::from_ratio)
                .transpose()?,
            p95_slippage_delta_ppb: market_p95.map(signed).transpose()?,
            v1_actionable_headroom_matched_observation_sum_usd_micro: ExactRationalV1::from_ratio(
                &state.totals.v1_borrow_capacity,
            )?,
            v1_gross_headroom_matched_observation_sum_usd_micro: ExactRationalV1::from_ratio(
                &state.totals.v1_gross_borrow_capacity,
            )?,
            dusk_actionable_headroom_matched_observation_sum_usd_micro:
                ExactRationalV1::from_ratio(&state.totals.dusk_borrow_capacity)?,
            dusk_capacity_decomposition: market_capacity_decomposition,
            projected_total_debt_capacity: market_projected_total_debt_capacity,
            v1_debt_increase_notional_usd_micro: ExactRationalV1::from_ratio(
                &state.totals.v1_borrowed_notional,
            )?,
            dusk_debt_increase_notional_usd_micro: ExactRationalV1::from_ratio(
                &state.totals.dusk_borrowed_notional,
            )?,
            dusk_unmet_borrow_notional_usd_micro: ExactRationalV1::from_ratio(
                &state.totals.dusk_unmet_borrow_notional,
            )?,
            v1_bad_debt_usd_micro: ExactRationalV1::from_ratio(&state.totals.v1_bad_debt)?,
            dusk_bad_debt_usd_micro: ExactRationalV1::from_ratio(&state.totals.dusk_bad_debt)?,
            bad_debt_delta_usd_micro_exact: ExactRationalV1::from_ratio(&market_bad_debt_delta)?,
            tail_liquidation_shortfall_delta_usd_micro: market_tail_delta
                .map(signed)
                .transpose()?,
            borrow_capacity_uplift_bps_exact: if state.borrow_count == 0 {
                None
            } else {
                market_borrow_uplift
                    .as_ref()
                    .map(ExactRationalV1::from_ratio)
                    .transpose()?
            },
            revenue_uplift_on_factual_capital_bps: uplift
                .as_ref()
                .map(rational_to_i128_trunc)
                .transpose()?
                .map(signed)
                .transpose()?,
            swap_count: state.swap_count,
            borrow_count: state.borrow_count,
            liquidation_count: state.liquidation_count,
            spot_probes: market_spot_probes,
            revenue_yield: market_revenue_yield,
            candidate_score: market_candidate_score,
        });
    }

    let execution_cost = nonzero_ratio(
        &aggregate.weighted_execution_cost_numerator,
        &aggregate.execution_weight,
    );
    let aggregate_v1_revenue = revenue_component_value(
        &aggregate,
        RevenueVenueV1::V1,
        RevenueOriginV1::FactualUserFlow,
        RevenueReturnComponentV1::AllStakeholderEconomic,
    )?;
    let aggregate_dusk_user_revenue = revenue_component_value(
        &aggregate,
        RevenueVenueV1::Dusk,
        RevenueOriginV1::FactualUserFlow,
        RevenueReturnComponentV1::AllStakeholderEconomic,
    )?;
    let aggregate_dusk_arb_revenue = revenue_component_value(
        &aggregate,
        RevenueVenueV1::Dusk,
        RevenueOriginV1::SyntheticArbitrage,
        RevenueReturnComponentV1::AllStakeholderEconomic,
    )?;
    let aggregate_dusk_settlement_revenue = revenue_component_value(
        &aggregate,
        RevenueVenueV1::Dusk,
        RevenueOriginV1::SyntheticProtocolSettlement,
        RevenueReturnComponentV1::AllStakeholderEconomic,
    )?;
    let capital_uplift = match (
        available_rational(&aggregate_v1_revenue)?,
        available_rational(&aggregate_dusk_user_revenue)?,
    ) {
        (Some(v1), Some(dusk)) if aggregate.factual_capital_missing_price_keys.is_empty() => {
            revenue_uplift_bps(&v1, &dusk, &aggregate.factual_capital_time, &duration)
        }
        _ => None,
    };
    let borrow_uplift = borrow_capacity_uplift(&aggregate);
    let p95_slippage = empirical_quantile(&mut aggregate.slippage_deltas, 95, 100);
    let equal_market = empirical_quantile(&mut equal_market_uplifts, 1, 2);
    let bad_debt_delta = &aggregate.dusk_bad_debt - &aggregate.v1_bad_debt;
    let window_liquidation_count: u64 = market_evidence
        .iter()
        .map(|market| market.liquidation_count)
        .sum();
    let tail_delta = if blocking.contains("dusk_liquidation_replay_incomplete") {
        None
    } else {
        liquidation_tail_delta(&aggregate.liquidations, window_liquidation_count)?
    };
    let spot_probes = spot_probe_evidence(&aggregate.spot_probes)?;
    let revenue_yield = revenue_yield_evidence(
        &aggregate,
        window_end_unix_seconds_exclusive - window_start_unix_seconds,
    )?;
    let aggregate_borrow_count = market_evidence.iter().try_fold(0_u64, |count, market| {
        count
            .checked_add(market.borrow_count)
            .context("aggregate borrow count overflow")
    })?;
    let dusk_capacity_decomposition =
        dusk_borrow_capacity_evidence(&mut aggregate, aggregate_borrow_count)?;
    let projected_total_debt_capacity = projected_total_debt_capacity_evidence(
        &aggregate,
        dusk_capacity_decomposition.underwriting_comparable_observation_count,
    )?;

    add_candidate_component_blockers(
        &mut blocking,
        execution_cost.is_none(),
        capital_uplift.is_none(),
        equal_market.is_none(),
        borrow_uplift.is_none(),
        p95_slippage.is_none(),
        tail_delta.is_none(),
    );
    let equal_market_exact =
        equal_market.map(|value| BigRational::from_integer(BigInt::from(value)));

    let candidate_score = candidate_score_from_components(
        expected_profile_id,
        paired_summary.hlp_scenario,
        &bad_debt_delta,
        tail_delta,
        execution_cost.as_ref(),
        p95_slippage,
        capital_uplift.as_ref(),
        equal_market_exact.as_ref(),
        borrow_uplift.as_ref(),
        &blocking,
    )?;

    let scored_markets: BTreeSet<String> = markets_for_commitment(
        expected_included_markets.as_ref(),
        market_evidence.iter().map(|market| market.market.as_str()),
    );
    Ok(CandidateScoreEvidenceV1 {
        schema_version: AGGREGATE_SCHEMA_VERSION,
        profile_id: expected_profile_id.to_owned(),
        arm: expected_arm,
        hlp_scenario: paired_summary.hlp_scenario,
        window_start_unix_seconds,
        window_end_unix_seconds_exclusive,
        paired_metric_ledger_sha256: paired_summary.ledger_sha256.clone(),
        spot_probes_enabled: paired_summary.spot_probes_enabled,
        spot_probe_sizing_source_sha256: paired_summary.spot_probe_sizing_source_sha256.clone(),
        spot_probe_sets_sha256: paired_summary.spot_probe_sets_sha256.clone(),
        included_markets_sha256: market_set_sha256(&scored_markets),
        included_market_count: u64::try_from(scored_markets.len())?,
        historical_price_provider: prices.provider,
        historical_price_rows: prices.rows,
        missing_price_keys: missing,
        blocking_reasons: blocking,
        stakeholder_revenue_v1_usd_micro: aggregate_v1_revenue,
        stakeholder_revenue_dusk_user_usd_micro: aggregate_dusk_user_revenue,
        stakeholder_revenue_dusk_synthetic_arbitrage_usd_micro: aggregate_dusk_arb_revenue,
        stakeholder_revenue_dusk_synthetic_protocol_settlement_usd_micro:
            aggregate_dusk_settlement_revenue,
        factual_capital_time_usd_micro_seconds: ExactRationalV1::from_ratio(
            &aggregate.factual_capital_time,
        )?,
        v1_actionable_headroom_matched_observation_sum_usd_micro: ExactRationalV1::from_ratio(
            &aggregate.v1_borrow_capacity,
        )?,
        v1_gross_headroom_matched_observation_sum_usd_micro: ExactRationalV1::from_ratio(
            &aggregate.v1_gross_borrow_capacity,
        )?,
        dusk_actionable_headroom_matched_observation_sum_usd_micro: ExactRationalV1::from_ratio(
            &aggregate.dusk_borrow_capacity,
        )?,
        dusk_capacity_decomposition,
        projected_total_debt_capacity,
        factual_swap_input_notional_usd_micro: ExactRationalV1::from_ratio(
            &aggregate.execution_weight,
        )?,
        v1_debt_increase_notional_usd_micro: ExactRationalV1::from_ratio(
            &aggregate.v1_borrowed_notional,
        )?,
        dusk_debt_increase_notional_usd_micro: ExactRationalV1::from_ratio(
            &aggregate.dusk_borrowed_notional,
        )?,
        dusk_unmet_borrow_notional_usd_micro: ExactRationalV1::from_ratio(
            &aggregate.dusk_unmet_borrow_notional,
        )?,
        v1_bad_debt_usd_micro: ExactRationalV1::from_ratio(&aggregate.v1_bad_debt)?,
        dusk_bad_debt_usd_micro: ExactRationalV1::from_ratio(&aggregate.dusk_bad_debt)?,
        volume_weighted_execution_cost_delta_ppb_exact: execution_cost
            .as_ref()
            .map(ExactRationalV1::from_ratio)
            .transpose()?,
        p95_slippage_delta_ppb: p95_slippage.map(signed).transpose()?,
        capital_weighted_stakeholder_revenue_uplift_bps_exact: capital_uplift
            .as_ref()
            .map(ExactRationalV1::from_ratio)
            .transpose()?,
        equal_market_stakeholder_revenue_uplift_bps: equal_market.map(signed).transpose()?,
        borrow_capacity_uplift_bps_exact: borrow_uplift
            .as_ref()
            .map(ExactRationalV1::from_ratio)
            .transpose()?,
        bad_debt_delta_usd_micro_exact: Some(ExactRationalV1::from_ratio(&bad_debt_delta)?),
        tail_liquidation_shortfall_delta_usd_micro: tail_delta.map(signed).transpose()?,
        spot_probes,
        revenue_yield,
        markets: market_evidence,
        candidate_score,
    })
}

fn markets_for_commitment<'a>(
    expected: Option<&BTreeSet<String>>,
    observed: impl Iterator<Item = &'a str>,
) -> BTreeSet<String> {
    expected
        .cloned()
        .unwrap_or_else(|| observed.map(str::to_owned).collect())
}

/// Stable, domain-separated identity for an exact market population.
pub fn market_set_sha256(markets: &BTreeSet<String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"v1-vs-dusk/included-markets/v1\0");
    for market in markets {
        hasher.update((market.len() as u64).to_le_bytes());
        hasher.update(market.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn observe_window_row(
    state: &mut MarketAggregateState,
    row: &PairedOperationMetricRowV1,
    prices: &HistoricalPriceIndex,
) -> Result<()> {
    if row.dusk_status == CounterfactualStatusV1::DeferredExternalState {
        state
            .blocking_reasons
            .insert("counterfactual_replay_deferred_external_state".to_owned());
    }
    if let Some(swap) = &row.swap {
        state.swap_count = state
            .swap_count
            .checked_add(1)
            .context("swap count overflow")?;
        if !row.synthetic_arbitrage {
            if let (Some(v1), Some(cost), Some(slippage)) = (
                swap.v1.as_ref(),
                swap.quoted_execution_cost_delta_ppb.as_ref(),
                swap.slippage_delta_ppb.as_ref(),
            ) {
                let (mint, decimals) = if v1.asset_in_base {
                    (&state.token0, state.token0_decimals)
                } else {
                    (&state.token1, state.token1_decimals)
                };
                if let Some(weight) = prices.value_atoms(
                    mint,
                    decimals,
                    row.block_time_unix_seconds,
                    parse_token(&v1.reserve_credit_in)?,
                    &mut state.missing_price_keys,
                )? {
                    let cost = BigRational::from_integer(BigInt::from(parse_signed(cost)?));
                    state.totals.weighted_execution_cost_numerator += &cost * &weight;
                    state.totals.execution_weight += weight;
                    state.totals.slippage_deltas.push(parse_signed(slippage)?);
                }
            }
        }
    }
    if let Some(probes) = &row.spot_probes {
        for observation in &probes.observations {
            let accumulator = state
                .totals
                .spot_probes
                .entry((observation.size_kind, observation.direction))
                .or_default();
            accumulator.observation_count = accumulator
                .observation_count
                .checked_add(1)
                .context("spot probe aggregate observation count overflow")?;
            let v1_executable = observation
                .v1
                .as_ref()
                .is_some_and(|quote| quote.executable);
            let dusk_executable = observation
                .dusk
                .as_ref()
                .is_some_and(|quote| quote.executable);
            if v1_executable {
                accumulator.v1_executable_count = accumulator
                    .v1_executable_count
                    .checked_add(1)
                    .context("V1 spot probe executable count overflow")?;
            }
            if dusk_executable {
                accumulator.dusk_executable_count = accumulator
                    .dusk_executable_count
                    .checked_add(1)
                    .context("Dusk spot probe executable count overflow")?;
            }
            if let (Some(v1), Some(dusk)) = (&observation.v1, &observation.dusk) {
                if v1.executable && dusk.executable {
                    accumulator.paired_executable_count = accumulator
                        .paired_executable_count
                        .checked_add(1)
                        .context("paired spot probe executable count overflow")?;
                    accumulator.execution_cost_delta_sum +=
                        BigRational::from_integer(BigInt::from(
                            parse_signed(&dusk.execution_cost_ppb)?
                                .checked_sub(parse_signed(&v1.execution_cost_ppb)?)
                                .context("spot probe execution cost delta overflow")?,
                        ));
                    let expected_gross =
                        quote_improvement_ppb(&v1.gross_output, &dusk.gross_output)?;
                    let expected_net = quote_improvement_ppb(&v1.net_output, &dusk.net_output)?;
                    if observation.gross_quote_improvement_ppb.as_ref() != Some(&expected_gross)
                        || observation.net_quote_improvement_ppb.as_ref() != Some(&expected_net)
                    {
                        bail!("spot probe quote-improvement ratios differ from exact outputs");
                    }
                    accumulator.gross_quote_improvement_sum +=
                        BigRational::from_integer(BigInt::from(parse_signed(&expected_gross)?));
                    accumulator.net_quote_improvement_sum +=
                        BigRational::from_integer(BigInt::from(parse_signed(&expected_net)?));
                    accumulator.slippage_delta_sum += BigRational::from_integer(BigInt::from(
                        parse_signed(&dusk.slippage_ppb)?
                            .checked_sub(parse_signed(&v1.slippage_ppb)?)
                            .context("spot probe slippage delta overflow")?,
                    ));
                    if let (Some(v1_depth), Some(dusk_depth)) = (
                        &v1.cpmm_equivalent_depth_per_external_lp_value,
                        &dusk.cpmm_equivalent_depth_per_external_lp_value,
                    ) {
                        accumulator.depth_delta_sum +=
                            dusk_depth.to_rational()? - v1_depth.to_rational()?;
                        accumulator.depth_pair_count = accumulator
                            .depth_pair_count
                            .checked_add(1)
                            .context("spot probe depth pair count overflow")?;
                    }
                }
            }
            if !(v1_executable && dusk_executable)
                && (observation.gross_quote_improvement_ppb.is_some()
                    || observation.net_quote_improvement_ppb.is_some())
            {
                bail!("unpaired spot probe carries quote-improvement evidence");
            }
        }
    }

    let origin =
        revenue_origin_from_flags(row.synthetic_arbitrage, row.synthetic_protocol_settlement)?;
    if row.revenue.dusk_revenue_origin != origin {
        bail!("paired revenue origin differs from row provenance flags");
    }
    // V1 contributes only to factual user-flow rows. The metric writer already
    // requires its four sides to be zero for both synthetic origins; retaining
    // that invariant here prevents a downstream origin reclassification.
    if origin == RevenueOriginV1::FactualUserFlow {
        observe_revenue_sides(
            RevenueVenueV1::V1,
            RevenueOriginV1::FactualUserFlow,
            &row.revenue.v1_base,
            &row.revenue.v1_quote,
            state,
            row.block_time_unix_seconds,
            prices,
        )?;
    } else if RevenueLaneV1::ALL.into_iter().any(|lane| {
        row.revenue.v1_base.amount(lane).as_str() != "0"
            || row.revenue.v1_quote.amount(lane).as_str() != "0"
    }) {
        bail!("synthetic row carries nonzero V1 revenue");
    }
    observe_revenue_sides(
        RevenueVenueV1::Dusk,
        origin,
        &row.revenue.dusk_base,
        &row.revenue.dusk_quote,
        state,
        row.block_time_unix_seconds,
        prices,
    )?;

    if let Some(borrow) = &row.borrow {
        state.borrow_count = state
            .borrow_count
            .checked_add(1)
            .context("borrow count overflow")?;
        let (mint, decimals) = if borrow.debt_asset_base {
            (&state.token0, state.token0_decimals)
        } else {
            (&state.token1, state.token1_decimals)
        };
        let v1_gross_headroom = parse_token(&borrow.v1_gross_additional_headroom)?;
        let v1_actionable_headroom = parse_token(&borrow.v1_actionable_additional_headroom)?;
        let dusk_gross_headroom = parse_token(&borrow.dusk_gross_additional_headroom)?;
        let dusk_actionable_headroom = parse_token(&borrow.dusk_actionable_additional_headroom)?;
        if v1_actionable_headroom > v1_gross_headroom
            || dusk_actionable_headroom > dusk_gross_headroom
        {
            bail!("actionable raw borrow headroom exceeds gross raw headroom");
        }
        let unique_constraints: BTreeSet<_> =
            borrow.dusk_limiting_constraints.iter().copied().collect();
        if unique_constraints.len() != borrow.dusk_limiting_constraints.len() {
            bail!("Dusk borrow capacity contains duplicate limiting constraints");
        }
        if !borrow.dusk_current_underwriting_satisfied {
            state
                .totals
                .dusk_current_underwriting_unsatisfied_observations = state
                .totals
                .dusk_current_underwriting_unsatisfied_observations
                .checked_add(1)
                .context("Dusk current-underwriting failure count overflow")?;
        } else {
            for (atoms, target) in [
                (v1_actionable_headroom, &mut state.totals.v1_borrow_capacity),
                (
                    v1_gross_headroom,
                    &mut state.totals.v1_gross_borrow_capacity,
                ),
                (
                    dusk_actionable_headroom,
                    &mut state.totals.dusk_borrow_capacity,
                ),
                (
                    dusk_gross_headroom,
                    &mut state.totals.dusk_gross_borrow_capacity,
                ),
            ] {
                if let Some(value) = prices.value_atoms(
                    mint,
                    decimals,
                    row.block_time_unix_seconds,
                    atoms,
                    &mut state.missing_price_keys,
                )? {
                    *target += value;
                }
            }
            for (atoms, target) in [
                (
                    parse_token(&borrow.v1_gross_admissible_max_debt)?,
                    &mut state.totals.v1_gross_projected_total_debt,
                ),
                (
                    parse_token(&borrow.v1_actionable_max_debt)?,
                    &mut state.totals.v1_actionable_projected_total_debt,
                ),
                (
                    parse_token(&borrow.dusk_gross_admissible_max_debt)?,
                    &mut state.totals.dusk_gross_projected_total_debt,
                ),
                (
                    parse_token(&borrow.dusk_actionable_max_debt)?,
                    &mut state.totals.dusk_actionable_projected_total_debt,
                ),
            ] {
                if let Some(value) = prices.value_atoms(
                    mint,
                    decimals,
                    row.block_time_unix_seconds,
                    atoms,
                    &mut state.missing_price_keys,
                )? {
                    *target += value;
                }
            }
            if dusk_gross_headroom == 0 {
                state.totals.dusk_zero_gross_capacity_observations = state
                    .totals
                    .dusk_zero_gross_capacity_observations
                    .checked_add(1)
                    .context("Dusk zero-gross-headroom count overflow")?;
            } else {
                state
                    .totals
                    .dusk_actionable_to_gross_bps
                    .push(BigRational::new(
                        BigInt::from(dusk_actionable_headroom) * BigInt::from(10_000_u16),
                        BigInt::from(dusk_gross_headroom),
                    ));
            }
            let capacity_loss_atoms = dusk_gross_headroom - dusk_actionable_headroom;
            if let Some(value) = prices.value_atoms(
                mint,
                decimals,
                row.block_time_unix_seconds,
                capacity_loss_atoms,
                &mut state.missing_price_keys,
            )? {
                state.totals.dusk_gross_to_actionable_capacity_loss += &value;
                for constraint in &borrow.dusk_limiting_constraints {
                    *state
                        .totals
                        .dusk_tied_constraint_capacity_loss
                        .entry(*constraint)
                        .or_default() += &value;
                }
            }
            for constraint in unique_constraints {
                let count = state
                    .totals
                    .dusk_limiting_constraint_counts
                    .entry(constraint)
                    .or_default();
                *count = count
                    .checked_add(1)
                    .context("Dusk limiting-constraint count overflow")?;
            }
        }
        if !borrow.dusk_current_global_health_floor_satisfied {
            state
                .totals
                .dusk_current_global_health_floor_unsatisfied_observations = state
                .totals
                .dusk_current_global_health_floor_unsatisfied_observations
                .checked_add(1)
                .context("Dusk current-global-health failure count overflow")?;
        }
        let v1_before = parse_token(&borrow.v1_actual_debt_before)?;
        let v1_after = parse_token(&borrow.v1_actual_debt_after)?;
        let v1_change = v1_after.saturating_sub(v1_before);
        if let Some(value) = prices.value_atoms(
            mint,
            decimals,
            row.block_time_unix_seconds,
            v1_change,
            &mut state.missing_price_keys,
        )? {
            state.totals.v1_borrowed_notional += value;
        }
        if let Some(value) = prices.value_atoms(
            mint,
            decimals,
            row.block_time_unix_seconds,
            parse_token(&borrow.dusk_filled_change)?,
            &mut state.missing_price_keys,
        )? {
            state.totals.dusk_borrowed_notional += value;
        }
        if let Some(value) = prices.value_atoms(
            mint,
            decimals,
            row.block_time_unix_seconds,
            parse_token(&borrow.dusk_unmet_change)?,
            &mut state.missing_price_keys,
        )? {
            state.totals.dusk_unmet_borrow_notional += value;
        }
    }

    if let Some(safety) = &row.safety {
        state.liquidation_count = state
            .liquidation_count
            .checked_add(1)
            .context("liquidation count overflow")?;
        let (mint, decimals) = if safety.debt_asset_base {
            (&state.token0, state.token0_decimals)
        } else {
            (&state.token1, state.token1_decimals)
        };
        let v1 = prices.value_atoms(
            mint,
            decimals,
            row.block_time_unix_seconds,
            parse_token(&safety.v1_liquidation_shortfall)?,
            &mut state.missing_price_keys,
        )?;
        let dusk = safety
            .dusk_liquidation_shortfall
            .as_ref()
            .map(|amount| {
                prices.value_atoms(
                    mint,
                    decimals,
                    row.block_time_unix_seconds,
                    parse_token(amount)?,
                    &mut state.missing_price_keys,
                )
            })
            .transpose()?
            .flatten();
        match (v1, dusk) {
            (Some(v1), Some(dusk)) => {
                state.totals.v1_bad_debt += &v1;
                state.totals.dusk_bad_debt += &dusk;
                let entry = state
                    .totals
                    .liquidations
                    .entry(safety.source_operation_id.clone())
                    .or_insert_with(|| (BigRational::zero(), BigRational::zero()));
                entry.0 += v1;
                entry.1 += dusk;
            }
            _ => {
                state
                    .blocking_reasons
                    .insert("dusk_liquidation_replay_incomplete".to_owned());
            }
        }
    }
    Ok(())
}

fn integrate_capital_until(
    state: &mut MarketAggregateState,
    target_time: i64,
    window_start: i64,
    window_end: i64,
    prices: &HistoricalPriceIndex,
) -> Result<()> {
    let mut cursor = state.last_time.max(window_start);
    let end = target_time.min(window_end);
    while cursor < end {
        let next_hour = (cursor.div_euclid(HOUR_SECONDS) + 1)
            .checked_mul(HOUR_SECONDS)
            .context("hour boundary overflow")?;
        let segment_end = end.min(next_hour);
        let seconds = segment_end - cursor;
        let mut segment_missing = BTreeSet::new();
        if let Some(capital) = value_token_pair(
            &state.token0,
            state.token0_decimals,
            parse_token(&state.capital.v1_reserve0)?,
            &state.token1,
            state.token1_decimals,
            parse_token(&state.capital.v1_reserve1)?,
            cursor,
            prices,
            &mut segment_missing,
        )? {
            state.totals.factual_capital_time +=
                capital * BigRational::from_integer(BigInt::from(seconds));
        }
        state
            .totals
            .factual_capital_missing_price_keys
            .extend(segment_missing.iter().cloned());
        state.missing_price_keys.extend(segment_missing);
        cursor = segment_end;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_revenue_sides(
    venue: RevenueVenueV1,
    origin: RevenueOriginV1,
    base: &RevenueSideDeltaV1,
    quote: &RevenueSideDeltaV1,
    state: &mut MarketAggregateState,
    unix_seconds: i64,
    prices: &HistoricalPriceIndex,
) -> Result<()> {
    for lane in RevenueLaneV1::ALL {
        let base_atoms = parse_token(base.amount(lane))?;
        let quote_atoms = parse_token(quote.amount(lane))?;
        if !revenue_lane_supported(venue, lane) && (base_atoms != 0 || quote_atoms != 0) {
            bail!("venue carries nonzero revenue in a structurally unavailable lane");
        }
        let mut lane_missing = BTreeSet::new();
        let value = value_token_pair(
            &state.token0,
            state.token0_decimals,
            base_atoms,
            &state.token1,
            state.token1_decimals,
            quote_atoms,
            unix_seconds,
            prices,
            &mut lane_missing,
        )?;
        if lane.accounting_class() == RevenueAccountingClassV1::EconomicRevenue {
            state
                .missing_price_keys
                .extend(lane_missing.iter().cloned());
        }
        let accumulator = state
            .totals
            .revenue_lanes
            .entry((venue, origin, lane))
            .or_default();
        accumulator
            .missing_price_keys
            .extend(lane_missing.into_iter());
        if let Some(value) = value {
            accumulator.value_usd_micro += value;
        }
    }
    Ok(())
}

fn revenue_lane_supported(venue: RevenueVenueV1, lane: RevenueLaneV1) -> bool {
    match venue {
        RevenueVenueV1::V1 => !matches!(
            lane,
            RevenueLaneV1::ProtocolBuybackSwapEarned
                | RevenueLaneV1::RetainedRecenterInventory
                | RevenueLaneV1::CompoundedLpSwapPrincipal
                | RevenueLaneV1::ProtocolBuybackPublicInterestCashRealized
                | RevenueLaneV1::HlpFundingInterestEarned
                | RevenueLaneV1::ProtocolFeeAuctionHlpFundingInterestEarned
                | RevenueLaneV1::ProtocolBuybackHlpFundingInterestEarned
        ),
        RevenueVenueV1::Dusk => !matches!(
            lane,
            RevenueLaneV1::FlashloanFeeV1Only | RevenueLaneV1::ProtocolClaimRealizedV1Only
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn value_token_pair(
    mint0: &str,
    decimals0: u8,
    amount0: u128,
    mint1: &str,
    decimals1: u8,
    amount1: u128,
    unix_seconds: i64,
    prices: &HistoricalPriceIndex,
    missing: &mut BTreeSet<String>,
) -> Result<Option<BigRational>> {
    let left = prices.value_atoms(mint0, decimals0, unix_seconds, amount0, missing)?;
    let right = prices.value_atoms(mint1, decimals1, unix_seconds, amount1, missing)?;
    Ok(match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        _ => None,
    })
}

fn revenue_uplift_bps(
    v1: &BigRational,
    dusk: &BigRational,
    capital_time: &BigRational,
    duration: &BigRational,
) -> Option<BigRational> {
    if capital_time.is_zero() {
        None
    } else {
        Some(
            (dusk - v1) * duration * BigRational::from_integer(BigInt::from(10_000_u16))
                / capital_time,
        )
    }
}

fn nonzero_ratio(numerator: &BigRational, denominator: &BigRational) -> Option<BigRational> {
    (!denominator.is_zero()).then(|| numerator / denominator)
}

fn borrow_capacity_uplift(totals: &AggregateTotals) -> Option<BigRational> {
    if totals.v1_borrow_capacity.is_zero() {
        // A missing/zero V1 actionable-headroom denominator is N/A. Candidate
        // scoring must not publish fabricated parity for a spot-only market.
        None
    } else {
        Some(
            (&totals.dusk_borrow_capacity - &totals.v1_borrow_capacity)
                * BigRational::from_integer(BigInt::from(10_000_u16))
                / &totals.v1_borrow_capacity,
        )
    }
}

fn dusk_borrow_capacity_evidence(
    totals: &mut AggregateTotals,
    observation_count: u64,
) -> Result<DuskBorrowCapacityAggregateV1> {
    let underwriting_comparable_observation_count = observation_count
        .checked_sub(totals.dusk_current_underwriting_unsatisfied_observations)
        .context("Dusk underwriting-unsatisfied count exceeds matched observations")?;
    let headroom_ratio_observation_count =
        u64::try_from(totals.dusk_actionable_to_gross_bps.len())?;
    let comparable_partition = headroom_ratio_observation_count
        .checked_add(totals.dusk_zero_gross_capacity_observations)
        .context("Dusk comparable capacity partition count overflow")?;
    if comparable_partition != underwriting_comparable_observation_count {
        bail!(
            "Dusk comparable capacity observations do not partition exactly: expected {underwriting_comparable_observation_count}, observed {comparable_partition}"
        );
    }
    if &totals.dusk_gross_borrow_capacity - &totals.dusk_borrow_capacity
        != totals.dusk_gross_to_actionable_capacity_loss
    {
        bail!(
            "Dusk gross/actionable raw-headroom observation sums do not equal exact capacity loss"
        );
    }
    let p50 = exact_rational_quantile(&mut totals.dusk_actionable_to_gross_bps, 1, 2);
    let p95 = exact_rational_quantile(&mut totals.dusk_actionable_to_gross_bps, 95, 100);
    let constraints = [
        DuskBorrowCapacityConstraintV1::Underwriting,
        DuskBorrowCapacityConstraintV1::GlobalHealthFloor,
        DuskBorrowCapacityConstraintV1::Cash,
        DuskBorrowCapacityConstraintV1::DailyBorrowBucket,
        DuskBorrowCapacityConstraintV1::ReduceOnly,
    ]
    .into_iter()
    .map(|constraint| {
        let tied_loss = totals
            .dusk_tied_constraint_capacity_loss
            .get(&constraint)
            .cloned()
            .unwrap_or_default();
        Ok(DuskBorrowCapacityConstraintAggregateV1 {
            constraint,
            limiting_matched_observation_count: totals
                .dusk_limiting_constraint_counts
                .get(&constraint)
                .copied()
                .unwrap_or(0),
            tied_additional_headroom_loss_usd_micro: ExactRationalV1::from_ratio(&tied_loss)?,
        })
    })
    .collect::<Result<_>>()?;
    Ok(DuskBorrowCapacityAggregateV1 {
        matched_borrow_prestate_observation_count: observation_count,
        underwriting_comparable_observation_count,
        headroom_ratio_observation_count,
        gross_additional_headroom_matched_observation_sum_usd_micro: ExactRationalV1::from_ratio(
            &totals.dusk_gross_borrow_capacity,
        )?,
        actionable_additional_headroom_matched_observation_sum_usd_micro:
            ExactRationalV1::from_ratio(&totals.dusk_borrow_capacity)?,
        gross_to_actionable_additional_headroom_loss_usd_micro: ExactRationalV1::from_ratio(
            &totals.dusk_gross_to_actionable_capacity_loss,
        )?,
        actionable_to_gross_additional_headroom_bps_p50_exact: p50
            .as_ref()
            .map(ExactRationalV1::from_ratio)
            .transpose()?,
        actionable_to_gross_additional_headroom_bps_p95_exact: p95
            .as_ref()
            .map(ExactRationalV1::from_ratio)
            .transpose()?,
        zero_gross_headroom_observation_count: totals.dusk_zero_gross_capacity_observations,
        current_underwriting_unsatisfied_observation_count: totals
            .dusk_current_underwriting_unsatisfied_observations,
        current_global_health_floor_unsatisfied_observation_count: totals
            .dusk_current_global_health_floor_unsatisfied_observations,
        constraints,
    })
}

fn projected_total_debt_capacity_evidence(
    totals: &AggregateTotals,
    matched_observation_count: u64,
) -> Result<BorrowProjectedTotalDebtAggregateV1> {
    Ok(BorrowProjectedTotalDebtAggregateV1 {
        matched_observation_count,
        v1_gross_max_debt_observation_sum_usd_micro: ExactRationalV1::from_ratio(
            &totals.v1_gross_projected_total_debt,
        )?,
        v1_actionable_max_debt_observation_sum_usd_micro: ExactRationalV1::from_ratio(
            &totals.v1_actionable_projected_total_debt,
        )?,
        dusk_gross_max_debt_observation_sum_usd_micro: ExactRationalV1::from_ratio(
            &totals.dusk_gross_projected_total_debt,
        )?,
        dusk_actionable_max_debt_observation_sum_usd_micro: ExactRationalV1::from_ratio(
            &totals.dusk_actionable_projected_total_debt,
        )?,
    })
}

fn liquidation_tail_delta(
    liquidations: &BTreeMap<String, (BigRational, BigRational)>,
    liquidation_count: u64,
) -> Result<Option<i128>> {
    if liquidation_count == 0 {
        return Ok(Some(0));
    }
    let mut deltas: Vec<i128> = liquidations
        .values()
        .map(|(v1, dusk)| rational_to_i128_trunc(&(dusk - v1)))
        .collect::<Result<_>>()?;
    Ok(empirical_quantile(&mut deltas, 95, 100))
}

#[allow(clippy::too_many_arguments)]
fn add_candidate_component_blockers(
    blocking: &mut BTreeSet<String>,
    missing_execution_cost: bool,
    missing_capital: bool,
    missing_equal_market: bool,
    missing_borrow_capacity: bool,
    missing_slippage: bool,
    missing_liquidation_tail: bool,
) {
    for (reason, missing) in [
        ("no_execution_cost_observations", missing_execution_cost),
        ("no_capital_denominator", missing_capital),
        ("no_equal_market_revenue_observations", missing_equal_market),
        ("no_borrow_capacity_observations", missing_borrow_capacity),
        ("no_slippage_observations", missing_slippage),
        ("no_exact_liquidation_tail", missing_liquidation_tail),
    ] {
        if missing {
            blocking.insert(reason.to_owned());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn candidate_score_from_components(
    profile_id: &str,
    hlp_scenario: HlpScenario,
    bad_debt_delta: &BigRational,
    tail_liquidation_delta: Option<i128>,
    execution_cost: Option<&BigRational>,
    p95_slippage: Option<i128>,
    capital_uplift: Option<&BigRational>,
    equal_market_uplift: Option<&BigRational>,
    borrow_uplift: Option<&BigRational>,
    blocking: &BTreeSet<String>,
) -> Result<Option<CandidateScoreV1>> {
    if !blocking.is_empty() {
        return Ok(None);
    }
    let execution = rational_to_i128_trunc(execution_cost.context("checked execution cost")?)?;
    let capital = rational_to_i128_trunc(capital_uplift.context("checked capital uplift")?)?;
    let equal_market =
        rational_to_i128_trunc(equal_market_uplift.context("checked equal-market uplift")?)?;
    let borrow = rational_to_i128_trunc(borrow_uplift.context("checked borrow uplift")?)?;
    let bad_debt = rational_to_i128_trunc(bad_debt_delta)?;
    Ok(Some(CandidateScoreV1 {
        profile_id: profile_id.to_owned(),
        hlp_scenario,
        bad_debt_delta_usd_micro: bad_debt,
        tail_liquidation_shortfall_delta_usd_micro: tail_liquidation_delta
            .context("checked liquidation tail")?,
        volume_weighted_execution_cost_delta_ppb: i64::try_from(execution)?,
        p95_slippage_delta_ppb: i64::try_from(p95_slippage.context("checked slippage")?)?,
        capital_weighted_stakeholder_revenue_uplift_bps: i64::try_from(capital)?,
        equal_market_stakeholder_revenue_uplift_bps: i64::try_from(equal_market)?,
        quote_quality_uplift_ppb: i64::try_from(
            execution.checked_neg().context("quote quality overflow")?,
        )?,
        borrow_capacity_uplift_bps: i64::try_from(borrow)?,
    }))
}

fn revenue_yield_evidence(
    totals: &AggregateTotals,
    window_seconds: i64,
) -> Result<RevenueYieldEvidenceV1> {
    let window_seconds = u64::try_from(window_seconds)
        .context("revenue-yield window must have positive duration")?;
    if window_seconds == 0 {
        bail!("revenue-yield window must have positive duration");
    }
    let lane_totals = revenue_lane_totals(totals)?;
    let capital = if totals.factual_capital_missing_price_keys.is_empty() {
        AvailableExactRationalV1::available(&totals.factual_capital_time)?
    } else {
        AvailableExactRationalV1::unavailable(MetricAvailability::MissingHistoricalUsdMark)
    };
    let mut common_returns = Vec::new();
    for (venue, origin) in revenue_venue_origins() {
        for component in [
            RevenueReturnComponentV1::LpSwap,
            RevenueReturnComponentV1::LpPublicInterestAccrued,
            RevenueReturnComponentV1::ExternalLpCombinedEconomic,
            RevenueReturnComponentV1::LpPublicInterestCashMemo,
            RevenueReturnComponentV1::ProtocolPublicInterestCashMemo,
            RevenueReturnComponentV1::NetHlpCommonCapitalContribution,
            RevenueReturnComponentV1::ProtocolEconomic,
            RevenueReturnComponentV1::AllStakeholderEconomic,
        ] {
            let beneficiary = component_beneficiary(component);
            let revenue = revenue_component_value(totals, venue, origin, component)?;
            let (absolute_return_bps_exact, apr_bps_exact) =
                common_capital_returns(&revenue, &capital, window_seconds)?;
            common_returns.push(RevenueReturnEvidenceV1 {
                venue,
                origin,
                beneficiary,
                component,
                denominator_basis: RevenueDenominatorBasisV1::FactualV1AccountedLpCapitalTime,
                revenue_usd_micro: revenue,
                absolute_return_bps_exact,
                apr_bps_exact,
            });
        }
    }

    let missing_owner =
        AvailableExactRationalV1::unavailable(MetricAvailability::MissingOwnershipDenominator);
    for (beneficiary, component, denominator_basis) in [
        (
            RevenueBeneficiaryV1::ExternalLp,
            RevenueReturnComponentV1::ExternalLpCombinedEconomic,
            RevenueDenominatorBasisV1::ExactDuskExternalLpOwnerClaimCapitalTime,
        ),
        (
            RevenueBeneficiaryV1::Hlp,
            RevenueReturnComponentV1::NetHlpCommonCapitalContribution,
            RevenueDenominatorBasisV1::ExactDuskHlpCapitalTime,
        ),
    ] {
        common_returns.push(RevenueReturnEvidenceV1 {
            venue: RevenueVenueV1::Dusk,
            origin: RevenueOriginV1::FactualUserFlow,
            beneficiary,
            component,
            denominator_basis,
            revenue_usd_micro: revenue_component_value(
                totals,
                RevenueVenueV1::Dusk,
                RevenueOriginV1::FactualUserFlow,
                component,
            )?,
            absolute_return_bps_exact: missing_owner.clone(),
            apr_bps_exact: missing_owner.clone(),
        });
    }
    common_returns.sort_by_key(|row| {
        (
            row.venue,
            row.origin,
            row.denominator_basis as u8,
            row.beneficiary,
            row.component,
        )
    });

    Ok(RevenueYieldEvidenceV1 {
        window_seconds,
        lane_totals,
        common_factual_capital_time_usd_micro_seconds: capital,
        common_factual_capital_returns: common_returns,
        actual_dusk_external_lp_apy_bps_exact: missing_owner.clone(),
        actual_dusk_hlp_apy_bps_exact: missing_owner,
        inventory_pnl_exact: None,
        inventory_pnl_unsupported_reason:
            InventoryPnlUnsupportedReasonV1::PassiveLpOwnershipNavNotCaptured,
    })
}

fn revenue_venue_origins() -> [(RevenueVenueV1, RevenueOriginV1); 4] {
    [
        (RevenueVenueV1::V1, RevenueOriginV1::FactualUserFlow),
        (RevenueVenueV1::Dusk, RevenueOriginV1::FactualUserFlow),
        (RevenueVenueV1::Dusk, RevenueOriginV1::SyntheticArbitrage),
        (
            RevenueVenueV1::Dusk,
            RevenueOriginV1::SyntheticProtocolSettlement,
        ),
    ]
}

fn revenue_lane_totals(totals: &AggregateTotals) -> Result<Vec<RevenueUsdLaneTotalV1>> {
    let mut output = Vec::with_capacity(RevenueLaneV1::ALL.len() * 4);
    for (venue, origin) in revenue_venue_origins() {
        for lane in RevenueLaneV1::ALL {
            let accumulator = totals
                .revenue_lanes
                .get(&(venue, origin, lane))
                .cloned()
                .unwrap_or_default();
            let usd_micro = if !revenue_lane_supported(venue, lane) {
                AvailableExactRationalV1::unavailable(
                    MetricAvailability::NotApplicableVenueMechanism,
                )
            } else if venue == RevenueVenueV1::V1
                && lane == RevenueLaneV1::LpPublicInterestCashRealized
            {
                // V1 has no exact LP cash-realization witness. Its native
                // cash-covered protocol-interest field is independently exposed in
                // the fee-auction protocol cash lane.
                AvailableExactRationalV1::unavailable(
                    MetricAvailability::MissingCashRealizationEvidence,
                )
            } else if accumulator.missing_price_keys.is_empty() {
                AvailableExactRationalV1::available(&accumulator.value_usd_micro)?
            } else {
                AvailableExactRationalV1::unavailable(MetricAvailability::MissingHistoricalUsdMark)
            };
            output.push(RevenueUsdLaneTotalV1 {
                venue,
                origin,
                lane,
                accounting_class: lane.accounting_class(),
                usd_micro,
                missing_price_keys: accumulator.missing_price_keys,
            });
        }
    }
    Ok(output)
}

fn component_beneficiary(component: RevenueReturnComponentV1) -> RevenueBeneficiaryV1 {
    match component {
        RevenueReturnComponentV1::LpSwap
        | RevenueReturnComponentV1::LpPublicInterestAccrued
        | RevenueReturnComponentV1::ExternalLpCombinedEconomic
        | RevenueReturnComponentV1::LpPublicInterestCashMemo => RevenueBeneficiaryV1::ExternalLp,
        RevenueReturnComponentV1::NetHlpCommonCapitalContribution => RevenueBeneficiaryV1::Hlp,
        RevenueReturnComponentV1::ProtocolPublicInterestCashMemo
        | RevenueReturnComponentV1::ProtocolEconomic => RevenueBeneficiaryV1::Protocol,
        RevenueReturnComponentV1::AllStakeholderEconomic => RevenueBeneficiaryV1::AllStakeholders,
    }
}

fn component_lanes(component: RevenueReturnComponentV1) -> &'static [RevenueLaneV1] {
    match component {
        RevenueReturnComponentV1::LpSwap => &[RevenueLaneV1::LpSwapEarned],
        RevenueReturnComponentV1::LpPublicInterestAccrued => {
            &[RevenueLaneV1::LpPublicInterestAccrued]
        }
        RevenueReturnComponentV1::ExternalLpCombinedEconomic => &[
            RevenueLaneV1::LpSwapEarned,
            RevenueLaneV1::LpPublicInterestAccrued,
            RevenueLaneV1::HlpFundingInterestEarned,
        ],
        RevenueReturnComponentV1::LpPublicInterestCashMemo => {
            &[RevenueLaneV1::LpPublicInterestCashRealized]
        }
        RevenueReturnComponentV1::ProtocolPublicInterestCashMemo => &[
            RevenueLaneV1::ProtocolFeeAuctionPublicInterestCashRealized,
            RevenueLaneV1::ProtocolBuybackPublicInterestCashRealized,
        ],
        RevenueReturnComponentV1::NetHlpCommonCapitalContribution => {
            &[RevenueLaneV1::HlpFundingInterestEarned]
        }
        RevenueReturnComponentV1::ProtocolEconomic => &[
            RevenueLaneV1::ProtocolFeeAuctionSwapEarned,
            RevenueLaneV1::ProtocolBuybackSwapEarned,
            RevenueLaneV1::ProtocolPublicInterestAccrued,
            RevenueLaneV1::ProtocolFeeAuctionHlpFundingInterestEarned,
            RevenueLaneV1::ProtocolBuybackHlpFundingInterestEarned,
        ],
        RevenueReturnComponentV1::AllStakeholderEconomic => &[
            RevenueLaneV1::LpSwapEarned,
            RevenueLaneV1::ProtocolFeeAuctionSwapEarned,
            RevenueLaneV1::ProtocolBuybackSwapEarned,
            RevenueLaneV1::LpPublicInterestAccrued,
            RevenueLaneV1::ProtocolPublicInterestAccrued,
            RevenueLaneV1::HlpFundingInterestEarned,
            RevenueLaneV1::ProtocolFeeAuctionHlpFundingInterestEarned,
            RevenueLaneV1::ProtocolBuybackHlpFundingInterestEarned,
        ],
    }
}

fn revenue_component_value(
    totals: &AggregateTotals,
    venue: RevenueVenueV1,
    origin: RevenueOriginV1,
    component: RevenueReturnComponentV1,
) -> Result<AvailableExactRationalV1> {
    let supported: Vec<_> = component_lanes(component)
        .iter()
        .copied()
        .filter(|lane| revenue_lane_supported(venue, *lane))
        .collect();
    if supported.is_empty() {
        return Ok(AvailableExactRationalV1::unavailable(
            MetricAvailability::NotApplicableVenueMechanism,
        ));
    }
    if venue == RevenueVenueV1::V1
        && component == RevenueReturnComponentV1::LpPublicInterestCashMemo
    {
        return Ok(AvailableExactRationalV1::unavailable(
            MetricAvailability::MissingCashRealizationEvidence,
        ));
    }
    let mut total = BigRational::zero();
    for lane in supported {
        if let Some(value) = totals.revenue_lanes.get(&(venue, origin, lane)) {
            if !value.missing_price_keys.is_empty() {
                return Ok(AvailableExactRationalV1::unavailable(
                    MetricAvailability::MissingHistoricalUsdMark,
                ));
            }
            total += &value.value_usd_micro;
        }
    }
    AvailableExactRationalV1::available(&total)
}

fn common_capital_returns(
    revenue: &AvailableExactRationalV1,
    capital_time: &AvailableExactRationalV1,
    window_seconds: u64,
) -> Result<(AvailableExactRationalV1, AvailableExactRationalV1)> {
    if revenue.availability != MetricAvailability::Available {
        let unavailable = AvailableExactRationalV1::unavailable(revenue.availability.clone());
        return Ok((unavailable.clone(), unavailable));
    }
    if capital_time.availability != MetricAvailability::Available {
        let unavailable = AvailableExactRationalV1::unavailable(capital_time.availability.clone());
        return Ok((unavailable.clone(), unavailable));
    }
    let revenue = revenue
        .value
        .as_ref()
        .context("available revenue lacks value")?;
    let capital = capital_time
        .value
        .as_ref()
        .context("available capital-time lacks value")?;
    let revenue = exact_to_rational(revenue)?;
    let capital = exact_to_rational(capital)?;
    if capital.is_zero() {
        let unavailable = AvailableExactRationalV1::unavailable(MetricAvailability::NotEvaluated);
        return Ok((unavailable.clone(), unavailable));
    }
    let bps = BigRational::from_integer(BigInt::from(10_000_u16));
    let absolute =
        revenue.clone() * BigRational::from_integer(BigInt::from(window_seconds)) * &bps / &capital;
    let absolute = AvailableExactRationalV1::available(&absolute)?;
    let apr = if window_seconds < MIN_ANNUALIZED_WINDOW_SECONDS {
        AvailableExactRationalV1::unavailable(MetricAvailability::WindowTooShortForAnnualization)
    } else {
        let apr = revenue * BigRational::from_integer(BigInt::from(YEAR_SECONDS)) * bps / capital;
        AvailableExactRationalV1::available(&apr)?
    };
    Ok((absolute, apr))
}

fn exact_to_rational(value: &ExactRationalV1) -> Result<BigRational> {
    Ok(BigRational::new(
        BigInt::from_str(value.numerator.as_str())?,
        BigInt::from_str(value.denominator.as_str())?,
    ))
}

fn available_rational(value: &AvailableExactRationalV1) -> Result<Option<BigRational>> {
    match value.availability {
        MetricAvailability::Available => value
            .value
            .as_ref()
            .map(exact_to_rational)
            .transpose()
            .and_then(|value| value.context("available rational lacks a value").map(Some)),
        _ => {
            if value.value.is_some() {
                bail!("unavailable rational carries a value");
            }
            Ok(None)
        }
    }
}

fn spot_probe_evidence(
    values: &BTreeMap<(SpotProbeSizeKindV1, SpotProbeDirectionV1), SpotProbeAccumulator>,
) -> Result<Vec<SpotProbeAggregateCellV1>> {
    values
        .iter()
        .map(|((size_kind, direction), value)| {
            let paired_denominator =
                BigRational::from_integer(BigInt::from(value.paired_executable_count));
            let depth_denominator = BigRational::from_integer(BigInt::from(value.depth_pair_count));
            let execution = (value.paired_executable_count > 0)
                .then(|| &value.execution_cost_delta_sum / &paired_denominator);
            let gross_quote_improvement = (value.paired_executable_count > 0)
                .then(|| &value.gross_quote_improvement_sum / &paired_denominator);
            let net_quote_improvement = (value.paired_executable_count > 0)
                .then(|| &value.net_quote_improvement_sum / &paired_denominator);
            let slippage = (value.paired_executable_count > 0)
                .then(|| &value.slippage_delta_sum / &paired_denominator);
            let depth =
                (value.depth_pair_count > 0).then(|| &value.depth_delta_sum / &depth_denominator);
            Ok(SpotProbeAggregateCellV1 {
                size_kind: *size_kind,
                direction: *direction,
                observation_count: value.observation_count,
                v1_executable_count: value.v1_executable_count,
                dusk_executable_count: value.dusk_executable_count,
                paired_executable_count: value.paired_executable_count,
                mean_execution_cost_delta_ppb_exact: execution
                    .as_ref()
                    .map(ExactRationalV1::from_ratio)
                    .transpose()?,
                mean_gross_quote_improvement_ppb_exact: gross_quote_improvement
                    .as_ref()
                    .map(ExactRationalV1::from_ratio)
                    .transpose()?,
                mean_net_quote_improvement_ppb_exact: net_quote_improvement
                    .as_ref()
                    .map(ExactRationalV1::from_ratio)
                    .transpose()?,
                mean_slippage_delta_ppb_exact: slippage
                    .as_ref()
                    .map(ExactRationalV1::from_ratio)
                    .transpose()?,
                mean_cpmm_equivalent_depth_delta_exact: depth
                    .as_ref()
                    .map(ExactRationalV1::from_ratio)
                    .transpose()?,
            })
        })
        .collect()
}

fn add_totals(target: &mut AggregateTotals, source: &AggregateTotals) -> Result<()> {
    target.weighted_execution_cost_numerator += &source.weighted_execution_cost_numerator;
    target.execution_weight += &source.execution_weight;
    target
        .slippage_deltas
        .extend_from_slice(&source.slippage_deltas);
    for (key, source_lane) in &source.revenue_lanes {
        let target_lane = target.revenue_lanes.entry(*key).or_default();
        target_lane.value_usd_micro += &source_lane.value_usd_micro;
        target_lane
            .missing_price_keys
            .extend(source_lane.missing_price_keys.iter().cloned());
    }
    target.factual_capital_time += &source.factual_capital_time;
    target
        .factual_capital_missing_price_keys
        .extend(source.factual_capital_missing_price_keys.iter().cloned());
    target.v1_borrow_capacity += &source.v1_borrow_capacity;
    target.v1_gross_borrow_capacity += &source.v1_gross_borrow_capacity;
    target.dusk_borrow_capacity += &source.dusk_borrow_capacity;
    target.dusk_gross_borrow_capacity += &source.dusk_gross_borrow_capacity;
    target.dusk_gross_to_actionable_capacity_loss += &source.dusk_gross_to_actionable_capacity_loss;
    target.v1_gross_projected_total_debt += &source.v1_gross_projected_total_debt;
    target.v1_actionable_projected_total_debt += &source.v1_actionable_projected_total_debt;
    target.dusk_gross_projected_total_debt += &source.dusk_gross_projected_total_debt;
    target.dusk_actionable_projected_total_debt += &source.dusk_actionable_projected_total_debt;
    target
        .dusk_actionable_to_gross_bps
        .extend_from_slice(&source.dusk_actionable_to_gross_bps);
    target.dusk_zero_gross_capacity_observations = target
        .dusk_zero_gross_capacity_observations
        .checked_add(source.dusk_zero_gross_capacity_observations)
        .context("aggregate Dusk zero-gross-capacity count overflow")?;
    target.dusk_current_underwriting_unsatisfied_observations = target
        .dusk_current_underwriting_unsatisfied_observations
        .checked_add(source.dusk_current_underwriting_unsatisfied_observations)
        .context("aggregate Dusk underwriting-failure count overflow")?;
    target.dusk_current_global_health_floor_unsatisfied_observations = target
        .dusk_current_global_health_floor_unsatisfied_observations
        .checked_add(source.dusk_current_global_health_floor_unsatisfied_observations)
        .context("aggregate Dusk global-health-failure count overflow")?;
    for (constraint, source_count) in &source.dusk_limiting_constraint_counts {
        let target_count = target
            .dusk_limiting_constraint_counts
            .entry(*constraint)
            .or_default();
        *target_count = target_count
            .checked_add(*source_count)
            .context("aggregate Dusk limiting-constraint count overflow")?;
    }
    for (constraint, source_loss) in &source.dusk_tied_constraint_capacity_loss {
        *target
            .dusk_tied_constraint_capacity_loss
            .entry(*constraint)
            .or_default() += source_loss;
    }
    target.v1_borrowed_notional += &source.v1_borrowed_notional;
    target.dusk_borrowed_notional += &source.dusk_borrowed_notional;
    target.dusk_unmet_borrow_notional += &source.dusk_unmet_borrow_notional;
    target.v1_bad_debt += &source.v1_bad_debt;
    target.dusk_bad_debt += &source.dusk_bad_debt;
    for (operation_id, (v1, dusk)) in &source.liquidations {
        let entry = target
            .liquidations
            .entry(operation_id.clone())
            .or_insert_with(|| (BigRational::zero(), BigRational::zero()));
        entry.0 += v1;
        entry.1 += dusk;
    }
    for (key, source_probe) in &source.spot_probes {
        let target_probe = target.spot_probes.entry(*key).or_default();
        target_probe.observation_count = target_probe
            .observation_count
            .checked_add(source_probe.observation_count)
            .context("aggregate spot probe observation count overflow")?;
        target_probe.v1_executable_count = target_probe
            .v1_executable_count
            .checked_add(source_probe.v1_executable_count)
            .context("aggregate V1 spot probe executable count overflow")?;
        target_probe.dusk_executable_count = target_probe
            .dusk_executable_count
            .checked_add(source_probe.dusk_executable_count)
            .context("aggregate Dusk spot probe executable count overflow")?;
        target_probe.paired_executable_count = target_probe
            .paired_executable_count
            .checked_add(source_probe.paired_executable_count)
            .context("aggregate paired spot probe executable count overflow")?;
        target_probe.execution_cost_delta_sum += &source_probe.execution_cost_delta_sum;
        target_probe.gross_quote_improvement_sum += &source_probe.gross_quote_improvement_sum;
        target_probe.net_quote_improvement_sum += &source_probe.net_quote_improvement_sum;
        target_probe.slippage_delta_sum += &source_probe.slippage_delta_sum;
        target_probe.depth_delta_sum += &source_probe.depth_delta_sum;
        target_probe.depth_pair_count = target_probe
            .depth_pair_count
            .checked_add(source_probe.depth_pair_count)
            .context("aggregate spot probe depth count overflow")?;
    }
    Ok(())
}

fn empirical_quantile(values: &mut [i128], numerator: usize, denominator: usize) -> Option<i128> {
    let index = nearest_rank_index(values.len(), numerator, denominator)?;
    values.sort_unstable();
    values.get(index).copied()
}

fn exact_rational_quantile(
    values: &mut [BigRational],
    numerator: usize,
    denominator: usize,
) -> Option<BigRational> {
    let index = nearest_rank_index(values.len(), numerator, denominator)?;
    values.sort();
    values.get(index).cloned()
}

fn nearest_rank_index(len: usize, numerator: usize, denominator: usize) -> Option<usize> {
    if len == 0 || denominator == 0 || numerator > denominator {
        return None;
    }
    if numerator == 0 {
        return Some(0);
    }
    let rank = len
        .checked_mul(numerator)?
        .checked_add(denominator.checked_sub(1)?)?
        / denominator;
    Some(rank.saturating_sub(1).min(len - 1))
}

fn rational_to_i128_trunc(value: &BigRational) -> Result<i128> {
    (value.numer() / value.denom())
        .to_i128()
        .context("exact rational does not fit i128")
}

fn parse_token(value: &TokenAmount) -> Result<u128> {
    value
        .as_str()
        .parse::<u128>()
        .context("token amount exceeds u128")
}

fn parse_signed(value: &SignedTokenAmount) -> Result<i128> {
    value
        .as_str()
        .parse::<i128>()
        .context("signed amount exceeds i128")
}

fn signed(value: i128) -> Result<SignedTokenAmount> {
    SignedTokenAmount::new(value.to_string()).context("invalid signed metric")
}

fn pow10_big(exponent: u8) -> BigInt {
    BigInt::from(10_u8).pow(u32::from(exponent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_usd_conversion_retains_sub_micro_fraction() {
        let price = DecimalPrice::parse("1.23456789").unwrap();
        let value = BigRational::new(
            BigInt::from(1_u8) * price.coefficient * BigInt::from(USD_MICRO_SCALE),
            pow10_big(6) * price.scale,
        );
        assert_eq!(value.numer().to_string(), "123456789");
        assert_eq!(value.denom().to_string(), "100000000");
    }

    #[test]
    fn empirical_quantiles_use_preregistered_nearest_rank_ceil() {
        let mut values = vec![50, 10, 40, 20, 30];
        assert_eq!(empirical_quantile(&mut values, 95, 100), Some(50));
        let mut even = vec![4, 1, 3, 2];
        assert_eq!(empirical_quantile(&mut even, 1, 2), Some(2));
    }

    #[test]
    fn dusk_capacity_evidence_keeps_exact_distribution_and_tied_attribution() {
        let mut totals = AggregateTotals {
            dusk_borrow_capacity: BigRational::from_integer(BigInt::from(210_u16)),
            dusk_gross_borrow_capacity: BigRational::from_integer(BigInt::from(300_u16)),
            dusk_gross_to_actionable_capacity_loss: BigRational::from_integer(BigInt::from(90_u8)),
            dusk_actionable_to_gross_bps: vec![
                BigRational::from_integer(BigInt::from(10_000_u16)),
                BigRational::from_integer(BigInt::from(5_000_u16)),
                BigRational::from_integer(BigInt::from(7_500_u16)),
            ],
            dusk_zero_gross_capacity_observations: 1,
            dusk_current_underwriting_unsatisfied_observations: 1,
            dusk_current_global_health_floor_unsatisfied_observations: 2,
            dusk_limiting_constraint_counts: BTreeMap::from([
                (DuskBorrowCapacityConstraintV1::Cash, 2),
                (DuskBorrowCapacityConstraintV1::DailyBorrowBucket, 1),
            ]),
            dusk_tied_constraint_capacity_loss: BTreeMap::from([
                (
                    DuskBorrowCapacityConstraintV1::Cash,
                    BigRational::from_integer(BigInt::from(90_u8)),
                ),
                (
                    DuskBorrowCapacityConstraintV1::DailyBorrowBucket,
                    BigRational::from_integer(BigInt::from(30_u8)),
                ),
            ]),
            ..AggregateTotals::default()
        };
        let evidence = dusk_borrow_capacity_evidence(&mut totals, 5).unwrap();
        assert_eq!(evidence.matched_borrow_prestate_observation_count, 5);
        assert_eq!(evidence.underwriting_comparable_observation_count, 4);
        assert_eq!(evidence.headroom_ratio_observation_count, 3);
        assert_eq!(
            evidence.current_underwriting_unsatisfied_observation_count,
            1
        );
        assert_eq!(
            evidence
                .actionable_to_gross_additional_headroom_bps_p50_exact
                .as_ref()
                .unwrap()
                .numerator
                .as_str(),
            "7500"
        );
        assert_eq!(
            evidence
                .actionable_to_gross_additional_headroom_bps_p95_exact
                .as_ref()
                .unwrap()
                .numerator
                .as_str(),
            "10000"
        );
        let cash = evidence
            .constraints
            .iter()
            .find(|row| row.constraint == DuskBorrowCapacityConstraintV1::Cash)
            .unwrap();
        assert_eq!(cash.limiting_matched_observation_count, 2);
        assert_eq!(
            cash.tied_additional_headroom_loss_usd_micro
                .numerator
                .as_str(),
            "90"
        );
        assert_eq!(evidence.constraints.len(), 5);
    }

    #[test]
    fn dusk_capacity_evidence_rejects_an_unpartitioned_observation_set() {
        let mut totals = AggregateTotals::default();
        assert!(dusk_borrow_capacity_evidence(&mut totals, 1).is_err());
    }

    #[test]
    fn zero_v1_actionable_headroom_is_not_fabricated_capacity_parity() {
        assert!(borrow_capacity_uplift(&AggregateTotals::default()).is_none());
    }

    #[test]
    fn revenue_uplift_uses_time_weighted_factual_capital() {
        let v1 = BigRational::from_integer(BigInt::from(10));
        let dusk = BigRational::from_integer(BigInt::from(12));
        let capital_time = BigRational::from_integer(BigInt::from(100 * 10));
        let duration = BigRational::from_integer(BigInt::from(10));
        assert_eq!(
            rational_to_i128_trunc(
                &revenue_uplift_bps(&v1, &dusk, &capital_time, &duration).unwrap()
            )
            .unwrap(),
            200
        );
    }

    #[test]
    fn revenue_yield_uses_absolute_return_below_30_days_and_apr_afterward() {
        let ten_days = 10_u64 * 24 * 60 * 60;
        let mut totals = AggregateTotals::default();
        totals.revenue_lanes.insert(
            (
                RevenueVenueV1::V1,
                RevenueOriginV1::FactualUserFlow,
                RevenueLaneV1::LpSwapEarned,
            ),
            RevenueLaneAccumulator {
                value_usd_micro: BigRational::from_integer(BigInt::from(10_u8)),
                ..RevenueLaneAccumulator::default()
            },
        );
        totals.revenue_lanes.insert(
            (
                RevenueVenueV1::Dusk,
                RevenueOriginV1::FactualUserFlow,
                RevenueLaneV1::LpSwapEarned,
            ),
            RevenueLaneAccumulator {
                value_usd_micro: BigRational::from_integer(BigInt::from(12_u8)),
                ..RevenueLaneAccumulator::default()
            },
        );
        totals.factual_capital_time = BigRational::from_integer(BigInt::from(100_u8))
            * BigRational::from_integer(BigInt::from(ten_days));
        let short = revenue_yield_evidence(&totals, i64::try_from(ten_days).unwrap()).unwrap();
        let short_v1 = short
            .common_factual_capital_returns
            .iter()
            .find(|row| {
                row.venue == RevenueVenueV1::V1
                    && row.origin == RevenueOriginV1::FactualUserFlow
                    && row.component == RevenueReturnComponentV1::LpSwap
                    && row.denominator_basis
                        == RevenueDenominatorBasisV1::FactualV1AccountedLpCapitalTime
            })
            .unwrap();
        assert_eq!(
            short_v1
                .absolute_return_bps_exact
                .value
                .as_ref()
                .unwrap()
                .numerator
                .as_str(),
            "1000"
        );
        assert_eq!(
            short_v1.apr_bps_exact.availability,
            MetricAvailability::WindowTooShortForAnnualization
        );
        assert!(short.inventory_pnl_exact.is_none());
        assert_eq!(
            short.inventory_pnl_unsupported_reason,
            InventoryPnlUnsupportedReasonV1::PassiveLpOwnershipNavNotCaptured
        );

        let thirty_days = MIN_ANNUALIZED_WINDOW_SECONDS;
        totals.factual_capital_time = BigRational::from_integer(BigInt::from(100_u8))
            * BigRational::from_integer(BigInt::from(thirty_days));
        let long = revenue_yield_evidence(&totals, i64::try_from(thirty_days).unwrap()).unwrap();
        assert!(long.common_factual_capital_returns.iter().any(|row| {
            row.venue == RevenueVenueV1::V1
                && row.component == RevenueReturnComponentV1::LpSwap
                && row.apr_bps_exact.availability == MetricAvailability::Available
        }));
        assert_eq!(
            long.actual_dusk_external_lp_apy_bps_exact.availability,
            MetricAvailability::MissingOwnershipDenominator
        );
    }

    #[test]
    fn nonzero_missing_price_invalidates_only_that_revenue_lane() {
        let mut totals = AggregateTotals::default();
        totals.revenue_lanes.insert(
            (
                RevenueVenueV1::Dusk,
                RevenueOriginV1::FactualUserFlow,
                RevenueLaneV1::RetainedRecenterInventory,
            ),
            RevenueLaneAccumulator {
                value_usd_micro: BigRational::zero(),
                missing_price_keys: BTreeSet::from(["mint@0".to_owned()]),
            },
        );
        let lanes = revenue_lane_totals(&totals).unwrap();
        let inventory = lanes
            .iter()
            .find(|row| {
                row.venue == RevenueVenueV1::Dusk
                    && row.origin == RevenueOriginV1::FactualUserFlow
                    && row.lane == RevenueLaneV1::RetainedRecenterInventory
            })
            .unwrap();
        assert_eq!(
            inventory.usd_micro.availability,
            MetricAvailability::MissingHistoricalUsdMark
        );
        let core = revenue_component_value(
            &totals,
            RevenueVenueV1::Dusk,
            RevenueOriginV1::FactualUserFlow,
            RevenueReturnComponentV1::AllStakeholderEconomic,
        )
        .unwrap();
        assert_eq!(core.availability, MetricAvailability::Available);
    }

    #[test]
    fn v1_protocol_cash_is_exact_while_lp_cash_and_buyback_are_unavailable() {
        let mut totals = AggregateTotals::default();
        totals.revenue_lanes.insert(
            (
                RevenueVenueV1::V1,
                RevenueOriginV1::FactualUserFlow,
                RevenueLaneV1::ProtocolFeeAuctionPublicInterestCashRealized,
            ),
            RevenueLaneAccumulator {
                value_usd_micro: BigRational::from_integer(BigInt::from(7_u8)),
                ..RevenueLaneAccumulator::default()
            },
        );
        let lanes = revenue_lane_totals(&totals).unwrap();
        let lane = |wanted| {
            lanes
                .iter()
                .find(|row| {
                    row.venue == RevenueVenueV1::V1
                        && row.origin == RevenueOriginV1::FactualUserFlow
                        && row.lane == wanted
                })
                .unwrap()
        };
        assert_eq!(
            lane(RevenueLaneV1::LpPublicInterestCashRealized)
                .usd_micro
                .availability,
            MetricAvailability::MissingCashRealizationEvidence
        );
        assert_eq!(
            lane(RevenueLaneV1::ProtocolFeeAuctionPublicInterestCashRealized)
                .usd_micro
                .value
                .as_ref()
                .unwrap()
                .numerator
                .as_str(),
            "7"
        );
        assert_eq!(
            lane(RevenueLaneV1::ProtocolBuybackPublicInterestCashRealized)
                .usd_micro
                .availability,
            MetricAvailability::NotApplicableVenueMechanism
        );
        let protocol_cash = revenue_component_value(
            &totals,
            RevenueVenueV1::V1,
            RevenueOriginV1::FactualUserFlow,
            RevenueReturnComponentV1::ProtocolPublicInterestCashMemo,
        )
        .unwrap();
        assert_eq!(
            protocol_cash.value.as_ref().unwrap().numerator.as_str(),
            "7"
        );
    }

    #[test]
    fn net_hlp_and_protocol_cuts_have_disjoint_component_attribution() {
        let mut totals = AggregateTotals::default();
        for (lane, value) in [
            (RevenueLaneV1::HlpFundingInterestEarned, 10_u8),
            (RevenueLaneV1::ProtocolFeeAuctionHlpFundingInterestEarned, 2),
            (RevenueLaneV1::ProtocolBuybackHlpFundingInterestEarned, 3),
        ] {
            totals.revenue_lanes.insert(
                (RevenueVenueV1::Dusk, RevenueOriginV1::FactualUserFlow, lane),
                RevenueLaneAccumulator {
                    value_usd_micro: BigRational::from_integer(BigInt::from(value)),
                    ..RevenueLaneAccumulator::default()
                },
            );
        }
        let value = |component| {
            revenue_component_value(
                &totals,
                RevenueVenueV1::Dusk,
                RevenueOriginV1::FactualUserFlow,
                component,
            )
            .unwrap()
            .value
            .unwrap()
            .numerator
            .as_str()
            .to_owned()
        };
        assert_eq!(
            value(RevenueReturnComponentV1::ExternalLpCombinedEconomic),
            "10"
        );
        assert_eq!(
            value(RevenueReturnComponentV1::NetHlpCommonCapitalContribution),
            "10"
        );
        assert_eq!(value(RevenueReturnComponentV1::ProtocolEconomic), "5");
        assert_eq!(
            value(RevenueReturnComponentV1::AllStakeholderEconomic),
            "15"
        );
    }

    #[test]
    fn per_market_candidate_scores_are_independently_gated_with_own_tail() {
        let zero = BigRational::zero();
        let positive = BigRational::from_integer(BigInt::from(25_u8));
        let clean_blocking = BTreeSet::new();
        let clean = candidate_score_from_components(
            "profile",
            HlpScenario::NoHlp,
            &zero,
            Some(-7),
            Some(&zero),
            Some(0),
            Some(&positive),
            Some(&positive),
            Some(&zero),
            &clean_blocking,
        )
        .unwrap()
        .unwrap();
        assert_eq!(clean.tail_liquidation_shortfall_delta_usd_micro, -7);
        assert_eq!(clean.capital_weighted_stakeholder_revenue_uplift_bps, 25);

        let blocked = BTreeSet::from(["missing_historical_usd_marks".to_owned()]);
        assert!(candidate_score_from_components(
            "profile",
            HlpScenario::NoHlp,
            &zero,
            Some(0),
            Some(&zero),
            Some(0),
            Some(&zero),
            Some(&zero),
            Some(&zero),
            &blocked,
        )
        .unwrap()
        .is_none());

        let liquidations = BTreeMap::from([
            (
                "a".to_owned(),
                (
                    BigRational::from_integer(BigInt::from(10_u8)),
                    BigRational::from_integer(BigInt::from(7_u8)),
                ),
            ),
            (
                "b".to_owned(),
                (
                    BigRational::from_integer(BigInt::from(4_u8)),
                    BigRational::from_integer(BigInt::from(5_u8)),
                ),
            ),
        ]);
        assert_eq!(liquidation_tail_delta(&liquidations, 2).unwrap(), Some(1));
        assert_eq!(
            liquidation_tail_delta(&BTreeMap::new(), 0).unwrap(),
            Some(0)
        );
        assert_eq!(liquidation_tail_delta(&BTreeMap::new(), 1).unwrap(), None);
    }
}

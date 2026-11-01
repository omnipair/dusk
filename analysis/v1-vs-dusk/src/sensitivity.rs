//! Deterministic holdout sensitivity evidence.
//!
//! This module consumes the exact paired metric JSONL ledger and exact
//! historical provider marks directly.  It keeps USD micro-unit valuations and
//! bootstrap statistics as arbitrary-precision rationals until the explicit
//! signed-integer reporting boundary.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{Signed, Zero};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    canonical::{HistoricalPriceMarkV1, SignedTokenAmount, TokenAmount},
    metric_engine::{
        revenue_origin_from_flags, PairedMetricLedgerSummaryV1, PairedOperationMetricRowV1,
        RevenueAccountingClassV1, RevenueLaneV1, RevenueOriginV1, RevenueSideDeltaV1,
        PAIRED_METRIC_SCHEMA_VERSION,
    },
    replay::{HlpScenario, ReplayArm},
};

pub const SENSITIVITY_SCHEMA_VERSION: u16 = 2;
pub const UTC_SEVEN_DAY_BLOCK_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const BOOTSTRAP_ALGORITHM_V1: &str = "splitmix64_paired_utc_7d_with_replacement_v1";
pub const REPORTING_ROUNDING_V1: &str = "truncate_toward_zero";

const HOUR_SECONDS: i64 = 3_600;
const USD_MICRO_SCALE: u64 = 1_000_000;
const ARTIFACT_HASH_DOMAIN: &[u8] = b"v1-vs-dusk/sensitivity-artifact/v2\0";
const SUMMARY_HASH_DOMAIN: &[u8] = b"v1-vs-dusk/paired-summary/v2\0";
const SAMPLE_PATH_HASH_DOMAIN: &[u8] = b"v1-vs-dusk/sensitivity-sample-path/v1\0";

/// Immutable inputs for one holdout sensitivity run.
///
/// `paired_metric_ledger_sha256` is taken from `paired_summary`; the prices
/// hash is supplied separately because the historical mark JSONL has no
/// summary type.  Both are recomputed from the exact bytes before parsing.
pub struct SensitivityRequestV1<'a> {
    pub run_id: &'a str,
    pub profile_id: &'a str,
    pub arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub holdout_start_unix_seconds: i64,
    pub holdout_end_unix_seconds_exclusive: i64,
    pub paired_metric_ledger_path: &'a Path,
    pub paired_summary: &'a PairedMetricLedgerSummaryV1,
    pub historical_prices_path: &'a Path,
    pub historical_prices_sha256: &'a str,
    pub historical_price_provider: &'a str,
    /// Frozen headline population. `None` uses every market with a holdout
    /// metric row; `Some` is hash-bound and must exist in the ledger.
    pub included_markets: Option<&'a BTreeSet<String>>,
    /// Frozen into the artifact. Identical inputs, seed, and sample count yield
    /// byte-identical evidence.
    pub bootstrap_seed: u64,
    pub bootstrap_samples: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensitivityExactRationalV1 {
    pub numerator: SignedTokenAmount,
    pub denominator: TokenAmount,
}

impl SensitivityExactRationalV1 {
    fn from_ratio(value: &BigRational) -> Result<Self> {
        if value.denom().is_zero() || value.denom().is_negative() {
            bail!("sensitivity rational denominator is not positive");
        }
        Ok(Self {
            numerator: SignedTokenAmount::new(value.numer().to_string())?,
            denominator: TokenAmount::new(value.denom().to_string())?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactAndReportedSignedV1 {
    pub exact: SensitivityExactRationalV1,
    /// Explicit lossy presentation boundary; see `reporting_rounding`.
    pub reported_signed_integer: SignedTokenAmount,
}

impl ExactAndReportedSignedV1 {
    fn from_ratio(value: &BigRational) -> Result<Self> {
        Ok(Self {
            exact: SensitivityExactRationalV1::from_ratio(value)?,
            reported_signed_integer: truncate_to_signed(value)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensitivityPointEstimateV1 {
    pub stakeholder_revenue_v1_usd_micro: SensitivityExactRationalV1,
    pub stakeholder_revenue_dusk_user_flow_usd_micro: SensitivityExactRationalV1,
    /// Report headline: Dusk user-flow revenue less factual V1 revenue.
    pub stakeholder_user_flow_revenue_delta_usd_micro: ExactAndReportedSignedV1,
    /// Explicitly excluded from the user-flow revenue delta.
    pub dusk_synthetic_arbitrage_revenue_usd_micro: SensitivityExactRationalV1,
    /// Protocol settlement is neither user flow nor arbitrage and is always
    /// reported as an independent counterfactual origin.
    pub dusk_synthetic_protocol_settlement_revenue_usd_micro: SensitivityExactRationalV1,
    /// Factual post-transfer-fee reserve credit, matching the paired aggregate
    /// engine's execution-cost weight.
    pub factual_swap_input_usd_micro: SensitivityExactRationalV1,
    /// Volume-weighted quoted cost delta. `None` means at least one factual
    /// swap lacked a comparable quote, or the factual input weight was zero.
    pub quoted_execution_cost_delta_ppb: Option<ExactAndReportedSignedV1>,
    pub quoted_execution_cost_unsupported_reason: Option<String>,
    pub unsupported_execution_rows: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapIntervalV1 {
    pub lower_2_5_percent: ExactAndReportedSignedV1,
    pub median: ExactAndReportedSignedV1,
    pub upper_97_5_percent: ExactAndReportedSignedV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedBootstrapEvidenceV1 {
    pub algorithm: String,
    pub block_seconds: i64,
    pub seed: u64,
    pub samples: u32,
    pub observed_block_count: u64,
    /// Commits to every sampled UTC block index, proving all paired lanes used
    /// one common resampling path rather than independent draws.
    pub sampled_block_sequence_sha256: String,
    pub stakeholder_user_flow_revenue_delta_usd_micro: BootstrapIntervalV1,
    pub dusk_synthetic_arbitrage_revenue_usd_micro: BootstrapIntervalV1,
    pub dusk_synthetic_protocol_settlement_revenue_usd_micro: BootstrapIntervalV1,
    pub quoted_execution_cost_delta_ppb: Option<BootstrapIntervalV1>,
    pub execution_cost_unsupported_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopMarketRankingBasisV1 {
    FactualUsdSwapInput,
    FactualStakeholderRevenueUsd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankedMarketV1 {
    pub rank: u64,
    pub market: String,
    pub factual_value_usd_micro: SensitivityExactRationalV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopMarketRankingV1 {
    pub basis: TopMarketRankingBasisV1,
    /// Descending exact value, then ascending market identity for ties.
    pub markets: Vec<RankedMarketV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopMarketExclusionV1 {
    pub basis: TopMarketRankingBasisV1,
    pub excluded_market: String,
    pub excluded_factual_value_usd_micro: SensitivityExactRationalV1,
    pub remaining_market_count: u64,
    pub estimate_without_top_market: SensitivityPointEstimateV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensitivityArtifactV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub profile_id: String,
    pub arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub holdout_start_unix_seconds: i64,
    pub holdout_end_unix_seconds_exclusive: i64,
    pub paired_metric_ledger_sha256: String,
    pub paired_metric_summary_sha256: String,
    pub included_markets_sha256: String,
    pub historical_prices_sha256: String,
    pub historical_price_provider: String,
    pub historical_price_rows: u64,
    pub paired_metric_rows: u64,
    pub holdout_metric_rows: u64,
    pub included_market_count: u64,
    pub reporting_rounding: String,
    pub point_estimate: SensitivityPointEstimateV1,
    pub paired_bootstrap: PairedBootstrapEvidenceV1,
    pub top_market_rankings: Vec<TopMarketRankingV1>,
    pub top_market_exclusions: Vec<TopMarketExclusionV1>,
    pub top_market_exclusion_unsupported_reason: Option<String>,
    pub artifact_sha256: String,
}

impl SensitivityArtifactV1 {
    fn seal(mut self) -> Result<Self> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = hash_serialized(ARTIFACT_HASH_DOMAIN, &self)?;
        Ok(self)
    }

    /// Verifies the self-contained artifact commitment. Input files must be
    /// rechecked by calling `build_sensitivity_evidence` again.
    pub fn verify_self_hash(&self) -> Result<()> {
        if self.schema_version != SENSITIVITY_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || self.profile_id.trim().is_empty()
            || self.holdout_start_unix_seconds >= self.holdout_end_unix_seconds_exclusive
            || self.reporting_rounding != REPORTING_ROUNDING_V1
            || self.paired_bootstrap.algorithm != BOOTSTRAP_ALGORITHM_V1
            || self.paired_bootstrap.block_seconds != UTC_SEVEN_DAY_BLOCK_SECONDS
            || self.paired_bootstrap.samples == 0
            || self.paired_bootstrap.observed_block_count == 0
            || !valid_sha256(&self.paired_metric_ledger_sha256)
            || !valid_sha256(&self.paired_metric_summary_sha256)
            || !valid_sha256(&self.included_markets_sha256)
            || !valid_sha256(&self.historical_prices_sha256)
            || !valid_sha256(&self.paired_bootstrap.sampled_block_sequence_sha256)
            || !valid_sha256(&self.artifact_sha256)
        {
            bail!("sensitivity artifact identity is invalid");
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256 != hash_serialized(ARTIFACT_HASH_DOMAIN, &unsigned)? {
            bail!("sensitivity artifact self-hash mismatch");
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
struct Totals {
    v1_revenue: BigRational,
    dusk_user_revenue: BigRational,
    dusk_synthetic_arbitrage_revenue: BigRational,
    dusk_synthetic_protocol_settlement_revenue: BigRational,
    factual_swap_input: BigRational,
    weighted_execution_cost_numerator: BigRational,
    execution_weight: BigRational,
    unsupported_execution_rows: u64,
}

impl Totals {
    fn add_assign(&mut self, other: &Self) -> Result<()> {
        self.v1_revenue += &other.v1_revenue;
        self.dusk_user_revenue += &other.dusk_user_revenue;
        self.dusk_synthetic_arbitrage_revenue += &other.dusk_synthetic_arbitrage_revenue;
        self.dusk_synthetic_protocol_settlement_revenue +=
            &other.dusk_synthetic_protocol_settlement_revenue;
        self.factual_swap_input += &other.factual_swap_input;
        self.weighted_execution_cost_numerator += &other.weighted_execution_cost_numerator;
        self.execution_weight += &other.execution_weight;
        self.unsupported_execution_rows = self
            .unsupported_execution_rows
            .checked_add(other.unsupported_execution_rows)
            .context("unsupported execution row count overflow")?;
        Ok(())
    }

    fn subtract(&self, other: &Self) -> Result<Self> {
        Ok(Self {
            v1_revenue: &self.v1_revenue - &other.v1_revenue,
            dusk_user_revenue: &self.dusk_user_revenue - &other.dusk_user_revenue,
            dusk_synthetic_arbitrage_revenue: &self.dusk_synthetic_arbitrage_revenue
                - &other.dusk_synthetic_arbitrage_revenue,
            dusk_synthetic_protocol_settlement_revenue: &self
                .dusk_synthetic_protocol_settlement_revenue
                - &other.dusk_synthetic_protocol_settlement_revenue,
            factual_swap_input: &self.factual_swap_input - &other.factual_swap_input,
            weighted_execution_cost_numerator: &self.weighted_execution_cost_numerator
                - &other.weighted_execution_cost_numerator,
            execution_weight: &self.execution_weight - &other.execution_weight,
            unsupported_execution_rows: self
                .unsupported_execution_rows
                .checked_sub(other.unsupported_execution_rows)
                .context("market execution support count exceeds aggregate")?,
        })
    }
}

#[derive(Clone)]
struct MarketIdentity {
    token0: String,
    token1: String,
    token0_decimals: u8,
    token1_decimals: u8,
    last_time: i64,
}

#[derive(Clone)]
struct DecimalPrice {
    coefficient: BigInt,
    scale: BigInt,
    token_decimals: Option<u8>,
}

struct HistoricalPriceIndex {
    rows: u64,
    marks: BTreeMap<(String, i64), DecimalPrice>,
}

impl HistoricalPriceIndex {
    fn from_jsonl(path: &Path, expected_sha256: &str, expected_provider: &str) -> Result<Self> {
        if expected_provider.trim().is_empty() {
            bail!("historical price provider is empty");
        }
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if sha256_bytes(&bytes) != expected_sha256 {
            bail!("historical price file hash mismatch");
        }
        let mut marks = BTreeMap::new();
        let mut rows = 0_u64;
        for (line_number, line) in strict_jsonl_lines(&bytes)?.into_iter().enumerate() {
            let mark: HistoricalPriceMarkV1 = serde_json::from_slice(line)
                .with_context(|| format!("decoding historical price line {}", line_number + 1))?;
            mark.validate()?;
            if mark.provider != expected_provider {
                bail!("historical price provider identity mismatch");
            }
            let key = (mark.mint.clone(), mark.bucket_unix_seconds);
            let price = DecimalPrice::parse(mark.price_usd.as_str(), mark.token_decimals)?;
            if marks.insert(key, price).is_some() {
                bail!("duplicate historical provider mark for one mint/hour");
            }
            rows = rows
                .checked_add(1)
                .context("historical price row overflow")?;
        }
        if marks.is_empty() {
            bail!("historical price file is empty");
        }
        Ok(Self { rows, marks })
    }

    fn value_atoms(
        &self,
        mint: &str,
        decimals: u8,
        unix_seconds: i64,
        atoms: &BigInt,
    ) -> Result<BigRational> {
        if atoms.is_negative() {
            bail!("unsigned token atoms became negative");
        }
        if atoms.is_zero() {
            return Ok(BigRational::zero());
        }
        let bucket = unix_seconds.div_euclid(HOUR_SECONDS) * HOUR_SECONDS;
        let price = self
            .marks
            .get(&(mint.to_owned(), bucket))
            .with_context(|| format!("missing historical mark {mint}@{bucket}"))?;
        if price
            .token_decimals
            .is_some_and(|price_decimals| price_decimals != decimals)
        {
            bail!("historical mark token-decimals identity mismatch for {mint}@{bucket}");
        }
        Ok(BigRational::new(
            atoms * &price.coefficient * BigInt::from(USD_MICRO_SCALE),
            pow10_big(decimals) * &price.scale,
        ))
    }
}

impl DecimalPrice {
    fn parse(value: &str, token_decimals: Option<u8>) -> Result<Self> {
        let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
        let coefficient = BigInt::from_str(&format!("{integer}{fraction}"))
            .context("invalid exact historical price")?;
        if coefficient <= BigInt::zero() {
            bail!("historical price is not positive");
        }
        Ok(Self {
            coefficient,
            scale: BigInt::from(10_u8).pow(u32::try_from(fraction.len())?),
            token_decimals,
        })
    }
}

/// Builds hash-bound, deterministic sensitivity evidence from a paired metric
/// ledger. Rows before the holdout are verified but do not enter estimates;
/// this permits native replay prefixes without treating them as holdout data.
pub fn build_sensitivity_evidence(
    request: &SensitivityRequestV1<'_>,
) -> Result<SensitivityArtifactV1> {
    validate_request(request)?;
    let prices = HistoricalPriceIndex::from_jsonl(
        request.historical_prices_path,
        request.historical_prices_sha256,
        request.historical_price_provider,
    )?;
    let ledger_bytes = fs::read(request.paired_metric_ledger_path)
        .with_context(|| format!("reading {}", request.paired_metric_ledger_path.display()))?;
    if sha256_bytes(&ledger_bytes) != request.paired_summary.ledger_sha256 {
        bail!("paired metric ledger hash differs from its summary");
    }

    let mut all_totals = Totals::default();
    let mut weekly_totals = BTreeMap::<i64, Totals>::new();
    let mut market_totals = BTreeMap::<String, Totals>::new();
    let mut identities = BTreeMap::<String, MarketIdentity>::new();
    let mut holdout_markets = BTreeSet::new();
    let mut summary_markets = BTreeSet::new();
    let mut row_count = 0_u64;
    let mut holdout_row_count = 0_u64;
    let mut synthetic_rows = 0_u64;
    let mut synthetic_settlement_rows = 0_u64;
    let mut swap_rows = 0_u64;
    let mut borrow_rows = 0_u64;
    let mut repay_rows = 0_u64;
    let mut inexact_liquidation_rows = 0_u64;
    let mut first_order = None;
    let mut last_order = None;
    let mut previous_order = None;
    let mut previous_time = None;

    for (line_number, line) in strict_jsonl_lines(&ledger_bytes)?.into_iter().enumerate() {
        let row: PairedOperationMetricRowV1 = serde_json::from_slice(line)
            .with_context(|| format!("decoding paired metric line {}", line_number + 1))?;
        validate_row_identity(&row, request)?;
        if previous_order.is_some_and(|order| row.order < order) {
            bail!(
                "paired metric chain order regressed at line {}",
                line_number + 1
            );
        }
        if previous_time.is_some_and(|time| row.block_time_unix_seconds < time) {
            bail!(
                "paired metric block time regressed at line {}",
                line_number + 1
            );
        }
        previous_order = Some(row.order);
        previous_time = Some(row.block_time_unix_seconds);
        first_order.get_or_insert(row.order);
        last_order = Some(row.order);
        row_count = row_count
            .checked_add(1)
            .context("paired row count overflow")?;
        summary_markets.insert(row.market.clone());
        if row.synthetic_arbitrage {
            synthetic_rows = synthetic_rows
                .checked_add(1)
                .context("row count overflow")?;
        }
        if row.synthetic_protocol_settlement {
            synthetic_settlement_rows = synthetic_settlement_rows
                .checked_add(1)
                .context("row count overflow")?;
        }
        if row.swap.is_some() {
            swap_rows = swap_rows.checked_add(1).context("row count overflow")?;
        }
        if row.borrow.is_some() {
            borrow_rows = borrow_rows.checked_add(1).context("row count overflow")?;
        }
        if row.repay.is_some() {
            repay_rows = repay_rows.checked_add(1).context("row count overflow")?;
        }
        if row
            .safety
            .as_ref()
            .is_some_and(|safety| !safety.dusk_exact_liquidation_available)
        {
            inexact_liquidation_rows = inexact_liquidation_rows
                .checked_add(1)
                .context("row count overflow")?;
        }
        validate_market_identity(&mut identities, &row)?;

        if !(request.holdout_start_unix_seconds..request.holdout_end_unix_seconds_exclusive)
            .contains(&row.block_time_unix_seconds)
        {
            continue;
        }
        if request
            .included_markets
            .is_some_and(|included| !included.contains(&row.market))
        {
            continue;
        }
        holdout_row_count = holdout_row_count
            .checked_add(1)
            .context("holdout row count overflow")?;
        holdout_markets.insert(row.market.clone());
        let row_totals = observe_holdout_row(&row, &prices)?;
        all_totals.add_assign(&row_totals)?;
        market_totals
            .entry(row.market.clone())
            .or_default()
            .add_assign(&row_totals)?;
        let block = utc_seven_day_bucket(row.block_time_unix_seconds)?;
        weekly_totals
            .entry(block)
            .or_default()
            .add_assign(&row_totals)?;
    }

    verify_summary(
        request.paired_summary,
        request.profile_id,
        row_count,
        summary_markets.len(),
        synthetic_rows,
        synthetic_settlement_rows,
        swap_rows,
        borrow_rows,
        repay_rows,
        inexact_liquidation_rows,
        first_order,
        last_order,
    )?;
    if holdout_row_count == 0 || weekly_totals.is_empty() {
        bail!("paired metric ledger has no rows in the requested holdout");
    }
    let analysis_markets = if let Some(expected) = request.included_markets {
        for market in expected {
            if !identities.contains_key(market) {
                bail!("included market {market} is absent from the paired metric ledger");
            }
            market_totals.entry(market.clone()).or_default();
        }
        expected.clone()
    } else {
        holdout_markets
    };
    if analysis_markets.is_empty() {
        bail!("sensitivity market population is empty");
    }

    let point_estimate = point_estimate(&all_totals)?;
    let paired_bootstrap = bootstrap(
        &weekly_totals,
        request.bootstrap_seed,
        request.bootstrap_samples,
    )?;
    let (top_market_rankings, top_market_exclusions, top_market_exclusion_reason) =
        top_market_sensitivity(&all_totals, &market_totals)?;

    let artifact = SensitivityArtifactV1 {
        schema_version: SENSITIVITY_SCHEMA_VERSION,
        run_id: request.run_id.to_owned(),
        profile_id: request.profile_id.to_owned(),
        arm: request.arm,
        hlp_scenario: request.hlp_scenario,
        holdout_start_unix_seconds: request.holdout_start_unix_seconds,
        holdout_end_unix_seconds_exclusive: request.holdout_end_unix_seconds_exclusive,
        paired_metric_ledger_sha256: request.paired_summary.ledger_sha256.clone(),
        paired_metric_summary_sha256: hash_serialized(SUMMARY_HASH_DOMAIN, request.paired_summary)?,
        included_markets_sha256: market_set_sha256(&analysis_markets),
        historical_prices_sha256: request.historical_prices_sha256.to_owned(),
        historical_price_provider: request.historical_price_provider.to_owned(),
        historical_price_rows: prices.rows,
        paired_metric_rows: row_count,
        holdout_metric_rows: holdout_row_count,
        included_market_count: u64::try_from(analysis_markets.len())?,
        reporting_rounding: REPORTING_ROUNDING_V1.to_owned(),
        point_estimate,
        paired_bootstrap,
        top_market_rankings,
        top_market_exclusions,
        top_market_exclusion_unsupported_reason: top_market_exclusion_reason,
        artifact_sha256: String::new(),
    }
    .seal()?;
    artifact.verify_self_hash()?;
    Ok(artifact)
}

fn validate_request(request: &SensitivityRequestV1<'_>) -> Result<()> {
    if request.run_id.trim().is_empty()
        || request.profile_id.trim().is_empty()
        || request.historical_price_provider.trim().is_empty()
    {
        bail!("sensitivity run, profile, and provider identities must be non-empty");
    }
    if request.holdout_start_unix_seconds >= request.holdout_end_unix_seconds_exclusive {
        bail!("sensitivity holdout window is empty");
    }
    if request.bootstrap_samples == 0 {
        bail!("bootstrap sample count is zero");
    }
    if request.included_markets.is_some_and(BTreeSet::is_empty) {
        bail!("included sensitivity market population is empty");
    }
    if request
        .included_markets
        .is_some_and(|markets| markets.iter().any(|market| market.trim().is_empty()))
    {
        bail!("included sensitivity market identity is empty");
    }
    if request.bootstrap_samples > 1_000_000 {
        bail!("bootstrap sample count exceeds the deterministic safety limit");
    }
    if !valid_sha256(request.historical_prices_sha256)
        || !valid_sha256(&request.paired_summary.ledger_sha256)
    {
        bail!("sensitivity input commitment is not a canonical SHA-256");
    }
    if request.paired_summary.schema_version != PAIRED_METRIC_SCHEMA_VERSION
        || request.paired_summary.profile_id != request.profile_id
        || request.paired_summary.hlp_scenario != request.hlp_scenario
    {
        bail!("paired metric summary identity mismatch");
    }
    Ok(())
}

fn validate_row_identity(
    row: &PairedOperationMetricRowV1,
    request: &SensitivityRequestV1<'_>,
) -> Result<()> {
    if row.schema_version != PAIRED_METRIC_SCHEMA_VERSION
        || row.profile_id != request.profile_id
        || row.arm != request.arm
        || row.hlp_scenario != request.hlp_scenario
        || row.market.trim().is_empty()
        || row.token0.trim().is_empty()
        || row.token1.trim().is_empty()
        || row.operation_id.trim().is_empty()
        || row.factual_record_sha256.trim().is_empty()
        || row.dusk_state_sha256.trim().is_empty()
    {
        bail!("paired metric row identity mismatch");
    }
    if row.revenue.dusk_revenue_origin
        != revenue_origin_from_flags(row.synthetic_arbitrage, row.synthetic_protocol_settlement)?
    {
        bail!("paired metric revenue origin differs from row provenance flags");
    }
    Ok(())
}

fn validate_market_identity(
    identities: &mut BTreeMap<String, MarketIdentity>,
    row: &PairedOperationMetricRowV1,
) -> Result<()> {
    match identities.get_mut(&row.market) {
        Some(identity) => {
            if identity.token0 != row.token0
                || identity.token1 != row.token1
                || identity.token0_decimals != row.token0_decimals
                || identity.token1_decimals != row.token1_decimals
            {
                bail!("paired metric token identity changed within a market");
            }
            if row.block_time_unix_seconds < identity.last_time {
                bail!("paired metric market time regressed");
            }
            identity.last_time = row.block_time_unix_seconds;
        }
        None => {
            identities.insert(
                row.market.clone(),
                MarketIdentity {
                    token0: row.token0.clone(),
                    token1: row.token1.clone(),
                    token0_decimals: row.token0_decimals,
                    token1_decimals: row.token1_decimals,
                    last_time: row.block_time_unix_seconds,
                },
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_summary(
    summary: &PairedMetricLedgerSummaryV1,
    profile_id: &str,
    row_count: u64,
    market_count: usize,
    synthetic_rows: u64,
    synthetic_settlement_rows: u64,
    swap_rows: u64,
    borrow_rows: u64,
    repay_rows: u64,
    inexact_liquidation_rows: u64,
    first_order: Option<crate::canonical::ChainOrder>,
    last_order: Option<crate::canonical::ChainOrder>,
) -> Result<()> {
    if summary.schema_version != PAIRED_METRIC_SCHEMA_VERSION
        || summary.profile_id != profile_id
        || summary.row_count != row_count
        || summary.market_count != u64::try_from(market_count)?
        || summary.synthetic_arbitrage_rows != synthetic_rows
        || summary.synthetic_protocol_settlement_rows != synthetic_settlement_rows
        || summary.swap_rows != swap_rows
        || summary.borrow_rows != borrow_rows
        || summary.repay_rows != repay_rows
        || summary.liquidation_rows_without_dusk_exactness != inexact_liquidation_rows
        || summary.first_order != first_order
        || summary.last_order != last_order
    {
        bail!("paired metric summary coverage differs from the ledger");
    }
    Ok(())
}

fn observe_holdout_row(
    row: &PairedOperationMetricRowV1,
    prices: &HistoricalPriceIndex,
) -> Result<Totals> {
    let (v1_atoms, dusk_atoms) = stakeholder_revenue_atoms(&row.revenue)?;
    let v1_value = value_pair(prices, row, &v1_atoms.0, &v1_atoms.1)?;
    let dusk_value = value_pair(prices, row, &dusk_atoms.0, &dusk_atoms.1)?;
    let mut totals = Totals {
        v1_revenue: v1_value,
        ..Totals::default()
    };
    match row.revenue.dusk_revenue_origin {
        RevenueOriginV1::FactualUserFlow => totals.dusk_user_revenue = dusk_value,
        RevenueOriginV1::SyntheticArbitrage => {
            if !totals.v1_revenue.is_zero() {
                bail!("synthetic arbitrage row carries factual V1 stakeholder revenue");
            }
            totals.dusk_synthetic_arbitrage_revenue = dusk_value;
        }
        RevenueOriginV1::SyntheticProtocolSettlement => {
            if !totals.v1_revenue.is_zero() {
                bail!("synthetic settlement row carries factual V1 stakeholder revenue");
            }
            totals.dusk_synthetic_protocol_settlement_revenue = dusk_value;
        }
    }

    if let Some(swap) = &row.swap {
        if row.synthetic_arbitrage || row.synthetic_protocol_settlement {
            if swap.v1.is_some() {
                bail!("synthetic row carries a factual V1 swap");
            }
        } else {
            let factual = swap
                .v1
                .as_ref()
                .context("non-synthetic swap row lacks factual V1 execution")?;
            let (mint, decimals) = if factual.asset_in_base {
                (&row.token0, row.token0_decimals)
            } else {
                (&row.token1, row.token1_decimals)
            };
            let weight = prices.value_atoms(
                mint,
                decimals,
                row.block_time_unix_seconds,
                &parse_unsigned(&factual.reserve_credit_in)?,
            )?;
            totals.factual_swap_input += &weight;
            if let Some(cost) = &swap.quoted_execution_cost_delta_ppb {
                totals.weighted_execution_cost_numerator +=
                    BigRational::from_integer(parse_signed(cost)?) * &weight;
                totals.execution_weight += weight;
            } else {
                totals.unsupported_execution_rows = 1;
            }
        }
    }
    Ok(totals)
}

fn stakeholder_revenue_atoms(
    revenue: &crate::metric_engine::PairedRevenueDeltaV1,
) -> Result<((BigInt, BigInt), (BigInt, BigInt))> {
    fn core(side: &RevenueSideDeltaV1) -> Result<BigInt> {
        RevenueLaneV1::ALL
            .into_iter()
            .filter(|lane| lane.accounting_class() == RevenueAccountingClassV1::EconomicRevenue)
            .try_fold(BigInt::zero(), |total, lane| {
                Ok(total + parse_unsigned(side.amount(lane))?)
            })
    }
    Ok((
        (core(&revenue.v1_base)?, core(&revenue.v1_quote)?),
        (core(&revenue.dusk_base)?, core(&revenue.dusk_quote)?),
    ))
}

fn value_pair(
    prices: &HistoricalPriceIndex,
    row: &PairedOperationMetricRowV1,
    amount0: &BigInt,
    amount1: &BigInt,
) -> Result<BigRational> {
    Ok(prices.value_atoms(
        &row.token0,
        row.token0_decimals,
        row.block_time_unix_seconds,
        amount0,
    )? + prices.value_atoms(
        &row.token1,
        row.token1_decimals,
        row.block_time_unix_seconds,
        amount1,
    )?)
}

fn point_estimate(totals: &Totals) -> Result<SensitivityPointEstimateV1> {
    let delta = &totals.dusk_user_revenue - &totals.v1_revenue;
    let (execution, execution_reason) = if totals.unsupported_execution_rows > 0 {
        (
            None,
            Some("one_or_more_factual_swaps_lack_a_comparable_dusk_quote".to_owned()),
        )
    } else if totals.execution_weight.is_zero() {
        (None, Some("factual_swap_input_weight_is_zero".to_owned()))
    } else {
        (
            Some(ExactAndReportedSignedV1::from_ratio(
                &(&totals.weighted_execution_cost_numerator / &totals.execution_weight),
            )?),
            None,
        )
    };
    Ok(SensitivityPointEstimateV1 {
        stakeholder_revenue_v1_usd_micro: SensitivityExactRationalV1::from_ratio(
            &totals.v1_revenue,
        )?,
        stakeholder_revenue_dusk_user_flow_usd_micro: SensitivityExactRationalV1::from_ratio(
            &totals.dusk_user_revenue,
        )?,
        stakeholder_user_flow_revenue_delta_usd_micro: ExactAndReportedSignedV1::from_ratio(
            &delta,
        )?,
        dusk_synthetic_arbitrage_revenue_usd_micro: SensitivityExactRationalV1::from_ratio(
            &totals.dusk_synthetic_arbitrage_revenue,
        )?,
        dusk_synthetic_protocol_settlement_revenue_usd_micro:
            SensitivityExactRationalV1::from_ratio(
                &totals.dusk_synthetic_protocol_settlement_revenue,
            )?,
        factual_swap_input_usd_micro: SensitivityExactRationalV1::from_ratio(
            &totals.factual_swap_input,
        )?,
        quoted_execution_cost_delta_ppb: execution,
        quoted_execution_cost_unsupported_reason: execution_reason,
        unsupported_execution_rows: totals.unsupported_execution_rows,
    })
}

fn bootstrap(
    weekly_totals: &BTreeMap<i64, Totals>,
    seed: u64,
    samples: u32,
) -> Result<PairedBootstrapEvidenceV1> {
    let blocks: Vec<(i64, &Totals)> = weekly_totals
        .iter()
        .map(|(bucket, totals)| (*bucket, totals))
        .collect();
    if blocks.is_empty() {
        bail!("paired bootstrap has no UTC seven-day blocks");
    }
    let mut generator = SplitMix64::new(seed);
    let mut path_hasher = Sha256::new();
    path_hasher.update(SAMPLE_PATH_HASH_DOMAIN);
    path_hasher.update(seed.to_be_bytes());
    path_hasher.update(samples.to_be_bytes());
    path_hasher.update(u64::try_from(blocks.len())?.to_be_bytes());
    let mut revenue_samples = Vec::with_capacity(usize::try_from(samples)?);
    let mut synthetic_samples = Vec::with_capacity(usize::try_from(samples)?);
    let mut settlement_samples = Vec::with_capacity(usize::try_from(samples)?);
    let mut execution_samples = Vec::with_capacity(usize::try_from(samples)?);
    let mut execution_supported = true;

    for sample in 0..samples {
        let sampled = paired_resample_once(&blocks, &mut generator, sample, &mut path_hasher)?;
        revenue_samples.push(&sampled.dusk_user_revenue - &sampled.v1_revenue);
        synthetic_samples.push(sampled.dusk_synthetic_arbitrage_revenue.clone());
        settlement_samples.push(sampled.dusk_synthetic_protocol_settlement_revenue.clone());
        if sampled.unsupported_execution_rows == 0 && !sampled.execution_weight.is_zero() {
            execution_samples
                .push(&sampled.weighted_execution_cost_numerator / &sampled.execution_weight);
        } else {
            execution_supported = false;
        }
    }

    let (execution_interval, execution_reason) = if execution_supported {
        (Some(interval(&mut execution_samples)?), None)
    } else {
        (
            None,
            Some(
                "at_least_one_paired_sample_has_missing_quote_or_zero_factual_swap_weight"
                    .to_owned(),
            ),
        )
    };
    Ok(PairedBootstrapEvidenceV1 {
        algorithm: BOOTSTRAP_ALGORITHM_V1.to_owned(),
        block_seconds: UTC_SEVEN_DAY_BLOCK_SECONDS,
        seed,
        samples,
        observed_block_count: u64::try_from(blocks.len())?,
        sampled_block_sequence_sha256: hex::encode(path_hasher.finalize()),
        stakeholder_user_flow_revenue_delta_usd_micro: interval(&mut revenue_samples)?,
        dusk_synthetic_arbitrage_revenue_usd_micro: interval(&mut synthetic_samples)?,
        dusk_synthetic_protocol_settlement_revenue_usd_micro: interval(&mut settlement_samples)?,
        quoted_execution_cost_delta_ppb: execution_interval,
        execution_cost_unsupported_reason: execution_reason,
    })
}

/// Samples whole UTC blocks. Each selected `Totals` contains both factual and
/// Dusk observations, so their pairing cannot be broken by separate RNG calls.
fn paired_resample_once(
    blocks: &[(i64, &Totals)],
    generator: &mut SplitMix64,
    sample: u32,
    path_hasher: &mut Sha256,
) -> Result<Totals> {
    let mut sampled = Totals::default();
    for draw in 0..blocks.len() {
        let index = generator.index(blocks.len())?;
        let (bucket, totals) = blocks[index];
        path_hasher.update(sample.to_be_bytes());
        path_hasher.update(u64::try_from(draw)?.to_be_bytes());
        path_hasher.update(bucket.to_be_bytes());
        sampled.add_assign(totals)?;
    }
    Ok(sampled)
}

fn interval(samples: &mut [BigRational]) -> Result<BootstrapIntervalV1> {
    if samples.is_empty() {
        bail!("bootstrap interval has no samples");
    }
    samples.sort();
    let last = samples.len() - 1;
    let lower = last.saturating_mul(25) / 1_000;
    let median = last.saturating_mul(500) / 1_000;
    let upper = last.saturating_mul(975) / 1_000;
    Ok(BootstrapIntervalV1 {
        lower_2_5_percent: ExactAndReportedSignedV1::from_ratio(&samples[lower])?,
        median: ExactAndReportedSignedV1::from_ratio(&samples[median])?,
        upper_97_5_percent: ExactAndReportedSignedV1::from_ratio(&samples[upper])?,
    })
}

fn top_market_sensitivity(
    aggregate: &Totals,
    markets: &BTreeMap<String, Totals>,
) -> Result<(
    Vec<TopMarketRankingV1>,
    Vec<TopMarketExclusionV1>,
    Option<String>,
)> {
    let bases = [
        TopMarketRankingBasisV1::FactualUsdSwapInput,
        TopMarketRankingBasisV1::FactualStakeholderRevenueUsd,
    ];
    let rankings = bases
        .into_iter()
        .map(|basis| rank_markets(markets, basis))
        .collect::<Result<Vec<_>>>()?;
    if markets.len() < 2 {
        return Ok((
            rankings,
            Vec::new(),
            Some("top_market_exclusion_requires_at_least_two_holdout_markets".to_owned()),
        ));
    }
    let mut exclusions = Vec::with_capacity(rankings.len());
    for ranking in &rankings {
        let top = ranking
            .markets
            .first()
            .context("market ranking unexpectedly empty")?;
        let excluded = markets
            .get(&top.market)
            .context("top-ranked market is absent from totals")?;
        exclusions.push(TopMarketExclusionV1 {
            basis: ranking.basis,
            excluded_market: top.market.clone(),
            excluded_factual_value_usd_micro: top.factual_value_usd_micro.clone(),
            remaining_market_count: u64::try_from(markets.len() - 1)?,
            estimate_without_top_market: point_estimate(&aggregate.subtract(excluded)?)?,
        });
    }
    Ok((rankings, exclusions, None))
}

fn rank_markets(
    markets: &BTreeMap<String, Totals>,
    basis: TopMarketRankingBasisV1,
) -> Result<TopMarketRankingV1> {
    let mut values: Vec<(&String, &BigRational)> = markets
        .iter()
        .map(|(market, totals)| {
            let value = match basis {
                TopMarketRankingBasisV1::FactualUsdSwapInput => &totals.factual_swap_input,
                TopMarketRankingBasisV1::FactualStakeholderRevenueUsd => &totals.v1_revenue,
            };
            (market, value)
        })
        .collect();
    values.sort_by(|(left_market, left_value), (right_market, right_value)| {
        right_value
            .cmp(left_value)
            .then_with(|| left_market.cmp(right_market))
    });
    let ranked = values
        .into_iter()
        .enumerate()
        .map(|(index, (market, value))| {
            Ok(RankedMarketV1 {
                rank: u64::try_from(index + 1)?,
                market: market.clone(),
                factual_value_usd_micro: SensitivityExactRationalV1::from_ratio(value)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TopMarketRankingV1 {
        basis,
        markets: ranked,
    })
}

fn utc_seven_day_bucket(unix_seconds: i64) -> Result<i64> {
    unix_seconds
        .div_euclid(UTC_SEVEN_DAY_BLOCK_SECONDS)
        .checked_mul(UTC_SEVEN_DAY_BLOCK_SECONDS)
        .context("UTC seven-day bucket overflow")
}

fn strict_jsonl_lines(bytes: &[u8]) -> Result<Vec<&[u8]>> {
    if bytes.is_empty() {
        bail!("JSONL input is empty");
    }
    if !bytes.ends_with(b"\n") {
        bail!("JSONL input is not newline-terminated");
    }
    let mut lines = Vec::new();
    for line in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n') {
        if line.is_empty() || line.iter().all(u8::is_ascii_whitespace) {
            bail!("JSONL input contains an empty line");
        }
        lines.push(line);
    }
    Ok(lines)
}

fn parse_unsigned(value: &TokenAmount) -> Result<BigInt> {
    BigInt::from_str(value.as_str()).context("invalid unsigned token amount")
}

fn parse_signed(value: &SignedTokenAmount) -> Result<BigInt> {
    BigInt::from_str(value.as_str()).context("invalid signed token amount")
}

fn truncate_to_signed(value: &BigRational) -> Result<SignedTokenAmount> {
    SignedTokenAmount::new((value.numer() / value.denom()).to_string())
        .context("truncated signed sensitivity value is not canonical")
}

fn pow10_big(exponent: u8) -> BigInt {
    BigInt::from(10_u8).pow(u32::from(exponent))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Stable identity for the market population used by sensitivity estimates.
pub fn market_set_sha256(markets: &BTreeSet<String>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"v1-vs-dusk/sensitivity-market-set/v1\0");
    for market in markets {
        digest.update((market.len() as u64).to_be_bytes());
        digest.update(market.as_bytes());
    }
    hex::encode(digest.finalize())
}

fn hash_serialized<T: Serialize>(domain: &[u8], value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut digest = Sha256::new();
    digest.update(domain);
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

/// Tiny, fully specified PRNG so dependency upgrades cannot change resampling.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn index(&mut self, upper: usize) -> Result<usize> {
        if upper == 0 {
            bail!("bootstrap PRNG upper bound is zero");
        }
        // Rejection sampling avoids modulo bias while remaining deterministic.
        let upper_u64 = u64::try_from(upper)?;
        let threshold = upper_u64.wrapping_neg() % upper_u64;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return Ok(usize::try_from(value % upper_u64)?);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canonical::{ChainOrder, ExactDecimal, HistoricalPriceQuality},
        counterfactual::CounterfactualStatusV1,
        metric_engine::{
            CapitalCheckpointV1, PairedRevenueDeltaV1, PairedSwapMetricV1, SwapExecutionMetricV1,
        },
        replay::HlpScenario,
    };
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    };

    static TEST_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn deterministic_artifact_and_paired_resampling_path() {
        let fixture = Fixture::new("determinism");
        let rows = vec![
            row("market-a", 1, 0, 10, 20, 100, 12),
            row(
                "market-a",
                u64::try_from(UTC_SEVEN_DAY_BLOCK_SECONDS).unwrap() + 1,
                1,
                30,
                60,
                200,
                24,
            ),
            row(
                "market-b",
                u64::try_from(2 * UTC_SEVEN_DAY_BLOCK_SECONDS).unwrap() + 1,
                2,
                50,
                100,
                300,
                36,
            ),
        ];
        let input = fixture.write(rows, true);
        let first = build_sensitivity_evidence(&input.request(44, 250)).unwrap();
        let second = build_sensitivity_evidence(&input.request(44, 250)).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.artifact_sha256, second.artifact_sha256);
        assert_eq!(
            first.paired_bootstrap.sampled_block_sequence_sha256,
            second.paired_bootstrap.sampled_block_sequence_sha256
        );

        // Directly prove every lane consumes one common draw vector: each
        // block has Dusk user revenue exactly 2x V1, so paired delta equals the
        // resampled V1 total for every sample.
        let mut blocks = BTreeMap::new();
        for index in 1..=3_i64 {
            blocks.insert(
                index * UTC_SEVEN_DAY_BLOCK_SECONDS,
                Totals {
                    v1_revenue: BigRational::from_integer(BigInt::from(index)),
                    dusk_user_revenue: BigRational::from_integer(BigInt::from(index * 2)),
                    ..Totals::default()
                },
            );
        }
        let evidence = bootstrap(&blocks, 7, 50).unwrap();
        assert_eq!(evidence.observed_block_count, 3);
        assert!(
            evidence
                .stakeholder_user_flow_revenue_delta_usd_micro
                .median
                .reported_signed_integer
                .as_str()
                .parse::<i64>()
                .unwrap()
                > 0
        );
        let borrowed: Vec<_> = blocks
            .iter()
            .map(|(bucket, totals)| (*bucket, totals))
            .collect();
        let mut generator = SplitMix64::new(91);
        let mut commitment = Sha256::new();
        for sample in 0..100 {
            let paired =
                paired_resample_once(&borrowed, &mut generator, sample, &mut commitment).unwrap();
            assert_eq!(
                paired.dusk_user_revenue,
                &paired.v1_revenue + &paired.v1_revenue
            );
            assert_eq!(
                &paired.dusk_user_revenue - &paired.v1_revenue,
                paired.v1_revenue
            );
        }
    }

    #[test]
    fn arbitration_and_protocol_settlement_remain_separate() {
        let totals = Totals {
            v1_revenue: BigRational::from_integer(BigInt::from(2_u8)),
            dusk_user_revenue: BigRational::from_integer(BigInt::from(5_u8)),
            dusk_synthetic_arbitrage_revenue: BigRational::from_integer(BigInt::from(7_u8)),
            dusk_synthetic_protocol_settlement_revenue: BigRational::from_integer(BigInt::from(
                11_u8,
            )),
            ..Totals::default()
        };
        let point = point_estimate(&totals).unwrap();
        assert_eq!(
            point
                .dusk_synthetic_arbitrage_revenue_usd_micro
                .numerator
                .as_str(),
            "7"
        );
        assert_eq!(
            point
                .dusk_synthetic_protocol_settlement_revenue_usd_micro
                .numerator
                .as_str(),
            "11"
        );
        assert_eq!(
            point
                .stakeholder_user_flow_revenue_delta_usd_micro
                .exact
                .numerator
                .as_str(),
            "3"
        );
    }

    #[test]
    fn top_market_ties_break_by_market_identity() {
        let mut markets = BTreeMap::new();
        for market in ["market-z", "market-a"] {
            markets.insert(
                market.to_owned(),
                Totals {
                    factual_swap_input: BigRational::from_integer(BigInt::from(10)),
                    v1_revenue: BigRational::from_integer(BigInt::from(5)),
                    ..Totals::default()
                },
            );
        }
        for basis in [
            TopMarketRankingBasisV1::FactualUsdSwapInput,
            TopMarketRankingBasisV1::FactualStakeholderRevenueUsd,
        ] {
            let ranking = rank_markets(&markets, basis).unwrap();
            assert_eq!(ranking.markets[0].market, "market-a");
            assert_eq!(ranking.markets[1].market, "market-z");
        }
    }

    #[test]
    fn missing_historical_mark_is_a_hard_failure() {
        let fixture = Fixture::new("missing-mark");
        let mut missing_quote = row("market-a", 1, 0, 10, 20, 100, 12);
        missing_quote.revenue.v1_quote.lp_swap_earned = token(1);
        missing_quote.revenue.dusk_quote.lp_swap_earned = token(1);
        let input = fixture.write(vec![missing_quote], false);
        let error = build_sensitivity_evidence(&input.request(1, 10)).unwrap_err();
        assert!(error
            .to_string()
            .contains("missing historical mark mint-1@0"));
    }

    #[test]
    fn changed_seed_changes_the_committed_resampling_path() {
        let fixture = Fixture::new("seed");
        let rows = vec![
            row("market-a", 1, 0, 10, 20, 100, 10),
            row(
                "market-b",
                u64::try_from(UTC_SEVEN_DAY_BLOCK_SECONDS).unwrap() + 1,
                1,
                20,
                40,
                200,
                20,
            ),
        ];
        let input = fixture.write(rows, true);
        let first = build_sensitivity_evidence(&input.request(1, 100)).unwrap();
        let second = build_sensitivity_evidence(&input.request(2, 100)).unwrap();
        assert_ne!(
            first.paired_bootstrap.sampled_block_sequence_sha256,
            second.paired_bootstrap.sampled_block_sequence_sha256
        );
    }

    struct Fixture {
        directory: PathBuf,
    }

    struct FixtureInput {
        ledger_path: PathBuf,
        price_path: PathBuf,
        summary: PairedMetricLedgerSummaryV1,
        prices_sha256: String,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let sequence = TEST_DIRECTORY_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "v1-vs-dusk-sensitivity-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&directory).unwrap();
            Self { directory }
        }

        fn write(
            &self,
            rows: Vec<PairedOperationMetricRowV1>,
            include_quote_mark: bool,
        ) -> FixtureInput {
            let ledger_path = self.directory.join("paired.jsonl");
            let mut ledger = Vec::new();
            for row in &rows {
                ledger.extend(serde_json::to_vec(row).unwrap());
                ledger.push(b'\n');
            }
            fs::write(&ledger_path, &ledger).unwrap();
            let markets: BTreeSet<_> = rows.iter().map(|row| row.market.clone()).collect();
            let summary = PairedMetricLedgerSummaryV1 {
                schema_version: PAIRED_METRIC_SCHEMA_VERSION,
                profile_id: "profile".to_owned(),
                hlp_scenario: HlpScenario::NoHlp,
                row_count: u64::try_from(rows.len()).unwrap(),
                market_count: u64::try_from(markets.len()).unwrap(),
                synthetic_arbitrage_rows: 0,
                synthetic_protocol_settlement_rows: 0,
                swap_rows: u64::try_from(rows.len()).unwrap(),
                spot_probes_enabled: false,
                spot_probe_sizing_window: None,
                spot_probe_rows: 0,
                spot_probe_observation_count: 0,
                spot_probe_sizing_source_sha256: None,
                spot_probe_sets_sha256: hex::encode(Sha256::new().finalize()),
                borrow_rows: 0,
                repay_rows: 0,
                liquidation_rows_without_dusk_exactness: 0,
                first_order: rows.first().map(|row| row.order),
                last_order: rows.last().map(|row| row.order),
                ledger_sha256: sha256_bytes(&ledger),
            };
            let price_path = self.directory.join("prices.jsonl");
            let mut price_bytes = Vec::new();
            let max_hour = rows
                .iter()
                .map(|row| row.block_time_unix_seconds.div_euclid(HOUR_SECONDS))
                .max()
                .unwrap_or(0);
            for hour in 0..=max_hour {
                let bucket = hour * HOUR_SECONDS;
                let marks = if include_quote_mark {
                    vec![price("mint-0", bucket), price("mint-1", bucket)]
                } else {
                    vec![price("mint-0", bucket)]
                };
                for mark in marks {
                    price_bytes.extend(serde_json::to_vec(&mark).unwrap());
                    price_bytes.push(b'\n');
                }
            }
            fs::write(&price_path, &price_bytes).unwrap();
            FixtureInput {
                ledger_path,
                price_path,
                summary,
                prices_sha256: sha256_bytes(&price_bytes),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    impl FixtureInput {
        fn request(&self, seed: u64, samples: u32) -> SensitivityRequestV1<'_> {
            SensitivityRequestV1 {
                run_id: "run",
                profile_id: "profile",
                arm: ReplayArm::FixedFlow,
                hlp_scenario: HlpScenario::NoHlp,
                holdout_start_unix_seconds: 0,
                holdout_end_unix_seconds_exclusive: 4 * UTC_SEVEN_DAY_BLOCK_SECONDS,
                paired_metric_ledger_path: &self.ledger_path,
                paired_summary: &self.summary,
                historical_prices_path: &self.price_path,
                historical_prices_sha256: &self.prices_sha256,
                historical_price_provider: "provider",
                included_markets: None,
                bootstrap_seed: seed,
                bootstrap_samples: samples,
            }
        }
    }

    fn price(mint: &str, bucket: i64) -> HistoricalPriceMarkV1 {
        HistoricalPriceMarkV1 {
            mint: mint.to_owned(),
            bucket_unix_seconds: bucket,
            price_usd: ExactDecimal::new("1").unwrap(),
            token_decimals: Some(0),
            provider: "provider".to_owned(),
            quality: HistoricalPriceQuality::Historical,
        }
    }

    fn row(
        market: &str,
        timestamp: u64,
        transaction_index: u32,
        v1_revenue: u64,
        dusk_revenue: u64,
        swap_input: u64,
        cost_ppb: i64,
    ) -> PairedOperationMetricRowV1 {
        let timestamp = i64::try_from(timestamp).unwrap();
        PairedOperationMetricRowV1 {
            schema_version: PAIRED_METRIC_SCHEMA_VERSION,
            profile_id: "profile".to_owned(),
            market: market.to_owned(),
            token0: "mint-0".to_owned(),
            token1: "mint-1".to_owned(),
            token0_decimals: 0,
            token1_decimals: 0,
            operation_id: format!("operation-{timestamp}-{transaction_index}"),
            order: ChainOrder {
                slot: u64::try_from(timestamp).unwrap(),
                transaction_index,
                outer_instruction_index: 0,
                inner_execution_index: None,
                event_sequence: 0,
            },
            block_time_unix_seconds: timestamp,
            arm: ReplayArm::FixedFlow,
            hlp_scenario: HlpScenario::NoHlp,
            synthetic_arbitrage: false,
            synthetic_protocol_settlement: false,
            dusk_status: CounterfactualStatusV1::Executed,
            swap: Some(PairedSwapMetricV1 {
                v1: Some(SwapExecutionMetricV1 {
                    asset_in_base: true,
                    gross_amount_in: token(swap_input),
                    reserve_credit_in: token(swap_input),
                    gross_amount_out: token(swap_input),
                    recipient_credit_out: token(swap_input),
                    slippage_ppb: signed(0),
                }),
                dusk_quote: Some(SwapExecutionMetricV1 {
                    asset_in_base: true,
                    gross_amount_in: token(swap_input),
                    reserve_credit_in: token(swap_input),
                    gross_amount_out: token(swap_input),
                    recipient_credit_out: token(swap_input),
                    slippage_ppb: signed(cost_ppb),
                }),
                dusk_realized: true,
                original_min_out_passed: Some(true),
                quoted_execution_cost_delta_ppb: Some(signed(cost_ppb)),
                realized_execution_cost_delta_ppb: Some(signed(cost_ppb)),
                slippage_delta_ppb: Some(signed(cost_ppb)),
            }),
            spot_probes: None,
            borrow: None,
            repay: None,
            safety: None,
            revenue: PairedRevenueDeltaV1 {
                v1_base: revenue_side(v1_revenue),
                v1_quote: revenue_side(0),
                dusk_base: revenue_side(dusk_revenue),
                dusk_quote: revenue_side(0),
                dusk_revenue_origin: RevenueOriginV1::FactualUserFlow,
            },
            capital_after: zero_capital(),
            factual_record_sha256: "factual".to_owned(),
            dusk_state_sha256: "dusk".to_owned(),
        }
    }

    fn revenue_side(value: u64) -> RevenueSideDeltaV1 {
        RevenueSideDeltaV1 {
            lp_swap_earned: token(value),
            protocol_fee_auction_swap_earned: token(0),
            protocol_buyback_swap_earned: token(0),
            retained_recenter_inventory: token(0),
            compounded_lp_swap_principal: token(0),
            lp_public_interest_accrued: token(0),
            protocol_public_interest_accrued: token(0),
            lp_public_interest_cash_realized: token(0),
            protocol_fee_auction_public_interest_cash_realized: token(0),
            protocol_buyback_public_interest_cash_realized: token(0),
            hlp_funding_interest_earned: token(0),
            protocol_fee_auction_hlp_funding_interest_earned: token(0),
            protocol_buyback_hlp_funding_interest_earned: token(0),
            flashloan_fee_v1_only: token(0),
            protocol_claim_realized_v1_only: token(0),
        }
    }

    fn zero_capital() -> CapitalCheckpointV1 {
        CapitalCheckpointV1 {
            v1_reserve0: token(0),
            v1_reserve1: token(0),
            v1_cash_reserve0: token(0),
            v1_cash_reserve1: token(0),
            v1_total_debt0: token(0),
            v1_total_debt1: token(0),
            v1_total_collateral0: token(0),
            v1_total_collateral1: token(0),
            v1_lp_supply: token(0),
            dusk_base_live_reserve: token(0),
            dusk_quote_live_reserve: token(0),
            dusk_base_cash_reserve: token(0),
            dusk_quote_cash_reserve: token(0),
            dusk_base_total_debt: token(0),
            dusk_quote_total_debt: token(0),
            dusk_ylp_supply: token(0),
        }
    }

    fn token(value: u64) -> TokenAmount {
        TokenAmount::new(value.to_string()).unwrap()
    }

    fn signed(value: i64) -> SignedTokenAmount {
        SignedTokenAmount::new(value.to_string()).unwrap()
    }
}

//! Exact paired metric rows emitted at the canonical replay join.
//!
//! The engine intentionally stores raw integer observations before producing
//! aggregate claims. Historical USD conversion, weighting, bootstrap sampling,
//! and report formatting consume this ledger; they never reinterpret events.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use dusk::constants::{BPS_DENOMINATOR, NAD};
use omnipair::CPCurve;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    canonical::{ChainOrder, EventPayloadV1, SignedTokenAmount, TokenAmount},
    counterfactual::{
        CounterfactualOperationRecordV1, CounterfactualStatusV1, DuskBorrowCapacityConstraintV1,
        DuskDebtObservationV1, DuskMarketCheckpointV1, DuskPublicInterestSideTransitionV1,
        DuskRevenueCheckpointV1, DuskSwapObservationV1,
    },
    evaluation::CounterfactualRecordObserverV1,
    factual_replay::{V1BehaviorObservationV1, V1RevenueDeltaV1},
    replay::{CanonicalOperationV1, HlpScenario, OperationPayloadV1, ReplayArm},
    spot_probe::{SpotProbeSetV1, SpotProbeSizingWindowV1},
    v1_engine::{V1BorrowCapacityV1, V1PairCheckpointV1, V1TokenSideV1},
    v1_reconciliation::V1ReplayLedgerRowV1,
};

pub const PAIRED_METRIC_SCHEMA_VERSION: u16 = 5;
const PPB: i128 = 1_000_000_000;

/// Venue owning one exact revenue lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevenueVenueV1 {
    V1,
    Dusk,
}

/// Mutually-exclusive provenance for Dusk revenue. V1 revenue exists only on
/// factual user-flow rows; both counterfactual origins deliberately carry zero
/// V1 revenue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevenueOriginV1 {
    FactualUserFlow,
    SyntheticArbitrage,
    SyntheticProtocolSettlement,
}

/// Stable exhaustive lane identifier. Adding or changing a lane requires a
/// paired-ledger schema bump so downstream accounting cannot silently drop it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevenueLaneV1 {
    LpSwapEarned,
    ProtocolFeeAuctionSwapEarned,
    ProtocolBuybackSwapEarned,
    RetainedRecenterInventory,
    CompoundedLpSwapPrincipal,
    LpPublicInterestAccrued,
    ProtocolPublicInterestAccrued,
    LpPublicInterestCashRealized,
    ProtocolFeeAuctionPublicInterestCashRealized,
    ProtocolBuybackPublicInterestCashRealized,
    HlpFundingInterestEarned,
    ProtocolFeeAuctionHlpFundingInterestEarned,
    ProtocolBuybackHlpFundingInterestEarned,
    FlashloanFeeV1Only,
    ProtocolClaimRealizedV1Only,
}

impl RevenueLaneV1 {
    pub const ALL: [Self; 15] = [
        Self::LpSwapEarned,
        Self::ProtocolFeeAuctionSwapEarned,
        Self::ProtocolBuybackSwapEarned,
        Self::RetainedRecenterInventory,
        Self::CompoundedLpSwapPrincipal,
        Self::LpPublicInterestAccrued,
        Self::ProtocolPublicInterestAccrued,
        Self::LpPublicInterestCashRealized,
        Self::ProtocolFeeAuctionPublicInterestCashRealized,
        Self::ProtocolBuybackPublicInterestCashRealized,
        Self::HlpFundingInterestEarned,
        Self::ProtocolFeeAuctionHlpFundingInterestEarned,
        Self::ProtocolBuybackHlpFundingInterestEarned,
        Self::FlashloanFeeV1Only,
        Self::ProtocolClaimRealizedV1Only,
    ];

    pub const fn accounting_class(self) -> RevenueAccountingClassV1 {
        match self {
            Self::LpSwapEarned
            | Self::ProtocolFeeAuctionSwapEarned
            | Self::ProtocolBuybackSwapEarned
            | Self::LpPublicInterestAccrued
            | Self::ProtocolPublicInterestAccrued
            | Self::HlpFundingInterestEarned
            | Self::ProtocolFeeAuctionHlpFundingInterestEarned
            | Self::ProtocolBuybackHlpFundingInterestEarned => {
                RevenueAccountingClassV1::EconomicRevenue
            }
            Self::LpPublicInterestCashRealized
            | Self::ProtocolFeeAuctionPublicInterestCashRealized
            | Self::ProtocolBuybackPublicInterestCashRealized
            | Self::ProtocolClaimRealizedV1Only => RevenueAccountingClassV1::CashRealizationMemo,
            Self::RetainedRecenterInventory => RevenueAccountingClassV1::InventoryMemo,
            Self::CompoundedLpSwapPrincipal => RevenueAccountingClassV1::PrincipalDispositionMemo,
            Self::FlashloanFeeV1Only => RevenueAccountingClassV1::VenueOnlySupplement,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevenueAccountingClassV1 {
    EconomicRevenue,
    CashRealizationMemo,
    InventoryMemo,
    PrincipalDispositionMemo,
    VenueOnlySupplement,
}

pub fn revenue_origin_from_flags(
    synthetic_arbitrage: bool,
    synthetic_protocol_settlement: bool,
) -> Result<RevenueOriginV1> {
    match (synthetic_arbitrage, synthetic_protocol_settlement) {
        (false, false) => Ok(RevenueOriginV1::FactualUserFlow),
        (true, false) => Ok(RevenueOriginV1::SyntheticArbitrage),
        (false, true) => Ok(RevenueOriginV1::SyntheticProtocolSettlement),
        (true, true) => bail!("paired metric row cannot have two synthetic revenue origins"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevenueSideDeltaV1 {
    pub lp_swap_earned: TokenAmount,
    pub protocol_fee_auction_swap_earned: TokenAmount,
    pub protocol_buyback_swap_earned: TokenAmount,
    pub retained_recenter_inventory: TokenAmount,
    pub compounded_lp_swap_principal: TokenAmount,
    pub lp_public_interest_accrued: TokenAmount,
    pub protocol_public_interest_accrued: TokenAmount,
    pub lp_public_interest_cash_realized: TokenAmount,
    pub protocol_fee_auction_public_interest_cash_realized: TokenAmount,
    pub protocol_buyback_public_interest_cash_realized: TokenAmount,
    pub hlp_funding_interest_earned: TokenAmount,
    pub protocol_fee_auction_hlp_funding_interest_earned: TokenAmount,
    pub protocol_buyback_hlp_funding_interest_earned: TokenAmount,
    pub flashloan_fee_v1_only: TokenAmount,
    pub protocol_claim_realized_v1_only: TokenAmount,
}

impl RevenueSideDeltaV1 {
    fn zero() -> Self {
        Self {
            lp_swap_earned: token(0),
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

    pub fn amount(&self, lane: RevenueLaneV1) -> &TokenAmount {
        match lane {
            RevenueLaneV1::LpSwapEarned => &self.lp_swap_earned,
            RevenueLaneV1::ProtocolFeeAuctionSwapEarned => &self.protocol_fee_auction_swap_earned,
            RevenueLaneV1::ProtocolBuybackSwapEarned => &self.protocol_buyback_swap_earned,
            RevenueLaneV1::RetainedRecenterInventory => &self.retained_recenter_inventory,
            RevenueLaneV1::CompoundedLpSwapPrincipal => &self.compounded_lp_swap_principal,
            RevenueLaneV1::LpPublicInterestAccrued => &self.lp_public_interest_accrued,
            RevenueLaneV1::ProtocolPublicInterestAccrued => &self.protocol_public_interest_accrued,
            RevenueLaneV1::LpPublicInterestCashRealized => &self.lp_public_interest_cash_realized,
            RevenueLaneV1::ProtocolFeeAuctionPublicInterestCashRealized => {
                &self.protocol_fee_auction_public_interest_cash_realized
            }
            RevenueLaneV1::ProtocolBuybackPublicInterestCashRealized => {
                &self.protocol_buyback_public_interest_cash_realized
            }
            RevenueLaneV1::HlpFundingInterestEarned => &self.hlp_funding_interest_earned,
            RevenueLaneV1::ProtocolFeeAuctionHlpFundingInterestEarned => {
                &self.protocol_fee_auction_hlp_funding_interest_earned
            }
            RevenueLaneV1::ProtocolBuybackHlpFundingInterestEarned => {
                &self.protocol_buyback_hlp_funding_interest_earned
            }
            RevenueLaneV1::FlashloanFeeV1Only => &self.flashloan_fee_v1_only,
            RevenueLaneV1::ProtocolClaimRealizedV1Only => &self.protocol_claim_realized_v1_only,
        }
    }

    pub fn stakeholder_economic_core_atoms(&self) -> Result<u128> {
        RevenueLaneV1::ALL
            .into_iter()
            .filter(|lane| lane.accounting_class() == RevenueAccountingClassV1::EconomicRevenue)
            .try_fold(0_u128, |total, lane| {
                total
                    .checked_add(parse_token(self.amount(lane))?)
                    .context("stakeholder economic revenue overflow")
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedRevenueDeltaV1 {
    pub v1_base: RevenueSideDeltaV1,
    pub v1_quote: RevenueSideDeltaV1,
    pub dusk_base: RevenueSideDeltaV1,
    pub dusk_quote: RevenueSideDeltaV1,
    /// Synthetic revenue is never counted as factual user-flow revenue.
    pub dusk_revenue_origin: RevenueOriginV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapExecutionMetricV1 {
    pub asset_in_base: bool,
    pub gross_amount_in: TokenAmount,
    pub reserve_credit_in: TokenAmount,
    pub gross_amount_out: TokenAmount,
    pub recipient_credit_out: TokenAmount,
    pub slippage_ppb: SignedTokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedSwapMetricV1 {
    /// Absent for a synthetic arbitrage row.
    pub v1: Option<SwapExecutionMetricV1>,
    /// The native quote exists even when historical intent failed min-out.
    pub dusk_quote: Option<SwapExecutionMetricV1>,
    pub dusk_realized: bool,
    pub original_min_out_passed: Option<bool>,
    /// Positive means Dusk returned less output than V1 for the same flow.
    pub quoted_execution_cost_delta_ppb: Option<SignedTokenAmount>,
    /// Absent when the Dusk intent was skipped or rejected.
    pub realized_execution_cost_delta_ppb: Option<SignedTokenAmount>,
    pub slippage_delta_ppb: Option<SignedTokenAmount>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedBorrowMetricV1 {
    pub debt_asset_base: bool,
    pub v1_actual_debt_before: TokenAmount,
    pub v1_actual_debt_after: TokenAmount,
    pub v1_gross_admissible_max_debt: TokenAmount,
    pub v1_actionable_max_debt: TokenAmount,
    pub dusk_gross_admissible_max_debt: TokenAmount,
    pub dusk_actionable_max_debt: TokenAmount,
    /// Raw requested-borrow atoms available before execution-capital gates.
    pub v1_gross_additional_headroom: TokenAmount,
    pub dusk_gross_additional_headroom: TokenAmount,
    /// Raw requested-borrow atoms available after every execution gate.
    pub v1_actionable_additional_headroom: TokenAmount,
    pub dusk_actionable_additional_headroom: TokenAmount,
    pub dusk_underwriting_max_additional: TokenAmount,
    pub dusk_global_health_floor_max_additional: TokenAmount,
    pub dusk_cash_max_additional: TokenAmount,
    pub dusk_daily_bucket_max_additional: TokenAmount,
    pub dusk_reduce_only_max_additional: Option<TokenAmount>,
    pub dusk_actionable_max_additional: TokenAmount,
    pub dusk_current_underwriting_satisfied: bool,
    pub dusk_current_global_health_floor_satisfied: bool,
    pub dusk_limiting_constraints: Vec<DuskBorrowCapacityConstraintV1>,
    pub preserved_capacity_fraction_numerator: TokenAmount,
    pub preserved_capacity_fraction_denominator: TokenAmount,
    pub dusk_target_debt: TokenAmount,
    pub dusk_debt_after: TokenAmount,
    pub dusk_raw_borrow_request: TokenAmount,
    pub dusk_requested_change: TokenAmount,
    pub dusk_filled_change: TokenAmount,
    pub dusk_unmet_change: TokenAmount,
    pub dusk_target_debt_gap: TokenAmount,
    pub actionable_capacity_multiplier_numerator: TokenAmount,
    pub actionable_capacity_multiplier_denominator: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedRepayMetricV1 {
    pub v1_debt_before: TokenAmount,
    pub v1_debt_after: TokenAmount,
    pub preserved_reduction_fraction_numerator: TokenAmount,
    pub preserved_reduction_fraction_denominator: TokenAmount,
    pub dusk_debt_before: TokenAmount,
    pub dusk_debt_after: TokenAmount,
    pub dusk_filled_reduction: TokenAmount,
    pub dusk_unmet_reduction: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyMetricV1 {
    pub source_operation_id: String,
    pub debt_asset_base: bool,
    pub v1_liquidation_shortfall: TokenAmount,
    pub dusk_liquidation_shortfall: Option<TokenAmount>,
    pub dusk_exact_liquidation_available: bool,
    pub dusk_settlement_final: bool,
    pub dusk_residual_auction_open: bool,
    pub synthetic_protocol_settlement: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapitalCheckpointV1 {
    pub v1_reserve0: TokenAmount,
    pub v1_reserve1: TokenAmount,
    pub v1_cash_reserve0: TokenAmount,
    pub v1_cash_reserve1: TokenAmount,
    pub v1_total_debt0: TokenAmount,
    pub v1_total_debt1: TokenAmount,
    pub v1_total_collateral0: TokenAmount,
    pub v1_total_collateral1: TokenAmount,
    pub v1_lp_supply: TokenAmount,
    pub dusk_base_live_reserve: TokenAmount,
    pub dusk_quote_live_reserve: TokenAmount,
    pub dusk_base_cash_reserve: TokenAmount,
    pub dusk_quote_cash_reserve: TokenAmount,
    pub dusk_base_total_debt: TokenAmount,
    pub dusk_quote_total_debt: TokenAmount,
    pub dusk_ylp_supply: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedOperationMetricRowV1 {
    pub schema_version: u16,
    pub profile_id: String,
    pub market: String,
    pub token0: String,
    pub token1: String,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub operation_id: String,
    pub order: ChainOrder,
    pub block_time_unix_seconds: i64,
    pub arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub synthetic_arbitrage: bool,
    pub synthetic_protocol_settlement: bool,
    pub dusk_status: CounterfactualStatusV1,
    pub swap: Option<PairedSwapMetricV1>,
    pub spot_probes: Option<SpotProbeSetV1>,
    pub borrow: Option<PairedBorrowMetricV1>,
    pub repay: Option<PairedRepayMetricV1>,
    pub safety: Option<SafetyMetricV1>,
    pub revenue: PairedRevenueDeltaV1,
    pub capital_after: CapitalCheckpointV1,
    pub factual_record_sha256: String,
    pub dusk_state_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedMetricLedgerSummaryV1 {
    pub schema_version: u16,
    pub profile_id: String,
    pub hlp_scenario: HlpScenario,
    pub row_count: u64,
    pub market_count: u64,
    pub synthetic_arbitrage_rows: u64,
    pub synthetic_protocol_settlement_rows: u64,
    pub swap_rows: u64,
    pub spot_probes_enabled: bool,
    pub spot_probe_sizing_window: Option<SpotProbeSizingWindowV1>,
    pub spot_probe_rows: u64,
    pub spot_probe_observation_count: u64,
    pub spot_probe_sizing_source_sha256: Option<String>,
    pub spot_probe_sets_sha256: String,
    pub borrow_rows: u64,
    pub repay_rows: u64,
    pub liquidation_rows_without_dusk_exactness: u64,
    pub first_order: Option<ChainOrder>,
    pub last_order: Option<ChainOrder>,
    pub ledger_sha256: String,
}

/// Atomic JSONL writer plus the minimum previous-state cache needed for exact
/// slippage and accrued-versus-cash interest accounting.
pub struct PairedMetricLedgerWriterV1 {
    profile_id: String,
    hlp_scenario: HlpScenario,
    final_path: PathBuf,
    partial_path: PathBuf,
    writer: BufWriter<File>,
    digest: Sha256,
    row_count: u64,
    synthetic_rows: u64,
    synthetic_protocol_settlement_rows: u64,
    swap_rows: u64,
    expected_spot_probe_window: Option<SpotProbeSizingWindowV1>,
    spot_probe_rows: u64,
    spot_probe_observation_count: u64,
    spot_probe_sizing_source_sha256: Option<String>,
    spot_probe_digest: Sha256,
    borrow_rows: u64,
    repay_rows: u64,
    missing_liquidations: u64,
    first_order: Option<ChainOrder>,
    last_order: Option<ChainOrder>,
    markets: BTreeMap<String, MarketMetricState>,
}

#[derive(Clone)]
struct MarketMetricState {
    token0: String,
    token1: String,
    token0_decimals: u8,
    token1_decimals: u8,
    v1_pair: V1PairCheckpointV1,
    dusk_revenue: DuskRevenueCheckpointV1,
}

impl PairedMetricLedgerWriterV1 {
    pub fn create(
        path: &Path,
        profile_id: impl Into<String>,
        hlp_scenario: HlpScenario,
    ) -> Result<Self> {
        Self::create_with_spot_probe_window(path, profile_id, hlp_scenario, None)
    }

    pub fn create_with_spot_probe_window(
        path: &Path,
        profile_id: impl Into<String>,
        hlp_scenario: HlpScenario,
        spot_probe_window: Option<SpotProbeSizingWindowV1>,
    ) -> Result<Self> {
        if let Some(window) = &spot_probe_window {
            window.validate()?;
        }
        if path.exists() {
            bail!(
                "refusing to overwrite paired metric ledger {}",
                path.display()
            );
        }
        let partial_path = path.with_extension("jsonl.partial");
        if partial_path.exists() {
            bail!(
                "paired metric partial exists {}; inspect it before retrying",
                partial_path.display()
            );
        }
        let writer = BufWriter::new(
            File::create(&partial_path)
                .with_context(|| format!("creating {}", partial_path.display()))?,
        );
        Ok(Self {
            profile_id: profile_id.into(),
            hlp_scenario,
            final_path: path.to_path_buf(),
            partial_path,
            writer,
            digest: Sha256::new(),
            row_count: 0,
            synthetic_rows: 0,
            synthetic_protocol_settlement_rows: 0,
            swap_rows: 0,
            expected_spot_probe_window: spot_probe_window,
            spot_probe_rows: 0,
            spot_probe_observation_count: 0,
            spot_probe_sizing_source_sha256: None,
            spot_probe_digest: Sha256::new(),
            borrow_rows: 0,
            repay_rows: 0,
            missing_liquidations: 0,
            first_order: None,
            last_order: None,
            markets: BTreeMap::new(),
        })
    }

    pub fn commit(mut self) -> Result<PairedMetricLedgerSummaryV1> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        drop(self.writer);
        fs::rename(&self.partial_path, &self.final_path).with_context(|| {
            format!(
                "committing paired metric ledger {}",
                self.final_path.display()
            )
        })?;
        Ok(PairedMetricLedgerSummaryV1 {
            schema_version: PAIRED_METRIC_SCHEMA_VERSION,
            profile_id: self.profile_id,
            hlp_scenario: self.hlp_scenario,
            row_count: self.row_count,
            market_count: u64::try_from(self.markets.len())?,
            synthetic_arbitrage_rows: self.synthetic_rows,
            synthetic_protocol_settlement_rows: self.synthetic_protocol_settlement_rows,
            swap_rows: self.swap_rows,
            spot_probes_enabled: self.expected_spot_probe_window.is_some(),
            spot_probe_sizing_window: self.expected_spot_probe_window,
            spot_probe_rows: self.spot_probe_rows,
            spot_probe_observation_count: self.spot_probe_observation_count,
            spot_probe_sizing_source_sha256: self.spot_probe_sizing_source_sha256,
            spot_probe_sets_sha256: hex::encode(self.spot_probe_digest.finalize()),
            borrow_rows: self.borrow_rows,
            repay_rows: self.repay_rows,
            liquidation_rows_without_dusk_exactness: self.missing_liquidations,
            first_order: self.first_order,
            last_order: self.last_order,
            ledger_sha256: hex::encode(self.digest.finalize()),
        })
    }

    fn write_row(&mut self, row: &PairedOperationMetricRowV1) -> Result<()> {
        let bytes = serde_json::to_vec(row)?;
        self.writer.write_all(&bytes)?;
        self.writer.write_all(b"\n")?;
        self.digest.update(&bytes);
        self.digest.update(b"\n");
        self.row_count = checked_add_u64(self.row_count, 1)?;
        Ok(())
    }
}

impl CounterfactualRecordObserverV1 for PairedMetricLedgerWriterV1 {
    fn observe(
        &mut self,
        operation: &CanonicalOperationV1,
        factual: &V1ReplayLedgerRowV1,
        dusk: &CounterfactualOperationRecordV1,
    ) -> Result<()> {
        let revenue_origin = revenue_origin_from_flags(
            dusk.synthetic_arbitrage,
            dusk.synthetic_protocol_settlement,
        )?;
        if factual.market != dusk.market
            || (!synthetic_record_uses_checkpoint_anchor(
                dusk.synthetic_arbitrage,
                dusk.synthetic_protocol_settlement,
            ) && factual.operation_id != operation.operation_id)
        {
            bail!(
                "paired metric observer received a cross-market or cross-operation join: factual_market={} dusk_market={} factual_operation={} canonical_operation={} dusk_operation={} synthetic_arbitrage={} synthetic_protocol_settlement={}",
                factual.market,
                dusk.market,
                factual.operation_id,
                operation.operation_id,
                dusk.operation_id,
                dusk.synthetic_arbitrage,
                dusk.synthetic_protocol_settlement,
            );
        }
        if (!dusk.synthetic_protocol_settlement && dusk.operation_id != operation.operation_id)
            || dusk.slot != operation.order.slot
        {
            bail!("Dusk record identity differs from its canonical operation");
        }
        if dusk.hlp_scenario != self.hlp_scenario {
            bail!("Dusk record belongs to another hLP scenario");
        }
        let prior = self.markets.get(&factual.market).cloned();
        let after_pair = if dusk.synthetic_protocol_settlement {
            &prior
                .as_ref()
                .context("synthetic floor settlement has no prior market checkpoint")?
                .v1_pair
        } else {
            &factual.compact_checkpoint.pair
        };
        if let Some(prior) = &prior {
            if prior.token0 != after_pair.token0.to_string()
                || prior.token1 != after_pair.token1.to_string()
                || prior.token0_decimals != after_pair.token0_decimals
                || prior.token1_decimals != after_pair.token1_decimals
            {
                bail!("market token identity or decimals changed during metric replay");
            }
        } else if !matches!(
            operation.payload,
            OperationPayloadV1::InitializeMarket { .. }
        ) {
            bail!("metric replay observed a market before initialization");
        }

        let swap = build_swap_metric(operation, factual, dusk, prior.as_ref(), after_pair)?;
        let factual_swap = matches!(operation.payload, OperationPayloadV1::SwapExactIn { .. })
            && !dusk.synthetic_arbitrage
            && !dusk.synthetic_protocol_settlement;
        let expected_spot_probe = factual_swap
            && self
                .expected_spot_probe_window
                .as_ref()
                .is_some_and(|window| window.contains(operation.block_time_unix_seconds));
        if expected_spot_probe != dusk.spot_probes.is_some() {
            bail!("paired metric row spot-probe presence differs from its sealed sizing window");
        }
        if let Some(probes) = &dusk.spot_probes {
            probes.validate_hash()?;
            if probes.profile_id != self.profile_id
                || probes.arm != dusk.arm
                || probes.hlp_scenario != self.hlp_scenario
                || probes.operation_id != operation.operation_id
                || probes.market != factual.market
                || probes.slot != operation.order.slot
                || probes.factual_record_sha256 != factual.factual_record_sha256
                || self.expected_spot_probe_window.as_ref() != Some(&probes.sizing_window)
            {
                bail!("spot probe set identity differs from its paired metric row");
            }
            if self
                .spot_probe_sizing_source_sha256
                .as_ref()
                .is_some_and(|hash| hash != &probes.sizing_source_sha256)
            {
                bail!("paired metric ledger mixed spot probe sizing sources");
            }
            self.spot_probe_sizing_source_sha256
                .get_or_insert_with(|| probes.sizing_source_sha256.clone());
            self.spot_probe_rows = checked_add_u64(self.spot_probe_rows, 1)?;
            self.spot_probe_observation_count = self
                .spot_probe_observation_count
                .checked_add(u64::try_from(probes.observations.len())?)
                .context("paired spot probe observation count overflow")?;
            self.spot_probe_digest
                .update(probes.probe_set_sha256.as_bytes());
            self.spot_probe_digest.update(b"\n");
        }
        let (borrow, repay) = if synthetic_record_uses_checkpoint_anchor(
            dusk.synthetic_arbitrage,
            dusk.synthetic_protocol_settlement,
        ) {
            (None, None)
        } else {
            (
                build_borrow_metric(factual, dusk).with_context(|| {
                    format!(
                        "building borrow metric for operation {} in market {}",
                        operation.operation_id, factual.market
                    )
                })?,
                build_repay_metric(factual, dusk).with_context(|| {
                    format!(
                        "building repay metric for operation {} in market {}",
                        operation.operation_id, factual.market
                    )
                })?,
            )
        };
        let safety = build_safety_metric(operation, dusk)?;
        let revenue = build_revenue_delta(&factual.revenue_delta, dusk, prior.as_ref())?;
        if revenue.dusk_revenue_origin != revenue_origin {
            bail!("paired metric revenue origin differs from its Dusk row");
        }
        let row = PairedOperationMetricRowV1 {
            schema_version: PAIRED_METRIC_SCHEMA_VERSION,
            profile_id: self.profile_id.clone(),
            market: factual.market.clone(),
            token0: after_pair.token0.to_string(),
            token1: after_pair.token1.to_string(),
            token0_decimals: after_pair.token0_decimals,
            token1_decimals: after_pair.token1_decimals,
            operation_id: operation.operation_id.clone(),
            order: operation.order,
            block_time_unix_seconds: operation.block_time_unix_seconds,
            arm: dusk.arm,
            hlp_scenario: dusk.hlp_scenario,
            synthetic_arbitrage: dusk.synthetic_arbitrage,
            synthetic_protocol_settlement: dusk.synthetic_protocol_settlement,
            dusk_status: dusk.status,
            swap,
            spot_probes: dusk.spot_probes.clone(),
            borrow,
            repay,
            safety,
            revenue,
            capital_after: capital_checkpoint(after_pair, &dusk.checkpoint)?,
            factual_record_sha256: factual.factual_record_sha256.clone(),
            dusk_state_sha256: dusk.state_sha256.clone(),
        };
        if self.first_order.is_none() {
            self.first_order = Some(operation.order);
        }
        self.last_order = Some(operation.order);
        if row.synthetic_arbitrage {
            self.synthetic_rows = checked_add_u64(self.synthetic_rows, 1)?;
        }
        if row.synthetic_protocol_settlement {
            self.synthetic_protocol_settlement_rows =
                checked_add_u64(self.synthetic_protocol_settlement_rows, 1)?;
        }
        if row.swap.is_some() {
            self.swap_rows = checked_add_u64(self.swap_rows, 1)?;
        }
        if row.borrow.is_some() {
            self.borrow_rows = checked_add_u64(self.borrow_rows, 1)?;
        }
        if row.repay.is_some() {
            self.repay_rows = checked_add_u64(self.repay_rows, 1)?;
        }
        if row
            .safety
            .as_ref()
            .is_some_and(|metric| !metric.dusk_exact_liquidation_available)
        {
            self.missing_liquidations = checked_add_u64(self.missing_liquidations, 1)?;
        }
        self.write_row(&row)?;
        self.markets.insert(
            factual.market.clone(),
            MarketMetricState {
                token0: after_pair.token0.to_string(),
                token1: after_pair.token1.to_string(),
                token0_decimals: after_pair.token0_decimals,
                token1_decimals: after_pair.token1_decimals,
                v1_pair: after_pair.clone(),
                dusk_revenue: dusk.revenue.clone(),
            },
        );
        Ok(())
    }
}

fn synthetic_record_uses_checkpoint_anchor(
    synthetic_arbitrage: bool,
    synthetic_protocol_settlement: bool,
) -> bool {
    synthetic_arbitrage || synthetic_protocol_settlement
}

fn build_swap_metric(
    operation: &CanonicalOperationV1,
    factual: &V1ReplayLedgerRowV1,
    dusk: &CounterfactualOperationRecordV1,
    prior: Option<&MarketMetricState>,
    after_pair: &V1PairCheckpointV1,
) -> Result<Option<PairedSwapMetricV1>> {
    if dusk.synthetic_arbitrage {
        let dusk_quote = dusk
            .swap
            .as_ref()
            .map(|quote| dusk_swap_metric(quote, after_pair))
            .transpose()?;
        return Ok(Some(PairedSwapMetricV1 {
            v1: None,
            dusk_quote,
            dusk_realized: dusk.status == CounterfactualStatusV1::Executed,
            original_min_out_passed: None,
            quoted_execution_cost_delta_ppb: None,
            realized_execution_cost_delta_ppb: None,
            slippage_delta_ppb: None,
        }));
    }
    let OperationPayloadV1::SwapExactIn {
        token0_in,
        gross_amount_in,
        vault_credit_in,
        vault_debit_out,
        recipient_credit_out,
        ..
    } = &operation.payload
    else {
        if dusk.swap.is_some() {
            bail!("non-swap canonical operation emitted a Dusk swap observation");
        }
        return Ok(None);
    };
    let _prior = prior.context("V1 swap has no preceding exact market checkpoint")?;
    let gross_input = parse_token(gross_amount_in)?;
    let v1_gross_out = parse_token(vault_debit_out)?;
    let swap_fee = fee_ceil_u128(gross_input, after_pair.swap_fee_bps)?;
    let protocol_fee = fee_ceil_u128(swap_fee, factual.compact_checkpoint.authority.swap_bps)?;
    let curve_input = gross_input
        .checked_sub(swap_fee)
        .context("V1 swap fee exceeds gross input")?;
    let reserve_input_credit = gross_input
        .checked_sub(protocol_fee)
        .context("V1 protocol swap fee exceeds gross input")?;
    let (pre_reserve0, pre_reserve1) = if *token0_in {
        (
            u128::from(after_pair.reserve0)
                .checked_sub(reserve_input_credit)
                .context("V1 token0 pre-swap reserve underflow")?,
            u128::from(after_pair.reserve1)
                .checked_add(v1_gross_out)
                .context("V1 token1 pre-swap reserve overflow")?,
        )
    } else {
        (
            u128::from(after_pair.reserve0)
                .checked_add(v1_gross_out)
                .context("V1 token0 pre-swap reserve overflow")?,
            u128::from(after_pair.reserve1)
                .checked_sub(reserve_input_credit)
                .context("V1 token1 pre-swap reserve underflow")?,
        )
    };
    let pre_reserve0_u64 = u64::try_from(pre_reserve0)?;
    let pre_reserve1_u64 = u64::try_from(pre_reserve1)?;
    let reproduced = CPCurve::calculate_amount_out(
        if *token0_in {
            pre_reserve0_u64
        } else {
            pre_reserve1_u64
        },
        if *token0_in {
            pre_reserve1_u64
        } else {
            pre_reserve0_u64
        },
        u64::try_from(curve_input)?,
    )
    .map_err(|error| anyhow::anyhow!("V1 paired swap native quote failed: {error}"))?;
    if u128::from(reproduced) != v1_gross_out {
        bail!("V1 paired swap output differs from reconstructed post-accrual pre-state quote");
    }
    let (expected_lp_fee, expected_protocol_fee) = if *token0_in {
        (
            factual.revenue_delta.lp_swap_fee0,
            factual.revenue_delta.protocol_swap_fee0,
        )
    } else {
        (
            factual.revenue_delta.lp_swap_fee1,
            factual.revenue_delta.protocol_swap_fee1,
        )
    };
    if expected_protocol_fee != protocol_fee
        || expected_lp_fee
            != swap_fee
                .checked_sub(protocol_fee)
                .context("V1 protocol fee exceeds total fee")?
    {
        bail!("V1 paired swap fee split differs from factual native revenue delta");
    }
    let v1_slippage = cpmm_slippage_ppb(
        *token0_in,
        curve_input,
        v1_gross_out,
        pre_reserve0_u64,
        pre_reserve1_u64,
    )?;
    let v1 = SwapExecutionMetricV1 {
        asset_in_base: *token0_in,
        gross_amount_in: gross_amount_in.clone(),
        reserve_credit_in: vault_credit_in.clone(),
        gross_amount_out: vault_debit_out.clone(),
        recipient_credit_out: recipient_credit_out.clone(),
        slippage_ppb: signed(v1_slippage)?,
    };
    let dusk_quote = dusk
        .swap
        .as_ref()
        .map(|quote| dusk_swap_metric(quote, after_pair))
        .transpose()?;
    let quote_cost = dusk_quote
        .as_ref()
        .map(|quote| {
            output_cost_delta_ppb(
                parse_token(recipient_credit_out)?,
                parse_token(&quote.recipient_credit_out)?,
            )
        })
        .transpose()?
        .map(signed)
        .transpose()?;
    let realized = dusk.status == CounterfactualStatusV1::Executed;
    let realized_cost = if realized { quote_cost.clone() } else { None };
    let slippage_delta = dusk_quote
        .as_ref()
        .map(|quote| {
            let dusk_slippage = parse_signed(&quote.slippage_ppb)?;
            dusk_slippage
                .checked_sub(v1_slippage)
                .context("slippage delta overflow")
        })
        .transpose()?
        .map(signed)
        .transpose()?;
    Ok(Some(PairedSwapMetricV1 {
        v1: Some(v1),
        dusk_quote,
        dusk_realized: realized,
        original_min_out_passed: dusk
            .swap
            .as_ref()
            .map(|quote| quote.original_min_out_passed),
        quoted_execution_cost_delta_ppb: quote_cost,
        realized_execution_cost_delta_ppb: realized_cost,
        slippage_delta_ppb: slippage_delta,
    }))
}

fn dusk_swap_metric(
    quote: &DuskSwapObservationV1,
    pair: &V1PairCheckpointV1,
) -> Result<SwapExecutionMetricV1> {
    let input = parse_token(&quote.curve_input)?;
    let output = parse_token(&quote.curve_gross_output)?;
    let start_price = parse_token(&quote.start_price_nad)?;
    let slippage = concentrated_slippage_ppb(
        quote.asset_in_base,
        input,
        output,
        start_price,
        pair.token0_decimals,
        pair.token1_decimals,
    )?;
    Ok(SwapExecutionMetricV1 {
        asset_in_base: quote.asset_in_base,
        gross_amount_in: quote.gross_amount_in.clone(),
        reserve_credit_in: quote.reserve_credit_in.clone(),
        gross_amount_out: quote.amount_out_before_token_fee.clone(),
        recipient_credit_out: quote.recipient_credit_out.clone(),
        slippage_ppb: signed(slippage)?,
    })
}

fn build_borrow_metric(
    factual: &V1ReplayLedgerRowV1,
    dusk: &CounterfactualOperationRecordV1,
) -> Result<Option<PairedBorrowMetricV1>> {
    let Some(V1BehaviorObservationV1::Borrow {
        capacity_before,
        capacity_after,
        capacity_fraction_numerator,
        capacity_fraction_denominator,
        ..
    }) = factual.behavior.as_ref()
    else {
        return Ok(None);
    };
    let observation = dusk
        .debt
        .as_ref()
        .context("factual borrow has no Dusk debt observation")?;
    validate_dusk_borrow_capacity_observation(observation)?;
    validate_v1_borrow_capacity_pair(capacity_before, capacity_after, observation)?;
    let v1_actionable = capacity_before
        .actual_debt
        .checked_add(capacity_before.actionable_remaining_capacity)
        .context("V1 actionable capacity overflow")?;
    let dusk_gross = observation
        .gross_admissible_max_debt
        .as_ref()
        .context("factual borrow has no Dusk gross capacity decomposition")?;
    let dusk_actionable_token = observation
        .actionable_max_debt
        .as_ref()
        .context("factual borrow has no Dusk actionable capacity decomposition")?;
    let required_capacity = |value: &Option<TokenAmount>, name: &str| {
        value
            .clone()
            .with_context(|| format!("factual borrow has no Dusk {name} capacity bound"))
    };
    let dusk_gross_headroom = required_capacity(
        &observation.underwriting_max_additional,
        "gross additional headroom",
    )?;
    let dusk_actionable_headroom = required_capacity(
        &observation.actionable_max_additional,
        "actionable additional headroom",
    )?;
    Ok(Some(PairedBorrowMetricV1 {
        debt_asset_base: observation.debt_asset_base,
        v1_actual_debt_before: token(u128::from(capacity_before.actual_debt)),
        v1_actual_debt_after: token(u128::from(capacity_after.actual_debt)),
        v1_gross_admissible_max_debt: token(u128::from(capacity_before.admissible_max_debt)),
        v1_actionable_max_debt: token(u128::from(v1_actionable)),
        dusk_gross_admissible_max_debt: dusk_gross.clone(),
        dusk_actionable_max_debt: dusk_actionable_token.clone(),
        v1_gross_additional_headroom: token(u128::from(capacity_before.remaining_capacity)),
        dusk_gross_additional_headroom: dusk_gross_headroom.clone(),
        v1_actionable_additional_headroom: token(u128::from(
            capacity_before.actionable_remaining_capacity,
        )),
        dusk_actionable_additional_headroom: dusk_actionable_headroom.clone(),
        dusk_underwriting_max_additional: required_capacity(
            &observation.underwriting_max_additional,
            "underwriting",
        )?,
        dusk_global_health_floor_max_additional: required_capacity(
            &observation.global_health_floor_max_additional,
            "global-health-floor",
        )?,
        dusk_cash_max_additional: required_capacity(&observation.cash_max_additional, "cash")?,
        dusk_daily_bucket_max_additional: required_capacity(
            &observation.daily_bucket_max_additional,
            "daily-bucket",
        )?,
        dusk_reduce_only_max_additional: observation.reduce_only_max_additional.clone(),
        dusk_actionable_max_additional: required_capacity(
            &observation.actionable_max_additional,
            "actionable-additional",
        )?,
        dusk_current_underwriting_satisfied: observation
            .current_underwriting_satisfied
            .context("factual borrow has no Dusk current-underwriting assertion")?,
        dusk_current_global_health_floor_satisfied: observation
            .current_global_health_floor_satisfied
            .context("factual borrow has no Dusk current-global-health assertion")?,
        dusk_limiting_constraints: observation.limiting_constraints.clone(),
        preserved_capacity_fraction_numerator: capacity_fraction_numerator.clone(),
        preserved_capacity_fraction_denominator: capacity_fraction_denominator.clone(),
        dusk_target_debt: observation.target_debt.clone(),
        dusk_debt_after: observation.debt_after.clone(),
        dusk_raw_borrow_request: observation
            .raw_borrow_request
            .clone()
            .context("factual borrow has no raw Dusk borrow request")?,
        dusk_requested_change: observation.requested_change.clone(),
        dusk_filled_change: observation.filled_change.clone(),
        dusk_unmet_change: observation.unmet_change.clone(),
        dusk_target_debt_gap: observation
            .target_debt_gap
            .clone()
            .context("factual borrow has no Dusk target-debt gap")?,
        actionable_capacity_multiplier_numerator: dusk_actionable_headroom,
        actionable_capacity_multiplier_denominator: token(u128::from(
            capacity_before.actionable_remaining_capacity,
        )),
    }))
}

fn validate_v1_borrow_capacity_pair(
    before: &V1BorrowCapacityV1,
    after: &V1BorrowCapacityV1,
    dusk: &DuskDebtObservationV1,
) -> Result<()> {
    if before.position != after.position
        || before.debt_token != after.debt_token
        || dusk.debt_asset_base != (before.debt_token == V1TokenSideV1::Token0)
    {
        bail!("V1 and Dusk borrow observations disagree on position or debt-token side");
    }
    for capacity in [before, after] {
        let remaining = capacity
            .admissible_max_debt
            .saturating_sub(capacity.actual_debt);
        if capacity.remaining_capacity != remaining
            || capacity.actionable_remaining_capacity != remaining.min(capacity.cash_reserve)
        {
            bail!("V1 borrow capacity headroom is inconsistent with native capacity fields");
        }
    }
    Ok(())
}

fn validate_dusk_borrow_capacity_observation(observation: &DuskDebtObservationV1) -> Result<()> {
    let required = |value: &Option<TokenAmount>, name: &str| -> Result<u128> {
        parse_token(
            value
                .as_ref()
                .with_context(|| format!("Dusk borrow observation has no {name}"))?,
        )
    };
    let before = parse_token(&observation.debt_before)?;
    let gross = required(
        &observation.gross_admissible_max_debt,
        "gross admissible max debt",
    )?;
    let actionable = required(&observation.actionable_max_debt, "actionable max debt")?;
    let underwriting = required(
        &observation.underwriting_max_additional,
        "underwriting additional bound",
    )?;
    let global_health = required(
        &observation.global_health_floor_max_additional,
        "global-health additional bound",
    )?;
    let cash = required(&observation.cash_max_additional, "cash additional bound")?;
    let daily = required(
        &observation.daily_bucket_max_additional,
        "daily-bucket additional bound",
    )?;
    let reduce_only = observation
        .reduce_only_max_additional
        .as_ref()
        .map(parse_token)
        .transpose()?;
    if reduce_only.is_some_and(|bound| bound != 0) {
        bail!("Dusk reduce-only capacity bound must be exactly zero when present");
    }
    let actionable_additional = required(
        &observation.actionable_max_additional,
        "actionable additional bound",
    )?;
    let expected_actionable = [underwriting, global_health, cash, daily]
        .into_iter()
        .chain(reduce_only)
        .min()
        .context("Dusk borrow capacity has no execution bounds")?;
    if actionable_additional != expected_actionable {
        bail!("Dusk actionable capacity is not the exact minimum native bound");
    }
    let mut expected_constraints = Vec::new();
    for (bound, constraint) in [
        (underwriting, DuskBorrowCapacityConstraintV1::Underwriting),
        (
            global_health,
            DuskBorrowCapacityConstraintV1::GlobalHealthFloor,
        ),
        (cash, DuskBorrowCapacityConstraintV1::Cash),
        (daily, DuskBorrowCapacityConstraintV1::DailyBorrowBucket),
    ] {
        if bound == actionable_additional {
            expected_constraints.push(constraint);
        }
    }
    if reduce_only == Some(actionable_additional) {
        expected_constraints.push(DuskBorrowCapacityConstraintV1::ReduceOnly);
    }
    if observation.limiting_constraints != expected_constraints {
        bail!("Dusk limiting constraints do not exactly match all tied native bounds");
    }

    let underwriting_satisfied = observation
        .current_underwriting_satisfied
        .context("Dusk borrow observation has no current-underwriting assertion")?;
    if underwriting_satisfied {
        if gross < before || actionable < before || actionable > gross {
            bail!("Dusk underwritten gross/actionable debt ordering is invalid");
        }
    } else if gross >= before || underwriting != 0 || actionable != before {
        bail!("Dusk over-underwriting pre-state is not represented fail-closed");
    }
    let global_health_satisfied = observation
        .current_global_health_floor_satisfied
        .context("Dusk borrow observation has no current-global-health assertion")?;
    if !global_health_satisfied && global_health != 0 {
        bail!("Dusk unhealthy global pre-state exposes nonzero global-health capacity");
    }

    if parse_token(&observation.theoretical_max_debt)? != gross {
        bail!("Dusk theoretical capacity alias is not the gross admissible maximum");
    }
    let target = parse_token(&observation.target_debt)?;
    let after = parse_token(&observation.debt_after)?;
    let raw_request = required(&observation.raw_borrow_request, "raw borrow request")?;
    if raw_request > actionable_additional {
        bail!("Dusk raw borrow request exceeds actionable native capacity");
    }
    let requested_change = parse_token(&observation.requested_change)?;
    let filled_change = parse_token(&observation.filled_change)?;
    let unmet_change = parse_token(&observation.unmet_change)?;
    let target_gap = required(&observation.target_debt_gap, "target debt gap")?;
    if requested_change != target.saturating_sub(before)
        || filled_change != after.saturating_sub(before)
        || unmet_change != target.saturating_sub(after)
        || target_gap != unmet_change
    {
        bail!("Dusk borrow demand fields mix raw request and post-debt atom semantics");
    }
    Ok(())
}

fn build_repay_metric(
    factual: &V1ReplayLedgerRowV1,
    dusk: &CounterfactualOperationRecordV1,
) -> Result<Option<PairedRepayMetricV1>> {
    let Some(V1BehaviorObservationV1::Repay {
        debt_before,
        debt_after,
        reduction_fraction_numerator,
        reduction_fraction_denominator,
    }) = factual.behavior.as_ref()
    else {
        return Ok(None);
    };
    let observation = dusk
        .debt
        .as_ref()
        .context("factual repay has no Dusk debt observation")?;
    Ok(Some(PairedRepayMetricV1 {
        v1_debt_before: debt_before.clone(),
        v1_debt_after: debt_after.clone(),
        preserved_reduction_fraction_numerator: reduction_fraction_numerator.clone(),
        preserved_reduction_fraction_denominator: reduction_fraction_denominator.clone(),
        dusk_debt_before: observation.debt_before.clone(),
        dusk_debt_after: observation.debt_after.clone(),
        dusk_filled_reduction: observation.filled_change.clone(),
        dusk_unmet_reduction: observation.unmet_change.clone(),
    }))
}

fn build_safety_metric(
    operation: &CanonicalOperationV1,
    dusk: &CounterfactualOperationRecordV1,
) -> Result<Option<SafetyMetricV1>> {
    let Some(observation) = dusk.liquidation.as_ref() else {
        if matches!(operation.payload, OperationPayloadV1::Liquidate { .. }) {
            bail!("liquidation operation has no typed Dusk liquidation observation");
        }
        return Ok(None);
    };
    let shortfall = if dusk.synthetic_protocol_settlement {
        token(0)
    } else {
        let OperationPayloadV1::Liquidate { debt_token0, .. } = &operation.payload else {
            bail!("non-synthetic Dusk liquidation is not paired to a canonical liquidation");
        };
        if *debt_token0 != observation.debt_asset_base {
            bail!("Dusk liquidation debt side differs from canonical V1 demand");
        }
        let mut shortfalls = operation.assertions.iter().filter_map(|assertion| {
            if let EventPayloadV1::Liquidation(payload) = &assertion.payload {
                Some(payload.shortfall.clone())
            } else {
                None
            }
        });
        let shortfall = shortfalls
            .next()
            .context("liquidation operation has no exact shortfall assertion")?;
        if shortfalls.next().is_some() {
            bail!("liquidation operation has multiple shortfall assertions");
        }
        shortfall
    };
    Ok(Some(SafetyMetricV1 {
        source_operation_id: observation.source_operation_id.clone(),
        debt_asset_base: observation.debt_asset_base,
        v1_liquidation_shortfall: shortfall,
        dusk_liquidation_shortfall: Some(observation.socialized_loss.clone()),
        dusk_exact_liquidation_available: dusk_liquidation_exactness_available(
            observation.phase,
            observation.auction_triggered,
            observation.transition_executed,
            dusk.synthetic_protocol_settlement,
        ),
        dusk_settlement_final: observation.settlement_final,
        dusk_residual_auction_open: observation.residual_auction_open,
        synthetic_protocol_settlement: dusk.synthetic_protocol_settlement,
    }))
}

fn dusk_liquidation_exactness_available(
    phase: crate::counterfactual::DuskLiquidationPhaseV1,
    auction_triggered: bool,
    transition_executed: bool,
    synthetic_protocol_settlement: bool,
) -> bool {
    if synthetic_protocol_settlement {
        // A synthetic floor row is exact only when the floor transition committed.
        transition_executed
    } else {
        // Starting an auction is itself an exact native transition, even when the
        // single policy bid cannot execute. Terminal incompleteness is tracked by
        // `residual_auction_open` and the evaluation cutoff gate, not as missing
        // liquidation replay data.
        transition_executed
            || auction_triggered
            || phase == crate::counterfactual::DuskLiquidationPhaseV1::NotLiquidatable
    }
}

fn build_revenue_delta(
    v1: &V1RevenueDeltaV1,
    dusk: &CounterfactualOperationRecordV1,
    prior: Option<&MarketMetricState>,
) -> Result<PairedRevenueDeltaV1> {
    let (v1_base, v1_quote) = if dusk.synthetic_arbitrage || dusk.synthetic_protocol_settlement {
        (RevenueSideDeltaV1::zero(), RevenueSideDeltaV1::zero())
    } else {
        v1_revenue(v1)
    };
    let mut dusk_base = RevenueSideDeltaV1::zero();
    let mut dusk_quote = RevenueSideDeltaV1::zero();
    if let Some(swap) = &dusk.swap {
        if dusk.status == CounterfactualStatusV1::Executed {
            let side = if swap.asset_in_base {
                &mut dusk_base
            } else {
                &mut dusk_quote
            };
            let base_fee = parse_token(&swap.base_fee)?;
            let protocol = proportional_bps(base_fee, dusk.protocol_swap_fee_bps)?;
            let buyback = proportional_bps(protocol, dusk.protocol_buyback_auction_bps)?;
            let fee_auction = protocol
                .checked_sub(buyback)
                .context("protocol swap split underflow")?;
            let distributed_dynamic = parse_token(&swap.distributed_dynamic_fee)?;
            let lp = base_fee
                .checked_sub(protocol)
                .and_then(|value| value.checked_add(distributed_dynamic))
                .context("LP swap revenue overflow")?;
            side.lp_swap_earned = token(lp);
            side.protocol_fee_auction_swap_earned = token(fee_auction);
            side.protocol_buyback_swap_earned = token(buyback);
            side.retained_recenter_inventory = swap.retained_recenter_inventory.clone();
            side.compounded_lp_swap_principal = token(
                parse_token(&swap.compounded_base_lp_fee)?
                    .checked_add(parse_token(&swap.compounded_dynamic_lp_fee)?)
                    .context("compounded swap revenue overflow")?,
            );
        }
    }

    assert_zero_referral_interest_delta(prior.map(|state| &state.dusk_revenue), &dusk.revenue)?;
    let hlp_base_interest_credit = dusk
        .hlp
        .as_ref()
        .map(|hlp| parse_token(&hlp.quote_hlp_interest_credit_base))
        .transpose()?
        .unwrap_or(0);
    let hlp_quote_interest_credit = dusk
        .hlp
        .as_ref()
        .map(|hlp| parse_token(&hlp.base_hlp_interest_credit_quote))
        .transpose()?
        .unwrap_or(0);
    let public_base_interest_credit = parse_token(&dusk.cash.base_interest_credit)?
        .checked_sub(hlp_base_interest_credit)
        .context("base hLP interest exceeds measured interest-vault credit")?;
    let public_quote_interest_credit = parse_token(&dusk.cash.quote_interest_credit)?
        .checked_sub(hlp_quote_interest_credit)
        .context("quote hLP interest exceeds measured interest-vault credit")?;
    dusk_interest_delta(
        &dusk.public_interest.base,
        public_base_interest_credit,
        dusk,
        &mut dusk_base,
    )?;
    dusk_interest_delta(
        &dusk.public_interest.quote,
        public_quote_interest_credit,
        dusk,
        &mut dusk_quote,
    )?;
    apply_hlp_funding_revenue(hlp_base_interest_credit, dusk, &mut dusk_base)?;
    apply_hlp_funding_revenue(hlp_quote_interest_credit, dusk, &mut dusk_quote)?;
    Ok(PairedRevenueDeltaV1 {
        v1_base,
        v1_quote,
        dusk_base,
        dusk_quote,
        dusk_revenue_origin: revenue_origin_from_flags(
            dusk.synthetic_arbitrage,
            dusk.synthetic_protocol_settlement,
        )?,
    })
}

fn apply_hlp_funding_revenue(
    interest_credit: u128,
    record: &CounterfactualOperationRecordV1,
    output: &mut RevenueSideDeltaV1,
) -> Result<()> {
    let protocol = proportional_bps(interest_credit, record.protocol_interest_fee_bps)?;
    let buyback = proportional_bps(protocol, record.protocol_buyback_auction_bps)?;
    let fee_auction = protocol
        .checked_sub(buyback)
        .context("hLP protocol interest split underflow")?;
    output.hlp_funding_interest_earned = token(
        interest_credit
            .checked_sub(protocol)
            .context("hLP LP interest split underflow")?,
    );
    output.protocol_buyback_hlp_funding_interest_earned = token(
        parse_token(&output.protocol_buyback_hlp_funding_interest_earned)?
            .checked_add(buyback)
            .context("hLP buyback interest overflow")?,
    );
    output.protocol_fee_auction_hlp_funding_interest_earned = token(
        parse_token(&output.protocol_fee_auction_hlp_funding_interest_earned)?
            .checked_add(fee_auction)
            .context("hLP fee-auction interest overflow")?,
    );
    Ok(())
}

fn dusk_interest_delta(
    transition: &DuskPublicInterestSideTransitionV1,
    observed_net_interest_credit: u128,
    record: &CounterfactualOperationRecordV1,
    output: &mut RevenueSideDeltaV1,
) -> Result<()> {
    validate_public_interest_transition(transition)?;
    let accrued = parse_token(&transition.clock_accrued)?;
    let cash_interest_credit = parse_token(&transition.net_interest_vault_credit)?;
    if cash_interest_credit != observed_net_interest_credit {
        bail!("Dusk public-interest witness disagrees with net interest-vault cash flow");
    }
    let accrued_protocol = proportional_bps(accrued, record.protocol_interest_fee_bps)?;
    output.protocol_public_interest_accrued = token(accrued_protocol);
    output.lp_public_interest_accrued = token(
        accrued
            .checked_sub(accrued_protocol)
            .context("Dusk accrued interest split underflow")?,
    );

    let cash_protocol = proportional_bps(cash_interest_credit, record.protocol_interest_fee_bps)?;
    let cash_buyback = proportional_bps(cash_protocol, record.protocol_buyback_auction_bps)?;
    output.protocol_buyback_public_interest_cash_realized = token(cash_buyback);
    output.protocol_fee_auction_public_interest_cash_realized = token(
        cash_protocol
            .checked_sub(cash_buyback)
            .context("Dusk cash interest protocol split underflow")?,
    );
    output.lp_public_interest_cash_realized = token(
        cash_interest_credit
            .checked_sub(cash_protocol)
            .context("Dusk cash interest LP split underflow")?,
    );
    Ok(())
}

fn validate_public_interest_transition(
    transition: &DuskPublicInterestSideTransitionV1,
) -> Result<()> {
    let before = parse_token(&transition.outstanding_before_operation)?;
    let after_clock = parse_token(&transition.outstanding_after_clock_before_transition)?;
    let after_transition = parse_token(&transition.outstanding_after_transition)?;
    let clock_accrued = parse_token(&transition.clock_accrued)?;
    let transition_created = parse_token(&transition.transition_interest_created)?;
    let gross_cash_paid = parse_token(&transition.gross_cash_interest_paid)?;
    let net_credit = parse_token(&transition.net_interest_vault_credit)?;
    let removed = parse_token(&transition.total_interest_removed)?;
    let written_off = parse_token(&transition.interest_written_off)?;
    if before.checked_add(clock_accrued) != Some(after_clock)
        || after_clock.checked_add(transition_created) != after_transition.checked_add(removed)
        || gross_cash_paid.checked_add(written_off) != Some(removed)
        || net_credit > gross_cash_paid
    {
        bail!("Dusk public-interest transition witness violates its exact identities");
    }
    Ok(())
}

fn assert_zero_referral_interest_delta(
    before: Option<&DuskRevenueCheckpointV1>,
    after: &DuskRevenueCheckpointV1,
) -> Result<()> {
    for (prior, current) in [
        (
            before.map(|checkpoint| &checkpoint.base.referral_interest_liability),
            &after.base.referral_interest_liability,
        ),
        (
            before.map(|checkpoint| &checkpoint.quote.referral_interest_liability),
            &after.quote.referral_interest_liability,
        ),
    ] {
        let current = parse_token(current)?;
        if prior.map(parse_token).transpose()?.unwrap_or(0) != current {
            bail!("Dusk referral-interest liability changed in a zero-referral replay");
        }
    }
    Ok(())
}

fn v1_revenue(delta: &V1RevenueDeltaV1) -> (RevenueSideDeltaV1, RevenueSideDeltaV1) {
    let side = |lp_swap,
                protocol_swap,
                lp_interest,
                protocol_interest,
                cash_covered_protocol_interest,
                flashloan,
                claim| {
        RevenueSideDeltaV1 {
            lp_swap_earned: token(lp_swap),
            protocol_fee_auction_swap_earned: token(protocol_swap),
            protocol_buyback_swap_earned: token(0),
            retained_recenter_inventory: token(0),
            compounded_lp_swap_principal: token(0),
            lp_public_interest_accrued: token(lp_interest),
            protocol_public_interest_accrued: token(protocol_interest),
            // V1 has no exact LP cash-realization witness. The native engine does,
            // however, expose the protocol share actually removed from cash reserve;
            // V1 has no buyback split, so that exact cash memo belongs wholly to the
            // fee-auction lane.
            lp_public_interest_cash_realized: token(0),
            protocol_fee_auction_public_interest_cash_realized: token(
                cash_covered_protocol_interest,
            ),
            protocol_buyback_public_interest_cash_realized: token(0),
            hlp_funding_interest_earned: token(0),
            protocol_fee_auction_hlp_funding_interest_earned: token(0),
            protocol_buyback_hlp_funding_interest_earned: token(0),
            flashloan_fee_v1_only: token(flashloan),
            protocol_claim_realized_v1_only: token(claim),
        }
    };
    (
        side(
            delta.lp_swap_fee0,
            delta.protocol_swap_fee0,
            delta.lp_interest0,
            delta.protocol_interest0,
            delta.cash_covered_protocol_interest0,
            delta.flashloan_fee0,
            delta.protocol_claim_realized0,
        ),
        side(
            delta.lp_swap_fee1,
            delta.protocol_swap_fee1,
            delta.lp_interest1,
            delta.protocol_interest1,
            delta.cash_covered_protocol_interest1,
            delta.flashloan_fee1,
            delta.protocol_claim_realized1,
        ),
    )
}

fn capital_checkpoint(
    v1: &V1PairCheckpointV1,
    dusk: &DuskMarketCheckpointV1,
) -> Result<CapitalCheckpointV1> {
    Ok(CapitalCheckpointV1 {
        v1_reserve0: token(u128::from(v1.reserve0)),
        v1_reserve1: token(u128::from(v1.reserve1)),
        v1_cash_reserve0: token(u128::from(v1.cash_reserve0)),
        v1_cash_reserve1: token(u128::from(v1.cash_reserve1)),
        v1_total_debt0: token(u128::from(v1.total_debt0)),
        v1_total_debt1: token(u128::from(v1.total_debt1)),
        v1_total_collateral0: token(u128::from(v1.total_collateral0)),
        v1_total_collateral1: token(u128::from(v1.total_collateral1)),
        v1_lp_supply: token(u128::from(v1.total_supply)),
        dusk_base_live_reserve: dusk.base.live_reserve.clone(),
        dusk_quote_live_reserve: dusk.quote.live_reserve.clone(),
        dusk_base_cash_reserve: dusk.base.cash_reserve.clone(),
        dusk_quote_cash_reserve: dusk.quote.cash_reserve.clone(),
        dusk_base_total_debt: token(
            parse_token(&dusk.base.fixed_debt)?
                .checked_add(parse_token(&dusk.base.isolated_debt)?)
                .context("Dusk base debt overflow")?,
        ),
        dusk_quote_total_debt: token(
            parse_token(&dusk.quote.fixed_debt)?
                .checked_add(parse_token(&dusk.quote.isolated_debt)?)
                .context("Dusk quote debt overflow")?,
        ),
        dusk_ylp_supply: dusk.base.ylp_supply.clone(),
    })
}

fn cpmm_slippage_ppb(
    token0_in: bool,
    input: u128,
    output: u128,
    reserve0: u64,
    reserve1: u64,
) -> Result<i128> {
    let (reserve_in, reserve_out) = if token0_in {
        (u128::from(reserve0), u128::from(reserve1))
    } else {
        (u128::from(reserve1), u128::from(reserve0))
    };
    let ideal_numerator = input
        .checked_mul(reserve_out)
        .context("V1 ideal output overflow")?;
    let realized_numerator = output
        .checked_mul(reserve_in)
        .context("V1 realized output overflow")?;
    signed_ratio_ppb(ideal_numerator, realized_numerator, ideal_numerator)
}

fn fee_ceil_u128(amount: u128, bps: u16) -> Result<u128> {
    if bps > BPS_DENOMINATOR {
        bail!("swap fee exceeds 10,000 bps");
    }
    let denominator = u128::from(BPS_DENOMINATOR);
    amount
        .checked_mul(u128::from(bps))
        .and_then(|value| value.checked_add(denominator - 1))
        .map(|value| value / denominator)
        .context("swap fee arithmetic overflow")
}

fn concentrated_slippage_ppb(
    base_in: bool,
    input: u128,
    output: u128,
    price_nad: u128,
    base_decimals: u8,
    quote_decimals: u8,
) -> Result<i128> {
    if input == 0 || price_nad == 0 {
        bail!("Dusk slippage requires nonzero input and start price");
    }
    let (ideal_numerator, ideal_denominator) = if base_in {
        if quote_decimals >= base_decimals {
            let scale = pow10(quote_decimals - base_decimals)?;
            (
                input
                    .checked_mul(price_nad)
                    .and_then(|value| value.checked_mul(scale))
                    .context("Dusk base-in ideal output overflow")?,
                u128::from(NAD),
            )
        } else {
            let scale = pow10(base_decimals - quote_decimals)?;
            (
                input
                    .checked_mul(price_nad)
                    .context("Dusk base-in ideal output overflow")?,
                u128::from(NAD)
                    .checked_mul(scale)
                    .context("Dusk base-in ideal denominator overflow")?,
            )
        }
    } else if base_decimals >= quote_decimals {
        let scale = pow10(base_decimals - quote_decimals)?;
        (
            input
                .checked_mul(u128::from(NAD))
                .and_then(|value| value.checked_mul(scale))
                .context("Dusk quote-in ideal output overflow")?,
            price_nad,
        )
    } else {
        let scale = pow10(quote_decimals - base_decimals)?;
        (
            input
                .checked_mul(u128::from(NAD))
                .context("Dusk quote-in ideal output overflow")?,
            price_nad
                .checked_mul(scale)
                .context("Dusk quote-in ideal denominator overflow")?,
        )
    };
    let realized_numerator = output
        .checked_mul(ideal_denominator)
        .context("Dusk realized output overflow")?;
    signed_ratio_ppb(ideal_numerator, realized_numerator, ideal_numerator)
}

fn output_cost_delta_ppb(v1_output: u128, dusk_output: u128) -> Result<i128> {
    if v1_output == 0 {
        bail!("execution cost comparison has zero V1 output");
    }
    signed_ratio_ppb(v1_output, dusk_output, v1_output)
}

/// Computes `(left - right) / denominator` in ppb, retaining the sign.
fn signed_ratio_ppb(left: u128, right: u128, denominator: u128) -> Result<i128> {
    if denominator == 0 {
        bail!("scaled ratio denominator is zero");
    }
    let left = i128::try_from(left).context("scaled ratio left exceeds i128")?;
    let right = i128::try_from(right).context("scaled ratio right exceeds i128")?;
    let denominator =
        i128::try_from(denominator).context("scaled ratio denominator exceeds i128")?;
    left.checked_sub(right)
        .and_then(|value| value.checked_mul(PPB))
        .and_then(|value| value.checked_div(denominator))
        .context("scaled ratio overflow")
}

fn proportional_bps(value: u128, bps: u16) -> Result<u128> {
    if bps > BPS_DENOMINATOR {
        bail!("protocol allocation exceeds 10,000 bps");
    }
    value
        .checked_mul(u128::from(bps))
        .and_then(|product| product.checked_div(u128::from(BPS_DENOMINATOR)))
        .context("basis-point allocation overflow")
}

fn pow10(exponent: u8) -> Result<u128> {
    10_u128
        .checked_pow(u32::from(exponent))
        .context("token decimal scale exceeds u128")
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
        .context("signed metric exceeds i128")
}

fn token(value: u128) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u128 is canonical")
}

fn signed(value: i128) -> Result<SignedTokenAmount> {
    SignedTokenAmount::new(value.to_string()).context("signed i128 is not canonical")
}

fn checked_add_u64(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right).context("metric row count overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counterfactual::DuskRevenueSideCheckpointV1;

    fn zero_revenue_side() -> DuskRevenueSideCheckpointV1 {
        DuskRevenueSideCheckpointV1 {
            swap_fee_growth_index_q64: token(0),
            interest_growth_index_q64: token(0),
            lp_swap_fee_liability: token(0),
            lp_interest_liability: token(0),
            unallocated_lp_swap_fee_liability: token(0),
            unallocated_lp_interest_liability: token(0),
            protocol_swap_fee_liability: token(0),
            protocol_interest_fee_liability: token(0),
            buyback_swap_fee_liability: token(0),
            buyback_interest_fee_liability: token(0),
            referral_interest_liability: token(0),
            swap_fee_custody_balance: token(0),
            interest_vault_balance: token(0),
        }
    }

    #[test]
    fn cpmm_and_concentrated_slippage_share_the_same_sign_convention() {
        let cpmm = cpmm_slippage_ppb(true, 100, 90, 1_000, 1_000).unwrap();
        let concentrated = concentrated_slippage_ppb(true, 100, 90, u128::from(NAD), 6, 6).unwrap();
        assert_eq!(cpmm, 100_000_000);
        assert_eq!(concentrated, cpmm);
    }

    #[test]
    fn decimal_normalization_is_directionally_symmetric() {
        let base_in =
            concentrated_slippage_ppb(true, 1_000_000, 2_000_000_000, 2_000_000_000, 6, 9).unwrap();
        let quote_in =
            concentrated_slippage_ppb(false, 2_000_000_000, 1_000_000, 2_000_000_000, 6, 9)
                .unwrap();
        assert_eq!(base_in, 0);
        assert_eq!(quote_in, 0);
    }

    #[test]
    fn protocol_split_excludes_retained_recenter_inventory() {
        let base_fee = 1_000_u128;
        let protocol = proportional_bps(base_fee, 2_000).unwrap();
        let lp = base_fee - protocol + 77;
        assert_eq!(protocol, 200);
        assert_eq!(lp, 877);
        // A retained surcharge is deliberately not part of either amount.
        let retained = 23_u128;
        assert_eq!(lp + protocol + retained, 1_100);
    }

    #[test]
    fn v1_protocol_cash_uses_only_the_native_cash_covered_witness() {
        let delta = V1RevenueDeltaV1 {
            lp_interest0: 11,
            protocol_interest0: 7,
            nominal_protocol_interest0: 9,
            cash_covered_protocol_interest0: 5,
            ..V1RevenueDeltaV1::default()
        };
        let (base, quote) = v1_revenue(&delta);
        assert_eq!(base.lp_public_interest_accrued.as_str(), "11");
        assert_eq!(base.protocol_public_interest_accrued.as_str(), "7");
        assert_eq!(base.lp_public_interest_cash_realized.as_str(), "0");
        assert_eq!(
            base.protocol_fee_auction_public_interest_cash_realized
                .as_str(),
            "5"
        );
        assert_eq!(
            base.protocol_buyback_public_interest_cash_realized.as_str(),
            "0"
        );
        assert_eq!(quote, RevenueSideDeltaV1::zero());
    }

    #[test]
    fn public_interest_witness_proves_accrual_payment_and_writeoff_partition() {
        let valid = DuskPublicInterestSideTransitionV1 {
            outstanding_before_operation: token(10),
            outstanding_after_clock_before_transition: token(14),
            outstanding_after_transition: token(9),
            clock_accrued: token(4),
            transition_interest_created: token(0),
            gross_cash_interest_paid: token(3),
            net_interest_vault_credit: token(2),
            total_interest_removed: token(5),
            interest_written_off: token(2),
        };
        validate_public_interest_transition(&valid).unwrap();

        let mut invalid = valid;
        invalid.interest_written_off = token(1);
        assert!(validate_public_interest_transition(&invalid).is_err());
    }

    #[test]
    fn referral_liability_must_stay_zero_for_stakeholder_classification() {
        let prior = DuskRevenueCheckpointV1 {
            base: zero_revenue_side(),
            quote: zero_revenue_side(),
        };
        assert_zero_referral_interest_delta(None, &prior).unwrap();
        let mut after = prior.clone();
        after.base.referral_interest_liability = token(1);
        assert!(assert_zero_referral_interest_delta(Some(&prior), &after).is_err());
    }

    #[test]
    fn exact_pending_auction_is_distinct_from_uncommitted_synthetic_floor() {
        use crate::counterfactual::DuskLiquidationPhaseV1::{FloorSettlement, ImmediateBid};

        assert!(dusk_liquidation_exactness_available(
            ImmediateBid,
            true,
            false,
            false
        ));
        assert!(!dusk_liquidation_exactness_available(
            FloorSettlement,
            true,
            false,
            true
        ));
        assert!(dusk_liquidation_exactness_available(
            FloorSettlement,
            true,
            true,
            true
        ));
    }

    #[test]
    fn borrow_capacity_validation_keeps_raw_requests_separate_from_debt_atoms() {
        let observation = DuskDebtObservationV1 {
            debt_asset_base: true,
            debt_before: token(100),
            gross_admissible_max_debt: Some(token(200)),
            actionable_max_debt: Some(token(150)),
            theoretical_max_debt: token(200),
            underwriting_max_additional: Some(token(100)),
            global_health_floor_max_additional: Some(token(50)),
            cash_max_additional: Some(token(75)),
            daily_bucket_max_additional: Some(token(80)),
            reduce_only_max_additional: None,
            actionable_max_additional: Some(token(50)),
            current_underwriting_satisfied: Some(true),
            current_global_health_floor_satisfied: Some(true),
            limiting_constraints: vec![DuskBorrowCapacityConstraintV1::GlobalHealthFloor],
            target_debt: token(102),
            debt_after: token(102),
            raw_borrow_request: Some(token(1)),
            requested_change: token(2),
            filled_change: token(2),
            unmet_change: token(0),
            target_debt_gap: Some(token(0)),
            interest_paid_before_transfer_fee: token(0),
        };
        validate_dusk_borrow_capacity_observation(&observation).unwrap();

        let mut mixed_units = observation.clone();
        mixed_units.requested_change = token(1);
        assert!(validate_dusk_borrow_capacity_observation(&mixed_units).is_err());
    }

    #[test]
    fn borrow_capacity_validation_rejects_incomplete_tied_constraints() {
        let observation = DuskDebtObservationV1 {
            debt_asset_base: true,
            debt_before: token(0),
            gross_admissible_max_debt: Some(token(100)),
            actionable_max_debt: Some(token(0)),
            theoretical_max_debt: token(100),
            underwriting_max_additional: Some(token(100)),
            global_health_floor_max_additional: Some(token(0)),
            cash_max_additional: Some(token(0)),
            daily_bucket_max_additional: Some(token(50)),
            reduce_only_max_additional: None,
            actionable_max_additional: Some(token(0)),
            current_underwriting_satisfied: Some(true),
            current_global_health_floor_satisfied: Some(false),
            limiting_constraints: vec![DuskBorrowCapacityConstraintV1::GlobalHealthFloor],
            target_debt: token(50),
            debt_after: token(0),
            raw_borrow_request: Some(token(0)),
            requested_change: token(50),
            filled_change: token(0),
            unmet_change: token(50),
            target_debt_gap: Some(token(50)),
            interest_paid_before_transfer_fee: token(0),
        };
        assert!(validate_dusk_borrow_capacity_observation(&observation).is_err());
    }

    #[test]
    fn borrow_capacity_validation_rejects_cross_asset_and_invalid_v1_headroom() {
        let before = V1BorrowCapacityV1 {
            position: Default::default(),
            debt_token: V1TokenSideV1::Token0,
            collateral_amount: 1_000,
            actual_debt: 10,
            admissible_max_debt: 100,
            remaining_capacity: 90,
            cash_reserve: 50,
            actionable_remaining_capacity: 50,
            max_cf_bps: 8_000,
            liquidation_cf_bps: 9_000,
        };
        let after = V1BorrowCapacityV1 {
            actual_debt: 20,
            remaining_capacity: 80,
            ..before
        };
        let mut dusk = DuskDebtObservationV1 {
            debt_asset_base: false,
            debt_before: token(10),
            gross_admissible_max_debt: Some(token(100)),
            actionable_max_debt: Some(token(60)),
            theoretical_max_debt: token(100),
            underwriting_max_additional: Some(token(90)),
            global_health_floor_max_additional: Some(token(80)),
            cash_max_additional: Some(token(50)),
            daily_bucket_max_additional: Some(token(70)),
            reduce_only_max_additional: None,
            actionable_max_additional: Some(token(50)),
            current_underwriting_satisfied: Some(true),
            current_global_health_floor_satisfied: Some(true),
            limiting_constraints: vec![DuskBorrowCapacityConstraintV1::Cash],
            target_debt: token(20),
            debt_after: token(20),
            raw_borrow_request: Some(token(10)),
            requested_change: token(10),
            filled_change: token(10),
            unmet_change: token(0),
            target_debt_gap: Some(token(0)),
            interest_paid_before_transfer_fee: token(0),
        };
        assert!(validate_v1_borrow_capacity_pair(&before, &after, &dusk).is_err());

        dusk.debt_asset_base = true;
        validate_v1_borrow_capacity_pair(&before, &after, &dusk).unwrap();

        let invalid = V1BorrowCapacityV1 {
            remaining_capacity: 91,
            ..before
        };
        assert!(validate_v1_borrow_capacity_pair(&invalid, &after, &dusk).is_err());
    }

    #[test]
    fn synthetic_records_join_an_anchor_to_the_latest_same_market_checkpoint() {
        assert!(synthetic_record_uses_checkpoint_anchor(true, false));
        assert!(synthetic_record_uses_checkpoint_anchor(false, true));
        assert!(!synthetic_record_uses_checkpoint_anchor(false, false));
    }
}

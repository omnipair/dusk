//! Dusk counterfactual replay over canonical V1 operations.
//!
//! Every accepted transition calls Dusk's analysis-only native wrappers. A
//! rejected intent leaves state unchanged. Token-2022 outcomes are calculated
//! from an exact historical mint schedule; observed V1 credits are assertions,
//! never extrapolated fee oracles.

use std::{collections::BTreeMap, str::FromStr};

use anyhow::{bail, Context, Result};
use dusk::{
    benchmark_api::{
        BenchmarkAddYlpRequest, BenchmarkAssetCashFlow, BenchmarkBorrowCapacityConstraint,
        BenchmarkBorrowPosition, BenchmarkBorrowRequest, BenchmarkCashFlow, BenchmarkClock,
        BenchmarkDepositCollateralRequest, BenchmarkExistingBorrowCapacity,
        BenchmarkHlpAwareSwapRequest, BenchmarkHlpOwnedState, BenchmarkHlpWithdrawRequest,
        BenchmarkLeveragePolicy, BenchmarkLeverageTokenBalances,
        BenchmarkLiquidationExecuteRequest, BenchmarkLiquidationPhase,
        BenchmarkLiquidationPlanRequest, BenchmarkLiquidationPreviewRequest, BenchmarkMarket,
        BenchmarkMarketInit, BenchmarkPublicInterestCheckpoint, BenchmarkPublicInterestPayment,
        BenchmarkPublicInterestPayments, BenchmarkPublicInterestTransition,
        BenchmarkRemoveYlpRequest, BenchmarkRepayRequest, BenchmarkRevenueCheckpoint,
        BenchmarkStartLiquidationAuctionRequest, BenchmarkSwapRequest,
        BenchmarkTokenTransferOutcome, BenchmarkWithdrawCollateralRequest,
    },
    constants::{BPS_DENOMINATOR, LIQUIDATION_BACKSTOP_CALLER_BPS, MIN_HALF_LIFE_MS},
    errors::ErrorCode as DuskErrorCode,
    state::{
        AmmConfig, IrmConfig, MarketAsset, MarketConfig, MarketSide, ProtocolAuctionSplit,
        DEFAULT_DAILY_BORROW_BPS, SWAP_FEE_COLLECT_INPUT_ASSET,
    },
};
use omnipair::constants::MIN_LIQUIDITY;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;

use crate::{
    calibrate::DuskProfileV1,
    canonical::{SignedTokenAmount, TokenAmount},
    factual_replay::V1BehaviorObservationV1,
    replay::{CanonicalOperationV1, HlpScenario, OperationPayloadV1, ReplayArm},
    spot_probe::{
        derive_quote_metrics, quote_improvement_ppb, unsupported_quote, SpotProbeObservationV1,
        SpotProbePlanV1, SpotProbeQuoteV1, SpotProbeSetV1, SpotProbeUnsupportedReasonV1,
        SPOT_PROBE_SCHEMA_VERSION,
    },
    transfer_fee::ValidatedTransferFeeRegistryV1,
};

pub const COUNTERFACTUAL_SCHEMA_VERSION: u16 = 4;

const DEBT_SHARE_DIVISION_ERROR_CODE: u32 =
    anchor_lang::error::ERROR_CODE_OFFSET + DuskErrorCode::DebtShareDivisionOverflow as u32;
const DEBT_REPAYMENT_TOO_SMALL_SOURCE_SUFFIX: &str = "programs/dusk/src/state/market.rs";
const DEBT_REPAYMENT_TOO_SMALL_SOURCE_LINE: u32 = 1309;

/// Matches only the native repayment branch where a positive raw-token cap is
/// too small to burn even one debt share. This is a normal counterfactual
/// rejection, not permission to swallow other debt math or invariant errors.
fn is_unrepresentable_debt_share_repayment(error: &anchor_lang::error::Error) -> bool {
    let anchor_lang::error::Error::AnchorError(anchor_error) = error else {
        return false;
    };
    let Some(anchor_lang::error::ErrorOrigin::Source(source)) = &anchor_error.error_origin else {
        return false;
    };
    anchor_error.error_code_number == DEBT_SHARE_DIVISION_ERROR_CODE
        && anchor_error.error_name == "DebtShareDivisionOverflow"
        && source
            .filename
            .ends_with(DEBT_REPAYMENT_TOO_SMALL_SOURCE_SUFFIX)
        && source.line == DEBT_REPAYMENT_TOO_SMALL_SOURCE_LINE
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityFractionV1 {
    pub numerator: u64,
    pub denominator: u64,
}

impl CapacityFractionV1 {
    pub fn validate(self) -> Result<Self> {
        if self.denominator == 0 || self.numerator > self.denominator {
            bail!("capacity/debt-reduction fraction must be within [0, 1]");
        }
        Ok(self)
    }

    fn from_factual_ratio(numerator: u64, denominator: u64) -> Result<Self> {
        let (numerator, denominator) = if denominator == 0 {
            (u64::from(numerator > 0), 1)
        } else {
            (numerator.min(denominator), denominator)
        };
        Self {
            numerator,
            denominator,
        }
        .validate()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BehavioralContextV1 {
    pub borrow_capacity_fraction: Option<CapacityFractionV1>,
    pub repayment_fraction: Option<CapacityFractionV1>,
}

/// One exact, schedule-resolved protocol-interest policy epoch. Evaluation
/// resolves this before mutating a market and never falls back to the factual
/// V1 surcharge bps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProtocolInterestPolicyV1 {
    pub schedule_sha256: String,
    pub epoch_id: String,
    pub factual_v1_interest_bps: u16,
    pub dusk_protocol_interest_share_bps: u16,
}

impl ResolvedProtocolInterestPolicyV1 {
    fn validate(&self) -> Result<()> {
        if self.schedule_sha256.len() != 64
            || !self
                .schedule_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.epoch_id.is_empty()
            || self.factual_v1_interest_bps > BPS_DENOMINATOR
            || self.dusk_protocol_interest_share_bps > BPS_DENOMINATOR
        {
            bail!("invalid resolved protocol-interest policy epoch");
        }
        Ok(())
    }
}

impl BehavioralContextV1 {
    pub fn from_factual(observation: Option<&V1BehaviorObservationV1>) -> Result<Self> {
        let mut context = Self::default();
        match observation {
            Some(V1BehaviorObservationV1::Borrow {
                capacity_fraction_numerator,
                capacity_fraction_denominator,
                ..
            }) => {
                context.borrow_capacity_fraction = Some(CapacityFractionV1::from_factual_ratio(
                    amount_u64(
                        "V1 capacity fraction numerator",
                        capacity_fraction_numerator.as_str(),
                    )?,
                    amount_u64(
                        "V1 capacity fraction denominator",
                        capacity_fraction_denominator.as_str(),
                    )?,
                )?);
            }
            Some(V1BehaviorObservationV1::Repay {
                reduction_fraction_numerator,
                reduction_fraction_denominator,
                ..
            }) => {
                context.repayment_fraction = Some(CapacityFractionV1::from_factual_ratio(
                    amount_u64(
                        "V1 repayment fraction numerator",
                        reduction_fraction_numerator.as_str(),
                    )?,
                    amount_u64(
                        "V1 repayment fraction denominator",
                        reduction_fraction_denominator.as_str(),
                    )?,
                )?);
            }
            None => {}
        }
        Ok(context)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterfactualStatusV1 {
    Executed,
    PartiallyFilled,
    Rejected,
    SkippedIntentMinOut,
    NoEconomicCounterpart,
    DeferredExternalState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterfactualConstraintV1 {
    HistoricalMinOut,
    NativeLiquidityOrHealth,
    Cash,
    DailyBorrowBucket,
    GlobalHealth,
    PositionHealth,
    DebtShareRounding,
    ReduceOnly,
    TransferFeeSchedule,
    HlpExternalState,
    LpOwnershipProvenance,
    LiquidationAuctionState,
    ArbitragePriceBand,
    V1OnlyFlashloan,
    V1ClaimRealization,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuskBorrowCapacityConstraintV1 {
    Underwriting,
    GlobalHealthFloor,
    Cash,
    DailyBorrowBucket,
    ReduceOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterfactualCashFlowV1 {
    pub base_reserve_credit: TokenAmount,
    pub base_reserve_debit: TokenAmount,
    pub base_collateral_credit: TokenAmount,
    pub base_collateral_debit: TokenAmount,
    pub base_interest_credit: TokenAmount,
    pub base_insurance_credit: TokenAmount,
    pub base_insurance_debit: TokenAmount,
    pub base_recipient_credit: TokenAmount,
    pub quote_reserve_credit: TokenAmount,
    pub quote_reserve_debit: TokenAmount,
    pub quote_collateral_credit: TokenAmount,
    pub quote_collateral_debit: TokenAmount,
    pub quote_interest_credit: TokenAmount,
    pub quote_insurance_credit: TokenAmount,
    pub quote_insurance_debit: TokenAmount,
    pub quote_recipient_credit: TokenAmount,
}

/// Exact decomposition of one side's public-borrow interest transition.
///
/// `interest_written_off` is shortfall evidence only. It is never an economic
/// or cash revenue lane. hLP funding is deliberately excluded from this
/// witness and remains in the separately owned hLP settlement ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskPublicInterestSideTransitionV1 {
    pub outstanding_before_operation: TokenAmount,
    pub outstanding_after_clock_before_transition: TokenAmount,
    pub outstanding_after_transition: TokenAmount,
    pub clock_accrued: TokenAmount,
    pub transition_interest_created: TokenAmount,
    pub gross_cash_interest_paid: TokenAmount,
    pub net_interest_vault_credit: TokenAmount,
    pub total_interest_removed: TokenAmount,
    pub interest_written_off: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskPublicInterestTransitionV1 {
    pub base: DuskPublicInterestSideTransitionV1,
    pub quote: DuskPublicInterestSideTransitionV1,
}

impl CounterfactualCashFlowV1 {
    fn from_native(cash: BenchmarkCashFlow) -> Self {
        Self {
            base_reserve_credit: token(cash.base.reserve_vault_credit),
            base_reserve_debit: token(cash.base.reserve_vault_debit),
            base_collateral_credit: token(cash.base.collateral_vault_credit),
            base_collateral_debit: token(cash.base.collateral_vault_debit),
            base_interest_credit: token(cash.base.interest_vault_credit),
            base_insurance_credit: token(cash.base.insurance_vault_credit),
            base_insurance_debit: token(cash.base.insurance_vault_debit),
            base_recipient_credit: token(cash.base.recipient_credit),
            quote_reserve_credit: token(cash.quote.reserve_vault_credit),
            quote_reserve_debit: token(cash.quote.reserve_vault_debit),
            quote_collateral_credit: token(cash.quote.collateral_vault_credit),
            quote_collateral_debit: token(cash.quote.collateral_vault_debit),
            quote_interest_credit: token(cash.quote.interest_vault_credit),
            quote_insurance_credit: token(cash.quote.insurance_vault_credit),
            quote_insurance_debit: token(cash.quote.insurance_vault_debit),
            quote_recipient_credit: token(cash.quote.recipient_credit),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskSwapObservationV1 {
    pub asset_in_base: bool,
    pub gross_amount_in: TokenAmount,
    pub input_transfer_fee: TokenAmount,
    pub reserve_credit_in: TokenAmount,
    pub curve_input: TokenAmount,
    pub curve_gross_output: TokenAmount,
    pub amount_out_before_token_fee: TokenAmount,
    pub output_transfer_fee: TokenAmount,
    pub recipient_credit_out: TokenAmount,
    pub base_fee: TokenAmount,
    pub divergence_fee: TokenAmount,
    pub volatility_fee: TokenAmount,
    pub dynamic_fee: TokenAmount,
    /// Dynamic surcharge distributed to yLPs. Retained recenter inventory is
    /// deliberately excluded until a later transition realizes it.
    pub distributed_dynamic_fee: TokenAmount,
    pub retained_recenter_inventory: TokenAmount,
    pub compounded_base_lp_fee: TokenAmount,
    pub compounded_dynamic_lp_fee: TokenAmount,
    pub total_fee: TokenAmount,
    pub start_price_nad: TokenAmount,
    pub end_price_nad: TokenAmount,
    pub reserve_end_price_nad: TokenAmount,
    pub original_min_out: TokenAmount,
    pub original_min_out_passed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskDebtObservationV1 {
    pub debt_asset_base: bool,
    pub debt_before: TokenAmount,
    /// Dynamic position-underwriting maximum before global-floor, cash,
    /// daily-bucket, and reduce-only execution limits.
    pub gross_admissible_max_debt: Option<TokenAmount>,
    /// Maximum total debt executable after every native bound.
    pub actionable_max_debt: Option<TokenAmount>,
    /// Backward-readable semantic alias for gross admissible capacity.
    pub theoretical_max_debt: TokenAmount,
    pub underwriting_max_additional: Option<TokenAmount>,
    pub global_health_floor_max_additional: Option<TokenAmount>,
    pub cash_max_additional: Option<TokenAmount>,
    pub daily_bucket_max_additional: Option<TokenAmount>,
    pub reduce_only_max_additional: Option<TokenAmount>,
    pub actionable_max_additional: Option<TokenAmount>,
    pub current_underwriting_satisfied: Option<bool>,
    pub current_global_health_floor_satisfied: Option<bool>,
    pub limiting_constraints: Vec<DuskBorrowCapacityConstraintV1>,
    pub target_debt: TokenAmount,
    pub debt_after: TokenAmount,
    /// Raw reserve-vault debit requested from native borrow execution. This is
    /// distinct from share-rounded debt change.
    pub raw_borrow_request: Option<TokenAmount>,
    pub requested_change: TokenAmount,
    pub filled_change: TokenAmount,
    pub unmet_change: TokenAmount,
    pub target_debt_gap: Option<TokenAmount>,
    /// Gross borrower interest retired by a repayment before a token-program
    /// transfer fee. Zero for borrow observations.
    pub interest_paid_before_transfer_fee: TokenAmount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuskLiquidationPhaseV1 {
    ImmediateBid,
    FloorSettlement,
    NotLiquidatable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskLiquidationObservationV1 {
    pub source_operation_id: String,
    pub position: String,
    pub debt_asset_base: bool,
    pub phase: DuskLiquidationPhaseV1,
    pub auction_triggered: bool,
    pub transition_executed: bool,
    pub settlement_final: bool,
    pub residual_auction_open: bool,
    pub first_floor_unix_timestamp: Option<i64>,
    pub requested_repay_gross: TokenAmount,
    pub repaid_amount: TokenAmount,
    pub interest_paid: TokenAmount,
    pub collateral_seized: TokenAmount,
    pub collateral_to_liquidator_gross: TokenAmount,
    pub collateral_to_liquidator_net: TokenAmount,
    pub insurance_funded_gross: TokenAmount,
    pub insurance_funded_net: TokenAmount,
    pub insurance_drawn_gross: TokenAmount,
    pub insurance_drawn_net: TokenAmount,
    pub socialized_loss: TokenAmount,
    pub remaining_debt: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskRevenueSideCheckpointV1 {
    pub swap_fee_growth_index_q64: TokenAmount,
    pub interest_growth_index_q64: TokenAmount,
    pub lp_swap_fee_liability: TokenAmount,
    pub lp_interest_liability: TokenAmount,
    pub unallocated_lp_swap_fee_liability: TokenAmount,
    pub unallocated_lp_interest_liability: TokenAmount,
    pub protocol_swap_fee_liability: TokenAmount,
    pub protocol_interest_fee_liability: TokenAmount,
    pub buyback_swap_fee_liability: TokenAmount,
    pub buyback_interest_fee_liability: TokenAmount,
    pub referral_interest_liability: TokenAmount,
    pub swap_fee_custody_balance: TokenAmount,
    pub interest_vault_balance: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskRevenueCheckpointV1 {
    pub base: DuskRevenueSideCheckpointV1,
    pub quote: DuskRevenueSideCheckpointV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskMarketSideCheckpointV1 {
    pub live_reserve: TokenAmount,
    pub cash_reserve: TokenAmount,
    pub protected_recenter_reserve: TokenAmount,
    pub base_hlp_backing_inventory: TokenAmount,
    pub quote_hlp_backing_inventory: TokenAmount,
    pub ylp_supply: TokenAmount,
    pub fixed_debt_shares: TokenAmount,
    pub fixed_debt: TokenAmount,
    pub fixed_debt_principal: TokenAmount,
    pub isolated_debt_shares: TokenAmount,
    pub isolated_debt: TokenAmount,
    pub isolated_debt_principal: TokenAmount,
    pub borrow_index_nad: TokenAmount,
    pub rate_at_target_nad: TokenAmount,
    pub daily_borrow_bucket: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskCurveCheckpointV1 {
    pub base_curve_reserve: TokenAmount,
    pub quote_curve_reserve: TokenAmount,
    pub spot_price_nad: TokenAmount,
    pub center_price_nad: TokenAmount,
    pub q_nad: TokenAmount,
    pub volatility_accumulator_nad: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskMarketCheckpointV1 {
    pub clock_slot: u64,
    pub clock_unix_timestamp: i64,
    pub base: DuskMarketSideCheckpointV1,
    pub quote: DuskMarketSideCheckpointV1,
    pub global_health_base_contribution_for_quote_debt: TokenAmount,
    pub global_health_quote_contribution_for_base_debt: TokenAmount,
    pub reduce_only: bool,
    pub curve: Option<DuskCurveCheckpointV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskHlpOwnedCheckpointV1 {
    pub owner_ylp_balance: TokenAmount,
    pub base_hlp_balance: TokenAmount,
    pub quote_hlp_balance: TokenAmount,
    pub base_hlp_vault_ylp_balance: TokenAmount,
    pub quote_hlp_vault_ylp_balance: TokenAmount,
    pub base_hlp_measured_base_interest_credit: TokenAmount,
    pub base_hlp_measured_quote_interest_credit: TokenAmount,
    pub quote_hlp_measured_base_interest_credit: TokenAmount,
    pub quote_hlp_measured_quote_interest_credit: TokenAmount,
    pub base_hlp_ylp_shares: TokenAmount,
    pub base_hlp_debt_shares: TokenAmount,
    pub base_hlp_debt_principal: TokenAmount,
    pub base_hlp_live_reserve_base: TokenAmount,
    pub base_hlp_live_reserve_quote: TokenAmount,
    pub base_hlp_residual_exposure: SignedTokenAmount,
    pub base_hlp_last_nav_nad: TokenAmount,
    pub base_hlp_cached_settlement_price_nad: TokenAmount,
    pub quote_hlp_ylp_shares: TokenAmount,
    pub quote_hlp_debt_shares: TokenAmount,
    pub quote_hlp_debt_principal: TokenAmount,
    pub quote_hlp_live_reserve_base: TokenAmount,
    pub quote_hlp_live_reserve_quote: TokenAmount,
    pub quote_hlp_residual_exposure: SignedTokenAmount,
    pub quote_hlp_last_nav_nad: TokenAmount,
    pub quote_hlp_cached_settlement_price_nad: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskHlpOperationObservationV1 {
    pub scenario: HlpScenario,
    /// Common pro-rata gross-allocation lattice used by the joint maximizer.
    pub allocation_numerator: TokenAmount,
    pub allocation_denominator: TokenAmount,
    pub entry_order: String,
    pub external_gross_base: TokenAmount,
    pub external_gross_quote: TokenAmount,
    pub ylp_gross_base: TokenAmount,
    pub ylp_gross_quote: TokenAmount,
    pub ylp_reserve_credit_base: TokenAmount,
    pub ylp_reserve_credit_quote: TokenAmount,
    pub base_hlp_gross: TokenAmount,
    pub base_hlp_reserve_credit: TokenAmount,
    pub quote_hlp_gross: TokenAmount,
    pub quote_hlp_reserve_credit: TokenAmount,
    pub ylp_minted: TokenAmount,
    pub base_hlp_minted: TokenAmount,
    pub quote_hlp_minted: TokenAmount,
    pub ylp_burned: TokenAmount,
    pub base_hlp_burned: TokenAmount,
    pub quote_hlp_burned: TokenAmount,
    pub base_hlp_target_out_gross: TokenAmount,
    pub base_hlp_target_out_net: TokenAmount,
    pub quote_hlp_target_out_gross: TokenAmount,
    pub quote_hlp_target_out_net: TokenAmount,
    /// Base-target hLP funding interest is denominated in quote.
    pub base_hlp_interest_paid_quote_gross: TokenAmount,
    pub base_hlp_interest_credit_quote: TokenAmount,
    /// Quote-target hLP funding interest is denominated in base.
    pub quote_hlp_interest_paid_base_gross: TokenAmount,
    pub quote_hlp_interest_credit_base: TokenAmount,
    pub no_extra_capital_verified: bool,
}

impl DuskHlpOperationObservationV1 {
    fn empty(scenario: HlpScenario) -> Self {
        Self {
            scenario,
            allocation_numerator: token(0),
            allocation_denominator: token(0),
            entry_order: String::new(),
            external_gross_base: token(0),
            external_gross_quote: token(0),
            ylp_gross_base: token(0),
            ylp_gross_quote: token(0),
            ylp_reserve_credit_base: token(0),
            ylp_reserve_credit_quote: token(0),
            base_hlp_gross: token(0),
            base_hlp_reserve_credit: token(0),
            quote_hlp_gross: token(0),
            quote_hlp_reserve_credit: token(0),
            ylp_minted: token(0),
            base_hlp_minted: token(0),
            quote_hlp_minted: token(0),
            ylp_burned: token(0),
            base_hlp_burned: token(0),
            quote_hlp_burned: token(0),
            base_hlp_target_out_gross: token(0),
            base_hlp_target_out_net: token(0),
            quote_hlp_target_out_gross: token(0),
            quote_hlp_target_out_net: token(0),
            base_hlp_interest_paid_quote_gross: token(0),
            base_hlp_interest_credit_quote: token(0),
            quote_hlp_interest_paid_base_gross: token(0),
            quote_hlp_interest_credit_base: token(0),
            no_extra_capital_verified: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterfactualOperationRecordV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub market: String,
    pub slot: u64,
    pub arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub synthetic_arbitrage: bool,
    pub synthetic_protocol_settlement: bool,
    pub status: CounterfactualStatusV1,
    pub protocol_swap_fee_bps: u16,
    pub protocol_interest_fee_bps: u16,
    pub protocol_interest_schedule_sha256: String,
    pub protocol_interest_epoch_id: String,
    pub factual_v1_interest_bps: u16,
    pub protocol_fee_auction_bps: u16,
    pub protocol_buyback_auction_bps: u16,
    pub limiting_constraints: Vec<CounterfactualConstraintV1>,
    pub cash: CounterfactualCashFlowV1,
    pub public_interest: DuskPublicInterestTransitionV1,
    pub swap: Option<DuskSwapObservationV1>,
    pub spot_probes: Option<SpotProbeSetV1>,
    pub debt: Option<DuskDebtObservationV1>,
    pub liquidation: Option<DuskLiquidationObservationV1>,
    pub hlp: Option<DuskHlpOperationObservationV1>,
    pub hlp_checkpoint: DuskHlpOwnedCheckpointV1,
    pub checkpoint: DuskMarketCheckpointV1,
    pub revenue: DuskRevenueCheckpointV1,
    pub state_sha256: String,
}

impl CounterfactualOperationRecordV1 {
    pub fn bind_spot_probes(&mut self, probes: SpotProbeSetV1) -> Result<()> {
        probes.validate_hash()?;
        if self.spot_probes.is_some()
            || probes.schema_version != SPOT_PROBE_SCHEMA_VERSION
            || probes.operation_id != self.operation_id
            || probes.market != self.market
            || probes.slot != self.slot
            || probes.arm != self.arm
            || probes.hlp_scenario != self.hlp_scenario
        {
            bail!("spot probe set identity differs from its counterfactual record");
        }
        self.spot_probes = Some(probes);
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct PendingDuskLiquidationV1 {
    source_operation_id: String,
    position: String,
    debt_asset: MarketAsset,
    remaining_liquidator_gross: u64,
    first_floor_unix_timestamp: i64,
    floor_attempted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlpEntryOrderV1 {
    BaseThenQuote,
    QuoteThenBase,
}

struct HlpDepositSimulationV1 {
    market: BenchmarkMarket,
    base_hlp: BenchmarkHlpOwnedState,
    quote_hlp: BenchmarkHlpOwnedState,
    owner_ylp_minted: u64,
    cash: BenchmarkCashFlow,
    observation: DuskHlpOperationObservationV1,
}

pub struct DuskCounterfactualMarketV1<'a> {
    market: BenchmarkMarket,
    hlp_scenario: HlpScenario,
    base_hlp: Option<BenchmarkHlpOwnedState>,
    quote_hlp: Option<BenchmarkHlpOwnedState>,
    owner_ylp_balance: u64,
    positions: BTreeMap<String, BenchmarkBorrowPosition>,
    transfers: &'a ValidatedTransferFeeRegistryV1,
    token0: String,
    token1: String,
    market_key: Pubkey,
    profile_id: String,
    protocol_swap_fee_bps: u16,
    protocol_interest_fee_bps: u16,
    protocol_interest_schedule_sha256: String,
    protocol_interest_epoch_id: String,
    factual_v1_interest_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
    global_reduce_only: bool,
    v1_lp_supply: u64,
    lp_ownership_ambiguous: bool,
    base_collateral_vault_balance: u64,
    quote_collateral_vault_balance: u64,
    pending_liquidations: BTreeMap<String, PendingDuskLiquidationV1>,
}

/// Full private counterfactual state retained only long enough to evaluate
/// standardized leverage cells. It deliberately has no serde implementation:
/// public campaign artifacts commit to `state_sha256` and never persist native
/// accounts or holder YieldAccounts.
pub(crate) struct DuskLeveragePrivateSnapshotV1 {
    pub(crate) market: BenchmarkMarket,
    pub(crate) hlp_scenario: HlpScenario,
    pub(crate) base_hlp: Option<BenchmarkHlpOwnedState>,
    pub(crate) quote_hlp: Option<BenchmarkHlpOwnedState>,
    pub(crate) base_collateral_vault_balance: u64,
    pub(crate) quote_collateral_vault_balance: u64,
    pub(crate) protocol_swap_fee_bps: u16,
    pub(crate) protocol_interest_fee_bps: u16,
    pub(crate) protocol_auction_split: ProtocolAuctionSplit,
    pub(crate) global_reduce_only: bool,
    pub(crate) state_sha256: String,
}

impl DuskLeveragePrivateSnapshotV1 {
    pub(crate) fn token_balances(
        &self,
        debt_asset: MarketAsset,
        starting_margin_gross: u64,
    ) -> Result<BenchmarkLeverageTokenBalances> {
        let checkpoint = self.market.checkpoint()?;
        let revenue = self.market.revenue_checkpoint();
        let reserve_custody = |side: dusk::benchmark_api::BenchmarkMarketSideCheckpoint,
                               fee: dusk::benchmark_api::BenchmarkRevenueSideCheckpoint|
         -> Result<u64> {
            side.cash_reserve
                .checked_add(fee.swap_fee_custody_balance)
                .and_then(|value| value.checked_add(side.base_hlp_backing_inventory))
                .and_then(|value| value.checked_add(side.quote_hlp_backing_inventory))
                .and_then(|value| value.checked_add(side.protected_recenter_reserve))
                .context("Dusk leverage reserve custody overflow")
        };
        let (owner_base_balance, owner_quote_balance) = match debt_asset {
            MarketAsset::Base => (starting_margin_gross, 0),
            MarketAsset::Quote => (0, starting_margin_gross),
        };
        Ok(BenchmarkLeverageTokenBalances {
            owner_base_balance,
            owner_quote_balance,
            // The counterfactual injects no unsolicited custody. These are
            // therefore its exact physical floors, not synthetic liquidity.
            base_reserve_vault_balance: reserve_custody(checkpoint.base, revenue.base)?,
            quote_reserve_vault_balance: reserve_custody(checkpoint.quote, revenue.quote)?,
            base_interest_vault_balance: revenue.base.interest_vault_balance,
            quote_interest_vault_balance: revenue.quote.interest_vault_balance,
            base_leverage_collateral_vault_balance: self.base_collateral_vault_balance,
            quote_leverage_collateral_vault_balance: self.quote_collateral_vault_balance,
        })
    }

    pub(crate) const fn policy(
        &self,
        collateral_mint_is_leverage_eligible: bool,
    ) -> BenchmarkLeveragePolicy {
        BenchmarkLeveragePolicy {
            protocol_swap_fee_bps: self.protocol_swap_fee_bps,
            protocol_interest_fee_bps: self.protocol_interest_fee_bps,
            protocol_auction_split: self.protocol_auction_split,
            max_referral_interest_share_bps: 0,
            global_reduce_only: self.global_reduce_only,
            collateral_mint_is_leverage_eligible,
            // Launch-only policy is disabled for historical replay.
            launch_same_transaction_guard_satisfied: true,
        }
    }
}

impl<'a> DuskCounterfactualMarketV1<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_from_v1_with_resolved_interest_policy(
        operation: &CanonicalOperationV1,
        profile: &DuskProfileV1,
        protocol_swap_fee_bps: u16,
        interest_policy: &ResolvedProtocolInterestPolicyV1,
        transfers: &'a ValidatedTransferFeeRegistryV1,
        arm: ReplayArm,
        hlp_scenario: HlpScenario,
    ) -> Result<(Self, CounterfactualOperationRecordV1)> {
        interest_policy.validate()?;
        let (mut market, mut record) = Self::initialize_from_v1_with_hlp(
            operation,
            profile,
            protocol_swap_fee_bps,
            interest_policy.dusk_protocol_interest_share_bps,
            transfers,
            arm,
            hlp_scenario,
        )?;
        market.protocol_interest_schedule_sha256 = interest_policy.schedule_sha256.clone();
        market.protocol_interest_epoch_id = interest_policy.epoch_id.clone();
        market.factual_v1_interest_bps = interest_policy.factual_v1_interest_bps;
        record.protocol_interest_schedule_sha256 = interest_policy.schedule_sha256.clone();
        record.protocol_interest_epoch_id = interest_policy.epoch_id.clone();
        record.factual_v1_interest_bps = interest_policy.factual_v1_interest_bps;
        Ok((market, record))
    }

    pub(crate) fn capture_leverage_private_snapshot(
        &self,
    ) -> Result<DuskLeveragePrivateSnapshotV1> {
        match (
            self.hlp_scenario,
            self.base_hlp.as_ref(),
            self.quote_hlp.as_ref(),
        ) {
            (HlpScenario::NoHlp, None, None) | (HlpScenario::MaximumFeasible, Some(_), Some(_)) => {
            }
            _ => bail!("counterfactual hLP scenario has incomplete owned state"),
        }
        Ok(DuskLeveragePrivateSnapshotV1 {
            market: self.market.try_fork()?,
            hlp_scenario: self.hlp_scenario,
            base_hlp: self
                .base_hlp
                .as_ref()
                .map(BenchmarkHlpOwnedState::try_fork)
                .transpose()?,
            quote_hlp: self
                .quote_hlp
                .as_ref()
                .map(BenchmarkHlpOwnedState::try_fork)
                .transpose()?,
            base_collateral_vault_balance: self.base_collateral_vault_balance,
            quote_collateral_vault_balance: self.quote_collateral_vault_balance,
            protocol_swap_fee_bps: self.protocol_swap_fee_bps,
            protocol_interest_fee_bps: self.protocol_interest_fee_bps,
            protocol_auction_split: self.protocol_auction_split,
            global_reduce_only: self.global_reduce_only,
            state_sha256: state_sha256(
                &self.market,
                &self.positions,
                &self.pending_liquidations,
                self.hlp_scenario,
                self.owner_ylp_balance,
                self.base_hlp.as_ref(),
                self.quote_hlp.as_ref(),
                &self.profile_id,
            )?,
        })
    }

    pub(crate) fn profile_id(&self) -> &str {
        &self.profile_id
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize_from_v1(
        operation: &CanonicalOperationV1,
        profile: &DuskProfileV1,
        protocol_swap_fee_bps: u16,
        protocol_interest_fee_bps: u16,
        transfers: &'a ValidatedTransferFeeRegistryV1,
        arm: ReplayArm,
    ) -> Result<(Self, CounterfactualOperationRecordV1)> {
        Self::initialize_from_v1_with_hlp(
            operation,
            profile,
            protocol_swap_fee_bps,
            protocol_interest_fee_bps,
            transfers,
            arm,
            HlpScenario::NoHlp,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize_from_v1_with_hlp(
        operation: &CanonicalOperationV1,
        profile: &DuskProfileV1,
        protocol_swap_fee_bps: u16,
        protocol_interest_fee_bps: u16,
        transfers: &'a ValidatedTransferFeeRegistryV1,
        arm: ReplayArm,
        hlp_scenario: HlpScenario,
    ) -> Result<(Self, CounterfactualOperationRecordV1)> {
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
            fields,
            ..
        } = &operation.payload
        else {
            bail!("Dusk market initialization requires a V1 initialize operation");
        };
        let base_fee_bps = required_u16(fields, "swap_fee_bps")?;
        profile.validate_for_base_fee(base_fee_bps)?;
        let market_key = Pubkey::from_str(&operation.pair).context("invalid V1 pair public key")?;
        let token0_key = Pubkey::from_str(token0).context("invalid token0 public key")?;
        let token1_key = Pubkey::from_str(token1).context("invalid token1 public key")?;
        let authority = operation
            .actor
            .as_ref()
            .context("V1 initializer has no canonical actor")
            .and_then(|actor| Pubkey::from_str(actor).context("invalid initializer public key"))?;
        let clock = operation_clock(operation);
        let config = market_config(profile, base_fee_bps, operation.block_time_unix_seconds)?;
        let base_side = MarketSide {
            asset_mint: token0_key,
            asset_decimals: *token0_decimals,
            hlp_mint: synthetic_key("base-hlp-mint", &operation.pair, &profile.profile_id),
            reserve_vault: synthetic_key("base-reserve", &operation.pair, &profile.profile_id),
            collateral_vault: synthetic_key(
                "base-collateral",
                &operation.pair,
                &profile.profile_id,
            ),
            interest_vault: synthetic_key("base-interest", &operation.pair, &profile.profile_id),
            ..MarketSide::default()
        };
        let quote_side = MarketSide {
            asset_mint: token1_key,
            asset_decimals: *token1_decimals,
            hlp_mint: synthetic_key("quote-hlp-mint", &operation.pair, &profile.profile_id),
            reserve_vault: synthetic_key("quote-reserve", &operation.pair, &profile.profile_id),
            collateral_vault: synthetic_key(
                "quote-collateral",
                &operation.pair,
                &profile.profile_id,
            ),
            interest_vault: synthetic_key("quote-interest", &operation.pair, &profile.profile_id),
            ..MarketSide::default()
        };
        let mut params_hash = [0_u8; 32];
        params_hash.copy_from_slice(&Sha256::digest(
            format!(
                "dusk-counterfactual:{}:{}",
                operation.pair, profile.profile_id
            )
            .as_bytes(),
        ));
        let market = BenchmarkMarket::initialize_at(
            market_key,
            BenchmarkMarketInit {
                ylp_mint: synthetic_key("ylp-mint", &operation.pair, &profile.profile_id),
                base_side,
                quote_side,
                config,
                base_hlp_ylp_vault: synthetic_key(
                    "base-hlp-ylp-vault",
                    &operation.pair,
                    &profile.profile_id,
                ),
                quote_hlp_ylp_vault: synthetic_key(
                    "quote-hlp-ylp-vault",
                    &operation.pair,
                    &profile.profile_id,
                ),
                base_insurance_vault: synthetic_key(
                    "base-insurance",
                    &operation.pair,
                    &profile.profile_id,
                ),
                quote_insurance_vault: synthetic_key(
                    "quote-insurance",
                    &operation.pair,
                    &profile.profile_id,
                ),
                params_hash,
                initial_liquidity_authority: authority,
                bootstrap_price_nad: 0,
                launch_fee_progress_offset: 0,
                bump: 255,
            },
            clock,
        )?;
        let mut replay = Self {
            market,
            hlp_scenario,
            base_hlp: None,
            quote_hlp: None,
            owner_ylp_balance: 0,
            positions: BTreeMap::new(),
            transfers,
            token0: token0.clone(),
            token1: token1.clone(),
            market_key,
            profile_id: profile.profile_id.clone(),
            protocol_swap_fee_bps,
            protocol_interest_fee_bps,
            protocol_interest_schedule_sha256: "0".repeat(64),
            protocol_interest_epoch_id: "direct_unsealed".to_owned(),
            factual_v1_interest_bps: protocol_interest_fee_bps,
            protocol_auction_split: ProtocolAuctionSplit::default(),
            global_reduce_only: false,
            v1_lp_supply: amount_u64("initialize.minted_lp", minted_lp.as_str())?
                .checked_add(MIN_LIQUIDITY)
                .context("V1 initial LP supply overflow")?,
            lp_ownership_ambiguous: false,
            base_collateral_vault_balance: 0,
            quote_collateral_vault_balance: 0,
            pending_liquidations: BTreeMap::new(),
        };
        if hlp_scenario == HlpScenario::MaximumFeasible {
            let owner = synthetic_key("maximum-hlp-owner", &operation.pair, &profile.profile_id);
            replay.base_hlp = Some(BenchmarkHlpOwnedState::initialize(
                &replay.market,
                MarketAsset::Base,
                owner,
                owner,
            )?);
            replay.quote_hlp = Some(BenchmarkHlpOwnedState::initialize(
                &replay.market,
                MarketAsset::Quote,
                owner,
                owner,
            )?);
        }
        transfers.assert_observed_credit(
            token0,
            operation.order.slot,
            amount_u64("initialize.gross_amount0", gross_amount0.as_str())?,
            amount_u64("initialize.vault_credit0", vault_credit0.as_str())?,
        )?;
        transfers.assert_observed_credit(
            token1,
            operation.order.slot,
            amount_u64("initialize.gross_amount1", gross_amount1.as_str())?,
            amount_u64("initialize.vault_credit1", vault_credit1.as_str())?,
        )?;
        let (cash, hlp) = replay.execute_lp_deposit(
            operation,
            authority,
            amount_u64("initialize.gross_amount0", gross_amount0.as_str())?,
            amount_u64("initialize.gross_amount1", gross_amount1.as_str())?,
            amount_u64("initialize.vault_credit0", vault_credit0.as_str())?,
            amount_u64("initialize.vault_credit1", vault_credit1.as_str())?,
        )?;
        let record = replay.record_with_hlp(
            operation,
            arm,
            CounterfactualStatusV1::Executed,
            Vec::new(),
            cash,
            None,
            None,
            false,
            hlp,
        )?;
        Ok((replay, record))
    }

    /// Quotes every standardized spot probe from one exact pre-operation
    /// state. Clock advancement and all native previews happen on a fork;
    /// neither the historical swap nor another probe can affect this set.
    pub fn preview_spot_probes(
        &self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        plan: &SpotProbePlanV1,
    ) -> Result<SpotProbeSetV1> {
        if !matches!(operation.payload, OperationPayloadV1::SwapExactIn { .. })
            || operation.pair != self.market_key.to_string()
            || plan.factual.market != operation.pair
            || plan.factual.token0 != self.token0
            || plan.factual.token1 != self.token1
            || !plan
                .sizing_window
                .contains(operation.block_time_unix_seconds)
            || plan.entries.len() != 2 * (plan.sizing_window.standardized_trade_nav_bps.len() + 2)
        {
            bail!("spot probe plan identity or cardinality differs from its factual swap");
        }
        let clock = operation_clock(operation);
        let mut preview_market = self.market.try_fork()?;
        preview_market.advance_to(clock)?;
        let dusk_pre_state_sha256 = state_sha256(
            &preview_market,
            &self.positions,
            &self.pending_liquidations,
            self.hlp_scenario,
            self.owner_ylp_balance,
            self.base_hlp.as_ref(),
            self.quote_hlp.as_ref(),
            &self.profile_id,
        )?;
        let mut observations = Vec::with_capacity(plan.entries.len());
        for entry in &plan.entries {
            let dusk = match entry.gross_input.filter(|amount| *amount > 0) {
                None => None,
                Some(gross_input) => {
                    let asset_in = if entry.direction.token0_in() {
                        MarketAsset::Base
                    } else {
                        MarketAsset::Quote
                    };
                    let input_mint = if entry.direction.token0_in() {
                        &self.token0
                    } else {
                        &self.token1
                    };
                    let output_mint = if entry.direction.token0_in() {
                        &self.token1
                    } else {
                        &self.token0
                    };
                    let input_transfer =
                        self.transfers
                            .transfer(input_mint, operation.order.slot, gross_input)?;
                    let request = BenchmarkSwapRequest {
                        asset_in,
                        reserve_credit: input_transfer.net_amount,
                        protocol_fee_bps: self.protocol_swap_fee_bps,
                        protocol_auction_split: self.protocol_auction_split,
                    };
                    Some(match preview_market.preview_swap(request) {
                        Err(_) => unsupported_quote(
                            gross_input,
                            input_transfer.fee_amount,
                            input_transfer.net_amount,
                            SpotProbeUnsupportedReasonV1::NativePreviewRejected,
                        ),
                        Ok(preview_execution) => {
                            let exact_execution = match self.hlp_scenario {
                                HlpScenario::NoHlp => Ok(preview_execution),
                                HlpScenario::MaximumFeasible => {
                                    match (self.base_hlp.as_ref(), self.quote_hlp.as_ref()) {
                                        (Some(base_hlp), Some(quote_hlp)) => {
                                            let mut market_fork = preview_market.try_fork()?;
                                            let mut base_fork = base_hlp.try_fork()?;
                                            let mut quote_fork = quote_hlp.try_fork()?;
                                            let base_interest = self.transfers.transfer(
                                                &self.token1,
                                                operation.order.slot,
                                                preview_execution.base_rebalance.interest_paid,
                                            )?;
                                            let quote_interest = self.transfers.transfer(
                                                &self.token0,
                                                operation.order.slot,
                                                preview_execution.quote_rebalance.interest_paid,
                                            )?;
                                            market_fork
                                                .execute_swap_with_hlp(
                                                    &mut base_fork,
                                                    &mut quote_fork,
                                                    BenchmarkHlpAwareSwapRequest {
                                                        swap: request,
                                                        base_hlp_interest_transfer:
                                                            BenchmarkTokenTransferOutcome {
                                                                source_debit: preview_execution
                                                                    .base_rebalance
                                                                    .interest_paid,
                                                                destination_credit: base_interest
                                                                    .net_amount,
                                                            },
                                                        quote_hlp_interest_transfer:
                                                            BenchmarkTokenTransferOutcome {
                                                                source_debit: preview_execution
                                                                    .quote_rebalance
                                                                    .interest_paid,
                                                                destination_credit: quote_interest
                                                                    .net_amount,
                                                            },
                                                        protocol_interest_fee_bps: self
                                                            .protocol_interest_fee_bps,
                                                        protocol_auction_split: self
                                                            .protocol_auction_split,
                                                    },
                                                )
                                                .map(|settled| settled.swap)
                                                .map_err(|_| {
                                                    SpotProbeUnsupportedReasonV1::HlpSettlementRejected
                                                })
                                        }
                                        _ => {
                                            Err(SpotProbeUnsupportedReasonV1::MissingHlpOwnedState)
                                        }
                                    }
                                }
                            };
                            match exact_execution {
                                Err(reason) => unsupported_quote(
                                    gross_input,
                                    input_transfer.fee_amount,
                                    input_transfer.net_amount,
                                    reason,
                                ),
                                Ok(execution) => {
                                    if execution != preview_execution {
                                        bail!("hLP-aware spot probe execution differs from native preview");
                                    }
                                    let quote = execution.quote;
                                    let output_transfer = self.transfers.transfer(
                                        output_mint,
                                        operation.order.slot,
                                        quote.amount_out,
                                    )?;
                                    let (execution_cost, slippage, depth, depth_reason) =
                                        derive_quote_metrics(
                                            entry.direction,
                                            quote.fee.amount_in_for_quote,
                                            quote.gross_amount_out,
                                            gross_input,
                                            output_transfer.net_amount,
                                            plan.factual_reference_price_nad,
                                            quote.start_price_nad,
                                            plan.factual.token0_decimals,
                                            plan.factual.token1_decimals,
                                            None,
                                        )?;
                                    let protocol_fee = u64::try_from(
                                        u128::from(quote.fee.base_fee_debit)
                                            .checked_mul(u128::from(self.protocol_swap_fee_bps))
                                            .context("Dusk probe protocol fee overflow")?
                                            / u128::from(BPS_DENOMINATOR),
                                    )?;
                                    let lp_fee = quote
                                        .fee
                                        .base_fee_debit
                                        .checked_sub(protocol_fee)
                                        .and_then(|value| {
                                            value.checked_add(quote.fee.distributed_surcharge_debit)
                                        })
                                        .context("Dusk probe LP fee overflow")?;
                                    SpotProbeQuoteV1 {
                                        executable: true,
                                        unsupported_reason: None,
                                        gross_input: token(gross_input),
                                        input_transfer_fee: token(input_transfer.fee_amount),
                                        net_input: token(input_transfer.net_amount),
                                        curve_input: token(quote.fee.amount_in_for_quote),
                                        curve_gross_output: token(quote.gross_amount_out),
                                        gross_output: token(quote.amount_out),
                                        output_transfer_fee: token(output_transfer.fee_amount),
                                        net_output: token(output_transfer.net_amount),
                                        lp_swap_fee: token(lp_fee),
                                        protocol_swap_fee: token(protocol_fee),
                                        dynamic_swap_fee: token(quote.fee.dynamic_surcharge_debit),
                                        start_price_nad: token(quote.start_price_nad),
                                        end_price_nad: token(quote.end_price_nad),
                                        common_reference_price_nad: token(
                                            plan.factual_reference_price_nad,
                                        ),
                                        execution_cost_ppb: execution_cost,
                                        slippage_ppb: slippage,
                                        cpmm_equivalent_depth_per_external_lp_value: depth,
                                        external_lp_nav_in_input: None,
                                        external_lp_nav_basis: None,
                                        depth_unsupported_reason: depth_reason,
                                    }
                                }
                            }
                        }
                    })
                }
            };
            let (gross_quote_improvement_ppb, net_quote_improvement_ppb) =
                match (entry.v1.as_ref(), dusk.as_ref()) {
                    (Some(v1), Some(dusk)) if v1.executable && dusk.executable => (
                        Some(quote_improvement_ppb(&v1.gross_output, &dusk.gross_output)?),
                        Some(quote_improvement_ppb(&v1.net_output, &dusk.net_output)?),
                    ),
                    _ => (None, None),
                };
            observations.push(SpotProbeObservationV1 {
                size_kind: entry.size_kind,
                direction: entry.direction,
                factual_nav_bps: entry.factual_nav_bps,
                gross_input: entry.gross_input.map(token),
                size_unsupported_reason: entry.size_unsupported_reason,
                v1: entry.v1.clone(),
                dusk,
                gross_quote_improvement_ppb,
                net_quote_improvement_ppb,
            });
        }
        SpotProbeSetV1 {
            schema_version: SPOT_PROBE_SCHEMA_VERSION,
            profile_id: self.profile_id.clone(),
            arm,
            hlp_scenario: self.hlp_scenario,
            operation_id: operation.operation_id.clone(),
            market: operation.pair.clone(),
            slot: operation.order.slot,
            factual_record_sha256: plan.factual_record_sha256.clone(),
            factual_pre_state_sha256: plan.factual_pre_state_sha256.clone(),
            dusk_pre_state_sha256,
            sizing_window: plan.sizing_window.clone(),
            sizing_source_sha256: plan.sizing_source_sha256.clone(),
            observations,
            probe_set_sha256: String::new(),
        }
        .seal()
    }

    pub fn apply(
        &mut self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        behavior: BehavioralContextV1,
    ) -> Result<CounterfactualOperationRecordV1> {
        if operation.pair != self.market_key.to_string()
            && !matches!(operation.payload, OperationPayloadV1::UpdateConfig { .. })
        {
            bail!("counterfactual operation belongs to a different market");
        }
        let public_interest_before = self.market.public_interest_checkpoint()?;
        let mut public_interest_clock_preview = self.market.try_fork()?;
        public_interest_clock_preview.advance_to(operation_clock(operation))?;
        let public_interest_after_clock =
            public_interest_clock_preview.public_interest_checkpoint()?;
        let mut record = match &operation.payload {
            OperationPayloadV1::InitializeMarket { .. } => {
                bail!("counterfactual market cannot be initialized twice")
            }
            OperationPayloadV1::AddLiquidity {
                gross_amount0,
                gross_amount1,
                vault_credit0,
                vault_credit1,
                minted_lp,
                ..
            } => {
                let base_credit = amount_u64("add.vault_credit0", vault_credit0.as_str())?;
                let quote_credit = amount_u64("add.vault_credit1", vault_credit1.as_str())?;
                let base_transfer = self.transfers.assert_observed_credit_with_maximum_gross(
                    &self.token0,
                    operation.order.slot,
                    amount_u64("add.gross_amount0", gross_amount0.as_str())?,
                    base_credit,
                )?;
                let quote_transfer = self.transfers.assert_observed_credit_with_maximum_gross(
                    &self.token1,
                    operation.order.slot,
                    amount_u64("add.gross_amount1", gross_amount1.as_str())?,
                    quote_credit,
                )?;
                let factual_minted = amount_u64("add.minted_lp", minted_lp.as_str())?;
                let owner = actor(operation)?;
                let (record, dusk_deposit_backed) = match self.execute_lp_deposit(
                    operation,
                    owner,
                    base_transfer.gross_amount,
                    quote_transfer.gross_amount,
                    base_credit,
                    quote_credit,
                ) {
                    Ok((cash, hlp)) => (
                        self.record_with_hlp(
                            operation,
                            arm,
                            CounterfactualStatusV1::Executed,
                            Vec::new(),
                            cash,
                            None,
                            None,
                            false,
                            hlp,
                        )?,
                        true,
                    ),
                    Err(_) => (
                        self.rejected(
                            operation,
                            arm,
                            CounterfactualConstraintV1::NativeLiquidityOrHealth,
                        )?,
                        false,
                    ),
                };
                // This is the factual V1 LP supply used only to translate each
                // later factual burn into the same ownership fraction on Dusk.
                // It must follow V1 history even when Dusk rejects the deposit.
                self.v1_lp_supply = self
                    .v1_lp_supply
                    .checked_add(factual_minted)
                    .context("V1 LP supply overflow")?;
                if !dusk_deposit_backed {
                    self.lp_ownership_ambiguous = true;
                }
                Ok(record)
            }
            OperationPayloadV1::RemoveLiquidity { burned_lp, .. } => {
                let burned = amount_u64("remove.burned_lp", burned_lp.as_str())?;
                if burned == 0 || burned > self.v1_lp_supply {
                    bail!("factual V1 LP burn is inconsistent with factual LP supply");
                }
                let record = if self.lp_ownership_ambiguous {
                    self.record(
                        operation,
                        arm,
                        CounterfactualStatusV1::DeferredExternalState,
                        vec![CounterfactualConstraintV1::LpOwnershipProvenance],
                        BenchmarkCashFlow::default(),
                        None,
                        None,
                        false,
                    )?
                } else {
                    self.remove_liquidity(operation, arm, burned_lp)?
                };
                self.v1_lp_supply = self
                    .v1_lp_supply
                    .checked_sub(burned)
                    .context("V1 LP supply underflow")?;
                if record.status != CounterfactualStatusV1::Executed {
                    self.lp_ownership_ambiguous = true;
                }
                Ok(record)
            }
            OperationPayloadV1::SwapExactIn {
                token0_in,
                gross_amount_in,
                vault_credit_in,
                min_amount_out,
                ..
            } => self.swap(
                operation,
                arm,
                *token0_in,
                gross_amount_in,
                vault_credit_in,
                min_amount_out,
                false,
            ),
            OperationPayloadV1::CollateralDelta {
                position,
                amount0,
                amount1,
                gross_transfer_amount,
                vault_amount,
                counterparty_amount,
                ..
            } => self.collateral(
                operation,
                arm,
                position,
                amount0,
                amount1,
                gross_transfer_amount,
                vault_amount,
                counterparty_amount,
            ),
            OperationPayloadV1::Borrow {
                position,
                debt_token0,
                ..
            } => self.borrow(
                operation,
                arm,
                position,
                *debt_token0,
                behavior
                    .borrow_capacity_fraction
                    .context("borrow replay requires the factual V1 capacity fraction")?
                    .validate()?,
            ),
            OperationPayloadV1::Repay {
                position,
                debt_token0,
                ..
            } => self.repay(
                operation,
                arm,
                position,
                *debt_token0,
                behavior
                    .repayment_fraction
                    .context("repay replay requires the factual V1 debt-reduction fraction")?
                    .validate()?,
            ),
            OperationPayloadV1::Liquidate {
                position,
                debt_token0,
                requested_repay,
                ..
            } => self.liquidate(operation, arm, position, *debt_token0, requested_repay),
            OperationPayloadV1::Flashloan { .. } => self.record(
                operation,
                arm,
                CounterfactualStatusV1::NoEconomicCounterpart,
                vec![CounterfactualConstraintV1::V1OnlyFlashloan],
                BenchmarkCashFlow::default(),
                None,
                None,
                false,
            ),
            OperationPayloadV1::ClaimProtocolRevenue { .. } => self.record(
                operation,
                arm,
                CounterfactualStatusV1::NoEconomicCounterpart,
                vec![CounterfactualConstraintV1::V1ClaimRealization],
                BenchmarkCashFlow::default(),
                None,
                None,
                false,
            ),
            OperationPayloadV1::UpdateConfig { fields } => {
                self.apply_config(operation, arm, fields)
            }
            OperationPayloadV1::ClockOnly => {
                self.market.advance_to(operation_clock(operation))?;
                self.record(
                    operation,
                    arm,
                    CounterfactualStatusV1::Executed,
                    Vec::new(),
                    BenchmarkCashFlow::default(),
                    None,
                    None,
                    false,
                )
            }
        }?;
        let committed_clock = self.market.checkpoint()?.clock.slot >= operation.order.slot;
        self.attach_public_interest_transition(
            public_interest_before,
            if committed_clock {
                public_interest_after_clock
            } else {
                public_interest_before
            },
            &mut record,
        )?;
        Ok(record)
    }

    pub fn apply_with_resolved_interest_policy(
        &mut self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        behavior: BehavioralContextV1,
        interest_policy: &ResolvedProtocolInterestPolicyV1,
    ) -> Result<CounterfactualOperationRecordV1> {
        interest_policy.validate()?;
        if interest_policy.schedule_sha256 != self.protocol_interest_schedule_sha256 {
            bail!("protocol-interest schedule changed during counterfactual replay");
        }
        let epoch_changed = interest_policy.epoch_id != self.protocol_interest_epoch_id;
        let protocol_epoch_update = match &operation.payload {
            OperationPayloadV1::UpdateConfig { fields }
                if matches!(
                    fields.get("instruction_kind").map(String::as_str),
                    Some("update_futarchy_authority" | "update_protocol_revenue")
                ) =>
            {
                Some(hex::encode(Sha256::digest(
                    format!("{}:ProtocolRevenue:__global__", operation.operation_id).as_bytes(),
                )))
            }
            _ => None,
        };
        if let Some(expected_epoch_id) = protocol_epoch_update {
            if !epoch_changed || interest_policy.epoch_id != expected_epoch_id {
                bail!("canonical protocol-revenue update is not a distinct frozen epoch");
            }
        } else if epoch_changed {
            bail!("protocol-interest epoch changed without a canonical config operation");
        } else if interest_policy.factual_v1_interest_bps != self.factual_v1_interest_bps
            || interest_policy.dusk_protocol_interest_share_bps != self.protocol_interest_fee_bps
        {
            bail!("protocol-interest policy changed inside one frozen epoch");
        }

        let previous_epoch = self.protocol_interest_epoch_id.clone();
        let previous_v1_bps = self.factual_v1_interest_bps;
        let previous_dusk_bps = self.protocol_interest_fee_bps;
        self.protocol_interest_epoch_id = interest_policy.epoch_id.clone();
        self.factual_v1_interest_bps = interest_policy.factual_v1_interest_bps;
        self.protocol_interest_fee_bps = interest_policy.dusk_protocol_interest_share_bps;
        match self.apply(operation, arm, behavior) {
            Ok(record) => Ok(record),
            Err(error) => {
                self.protocol_interest_epoch_id = previous_epoch;
                self.factual_v1_interest_bps = previous_v1_bps;
                self.protocol_interest_fee_bps = previous_dusk_bps;
                Err(error)
            }
        }
    }

    /// At an intent-arm block boundary, executes the smallest representable
    /// external arbitrage credit that returns Dusk's marginal price to the
    /// observed V1 band. `None` means the market was already in-band. An
    /// unreachable band is emitted as a typed rejected synthetic operation;
    /// no state or capital is silently injected.
    pub fn arbitrage_to_price_band(
        &mut self,
        anchor: &CanonicalOperationV1,
        lower_price_nad: u64,
        upper_price_nad: u64,
    ) -> Result<Option<CounterfactualOperationRecordV1>> {
        if lower_price_nad == 0 || lower_price_nad > upper_price_nad {
            bail!("observed V1 price band is invalid");
        }
        let clock = operation_clock(anchor);
        let mut at_boundary = self.market.try_fork()?;
        at_boundary.advance_to(clock)?;
        let current_price = at_boundary.curve_snapshot()?.spot_price_nad;
        if (lower_price_nad..=upper_price_nad).contains(&current_price) {
            return Ok(None);
        }

        // Dusk prices are quote-per-base: base input moves price downward and
        // quote input moves it upward.
        let asset_in = if current_price > upper_price_nad {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let checkpoint = at_boundary.checkpoint()?;
        let input_live_reserve = if asset_in == MarketAsset::Base {
            checkpoint.base.live_reserve
        } else {
            checkpoint.quote.live_reserve
        };
        let numerical_maximum = u64::MAX.saturating_sub(input_live_reserve);
        let reaches_near_edge = |price: u64| {
            if asset_in == MarketAsset::Base {
                price <= upper_price_nad
            } else {
                price >= lower_price_nad
            }
        };

        let mut last_below = 0_u64;
        let mut candidate = 1_u64.min(numerical_maximum);
        let high = loop {
            if candidate == 0 {
                break None;
            }
            match self.preview_arbitrage_price(clock, asset_in, candidate) {
                Ok(price) if reaches_near_edge(price) => break Some(candidate),
                Ok(_) => {
                    last_below = candidate;
                    if candidate == numerical_maximum {
                        break None;
                    }
                    candidate = candidate
                        .saturating_mul(2)
                        .min(numerical_maximum)
                        .max(candidate + 1);
                }
                Err(_) => {
                    // Native overflow/liquidity failures bound the feasible
                    // integer domain. Locate its last previewable point, then
                    // fail closed if even that point cannot reach the band.
                    let mut low_valid = last_below;
                    let mut high_invalid = candidate;
                    while low_valid + 1 < high_invalid {
                        let middle = low_valid + (high_invalid - low_valid) / 2;
                        if self
                            .preview_arbitrage_price(clock, asset_in, middle)
                            .is_ok()
                        {
                            low_valid = middle;
                        } else {
                            high_invalid = middle;
                        }
                    }
                    if low_valid > last_below
                        && reaches_near_edge(
                            self.preview_arbitrage_price(clock, asset_in, low_valid)?,
                        )
                    {
                        break Some(low_valid);
                    }
                    break None;
                }
            }
        };

        let Some(mut high) = high else {
            return self
                .record(
                    anchor,
                    ReplayArm::Intent,
                    CounterfactualStatusV1::Rejected,
                    vec![CounterfactualConstraintV1::ArbitragePriceBand],
                    BenchmarkCashFlow::default(),
                    None,
                    None,
                    true,
                )
                .map(Some);
        };
        let mut low = last_below.min(high.saturating_sub(1));
        while low + 1 < high {
            let middle = low + (high - low) / 2;
            let price = self.preview_arbitrage_price(clock, asset_in, middle)?;
            if reaches_near_edge(price) {
                high = middle;
            } else {
                low = middle;
            }
        }
        let final_price = self.preview_arbitrage_price(clock, asset_in, high)?;
        if !(lower_price_nad..=upper_price_nad).contains(&final_price) {
            return self
                .record(
                    anchor,
                    ReplayArm::Intent,
                    CounterfactualStatusV1::Rejected,
                    vec![CounterfactualConstraintV1::ArbitragePriceBand],
                    BenchmarkCashFlow::default(),
                    None,
                    None,
                    true,
                )
                .map(Some);
        }

        let mint = self.mint(asset_in);
        let gross = self
            .transfers
            .gross_for_exact_net(mint, anchor.order.slot, high)?
            .gross_amount;
        let mut synthetic = anchor.clone();
        synthetic.operation_id = format!(
            "synthetic-arbitrage:{}:{}:{}",
            anchor.operation_id,
            if asset_in == MarketAsset::Base {
                "base"
            } else {
                "quote"
            },
            high
        );
        synthetic.signature = format!("synthetic:arbitrage:{}", anchor.signature);
        synthetic.order.transaction_index = u32::MAX;
        synthetic.order.outer_instruction_index = u32::MAX;
        synthetic.order.inner_execution_index = None;
        synthetic.order.event_sequence = u32::MAX;
        synthetic.actor = None;
        synthetic.assertions.clear();
        synthetic.payload = OperationPayloadV1::SwapExactIn {
            token0_in: asset_in == MarketAsset::Base,
            gross_amount_in: token(gross),
            vault_credit_in: token(high),
            min_amount_out: token(0),
            vault_debit_out: token(0),
            recipient_credit_out: token(0),
        };
        let OperationPayloadV1::SwapExactIn {
            token0_in,
            gross_amount_in,
            vault_credit_in,
            min_amount_out,
            ..
        } = &synthetic.payload
        else {
            unreachable!("synthetic payload is a swap")
        };
        self.swap(
            &synthetic,
            ReplayArm::Intent,
            *token0_in,
            gross_amount_in,
            vault_credit_in,
            min_amount_out,
            true,
        )
        .map(Some)
    }

    fn preview_arbitrage_price(
        &self,
        clock: BenchmarkClock,
        asset_in: MarketAsset,
        reserve_credit: u64,
    ) -> Result<u64> {
        let mut market = self.market.try_fork()?;
        market.advance_to(clock)?;
        if reserve_credit == 0 {
            return Ok(market.curve_snapshot()?.spot_price_nad);
        }
        Ok(market
            .preview_swap(BenchmarkSwapRequest {
                asset_in,
                reserve_credit,
                protocol_fee_bps: self.protocol_swap_fee_bps,
                protocol_auction_split: self.protocol_auction_split,
            })?
            .quote
            .end_price_nad)
    }

    #[allow(clippy::too_many_arguments)]
    fn swap(
        &mut self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        token0_in: bool,
        gross_amount_in: &TokenAmount,
        vault_credit_in: &TokenAmount,
        min_amount_out: &TokenAmount,
        synthetic_arbitrage: bool,
    ) -> Result<CounterfactualOperationRecordV1> {
        let gross = amount_u64("swap.gross_amount_in", gross_amount_in.as_str())?;
        let reserve_credit = amount_u64("swap.vault_credit_in", vault_credit_in.as_str())?;
        let min_out = amount_u64("swap.min_amount_out", min_amount_out.as_str())?;
        let input_mint = if token0_in {
            &self.token0
        } else {
            &self.token1
        };
        self.transfers.assert_observed_credit(
            input_mint,
            operation.order.slot,
            gross,
            reserve_credit,
        )?;
        let request = BenchmarkSwapRequest {
            asset_in: if token0_in {
                MarketAsset::Base
            } else {
                MarketAsset::Quote
            },
            reserve_credit,
            protocol_fee_bps: self.protocol_swap_fee_bps,
            protocol_auction_split: self.protocol_auction_split,
        };
        let clock = operation_clock(operation);
        let mut preview_market = self.market.try_fork()?;
        preview_market.advance_to(clock)?;
        let preview = match preview_market.preview_swap(request) {
            Ok(preview) => preview,
            Err(_) => {
                return self.rejected(
                    operation,
                    arm,
                    CounterfactualConstraintV1::NativeLiquidityOrHealth,
                )
            }
        };
        let min_passed = preview.quote.amount_out >= min_out;
        let output_mint = if token0_in {
            &self.token1
        } else {
            &self.token0
        };
        let recipient_credit = self
            .transfers
            .transfer(output_mint, operation.order.slot, preview.quote.amount_out)?
            .net_amount;
        let input_transfer_fee = gross
            .checked_sub(reserve_credit)
            .context("counterfactual swap reserve credit exceeds gross input")?;
        let output_transfer_fee = preview
            .quote
            .amount_out
            .checked_sub(recipient_credit)
            .context("counterfactual swap recipient credit exceeds gross output")?;
        let observation = DuskSwapObservationV1 {
            asset_in_base: token0_in,
            gross_amount_in: token(gross),
            input_transfer_fee: token(input_transfer_fee),
            reserve_credit_in: token(reserve_credit),
            curve_input: token(preview.quote.fee.amount_in_for_quote),
            curve_gross_output: token(preview.quote.gross_amount_out),
            amount_out_before_token_fee: token(preview.quote.amount_out),
            output_transfer_fee: token(output_transfer_fee),
            recipient_credit_out: token(recipient_credit),
            base_fee: token(preview.quote.fee.base_fee_debit),
            divergence_fee: token(preview.quote.fee.divergence_surcharge_debit),
            volatility_fee: token(preview.quote.fee.volatility_surcharge_debit),
            dynamic_fee: token(preview.quote.fee.dynamic_surcharge_debit),
            distributed_dynamic_fee: token(preview.quote.fee.distributed_surcharge_debit),
            retained_recenter_inventory: token(preview.quote.fee.retained_surcharge),
            compounded_base_lp_fee: token(preview.quote.fee.compounded_base_fee_debit),
            compounded_dynamic_lp_fee: token(preview.quote.fee.compounded_dynamic_surcharge_debit),
            total_fee: token(preview.quote.fee.total_fee_debit),
            start_price_nad: token(preview.quote.start_price_nad),
            end_price_nad: token(preview.quote.end_price_nad),
            reserve_end_price_nad: token(preview.quote.reserve_end_price_nad),
            original_min_out: token(min_out),
            original_min_out_passed: min_passed,
        };
        if arm == ReplayArm::Intent && !min_passed {
            return self.record(
                operation,
                arm,
                CounterfactualStatusV1::SkippedIntentMinOut,
                vec![CounterfactualConstraintV1::HistoricalMinOut],
                BenchmarkCashFlow::default(),
                Some(observation),
                None,
                synthetic_arbitrage,
            );
        }
        let mut cash = BenchmarkCashFlow::default();
        match request.asset_in {
            MarketAsset::Base => {
                cash.base.reserve_vault_credit = reserve_credit;
                cash.quote.reserve_vault_debit = preview.quote.amount_out;
                cash.quote.recipient_credit = recipient_credit;
            }
            MarketAsset::Quote => {
                cash.quote.reserve_vault_credit = reserve_credit;
                cash.base.reserve_vault_debit = preview.quote.amount_out;
                cash.base.recipient_credit = recipient_credit;
            }
        }
        let hlp = match self.hlp_scenario {
            HlpScenario::NoHlp => {
                self.market.advance_to(clock)?;
                self.market.execute_swap(request)?;
                None
            }
            HlpScenario::MaximumFeasible => {
                let base_interest_gross = preview.base_rebalance.interest_paid;
                let quote_interest_gross = preview.quote_rebalance.interest_paid;
                let base_interest_transfer = self.transfers.transfer(
                    &self.token1,
                    operation.order.slot,
                    base_interest_gross,
                )?;
                let quote_interest_transfer = self.transfers.transfer(
                    &self.token0,
                    operation.order.slot,
                    quote_interest_gross,
                )?;
                let mut next_market = self.market.try_fork()?;
                let mut next_base = self
                    .base_hlp
                    .as_ref()
                    .context("maximum-feasible scenario has no base hLP state")?
                    .try_fork()?;
                let mut next_quote = self
                    .quote_hlp
                    .as_ref()
                    .context("maximum-feasible scenario has no quote hLP state")?
                    .try_fork()?;
                next_market.advance_to(clock)?;
                let execution = next_market.execute_swap_with_hlp(
                    &mut next_base,
                    &mut next_quote,
                    BenchmarkHlpAwareSwapRequest {
                        swap: request,
                        base_hlp_interest_transfer: BenchmarkTokenTransferOutcome {
                            source_debit: base_interest_gross,
                            destination_credit: base_interest_transfer.net_amount,
                        },
                        quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome {
                            source_debit: quote_interest_gross,
                            destination_credit: quote_interest_transfer.net_amount,
                        },
                        protocol_interest_fee_bps: self.protocol_interest_fee_bps,
                        protocol_auction_split: self.protocol_auction_split,
                    },
                )?;
                checked_accumulate_cash(&mut cash, execution.hlp_interest_cash)?;
                self.market = next_market;
                self.base_hlp = Some(next_base);
                self.quote_hlp = Some(next_quote);
                let mut hlp = DuskHlpOperationObservationV1::empty(self.hlp_scenario);
                hlp.base_hlp_interest_paid_quote_gross = token(base_interest_gross);
                hlp.base_hlp_interest_credit_quote = token(base_interest_transfer.net_amount);
                hlp.quote_hlp_interest_paid_base_gross = token(quote_interest_gross);
                hlp.quote_hlp_interest_credit_base = token(quote_interest_transfer.net_amount);
                Some(hlp)
            }
        };
        match hlp {
            Some(hlp) => self.record_with_hlp(
                operation,
                arm,
                CounterfactualStatusV1::Executed,
                Vec::new(),
                cash,
                Some(observation),
                None,
                synthetic_arbitrage,
                hlp,
            ),
            None => self.record(
                operation,
                arm,
                CounterfactualStatusV1::Executed,
                Vec::new(),
                cash,
                Some(observation),
                None,
                synthetic_arbitrage,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_lp_deposit(
        &mut self,
        operation: &CanonicalOperationV1,
        owner: Pubkey,
        gross_base: u64,
        gross_quote: u64,
        observed_base_credit: u64,
        observed_quote_credit: u64,
    ) -> Result<(BenchmarkCashFlow, DuskHlpOperationObservationV1)> {
        let clock = operation_clock(operation);
        match self.hlp_scenario {
            HlpScenario::NoHlp => {
                let execution = self.market.execute_add_ylp(BenchmarkAddYlpRequest {
                    clock,
                    owner,
                    base_reserve_credit: observed_base_credit,
                    quote_reserve_credit: observed_quote_credit,
                    min_ylp_amount: 0,
                    global_reduce_only: self.global_reduce_only,
                })?;
                self.owner_ylp_balance = self
                    .owner_ylp_balance
                    .checked_add(execution.receipt.ylp_amount)
                    .context("aggregate yLP holder balance overflow")?;
                let mut observation = DuskHlpOperationObservationV1::empty(self.hlp_scenario);
                observation.external_gross_base = token(gross_base);
                observation.external_gross_quote = token(gross_quote);
                observation.ylp_gross_base = token(gross_base);
                observation.ylp_gross_quote = token(gross_quote);
                observation.ylp_reserve_credit_base = token(observed_base_credit);
                observation.ylp_reserve_credit_quote = token(observed_quote_credit);
                observation.ylp_minted = token(execution.receipt.ylp_amount);
                Ok((execution.cash, observation))
            }
            HlpScenario::MaximumFeasible => {
                let simulation =
                    self.maximize_joint_hlp_deposit(operation, owner, gross_base, gross_quote)?;
                self.market = simulation.market;
                self.base_hlp = Some(simulation.base_hlp);
                self.quote_hlp = Some(simulation.quote_hlp);
                self.owner_ylp_balance = self
                    .owner_ylp_balance
                    .checked_add(simulation.owner_ylp_minted)
                    .context("aggregate yLP holder balance overflow")?;
                Ok((simulation.cash, simulation.observation))
            }
        }
    }

    /// Maximizes the common pro-rata fraction of the two exact gross wallet
    /// budgets assigned to hLP. This one-dimensional lattice is the frozen
    /// joint objective: it treats both tokens symmetrically, preserves their
    /// contributed proportions, and has a deterministic base-first tie-break.
    /// Every candidate adds its gross remainder to yLP before invoking both
    /// native maximum-entry helpers, so no token atom is used twice.
    fn maximize_joint_hlp_deposit(
        &self,
        operation: &CanonicalOperationV1,
        owner: Pubkey,
        gross_base: u64,
        gross_quote: u64,
    ) -> Result<HlpDepositSimulationV1> {
        let denominator = gross_base.max(gross_quote);
        if denominator == 0 {
            bail!("LP deposit has no gross token budget");
        }
        // Zero hLP is the exact all-yLP baseline. If it cannot execute, the
        // factual cash flow cannot initialize or add liquidity on Dusk.
        self.best_joint_hlp_simulation(operation, owner, gross_base, gross_quote, 0, denominator)?
            .context("all-yLP baseline is not executable")?;

        let mut low = 0_u64;
        let mut high = denominator;
        while low < high {
            let middle = low + (high - low) / 2 + (high - low) % 2;
            if self
                .best_joint_hlp_simulation(
                    operation,
                    owner,
                    gross_base,
                    gross_quote,
                    middle,
                    denominator,
                )?
                .is_some()
            {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        self.best_joint_hlp_simulation(operation, owner, gross_base, gross_quote, low, denominator)?
            .context("selected maximum-feasible hLP allocation stopped being executable")
    }

    #[allow(clippy::too_many_arguments)]
    fn best_joint_hlp_simulation(
        &self,
        operation: &CanonicalOperationV1,
        owner: Pubkey,
        gross_base: u64,
        gross_quote: u64,
        numerator: u64,
        denominator: u64,
    ) -> Result<Option<HlpDepositSimulationV1>> {
        match self.simulate_joint_hlp_deposit(
            operation,
            owner,
            gross_base,
            gross_quote,
            numerator,
            denominator,
            HlpEntryOrderV1::BaseThenQuote,
        ) {
            Ok(Some(simulation)) => Ok(Some(simulation)),
            Ok(None) => self.simulate_joint_hlp_deposit(
                operation,
                owner,
                gross_base,
                gross_quote,
                numerator,
                denominator,
                HlpEntryOrderV1::QuoteThenBase,
            ),
            Err(base_first_error) => match self.simulate_joint_hlp_deposit(
                operation,
                owner,
                gross_base,
                gross_quote,
                numerator,
                denominator,
                HlpEntryOrderV1::QuoteThenBase,
            ) {
                Ok(Some(simulation)) => Ok(Some(simulation)),
                Ok(None) | Err(_) => Err(base_first_error),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn simulate_joint_hlp_deposit(
        &self,
        operation: &CanonicalOperationV1,
        owner: Pubkey,
        gross_base: u64,
        gross_quote: u64,
        numerator: u64,
        denominator: u64,
        order: HlpEntryOrderV1,
    ) -> Result<Option<HlpDepositSimulationV1>> {
        if denominator == 0 || numerator > denominator {
            bail!("invalid hLP allocation lattice point");
        }
        let base_hlp_gross = proportional_floor(gross_base, numerator, denominator)?;
        let quote_hlp_gross = proportional_floor(gross_quote, numerator, denominator)?;
        let base_ylp_gross = gross_base
            .checked_sub(base_hlp_gross)
            .context("base hLP allocation exceeds wallet budget")?;
        let quote_ylp_gross = gross_quote
            .checked_sub(quote_hlp_gross)
            .context("quote hLP allocation exceeds wallet budget")?;
        let base_ylp_transfer =
            self.transfers
                .transfer(&self.token0, operation.order.slot, base_ylp_gross)?;
        let quote_ylp_transfer =
            self.transfers
                .transfer(&self.token1, operation.order.slot, quote_ylp_gross)?;
        let base_hlp_transfer =
            self.transfers
                .transfer(&self.token0, operation.order.slot, base_hlp_gross)?;
        let quote_hlp_transfer =
            self.transfers
                .transfer(&self.token1, operation.order.slot, quote_hlp_gross)?;

        let mut market = self.market.try_fork()?;
        let mut base_hlp = self
            .base_hlp
            .as_ref()
            .context("maximum-feasible scenario has no base hLP state")?
            .try_fork()?;
        let mut quote_hlp = self
            .quote_hlp
            .as_ref()
            .context("maximum-feasible scenario has no quote hLP state")?
            .try_fork()?;
        let clock = operation_clock(operation);
        let mut cash = BenchmarkCashFlow::default();
        let mut ylp_minted = 0_u64;
        if base_ylp_transfer.net_amount > 0 || quote_ylp_transfer.net_amount > 0 {
            let execution = match market.execute_add_ylp(BenchmarkAddYlpRequest {
                clock,
                owner,
                base_reserve_credit: base_ylp_transfer.net_amount,
                quote_reserve_credit: quote_ylp_transfer.net_amount,
                min_ylp_amount: 0,
                global_reduce_only: self.global_reduce_only,
            }) {
                Ok(execution) => execution,
                Err(error) if numerator == 0 => return Err(error.into()),
                Err(_) => return Ok(None),
            };
            ylp_minted = execution.receipt.ylp_amount;
            checked_accumulate_cash(&mut cash, execution.cash)?;
        } else if market.checkpoint()?.base.ylp_supply == 0 {
            return Ok(None);
        }

        let mut base_hlp_minted = 0_u64;
        let mut quote_hlp_minted = 0_u64;
        let ordered_assets = match order {
            HlpEntryOrderV1::BaseThenQuote => [MarketAsset::Base, MarketAsset::Quote],
            HlpEntryOrderV1::QuoteThenBase => [MarketAsset::Quote, MarketAsset::Base],
        };
        for asset in ordered_assets {
            let (gross, net, external, counterpart, minted) = match asset {
                MarketAsset::Base => (
                    base_hlp_gross,
                    base_hlp_transfer.net_amount,
                    &mut base_hlp,
                    &mut quote_hlp,
                    &mut base_hlp_minted,
                ),
                MarketAsset::Quote => (
                    quote_hlp_gross,
                    quote_hlp_transfer.net_amount,
                    &mut quote_hlp,
                    &mut base_hlp,
                    &mut quote_hlp_minted,
                ),
            };
            if gross == 0 {
                continue;
            }
            if net == 0 {
                return Ok(None);
            }
            let Some(plan) =
                market.maximize_hlp_entry(external, clock, net, self.global_reduce_only)?
            else {
                return Ok(None);
            };
            if plan.target_reserve_credit != net {
                return Ok(None);
            }
            let execution = market.execute_maximum_hlp_entry(
                external,
                counterpart,
                plan,
                BenchmarkTokenTransferOutcome {
                    source_debit: gross,
                    destination_credit: net,
                },
                Default::default(),
                self.global_reduce_only,
            )?;
            *minted = execution.market.receipt.native.hlp_amount;
            checked_accumulate_cash(&mut cash, execution.market.cash)?;
        }

        let mut observation = DuskHlpOperationObservationV1::empty(self.hlp_scenario);
        observation.allocation_numerator = token(numerator);
        observation.allocation_denominator = token(denominator);
        observation.entry_order = match order {
            HlpEntryOrderV1::BaseThenQuote => "base_then_quote",
            HlpEntryOrderV1::QuoteThenBase => "quote_then_base",
        }
        .to_owned();
        observation.external_gross_base = token(gross_base);
        observation.external_gross_quote = token(gross_quote);
        observation.ylp_gross_base = token(base_ylp_gross);
        observation.ylp_gross_quote = token(quote_ylp_gross);
        observation.ylp_reserve_credit_base = token(base_ylp_transfer.net_amount);
        observation.ylp_reserve_credit_quote = token(quote_ylp_transfer.net_amount);
        observation.base_hlp_gross = token(base_hlp_gross);
        observation.base_hlp_reserve_credit = token(base_hlp_transfer.net_amount);
        observation.quote_hlp_gross = token(quote_hlp_gross);
        observation.quote_hlp_reserve_credit = token(quote_hlp_transfer.net_amount);
        observation.ylp_minted = token(ylp_minted);
        observation.base_hlp_minted = token(base_hlp_minted);
        observation.quote_hlp_minted = token(quote_hlp_minted);
        observation.no_extra_capital_verified = base_ylp_gross
            .checked_add(base_hlp_gross)
            .is_some_and(|value| value == gross_base)
            && quote_ylp_gross
                .checked_add(quote_hlp_gross)
                .is_some_and(|value| value == gross_quote);
        if !observation.no_extra_capital_verified {
            bail!("hLP allocation did not conserve external gross capital");
        }
        Ok(Some(HlpDepositSimulationV1 {
            market,
            base_hlp,
            quote_hlp,
            owner_ylp_minted: ylp_minted,
            cash,
            observation,
        }))
    }

    fn remove_liquidity(
        &mut self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        burned_lp: &TokenAmount,
    ) -> Result<CounterfactualOperationRecordV1> {
        let burned = amount_u64("remove.burned_lp", burned_lp.as_str())?;
        let ylp_amount =
            proportional_holder_burn(self.owner_ylp_balance, burned, self.v1_lp_supply)?;
        let base_hlp_amount = match self.base_hlp.as_ref() {
            Some(state) => proportional_holder_burn(
                state.holder_hlp_token_balance(),
                burned,
                self.v1_lp_supply,
            )?,
            None => 0,
        };
        let quote_hlp_amount = match self.quote_hlp.as_ref() {
            Some(state) => proportional_holder_burn(
                state.holder_hlp_token_balance(),
                burned,
                self.v1_lp_supply,
            )?,
            None => 0,
        };
        let clock = operation_clock(operation);
        let mut next_market = self.market.try_fork()?;
        next_market.advance_to(clock)?;
        let mut next_base_hlp = self
            .base_hlp
            .as_ref()
            .map(BenchmarkHlpOwnedState::try_fork)
            .transpose()?;
        let mut next_quote_hlp = self
            .quote_hlp
            .as_ref()
            .map(BenchmarkHlpOwnedState::try_fork)
            .transpose()?;
        let mut next_ylp_balance = self.owner_ylp_balance;
        let mut cash = BenchmarkCashFlow::default();
        let mut hlp = DuskHlpOperationObservationV1::empty(self.hlp_scenario);
        hlp.ylp_burned = token(ylp_amount);
        hlp.base_hlp_burned = token(base_hlp_amount);
        hlp.quote_hlp_burned = token(quote_hlp_amount);

        if base_hlp_amount > 0 {
            let external = next_base_hlp
                .as_mut()
                .context("base hLP ownership disappeared during withdrawal")?;
            let preview = next_market.preview_hlp_withdrawal(external, clock, base_hlp_amount)?;
            let target = self.transfers.transfer(
                &self.token0,
                operation.order.slot,
                preview.target_amount_out,
            )?;
            let interest = self.transfers.transfer(
                &self.token1,
                operation.order.slot,
                preview.interest_paid,
            )?;
            let execution = next_market.execute_hlp_withdraw(
                external,
                BenchmarkHlpWithdrawRequest {
                    clock,
                    hlp_amount: base_hlp_amount,
                    target_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: preview.target_amount_out,
                        destination_credit: target.net_amount,
                    },
                    interest_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: preview.interest_paid,
                        destination_credit: interest.net_amount,
                    },
                    min_target_recipient_credit: 0,
                    protocol_interest_fee_bps: self.protocol_interest_fee_bps,
                    protocol_auction_split: self.protocol_auction_split,
                },
            )?;
            checked_accumulate_cash(&mut cash, execution.market.cash)?;
            hlp.base_hlp_target_out_gross = token(preview.target_amount_out);
            hlp.base_hlp_target_out_net = token(target.net_amount);
            hlp.base_hlp_interest_paid_quote_gross = token(preview.interest_paid);
            hlp.base_hlp_interest_credit_quote = token(interest.net_amount);
        }
        if quote_hlp_amount > 0 {
            let external = next_quote_hlp
                .as_mut()
                .context("quote hLP ownership disappeared during withdrawal")?;
            let preview = next_market.preview_hlp_withdrawal(external, clock, quote_hlp_amount)?;
            let target = self.transfers.transfer(
                &self.token1,
                operation.order.slot,
                preview.target_amount_out,
            )?;
            let interest = self.transfers.transfer(
                &self.token0,
                operation.order.slot,
                preview.interest_paid,
            )?;
            let execution = next_market.execute_hlp_withdraw(
                external,
                BenchmarkHlpWithdrawRequest {
                    clock,
                    hlp_amount: quote_hlp_amount,
                    target_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: preview.target_amount_out,
                        destination_credit: target.net_amount,
                    },
                    interest_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: preview.interest_paid,
                        destination_credit: interest.net_amount,
                    },
                    min_target_recipient_credit: 0,
                    protocol_interest_fee_bps: self.protocol_interest_fee_bps,
                    protocol_auction_split: self.protocol_auction_split,
                },
            )?;
            checked_accumulate_cash(&mut cash, execution.market.cash)?;
            hlp.quote_hlp_target_out_gross = token(preview.target_amount_out);
            hlp.quote_hlp_target_out_net = token(target.net_amount);
            hlp.quote_hlp_interest_paid_base_gross = token(preview.interest_paid);
            hlp.quote_hlp_interest_credit_base = token(interest.net_amount);
        }
        if ylp_amount > 0 {
            let mut preview_market = next_market.try_fork()?;
            let provisional = preview_market.execute_remove_ylp(BenchmarkRemoveYlpRequest {
                clock,
                ylp_amount,
                owner_ylp_balance_before: next_ylp_balance,
                base_recipient_credit: u64::MAX,
                quote_recipient_credit: u64::MAX,
                min_base_recipient_credit: 0,
                min_quote_recipient_credit: 0,
            })?;
            let base_recipient = self
                .transfers
                .transfer(
                    &self.token0,
                    operation.order.slot,
                    provisional.receipt.base_reserve_amount,
                )?
                .net_amount;
            let quote_recipient = self
                .transfers
                .transfer(
                    &self.token1,
                    operation.order.slot,
                    provisional.receipt.quote_reserve_amount,
                )?
                .net_amount;
            let execution = next_market.execute_remove_ylp(BenchmarkRemoveYlpRequest {
                clock,
                ylp_amount,
                owner_ylp_balance_before: next_ylp_balance,
                base_recipient_credit: base_recipient,
                quote_recipient_credit: quote_recipient,
                min_base_recipient_credit: 0,
                min_quote_recipient_credit: 0,
            })?;
            checked_accumulate_cash(&mut cash, execution.cash)?;
            next_ylp_balance = next_ylp_balance
                .checked_sub(ylp_amount)
                .context("aggregate yLP holder balance underflow")?;
        }

        self.market = next_market;
        self.base_hlp = next_base_hlp;
        self.quote_hlp = next_quote_hlp;
        self.owner_ylp_balance = next_ylp_balance;
        self.record_with_hlp(
            operation,
            arm,
            CounterfactualStatusV1::Executed,
            Vec::new(),
            cash,
            None,
            None,
            false,
            hlp,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn collateral(
        &mut self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        position_key: &str,
        amount0: &SignedTokenAmount,
        amount1: &SignedTokenAmount,
        gross_transfer_amount: &TokenAmount,
        vault_amount: &TokenAmount,
        counterparty_amount: &TokenAmount,
    ) -> Result<CounterfactualOperationRecordV1> {
        let delta0 = amount0.as_str().parse::<i128>()?;
        let delta1 = amount1.as_str().parse::<i128>()?;
        let (asset, delta) = match (delta0 != 0, delta1 != 0) {
            (true, false) => (MarketAsset::Base, delta0),
            (false, true) => (MarketAsset::Quote, delta1),
            _ => bail!("collateral operation must change exactly one token"),
        };
        let mint = if asset == MarketAsset::Base {
            self.token0.clone()
        } else {
            self.token1.clone()
        };
        let vault_amount = amount_u64("collateral.vault_amount", vault_amount.as_str())?;
        let counterparty = amount_u64(
            "collateral.counterparty_amount",
            counterparty_amount.as_str(),
        )?;
        self.ensure_position(operation, position_key)?;
        let position = self
            .positions
            .get_mut(position_key)
            .expect("inserted above");
        let result = if delta > 0 {
            self.transfers.assert_observed_credit(
                &mint,
                operation.order.slot,
                amount_u64(
                    "collateral.gross_transfer_amount",
                    gross_transfer_amount.as_str(),
                )?,
                vault_amount,
            )?;
            self.market
                .execute_deposit_collateral(
                    position,
                    BenchmarkDepositCollateralRequest {
                        clock: operation_clock(operation),
                        collateral_asset: asset,
                        collateral_credit: vault_amount,
                    },
                )
                .map(|execution| execution.market.cash)
        } else {
            self.transfers.assert_observed_credit(
                &mint,
                operation.order.slot,
                vault_amount,
                counterparty,
            )?;
            let balance = if asset == MarketAsset::Base {
                self.base_collateral_vault_balance
            } else {
                self.quote_collateral_vault_balance
            };
            self.market
                .execute_withdraw_collateral(
                    position,
                    BenchmarkWithdrawCollateralRequest {
                        clock: operation_clock(operation),
                        collateral_asset: asset,
                        collateral_debit: vault_amount,
                        collateral_vault_balance_before: balance,
                        recipient_credit: counterparty,
                        min_recipient_credit: 0,
                        min_liquidation_cf_bps: 0,
                        global_reduce_only: self.global_reduce_only,
                    },
                )
                .map(|execution| execution.market.cash)
        };
        match result {
            Ok(cash) => {
                let balance = if asset == MarketAsset::Base {
                    &mut self.base_collateral_vault_balance
                } else {
                    &mut self.quote_collateral_vault_balance
                };
                if delta > 0 {
                    *balance = balance
                        .checked_add(vault_amount)
                        .context("collateral vault overflow")?;
                } else {
                    *balance = balance
                        .checked_sub(vault_amount)
                        .context("collateral vault underflow")?;
                }
                self.record(
                    operation,
                    arm,
                    CounterfactualStatusV1::Executed,
                    Vec::new(),
                    cash,
                    None,
                    None,
                    false,
                )
            }
            Err(_) => self.rejected(operation, arm, CounterfactualConstraintV1::PositionHealth),
        }
    }

    fn borrow(
        &mut self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        position_key: &str,
        debt_token0: bool,
        fraction: CapacityFractionV1,
    ) -> Result<CounterfactualOperationRecordV1> {
        self.ensure_position(operation, position_key)?;
        let asset = if debt_token0 {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let clock = operation_clock(operation);
        let position = self.positions.get(position_key).expect("inserted above");
        let capacity = self.market.preview_existing_borrow_capacity(
            position,
            asset,
            clock,
            self.global_reduce_only,
        )?;
        let target = capacity
            .gross_admissible_max_debt
            .checked_mul(u128::from(fraction.numerator))
            .context("capacity fraction overflow")?
            / u128::from(fraction.denominator);
        let target_request = self.market.preview_existing_borrow_request_for_target(
            position,
            asset,
            clock,
            self.global_reduce_only,
            target,
        )?;
        let executable = target_request
            .borrow_request
            .min(capacity.actionable_max_additional);
        let requested_change = target.saturating_sub(capacity.debt_before);
        if requested_change == 0 {
            return self.record(
                operation,
                arm,
                CounterfactualStatusV1::Executed,
                Vec::new(),
                BenchmarkCashFlow::default(),
                None,
                Some(borrow_debt_observation(
                    asset,
                    &capacity,
                    target,
                    capacity.debt_before,
                    executable,
                    0,
                    0,
                )),
                false,
            );
        }
        let mut limiting_constraints = if executable < target_request.borrow_request {
            capacity_counterfactual_constraints(&capacity)
        } else {
            Vec::new()
        };
        if executable == 0 {
            if target_request.borrow_request == 0 && target_request.target_debt_gap > 0 {
                limiting_constraints.push(CounterfactualConstraintV1::DebtShareRounding);
            }
            return self.record(
                operation,
                arm,
                CounterfactualStatusV1::Rejected,
                limiting_constraints,
                BenchmarkCashFlow::default(),
                None,
                Some(borrow_debt_observation(
                    asset,
                    &capacity,
                    target,
                    capacity.debt_before,
                    executable,
                    0,
                    0,
                )),
                false,
            );
        }
        let recipient = self
            .transfers
            .transfer(self.mint(asset), operation.order.slot, executable)?
            .net_amount;
        let position = self
            .positions
            .get_mut(position_key)
            .expect("inserted above");
        match self.market.execute_borrow(
            position,
            BenchmarkBorrowRequest {
                clock,
                debt_asset: asset,
                borrow_amount: executable,
                recipient_credit: recipient,
                min_recipient_credit: 0,
                min_liquidation_cf_bps: 0,
                global_reduce_only: self.global_reduce_only,
                referral_partner: Pubkey::default(),
                referral_interest_share_bps: 0,
                referral_interest_share_cap_bps: 0,
            },
        ) {
            Ok(execution) => {
                let after = if asset == MarketAsset::Base {
                    execution.position_after.fixed_base_debt
                } else {
                    execution.position_after.fixed_quote_debt
                };
                let filled = after.saturating_sub(capacity.debt_before);
                let unmet = target.saturating_sub(after);
                if unmet > 0
                    && target_request.borrow_request <= capacity.actionable_max_additional
                    && target_request.target_debt_gap > 0
                    && !limiting_constraints
                        .contains(&CounterfactualConstraintV1::DebtShareRounding)
                {
                    limiting_constraints.push(CounterfactualConstraintV1::DebtShareRounding);
                }
                self.record(
                    operation,
                    arm,
                    if unmet == 0 {
                        CounterfactualStatusV1::Executed
                    } else {
                        CounterfactualStatusV1::PartiallyFilled
                    },
                    if unmet == 0 {
                        Vec::new()
                    } else {
                        limiting_constraints
                    },
                    execution.market.cash,
                    None,
                    Some(borrow_debt_observation(
                        asset, &capacity, target, after, executable, filled, 0,
                    )),
                    false,
                )
            }
            Err(_) => self.record(
                operation,
                arm,
                CounterfactualStatusV1::Rejected,
                vec![CounterfactualConstraintV1::NativeLiquidityOrHealth],
                BenchmarkCashFlow::default(),
                None,
                Some(borrow_debt_observation(
                    asset,
                    &capacity,
                    target,
                    capacity.debt_before,
                    executable,
                    0,
                    0,
                )),
                false,
            ),
        }
    }

    fn repay(
        &mut self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        position_key: &str,
        debt_token0: bool,
        fraction: CapacityFractionV1,
    ) -> Result<CounterfactualOperationRecordV1> {
        self.ensure_position(operation, position_key)?;
        let asset = if debt_token0 {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let clock = operation_clock(operation);
        let position = self.positions.get(position_key).expect("inserted above");
        let mut preview_market = self.market.try_fork()?;
        preview_market.advance_to(clock)?;
        let before_checkpoint = position.checkpoint(&preview_market)?;
        let debt_before = u64::try_from(if asset == MarketAsset::Base {
            before_checkpoint.fixed_base_debt
        } else {
            before_checkpoint.fixed_quote_debt
        })?;
        if debt_before == 0 || fraction.numerator == 0 {
            return self.record(
                operation,
                arm,
                CounterfactualStatusV1::Executed,
                Vec::new(),
                BenchmarkCashFlow::default(),
                None,
                Some(repay_debt_observation(
                    asset,
                    debt_before,
                    debt_before,
                    debt_before,
                    debt_before,
                    0,
                    0,
                    0,
                )),
                false,
            );
        }
        let requested_reduction = u64::try_from(
            u128::from(debt_before)
                .checked_mul(u128::from(fraction.numerator))
                .context("repayment fraction overflow")?
                .checked_add(u128::from(fraction.denominator - 1))
                .context("repayment rounding overflow")?
                / u128::from(fraction.denominator),
        )?;
        let quote = match preview_market.market().fixed_repayment_for_max(
            position.position(),
            asset,
            requested_reduction,
        ) {
            Ok(quote) => quote,
            Err(error) if is_unrepresentable_debt_share_repayment(&error) => {
                return self.record(
                    operation,
                    arm,
                    CounterfactualStatusV1::Rejected,
                    vec![CounterfactualConstraintV1::DebtShareRounding],
                    BenchmarkCashFlow::default(),
                    None,
                    Some(repay_debt_observation(
                        asset,
                        debt_before,
                        debt_before,
                        debt_before.saturating_sub(requested_reduction),
                        debt_before,
                        requested_reduction,
                        0,
                        0,
                    )),
                    false,
                );
            }
            Err(error) => return Err(error.into()),
        };
        let mut native_position = position.try_fork()?.into_position();
        let native_receipt = preview_market
            .transact(|market, _| market.repay(&mut native_position, asset, quote.cash_repaid))?;
        let interest_credit = self
            .transfers
            .transfer(
                self.mint(asset),
                operation.order.slot,
                native_receipt.interest_paid,
            )?
            .net_amount;
        let _gross_user_transfer = self.transfers.gross_for_exact_net(
            self.mint(asset),
            operation.order.slot,
            quote.cash_repaid,
        )?;
        let position = self
            .positions
            .get_mut(position_key)
            .expect("inserted above");
        match self.market.execute_repay(
            position,
            BenchmarkRepayRequest {
                clock,
                max_repay_credit: quote.cash_repaid,
                interest_vault_credit: interest_credit,
                protocol_interest_fee_bps: self.protocol_interest_fee_bps,
                protocol_auction_split: self.protocol_auction_split,
                referral_interest_amount: 0,
                debt_asset: asset,
            },
        ) {
            Ok(execution) => {
                let after = if asset == MarketAsset::Base {
                    u64::try_from(execution.position_after.fixed_base_debt)?
                } else {
                    u64::try_from(execution.position_after.fixed_quote_debt)?
                };
                let filled = debt_before.saturating_sub(after);
                let unmet = requested_reduction.saturating_sub(filled);
                self.record(
                    operation,
                    arm,
                    if unmet == 0 {
                        CounterfactualStatusV1::Executed
                    } else {
                        CounterfactualStatusV1::PartiallyFilled
                    },
                    if unmet == 0 {
                        Vec::new()
                    } else {
                        vec![CounterfactualConstraintV1::NativeLiquidityOrHealth]
                    },
                    execution.market.cash,
                    None,
                    Some(repay_debt_observation(
                        asset,
                        debt_before,
                        debt_before,
                        debt_before.saturating_sub(requested_reduction),
                        after,
                        requested_reduction,
                        filled,
                        native_receipt.interest_paid,
                    )),
                    false,
                )
            }
            Err(_) => self.record(
                operation,
                arm,
                CounterfactualStatusV1::Rejected,
                vec![CounterfactualConstraintV1::NativeLiquidityOrHealth],
                BenchmarkCashFlow::default(),
                None,
                Some(repay_debt_observation(
                    asset,
                    debt_before,
                    debt_before,
                    debt_before.saturating_sub(requested_reduction),
                    debt_before,
                    requested_reduction,
                    0,
                    0,
                )),
                false,
            ),
        }
    }

    fn liquidate(
        &mut self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        position_key: &str,
        debt_token0: bool,
        requested_repay: &TokenAmount,
    ) -> Result<CounterfactualOperationRecordV1> {
        self.ensure_position(operation, position_key)?;
        let debt_asset = if debt_token0 {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let clock = operation_clock(operation);
        let requested_gross = amount_u64("liquidation.requested_repay", requested_repay.as_str())?;
        if requested_gross == 0 {
            bail!("liquidation requested repay must be positive");
        }
        let position_checkpoint = self
            .positions
            .get(position_key)
            .context("missing Dusk liquidation position")?
            .checkpoint(&self.market)?;
        let active_asset = if position_checkpoint.auction_debt_asset == u8::MAX {
            None
        } else {
            Some(
                MarketAsset::try_from_code(position_checkpoint.auction_debt_asset)
                    .context("invalid Dusk liquidation auction side")?,
            )
        };
        if let Some(active_asset) = active_asset {
            if active_asset != debt_asset {
                bail!("historical liquidation demand targets the opposite active Dusk auction");
            }
        } else {
            let mut at_clock = self.market.try_fork()?;
            at_clock.advance_to(clock)?;
            let position = self
                .positions
                .get(position_key)
                .context("missing Dusk liquidation position")?;
            if !at_clock
                .market()
                .is_position_liquidatable(position.position(), debt_asset)?
            {
                let remaining_debt = if debt_asset == MarketAsset::Base {
                    position_checkpoint.fixed_base_debt
                } else {
                    position_checkpoint.fixed_quote_debt
                };
                return self.record_with_liquidation(
                    operation,
                    arm,
                    CounterfactualStatusV1::Rejected,
                    vec![CounterfactualConstraintV1::PositionHealth],
                    BenchmarkCashFlow::default(),
                    liquidation_observation(
                        &operation.operation_id,
                        position_key,
                        debt_asset,
                        DuskLiquidationPhaseV1::NotLiquidatable,
                        false,
                        false,
                        true,
                        false,
                        None,
                        requested_gross,
                        None,
                        remaining_debt,
                    ),
                    false,
                );
            }
            let position = self
                .positions
                .get_mut(position_key)
                .context("missing Dusk liquidation position")?;
            self.market.execute_start_liquidation_auction(
                position,
                BenchmarkStartLiquidationAuctionRequest { clock, debt_asset },
            )?;
        }

        let max_repay_credit = self
            .transfers
            .transfer(self.mint(debt_asset), operation.order.slot, requested_gross)?
            .net_amount;
        let plan_request = BenchmarkLiquidationPlanRequest {
            clock,
            debt_asset,
            phase: BenchmarkLiquidationPhase::Bid,
            max_repay_credit,
            collateral_reserve_credit: 0,
            protocol_swap_fee_bps: self.protocol_swap_fee_bps,
            protocol_auction_split: self.protocol_auction_split,
        };
        let position = self
            .positions
            .get(position_key)
            .context("missing Dusk liquidation position")?;
        let plan = match self.market.preview_liquidation_plan(position, plan_request) {
            Ok(plan) => plan,
            Err(_) => {
                let checkpoint = position.checkpoint(&self.market)?;
                let first_floor = position.first_liquidation_floor_unix_timestamp()?;
                let remaining_debt = position_debt_from_checkpoint(&checkpoint, debt_asset);
                self.merge_pending_liquidation(PendingDuskLiquidationV1 {
                    source_operation_id: operation.operation_id.clone(),
                    position: position_key.to_owned(),
                    debt_asset,
                    remaining_liquidator_gross: requested_gross,
                    first_floor_unix_timestamp: first_floor,
                    floor_attempted: false,
                })?;
                return self.record_with_liquidation(
                    operation,
                    arm,
                    CounterfactualStatusV1::PartiallyFilled,
                    vec![CounterfactualConstraintV1::LiquidationAuctionState],
                    BenchmarkCashFlow::default(),
                    liquidation_observation(
                        &operation.operation_id,
                        position_key,
                        debt_asset,
                        DuskLiquidationPhaseV1::ImmediateBid,
                        true,
                        false,
                        false,
                        true,
                        Some(first_floor),
                        requested_gross,
                        None,
                        remaining_debt,
                    ),
                    false,
                );
            }
        };
        let debt_outcome = self.transfers.gross_for_exact_net(
            self.mint(debt_asset),
            operation.order.slot,
            plan.repay_credit,
        )?;
        if debt_outcome.gross_amount > requested_gross {
            bail!("native liquidation repayment exceeds decoded historical gross demand");
        }
        let preview_request = BenchmarkLiquidationPreviewRequest {
            plan: plan_request,
            insurance_draw_credit: 0,
        };
        let preview = match self.market.preview_liquidation(position, preview_request) {
            Ok(preview) => preview,
            Err(_) => {
                let checkpoint = position.checkpoint(&self.market)?;
                let first_floor = position.first_liquidation_floor_unix_timestamp()?;
                let remaining_debt = position_debt_from_checkpoint(&checkpoint, debt_asset);
                self.merge_pending_liquidation(PendingDuskLiquidationV1 {
                    source_operation_id: operation.operation_id.clone(),
                    position: position_key.to_owned(),
                    debt_asset,
                    remaining_liquidator_gross: requested_gross,
                    first_floor_unix_timestamp: first_floor,
                    floor_attempted: false,
                })?;
                return self.record_with_liquidation(
                    operation,
                    arm,
                    CounterfactualStatusV1::PartiallyFilled,
                    vec![CounterfactualConstraintV1::LiquidationAuctionState],
                    BenchmarkCashFlow::default(),
                    liquidation_observation(
                        &operation.operation_id,
                        position_key,
                        debt_asset,
                        DuskLiquidationPhaseV1::ImmediateBid,
                        true,
                        false,
                        false,
                        true,
                        Some(first_floor),
                        requested_gross,
                        None,
                        remaining_debt,
                    ),
                    false,
                );
            }
        };
        let interest_transfer = self.transfer_outcome(
            debt_asset,
            operation.order.slot,
            preview.native.interest_paid,
        )?;
        let collateral_transfer = self.transfer_outcome(
            debt_asset.opposite(),
            operation.order.slot,
            preview.native.collateral_to_liquidator,
        )?;
        let insurance_funding_transfer = self.transfer_outcome(
            debt_asset.opposite(),
            operation.order.slot,
            preview.native.insurance_funded,
        )?;
        let debt_transfer = BenchmarkTokenTransferOutcome {
            source_debit: debt_outcome.gross_amount,
            destination_credit: debt_outcome.net_amount,
        };
        let mut balances = self.liquidation_balances(debt_asset, requested_gross)?;
        let position = self
            .positions
            .get_mut(position_key)
            .context("missing Dusk liquidation position")?;
        let execution = self.market.execute_liquidation(
            position,
            &mut balances,
            BenchmarkLiquidationExecuteRequest {
                preview: preview_request,
                max_repay_source_debit: requested_gross,
                debt_transfer,
                insurance_draw_transfer: BenchmarkTokenTransferOutcome::default(),
                interest_transfer,
                collateral_transfer,
                collateral_swap_transfer: BenchmarkTokenTransferOutcome::default(),
                owner_residual_transfer: BenchmarkTokenTransferOutcome::default(),
                insurance_funding_transfer,
                min_collateral_recipient_credit: 0,
                protocol_interest_fee_bps: self.protocol_interest_fee_bps,
                protocol_auction_split: self.protocol_auction_split,
                referral_interest_amount: 0,
            },
        )?;
        self.base_collateral_vault_balance = balances.base_collateral_vault_balance;
        self.quote_collateral_vault_balance = balances.quote_collateral_vault_balance;
        let residual_open = execution.position_after.auction_debt_asset != u8::MAX;
        let remaining_gross = requested_gross.saturating_sub(debt_transfer.source_debit);
        if residual_open {
            self.merge_pending_liquidation(PendingDuskLiquidationV1 {
                source_operation_id: operation.operation_id.clone(),
                position: position_key.to_owned(),
                debt_asset,
                remaining_liquidator_gross: remaining_gross,
                first_floor_unix_timestamp: execution
                    .market
                    .receipt
                    .plan
                    .first_floor_unix_timestamp,
                floor_attempted: false,
            })?;
        } else {
            self.pending_liquidations.remove(position_key);
        }
        let observation = liquidation_observation(
            &operation.operation_id,
            position_key,
            debt_asset,
            DuskLiquidationPhaseV1::ImmediateBid,
            true,
            true,
            !residual_open,
            residual_open,
            residual_open.then_some(execution.market.receipt.plan.first_floor_unix_timestamp),
            requested_gross,
            Some((
                execution.market.receipt.native,
                collateral_transfer.destination_credit,
                insurance_funding_transfer.destination_credit,
                execution.market.receipt.plan.insurance_draw_debit,
            )),
            execution.market.receipt.native.remaining_debt,
        );
        self.record_with_liquidation(
            operation,
            arm,
            if residual_open {
                CounterfactualStatusV1::PartiallyFilled
            } else {
                CounterfactualStatusV1::Executed
            },
            if residual_open {
                vec![CounterfactualConstraintV1::LiquidationAuctionState]
            } else {
                Vec::new()
            },
            execution.market.cash,
            observation,
            false,
        )
    }

    /// Executes one due floor transition before the next factual operation.
    /// Records are explicitly synthetic and retain the originating V1 demand
    /// identity; no new bidder balance is injected.
    pub fn settle_due_liquidations(
        &mut self,
        anchor: &CanonicalOperationV1,
        arm: ReplayArm,
    ) -> Result<Vec<CounterfactualOperationRecordV1>> {
        let due: Vec<String> = self
            .pending_liquidations
            .iter()
            .filter(|(_, pending)| {
                !pending.floor_attempted
                    && anchor.block_time_unix_seconds >= pending.first_floor_unix_timestamp
            })
            .map(|(position, _)| position.clone())
            .collect();
        let mut records = Vec::with_capacity(due.len());
        for position_key in due {
            let pending = self
                .pending_liquidations
                .get(&position_key)
                .context("missing due liquidation")?
                .clone();
            let record = self.settle_due_liquidation(anchor, arm, &pending)?;
            if let Some(still_pending) = self.pending_liquidations.get_mut(&position_key) {
                still_pending.floor_attempted = true;
            }
            records.push(record);
        }
        Ok(records)
    }

    pub fn pending_liquidation_count(&self) -> usize {
        self.pending_liquidations.len()
    }

    fn settle_due_liquidation(
        &mut self,
        anchor: &CanonicalOperationV1,
        arm: ReplayArm,
        pending: &PendingDuskLiquidationV1,
    ) -> Result<CounterfactualOperationRecordV1> {
        let clock = operation_clock(anchor);
        let public_interest_before = self.market.public_interest_checkpoint()?;
        let mut clock_preview = self.market.try_fork()?;
        clock_preview.advance_to(clock)?;
        let public_interest_after_clock = clock_preview.public_interest_checkpoint()?;
        let position = self
            .positions
            .get(&pending.position)
            .context("missing pending Dusk liquidation position")?;
        let position_checkpoint = position.checkpoint(&self.market)?;
        let collateral_consumed = match pending.debt_asset {
            MarketAsset::Base => position_checkpoint.quote_collateral,
            MarketAsset::Quote => position_checkpoint.base_collateral,
        };
        let caller_bounty = u64::try_from(
            (collateral_consumed as u128)
                .checked_mul(LIQUIDATION_BACKSTOP_CALLER_BPS as u128)
                .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
                .context("floor caller bounty overflow")?,
        )
        .context("floor caller bounty exceeds u64")?;
        let collateral_swap_debit = collateral_consumed
            .checked_sub(caller_bounty)
            .context("floor collateral swap underflow")?;
        let collateral_swap_transfer = self.transfer_outcome(
            pending.debt_asset.opposite(),
            anchor.order.slot,
            collateral_swap_debit,
        )?;
        let plan_request = BenchmarkLiquidationPlanRequest {
            clock,
            debt_asset: pending.debt_asset,
            phase: BenchmarkLiquidationPhase::Floor,
            max_repay_credit: 0,
            collateral_reserve_credit: collateral_swap_transfer.destination_credit,
            protocol_swap_fee_bps: self.protocol_swap_fee_bps,
            protocol_auction_split: self.protocol_auction_split,
        };
        let plan = self
            .market
            .preview_liquidation_plan(position, plan_request)?;
        let insurance_outcome = self.transfers.transfer(
            self.mint(pending.debt_asset),
            anchor.order.slot,
            plan.insurance_draw_debit,
        )?;
        let preview_request = BenchmarkLiquidationPreviewRequest {
            plan: plan_request,
            insurance_draw_credit: insurance_outcome.net_amount,
        };
        let preview = self.market.preview_liquidation(position, preview_request)?;
        let insurance_draw_transfer = BenchmarkTokenTransferOutcome {
            source_debit: insurance_outcome.gross_amount,
            destination_credit: insurance_outcome.net_amount,
        };
        let interest_transfer = self.transfer_outcome(
            pending.debt_asset,
            anchor.order.slot,
            preview.native.interest_paid,
        )?;
        let collateral_transfer = self.transfer_outcome(
            pending.debt_asset.opposite(),
            anchor.order.slot,
            preview.native.collateral_to_liquidator,
        )?;
        let owner_residual_transfer = self.transfer_outcome(
            pending.debt_asset,
            anchor.order.slot,
            preview.owner_residual,
        )?;
        let insurance_funding_transfer = BenchmarkTokenTransferOutcome::default();
        let mut balances = self.liquidation_balances(pending.debt_asset, 0)?;
        let position = self
            .positions
            .get_mut(&pending.position)
            .context("missing pending Dusk liquidation position")?;
        let execution = self.market.execute_liquidation(
            position,
            &mut balances,
            BenchmarkLiquidationExecuteRequest {
                preview: preview_request,
                max_repay_source_debit: 0,
                debt_transfer: BenchmarkTokenTransferOutcome::default(),
                insurance_draw_transfer,
                interest_transfer,
                collateral_transfer,
                collateral_swap_transfer,
                owner_residual_transfer,
                insurance_funding_transfer,
                min_collateral_recipient_credit: 0,
                protocol_interest_fee_bps: self.protocol_interest_fee_bps,
                protocol_auction_split: self.protocol_auction_split,
                referral_interest_amount: 0,
            },
        )?;
        self.base_collateral_vault_balance = balances.base_collateral_vault_balance;
        self.quote_collateral_vault_balance = balances.quote_collateral_vault_balance;
        let residual_open = execution.position_after.auction_debt_asset != u8::MAX;
        if residual_open {
            let entry = self
                .pending_liquidations
                .get_mut(&pending.position)
                .context("missing pending liquidation after floor")?;
            entry.floor_attempted = true;
        } else {
            self.pending_liquidations.remove(&pending.position);
        }
        let observation = liquidation_observation(
            &pending.source_operation_id,
            &pending.position,
            pending.debt_asset,
            DuskLiquidationPhaseV1::FloorSettlement,
            true,
            true,
            !residual_open,
            residual_open,
            residual_open.then_some(pending.first_floor_unix_timestamp),
            pending.remaining_liquidator_gross,
            Some((
                execution.market.receipt.native,
                collateral_transfer.destination_credit,
                insurance_funding_transfer.destination_credit,
                execution.market.receipt.plan.insurance_draw_debit,
            )),
            execution.market.receipt.native.remaining_debt,
        );
        let mut record = self.record_with_liquidation(
            anchor,
            arm,
            CounterfactualStatusV1::Executed,
            if residual_open {
                vec![CounterfactualConstraintV1::LiquidationAuctionState]
            } else {
                Vec::new()
            },
            execution.market.cash,
            observation,
            true,
        )?;
        self.attach_public_interest_transition(
            public_interest_before,
            public_interest_after_clock,
            &mut record,
        )?;
        Ok(record)
    }

    fn merge_pending_liquidation(&mut self, pending: PendingDuskLiquidationV1) -> Result<()> {
        if let Some(existing) = self.pending_liquidations.get_mut(&pending.position) {
            if existing.debt_asset != pending.debt_asset {
                bail!("pending Dusk liquidation debt side changed");
            }
            existing.remaining_liquidator_gross = existing
                .remaining_liquidator_gross
                .checked_add(pending.remaining_liquidator_gross)
                .context("pending liquidator capital overflow")?;
            existing.source_operation_id = pending.source_operation_id;
            existing.first_floor_unix_timestamp = existing
                .first_floor_unix_timestamp
                .min(pending.first_floor_unix_timestamp);
            existing.floor_attempted = false;
        } else {
            self.pending_liquidations
                .insert(pending.position.clone(), pending);
        }
        Ok(())
    }

    fn transfer_outcome(
        &self,
        asset: MarketAsset,
        slot: u64,
        gross: u64,
    ) -> Result<BenchmarkTokenTransferOutcome> {
        let outcome = self.transfers.transfer(self.mint(asset), slot, gross)?;
        Ok(BenchmarkTokenTransferOutcome {
            source_debit: outcome.gross_amount,
            destination_credit: outcome.net_amount,
        })
    }

    fn liquidation_balances(
        &self,
        debt_asset: MarketAsset,
        liquidator_gross: u64,
    ) -> Result<dusk::benchmark_api::BenchmarkLiquidationTokenBalances> {
        let (base, quote) = if debt_asset == MarketAsset::Base {
            (liquidator_gross, 0)
        } else {
            (0, liquidator_gross)
        };
        self.market
            .liquidation_token_balances_at_custody_floor(
                base,
                quote,
                self.base_collateral_vault_balance,
                self.quote_collateral_vault_balance,
            )
            .map_err(Into::into)
    }

    fn apply_config(
        &mut self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        fields: &BTreeMap<String, String>,
    ) -> Result<CounterfactualOperationRecordV1> {
        let kind = fields
            .get("instruction_kind")
            .context("config operation lacks instruction_kind")?;
        match kind.as_str() {
            "set_pair_reduce_only" => {
                self.market.advance_to(operation_clock(operation))?;
                let reduce_only = required_bool(fields, "args.reduce_only")?;
                self.market.transact(|market, _| {
                    market.reduce_only = reduce_only;
                    Ok(())
                })?;
            }
            "set_global_reduce_only" => {
                self.global_reduce_only = required_bool(fields, "args.reduce_only")?;
            }
            "update_protocol_revenue" => {
                if let Some(value) = optional_u16(fields, "args.swap_bps")? {
                    self.protocol_swap_fee_bps = value;
                }
                if let Some(value) = optional_u16(fields, "args.interest_bps")? {
                    if value != self.factual_v1_interest_bps {
                        bail!("resolved protocol-interest epoch disagrees with V1 config");
                    }
                }
            }
            "init_futarchy_authority"
            | "update_futarchy_authority"
            | "update_revenue_recipients" => {}
            _ => bail!("unsupported Dusk-mapped config operation {kind}"),
        }
        self.record(
            operation,
            arm,
            CounterfactualStatusV1::Executed,
            Vec::new(),
            BenchmarkCashFlow::default(),
            None,
            None,
            false,
        )
    }

    fn ensure_position(&mut self, operation: &CanonicalOperationV1, position: &str) -> Result<()> {
        if self.positions.contains_key(position) {
            return Ok(());
        }
        let owner = actor(operation)?;
        let position_key = Pubkey::from_str(position).context("invalid V1 position public key")?;
        self.positions.insert(
            position.to_owned(),
            BenchmarkBorrowPosition::initialize(owner, self.market_key, position_key, 255),
        );
        Ok(())
    }

    fn mint(&self, asset: MarketAsset) -> &str {
        match asset {
            MarketAsset::Base => &self.token0,
            MarketAsset::Quote => &self.token1,
        }
    }

    fn attach_public_interest_transition(
        &self,
        before_operation: BenchmarkPublicInterestCheckpoint,
        after_clock_before_transition: BenchmarkPublicInterestCheckpoint,
        record: &mut CounterfactualOperationRecordV1,
    ) -> Result<()> {
        if record.debt.is_some() && record.liquidation.is_some() {
            bail!("counterfactual row cannot be both a debt and liquidation transition");
        }
        let mut payments = BenchmarkPublicInterestPayments::default();
        if let Some(debt) = record.debt.as_ref() {
            let payment = BenchmarkPublicInterestPayment {
                gross_cash_interest_paid: amount_u64(
                    "Dusk debt gross cash interest paid",
                    debt.interest_paid_before_transfer_fee.as_str(),
                )?,
                net_interest_vault_credit: amount_u64(
                    "Dusk debt net interest-vault credit",
                    if debt.debt_asset_base {
                        record.cash.base_interest_credit.as_str()
                    } else {
                        record.cash.quote_interest_credit.as_str()
                    },
                )?,
            };
            if debt.debt_asset_base {
                payments.base = payment;
            } else {
                payments.quote = payment;
            }
        } else if let Some(liquidation) = record.liquidation.as_ref() {
            let payment = BenchmarkPublicInterestPayment {
                gross_cash_interest_paid: amount_u64(
                    "Dusk liquidation gross cash interest paid",
                    liquidation.interest_paid.as_str(),
                )?,
                net_interest_vault_credit: amount_u64(
                    "Dusk liquidation net interest-vault credit",
                    if liquidation.debt_asset_base {
                        record.cash.base_interest_credit.as_str()
                    } else {
                        record.cash.quote_interest_credit.as_str()
                    },
                )?,
            };
            if liquidation.debt_asset_base {
                payments.base = payment;
            } else {
                payments.quote = payment;
            }
        }
        let transition = BenchmarkPublicInterestTransition::checked(
            before_operation,
            after_clock_before_transition,
            self.market.public_interest_checkpoint()?,
            payments,
        )?;
        record.public_interest = dusk_public_interest(transition);
        Ok(())
    }

    fn rejected(
        &self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        constraint: CounterfactualConstraintV1,
    ) -> Result<CounterfactualOperationRecordV1> {
        self.record(
            operation,
            arm,
            CounterfactualStatusV1::Rejected,
            vec![constraint],
            BenchmarkCashFlow::default(),
            None,
            None,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_with_liquidation(
        &self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        status: CounterfactualStatusV1,
        limiting_constraints: Vec<CounterfactualConstraintV1>,
        cash: BenchmarkCashFlow,
        liquidation: DuskLiquidationObservationV1,
        synthetic_protocol_settlement: bool,
    ) -> Result<CounterfactualOperationRecordV1> {
        let mut record = self.record(
            operation,
            arm,
            status,
            limiting_constraints,
            cash,
            None,
            None,
            false,
        )?;
        if synthetic_protocol_settlement {
            record.operation_id = format!(
                "{}:dusk-floor:{}:{}",
                liquidation.source_operation_id, operation.order.slot, liquidation.position
            );
            record.synthetic_protocol_settlement = true;
        }
        record.liquidation = Some(liquidation);
        Ok(record)
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        status: CounterfactualStatusV1,
        limiting_constraints: Vec<CounterfactualConstraintV1>,
        cash: BenchmarkCashFlow,
        swap: Option<DuskSwapObservationV1>,
        debt: Option<DuskDebtObservationV1>,
        synthetic_arbitrage: bool,
    ) -> Result<CounterfactualOperationRecordV1> {
        self.record_internal(
            operation,
            arm,
            status,
            limiting_constraints,
            cash,
            swap,
            debt,
            synthetic_arbitrage,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_with_hlp(
        &self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        status: CounterfactualStatusV1,
        limiting_constraints: Vec<CounterfactualConstraintV1>,
        cash: BenchmarkCashFlow,
        swap: Option<DuskSwapObservationV1>,
        debt: Option<DuskDebtObservationV1>,
        synthetic_arbitrage: bool,
        hlp: DuskHlpOperationObservationV1,
    ) -> Result<CounterfactualOperationRecordV1> {
        self.record_internal(
            operation,
            arm,
            status,
            limiting_constraints,
            cash,
            swap,
            debt,
            synthetic_arbitrage,
            Some(hlp),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_internal(
        &self,
        operation: &CanonicalOperationV1,
        arm: ReplayArm,
        status: CounterfactualStatusV1,
        limiting_constraints: Vec<CounterfactualConstraintV1>,
        cash: BenchmarkCashFlow,
        swap: Option<DuskSwapObservationV1>,
        debt: Option<DuskDebtObservationV1>,
        synthetic_arbitrage: bool,
        hlp: Option<DuskHlpOperationObservationV1>,
    ) -> Result<CounterfactualOperationRecordV1> {
        let checkpoint = dusk_checkpoint(&self.market)?;
        let revenue = dusk_revenue(self.market.revenue_checkpoint());
        let public_interest_checkpoint = self.market.public_interest_checkpoint()?;
        let hlp_checkpoint = dusk_hlp_checkpoint(
            &self.market,
            self.owner_ylp_balance,
            self.base_hlp.as_ref(),
            self.quote_hlp.as_ref(),
        )?;
        Ok(CounterfactualOperationRecordV1 {
            schema_version: COUNTERFACTUAL_SCHEMA_VERSION,
            operation_id: operation.operation_id.clone(),
            market: self.market_key.to_string(),
            slot: operation.order.slot,
            arm,
            hlp_scenario: self.hlp_scenario,
            synthetic_arbitrage,
            synthetic_protocol_settlement: false,
            status,
            protocol_swap_fee_bps: self.protocol_swap_fee_bps,
            protocol_interest_fee_bps: self.protocol_interest_fee_bps,
            protocol_interest_schedule_sha256: self.protocol_interest_schedule_sha256.clone(),
            protocol_interest_epoch_id: self.protocol_interest_epoch_id.clone(),
            factual_v1_interest_bps: self.factual_v1_interest_bps,
            protocol_fee_auction_bps: self.protocol_auction_split.fee_auction_bps,
            protocol_buyback_auction_bps: self.protocol_auction_split.buyback_auction_bps,
            limiting_constraints,
            cash: CounterfactualCashFlowV1::from_native(cash),
            public_interest: dusk_public_interest(BenchmarkPublicInterestTransition::identity(
                public_interest_checkpoint,
            )),
            swap,
            spot_probes: None,
            debt,
            liquidation: None,
            hlp,
            hlp_checkpoint,
            checkpoint,
            revenue,
            state_sha256: state_sha256(
                &self.market,
                &self.positions,
                &self.pending_liquidations,
                self.hlp_scenario,
                self.owner_ylp_balance,
                self.base_hlp.as_ref(),
                self.quote_hlp.as_ref(),
                &self.profile_id,
            )?,
        })
    }
}

fn market_config(
    profile: &DuskProfileV1,
    base_fee_bps: u16,
    start_time: i64,
) -> Result<MarketConfig> {
    let mut amm = AmmConfig::default();
    amm.peak_amplification_nad = profile.peak_amplification_nad;
    amm.core_half_width_bps = profile.core_half_width_bps;
    amm.fade_width_bps = profile.fade_width_bps;
    amm.center_ema_half_life_ms = profile.center_ema_half_life_ms;
    amm.adjustment_threshold_nad = profile.adjustment_threshold_nad;
    amm.adjustment_step_nad = profile.adjustment_step_nad;
    amm.min_adjustment_interval_slots = profile.min_adjustment_interval_slots;
    amm.volatility_shock_cap_nad = profile.volatility_shock_cap_nad;
    amm.volatility_cap_nad = profile.volatility_accumulator_cap_nad;
    amm.divergence_fee_coefficient_nad = profile.divergence_fee_coefficient_nad;
    amm.volatility_fee_coefficient_nad = profile.volatility_fee_coefficient_nad;
    amm.volatility_half_life_ms = profile.volatility_half_life_ms;
    amm.swap_fee_collect_mode = SWAP_FEE_COLLECT_INPUT_ASSET;
    amm.compounding_fee_bps = profile.compounding_fee_bps;
    // Launch-only premiums and rate limits remain disabled for historical
    // counterfactual replay.
    amm.launch_fee_start_bps = 0;
    amm.launch_fee_duration_seconds = 0;
    amm.launch_fee_decay_mode = 0;
    amm.launch_market_price_step_bps = 0;
    amm.launch_market_number_of_periods = 0;
    amm.launch_market_reduction_factor_bps = 0;
    amm.launch_rate_limit_asset = 0;
    amm.launch_rate_limit_reference_nad = 0;
    amm.launch_rate_limit_increment_bps = 0;
    amm.launch_rate_limit_max_fee_bps = 0;
    amm.launch_rate_limit_duration_seconds = 0;
    let config = MarketConfig {
        swap_fee_bps: base_fee_bps,
        divergence_fee_share_cap_bps: profile.divergence_fee_share_cap_bps,
        volatility_fee_share_cap_bps: profile.volatility_fee_share_cap_bps,
        target_hlp_leverage_bps: 2 * BPS_DENOMINATOR,
        settlement_divergence_bps: 500,
        ema_half_life_ms: MIN_HALF_LIFE_MS,
        directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
        curve_depth_ema_half_life_ms: MIN_HALF_LIFE_MS,
        max_daily_borrow_bps: DEFAULT_DAILY_BORROW_BPS,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 11_000,
        amm,
        irm: IrmConfig {
            target_utilization_bps: profile.target_utilization_bps,
            curve_steepness_nad: profile.irm_curve_steepness_nad,
            adjustment_speed_per_year: profile.irm_adjustment_speed_per_year,
        },
        start_time,
    };
    config.validate()?;
    Ok(config)
}

fn operation_clock(operation: &CanonicalOperationV1) -> BenchmarkClock {
    BenchmarkClock {
        slot: operation.order.slot,
        unix_timestamp: operation.block_time_unix_seconds,
    }
}

fn actor(operation: &CanonicalOperationV1) -> Result<Pubkey> {
    Pubkey::from_str(
        operation
            .actor
            .as_deref()
            .context("canonical operation has no actor")?,
    )
    .context("canonical actor is not a public key")
}

fn borrow_debt_observation(
    asset: MarketAsset,
    capacity: &BenchmarkExistingBorrowCapacity,
    target: u128,
    after: u128,
    requested: u64,
    filled: u128,
    interest_paid_before_transfer_fee: u64,
) -> DuskDebtObservationV1 {
    DuskDebtObservationV1 {
        debt_asset_base: asset == MarketAsset::Base,
        debt_before: token128(capacity.debt_before),
        gross_admissible_max_debt: Some(token128(capacity.gross_admissible_max_debt)),
        actionable_max_debt: Some(token128(capacity.actionable_max_debt)),
        theoretical_max_debt: token128(capacity.gross_admissible_max_debt),
        underwriting_max_additional: Some(token(capacity.underwriting_max_additional)),
        global_health_floor_max_additional: Some(token(
            capacity.global_health_floor_max_additional,
        )),
        cash_max_additional: Some(token(capacity.cash_max_additional)),
        daily_bucket_max_additional: Some(token(capacity.daily_bucket_max_additional)),
        reduce_only_max_additional: capacity.reduce_only_max_additional.map(token),
        actionable_max_additional: Some(token(capacity.actionable_max_additional)),
        current_underwriting_satisfied: Some(capacity.current_underwriting_satisfied),
        current_global_health_floor_satisfied: Some(capacity.current_global_health_floor_satisfied),
        limiting_constraints: capacity
            .limiting_constraints
            .iter()
            .copied()
            .map(dusk_capacity_constraint)
            .collect(),
        target_debt: token128(target),
        debt_after: token128(after),
        raw_borrow_request: Some(token(requested)),
        requested_change: token128(target.saturating_sub(capacity.debt_before)),
        filled_change: token128(filled),
        unmet_change: token128(target.saturating_sub(after)),
        target_debt_gap: Some(token128(target.saturating_sub(after))),
        interest_paid_before_transfer_fee: token(interest_paid_before_transfer_fee),
    }
}

#[allow(clippy::too_many_arguments)]
fn repay_debt_observation(
    asset: MarketAsset,
    before: u64,
    maximum: u64,
    target: u64,
    after: u64,
    requested: u64,
    filled: u64,
    interest_paid_before_transfer_fee: u64,
) -> DuskDebtObservationV1 {
    DuskDebtObservationV1 {
        debt_asset_base: asset == MarketAsset::Base,
        debt_before: token(before),
        gross_admissible_max_debt: None,
        actionable_max_debt: None,
        theoretical_max_debt: token(maximum),
        underwriting_max_additional: None,
        global_health_floor_max_additional: None,
        cash_max_additional: None,
        daily_bucket_max_additional: None,
        reduce_only_max_additional: None,
        actionable_max_additional: None,
        current_underwriting_satisfied: None,
        current_global_health_floor_satisfied: None,
        limiting_constraints: Vec::new(),
        target_debt: token(target),
        debt_after: token(after),
        raw_borrow_request: None,
        requested_change: token(requested),
        filled_change: token(filled),
        unmet_change: token(requested.saturating_sub(filled)),
        target_debt_gap: None,
        interest_paid_before_transfer_fee: token(interest_paid_before_transfer_fee),
    }
}

const fn dusk_capacity_constraint(
    value: BenchmarkBorrowCapacityConstraint,
) -> DuskBorrowCapacityConstraintV1 {
    match value {
        BenchmarkBorrowCapacityConstraint::Underwriting => {
            DuskBorrowCapacityConstraintV1::Underwriting
        }
        BenchmarkBorrowCapacityConstraint::GlobalHealthFloor => {
            DuskBorrowCapacityConstraintV1::GlobalHealthFloor
        }
        BenchmarkBorrowCapacityConstraint::Cash => DuskBorrowCapacityConstraintV1::Cash,
        BenchmarkBorrowCapacityConstraint::DailyBorrowBucket => {
            DuskBorrowCapacityConstraintV1::DailyBorrowBucket
        }
        BenchmarkBorrowCapacityConstraint::ReduceOnly => DuskBorrowCapacityConstraintV1::ReduceOnly,
    }
}

fn capacity_counterfactual_constraints(
    capacity: &BenchmarkExistingBorrowCapacity,
) -> Vec<CounterfactualConstraintV1> {
    capacity
        .limiting_constraints
        .iter()
        .copied()
        .map(|constraint| match constraint {
            BenchmarkBorrowCapacityConstraint::Underwriting => {
                CounterfactualConstraintV1::PositionHealth
            }
            BenchmarkBorrowCapacityConstraint::GlobalHealthFloor => {
                CounterfactualConstraintV1::GlobalHealth
            }
            BenchmarkBorrowCapacityConstraint::Cash => CounterfactualConstraintV1::Cash,
            BenchmarkBorrowCapacityConstraint::DailyBorrowBucket => {
                CounterfactualConstraintV1::DailyBorrowBucket
            }
            BenchmarkBorrowCapacityConstraint::ReduceOnly => CounterfactualConstraintV1::ReduceOnly,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn liquidation_observation(
    source_operation_id: &str,
    position: &str,
    debt_asset: MarketAsset,
    phase: DuskLiquidationPhaseV1,
    auction_triggered: bool,
    transition_executed: bool,
    settlement_final: bool,
    residual_auction_open: bool,
    first_floor_unix_timestamp: Option<i64>,
    requested_repay_gross: u64,
    receipt: Option<(dusk::market::LiquidationReceipt, u64, u64, u64)>,
    remaining_debt: u128,
) -> DuskLiquidationObservationV1 {
    let (native, collateral_net, insurance_funding_net, insurance_draw_gross) = receipt
        .map(
            |(native, collateral_net, insurance_funding_net, insurance_draw_gross)| {
                (
                    native,
                    collateral_net,
                    insurance_funding_net,
                    insurance_draw_gross,
                )
            },
        )
        .unwrap_or_default();
    DuskLiquidationObservationV1 {
        source_operation_id: source_operation_id.to_owned(),
        position: position.to_owned(),
        debt_asset_base: debt_asset == MarketAsset::Base,
        phase,
        auction_triggered,
        transition_executed,
        settlement_final,
        residual_auction_open,
        first_floor_unix_timestamp,
        requested_repay_gross: token(requested_repay_gross),
        repaid_amount: token(native.repaid_amount),
        interest_paid: token(native.interest_paid),
        collateral_seized: token(native.collateral_seized),
        collateral_to_liquidator_gross: token(native.collateral_to_liquidator),
        collateral_to_liquidator_net: token(collateral_net),
        insurance_funded_gross: token(native.insurance_funded),
        insurance_funded_net: token(insurance_funding_net),
        insurance_drawn_gross: token(insurance_draw_gross),
        insurance_drawn_net: token(native.insurance_drawn),
        socialized_loss: token(native.socialized_loss),
        remaining_debt: token128(remaining_debt),
    }
}

fn position_debt_from_checkpoint(
    checkpoint: &dusk::benchmark_api::BenchmarkBorrowPositionCheckpoint,
    debt_asset: MarketAsset,
) -> u128 {
    match debt_asset {
        MarketAsset::Base => checkpoint.fixed_base_debt,
        MarketAsset::Quote => checkpoint.fixed_quote_debt,
    }
}

fn state_sha256(
    market: &BenchmarkMarket,
    positions: &BTreeMap<String, BenchmarkBorrowPosition>,
    pending_liquidations: &BTreeMap<String, PendingDuskLiquidationV1>,
    hlp_scenario: HlpScenario,
    owner_ylp_balance: u64,
    base_hlp: Option<&BenchmarkHlpOwnedState>,
    quote_hlp: Option<&BenchmarkHlpOwnedState>,
    profile_id: &str,
) -> Result<String> {
    let checkpoint = market.checkpoint()?;
    // A fully withdrawn market is still a valid replay state even when Dusk
    // cannot construct a spot curve from zero liquidity. Keep the state hash
    // available and make curve availability explicit instead of turning a
    // successful withdrawal (or a later rejected intent) into a replay error.
    let curve = market.curve_snapshot().ok();
    let revenue = market.revenue_checkpoint();
    let mut position_rows = Vec::with_capacity(positions.len());
    for (key, position) in positions {
        let value = position.checkpoint(market)?;
        position_rows.push(serde_json::json!({
            "key": key,
            "base_collateral": value.base_collateral.to_string(),
            "quote_collateral": value.quote_collateral.to_string(),
            "fixed_base_shares": value.fixed_base_shares.to_string(),
            "fixed_quote_shares": value.fixed_quote_shares.to_string(),
            "fixed_base_debt": value.fixed_base_debt.to_string(),
            "fixed_quote_debt": value.fixed_quote_debt.to_string(),
            "base_liquidation_cf_bps": value.base_liquidation_cf_bps,
            "quote_liquidation_cf_bps": value.quote_liquidation_cf_bps,
            "auction_debt_asset": value.auction_debt_asset,
            "auction_start_time": value.auction_start_time,
            "auction_start_price_nad": value.auction_start_price_nad.to_string(),
            "auction_floor_price_nad": value.auction_floor_price_nad.to_string(),
        }));
    }
    let pending_rows: Vec<_> = pending_liquidations
        .values()
        .map(|pending| {
            serde_json::json!({
                "source_operation_id": pending.source_operation_id,
                "position": pending.position,
                "debt_asset": pending.debt_asset.code(),
                "remaining_liquidator_gross": pending.remaining_liquidator_gross.to_string(),
                "first_floor_unix_timestamp": pending.first_floor_unix_timestamp,
                "floor_attempted": pending.floor_attempted,
            })
        })
        .collect();
    let hlp_json = |state: Option<&BenchmarkHlpOwnedState>| {
        state.map(|state| {
            let value = state.checkpoint();
            serde_json::json!({
                "target_asset": value.target_asset.code(),
                "owner": value.owner.to_string(),
                "hlp_mint": value.hlp_mint.to_string(),
                "hlp_mint_supply": value.hlp_mint_supply.to_string(),
                "holder_hlp_token_balance": value.holder_hlp_token_balance.to_string(),
                "hlp_vault_ylp_balance": value.hlp_vault_ylp_balance.to_string(),
                "measured_base_interest_vault_credits": value.measured_base_interest_vault_credits.to_string(),
                "measured_quote_interest_vault_credits": value.measured_quote_interest_vault_credits.to_string(),
                "base_yield_account": yield_account_json(&value.base_yield_account),
                "quote_yield_account": yield_account_json(&value.quote_yield_account),
            })
        })
    };
    let market_hlp_json = |value: dusk::benchmark_api::BenchmarkHlpCheckpoint| {
        serde_json::json!({
            "ylp_shares": value.ylp_shares.to_string(),
            "hlp_supply": value.hlp_supply.to_string(),
            "debt_shares": value.debt_shares.to_string(),
            "debt_principal": value.debt_principal.to_string(),
            "base_hlp_live_reserve": value.base_hlp_live_reserve.to_string(),
            "quote_hlp_live_reserve": value.quote_hlp_live_reserve.to_string(),
            "residual_exposure": value.residual_exposure.to_string(),
            "last_nav_nad": value.last_nav_nad.to_string(),
            "cached_settlement_price_nad": value.cached_settlement_price_nad.to_string(),
        })
    };
    let value = serde_json::json!({
        "profile_id": profile_id,
        "hlp_scenario": hlp_scenario,
        "owner_ylp_balance": owner_ylp_balance.to_string(),
        "base_hlp_external": hlp_json(base_hlp),
        "quote_hlp_external": hlp_json(quote_hlp),
        "base_hlp_market": market_hlp_json(checkpoint.base_hlp),
        "quote_hlp_market": market_hlp_json(checkpoint.quote_hlp),
        "clock_slot": checkpoint.clock.slot,
        "clock_unix_timestamp": checkpoint.clock.unix_timestamp,
        "base_live_reserve": checkpoint.base.live_reserve.to_string(),
        "quote_live_reserve": checkpoint.quote.live_reserve.to_string(),
        "base_cash_reserve": checkpoint.base.cash_reserve.to_string(),
        "quote_cash_reserve": checkpoint.quote.cash_reserve.to_string(),
        "base_ylp_supply": checkpoint.base.ylp_supply.to_string(),
        "quote_ylp_supply": checkpoint.quote.ylp_supply.to_string(),
        "base_fixed_debt": checkpoint.base.fixed_debt.to_string(),
        "quote_fixed_debt": checkpoint.quote.fixed_debt.to_string(),
        "base_borrow_index_nad": checkpoint.base.borrow_index_nad.to_string(),
        "quote_borrow_index_nad": checkpoint.quote.borrow_index_nad.to_string(),
        "curve": curve.map(|curve| serde_json::json!({
            "spot_price_nad": curve.spot_price_nad.to_string(),
            "center_price_nad": curve.center_price_nad.to_string(),
            "q_nad": curve.curve_depth_nad.to_string(),
            "volatility_accumulator_nad": curve.volatility_accumulator_nad.to_string(),
        })),
        "revenue": revenue_json(revenue),
        "positions": position_rows,
        "pending_liquidations": pending_rows,
    });
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&value)?)))
}

fn yield_account_json(
    value: &dusk::benchmark_api::BenchmarkYieldAccountCheckpoint,
) -> serde_json::Value {
    serde_json::json!({
        "account_key": value.account_key.to_string(),
        "owner": value.owner.to_string(),
        "market": value.market.to_string(),
        "lp_mint": value.lp_mint.to_string(),
        "asset_mint": value.asset_mint.to_string(),
        "token_kind": value.token_kind,
        "recipient": value.recipient.to_string(),
        "swap_fee_checkpoint_q64": value.swap_fee_checkpoint_q64.to_string(),
        "interest_checkpoint_q64": value.interest_checkpoint_q64.to_string(),
        "accrued_swap_fee_amount": value.accrued_swap_fee_amount.to_string(),
        "accrued_interest_amount": value.accrued_interest_amount.to_string(),
        "swap_fee_remainder_q64": value.swap_fee_remainder_q64.to_string(),
        "interest_remainder_q64": value.interest_remainder_q64.to_string(),
        "bump": value.bump,
    })
}

fn dusk_hlp_checkpoint(
    market: &BenchmarkMarket,
    owner_ylp_balance: u64,
    base_hlp: Option<&BenchmarkHlpOwnedState>,
    quote_hlp: Option<&BenchmarkHlpOwnedState>,
) -> Result<DuskHlpOwnedCheckpointV1> {
    let market = market.checkpoint()?;
    let base = base_hlp.map(BenchmarkHlpOwnedState::checkpoint);
    let quote = quote_hlp.map(BenchmarkHlpOwnedState::checkpoint);
    Ok(DuskHlpOwnedCheckpointV1 {
        owner_ylp_balance: token(owner_ylp_balance),
        base_hlp_balance: token(
            base.as_ref()
                .map(|value| value.holder_hlp_token_balance)
                .unwrap_or(0),
        ),
        quote_hlp_balance: token(
            quote
                .as_ref()
                .map(|value| value.holder_hlp_token_balance)
                .unwrap_or(0),
        ),
        base_hlp_vault_ylp_balance: token(
            base.as_ref()
                .map(|value| value.hlp_vault_ylp_balance)
                .unwrap_or(0),
        ),
        quote_hlp_vault_ylp_balance: token(
            quote
                .as_ref()
                .map(|value| value.hlp_vault_ylp_balance)
                .unwrap_or(0),
        ),
        base_hlp_measured_base_interest_credit: token(
            base.as_ref()
                .map(|value| value.measured_base_interest_vault_credits)
                .unwrap_or(0),
        ),
        base_hlp_measured_quote_interest_credit: token(
            base.as_ref()
                .map(|value| value.measured_quote_interest_vault_credits)
                .unwrap_or(0),
        ),
        quote_hlp_measured_base_interest_credit: token(
            quote
                .as_ref()
                .map(|value| value.measured_base_interest_vault_credits)
                .unwrap_or(0),
        ),
        quote_hlp_measured_quote_interest_credit: token(
            quote
                .as_ref()
                .map(|value| value.measured_quote_interest_vault_credits)
                .unwrap_or(0),
        ),
        base_hlp_ylp_shares: token(market.base_hlp.ylp_shares),
        base_hlp_debt_shares: token128(market.base_hlp.debt_shares),
        base_hlp_debt_principal: token(market.base_hlp.debt_principal),
        base_hlp_live_reserve_base: token(market.base_hlp.base_hlp_live_reserve),
        base_hlp_live_reserve_quote: token(market.base_hlp.quote_hlp_live_reserve),
        base_hlp_residual_exposure: signed_token(market.base_hlp.residual_exposure),
        base_hlp_last_nav_nad: token128(market.base_hlp.last_nav_nad),
        base_hlp_cached_settlement_price_nad: token128(market.base_hlp.cached_settlement_price_nad),
        quote_hlp_ylp_shares: token(market.quote_hlp.ylp_shares),
        quote_hlp_debt_shares: token128(market.quote_hlp.debt_shares),
        quote_hlp_debt_principal: token(market.quote_hlp.debt_principal),
        quote_hlp_live_reserve_base: token(market.quote_hlp.base_hlp_live_reserve),
        quote_hlp_live_reserve_quote: token(market.quote_hlp.quote_hlp_live_reserve),
        quote_hlp_residual_exposure: signed_token(market.quote_hlp.residual_exposure),
        quote_hlp_last_nav_nad: token128(market.quote_hlp.last_nav_nad),
        quote_hlp_cached_settlement_price_nad: token128(
            market.quote_hlp.cached_settlement_price_nad,
        ),
    })
}

fn dusk_checkpoint(market: &BenchmarkMarket) -> Result<DuskMarketCheckpointV1> {
    let checkpoint = market.checkpoint()?;
    let side =
        |value: dusk::benchmark_api::BenchmarkMarketSideCheckpoint| DuskMarketSideCheckpointV1 {
            live_reserve: token(value.live_reserve),
            cash_reserve: token(value.cash_reserve),
            protected_recenter_reserve: token(value.protected_recenter_reserve),
            base_hlp_backing_inventory: token(value.base_hlp_backing_inventory),
            quote_hlp_backing_inventory: token(value.quote_hlp_backing_inventory),
            ylp_supply: token(value.ylp_supply),
            fixed_debt_shares: token128(value.fixed_debt_shares),
            fixed_debt: token128(value.fixed_debt),
            fixed_debt_principal: token(value.fixed_debt_principal),
            isolated_debt_shares: token128(value.isolated_debt_shares),
            isolated_debt: token128(value.isolated_debt),
            isolated_debt_principal: token(value.isolated_debt_principal),
            borrow_index_nad: token128(value.borrow_index_nad),
            rate_at_target_nad: token128(value.rate_at_target_nad),
            daily_borrow_bucket: token(value.daily_borrow_bucket),
        };
    let curve = market
        .curve_snapshot()
        .ok()
        .map(|curve| DuskCurveCheckpointV1 {
            base_curve_reserve: token(curve.base_curve_reserve),
            quote_curve_reserve: token(curve.quote_curve_reserve),
            spot_price_nad: token(curve.spot_price_nad),
            center_price_nad: token(curve.center_price_nad),
            q_nad: token128(curve.curve_depth_nad),
            volatility_accumulator_nad: token(curve.volatility_accumulator_nad),
        });
    Ok(DuskMarketCheckpointV1 {
        clock_slot: checkpoint.clock.slot,
        clock_unix_timestamp: checkpoint.clock.unix_timestamp,
        base: side(checkpoint.base),
        quote: side(checkpoint.quote),
        global_health_base_contribution_for_quote_debt: token(
            checkpoint.global_health_base_contribution_for_quote_debt,
        ),
        global_health_quote_contribution_for_base_debt: token(
            checkpoint.global_health_quote_contribution_for_base_debt,
        ),
        reduce_only: checkpoint.reduce_only,
        curve,
    })
}

fn dusk_revenue(value: BenchmarkRevenueCheckpoint) -> DuskRevenueCheckpointV1 {
    let side =
        |value: dusk::benchmark_api::BenchmarkRevenueSideCheckpoint| DuskRevenueSideCheckpointV1 {
            swap_fee_growth_index_q64: token128(value.swap_fee_growth_index_q64),
            interest_growth_index_q64: token128(value.interest_growth_index_q64),
            lp_swap_fee_liability: token(value.lp_swap_fee_liability),
            lp_interest_liability: token(value.lp_interest_liability),
            unallocated_lp_swap_fee_liability: token(value.unallocated_lp_swap_fee_liability),
            unallocated_lp_interest_liability: token(value.unallocated_lp_interest_liability),
            protocol_swap_fee_liability: token(value.protocol_swap_fee_liability),
            protocol_interest_fee_liability: token(value.protocol_interest_fee_liability),
            buyback_swap_fee_liability: token(value.buyback_swap_fee_liability),
            buyback_interest_fee_liability: token(value.buyback_interest_fee_liability),
            referral_interest_liability: token(value.referral_interest_liability),
            swap_fee_custody_balance: token(value.swap_fee_custody_balance),
            interest_vault_balance: token(value.interest_vault_balance),
        };
    DuskRevenueCheckpointV1 {
        base: side(value.base),
        quote: side(value.quote),
    }
}

fn dusk_public_interest(
    value: BenchmarkPublicInterestTransition,
) -> DuskPublicInterestTransitionV1 {
    let side = |value: dusk::benchmark_api::BenchmarkPublicInterestSideTransition| {
        DuskPublicInterestSideTransitionV1 {
            outstanding_before_operation: token128(value.outstanding_before_operation),
            outstanding_after_clock_before_transition: token128(
                value.outstanding_after_clock_before_transition,
            ),
            outstanding_after_transition: token128(value.outstanding_after_transition),
            clock_accrued: token128(value.clock_accrued),
            transition_interest_created: token128(value.transition_interest_created),
            gross_cash_interest_paid: token128(value.gross_cash_interest_paid),
            net_interest_vault_credit: token128(value.net_interest_vault_credit),
            total_interest_removed: token128(value.total_interest_removed),
            interest_written_off: token128(value.interest_written_off),
        }
    };
    DuskPublicInterestTransitionV1 {
        base: side(value.base),
        quote: side(value.quote),
    }
}

fn revenue_json(revenue: BenchmarkRevenueCheckpoint) -> serde_json::Value {
    serde_json::json!({
        "base": {
            "lp_swap": revenue.base.lp_swap_fee_liability.to_string(),
            "lp_interest": revenue.base.lp_interest_liability.to_string(),
            "protocol_swap": revenue.base.protocol_swap_fee_liability.to_string(),
            "protocol_interest": revenue.base.protocol_interest_fee_liability.to_string(),
            "buyback_swap": revenue.base.buyback_swap_fee_liability.to_string(),
            "buyback_interest": revenue.base.buyback_interest_fee_liability.to_string(),
        },
        "quote": {
            "lp_swap": revenue.quote.lp_swap_fee_liability.to_string(),
            "lp_interest": revenue.quote.lp_interest_liability.to_string(),
            "protocol_swap": revenue.quote.protocol_swap_fee_liability.to_string(),
            "protocol_interest": revenue.quote.protocol_interest_fee_liability.to_string(),
            "buyback_swap": revenue.quote.buyback_swap_fee_liability.to_string(),
            "buyback_interest": revenue.quote.buyback_interest_fee_liability.to_string(),
        }
    })
}

fn synthetic_key(domain: &str, pair: &str, profile_id: &str) -> Pubkey {
    let digest = Sha256::digest(format!("{domain}:{pair}:{profile_id}").as_bytes());
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&digest);
    Pubkey::new_from_array(bytes)
}

fn required_u16(fields: &BTreeMap<String, String>, key: &str) -> Result<u16> {
    fields
        .get(key)
        .with_context(|| format!("missing canonical field {key}"))?
        .parse::<u16>()
        .with_context(|| format!("invalid u16 canonical field {key}"))
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
    fields
        .get(key)
        .with_context(|| format!("missing canonical field {key}"))?
        .parse::<bool>()
        .with_context(|| format!("invalid bool canonical field {key}"))
}

fn amount_u64(field: &str, value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .with_context(|| format!("canonical amount {field} exceeds u64"))
}

fn proportional_floor(value: u64, numerator: u64, denominator: u64) -> Result<u64> {
    if denominator == 0 || numerator > denominator {
        bail!("invalid proportional allocation fraction");
    }
    u64::try_from(
        u128::from(value)
            .checked_mul(u128::from(numerator))
            .context("proportional allocation overflow")?
            / u128::from(denominator),
    )
    .context("proportional allocation exceeds u64")
}

fn proportional_holder_burn(balance: u64, burned_lp: u64, lp_supply: u64) -> Result<u64> {
    if lp_supply == 0 || burned_lp > lp_supply {
        bail!("invalid LP ownership fraction");
    }
    if burned_lp == lp_supply {
        return Ok(balance);
    }
    proportional_floor(balance, burned_lp, lp_supply)
}

fn checked_accumulate_cash(total: &mut BenchmarkCashFlow, delta: BenchmarkCashFlow) -> Result<()> {
    checked_accumulate_asset_cash(&mut total.base, delta.base)?;
    checked_accumulate_asset_cash(&mut total.quote, delta.quote)
}

fn checked_accumulate_asset_cash(
    total: &mut BenchmarkAssetCashFlow,
    delta: BenchmarkAssetCashFlow,
) -> Result<()> {
    macro_rules! add_field {
        ($field:ident) => {
            total.$field = total.$field.checked_add(delta.$field).context(concat!(
                "counterfactual cash overflow in ",
                stringify!($field)
            ))?;
        };
    }
    add_field!(reserve_vault_credit);
    add_field!(reserve_vault_debit);
    add_field!(collateral_vault_credit);
    add_field!(collateral_vault_debit);
    add_field!(interest_vault_credit);
    add_field!(insurance_vault_credit);
    add_field!(insurance_vault_debit);
    add_field!(recipient_credit);
    Ok(())
}

fn token(value: u64) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u64 is a canonical token amount")
}

fn token128(value: u128) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u128 is a canonical token amount")
}

fn signed_token(value: i128) -> SignedTokenAmount {
    SignedTokenAmount::new(value.to_string()).expect("i128 is a canonical signed token amount")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canonical::ChainOrder,
        spot_probe::{
            SpotProbeDirectionV1, SpotProbePlanEntryV1, SpotProbeSizeKindV1, V1SpotPreStateV1,
        },
        transfer_fee::{
            EpochScheduleV1, MintTransferFeeScheduleV1, TokenProgramKindV1, TransferFeeEpochV1,
            TransferFeeRegistryV1,
        },
    };

    fn order(slot: u64, outer_instruction_index: u32) -> ChainOrder {
        ChainOrder {
            slot,
            transaction_index: 0,
            outer_instruction_index,
            inner_execution_index: None,
            event_sequence: 0,
        }
    }

    fn amount(value: u64) -> TokenAmount {
        token(value)
    }

    #[test]
    fn factual_behavior_fraction_saturates_historical_over_capacity() {
        assert_eq!(
            CapacityFractionV1::from_factual_ratio(101, 100).unwrap(),
            CapacityFractionV1 {
                numerator: 100,
                denominator: 100,
            }
        );
        assert_eq!(
            CapacityFractionV1::from_factual_ratio(7, 0).unwrap(),
            CapacityFractionV1 {
                numerator: 1,
                denominator: 1,
            }
        );
        assert_eq!(
            CapacityFractionV1::from_factual_ratio(0, 0).unwrap(),
            CapacityFractionV1 {
                numerator: 0,
                denominator: 1,
            }
        );
    }

    #[test]
    fn only_sub_share_repayment_is_a_typed_counterfactual_rejection() {
        let too_small =
            dusk::state::Debt::repayment_for_max(1, 1, u128::from(dusk::constants::NAD) * 2, 1)
                .unwrap_err();
        assert!(is_unrepresentable_debt_share_repayment(&too_small));

        let invalid_index =
            dusk::state::Debt::repayment_for_max(1, 1, u128::from(dusk::constants::NAD) - 1, 1)
                .unwrap_err();
        assert!(!is_unrepresentable_debt_share_repayment(&invalid_index));
    }

    #[test]
    #[ignore = "requires explicit sealed benchmark artifact paths"]
    fn diagnose_live_maximum_hlp_initialization() {
        use std::io::BufRead;

        let operations_path = std::env::var("DUSK_DIAG_OPERATIONS").unwrap();
        let fees_path = std::env::var("DUSK_DIAG_FEES").unwrap();
        let operations = std::io::BufReader::new(std::fs::File::open(operations_path).unwrap());
        let operations = operations
            .lines()
            .map(|line| serde_json::from_str::<CanonicalOperationV1>(&line.unwrap()).unwrap())
            .filter(|operation| {
                matches!(
                    operation.payload,
                    OperationPayloadV1::InitializeMarket { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            operations.len(),
            5,
            "expected five selected-market initializations"
        );
        let registry: TransferFeeRegistryV1 =
            serde_json::from_reader(std::fs::File::open(fees_path).unwrap()).unwrap();
        let registry = registry.validate().unwrap();
        let profiles = crate::calibrate::explore_profiles(
            32,
            20_260_820,
            crate::calibrate::SearchDomainV1::default(),
        );
        let mut executable = Vec::new();
        for profile in profiles {
            let mut rejected = None;
            for operation in &operations {
                if let Err(error) = DuskCounterfactualMarketV1::initialize_from_v1_with_hlp(
                    operation,
                    &profile,
                    1_000,
                    1_500,
                    &registry,
                    ReplayArm::FixedFlow,
                    HlpScenario::MaximumFeasible,
                ) {
                    rejected = Some((operation.pair.clone(), error));
                    break;
                }
            }
            match rejected {
                None => executable.push(profile.profile_id.clone()),
                Some((market, error)) => {
                    eprintln!("{} rejected by {market}: {error:#}", profile.profile_id)
                }
            }
        }
        assert!(
            !executable.is_empty(),
            "no sampled profile can initialize all selected markets"
        );
        eprintln!("profiles executable across all five markets: {executable:?}");
    }

    fn parse_amount(value: &TokenAmount) -> u64 {
        value.as_str().parse().unwrap()
    }

    fn fee_epoch(bps: u16, maximum_fee: u64) -> TransferFeeEpochV1 {
        TransferFeeEpochV1 {
            effective_epoch: 0,
            transfer_fee_basis_points: bps,
            maximum_fee: amount(maximum_fee),
            source_signature: "genesis-or-mint-initialize".to_owned(),
            source_slot: 0,
            decoded_instruction_sha256: "b".repeat(64),
        }
    }

    fn registry(
        token0: Pubkey,
        token1: Pubkey,
        token1_fee_bps: u16,
    ) -> ValidatedTransferFeeRegistryV1 {
        let schedule = |mint: Pubkey, bps: u16| MintTransferFeeScheduleV1 {
            schema_version: 1,
            mint: mint.to_string(),
            token_program: if bps == 0 {
                TokenProgramKindV1::LegacyToken
            } else {
                TokenProgramKindV1::Token2022
            },
            epochs: vec![fee_epoch(bps, if bps == 0 { 0 } else { 10_000 })],
        };
        TransferFeeRegistryV1 {
            schema_version: 1,
            epoch_schedule: EpochScheduleV1 {
                slots_per_epoch: 432_000,
                leader_schedule_slot_offset: 432_000,
                warmup: false,
                first_normal_epoch: 0,
                first_normal_slot: 0,
                source_response_sha256: "a".repeat(64),
            },
            mints: vec![schedule(token0, 0), schedule(token1, token1_fee_bps)],
        }
        .validate()
        .unwrap()
    }

    fn initialize_operation(
        pair: Pubkey,
        token0: Pubkey,
        token1: Pubkey,
        token1_credit: u64,
    ) -> CanonicalOperationV1 {
        CanonicalOperationV1 {
            schema_version: 1,
            operation_id: "initialize".to_owned(),
            signature: "initialize-signature".to_owned(),
            pair: pair.to_string(),
            actor: Some(Pubkey::new_from_array([9; 32]).to_string()),
            order: order(1, 0),
            block_time_unix_seconds: 1_700_000_000,
            decoder_version_id: "verified-v1".to_owned(),
            payload: OperationPayloadV1::InitializeMarket {
                token0: token0.to_string(),
                token1: token1.to_string(),
                token0_decimals: 6,
                token1_decimals: 6,
                gross_amount0: amount(1_000_000),
                gross_amount1: amount(1_000_000),
                vault_credit0: amount(1_000_000),
                vault_credit1: amount(token1_credit),
                minted_lp: amount(998_000),
                min_liquidity_out: amount(0),
                fields: BTreeMap::from([("swap_fee_bps".to_owned(), "30".to_owned())]),
            },
            assertions: Vec::new(),
        }
    }

    fn swap_operation(pair: Pubkey, slot: u64, min_out: u64) -> CanonicalOperationV1 {
        CanonicalOperationV1 {
            schema_version: 1,
            operation_id: format!("swap-{slot}"),
            signature: format!("swap-signature-{slot}"),
            pair: pair.to_string(),
            actor: Some(Pubkey::new_from_array([8; 32]).to_string()),
            order: order(slot, 0),
            block_time_unix_seconds: 1_700_000_000 + i64::try_from(slot).unwrap(),
            decoder_version_id: "verified-v1".to_owned(),
            payload: OperationPayloadV1::SwapExactIn {
                token0_in: true,
                gross_amount_in: amount(10_000),
                vault_credit_in: amount(10_000),
                min_amount_out: amount(min_out),
                vault_debit_out: amount(0),
                recipient_credit_out: amount(0),
            },
            assertions: Vec::new(),
        }
    }

    fn protocol_revenue_update_operation(
        pair: Pubkey,
        slot: u64,
        interest_bps: u16,
    ) -> CanonicalOperationV1 {
        CanonicalOperationV1 {
            schema_version: 1,
            operation_id: format!("protocol-revenue-{slot}"),
            signature: format!("protocol-revenue-signature-{slot}"),
            pair: pair.to_string(),
            actor: Some(Pubkey::new_from_array([6; 32]).to_string()),
            order: order(slot, 0),
            block_time_unix_seconds: 1_700_000_000 + i64::try_from(slot).unwrap(),
            decoder_version_id: "verified-v1".to_owned(),
            payload: OperationPayloadV1::UpdateConfig {
                fields: BTreeMap::from([
                    (
                        "instruction_kind".to_owned(),
                        "update_protocol_revenue".to_owned(),
                    ),
                    ("args.interest_bps".to_owned(), interest_bps.to_string()),
                ]),
            },
            assertions: Vec::new(),
        }
    }

    fn add_liquidity_operation(pair: Pubkey, slot: u64, minted_lp: u64) -> CanonicalOperationV1 {
        CanonicalOperationV1 {
            schema_version: 1,
            operation_id: format!("add-{slot}"),
            signature: format!("add-signature-{slot}"),
            pair: pair.to_string(),
            actor: Some(Pubkey::new_from_array([7; 32]).to_string()),
            order: order(slot, 0),
            block_time_unix_seconds: 1_700_000_000 + i64::try_from(slot).unwrap(),
            decoder_version_id: "verified-v1".to_owned(),
            payload: OperationPayloadV1::AddLiquidity {
                gross_amount0: amount(100_000),
                gross_amount1: amount(100_000),
                vault_credit0: amount(100_000),
                vault_credit1: amount(100_000),
                minted_lp: amount(minted_lp),
                min_liquidity_out: amount(0),
            },
            assertions: Vec::new(),
        }
    }

    fn remove_liquidity_operation(pair: Pubkey, slot: u64, burned_lp: u64) -> CanonicalOperationV1 {
        CanonicalOperationV1 {
            schema_version: 1,
            operation_id: format!("remove-{slot}"),
            signature: format!("remove-signature-{slot}"),
            pair: pair.to_string(),
            actor: Some(Pubkey::new_from_array([7; 32]).to_string()),
            order: order(slot, 0),
            block_time_unix_seconds: 1_700_000_000 + i64::try_from(slot).unwrap(),
            decoder_version_id: "verified-v1".to_owned(),
            payload: OperationPayloadV1::RemoveLiquidity {
                requested_lp: amount(burned_lp),
                burned_lp: amount(burned_lp),
                vault_debit0: amount(0),
                vault_debit1: amount(0),
                recipient_credit0: amount(0),
                recipient_credit1: amount(0),
                min_amount0_out: amount(0),
                min_amount1_out: amount(0),
            },
            assertions: Vec::new(),
        }
    }

    fn liquidation_operation(
        pair: Pubkey,
        position: Pubkey,
        slot: u64,
        requested_repay: u64,
    ) -> CanonicalOperationV1 {
        CanonicalOperationV1 {
            schema_version: 1,
            operation_id: format!("liquidate-{slot}"),
            signature: format!("liquidate-signature-{slot}"),
            pair: pair.to_string(),
            actor: Some(Pubkey::new_from_array([6; 32]).to_string()),
            order: order(slot, 0),
            block_time_unix_seconds: 1_700_000_000 + i64::try_from(slot).unwrap(),
            decoder_version_id: "verified-v1".to_owned(),
            payload: OperationPayloadV1::Liquidate {
                position: position.to_string(),
                debt_token0: true,
                requested_repay: amount(requested_repay),
                collateral_vault_debit: amount(0),
                reserve_vault_credit: amount(0),
                liquidator_recipient_credit: amount(0),
            },
            assertions: Vec::new(),
        }
    }

    fn profile() -> DuskProfileV1 {
        let amm = AmmConfig::default();
        let irm = IrmConfig::default();
        let mut profile = DuskProfileV1 {
            schema_version: 1,
            profile_id: String::new(),
            peak_amplification_nad: amm.peak_amplification_nad,
            core_half_width_bps: amm.core_half_width_bps,
            fade_width_bps: amm.fade_width_bps,
            center_ema_half_life_ms: amm.center_ema_half_life_ms,
            adjustment_threshold_nad: amm.adjustment_threshold_nad,
            adjustment_step_nad: amm.adjustment_step_nad,
            min_adjustment_interval_slots: amm.min_adjustment_interval_slots,
            divergence_fee_share_cap_bps: 0,
            volatility_fee_share_cap_bps: 0,
            divergence_fee_coefficient_nad: amm.divergence_fee_coefficient_nad,
            volatility_fee_coefficient_nad: amm.volatility_fee_coefficient_nad,
            volatility_half_life_ms: amm.volatility_half_life_ms,
            volatility_shock_cap_nad: amm.volatility_shock_cap_nad,
            volatility_accumulator_cap_nad: amm.volatility_cap_nad,
            compounding_fee_bps: amm.compounding_fee_bps,
            target_utilization_bps: irm.target_utilization_bps,
            irm_curve_steepness_nad: irm.curve_steepness_nad,
            irm_adjustment_speed_per_year: irm.adjustment_speed_per_year,
        };
        profile.recompute_id();
        profile
    }

    fn spot_probe_plan(
        pair: Pubkey,
        token0: Pubkey,
        token1: Pubkey,
        gross_input: u64,
    ) -> SpotProbePlanV1 {
        let mut entries = Vec::new();
        for direction in [
            SpotProbeDirectionV1::Token0In,
            SpotProbeDirectionV1::Token1In,
        ] {
            for size_kind in [
                SpotProbeSizeKindV1::FactualNav10Bps,
                SpotProbeSizeKindV1::FactualNav100Bps,
                SpotProbeSizeKindV1::FactualNav500Bps,
                SpotProbeSizeKindV1::FactualGrossInputP50,
                SpotProbeSizeKindV1::FactualGrossInputP90,
            ] {
                entries.push(SpotProbePlanEntryV1 {
                    size_kind,
                    direction,
                    factual_nav_bps: size_kind.factual_nav_bps(),
                    gross_input: Some(gross_input),
                    size_unsupported_reason: None,
                    v1: None,
                });
            }
        }
        SpotProbePlanV1 {
            factual: V1SpotPreStateV1 {
                market: pair.to_string(),
                token0: token0.to_string(),
                token1: token1.to_string(),
                token0_decimals: 6,
                token1_decimals: 6,
                reserve0: 1_000_000,
                reserve1: 1_000_000,
                cash_reserve0: 1_000_000,
                cash_reserve1: 1_000_000,
                swap_fee_bps: 30,
                protocol_swap_fee_bps: 2_000,
            },
            factual_reference_price_nad: dusk::constants::NAD,
            factual_record_sha256: "c".repeat(64),
            factual_pre_state_sha256: "d".repeat(64),
            sizing_window: crate::spot_probe::SpotProbeSizingWindowV1 {
                start_unix_seconds: 0,
                end_unix_seconds_exclusive: i64::MAX,
                standardized_trade_nav_bps: vec![10, 100, 500],
            },
            sizing_source_sha256: "e".repeat(64),
            entries,
        }
    }

    #[test]
    fn intent_min_out_is_atomic_but_fixed_flow_executes() {
        let pair = Pubkey::new_from_array([1; 32]);
        let token0 = Pubkey::new_from_array([2; 32]);
        let token1 = Pubkey::new_from_array([3; 32]);
        let transfers = registry(token0, token1, 0);
        let initialization = initialize_operation(pair, token0, token1, 1_000_000);
        let (mut intent, initialized) = DuskCounterfactualMarketV1::initialize_from_v1(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::Intent,
        )
        .unwrap();
        let impossible = swap_operation(pair, 2, u64::MAX);
        let skipped = intent
            .apply(
                &impossible,
                ReplayArm::Intent,
                BehavioralContextV1::default(),
            )
            .unwrap();
        assert_eq!(skipped.status, CounterfactualStatusV1::SkippedIntentMinOut);
        assert_eq!(skipped.state_sha256, initialized.state_sha256);

        let (mut fixed, initialized_fixed) = DuskCounterfactualMarketV1::initialize_from_v1(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::FixedFlow,
        )
        .unwrap();
        let executed = fixed
            .apply(
                &impossible,
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
            )
            .unwrap();
        assert_eq!(executed.status, CounterfactualStatusV1::Executed);
        assert!(!executed.swap.unwrap().original_min_out_passed);
        assert_ne!(executed.state_sha256, initialized_fixed.state_sha256);

        let lower = dusk::constants::NAD * 99 / 100;
        let upper = dusk::constants::NAD * 101 / 100;
        let arbitrage = fixed
            .arbitrage_to_price_band(&impossible, lower, upper)
            .unwrap()
            .expect("the user swap moved the market outside the V1 band");
        assert!(arbitrage.synthetic_arbitrage);
        assert_eq!(arbitrage.status, CounterfactualStatusV1::Executed);
        let observation = arbitrage.swap.unwrap();
        assert!(!observation.asset_in_base);
        assert!(
            (lower..=upper).contains(&observation.end_price_nad.as_str().parse::<u64>().unwrap())
        );
    }

    #[test]
    fn resolved_interest_epoch_update_retains_dusk_share_and_rejects_reuse() {
        let pair = Pubkey::new_from_array([71; 32]);
        let token0 = Pubkey::new_from_array([72; 32]);
        let token1 = Pubkey::new_from_array([73; 32]);
        let transfers = registry(token0, token1, 0);
        let initialization = initialize_operation(pair, token0, token1, 1_000_000);
        let initial_policy = ResolvedProtocolInterestPolicyV1 {
            schedule_sha256: "a".repeat(64),
            epoch_id: "initial-epoch".to_owned(),
            factual_v1_interest_bps: 1_000,
            dusk_protocol_interest_share_bps: 909,
        };
        let (mut replay, _) =
            DuskCounterfactualMarketV1::initialize_from_v1_with_resolved_interest_policy(
                &initialization,
                &profile(),
                2_000,
                &initial_policy,
                &transfers,
                ReplayArm::FixedFlow,
                HlpScenario::NoHlp,
            )
            .unwrap();
        let update = protocol_revenue_update_operation(pair, 2, 1_000);
        assert!(replay
            .apply_with_resolved_interest_policy(
                &update,
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
                &initial_policy,
            )
            .is_err());

        let next_policy = ResolvedProtocolInterestPolicyV1 {
            schedule_sha256: initial_policy.schedule_sha256.clone(),
            epoch_id: hex::encode(Sha256::digest(
                format!("{}:ProtocolRevenue:__global__", update.operation_id).as_bytes(),
            )),
            factual_v1_interest_bps: 1_000,
            dusk_protocol_interest_share_bps: 909,
        };
        let record = replay
            .apply_with_resolved_interest_policy(
                &update,
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
                &next_policy,
            )
            .unwrap();
        assert_eq!(record.protocol_interest_fee_bps, 909);
        assert_eq!(record.factual_v1_interest_bps, 1_000);
        assert_eq!(record.protocol_interest_epoch_id, next_policy.epoch_id);
    }

    #[test]
    fn counterfactual_output_uses_exact_token_2022_schedule() {
        let pair = Pubkey::new_from_array([4; 32]);
        let token0 = Pubkey::new_from_array([5; 32]);
        let token1 = Pubkey::new_from_array([6; 32]);
        let transfers = registry(token0, token1, 25);
        let initialization = initialize_operation(pair, token0, token1, 997_500);
        let (mut replay, _) = DuskCounterfactualMarketV1::initialize_from_v1(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::FixedFlow,
        )
        .unwrap();
        let record = replay
            .apply(
                &swap_operation(pair, 2, 0),
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
            )
            .unwrap();
        let swap = record.swap.unwrap();
        let gross = swap.amount_out_before_token_fee.as_str().parse().unwrap();
        let expected = transfers.transfer(&token1.to_string(), 2, gross).unwrap();
        assert_eq!(
            swap.recipient_credit_out.as_str(),
            expected.net_amount.to_string()
        );
        assert!(expected.fee_amount > 0);
    }

    #[test]
    fn spot_probes_cover_both_directions_token_2022_and_never_mutate_same_slot_state() {
        let pair = Pubkey::new_from_array([51; 32]);
        let token0 = Pubkey::new_from_array([52; 32]);
        let token1 = Pubkey::new_from_array([53; 32]);
        let transfers = registry(token0, token1, 25);
        let initialization = initialize_operation(pair, token0, token1, 997_500);
        let (replay, initialized) = DuskCounterfactualMarketV1::initialize_from_v1(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::FixedFlow,
        )
        .unwrap();
        let operation = swap_operation(pair, 2, 0);
        let plan = spot_probe_plan(pair, token0, token1, 10_000);
        let before = state_sha256(
            &replay.market,
            &replay.positions,
            &replay.pending_liquidations,
            replay.hlp_scenario,
            replay.owner_ylp_balance,
            replay.base_hlp.as_ref(),
            replay.quote_hlp.as_ref(),
            &replay.profile_id,
        )
        .unwrap();
        assert_eq!(before, initialized.state_sha256);
        let probes = replay
            .preview_spot_probes(&operation, ReplayArm::FixedFlow, &plan)
            .unwrap();
        let after = state_sha256(
            &replay.market,
            &replay.positions,
            &replay.pending_liquidations,
            replay.hlp_scenario,
            replay.owner_ylp_balance,
            replay.base_hlp.as_ref(),
            replay.quote_hlp.as_ref(),
            &replay.profile_id,
        )
        .unwrap();
        assert_eq!(before, after);
        assert_eq!(probes.observations.len(), 10);
        probes.validate_hash().unwrap();
        for direction in [
            SpotProbeDirectionV1::Token0In,
            SpotProbeDirectionV1::Token1In,
        ] {
            let quotes: Vec<_> = probes
                .observations
                .iter()
                .filter(|probe| probe.direction == direction)
                .map(|probe| probe.dusk.as_ref().unwrap())
                .collect();
            assert_eq!(quotes.len(), 5);
            assert!(quotes.iter().all(|quote| *quote == quotes[0]));
            if direction == SpotProbeDirectionV1::Token0In {
                assert!(parse_amount(&quotes[0].output_transfer_fee) > 0);
                assert!(
                    parse_amount(&quotes[0].gross_output) > parse_amount(&quotes[0].net_output)
                );
            } else {
                assert!(parse_amount(&quotes[0].input_transfer_fee) > 0);
                assert!(parse_amount(&quotes[0].gross_input) > parse_amount(&quotes[0].net_input));
            }
        }
    }

    #[test]
    fn maximum_hlp_spot_probe_is_settlement_aware_and_late_failure_is_atomic() {
        let pair = Pubkey::new_from_array([54; 32]);
        let token0 = Pubkey::new_from_array([55; 32]);
        let token1 = Pubkey::new_from_array([56; 32]);
        let transfers = registry(token0, token1, 25);
        let initialization = initialize_operation(pair, token0, token1, 997_500);
        let (mut replay, _) = DuskCounterfactualMarketV1::initialize_from_v1_with_hlp(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::FixedFlow,
            HlpScenario::MaximumFeasible,
        )
        .unwrap();
        let operation = swap_operation(pair, 1_000_000, 0);
        let plan = spot_probe_plan(pair, token0, token1, 10_000);
        let before = state_sha256(
            &replay.market,
            &replay.positions,
            &replay.pending_liquidations,
            replay.hlp_scenario,
            replay.owner_ylp_balance,
            replay.base_hlp.as_ref(),
            replay.quote_hlp.as_ref(),
            &replay.profile_id,
        )
        .unwrap();
        let valid = replay
            .preview_spot_probes(&operation, ReplayArm::FixedFlow, &plan)
            .unwrap();
        assert!(valid
            .observations
            .iter()
            .filter_map(|observation| observation.dusk.as_ref())
            .any(|quote| quote.executable));
        assert_eq!(
            before,
            state_sha256(
                &replay.market,
                &replay.positions,
                &replay.pending_liquidations,
                replay.hlp_scenario,
                replay.owner_ylp_balance,
                replay.base_hlp.as_ref(),
                replay.quote_hlp.as_ref(),
                &replay.profile_id,
            )
            .unwrap()
        );

        // The state-only native preview does not consume this interest split;
        // hLP settlement does. Making it invalid forces a post-preview failure
        // and proves both owned hLP accounts and Market remain uncommitted.
        replay.protocol_interest_fee_bps = BPS_DENOMINATOR + 1;
        let rejected = replay
            .preview_spot_probes(&operation, ReplayArm::FixedFlow, &plan)
            .unwrap();
        assert!(rejected
            .observations
            .iter()
            .filter_map(|observation| observation.dusk.as_ref())
            .any(|quote| {
                quote.unsupported_reason
                    == Some(SpotProbeUnsupportedReasonV1::HlpSettlementRejected)
            }));
        assert_eq!(
            before,
            state_sha256(
                &replay.market,
                &replay.positions,
                &replay.pending_liquidations,
                replay.hlp_scenario,
                replay.owner_ylp_balance,
                replay.base_hlp.as_ref(),
                replay.quote_hlp.as_ref(),
                &replay.profile_id,
            )
            .unwrap()
        );
    }

    #[test]
    fn rejected_v1_deposit_keeps_factual_supply_and_blocks_free_withdrawal() {
        let pair = Pubkey::new_from_array([10; 32]);
        let token0 = Pubkey::new_from_array([11; 32]);
        let token1 = Pubkey::new_from_array([12; 32]);
        let transfers = registry(token0, token1, 0);
        let initialization = initialize_operation(pair, token0, token1, 1_000_000);
        let (mut replay, _) = DuskCounterfactualMarketV1::initialize_from_v1(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::FixedFlow,
        )
        .unwrap();
        let factual_supply_before = replay.v1_lp_supply;
        replay.global_reduce_only = true;
        let rejected = replay
            .apply(
                &add_liquidity_operation(pair, 2, 100_000),
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
            )
            .unwrap();
        assert_eq!(rejected.status, CounterfactualStatusV1::Rejected);
        assert_eq!(replay.v1_lp_supply, factual_supply_before + 100_000);

        replay.global_reduce_only = false;
        let withdrawal = replay
            .apply(
                &remove_liquidity_operation(pair, 3, 50_000),
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
            )
            .unwrap();
        assert_eq!(
            withdrawal.status,
            CounterfactualStatusV1::DeferredExternalState
        );
        assert_eq!(
            withdrawal.limiting_constraints,
            vec![CounterfactualConstraintV1::LpOwnershipProvenance]
        );
        assert_eq!(replay.v1_lp_supply, factual_supply_before + 50_000);
    }

    #[test]
    fn maximum_hlp_uses_only_lp_cash_and_exhausts_holder_state_pro_rata() {
        let pair = Pubkey::new_from_array([20; 32]);
        let token0 = Pubkey::new_from_array([21; 32]);
        let token1 = Pubkey::new_from_array([22; 32]);
        let transfers = registry(token0, token1, 0);
        let initialization = initialize_operation(pair, token0, token1, 1_000_000);
        let (mut replay, initialized) = DuskCounterfactualMarketV1::initialize_from_v1_with_hlp(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::FixedFlow,
            HlpScenario::MaximumFeasible,
        )
        .unwrap();
        let initial_hlp = initialized.hlp.as_ref().unwrap();
        assert!(initial_hlp.no_extra_capital_verified);
        assert_eq!(
            parse_amount(&initial_hlp.ylp_gross_base) + parse_amount(&initial_hlp.base_hlp_gross),
            1_000_000
        );
        assert_eq!(
            parse_amount(&initial_hlp.ylp_gross_quote) + parse_amount(&initial_hlp.quote_hlp_gross),
            1_000_000
        );
        assert!(
            parse_amount(&initial_hlp.base_hlp_minted) > 0
                || parse_amount(&initial_hlp.quote_hlp_minted) > 0
        );
        assert!(parse_amount(&initialized.hlp_checkpoint.owner_ylp_balance) > 0);

        let added = replay
            .apply(
                &add_liquidity_operation(pair, 2, 100_000),
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
            )
            .unwrap();
        assert_eq!(added.hlp_scenario, HlpScenario::MaximumFeasible);
        assert!(added.hlp.as_ref().unwrap().no_extra_capital_verified);

        let swapped = replay
            .apply(
                &swap_operation(pair, 1_000_000, 0),
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
            )
            .unwrap();
        assert!(swapped.hlp.is_some());
        let swap_hlp = swapped.hlp.as_ref().unwrap();
        assert!(
            parse_amount(&swap_hlp.base_hlp_interest_credit_quote) > 0
                || parse_amount(&swap_hlp.quote_hlp_interest_credit_base) > 0
        );
        assert_ne!(swapped.state_sha256, added.state_sha256);

        let entire_factual_supply = replay.v1_lp_supply;
        let removed = replay
            .apply(
                &remove_liquidity_operation(pair, 1_000_001, entire_factual_supply),
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
            )
            .unwrap();
        let withdrawal = removed.hlp.as_ref().unwrap();
        assert!(parse_amount(&withdrawal.ylp_burned) > 0);
        assert_eq!(removed.hlp_checkpoint.owner_ylp_balance.as_str(), "0");
        assert_eq!(removed.hlp_checkpoint.base_hlp_balance.as_str(), "0");
        assert_eq!(removed.hlp_checkpoint.quote_hlp_balance.as_str(), "0");
    }

    #[test]
    fn maximum_hlp_split_recomputes_token_2022_outcomes_without_extra_gross() {
        let pair = Pubkey::new_from_array([23; 32]);
        let token0 = Pubkey::new_from_array([24; 32]);
        let token1 = Pubkey::new_from_array([25; 32]);
        let transfers = registry(token0, token1, 25);
        let initialization = initialize_operation(pair, token0, token1, 997_500);
        let (_, initialized) = DuskCounterfactualMarketV1::initialize_from_v1_with_hlp(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::FixedFlow,
            HlpScenario::MaximumFeasible,
        )
        .unwrap();
        let hlp = initialized.hlp.as_ref().unwrap();
        assert!(hlp.no_extra_capital_verified);
        assert_eq!(
            parse_amount(&hlp.ylp_gross_quote) + parse_amount(&hlp.quote_hlp_gross),
            1_000_000
        );
        let ylp = transfers
            .transfer(&token1.to_string(), 1, parse_amount(&hlp.ylp_gross_quote))
            .unwrap();
        let single = transfers
            .transfer(&token1.to_string(), 1, 1_000_000)
            .unwrap();
        assert_eq!(
            hlp.ylp_reserve_credit_quote.as_str(),
            ylp.net_amount.to_string()
        );
        assert_eq!(single.net_amount, 997_500);
    }

    #[test]
    fn maximum_hlp_late_deposit_failure_rolls_back_all_owned_state() {
        let pair = Pubkey::new_from_array([26; 32]);
        let token0 = Pubkey::new_from_array([27; 32]);
        let token1 = Pubkey::new_from_array([28; 32]);
        let transfers = registry(token0, token1, 0);
        let initialization = initialize_operation(pair, token0, token1, 1_000_000);
        let (mut replay, initialized) = DuskCounterfactualMarketV1::initialize_from_v1_with_hlp(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::FixedFlow,
            HlpScenario::MaximumFeasible,
        )
        .unwrap();
        replay.global_reduce_only = true;
        let rejected = replay
            .apply(
                &add_liquidity_operation(pair, 2, 100_000),
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
            )
            .unwrap();
        assert_eq!(rejected.status, CounterfactualStatusV1::Rejected);
        assert_eq!(rejected.state_sha256, initialized.state_sha256);
        assert_eq!(rejected.hlp_checkpoint, initialized.hlp_checkpoint);
    }

    #[test]
    fn borrow_observation_does_not_alias_raw_request_to_share_rounded_debt_change() {
        let capacity = BenchmarkExistingBorrowCapacity {
            debt_asset: MarketAsset::Base,
            debt_before: 100,
            gross_admissible_max_debt: 200,
            actionable_max_debt: 150,
            underwriting_max_additional: 100,
            global_health_floor_max_additional: 50,
            cash_max_additional: 75,
            daily_bucket_max_additional: 80,
            reduce_only_max_additional: None,
            actionable_max_additional: 50,
            current_underwriting_satisfied: true,
            current_global_health_floor_satisfied: true,
            market_reduce_only: false,
            global_reduce_only: false,
            limiting_constraints: vec![BenchmarkBorrowCapacityConstraint::GlobalHealthFloor],
        };
        let observation = borrow_debt_observation(MarketAsset::Base, &capacity, 102, 102, 1, 2, 0);
        assert_eq!(observation.raw_borrow_request.unwrap().as_str(), "1");
        assert_eq!(observation.requested_change.as_str(), "2");
        assert_eq!(observation.filled_change.as_str(), "2");
        assert_eq!(observation.unmet_change.as_str(), "0");
        assert_eq!(observation.target_debt_gap.unwrap().as_str(), "0");
    }

    #[test]
    fn liquidation_policy_carries_auction_to_exact_synthetic_floor() {
        let pair = Pubkey::new_from_array([13; 32]);
        let token0 = Pubkey::new_from_array([14; 32]);
        let token1 = Pubkey::new_from_array([15; 32]);
        let transfers = registry(token0, token1, 25);
        let initialization = initialize_operation(pair, token0, token1, 997_500);
        let (mut replay, _) = DuskCounterfactualMarketV1::initialize_from_v1(
            &initialization,
            &profile(),
            2_000,
            1_000,
            &transfers,
            ReplayArm::FixedFlow,
        )
        .unwrap();
        let position_key = Pubkey::new_from_array([16; 32]);
        let mut position = BenchmarkBorrowPosition::initialize(
            Pubkey::new_from_array([17; 32]),
            pair,
            position_key,
            250,
        );
        replay
            .market
            .execute_deposit_collateral(
                &mut position,
                BenchmarkDepositCollateralRequest {
                    clock: BenchmarkClock {
                        slot: 2,
                        unix_timestamp: 1_700_000_002,
                    },
                    collateral_asset: MarketAsset::Quote,
                    collateral_credit: 300_000,
                },
            )
            .unwrap();
        replay.quote_collateral_vault_balance = 300_000;
        let capacity = replay
            .market
            .preview_borrow_capacity(MarketAsset::Quote, 300_000, None)
            .unwrap()
            .max_borrow_amount;
        replay
            .market
            .execute_borrow(
                &mut position,
                BenchmarkBorrowRequest {
                    clock: BenchmarkClock {
                        slot: 3,
                        unix_timestamp: 1_700_000_003,
                    },
                    debt_asset: MarketAsset::Base,
                    borrow_amount: capacity / 2,
                    recipient_credit: capacity / 2,
                    min_recipient_credit: 0,
                    min_liquidation_cf_bps: 0,
                    global_reduce_only: false,
                    referral_partner: Pubkey::default(),
                    referral_interest_share_bps: 0,
                    referral_interest_share_cap_bps: 0,
                },
            )
            .unwrap();
        let mut raw_position = position.into_position();
        raw_position.quote_collateral = 1_000;
        replay.positions.insert(
            position_key.to_string(),
            BenchmarkBorrowPosition::from_position_state(raw_position).unwrap(),
        );

        let liquidation = liquidation_operation(pair, position_key, 4, 1);
        let immediate = replay
            .apply(
                &liquidation,
                ReplayArm::FixedFlow,
                BehavioralContextV1::default(),
            )
            .unwrap();
        let immediate_observation = immediate.liquidation.unwrap();
        assert!(immediate_observation.auction_triggered);
        assert!(immediate_observation.transition_executed);
        assert!(immediate_observation.residual_auction_open);
        assert_eq!(replay.pending_liquidation_count(), 1);

        let mut anchor = swap_operation(pair, 1_000, 0);
        anchor.payload = OperationPayloadV1::ClockOnly;
        let settlements = replay
            .settle_due_liquidations(&anchor, ReplayArm::FixedFlow)
            .unwrap();
        assert_eq!(settlements.len(), 1);
        let settlement = &settlements[0];
        assert!(settlement.synthetic_protocol_settlement);
        let observation = settlement.liquidation.as_ref().unwrap();
        assert_eq!(observation.phase, DuskLiquidationPhaseV1::FloorSettlement);
        assert!(observation.transition_executed);
        assert!(observation.settlement_final);
        assert!(observation.socialized_loss.as_str().parse::<u64>().unwrap() > 0);
        assert_eq!(observation.collateral_to_liquidator_gross.as_str(), "4");
        assert!(
            observation
                .collateral_to_liquidator_net
                .as_str()
                .parse::<u64>()
                .unwrap()
                > 0
        );
        assert_eq!(replay.pending_liquidation_count(), 0);
    }
}

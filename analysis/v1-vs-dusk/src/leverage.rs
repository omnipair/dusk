//! Exact isolated-leverage comparison at one paired market checkpoint.
//!
//! Dusk is executed through the feature-only native benchmark wrapper. V1 is
//! executed as the sequence its product actually requires: margin swap,
//! collateral deposit, then borrow -> swap -> deposit loops. Every operation
//! is staged on a fork, every clock is caller supplied, and every token CPI is
//! resolved through the decoded historical transfer-fee registry.

use dusk::{
    benchmark_api::{
        BenchmarkClock, BenchmarkExecuteLeverageCloseRequest, BenchmarkExecuteLeverageOpenRequest,
        BenchmarkHlpOwnedState, BenchmarkLeverageOwnedState, BenchmarkLeveragePolicy,
        BenchmarkLeverageTokenBalances, BenchmarkMarket, BenchmarkPrepareLeverageCloseRequest,
        BenchmarkPrepareLeverageOpenRequest, BenchmarkTokenTransferOutcome,
    },
    constants::{
        BPS_DENOMINATOR as DUSK_BPS_DENOMINATOR, LEVERAGE_INITIAL_MARGIN_BPS,
        LEVERAGE_MAINTENANCE_BUFFER_BPS, LEVERAGE_MAX_MULTIPLIER_BPS,
    },
    math::interest::{instantaneous_rate_apr_nad, utilization_error_nad},
    state::MarketAsset,
};
use omnipair::{
    ceil_div,
    constants::{BPS_DENOMINATOR as V1_BPS_DENOMINATOR, NAD as V1_NAD, POSITION_SEED_PREFIX},
    state::UserPosition,
    CPCurve,
};
use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::{
    canonical::{IntegerStringError, SignedTokenAmount, TokenAmount},
    replay::HlpScenario,
    transfer_fee::{TransferFeeError, TransferOutcomeV1, ValidatedTransferFeeRegistryV1},
    v1_engine::{
        V1BorrowInputV1, V1CollateralInputV1, V1EngineError, V1NativeEngine, V1RepayInputV1,
        V1SwapInputV1, V1TokenSideV1,
    },
};

pub const LEVERAGE_BENCHMARK_SCHEMA_VERSION: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeverageDirectionV1 {
    /// Acquire token0/base while debt is token1/quote.
    LongBase,
    /// Acquire token1/quote while debt is token0/base.
    LongQuote,
}

impl LeverageDirectionV1 {
    const fn dusk_debt(self) -> MarketAsset {
        match self {
            Self::LongBase => MarketAsset::Quote,
            Self::LongQuote => MarketAsset::Base,
        }
    }

    const fn v1_debt(self) -> V1TokenSideV1 {
        match self {
            Self::LongBase => V1TokenSideV1::Token1,
            Self::LongQuote => V1TokenSideV1::Token0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V1LoopClockV1 {
    pub borrow_slot: u64,
    pub swap_slot: u64,
    pub deposit_slot: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V1UnwindClockV1 {
    pub withdraw_slot: u64,
    pub swap_slot: u64,
    pub repay_slot: u64,
}

/// A standardized path mark. `collateral_price_nad` is debt atoms per one
/// collateral atom, NAD-scaled. Dusk's concentrated unwind threshold is not a
/// linear oracle price, so its exact closeout value at the corresponding
/// counterfactual checkpoint is mandatory too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdversePriceMarkV1 {
    pub slot: u64,
    pub collateral_price_nad: u64,
    /// Exact native Dusk closeout value at the corresponding future
    /// counterfactual state. A factual V1 price observation is still usable
    /// when this is absent, but Dusk path-breach claims are then unavailable.
    pub dusk_closeout_value: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcedRealizedMaeV1 {
    pub adverse_excursion_bps: u64,
    pub source_sha256: String,
}

/// Exact caller-owned state. The benchmark creates no reserve balance,
/// position clock, mint extension result, or adverse-path closeout value.
pub struct LeverageBenchmarkInputV1<'a> {
    pub benchmark_id: String,
    pub direction: LeverageDirectionV1,
    pub owner: Pubkey,
    pub position_id: Pubkey,
    pub starting_margin_gross: u64,
    pub dusk_multiplier_bps: u64,
    pub dusk_open_clock: BenchmarkClock,
    pub dusk_close_clock: BenchmarkClock,
    pub dusk_token_balances: BenchmarkLeverageTokenBalances,
    pub dusk_policy: BenchmarkLeveragePolicy,
    pub hlp_scenario: HlpScenario,
    /// Maximum-feasible replay must provide the actual counterfactual holder
    /// accounts. `None` is accepted only for the no-hLP control.
    pub dusk_base_hlp: Option<&'a BenchmarkHlpOwnedState>,
    pub dusk_quote_hlp: Option<&'a BenchmarkHlpOwnedState>,
    /// Fingerprint of the mint-account decode used by the native collateral
    /// risk-mint eligibility decision.
    pub collateral_mint_decode_sha256: String,
    pub v1_initial_swap_slot: u64,
    pub v1_initial_deposit_slot: u64,
    pub v1_loop_clocks: &'a [V1LoopClockV1],
    pub v1_max_loop_iterations: usize,
    pub v1_unwind_clocks: &'a [V1UnwindClockV1],
    pub v1_max_unwind_iterations: usize,
    pub adverse_path: &'a [AdversePriceMarkV1],
    pub realized_mae: Option<SourcedRealizedMaeV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactRatioV1 {
    pub numerator: TokenAmount,
    pub denominator: TokenAmount,
    /// A zero denominator is emitted only with this flag, for a genuinely
    /// unbounded protocol formula; it is never replaced by a large sentinel.
    pub unbounded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpeningBenchmarkV1 {
    pub starting_margin_gross: TokenAmount,
    pub starting_margin_net: TokenAmount,
    pub borrowed_amount: TokenAmount,
    pub deployed_notional: TokenAmount,
    pub collateral_acquired: TokenAmount,
    pub transfer_fees_paid: TokenAmount,
    pub immediate_closeout_value: TokenAmount,
    pub opening_cost: SignedTokenAmount,
    pub limiting_borrow_liquidity: TokenAmount,
    pub loop_iterations: TokenAmount,
    pub loop_iteration_limit_reached: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundingAprBenchmarkV1 {
    pub utilization_scale: String,
    pub utilization: TokenAmount,
    pub target_utilization_bps: Option<TokenAmount>,
    pub rate_at_target_nad: Option<TokenAmount>,
    pub instantaneous_borrow_apr_nad: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiquidationThresholdKindV1 {
    /// Exact minimum native closeout value in debt atoms.
    CloseoutValue,
    /// Exact V1 collateral price in debt-per-collateral NAD.
    CollateralPriceNad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdversePathAvailabilityV1 {
    Complete,
    /// Replaying a synthetic isolated position through all subsequent Dusk
    /// operations requires a separately owned future-state branch. Factual
    /// V1 prices must never be substituted for native Dusk closeout values.
    MissingExactDuskFutureCounterfactualState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiquidationBenchmarkV1 {
    pub threshold_kind: LiquidationThresholdKindV1,
    pub threshold: TokenAmount,
    pub initial_margin_bps: TokenAmount,
    pub maintenance_margin_bps: TokenAmount,
    pub equity_bps: TokenAmount,
    pub initial_buffer_bps: SignedTokenAmount,
    pub maintenance_buffer_bps: SignedTokenAmount,
    pub adverse_path_availability: AdversePathAvailabilityV1,
    pub first_adverse_path_breach: Option<TokenAmount>,
    pub standardized_max_adverse_excursion_bps: TokenAmount,
    pub realized_max_adverse_excursion_bps: Option<TokenAmount>,
    pub realized_mae_source_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnwindLimitV1 {
    None,
    IterationLimit,
    CollateralVaultLiquidity,
    HealthRounding,
    InsufficientSwapProceeds,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnwindBenchmarkV1 {
    pub executable_full_unwind: bool,
    pub limiting_reason: UnwindLimitV1,
    pub collateral_sold: TokenAmount,
    pub gross_debt_output: TokenAmount,
    pub terminal_owner_debt_balance: TokenAmount,
    pub remaining_debt: TokenAmount,
    pub remaining_collateral: TokenAmount,
    pub transfer_fees_paid: TokenAmount,
    pub unwind_impact_bps: TokenAmount,
    pub cost_vs_starting_margin: SignedTokenAmount,
    pub iterations: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VenueLeverageBenchmarkV1 {
    pub theoretical_leverage: ExactRatioV1,
    pub executable_leverage: ExactRatioV1,
    pub opening: OpeningBenchmarkV1,
    pub funding: FundingAprBenchmarkV1,
    pub liquidation: LiquidationBenchmarkV1,
    pub unwind: UnwindBenchmarkV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeverageBenchmarkResultV1 {
    pub schema_version: u16,
    pub benchmark_id: String,
    pub direction: LeverageDirectionV1,
    pub hlp_scenario: HlpScenario,
    pub dusk: VenueLeverageBenchmarkV1,
    pub v1: VenueLeverageBenchmarkV1,
}

#[derive(Debug, Error)]
pub enum LeverageBenchmarkError {
    #[error("invalid leverage benchmark input: {0}")]
    InvalidInput(&'static str),
    #[error("Dusk and V1 mint identities or direction do not match")]
    MarketIdentityMismatch,
    #[error("missing exact V1 {stage} clock at loop index {index}")]
    MissingClock { stage: &'static str, index: usize },
    #[error("collateral mint is not eligible for native Dusk isolated leverage")]
    UnsupportedDuskCollateralMint,
    #[error("native Dusk transition failed: {0}")]
    DuskNative(String),
    #[error("native V1 transition failed: {0}")]
    V1(#[from] V1EngineError),
    #[error("historical transfer-fee resolution failed: {0}")]
    TransferFee(#[from] TransferFeeError),
    #[error("integer-string conversion failed")]
    IntegerString,
    #[error("leverage benchmark arithmetic overflow during {0}")]
    Arithmetic(&'static str),
}

impl From<IntegerStringError> for LeverageBenchmarkError {
    fn from(_: IntegerStringError) -> Self {
        Self::IntegerString
    }
}

#[derive(Clone, Copy)]
struct V1SwapQuote {
    amount_out: u64,
    spot_value: u64,
}

#[derive(Default)]
struct V1Ledger {
    owner_debt: u64,
    owner_collateral: u64,
    borrowed: u64,
    deployed_notional: u64,
    collateral_acquired: u64,
    transfer_fees: u64,
    gross_debt_output: u64,
    collateral_sold: u64,
    unwind_spot_value: u128,
}

/// Runs both venues on independent forks. Neither caller-owned checkpoint is
/// mutated on success or failure.
pub fn benchmark_leverage_exact(
    dusk_market: &BenchmarkMarket,
    v1_engine: &V1NativeEngine,
    transfer_fees: &ValidatedTransferFeeRegistryV1,
    input: LeverageBenchmarkInputV1<'_>,
) -> Result<LeverageBenchmarkResultV1, LeverageBenchmarkError> {
    validate_input(dusk_market, v1_engine, &input)?;
    let dusk = run_dusk(dusk_market, transfer_fees, &input)?;
    let v1 = run_v1(v1_engine, transfer_fees, &input)?;
    Ok(LeverageBenchmarkResultV1 {
        schema_version: LEVERAGE_BENCHMARK_SCHEMA_VERSION,
        benchmark_id: input.benchmark_id,
        direction: input.direction,
        hlp_scenario: input.hlp_scenario,
        dusk,
        v1,
    })
}

fn validate_input(
    dusk_market: &BenchmarkMarket,
    v1_engine: &V1NativeEngine,
    input: &LeverageBenchmarkInputV1<'_>,
) -> Result<(), LeverageBenchmarkError> {
    if input.benchmark_id.trim().is_empty()
        || input.starting_margin_gross == 0
        || input.dusk_multiplier_bps <= u64::from(DUSK_BPS_DENOMINATOR)
        || input.adverse_path.is_empty()
        || !valid_sha256(&input.collateral_mint_decode_sha256)
    {
        return Err(LeverageBenchmarkError::InvalidInput(
            "identity, margin, multiplier, path, or mint fingerprint",
        ));
    }
    if !input.dusk_policy.collateral_mint_is_leverage_eligible {
        return Err(LeverageBenchmarkError::UnsupportedDuskCollateralMint);
    }
    if input.v1_max_loop_iterations > input.v1_loop_clocks.len() {
        return Err(LeverageBenchmarkError::MissingClock {
            stage: "open",
            index: input.v1_loop_clocks.len(),
        });
    }
    if input.v1_max_unwind_iterations > input.v1_unwind_clocks.len() {
        return Err(LeverageBenchmarkError::MissingClock {
            stage: "unwind",
            index: input.v1_unwind_clocks.len(),
        });
    }
    match (
        input.hlp_scenario,
        input.dusk_base_hlp,
        input.dusk_quote_hlp,
    ) {
        (HlpScenario::NoHlp, None, None) | (HlpScenario::MaximumFeasible, Some(_), Some(_)) => {}
        _ => {
            return Err(LeverageBenchmarkError::InvalidInput(
                "hLP scenario and owned states",
            ));
        }
    }
    let v1_pair = v1_engine.pair();
    let dusk = dusk_market.market();
    if dusk.base_side.asset_mint != v1_pair.token0
        || dusk.quote_side.asset_mint != v1_pair.token1
        || input.dusk_open_clock.slot < dusk_market.clock().slot
        || input.dusk_close_clock.slot < input.dusk_open_clock.slot
    {
        return Err(LeverageBenchmarkError::MarketIdentityMismatch);
    }
    let mut last_slot = 0;
    for mark in input.adverse_path {
        if mark.collateral_price_nad == 0 || mark.slot < last_slot {
            return Err(LeverageBenchmarkError::InvalidInput("adverse price path"));
        }
        last_slot = mark.slot;
    }
    if input.adverse_path[0].slot < input.dusk_open_clock.slot {
        return Err(LeverageBenchmarkError::InvalidInput(
            "adverse path starts before the benchmark checkpoint",
        ));
    }
    if let Some(realized) = &input.realized_mae {
        if !valid_sha256(&realized.source_sha256) {
            return Err(LeverageBenchmarkError::InvalidInput("realized MAE source"));
        }
    }
    Ok(())
}

fn run_dusk(
    source: &BenchmarkMarket,
    fees: &ValidatedTransferFeeRegistryV1,
    input: &LeverageBenchmarkInputV1<'_>,
) -> Result<VenueLeverageBenchmarkV1, LeverageBenchmarkError> {
    let mut market = source
        .try_fork()
        .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?;
    let debt_asset = input.direction.dusk_debt();
    let collateral_asset = debt_asset.opposite();
    let debt_mint = market.market().side(debt_asset).asset_mint.to_string();
    let collateral_mint = market
        .market()
        .side(collateral_asset)
        .asset_mint
        .to_string();
    let margin = fees.transfer(
        &debt_mint,
        input.dusk_open_clock.slot,
        input.starting_margin_gross,
    )?;
    if margin.net_amount == 0 {
        return Err(LeverageBenchmarkError::InvalidInput("zero post-fee margin"));
    }
    let mut leverage = BenchmarkLeverageOwnedState::initialize_for_open(
        &market,
        input.owner,
        input.owner,
        input.position_id,
        input.dusk_token_balances,
        None,
    )
    .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?;
    let (mut base_hlp, mut quote_hlp) = match (
        input.hlp_scenario,
        input.dusk_base_hlp,
        input.dusk_quote_hlp,
    ) {
        (HlpScenario::NoHlp, None, None) => (
            BenchmarkHlpOwnedState::initialize(
                &market,
                MarketAsset::Base,
                input.owner,
                input.owner,
            )
            .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?,
            BenchmarkHlpOwnedState::initialize(
                &market,
                MarketAsset::Quote,
                input.owner,
                input.owner,
            )
            .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?,
        ),
        (HlpScenario::MaximumFeasible, Some(base), Some(quote)) => (
            base.try_fork()
                .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?,
            quote
                .try_fork()
                .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?,
        ),
        _ => {
            return Err(LeverageBenchmarkError::InvalidInput(
                "hLP scenario and owned states",
            ));
        }
    };
    let cash_before = market.market().side(debt_asset).reserves.cash_reserve;
    let prepared = market
        .prepare_leverage_open(
            &leverage,
            &base_hlp,
            &quote_hlp,
            BenchmarkPrepareLeverageOpenRequest {
                clock: input.dusk_open_clock,
                debt_asset,
                margin_transfer: token_outcome(margin),
                multiplier_bps: input.dusk_multiplier_bps,
                min_collateral_out: 1,
                limit_price_nad: 0,
                requested_referrer: None,
                policy: input.dusk_policy,
            },
        )
        .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?;
    let open_quote = prepared.quote();
    let collateral = fee_free_transfer(
        fees,
        &collateral_mint,
        input.dusk_open_clock.slot,
        open_quote.swap.amount_out,
    )?;
    let base_hlp_interest = fees.transfer(
        &market.market().quote_side.asset_mint.to_string(),
        input.dusk_open_clock.slot,
        open_quote.settlement.base_hlp_interest_debit,
    )?;
    let quote_hlp_interest = fees.transfer(
        &market.market().base_side.asset_mint.to_string(),
        input.dusk_open_clock.slot,
        open_quote.settlement.quote_hlp_interest_debit,
    )?;
    let open = market
        .execute_prepared_leverage_open(
            &mut leverage,
            &mut base_hlp,
            &mut quote_hlp,
            prepared,
            BenchmarkExecuteLeverageOpenRequest {
                collateral_transfer: token_outcome(collateral),
                base_hlp_interest_transfer: token_outcome(base_hlp_interest),
                quote_hlp_interest_transfer: token_outcome(quote_hlp_interest),
            },
        )
        .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?;
    let metrics = open.market.receipt.metrics;
    let funding = dusk_funding(&market, debt_asset)?;
    let opened_position = leverage
        .position()
        .ok_or(LeverageBenchmarkError::InvalidInput(
            "missing opened Dusk position",
        ))?;
    let opened_principal = opened_position.debt_principal;
    let collateral_amount = opened_position.collateral_amount;
    let prepared_close = market
        .prepare_leverage_close(
            &leverage,
            &base_hlp,
            &quote_hlp,
            BenchmarkPrepareLeverageCloseRequest {
                clock: input.dusk_close_clock,
                collateral_transfer: token_outcome(fee_free_transfer(
                    fees,
                    &collateral_mint,
                    input.dusk_close_clock.slot,
                    collateral_amount,
                )?),
                min_residual_out: 0,
                policy: input.dusk_policy,
            },
        )
        .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?;
    let close_quote = prepared_close.quote();
    let residual = fees.transfer(
        &debt_mint,
        input.dusk_close_clock.slot,
        close_quote.expected_gross_residual,
    )?;
    let position_interest = fees.transfer(
        &debt_mint,
        input.dusk_close_clock.slot,
        close_quote.settlement.position_interest_debit,
    )?;
    let close_base_hlp_interest = fees.transfer(
        &market.market().quote_side.asset_mint.to_string(),
        input.dusk_close_clock.slot,
        close_quote.settlement.base_hlp_interest_debit,
    )?;
    let close_quote_hlp_interest = fees.transfer(
        &market.market().base_side.asset_mint.to_string(),
        input.dusk_close_clock.slot,
        close_quote.settlement.quote_hlp_interest_debit,
    )?;
    let close = market
        .execute_prepared_leverage_close(
            &mut leverage,
            &mut base_hlp,
            &mut quote_hlp,
            prepared_close,
            BenchmarkExecuteLeverageCloseRequest {
                residual_transfer: token_outcome(residual),
                interest_transfer: token_outcome(position_interest),
                base_hlp_interest_transfer: token_outcome(close_base_hlp_interest),
                quote_hlp_interest_transfer: token_outcome(close_quote_hlp_interest),
            },
        )
        .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?;
    let native_close = close.market.receipt.native;
    if native_close.interest_paid != close_quote.settlement.position_interest_debit
        || opened_principal > u128::from(close_quote.debt_amount)
    {
        return Err(LeverageBenchmarkError::Arithmetic(
            "Dusk interest reconciliation",
        ));
    }
    let open_transfer_fees = sum_u64(
        [
            margin.fee_amount,
            collateral.fee_amount,
            base_hlp_interest.fee_amount,
            quote_hlp_interest.fee_amount,
        ],
        "Dusk open transfer fees",
    )?;
    let close_transfer_fees = sum_u64(
        [
            residual.fee_amount,
            position_interest.fee_amount,
            close_base_hlp_interest.fee_amount,
            close_quote_hlp_interest.fee_amount,
        ],
        "Dusk close transfer fees",
    )?;
    let theoretical = ratio(
        u128::from(LEVERAGE_MAX_MULTIPLIER_BPS),
        u128::from(DUSK_BPS_DENOMINATOR),
    )?;
    let executable = ratio(
        u128::from(open_quote.notional),
        u128::from(margin.net_amount),
    )?;
    let equity_bps = metrics.equity_bps;
    let realized = realized_mae(input)?;
    let threshold = metrics.minimum_healthy_closeout_value;
    let adverse_path_availability = if input
        .adverse_path
        .iter()
        .all(|mark| mark.dusk_closeout_value.is_some())
    {
        AdversePathAvailabilityV1::Complete
    } else {
        AdversePathAvailabilityV1::MissingExactDuskFutureCounterfactualState
    };
    let first_breach = if adverse_path_availability == AdversePathAvailabilityV1::Complete {
        input
            .adverse_path
            .iter()
            .position(|mark| {
                mark.dusk_closeout_value
                    .is_some_and(|value| value < threshold)
            })
            .map(amount_usize)
            .transpose()?
    } else {
        None
    };
    let unwind_impact_bps = close_quote.metrics_before.unwind_impact_bps;
    let cost = i128::from(margin.net_amount) - i128::from(residual.net_amount);
    Ok(VenueLeverageBenchmarkV1 {
        theoretical_leverage: theoretical,
        executable_leverage: executable,
        opening: OpeningBenchmarkV1 {
            starting_margin_gross: amount(input.starting_margin_gross)?,
            starting_margin_net: amount(margin.net_amount)?,
            borrowed_amount: amount(open_quote.borrowed_amount)?,
            deployed_notional: amount(open_quote.notional)?,
            collateral_acquired: amount(open_quote.swap.amount_out)?,
            transfer_fees_paid: amount(open_transfer_fees)?,
            immediate_closeout_value: amount(metrics.closeout_value)?,
            opening_cost: signed(
                i128::from(open_quote.notional) - i128::from(metrics.closeout_value),
            )?,
            limiting_borrow_liquidity: amount(cash_before)?,
            loop_iterations: amount(1u64)?,
            loop_iteration_limit_reached: false,
        },
        funding,
        liquidation: LiquidationBenchmarkV1 {
            threshold_kind: LiquidationThresholdKindV1::CloseoutValue,
            threshold: amount(threshold)?,
            initial_margin_bps: amount(LEVERAGE_INITIAL_MARGIN_BPS)?,
            maintenance_margin_bps: amount(LEVERAGE_MAINTENANCE_BUFFER_BPS)?,
            equity_bps: amount_u128(equity_bps)?,
            initial_buffer_bps: signed(
                equity_bps as i128 - i128::from(LEVERAGE_INITIAL_MARGIN_BPS),
            )?,
            maintenance_buffer_bps: signed(
                equity_bps as i128 - i128::from(LEVERAGE_MAINTENANCE_BUFFER_BPS),
            )?,
            adverse_path_availability,
            first_adverse_path_breach: first_breach,
            standardized_max_adverse_excursion_bps: amount(standardized_mae(input.adverse_path)?)?,
            realized_max_adverse_excursion_bps: realized.as_ref().map(|value| value.0.clone()),
            realized_mae_source_sha256: realized.map(|value| value.1),
        },
        unwind: UnwindBenchmarkV1 {
            executable_full_unwind: native_close.remaining_debt_amount == 0
                && native_close.remaining_collateral_amount == 0,
            limiting_reason: UnwindLimitV1::None,
            collateral_sold: amount(native_close.collateral_sold)?,
            gross_debt_output: amount(native_close.closeout_value)?,
            terminal_owner_debt_balance: amount(residual.net_amount)?,
            remaining_debt: amount(native_close.remaining_debt_amount)?,
            remaining_collateral: amount(native_close.remaining_collateral_amount)?,
            transfer_fees_paid: amount(close_transfer_fees)?,
            unwind_impact_bps: amount_u128(unwind_impact_bps)?,
            cost_vs_starting_margin: signed(cost)?,
            iterations: amount(1u64)?,
        },
    })
}

fn run_v1(
    source: &V1NativeEngine,
    fees: &ValidatedTransferFeeRegistryV1,
    input: &LeverageBenchmarkInputV1<'_>,
) -> Result<VenueLeverageBenchmarkV1, LeverageBenchmarkError> {
    let mut engine = source.try_fork();
    let debt_side = input.direction.v1_debt();
    let collateral_side = debt_side.opposite();
    let debt_mint = mint_for_v1(engine.pair(), debt_side).to_string();
    let collateral_mint = mint_for_v1(engine.pair(), collateral_side).to_string();
    let position_key = insert_synthetic_position(&mut engine, input.owner)?;
    let margin = fees.transfer(
        &debt_mint,
        input.v1_initial_swap_slot,
        input.starting_margin_gross,
    )?;
    if margin.net_amount == 0 {
        return Err(LeverageBenchmarkError::InvalidInput(
            "zero post-fee V1 margin",
        ));
    }
    let mut ledger = V1Ledger {
        owner_debt: input.starting_margin_gross,
        ..V1Ledger::default()
    };
    apply_v1_swap(
        &mut engine,
        fees,
        input.v1_initial_swap_slot,
        debt_side,
        input.starting_margin_gross,
        &debt_mint,
        &collateral_mint,
        true,
        &mut ledger,
    )?;
    deposit_all_v1_collateral(
        &mut engine,
        fees,
        input.v1_initial_deposit_slot,
        position_key,
        collateral_side,
        &collateral_mint,
        &mut ledger,
    )?;
    let initial_capacity = engine.borrow_capacity(position_key, debt_side)?;
    let theoretical_denominator = V1_BPS_DENOMINATOR
        .checked_sub(initial_capacity.max_cf_bps)
        .ok_or(LeverageBenchmarkError::InvalidInput(
            "V1 max CF exceeds 100%",
        ))?;
    let target_notional = u64::try_from(
        u128::from(margin.net_amount)
            .checked_mul(u128::from(input.dusk_multiplier_bps))
            .and_then(|value| value.checked_div(u128::from(V1_BPS_DENOMINATOR)))
            .ok_or(LeverageBenchmarkError::Arithmetic("V1 target notional"))?,
    )
    .map_err(|_| LeverageBenchmarkError::Arithmetic("V1 target notional cast"))?
    .max(input.starting_margin_gross);
    let mut loops = 0usize;
    let mut iteration_limit_reached = false;
    for index in 0..input.v1_max_loop_iterations {
        let clock =
            input
                .v1_loop_clocks
                .get(index)
                .ok_or(LeverageBenchmarkError::MissingClock {
                    stage: "open",
                    index,
                })?;
        let capacity = engine.borrow_capacity(position_key, debt_side)?;
        let remaining_target = target_notional.saturating_sub(ledger.deployed_notional);
        if remaining_target == 0 {
            break;
        }
        let borrow_amount = capacity.actionable_remaining_capacity.min(remaining_target);
        if borrow_amount == 0 {
            break;
        }
        let borrow_transfer = fees.transfer(&debt_mint, clock.borrow_slot, borrow_amount)?;
        engine.apply_borrow(V1BorrowInputV1 {
            slot: clock.borrow_slot,
            position: position_key,
            debt_token: debt_side,
            requested_amount: borrow_amount,
            recipient_credit: borrow_transfer.net_amount,
        })?;
        ledger.borrowed = checked_add(ledger.borrowed, borrow_amount, "V1 borrowed")?;
        ledger.owner_debt = checked_add(
            ledger.owner_debt,
            borrow_transfer.net_amount,
            "V1 owner debt balance",
        )?;
        ledger.transfer_fees = checked_add(
            ledger.transfer_fees,
            borrow_transfer.fee_amount,
            "V1 transfer fees",
        )?;
        apply_v1_swap(
            &mut engine,
            fees,
            clock.swap_slot,
            debt_side,
            borrow_transfer.net_amount,
            &debt_mint,
            &collateral_mint,
            true,
            &mut ledger,
        )?;
        deposit_all_v1_collateral(
            &mut engine,
            fees,
            clock.deposit_slot,
            position_key,
            collateral_side,
            &collateral_mint,
            &mut ledger,
        )?;
        loops += 1;
    }
    let remaining_capacity = engine.borrow_capacity(position_key, debt_side)?;
    if loops == input.v1_max_loop_iterations
        && ledger.deployed_notional < target_notional
        && remaining_capacity.actionable_remaining_capacity > 0
    {
        iteration_limit_reached = true;
    }
    let open_pair = engine.pair();
    let open_position = engine
        .position(&position_key)
        .ok_or(V1EngineError::MissingPosition(position_key))?;
    let debt_amount = v1_debt(open_position, open_pair, debt_side)?;
    let collateral_amount = v1_collateral(open_position, collateral_side);
    let liquidation_price = open_position
        .get_liquidation_price(open_pair, &mint_for_v1(open_pair, debt_side))
        .map_err(|error| v1_native_error("UserPosition::get_liquidation_price", error))?;
    let liquidation_cf = open_position
        .get_liquidation_cf_bps(open_pair, &mint_for_v1(open_pair, debt_side))
        .map_err(|error| v1_native_error("UserPosition::get_liquidation_cf_bps", error))?;
    let current_value = v1_collateral_value(open_pair, collateral_side, collateral_amount)?;
    let equity = current_value.saturating_sub(debt_amount);
    let equity_bps = if current_value == 0 {
        0
    } else {
        u128::from(equity) * u128::from(V1_BPS_DENOMINATOR) / u128::from(current_value)
    };
    let initial_margin_bps = theoretical_denominator;
    let maintenance_margin_bps = V1_BPS_DENOMINATOR.checked_sub(liquidation_cf).ok_or(
        LeverageBenchmarkError::InvalidInput("V1 liquidation CF exceeds 100%"),
    )?;
    let immediate_reverse = quote_v1_swap(
        &engine,
        input
            .v1_loop_clocks
            .get(loops.saturating_sub(1))
            .map_or(input.v1_initial_deposit_slot, |clock| clock.deposit_slot),
        collateral_side,
        collateral_amount,
    )?;
    let funding = v1_funding(&engine, debt_side)?;
    let open_transfer_fees = ledger.transfer_fees;
    let deployed_notional = ledger.deployed_notional;
    let collateral_acquired = ledger.collateral_acquired;
    let borrowed = ledger.borrowed;

    let (unwind_limit, unwind_iterations) = unwind_v1(
        &mut engine,
        fees,
        input,
        position_key,
        debt_side,
        collateral_side,
        &debt_mint,
        &collateral_mint,
        &mut ledger,
    )?;
    let final_position = engine
        .position(&position_key)
        .ok_or(V1EngineError::MissingPosition(position_key))?;
    let remaining_debt = v1_debt(final_position, engine.pair(), debt_side)?;
    let remaining_collateral = v1_collateral(final_position, collateral_side);
    let unwind_impact_bps = if ledger.unwind_spot_value == 0
        || u128::from(ledger.gross_debt_output) >= ledger.unwind_spot_value
    {
        0
    } else {
        (ledger.unwind_spot_value - u128::from(ledger.gross_debt_output))
            * u128::from(V1_BPS_DENOMINATOR)
            / ledger.unwind_spot_value
    };
    let realized = realized_mae(input)?;
    let first_breach = input
        .adverse_path
        .iter()
        .position(|mark| mark.collateral_price_nad <= liquidation_price)
        .map(amount_usize)
        .transpose()?;
    Ok(VenueLeverageBenchmarkV1 {
        theoretical_leverage: ratio(
            u128::from(V1_BPS_DENOMINATOR),
            u128::from(theoretical_denominator),
        )?,
        executable_leverage: ratio(u128::from(deployed_notional), u128::from(margin.net_amount))?,
        opening: OpeningBenchmarkV1 {
            starting_margin_gross: amount(input.starting_margin_gross)?,
            starting_margin_net: amount(margin.net_amount)?,
            borrowed_amount: amount(borrowed)?,
            deployed_notional: amount(deployed_notional)?,
            collateral_acquired: amount(collateral_acquired)?,
            transfer_fees_paid: amount(open_transfer_fees)?,
            immediate_closeout_value: amount(immediate_reverse.amount_out)?,
            opening_cost: signed(
                i128::from(deployed_notional) - i128::from(immediate_reverse.amount_out),
            )?,
            limiting_borrow_liquidity: amount(remaining_capacity.cash_reserve)?,
            loop_iterations: amount_usize(loops)?,
            loop_iteration_limit_reached: iteration_limit_reached,
        },
        funding,
        liquidation: LiquidationBenchmarkV1 {
            threshold_kind: LiquidationThresholdKindV1::CollateralPriceNad,
            threshold: amount(liquidation_price)?,
            initial_margin_bps: amount(initial_margin_bps)?,
            maintenance_margin_bps: amount(maintenance_margin_bps)?,
            equity_bps: amount_u128(equity_bps)?,
            initial_buffer_bps: signed(equity_bps as i128 - i128::from(initial_margin_bps))?,
            maintenance_buffer_bps: signed(
                equity_bps as i128 - i128::from(maintenance_margin_bps),
            )?,
            adverse_path_availability: AdversePathAvailabilityV1::Complete,
            first_adverse_path_breach: first_breach,
            standardized_max_adverse_excursion_bps: amount(standardized_mae(input.adverse_path)?)?,
            realized_max_adverse_excursion_bps: realized.as_ref().map(|value| value.0.clone()),
            realized_mae_source_sha256: realized.map(|value| value.1),
        },
        unwind: UnwindBenchmarkV1 {
            executable_full_unwind: remaining_debt == 0 && remaining_collateral == 0,
            limiting_reason: unwind_limit,
            collateral_sold: amount(ledger.collateral_sold)?,
            gross_debt_output: amount(ledger.gross_debt_output)?,
            terminal_owner_debt_balance: amount(ledger.owner_debt)?,
            remaining_debt: amount(remaining_debt)?,
            remaining_collateral: amount(remaining_collateral)?,
            transfer_fees_paid: amount(
                ledger.transfer_fees.checked_sub(open_transfer_fees).ok_or(
                    LeverageBenchmarkError::Arithmetic("V1 unwind transfer fees"),
                )?,
            )?,
            unwind_impact_bps: amount_u128(unwind_impact_bps)?,
            cost_vs_starting_margin: signed(
                i128::from(margin.net_amount) - i128::from(ledger.owner_debt),
            )?,
            iterations: amount_usize(unwind_iterations)?,
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn unwind_v1(
    engine: &mut V1NativeEngine,
    fees: &ValidatedTransferFeeRegistryV1,
    input: &LeverageBenchmarkInputV1<'_>,
    position_key: Pubkey,
    debt_side: V1TokenSideV1,
    collateral_side: V1TokenSideV1,
    debt_mint: &str,
    collateral_mint: &str,
    ledger: &mut V1Ledger,
) -> Result<(UnwindLimitV1, usize), LeverageBenchmarkError> {
    let mut iterations = 0usize;
    for index in 0..input.v1_max_unwind_iterations {
        let position = engine
            .position(&position_key)
            .ok_or(V1EngineError::MissingPosition(position_key))?;
        let debt = v1_debt(position, engine.pair(), debt_side)?;
        let collateral = v1_collateral(position, collateral_side);
        if debt == 0 && collateral == 0 {
            return Ok((UnwindLimitV1::None, iterations));
        }
        let clock =
            input
                .v1_unwind_clocks
                .get(index)
                .ok_or(LeverageBenchmarkError::MissingClock {
                    stage: "unwind",
                    index,
                })?;

        // Residual debt tokens from the prior reverse swap are used before
        // touching more collateral. There is no synthetic repayment capital.
        if debt > 0 && ledger.owner_debt > 0 {
            repay_v1_owner_balance(
                engine,
                fees,
                clock.repay_slot,
                position_key,
                debt_side,
                debt_mint,
                ledger,
            )?;
        }

        let position = engine
            .position(&position_key)
            .ok_or(V1EngineError::MissingPosition(position_key))?;
        let debt = v1_debt(position, engine.pair(), debt_side)?;
        let collateral = v1_collateral(position, collateral_side);
        if collateral == 0 {
            return Ok((UnwindLimitV1::InsufficientSwapProceeds, iterations));
        }
        let withdraw_amount = if debt == 0 {
            collateral
        } else {
            maximum_safe_v1_withdrawal(
                engine,
                fees,
                clock.withdraw_slot,
                position_key,
                collateral_side,
                collateral_mint,
                collateral,
            )?
        };
        if withdraw_amount == 0 {
            let vault = match collateral_side {
                V1TokenSideV1::Token0 => engine.custody().collateral_vault0,
                V1TokenSideV1::Token1 => engine.custody().collateral_vault1,
            };
            return Ok((
                if vault == 0 {
                    UnwindLimitV1::CollateralVaultLiquidity
                } else {
                    UnwindLimitV1::HealthRounding
                },
                iterations,
            ));
        }
        let withdrawal_transfer =
            fees.transfer(collateral_mint, clock.withdraw_slot, withdraw_amount)?;
        engine.apply_collateral(V1CollateralInputV1 {
            slot: clock.withdraw_slot,
            position: position_key,
            collateral_token: collateral_side,
            amount: withdraw_amount,
            requested_amount: if debt == 0 && withdraw_amount == collateral {
                u64::MAX
            } else {
                withdraw_amount
            },
            deposit: false,
            collateral_vault_amount: withdraw_amount,
            counterparty_amount: withdrawal_transfer.net_amount,
        })?;
        ledger.owner_collateral = checked_add(
            ledger.owner_collateral,
            withdrawal_transfer.net_amount,
            "V1 withdrawn collateral",
        )?;
        ledger.transfer_fees = checked_add(
            ledger.transfer_fees,
            withdrawal_transfer.fee_amount,
            "V1 unwind transfer fee",
        )?;
        if ledger.owner_collateral > 0 {
            let quote = quote_v1_swap(
                engine,
                clock.swap_slot,
                collateral_side,
                ledger.owner_collateral,
            )?;
            ledger.unwind_spot_value = ledger
                .unwind_spot_value
                .checked_add(u128::from(quote.spot_value))
                .ok_or(LeverageBenchmarkError::Arithmetic("V1 unwind spot value"))?;
            let output_before = ledger.owner_debt;
            apply_v1_swap(
                engine,
                fees,
                clock.swap_slot,
                collateral_side,
                ledger.owner_collateral,
                collateral_mint,
                debt_mint,
                false,
                ledger,
            )?;
            let recipient_output = ledger
                .owner_debt
                .checked_sub(output_before)
                .ok_or(LeverageBenchmarkError::Arithmetic("V1 recipient output"))?;
            let gross_output = quote.amount_out;
            ledger.gross_debt_output = checked_add(
                ledger.gross_debt_output,
                gross_output,
                "V1 unwind debt output",
            )?;
            ledger.collateral_sold = checked_add(
                ledger.collateral_sold,
                withdraw_amount.min(withdrawal_transfer.net_amount),
                "V1 unwind collateral sold",
            )?;
            if recipient_output == 0 && debt > 0 {
                return Ok((UnwindLimitV1::InsufficientSwapProceeds, iterations + 1));
            }
        }
        let current_debt = v1_debt(
            engine
                .position(&position_key)
                .ok_or(V1EngineError::MissingPosition(position_key))?,
            engine.pair(),
            debt_side,
        )?;
        if current_debt > 0 && ledger.owner_debt > 0 {
            repay_v1_owner_balance(
                engine,
                fees,
                clock.repay_slot,
                position_key,
                debt_side,
                debt_mint,
                ledger,
            )?;
        }
        iterations += 1;
    }
    let position = engine
        .position(&position_key)
        .ok_or(V1EngineError::MissingPosition(position_key))?;
    if v1_debt(position, engine.pair(), debt_side)? == 0
        && v1_collateral(position, collateral_side) == 0
    {
        Ok((UnwindLimitV1::None, iterations))
    } else {
        Ok((UnwindLimitV1::IterationLimit, iterations))
    }
}

fn maximum_safe_v1_withdrawal(
    engine: &V1NativeEngine,
    fees: &ValidatedTransferFeeRegistryV1,
    slot: u64,
    position_key: Pubkey,
    collateral_side: V1TokenSideV1,
    collateral_mint: &str,
    available: u64,
) -> Result<u64, LeverageBenchmarkError> {
    let mut low = 0u64;
    let mut high = available;
    while low < high {
        let middle = low + (high - low) / 2 + 1;
        let transfer = fees.transfer(collateral_mint, slot, middle)?;
        let mut fork = engine.try_fork();
        match fork.apply_collateral(V1CollateralInputV1 {
            slot,
            position: position_key,
            collateral_token: collateral_side,
            amount: middle,
            requested_amount: middle,
            deposit: false,
            collateral_vault_amount: middle,
            counterparty_amount: transfer.net_amount,
        }) {
            Ok(_) => low = middle,
            Err(
                V1EngineError::BorrowingPowerExceeded { .. }
                | V1EngineError::InsufficientVaultBalance { .. }
                | V1EngineError::InsufficientCollateral { .. },
            ) => high = middle - 1,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(low)
}

fn repay_v1_owner_balance(
    engine: &mut V1NativeEngine,
    fees: &ValidatedTransferFeeRegistryV1,
    slot: u64,
    position_key: Pubkey,
    debt_side: V1TokenSideV1,
    debt_mint: &str,
    ledger: &mut V1Ledger,
) -> Result<(), LeverageBenchmarkError> {
    let debt = v1_debt(
        engine
            .position(&position_key)
            .ok_or(V1EngineError::MissingPosition(position_key))?,
        engine.pair(),
        debt_side,
    )?;
    let repay = ledger.owner_debt.min(debt);
    if repay == 0 {
        return Ok(());
    }
    let transfer = fees.transfer(debt_mint, slot, repay)?;
    engine.apply_repay(V1RepayInputV1 {
        slot,
        position: position_key,
        debt_token: debt_side,
        requested_amount: if repay == debt { u64::MAX } else { repay },
        vault_credit: transfer.net_amount,
    })?;
    ledger.owner_debt -= repay;
    ledger.transfer_fees = checked_add(
        ledger.transfer_fees,
        transfer.fee_amount,
        "V1 repay transfer fee",
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_v1_swap(
    engine: &mut V1NativeEngine,
    fees: &ValidatedTransferFeeRegistryV1,
    slot: u64,
    token_in: V1TokenSideV1,
    gross_input: u64,
    input_mint: &str,
    output_mint: &str,
    input_is_debt: bool,
    ledger: &mut V1Ledger,
) -> Result<(), LeverageBenchmarkError> {
    if gross_input == 0 {
        return Err(LeverageBenchmarkError::InvalidInput("zero V1 swap input"));
    }
    let input_transfer = fees.transfer(input_mint, slot, gross_input)?;
    let quote = quote_v1_swap(engine, slot, token_in, gross_input)?;
    let output_transfer = fees.transfer(output_mint, slot, quote.amount_out)?;
    engine.apply_swap(V1SwapInputV1 {
        slot,
        token_in,
        gross_amount_in: gross_input,
        min_amount_out: quote.amount_out,
        vault_credit_in: input_transfer.net_amount,
        vault_debit_out: quote.amount_out,
        recipient_credit_out: output_transfer.net_amount,
    })?;
    if input_is_debt {
        if ledger.owner_debt < gross_input {
            return Err(LeverageBenchmarkError::InvalidInput(
                "V1 owner debt balance",
            ));
        }
        ledger.owner_debt -= gross_input;
        ledger.owner_collateral = checked_add(
            ledger.owner_collateral,
            output_transfer.net_amount,
            "V1 swap collateral output",
        )?;
        ledger.deployed_notional = checked_add(
            ledger.deployed_notional,
            gross_input,
            "V1 deployed notional",
        )?;
        ledger.collateral_acquired = checked_add(
            ledger.collateral_acquired,
            output_transfer.net_amount,
            "V1 collateral acquired",
        )?;
    } else if ledger.owner_collateral >= gross_input {
        ledger.owner_collateral -= gross_input;
        ledger.owner_debt = checked_add(
            ledger.owner_debt,
            output_transfer.net_amount,
            "V1 swap debt output",
        )?;
    } else {
        return Err(LeverageBenchmarkError::InvalidInput(
            "V1 owner collateral balance",
        ));
    }
    ledger.transfer_fees = sum_u64(
        [
            ledger.transfer_fees,
            input_transfer.fee_amount,
            output_transfer.fee_amount,
        ],
        "V1 swap transfer fees",
    )?;
    Ok(())
}

fn deposit_all_v1_collateral(
    engine: &mut V1NativeEngine,
    fees: &ValidatedTransferFeeRegistryV1,
    slot: u64,
    position_key: Pubkey,
    collateral_side: V1TokenSideV1,
    collateral_mint: &str,
    ledger: &mut V1Ledger,
) -> Result<(), LeverageBenchmarkError> {
    let amount = ledger.owner_collateral;
    if amount == 0 {
        return Err(LeverageBenchmarkError::InvalidInput(
            "zero V1 collateral deposit",
        ));
    }
    let transfer = fees.transfer(collateral_mint, slot, amount)?;
    engine.apply_collateral(V1CollateralInputV1 {
        slot,
        position: position_key,
        collateral_token: collateral_side,
        amount,
        requested_amount: amount,
        deposit: true,
        collateral_vault_amount: transfer.net_amount,
        counterparty_amount: amount,
    })?;
    ledger.owner_collateral = 0;
    ledger.transfer_fees = checked_add(
        ledger.transfer_fees,
        transfer.fee_amount,
        "V1 collateral transfer fee",
    )?;
    Ok(())
}

fn quote_v1_swap(
    engine: &V1NativeEngine,
    slot: u64,
    token_in: V1TokenSideV1,
    gross_input: u64,
) -> Result<V1SwapQuote, LeverageBenchmarkError> {
    let mut fork = engine.try_fork();
    fork.advance_to_slot(slot)?;
    let pair = fork.pair();
    let fee = ceil_div(
        u128::from(gross_input)
            .checked_mul(u128::from(pair.swap_fee_bps))
            .ok_or(LeverageBenchmarkError::Arithmetic("V1 swap fee"))?,
        u128::from(V1_BPS_DENOMINATOR),
    )
    .ok_or(LeverageBenchmarkError::Arithmetic("V1 swap fee division"))?;
    let fee =
        u64::try_from(fee).map_err(|_| LeverageBenchmarkError::Arithmetic("V1 swap fee cast"))?;
    let net = gross_input
        .checked_sub(fee)
        .ok_or(LeverageBenchmarkError::Arithmetic("V1 swap net input"))?;
    let (reserve_in, reserve_out) = match token_in {
        V1TokenSideV1::Token0 => (pair.reserve0, pair.reserve1),
        V1TokenSideV1::Token1 => (pair.reserve1, pair.reserve0),
    };
    let amount_out = CPCurve::calculate_amount_out(reserve_in, reserve_out, net)
        .map_err(|error| v1_native_error("CPCurve::calculate_amount_out", error))?;
    let spot_value = u64::try_from(
        u128::from(gross_input)
            .checked_mul(u128::from(reserve_out))
            .and_then(|value| value.checked_div(u128::from(reserve_in)))
            .ok_or(LeverageBenchmarkError::Arithmetic("V1 spot value"))?,
    )
    .map_err(|_| LeverageBenchmarkError::Arithmetic("V1 spot value cast"))?;
    Ok(V1SwapQuote {
        amount_out,
        spot_value,
    })
}

fn insert_synthetic_position(
    engine: &mut V1NativeEngine,
    owner: Pubkey,
) -> Result<Pubkey, LeverageBenchmarkError> {
    if owner == Pubkey::default() {
        return Err(LeverageBenchmarkError::InvalidInput("zero V1 owner"));
    }
    let (key, bump) = Pubkey::find_program_address(
        &[
            POSITION_SEED_PREFIX,
            engine.pair_key().as_ref(),
            owner.as_ref(),
        ],
        &omnipair::ID,
    );
    let mut position = UserPosition {
        owner: Pubkey::default(),
        pair: Pubkey::default(),
        collateral0_liquidation_cf_bps: 0,
        collateral1_liquidation_cf_bps: 0,
        collateral0: 0,
        collateral1: 0,
        debt0_shares: 0,
        debt1_shares: 0,
        bump: 0,
    };
    position
        .initialize(owner, engine.pair_key(), bump)
        .map_err(|error| v1_native_error("UserPosition::initialize", error))?;
    engine.insert_decoded_position(key, position)?;
    Ok(key)
}

fn dusk_funding(
    market: &BenchmarkMarket,
    debt_asset: MarketAsset,
) -> Result<FundingAprBenchmarkV1, LeverageBenchmarkError> {
    let native = market.market();
    let utilization = native
        .lending_utilization_bps(debt_asset)
        .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?;
    let target = native.config.irm.target_utilization_bps;
    let rate_at_target = match debt_asset {
        MarketAsset::Base => native.debt.base_rate_at_target_nad,
        MarketAsset::Quote => native.debt.quote_rate_at_target_nad,
    };
    let error = utilization_error_nad(u64::from(utilization), u64::from(target))
        .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?;
    let rate = instantaneous_rate_apr_nad(
        rate_at_target,
        error,
        u128::from(native.config.irm.curve_steepness_nad),
    )
    .map_err(|error| LeverageBenchmarkError::DuskNative(error.to_string()))?;
    Ok(FundingAprBenchmarkV1 {
        utilization_scale: "bps".to_owned(),
        utilization: amount(utilization)?,
        target_utilization_bps: Some(amount(target)?),
        rate_at_target_nad: Some(amount_u128(rate_at_target)?),
        instantaneous_borrow_apr_nad: amount_u128(rate)?,
    })
}

fn v1_funding(
    engine: &V1NativeEngine,
    debt_side: V1TokenSideV1,
) -> Result<FundingAprBenchmarkV1, LeverageBenchmarkError> {
    let pair = engine.pair();
    let (debt, reserve, rate) = match debt_side {
        V1TokenSideV1::Token0 => (pair.total_debt0, pair.reserve0, pair.last_rate0),
        V1TokenSideV1::Token1 => (pair.total_debt1, pair.reserve1, pair.last_rate1),
    };
    let utilization = if reserve == 0 {
        0
    } else {
        u64::try_from(u128::from(debt) * u128::from(V1_NAD) / u128::from(reserve))
            .map_err(|_| LeverageBenchmarkError::Arithmetic("V1 utilization"))?
    };
    Ok(FundingAprBenchmarkV1 {
        utilization_scale: "nad".to_owned(),
        utilization: amount(utilization)?,
        target_utilization_bps: None,
        rate_at_target_nad: None,
        instantaneous_borrow_apr_nad: amount(rate)?,
    })
}

fn v1_collateral_value(
    pair: &omnipair::state::Pair,
    collateral_side: V1TokenSideV1,
    collateral: u64,
) -> Result<u64, LeverageBenchmarkError> {
    let price = match collateral_side {
        // The engine has already advanced these stored EMAs to the operation
        // slot. Calling Pair::ema_price*_nad off-chain would read Clock::get
        // and would therefore be both non-deterministic and unavailable.
        V1TokenSideV1::Token0 => pair.last_price0_ema.symmetric,
        V1TokenSideV1::Token1 => pair.last_price1_ema.symmetric,
    };
    u64::try_from(
        u128::from(collateral)
            .checked_mul(u128::from(price))
            .and_then(|value| value.checked_div(u128::from(V1_NAD)))
            .ok_or(LeverageBenchmarkError::Arithmetic("V1 collateral value"))?,
    )
    .map_err(|_| LeverageBenchmarkError::Arithmetic("V1 collateral value cast"))
}

fn v1_debt(
    position: &UserPosition,
    pair: &omnipair::state::Pair,
    side: V1TokenSideV1,
) -> Result<u64, LeverageBenchmarkError> {
    match side {
        V1TokenSideV1::Token0 => {
            position.calculate_debt0(pair.total_debt0, pair.total_debt0_shares)
        }
        V1TokenSideV1::Token1 => {
            position.calculate_debt1(pair.total_debt1, pair.total_debt1_shares)
        }
    }
    .map_err(|error| v1_native_error("UserPosition::calculate_debt", error))
}

const fn v1_collateral(position: &UserPosition, side: V1TokenSideV1) -> u64 {
    match side {
        V1TokenSideV1::Token0 => position.collateral0,
        V1TokenSideV1::Token1 => position.collateral1,
    }
}

const fn mint_for_v1(pair: &omnipair::state::Pair, side: V1TokenSideV1) -> Pubkey {
    match side {
        V1TokenSideV1::Token0 => pair.token0,
        V1TokenSideV1::Token1 => pair.token1,
    }
}

fn fee_free_transfer(
    fees: &ValidatedTransferFeeRegistryV1,
    mint: &str,
    slot: u64,
    amount: u64,
) -> Result<TransferOutcomeV1, LeverageBenchmarkError> {
    let outcome = fees.transfer(mint, slot, amount)?;
    if outcome.net_amount != outcome.gross_amount {
        return Err(LeverageBenchmarkError::UnsupportedDuskCollateralMint);
    }
    Ok(outcome)
}

const fn token_outcome(outcome: TransferOutcomeV1) -> BenchmarkTokenTransferOutcome {
    BenchmarkTokenTransferOutcome {
        source_debit: outcome.gross_amount,
        destination_credit: outcome.net_amount,
    }
}

fn standardized_mae(path: &[AdversePriceMarkV1]) -> Result<u64, LeverageBenchmarkError> {
    let entry = path
        .first()
        .ok_or(LeverageBenchmarkError::InvalidInput("empty adverse path"))?
        .collateral_price_nad;
    let minimum = path
        .iter()
        .map(|mark| mark.collateral_price_nad)
        .min()
        .ok_or(LeverageBenchmarkError::InvalidInput("empty adverse path"))?;
    u64::try_from(
        u128::from(entry.saturating_sub(minimum))
            .checked_mul(u128::from(V1_BPS_DENOMINATOR))
            .and_then(|value| value.checked_div(u128::from(entry)))
            .ok_or(LeverageBenchmarkError::Arithmetic("standardized MAE"))?,
    )
    .map_err(|_| LeverageBenchmarkError::Arithmetic("standardized MAE cast"))
}

fn realized_mae(
    input: &LeverageBenchmarkInputV1<'_>,
) -> Result<Option<(TokenAmount, String)>, LeverageBenchmarkError> {
    input
        .realized_mae
        .as_ref()
        .map(|value| {
            Ok((
                amount(value.adverse_excursion_bps)?,
                value.source_sha256.clone(),
            ))
        })
        .transpose()
}

fn ratio(numerator: u128, denominator: u128) -> Result<ExactRatioV1, LeverageBenchmarkError> {
    Ok(ExactRatioV1 {
        numerator: amount_u128(numerator)?,
        denominator: amount_u128(denominator)?,
        unbounded: denominator == 0,
    })
}

fn amount(value: impl Into<u128>) -> Result<TokenAmount, LeverageBenchmarkError> {
    amount_u128(value.into())
}

fn amount_u128(value: u128) -> Result<TokenAmount, LeverageBenchmarkError> {
    Ok(TokenAmount::new(value.to_string())?)
}

fn amount_usize(value: usize) -> Result<TokenAmount, LeverageBenchmarkError> {
    amount_u128(value as u128)
}

fn signed(value: i128) -> Result<SignedTokenAmount, LeverageBenchmarkError> {
    Ok(SignedTokenAmount::new(value.to_string())?)
}

fn checked_add(
    left: u64,
    right: u64,
    operation: &'static str,
) -> Result<u64, LeverageBenchmarkError> {
    left.checked_add(right)
        .ok_or(LeverageBenchmarkError::Arithmetic(operation))
}

fn sum_u64<const N: usize>(
    values: [u64; N],
    operation: &'static str,
) -> Result<u64, LeverageBenchmarkError> {
    values
        .into_iter()
        .try_fold(0u64, |total, value| checked_add(total, value, operation))
}

fn v1_native_error(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> LeverageBenchmarkError {
    LeverageBenchmarkError::V1(V1EngineError::NativeMath {
        operation,
        detail: error.to_string(),
    })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dusk::{
        benchmark_api::{BenchmarkMarketInit, BenchmarkMaximumHlpEntry},
        constants::{MIN_HALF_LIFE_MS, NAD},
        state::{
            AmmConfig, IrmConfig, MarketConfig, MarketSide, ProtocolAuctionSplit,
            DEFAULT_DAILY_BORROW_BPS,
        },
    };
    use omnipair::{
        constants::VERSION,
        state::{FutarchyAuthority, Pair, RateModel, VaultBumps},
    };

    use crate::{
        canonical::TokenAmount,
        transfer_fee::{
            EpochScheduleV1, MintTransferFeeScheduleV1, TokenProgramKindV1, TransferFeeEpochV1,
            TransferFeeRegistryV1, TRANSFER_FEE_SCHEMA_VERSION,
        },
        v1_engine::{V1CustodyCheckpointV1, V1DecodedAccountsV1, V1RevenueCheckpointV1},
    };

    const BASE: Pubkey = Pubkey::new_from_array([1; 32]);
    const QUOTE: Pubkey = Pubkey::new_from_array([2; 32]);
    const PAIR_KEY: Pubkey = Pubkey::new_from_array([3; 32]);
    const RATE_KEY: Pubkey = Pubkey::new_from_array([4; 32]);
    const OWNER: Pubkey = Pubkey::new_from_array([5; 32]);
    const MARKET_KEY: Pubkey = Pubkey::new_from_array([6; 32]);
    const POSITION_ID: Pubkey = Pubkey::new_from_array([7; 32]);

    fn market_config() -> MarketConfig {
        MarketConfig {
            swap_fee_bps: 30,
            divergence_fee_share_cap_bps: 0,
            volatility_fee_share_cap_bps: 0,
            target_hlp_leverage_bps: 20_000,
            settlement_divergence_bps: 10_000,
            ema_half_life_ms: MIN_HALF_LIFE_MS,
            directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
            curve_depth_ema_half_life_ms: MIN_HALF_LIFE_MS,
            max_daily_borrow_bps: DEFAULT_DAILY_BORROW_BPS,
            global_health_contribution_cap_bps: 15_000,
            borrow_market_health_floor_bps: 10_000,
            amm: AmmConfig::default(),
            irm: IrmConfig::default(),
            start_time: 0,
        }
    }

    fn dusk_market() -> BenchmarkMarket {
        let base_side = MarketSide {
            asset_mint: BASE,
            asset_decimals: 6,
            hlp_mint: Pubkey::new_from_array([11; 32]),
            reserve_vault: Pubkey::new_from_array([12; 32]),
            collateral_vault: Pubkey::new_from_array([13; 32]),
            interest_vault: Pubkey::new_from_array([14; 32]),
            ..MarketSide::default()
        };
        let quote_side = MarketSide {
            asset_mint: QUOTE,
            asset_decimals: 6,
            hlp_mint: Pubkey::new_from_array([15; 32]),
            reserve_vault: Pubkey::new_from_array([16; 32]),
            collateral_vault: Pubkey::new_from_array([17; 32]),
            interest_vault: Pubkey::new_from_array([18; 32]),
            ..MarketSide::default()
        };
        let mut market = BenchmarkMarket::initialize_at(
            MARKET_KEY,
            BenchmarkMarketInit {
                ylp_mint: Pubkey::new_from_array([19; 32]),
                base_side,
                quote_side,
                config: market_config(),
                base_hlp_ylp_vault: Pubkey::new_from_array([20; 32]),
                quote_hlp_ylp_vault: Pubkey::new_from_array([21; 32]),
                base_insurance_vault: Pubkey::new_from_array([22; 32]),
                quote_insurance_vault: Pubkey::new_from_array([23; 32]),
                params_hash: [9; 32],
                initial_liquidity_authority: Pubkey::new_from_array([24; 32]),
                bootstrap_price_nad: 0,
                launch_fee_progress_offset: 0,
                bump: 250,
            },
            BenchmarkClock {
                slot: 0,
                unix_timestamp: 0,
            },
        )
        .unwrap();
        market
            .market_mut()
            .add_liquidity(1_000_000_000, 1_000_000_000)
            .unwrap();
        market
            .advance_to(BenchmarkClock {
                slot: 1,
                unix_timestamp: 1,
            })
            .unwrap();
        market
    }

    fn v1_engine() -> V1NativeEngine {
        let rate_model = RateModel::new(3_000, 5_000, 3_600_000, 100, 1_000, 200);
        let mut pair = Pair::initialize(
            BASE,
            QUOTE,
            Pubkey::new_from_array([25; 32]),
            6,
            6,
            RATE_KEY,
            30,
            60_000,
            Some(8_000),
            1,
            [8; 32],
            VERSION,
            249,
            VaultBumps {
                reserve0: 1,
                reserve1: 2,
                collateral0: 3,
                collateral1: 4,
            },
            rate_model.initial_rate,
        );
        pair.reserve0 = 1_000_000_000;
        pair.reserve1 = 1_000_000_000;
        pair.cash_reserve0 = 1_000_000_000;
        pair.cash_reserve1 = 1_000_000_000;
        pair.last_price0_ema.symmetric = NAD;
        pair.last_price0_ema.directional = NAD;
        pair.last_price1_ema.symmetric = NAD;
        pair.last_price1_ema.directional = NAD;
        pair.total_supply = 1_000_000_000;
        let authority = FutarchyAuthority::initialize(
            Pubkey::new_from_array([26; 32]),
            2_000,
            1_000,
            Pubkey::new_from_array([27; 32]),
            Pubkey::new_from_array([28; 32]),
            Pubkey::new_from_array([29; 32]),
            4_000,
            3_000,
            3_000,
            30,
        )
        .unwrap();
        V1NativeEngine::from_decoded_accounts(V1DecodedAccountsV1 {
            program_version: VERSION,
            pair_key: PAIR_KEY,
            pair,
            rate_model_key: RATE_KEY,
            rate_model,
            futarchy_authority: authority,
            positions: vec![],
            custody: V1CustodyCheckpointV1 {
                reserve_vault0: 1_000_000_000,
                reserve_vault1: 1_000_000_000,
                collateral_vault0: 0,
                collateral_vault1: 0,
            },
            revenue: V1RevenueCheckpointV1 {
                lp_swap_fee0: 0,
                lp_swap_fee1: 0,
                protocol_swap_fee0: 0,
                protocol_swap_fee1: 0,
                lp_interest0: 0,
                lp_interest1: 0,
                protocol_interest0: 0,
                protocol_interest1: 0,
                flashloan_fee0: 0,
                flashloan_fee1: 0,
                protocol_claim_realized0: 0,
                protocol_claim_realized1: 0,
            },
        })
        .unwrap()
    }

    fn token_amount(value: u64) -> TokenAmount {
        TokenAmount::new(value.to_string()).unwrap()
    }

    fn registry(base_fee_bps: u16) -> ValidatedTransferFeeRegistryV1 {
        let mint = |key: Pubkey, bps: u16| MintTransferFeeScheduleV1 {
            schema_version: TRANSFER_FEE_SCHEMA_VERSION,
            mint: key.to_string(),
            token_program: if bps == 0 {
                TokenProgramKindV1::LegacyToken
            } else {
                TokenProgramKindV1::Token2022
            },
            epochs: vec![TransferFeeEpochV1 {
                effective_epoch: 0,
                transfer_fee_basis_points: bps,
                maximum_fee: token_amount(if bps == 0 { 0 } else { 10_000 }),
                source_signature: "mint-initialize".to_owned(),
                source_slot: 0,
                decoded_instruction_sha256: "b".repeat(64),
            }],
        };
        TransferFeeRegistryV1 {
            schema_version: TRANSFER_FEE_SCHEMA_VERSION,
            epoch_schedule: EpochScheduleV1 {
                slots_per_epoch: 432_000,
                leader_schedule_slot_offset: 432_000,
                warmup: false,
                first_normal_epoch: 0,
                first_normal_slot: 0,
                source_response_sha256: "a".repeat(64),
            },
            mints: vec![mint(BASE, base_fee_bps), mint(QUOTE, 0)],
        }
        .validate()
        .unwrap()
    }

    fn policy() -> BenchmarkLeveragePolicy {
        BenchmarkLeveragePolicy {
            protocol_swap_fee_bps: 0,
            protocol_interest_fee_bps: 2_000,
            protocol_auction_split: ProtocolAuctionSplit::default(),
            max_referral_interest_share_bps: 0,
            global_reduce_only: false,
            collateral_mint_is_leverage_eligible: true,
            launch_same_transaction_guard_satisfied: true,
        }
    }

    fn balances() -> BenchmarkLeverageTokenBalances {
        BenchmarkLeverageTokenBalances {
            owner_base_balance: 1_000_000,
            owner_quote_balance: 1_000_000,
            base_reserve_vault_balance: 2_000_000_000,
            quote_reserve_vault_balance: 2_000_000_000,
            base_interest_vault_balance: 0,
            quote_interest_vault_balance: 0,
            base_leverage_collateral_vault_balance: 0,
            quote_leverage_collateral_vault_balance: 0,
        }
    }

    fn loop_clocks(start: u64, count: usize) -> Vec<V1LoopClockV1> {
        (0..count)
            .map(|index| {
                let slot = start + index as u64;
                V1LoopClockV1 {
                    borrow_slot: slot,
                    swap_slot: slot,
                    deposit_slot: slot,
                }
            })
            .collect()
    }

    fn unwind_clocks(start: u64, count: usize) -> Vec<V1UnwindClockV1> {
        (0..count)
            .map(|index| {
                let slot = start + index as u64;
                V1UnwindClockV1 {
                    withdraw_slot: slot,
                    swap_slot: slot,
                    repay_slot: slot,
                }
            })
            .collect()
    }

    fn input<'a>(
        loops: &'a [V1LoopClockV1],
        unwind: &'a [V1UnwindClockV1],
        path: &'a [AdversePriceMarkV1],
        multiplier_bps: u64,
        max_loops: usize,
    ) -> LeverageBenchmarkInputV1<'a> {
        LeverageBenchmarkInputV1 {
            benchmark_id: "golden-market".to_owned(),
            direction: LeverageDirectionV1::LongBase,
            owner: OWNER,
            position_id: POSITION_ID,
            starting_margin_gross: 20_000,
            dusk_multiplier_bps: multiplier_bps,
            dusk_open_clock: BenchmarkClock {
                slot: 2,
                unix_timestamp: 2,
            },
            dusk_close_clock: BenchmarkClock {
                slot: path.last().map_or(2, |mark| mark.slot),
                unix_timestamp: path.last().map_or(2, |mark| mark.slot as i64),
            },
            dusk_token_balances: balances(),
            dusk_policy: policy(),
            hlp_scenario: HlpScenario::NoHlp,
            dusk_base_hlp: None,
            dusk_quote_hlp: None,
            collateral_mint_decode_sha256: "c".repeat(64),
            v1_initial_swap_slot: 2,
            v1_initial_deposit_slot: 2,
            v1_loop_clocks: loops,
            v1_max_loop_iterations: max_loops,
            v1_unwind_clocks: unwind,
            v1_max_unwind_iterations: unwind.len(),
            adverse_path: path,
            realized_mae: None,
        }
    }

    #[test]
    fn golden_dusk_quote_and_execution_are_native_exact() {
        let market = dusk_market();
        let engine = v1_engine();
        let fees = registry(0);
        let loops = loop_clocks(2, 8);
        let unwind = unwind_clocks(3, 16);
        let path = [
            AdversePriceMarkV1 {
                slot: 2,
                collateral_price_nad: NAD,
                dusk_closeout_value: Some(19_000),
            },
            AdversePriceMarkV1 {
                slot: 3,
                collateral_price_nad: NAD * 9 / 10,
                dusk_closeout_value: Some(17_000),
            },
        ];
        let request = input(&loops, &unwind, &path, 20_000, loops.len());

        let margin = fees.transfer(&QUOTE.to_string(), 2, 20_000).unwrap();
        let native_market = market.try_fork().unwrap();
        let native_leverage = BenchmarkLeverageOwnedState::initialize_for_open(
            &native_market,
            OWNER,
            OWNER,
            POSITION_ID,
            balances(),
            None,
        )
        .unwrap();
        let base_hlp =
            BenchmarkHlpOwnedState::initialize(&native_market, MarketAsset::Base, OWNER, OWNER)
                .unwrap();
        let quote_hlp =
            BenchmarkHlpOwnedState::initialize(&native_market, MarketAsset::Quote, OWNER, OWNER)
                .unwrap();
        let native_quote = native_market
            .prepare_leverage_open(
                &native_leverage,
                &base_hlp,
                &quote_hlp,
                BenchmarkPrepareLeverageOpenRequest {
                    clock: request.dusk_open_clock,
                    debt_asset: MarketAsset::Quote,
                    margin_transfer: token_outcome(margin),
                    multiplier_bps: 20_000,
                    min_collateral_out: 1,
                    limit_price_nad: 0,
                    requested_referrer: None,
                    policy: policy(),
                },
            )
            .unwrap()
            .quote();

        let result = benchmark_leverage_exact(&market, &engine, &fees, request).unwrap();
        assert_eq!(
            result.dusk.opening.borrowed_amount.as_str(),
            native_quote.borrowed_amount.to_string()
        );
        assert_eq!(
            result.dusk.opening.collateral_acquired.as_str(),
            native_quote.swap.amount_out.to_string()
        );
        assert_eq!(result.dusk.executable_leverage.numerator.as_str(), "40000");
        assert_eq!(
            result.dusk.executable_leverage.denominator.as_str(),
            "20000"
        );
        assert_eq!(result.dusk.unwind.remaining_debt.as_str(), "0");
        assert_eq!(result.dusk.unwind.remaining_collateral.as_str(), "0");
        assert!(
            result
                .v1
                .opening
                .loop_iterations
                .as_str()
                .parse::<u64>()
                .unwrap()
                > 0
        );
    }

    #[test]
    fn failure_rolls_back_both_caller_checkpoints() {
        let market = dusk_market();
        let engine = v1_engine();
        let market_before = market.checkpoint().unwrap();
        let v1_before = engine.checkpoint();
        let loops = loop_clocks(2, 2);
        let unwind = unwind_clocks(3, 2);
        let path = [AdversePriceMarkV1 {
            slot: 2,
            collateral_price_nad: NAD,
            dusk_closeout_value: Some(19_000),
        }];
        let mut request = input(&loops, &unwind, &path, 20_000, loops.len());
        request.starting_margin_gross = 2_000_000;

        assert!(benchmark_leverage_exact(&market, &engine, &registry(0), request).is_err());
        assert_eq!(market.checkpoint().unwrap(), market_before);
        assert_eq!(engine.checkpoint(), v1_before);
    }

    #[test]
    fn v1_loop_iteration_exhaustion_is_explicit() {
        let market = dusk_market();
        let engine = v1_engine();
        let loops = loop_clocks(2, 1);
        let unwind = unwind_clocks(3, 8);
        let path = [AdversePriceMarkV1 {
            slot: 2,
            collateral_price_nad: NAD,
            dusk_closeout_value: Some(10_000),
        }];
        let request = input(&loops, &unwind, &path, 40_000, 1);
        let result = benchmark_leverage_exact(&market, &engine, &registry(0), request).unwrap();
        assert!(result.v1.opening.loop_iteration_limit_reached);
        assert_eq!(result.v1.opening.loop_iterations.as_str(), "1");
    }

    #[test]
    fn token_2022_collateral_fee_fails_closed_before_execution() {
        let market = dusk_market();
        let engine = v1_engine();
        let loops = loop_clocks(2, 2);
        let unwind = unwind_clocks(3, 2);
        let path = [AdversePriceMarkV1 {
            slot: 2,
            collateral_price_nad: NAD,
            dusk_closeout_value: Some(19_000),
        }];
        let request = input(&loops, &unwind, &path, 20_000, loops.len());
        assert!(matches!(
            benchmark_leverage_exact(&market, &engine, &registry(25), request),
            Err(LeverageBenchmarkError::UnsupportedDuskCollateralMint)
        ));
    }

    #[test]
    fn active_hlp_prepared_settlement_is_consumed_exactly() {
        let mut market = dusk_market();
        let mut external =
            BenchmarkHlpOwnedState::initialize(&market, MarketAsset::Base, OWNER, OWNER).unwrap();
        let plan: BenchmarkMaximumHlpEntry = market
            .maximize_hlp_entry(
                &external,
                BenchmarkClock {
                    slot: 2,
                    unix_timestamp: 2,
                },
                10_000,
                false,
            )
            .unwrap()
            .unwrap();
        market
            .execute_maximum_hlp_entry(
                &mut external,
                plan,
                BenchmarkTokenTransferOutcome {
                    source_debit: plan.target_reserve_credit,
                    destination_credit: plan.target_reserve_credit,
                },
                false,
            )
            .unwrap();
        let engine = v1_engine();
        let open_slot = 1_000_003;
        let loops = loop_clocks(open_slot, 8);
        let unwind = unwind_clocks(open_slot + 1, 16);
        let path = [AdversePriceMarkV1 {
            slot: open_slot,
            collateral_price_nad: NAD,
            dusk_closeout_value: Some(19_000),
        }];
        let mut request = input(&loops, &unwind, &path, 20_000, loops.len());
        request.dusk_open_clock = BenchmarkClock {
            slot: open_slot,
            unix_timestamp: open_slot as i64,
        };
        request.dusk_close_clock = request.dusk_open_clock;
        request.v1_initial_swap_slot = open_slot;
        request.v1_initial_deposit_slot = open_slot;
        request.dusk_token_balances.base_reserve_vault_balance = 3_000_000_000;
        request.dusk_token_balances.quote_reserve_vault_balance = 3_000_000_000;
        let quote_external =
            BenchmarkHlpOwnedState::initialize(&market, MarketAsset::Quote, OWNER, OWNER).unwrap();
        request.hlp_scenario = HlpScenario::MaximumFeasible;
        request.dusk_base_hlp = Some(&external);
        request.dusk_quote_hlp = Some(&quote_external);
        let external_before = external.checkpoint();
        let quote_external_before = quote_external.checkpoint();
        let margin = registry(0)
            .transfer(&QUOTE.to_string(), open_slot, request.starting_margin_gross)
            .unwrap();
        let leverage = BenchmarkLeverageOwnedState::initialize_for_open(
            &market,
            OWNER,
            OWNER,
            POSITION_ID,
            request.dusk_token_balances,
            None,
        )
        .unwrap();
        let prepared = market
            .prepare_leverage_open(
                &leverage,
                &external,
                &quote_external,
                BenchmarkPrepareLeverageOpenRequest {
                    clock: request.dusk_open_clock,
                    debt_asset: MarketAsset::Quote,
                    margin_transfer: token_outcome(margin),
                    multiplier_bps: request.dusk_multiplier_bps,
                    min_collateral_out: 1,
                    limit_price_nad: 0,
                    requested_referrer: None,
                    policy: request.dusk_policy,
                },
            )
            .unwrap();
        assert!(prepared.quote().settlement.base_hlp_interest_debit > 0);
        let result = benchmark_leverage_exact(&market, &engine, &registry(0), request).unwrap();
        assert_eq!(result.hlp_scenario, HlpScenario::MaximumFeasible);
        assert_eq!(result.dusk.unwind.remaining_debt.as_str(), "0");
        assert_eq!(external.checkpoint(), external_before);
        assert_eq!(quote_external.checkpoint(), quote_external_before);
    }

    #[test]
    fn maximum_feasible_never_falls_back_to_empty_hlp_state() {
        let market = dusk_market();
        let engine = v1_engine();
        let loops = loop_clocks(2, 2);
        let unwind = unwind_clocks(2, 2);
        let path = [AdversePriceMarkV1 {
            slot: 2,
            collateral_price_nad: NAD,
            dusk_closeout_value: None,
        }];
        let mut request = input(&loops, &unwind, &path, 20_000, loops.len());
        request.hlp_scenario = HlpScenario::MaximumFeasible;
        assert!(matches!(
            benchmark_leverage_exact(&market, &engine, &registry(0), request),
            Err(LeverageBenchmarkError::InvalidInput(
                "hLP scenario and owned states"
            ))
        ));
    }

    #[test]
    fn missing_future_dusk_path_is_typed_without_losing_core_metrics() {
        let market = dusk_market();
        let engine = v1_engine();
        let loops = loop_clocks(2, 4);
        let unwind = unwind_clocks(3, 8);
        let path = [AdversePriceMarkV1 {
            slot: 2,
            collateral_price_nad: NAD,
            dusk_closeout_value: None,
        }];
        let result = benchmark_leverage_exact(
            &market,
            &engine,
            &registry(0),
            input(&loops, &unwind, &path, 20_000, loops.len()),
        )
        .unwrap();
        assert_eq!(
            result.dusk.liquidation.adverse_path_availability,
            AdversePathAvailabilityV1::MissingExactDuskFutureCounterfactualState
        );
        assert_eq!(
            result.v1.liquidation.adverse_path_availability,
            AdversePathAvailabilityV1::Complete
        );
        assert!(result.dusk.liquidation.first_adverse_path_breach.is_none());
    }
}

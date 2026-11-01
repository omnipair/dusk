//! Version-pinned factual replay of Omnipair V1 pair state.
//!
//! The adapter deliberately accepts decoded native accounts rather than a
//! convenient configuration with defaults. Pair operations stage the native
//! account state first, so a rejected operation cannot retain its preceding
//! clock/interest update. Token-program custody effects (including Token-2022
//! transfer fees) remain assertions owned by the canonical decoder; this
//! module reproduces the V1 program's accounted pair and position state.

use std::collections::{btree_map::Entry, BTreeMap};

use omnipair::constants::{
    BPS_DENOMINATOR, CLOSE_FACTOR_BPS, COLLATERAL_VAULT_SEED_PREFIX, DEFAULT_MAX_RATE_BPS,
    DEFAULT_MIN_RATE_BPS, DEFAULT_RATE_HALF_LIFE_MS, DIRECTIONAL_EMA_HALF_LIFE_MS,
    FLASHLOAN_FEE_BPS, FUTARCHY_AUTHORITY_SEED_PREFIX, LIQUIDATION_INCENTIVE_BPS,
    LIQUIDATION_PENALTY_BPS, LIQUIDITY_WITHDRAWAL_FEE_BPS, LTV_BUFFER_BPS, MAX_HALF_LIFE_MS,
    MIN_HALF_LIFE_MS, MIN_LIQUIDITY, NAD, NATURAL_LOG_OF_TWO_NAD, PAIR_SEED_PREFIX,
    POST_WITHDRAW_DEBT_COVERAGE_BPS, RATE_MODEL_SEED_PREFIX, RESERVE_VAULT_SEED_PREFIX,
    TARGET_UTIL_END_BPS, TARGET_UTIL_START_BPS, TAYLOR_TERMS, VERSION,
};
use omnipair::state::{
    DebtDecreaseReason, FutarchyAuthority, Pair, RateModel, RevenueDistribution, RevenueRecipients,
    UserPosition, VaultBumps,
};
use omnipair::{
    ceil_div, construct_virtual_reserves_at_pessimistic_price, pessimistic_max_debt, slots_to_ms,
    taylor_exp, CPCurve,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use thiserror::Error;

pub const V1_NATIVE_ENGINE_SCHEMA_VERSION: u16 = 1;
pub const V1_NATIVE_ENGINE_ID: &str = "omnipair-v1-native-v1";
/// The only source tree whose native constructors/math are linked here.
pub const V1_NATIVE_SUPPORTED_SOURCE_RELEASE: &str = "omnipair-v0.10.5";

/// Known factual program revisions. Unknown revisions are never interpreted
/// using the newest available math.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V1ProgramRevisionV1 {
    OmnipairV1,
}

impl V1ProgramRevisionV1 {
    pub fn from_program_version(version: u8) -> Result<Self, V1EngineError> {
        match version {
            VERSION => Ok(Self::OmnipairV1),
            unsupported => Err(V1EngineError::UnsupportedProgramVersion(unsupported)),
        }
    }

    pub const fn program_version(self) -> u8 {
        match self {
            Self::OmnipairV1 => VERSION,
        }
    }
}

/// Exact address rule used by the deployed revision that created a rate-model
/// account.
///
/// Historical V1 initialized an explicit fresh signer account. Commit
/// `94ebe84` changed only this account constraint to a pair-derived PDA. The
/// pair account version remained `1`, so callers must supply revision evidence
/// instead of inferring the newer rule from the account version or slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V1RateModelAddressPolicyV1 {
    /// Exact key emitted by the initialize operation's direct-child
    /// `PairCreatedEvent`. Normalization additionally requires that key to
    /// equal initialize instruction account 5.
    HistoricalExplicitInitAccount,
    /// `find_program_address(["rate_model", pair], omnipair_program_id)`.
    PairDerivedPda,
}

/// Source rule used for add-collateral liquidation-CF transitions.
///
/// Pair account version `1` spans both observed behaviors, and historical
/// decoder dispatch has not yet been certified. A decision-grade replay may
/// therefore select between the two exact source transitions only when a
/// unique direct-child position event proves which transition occurred.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V1CollateralFactorTransitionPolicyV1 {
    /// Execute only the behavior of the linked, revision-certified source.
    SourceNative,
    /// On deposits, require an event witness that uniquely selects either
    /// preservation or native-formula recomputation of liquidation CF.
    EventWitnessedCollateralFactorTransition,
}

/// Whether replay may replace a linked-source post-state field with exact
/// selected-panel chain evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V1PostStateCorrectionPolicyV1 {
    /// Revision-dispatched/AuditGrade replay: linked native state is final.
    SourceNative,
    /// Provisional DecisionGrade replay may apply narrowly typed corrections
    /// that are independently bound to direct-child events and token CPIs.
    DecisionGradeEventAndTokenCpi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V1TokenSideV1 {
    Token0,
    Token1,
}

impl V1TokenSideV1 {
    pub const fn opposite(self) -> Self {
        match self {
            Self::Token0 => Self::Token1,
            Self::Token1 => Self::Token0,
        }
    }
}

/// Exact active global authority state at the market's initialize slot.
/// Every field is required; callers must join the relevant configuration
/// epochs before constructing this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CanonicalAuthorityInitializationV1 {
    pub version: u8,
    pub authority: Pubkey,
    pub swap_bps: u16,
    pub interest_bps: u16,
    pub futarchy_treasury: Pubkey,
    pub buybacks_vault: Pubkey,
    pub team_treasury: Pubkey,
    pub futarchy_treasury_bps: u16,
    pub buybacks_vault_bps: u16,
    pub team_treasury_bps: u16,
    pub global_reduce_only: bool,
    pub bump: u8,
}

/// Fully decoded initialize instruction joined to its PairCreated assertion
/// and active global authority configuration. Optional fields preserve the
/// original instruction's `Option` encoding; `initial_rate_bps` is required
/// even though PairCreatedEvent omitted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CanonicalInitializationInputV1 {
    pub program_version: u8,
    pub pair_account_version: u8,
    pub slot: u64,
    pub pair_key: Pubkey,
    pub token0: Pubkey,
    pub token1: Pubkey,
    pub lp_mint: Pubkey,
    pub rate_model_key: Pubkey,
    pub rate_model_address_policy: V1RateModelAddressPolicyV1,
    pub collateral_factor_transition_policy: V1CollateralFactorTransitionPolicyV1,
    pub post_state_correction_policy: V1PostStateCorrectionPolicyV1,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub swap_fee_bps: u16,
    pub half_life_ms: u64,
    pub fixed_cf_bps: Option<u16>,
    pub target_util_start_bps_arg: Option<u64>,
    pub target_util_end_bps_arg: Option<u64>,
    pub rate_half_life_ms_arg: Option<u64>,
    pub min_rate_bps_arg: Option<u64>,
    pub max_rate_bps_arg: Option<u64>,
    /// Required resolved instruction value (explicit or V1 default).
    pub initial_rate_bps: u64,
    pub params_hash: [u8; 32],
    pub gross_amount0: u64,
    pub gross_amount1: u64,
    pub vault_credit0: u64,
    pub vault_credit1: u64,
    pub minted_lp: u64,
    pub min_liquidity_out: u64,
    pub authority: V1CanonicalAuthorityInitializationV1,
}

/// Native accounts decoded at an exact historical checkpoint. There are no
/// fallback values: the caller must supply the pair, its rate model, authority,
/// and every position that exists at the checkpoint.
pub struct V1DecodedAccountsV1 {
    pub program_version: u8,
    pub pair_key: Pubkey,
    pub pair: Pair,
    pub rate_model_key: Pubkey,
    pub rate_model: RateModel,
    pub futarchy_authority: FutarchyAuthority,
    pub positions: Vec<(Pubkey, UserPosition)>,
    /// Physical token-account balances at the imported checkpoint.
    pub custody: V1CustodyCheckpointV1,
    /// Revenue reconstructed from deployment through the imported checkpoint.
    pub revenue: V1RevenueCheckpointV1,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CustodyCheckpointV1 {
    pub reserve_vault0: u64,
    pub reserve_vault1: u64,
    pub collateral_vault0: u64,
    pub collateral_vault1: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RevenueCheckpointV1 {
    pub lp_swap_fee0: u128,
    pub lp_swap_fee1: u128,
    pub protocol_swap_fee0: u128,
    pub protocol_swap_fee1: u128,
    /// Economic LP entitlement: native LP interest plus the nominal protocol
    /// portion that could not be paid from cash reserve at accrual time.
    pub lp_interest0: u128,
    pub lp_interest1: u128,
    /// Cash-covered protocol entitlement. Full nominal protocol interest is
    /// retained on each [`V1InterestAccrualV1`] source witness.
    pub protocol_interest0: u128,
    pub protocol_interest1: u128,
    /// V1-only LP revenue; kept outside the apples-to-apples core lanes.
    pub flashloan_fee0: u128,
    pub flashloan_fee1: u128,
    /// Realization of previously accrued protocol inventory, never new revenue.
    pub protocol_claim_realized0: u128,
    pub protocol_claim_realized1: u128,
}

impl V1RevenueCheckpointV1 {
    const ZERO: Self = Self {
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
    };
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1PairCheckpointV1 {
    pub token0: Pubkey,
    pub token1: Pubkey,
    pub lp_mint: Pubkey,
    pub rate_model: Pubkey,
    pub swap_fee_bps: u16,
    pub half_life: u64,
    pub fixed_cf_bps: Option<u16>,
    pub reserve0: u64,
    pub reserve1: u64,
    pub cash_reserve0: u64,
    pub cash_reserve1: u64,
    pub symmetric_price0_ema_nad: u64,
    pub directional_price0_ema_nad: u64,
    pub symmetric_price1_ema_nad: u64,
    pub directional_price1_ema_nad: u64,
    pub last_update_slot: u64,
    pub last_rate0_nad: u64,
    pub last_rate1_nad: u64,
    pub total_debt0: u64,
    pub total_debt1: u64,
    pub total_debt0_shares: u128,
    pub total_debt1_shares: u128,
    pub total_supply: u64,
    pub total_collateral0: u64,
    pub total_collateral1: u64,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub params_hash: [u8; 32],
    pub version: u8,
    pub bump: u8,
    pub reserve0_vault_bump: u8,
    pub reserve1_vault_bump: u8,
    pub collateral0_vault_bump: u8,
    pub collateral1_vault_bump: u8,
    pub reduce_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RateModelCheckpointV1 {
    pub key: Pubkey,
    pub exp_rate: u64,
    pub target_util_start_nad: u64,
    pub target_util_end_nad: u64,
    pub half_life_ms: u64,
    pub min_rate_nad: u64,
    pub max_rate_nad: u64,
    pub initial_rate_nad: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1AuthorityCheckpointV1 {
    pub version: u8,
    pub authority: Pubkey,
    pub swap_bps: u16,
    pub interest_bps: u16,
    pub futarchy_treasury: Pubkey,
    pub buybacks_vault: Pubkey,
    pub team_treasury: Pubkey,
    pub futarchy_treasury_bps: u16,
    pub buybacks_vault_bps: u16,
    pub team_treasury_bps: u16,
    pub global_reduce_only: bool,
    pub bump: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1PositionCheckpointV1 {
    pub owner: Pubkey,
    pub pair: Pubkey,
    pub collateral0_liquidation_cf_bps: u16,
    pub collateral1_liquidation_cf_bps: u16,
    pub collateral0: u64,
    pub collateral1: u64,
    pub debt0_shares: u128,
    pub debt1_shares: u128,
    pub bump: u8,
}

/// Full raw-unit assertion checkpoint. Emitted V1 update/position events should
/// be converted to one of these only after the decoder has joined all required
/// account fields; events are assertions, never an extra state transition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1NativeCheckpointV1 {
    pub schema_version: u16,
    pub engine_id: String,
    pub program_revision: V1ProgramRevisionV1,
    pub pair_key: Pubkey,
    pub pair: V1PairCheckpointV1,
    pub rate_model: V1RateModelCheckpointV1,
    pub authority: V1AuthorityCheckpointV1,
    pub positions: BTreeMap<Pubkey, V1PositionCheckpointV1>,
    pub custody: V1CustodyCheckpointV1,
    pub revenue: V1RevenueCheckpointV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1InterestAccrualV1 {
    pub slot_before: u64,
    pub slot_after: u64,
    pub elapsed_ms: u64,
    pub utilization0_nad: u64,
    pub utilization1_nad: u64,
    pub rate0_nad: u64,
    pub rate1_nad: u64,
    pub integral0_nad: u64,
    pub integral1_nad: u64,
    pub lp_interest0: u64,
    pub lp_interest1: u64,
    pub protocol_interest0: u64,
    pub protocol_interest1: u64,
    pub borrower_cost0: u64,
    pub borrower_cost1: u64,
    pub cash_covered_protocol_interest0: u64,
    pub cash_covered_protocol_interest1: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1SwapInputV1 {
    pub slot: u64,
    pub token_in: V1TokenSideV1,
    /// Instruction argument, before any Token-2022 transfer fee.
    pub gross_amount_in: u64,
    pub min_amount_out: u64,
    pub vault_credit_in: u64,
    pub vault_debit_out: u64,
    pub recipient_credit_out: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1SwapReceiptV1 {
    pub accrual: V1InterestAccrualV1,
    pub token_in: V1TokenSideV1,
    pub gross_amount_in: u64,
    pub amount_in_after_fee: u64,
    pub amount_out: u64,
    pub lp_fee: u64,
    pub protocol_fee: u64,
    /// Economic lanes derived from the active authority configuration. Early
    /// SwapEvent fee fields encoded total fee as LP and zero as protocol.
    pub accounted_lp_fee: u64,
    pub accounted_protocol_fee: u64,
    pub vault_credit_in: u64,
    pub vault_debit_out: u64,
    pub recipient_credit_out: u64,
    pub reserve0_after: u64,
    pub reserve1_after: u64,
    pub cash_reserve0_after: u64,
    pub cash_reserve1_after: u64,
    pub transition_correction: Option<V1SwapTransitionCorrectionV1>,
}

/// Exact values jointly witnessed by the unique direct-child SwapEvent and
/// the token-program CPIs already bound into the canonical operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1SwapTransitionWitnessV1 {
    pub event_token_in: V1TokenSideV1,
    pub event_gross_amount_in: u64,
    pub event_amount_in_after_fee: u64,
    pub event_amount_out: u64,
    pub event_lp_fee: u64,
    pub event_protocol_fee: u64,
    pub event_reserve0_after: u64,
    pub event_reserve1_after: u64,
    pub token_cpi_vault_credit_in: u64,
    pub token_cpi_vault_debit_out: u64,
    pub token_cpi_recipient_credit_out: u64,
}

/// Narrow provisional correction applied when a revision-unresolved linked
/// engine disagrees with exact chain witnesses. Total-debt shares are not
/// changed: the reserve/cash identity alignment is explicit evidence, not a
/// synthetic swap fee or interest-revenue booking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1SwapTransitionCorrectionV1 {
    pub source_native_amount_in_after_fee: u64,
    pub source_native_amount_out: u64,
    pub source_native_lp_fee: u64,
    pub source_native_protocol_fee: u64,
    pub source_native_reserve0_after: u64,
    pub source_native_reserve1_after: u64,
    pub witnessed_amount_in_after_fee: u64,
    pub witnessed_amount_out: u64,
    pub witnessed_lp_fee: u64,
    pub witnessed_protocol_fee: u64,
    pub accounted_lp_fee: u64,
    pub accounted_protocol_fee: u64,
    pub witnessed_reserve0_after: u64,
    pub witnessed_reserve1_after: u64,
    pub reserve_debt_alignment_side: V1TokenSideV1,
    pub total_debt_before_alignment: u64,
    pub total_debt_after_alignment: u64,
    pub total_debt_shares_unchanged: u128,
    pub protocol_inventory_before: u64,
    pub protocol_inventory_after: u64,
    pub protocol_inventory_delta: u64,
    pub reserve_model_inventory_delta: u64,
    pub output_protocol_inventory_before: u64,
    pub output_protocol_inventory_after: u64,
    pub output_protocol_inventory_delta: u64,
    pub output_reserve_model_inventory_delta: u64,
    pub k_before: u128,
    pub k_after: u128,
    pub k_delta: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1BorrowInputV1 {
    pub slot: u64,
    pub position: Pubkey,
    pub debt_token: V1TokenSideV1,
    /// `u64::MAX` preserves V1's borrow-remaining-capacity sentinel.
    pub requested_amount: u64,
    pub recipient_credit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1BorrowCapacityV1 {
    pub position: Pubkey,
    pub debt_token: V1TokenSideV1,
    pub collateral_amount: u64,
    pub actual_debt: u64,
    pub admissible_max_debt: u64,
    pub remaining_capacity: u64,
    pub cash_reserve: u64,
    pub actionable_remaining_capacity: u64,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1BorrowReceiptV1 {
    pub accrual: V1InterestAccrualV1,
    pub capacity_before: V1BorrowCapacityV1,
    pub borrowed_amount: u64,
    pub debt_after: u64,
    pub debt_shares_minted: u128,
    pub recipient_credit: u64,
    pub transition_correction: Option<V1BorrowTransitionCorrectionV1>,
}

/// Direct-child DebtDelta + Position fields cross-bound to token CPI custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1BorrowTransitionWitnessV1 {
    pub event_debt_token: V1TokenSideV1,
    pub event_borrowed_amount: u64,
    pub token_cpi_vault_debit: u64,
    pub token_cpi_recipient_credit: u64,
    pub event_position: Pubkey,
    pub event_collateral0: u64,
    pub event_collateral1: u64,
    pub event_debt0_shares: u128,
    pub event_debt1_shares: u128,
    pub event_collateral0_max_cf_bps: u16,
    pub event_collateral1_max_cf_bps: u16,
    pub event_collateral0_liquidation_cf_bps: u16,
    pub event_collateral1_liquidation_cf_bps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1BorrowTransitionCorrectionV1 {
    pub source_native_position_debt_shares_after: u128,
    pub witnessed_position_debt_shares_after: u128,
    pub source_native_total_debt_shares_after: u128,
    pub witnessed_total_debt_shares_after: u128,
    pub source_native_total_debt_after: u64,
    pub witnessed_total_debt_after: u64,
    pub witnessed_position_debt_share_delta: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RepayInputV1 {
    pub slot: u64,
    pub position: Pubkey,
    pub debt_token: V1TokenSideV1,
    /// `u64::MAX` preserves V1's repay-all sentinel.
    pub requested_amount: u64,
    /// Exact physical reserve-vault credit after any token transfer fee.
    pub vault_credit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RepayReceiptV1 {
    pub accrual: V1InterestAccrualV1,
    pub debt_before: u64,
    pub repaid_amount: u64,
    pub debt_after: u64,
    pub debt_shares_burned: u128,
    pub vault_credit: u64,
    pub transition_correction: Option<V1RepayTransitionCorrectionV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RepayTransitionWitnessV1 {
    pub event_debt_token: V1TokenSideV1,
    pub event_repaid_amount: u64,
    pub token_cpi_vault_credit: u64,
    pub event_position: Pubkey,
    pub event_collateral0: u64,
    pub event_collateral1: u64,
    pub event_debt0_shares: u128,
    pub event_debt1_shares: u128,
    pub event_collateral0_max_cf_bps: u16,
    pub event_collateral1_max_cf_bps: u16,
    pub event_collateral0_liquidation_cf_bps: u16,
    pub event_collateral1_liquidation_cf_bps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RepayTransitionCorrectionV1 {
    pub source_native_position_debt_before: u64,
    pub witnessed_position_debt_before: u64,
    pub modeled_position_debt_gap: u64,
    pub source_native_repaid_amount: u64,
    pub witnessed_repaid_amount: u64,
    pub source_native_position_debt_shares_after: u128,
    pub witnessed_position_debt_shares_after: u128,
    pub source_native_total_debt_shares_after: u128,
    pub witnessed_total_debt_shares_after: u128,
    pub source_native_total_debt_after: u64,
    pub witnessed_total_debt_after: u64,
    pub witnessed_position_debt_share_delta: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1AddLiquidityInputV1 {
    pub slot: u64,
    /// Exact instruction maxima used by V1's native proportional mint math.
    pub amount0_in: u64,
    pub amount1_in: u64,
    /// Physical vault credits observed after token-program execution.
    pub vault_credit0: u64,
    pub vault_credit1: u64,
    pub min_liquidity_out: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1AddLiquidityReceiptV1 {
    pub accrual: V1InterestAccrualV1,
    pub amount0_used: u64,
    pub amount1_used: u64,
    pub vault_credit0: u64,
    pub vault_credit1: u64,
    pub liquidity_minted: u64,
    pub total_supply_after: u64,
    pub supply_correction: Option<V1LpMintSupplyCorrectionV1>,
    pub cash_inventory_correction: Option<V1AddLiquidityCashInventoryCorrectionV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1LpMintSupplyWitnessV1 {
    pub event_amount0_used: u64,
    pub event_amount1_used: u64,
    /// Exact direct-child MintEvent amount.
    pub event_liquidity_minted: u64,
    /// Exact MintTo CPI amount bound by canonical normalization.
    pub token_cpi_supply_delta: u64,
    /// Exact transfer-CPI credits bound by canonical normalization.
    pub token_cpi_vault_credit0: u64,
    pub token_cpi_vault_credit1: u64,
    /// Optional exact post-operation cash checkpoints from the unique
    /// direct-child LP-position event.
    pub event_cash_reserve0_after: Option<u64>,
    pub event_cash_reserve1_after: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1LpMintSupplyCorrectionV1 {
    pub source_native_amount0_used: u64,
    pub source_native_amount1_used: u64,
    pub source_native_liquidity_minted: u64,
    pub witnessed_amount0_used: u64,
    pub witnessed_amount1_used: u64,
    pub witnessed_liquidity_minted: u64,
    pub total_supply_before: u64,
    pub total_supply_after: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1AddLiquidityCashInventoryCorrectionV1 {
    pub source_native_cash_reserve0_after: u64,
    pub source_native_cash_reserve1_after: u64,
    pub witnessed_cash_reserve0_after: u64,
    pub witnessed_cash_reserve1_after: u64,
    pub source_native_total_debt0_after: u64,
    pub source_native_total_debt1_after: u64,
    pub witnessed_total_debt0_after: u64,
    pub witnessed_total_debt1_after: u64,
    pub source_native_protocol_interest0_after: u128,
    pub source_native_protocol_interest1_after: u128,
    pub witnessed_protocol_interest0_after: u128,
    pub witnessed_protocol_interest1_after: u128,
    pub source_native_cash_covered_protocol_interest0: u64,
    pub source_native_cash_covered_protocol_interest1: u64,
    /// Non-negative inventory-backed protocol-interest catch-up attributed at
    /// this checkpoint. A negative one-atom inventory residual is retained
    /// separately and never rewritten into economic revenue.
    pub witnessed_cash_covered_protocol_interest0: u64,
    pub witnessed_cash_covered_protocol_interest1: u64,
    pub protocol_interest_inventory_rounding_atoms0: i128,
    pub protocol_interest_inventory_rounding_atoms1: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RemoveLiquidityInputV1 {
    pub slot: u64,
    pub liquidity_burned: u64,
    pub vault_debit0: u64,
    pub vault_debit1: u64,
    pub recipient_credit0: u64,
    pub recipient_credit1: u64,
    pub min_amount0_out: u64,
    pub min_amount1_out: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RemoveLiquidityReceiptV1 {
    pub accrual: V1InterestAccrualV1,
    pub liquidity_burned: u64,
    pub amount0_gross: u64,
    pub amount1_gross: u64,
    pub withdrawal_fee0: u64,
    pub withdrawal_fee1: u64,
    pub amount0_out: u64,
    pub amount1_out: u64,
    pub recipient_credit0: u64,
    pub recipient_credit1: u64,
    pub total_supply_after: u64,
    pub output_correction: Option<V1RemoveLiquidityOutputCorrectionV1>,
    pub cash_inventory_correction: Option<V1RemoveLiquidityCashInventoryCorrectionV1>,
    pub debt_coverage_exception: Option<V1RemoveLiquidityDebtCoverageExceptionV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RemoveLiquidityOutputCorrectionV1 {
    pub source_native_amount0_out: u64,
    pub source_native_amount1_out: u64,
    pub witnessed_amount0_out: u64,
    pub witnessed_amount1_out: u64,
    pub source_native_reserve0_after: u64,
    pub source_native_reserve1_after: u64,
    pub witnessed_reserve0_after: u64,
    pub witnessed_reserve1_after: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RemoveLiquidityCashWitnessV1 {
    pub event_liquidity_burned: u64,
    pub event_amount0_out: u64,
    pub event_amount1_out: u64,
    pub event_cash_reserve0_after: u64,
    pub event_cash_reserve1_after: u64,
    pub token_cpi_vault_debit0: u64,
    pub token_cpi_vault_debit1: u64,
    pub token_cpi_recipient_credit0: u64,
    pub token_cpi_recipient_credit1: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RemoveLiquidityCashInventoryCorrectionV1 {
    pub source_native_cash_reserve0_after: u64,
    pub source_native_cash_reserve1_after: u64,
    pub witnessed_cash_reserve0_after: u64,
    pub witnessed_cash_reserve1_after: u64,
    pub source_native_total_debt0_after: u64,
    pub source_native_total_debt1_after: u64,
    pub witnessed_total_debt0_after: u64,
    pub witnessed_total_debt1_after: u64,
    /// Signed event-minus-native cash checkpoint residual. It is retained as
    /// model/timing evidence and never rewritten into revenue.
    pub cash_checkpoint_rounding_atoms0: i128,
    pub cash_checkpoint_rounding_atoms1: i128,
}

/// Exact post-withdraw debt-coverage failure retained only when a unique
/// direct-child Burn event, both output token CPIs, and the LP cash checkpoint
/// bind the historical post-state under the provisional DecisionGrade policy.
/// Source-native/Audit replay still rejects the same transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RemoveLiquidityDebtCoverageExceptionV1 {
    pub required_token1_for_debt0: u64,
    pub buffered_token1_for_debt0: u128,
    pub post_reserve1: u64,
    pub token1_shortfall_atoms: u128,
    pub required_token0_for_debt1: u64,
    pub buffered_token0_for_debt1: u128,
    pub post_reserve0: u64,
    pub token0_shortfall_atoms: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CollateralInputV1 {
    pub slot: u64,
    pub position: Pubkey,
    pub collateral_token: V1TokenSideV1,
    pub amount: u64,
    /// Exact instruction argument, preserving remove-all `u64::MAX`.
    pub requested_amount: u64,
    pub deposit: bool,
    /// Deposit credit or withdrawal debit measured on the collateral vault.
    pub collateral_vault_amount: u64,
    /// Sender debit on deposit or recipient credit on withdrawal.
    pub counterparty_amount: u64,
}

/// Exact collateral-factor fields from the unique direct-child position
/// assertion emitted by an add-collateral instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1PositionCollateralFactorWitnessV1 {
    pub collateral0_max_cf_bps: u16,
    pub collateral1_max_cf_bps: u16,
    pub collateral0_liquidation_cf_bps: u16,
    pub collateral1_liquidation_cf_bps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CollateralDepositFactorCorrectionV1 {
    pub collateral_token: V1TokenSideV1,
    pub preserve_candidate: V1PositionCollateralFactorWitnessV1,
    pub recompute_candidate: V1PositionCollateralFactorWitnessV1,
    pub witnessed: V1PositionCollateralFactorWitnessV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CollateralWithdrawalWitnessV1 {
    pub event_position: Pubkey,
    pub event_amount: u64,
    pub token_cpi_collateral_vault_debit: u64,
    pub token_cpi_recipient_credit: u64,
    pub event_collateral0: u64,
    pub event_collateral1: u64,
    pub event_debt0_shares: u128,
    pub event_debt1_shares: u128,
    pub event_collateral0_max_cf_bps: u16,
    pub event_collateral1_max_cf_bps: u16,
    pub event_collateral0_liquidation_cf_bps: u16,
    pub event_collateral1_liquidation_cf_bps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CollateralWithdrawalFactorCorrectionV1 {
    pub collateral_token: V1TokenSideV1,
    pub requested_amount: u64,
    pub source_native_resolved_amount: u64,
    pub witnessed_resolved_amount: u64,
    pub source_native_collateral0_max_cf_bps: u16,
    pub source_native_collateral1_max_cf_bps: u16,
    pub source_native_collateral0_liquidation_cf_bps: u16,
    pub source_native_collateral1_liquidation_cf_bps: u16,
    pub witnessed_collateral0_max_cf_bps: u16,
    pub witnessed_collateral1_max_cf_bps: u16,
    pub witnessed_collateral0_liquidation_cf_bps: u16,
    pub witnessed_collateral1_liquidation_cf_bps: u16,
    pub post_capacity_exception: Option<V1CollateralWithdrawalPostCapacityExceptionV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CollateralWithdrawalPostCapacityExceptionV1 {
    pub post_withdraw_debt: u64,
    pub admissible_max_debt: u64,
    pub excess_debt_atoms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CollateralReceiptV1 {
    pub accrual: V1InterestAccrualV1,
    pub deposited: bool,
    pub collateral_token: V1TokenSideV1,
    pub accounted_amount: u64,
    pub collateral_vault_amount: u64,
    pub counterparty_amount: u64,
    pub collateral0_after: u64,
    pub collateral1_after: u64,
    pub deposit_factor_correction: Option<V1CollateralDepositFactorCorrectionV1>,
    pub withdrawal_factor_correction: Option<V1CollateralWithdrawalFactorCorrectionV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1LiquidationInputV1 {
    pub slot: u64,
    pub position: Pubkey,
    pub debt_token: V1TokenSideV1,
    /// Exact physical credit of seized collateral into the reserve vault.
    pub reserve_vault_credit: u64,
    /// Exact credit received by the liquidator after transfer fees.
    pub liquidator_recipient_credit: u64,
}

/// Exact direct-child Liquidation event joined to the instruction-scoped
/// collateral-vault debit, reserve-vault credit, and liquidator credit CPIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1LiquidationTransitionWitnessV1 {
    pub event_position: Pubkey,
    pub event_debt_token: V1TokenSideV1,
    pub event_debt_written_off: u64,
    pub event_collateral_seized: u64,
    pub event_liquidator_incentive: u64,
    pub event_shortfall: u128,
    pub token_cpi_collateral_vault_debit: u64,
    pub token_cpi_reserve_vault_credit: u64,
    pub token_cpi_liquidator_recipient_credit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1LiquidationTransitionCorrectionV1 {
    pub collateral_token: V1TokenSideV1,
    pub source_native_debt_written_off: u64,
    pub witnessed_debt_written_off: u64,
    pub debt_shares_written_off: u128,
    pub source_native_collateral_seized: u64,
    pub witnessed_collateral_seized: u64,
    pub source_native_collateral_to_reserves: u64,
    pub witnessed_collateral_to_reserves: u64,
    pub source_native_liquidator_incentive: u64,
    pub witnessed_liquidator_incentive: u64,
    pub source_native_debt_reserve_after: u64,
    pub witnessed_debt_reserve_after: u64,
    pub source_native_total_debt_after: u64,
    pub witnessed_total_debt_after: u64,
    pub source_native_collateral_reserve_after: u64,
    pub witnessed_collateral_reserve_after: u64,
    pub source_native_collateral_cash_after: u64,
    pub witnessed_collateral_cash_after: u64,
    pub source_native_total_collateral_after: u64,
    pub witnessed_total_collateral_after: u64,
    pub source_native_position_collateral_after: u64,
    pub witnessed_position_collateral_after: u64,
    pub source_native_collateral_vault_after: u64,
    pub witnessed_collateral_vault_after: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1LiquidationReceiptV1 {
    pub accrual: V1InterestAccrualV1,
    pub debt_token: V1TokenSideV1,
    pub debt_written_off: u64,
    pub debt_shares_written_off: u128,
    pub collateral_seized: u64,
    pub collateral_to_reserves: u64,
    pub reserve_vault_credit: u64,
    pub liquidator_incentive: u64,
    pub liquidator_recipient_credit: u64,
    pub collateral_price_nad: u64,
    pub shortfall: u128,
    pub k_before: u128,
    pub k_after: u128,
    pub transition_correction: Option<V1LiquidationTransitionCorrectionV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1FlashloanInputV1 {
    pub slot: u64,
    pub amount0: u64,
    pub amount1: u64,
    /// Exact net vault balance increases after the callback, including fees
    /// and any overpayment. Canonical history must not assume fee-only return.
    pub net_vault_credit0: u64,
    pub net_vault_credit1: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1FlashloanReceiptV1 {
    pub accrual: V1InterestAccrualV1,
    pub amount0: u64,
    pub amount1: u64,
    pub fee0: u64,
    pub fee1: u64,
    pub net_vault_credit0: u64,
    pub net_vault_credit1: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1ProtocolClaimInputV1 {
    pub slot: u64,
    pub amount0: u64,
    pub amount1: u64,
}

/// Exact DecisionGrade reconciliation of a historical protocol-inventory
/// realization whose instruction-scoped token CPIs debit less than the
/// linked source revision's modeled full-claim amount.
///
/// The pair remains at its source-native post-accrual state. Only physical
/// custody and cumulative realized-claim accounting follow the CPI witness;
/// the residual remains in the reserve vault for a later claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1ProtocolClaimRealizationCorrectionV1 {
    pub source_native_claimable0: u64,
    pub source_native_claimable1: u64,
    pub witnessed_realized0: u64,
    pub witnessed_realized1: u64,
    pub modeled_residual0: u64,
    pub modeled_residual1: u64,
    pub reserve_vault0_before_claim: u64,
    pub reserve_vault1_before_claim: u64,
    pub reserve_vault0_after_claim: u64,
    pub reserve_vault1_after_claim: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1ProtocolClaimReceiptV1 {
    pub accrual: V1InterestAccrualV1,
    /// Exact amount physically realized by this claim operation. In
    /// SourceNative mode this equals the full modeled claimable inventory.
    pub claimable0: u64,
    pub claimable1: u64,
    pub futarchy_amount0: u64,
    pub futarchy_amount1: u64,
    pub buybacks_amount0: u64,
    pub buybacks_amount1: u64,
    pub team_amount0: u64,
    pub team_amount1: u64,
    pub realization_correction: Option<V1ProtocolClaimRealizationCorrectionV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1InitializationAssertionV1 {
    pub slot: u64,
    pub token0: Pubkey,
    pub token1: Pubkey,
    pub gross_amount0: u64,
    pub gross_amount1: u64,
    pub vault_credit0: u64,
    pub vault_credit1: u64,
    pub minted_lp: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum V1EngineError {
    #[error("unsupported V1 program version {0}")]
    UnsupportedProgramVersion(u8),
    #[error("unsupported V1 pair account version {0}")]
    UnsupportedPairVersion(u8),
    #[error("unsupported V1 futarchy authority version {0}")]
    UnsupportedAuthorityVersion(u8),
    #[error("decoded rate-model key does not match the pair account")]
    RateModelKeyMismatch,
    #[error("canonical {account} PDA is {actual}, expected {expected} for the linked V1 program")]
    CanonicalPdaMismatch {
        account: &'static str,
        expected: Pubkey,
        actual: Pubkey,
    },
    #[error("canonical {account} bump is {actual}, expected {expected}")]
    CanonicalBumpMismatch {
        account: &'static str,
        expected: u8,
        actual: u8,
    },
    #[error("canonical initialize params_hash does not match the exact instruction arguments")]
    CanonicalParamsHashMismatch,
    #[error("decoded position {position} belongs to a different pair")]
    PositionPairMismatch { position: Pubkey },
    #[error("decoded position {0} appears more than once")]
    DuplicatePosition(Pubkey),
    #[error("position {0} was not present in the decoded checkpoint")]
    MissingPosition(Pubkey),
    #[error("slot {requested} precedes the current replay slot {current}")]
    NonMonotonicSlot { current: u64, requested: u64 },
    #[error("amount must be greater than zero")]
    AmountZero,
    #[error("user balance {available} is below requested input {required}")]
    InsufficientUserBalance { available: u64, required: u64 },
    #[error("swap output {actual} is below min-out {minimum}")]
    SlippageExceeded { minimum: u64, actual: u64 },
    #[error("token{token} cash reserve {available} is below required output {required}")]
    InsufficientCash {
        token: u8,
        available: u64,
        required: u64,
    },
    #[error("V1 borrowing is disabled by decoded reduce-only state")]
    ReduceOnly,
    #[error("requested post-borrow debt {requested} exceeds admissible debt {maximum}")]
    BorrowingPowerExceeded { requested: u64, maximum: u64 },
    #[error("requested repayment {requested} exceeds position debt {debt}")]
    InsufficientDebt { requested: u64, debt: u64 },
    #[error("position has no debt on the requested side")]
    ZeroDebt,
    #[error("requested LP burn {requested} exceeds total supply {available}")]
    InsufficientLpSupply { available: u64, requested: u64 },
    #[error("requested collateral withdrawal {requested} exceeds position collateral {available}")]
    InsufficientCollateral { available: u64, requested: u64 },
    #[error("post-transition debt coverage is insufficient")]
    InsufficientDebtCoverage,
    #[error("position is not undercollateralized")]
    NotUndercollateralized,
    #[error("canonical custody delta for {field} is {actual}, expected {expected}")]
    CustodyDeltaMismatch {
        field: &'static str,
        expected: u64,
        actual: u64,
    },
    #[error("physical {vault} balance {available} is below debit {required}")]
    InsufficientVaultBalance {
        vault: &'static str,
        available: u64,
        required: u64,
    },
    #[error("invalid or unsupported configuration field {0}")]
    InvalidConfiguration(&'static str),
    #[error("V1 invariant decreased from {before} to {after}")]
    BrokenInvariant { before: u128, after: u128 },
    #[error("native V1 math failed during {operation}: {detail}")]
    NativeMath {
        operation: &'static str,
        detail: String,
    },
    #[error("arithmetic overflow during {0}")]
    ArithmeticOverflow(&'static str),
    #[error(
        "direct-child position assertion did not uniquely prove the add-collateral CF transition (preserve={preserve_matches}, recompute={recompute_matches})"
    )]
    UnprovenCollateralFactorTransition {
        preserve_matches: bool,
        recompute_matches: bool,
    },
    #[error("factual V1 assertion checkpoint does not match replayed state")]
    CheckpointMismatch,
    #[error("factual V1 assertion checkpoint field {0} does not match replayed state")]
    CheckpointMismatchField(&'static str),
    #[error(
        "factual V1 assertion checkpoint field {field} mismatched: event {event}, replayed {replayed}"
    )]
    CheckpointValueMismatch {
        field: &'static str,
        event: u128,
        replayed: u128,
    },
}

#[derive(Clone)]
struct V1AuthorityStateV1 {
    version: u8,
    authority: Pubkey,
    swap_bps: u16,
    interest_bps: u16,
    recipients: RevenueRecipients,
    distribution: RevenueDistribution,
    global_reduce_only: bool,
    bump: u8,
}

pub struct V1NativeEngine {
    revision: V1ProgramRevisionV1,
    collateral_factor_transition_policy: V1CollateralFactorTransitionPolicyV1,
    post_state_correction_policy: V1PostStateCorrectionPolicyV1,
    pair_key: Pubkey,
    pair: Pair,
    rate_model_key: Pubkey,
    rate_model: RateModel,
    authority: V1AuthorityStateV1,
    positions: BTreeMap<Pubkey, UserPosition>,
    custody: V1CustodyCheckpointV1,
    revenue: V1RevenueCheckpointV1,
}

impl V1NativeEngine {
    /// Reconstructs genesis state without a historical account snapshot.
    ///
    /// This constructor is version-pinned to [`V1_NATIVE_SUPPORTED_SOURCE_RELEASE`].
    /// It invokes that source tree's native `RateModel`, `Pair`, and
    /// `FutarchyAuthority` initializers, validates every deterministic PDA and
    /// initialization guard that affects state, and initializes custody from
    /// measured vault credits. No configuration default is chosen by the
    /// adapter: optional instruction arguments are supplied with their exact
    /// original presence, while the otherwise-unemitted resolved initial rate
    /// is mandatory.
    pub fn from_canonical_initialization(
        input: V1CanonicalInitializationInputV1,
    ) -> Result<Self, V1EngineError> {
        let revision = V1ProgramRevisionV1::from_program_version(input.program_version)?;
        if input.pair_account_version != revision.program_version() {
            return Err(V1EngineError::UnsupportedPairVersion(
                input.pair_account_version,
            ));
        }
        if input.authority.version != FutarchyAuthority::CURRENT_VERSION {
            return Err(V1EngineError::UnsupportedAuthorityVersion(
                input.authority.version,
            ));
        }
        if input.token0 >= input.token1 {
            return Err(V1EngineError::InvalidConfiguration("token_order"));
        }
        if input.swap_fee_bps > BPS_DENOMINATOR / 2 {
            return Err(V1EngineError::InvalidConfiguration("swap_fee_bps"));
        }
        if !(MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(&input.half_life_ms) {
            return Err(V1EngineError::InvalidConfiguration("half_life_ms"));
        }
        if input
            .fixed_cf_bps
            .is_some_and(|value| !(100..=BPS_DENOMINATOR).contains(&value))
        {
            return Err(V1EngineError::InvalidConfiguration("fixed_cf_bps"));
        }
        if input.gross_amount0 == 0 || input.gross_amount1 == 0 {
            return Err(V1EngineError::AmountZero);
        }

        let target_util_start_bps = input
            .target_util_start_bps_arg
            .unwrap_or(TARGET_UTIL_START_BPS);
        let target_util_end_bps = input.target_util_end_bps_arg.unwrap_or(TARGET_UTIL_END_BPS);
        let rate_half_life_ms = input
            .rate_half_life_ms_arg
            .unwrap_or(DEFAULT_RATE_HALF_LIFE_MS);
        let min_rate_bps = input.min_rate_bps_arg.unwrap_or(DEFAULT_MIN_RATE_BPS);
        let max_rate_bps = input.max_rate_bps_arg.unwrap_or(DEFAULT_MAX_RATE_BPS);
        if !RateModel::validate_util_bounds(target_util_start_bps, target_util_end_bps) {
            return Err(V1EngineError::InvalidConfiguration(
                "rate_model_utilization",
            ));
        }
        if !RateModel::validate_rate_params(
            rate_half_life_ms,
            min_rate_bps,
            max_rate_bps,
            input.initial_rate_bps,
        ) {
            return Err(V1EngineError::InvalidConfiguration("rate_model_parameters"));
        }

        let expected_hash = canonical_initialize_params_hash(&input);
        if input.params_hash != expected_hash {
            return Err(V1EngineError::CanonicalParamsHashMismatch);
        }
        let (expected_pair, pair_bump) = Pubkey::find_program_address(
            &[
                PAIR_SEED_PREFIX,
                input.token0.as_ref(),
                input.token1.as_ref(),
                input.params_hash.as_ref(),
            ],
            &omnipair::ID,
        );
        if input.pair_key != expected_pair {
            return Err(V1EngineError::CanonicalPdaMismatch {
                account: "pair",
                expected: expected_pair,
                actual: input.pair_key,
            });
        }
        if input.rate_model_address_policy == V1RateModelAddressPolicyV1::PairDerivedPda {
            let (expected_rate_model, _) = Pubkey::find_program_address(
                &[RATE_MODEL_SEED_PREFIX, input.pair_key.as_ref()],
                &omnipair::ID,
            );
            if input.rate_model_key != expected_rate_model {
                return Err(V1EngineError::CanonicalPdaMismatch {
                    account: "rate_model",
                    expected: expected_rate_model,
                    actual: input.rate_model_key,
                });
            }
        }
        let (_, authority_bump) =
            Pubkey::find_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX], &omnipair::ID);
        if input.authority.bump != authority_bump {
            return Err(V1EngineError::CanonicalBumpMismatch {
                account: "futarchy_authority",
                expected: authority_bump,
                actual: input.authority.bump,
            });
        }
        let reserve0_bump = Pubkey::find_program_address(
            &[
                RESERVE_VAULT_SEED_PREFIX,
                input.pair_key.as_ref(),
                input.token0.as_ref(),
            ],
            &omnipair::ID,
        )
        .1;
        let reserve1_bump = Pubkey::find_program_address(
            &[
                RESERVE_VAULT_SEED_PREFIX,
                input.pair_key.as_ref(),
                input.token1.as_ref(),
            ],
            &omnipair::ID,
        )
        .1;
        let collateral0_bump = Pubkey::find_program_address(
            &[
                COLLATERAL_VAULT_SEED_PREFIX,
                input.pair_key.as_ref(),
                input.token0.as_ref(),
            ],
            &omnipair::ID,
        )
        .1;
        let collateral1_bump = Pubkey::find_program_address(
            &[
                COLLATERAL_VAULT_SEED_PREFIX,
                input.pair_key.as_ref(),
                input.token1.as_ref(),
            ],
            &omnipair::ID,
        )
        .1;

        let rate_model = RateModel::new(
            target_util_start_bps,
            target_util_end_bps,
            rate_half_life_ms,
            min_rate_bps,
            max_rate_bps,
            input.initial_rate_bps,
        );
        let mut pair = Pair::initialize(
            input.token0,
            input.token1,
            input.lp_mint,
            input.token0_decimals,
            input.token1_decimals,
            input.rate_model_key,
            input.swap_fee_bps,
            input.half_life_ms,
            input.fixed_cf_bps,
            input.slot,
            input.params_hash,
            input.pair_account_version,
            pair_bump,
            VaultBumps {
                reserve0: reserve0_bump,
                reserve1: reserve1_bump,
                collateral0: collateral0_bump,
                collateral1: collateral1_bump,
            },
            rate_model.initial_rate,
        );
        let liquidity = integer_sqrt(
            (input.gross_amount0 as u128)
                .checked_mul(input.gross_amount1 as u128)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "initial liquidity product",
                ))?,
        )
        .checked_sub(MIN_LIQUIDITY as u128)
        .ok_or(V1EngineError::ArithmeticOverflow(
            "initial minimum liquidity",
        ))?;
        let liquidity = u64::try_from(liquidity)
            .map_err(|_| V1EngineError::ArithmeticOverflow("initial LP conversion"))?;
        if input.minted_lp != liquidity {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "initial LP minted",
                expected: liquidity,
                actual: input.minted_lp,
            });
        }
        if liquidity < input.min_liquidity_out {
            return Err(V1EngineError::SlippageExceeded {
                minimum: input.min_liquidity_out,
                actual: liquidity,
            });
        }
        pair.reserve0 = input.gross_amount0;
        pair.reserve1 = input.gross_amount1;
        pair.cash_reserve0 = input.gross_amount0;
        pair.cash_reserve1 = input.gross_amount1;
        pair.total_supply = MIN_LIQUIDITY
            .checked_add(liquidity)
            .ok_or(V1EngineError::ArithmeticOverflow("initial LP supply"))?;
        pair.last_price0_ema.symmetric = pair.spot_price0_nad();
        pair.last_price0_ema.directional = pair.spot_price0_nad();
        pair.last_price1_ema.symmetric = pair.spot_price1_nad();
        pair.last_price1_ema.directional = pair.spot_price1_nad();

        if input.authority.swap_bps > BPS_DENOMINATOR
            || input.authority.interest_bps > BPS_DENOMINATOR
        {
            return Err(V1EngineError::InvalidConfiguration(
                "authority_revenue_share",
            ));
        }
        let mut futarchy_authority = FutarchyAuthority::initialize(
            input.authority.authority,
            input.authority.swap_bps,
            input.authority.interest_bps,
            input.authority.futarchy_treasury,
            input.authority.buybacks_vault,
            input.authority.team_treasury,
            input.authority.futarchy_treasury_bps,
            input.authority.buybacks_vault_bps,
            input.authority.team_treasury_bps,
            input.authority.bump,
        )
        .map_err(|error| V1EngineError::NativeMath {
            operation: "FutarchyAuthority::initialize",
            detail: error.to_string(),
        })?;
        futarchy_authority.global_reduce_only = input.authority.global_reduce_only;
        let mut engine = Self::from_decoded_accounts(V1DecodedAccountsV1 {
            program_version: input.program_version,
            pair_key: input.pair_key,
            pair,
            rate_model_key: input.rate_model_key,
            rate_model,
            futarchy_authority,
            positions: Vec::new(),
            custody: V1CustodyCheckpointV1 {
                reserve_vault0: input.vault_credit0,
                reserve_vault1: input.vault_credit1,
                collateral_vault0: 0,
                collateral_vault1: 0,
            },
            revenue: V1RevenueCheckpointV1::ZERO,
        })?;
        engine.collateral_factor_transition_policy = input.collateral_factor_transition_policy;
        engine.post_state_correction_policy = input.post_state_correction_policy;
        Ok(engine)
    }

    pub fn from_decoded_accounts(accounts: V1DecodedAccountsV1) -> Result<Self, V1EngineError> {
        let revision = V1ProgramRevisionV1::from_program_version(accounts.program_version)?;
        if accounts.pair.version != revision.program_version() {
            return Err(V1EngineError::UnsupportedPairVersion(accounts.pair.version));
        }
        if accounts.futarchy_authority.version != FutarchyAuthority::CURRENT_VERSION {
            return Err(V1EngineError::UnsupportedAuthorityVersion(
                accounts.futarchy_authority.version,
            ));
        }
        if accounts.pair.rate_model != accounts.rate_model_key {
            return Err(V1EngineError::RateModelKeyMismatch);
        }

        let mut positions = BTreeMap::new();
        for (key, position) in accounts.positions {
            if position.pair != accounts.pair_key {
                return Err(V1EngineError::PositionPairMismatch { position: key });
            }
            match positions.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(position);
                }
                Entry::Occupied(_) => return Err(V1EngineError::DuplicatePosition(key)),
            }
        }

        Ok(Self {
            revision,
            collateral_factor_transition_policy: V1CollateralFactorTransitionPolicyV1::SourceNative,
            post_state_correction_policy: V1PostStateCorrectionPolicyV1::SourceNative,
            pair_key: accounts.pair_key,
            pair: accounts.pair,
            rate_model_key: accounts.rate_model_key,
            rate_model: accounts.rate_model,
            authority: V1AuthorityStateV1 {
                version: accounts.futarchy_authority.version,
                authority: accounts.futarchy_authority.authority,
                swap_bps: accounts.futarchy_authority.revenue_share.swap_bps,
                interest_bps: accounts.futarchy_authority.revenue_share.interest_bps,
                recipients: accounts.futarchy_authority.recipients.clone(),
                distribution: accounts.futarchy_authority.revenue_distribution.clone(),
                global_reduce_only: accounts.futarchy_authority.global_reduce_only,
                bump: accounts.futarchy_authority.bump,
            },
            positions,
            custody: accounts.custody,
            revenue: accounts.revenue,
        })
    }

    pub const fn engine_id(&self) -> &'static str {
        V1_NATIVE_ENGINE_ID
    }

    pub const fn revision(&self) -> V1ProgramRevisionV1 {
        self.revision
    }

    pub const fn collateral_factor_transition_policy(
        &self,
    ) -> V1CollateralFactorTransitionPolicyV1 {
        self.collateral_factor_transition_policy
    }

    pub const fn post_state_correction_policy(&self) -> V1PostStateCorrectionPolicyV1 {
        self.post_state_correction_policy
    }

    pub fn pair(&self) -> &Pair {
        &self.pair
    }

    pub fn position(&self, key: &Pubkey) -> Option<&UserPosition> {
        self.positions.get(key)
    }

    pub const fn pair_key(&self) -> Pubkey {
        self.pair_key
    }

    pub const fn custody(&self) -> V1CustodyCheckpointV1 {
        self.custody
    }

    pub const fn revenue(&self) -> V1RevenueCheckpointV1 {
        self.revenue
    }

    pub fn try_fork(&self) -> Self {
        Self {
            revision: self.revision,
            collateral_factor_transition_policy: self.collateral_factor_transition_policy,
            post_state_correction_policy: self.post_state_correction_policy,
            pair_key: self.pair_key,
            pair: copy_pair(&self.pair),
            rate_model_key: self.rate_model_key,
            rate_model: copy_rate_model(&self.rate_model),
            authority: self.authority.clone(),
            positions: self
                .positions
                .iter()
                .map(|(key, position)| (*key, copy_position(position)))
                .collect(),
            custody: self.custody,
            revenue: self.revenue,
        }
    }

    /// Adds an account created after the initial checkpoint. Its complete
    /// decoded state is required; existing positions cannot be overwritten.
    pub fn insert_decoded_position(
        &mut self,
        key: Pubkey,
        position: UserPosition,
    ) -> Result<(), V1EngineError> {
        if position.pair != self.pair_key {
            return Err(V1EngineError::PositionPairMismatch { position: key });
        }
        match self.positions.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(position);
                Ok(())
            }
            Entry::Occupied(_) => Err(V1EngineError::DuplicatePosition(key)),
        }
    }

    pub fn checkpoint(&self) -> V1NativeCheckpointV1 {
        V1NativeCheckpointV1 {
            schema_version: V1_NATIVE_ENGINE_SCHEMA_VERSION,
            engine_id: V1_NATIVE_ENGINE_ID.to_owned(),
            program_revision: self.revision,
            pair_key: self.pair_key,
            pair: pair_checkpoint(&self.pair),
            rate_model: rate_model_checkpoint(self.rate_model_key, &self.rate_model),
            authority: V1AuthorityCheckpointV1 {
                version: self.authority.version,
                authority: self.authority.authority,
                swap_bps: self.authority.swap_bps,
                interest_bps: self.authority.interest_bps,
                futarchy_treasury: self.authority.recipients.futarchy_treasury,
                buybacks_vault: self.authority.recipients.buybacks_vault,
                team_treasury: self.authority.recipients.team_treasury,
                futarchy_treasury_bps: self.authority.distribution.futarchy_treasury_bps,
                buybacks_vault_bps: self.authority.distribution.buybacks_vault_bps,
                team_treasury_bps: self.authority.distribution.team_treasury_bps,
                global_reduce_only: self.authority.global_reduce_only,
                bump: self.authority.bump,
            },
            positions: self
                .positions
                .iter()
                .map(|(key, position)| (*key, position_checkpoint(position)))
                .collect(),
            custody: self.custody,
            revenue: self.revenue,
        }
    }

    pub fn assert_checkpoint(&self, expected: &V1NativeCheckpointV1) -> Result<(), V1EngineError> {
        if &self.checkpoint() == expected {
            Ok(())
        } else {
            Err(V1EngineError::CheckpointMismatch)
        }
    }

    /// Reconstructs the deployment transition with native V1 initialization
    /// fields and asserts the imported post-instruction account byte-domain.
    pub fn assert_native_initialization(
        &self,
        input: V1InitializationAssertionV1,
    ) -> Result<(), V1EngineError> {
        if input.token0 != self.pair.token0 || input.token1 != self.pair.token1 {
            return Err(V1EngineError::CheckpointMismatch);
        }
        if input.gross_amount0 == 0 || input.gross_amount1 == 0 {
            return Err(V1EngineError::AmountZero);
        }
        let mut reconstructed = Pair::initialize(
            self.pair.token0,
            self.pair.token1,
            self.pair.lp_mint,
            self.pair.token0_decimals,
            self.pair.token1_decimals,
            self.rate_model_key,
            self.pair.swap_fee_bps,
            self.pair.half_life,
            self.pair.fixed_cf_bps,
            input.slot,
            self.pair.params_hash,
            self.pair.version,
            self.pair.bump,
            self.pair.vault_bumps,
            self.rate_model.initial_rate,
        );
        let liquidity_sqrt = integer_sqrt(
            (input.gross_amount0 as u128)
                .checked_mul(input.gross_amount1 as u128)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "initial liquidity product",
                ))?,
        );
        let minted_lp = u64::try_from(liquidity_sqrt.checked_sub(MIN_LIQUIDITY as u128).ok_or(
            V1EngineError::ArithmeticOverflow("initial minimum liquidity"),
        )?)
        .map_err(|_| V1EngineError::ArithmeticOverflow("initial LP conversion"))?;
        if minted_lp != input.minted_lp {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "initial LP minted",
                expected: minted_lp,
                actual: input.minted_lp,
            });
        }
        reconstructed.reserve0 = input.gross_amount0;
        reconstructed.reserve1 = input.gross_amount1;
        reconstructed.cash_reserve0 = input.gross_amount0;
        reconstructed.cash_reserve1 = input.gross_amount1;
        reconstructed.total_supply = MIN_LIQUIDITY
            .checked_add(minted_lp)
            .ok_or(V1EngineError::ArithmeticOverflow("initial LP supply"))?;
        reconstructed.last_price0_ema.symmetric = reconstructed.spot_price0_nad();
        reconstructed.last_price0_ema.directional = reconstructed.spot_price0_nad();
        reconstructed.last_price1_ema.symmetric = reconstructed.spot_price1_nad();
        reconstructed.last_price1_ema.directional = reconstructed.spot_price1_nad();
        if pair_checkpoint(&reconstructed) != pair_checkpoint(&self.pair)
            || !self.positions.is_empty()
            || self.revenue != V1RevenueCheckpointV1::ZERO
        {
            return Err(V1EngineError::CheckpointMismatch);
        }
        let expected_custody = V1CustodyCheckpointV1 {
            reserve_vault0: input.vault_credit0,
            reserve_vault1: input.vault_credit1,
            collateral_vault0: 0,
            collateral_vault1: 0,
        };
        if self.custody != expected_custody {
            return Err(V1EngineError::CheckpointMismatch);
        }
        Ok(())
    }

    /// Applies V1's exact slot-based EMA, utilization, native rate-model, and
    /// interest-accounting transition at an explicitly supplied historical
    /// slot. The public V1 `Pair::update` reads the Solana Clock sysvar, so this
    /// adapter mirrors only that orchestration while invoking V1's public math.
    pub fn advance_to_slot(&mut self, slot: u64) -> Result<V1InterestAccrualV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut revenue = self.revenue;
        let receipt = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            slot,
        )?;
        self.pair = pair;
        self.revenue = revenue;
        Ok(receipt)
    }

    /// Replays one successful V1 swap, including its preceding `Pair::update`.
    /// All mutation is staged to preserve Solana instruction rollback on error.
    pub fn apply_swap(&mut self, input: V1SwapInputV1) -> Result<V1SwapReceiptV1, V1EngineError> {
        self.apply_swap_inner(input, None)
    }

    /// Provisional DecisionGrade swap replay. The event tuple determines the
    /// economic transition only after the canonical token CPIs independently
    /// bind custody in/out. AuditGrade callers cannot enter this path.
    pub fn apply_event_and_cpi_witnessed_swap(
        &mut self,
        input: V1SwapInputV1,
        witness: V1SwapTransitionWitnessV1,
    ) -> Result<V1SwapReceiptV1, V1EngineError> {
        if self.post_state_correction_policy
            != V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        {
            return Err(V1EngineError::CheckpointMismatch);
        }
        self.apply_swap_inner(input, Some(witness))
    }

    fn apply_swap_inner(
        &mut self,
        input: V1SwapInputV1,
        transition_witness: Option<V1SwapTransitionWitnessV1>,
    ) -> Result<V1SwapReceiptV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut revenue = self.revenue;
        let mut custody = self.custody;
        let accrual = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            input.slot,
        )?;
        if input.gross_amount_in == 0 {
            return Err(V1EngineError::AmountZero);
        }
        let k_before = pair.k();
        let swap_fee = ceil_div(
            (input.gross_amount_in as u128)
                .checked_mul(pair.swap_fee_bps as u128)
                .ok_or(V1EngineError::ArithmeticOverflow("swap fee"))?,
            BPS_DENOMINATOR as u128,
        )
        .ok_or(V1EngineError::ArithmeticOverflow("swap fee division"))?
            as u64;
        let protocol_fee = ceil_div(
            (swap_fee as u128)
                .checked_mul(self.authority.swap_bps as u128)
                .ok_or(V1EngineError::ArithmeticOverflow("protocol swap fee"))?,
            BPS_DENOMINATOR as u128,
        )
        .ok_or(V1EngineError::ArithmeticOverflow(
            "protocol swap fee division",
        ))? as u64;
        let source_native_amount_in_after_fee = input
            .gross_amount_in
            .checked_sub(swap_fee)
            .ok_or(V1EngineError::ArithmeticOverflow("net swap input"))?;
        let source_native_amount_in_with_lp_fee =
            input
                .gross_amount_in
                .checked_sub(protocol_fee)
                .ok_or(V1EngineError::ArithmeticOverflow("LP-reserve swap input"))?;

        let (reserve_in, reserve_out, cash_out, output_token) = match input.token_in {
            V1TokenSideV1::Token0 => (pair.reserve0, pair.reserve1, pair.cash_reserve1, 1),
            V1TokenSideV1::Token1 => (pair.reserve1, pair.reserve0, pair.cash_reserve0, 0),
        };
        let source_native_amount_out = CPCurve::calculate_amount_out(
            reserve_in,
            reserve_out,
            source_native_amount_in_after_fee,
        )
        .map_err(|error| native_error("CPCurve::calculate_amount_out", error))?;
        let source_native_reserve_in_after = reserve_in
            .checked_add(source_native_amount_in_with_lp_fee)
            .ok_or(V1EngineError::ArithmeticOverflow("swap input reserve"))?;
        let source_native_reserve_out_after = reserve_out
            .checked_sub(source_native_amount_out)
            .ok_or(V1EngineError::ArithmeticOverflow("swap output reserve"))?;
        let source_native_lp_fee = swap_fee.checked_sub(protocol_fee).unwrap_or(0);
        let (source_native_reserve0_after, source_native_reserve1_after) = match input.token_in {
            V1TokenSideV1::Token0 => (
                source_native_reserve_in_after,
                source_native_reserve_out_after,
            ),
            V1TokenSideV1::Token1 => (
                source_native_reserve_out_after,
                source_native_reserve_in_after,
            ),
        };

        let (
            amount_in_after_fee,
            amount_out,
            lp_fee,
            protocol_fee,
            accounted_lp_fee,
            accounted_protocol_fee,
            transition_correction,
        ) = if let Some(witness) = transition_witness {
            if witness.event_token_in != input.token_in
                || witness.event_gross_amount_in != input.gross_amount_in
                || witness.token_cpi_vault_credit_in != input.vault_credit_in
                || witness.token_cpi_vault_debit_out != input.vault_debit_out
                || witness.token_cpi_recipient_credit_out != input.recipient_credit_out
                || witness.event_amount_out != witness.token_cpi_vault_debit_out
                || witness.event_amount_out != witness.token_cpi_recipient_credit_out
                || input.vault_credit_in != input.gross_amount_in
            {
                return Err(V1EngineError::CheckpointMismatch);
            }
            let witnessed_total = witness
                .event_amount_in_after_fee
                .checked_add(witness.event_lp_fee)
                .and_then(|value| value.checked_add(witness.event_protocol_fee))
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "witnessed swap fee conservation",
                ))?;
            if witnessed_total != input.gross_amount_in {
                return Err(V1EngineError::CheckpointMismatch);
            }
            let witnessed_total_fee = input
                .gross_amount_in
                .checked_sub(witness.event_amount_in_after_fee)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "witnessed swap total fee",
                ))?;
            if witnessed_total_fee != swap_fee {
                return Err(V1EngineError::CheckpointMismatch);
            }
            if witness.event_amount_out < input.min_amount_out {
                return Err(V1EngineError::SlippageExceeded {
                    minimum: input.min_amount_out,
                    actual: witness.event_amount_out,
                });
            }
            if cash_out < witness.event_amount_out {
                return Err(V1EngineError::InsufficientCash {
                    token: output_token,
                    available: cash_out,
                    required: witness.event_amount_out,
                });
            }

            let (
                cash0_after,
                cash1_after,
                expected_output_reserve_after,
                observed_output_reserve_after,
                input_reserve_after,
                input_cash_after,
                output_reserve_after,
                output_cash_after,
                total_debt_before_alignment,
                total_debt_shares_unchanged,
            ) = match input.token_in {
                V1TokenSideV1::Token0 => {
                    let cash0_after = witness
                        .event_reserve0_after
                        .checked_sub(pair.total_debt0)
                        .ok_or(V1EngineError::CheckpointMismatchField(
                            "swap.input_reserve_below_debt0",
                        ))?;
                    let cash1_after = witness
                        .event_reserve1_after
                        .checked_sub(pair.total_debt1)
                        .ok_or(V1EngineError::CheckpointMismatchField(
                            "swap.output_reserve_below_debt1",
                        ))?;
                    let expected_output =
                        pair.reserve1.checked_sub(witness.event_amount_out).ok_or(
                            V1EngineError::ArithmeticOverflow("witnessed token1 output reserve"),
                        )?;
                    (
                        cash0_after,
                        cash1_after,
                        expected_output,
                        witness.event_reserve1_after,
                        witness.event_reserve0_after,
                        cash0_after,
                        witness.event_reserve1_after,
                        cash1_after,
                        pair.total_debt0,
                        pair.total_debt0_shares,
                    )
                }
                V1TokenSideV1::Token1 => {
                    let cash1_after = witness
                        .event_reserve1_after
                        .checked_sub(pair.total_debt1)
                        .ok_or(V1EngineError::CheckpointMismatchField(
                            "swap.input_reserve_below_debt1",
                        ))?;
                    let cash0_after = witness
                        .event_reserve0_after
                        .checked_sub(pair.total_debt0)
                        .ok_or(V1EngineError::CheckpointMismatchField(
                            "swap.output_reserve_below_debt0",
                        ))?;
                    let expected_output =
                        pair.reserve0.checked_sub(witness.event_amount_out).ok_or(
                            V1EngineError::ArithmeticOverflow("witnessed token0 output reserve"),
                        )?;
                    (
                        cash0_after,
                        cash1_after,
                        expected_output,
                        witness.event_reserve0_after,
                        witness.event_reserve1_after,
                        cash1_after,
                        witness.event_reserve0_after,
                        cash0_after,
                        pair.total_debt1,
                        pair.total_debt1_shares,
                    )
                }
            };
            // Preserve output-side debt and shares. A lower event reserve than
            // the linked engine predicts is carried as physical
            // custody-minus-cash inventory, never synthesized as borrower
            // debt or economic revenue.
            let output_reserve_model_inventory_delta = expected_output_reserve_after
                .checked_sub(observed_output_reserve_after)
                .ok_or(V1EngineError::CheckpointValueMismatch {
                    field: "swap.output_reserve_after",
                    event: u128::from(observed_output_reserve_after),
                    replayed: u128::from(expected_output_reserve_after),
                })?;
            if output_reserve_after < output_cash_after {
                return Err(V1EngineError::CheckpointMismatchField(
                    "swap.output_reserve_below_cash",
                ));
            }
            let output_debt_after = output_reserve_after - output_cash_after;
            let output_debt_before = match input.token_in {
                V1TokenSideV1::Token0 => pair.total_debt1,
                V1TokenSideV1::Token1 => pair.total_debt0,
            };
            if output_debt_after != output_debt_before {
                return Err(V1EngineError::CheckpointMismatchField(
                    "swap.output_debt_unchanged",
                ));
            }
            let (output_vault_before, output_cash_before) = match input.token_in {
                V1TokenSideV1::Token0 => (custody.reserve_vault1, pair.cash_reserve1),
                V1TokenSideV1::Token1 => (custody.reserve_vault0, pair.cash_reserve0),
            };
            let output_vault_after = output_vault_before
                .checked_sub(input.vault_debit_out)
                .ok_or(V1EngineError::InsufficientVaultBalance {
                    vault: "swap output reserve vault",
                    available: output_vault_before,
                    required: input.vault_debit_out,
                })?;
            if output_vault_before < output_cash_before || output_vault_after < output_cash_after {
                return Err(V1EngineError::CheckpointMismatchField(
                    "swap.output_inventory",
                ));
            }
            let output_protocol_inventory_before = output_vault_before - output_cash_before;
            let output_protocol_inventory_after = output_vault_after - output_cash_after;
            let output_protocol_inventory_delta = output_protocol_inventory_after
                .checked_sub(output_protocol_inventory_before)
                .ok_or(V1EngineError::CheckpointMismatchField(
                    "swap.output_protocol_inventory_delta",
                ))?;
            if output_protocol_inventory_delta != output_reserve_model_inventory_delta {
                return Err(V1EngineError::CheckpointMismatchField(
                    "swap.output_inventory_model_delta",
                ));
            }
            if input_reserve_after
                != input_cash_after
                    .checked_add(total_debt_before_alignment)
                    .ok_or(V1EngineError::ArithmeticOverflow(
                        "witnessed swap input reserve identity",
                    ))?
            {
                return Err(V1EngineError::CheckpointMismatchField(
                    "swap.input_reserve_identity",
                ));
            }
            let (input_vault_before, input_cash_before) = match input.token_in {
                V1TokenSideV1::Token0 => (custody.reserve_vault0, pair.cash_reserve0),
                V1TokenSideV1::Token1 => (custody.reserve_vault1, pair.cash_reserve1),
            };
            let input_vault_after = input_vault_before
                .checked_add(input.vault_credit_in)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "witnessed swap input custody",
                ))?;
            if input_vault_before < input_cash_before {
                return Err(V1EngineError::CheckpointMismatchField(
                    "swap.input_inventory_before",
                ));
            }
            if input_vault_after < input_cash_after {
                return Err(V1EngineError::CheckpointMismatchField(
                    "swap.input_inventory_after",
                ));
            }
            let protocol_inventory_before = input_vault_before - input_cash_before;
            let protocol_inventory_after = input_vault_after - input_cash_after;
            let protocol_inventory_delta = protocol_inventory_after
                .checked_sub(protocol_inventory_before)
                .ok_or(V1EngineError::CheckpointMismatchField(
                    "swap.protocol_inventory_delta",
                ))?;
            if protocol_inventory_delta < protocol_fee {
                return Err(V1EngineError::CheckpointMismatchField(
                    "swap.protocol_inventory_fee_floor",
                ));
            }
            let reserve_model_inventory_delta = protocol_inventory_delta - protocol_fee;
            let total_debt_after_alignment = total_debt_before_alignment;
            let witnessed_k_after = (witness.event_reserve0_after as u128)
                .checked_mul(witness.event_reserve1_after as u128)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "witnessed swap constant product",
                ))?;
            let k_delta = i128::try_from(witnessed_k_after)
                .and_then(|after| i128::try_from(k_before).map(|before| after - before))
                .map_err(|_| V1EngineError::ArithmeticOverflow("witnessed swap K delta"))?;

            pair.reserve0 = witness.event_reserve0_after;
            pair.reserve1 = witness.event_reserve1_after;
            pair.cash_reserve0 = cash0_after;
            pair.cash_reserve1 = cash1_after;
            // Swaps never change borrower debt or aggregate debt shares. Any
            // residual event-reserve drift is carried by exact cash/inventory,
            // not synthesized as borrower debt or revenue.

            let correction = (source_native_amount_in_after_fee
                != witness.event_amount_in_after_fee
                || source_native_amount_out != witness.event_amount_out
                || source_native_lp_fee != witness.event_lp_fee
                || protocol_fee != witness.event_protocol_fee
                || source_native_reserve0_after != witness.event_reserve0_after
                || source_native_reserve1_after != witness.event_reserve1_after
                || total_debt_before_alignment != total_debt_after_alignment
                || witnessed_k_after < k_before)
                .then_some(V1SwapTransitionCorrectionV1 {
                    source_native_amount_in_after_fee,
                    source_native_amount_out,
                    source_native_lp_fee,
                    source_native_protocol_fee: protocol_fee,
                    source_native_reserve0_after,
                    source_native_reserve1_after,
                    witnessed_amount_in_after_fee: witness.event_amount_in_after_fee,
                    witnessed_amount_out: witness.event_amount_out,
                    witnessed_lp_fee: witness.event_lp_fee,
                    witnessed_protocol_fee: witness.event_protocol_fee,
                    accounted_lp_fee: source_native_lp_fee,
                    accounted_protocol_fee: protocol_fee,
                    witnessed_reserve0_after: witness.event_reserve0_after,
                    witnessed_reserve1_after: witness.event_reserve1_after,
                    reserve_debt_alignment_side: input.token_in,
                    total_debt_before_alignment,
                    total_debt_after_alignment,
                    total_debt_shares_unchanged,
                    protocol_inventory_before,
                    protocol_inventory_after,
                    protocol_inventory_delta,
                    reserve_model_inventory_delta,
                    output_protocol_inventory_before,
                    output_protocol_inventory_after,
                    output_protocol_inventory_delta,
                    output_reserve_model_inventory_delta,
                    k_before,
                    k_after: witnessed_k_after,
                    k_delta,
                });
            (
                witness.event_amount_in_after_fee,
                witness.event_amount_out,
                witness.event_lp_fee,
                witness.event_protocol_fee,
                source_native_lp_fee,
                protocol_fee,
                correction,
            )
        } else {
            if source_native_amount_out < input.min_amount_out {
                return Err(V1EngineError::SlippageExceeded {
                    minimum: input.min_amount_out,
                    actual: source_native_amount_out,
                });
            }
            if cash_out < source_native_amount_out {
                return Err(V1EngineError::InsufficientCash {
                    token: output_token,
                    available: cash_out,
                    required: source_native_amount_out,
                });
            }
            if input.vault_debit_out != source_native_amount_out {
                return Err(V1EngineError::CustodyDeltaMismatch {
                    field: "swap vault_debit_out",
                    expected: source_native_amount_out,
                    actual: input.vault_debit_out,
                });
            }
            match input.token_in {
                V1TokenSideV1::Token0 => {
                    pair.reserve0 = source_native_reserve_in_after;
                    pair.reserve1 = source_native_reserve_out_after;
                    pair.cash_reserve0 = pair
                        .cash_reserve0
                        .saturating_add(source_native_amount_in_with_lp_fee);
                    pair.cash_reserve1 =
                        pair.cash_reserve1.saturating_sub(source_native_amount_out);
                }
                V1TokenSideV1::Token1 => {
                    pair.reserve1 = source_native_reserve_in_after;
                    pair.reserve0 = source_native_reserve_out_after;
                    pair.cash_reserve1 = pair
                        .cash_reserve1
                        .saturating_add(source_native_amount_in_with_lp_fee);
                    pair.cash_reserve0 =
                        pair.cash_reserve0.saturating_sub(source_native_amount_out);
                }
            }
            (
                source_native_amount_in_after_fee,
                source_native_amount_out,
                source_native_lp_fee,
                protocol_fee,
                source_native_lp_fee,
                protocol_fee,
                None,
            )
        };
        let k_after = pair.k();
        if k_after < k_before && transition_witness.is_none() {
            return Err(V1EngineError::BrokenInvariant {
                before: k_before,
                after: k_after,
            });
        }

        add_swap_revenue(
            &mut revenue,
            input.token_in,
            accounted_lp_fee,
            accounted_protocol_fee,
        )?;
        credit_reserve_vault(&mut custody, input.token_in, input.vault_credit_in)?;
        debit_reserve_vault(
            &mut custody,
            input.token_in.opposite(),
            input.vault_debit_out,
        )?;
        let receipt = V1SwapReceiptV1 {
            accrual,
            token_in: input.token_in,
            gross_amount_in: input.gross_amount_in,
            amount_in_after_fee,
            amount_out,
            lp_fee,
            protocol_fee,
            accounted_lp_fee,
            accounted_protocol_fee,
            vault_credit_in: input.vault_credit_in,
            vault_debit_out: input.vault_debit_out,
            recipient_credit_out: input.recipient_credit_out,
            reserve0_after: pair.reserve0,
            reserve1_after: pair.reserve1,
            cash_reserve0_after: pair.cash_reserve0,
            cash_reserve1_after: pair.cash_reserve1,
            transition_correction,
        };
        self.pair = pair;
        self.custody = custody;
        self.revenue = revenue;
        Ok(receipt)
    }

    /// Returns V1's native impact-aware (or configured fixed-CF) capacity after
    /// bringing the engine to the desired operation slot.
    pub fn borrow_capacity(
        &self,
        position: Pubkey,
        debt_token: V1TokenSideV1,
    ) -> Result<V1BorrowCapacityV1, V1EngineError> {
        let position_account = self
            .positions
            .get(&position)
            .ok_or(V1EngineError::MissingPosition(position))?;
        borrow_capacity_for(&self.pair, position, position_account, debt_token)
    }

    pub fn apply_borrow(
        &mut self,
        input: V1BorrowInputV1,
    ) -> Result<V1BorrowReceiptV1, V1EngineError> {
        self.apply_borrow_inner(input, None)
    }

    pub fn apply_event_and_cpi_witnessed_borrow(
        &mut self,
        input: V1BorrowInputV1,
        witness: V1BorrowTransitionWitnessV1,
    ) -> Result<V1BorrowReceiptV1, V1EngineError> {
        if self.post_state_correction_policy
            != V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        {
            return Err(V1EngineError::CheckpointMismatch);
        }
        self.apply_borrow_inner(input, Some(witness))
    }

    fn apply_borrow_inner(
        &mut self,
        input: V1BorrowInputV1,
        transition_witness: Option<V1BorrowTransitionWitnessV1>,
    ) -> Result<V1BorrowReceiptV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut position = copy_position(
            self.positions
                .get(&input.position)
                .ok_or(V1EngineError::MissingPosition(input.position))?,
        );
        let mut revenue = self.revenue;
        let mut custody = self.custody;
        let accrual = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            input.slot,
        )?;
        if input.requested_amount == 0 {
            return Err(V1EngineError::AmountZero);
        }
        if self.authority.global_reduce_only || pair.reduce_only {
            return Err(V1EngineError::ReduceOnly);
        }

        let capacity_before =
            borrow_capacity_for(&pair, input.position, &position, input.debt_token)?;
        let borrowed_amount = if input.requested_amount == u64::MAX {
            capacity_before.remaining_capacity
        } else {
            input.requested_amount
        };
        if borrowed_amount == 0 {
            return Err(V1EngineError::AmountZero);
        }
        let debt_after = capacity_before
            .actual_debt
            .checked_add(borrowed_amount)
            .ok_or(V1EngineError::ArithmeticOverflow("post-borrow debt"))?;
        if debt_after > capacity_before.admissible_max_debt {
            return Err(V1EngineError::BorrowingPowerExceeded {
                requested: debt_after,
                maximum: capacity_before.admissible_max_debt,
            });
        }
        if borrowed_amount > capacity_before.cash_reserve {
            return Err(V1EngineError::InsufficientCash {
                token: side_number(input.debt_token),
                available: capacity_before.cash_reserve,
                required: borrowed_amount,
            });
        }

        let debt_mint = mint_for_side(&pair, input.debt_token);
        let shares_before = shares_for_side(&position, input.debt_token);
        let total_debt_before = match input.debt_token {
            V1TokenSideV1::Token0 => pair.total_debt0,
            V1TokenSideV1::Token1 => pair.total_debt1,
        };
        let total_debt_shares_before = match input.debt_token {
            V1TokenSideV1::Token0 => pair.total_debt0_shares,
            V1TokenSideV1::Token1 => pair.total_debt1_shares,
        };
        position
            .increase_debt(&mut pair, &debt_mint, borrowed_amount)
            .map_err(|error| native_error("UserPosition::increase_debt", error))?;
        position.set_liquidation_cf_for_debt_token(
            &debt_mint,
            &pair,
            capacity_before.liquidation_cf_bps,
        );
        let source_native_shares_after = shares_for_side(&position, input.debt_token);
        let source_native_total_debt_after = match input.debt_token {
            V1TokenSideV1::Token0 => pair.total_debt0,
            V1TokenSideV1::Token1 => pair.total_debt1,
        };
        let source_native_total_debt_shares_after = match input.debt_token {
            V1TokenSideV1::Token0 => pair.total_debt0_shares,
            V1TokenSideV1::Token1 => pair.total_debt1_shares,
        };
        let transition_correction = if let Some(witness) = transition_witness {
            if witness.event_debt_token != input.debt_token
                || witness.event_position != input.position
                || witness.event_borrowed_amount != borrowed_amount
                || witness.token_cpi_vault_debit != borrowed_amount
                || witness.token_cpi_recipient_credit != borrowed_amount
                || input.recipient_credit != witness.token_cpi_recipient_credit
                || witness.event_collateral0 != position.collateral0
                || witness.event_collateral1 != position.collateral1
                || witness.event_collateral0_liquidation_cf_bps
                    != position.collateral0_liquidation_cf_bps
                || witness.event_collateral1_liquidation_cf_bps
                    != position.collateral1_liquidation_cf_bps
                || witness.event_collateral0_max_cf_bps
                    != max_cf_from_liquidation(witness.event_collateral0_liquidation_cf_bps)
                || witness.event_collateral1_max_cf_bps
                    != max_cf_from_liquidation(witness.event_collateral1_liquidation_cf_bps)
            {
                return Err(V1EngineError::CheckpointMismatch);
            }
            let (witnessed_position_shares_after, opposite_witnessed, opposite_before) =
                match input.debt_token {
                    V1TokenSideV1::Token0 => (
                        witness.event_debt0_shares,
                        witness.event_debt1_shares,
                        self.positions
                            .get(&input.position)
                            .ok_or(V1EngineError::MissingPosition(input.position))?
                            .debt1_shares,
                    ),
                    V1TokenSideV1::Token1 => (
                        witness.event_debt1_shares,
                        witness.event_debt0_shares,
                        self.positions
                            .get(&input.position)
                            .ok_or(V1EngineError::MissingPosition(input.position))?
                            .debt0_shares,
                    ),
                };
            if opposite_witnessed != opposite_before
                || witnessed_position_shares_after < shares_before
            {
                return Err(V1EngineError::CheckpointMismatch);
            }
            let witnessed_position_debt_share_delta = witnessed_position_shares_after
                .checked_sub(shares_before)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "witnessed borrow position share delta",
                ))?;
            let witnessed_total_debt_shares_after = total_debt_shares_before
                .checked_add(witnessed_position_debt_share_delta)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "witnessed borrow aggregate share delta",
                ))?;
            let (reserve, cash, witnessed_total_debt_after) = match input.debt_token {
                V1TokenSideV1::Token0 => {
                    let cash = pair.cash_reserve0;
                    if pair.reserve0 < cash {
                        return Err(V1EngineError::CheckpointMismatch);
                    }
                    let debt = pair.reserve0 - cash;
                    pair.total_debt0 = debt;
                    pair.total_debt0_shares = witnessed_total_debt_shares_after;
                    position.debt0_shares = witnessed_position_shares_after;
                    (pair.reserve0, cash, debt)
                }
                V1TokenSideV1::Token1 => {
                    let cash = pair.cash_reserve1;
                    if pair.reserve1 < cash {
                        return Err(V1EngineError::CheckpointMismatch);
                    }
                    let debt = pair.reserve1 - cash;
                    pair.total_debt1 = debt;
                    pair.total_debt1_shares = witnessed_total_debt_shares_after;
                    position.debt1_shares = witnessed_position_shares_after;
                    (pair.reserve1, cash, debt)
                }
            };
            if reserve
                != cash.checked_add(witnessed_total_debt_after).ok_or(
                    V1EngineError::ArithmeticOverflow("witnessed borrow reserve identity"),
                )?
                || witnessed_total_debt_after
                    != total_debt_before.checked_add(borrowed_amount).ok_or(
                        V1EngineError::ArithmeticOverflow("witnessed borrow aggregate debt delta"),
                    )?
            {
                return Err(V1EngineError::CheckpointMismatch);
            }
            (source_native_shares_after != witnessed_position_shares_after
                || source_native_total_debt_shares_after != witnessed_total_debt_shares_after
                || source_native_total_debt_after != witnessed_total_debt_after)
                .then_some(V1BorrowTransitionCorrectionV1 {
                    source_native_position_debt_shares_after: source_native_shares_after,
                    witnessed_position_debt_shares_after: witnessed_position_shares_after,
                    source_native_total_debt_shares_after,
                    witnessed_total_debt_shares_after,
                    source_native_total_debt_after,
                    witnessed_total_debt_after,
                    witnessed_position_debt_share_delta,
                })
        } else {
            None
        };
        let shares_after = shares_for_side(&position, input.debt_token);
        let native_debt_after = debt_for_side(&position, &pair, input.debt_token)?;
        let receipt = V1BorrowReceiptV1 {
            accrual,
            capacity_before,
            borrowed_amount,
            debt_after: native_debt_after,
            debt_shares_minted: shares_after
                .checked_sub(shares_before)
                .ok_or(V1EngineError::ArithmeticOverflow("borrowed debt shares"))?,
            recipient_credit: input.recipient_credit,
            transition_correction,
        };
        debit_reserve_vault(&mut custody, input.debt_token, borrowed_amount)?;
        self.pair = pair;
        self.positions.insert(input.position, position);
        self.custody = custody;
        self.revenue = revenue;
        Ok(receipt)
    }

    pub fn apply_repay(
        &mut self,
        input: V1RepayInputV1,
    ) -> Result<V1RepayReceiptV1, V1EngineError> {
        self.apply_repay_inner(input, None)
    }

    pub fn apply_event_and_cpi_witnessed_repay(
        &mut self,
        input: V1RepayInputV1,
        witness: V1RepayTransitionWitnessV1,
    ) -> Result<V1RepayReceiptV1, V1EngineError> {
        if self.post_state_correction_policy
            != V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        {
            return Err(V1EngineError::CheckpointMismatch);
        }
        self.apply_repay_inner(input, Some(witness))
    }

    fn apply_repay_inner(
        &mut self,
        input: V1RepayInputV1,
        transition_witness: Option<V1RepayTransitionWitnessV1>,
    ) -> Result<V1RepayReceiptV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut position = copy_position(
            self.positions
                .get(&input.position)
                .ok_or(V1EngineError::MissingPosition(input.position))?,
        );
        let mut revenue = self.revenue;
        let mut custody = self.custody;
        let accrual = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            input.slot,
        )?;
        if input.requested_amount == 0 {
            return Err(V1EngineError::AmountZero);
        }

        let debt_before = debt_for_side(&position, &pair, input.debt_token)?;
        if debt_before == 0 {
            return Err(V1EngineError::ZeroDebt);
        }
        let source_native_repaid_amount = if input.requested_amount == u64::MAX {
            debt_before
        } else {
            input.requested_amount
        };
        if source_native_repaid_amount > debt_before {
            return Err(V1EngineError::InsufficientDebt {
                requested: source_native_repaid_amount,
                debt: debt_before,
            });
        }
        let pair_before_transition = copy_pair(&pair);
        let position_before_transition = copy_position(&position);
        let debt_mint = mint_for_side(&pair, input.debt_token);
        let shares_before = shares_for_side(&position, input.debt_token);
        let total_debt_before = match input.debt_token {
            V1TokenSideV1::Token0 => pair.total_debt0,
            V1TokenSideV1::Token1 => pair.total_debt1,
        };
        let total_debt_shares_before = match input.debt_token {
            V1TokenSideV1::Token0 => pair.total_debt0_shares,
            V1TokenSideV1::Token1 => pair.total_debt1_shares,
        };
        position
            .decrease_debt(
                &mut pair,
                &debt_mint,
                source_native_repaid_amount,
                DebtDecreaseReason::Repayment,
            )
            .map_err(|error| native_error("UserPosition::decrease_debt", error))?;
        let source_native_shares_after = shares_for_side(&position, input.debt_token);
        let source_native_total_debt_after = match input.debt_token {
            V1TokenSideV1::Token0 => pair.total_debt0,
            V1TokenSideV1::Token1 => pair.total_debt1,
        };
        let source_native_total_debt_shares_after = match input.debt_token {
            V1TokenSideV1::Token0 => pair.total_debt0_shares,
            V1TokenSideV1::Token1 => pair.total_debt1_shares,
        };
        let (repaid_amount, transition_correction) = if let Some(witness) = transition_witness {
            let witnessed_repaid_amount = witness.event_repaid_amount;
            if witness.event_debt_token != input.debt_token
                || witness.event_position != input.position
                || witness.token_cpi_vault_credit != input.vault_credit
                || witness.token_cpi_vault_credit != witnessed_repaid_amount
                || (input.requested_amount != u64::MAX
                    && input.requested_amount != witnessed_repaid_amount)
                || witnessed_repaid_amount == 0
                || (input.requested_amount != u64::MAX && witnessed_repaid_amount > debt_before)
                || witness.event_collateral0 != position_before_transition.collateral0
                || witness.event_collateral1 != position_before_transition.collateral1
                || witness.event_collateral0_liquidation_cf_bps
                    != position_before_transition.collateral0_liquidation_cf_bps
                || witness.event_collateral1_liquidation_cf_bps
                    != position_before_transition.collateral1_liquidation_cf_bps
                || witness.event_collateral0_max_cf_bps
                    != max_cf_from_liquidation(witness.event_collateral0_liquidation_cf_bps)
                || witness.event_collateral1_max_cf_bps
                    != max_cf_from_liquidation(witness.event_collateral1_liquidation_cf_bps)
            {
                return Err(V1EngineError::CheckpointMismatch);
            }
            let (witnessed_position_shares_after, opposite_witnessed, opposite_before) =
                match input.debt_token {
                    V1TokenSideV1::Token0 => (
                        witness.event_debt0_shares,
                        witness.event_debt1_shares,
                        position_before_transition.debt1_shares,
                    ),
                    V1TokenSideV1::Token1 => (
                        witness.event_debt1_shares,
                        witness.event_debt0_shares,
                        position_before_transition.debt0_shares,
                    ),
                };
            let witnessed_full_payoff = input.requested_amount == u64::MAX;
            if opposite_witnessed != opposite_before
                || witnessed_position_shares_after > shares_before
                || (witnessed_full_payoff
                    && (shares_before == 0 || witnessed_position_shares_after != 0))
            {
                return Err(V1EngineError::CheckpointMismatch);
            }
            let witnessed_position_debt_share_delta = shares_before
                .checked_sub(witnessed_position_shares_after)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "witnessed repay position share delta",
                ))?;
            let witnessed_total_debt_shares_after = total_debt_shares_before
                .checked_sub(witnessed_position_debt_share_delta)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "witnessed repay aggregate share delta",
                ))?;
            pair = pair_before_transition;
            position = position_before_transition;
            let witnessed_total_debt_after = total_debt_before
                .checked_sub(witnessed_repaid_amount)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "witnessed repay aggregate debt delta",
                ))?;
            match input.debt_token {
                V1TokenSideV1::Token0 => {
                    pair.cash_reserve0 = pair
                        .cash_reserve0
                        .checked_add(witnessed_repaid_amount)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "witnessed repay token0 cash",
                        ))?;
                    pair.total_debt0 = witnessed_total_debt_after;
                    pair.total_debt0_shares = witnessed_total_debt_shares_after;
                    position.debt0_shares = witnessed_position_shares_after;
                    if pair.reserve0
                        != pair.cash_reserve0.checked_add(pair.total_debt0).ok_or(
                            V1EngineError::ArithmeticOverflow(
                                "witnessed repay token0 reserve identity",
                            ),
                        )?
                    {
                        return Err(V1EngineError::CheckpointMismatch);
                    }
                }
                V1TokenSideV1::Token1 => {
                    pair.cash_reserve1 = pair
                        .cash_reserve1
                        .checked_add(witnessed_repaid_amount)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "witnessed repay token1 cash",
                        ))?;
                    pair.total_debt1 = witnessed_total_debt_after;
                    pair.total_debt1_shares = witnessed_total_debt_shares_after;
                    position.debt1_shares = witnessed_position_shares_after;
                    if pair.reserve1
                        != pair.cash_reserve1.checked_add(pair.total_debt1).ok_or(
                            V1EngineError::ArithmeticOverflow(
                                "witnessed repay token1 reserve identity",
                            ),
                        )?
                    {
                        return Err(V1EngineError::CheckpointMismatch);
                    }
                }
            }
            let correction = (source_native_repaid_amount != witnessed_repaid_amount
                || source_native_shares_after != witnessed_position_shares_after
                || source_native_total_debt_shares_after != witnessed_total_debt_shares_after
                || source_native_total_debt_after != witnessed_total_debt_after)
                .then_some(V1RepayTransitionCorrectionV1 {
                    source_native_position_debt_before: debt_before,
                    witnessed_position_debt_before: if witnessed_full_payoff {
                        witnessed_repaid_amount
                    } else {
                        debt_before
                    },
                    modeled_position_debt_gap: if witnessed_full_payoff {
                        witnessed_repaid_amount.saturating_sub(debt_before)
                    } else {
                        0
                    },
                    source_native_repaid_amount,
                    witnessed_repaid_amount,
                    source_native_position_debt_shares_after: source_native_shares_after,
                    witnessed_position_debt_shares_after: witnessed_position_shares_after,
                    source_native_total_debt_shares_after,
                    witnessed_total_debt_shares_after,
                    source_native_total_debt_after,
                    witnessed_total_debt_after,
                    witnessed_position_debt_share_delta,
                });
            (witnessed_repaid_amount, correction)
        } else {
            (source_native_repaid_amount, None)
        };
        let shares_after = shares_for_side(&position, input.debt_token);
        let debt_after = debt_for_side(&position, &pair, input.debt_token)?;
        let receipt_debt_before =
            if transition_witness.is_some() && input.requested_amount == u64::MAX {
                repaid_amount
            } else {
                debt_before
            };
        let receipt = V1RepayReceiptV1 {
            accrual,
            debt_before: receipt_debt_before,
            repaid_amount,
            debt_after,
            debt_shares_burned: shares_before
                .checked_sub(shares_after)
                .ok_or(V1EngineError::ArithmeticOverflow("repaid debt shares"))?,
            vault_credit: input.vault_credit,
            transition_correction,
        };
        credit_reserve_vault(&mut custody, input.debt_token, input.vault_credit)?;
        self.pair = pair;
        self.positions.insert(input.position, position);
        self.custody = custody;
        self.revenue = revenue;
        Ok(receipt)
    }

    pub fn apply_add_liquidity(
        &mut self,
        input: V1AddLiquidityInputV1,
    ) -> Result<V1AddLiquidityReceiptV1, V1EngineError> {
        self.apply_add_liquidity_inner(input, None)
    }

    pub fn apply_event_and_cpi_witnessed_add_liquidity(
        &mut self,
        input: V1AddLiquidityInputV1,
        witness: V1LpMintSupplyWitnessV1,
    ) -> Result<V1AddLiquidityReceiptV1, V1EngineError> {
        if self.post_state_correction_policy
            != V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        {
            return Err(V1EngineError::InvalidConfiguration(
                "decision_grade_event_and_token_cpi_correction_policy",
            ));
        }
        if witness.event_liquidity_minted == 0
            || witness.event_liquidity_minted != witness.token_cpi_supply_delta
            || witness.event_amount0_used != witness.token_cpi_vault_credit0
            || witness.event_amount1_used != witness.token_cpi_vault_credit1
            || witness.token_cpi_vault_credit0 != input.vault_credit0
            || witness.token_cpi_vault_credit1 != input.vault_credit1
            || witness.event_amount0_used > input.amount0_in
            || witness.event_amount1_used > input.amount1_in
        {
            return Err(V1EngineError::InvalidConfiguration(
                "lp_mint_event_and_token_cpi_witness",
            ));
        }
        self.apply_add_liquidity_inner(input, Some(witness))
    }

    fn apply_add_liquidity_inner(
        &mut self,
        input: V1AddLiquidityInputV1,
        supply_witness: Option<V1LpMintSupplyWitnessV1>,
    ) -> Result<V1AddLiquidityReceiptV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut revenue = self.revenue;
        let mut custody = self.custody;
        let mut accrual = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            input.slot,
        )?;
        if self.authority.global_reduce_only || pair.reduce_only {
            return Err(V1EngineError::ReduceOnly);
        }
        if input.amount0_in == 0 || input.amount1_in == 0 {
            return Err(V1EngineError::AmountZero);
        }
        if pair.reserve0 == 0 || pair.reserve1 == 0 || pair.total_supply == 0 {
            return Err(V1EngineError::BrokenInvariant {
                before: pair.k(),
                after: 0,
            });
        }

        let liquidity0 = (input.amount0_in as u128)
            .checked_mul(pair.total_supply as u128)
            .and_then(|value| value.checked_div(pair.reserve0 as u128))
            .ok_or(V1EngineError::ArithmeticOverflow("token0 liquidity quote"))?;
        let liquidity1 = (input.amount1_in as u128)
            .checked_mul(pair.total_supply as u128)
            .and_then(|value| value.checked_div(pair.reserve1 as u128))
            .ok_or(V1EngineError::ArithmeticOverflow("token1 liquidity quote"))?;
        let source_native_liquidity_minted = u64::try_from(liquidity0.min(liquidity1))
            .map_err(|_| V1EngineError::ArithmeticOverflow("LP mint conversion"))?;
        if source_native_liquidity_minted == 0 {
            return Err(V1EngineError::AmountZero);
        }
        let liquidity_minted = supply_witness
            .map(|witness| witness.event_liquidity_minted)
            .unwrap_or(source_native_liquidity_minted);
        if liquidity_minted < input.min_liquidity_out {
            return Err(V1EngineError::SlippageExceeded {
                minimum: input.min_liquidity_out,
                actual: liquidity_minted,
            });
        }
        let source_native_amount0_used = u64::try_from(
            ceil_div(
                (source_native_liquidity_minted as u128)
                    .checked_mul(pair.reserve0 as u128)
                    .ok_or(V1EngineError::ArithmeticOverflow("token0 liquidity used"))?,
                pair.total_supply as u128,
            )
            .ok_or(V1EngineError::ArithmeticOverflow(
                "token0 liquidity used division",
            ))?,
        )
        .map_err(|_| V1EngineError::ArithmeticOverflow("token0 liquidity conversion"))?;
        let source_native_amount1_used = u64::try_from(
            ceil_div(
                (source_native_liquidity_minted as u128)
                    .checked_mul(pair.reserve1 as u128)
                    .ok_or(V1EngineError::ArithmeticOverflow("token1 liquidity used"))?,
                pair.total_supply as u128,
            )
            .ok_or(V1EngineError::ArithmeticOverflow(
                "token1 liquidity used division",
            ))?,
        )
        .map_err(|_| V1EngineError::ArithmeticOverflow("token1 liquidity conversion"))?;
        let amount0_used = supply_witness
            .map(|witness| witness.event_amount0_used)
            .unwrap_or(source_native_amount0_used);
        let amount1_used = supply_witness
            .map(|witness| witness.event_amount1_used)
            .unwrap_or(source_native_amount1_used);

        pair.reserve0 = pair
            .reserve0
            .checked_add(amount0_used)
            .ok_or(V1EngineError::ArithmeticOverflow("token0 reserve add"))?;
        pair.reserve1 = pair
            .reserve1
            .checked_add(amount1_used)
            .ok_or(V1EngineError::ArithmeticOverflow("token1 reserve add"))?;
        pair.cash_reserve0 = pair
            .cash_reserve0
            .checked_add(amount0_used)
            .ok_or(V1EngineError::ArithmeticOverflow("token0 cash add"))?;
        pair.cash_reserve1 = pair
            .cash_reserve1
            .checked_add(amount1_used)
            .ok_or(V1EngineError::ArithmeticOverflow("token1 cash add"))?;
        let total_supply_before = pair.total_supply;
        pair.total_supply = total_supply_before
            .checked_add(liquidity_minted)
            .ok_or(V1EngineError::ArithmeticOverflow("LP supply add"))?;
        credit_reserve_vault(&mut custody, V1TokenSideV1::Token0, input.vault_credit0)?;
        credit_reserve_vault(&mut custody, V1TokenSideV1::Token1, input.vault_credit1)?;

        let source_native_cash_reserve0_after = pair.cash_reserve0;
        let source_native_cash_reserve1_after = pair.cash_reserve1;
        let source_native_total_debt0_after = pair.total_debt0;
        let source_native_total_debt1_after = pair.total_debt1;
        let source_native_protocol_interest0_after = revenue.protocol_interest0;
        let source_native_protocol_interest1_after = revenue.protocol_interest1;
        let source_native_cash_covered_protocol_interest0 = accrual.cash_covered_protocol_interest0;
        let source_native_cash_covered_protocol_interest1 = accrual.cash_covered_protocol_interest1;
        let cash_inventory_correction = match supply_witness.map(|witness| {
            (
                witness.event_cash_reserve0_after,
                witness.event_cash_reserve1_after,
            )
        }) {
            Some((Some(cash0), Some(cash1))) => {
                if cash0 > custody.reserve_vault0
                    || cash1 > custody.reserve_vault1
                    || cash0 > pair.reserve0
                    || cash1 > pair.reserve1
                    // Without full cash coverage, inventory alone cannot
                    // separate nominal protocol interest from LP entitlement.
                    || accrual.protocol_interest0
                        != accrual.cash_covered_protocol_interest0
                    || accrual.protocol_interest1
                        != accrual.cash_covered_protocol_interest1
                {
                    return Err(V1EngineError::CheckpointMismatch);
                }
                let protocol_inventory0 = u128::from(custody.reserve_vault0 - cash0);
                let protocol_inventory1 = u128::from(custody.reserve_vault1 - cash1);
                let witnessed_protocol_interest0_after = protocol_inventory0
                    .checked_add(revenue.protocol_claim_realized0)
                    .and_then(|value| value.checked_sub(revenue.protocol_swap_fee0))
                    .ok_or(V1EngineError::CheckpointMismatch)?;
                let witnessed_protocol_interest1_after = protocol_inventory1
                    .checked_add(revenue.protocol_claim_realized1)
                    .and_then(|value| value.checked_sub(revenue.protocol_swap_fee1))
                    .ok_or(V1EngineError::CheckpointMismatch)?;
                let inventory_protocol_interest_delta0 =
                    i128::try_from(witnessed_protocol_interest0_after)
                        .and_then(|after| {
                            i128::try_from(self.revenue.protocol_interest0)
                                .map(|before| after - before)
                        })
                        .map_err(|_| {
                            V1EngineError::ArithmeticOverflow(
                                "token0 inventory protocol-interest difference",
                            )
                        })?;
                let inventory_protocol_interest_delta1 =
                    i128::try_from(witnessed_protocol_interest1_after)
                        .and_then(|after| {
                            i128::try_from(self.revenue.protocol_interest1)
                                .map(|before| after - before)
                        })
                        .map_err(|_| {
                            V1EngineError::ArithmeticOverflow(
                                "token1 inventory protocol-interest difference",
                            )
                        })?;
                // Inventory after subtracting separately proven swap lanes is
                // not a unique economic-interest attribution. Preserve native
                // revenue whenever it falls below the source-native
                // checkpoint, and retain the exact signed residual instead.
                let protocol_interest_inventory_rounding_atoms0 =
                    i128::try_from(witnessed_protocol_interest0_after)
                        .and_then(|witnessed| {
                            i128::try_from(source_native_protocol_interest0_after)
                                .map(|source_native| (witnessed - source_native).min(0))
                        })
                        .map_err(|_| {
                            V1EngineError::ArithmeticOverflow(
                                "token0 protocol-interest inventory residual",
                            )
                        })?;
                let protocol_interest_inventory_rounding_atoms1 =
                    i128::try_from(witnessed_protocol_interest1_after)
                        .and_then(|witnessed| {
                            i128::try_from(source_native_protocol_interest1_after)
                                .map(|source_native| (witnessed - source_native).min(0))
                        })
                        .map_err(|_| {
                            V1EngineError::ArithmeticOverflow(
                                "token1 protocol-interest inventory residual",
                            )
                        })?;
                let witnessed_cash_covered_protocol_interest0 =
                    if protocol_interest_inventory_rounding_atoms0 == 0 {
                        u64::try_from(inventory_protocol_interest_delta0.max(0)).map_err(|_| {
                            V1EngineError::ArithmeticOverflow("witnessed token0 protocol interest")
                        })?
                    } else {
                        0
                    };
                let witnessed_cash_covered_protocol_interest1 =
                    if protocol_interest_inventory_rounding_atoms1 == 0 {
                        u64::try_from(inventory_protocol_interest_delta1.max(0)).map_err(|_| {
                            V1EngineError::ArithmeticOverflow("witnessed token1 protocol interest")
                        })?
                    } else {
                        0
                    };
                let witnessed_total_debt0_after = pair.reserve0 - cash0;
                let witnessed_total_debt1_after = pair.reserve1 - cash1;
                if protocol_interest_inventory_rounding_atoms0 == 0 {
                    let expected_total_debt0_after = self
                        .pair
                        .total_debt0
                        .checked_add(accrual.lp_interest0)
                        .and_then(|value| {
                            value.checked_add(witnessed_cash_covered_protocol_interest0)
                        })
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "witnessed token0 borrower cost",
                        ))?;
                    if witnessed_total_debt0_after != expected_total_debt0_after {
                        return Err(V1EngineError::CheckpointMismatch);
                    }
                }
                if protocol_interest_inventory_rounding_atoms1 == 0 {
                    let expected_total_debt1_after = self
                        .pair
                        .total_debt1
                        .checked_add(accrual.lp_interest1)
                        .and_then(|value| {
                            value.checked_add(witnessed_cash_covered_protocol_interest1)
                        })
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "witnessed token1 borrower cost",
                        ))?;
                    if witnessed_total_debt1_after != expected_total_debt1_after {
                        return Err(V1EngineError::CheckpointMismatch);
                    }
                }

                pair.cash_reserve0 = cash0;
                pair.cash_reserve1 = cash1;
                pair.total_debt0 = witnessed_total_debt0_after;
                pair.total_debt1 = witnessed_total_debt1_after;
                if protocol_interest_inventory_rounding_atoms0 == 0 {
                    revenue.protocol_interest0 = witnessed_protocol_interest0_after;
                    accrual.protocol_interest0 = witnessed_cash_covered_protocol_interest0;
                    accrual.cash_covered_protocol_interest0 =
                        witnessed_cash_covered_protocol_interest0;
                    accrual.borrower_cost0 = accrual
                        .lp_interest0
                        .checked_add(witnessed_cash_covered_protocol_interest0)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "witnessed token0 borrower cost receipt",
                        ))?;
                }
                if protocol_interest_inventory_rounding_atoms1 == 0 {
                    revenue.protocol_interest1 = witnessed_protocol_interest1_after;
                    accrual.protocol_interest1 = witnessed_cash_covered_protocol_interest1;
                    accrual.cash_covered_protocol_interest1 =
                        witnessed_cash_covered_protocol_interest1;
                    accrual.borrower_cost1 = accrual
                        .lp_interest1
                        .checked_add(witnessed_cash_covered_protocol_interest1)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "witnessed token1 borrower cost receipt",
                        ))?;
                }

                (source_native_cash_reserve0_after != cash0
                    || source_native_cash_reserve1_after != cash1
                    || source_native_protocol_interest0_after != witnessed_protocol_interest0_after
                    || source_native_protocol_interest1_after != witnessed_protocol_interest1_after)
                    .then_some(V1AddLiquidityCashInventoryCorrectionV1 {
                        source_native_cash_reserve0_after,
                        source_native_cash_reserve1_after,
                        witnessed_cash_reserve0_after: cash0,
                        witnessed_cash_reserve1_after: cash1,
                        source_native_total_debt0_after,
                        source_native_total_debt1_after,
                        witnessed_total_debt0_after,
                        witnessed_total_debt1_after,
                        source_native_protocol_interest0_after,
                        source_native_protocol_interest1_after,
                        witnessed_protocol_interest0_after,
                        witnessed_protocol_interest1_after,
                        source_native_cash_covered_protocol_interest0,
                        source_native_cash_covered_protocol_interest1,
                        witnessed_cash_covered_protocol_interest0,
                        witnessed_cash_covered_protocol_interest1,
                        protocol_interest_inventory_rounding_atoms0,
                        protocol_interest_inventory_rounding_atoms1,
                    })
            }
            Some((None, None)) | None => None,
            Some(_) => return Err(V1EngineError::CheckpointMismatch),
        };

        let receipt = V1AddLiquidityReceiptV1 {
            accrual,
            amount0_used,
            amount1_used,
            vault_credit0: input.vault_credit0,
            vault_credit1: input.vault_credit1,
            liquidity_minted,
            total_supply_after: pair.total_supply,
            supply_correction: (source_native_liquidity_minted != liquidity_minted
                || source_native_amount0_used != amount0_used
                || source_native_amount1_used != amount1_used)
                .then_some(V1LpMintSupplyCorrectionV1 {
                    source_native_amount0_used,
                    source_native_amount1_used,
                    source_native_liquidity_minted,
                    witnessed_amount0_used: amount0_used,
                    witnessed_amount1_used: amount1_used,
                    witnessed_liquidity_minted: liquidity_minted,
                    total_supply_before,
                    total_supply_after: pair.total_supply,
                }),
            cash_inventory_correction,
        };
        self.pair = pair;
        self.custody = custody;
        self.revenue = revenue;
        Ok(receipt)
    }

    pub fn apply_remove_liquidity(
        &mut self,
        input: V1RemoveLiquidityInputV1,
    ) -> Result<V1RemoveLiquidityReceiptV1, V1EngineError> {
        self.apply_remove_liquidity_inner(input, None)
    }

    pub fn apply_event_and_cpi_witnessed_remove_liquidity(
        &mut self,
        input: V1RemoveLiquidityInputV1,
        witness: V1RemoveLiquidityCashWitnessV1,
    ) -> Result<V1RemoveLiquidityReceiptV1, V1EngineError> {
        if self.post_state_correction_policy
            != V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        {
            return Err(V1EngineError::InvalidConfiguration(
                "decision_grade_event_and_token_cpi_correction_policy",
            ));
        }
        if witness.event_liquidity_burned != input.liquidity_burned
            || witness.event_amount0_out != witness.token_cpi_vault_debit0
            || witness.event_amount1_out != witness.token_cpi_vault_debit1
            || witness.event_amount0_out != witness.token_cpi_recipient_credit0
            || witness.event_amount1_out != witness.token_cpi_recipient_credit1
            || witness.token_cpi_vault_debit0 != input.vault_debit0
            || witness.token_cpi_vault_debit1 != input.vault_debit1
            || witness.token_cpi_recipient_credit0 != input.recipient_credit0
            || witness.token_cpi_recipient_credit1 != input.recipient_credit1
        {
            return Err(V1EngineError::InvalidConfiguration(
                "lp_burn_event_and_token_cpi_witness",
            ));
        }
        self.apply_remove_liquidity_inner(input, Some(witness))
    }

    fn apply_remove_liquidity_inner(
        &mut self,
        input: V1RemoveLiquidityInputV1,
        cash_witness: Option<V1RemoveLiquidityCashWitnessV1>,
    ) -> Result<V1RemoveLiquidityReceiptV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut revenue = self.revenue;
        let mut custody = self.custody;
        let accrual = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            input.slot,
        )?;
        if input.liquidity_burned == 0 {
            return Err(V1EngineError::AmountZero);
        }
        let burnable_supply = pair.total_supply.saturating_sub(MIN_LIQUIDITY);
        if input.liquidity_burned > burnable_supply {
            return Err(V1EngineError::InsufficientLpSupply {
                available: burnable_supply,
                requested: input.liquidity_burned,
            });
        }

        let amount0_gross = u64::try_from(
            (input.liquidity_burned as u128)
                .checked_mul(pair.reserve0 as u128)
                .and_then(|value| value.checked_div(pair.total_supply as u128))
                .ok_or(V1EngineError::ArithmeticOverflow("token0 LP withdrawal"))?,
        )
        .map_err(|_| V1EngineError::ArithmeticOverflow("token0 LP withdrawal conversion"))?;
        let amount1_gross = u64::try_from(
            (input.liquidity_burned as u128)
                .checked_mul(pair.reserve1 as u128)
                .and_then(|value| value.checked_div(pair.total_supply as u128))
                .ok_or(V1EngineError::ArithmeticOverflow("token1 LP withdrawal"))?,
        )
        .map_err(|_| V1EngineError::ArithmeticOverflow("token1 LP withdrawal conversion"))?;
        let source_native_withdrawal_fee0 =
            proportional_ceil(amount0_gross, LIQUIDITY_WITHDRAWAL_FEE_BPS)?;
        let source_native_withdrawal_fee1 =
            proportional_ceil(amount1_gross, LIQUIDITY_WITHDRAWAL_FEE_BPS)?;
        let source_native_amount0_out = amount0_gross
            .checked_sub(source_native_withdrawal_fee0)
            .ok_or(V1EngineError::ArithmeticOverflow("token0 LP output"))?;
        let source_native_amount1_out = amount1_gross
            .checked_sub(source_native_withdrawal_fee1)
            .ok_or(V1EngineError::ArithmeticOverflow("token1 LP output"))?;
        let amount0_out = cash_witness
            .map(|witness| witness.event_amount0_out)
            .unwrap_or(source_native_amount0_out);
        let amount1_out = cash_witness
            .map(|witness| witness.event_amount1_out)
            .unwrap_or(source_native_amount1_out);
        let withdrawal_fee0 = amount0_gross.checked_sub(amount0_out).ok_or(
            V1EngineError::CheckpointMismatchField("lp_burn.amount0_out_above_gross"),
        )?;
        let withdrawal_fee1 = amount1_gross.checked_sub(amount1_out).ok_or(
            V1EngineError::CheckpointMismatchField("lp_burn.amount1_out_above_gross"),
        )?;
        if amount0_out < input.min_amount0_out {
            return Err(V1EngineError::SlippageExceeded {
                minimum: input.min_amount0_out,
                actual: amount0_out,
            });
        }
        if amount1_out < input.min_amount1_out {
            return Err(V1EngineError::SlippageExceeded {
                minimum: input.min_amount1_out,
                actual: amount1_out,
            });
        }
        if pair.cash_reserve0 < amount0_out {
            return Err(V1EngineError::InsufficientCash {
                token: 0,
                available: pair.cash_reserve0,
                required: amount0_out,
            });
        }
        if pair.cash_reserve1 < amount1_out {
            return Err(V1EngineError::InsufficientCash {
                token: 1,
                available: pair.cash_reserve1,
                required: amount1_out,
            });
        }
        if input.vault_debit0 != amount0_out {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "remove_liquidity vault_debit0",
                expected: amount0_out,
                actual: input.vault_debit0,
            });
        }
        if input.vault_debit1 != amount1_out {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "remove_liquidity vault_debit1",
                expected: amount1_out,
                actual: input.vault_debit1,
            });
        }

        let source_native_reserve0_after =
            pair.reserve0.checked_sub(source_native_amount0_out).ok_or(
                V1EngineError::ArithmeticOverflow("source-native token0 reserve withdrawal"),
            )?;
        let source_native_reserve1_after =
            pair.reserve1.checked_sub(source_native_amount1_out).ok_or(
                V1EngineError::ArithmeticOverflow("source-native token1 reserve withdrawal"),
            )?;
        let post_reserve0 =
            pair.reserve0
                .checked_sub(amount0_out)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "token0 reserve withdrawal",
                ))?;
        let post_reserve1 =
            pair.reserve1
                .checked_sub(amount1_out)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "token1 reserve withdrawal",
                ))?;
        let debt_coverage_exception =
            post_withdraw_debt_coverage_exception(&pair, post_reserve0, post_reserve1)?;
        if debt_coverage_exception.is_some()
            && (cash_witness.is_none()
                || self.post_state_correction_policy
                    != V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi)
        {
            return Err(V1EngineError::InsufficientDebtCoverage);
        }
        let output_correction = (source_native_amount0_out != amount0_out
            || source_native_amount1_out != amount1_out)
            .then_some(V1RemoveLiquidityOutputCorrectionV1 {
                source_native_amount0_out,
                source_native_amount1_out,
                witnessed_amount0_out: amount0_out,
                witnessed_amount1_out: amount1_out,
                source_native_reserve0_after,
                source_native_reserve1_after,
                witnessed_reserve0_after: post_reserve0,
                witnessed_reserve1_after: post_reserve1,
            });

        pair.reserve0 = post_reserve0;
        pair.reserve1 = post_reserve1;
        pair.cash_reserve0 -= amount0_out;
        pair.cash_reserve1 -= amount1_out;
        pair.total_supply -= input.liquidity_burned;
        debit_reserve_vault(&mut custody, V1TokenSideV1::Token0, input.vault_debit0)?;
        debit_reserve_vault(&mut custody, V1TokenSideV1::Token1, input.vault_debit1)?;

        let source_native_cash_reserve0_after = pair.cash_reserve0;
        let source_native_cash_reserve1_after = pair.cash_reserve1;
        let source_native_total_debt0_after = pair.total_debt0;
        let source_native_total_debt1_after = pair.total_debt1;
        let cash_inventory_correction = if let Some(witness) = cash_witness {
            let cash0 = witness.event_cash_reserve0_after;
            let cash1 = witness.event_cash_reserve1_after;
            if cash0 > custody.reserve_vault0
                || cash1 > custody.reserve_vault1
                || cash0 > pair.reserve0
                || cash1 > pair.reserve1
            {
                return Err(V1EngineError::CheckpointMismatchField(
                    "lp_burn.cash_checkpoint_bounds",
                ));
            }
            let witnessed_total_debt0_after = pair.reserve0 - cash0;
            let witnessed_total_debt1_after = pair.reserve1 - cash1;
            let cash_checkpoint_rounding_atoms0 =
                i128::from(cash0) - i128::from(source_native_cash_reserve0_after);
            let cash_checkpoint_rounding_atoms1 =
                i128::from(cash1) - i128::from(source_native_cash_reserve1_after);

            pair.cash_reserve0 = cash0;
            pair.cash_reserve1 = cash1;
            pair.total_debt0 = witnessed_total_debt0_after;
            pair.total_debt1 = witnessed_total_debt1_after;

            (cash_checkpoint_rounding_atoms0 != 0 || cash_checkpoint_rounding_atoms1 != 0)
                .then_some(V1RemoveLiquidityCashInventoryCorrectionV1 {
                    source_native_cash_reserve0_after,
                    source_native_cash_reserve1_after,
                    witnessed_cash_reserve0_after: cash0,
                    witnessed_cash_reserve1_after: cash1,
                    source_native_total_debt0_after,
                    source_native_total_debt1_after,
                    witnessed_total_debt0_after,
                    witnessed_total_debt1_after,
                    cash_checkpoint_rounding_atoms0,
                    cash_checkpoint_rounding_atoms1,
                })
        } else {
            None
        };

        let receipt = V1RemoveLiquidityReceiptV1 {
            accrual,
            liquidity_burned: input.liquidity_burned,
            amount0_gross,
            amount1_gross,
            withdrawal_fee0,
            withdrawal_fee1,
            amount0_out,
            amount1_out,
            recipient_credit0: input.recipient_credit0,
            recipient_credit1: input.recipient_credit1,
            total_supply_after: pair.total_supply,
            output_correction,
            cash_inventory_correction,
            debt_coverage_exception,
        };
        self.pair = pair;
        self.custody = custody;
        self.revenue = revenue;
        Ok(receipt)
    }

    pub fn apply_collateral(
        &mut self,
        input: V1CollateralInputV1,
    ) -> Result<V1CollateralReceiptV1, V1EngineError> {
        self.apply_collateral_inner(input, None, None)
    }

    /// Applies a provisional historical add-collateral transition only when
    /// the direct-child position assertion uniquely proves preservation or
    /// recomputation under the linked source's exact native formula.
    pub fn apply_event_witnessed_collateral(
        &mut self,
        input: V1CollateralInputV1,
        witness: V1PositionCollateralFactorWitnessV1,
    ) -> Result<V1CollateralReceiptV1, V1EngineError> {
        if self.collateral_factor_transition_policy
            != V1CollateralFactorTransitionPolicyV1::EventWitnessedCollateralFactorTransition
        {
            return Err(V1EngineError::InvalidConfiguration(
                "event_witnessed_collateral_factor_transition_policy",
            ));
        }
        if !input.deposit {
            return Err(V1EngineError::InvalidConfiguration(
                "event_witnessed_collateral_factor_transition_requires_deposit",
            ));
        }
        self.apply_collateral_inner(input, Some(witness), None)
    }

    pub fn apply_event_and_cpi_witnessed_collateral_withdrawal(
        &mut self,
        input: V1CollateralInputV1,
        witness: V1CollateralWithdrawalWitnessV1,
    ) -> Result<V1CollateralReceiptV1, V1EngineError> {
        if self.post_state_correction_policy
            != V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        {
            return Err(V1EngineError::InvalidConfiguration(
                "decision_grade_event_and_token_cpi_correction_policy",
            ));
        }
        if input.deposit
            || witness.event_position != input.position
            || witness.event_amount != input.amount
            || witness.token_cpi_collateral_vault_debit != input.collateral_vault_amount
            || witness.token_cpi_recipient_credit != input.counterparty_amount
            || witness.event_amount != witness.token_cpi_collateral_vault_debit
            || witness.event_amount != witness.token_cpi_recipient_credit
        {
            return Err(V1EngineError::InvalidConfiguration(
                "collateral_withdrawal_event_and_token_cpi_witness",
            ));
        }
        self.apply_collateral_inner(input, None, Some(witness))
    }

    fn apply_collateral_inner(
        &mut self,
        input: V1CollateralInputV1,
        factor_witness: Option<V1PositionCollateralFactorWitnessV1>,
        withdrawal_witness: Option<V1CollateralWithdrawalWitnessV1>,
    ) -> Result<V1CollateralReceiptV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut position = copy_position(
            self.positions
                .get(&input.position)
                .ok_or(V1EngineError::MissingPosition(input.position))?,
        );
        let mut revenue = self.revenue;
        let mut custody = self.custody;
        let accrual = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            input.slot,
        )?;
        if input.amount == 0 {
            return Err(V1EngineError::AmountZero);
        }
        let mut deposit_factor_correction = None;
        let mut withdrawal_factor_correction = None;
        let mut withdrawal_post_capacity_exception = None;

        if input.deposit {
            if input.requested_amount != input.amount {
                return Err(V1EngineError::CustodyDeltaMismatch {
                    field: "collateral deposit requested amount",
                    expected: input.amount,
                    actual: input.requested_amount,
                });
            }
            if input.counterparty_amount != input.amount {
                return Err(V1EngineError::CustodyDeltaMismatch {
                    field: "collateral deposit counterparty debit",
                    expected: input.amount,
                    actual: input.counterparty_amount,
                });
            }
            match input.collateral_token {
                V1TokenSideV1::Token0 => {
                    pair.total_collateral0 = pair
                        .total_collateral0
                        .checked_add(input.amount)
                        .ok_or(V1EngineError::ArithmeticOverflow("token0 collateral total"))?;
                    position.collateral0 = position.collateral0.checked_add(input.amount).ok_or(
                        V1EngineError::ArithmeticOverflow("token0 position collateral"),
                    )?;
                }
                V1TokenSideV1::Token1 => {
                    pair.total_collateral1 = pair
                        .total_collateral1
                        .checked_add(input.amount)
                        .ok_or(V1EngineError::ArithmeticOverflow("token1 collateral total"))?;
                    position.collateral1 = position.collateral1.checked_add(input.amount).ok_or(
                        V1EngineError::ArithmeticOverflow("token1 position collateral"),
                    )?;
                }
            }
            if let Some(witness) = factor_witness {
                deposit_factor_correction = apply_event_witnessed_collateral_factor_transition(
                    &pair,
                    &mut position,
                    input.collateral_token,
                    witness,
                    self.post_state_correction_policy
                        == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi,
                )?;
            }
            credit_collateral_vault(
                &mut custody,
                input.collateral_token,
                input.collateral_vault_amount,
            )?;
        } else {
            if input.collateral_vault_amount != input.amount {
                return Err(V1EngineError::CustodyDeltaMismatch {
                    field: "collateral withdrawal vault debit",
                    expected: input.amount,
                    actual: input.collateral_vault_amount,
                });
            }
            let available = collateral_for_side(&position, input.collateral_token);
            if available < input.amount {
                return Err(V1EngineError::InsufficientCollateral {
                    available,
                    requested: input.amount,
                });
            }
            let debt_token = input.collateral_token.opposite();
            let debt = debt_for_side(&position, &pair, debt_token)?;
            let resolved_amount = if input.requested_amount == u64::MAX && debt == 0 {
                available
            } else {
                input.requested_amount
            };
            let witnessed_max_sentinel_resolution = withdrawal_witness.is_some()
                && input.requested_amount == u64::MAX
                && resolved_amount != input.amount;
            if resolved_amount != input.amount && !witnessed_max_sentinel_resolution {
                return Err(V1EngineError::CustodyDeltaMismatch {
                    field: "collateral withdrawal resolved amount",
                    expected: resolved_amount,
                    actual: input.amount,
                });
            }
            if (self.authority.global_reduce_only || pair.reduce_only) && debt > 0 {
                return Err(V1EngineError::ReduceOnly);
            }
            let remaining = available - input.amount;
            if debt > 0 {
                let (post_limit, _, _) =
                    borrow_limit_for_collateral(&pair, input.collateral_token, remaining)?;
                if post_limit < debt {
                    if withdrawal_witness.is_some() && input.requested_amount == u64::MAX {
                        withdrawal_post_capacity_exception =
                            Some(V1CollateralWithdrawalPostCapacityExceptionV1 {
                                post_withdraw_debt: debt,
                                admissible_max_debt: post_limit,
                                excess_debt_atoms: debt - post_limit,
                            });
                    } else {
                        return Err(V1EngineError::BorrowingPowerExceeded {
                            requested: debt,
                            maximum: post_limit,
                        });
                    }
                }
            }

            match input.collateral_token {
                V1TokenSideV1::Token0 => {
                    pair.total_collateral0 =
                        pair.total_collateral0.checked_sub(input.amount).ok_or(
                            V1EngineError::ArithmeticOverflow("token0 collateral total withdrawal"),
                        )?;
                    position.collateral0 -= input.amount;
                }
                V1TokenSideV1::Token1 => {
                    pair.total_collateral1 =
                        pair.total_collateral1.checked_sub(input.amount).ok_or(
                            V1EngineError::ArithmeticOverflow("token1 collateral total withdrawal"),
                        )?;
                    position.collateral1 -= input.amount;
                }
            }
            let (_, _, liquidation_cf_bps) =
                borrow_limit_for_collateral(&pair, input.collateral_token, remaining)?;
            position.set_liquidation_cf_for_debt_token(
                &mint_for_side(&pair, debt_token),
                &pair,
                liquidation_cf_bps,
            );
            debit_collateral_vault(
                &mut custody,
                input.collateral_token,
                input.collateral_vault_amount,
            )?;
            if let Some(witness) = withdrawal_witness {
                if witness.event_collateral0 != position.collateral0
                    || witness.event_collateral1 != position.collateral1
                    || witness.event_debt0_shares != position.debt0_shares
                    || witness.event_debt1_shares != position.debt1_shares
                    || witness.event_collateral0_max_cf_bps
                        != max_cf_from_liquidation(witness.event_collateral0_liquidation_cf_bps)
                    || witness.event_collateral1_max_cf_bps
                        != max_cf_from_liquidation(witness.event_collateral1_liquidation_cf_bps)
                {
                    return Err(V1EngineError::CheckpointMismatchField(
                        "collateral_withdrawal.position_post_state",
                    ));
                }
                let source_native = collateral_factor_witness(&position);
                match input.collateral_token {
                    V1TokenSideV1::Token0 => {
                        if witness.event_collateral1_max_cf_bps
                            != source_native.collateral1_max_cf_bps
                            || witness.event_collateral1_liquidation_cf_bps
                                != source_native.collateral1_liquidation_cf_bps
                        {
                            return Err(V1EngineError::CheckpointMismatchField(
                                "collateral_withdrawal.opposite_factor_lane",
                            ));
                        }
                        position.collateral0_liquidation_cf_bps =
                            witness.event_collateral0_liquidation_cf_bps;
                    }
                    V1TokenSideV1::Token1 => {
                        if witness.event_collateral0_max_cf_bps
                            != source_native.collateral0_max_cf_bps
                            || witness.event_collateral0_liquidation_cf_bps
                                != source_native.collateral0_liquidation_cf_bps
                        {
                            return Err(V1EngineError::CheckpointMismatchField(
                                "collateral_withdrawal.opposite_factor_lane",
                            ));
                        }
                        position.collateral1_liquidation_cf_bps =
                            witness.event_collateral1_liquidation_cf_bps;
                    }
                }
                let witnessed = collateral_factor_witness(&position);
                let expected_witness = V1PositionCollateralFactorWitnessV1 {
                    collateral0_max_cf_bps: witness.event_collateral0_max_cf_bps,
                    collateral1_max_cf_bps: witness.event_collateral1_max_cf_bps,
                    collateral0_liquidation_cf_bps: witness.event_collateral0_liquidation_cf_bps,
                    collateral1_liquidation_cf_bps: witness.event_collateral1_liquidation_cf_bps,
                };
                if witnessed != expected_witness {
                    return Err(V1EngineError::CheckpointMismatchField(
                        "collateral_withdrawal.factor_application",
                    ));
                }
                withdrawal_factor_correction = (source_native != witnessed
                    || witnessed_max_sentinel_resolution)
                    .then_some(V1CollateralWithdrawalFactorCorrectionV1 {
                        collateral_token: input.collateral_token,
                        requested_amount: input.requested_amount,
                        source_native_resolved_amount: resolved_amount,
                        witnessed_resolved_amount: input.amount,
                        source_native_collateral0_max_cf_bps: source_native.collateral0_max_cf_bps,
                        source_native_collateral1_max_cf_bps: source_native.collateral1_max_cf_bps,
                        source_native_collateral0_liquidation_cf_bps: source_native
                            .collateral0_liquidation_cf_bps,
                        source_native_collateral1_liquidation_cf_bps: source_native
                            .collateral1_liquidation_cf_bps,
                        witnessed_collateral0_max_cf_bps: witnessed.collateral0_max_cf_bps,
                        witnessed_collateral1_max_cf_bps: witnessed.collateral1_max_cf_bps,
                        witnessed_collateral0_liquidation_cf_bps: witnessed
                            .collateral0_liquidation_cf_bps,
                        witnessed_collateral1_liquidation_cf_bps: witnessed
                            .collateral1_liquidation_cf_bps,
                        post_capacity_exception: withdrawal_post_capacity_exception,
                    });
            }
        }

        let receipt = V1CollateralReceiptV1 {
            accrual,
            deposited: input.deposit,
            collateral_token: input.collateral_token,
            accounted_amount: input.amount,
            collateral_vault_amount: input.collateral_vault_amount,
            counterparty_amount: input.counterparty_amount,
            collateral0_after: position.collateral0,
            collateral1_after: position.collateral1,
            deposit_factor_correction,
            withdrawal_factor_correction,
        };
        self.pair = pair;
        self.positions.insert(input.position, position);
        self.custody = custody;
        self.revenue = revenue;
        Ok(receipt)
    }

    pub fn apply_liquidation(
        &mut self,
        input: V1LiquidationInputV1,
    ) -> Result<V1LiquidationReceiptV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut position = copy_position(
            self.positions
                .get(&input.position)
                .ok_or(V1EngineError::MissingPosition(input.position))?,
        );
        let mut revenue = self.revenue;
        let mut custody = self.custody;
        let accrual = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            input.slot,
        )?;
        let collateral_token = input.debt_token.opposite();
        let user_debt_shares = shares_for_side(&position, input.debt_token);
        if user_debt_shares == 0 {
            return Err(V1EngineError::ZeroDebt);
        }
        let user_debt = debt_for_side(&position, &pair, input.debt_token)?;
        let user_collateral = collateral_for_side(&position, collateral_token);
        let liquidation_cf_bps = liquidation_cf_for_debt(&position, input.debt_token);
        let (collateral_reserve, debt_reserve, collateral_ema_nad, total_debt, total_debt_shares) =
            match input.debt_token {
                V1TokenSideV1::Token0 => (
                    pair.reserve1,
                    pair.reserve0,
                    pair.last_price1_ema.symmetric,
                    pair.total_debt0,
                    pair.total_debt0_shares,
                ),
                V1TokenSideV1::Token1 => (
                    pair.reserve0,
                    pair.reserve1,
                    pair.last_price0_ema.symmetric,
                    pair.total_debt1,
                    pair.total_debt1_shares,
                ),
            };
        let k_before = pair.k();
        let (collateral_ema_reserve, debt_ema_reserve) =
            construct_virtual_reserves_at_pessimistic_price(
                collateral_reserve,
                debt_reserve,
                collateral_ema_nad,
                collateral_ema_nad,
            )
            .map_err(|error| {
                native_error("construct_virtual_reserves_at_pessimistic_price", error)
            })?;
        let collateral_value_with_impact = CPCurve::calculate_amount_out(
            collateral_ema_reserve,
            debt_ema_reserve,
            user_collateral,
        )
        .map_err(|error| native_error("CPCurve::calculate_amount_out", error))?;
        let borrow_limit = (collateral_value_with_impact as u128)
            .checked_mul(liquidation_cf_bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(V1EngineError::ArithmeticOverflow(
                "liquidation borrow limit",
            ))?;
        if (user_debt as u128) < borrow_limit {
            return Err(V1EngineError::NotUndercollateralized);
        }
        let is_insolvent = user_debt > collateral_value_with_impact;
        let debt_shares_written_off = if is_insolvent {
            user_debt_shares
        } else {
            ceil_div(
                user_debt_shares
                    .checked_mul(CLOSE_FACTOR_BPS as u128)
                    .ok_or(V1EngineError::ArithmeticOverflow(
                        "liquidation close-factor shares",
                    ))?,
                BPS_DENOMINATOR as u128,
            )
            .ok_or(V1EngineError::ArithmeticOverflow(
                "liquidation close-factor division",
            ))?
            .min(user_debt_shares)
        };
        let debt_written_off = if total_debt_shares == 0 {
            0
        } else {
            u64::try_from(
                debt_shares_written_off
                    .checked_mul(total_debt as u128)
                    .and_then(|value| value.checked_div(total_debt_shares))
                    .ok_or(V1EngineError::ArithmeticOverflow(
                        "liquidation debt writeoff",
                    ))?,
            )
            .map_err(|_| V1EngineError::ArithmeticOverflow("liquidation debt conversion"))?
            .min(user_debt)
        };
        let collateral_base = CPCurve::calculate_amount_in(
            collateral_ema_reserve,
            debt_ema_reserve,
            debt_written_off,
        )
        .map_err(|error| native_error("CPCurve::calculate_amount_in", error))?;
        let collateral_with_penalty = ceil_div(
            (collateral_base as u128)
                .checked_mul((BPS_DENOMINATOR + LIQUIDATION_PENALTY_BPS) as u128)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "liquidation collateral penalty",
                ))?,
            BPS_DENOMINATOR as u128,
        )
        .ok_or(V1EngineError::ArithmeticOverflow(
            "liquidation collateral penalty division",
        ))?;
        let collateral_seized = u64::try_from(collateral_with_penalty.min(user_collateral as u128))
            .map_err(|_| V1EngineError::ArithmeticOverflow("liquidation collateral conversion"))?;
        let collateral_after = user_collateral.checked_sub(collateral_seized).ok_or(
            V1EngineError::ArithmeticOverflow("post-liquidation collateral"),
        )?;
        let (_, _, next_liquidation_cf_bps) =
            borrow_limit_for_collateral(&pair, collateral_token, collateral_after)?;
        let liquidator_incentive = u64::try_from(
            (collateral_base as u128)
                .checked_mul(LIQUIDATION_INCENTIVE_BPS as u128)
                .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
                .ok_or(V1EngineError::ArithmeticOverflow("liquidator incentive"))?,
        )
        .map_err(|_| V1EngineError::ArithmeticOverflow("liquidator incentive conversion"))?
        .min(collateral_seized);
        let collateral_to_reserves = collateral_seized.checked_sub(liquidator_incentive).ok_or(
            V1EngineError::ArithmeticOverflow("liquidation reserve collateral"),
        )?;
        if input.reserve_vault_credit > collateral_to_reserves {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "liquidation reserve vault credit",
                expected: collateral_to_reserves,
                actual: input.reserve_vault_credit,
            });
        }
        if input.liquidator_recipient_credit > liquidator_incentive {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "liquidation recipient credit",
                expected: liquidator_incentive,
                actual: input.liquidator_recipient_credit,
            });
        }

        let debt_mint = mint_for_side(&pair, input.debt_token);
        position
            .decrease_debt(
                &mut pair,
                &debt_mint,
                debt_written_off,
                DebtDecreaseReason::WriteOff(debt_shares_written_off),
            )
            .map_err(|error| native_error("UserPosition::decrease_debt liquidation", error))?;
        position.set_liquidation_cf_for_debt_token(&debt_mint, &pair, next_liquidation_cf_bps);
        match collateral_token {
            V1TokenSideV1::Token0 => {
                position.collateral0 -= collateral_seized;
                pair.total_collateral0 = pair
                    .total_collateral0
                    .checked_sub(collateral_seized)
                    .ok_or(V1EngineError::ArithmeticOverflow(
                        "token0 liquidation collateral total",
                    ))?;
                pair.reserve0 = pair.reserve0.checked_add(collateral_to_reserves).ok_or(
                    V1EngineError::ArithmeticOverflow("token0 liquidation reserve"),
                )?;
                pair.cash_reserve0 = pair.cash_reserve0.saturating_add(collateral_to_reserves);
            }
            V1TokenSideV1::Token1 => {
                position.collateral1 -= collateral_seized;
                pair.total_collateral1 = pair
                    .total_collateral1
                    .checked_sub(collateral_seized)
                    .ok_or(V1EngineError::ArithmeticOverflow(
                        "token1 liquidation collateral total",
                    ))?;
                pair.reserve1 = pair.reserve1.checked_add(collateral_to_reserves).ok_or(
                    V1EngineError::ArithmeticOverflow("token1 liquidation reserve"),
                )?;
                pair.cash_reserve1 = pair.cash_reserve1.saturating_add(collateral_to_reserves);
            }
        }
        debit_collateral_vault(&mut custody, collateral_token, collateral_seized)?;
        credit_reserve_vault(&mut custody, collateral_token, input.reserve_vault_credit)?;

        let collateral_price_nad = match collateral_token {
            V1TokenSideV1::Token0 => pair.last_price0_ema.symmetric,
            V1TokenSideV1::Token1 => pair.last_price1_ema.symmetric,
        };
        let receipt = V1LiquidationReceiptV1 {
            accrual,
            debt_token: input.debt_token,
            debt_written_off,
            debt_shares_written_off,
            collateral_seized,
            collateral_to_reserves,
            reserve_vault_credit: input.reserve_vault_credit,
            liquidator_incentive,
            liquidator_recipient_credit: input.liquidator_recipient_credit,
            collateral_price_nad,
            shortfall: if is_insolvent {
                (user_debt as u128).saturating_sub(collateral_value_with_impact as u128)
            } else {
                0
            },
            k_before,
            k_after: pair.k(),
            transition_correction: None,
        };
        self.pair = pair;
        self.positions.insert(input.position, position);
        self.custody = custody;
        self.revenue = revenue;
        Ok(receipt)
    }

    /// Runs every linked-source liquidation guard first, then reconciles the
    /// raw transition amounts to a unique direct-child event and its three
    /// instruction-scoped token CPIs. This provisional path may only select a
    /// smaller historical quote; the native close-factor share burn remains
    /// unchanged.
    pub fn apply_event_and_cpi_witnessed_liquidation(
        &mut self,
        input: V1LiquidationInputV1,
        witness: V1LiquidationTransitionWitnessV1,
    ) -> Result<V1LiquidationReceiptV1, V1EngineError> {
        if self.post_state_correction_policy
            != V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        {
            return Err(V1EngineError::InvalidConfiguration(
                "post_state_correction_policy",
            ));
        }
        if witness.event_position != input.position
            || witness.event_debt_token != input.debt_token
            || witness.event_debt_written_off == 0
            || witness.event_collateral_seized == 0
            || witness.token_cpi_collateral_vault_debit != witness.event_collateral_seized
            || witness.token_cpi_reserve_vault_credit != input.reserve_vault_credit
            || witness.token_cpi_liquidator_recipient_credit != input.liquidator_recipient_credit
            || witness.event_liquidator_incentive != witness.token_cpi_liquidator_recipient_credit
            || witness
                .token_cpi_reserve_vault_credit
                .checked_add(witness.token_cpi_liquidator_recipient_credit)
                != Some(witness.event_collateral_seized)
        {
            return Err(V1EngineError::CheckpointMismatchField(
                "liquidation.event_cpi_tuple",
            ));
        }

        // Retain the exact post-accrual pair used by the native capacity and
        // factor formulas. No state from this probe is committed.
        let mut post_accrual = self.try_fork();
        post_accrual.advance_to_slot(input.slot)?;

        let mut staged = self.try_fork();
        let mut receipt = staged.apply_liquidation(input)?;
        if witness.event_debt_written_off > receipt.debt_written_off {
            return Err(V1EngineError::CheckpointMismatchField(
                "liquidation.debt_exceeds_native_quote",
            ));
        }
        if witness.event_collateral_seized > receipt.collateral_seized {
            return Err(V1EngineError::CheckpointMismatchField(
                "liquidation.collateral_exceeds_native_quote",
            ));
        }
        if witness.event_liquidator_incentive > receipt.liquidator_incentive {
            return Err(V1EngineError::CheckpointMismatchField(
                "liquidation.incentive_exceeds_native_quote",
            ));
        }
        if witness.event_shortfall != receipt.shortfall {
            return Err(V1EngineError::CheckpointValueMismatch {
                field: "liquidation.native_shortfall",
                event: witness.event_shortfall,
                replayed: receipt.shortfall,
            });
        }

        let collateral_token = input.debt_token.opposite();
        let source_native_debt_written_off = receipt.debt_written_off;
        let source_native_collateral_seized = receipt.collateral_seized;
        let source_native_collateral_to_reserves = receipt.collateral_to_reserves;
        let source_native_liquidator_incentive = receipt.liquidator_incentive;
        let witnessed_collateral_to_reserves = witness
            .event_collateral_seized
            .checked_sub(witness.event_liquidator_incentive)
            .ok_or(V1EngineError::CheckpointMismatchField(
                "liquidation.collateral_split",
            ))?;
        if witnessed_collateral_to_reserves != input.reserve_vault_credit
            || witnessed_collateral_to_reserves > source_native_collateral_to_reserves
        {
            return Err(V1EngineError::CheckpointMismatchField(
                "liquidation.reserve_credit",
            ));
        }

        let source_native_position_collateral_after = staged
            .positions
            .get(&input.position)
            .map(|position| collateral_for_side(position, collateral_token))
            .ok_or(V1EngineError::MissingPosition(input.position))?;
        let (
            source_native_debt_reserve_after,
            source_native_total_debt_after,
            source_native_collateral_reserve_after,
            source_native_collateral_cash_after,
            source_native_total_collateral_after,
            source_native_collateral_vault_after,
        ) = match input.debt_token {
            V1TokenSideV1::Token0 => (
                staged.pair.reserve0,
                staged.pair.total_debt0,
                staged.pair.reserve1,
                staged.pair.cash_reserve1,
                staged.pair.total_collateral1,
                staged.custody.collateral_vault1,
            ),
            V1TokenSideV1::Token1 => (
                staged.pair.reserve1,
                staged.pair.total_debt1,
                staged.pair.reserve0,
                staged.pair.cash_reserve0,
                staged.pair.total_collateral0,
                staged.custody.collateral_vault0,
            ),
        };

        let debt_quote_gap = source_native_debt_written_off
            .checked_sub(witness.event_debt_written_off)
            .ok_or(V1EngineError::CheckpointMismatchField(
                "liquidation.debt_quote_gap",
            ))?;
        match input.debt_token {
            V1TokenSideV1::Token0 => {
                staged.pair.total_debt0 =
                    staged.pair.total_debt0.checked_add(debt_quote_gap).ok_or(
                        V1EngineError::ArithmeticOverflow("liquidation token0 witnessed debt"),
                    )?;
                staged.pair.reserve0 = staged.pair.reserve0.checked_add(debt_quote_gap).ok_or(
                    V1EngineError::ArithmeticOverflow("liquidation token0 witnessed reserve"),
                )?;
            }
            V1TokenSideV1::Token1 => {
                staged.pair.total_debt1 =
                    staged.pair.total_debt1.checked_add(debt_quote_gap).ok_or(
                        V1EngineError::ArithmeticOverflow("liquidation token1 witnessed debt"),
                    )?;
                staged.pair.reserve1 = staged.pair.reserve1.checked_add(debt_quote_gap).ok_or(
                    V1EngineError::ArithmeticOverflow("liquidation token1 witnessed reserve"),
                )?;
            }
        }

        let collateral_quote_gap = source_native_collateral_seized
            .checked_sub(witness.event_collateral_seized)
            .ok_or(V1EngineError::CheckpointMismatchField(
                "liquidation.collateral_quote_gap",
            ))?;
        let reserve_quote_gap = source_native_collateral_to_reserves
            .checked_sub(witnessed_collateral_to_reserves)
            .ok_or(V1EngineError::CheckpointMismatchField(
                "liquidation.reserve_quote_gap",
            ))?;
        {
            let position = staged
                .positions
                .get_mut(&input.position)
                .ok_or(V1EngineError::MissingPosition(input.position))?;
            match collateral_token {
                V1TokenSideV1::Token0 => {
                    position.collateral0 = position
                        .collateral0
                        .checked_add(collateral_quote_gap)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                        "liquidation token0 witnessed position collateral",
                    ))?;
                }
                V1TokenSideV1::Token1 => {
                    position.collateral1 = position
                        .collateral1
                        .checked_add(collateral_quote_gap)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                        "liquidation token1 witnessed position collateral",
                    ))?;
                }
            }
        }
        match collateral_token {
            V1TokenSideV1::Token0 => {
                staged.pair.total_collateral0 = staged
                    .pair
                    .total_collateral0
                    .checked_add(collateral_quote_gap)
                    .ok_or(V1EngineError::ArithmeticOverflow(
                        "liquidation token0 witnessed total collateral",
                    ))?;
                staged.pair.reserve0 = staged.pair.reserve0.checked_sub(reserve_quote_gap).ok_or(
                    V1EngineError::ArithmeticOverflow(
                        "liquidation token0 witnessed reserve credit",
                    ),
                )?;
                staged.pair.cash_reserve0 = staged
                    .pair
                    .cash_reserve0
                    .checked_sub(reserve_quote_gap)
                    .ok_or(V1EngineError::ArithmeticOverflow(
                        "liquidation token0 witnessed cash credit",
                    ))?;
            }
            V1TokenSideV1::Token1 => {
                staged.pair.total_collateral1 = staged
                    .pair
                    .total_collateral1
                    .checked_add(collateral_quote_gap)
                    .ok_or(V1EngineError::ArithmeticOverflow(
                        "liquidation token1 witnessed total collateral",
                    ))?;
                staged.pair.reserve1 = staged.pair.reserve1.checked_sub(reserve_quote_gap).ok_or(
                    V1EngineError::ArithmeticOverflow(
                        "liquidation token1 witnessed reserve credit",
                    ),
                )?;
                staged.pair.cash_reserve1 = staged
                    .pair
                    .cash_reserve1
                    .checked_sub(reserve_quote_gap)
                    .ok_or(V1EngineError::ArithmeticOverflow(
                        "liquidation token1 witnessed cash credit",
                    ))?;
            }
        }
        credit_collateral_vault(&mut staged.custody, collateral_token, collateral_quote_gap)?;

        let witnessed_position_collateral_after = staged
            .positions
            .get(&input.position)
            .map(|position| collateral_for_side(position, collateral_token))
            .ok_or(V1EngineError::MissingPosition(input.position))?;
        let (_, _, witnessed_liquidation_cf_bps) = borrow_limit_for_collateral(
            &post_accrual.pair,
            collateral_token,
            witnessed_position_collateral_after,
        )?;
        let debt_mint = mint_for_side(&staged.pair, input.debt_token);
        staged
            .positions
            .get_mut(&input.position)
            .ok_or(V1EngineError::MissingPosition(input.position))?
            .set_liquidation_cf_for_debt_token(
                &debt_mint,
                &staged.pair,
                witnessed_liquidation_cf_bps,
            );

        let (
            witnessed_debt_reserve_after,
            witnessed_total_debt_after,
            witnessed_collateral_reserve_after,
            witnessed_collateral_cash_after,
            witnessed_total_collateral_after,
            witnessed_collateral_vault_after,
        ) = match input.debt_token {
            V1TokenSideV1::Token0 => (
                staged.pair.reserve0,
                staged.pair.total_debt0,
                staged.pair.reserve1,
                staged.pair.cash_reserve1,
                staged.pair.total_collateral1,
                staged.custody.collateral_vault1,
            ),
            V1TokenSideV1::Token1 => (
                staged.pair.reserve1,
                staged.pair.total_debt1,
                staged.pair.reserve0,
                staged.pair.cash_reserve0,
                staged.pair.total_collateral0,
                staged.custody.collateral_vault0,
            ),
        };
        let transition_correction = (source_native_debt_written_off
            != witness.event_debt_written_off
            || source_native_collateral_seized != witness.event_collateral_seized
            || source_native_collateral_to_reserves != witnessed_collateral_to_reserves)
            .then_some(V1LiquidationTransitionCorrectionV1 {
                collateral_token,
                source_native_debt_written_off,
                witnessed_debt_written_off: witness.event_debt_written_off,
                debt_shares_written_off: receipt.debt_shares_written_off,
                source_native_collateral_seized,
                witnessed_collateral_seized: witness.event_collateral_seized,
                source_native_collateral_to_reserves,
                witnessed_collateral_to_reserves,
                source_native_liquidator_incentive,
                witnessed_liquidator_incentive: witness.event_liquidator_incentive,
                source_native_debt_reserve_after,
                witnessed_debt_reserve_after,
                source_native_total_debt_after,
                witnessed_total_debt_after,
                source_native_collateral_reserve_after,
                witnessed_collateral_reserve_after,
                source_native_collateral_cash_after,
                witnessed_collateral_cash_after,
                source_native_total_collateral_after,
                witnessed_total_collateral_after,
                source_native_position_collateral_after,
                witnessed_position_collateral_after,
                source_native_collateral_vault_after,
                witnessed_collateral_vault_after,
            });

        receipt.debt_written_off = witness.event_debt_written_off;
        receipt.collateral_seized = witness.event_collateral_seized;
        receipt.collateral_to_reserves = witnessed_collateral_to_reserves;
        receipt.liquidator_incentive = witness.event_liquidator_incentive;
        receipt.shortfall = witness.event_shortfall;
        receipt.k_after = staged.pair.k();
        receipt.transition_correction = transition_correction;
        *self = staged;
        Ok(receipt)
    }

    pub fn apply_flashloan(
        &mut self,
        input: V1FlashloanInputV1,
    ) -> Result<V1FlashloanReceiptV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut revenue = self.revenue;
        let mut custody = self.custody;
        let accrual = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            input.slot,
        )?;
        if input.amount0 == 0 && input.amount1 == 0 {
            return Err(V1EngineError::AmountZero);
        }
        if input.amount0 > pair.cash_reserve0 {
            return Err(V1EngineError::InsufficientCash {
                token: 0,
                available: pair.cash_reserve0,
                required: input.amount0,
            });
        }
        if input.amount1 > pair.cash_reserve1 {
            return Err(V1EngineError::InsufficientCash {
                token: 1,
                available: pair.cash_reserve1,
                required: input.amount1,
            });
        }
        let fee0 = proportional_ceil(input.amount0, FLASHLOAN_FEE_BPS)?;
        let fee1 = proportional_ceil(input.amount1, FLASHLOAN_FEE_BPS)?;
        if input.net_vault_credit0 < fee0 {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "flashloan net_vault_credit0",
                expected: fee0,
                actual: input.net_vault_credit0,
            });
        }
        if input.net_vault_credit1 < fee1 {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "flashloan net_vault_credit1",
                expected: fee1,
                actual: input.net_vault_credit1,
            });
        }
        pair.reserve0 = pair.reserve0.saturating_add(fee0);
        pair.reserve1 = pair.reserve1.saturating_add(fee1);
        pair.cash_reserve0 = pair.cash_reserve0.saturating_add(fee0);
        pair.cash_reserve1 = pair.cash_reserve1.saturating_add(fee1);
        credit_reserve_vault(&mut custody, V1TokenSideV1::Token0, input.net_vault_credit0)?;
        credit_reserve_vault(&mut custody, V1TokenSideV1::Token1, input.net_vault_credit1)?;
        revenue.flashloan_fee0 = revenue.flashloan_fee0.checked_add(fee0 as u128).ok_or(
            V1EngineError::ArithmeticOverflow("cumulative token0 flashloan fee"),
        )?;
        revenue.flashloan_fee1 = revenue.flashloan_fee1.checked_add(fee1 as u128).ok_or(
            V1EngineError::ArithmeticOverflow("cumulative token1 flashloan fee"),
        )?;

        let receipt = V1FlashloanReceiptV1 {
            accrual,
            amount0: input.amount0,
            amount1: input.amount1,
            fee0,
            fee1,
            net_vault_credit0: input.net_vault_credit0,
            net_vault_credit1: input.net_vault_credit1,
        };
        self.pair = pair;
        self.custody = custody;
        self.revenue = revenue;
        Ok(receipt)
    }

    pub fn apply_protocol_claim(
        &mut self,
        input: V1ProtocolClaimInputV1,
    ) -> Result<V1ProtocolClaimReceiptV1, V1EngineError> {
        self.apply_protocol_claim_inner(input, false)
    }

    /// Applies the exact instruction-scoped reserve-vault debit witnessed by
    /// canonical token CPIs when historical V1 realized less than the linked
    /// source revision's modeled full protocol inventory.
    pub fn apply_cpi_witnessed_protocol_claim(
        &mut self,
        input: V1ProtocolClaimInputV1,
    ) -> Result<V1ProtocolClaimReceiptV1, V1EngineError> {
        if self.post_state_correction_policy
            != V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
        {
            return Err(V1EngineError::InvalidConfiguration(
                "post_state_correction_policy",
            ));
        }
        self.apply_protocol_claim_inner(input, true)
    }

    fn apply_protocol_claim_inner(
        &mut self,
        input: V1ProtocolClaimInputV1,
        accept_cpi_realized_partial_claim: bool,
    ) -> Result<V1ProtocolClaimReceiptV1, V1EngineError> {
        let mut pair = copy_pair(&self.pair);
        let mut revenue = self.revenue;
        let mut custody = self.custody;
        let accrual = advance_pair(
            &mut pair,
            &self.rate_model,
            self.authority.interest_bps,
            &mut revenue,
            input.slot,
        )?;
        let claimable0 = custody.reserve_vault0.saturating_sub(pair.cash_reserve0);
        let claimable1 = custody.reserve_vault1.saturating_sub(pair.cash_reserve1);
        if accept_cpi_realized_partial_claim && input.amount0 == 0 && input.amount1 == 0 {
            return Err(V1EngineError::AmountZero);
        }
        let amount0_is_valid = if accept_cpi_realized_partial_claim {
            input.amount0 <= claimable0
        } else {
            input.amount0 == claimable0
        };
        if !amount0_is_valid {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "protocol claim amount0",
                expected: claimable0,
                actual: input.amount0,
            });
        }
        let amount1_is_valid = if accept_cpi_realized_partial_claim {
            input.amount1 <= claimable1
        } else {
            input.amount1 == claimable1
        };
        if !amount1_is_valid {
            return Err(V1EngineError::CustodyDeltaMismatch {
                field: "protocol claim amount1",
                expected: claimable1,
                actual: input.amount1,
            });
        }
        let distribution = &self.authority.distribution;
        if distribution
            .futarchy_treasury_bps
            .saturating_add(distribution.buybacks_vault_bps)
            .saturating_add(distribution.team_treasury_bps)
            != BPS_DENOMINATOR
        {
            return Err(V1EngineError::InvalidConfiguration("revenue_distribution"));
        }
        // The configured split remains a model attribution unless a direct
        // ProtocolFeeClaim event proves the recipient tuple. It is calculated
        // only over the physically realized CPI amount, never the unclaimed
        // residual inventory.
        let buybacks_amount0 = proportional_floor(input.amount0, distribution.buybacks_vault_bps)?;
        let buybacks_amount1 = proportional_floor(input.amount1, distribution.buybacks_vault_bps)?;
        let team_amount0 = proportional_floor(input.amount0, distribution.team_treasury_bps)?;
        let team_amount1 = proportional_floor(input.amount1, distribution.team_treasury_bps)?;
        let futarchy_amount0 = input
            .amount0
            .saturating_sub(buybacks_amount0)
            .saturating_sub(team_amount0);
        let futarchy_amount1 = input
            .amount1
            .saturating_sub(buybacks_amount1)
            .saturating_sub(team_amount1);
        let reserve_vault0_before_claim = custody.reserve_vault0;
        let reserve_vault1_before_claim = custody.reserve_vault1;
        debit_reserve_vault(&mut custody, V1TokenSideV1::Token0, input.amount0)?;
        debit_reserve_vault(&mut custody, V1TokenSideV1::Token1, input.amount1)?;
        revenue.protocol_claim_realized0 = revenue
            .protocol_claim_realized0
            .checked_add(input.amount0 as u128)
            .ok_or(V1EngineError::ArithmeticOverflow(
                "cumulative token0 protocol claims",
            ))?;
        revenue.protocol_claim_realized1 = revenue
            .protocol_claim_realized1
            .checked_add(input.amount1 as u128)
            .ok_or(V1EngineError::ArithmeticOverflow(
                "cumulative token1 protocol claims",
            ))?;

        let modeled_residual0 = claimable0 - input.amount0;
        let modeled_residual1 = claimable1 - input.amount1;
        let realization_correction = (accept_cpi_realized_partial_claim
            && (modeled_residual0 != 0 || modeled_residual1 != 0))
            .then_some(V1ProtocolClaimRealizationCorrectionV1 {
                source_native_claimable0: claimable0,
                source_native_claimable1: claimable1,
                witnessed_realized0: input.amount0,
                witnessed_realized1: input.amount1,
                modeled_residual0,
                modeled_residual1,
                reserve_vault0_before_claim,
                reserve_vault1_before_claim,
                reserve_vault0_after_claim: custody.reserve_vault0,
                reserve_vault1_after_claim: custody.reserve_vault1,
            });

        let receipt = V1ProtocolClaimReceiptV1 {
            accrual,
            claimable0: input.amount0,
            claimable1: input.amount1,
            futarchy_amount0,
            futarchy_amount1,
            buybacks_amount0,
            buybacks_amount1,
            team_amount0,
            team_amount1,
            realization_correction,
        };
        self.pair = pair;
        self.custody = custody;
        self.revenue = revenue;
        Ok(receipt)
    }

    pub fn set_pair_reduce_only(&mut self, reduce_only: bool) {
        self.pair.reduce_only = reduce_only;
    }

    pub fn set_global_reduce_only(&mut self, reduce_only: bool) {
        self.authority.global_reduce_only = reduce_only;
    }

    pub fn set_revenue_share(
        &mut self,
        swap_bps: Option<u16>,
        interest_bps: Option<u16>,
    ) -> Result<(), V1EngineError> {
        if let Some(swap_bps) = swap_bps {
            if swap_bps > BPS_DENOMINATOR {
                return Err(V1EngineError::InvalidConfiguration("swap_bps"));
            }
            self.authority.swap_bps = swap_bps;
        }
        if let Some(interest_bps) = interest_bps {
            if interest_bps > BPS_DENOMINATOR {
                return Err(V1EngineError::InvalidConfiguration("interest_bps"));
            }
            self.authority.interest_bps = interest_bps;
        }
        Ok(())
    }

    pub fn set_revenue_distribution(
        &mut self,
        distribution: RevenueDistribution,
    ) -> Result<(), V1EngineError> {
        if !distribution.is_valid() {
            return Err(V1EngineError::InvalidConfiguration("revenue_distribution"));
        }
        self.authority.distribution = distribution;
        Ok(())
    }

    pub fn set_revenue_recipients(&mut self, recipients: RevenueRecipients) {
        self.authority.recipients = recipients;
    }

    pub fn set_authority(&mut self, authority: Pubkey) {
        self.authority.authority = authority;
    }

    pub fn set_rate_model(&mut self, key: Pubkey, model: RateModel) {
        self.pair.rate_model = key;
        self.rate_model_key = key;
        self.rate_model = model;
    }
}

fn canonical_initialize_params_hash(input: &V1CanonicalInitializationInputV1) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(input.pair_account_version.to_le_bytes());
    digest.update(input.swap_fee_bps.to_le_bytes());
    digest.update(input.half_life_ms.to_le_bytes());
    digest.update(input.fixed_cf_bps.unwrap_or(0).to_le_bytes());
    digest.update(input.target_util_start_bps_arg.unwrap_or(0).to_le_bytes());
    digest.update(input.target_util_end_bps_arg.unwrap_or(0).to_le_bytes());
    digest.update(input.rate_half_life_ms_arg.unwrap_or(0).to_le_bytes());
    digest.update(input.min_rate_bps_arg.unwrap_or(0).to_le_bytes());
    digest.update(input.max_rate_bps_arg.unwrap_or(0).to_le_bytes());
    digest.finalize().into()
}

/// Exact V1 EMA formula with an explicit elapsed time. `Pair::update` delegates
/// elapsed time to the Clock sysvar; replay supplies the historical slot and
/// keeps the remaining integer operations byte-for-byte equivalent.
pub fn compute_v1_ema_from_elapsed_ms(
    last_ema: u64,
    input: u64,
    elapsed_ms: u64,
    half_life_ms: u64,
) -> u64 {
    if elapsed_ms == 0 || half_life_ms == 0 {
        return last_ema;
    }
    let x = (elapsed_ms as u128 * NATURAL_LOG_OF_TWO_NAD as u128) / half_life_ms as u128;
    let alpha = taylor_exp(-(x as i64), NAD, TAYLOR_TERMS);
    ((input as u128 * (NAD - alpha) as u128 + last_ema as u128 * alpha as u128) / NAD as u128)
        as u64
}

fn advance_pair(
    pair: &mut Pair,
    rate_model: &RateModel,
    interest_bps: u16,
    revenue: &mut V1RevenueCheckpointV1,
    slot: u64,
) -> Result<V1InterestAccrualV1, V1EngineError> {
    let slot_before = pair.last_update;
    if slot < slot_before {
        return Err(V1EngineError::NonMonotonicSlot {
            current: slot_before,
            requested: slot,
        });
    }

    let spot0 = pair.spot_price0_nad();
    let spot1 = pair.spot_price1_nad();
    let same_instruction_directional0 = pair.last_price0_ema.directional.min(spot0);
    let same_instruction_directional1 = pair.last_price1_ema.directional.min(spot1);
    let utilization0 = utilization_nad(pair.total_debt0, pair.reserve0);
    let utilization1 = utilization_nad(pair.total_debt1, pair.reserve1);
    if slot == slot_before {
        pair.last_price0_ema.directional = same_instruction_directional0;
        pair.last_price1_ema.directional = same_instruction_directional1;
        return Ok(V1InterestAccrualV1 {
            slot_before,
            slot_after: slot,
            elapsed_ms: 0,
            utilization0_nad: utilization0,
            utilization1_nad: utilization1,
            rate0_nad: pair.last_rate0,
            rate1_nad: pair.last_rate1,
            integral0_nad: 0,
            integral1_nad: 0,
            lp_interest0: 0,
            lp_interest1: 0,
            protocol_interest0: 0,
            protocol_interest1: 0,
            borrower_cost0: 0,
            borrower_cost1: 0,
            cash_covered_protocol_interest0: 0,
            cash_covered_protocol_interest1: 0,
        });
    }

    let elapsed_ms = slots_to_ms(slot_before, slot).ok_or(V1EngineError::ArithmeticOverflow(
        "slot-to-millisecond conversion",
    ))?;
    let symmetric0 = compute_v1_ema_from_elapsed_ms(
        pair.last_price0_ema.symmetric,
        spot0,
        elapsed_ms,
        pair.half_life,
    );
    let symmetric1 = compute_v1_ema_from_elapsed_ms(
        pair.last_price1_ema.symmetric,
        spot1,
        elapsed_ms,
        pair.half_life,
    );
    let directional0 = spot0.min(compute_v1_ema_from_elapsed_ms(
        same_instruction_directional0,
        spot0,
        elapsed_ms,
        DIRECTIONAL_EMA_HALF_LIFE_MS,
    ));
    let directional1 = spot1.min(compute_v1_ema_from_elapsed_ms(
        same_instruction_directional1,
        spot1,
        elapsed_ms,
        DIRECTIONAL_EMA_HALF_LIFE_MS,
    ));

    let (rate0, integral0) = rate_model.calculate_rate(pair.last_rate0, elapsed_ms, utilization0);
    let (rate1, integral1) = rate_model.calculate_rate(pair.last_rate1, elapsed_ms, utilization1);
    let total_interest0 = ceil_div(
        (pair.total_debt0 as u128)
            .checked_mul(integral0 as u128)
            .ok_or(V1EngineError::ArithmeticOverflow("token0 interest"))?,
        NAD as u128,
    )
    .ok_or(V1EngineError::ArithmeticOverflow(
        "token0 interest division",
    ))?;
    let total_interest1 = ceil_div(
        (pair.total_debt1 as u128)
            .checked_mul(integral1 as u128)
            .ok_or(V1EngineError::ArithmeticOverflow("token1 interest"))?,
        NAD as u128,
    )
    .ok_or(V1EngineError::ArithmeticOverflow(
        "token1 interest division",
    ))?;
    let protocol_interest0 = u64::try_from(
        total_interest0.checked_mul(interest_bps as u128).ok_or(
            V1EngineError::ArithmeticOverflow("token0 protocol interest"),
        )? / BPS_DENOMINATOR as u128,
    )
    .unwrap_or(u64::MAX);
    let protocol_interest1 = u64::try_from(
        total_interest1.checked_mul(interest_bps as u128).ok_or(
            V1EngineError::ArithmeticOverflow("token1 protocol interest"),
        )? / BPS_DENOMINATOR as u128,
    )
    .unwrap_or(u64::MAX);
    let lp_interest0 = u64::try_from(total_interest0).unwrap_or(u64::MAX);
    let lp_interest1 = u64::try_from(total_interest1).unwrap_or(u64::MAX);
    let borrower_cost0 = lp_interest0
        .checked_add(protocol_interest0)
        .ok_or(V1EngineError::ArithmeticOverflow("token0 borrower cost"))?;
    let borrower_cost1 = lp_interest1
        .checked_add(protocol_interest1)
        .ok_or(V1EngineError::ArithmeticOverflow("token1 borrower cost"))?;
    let total_debt0 = pair
        .total_debt0
        .checked_add(borrower_cost0)
        .ok_or(V1EngineError::ArithmeticOverflow("token0 total debt"))?;
    let total_debt1 = pair
        .total_debt1
        .checked_add(borrower_cost1)
        .ok_or(V1EngineError::ArithmeticOverflow("token1 total debt"))?;
    let cash_covered0 = protocol_interest0.min(pair.cash_reserve0);
    let cash_covered1 = protocol_interest1.min(pair.cash_reserve1);
    let reserve_delta0 = lp_interest0
        .checked_add(protocol_interest0 - cash_covered0)
        .ok_or(V1EngineError::ArithmeticOverflow("token0 reserve interest"))?;
    let reserve_delta1 = lp_interest1
        .checked_add(protocol_interest1 - cash_covered1)
        .ok_or(V1EngineError::ArithmeticOverflow("token1 reserve interest"))?;
    let reserve0 = pair.reserve0.saturating_add(reserve_delta0);
    let reserve1 = pair.reserve1.saturating_add(reserve_delta1);
    let cash_reserve0 = pair.cash_reserve0 - cash_covered0;
    let cash_reserve1 = pair.cash_reserve1 - cash_covered1;

    add_interest_revenue(
        revenue,
        lp_interest0,
        lp_interest1,
        protocol_interest0,
        protocol_interest1,
        cash_covered0,
        cash_covered1,
    )?;
    pair.last_price0_ema.symmetric = symmetric0;
    pair.last_price1_ema.symmetric = symmetric1;
    pair.last_price0_ema.directional = directional0;
    pair.last_price1_ema.directional = directional1;
    pair.last_rate0 = rate0;
    pair.last_rate1 = rate1;
    pair.total_debt0 = total_debt0;
    pair.total_debt1 = total_debt1;
    pair.reserve0 = reserve0;
    pair.reserve1 = reserve1;
    pair.cash_reserve0 = cash_reserve0;
    pair.cash_reserve1 = cash_reserve1;
    pair.last_update = slot;

    Ok(V1InterestAccrualV1 {
        slot_before,
        slot_after: slot,
        elapsed_ms,
        utilization0_nad: utilization0,
        utilization1_nad: utilization1,
        rate0_nad: rate0,
        rate1_nad: rate1,
        integral0_nad: integral0,
        integral1_nad: integral1,
        lp_interest0,
        lp_interest1,
        protocol_interest0,
        protocol_interest1,
        borrower_cost0,
        borrower_cost1,
        cash_covered_protocol_interest0: cash_covered0,
        cash_covered_protocol_interest1: cash_covered1,
    })
}

fn proportional_ceil(amount: u64, bps: u16) -> Result<u64, V1EngineError> {
    u64::try_from(
        ceil_div(
            (amount as u128)
                .checked_mul(bps as u128)
                .ok_or(V1EngineError::ArithmeticOverflow("proportional ceil"))?,
            BPS_DENOMINATOR as u128,
        )
        .ok_or(V1EngineError::ArithmeticOverflow(
            "proportional ceil division",
        ))?,
    )
    .map_err(|_| V1EngineError::ArithmeticOverflow("proportional ceil conversion"))
}

fn integer_sqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let mut estimate = 1_u128 << ((128 - value.leading_zeros() as usize + 1) / 2);
    loop {
        let next = (estimate + value / estimate) / 2;
        if next >= estimate {
            return estimate;
        }
        estimate = next;
    }
}

fn proportional_floor(amount: u64, bps: u16) -> Result<u64, V1EngineError> {
    u64::try_from(
        (amount as u128)
            .checked_mul(bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(V1EngineError::ArithmeticOverflow("proportional floor"))?,
    )
    .map_err(|_| V1EngineError::ArithmeticOverflow("proportional floor conversion"))
}

fn collateral_factor_witness(position: &UserPosition) -> V1PositionCollateralFactorWitnessV1 {
    V1PositionCollateralFactorWitnessV1 {
        collateral0_max_cf_bps: max_cf_from_liquidation(position.collateral0_liquidation_cf_bps),
        collateral1_max_cf_bps: max_cf_from_liquidation(position.collateral1_liquidation_cf_bps),
        collateral0_liquidation_cf_bps: position.collateral0_liquidation_cf_bps,
        collateral1_liquidation_cf_bps: position.collateral1_liquidation_cf_bps,
    }
}

fn max_cf_from_liquidation(liquidation_cf_bps: u16) -> u16 {
    ((liquidation_cf_bps as u32).saturating_mul((BPS_DENOMINATOR - LTV_BUFFER_BPS) as u32)
        / BPS_DENOMINATOR as u32) as u16
}

fn apply_event_witnessed_collateral_factor_transition(
    pair: &Pair,
    position: &mut UserPosition,
    collateral_token: V1TokenSideV1,
    witness: V1PositionCollateralFactorWitnessV1,
    allow_decision_grade_direct_witness: bool,
) -> Result<Option<V1CollateralDepositFactorCorrectionV1>, V1EngineError> {
    let preserve_candidate = collateral_factor_witness(position);
    let collateral_amount = collateral_for_side(position, collateral_token);
    let (_, native_max_cf_bps, native_liquidation_cf_bps) =
        borrow_limit_for_collateral(pair, collateral_token, collateral_amount)?;
    let mut recompute_candidate_position = copy_position(position);
    recompute_candidate_position.set_liquidation_cf_for_debt_token(
        &mint_for_side(pair, collateral_token.opposite()),
        pair,
        native_liquidation_cf_bps,
    );
    let recompute_candidate = collateral_factor_witness(&recompute_candidate_position);
    let derived_native_max = match collateral_token {
        V1TokenSideV1::Token0 => recompute_candidate.collateral0_max_cf_bps,
        V1TokenSideV1::Token1 => recompute_candidate.collateral1_max_cf_bps,
    };
    if derived_native_max != native_max_cf_bps {
        return Err(V1EngineError::NativeMath {
            operation: "event-witnessed add-collateral CF transition",
            detail: format!(
                "native max CF {native_max_cf_bps} disagrees with position-derived max CF {derived_native_max}"
            ),
        });
    }

    let preserve_matches = witness == preserve_candidate;
    let recompute_matches = witness == recompute_candidate;
    match (preserve_matches, recompute_matches) {
        (true, false) => Ok(None),
        (false, true) => {
            *position = recompute_candidate_position;
            Ok(None)
        }
        (true, true) if allow_decision_grade_direct_witness => Ok(None),
        (false, false) if allow_decision_grade_direct_witness => {
            if witness.collateral0_max_cf_bps
                != max_cf_from_liquidation(witness.collateral0_liquidation_cf_bps)
                || witness.collateral1_max_cf_bps
                    != max_cf_from_liquidation(witness.collateral1_liquidation_cf_bps)
            {
                return Err(V1EngineError::CheckpointMismatchField(
                    "collateral_deposit.witnessed_max_cf",
                ));
            }
            match collateral_token {
                V1TokenSideV1::Token0 => {
                    if witness.collateral1_max_cf_bps != preserve_candidate.collateral1_max_cf_bps
                        || witness.collateral1_liquidation_cf_bps
                            != preserve_candidate.collateral1_liquidation_cf_bps
                    {
                        return Err(V1EngineError::CheckpointMismatchField(
                            "collateral_deposit.opposite_factor_lane",
                        ));
                    }
                    position.collateral0_liquidation_cf_bps =
                        witness.collateral0_liquidation_cf_bps;
                }
                V1TokenSideV1::Token1 => {
                    if witness.collateral0_max_cf_bps != preserve_candidate.collateral0_max_cf_bps
                        || witness.collateral0_liquidation_cf_bps
                            != preserve_candidate.collateral0_liquidation_cf_bps
                    {
                        return Err(V1EngineError::CheckpointMismatchField(
                            "collateral_deposit.opposite_factor_lane",
                        ));
                    }
                    position.collateral1_liquidation_cf_bps =
                        witness.collateral1_liquidation_cf_bps;
                }
            }
            if collateral_factor_witness(position) != witness {
                return Err(V1EngineError::CheckpointMismatchField(
                    "collateral_deposit.factor_application",
                ));
            }
            Ok(Some(V1CollateralDepositFactorCorrectionV1 {
                collateral_token,
                preserve_candidate,
                recompute_candidate,
                witnessed: witness,
            }))
        }
        _ => Err(V1EngineError::UnprovenCollateralFactorTransition {
            preserve_matches,
            recompute_matches,
        }),
    }
}

fn borrow_limit_for_collateral(
    pair: &Pair,
    collateral_token: V1TokenSideV1,
    collateral_amount: u64,
) -> Result<(u64, u16, u16), V1EngineError> {
    let (symmetric_ema, directional_ema, collateral_reserve, debt_reserve, debt_total) =
        match collateral_token {
            V1TokenSideV1::Token0 => (
                pair.last_price0_ema.symmetric,
                pair.last_price0_ema.directional,
                pair.reserve0,
                pair.reserve1,
                pair.total_debt1,
            ),
            V1TokenSideV1::Token1 => (
                pair.last_price1_ema.symmetric,
                pair.last_price1_ema.directional,
                pair.reserve1,
                pair.reserve0,
                pair.total_debt0,
            ),
        };
    pessimistic_max_debt(
        collateral_amount,
        symmetric_ema,
        directional_ema,
        collateral_reserve,
        debt_reserve,
        debt_total,
        pair.fixed_cf_bps,
    )
    .map_err(|error| native_error("pessimistic_max_debt", error))
}

fn post_withdraw_debt_coverage_exception(
    pair: &Pair,
    post_reserve0: u64,
    post_reserve1: u64,
) -> Result<Option<V1RemoveLiquidityDebtCoverageExceptionV1>, V1EngineError> {
    let required_token1_for_debt0 = required_collateral_with_impact(
        pair.total_debt0,
        post_reserve1,
        post_reserve0,
        pair.last_price1_ema.symmetric,
        pair.last_price1_ema.directional,
    )?;
    let required_token0_for_debt1 = required_collateral_with_impact(
        pair.total_debt1,
        post_reserve0,
        post_reserve1,
        pair.last_price0_ema.symmetric,
        pair.last_price0_ema.directional,
    )?;
    let buffered_token1 = ceil_div(
        (required_token1_for_debt0 as u128)
            .checked_mul(POST_WITHDRAW_DEBT_COVERAGE_BPS as u128)
            .ok_or(V1EngineError::ArithmeticOverflow(
                "token1 debt coverage buffer",
            ))?,
        BPS_DENOMINATOR as u128,
    )
    .ok_or(V1EngineError::ArithmeticOverflow(
        "token1 debt coverage division",
    ))?;
    let buffered_token0 = ceil_div(
        (required_token0_for_debt1 as u128)
            .checked_mul(POST_WITHDRAW_DEBT_COVERAGE_BPS as u128)
            .ok_or(V1EngineError::ArithmeticOverflow(
                "token0 debt coverage buffer",
            ))?,
        BPS_DENOMINATOR as u128,
    )
    .ok_or(V1EngineError::ArithmeticOverflow(
        "token0 debt coverage division",
    ))?;
    let token1_shortfall_atoms = buffered_token1.saturating_sub(post_reserve1 as u128);
    let token0_shortfall_atoms = buffered_token0.saturating_sub(post_reserve0 as u128);
    if token1_shortfall_atoms == 0 && token0_shortfall_atoms == 0 {
        return Ok(None);
    }
    Ok(Some(V1RemoveLiquidityDebtCoverageExceptionV1 {
        required_token1_for_debt0,
        buffered_token1_for_debt0: buffered_token1,
        post_reserve1,
        token1_shortfall_atoms,
        required_token0_for_debt1,
        buffered_token0_for_debt1: buffered_token0,
        post_reserve0,
        token0_shortfall_atoms,
    }))
}

fn required_collateral_with_impact(
    debt_amount: u64,
    collateral_spot_reserve: u64,
    debt_spot_reserve: u64,
    collateral_ema_price_nad: u64,
    collateral_directional_ema_price_nad: u64,
) -> Result<u64, V1EngineError> {
    if debt_amount == 0 {
        return Ok(0);
    }
    if collateral_ema_price_nad == 0 || collateral_directional_ema_price_nad == 0 {
        return Err(V1EngineError::InsufficientDebtCoverage);
    }
    let (collateral_ema_reserve, debt_ema_reserve) =
        construct_virtual_reserves_at_pessimistic_price(
            collateral_spot_reserve,
            debt_spot_reserve,
            collateral_ema_price_nad,
            collateral_directional_ema_price_nad,
        )
        .map_err(|error| native_error("construct_virtual_reserves_at_pessimistic_price", error))?;
    if collateral_ema_reserve == 0 || debt_ema_reserve <= debt_amount {
        return Err(V1EngineError::InsufficientDebtCoverage);
    }
    CPCurve::calculate_amount_in(collateral_ema_reserve, debt_ema_reserve, debt_amount)
        .map_err(|error| native_error("CPCurve::calculate_amount_in", error))
}

fn collateral_for_side(position: &UserPosition, side: V1TokenSideV1) -> u64 {
    match side {
        V1TokenSideV1::Token0 => position.collateral0,
        V1TokenSideV1::Token1 => position.collateral1,
    }
}

fn liquidation_cf_for_debt(position: &UserPosition, debt: V1TokenSideV1) -> u16 {
    match debt {
        V1TokenSideV1::Token0 => position.collateral1_liquidation_cf_bps,
        V1TokenSideV1::Token1 => position.collateral0_liquidation_cf_bps,
    }
}

fn credit_reserve_vault(
    custody: &mut V1CustodyCheckpointV1,
    side: V1TokenSideV1,
    amount: u64,
) -> Result<(), V1EngineError> {
    let balance = match side {
        V1TokenSideV1::Token0 => &mut custody.reserve_vault0,
        V1TokenSideV1::Token1 => &mut custody.reserve_vault1,
    };
    *balance = balance
        .checked_add(amount)
        .ok_or(V1EngineError::ArithmeticOverflow("reserve vault credit"))?;
    Ok(())
}

fn debit_reserve_vault(
    custody: &mut V1CustodyCheckpointV1,
    side: V1TokenSideV1,
    amount: u64,
) -> Result<(), V1EngineError> {
    let (balance, vault) = match side {
        V1TokenSideV1::Token0 => (&mut custody.reserve_vault0, "reserve_vault0"),
        V1TokenSideV1::Token1 => (&mut custody.reserve_vault1, "reserve_vault1"),
    };
    if *balance < amount {
        return Err(V1EngineError::InsufficientVaultBalance {
            vault,
            available: *balance,
            required: amount,
        });
    }
    *balance -= amount;
    Ok(())
}

fn credit_collateral_vault(
    custody: &mut V1CustodyCheckpointV1,
    side: V1TokenSideV1,
    amount: u64,
) -> Result<(), V1EngineError> {
    let balance = match side {
        V1TokenSideV1::Token0 => &mut custody.collateral_vault0,
        V1TokenSideV1::Token1 => &mut custody.collateral_vault1,
    };
    *balance = balance
        .checked_add(amount)
        .ok_or(V1EngineError::ArithmeticOverflow("collateral vault credit"))?;
    Ok(())
}

fn debit_collateral_vault(
    custody: &mut V1CustodyCheckpointV1,
    side: V1TokenSideV1,
    amount: u64,
) -> Result<(), V1EngineError> {
    let (balance, vault) = match side {
        V1TokenSideV1::Token0 => (&mut custody.collateral_vault0, "collateral_vault0"),
        V1TokenSideV1::Token1 => (&mut custody.collateral_vault1, "collateral_vault1"),
    };
    if *balance < amount {
        return Err(V1EngineError::InsufficientVaultBalance {
            vault,
            available: *balance,
            required: amount,
        });
    }
    *balance -= amount;
    Ok(())
}

fn borrow_capacity_for(
    pair: &Pair,
    position_key: Pubkey,
    position: &UserPosition,
    debt_token: V1TokenSideV1,
) -> Result<V1BorrowCapacityV1, V1EngineError> {
    let (
        collateral_amount,
        collateral_ema,
        directional_ema,
        collateral_reserve,
        debt_reserve,
        total_debt,
        cash_reserve,
    ) = match debt_token {
        V1TokenSideV1::Token0 => (
            position.collateral1,
            pair.last_price1_ema.symmetric,
            pair.last_price1_ema.directional,
            pair.reserve1,
            pair.reserve0,
            pair.total_debt0,
            pair.cash_reserve0,
        ),
        V1TokenSideV1::Token1 => (
            position.collateral0,
            pair.last_price0_ema.symmetric,
            pair.last_price0_ema.directional,
            pair.reserve0,
            pair.reserve1,
            pair.total_debt1,
            pair.cash_reserve1,
        ),
    };
    let (admissible_max_debt, max_cf_bps, liquidation_cf_bps) = pessimistic_max_debt(
        collateral_amount,
        collateral_ema,
        directional_ema,
        collateral_reserve,
        debt_reserve,
        total_debt,
        pair.fixed_cf_bps,
    )
    .map_err(|error| native_error("pessimistic_max_debt", error))?;
    let actual_debt = debt_for_side(position, pair, debt_token)?;
    let remaining_capacity = admissible_max_debt.saturating_sub(actual_debt);
    Ok(V1BorrowCapacityV1 {
        position: position_key,
        debt_token,
        collateral_amount,
        actual_debt,
        admissible_max_debt,
        remaining_capacity,
        cash_reserve,
        actionable_remaining_capacity: remaining_capacity.min(cash_reserve),
        max_cf_bps,
        liquidation_cf_bps,
    })
}

fn debt_for_side(
    position: &UserPosition,
    pair: &Pair,
    side: V1TokenSideV1,
) -> Result<u64, V1EngineError> {
    match side {
        V1TokenSideV1::Token0 => position
            .calculate_debt0(pair.total_debt0, pair.total_debt0_shares)
            .map_err(|error| native_error("UserPosition::calculate_debt0", error)),
        V1TokenSideV1::Token1 => position
            .calculate_debt1(pair.total_debt1, pair.total_debt1_shares)
            .map_err(|error| native_error("UserPosition::calculate_debt1", error)),
    }
}

fn shares_for_side(position: &UserPosition, side: V1TokenSideV1) -> u128 {
    match side {
        V1TokenSideV1::Token0 => position.debt0_shares,
        V1TokenSideV1::Token1 => position.debt1_shares,
    }
}

fn mint_for_side(pair: &Pair, side: V1TokenSideV1) -> Pubkey {
    match side {
        V1TokenSideV1::Token0 => pair.token0,
        V1TokenSideV1::Token1 => pair.token1,
    }
}

const fn side_number(side: V1TokenSideV1) -> u8 {
    match side {
        V1TokenSideV1::Token0 => 0,
        V1TokenSideV1::Token1 => 1,
    }
}

fn utilization_nad(total_debt: u64, reserve: u64) -> u64 {
    if reserve == 0 {
        0
    } else {
        u64::try_from((total_debt as u128 * NAD as u128) / reserve as u128).unwrap_or(u64::MAX)
    }
}

fn add_swap_revenue(
    revenue: &mut V1RevenueCheckpointV1,
    side: V1TokenSideV1,
    lp_fee: u64,
    protocol_fee: u64,
) -> Result<(), V1EngineError> {
    let (lp_lane, protocol_lane) = match side {
        V1TokenSideV1::Token0 => (&mut revenue.lp_swap_fee0, &mut revenue.protocol_swap_fee0),
        V1TokenSideV1::Token1 => (&mut revenue.lp_swap_fee1, &mut revenue.protocol_swap_fee1),
    };
    *lp_lane = lp_lane
        .checked_add(lp_fee as u128)
        .ok_or(V1EngineError::ArithmeticOverflow("cumulative LP swap fee"))?;
    *protocol_lane = protocol_lane.checked_add(protocol_fee as u128).ok_or(
        V1EngineError::ArithmeticOverflow("cumulative protocol swap fee"),
    )?;
    Ok(())
}

fn add_interest_revenue(
    revenue: &mut V1RevenueCheckpointV1,
    lp0: u64,
    lp1: u64,
    protocol0: u64,
    protocol1: u64,
    cash_covered_protocol0: u64,
    cash_covered_protocol1: u64,
) -> Result<(), V1EngineError> {
    let economic_lp0 = lp0
        .checked_add(protocol0.checked_sub(cash_covered_protocol0).ok_or(
            V1EngineError::ArithmeticOverflow("token0 cash-covered protocol interest"),
        )?)
        .ok_or(V1EngineError::ArithmeticOverflow(
            "economic LP token0 interest",
        ))?;
    let economic_lp1 = lp1
        .checked_add(protocol1.checked_sub(cash_covered_protocol1).ok_or(
            V1EngineError::ArithmeticOverflow("token1 cash-covered protocol interest"),
        )?)
        .ok_or(V1EngineError::ArithmeticOverflow(
            "economic LP token1 interest",
        ))?;
    revenue.lp_interest0 = revenue
        .lp_interest0
        .checked_add(economic_lp0 as u128)
        .ok_or(V1EngineError::ArithmeticOverflow(
            "cumulative LP token0 interest",
        ))?;
    revenue.lp_interest1 = revenue
        .lp_interest1
        .checked_add(economic_lp1 as u128)
        .ok_or(V1EngineError::ArithmeticOverflow(
            "cumulative LP token1 interest",
        ))?;
    revenue.protocol_interest0 = revenue
        .protocol_interest0
        .checked_add(cash_covered_protocol0 as u128)
        .ok_or(V1EngineError::ArithmeticOverflow(
            "cumulative protocol token0 interest",
        ))?;
    revenue.protocol_interest1 = revenue
        .protocol_interest1
        .checked_add(cash_covered_protocol1 as u128)
        .ok_or(V1EngineError::ArithmeticOverflow(
            "cumulative protocol token1 interest",
        ))?;
    Ok(())
}

fn native_error(operation: &'static str, error: impl std::fmt::Display) -> V1EngineError {
    V1EngineError::NativeMath {
        operation,
        detail: error.to_string(),
    }
}

fn copy_pair(pair: &Pair) -> Pair {
    Pair {
        token0: pair.token0,
        token1: pair.token1,
        lp_mint: pair.lp_mint,
        rate_model: pair.rate_model,
        swap_fee_bps: pair.swap_fee_bps,
        half_life: pair.half_life,
        fixed_cf_bps: pair.fixed_cf_bps,
        reserve0: pair.reserve0,
        reserve1: pair.reserve1,
        cash_reserve0: pair.cash_reserve0,
        cash_reserve1: pair.cash_reserve1,
        last_price0_ema: pair.last_price0_ema,
        last_price1_ema: pair.last_price1_ema,
        last_update: pair.last_update,
        last_rate0: pair.last_rate0,
        last_rate1: pair.last_rate1,
        total_debt0: pair.total_debt0,
        total_debt1: pair.total_debt1,
        total_debt0_shares: pair.total_debt0_shares,
        total_debt1_shares: pair.total_debt1_shares,
        total_supply: pair.total_supply,
        total_collateral0: pair.total_collateral0,
        total_collateral1: pair.total_collateral1,
        token0_decimals: pair.token0_decimals,
        token1_decimals: pair.token1_decimals,
        params_hash: pair.params_hash,
        version: pair.version,
        bump: pair.bump,
        vault_bumps: pair.vault_bumps,
        reduce_only: pair.reduce_only,
    }
}

fn copy_rate_model(model: &RateModel) -> RateModel {
    RateModel {
        exp_rate: model.exp_rate,
        target_util_start: model.target_util_start,
        target_util_end: model.target_util_end,
        half_life_ms: model.half_life_ms,
        min_rate: model.min_rate,
        max_rate: model.max_rate,
        initial_rate: model.initial_rate,
    }
}

fn copy_position(position: &UserPosition) -> UserPosition {
    UserPosition {
        owner: position.owner,
        pair: position.pair,
        collateral0_liquidation_cf_bps: position.collateral0_liquidation_cf_bps,
        collateral1_liquidation_cf_bps: position.collateral1_liquidation_cf_bps,
        collateral0: position.collateral0,
        collateral1: position.collateral1,
        debt0_shares: position.debt0_shares,
        debt1_shares: position.debt1_shares,
        bump: position.bump,
    }
}

fn pair_checkpoint(pair: &Pair) -> V1PairCheckpointV1 {
    V1PairCheckpointV1 {
        token0: pair.token0,
        token1: pair.token1,
        lp_mint: pair.lp_mint,
        rate_model: pair.rate_model,
        swap_fee_bps: pair.swap_fee_bps,
        half_life: pair.half_life,
        fixed_cf_bps: pair.fixed_cf_bps,
        reserve0: pair.reserve0,
        reserve1: pair.reserve1,
        cash_reserve0: pair.cash_reserve0,
        cash_reserve1: pair.cash_reserve1,
        symmetric_price0_ema_nad: pair.last_price0_ema.symmetric,
        directional_price0_ema_nad: pair.last_price0_ema.directional,
        symmetric_price1_ema_nad: pair.last_price1_ema.symmetric,
        directional_price1_ema_nad: pair.last_price1_ema.directional,
        last_update_slot: pair.last_update,
        last_rate0_nad: pair.last_rate0,
        last_rate1_nad: pair.last_rate1,
        total_debt0: pair.total_debt0,
        total_debt1: pair.total_debt1,
        total_debt0_shares: pair.total_debt0_shares,
        total_debt1_shares: pair.total_debt1_shares,
        total_supply: pair.total_supply,
        total_collateral0: pair.total_collateral0,
        total_collateral1: pair.total_collateral1,
        token0_decimals: pair.token0_decimals,
        token1_decimals: pair.token1_decimals,
        params_hash: pair.params_hash,
        version: pair.version,
        bump: pair.bump,
        reserve0_vault_bump: pair.vault_bumps.reserve0,
        reserve1_vault_bump: pair.vault_bumps.reserve1,
        collateral0_vault_bump: pair.vault_bumps.collateral0,
        collateral1_vault_bump: pair.vault_bumps.collateral1,
        reduce_only: pair.reduce_only,
    }
}

fn rate_model_checkpoint(key: Pubkey, model: &RateModel) -> V1RateModelCheckpointV1 {
    V1RateModelCheckpointV1 {
        key,
        exp_rate: model.exp_rate,
        target_util_start_nad: model.target_util_start,
        target_util_end_nad: model.target_util_end,
        half_life_ms: model.half_life_ms,
        min_rate_nad: model.min_rate,
        max_rate_nad: model.max_rate,
        initial_rate_nad: model.initial_rate,
    }
}

fn position_checkpoint(position: &UserPosition) -> V1PositionCheckpointV1 {
    V1PositionCheckpointV1 {
        owner: position.owner,
        pair: position.pair,
        collateral0_liquidation_cf_bps: position.collateral0_liquidation_cf_bps,
        collateral1_liquidation_cf_bps: position.collateral1_liquidation_cf_bps,
        collateral0: position.collateral0,
        collateral1: position.collateral1,
        debt0_shares: position.debt0_shares,
        debt1_shares: position.debt1_shares,
        bump: position.bump,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnipair::constants::DEBT_SHARE_SCALE;
    use omnipair::state::VaultBumps;

    const PAIR_KEY: Pubkey = Pubkey::new_from_array([8; 32]);
    const POSITION_KEY: Pubkey = Pubkey::new_from_array([9; 32]);
    const TOKEN0: Pubkey = Pubkey::new_from_array([1; 32]);
    const TOKEN1: Pubkey = Pubkey::new_from_array([2; 32]);
    const RATE_MODEL_KEY: Pubkey = Pubkey::new_from_array([4; 32]);

    fn pair() -> Pair {
        let rate_model = rate_model();
        let mut pair = Pair::initialize(
            TOKEN0,
            TOKEN1,
            Pubkey::new_from_array([3; 32]),
            6,
            6,
            RATE_MODEL_KEY,
            30,
            60_000,
            Some(8_000),
            100,
            [5; 32],
            VERSION,
            6,
            VaultBumps {
                reserve0: 7,
                reserve1: 8,
                collateral0: 9,
                collateral1: 10,
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
        pair.total_collateral1 = 200_000_000;
        pair
    }

    fn rate_model() -> RateModel {
        RateModel::new(3_000, 5_000, 3_600_000, 100, 1_000, 200)
    }

    fn authority() -> FutarchyAuthority {
        FutarchyAuthority::initialize(
            Pubkey::new_from_array([10; 32]),
            2_000,
            1_000,
            Pubkey::new_from_array([11; 32]),
            Pubkey::new_from_array([12; 32]),
            Pubkey::new_from_array([13; 32]),
            4_000,
            3_000,
            3_000,
            14,
        )
        .unwrap()
    }

    fn position() -> UserPosition {
        UserPosition {
            owner: Pubkey::new_from_array([15; 32]),
            pair: PAIR_KEY,
            collateral0_liquidation_cf_bps: 0,
            collateral1_liquidation_cf_bps: 0,
            collateral0: 0,
            collateral1: 200_000_000,
            debt0_shares: 0,
            debt1_shares: 0,
            bump: 16,
        }
    }

    fn engine_with_version(program_version: u8) -> Result<V1NativeEngine, V1EngineError> {
        V1NativeEngine::from_decoded_accounts(V1DecodedAccountsV1 {
            program_version,
            pair_key: PAIR_KEY,
            pair: pair(),
            rate_model_key: RATE_MODEL_KEY,
            rate_model: rate_model(),
            futarchy_authority: authority(),
            positions: vec![(POSITION_KEY, position())],
            custody: V1CustodyCheckpointV1 {
                reserve_vault0: 1_000_000_000,
                reserve_vault1: 1_000_000_000,
                collateral_vault0: 0,
                collateral_vault1: 200_000_000,
            },
            revenue: V1RevenueCheckpointV1::ZERO,
        })
    }

    fn canonical_initialization() -> V1CanonicalInitializationInputV1 {
        let authority_bump =
            Pubkey::find_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX], &omnipair::ID).1;
        let mut input = V1CanonicalInitializationInputV1 {
            program_version: VERSION,
            pair_account_version: VERSION,
            slot: 100,
            pair_key: Pubkey::default(),
            token0: TOKEN0,
            token1: TOKEN1,
            lp_mint: Pubkey::new_from_array([3; 32]),
            rate_model_key: Pubkey::default(),
            rate_model_address_policy: V1RateModelAddressPolicyV1::PairDerivedPda,
            collateral_factor_transition_policy: V1CollateralFactorTransitionPolicyV1::SourceNative,
            post_state_correction_policy: V1PostStateCorrectionPolicyV1::SourceNative,
            token0_decimals: 6,
            token1_decimals: 6,
            swap_fee_bps: 30,
            half_life_ms: 60_000,
            fixed_cf_bps: Some(8_000),
            target_util_start_bps_arg: Some(3_000),
            target_util_end_bps_arg: Some(5_000),
            rate_half_life_ms_arg: Some(3_600_000),
            min_rate_bps_arg: Some(100),
            max_rate_bps_arg: Some(1_000),
            initial_rate_bps: 200,
            params_hash: [0; 32],
            gross_amount0: 1_000_000,
            gross_amount1: 1_000_000,
            vault_credit0: 999_900,
            vault_credit1: 1_000_000,
            minted_lp: 999_000,
            min_liquidity_out: 900_000,
            authority: V1CanonicalAuthorityInitializationV1 {
                version: FutarchyAuthority::CURRENT_VERSION,
                authority: Pubkey::new_from_array([10; 32]),
                swap_bps: 2_000,
                interest_bps: 1_000,
                futarchy_treasury: Pubkey::new_from_array([11; 32]),
                buybacks_vault: Pubkey::new_from_array([12; 32]),
                team_treasury: Pubkey::new_from_array([13; 32]),
                futarchy_treasury_bps: 4_000,
                buybacks_vault_bps: 3_000,
                team_treasury_bps: 3_000,
                global_reduce_only: false,
                bump: authority_bump,
            },
        };
        input.params_hash = canonical_initialize_params_hash(&input);
        input.pair_key = Pubkey::find_program_address(
            &[
                PAIR_SEED_PREFIX,
                input.token0.as_ref(),
                input.token1.as_ref(),
                input.params_hash.as_ref(),
            ],
            &omnipair::ID,
        )
        .0;
        input.rate_model_key = Pubkey::find_program_address(
            &[RATE_MODEL_SEED_PREFIX, input.pair_key.as_ref()],
            &omnipair::ID,
        )
        .0;
        input
    }

    #[test]
    fn canonical_initialization_reconstructs_all_native_genesis_accounts() {
        let input = canonical_initialization();
        let engine = V1NativeEngine::from_canonical_initialization(input).unwrap();
        let checkpoint = engine.checkpoint();

        assert_eq!(engine.pair_key(), input.pair_key);
        assert_eq!(checkpoint.pair.reserve0, input.gross_amount0);
        assert_eq!(checkpoint.pair.reserve1, input.gross_amount1);
        assert_eq!(
            checkpoint.pair.total_supply,
            MIN_LIQUIDITY + input.minted_lp
        );
        assert_eq!(checkpoint.rate_model.key, input.rate_model_key);
        assert_eq!(checkpoint.rate_model.initial_rate_nad, 20_000_000);
        assert_eq!(checkpoint.authority.swap_bps, input.authority.swap_bps);
        assert_eq!(checkpoint.custody.reserve_vault0, input.vault_credit0);
        assert_eq!(checkpoint.custody.reserve_vault1, input.vault_credit1);
        assert!(checkpoint.positions.is_empty());
        assert_eq!(checkpoint.revenue, V1RevenueCheckpointV1::ZERO);
        assert_eq!(V1_NATIVE_SUPPORTED_SOURCE_RELEASE, "omnipair-v0.10.5");
    }

    #[test]
    fn historical_explicit_rate_model_key_is_preserved_without_pda_inference() {
        let mut input = canonical_initialization();
        let explicit_historical_key = Pubkey::new_from_array([91; 32]);
        input.rate_model_key = explicit_historical_key;
        input.rate_model_address_policy = V1RateModelAddressPolicyV1::HistoricalExplicitInitAccount;

        let engine = V1NativeEngine::from_canonical_initialization(input).unwrap();
        assert_eq!(engine.checkpoint().rate_model.key, explicit_historical_key);

        input.rate_model_address_policy = V1RateModelAddressPolicyV1::PairDerivedPda;
        assert!(matches!(
            V1NativeEngine::from_canonical_initialization(input),
            Err(V1EngineError::CanonicalPdaMismatch {
                account: "rate_model",
                ..
            })
        ));
    }

    #[test]
    fn canonical_initialization_fails_closed_on_revision_or_hash_drift() {
        let mut wrong_version = canonical_initialization();
        wrong_version.pair_account_version = VERSION + 1;
        assert_eq!(
            V1NativeEngine::from_canonical_initialization(wrong_version).err(),
            Some(V1EngineError::UnsupportedPairVersion(VERSION + 1))
        );

        let mut wrong_hash = canonical_initialization();
        wrong_hash.params_hash[0] ^= 1;
        assert_eq!(
            V1NativeEngine::from_canonical_initialization(wrong_hash).err(),
            Some(V1EngineError::CanonicalParamsHashMismatch)
        );
    }

    #[test]
    fn unsupported_program_version_fails_closed() {
        assert_eq!(
            engine_with_version(VERSION + 1).err(),
            Some(V1EngineError::UnsupportedProgramVersion(VERSION + 1))
        );
    }

    #[test]
    fn event_witnessed_collateral_factor_transition_is_exact_and_fail_closed() {
        let amount = 100_000_000;
        let input = V1CollateralInputV1 {
            slot: 100,
            position: POSITION_KEY,
            collateral_token: V1TokenSideV1::Token0,
            amount,
            requested_amount: amount,
            deposit: true,
            collateral_vault_amount: amount,
            counterparty_amount: amount,
        };

        let mut strict = engine_with_version(VERSION).unwrap();
        assert_eq!(
            strict
                .apply_event_witnessed_collateral(
                    input,
                    V1PositionCollateralFactorWitnessV1 {
                        collateral0_max_cf_bps: 0,
                        collateral1_max_cf_bps: 0,
                        collateral0_liquidation_cf_bps: 0,
                        collateral1_liquidation_cf_bps: 0,
                    },
                )
                .unwrap_err(),
            V1EngineError::InvalidConfiguration(
                "event_witnessed_collateral_factor_transition_policy"
            )
        );

        let mut preserve = engine_with_version(VERSION).unwrap();
        preserve.collateral_factor_transition_policy =
            V1CollateralFactorTransitionPolicyV1::EventWitnessedCollateralFactorTransition;
        preserve
            .apply_event_witnessed_collateral(
                input,
                V1PositionCollateralFactorWitnessV1 {
                    collateral0_max_cf_bps: 0,
                    collateral1_max_cf_bps: 0,
                    collateral0_liquidation_cf_bps: 0,
                    collateral1_liquidation_cf_bps: 0,
                },
            )
            .unwrap();
        assert_eq!(
            preserve
                .position(&POSITION_KEY)
                .unwrap()
                .collateral0_liquidation_cf_bps,
            0
        );

        let mut recompute = engine_with_version(VERSION).unwrap();
        recompute.collateral_factor_transition_policy =
            V1CollateralFactorTransitionPolicyV1::EventWitnessedCollateralFactorTransition;
        let (_, expected_max, expected_liquidation) =
            borrow_limit_for_collateral(recompute.pair(), V1TokenSideV1::Token0, amount).unwrap();
        recompute
            .apply_event_witnessed_collateral(
                input,
                V1PositionCollateralFactorWitnessV1 {
                    collateral0_max_cf_bps: expected_max,
                    collateral1_max_cf_bps: 0,
                    collateral0_liquidation_cf_bps: expected_liquidation,
                    collateral1_liquidation_cf_bps: 0,
                },
            )
            .unwrap();
        assert_eq!(
            recompute
                .position(&POSITION_KEY)
                .unwrap()
                .collateral0_liquidation_cf_bps,
            expected_liquidation
        );

        // A later deposit whose exact recomputation equals the preserved state
        // provides no evidence for which source transition ran. Reject it and
        // retain the complete pre-operation checkpoint.
        let before_ambiguous = recompute.checkpoint();
        let ambiguous = recompute
            .apply_event_witnessed_collateral(
                input,
                V1PositionCollateralFactorWitnessV1 {
                    collateral0_max_cf_bps: expected_max,
                    collateral1_max_cf_bps: 0,
                    collateral0_liquidation_cf_bps: expected_liquidation,
                    collateral1_liquidation_cf_bps: 0,
                },
            )
            .unwrap_err();
        assert_eq!(
            ambiguous,
            V1EngineError::UnprovenCollateralFactorTransition {
                preserve_matches: true,
                recompute_matches: true,
            }
        );
        assert_eq!(recompute.checkpoint(), before_ambiguous);
    }

    #[test]
    fn decision_grade_collateral_withdrawal_binds_full_position_and_cpi() {
        fn withdrawal_engine() -> V1NativeEngine {
            let mut engine = engine_with_version(VERSION).unwrap();
            engine.pair.total_debt1 = 1;
            engine.pair.total_debt1_shares = DEBT_SHARE_SCALE as u128;
            engine.pair.cash_reserve1 -= 1;
            engine.custody.reserve_vault1 -= 1;
            engine.pair.total_collateral0 = 127;
            engine.custody.collateral_vault0 = 127;
            let position = engine.positions.get_mut(&POSITION_KEY).unwrap();
            position.collateral0 = 127;
            position.debt1_shares = DEBT_SHARE_SCALE as u128;
            position.collateral0_liquidation_cf_bps = 5_937;
            engine
        }

        let input = V1CollateralInputV1 {
            slot: withdrawal_engine().pair.last_update,
            position: POSITION_KEY,
            collateral_token: V1TokenSideV1::Token0,
            amount: 110,
            requested_amount: 110,
            deposit: false,
            collateral_vault_amount: 110,
            counterparty_amount: 110,
        };
        let witness = V1CollateralWithdrawalWitnessV1 {
            event_position: POSITION_KEY,
            event_amount: 110,
            token_cpi_collateral_vault_debit: 110,
            token_cpi_recipient_credit: 110,
            event_collateral0: 17,
            event_collateral1: 200_000_000,
            event_debt0_shares: 0,
            event_debt1_shares: DEBT_SHARE_SCALE as u128,
            event_collateral0_max_cf_bps: 4_750,
            event_collateral1_max_cf_bps: 0,
            event_collateral0_liquidation_cf_bps: 5_000,
            event_collateral1_liquidation_cf_bps: 0,
        };

        let mut corrected = withdrawal_engine();
        corrected.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let receipt = corrected
            .apply_event_and_cpi_witnessed_collateral_withdrawal(input, witness)
            .unwrap();
        let correction = receipt.withdrawal_factor_correction.unwrap();
        assert_eq!(corrected.pair.total_collateral0, 17);
        assert_eq!(corrected.custody.collateral_vault0, 17);
        let position = corrected.position(&POSITION_KEY).unwrap();
        assert_eq!(position.collateral0, 17);
        assert_eq!(position.debt1_shares, DEBT_SHARE_SCALE as u128);
        assert_eq!(position.collateral0_liquidation_cf_bps, 5_000);
        assert_eq!(correction.witnessed_collateral0_max_cf_bps, 4_750);
        assert_ne!(
            correction.source_native_collateral0_liquidation_cf_bps,
            correction.witnessed_collateral0_liquidation_cf_bps
        );

        let mut malformed = withdrawal_engine();
        malformed.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let malformed_before = malformed.checkpoint();
        let mut bad_witness = witness;
        bad_witness.event_collateral0_max_cf_bps -= 1;
        assert_eq!(
            malformed
                .apply_event_and_cpi_witnessed_collateral_withdrawal(input, bad_witness)
                .unwrap_err(),
            V1EngineError::CheckpointMismatchField("collateral_withdrawal.position_post_state")
        );
        assert_eq!(malformed.checkpoint(), malformed_before);
    }

    #[test]
    fn decision_grade_lp_mint_correction_requires_equal_event_and_cpi_witnesses() {
        let input = V1AddLiquidityInputV1 {
            slot: 100,
            amount0_in: 100_000_001,
            amount1_in: 100_000_001,
            vault_credit0: 100_000_000,
            vault_credit1: 100_000_000,
            min_liquidity_out: 0,
        };
        let witness = V1LpMintSupplyWitnessV1 {
            event_amount0_used: 100_000_000,
            event_amount1_used: 100_000_000,
            event_liquidity_minted: 100_000_000,
            token_cpi_supply_delta: 100_000_000,
            token_cpi_vault_credit0: 100_000_000,
            token_cpi_vault_credit1: 100_000_000,
            event_cash_reserve0_after: None,
            event_cash_reserve1_after: None,
        };

        let mut strict = engine_with_version(VERSION).unwrap();
        let strict_before = strict.checkpoint();
        assert_eq!(
            strict
                .apply_event_and_cpi_witnessed_add_liquidity(input, witness)
                .unwrap_err(),
            V1EngineError::InvalidConfiguration(
                "decision_grade_event_and_token_cpi_correction_policy"
            )
        );
        assert_eq!(strict.checkpoint(), strict_before);

        let mut corrected = engine_with_version(VERSION).unwrap();
        corrected.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let receipt = corrected
            .apply_event_and_cpi_witnessed_add_liquidity(input, witness)
            .unwrap();
        assert_eq!(receipt.liquidity_minted, 100_000_000);
        assert_eq!(receipt.amount0_used, 100_000_000);
        assert_eq!(receipt.amount1_used, 100_000_000);
        assert_eq!(receipt.total_supply_after, 1_100_000_000);
        assert_eq!(
            receipt.supply_correction,
            Some(V1LpMintSupplyCorrectionV1 {
                source_native_amount0_used: 100_000_001,
                source_native_amount1_used: 100_000_001,
                source_native_liquidity_minted: 100_000_001,
                witnessed_amount0_used: 100_000_000,
                witnessed_amount1_used: 100_000_000,
                witnessed_liquidity_minted: 100_000_000,
                total_supply_before: 1_000_000_000,
                total_supply_after: 1_100_000_000,
            })
        );

        let mut mismatched = engine_with_version(VERSION).unwrap();
        mismatched.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let mismatched_before = mismatched.checkpoint();
        assert_eq!(
            mismatched
                .apply_event_and_cpi_witnessed_add_liquidity(
                    input,
                    V1LpMintSupplyWitnessV1 {
                        event_amount0_used: 100_000_000,
                        event_amount1_used: 100_000_000,
                        event_liquidity_minted: 100_000_000,
                        token_cpi_supply_delta: 99_999_999,
                        token_cpi_vault_credit0: 100_000_000,
                        token_cpi_vault_credit1: 100_000_000,
                        event_cash_reserve0_after: None,
                        event_cash_reserve1_after: None,
                    },
                )
                .unwrap_err(),
            V1EngineError::InvalidConfiguration("lp_mint_event_and_token_cpi_witness")
        );
        assert_eq!(mismatched.checkpoint(), mismatched_before);
    }

    #[test]
    fn decision_grade_lp_cash_checkpoint_catches_up_protocol_inventory_exactly() {
        fn indebted_engine() -> V1NativeEngine {
            let mut engine = engine_with_version(VERSION).unwrap();
            engine.pair.total_debt1 = 100_000_000;
            engine.pair.total_debt1_shares = 100_000_000_u128 * DEBT_SHARE_SCALE as u128;
            engine.pair.cash_reserve1 = 900_000_000;
            engine.custody.reserve_vault1 = 900_000_000;
            engine
                .positions
                .get_mut(&POSITION_KEY)
                .unwrap()
                .debt1_shares = engine.pair.total_debt1_shares;
            engine
        }

        let input = V1AddLiquidityInputV1 {
            slot: 10_100,
            amount0_in: 100_000_000,
            amount1_in: 100_000_000,
            vault_credit0: 100_000_000,
            vault_credit1: 100_000_000,
            min_liquidity_out: 0,
        };
        let mut native = indebted_engine();
        let native_receipt = native.apply_add_liquidity(input).unwrap();
        let corrected_input = V1AddLiquidityInputV1 {
            vault_credit0: native_receipt.amount0_used,
            vault_credit1: native_receipt.amount1_used,
            ..input
        };
        let native_after = native.checkpoint();
        let witness = V1LpMintSupplyWitnessV1 {
            event_amount0_used: native_receipt.amount0_used,
            event_amount1_used: native_receipt.amount1_used,
            event_liquidity_minted: native_receipt.liquidity_minted,
            token_cpi_supply_delta: native_receipt.liquidity_minted,
            token_cpi_vault_credit0: corrected_input.vault_credit0,
            token_cpi_vault_credit1: corrected_input.vault_credit1,
            event_cash_reserve0_after: Some(native_after.pair.cash_reserve0),
            event_cash_reserve1_after: Some(native_after.pair.cash_reserve1 - 248),
        };

        let mut corrected = indebted_engine();
        corrected.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let receipt = corrected
            .apply_event_and_cpi_witnessed_add_liquidity(corrected_input, witness)
            .unwrap();
        let correction = receipt.cash_inventory_correction.unwrap();
        assert_eq!(
            corrected.pair.cash_reserve1,
            native_after.pair.cash_reserve1 - 248
        );
        assert_eq!(
            corrected.pair.total_debt1,
            native_after.pair.total_debt1 + 248
        );
        assert_eq!(corrected.pair.reserve1, native_after.pair.reserve1);
        assert_eq!(
            corrected.revenue.protocol_interest1,
            native_after.revenue.protocol_interest1 + 248
        );
        assert_eq!(
            corrected.revenue.lp_interest1,
            native_after.revenue.lp_interest1
        );
        assert_eq!(
            correction.witnessed_cash_covered_protocol_interest1,
            correction.source_native_cash_covered_protocol_interest1 + 248
        );

        let mut malformed = indebted_engine();
        malformed.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let malformed_before = malformed.checkpoint();
        let mut bad_witness = witness;
        bad_witness.event_cash_reserve1_after = Some(u64::MAX);
        assert_eq!(
            malformed
                .apply_event_and_cpi_witnessed_add_liquidity(corrected_input, bad_witness)
                .unwrap_err(),
            V1EngineError::CheckpointMismatch
        );
        assert_eq!(malformed.checkpoint(), malformed_before);
    }

    #[test]
    fn decision_grade_lp_cash_checkpoint_retains_signed_inventory_residual() {
        fn rounding_engine() -> V1NativeEngine {
            let mut engine = engine_with_version(VERSION).unwrap();
            engine.pair.total_debt1 = 100_000_000;
            engine.pair.total_debt1_shares = 100_000_000_u128 * DEBT_SHARE_SCALE as u128;
            engine.pair.cash_reserve1 = 900_000_000;
            engine.custody.reserve_vault1 = 900_000_010;
            engine.revenue.protocol_interest1 = 10;
            engine
                .positions
                .get_mut(&POSITION_KEY)
                .unwrap()
                .debt1_shares = engine.pair.total_debt1_shares;
            engine
        }

        let quoted_input = V1AddLiquidityInputV1 {
            slot: 10_100,
            amount0_in: 100_000_000,
            amount1_in: 100_000_000,
            vault_credit0: 100_000_000,
            vault_credit1: 100_000_000,
            min_liquidity_out: 0,
        };
        let mut quote = rounding_engine();
        let quote_receipt = quote.apply_add_liquidity(quoted_input).unwrap();
        let input = V1AddLiquidityInputV1 {
            vault_credit0: quote_receipt.amount0_used,
            vault_credit1: quote_receipt.amount1_used,
            ..quoted_input
        };
        let mut source_native = rounding_engine();
        let source_native_receipt = source_native.apply_add_liquidity(input).unwrap();
        let source_native_after = source_native.checkpoint();
        let witness = V1LpMintSupplyWitnessV1 {
            event_amount0_used: source_native_receipt.amount0_used,
            event_amount1_used: source_native_receipt.amount1_used,
            event_liquidity_minted: source_native_receipt.liquidity_minted,
            token_cpi_supply_delta: source_native_receipt.liquidity_minted,
            token_cpi_vault_credit0: input.vault_credit0,
            token_cpi_vault_credit1: input.vault_credit1,
            event_cash_reserve0_after: Some(source_native_after.pair.cash_reserve0),
            // Inventory implies cumulative protocol interest 9 while the
            // source-native checkpoint already carries 10: a one-atom
            // retained rounding residual, not negative economic revenue.
            event_cash_reserve1_after: Some(source_native_after.custody.reserve_vault1 - 9),
        };

        let mut corrected = rounding_engine();
        corrected.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let receipt = corrected
            .apply_event_and_cpi_witnessed_add_liquidity(input, witness)
            .unwrap();
        let correction = receipt.cash_inventory_correction.unwrap();
        assert_eq!(
            correction.protocol_interest_inventory_rounding_atoms1,
            i128::try_from(correction.witnessed_protocol_interest1_after).unwrap()
                - i128::try_from(correction.source_native_protocol_interest1_after).unwrap()
        );
        assert!(correction.protocol_interest_inventory_rounding_atoms1 < 0);
        assert_eq!(correction.witnessed_cash_covered_protocol_interest1, 0);
        assert_eq!(
            corrected.revenue.protocol_interest1,
            source_native_after.revenue.protocol_interest1
        );
        assert_eq!(
            corrected.pair.cash_reserve1,
            witness.event_cash_reserve1_after.unwrap()
        );
        assert_eq!(
            corrected.pair.total_debt1,
            corrected.pair.reserve1 - corrected.pair.cash_reserve1
        );

        let mut wider_residual = rounding_engine();
        wider_residual.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let mut wider_witness = witness;
        wider_witness.event_cash_reserve1_after =
            Some(source_native_after.custody.reserve_vault1 - 8);
        let wider_receipt = wider_residual
            .apply_event_and_cpi_witnessed_add_liquidity(input, wider_witness)
            .unwrap();
        let wider_correction = wider_receipt.cash_inventory_correction.unwrap();
        assert_eq!(
            wider_correction.protocol_interest_inventory_rounding_atoms1,
            correction.protocol_interest_inventory_rounding_atoms1 - 1
        );

        let mut malformed = rounding_engine();
        malformed.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let malformed_before = malformed.checkpoint();
        let mut bad_witness = witness;
        bad_witness.event_cash_reserve1_after =
            Some(source_native_after.custody.reserve_vault1 + 1);
        assert_eq!(
            malformed
                .apply_event_and_cpi_witnessed_add_liquidity(input, bad_witness)
                .unwrap_err(),
            V1EngineError::CheckpointMismatch
        );
        assert_eq!(malformed.checkpoint(), malformed_before);
    }

    #[test]
    fn decision_grade_lp_burn_cash_checkpoint_is_event_and_cpi_bound() {
        fn indebted_engine() -> V1NativeEngine {
            let mut engine = engine_with_version(VERSION).unwrap();
            engine.pair.total_debt0 = 100_000_000;
            engine.pair.total_debt1 = 100_000_000;
            engine.pair.total_debt0_shares = 100_000_000_u128 * DEBT_SHARE_SCALE as u128;
            engine.pair.total_debt1_shares = 100_000_000_u128 * DEBT_SHARE_SCALE as u128;
            engine.pair.cash_reserve0 = 900_000_000;
            engine.pair.cash_reserve1 = 900_000_000;
            engine.custody.reserve_vault0 = 900_001_000;
            engine.custody.reserve_vault1 = 900_001_000;
            let position = engine.positions.get_mut(&POSITION_KEY).unwrap();
            position.debt0_shares = engine.pair.total_debt0_shares;
            position.debt1_shares = engine.pair.total_debt1_shares;
            engine
        }

        let engine = indebted_engine();
        let liquidity_burned = 10_000_000;
        let amount0_gross = u64::try_from(
            (liquidity_burned as u128) * engine.pair.reserve0 as u128
                / engine.pair.total_supply as u128,
        )
        .unwrap();
        let amount1_gross = u64::try_from(
            (liquidity_burned as u128) * engine.pair.reserve1 as u128
                / engine.pair.total_supply as u128,
        )
        .unwrap();
        let amount0_out =
            amount0_gross - proportional_ceil(amount0_gross, LIQUIDITY_WITHDRAWAL_FEE_BPS).unwrap();
        let amount1_out =
            amount1_gross - proportional_ceil(amount1_gross, LIQUIDITY_WITHDRAWAL_FEE_BPS).unwrap();
        let input = V1RemoveLiquidityInputV1 {
            slot: engine.pair.last_update,
            liquidity_burned,
            vault_debit0: amount0_out,
            vault_debit1: amount1_out,
            recipient_credit0: amount0_out,
            recipient_credit1: amount1_out,
            min_amount0_out: 0,
            min_amount1_out: 0,
        };
        let mut source_native = indebted_engine();
        let source_receipt = source_native.apply_remove_liquidity(input).unwrap();
        let source_after = source_native.checkpoint();
        let witnessed_amount0_out = source_receipt.amount0_out - 1;
        let witnessed_amount1_out = source_receipt.amount1_out - 2;
        let witnessed_input = V1RemoveLiquidityInputV1 {
            vault_debit0: witnessed_amount0_out,
            vault_debit1: witnessed_amount1_out,
            recipient_credit0: witnessed_amount0_out,
            recipient_credit1: witnessed_amount1_out,
            ..input
        };
        let witness = V1RemoveLiquidityCashWitnessV1 {
            event_liquidity_burned: liquidity_burned,
            event_amount0_out: witnessed_amount0_out,
            event_amount1_out: witnessed_amount1_out,
            event_cash_reserve0_after: source_after.pair.cash_reserve0 + 2,
            event_cash_reserve1_after: source_after.pair.cash_reserve1 + 79,
            token_cpi_vault_debit0: witnessed_amount0_out,
            token_cpi_vault_debit1: witnessed_amount1_out,
            token_cpi_recipient_credit0: witnessed_amount0_out,
            token_cpi_recipient_credit1: witnessed_amount1_out,
        };

        let mut corrected = indebted_engine();
        corrected.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let receipt = corrected
            .apply_event_and_cpi_witnessed_remove_liquidity(witnessed_input, witness)
            .unwrap();
        let output_correction = receipt.output_correction.unwrap();
        assert_eq!(output_correction.source_native_amount0_out, amount0_out);
        assert_eq!(output_correction.source_native_amount1_out, amount1_out);
        assert_eq!(output_correction.witnessed_amount0_out, amount0_out - 1);
        assert_eq!(output_correction.witnessed_amount1_out, amount1_out - 2);
        let correction = receipt.cash_inventory_correction.unwrap();
        assert_eq!(correction.cash_checkpoint_rounding_atoms0, 1);
        assert_eq!(correction.cash_checkpoint_rounding_atoms1, 77);
        assert_eq!(corrected.pair.reserve0, source_after.pair.reserve0 + 1);
        assert_eq!(corrected.pair.reserve1, source_after.pair.reserve1 + 2);
        assert_eq!(corrected.pair.total_supply, source_after.pair.total_supply);
        assert_eq!(
            corrected.pair.total_debt0,
            source_after.pair.total_debt0 - 1
        );
        assert_eq!(
            corrected.pair.total_debt1,
            source_after.pair.total_debt1 - 77
        );
        assert_eq!(
            corrected.pair.total_debt0_shares,
            source_after.pair.total_debt0_shares
        );
        assert_eq!(
            corrected.pair.total_debt1_shares,
            source_after.pair.total_debt1_shares
        );
        assert_eq!(corrected.revenue, source_after.revenue);

        let mut malformed = indebted_engine();
        malformed.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let malformed_before = malformed.checkpoint();
        let mut bad_witness = witness;
        bad_witness.token_cpi_recipient_credit1 -= 1;
        assert_eq!(
            malformed
                .apply_event_and_cpi_witnessed_remove_liquidity(witnessed_input, bad_witness)
                .unwrap_err(),
            V1EngineError::InvalidConfiguration("lp_burn_event_and_token_cpi_witness")
        );
        assert_eq!(malformed.checkpoint(), malformed_before);
    }

    #[test]
    fn swap_matches_native_cp_curve_fee_and_reserve_math_exactly() {
        let mut engine = engine_with_version(VERSION).unwrap();
        let gross = 25_000_000u64;
        let swap_fee = ceil_div(gross as u128 * 30, BPS_DENOMINATOR as u128).unwrap() as u64;
        let protocol_fee =
            ceil_div(swap_fee as u128 * 2_000, BPS_DENOMINATOR as u128).unwrap() as u64;
        let net = gross - swap_fee;
        let expected_out =
            CPCurve::calculate_amount_out(1_000_000_000, 1_000_000_000, net).unwrap();
        let receipt = engine
            .apply_swap(V1SwapInputV1 {
                slot: 100,
                token_in: V1TokenSideV1::Token0,
                gross_amount_in: gross,
                min_amount_out: expected_out,
                vault_credit_in: gross,
                vault_debit_out: expected_out,
                recipient_credit_out: expected_out,
            })
            .unwrap();

        assert_eq!(receipt.amount_out, expected_out);
        assert_eq!(receipt.amount_in_after_fee, net);
        assert_eq!(receipt.protocol_fee, protocol_fee);
        assert_eq!(receipt.lp_fee, swap_fee - protocol_fee);
        assert_eq!(receipt.reserve0_after, 1_000_000_000 + gross - protocol_fee);
        assert_eq!(receipt.reserve1_after, 1_000_000_000 - expected_out);
        assert_eq!(receipt.cash_reserve0_after, receipt.reserve0_after);
        assert_eq!(receipt.cash_reserve1_after, receipt.reserve1_after);
        assert!(engine.pair().k() >= 1_000_000_000u128.pow(2));
    }

    #[test]
    fn decision_grade_swap_correction_is_event_cpi_bound_and_aligns_debt_explicitly() {
        let gross = 1_000_000_u64;
        let total_fee = 3_000_u64;
        let net = gross - total_fee;
        let output = CPCurve::calculate_amount_out(1_000_000_000, 1_000_000_000, net).unwrap();
        let input = V1SwapInputV1 {
            slot: 100,
            token_in: V1TokenSideV1::Token1,
            gross_amount_in: gross,
            min_amount_out: output,
            vault_credit_in: gross,
            vault_debit_out: output,
            recipient_credit_out: output,
        };
        let witness = V1SwapTransitionWitnessV1 {
            event_token_in: V1TokenSideV1::Token1,
            event_gross_amount_in: gross,
            event_amount_in_after_fee: net,
            event_amount_out: output,
            event_lp_fee: total_fee,
            event_protocol_fee: 0,
            // The exact output reserve is 16 atoms below linked native math;
            // debt stays unchanged and the delta remains physical inventory.
            event_reserve0_after: 1_000_000_000 - output - 16,
            // The exact event carries the configured 600-unit protocol lane
            // plus a separately retained 205-unit reserve-model inventory.
            event_reserve1_after: 1_000_000_000 + gross - 600 - 205,
            token_cpi_vault_credit_in: gross,
            token_cpi_vault_debit_out: output,
            token_cpi_recipient_credit_out: output,
        };

        let mut strict = engine_with_version(VERSION).unwrap();
        let strict_before = strict.checkpoint();
        assert_eq!(
            strict
                .apply_event_and_cpi_witnessed_swap(input, witness)
                .unwrap_err(),
            V1EngineError::CheckpointMismatch
        );
        assert_eq!(strict.checkpoint(), strict_before);

        let mut corrected = engine_with_version(VERSION).unwrap();
        corrected.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        corrected.pair.total_debt1 = 1_000;
        corrected.pair.cash_reserve1 = 999_999_000;
        corrected.pair.total_debt1_shares = 1_000_u128 * DEBT_SHARE_SCALE as u128;
        corrected
            .positions
            .get_mut(&POSITION_KEY)
            .unwrap()
            .debt1_shares = corrected.pair.total_debt1_shares;
        let shares_before = corrected.pair.total_debt1_shares;
        let receipt = corrected
            .apply_event_and_cpi_witnessed_swap(input, witness)
            .unwrap();
        assert_eq!(receipt.amount_in_after_fee, net);
        assert_eq!(receipt.amount_out, output);
        assert_eq!(receipt.lp_fee, total_fee);
        assert_eq!(receipt.protocol_fee, 0);
        assert_eq!(corrected.pair.reserve0, witness.event_reserve0_after);
        assert_eq!(corrected.pair.reserve1, witness.event_reserve1_after);
        assert_eq!(corrected.pair.cash_reserve0, 1_000_000_000 - output - 16);
        assert_eq!(corrected.pair.cash_reserve1, 1_000_998_195);
        assert_eq!(corrected.pair.total_debt1, 1_000);
        assert_eq!(corrected.pair.total_debt1_shares, shares_before);
        assert_eq!(corrected.revenue.lp_swap_fee1, 2_400);
        assert_eq!(corrected.revenue.protocol_swap_fee1, 600);
        let correction = receipt.transition_correction.unwrap();
        assert_eq!(correction.source_native_lp_fee, 2_400);
        assert_eq!(correction.source_native_protocol_fee, 600);
        assert_eq!(correction.total_debt_before_alignment, 1_000);
        assert_eq!(correction.total_debt_after_alignment, 1_000);
        assert_eq!(correction.total_debt_shares_unchanged, shares_before);
        assert_eq!(correction.protocol_inventory_delta, 805);
        assert_eq!(correction.reserve_model_inventory_delta, 205);
        assert_eq!(correction.output_protocol_inventory_delta, 16);
        assert_eq!(correction.output_reserve_model_inventory_delta, 16);

        let mut malformed = engine_with_version(VERSION).unwrap();
        malformed.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        malformed.pair.total_debt1 = 1_000;
        malformed.pair.cash_reserve1 = 999_999_000;
        let malformed_before = malformed.checkpoint();
        let mut bad_witness = witness;
        bad_witness.event_lp_fee -= 1;
        assert_eq!(
            malformed
                .apply_event_and_cpi_witnessed_swap(input, bad_witness)
                .unwrap_err(),
            V1EngineError::CheckpointMismatch
        );
        assert_eq!(malformed.checkpoint(), malformed_before);
    }

    #[test]
    fn decision_grade_borrow_correction_binds_debt_position_and_cpi_atomically() {
        let borrowed = 10_000_000_u64;
        let input = V1BorrowInputV1 {
            slot: 100,
            position: POSITION_KEY,
            debt_token: V1TokenSideV1::Token0,
            requested_amount: borrowed,
            recipient_credit: borrowed,
        };
        let mut native = engine_with_version(VERSION).unwrap();
        let native_receipt = native.apply_borrow(input).unwrap();
        let native_position = native.position(&POSITION_KEY).unwrap();
        let witnessed_shares = native_position.debt0_shares - 1;
        let witness = V1BorrowTransitionWitnessV1 {
            event_debt_token: V1TokenSideV1::Token0,
            event_borrowed_amount: borrowed,
            token_cpi_vault_debit: borrowed,
            token_cpi_recipient_credit: borrowed,
            event_position: POSITION_KEY,
            event_collateral0: native_position.collateral0,
            event_collateral1: native_position.collateral1,
            event_debt0_shares: witnessed_shares,
            event_debt1_shares: native_position.debt1_shares,
            event_collateral0_max_cf_bps: max_cf_from_liquidation(
                native_position.collateral0_liquidation_cf_bps,
            ),
            event_collateral1_max_cf_bps: max_cf_from_liquidation(
                native_position.collateral1_liquidation_cf_bps,
            ),
            event_collateral0_liquidation_cf_bps: native_position.collateral0_liquidation_cf_bps,
            event_collateral1_liquidation_cf_bps: native_position.collateral1_liquidation_cf_bps,
        };

        let mut strict = engine_with_version(VERSION).unwrap();
        let strict_before = strict.checkpoint();
        assert_eq!(
            strict
                .apply_event_and_cpi_witnessed_borrow(input, witness)
                .unwrap_err(),
            V1EngineError::CheckpointMismatch
        );
        assert_eq!(strict.checkpoint(), strict_before);

        let mut corrected = engine_with_version(VERSION).unwrap();
        corrected.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let receipt = corrected
            .apply_event_and_cpi_witnessed_borrow(input, witness)
            .unwrap();
        assert_eq!(receipt.borrowed_amount, borrowed);
        assert_eq!(receipt.debt_shares_minted, witnessed_shares);
        assert_eq!(corrected.pair.total_debt0, borrowed);
        assert_eq!(corrected.pair.reserve0, 1_000_000_000);
        assert_eq!(corrected.pair.cash_reserve0, 1_000_000_000 - borrowed);
        assert_eq!(corrected.pair.total_debt0_shares, witnessed_shares);
        assert_eq!(
            corrected.position(&POSITION_KEY).unwrap().debt0_shares,
            witnessed_shares
        );
        assert_eq!(corrected.custody.reserve_vault0, 1_000_000_000 - borrowed);
        let correction = receipt.transition_correction.unwrap();
        assert_eq!(
            correction.source_native_position_debt_shares_after,
            native_receipt.debt_shares_minted
        );
        assert_eq!(
            correction.witnessed_position_debt_shares_after,
            witnessed_shares
        );

        let mut malformed = engine_with_version(VERSION).unwrap();
        malformed.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let malformed_before = malformed.checkpoint();
        let mut bad_witness = witness;
        bad_witness.token_cpi_recipient_credit -= 1;
        assert_eq!(
            malformed
                .apply_event_and_cpi_witnessed_borrow(input, bad_witness)
                .unwrap_err(),
            V1EngineError::CheckpointMismatch
        );
        assert_eq!(malformed.checkpoint(), malformed_before);
    }

    #[test]
    fn decision_grade_repay_correction_binds_negative_debt_position_and_cpi() {
        let mut pre = engine_with_version(VERSION).unwrap();
        pre.apply_borrow(V1BorrowInputV1 {
            slot: 100,
            position: POSITION_KEY,
            debt_token: V1TokenSideV1::Token0,
            requested_amount: 100_000_000,
            recipient_credit: 100_000_000,
        })
        .unwrap();
        let repaid = 10_000_000_u64;
        let input = V1RepayInputV1 {
            slot: 100,
            position: POSITION_KEY,
            debt_token: V1TokenSideV1::Token0,
            requested_amount: repaid,
            vault_credit: repaid,
        };
        let mut native = pre.try_fork();
        let native_receipt = native.apply_repay(input).unwrap();
        let native_position = native.position(&POSITION_KEY).unwrap();
        let witnessed_shares = native_position.debt0_shares + 1;
        let witness = V1RepayTransitionWitnessV1 {
            event_debt_token: V1TokenSideV1::Token0,
            event_repaid_amount: repaid,
            token_cpi_vault_credit: repaid,
            event_position: POSITION_KEY,
            event_collateral0: native_position.collateral0,
            event_collateral1: native_position.collateral1,
            event_debt0_shares: witnessed_shares,
            event_debt1_shares: native_position.debt1_shares,
            event_collateral0_max_cf_bps: max_cf_from_liquidation(
                native_position.collateral0_liquidation_cf_bps,
            ),
            event_collateral1_max_cf_bps: max_cf_from_liquidation(
                native_position.collateral1_liquidation_cf_bps,
            ),
            event_collateral0_liquidation_cf_bps: native_position.collateral0_liquidation_cf_bps,
            event_collateral1_liquidation_cf_bps: native_position.collateral1_liquidation_cf_bps,
        };

        let mut corrected = pre.try_fork();
        corrected.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let pre_position_shares = corrected.position(&POSITION_KEY).unwrap().debt0_shares;
        let receipt = corrected
            .apply_event_and_cpi_witnessed_repay(input, witness)
            .unwrap();
        assert_eq!(receipt.repaid_amount, repaid);
        assert_eq!(
            receipt.debt_shares_burned,
            pre_position_shares - witnessed_shares
        );
        assert_eq!(corrected.pair.total_debt0, 90_000_000);
        assert_eq!(corrected.pair.cash_reserve0, 910_000_000);
        assert_eq!(corrected.pair.reserve0, 1_000_000_000);
        assert_eq!(
            corrected.position(&POSITION_KEY).unwrap().debt0_shares,
            witnessed_shares
        );
        assert_eq!(
            corrected.pair.total_debt0_shares,
            pre.pair.total_debt0_shares - (pre_position_shares - witnessed_shares)
        );
        assert_eq!(corrected.custody.reserve_vault0, 910_000_000);
        let correction = receipt.transition_correction.unwrap();
        assert_eq!(
            correction.source_native_position_debt_shares_after,
            native_position.debt0_shares
        );
        assert_eq!(
            correction.witnessed_position_debt_shares_after,
            witnessed_shares
        );
        assert_eq!(
            native_receipt.repaid_amount,
            correction.source_native_repaid_amount
        );

        let mut malformed = pre.try_fork();
        malformed.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let malformed_before = malformed.checkpoint();
        let mut bad_witness = witness;
        bad_witness.token_cpi_vault_credit -= 1;
        assert_eq!(
            malformed
                .apply_event_and_cpi_witnessed_repay(input, bad_witness)
                .unwrap_err(),
            V1EngineError::CheckpointMismatch
        );
        assert_eq!(malformed.checkpoint(), malformed_before);
    }

    #[test]
    fn decision_grade_max_repay_accepts_only_event_proven_full_payoff_gap() {
        let mut pre = engine_with_version(VERSION).unwrap();
        pre.apply_borrow(V1BorrowInputV1 {
            slot: 100,
            position: POSITION_KEY,
            debt_token: V1TokenSideV1::Token0,
            requested_amount: 100_000_000,
            recipient_credit: 100_000_000,
        })
        .unwrap();
        let target = copy_position(pre.position(&POSITION_KEY).unwrap());
        let target_shares = target.debt0_shares;
        let other_key = Pubkey::new_from_array([77; 32]);
        let mut other = copy_position(&target);
        other.owner = Pubkey::new_from_array([78; 32]);
        other.debt0_shares = target_shares;
        pre.positions.insert(other_key, other);
        pre.pair.total_debt0 = 199_999_997;
        pre.pair.cash_reserve0 = 800_000_003;
        pre.pair.total_debt0_shares = target_shares * 2;
        pre.custody.reserve_vault0 = 800_000_003;
        let modeled = debt_for_side(
            pre.position(&POSITION_KEY).unwrap(),
            pre.pair(),
            V1TokenSideV1::Token0,
        )
        .unwrap();
        assert_eq!(modeled, 99_999_999);

        let input = V1RepayInputV1 {
            slot: 100,
            position: POSITION_KEY,
            debt_token: V1TokenSideV1::Token0,
            requested_amount: u64::MAX,
            vault_credit: 100_000_000,
        };
        let witness = V1RepayTransitionWitnessV1 {
            event_debt_token: V1TokenSideV1::Token0,
            event_repaid_amount: 100_000_000,
            token_cpi_vault_credit: 100_000_000,
            event_position: POSITION_KEY,
            event_collateral0: target.collateral0,
            event_collateral1: target.collateral1,
            event_debt0_shares: 0,
            event_debt1_shares: target.debt1_shares,
            event_collateral0_max_cf_bps: max_cf_from_liquidation(
                target.collateral0_liquidation_cf_bps,
            ),
            event_collateral1_max_cf_bps: max_cf_from_liquidation(
                target.collateral1_liquidation_cf_bps,
            ),
            event_collateral0_liquidation_cf_bps: target.collateral0_liquidation_cf_bps,
            event_collateral1_liquidation_cf_bps: target.collateral1_liquidation_cf_bps,
        };
        pre.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let receipt = pre
            .apply_event_and_cpi_witnessed_repay(input, witness)
            .unwrap();
        assert_eq!(receipt.debt_before, 100_000_000);
        assert_eq!(receipt.repaid_amount, 100_000_000);
        assert_eq!(receipt.debt_after, 0);
        assert_eq!(receipt.debt_shares_burned, target_shares);
        assert_eq!(pre.position(&POSITION_KEY).unwrap().debt0_shares, 0);
        assert_eq!(pre.pair.total_debt0, 99_999_997);
        assert_eq!(pre.pair.cash_reserve0, 900_000_003);
        assert_eq!(pre.pair.total_debt0_shares, target_shares);
        let correction = receipt.transition_correction.unwrap();
        assert_eq!(correction.source_native_position_debt_before, modeled);
        assert_eq!(correction.witnessed_position_debt_before, 100_000_000);
        assert_eq!(correction.modeled_position_debt_gap, 1);
    }

    #[test]
    fn decision_grade_protocol_claim_realizes_exact_cpi_and_retains_residual() {
        let mut initial_custody = V1CustodyCheckpointV1 {
            reserve_vault0: 1_000_001_000,
            reserve_vault1: 1_000_002_000,
            collateral_vault0: 0,
            collateral_vault1: 200_000_000,
        };
        let accounts = move || V1DecodedAccountsV1 {
            program_version: VERSION,
            pair_key: PAIR_KEY,
            pair: pair(),
            rate_model_key: RATE_MODEL_KEY,
            rate_model: rate_model(),
            futarchy_authority: authority(),
            positions: vec![(POSITION_KEY, position())],
            custody: initial_custody,
            revenue: V1RevenueCheckpointV1::ZERO,
        };
        let partial = V1ProtocolClaimInputV1 {
            slot: 100,
            amount0: 900,
            amount1: 1_500,
        };

        let mut strict = V1NativeEngine::from_decoded_accounts(accounts()).unwrap();
        let strict_before = strict.checkpoint();
        assert_eq!(
            strict.apply_protocol_claim(partial),
            Err(V1EngineError::CustodyDeltaMismatch {
                field: "protocol claim amount0",
                expected: 1_000,
                actual: 900,
            })
        );
        assert_eq!(strict.checkpoint(), strict_before);

        let mut corrected = V1NativeEngine::from_decoded_accounts(accounts()).unwrap();
        corrected.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let pair_before = pair_checkpoint(corrected.pair());
        let receipt = corrected
            .apply_cpi_witnessed_protocol_claim(partial)
            .unwrap();
        assert_eq!(pair_checkpoint(corrected.pair()), pair_before);
        initial_custody.reserve_vault0 -= partial.amount0;
        initial_custody.reserve_vault1 -= partial.amount1;
        assert_eq!(corrected.custody(), initial_custody);
        assert_eq!(corrected.revenue().protocol_claim_realized0, 900);
        assert_eq!(corrected.revenue().protocol_claim_realized1, 1_500);
        assert_eq!(receipt.claimable0, 900);
        assert_eq!(receipt.claimable1, 1_500);
        assert_eq!(receipt.futarchy_amount0, 360);
        assert_eq!(receipt.buybacks_amount0, 270);
        assert_eq!(receipt.team_amount0, 270);
        assert_eq!(receipt.futarchy_amount1, 600);
        assert_eq!(receipt.buybacks_amount1, 450);
        assert_eq!(receipt.team_amount1, 450);
        let correction = receipt.realization_correction.unwrap();
        assert_eq!(correction.source_native_claimable0, 1_000);
        assert_eq!(correction.source_native_claimable1, 2_000);
        assert_eq!(correction.witnessed_realized0, 900);
        assert_eq!(correction.witnessed_realized1, 1_500);
        assert_eq!(correction.modeled_residual0, 100);
        assert_eq!(correction.modeled_residual1, 500);
        assert_eq!(correction.reserve_vault0_after_claim, 1_000_000_100);
        assert_eq!(correction.reserve_vault1_after_claim, 1_000_000_500);

        let mut malformed = V1NativeEngine::from_decoded_accounts(accounts()).unwrap();
        malformed.post_state_correction_policy =
            V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let malformed_before = malformed.checkpoint();
        assert_eq!(
            malformed.apply_cpi_witnessed_protocol_claim(V1ProtocolClaimInputV1 {
                slot: 100,
                amount0: 1_001,
                amount1: 2_000,
            }),
            Err(V1EngineError::CustodyDeltaMismatch {
                field: "protocol claim amount0",
                expected: 1_000,
                actual: 1_001,
            })
        );
        assert_eq!(malformed.checkpoint(), malformed_before);
    }

    #[test]
    fn accrual_uses_native_rate_model_and_exact_v1_accounting() {
        let mut initial_pair = pair();
        initial_pair.total_debt0 = 600_000_000;
        initial_pair.total_debt0_shares = 600_000_000u128 * DEBT_SHARE_SCALE as u128;
        initial_pair.cash_reserve0 = 400_000_000;
        let initial_rate = initial_pair.last_rate0;
        let model = rate_model();
        let elapsed_ms = slots_to_ms(100, 10_100).unwrap();
        let utilization = 600_000_000u64 * NAD / 1_000_000_000;
        let (expected_rate, integral) = model.calculate_rate(initial_rate, elapsed_ms, utilization);
        let lp_interest = ceil_div(600_000_000u128 * integral as u128, NAD as u128).unwrap() as u64;
        let protocol_interest = lp_interest * 1_000 / BPS_DENOMINATOR as u64;

        let mut engine = V1NativeEngine::from_decoded_accounts(V1DecodedAccountsV1 {
            program_version: VERSION,
            pair_key: PAIR_KEY,
            pair: initial_pair,
            rate_model_key: RATE_MODEL_KEY,
            rate_model: model,
            futarchy_authority: authority(),
            positions: vec![(POSITION_KEY, position())],
            custody: V1CustodyCheckpointV1 {
                reserve_vault0: 1_000_000_000,
                reserve_vault1: 1_000_000_000,
                collateral_vault0: 0,
                collateral_vault1: 200_000_000,
            },
            revenue: V1RevenueCheckpointV1::ZERO,
        })
        .unwrap();
        let receipt = engine.advance_to_slot(10_100).unwrap();

        assert_eq!(receipt.utilization0_nad, utilization);
        assert_eq!(receipt.rate0_nad, expected_rate);
        assert_eq!(receipt.integral0_nad, integral);
        assert_eq!(receipt.lp_interest0, lp_interest);
        assert_eq!(receipt.protocol_interest0, protocol_interest);
        assert_eq!(
            engine.pair().total_debt0,
            600_000_000 + lp_interest + protocol_interest
        );
        assert_eq!(engine.pair().reserve0, 1_000_000_000 + lp_interest);
        assert_eq!(engine.pair().cash_reserve0, 400_000_000 - protocol_interest);
        assert_eq!(engine.pair().last_rate0, expected_rate);
    }

    #[test]
    fn borrow_and_repay_match_native_position_share_transitions() {
        let mut engine = engine_with_version(VERSION).unwrap();
        let capacity = engine
            .borrow_capacity(POSITION_KEY, V1TokenSideV1::Token0)
            .unwrap();
        let expected_capacity = pessimistic_max_debt(
            200_000_000,
            NAD,
            NAD,
            1_000_000_000,
            1_000_000_000,
            0,
            Some(8_000),
        )
        .unwrap();
        assert_eq!(capacity.admissible_max_debt, expected_capacity.0);
        assert_eq!(capacity.max_cf_bps, expected_capacity.1);
        assert_eq!(capacity.liquidation_cf_bps, expected_capacity.2);

        let borrow_amount = capacity.admissible_max_debt / 2;
        let mut native_pair = pair();
        let mut native_position = position();
        native_position
            .increase_debt(&mut native_pair, &TOKEN0, borrow_amount)
            .unwrap();
        native_position.set_liquidation_cf_for_debt_token(
            &TOKEN0,
            &native_pair,
            capacity.liquidation_cf_bps,
        );
        let borrow = engine
            .apply_borrow(V1BorrowInputV1 {
                slot: 100,
                position: POSITION_KEY,
                debt_token: V1TokenSideV1::Token0,
                requested_amount: borrow_amount,
                recipient_credit: borrow_amount,
            })
            .unwrap();
        let replayed_position = engine.position(&POSITION_KEY).unwrap();
        assert_eq!(engine.pair().total_debt0, native_pair.total_debt0);
        assert_eq!(
            engine.pair().total_debt0_shares,
            native_pair.total_debt0_shares
        );
        assert_eq!(engine.pair().cash_reserve0, native_pair.cash_reserve0);
        assert_eq!(replayed_position.debt0_shares, native_position.debt0_shares);
        assert_eq!(borrow.debt_after, borrow_amount);

        let repay_amount = borrow_amount / 3;
        native_position
            .decrease_debt(
                &mut native_pair,
                &TOKEN0,
                repay_amount,
                DebtDecreaseReason::Repayment,
            )
            .unwrap();
        let repay = engine
            .apply_repay(V1RepayInputV1 {
                slot: 100,
                position: POSITION_KEY,
                debt_token: V1TokenSideV1::Token0,
                requested_amount: repay_amount,
                vault_credit: repay_amount,
            })
            .unwrap();
        let replayed_position = engine.position(&POSITION_KEY).unwrap();
        assert_eq!(engine.pair().total_debt0, native_pair.total_debt0);
        assert_eq!(
            engine.pair().total_debt0_shares,
            native_pair.total_debt0_shares
        );
        assert_eq!(engine.pair().cash_reserve0, native_pair.cash_reserve0);
        assert_eq!(replayed_position.debt0_shares, native_position.debt0_shares);
        assert_eq!(repay.debt_before, borrow_amount);
        assert_eq!(repay.debt_after, borrow_amount - repay_amount);
    }

    #[test]
    fn interest_revenue_allocates_only_cash_covered_protocol_economics() {
        let mut revenue = V1RevenueCheckpointV1::ZERO;
        add_interest_revenue(&mut revenue, 100, 50, 20, 10, 7, 10).unwrap();
        assert_eq!(revenue.lp_interest0, 113);
        assert_eq!(revenue.protocol_interest0, 7);
        assert_eq!(revenue.lp_interest1, 50);
        assert_eq!(revenue.protocol_interest1, 10);
        assert_eq!(revenue.lp_interest0 + revenue.protocol_interest0, 120);
        assert_eq!(revenue.lp_interest1 + revenue.protocol_interest1, 60);
    }

    #[test]
    fn failed_operation_rolls_back_clock_update_and_checkpoint_asserts_exactly() {
        let mut engine = engine_with_version(VERSION).unwrap();
        let before = engine.checkpoint();
        let error = engine
            .apply_swap(V1SwapInputV1 {
                slot: 200,
                token_in: V1TokenSideV1::Token0,
                gross_amount_in: 1_000,
                min_amount_out: u64::MAX,
                vault_credit_in: 1_000,
                vault_debit_out: 1,
                recipient_credit_out: 1,
            })
            .unwrap_err();
        assert!(matches!(error, V1EngineError::SlippageExceeded { .. }));
        engine.assert_checkpoint(&before).unwrap();

        let mut wrong = before;
        wrong.pair.reserve0 += 1;
        assert_eq!(
            engine.assert_checkpoint(&wrong),
            Err(V1EngineError::CheckpointMismatch)
        );
    }
}

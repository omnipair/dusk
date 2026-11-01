//! Strict canonical-operation orchestration for the factual V1 engine.
//!
//! Native account transitions happen exactly once in [`V1NativeEngine`].
//! Emitted events are consumed only as post-transition assertions. Every
//! operation is staged on a fork, reconciled in raw units, checked for
//! conservation, and committed only after all checks pass.

use std::{collections::BTreeMap, str::FromStr};

use omnipair::{
    constants::{
        BPS_DENOMINATOR, CLOSE_FACTOR_BPS, DEFAULT_MAX_RATE_BPS, DEFAULT_MIN_RATE_BPS,
        DEFAULT_RATE_HALF_LIFE_MS, LTV_BUFFER_BPS, NAD, POSITION_SEED_PREFIX, TARGET_UTIL_END_BPS,
        TARGET_UTIL_START_BPS,
    },
    state::{RevenueDistribution, RevenueRecipients, UserPosition},
};
use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::{
    canonical::{
        CanonicalEventV1, ChainOrder, EventPayloadV1, LiquidityAction, PairDeltaPayloadV1,
        SignedTokenAmount, TokenAmount,
    },
    pubkey_encoding::parse_schema_pubkey,
    replay::{CanonicalOperationV1, OperationPayloadV1, ReplayError},
    v1_engine::{
        V1AddLiquidityInputV1, V1AddLiquidityReceiptV1, V1BorrowCapacityV1, V1BorrowInputV1,
        V1BorrowReceiptV1, V1BorrowTransitionWitnessV1, V1CanonicalInitializationInputV1,
        V1CollateralFactorTransitionPolicyV1, V1CollateralInputV1, V1CollateralReceiptV1,
        V1CollateralWithdrawalWitnessV1, V1DecodedAccountsV1, V1EngineError, V1FlashloanInputV1,
        V1FlashloanReceiptV1, V1InitializationAssertionV1, V1InterestAccrualV1,
        V1LiquidationInputV1, V1LiquidationReceiptV1, V1LiquidationTransitionWitnessV1,
        V1LpMintSupplyWitnessV1, V1NativeCheckpointV1, V1NativeEngine,
        V1PositionCollateralFactorWitnessV1, V1PostStateCorrectionPolicyV1, V1ProtocolClaimInputV1,
        V1ProtocolClaimReceiptV1, V1RemoveLiquidityCashWitnessV1, V1RemoveLiquidityInputV1,
        V1RemoveLiquidityReceiptV1, V1RepayInputV1, V1RepayReceiptV1, V1RepayTransitionWitnessV1,
        V1RevenueCheckpointV1, V1SwapInputV1, V1SwapReceiptV1, V1SwapTransitionWitnessV1,
        V1TokenSideV1,
    },
};

pub const FACTUAL_REPLAY_SCHEMA_VERSION: u16 = 6;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum V1DecisionGradeCorrectionEvidenceV1 {
    LpMintTotalSupply {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        lp_mint: String,
        source_native_amount0_used: TokenAmount,
        source_native_amount1_used: TokenAmount,
        source_native_minted_lp: TokenAmount,
        event_amount0_used: TokenAmount,
        event_amount1_used: TokenAmount,
        event_minted_lp: TokenAmount,
        token_cpi_vault_credit0: TokenAmount,
        token_cpi_vault_credit1: TokenAmount,
        token_cpi_supply_delta: TokenAmount,
        total_supply_before: TokenAmount,
        total_supply_after: TokenAmount,
    },
    SwapTransition {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        token_in: V1TokenSideV1,
        gross_amount_in: TokenAmount,
        source_native_amount_in_after_fee: TokenAmount,
        source_native_amount_out: TokenAmount,
        source_native_lp_fee: TokenAmount,
        source_native_protocol_fee: TokenAmount,
        source_native_reserve0_after: TokenAmount,
        source_native_reserve1_after: TokenAmount,
        event_amount_in_after_fee: TokenAmount,
        event_amount_out: TokenAmount,
        event_lp_fee: TokenAmount,
        event_protocol_fee: TokenAmount,
        config_accounted_lp_fee: TokenAmount,
        config_accounted_protocol_fee: TokenAmount,
        event_reserve0_after: TokenAmount,
        event_reserve1_after: TokenAmount,
        token_cpi_vault_credit_in: TokenAmount,
        token_cpi_vault_debit_out: TokenAmount,
        token_cpi_recipient_credit_out: TokenAmount,
        reserve_debt_alignment_side: V1TokenSideV1,
        total_debt_before_alignment: TokenAmount,
        total_debt_after_alignment: TokenAmount,
        total_debt_shares_unchanged: TokenAmount,
        reserve_debt_alignment_amount: TokenAmount,
        protocol_inventory_before: TokenAmount,
        protocol_inventory_after: TokenAmount,
        protocol_inventory_delta: TokenAmount,
        reserve_model_inventory_delta: TokenAmount,
        output_protocol_inventory_before: TokenAmount,
        output_protocol_inventory_after: TokenAmount,
        output_protocol_inventory_delta: TokenAmount,
        output_reserve_model_inventory_delta: TokenAmount,
        k_before: TokenAmount,
        k_after: TokenAmount,
        k_delta: SignedTokenAmount,
    },
    BorrowTransition {
        debt_delta_event_id: String,
        debt_delta_raw_payload_sha256: String,
        position_event_id: String,
        position_raw_payload_sha256: String,
        position: String,
        debt_token: V1TokenSideV1,
        borrowed_amount: TokenAmount,
        token_cpi_vault_debit: TokenAmount,
        token_cpi_recipient_credit: TokenAmount,
        source_native_position_debt_shares_after: TokenAmount,
        event_position_debt_shares_after: TokenAmount,
        source_native_total_debt_shares_after: TokenAmount,
        witnessed_total_debt_shares_after: TokenAmount,
        source_native_total_debt_after: TokenAmount,
        witnessed_total_debt_after: TokenAmount,
        witnessed_position_debt_share_delta: TokenAmount,
    },
    CollateralDepositFactorTransition {
        collateral_delta_event_id: String,
        collateral_delta_raw_payload_sha256: String,
        position_event_id: String,
        position_raw_payload_sha256: String,
        position: String,
        collateral_token: V1TokenSideV1,
        deposited_amount: TokenAmount,
        token_cpi_collateral_vault_credit: TokenAmount,
        token_cpi_counterparty_debit: TokenAmount,
        preserve_candidate: V1PositionCollateralFactorWitnessV1,
        recompute_candidate: V1PositionCollateralFactorWitnessV1,
        event_witnessed: V1PositionCollateralFactorWitnessV1,
    },
    CollateralWithdrawalFactorTransition {
        collateral_delta_event_id: String,
        collateral_delta_raw_payload_sha256: String,
        position_event_id: String,
        position_raw_payload_sha256: String,
        position: String,
        collateral_token: V1TokenSideV1,
        requested_amount: TokenAmount,
        source_native_resolved_amount: TokenAmount,
        witnessed_resolved_amount: TokenAmount,
        withdrawn_amount: TokenAmount,
        token_cpi_collateral_vault_debit: TokenAmount,
        token_cpi_recipient_credit: TokenAmount,
        source_native_collateral0_max_cf_bps: u16,
        source_native_collateral1_max_cf_bps: u16,
        source_native_collateral0_liquidation_cf_bps: u16,
        source_native_collateral1_liquidation_cf_bps: u16,
        event_collateral0_max_cf_bps: u16,
        event_collateral1_max_cf_bps: u16,
        event_collateral0_liquidation_cf_bps: u16,
        event_collateral1_liquidation_cf_bps: u16,
        post_capacity_debt: Option<TokenAmount>,
        post_capacity_maximum: Option<TokenAmount>,
        post_capacity_excess_debt_atoms: Option<TokenAmount>,
    },
    LiquidationTransition {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        position: String,
        debt_token: V1TokenSideV1,
        source_native_debt_written_off: TokenAmount,
        event_debt_written_off: TokenAmount,
        debt_shares_written_off: TokenAmount,
        source_native_collateral_seized: TokenAmount,
        event_collateral_seized: TokenAmount,
        source_native_collateral_to_reserves: TokenAmount,
        token_cpi_reserve_vault_credit: TokenAmount,
        source_native_liquidator_incentive: TokenAmount,
        event_liquidator_incentive: TokenAmount,
        token_cpi_collateral_vault_debit: TokenAmount,
        token_cpi_liquidator_recipient_credit: TokenAmount,
        source_native_debt_reserve_after: TokenAmount,
        witnessed_debt_reserve_after: TokenAmount,
        source_native_total_debt_after: TokenAmount,
        witnessed_total_debt_after: TokenAmount,
        source_native_collateral_reserve_after: TokenAmount,
        witnessed_collateral_reserve_after: TokenAmount,
        source_native_collateral_cash_after: TokenAmount,
        witnessed_collateral_cash_after: TokenAmount,
        source_native_total_collateral_after: TokenAmount,
        witnessed_total_collateral_after: TokenAmount,
        source_native_position_collateral_after: TokenAmount,
        witnessed_position_collateral_after: TokenAmount,
        source_native_collateral_vault_after: TokenAmount,
        witnessed_collateral_vault_after: TokenAmount,
    },
    LpCashProtocolInterestCheckpointCatchUp {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        source_native_cash_reserve0_after: TokenAmount,
        source_native_cash_reserve1_after: TokenAmount,
        event_cash_reserve0_after: TokenAmount,
        event_cash_reserve1_after: TokenAmount,
        source_native_total_debt0_after: TokenAmount,
        source_native_total_debt1_after: TokenAmount,
        witnessed_total_debt0_after: TokenAmount,
        witnessed_total_debt1_after: TokenAmount,
        source_native_protocol_interest0_after: TokenAmount,
        source_native_protocol_interest1_after: TokenAmount,
        witnessed_protocol_interest0_after: TokenAmount,
        witnessed_protocol_interest1_after: TokenAmount,
        protocol_interest_inventory_rounding_atoms0: SignedTokenAmount,
        protocol_interest_inventory_rounding_atoms1: SignedTokenAmount,
        token_cpi_reserve_vault0_after: TokenAmount,
        token_cpi_reserve_vault1_after: TokenAmount,
    },
    LpBurnCashCheckpoint {
        burn_event_id: String,
        burn_raw_payload_sha256: String,
        cash_event_id: String,
        cash_raw_payload_sha256: String,
        liquidity_burned: TokenAmount,
        source_native_amount0_out: TokenAmount,
        source_native_amount1_out: TokenAmount,
        event_amount0_out: TokenAmount,
        event_amount1_out: TokenAmount,
        source_native_reserve0_after: TokenAmount,
        source_native_reserve1_after: TokenAmount,
        witnessed_reserve0_after: TokenAmount,
        witnessed_reserve1_after: TokenAmount,
        token_cpi_vault_debit0: TokenAmount,
        token_cpi_vault_debit1: TokenAmount,
        token_cpi_recipient_credit0: TokenAmount,
        token_cpi_recipient_credit1: TokenAmount,
        source_native_cash_reserve0_after: TokenAmount,
        source_native_cash_reserve1_after: TokenAmount,
        event_cash_reserve0_after: TokenAmount,
        event_cash_reserve1_after: TokenAmount,
        source_native_total_debt0_after: TokenAmount,
        source_native_total_debt1_after: TokenAmount,
        witnessed_total_debt0_after: TokenAmount,
        witnessed_total_debt1_after: TokenAmount,
        cash_checkpoint_rounding_atoms0: SignedTokenAmount,
        cash_checkpoint_rounding_atoms1: SignedTokenAmount,
    },
    LpBurnPostDebtCoverageException {
        burn_event_id: String,
        burn_raw_payload_sha256: String,
        cash_event_id: String,
        cash_raw_payload_sha256: String,
        liquidity_burned: TokenAmount,
        required_token1_for_debt0: TokenAmount,
        buffered_token1_for_debt0: TokenAmount,
        post_reserve1: TokenAmount,
        token1_shortfall_atoms: TokenAmount,
        token1_coverage_failed: bool,
        required_token0_for_debt1: TokenAmount,
        buffered_token0_for_debt1: TokenAmount,
        post_reserve0: TokenAmount,
        token0_shortfall_atoms: TokenAmount,
        token0_coverage_failed: bool,
    },
    RepayTransition {
        debt_delta_event_id: String,
        debt_delta_raw_payload_sha256: String,
        position_event_id: String,
        position_raw_payload_sha256: String,
        position: String,
        debt_token: V1TokenSideV1,
        requested_amount: TokenAmount,
        source_native_position_debt_before: TokenAmount,
        witnessed_position_debt_before: TokenAmount,
        modeled_position_debt_gap: TokenAmount,
        source_native_repaid_amount: TokenAmount,
        witnessed_repaid_amount: TokenAmount,
        token_cpi_vault_credit: TokenAmount,
        source_native_position_debt_shares_after: TokenAmount,
        event_position_debt_shares_after: TokenAmount,
        source_native_total_debt_shares_after: TokenAmount,
        witnessed_total_debt_shares_after: TokenAmount,
        source_native_total_debt_after: TokenAmount,
        witnessed_total_debt_after: TokenAmount,
        witnessed_position_debt_share_delta: TokenAmount,
    },
    ProtocolClaimPartialRealization {
        source_native_claimable0: TokenAmount,
        source_native_claimable1: TokenAmount,
        token_cpi_reserve_vault_debit0: TokenAmount,
        token_cpi_reserve_vault_debit1: TokenAmount,
        modeled_inventory_residual0: TokenAmount,
        modeled_inventory_residual1: TokenAmount,
        reserve_vault0_before_claim: TokenAmount,
        reserve_vault1_before_claim: TokenAmount,
        reserve_vault0_after_claim: TokenAmount,
        reserve_vault1_after_claim: TokenAmount,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum V1DecisionGradeRetainedEventLimitationV1 {
    LpPositionDerivedEntitlement {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        lp_amount: TokenAmount,
        observed_amount0: TokenAmount,
        observed_amount1: TokenAmount,
    },
    ProtocolInterestCheckpointCatchUpTiming {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        interval_start_slot: u64,
        checkpoint_slot: u64,
        source_native_cash_covered_protocol_interest0: TokenAmount,
        source_native_cash_covered_protocol_interest1: TokenAmount,
        witnessed_cash_covered_protocol_interest0: TokenAmount,
        witnessed_cash_covered_protocol_interest1: TokenAmount,
    },
    ProtocolInterestInventoryRounding {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        interval_start_slot: u64,
        checkpoint_slot: u64,
        inventory_implied_protocol_interest0_after: TokenAmount,
        inventory_implied_protocol_interest1_after: TokenAmount,
        source_native_protocol_interest0_after: TokenAmount,
        source_native_protocol_interest1_after: TokenAmount,
        protocol_interest_inventory_rounding_atoms0: SignedTokenAmount,
        protocol_interest_inventory_rounding_atoms1: SignedTokenAmount,
    },
    LpBurnCashCheckpointModelResidual {
        cash_event_id: String,
        cash_raw_payload_sha256: String,
        interval_start_slot: u64,
        checkpoint_slot: u64,
        cash_checkpoint_rounding_atoms0: SignedTokenAmount,
        cash_checkpoint_rounding_atoms1: SignedTokenAmount,
    },
    LiquidationDerivedMetadata {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        event_collateral_price_nad: TokenAmount,
        source_native_collateral_price_nad: TokenAmount,
        event_k_before: TokenAmount,
        source_native_k_before: TokenAmount,
        event_k_after: TokenAmount,
        source_native_k_after: TokenAmount,
    },
    SwapEventHistoricalFeeLabels {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        event_lp_fee_label: TokenAmount,
        event_protocol_fee_label: TokenAmount,
        config_accounted_lp_fee: TokenAmount,
        config_accounted_protocol_fee: TokenAmount,
        reserve_model_inventory_delta: TokenAmount,
        output_reserve_model_inventory_delta: TokenAmount,
    },
    SwapEventKDecrease {
        assertion_event_id: String,
        assertion_raw_payload_sha256: String,
        k_before: TokenAmount,
        k_after: TokenAmount,
        k_delta: SignedTokenAmount,
    },
    ProtocolClaimRecipientDistributionAttribution {
        token_cpi_realized0: TokenAmount,
        token_cpi_realized1: TokenAmount,
        modeled_futarchy_amount0: TokenAmount,
        modeled_futarchy_amount1: TokenAmount,
        modeled_buybacks_amount0: TokenAmount,
        modeled_buybacks_amount1: TokenAmount,
        modeled_team_amount0: TokenAmount,
        modeled_team_amount1: TokenAmount,
    },
    ClockOnlyZeroPositionLifecycleAssertion {
        collateral_delta_event_id: String,
        collateral_delta_raw_payload_sha256: String,
        position_event_id: String,
        position_raw_payload_sha256: String,
        position: String,
    },
    PositionOwnerEventAttribution {
        creation_event_id: String,
        creation_raw_payload_sha256: String,
        outer_actor: String,
        event_owner: String,
        position: String,
        position_bump: u8,
    },
    CollateralWithdrawalPostCapacityException {
        position_event_id: String,
        position_raw_payload_sha256: String,
        position: String,
        requested_amount: TokenAmount,
        witnessed_withdrawal_amount: TokenAmount,
        post_withdraw_debt: TokenAmount,
        admissible_max_debt: TokenAmount,
        excess_debt_atoms: TokenAmount,
    },
    ClockOnlyZeroLiquidationLifecycleAssertion {
        debt_delta_event_id: String,
        debt_delta_raw_payload_sha256: String,
        position_event_id: String,
        position_raw_payload_sha256: String,
        liquidation_event_id: String,
        liquidation_raw_payload_sha256: String,
        position: String,
        debt_token: V1TokenSideV1,
        pre_debt_shares: TokenAmount,
        post_debt_shares: TokenAmount,
        debt_shares_written_off: TokenAmount,
        observed_collateral_price_nad: TokenAmount,
        unchanged_k: TokenAmount,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RawUnitReconciliationV1 {
    pub source: String,
    pub field: String,
    pub expected: String,
    pub actual: String,
    pub raw_unit_difference: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum V1IdentityRelationV1 {
    Equal,
    GreaterThanOrEqual,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1ConservationIdentityV1 {
    pub name: String,
    pub relation: V1IdentityRelationV1,
    pub left: String,
    pub right: String,
    pub raw_unit_difference: String,
    pub holds: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1RevenueDeltaV1 {
    pub lp_swap_fee0: u128,
    pub lp_swap_fee1: u128,
    pub protocol_swap_fee0: u128,
    pub protocol_swap_fee1: u128,
    pub lp_interest0: u128,
    pub lp_interest1: u128,
    pub protocol_interest0: u128,
    pub protocol_interest1: u128,
    /// Native nominal protocol source amount before the V1 cash-reserve cap.
    pub nominal_protocol_interest0: u128,
    pub nominal_protocol_interest1: u128,
    /// Native protocol amount actually removed from cash reserve. This equals
    /// economic protocol interest and remains explicit source evidence.
    pub cash_covered_protocol_interest0: u128,
    pub cash_covered_protocol_interest1: u128,
    pub flashloan_fee0: u128,
    pub flashloan_fee1: u128,
    pub protocol_claim_realized0: u128,
    pub protocol_claim_realized1: u128,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1FactualOperationRecordV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub order: ChainOrder,
    pub checkpoint_before: Option<V1NativeCheckpointV1>,
    pub checkpoint_after: V1NativeCheckpointV1,
    pub reconciliations: Vec<V1RawUnitReconciliationV1>,
    pub conservation: Vec<V1ConservationIdentityV1>,
    pub revenue_delta: V1RevenueDeltaV1,
    /// Narrow DecisionGrade post-state corrections. Empty for AuditGrade and
    /// whenever linked native state already matches exact chain evidence.
    pub corrections: Vec<V1DecisionGradeCorrectionEvidenceV1>,
    /// Non-state event metadata retained verbatim when provisional
    /// DecisionGrade evidence cannot uniquely reconstruct its source inputs.
    pub retained_event_limitations: Vec<V1DecisionGradeRetainedEventLimitationV1>,
    /// Exact behavioral fractions consumed by the Dusk counterfactual. These
    /// are derived from native post-accrual V1 state, never reconstructed from
    /// emitted amount fields downstream.
    pub behavior: Option<V1BehaviorObservationV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum V1BehaviorObservationV1 {
    Borrow {
        capacity_before: V1BorrowCapacityV1,
        capacity_after: V1BorrowCapacityV1,
        capacity_fraction_numerator: TokenAmount,
        capacity_fraction_denominator: TokenAmount,
    },
    Repay {
        debt_before: TokenAmount,
        debt_after: TokenAmount,
        reduction_fraction_numerator: TokenAmount,
        reduction_fraction_denominator: TokenAmount,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1CanonicalFieldRequirementV1 {
    pub operation_kind: String,
    pub fields: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum V1AppliedReceiptV1 {
    Initialize(V1InitializationAssertionV1),
    AddLiquidity(V1AddLiquidityReceiptV1),
    RemoveLiquidity(V1RemoveLiquidityReceiptV1),
    Swap(V1SwapReceiptV1),
    Collateral(V1CollateralReceiptV1),
    Borrow(V1BorrowReceiptV1),
    Repay(V1RepayReceiptV1),
    Liquidation(V1LiquidationReceiptV1),
    Flashloan(V1FlashloanReceiptV1),
    ProtocolClaim(V1ProtocolClaimReceiptV1),
    Clock(V1InterestAccrualV1),
    Config,
}

impl V1AppliedReceiptV1 {
    fn accrual(&self) -> Option<&V1InterestAccrualV1> {
        match self {
            Self::AddLiquidity(receipt) => Some(&receipt.accrual),
            Self::RemoveLiquidity(receipt) => Some(&receipt.accrual),
            Self::Swap(receipt) => Some(&receipt.accrual),
            Self::Collateral(receipt) => Some(&receipt.accrual),
            Self::Borrow(receipt) => Some(&receipt.accrual),
            Self::Repay(receipt) => Some(&receipt.accrual),
            Self::Liquidation(receipt) => Some(&receipt.accrual),
            Self::Flashloan(receipt) => Some(&receipt.accrual),
            Self::ProtocolClaim(receipt) => Some(&receipt.accrual),
            Self::Clock(receipt) => Some(receipt),
            Self::Initialize(_) | Self::Config => None,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum V1FactualReplayError {
    #[error(transparent)]
    Replay(#[from] ReplayError),
    #[error(transparent)]
    Engine(#[from] V1EngineError),
    #[error("operation pair {operation_pair} does not match engine pair {engine_pair}")]
    PairMismatch {
        operation_pair: String,
        engine_pair: Pubkey,
    },
    #[error("operation {operation_id} is not strictly after the preceding canonical order")]
    NonCanonicalOrder { operation_id: String },
    #[error("invalid public key in canonical field {field}: {value}")]
    InvalidPubkey { field: String, value: String },
    #[error("canonical integer field {field} is outside its native domain: {value}")]
    InvalidAmount { field: String, value: String },
    #[error("operation {operation_id} is missing canonical fields: {fields:?}")]
    MissingCanonicalFields {
        operation_id: String,
        fields: Vec<String>,
    },
    #[error(
        "assertion {event_id} field {field} mismatched: expected {expected}, replayed {actual}"
    )]
    AssertionMismatch {
        event_id: String,
        field: String,
        expected: String,
        actual: String,
    },
    #[error("unsupported assertion payload {kind} in event {event_id}")]
    UnsupportedAssertion { event_id: String, kind: String },
    #[error("unsupported or incomplete config operation {0}")]
    UnsupportedConfig(String),
    #[error("operation {operation_id} broke conservation identity {identity}")]
    ConservationViolation {
        operation_id: String,
        identity: String,
    },
    #[error("position {position} cannot be initialized without a canonical actor")]
    MissingPositionOwner { position: Pubkey },
    #[error("canonical position PDA {actual} does not match derived V1 PDA {expected}")]
    PositionPdaMismatch { expected: Pubkey, actual: Pubkey },
    #[error("native V1 position initialization failed: {0}")]
    PositionInitialization(String),
    #[error("assertions in operation {operation_id} are not in strict canonical order")]
    NonCanonicalAssertionOrder { operation_id: String },
}

pub struct V1FactualReplay {
    engine: V1NativeEngine,
    last_order: Option<ChainOrder>,
}

impl V1FactualReplay {
    /// Imports an exact decoded account checkpoint. This is the only supported
    /// way to begin in the middle of a market lifecycle.
    pub fn from_checkpoint(
        engine: V1NativeEngine,
        last_order: Option<ChainOrder>,
    ) -> Result<Self, V1FactualReplayError> {
        let checkpoint = engine.checkpoint();
        let identities = conservation_identities(&checkpoint)?;
        if let Some(identity) = identities.iter().find(|identity| !identity.holds) {
            return Err(V1FactualReplayError::ConservationViolation {
                operation_id: "checkpoint_import".to_owned(),
                identity: identity.name.clone(),
            });
        }
        Ok(Self { engine, last_order })
    }

    /// Imports the post-initialize accounts and proves they are the exact
    /// result of V1's native initialization transition.
    pub fn from_initialized_operation(
        operation: &CanonicalOperationV1,
        accounts: V1DecodedAccountsV1,
    ) -> Result<(Self, V1FactualOperationRecordV1), V1FactualReplayError> {
        validate_operation(operation, None)?;
        let engine = V1NativeEngine::from_decoded_accounts(accounts)?;
        ensure_pair_scope(&engine, operation)?;
        let assertion = initialization_input(operation)?;
        engine.assert_native_initialization(assertion)?;
        let checkpoint_after = engine.checkpoint();
        let receipt = V1AppliedReceiptV1::Initialize(assertion);
        let mut reconciliations = Vec::new();
        reconcile_payload(operation, &receipt, &checkpoint_after, &mut reconciliations)?;
        reconcile_assertions(
            operation,
            None,
            &checkpoint_after,
            &receipt,
            &mut reconciliations,
            false,
        )?;
        let conservation = conservation_identities(&checkpoint_after)?;
        ensure_conservation(operation, &conservation)?;
        let revenue_delta = V1RevenueDeltaV1::default();
        let record = V1FactualOperationRecordV1 {
            schema_version: FACTUAL_REPLAY_SCHEMA_VERSION,
            operation_id: operation.operation_id.clone(),
            order: operation.order,
            checkpoint_before: None,
            checkpoint_after,
            reconciliations,
            conservation,
            revenue_delta,
            corrections: Vec::new(),
            retained_event_limitations: Vec::new(),
            behavior: None,
        };
        Ok((
            Self {
                engine,
                last_order: Some(operation.order),
            },
            record,
        ))
    }

    /// Snapshot-free genesis import. `input` is the typed join of decoded
    /// initialize arguments, the PairCreated assertion, and active global
    /// configuration epochs. Duplicated canonical values are reconciled before
    /// the reconstructed state is accepted.
    pub fn from_canonical_initialization(
        operation: &CanonicalOperationV1,
        input: V1CanonicalInitializationInputV1,
    ) -> Result<(Self, V1FactualOperationRecordV1), V1FactualReplayError> {
        validate_operation(operation, None)?;
        validate_canonical_initialization_join(operation, &input)?;
        let engine = V1NativeEngine::from_canonical_initialization(input)?;
        ensure_pair_scope(&engine, operation)?;
        let assertion = initialization_input(operation)?;
        engine.assert_native_initialization(assertion)?;
        let checkpoint_after = engine.checkpoint();
        let receipt = V1AppliedReceiptV1::Initialize(assertion);
        let decision_grade_limitations = engine.post_state_correction_policy()
            == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let mut reconciliations = Vec::new();
        reconcile_payload(operation, &receipt, &checkpoint_after, &mut reconciliations)?;
        reconcile_assertions(
            operation,
            None,
            &checkpoint_after,
            &receipt,
            &mut reconciliations,
            decision_grade_limitations,
        )?;
        let conservation = conservation_identities(&checkpoint_after)?;
        ensure_conservation(operation, &conservation)?;
        let record = V1FactualOperationRecordV1 {
            schema_version: FACTUAL_REPLAY_SCHEMA_VERSION,
            operation_id: operation.operation_id.clone(),
            order: operation.order,
            checkpoint_before: None,
            checkpoint_after,
            reconciliations,
            conservation,
            revenue_delta: V1RevenueDeltaV1::default(),
            corrections: Vec::new(),
            retained_event_limitations: retained_event_limitations(
                operation,
                decision_grade_limitations,
                None,
            ),
            behavior: None,
        };
        Ok((
            Self {
                engine,
                last_order: Some(operation.order),
            },
            record,
        ))
    }

    pub const fn engine(&self) -> &V1NativeEngine {
        &self.engine
    }

    pub const fn last_order(&self) -> Option<ChainOrder> {
        self.last_order
    }

    /// Applies and reconciles one operation atomically. Any native error,
    /// assertion mismatch, or identity failure leaves both state and order
    /// unchanged.
    pub fn apply_operation(
        &mut self,
        operation: &CanonicalOperationV1,
    ) -> Result<V1FactualOperationRecordV1, V1FactualReplayError> {
        validate_operation(operation, self.last_order)?;
        ensure_pair_scope(&self.engine, operation)?;
        if matches!(
            operation.payload,
            OperationPayloadV1::InitializeMarket { .. }
        ) {
            return Err(V1FactualReplayError::UnsupportedConfig(
                "initialize_market is only accepted by from_initialized_operation".to_owned(),
            ));
        }

        let checkpoint_before = self.engine.checkpoint();
        let mut staged = self.engine.try_fork();
        let receipt = apply_payload(&mut staged, operation)?;
        let checkpoint_after = staged.checkpoint();
        let decision_grade_limitations = staged.post_state_correction_policy()
            == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi;
        let mut reconciliations = Vec::new();
        reconcile_payload(operation, &receipt, &checkpoint_after, &mut reconciliations)?;
        reconcile_assertions(
            operation,
            Some(&checkpoint_before),
            &checkpoint_after,
            &receipt,
            &mut reconciliations,
            decision_grade_limitations,
        )?;
        let revenue_delta = revenue_delta(
            &checkpoint_before.revenue,
            &checkpoint_after.revenue,
            &receipt,
            operation,
        )?;
        reconcile_revenue_delta(&receipt, &revenue_delta, operation, &mut reconciliations)?;
        let conservation = conservation_identities(&checkpoint_after)?;
        ensure_conservation(operation, &conservation)?;
        let behavior = behavior_observation(&receipt, &staged)?;
        let corrections =
            decision_grade_correction_evidence(operation, &receipt, &checkpoint_after)?;

        self.engine = staged;
        self.last_order = Some(operation.order);
        Ok(V1FactualOperationRecordV1 {
            schema_version: FACTUAL_REPLAY_SCHEMA_VERSION,
            operation_id: operation.operation_id.clone(),
            order: operation.order,
            checkpoint_before: Some(checkpoint_before),
            checkpoint_after,
            reconciliations,
            conservation,
            revenue_delta,
            corrections,
            retained_event_limitations: retained_event_limitations(
                operation,
                decision_grade_limitations,
                Some(&receipt),
            ),
            behavior,
        })
    }
}

fn behavior_observation(
    receipt: &V1AppliedReceiptV1,
    engine: &V1NativeEngine,
) -> Result<Option<V1BehaviorObservationV1>, V1FactualReplayError> {
    let observation = match receipt {
        V1AppliedReceiptV1::Borrow(receipt) => {
            let capacity_after = engine.borrow_capacity(
                receipt.capacity_before.position,
                receipt.capacity_before.debt_token,
            )?;
            Some(V1BehaviorObservationV1::Borrow {
                capacity_before: receipt.capacity_before,
                capacity_fraction_numerator: factual_amount(capacity_after.actual_debt),
                capacity_fraction_denominator: factual_amount(capacity_after.admissible_max_debt),
                capacity_after,
            })
        }
        V1AppliedReceiptV1::Repay(receipt) => Some(V1BehaviorObservationV1::Repay {
            debt_before: factual_amount(receipt.debt_before),
            debt_after: factual_amount(receipt.debt_after),
            reduction_fraction_numerator: factual_amount(receipt.repaid_amount),
            reduction_fraction_denominator: factual_amount(receipt.debt_before),
        }),
        _ => None,
    };
    Ok(observation)
}

fn factual_amount(value: u64) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u64 is a canonical token amount")
}

fn factual_amount_u128(value: u128) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u128 is a canonical token amount")
}

fn factual_signed_amount(value: i128) -> SignedTokenAmount {
    SignedTokenAmount::new(value.to_string()).expect("i128 is a canonical signed token amount")
}

fn validate_canonical_initialization_join(
    operation: &CanonicalOperationV1,
    input: &V1CanonicalInitializationInputV1,
) -> Result<(), V1FactualReplayError> {
    let OperationPayloadV1::InitializeMarket {
        min_liquidity_out,
        fields,
        ..
    } = &operation.payload
    else {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "canonical genesis input requires initialize_market".to_owned(),
        ));
    };
    let pair_created: Vec<_> = operation
        .assertions
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayloadV1::Config(config)
                if config.get("event_kind").map(String::as_str) == Some("pair_created") =>
            {
                Some((event, config))
            }
            _ => None,
        })
        .collect();
    if pair_created.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec!["unique_direct_child_pair_created_assertion".to_owned()],
        });
    }
    // Membership in `operation.assertions` is produced only by the
    // provenance normalizer's verified-direct-parent join. PairCreatedEvent
    // emits the initialize rate-model account itself. Reconcile its exact key
    // against both the operation field and the typed replay input before the
    // historical explicit-account policy may bypass newer PDA derivation.
    let (_, pair_created_fields) = pair_created[0];
    let asserted_rate_model = required_field(pair_created_fields, "rate_model")?;
    let operation_rate_model = required_field(fields, "rate_model")?;
    let input_rate_model = input.rate_model_key.to_string();
    if asserted_rate_model != operation_rate_model || asserted_rate_model != input_rate_model {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: operation.operation_id.clone(),
            field: "initialize.rate_model direct-child join".to_owned(),
            expected: asserted_rate_model.to_owned(),
            actual: format!("operation={operation_rate_model},input={input_rate_model}"),
        });
    }
    let operation_min = parse_u64("initialize.min_liquidity_out", min_liquidity_out)?;
    if operation_min != input.min_liquidity_out {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: operation.operation_id.clone(),
            field: "initialize.min_liquidity_out join".to_owned(),
            expected: operation_min.to_string(),
            actual: input.min_liquidity_out.to_string(),
        });
    }
    let initial_rate_raw = fields
        .get("instruction.args.initial_rate_bps")
        .ok_or_else(|| V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec!["instruction.args.initial_rate_bps".to_owned()],
        })?;
    let initial_rate = if initial_rate_raw == "null" {
        omnipair::constants::DEFAULT_INITIAL_RATE_BPS
    } else {
        initial_rate_raw
            .parse::<u64>()
            .map_err(|_| V1FactualReplayError::InvalidAmount {
                field: "instruction.args.initial_rate_bps".to_owned(),
                value: initial_rate_raw.clone(),
            })?
    };
    if initial_rate != input.initial_rate_bps {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: operation.operation_id.clone(),
            field: "initialize.initial_rate_bps join".to_owned(),
            expected: initial_rate.to_string(),
            actual: input.initial_rate_bps.to_string(),
        });
    }
    let version = fields
        .get("instruction.args.version")
        .ok_or_else(|| V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec!["instruction.args.version".to_owned()],
        })?
        .parse::<u8>()
        .map_err(|_| V1FactualReplayError::InvalidAmount {
            field: "instruction.args.version".to_owned(),
            value: fields["instruction.args.version"].clone(),
        })?;
    if version != input.pair_account_version {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: operation.operation_id.clone(),
            field: "initialize.version join".to_owned(),
            expected: version.to_string(),
            actual: input.pair_account_version.to_string(),
        });
    }
    Ok(())
}

fn validate_operation(
    operation: &CanonicalOperationV1,
    last_order: Option<ChainOrder>,
) -> Result<(), V1FactualReplayError> {
    operation.validate()?;
    if last_order.is_some_and(|last| operation.order <= last) {
        return Err(V1FactualReplayError::NonCanonicalOrder {
            operation_id: operation.operation_id.clone(),
        });
    }
    if operation
        .assertions
        .windows(2)
        .any(|events| events[0].order >= events[1].order)
        || operation
            .assertions
            .iter()
            .any(|event| event.order < operation.order)
    {
        return Err(V1FactualReplayError::NonCanonicalAssertionOrder {
            operation_id: operation.operation_id.clone(),
        });
    }
    Ok(())
}

fn ensure_pair_scope(
    engine: &V1NativeEngine,
    operation: &CanonicalOperationV1,
) -> Result<(), V1FactualReplayError> {
    let is_global_config = matches!(
        &operation.payload,
        OperationPayloadV1::UpdateConfig { fields }
            if fields.get("instruction_kind").is_some_and(|kind| kind != "set_pair_reduce_only")
    );
    if !is_global_config && operation.pair != engine.pair_key().to_string() {
        return Err(V1FactualReplayError::PairMismatch {
            operation_pair: operation.pair.clone(),
            engine_pair: engine.pair_key(),
        });
    }
    Ok(())
}

fn initialization_input(
    operation: &CanonicalOperationV1,
) -> Result<V1InitializationAssertionV1, V1FactualReplayError> {
    let OperationPayloadV1::InitializeMarket {
        token0,
        token1,
        gross_amount0,
        gross_amount1,
        vault_credit0,
        vault_credit1,
        minted_lp,
        ..
    } = &operation.payload
    else {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "initial checkpoint requires initialize_market".to_owned(),
        ));
    };
    Ok(V1InitializationAssertionV1 {
        slot: operation.order.slot,
        token0: parse_pubkey("initialize.token0", token0)?,
        token1: parse_pubkey("initialize.token1", token1)?,
        gross_amount0: parse_u64("initialize.gross_amount0", gross_amount0)?,
        gross_amount1: parse_u64("initialize.gross_amount1", gross_amount1)?,
        vault_credit0: parse_u64("initialize.vault_credit0", vault_credit0)?,
        vault_credit1: parse_u64("initialize.vault_credit1", vault_credit1)?,
        minted_lp: parse_u64("initialize.minted_lp", minted_lp)?,
    })
}

fn apply_payload(
    engine: &mut V1NativeEngine,
    operation: &CanonicalOperationV1,
) -> Result<V1AppliedReceiptV1, V1FactualReplayError> {
    let slot = operation.order.slot;
    match &operation.payload {
        OperationPayloadV1::InitializeMarket { .. } => Err(
            V1FactualReplayError::UnsupportedConfig("duplicate initialize_market".to_owned()),
        ),
        OperationPayloadV1::AddLiquidity {
            gross_amount0,
            gross_amount1,
            vault_credit0,
            vault_credit1,
            min_liquidity_out,
            ..
        } => {
            let input = V1AddLiquidityInputV1 {
                slot,
                amount0_in: parse_u64("add_liquidity.gross_amount0", gross_amount0)?,
                amount1_in: parse_u64("add_liquidity.gross_amount1", gross_amount1)?,
                vault_credit0: parse_u64("add_liquidity.vault_credit0", vault_credit0)?,
                vault_credit1: parse_u64("add_liquidity.vault_credit1", vault_credit1)?,
                min_liquidity_out: parse_u64("add_liquidity.min_liquidity_out", min_liquidity_out)?,
            };
            let receipt = if engine.post_state_correction_policy()
                == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
            {
                let (witness, _) = unique_lp_mint_supply_witness(operation)?;
                engine.apply_event_and_cpi_witnessed_add_liquidity(input, witness)?
            } else {
                engine.apply_add_liquidity(input)?
            };
            Ok(V1AppliedReceiptV1::AddLiquidity(receipt))
        }
        OperationPayloadV1::RemoveLiquidity {
            requested_lp,
            vault_debit0,
            vault_debit1,
            recipient_credit0,
            recipient_credit1,
            min_amount0_out,
            min_amount1_out,
            ..
        } => {
            let input = V1RemoveLiquidityInputV1 {
                slot,
                liquidity_burned: parse_u64("remove_liquidity.requested_lp", requested_lp)?,
                vault_debit0: parse_u64("remove_liquidity.vault_debit0", vault_debit0)?,
                vault_debit1: parse_u64("remove_liquidity.vault_debit1", vault_debit1)?,
                recipient_credit0: parse_u64(
                    "remove_liquidity.recipient_credit0",
                    recipient_credit0,
                )?,
                recipient_credit1: parse_u64(
                    "remove_liquidity.recipient_credit1",
                    recipient_credit1,
                )?,
                min_amount0_out: parse_u64("remove_liquidity.min_amount0_out", min_amount0_out)?,
                min_amount1_out: parse_u64("remove_liquidity.min_amount1_out", min_amount1_out)?,
            };
            let receipt = if engine.post_state_correction_policy()
                == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
            {
                let (witness, _, _) = unique_lp_burn_cash_witness(operation)?;
                engine.apply_event_and_cpi_witnessed_remove_liquidity(input, witness)?
            } else {
                engine.apply_remove_liquidity(input)?
            };
            Ok(V1AppliedReceiptV1::RemoveLiquidity(receipt))
        }
        OperationPayloadV1::SwapExactIn {
            token0_in,
            gross_amount_in,
            vault_credit_in,
            min_amount_out,
            vault_debit_out,
            recipient_credit_out,
        } => {
            let input = V1SwapInputV1 {
                slot,
                token_in: side(*token0_in),
                gross_amount_in: parse_u64("swap.gross_amount_in", gross_amount_in)?,
                min_amount_out: parse_u64("swap.min_amount_out", min_amount_out)?,
                vault_credit_in: parse_u64("swap.vault_credit_in", vault_credit_in)?,
                vault_debit_out: parse_u64("swap.vault_debit_out", vault_debit_out)?,
                recipient_credit_out: parse_u64("swap.recipient_credit_out", recipient_credit_out)?,
            };
            let receipt = if engine.post_state_correction_policy()
                == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
            {
                let (witness, _) = unique_swap_transition_witness(operation)?;
                engine.apply_event_and_cpi_witnessed_swap(input, witness)?
            } else {
                engine.apply_swap(input)?
            };
            Ok(V1AppliedReceiptV1::Swap(receipt))
        }
        OperationPayloadV1::CollateralDelta {
            position,
            amount0,
            amount1,
            requested_amount,
            gross_transfer_amount,
            vault_amount,
            counterparty_amount,
        } => {
            let position = parse_pubkey("collateral.position", position)?;
            let (collateral_token, deposit, delta_amount) = collateral_delta(amount0, amount1)?;
            let gross = parse_u64("collateral.gross_transfer_amount", gross_transfer_amount)?;
            if delta_amount != gross {
                return Err(V1FactualReplayError::AssertionMismatch {
                    event_id: operation.operation_id.clone(),
                    field: "collateral signed delta versus instruction amount".to_owned(),
                    expected: gross.to_string(),
                    actual: delta_amount.to_string(),
                });
            }
            let factor_witness = if deposit
                && engine.collateral_factor_transition_policy()
                    == V1CollateralFactorTransitionPolicyV1::EventWitnessedCollateralFactorTransition
            {
                Some(unique_position_factor_witness(operation, position)?)
            } else {
                None
            };
            let withdrawal_witness = if !deposit
                && engine.post_state_correction_policy()
                    == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
            {
                Some(unique_collateral_withdrawal_witness(
                    operation,
                    position,
                    collateral_token,
                    gross,
                )?)
            } else {
                None
            };
            ensure_position(engine, operation, position, deposit)?;
            let input = V1CollateralInputV1 {
                slot,
                position,
                collateral_token,
                amount: gross,
                requested_amount: parse_u64("collateral.requested_amount", requested_amount)?,
                deposit,
                collateral_vault_amount: parse_u64("collateral.vault_amount", vault_amount)?,
                counterparty_amount: parse_u64(
                    "collateral.counterparty_amount",
                    counterparty_amount,
                )?,
            };
            let receipt = match (factor_witness, withdrawal_witness) {
                (Some(witness), None) => engine.apply_event_witnessed_collateral(input, witness)?,
                (None, Some(witness)) => {
                    engine.apply_event_and_cpi_witnessed_collateral_withdrawal(input, witness)?
                }
                (None, None) => engine.apply_collateral(input)?,
                (Some(_), Some(_)) => {
                    return Err(V1FactualReplayError::UnsupportedConfig(
                        "collateral operation selected conflicting witnesses".to_owned(),
                    ));
                }
            };
            Ok(V1AppliedReceiptV1::Collateral(receipt))
        }
        OperationPayloadV1::Borrow {
            position,
            debt_token0,
            requested_amount,
            gross_amount: _,
            recipient_credit,
        } => {
            let input = V1BorrowInputV1 {
                slot,
                position: parse_pubkey("borrow.position", position)?,
                debt_token: side(*debt_token0),
                // The current canonical schema contains the emitted actual
                // amount. It is state-exact, while the original sentinel is a
                // separately reported extraction requirement.
                requested_amount: parse_u64("borrow.requested_amount", requested_amount)?,
                recipient_credit: parse_u64("borrow.recipient_credit", recipient_credit)?,
            };
            let receipt = if engine.post_state_correction_policy()
                == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
            {
                let (witness, _, _) = unique_borrow_transition_witness(operation)?;
                engine.apply_event_and_cpi_witnessed_borrow(input, witness)?
            } else {
                engine.apply_borrow(input)?
            };
            Ok(V1AppliedReceiptV1::Borrow(receipt))
        }
        OperationPayloadV1::Repay {
            position,
            debt_token0,
            requested_amount,
            gross_amount: _,
            vault_credit,
        } => {
            let input = V1RepayInputV1 {
                slot,
                position: parse_pubkey("repay.position", position)?,
                debt_token: side(*debt_token0),
                requested_amount: parse_u64("repay.requested_amount", requested_amount)?,
                vault_credit: parse_u64("repay.vault_credit", vault_credit)?,
            };
            let receipt = if engine.post_state_correction_policy()
                == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
            {
                let (witness, _, _) = unique_repay_transition_witness(operation)?;
                engine.apply_event_and_cpi_witnessed_repay(input, witness)?
            } else {
                engine.apply_repay(input)?
            };
            Ok(V1AppliedReceiptV1::Repay(receipt))
        }
        OperationPayloadV1::Liquidate {
            position,
            debt_token0,
            reserve_vault_credit,
            liquidator_recipient_credit,
            ..
        } => {
            let input = V1LiquidationInputV1 {
                slot,
                position: parse_pubkey("liquidation.position", position)?,
                debt_token: side(*debt_token0),
                reserve_vault_credit: parse_u64(
                    "liquidation.reserve_vault_credit",
                    reserve_vault_credit,
                )?,
                liquidator_recipient_credit: parse_u64(
                    "liquidation.liquidator_recipient_credit",
                    liquidator_recipient_credit,
                )?,
            };
            let receipt = if engine.post_state_correction_policy()
                == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
            {
                let (witness, _) = unique_liquidation_transition_witness(operation)?;
                engine.apply_event_and_cpi_witnessed_liquidation(input, witness)?
            } else {
                engine.apply_liquidation(input)?
            };
            Ok(V1AppliedReceiptV1::Liquidation(receipt))
        }
        OperationPayloadV1::Flashloan {
            amount0,
            amount1,
            net_vault_credit0,
            net_vault_credit1,
            ..
        } => Ok(V1AppliedReceiptV1::Flashloan(engine.apply_flashloan(
            V1FlashloanInputV1 {
                slot,
                amount0: parse_u64("flashloan.amount0", amount0)?,
                amount1: parse_u64("flashloan.amount1", amount1)?,
                net_vault_credit0: parse_u64("flashloan.net_vault_credit0", net_vault_credit0)?,
                net_vault_credit1: parse_u64("flashloan.net_vault_credit1", net_vault_credit1)?,
            },
        )?)),
        OperationPayloadV1::ClaimProtocolRevenue { amount0, amount1 } => {
            let input = V1ProtocolClaimInputV1 {
                slot,
                amount0: parse_u64("claim.amount0", amount0)?,
                amount1: parse_u64("claim.amount1", amount1)?,
            };
            let receipt = if engine.post_state_correction_policy()
                == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
            {
                engine.apply_cpi_witnessed_protocol_claim(input)?
            } else {
                engine.apply_protocol_claim(input)?
            };
            Ok(V1AppliedReceiptV1::ProtocolClaim(receipt))
        }
        OperationPayloadV1::UpdateConfig { fields } => {
            apply_config(engine, fields)?;
            Ok(V1AppliedReceiptV1::Config)
        }
        OperationPayloadV1::ClockOnly => {
            if engine.post_state_correction_policy()
                == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
                && operation
                    .assertions
                    .iter()
                    .any(|event| matches!(&event.payload, EventPayloadV1::Liquidation(_)))
            {
                let input = decision_grade_clock_zero_liquidation_input(operation, engine)?;
                Ok(V1AppliedReceiptV1::Liquidation(
                    engine.apply_liquidation(input)?,
                ))
            } else {
                Ok(V1AppliedReceiptV1::Clock(engine.advance_to_slot(slot)?))
            }
        }
    }
}

fn unique_swap_transition_witness(
    operation: &CanonicalOperationV1,
) -> Result<(V1SwapTransitionWitnessV1, &CanonicalEventV1), V1FactualReplayError> {
    let OperationPayloadV1::SwapExactIn {
        token0_in,
        gross_amount_in,
        vault_credit_in,
        vault_debit_out,
        recipient_credit_out,
        ..
    } = &operation.payload
    else {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "swap transition witness requires swap_exact_in".to_owned(),
        ));
    };
    let swap_assertions: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| matches!(&event.payload, EventPayloadV1::Swap(_)))
        .collect();
    if swap_assertions.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec!["unique_direct_child_swap_assertion".to_owned()],
        });
    }
    // Normalization admits only provenance-verified direct-child events into
    // `assertions`; requiring exactly one removes event-selection ambiguity.
    let event = swap_assertions[0];
    let EventPayloadV1::Swap(payload) = &event.payload else {
        unreachable!("filtered to swap assertions")
    };
    let event_token_in = side(payload.token0_in);
    let event_gross_amount_in = parse_u64("swap_assertion.amount_in", &payload.amount_in)?;
    let event_amount_in_after_fee = parse_u64(
        "swap_assertion.amount_in_after_fee",
        &payload.amount_in_after_fee,
    )?;
    let event_amount_out = parse_u64("swap_assertion.amount_out", &payload.amount_out)?;
    let event_lp_fee = parse_u64("swap_assertion.lp_fee", &payload.lp_fee)?;
    let event_protocol_fee = parse_u64("swap_assertion.protocol_fee", &payload.protocol_fee)?;
    let event_reserve0_after = parse_u64("swap_assertion.reserve0_after", &payload.reserve0_after)?;
    let event_reserve1_after = parse_u64("swap_assertion.reserve1_after", &payload.reserve1_after)?;
    let canonical_token_in = side(*token0_in);
    let canonical_gross_amount_in = parse_u64("swap.gross_amount_in", gross_amount_in)?;
    let token_cpi_vault_credit_in = parse_u64("swap.vault_credit_in", vault_credit_in)?;
    let token_cpi_vault_debit_out = parse_u64("swap.vault_debit_out", vault_debit_out)?;
    let token_cpi_recipient_credit_out =
        parse_u64("swap.recipient_credit_out", recipient_credit_out)?;
    if event_token_in != canonical_token_in
        || event_gross_amount_in != canonical_gross_amount_in
        || event_amount_out != token_cpi_vault_debit_out
        || event_amount_out != token_cpi_recipient_credit_out
    {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: event.event_id.clone(),
            field: "swap_event versus canonical instruction/token_cpi tuple".to_owned(),
            expected: format!(
                "side={canonical_token_in:?},gross={canonical_gross_amount_in},out={token_cpi_vault_debit_out},recipient={token_cpi_recipient_credit_out}"
            ),
            actual: format!(
                "side={event_token_in:?},gross={event_gross_amount_in},out={event_amount_out}"
            ),
        });
    }
    Ok((
        V1SwapTransitionWitnessV1 {
            event_token_in,
            event_gross_amount_in,
            event_amount_in_after_fee,
            event_amount_out,
            event_lp_fee,
            event_protocol_fee,
            event_reserve0_after,
            event_reserve1_after,
            token_cpi_vault_credit_in,
            token_cpi_vault_debit_out,
            token_cpi_recipient_credit_out,
        },
        event,
    ))
}

fn unique_borrow_transition_witness(
    operation: &CanonicalOperationV1,
) -> Result<
    (
        V1BorrowTransitionWitnessV1,
        &CanonicalEventV1,
        &CanonicalEventV1,
    ),
    V1FactualReplayError,
> {
    let OperationPayloadV1::Borrow {
        position,
        debt_token0,
        requested_amount,
        gross_amount,
        recipient_credit,
    } = &operation.payload
    else {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "borrow transition witness requires borrow".to_owned(),
        ));
    };
    let debt_events: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| matches!(&event.payload, EventPayloadV1::DebtDelta(_)))
        .collect();
    let position_events: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| matches!(&event.payload, EventPayloadV1::Position(_)))
        .collect();
    if debt_events.len() != 1 || position_events.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec![
                "unique_direct_child_debt_delta_assertion".to_owned(),
                "unique_direct_child_position_assertion".to_owned(),
            ],
        });
    }
    let debt_event = debt_events[0];
    let position_event = position_events[0];
    let EventPayloadV1::DebtDelta(debt) = &debt_event.payload else {
        unreachable!("filtered to debt delta")
    };
    let amount0 = parse_i128("borrow_debt_delta.amount0", &debt.amount0)?;
    let amount1 = parse_i128("borrow_debt_delta.amount1", &debt.amount1)?;
    let (event_debt_token, event_borrowed_amount) = if amount0 > 0 && amount1 == 0 {
        (
            V1TokenSideV1::Token0,
            u64::try_from(amount0).map_err(|_| V1FactualReplayError::InvalidAmount {
                field: "borrow_debt_delta.amount0".to_owned(),
                value: amount0.to_string(),
            })?,
        )
    } else if amount1 > 0 && amount0 == 0 {
        (
            V1TokenSideV1::Token1,
            u64::try_from(amount1).map_err(|_| V1FactualReplayError::InvalidAmount {
                field: "borrow_debt_delta.amount1".to_owned(),
                value: amount1.to_string(),
            })?,
        )
    } else {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: debt_event.event_id.clone(),
            field: "borrow_debt_delta positive single side".to_owned(),
            expected: "exactly one positive debt delta".to_owned(),
            actual: format!("amount0={amount0},amount1={amount1}"),
        });
    };
    let canonical_position = parse_pubkey("borrow.position", position)?;
    let canonical_debt_token = side(*debt_token0);
    let canonical_requested = parse_u64("borrow.requested_amount", requested_amount)?;
    let canonical_gross = parse_u64("borrow.gross_amount", gross_amount)?;
    let token_cpi_recipient_credit = parse_u64("borrow.recipient_credit", recipient_credit)?;
    // Normalization binds `gross_amount` to both DebtDelta and the reserve
    // vault debit CPI; retain the value under its CPI role here.
    let token_cpi_vault_debit = canonical_gross;
    if event_debt_token != canonical_debt_token
        || event_borrowed_amount != canonical_gross
        || token_cpi_recipient_credit != canonical_gross
        || (canonical_requested != u64::MAX && canonical_requested != canonical_gross)
    {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: debt_event.event_id.clone(),
            field: "borrow DebtDelta/instruction/token_cpi tuple".to_owned(),
            expected: format!(
                "side={canonical_debt_token:?},requested={canonical_requested},gross={canonical_gross},recipient={token_cpi_recipient_credit}"
            ),
            actual: format!(
                "side={event_debt_token:?},amount={event_borrowed_amount}"
            ),
        });
    }
    let EventPayloadV1::Position(position_payload) = &position_event.payload else {
        unreachable!("filtered to position")
    };
    let event_position = parse_pubkey(
        "borrow_position_assertion.position",
        &position_payload.position,
    )?;
    if event_position != canonical_position {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: position_event.event_id.clone(),
            field: "borrow_position_assertion.position".to_owned(),
            expected: canonical_position.to_string(),
            actual: event_position.to_string(),
        });
    }
    Ok((
        V1BorrowTransitionWitnessV1 {
            event_debt_token,
            event_borrowed_amount,
            token_cpi_vault_debit,
            token_cpi_recipient_credit,
            event_position,
            event_collateral0: parse_u64(
                "borrow_position_assertion.collateral0",
                &position_payload.collateral0,
            )?,
            event_collateral1: parse_u64(
                "borrow_position_assertion.collateral1",
                &position_payload.collateral1,
            )?,
            event_debt0_shares: parse_u128(
                "borrow_position_assertion.debt0_shares",
                &position_payload.debt0_shares,
            )?,
            event_debt1_shares: parse_u128(
                "borrow_position_assertion.debt1_shares",
                &position_payload.debt1_shares,
            )?,
            event_collateral0_max_cf_bps: position_payload.collateral0_max_cf_bps,
            event_collateral1_max_cf_bps: position_payload.collateral1_max_cf_bps,
            event_collateral0_liquidation_cf_bps: position_payload.collateral0_liquidation_cf_bps,
            event_collateral1_liquidation_cf_bps: position_payload.collateral1_liquidation_cf_bps,
        },
        debt_event,
        position_event,
    ))
}

fn unique_repay_transition_witness(
    operation: &CanonicalOperationV1,
) -> Result<
    (
        V1RepayTransitionWitnessV1,
        &CanonicalEventV1,
        &CanonicalEventV1,
    ),
    V1FactualReplayError,
> {
    let OperationPayloadV1::Repay {
        position,
        debt_token0,
        requested_amount,
        gross_amount,
        vault_credit,
    } = &operation.payload
    else {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "repay transition witness requires repay".to_owned(),
        ));
    };
    let debt_events: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| matches!(&event.payload, EventPayloadV1::DebtDelta(_)))
        .collect();
    let position_events: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| matches!(&event.payload, EventPayloadV1::Position(_)))
        .collect();
    if debt_events.len() != 1 || position_events.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec![
                "unique_direct_child_debt_delta_assertion".to_owned(),
                "unique_direct_child_position_assertion".to_owned(),
            ],
        });
    }
    let debt_event = debt_events[0];
    let position_event = position_events[0];
    let EventPayloadV1::DebtDelta(debt) = &debt_event.payload else {
        unreachable!("filtered to debt delta")
    };
    let amount0 = parse_i128("repay_debt_delta.amount0", &debt.amount0)?;
    let amount1 = parse_i128("repay_debt_delta.amount1", &debt.amount1)?;
    let (event_debt_token, event_repaid_u128) = if amount0 < 0 && amount1 == 0 {
        (V1TokenSideV1::Token0, amount0.unsigned_abs())
    } else if amount1 < 0 && amount0 == 0 {
        (V1TokenSideV1::Token1, amount1.unsigned_abs())
    } else {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: debt_event.event_id.clone(),
            field: "repay_debt_delta negative single side".to_owned(),
            expected: "exactly one negative debt delta".to_owned(),
            actual: format!("amount0={amount0},amount1={amount1}"),
        });
    };
    let event_repaid_amount =
        u64::try_from(event_repaid_u128).map_err(|_| V1FactualReplayError::InvalidAmount {
            field: "repay_debt_delta.amount".to_owned(),
            value: event_repaid_u128.to_string(),
        })?;
    let canonical_position = parse_pubkey("repay.position", position)?;
    let canonical_debt_token = side(*debt_token0);
    let canonical_requested = parse_u64("repay.requested_amount", requested_amount)?;
    let canonical_gross = parse_u64("repay.gross_amount", gross_amount)?;
    let token_cpi_vault_credit = parse_u64("repay.vault_credit", vault_credit)?;
    if event_debt_token != canonical_debt_token
        || event_repaid_amount != canonical_gross
        || token_cpi_vault_credit != canonical_gross
        || (canonical_requested != u64::MAX && canonical_requested != canonical_gross)
    {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: debt_event.event_id.clone(),
            field: "repay DebtDelta/instruction/token_cpi tuple".to_owned(),
            expected: format!(
                "side={canonical_debt_token:?},requested={canonical_requested},gross={canonical_gross},vault={token_cpi_vault_credit}"
            ),
            actual: format!(
                "side={event_debt_token:?},amount={event_repaid_amount}"
            ),
        });
    }
    let EventPayloadV1::Position(position_payload) = &position_event.payload else {
        unreachable!("filtered to position")
    };
    let event_position = parse_pubkey(
        "repay_position_assertion.position",
        &position_payload.position,
    )?;
    if event_position != canonical_position {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: position_event.event_id.clone(),
            field: "repay_position_assertion.position".to_owned(),
            expected: canonical_position.to_string(),
            actual: event_position.to_string(),
        });
    }
    Ok((
        V1RepayTransitionWitnessV1 {
            event_debt_token,
            event_repaid_amount,
            token_cpi_vault_credit,
            event_position,
            event_collateral0: parse_u64(
                "repay_position_assertion.collateral0",
                &position_payload.collateral0,
            )?,
            event_collateral1: parse_u64(
                "repay_position_assertion.collateral1",
                &position_payload.collateral1,
            )?,
            event_debt0_shares: parse_u128(
                "repay_position_assertion.debt0_shares",
                &position_payload.debt0_shares,
            )?,
            event_debt1_shares: parse_u128(
                "repay_position_assertion.debt1_shares",
                &position_payload.debt1_shares,
            )?,
            event_collateral0_max_cf_bps: position_payload.collateral0_max_cf_bps,
            event_collateral1_max_cf_bps: position_payload.collateral1_max_cf_bps,
            event_collateral0_liquidation_cf_bps: position_payload.collateral0_liquidation_cf_bps,
            event_collateral1_liquidation_cf_bps: position_payload.collateral1_liquidation_cf_bps,
        },
        debt_event,
        position_event,
    ))
}

fn unique_liquidation_transition_witness(
    operation: &CanonicalOperationV1,
) -> Result<(V1LiquidationTransitionWitnessV1, &CanonicalEventV1), V1FactualReplayError> {
    let OperationPayloadV1::Liquidate {
        position,
        debt_token0,
        requested_repay,
        collateral_vault_debit,
        reserve_vault_credit,
        liquidator_recipient_credit,
    } = &operation.payload
    else {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "liquidation transition witness requires liquidate".to_owned(),
        ));
    };
    let events: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| matches!(&event.payload, EventPayloadV1::Liquidation(_)))
        .collect();
    if events.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec!["unique_direct_child_liquidation_assertion".to_owned()],
        });
    }
    let event = events[0];
    let EventPayloadV1::Liquidation(payload) = &event.payload else {
        unreachable!("filtered to liquidation event")
    };
    let canonical_position = parse_pubkey("liquidation.position", position)?;
    let event_position = parse_pubkey("liquidation_event.position", &payload.position)?;
    let canonical_debt_token = side(*debt_token0);
    let collateral0 = parse_u64(
        "liquidation_event.collateral0_liquidated",
        &payload.collateral0_liquidated,
    )?;
    let collateral1 = parse_u64(
        "liquidation_event.collateral1_liquidated",
        &payload.collateral1_liquidated,
    )?;
    let debt0 = parse_u64(
        "liquidation_event.debt0_liquidated",
        &payload.debt0_liquidated,
    )?;
    let debt1 = parse_u64(
        "liquidation_event.debt1_liquidated",
        &payload.debt1_liquidated,
    )?;
    let (event_debt_written_off, event_collateral_seized) = match canonical_debt_token {
        V1TokenSideV1::Token0 if debt0 != 0 && debt1 == 0 && collateral0 == 0 => {
            (debt0, collateral1)
        }
        V1TokenSideV1::Token1 if debt1 != 0 && debt0 == 0 && collateral1 == 0 => {
            (debt1, collateral0)
        }
        _ => {
            return Err(V1FactualReplayError::AssertionMismatch {
                event_id: event.event_id.clone(),
                field: "liquidation_event token sides".to_owned(),
                expected: format!("debt={canonical_debt_token:?}, collateral=opposite"),
                actual: format!(
                    "collateral0={collateral0},collateral1={collateral1},debt0={debt0},debt1={debt1}"
                ),
            });
        }
    };
    let canonical_requested = parse_u64("liquidation.requested_repay", requested_repay)?;
    let token_cpi_collateral_vault_debit =
        parse_u64("liquidation.collateral_vault_debit", collateral_vault_debit)?;
    let token_cpi_reserve_vault_credit =
        parse_u64("liquidation.reserve_vault_credit", reserve_vault_credit)?;
    let token_cpi_liquidator_recipient_credit = parse_u64(
        "liquidation.liquidator_recipient_credit",
        liquidator_recipient_credit,
    )?;
    let event_liquidator_incentive = parse_u64(
        "liquidation_event.liquidation_bonus",
        &payload.liquidation_bonus,
    )?;
    if event_position != canonical_position
        || canonical_requested != event_debt_written_off
        || token_cpi_collateral_vault_debit != event_collateral_seized
        || token_cpi_liquidator_recipient_credit != event_liquidator_incentive
        || token_cpi_reserve_vault_credit.checked_add(token_cpi_liquidator_recipient_credit)
            != Some(event_collateral_seized)
    {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: event.event_id.clone(),
            field: "liquidation event/instruction/token_cpi tuple".to_owned(),
            expected: format!(
                "position={canonical_position},requested={canonical_requested},collateral_debit={token_cpi_collateral_vault_debit},reserve_credit={token_cpi_reserve_vault_credit},recipient={token_cpi_liquidator_recipient_credit}"
            ),
            actual: format!(
                "position={event_position},debt={event_debt_written_off},collateral={event_collateral_seized},bonus={event_liquidator_incentive}"
            ),
        });
    }
    Ok((
        V1LiquidationTransitionWitnessV1 {
            event_position,
            event_debt_token: canonical_debt_token,
            event_debt_written_off,
            event_collateral_seized,
            event_liquidator_incentive,
            event_shortfall: parse_u128("liquidation_event.shortfall", &payload.shortfall)?,
            token_cpi_collateral_vault_debit,
            token_cpi_reserve_vault_credit,
            token_cpi_liquidator_recipient_credit,
        },
        event,
    ))
}

fn unique_lp_mint_supply_witness(
    operation: &CanonicalOperationV1,
) -> Result<(V1LpMintSupplyWitnessV1, &CanonicalEventV1), V1FactualReplayError> {
    let OperationPayloadV1::AddLiquidity {
        minted_lp,
        vault_credit0,
        vault_credit1,
        ..
    } = &operation.payload
    else {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "LP mint supply witness requires add_liquidity".to_owned(),
        ));
    };
    let mint_assertions: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| {
            matches!(
                &event.payload,
                EventPayloadV1::Liquidity(payload) if payload.action == LiquidityAction::Mint
            )
        })
        .collect();
    if mint_assertions.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec!["unique_direct_child_mint_assertion".to_owned()],
        });
    }
    let event = mint_assertions[0];
    let EventPayloadV1::Liquidity(payload) = &event.payload else {
        unreachable!("filtered to liquidity assertions")
    };
    let event_liquidity_minted = parse_u64("mint_assertion.liquidity", &payload.liquidity)?;
    let event_amount0_used = parse_u64("mint_assertion.amount0", &payload.amount0)?;
    let event_amount1_used = parse_u64("mint_assertion.amount1", &payload.amount1)?;
    // `build_operation_payload` assigns this field from the same MintEvent
    // only after `normalize::require_supply_change` uniquely matches the
    // routed MintTo CPI's mint, destination, authority, and exact amount.
    let token_cpi_supply_delta = parse_u64("add_liquidity.minted_lp", minted_lp)?;
    let token_cpi_vault_credit0 = parse_u64("add_liquidity.vault_credit0", vault_credit0)?;
    let token_cpi_vault_credit1 = parse_u64("add_liquidity.vault_credit1", vault_credit1)?;
    let lp_cash_assertions: Vec<_> = operation
        .assertions
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayloadV1::LpPosition(payload)
                if payload.cash_reserve0.is_some() || payload.cash_reserve1.is_some() =>
            {
                Some((event, payload))
            }
            _ => None,
        })
        .collect();
    if lp_cash_assertions.len() > 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec!["unique_direct_child_lp_cash_assertion".to_owned()],
        });
    }
    let (event_cash_reserve0_after, event_cash_reserve1_after) =
        if let Some((_, payload)) = lp_cash_assertions.first() {
            (
                payload
                    .cash_reserve0
                    .as_ref()
                    .map(|value| parse_u64("lp_cash_assertion.cash_reserve0", value))
                    .transpose()?,
                payload
                    .cash_reserve1
                    .as_ref()
                    .map(|value| parse_u64("lp_cash_assertion.cash_reserve1", value))
                    .transpose()?,
            )
        } else {
            (None, None)
        };
    if event_liquidity_minted != token_cpi_supply_delta
        || event_amount0_used != token_cpi_vault_credit0
        || event_amount1_used != token_cpi_vault_credit1
    {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: event.event_id.clone(),
            field: "mint_event versus token_cpi_deltas".to_owned(),
            expected: format!(
                "amount0={event_amount0_used},amount1={event_amount1_used},liquidity={event_liquidity_minted}"
            ),
            actual: format!(
                "amount0={token_cpi_vault_credit0},amount1={token_cpi_vault_credit1},liquidity={token_cpi_supply_delta}"
            ),
        });
    }
    Ok((
        V1LpMintSupplyWitnessV1 {
            event_amount0_used,
            event_amount1_used,
            event_liquidity_minted,
            token_cpi_supply_delta,
            token_cpi_vault_credit0,
            token_cpi_vault_credit1,
            event_cash_reserve0_after,
            event_cash_reserve1_after,
        },
        event,
    ))
}

fn unique_lp_burn_cash_witness(
    operation: &CanonicalOperationV1,
) -> Result<
    (
        V1RemoveLiquidityCashWitnessV1,
        &CanonicalEventV1,
        &CanonicalEventV1,
    ),
    V1FactualReplayError,
> {
    let OperationPayloadV1::RemoveLiquidity {
        requested_lp,
        burned_lp,
        vault_debit0,
        vault_debit1,
        recipient_credit0,
        recipient_credit1,
        ..
    } = &operation.payload
    else {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "LP burn cash witness requires remove_liquidity".to_owned(),
        ));
    };
    let burn_assertions: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| {
            matches!(
                &event.payload,
                EventPayloadV1::Liquidity(payload) if payload.action == LiquidityAction::Burn
            )
        })
        .collect();
    let cash_assertions: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| {
            matches!(
                &event.payload,
                EventPayloadV1::LpPosition(payload)
                    if payload.cash_reserve0.is_some() && payload.cash_reserve1.is_some()
            )
        })
        .collect();
    if burn_assertions.len() != 1 || cash_assertions.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec![
                "unique_direct_child_burn_assertion".to_owned(),
                "unique_direct_child_lp_cash_assertion".to_owned(),
            ],
        });
    }
    let burn_event = burn_assertions[0];
    let cash_event = cash_assertions[0];
    let EventPayloadV1::Liquidity(burn) = &burn_event.payload else {
        unreachable!("filtered to burn liquidity assertions")
    };
    let EventPayloadV1::LpPosition(cash) = &cash_event.payload else {
        unreachable!("filtered to LP cash assertions")
    };
    let event_liquidity_burned = parse_u64("burn_assertion.liquidity", &burn.liquidity)?;
    let event_amount0_out = parse_u64("burn_assertion.amount0", &burn.amount0)?;
    let event_amount1_out = parse_u64("burn_assertion.amount1", &burn.amount1)?;
    let requested_lp = parse_u64("remove_liquidity.requested_lp", requested_lp)?;
    let token_cpi_supply_delta = parse_u64("remove_liquidity.burned_lp", burned_lp)?;
    let token_cpi_vault_debit0 = parse_u64("remove_liquidity.vault_debit0", vault_debit0)?;
    let token_cpi_vault_debit1 = parse_u64("remove_liquidity.vault_debit1", vault_debit1)?;
    let token_cpi_recipient_credit0 =
        parse_u64("remove_liquidity.recipient_credit0", recipient_credit0)?;
    let token_cpi_recipient_credit1 =
        parse_u64("remove_liquidity.recipient_credit1", recipient_credit1)?;
    let event_cash_reserve0_after = parse_u64(
        "lp_cash_assertion.cash_reserve0",
        cash.cash_reserve0
            .as_ref()
            .expect("filtered to complete LP cash assertion"),
    )?;
    let event_cash_reserve1_after = parse_u64(
        "lp_cash_assertion.cash_reserve1",
        cash.cash_reserve1
            .as_ref()
            .expect("filtered to complete LP cash assertion"),
    )?;
    if requested_lp != token_cpi_supply_delta
        || event_liquidity_burned != token_cpi_supply_delta
        || event_amount0_out != token_cpi_vault_debit0
        || event_amount1_out != token_cpi_vault_debit1
        || event_amount0_out != token_cpi_recipient_credit0
        || event_amount1_out != token_cpi_recipient_credit1
    {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: burn_event.event_id.clone(),
            field: "burn_event versus token_cpi_deltas".to_owned(),
            expected: format!(
                "amount0={event_amount0_out},amount1={event_amount1_out},liquidity={event_liquidity_burned}"
            ),
            actual: format!(
                "amount0={token_cpi_vault_debit0},amount1={token_cpi_vault_debit1},liquidity={token_cpi_supply_delta}"
            ),
        });
    }
    Ok((
        V1RemoveLiquidityCashWitnessV1 {
            event_liquidity_burned,
            event_amount0_out,
            event_amount1_out,
            event_cash_reserve0_after,
            event_cash_reserve1_after,
            token_cpi_vault_debit0,
            token_cpi_vault_debit1,
            token_cpi_recipient_credit0,
            token_cpi_recipient_credit1,
        },
        burn_event,
        cash_event,
    ))
}

fn decision_grade_correction_evidence(
    operation: &CanonicalOperationV1,
    receipt: &V1AppliedReceiptV1,
    after: &V1NativeCheckpointV1,
) -> Result<Vec<V1DecisionGradeCorrectionEvidenceV1>, V1FactualReplayError> {
    match receipt {
        V1AppliedReceiptV1::AddLiquidity(receipt) => {
            let (witness, event) = unique_lp_mint_supply_witness(operation)?;
            let mut evidence = Vec::new();
            if let Some(correction) = receipt.supply_correction {
                if correction.witnessed_liquidity_minted != witness.event_liquidity_minted
                    || correction.witnessed_amount0_used != witness.event_amount0_used
                    || correction.witnessed_amount1_used != witness.event_amount1_used
                    || receipt.amount0_used != witness.event_amount0_used
                    || receipt.amount1_used != witness.event_amount1_used
                    || correction.total_supply_after != after.pair.total_supply
                {
                    return Err(V1EngineError::CheckpointMismatch.into());
                }
                evidence.push(V1DecisionGradeCorrectionEvidenceV1::LpMintTotalSupply {
                    assertion_event_id: event.event_id.clone(),
                    assertion_raw_payload_sha256: event.raw_payload_sha256.clone(),
                    lp_mint: after.pair.lp_mint.to_string(),
                    source_native_amount0_used: factual_amount(
                        correction.source_native_amount0_used,
                    ),
                    source_native_amount1_used: factual_amount(
                        correction.source_native_amount1_used,
                    ),
                    source_native_minted_lp: factual_amount(
                        correction.source_native_liquidity_minted,
                    ),
                    event_amount0_used: factual_amount(witness.event_amount0_used),
                    event_amount1_used: factual_amount(witness.event_amount1_used),
                    event_minted_lp: factual_amount(witness.event_liquidity_minted),
                    token_cpi_vault_credit0: factual_amount(witness.token_cpi_vault_credit0),
                    token_cpi_vault_credit1: factual_amount(witness.token_cpi_vault_credit1),
                    token_cpi_supply_delta: factual_amount(witness.token_cpi_supply_delta),
                    total_supply_before: factual_amount(correction.total_supply_before),
                    total_supply_after: factual_amount(correction.total_supply_after),
                });
            }
            if let Some(correction) = receipt.cash_inventory_correction {
                let lp_cash_events: Vec<_> = operation
                    .assertions
                    .iter()
                    .filter(|event| {
                        matches!(
                            &event.payload,
                            EventPayloadV1::LpPosition(payload)
                                if payload.cash_reserve0.is_some()
                                    && payload.cash_reserve1.is_some()
                        )
                    })
                    .collect();
                if lp_cash_events.len() != 1
                    || witness.event_cash_reserve0_after
                        != Some(correction.witnessed_cash_reserve0_after)
                    || witness.event_cash_reserve1_after
                        != Some(correction.witnessed_cash_reserve1_after)
                    || after.pair.cash_reserve0 != correction.witnessed_cash_reserve0_after
                    || after.pair.cash_reserve1 != correction.witnessed_cash_reserve1_after
                    || after.pair.total_debt0 != correction.witnessed_total_debt0_after
                    || after.pair.total_debt1 != correction.witnessed_total_debt1_after
                {
                    return Err(V1EngineError::CheckpointMismatch.into());
                }
                let cash_event = lp_cash_events[0];
                evidence.push(
                    V1DecisionGradeCorrectionEvidenceV1::LpCashProtocolInterestCheckpointCatchUp {
                        assertion_event_id: cash_event.event_id.clone(),
                        assertion_raw_payload_sha256: cash_event.raw_payload_sha256.clone(),
                        source_native_cash_reserve0_after: factual_amount(
                            correction.source_native_cash_reserve0_after,
                        ),
                        source_native_cash_reserve1_after: factual_amount(
                            correction.source_native_cash_reserve1_after,
                        ),
                        event_cash_reserve0_after: factual_amount(
                            correction.witnessed_cash_reserve0_after,
                        ),
                        event_cash_reserve1_after: factual_amount(
                            correction.witnessed_cash_reserve1_after,
                        ),
                        source_native_total_debt0_after: factual_amount(
                            correction.source_native_total_debt0_after,
                        ),
                        source_native_total_debt1_after: factual_amount(
                            correction.source_native_total_debt1_after,
                        ),
                        witnessed_total_debt0_after: factual_amount(
                            correction.witnessed_total_debt0_after,
                        ),
                        witnessed_total_debt1_after: factual_amount(
                            correction.witnessed_total_debt1_after,
                        ),
                        source_native_protocol_interest0_after: factual_amount_u128(
                            correction.source_native_protocol_interest0_after,
                        ),
                        source_native_protocol_interest1_after: factual_amount_u128(
                            correction.source_native_protocol_interest1_after,
                        ),
                        witnessed_protocol_interest0_after: factual_amount_u128(
                            correction.witnessed_protocol_interest0_after,
                        ),
                        witnessed_protocol_interest1_after: factual_amount_u128(
                            correction.witnessed_protocol_interest1_after,
                        ),
                        protocol_interest_inventory_rounding_atoms0: factual_signed_amount(
                            correction.protocol_interest_inventory_rounding_atoms0,
                        ),
                        protocol_interest_inventory_rounding_atoms1: factual_signed_amount(
                            correction.protocol_interest_inventory_rounding_atoms1,
                        ),
                        token_cpi_reserve_vault0_after: factual_amount(
                            after.custody.reserve_vault0,
                        ),
                        token_cpi_reserve_vault1_after: factual_amount(
                            after.custody.reserve_vault1,
                        ),
                    },
                );
            }
            Ok(evidence)
        }
        V1AppliedReceiptV1::RemoveLiquidity(receipt) => {
            if receipt.cash_inventory_correction.is_none()
                && receipt.debt_coverage_exception.is_none()
            {
                return Ok(Vec::new());
            }
            let (witness, burn_event, cash_event) = unique_lp_burn_cash_witness(operation)?;
            let (
                source_native_amount0_out,
                source_native_amount1_out,
                source_native_reserve0_after,
                source_native_reserve1_after,
                witnessed_reserve0_after,
                witnessed_reserve1_after,
            ) = receipt.output_correction.map_or(
                (
                    receipt.amount0_out,
                    receipt.amount1_out,
                    after.pair.reserve0,
                    after.pair.reserve1,
                    after.pair.reserve0,
                    after.pair.reserve1,
                ),
                |output| {
                    (
                        output.source_native_amount0_out,
                        output.source_native_amount1_out,
                        output.source_native_reserve0_after,
                        output.source_native_reserve1_after,
                        output.witnessed_reserve0_after,
                        output.witnessed_reserve1_after,
                    )
                },
            );
            let mut evidence = Vec::new();
            if let Some(correction) = receipt.cash_inventory_correction {
                if witness.event_cash_reserve0_after != correction.witnessed_cash_reserve0_after
                    || witness.event_cash_reserve1_after != correction.witnessed_cash_reserve1_after
                    || after.pair.cash_reserve0 != correction.witnessed_cash_reserve0_after
                    || after.pair.cash_reserve1 != correction.witnessed_cash_reserve1_after
                    || after.pair.total_debt0 != correction.witnessed_total_debt0_after
                    || after.pair.total_debt1 != correction.witnessed_total_debt1_after
                {
                    return Err(V1EngineError::CheckpointMismatch.into());
                }
                evidence.push(V1DecisionGradeCorrectionEvidenceV1::LpBurnCashCheckpoint {
                    burn_event_id: burn_event.event_id.clone(),
                    burn_raw_payload_sha256: burn_event.raw_payload_sha256.clone(),
                    cash_event_id: cash_event.event_id.clone(),
                    cash_raw_payload_sha256: cash_event.raw_payload_sha256.clone(),
                    liquidity_burned: factual_amount(witness.event_liquidity_burned),
                    source_native_amount0_out: factual_amount(source_native_amount0_out),
                    source_native_amount1_out: factual_amount(source_native_amount1_out),
                    event_amount0_out: factual_amount(witness.event_amount0_out),
                    event_amount1_out: factual_amount(witness.event_amount1_out),
                    source_native_reserve0_after: factual_amount(source_native_reserve0_after),
                    source_native_reserve1_after: factual_amount(source_native_reserve1_after),
                    witnessed_reserve0_after: factual_amount(witnessed_reserve0_after),
                    witnessed_reserve1_after: factual_amount(witnessed_reserve1_after),
                    token_cpi_vault_debit0: factual_amount(witness.token_cpi_vault_debit0),
                    token_cpi_vault_debit1: factual_amount(witness.token_cpi_vault_debit1),
                    token_cpi_recipient_credit0: factual_amount(
                        witness.token_cpi_recipient_credit0,
                    ),
                    token_cpi_recipient_credit1: factual_amount(
                        witness.token_cpi_recipient_credit1,
                    ),
                    source_native_cash_reserve0_after: factual_amount(
                        correction.source_native_cash_reserve0_after,
                    ),
                    source_native_cash_reserve1_after: factual_amount(
                        correction.source_native_cash_reserve1_after,
                    ),
                    event_cash_reserve0_after: factual_amount(
                        correction.witnessed_cash_reserve0_after,
                    ),
                    event_cash_reserve1_after: factual_amount(
                        correction.witnessed_cash_reserve1_after,
                    ),
                    source_native_total_debt0_after: factual_amount(
                        correction.source_native_total_debt0_after,
                    ),
                    source_native_total_debt1_after: factual_amount(
                        correction.source_native_total_debt1_after,
                    ),
                    witnessed_total_debt0_after: factual_amount(
                        correction.witnessed_total_debt0_after,
                    ),
                    witnessed_total_debt1_after: factual_amount(
                        correction.witnessed_total_debt1_after,
                    ),
                    cash_checkpoint_rounding_atoms0: factual_signed_amount(
                        correction.cash_checkpoint_rounding_atoms0,
                    ),
                    cash_checkpoint_rounding_atoms1: factual_signed_amount(
                        correction.cash_checkpoint_rounding_atoms1,
                    ),
                });
            }
            if let Some(exception) = receipt.debt_coverage_exception {
                if after.pair.reserve0 != exception.post_reserve0
                    || after.pair.reserve1 != exception.post_reserve1
                    || (exception.token0_shortfall_atoms == 0
                        && exception.token1_shortfall_atoms == 0)
                {
                    return Err(V1EngineError::CheckpointMismatch.into());
                }
                evidence.push(
                    V1DecisionGradeCorrectionEvidenceV1::LpBurnPostDebtCoverageException {
                        burn_event_id: burn_event.event_id.clone(),
                        burn_raw_payload_sha256: burn_event.raw_payload_sha256.clone(),
                        cash_event_id: cash_event.event_id.clone(),
                        cash_raw_payload_sha256: cash_event.raw_payload_sha256.clone(),
                        liquidity_burned: factual_amount(witness.event_liquidity_burned),
                        required_token1_for_debt0: factual_amount(
                            exception.required_token1_for_debt0,
                        ),
                        buffered_token1_for_debt0: factual_amount_u128(
                            exception.buffered_token1_for_debt0,
                        ),
                        post_reserve1: factual_amount(exception.post_reserve1),
                        token1_shortfall_atoms: factual_amount_u128(
                            exception.token1_shortfall_atoms,
                        ),
                        token1_coverage_failed: exception.token1_shortfall_atoms != 0,
                        required_token0_for_debt1: factual_amount(
                            exception.required_token0_for_debt1,
                        ),
                        buffered_token0_for_debt1: factual_amount_u128(
                            exception.buffered_token0_for_debt1,
                        ),
                        post_reserve0: factual_amount(exception.post_reserve0),
                        token0_shortfall_atoms: factual_amount_u128(
                            exception.token0_shortfall_atoms,
                        ),
                        token0_coverage_failed: exception.token0_shortfall_atoms != 0,
                    },
                );
            }
            Ok(evidence)
        }
        V1AppliedReceiptV1::Swap(receipt) => {
            let Some(correction) = receipt.transition_correction else {
                return Ok(Vec::new());
            };
            let (witness, event) = unique_swap_transition_witness(operation)?;
            if correction.witnessed_amount_in_after_fee != witness.event_amount_in_after_fee
                || correction.witnessed_amount_out != witness.event_amount_out
                || correction.witnessed_lp_fee != witness.event_lp_fee
                || correction.witnessed_protocol_fee != witness.event_protocol_fee
                || correction.witnessed_reserve0_after != witness.event_reserve0_after
                || correction.witnessed_reserve1_after != witness.event_reserve1_after
                || receipt.reserve0_after != after.pair.reserve0
                || receipt.reserve1_after != after.pair.reserve1
            {
                return Err(V1EngineError::CheckpointMismatch.into());
            }
            let reserve_debt_alignment_amount = correction
                .total_debt_before_alignment
                .abs_diff(correction.total_debt_after_alignment);
            Ok(vec![V1DecisionGradeCorrectionEvidenceV1::SwapTransition {
                assertion_event_id: event.event_id.clone(),
                assertion_raw_payload_sha256: event.raw_payload_sha256.clone(),
                token_in: receipt.token_in,
                gross_amount_in: factual_amount(receipt.gross_amount_in),
                source_native_amount_in_after_fee: factual_amount(
                    correction.source_native_amount_in_after_fee,
                ),
                source_native_amount_out: factual_amount(correction.source_native_amount_out),
                source_native_lp_fee: factual_amount(correction.source_native_lp_fee),
                source_native_protocol_fee: factual_amount(correction.source_native_protocol_fee),
                source_native_reserve0_after: factual_amount(
                    correction.source_native_reserve0_after,
                ),
                source_native_reserve1_after: factual_amount(
                    correction.source_native_reserve1_after,
                ),
                event_amount_in_after_fee: factual_amount(witness.event_amount_in_after_fee),
                event_amount_out: factual_amount(witness.event_amount_out),
                event_lp_fee: factual_amount(witness.event_lp_fee),
                event_protocol_fee: factual_amount(witness.event_protocol_fee),
                config_accounted_lp_fee: factual_amount(correction.accounted_lp_fee),
                config_accounted_protocol_fee: factual_amount(correction.accounted_protocol_fee),
                event_reserve0_after: factual_amount(witness.event_reserve0_after),
                event_reserve1_after: factual_amount(witness.event_reserve1_after),
                token_cpi_vault_credit_in: factual_amount(witness.token_cpi_vault_credit_in),
                token_cpi_vault_debit_out: factual_amount(witness.token_cpi_vault_debit_out),
                token_cpi_recipient_credit_out: factual_amount(
                    witness.token_cpi_recipient_credit_out,
                ),
                reserve_debt_alignment_side: correction.reserve_debt_alignment_side,
                total_debt_before_alignment: factual_amount(correction.total_debt_before_alignment),
                total_debt_after_alignment: factual_amount(correction.total_debt_after_alignment),
                total_debt_shares_unchanged: factual_amount_u128(
                    correction.total_debt_shares_unchanged,
                ),
                reserve_debt_alignment_amount: factual_amount(reserve_debt_alignment_amount),
                protocol_inventory_before: factual_amount(correction.protocol_inventory_before),
                protocol_inventory_after: factual_amount(correction.protocol_inventory_after),
                protocol_inventory_delta: factual_amount(correction.protocol_inventory_delta),
                reserve_model_inventory_delta: factual_amount(
                    correction.reserve_model_inventory_delta,
                ),
                output_protocol_inventory_before: factual_amount(
                    correction.output_protocol_inventory_before,
                ),
                output_protocol_inventory_after: factual_amount(
                    correction.output_protocol_inventory_after,
                ),
                output_protocol_inventory_delta: factual_amount(
                    correction.output_protocol_inventory_delta,
                ),
                output_reserve_model_inventory_delta: factual_amount(
                    correction.output_reserve_model_inventory_delta,
                ),
                k_before: factual_amount_u128(correction.k_before),
                k_after: factual_amount_u128(correction.k_after),
                k_delta: factual_signed_amount(correction.k_delta),
            }])
        }
        V1AppliedReceiptV1::Collateral(receipt) => {
            if let Some(correction) = receipt.deposit_factor_correction {
                let position = match &operation.payload {
                    OperationPayloadV1::CollateralDelta { position, .. } => {
                        parse_pubkey("collateral.position", position)?
                    }
                    _ => unreachable!("collateral receipt matched collateral payload"),
                };
                let witnessed = unique_position_factor_witness(operation, position)?;
                let collateral_events: Vec<_> = operation
                    .assertions
                    .iter()
                    .filter(|event| matches!(&event.payload, EventPayloadV1::CollateralDelta(_)))
                    .collect();
                let position_events: Vec<_> = operation
                    .assertions
                    .iter()
                    .filter(|event| matches!(&event.payload, EventPayloadV1::Position(_)))
                    .collect();
                if collateral_events.len() != 1 || position_events.len() != 1 {
                    return Err(V1EngineError::CheckpointMismatch.into());
                }
                let position_event = position_events[0];
                let EventPayloadV1::Position(position_payload) = &position_event.payload else {
                    unreachable!("filtered to position event")
                };
                let replayed = after
                    .positions
                    .get(&position)
                    .ok_or(V1EngineError::MissingPosition(position))?;
                if correction.witnessed != witnessed
                    || replayed.collateral0
                        != parse_u64(
                            "collateral_deposit_position.collateral0",
                            &position_payload.collateral0,
                        )?
                    || replayed.collateral1
                        != parse_u64(
                            "collateral_deposit_position.collateral1",
                            &position_payload.collateral1,
                        )?
                    || replayed.debt0_shares
                        != parse_u128(
                            "collateral_deposit_position.debt0_shares",
                            &position_payload.debt0_shares,
                        )?
                    || replayed.debt1_shares
                        != parse_u128(
                            "collateral_deposit_position.debt1_shares",
                            &position_payload.debt1_shares,
                        )?
                    || max_cf(replayed.collateral0_liquidation_cf_bps)
                        != witnessed.collateral0_max_cf_bps
                    || max_cf(replayed.collateral1_liquidation_cf_bps)
                        != witnessed.collateral1_max_cf_bps
                    || replayed.collateral0_liquidation_cf_bps
                        != witnessed.collateral0_liquidation_cf_bps
                    || replayed.collateral1_liquidation_cf_bps
                        != witnessed.collateral1_liquidation_cf_bps
                {
                    return Err(V1EngineError::CheckpointMismatch.into());
                }
                let collateral_event = collateral_events[0];
                return Ok(vec![
                    V1DecisionGradeCorrectionEvidenceV1::CollateralDepositFactorTransition {
                        collateral_delta_event_id: collateral_event.event_id.clone(),
                        collateral_delta_raw_payload_sha256: collateral_event
                            .raw_payload_sha256
                            .clone(),
                        position_event_id: position_event.event_id.clone(),
                        position_raw_payload_sha256: position_event.raw_payload_sha256.clone(),
                        position: position.to_string(),
                        collateral_token: correction.collateral_token,
                        deposited_amount: factual_amount(receipt.accounted_amount),
                        token_cpi_collateral_vault_credit: factual_amount(
                            receipt.collateral_vault_amount,
                        ),
                        token_cpi_counterparty_debit: factual_amount(receipt.counterparty_amount),
                        preserve_candidate: correction.preserve_candidate,
                        recompute_candidate: correction.recompute_candidate,
                        event_witnessed: correction.witnessed,
                    },
                ]);
            }
            let Some(correction) = receipt.withdrawal_factor_correction else {
                return Ok(Vec::new());
            };
            let position = match &operation.payload {
                OperationPayloadV1::CollateralDelta { position, .. } => {
                    parse_pubkey("collateral.position", position)?
                }
                _ => unreachable!("collateral receipt matched collateral payload"),
            };
            let witness = unique_collateral_withdrawal_witness(
                operation,
                position,
                correction.collateral_token,
                receipt.accounted_amount,
            )?;
            let collateral_event = operation
                .assertions
                .iter()
                .find(|event| matches!(&event.payload, EventPayloadV1::CollateralDelta(_)))
                .expect("unique witness proved collateral event");
            let position_event = operation
                .assertions
                .iter()
                .find(|event| matches!(&event.payload, EventPayloadV1::Position(_)))
                .expect("unique witness proved position event");
            let replayed = after
                .positions
                .get(&position)
                .ok_or(V1EngineError::MissingPosition(position))?;
            if replayed.collateral0 != witness.event_collateral0
                || replayed.collateral1 != witness.event_collateral1
                || replayed.debt0_shares != witness.event_debt0_shares
                || replayed.debt1_shares != witness.event_debt1_shares
                || replayed.collateral0_liquidation_cf_bps
                    != witness.event_collateral0_liquidation_cf_bps
                || replayed.collateral1_liquidation_cf_bps
                    != witness.event_collateral1_liquidation_cf_bps
            {
                return Err(V1EngineError::CheckpointMismatch.into());
            }
            Ok(vec![
                V1DecisionGradeCorrectionEvidenceV1::CollateralWithdrawalFactorTransition {
                    collateral_delta_event_id: collateral_event.event_id.clone(),
                    collateral_delta_raw_payload_sha256: collateral_event
                        .raw_payload_sha256
                        .clone(),
                    position_event_id: position_event.event_id.clone(),
                    position_raw_payload_sha256: position_event.raw_payload_sha256.clone(),
                    position: position.to_string(),
                    collateral_token: correction.collateral_token,
                    requested_amount: factual_amount(correction.requested_amount),
                    source_native_resolved_amount: factual_amount(
                        correction.source_native_resolved_amount,
                    ),
                    witnessed_resolved_amount: factual_amount(correction.witnessed_resolved_amount),
                    withdrawn_amount: factual_amount(witness.event_amount),
                    token_cpi_collateral_vault_debit: factual_amount(
                        witness.token_cpi_collateral_vault_debit,
                    ),
                    token_cpi_recipient_credit: factual_amount(witness.token_cpi_recipient_credit),
                    source_native_collateral0_max_cf_bps: correction
                        .source_native_collateral0_max_cf_bps,
                    source_native_collateral1_max_cf_bps: correction
                        .source_native_collateral1_max_cf_bps,
                    source_native_collateral0_liquidation_cf_bps: correction
                        .source_native_collateral0_liquidation_cf_bps,
                    source_native_collateral1_liquidation_cf_bps: correction
                        .source_native_collateral1_liquidation_cf_bps,
                    event_collateral0_max_cf_bps: correction.witnessed_collateral0_max_cf_bps,
                    event_collateral1_max_cf_bps: correction.witnessed_collateral1_max_cf_bps,
                    event_collateral0_liquidation_cf_bps: correction
                        .witnessed_collateral0_liquidation_cf_bps,
                    event_collateral1_liquidation_cf_bps: correction
                        .witnessed_collateral1_liquidation_cf_bps,
                    post_capacity_debt: correction
                        .post_capacity_exception
                        .map(|finding| factual_amount(finding.post_withdraw_debt)),
                    post_capacity_maximum: correction
                        .post_capacity_exception
                        .map(|finding| factual_amount(finding.admissible_max_debt)),
                    post_capacity_excess_debt_atoms: correction
                        .post_capacity_exception
                        .map(|finding| factual_amount(finding.excess_debt_atoms)),
                },
            ])
        }
        V1AppliedReceiptV1::Liquidation(receipt) => {
            let Some(correction) = receipt.transition_correction else {
                return Ok(Vec::new());
            };
            let (witness, event) = unique_liquidation_transition_witness(operation)?;
            let position_after = after
                .positions
                .get(&witness.event_position)
                .ok_or(V1EngineError::MissingPosition(witness.event_position))?;
            let (
                witnessed_debt_reserve_after,
                witnessed_total_debt_after,
                witnessed_collateral_reserve_after,
                witnessed_collateral_cash_after,
                witnessed_total_collateral_after,
                witnessed_position_collateral_after,
                witnessed_collateral_vault_after,
            ) = match witness.event_debt_token {
                V1TokenSideV1::Token0 => (
                    after.pair.reserve0,
                    after.pair.total_debt0,
                    after.pair.reserve1,
                    after.pair.cash_reserve1,
                    after.pair.total_collateral1,
                    position_after.collateral1,
                    after.custody.collateral_vault1,
                ),
                V1TokenSideV1::Token1 => (
                    after.pair.reserve1,
                    after.pair.total_debt1,
                    after.pair.reserve0,
                    after.pair.cash_reserve0,
                    after.pair.total_collateral0,
                    position_after.collateral0,
                    after.custody.collateral_vault0,
                ),
            };
            if correction.witnessed_debt_written_off != witness.event_debt_written_off
                || correction.witnessed_collateral_seized != witness.event_collateral_seized
                || correction.witnessed_collateral_to_reserves
                    != witness.token_cpi_reserve_vault_credit
                || correction.witnessed_liquidator_incentive != witness.event_liquidator_incentive
                || receipt.debt_written_off != witness.event_debt_written_off
                || receipt.collateral_seized != witness.event_collateral_seized
                || receipt.shortfall != witness.event_shortfall
                || correction.witnessed_debt_reserve_after != witnessed_debt_reserve_after
                || correction.witnessed_total_debt_after != witnessed_total_debt_after
                || correction.witnessed_collateral_reserve_after
                    != witnessed_collateral_reserve_after
                || correction.witnessed_collateral_cash_after != witnessed_collateral_cash_after
                || correction.witnessed_total_collateral_after != witnessed_total_collateral_after
                || correction.witnessed_position_collateral_after
                    != witnessed_position_collateral_after
                || correction.witnessed_collateral_vault_after != witnessed_collateral_vault_after
            {
                return Err(V1EngineError::CheckpointMismatch.into());
            }
            Ok(vec![
                V1DecisionGradeCorrectionEvidenceV1::LiquidationTransition {
                    assertion_event_id: event.event_id.clone(),
                    assertion_raw_payload_sha256: event.raw_payload_sha256.clone(),
                    position: witness.event_position.to_string(),
                    debt_token: witness.event_debt_token,
                    source_native_debt_written_off: factual_amount(
                        correction.source_native_debt_written_off,
                    ),
                    event_debt_written_off: factual_amount(witness.event_debt_written_off),
                    debt_shares_written_off: factual_amount_u128(
                        correction.debt_shares_written_off,
                    ),
                    source_native_collateral_seized: factual_amount(
                        correction.source_native_collateral_seized,
                    ),
                    event_collateral_seized: factual_amount(witness.event_collateral_seized),
                    source_native_collateral_to_reserves: factual_amount(
                        correction.source_native_collateral_to_reserves,
                    ),
                    token_cpi_reserve_vault_credit: factual_amount(
                        witness.token_cpi_reserve_vault_credit,
                    ),
                    source_native_liquidator_incentive: factual_amount(
                        correction.source_native_liquidator_incentive,
                    ),
                    event_liquidator_incentive: factual_amount(witness.event_liquidator_incentive),
                    token_cpi_collateral_vault_debit: factual_amount(
                        witness.token_cpi_collateral_vault_debit,
                    ),
                    token_cpi_liquidator_recipient_credit: factual_amount(
                        witness.token_cpi_liquidator_recipient_credit,
                    ),
                    source_native_debt_reserve_after: factual_amount(
                        correction.source_native_debt_reserve_after,
                    ),
                    witnessed_debt_reserve_after: factual_amount(
                        correction.witnessed_debt_reserve_after,
                    ),
                    source_native_total_debt_after: factual_amount(
                        correction.source_native_total_debt_after,
                    ),
                    witnessed_total_debt_after: factual_amount(
                        correction.witnessed_total_debt_after,
                    ),
                    source_native_collateral_reserve_after: factual_amount(
                        correction.source_native_collateral_reserve_after,
                    ),
                    witnessed_collateral_reserve_after: factual_amount(
                        correction.witnessed_collateral_reserve_after,
                    ),
                    source_native_collateral_cash_after: factual_amount(
                        correction.source_native_collateral_cash_after,
                    ),
                    witnessed_collateral_cash_after: factual_amount(
                        correction.witnessed_collateral_cash_after,
                    ),
                    source_native_total_collateral_after: factual_amount(
                        correction.source_native_total_collateral_after,
                    ),
                    witnessed_total_collateral_after: factual_amount(
                        correction.witnessed_total_collateral_after,
                    ),
                    source_native_position_collateral_after: factual_amount(
                        correction.source_native_position_collateral_after,
                    ),
                    witnessed_position_collateral_after: factual_amount(
                        correction.witnessed_position_collateral_after,
                    ),
                    source_native_collateral_vault_after: factual_amount(
                        correction.source_native_collateral_vault_after,
                    ),
                    witnessed_collateral_vault_after: factual_amount(
                        correction.witnessed_collateral_vault_after,
                    ),
                },
            ])
        }
        V1AppliedReceiptV1::Borrow(receipt) => {
            let Some(correction) = receipt.transition_correction else {
                return Ok(Vec::new());
            };
            let (witness, debt_event, position_event) =
                unique_borrow_transition_witness(operation)?;
            let witnessed_position_shares_after = match receipt.capacity_before.debt_token {
                V1TokenSideV1::Token0 => witness.event_debt0_shares,
                V1TokenSideV1::Token1 => witness.event_debt1_shares,
            };
            let position_after = after
                .positions
                .get(&witness.event_position)
                .ok_or(V1EngineError::MissingPosition(witness.event_position))?;
            let replayed_position_shares_after = match receipt.capacity_before.debt_token {
                V1TokenSideV1::Token0 => position_after.debt0_shares,
                V1TokenSideV1::Token1 => position_after.debt1_shares,
            };
            let replayed_total_debt_shares_after = match receipt.capacity_before.debt_token {
                V1TokenSideV1::Token0 => after.pair.total_debt0_shares,
                V1TokenSideV1::Token1 => after.pair.total_debt1_shares,
            };
            let replayed_total_debt_after = match receipt.capacity_before.debt_token {
                V1TokenSideV1::Token0 => after.pair.total_debt0,
                V1TokenSideV1::Token1 => after.pair.total_debt1,
            };
            if correction.witnessed_position_debt_shares_after != witnessed_position_shares_after
                || replayed_position_shares_after != witnessed_position_shares_after
                || correction.witnessed_total_debt_shares_after != replayed_total_debt_shares_after
                || correction.witnessed_total_debt_after != replayed_total_debt_after
            {
                return Err(V1EngineError::CheckpointMismatch.into());
            }
            Ok(vec![
                V1DecisionGradeCorrectionEvidenceV1::BorrowTransition {
                    debt_delta_event_id: debt_event.event_id.clone(),
                    debt_delta_raw_payload_sha256: debt_event.raw_payload_sha256.clone(),
                    position_event_id: position_event.event_id.clone(),
                    position_raw_payload_sha256: position_event.raw_payload_sha256.clone(),
                    position: witness.event_position.to_string(),
                    debt_token: witness.event_debt_token,
                    borrowed_amount: factual_amount(witness.event_borrowed_amount),
                    token_cpi_vault_debit: factual_amount(witness.token_cpi_vault_debit),
                    token_cpi_recipient_credit: factual_amount(witness.token_cpi_recipient_credit),
                    source_native_position_debt_shares_after: factual_amount_u128(
                        correction.source_native_position_debt_shares_after,
                    ),
                    event_position_debt_shares_after: factual_amount_u128(
                        witnessed_position_shares_after,
                    ),
                    source_native_total_debt_shares_after: factual_amount_u128(
                        correction.source_native_total_debt_shares_after,
                    ),
                    witnessed_total_debt_shares_after: factual_amount_u128(
                        correction.witnessed_total_debt_shares_after,
                    ),
                    source_native_total_debt_after: factual_amount(
                        correction.source_native_total_debt_after,
                    ),
                    witnessed_total_debt_after: factual_amount(
                        correction.witnessed_total_debt_after,
                    ),
                    witnessed_position_debt_share_delta: factual_amount_u128(
                        correction.witnessed_position_debt_share_delta,
                    ),
                },
            ])
        }
        V1AppliedReceiptV1::Repay(receipt) => {
            let Some(correction) = receipt.transition_correction else {
                return Ok(Vec::new());
            };
            let (witness, debt_event, position_event) = unique_repay_transition_witness(operation)?;
            let witnessed_position_shares_after = match witness.event_debt_token {
                V1TokenSideV1::Token0 => witness.event_debt0_shares,
                V1TokenSideV1::Token1 => witness.event_debt1_shares,
            };
            let position_after = after
                .positions
                .get(&witness.event_position)
                .ok_or(V1EngineError::MissingPosition(witness.event_position))?;
            let replayed_position_shares_after = match witness.event_debt_token {
                V1TokenSideV1::Token0 => position_after.debt0_shares,
                V1TokenSideV1::Token1 => position_after.debt1_shares,
            };
            let replayed_total_debt_shares_after = match witness.event_debt_token {
                V1TokenSideV1::Token0 => after.pair.total_debt0_shares,
                V1TokenSideV1::Token1 => after.pair.total_debt1_shares,
            };
            let replayed_total_debt_after = match witness.event_debt_token {
                V1TokenSideV1::Token0 => after.pair.total_debt0,
                V1TokenSideV1::Token1 => after.pair.total_debt1,
            };
            if correction.witnessed_position_debt_shares_after != witnessed_position_shares_after
                || replayed_position_shares_after != witnessed_position_shares_after
                || correction.witnessed_total_debt_shares_after != replayed_total_debt_shares_after
                || correction.witnessed_total_debt_after != replayed_total_debt_after
            {
                return Err(V1EngineError::CheckpointMismatch.into());
            }
            let requested_amount = match &operation.payload {
                OperationPayloadV1::Repay {
                    requested_amount, ..
                } => requested_amount.clone(),
                _ => unreachable!("receipt matched repay payload"),
            };
            Ok(vec![V1DecisionGradeCorrectionEvidenceV1::RepayTransition {
                debt_delta_event_id: debt_event.event_id.clone(),
                debt_delta_raw_payload_sha256: debt_event.raw_payload_sha256.clone(),
                position_event_id: position_event.event_id.clone(),
                position_raw_payload_sha256: position_event.raw_payload_sha256.clone(),
                position: witness.event_position.to_string(),
                debt_token: witness.event_debt_token,
                requested_amount,
                source_native_position_debt_before: factual_amount(
                    correction.source_native_position_debt_before,
                ),
                witnessed_position_debt_before: factual_amount(
                    correction.witnessed_position_debt_before,
                ),
                modeled_position_debt_gap: factual_amount(correction.modeled_position_debt_gap),
                source_native_repaid_amount: factual_amount(correction.source_native_repaid_amount),
                witnessed_repaid_amount: factual_amount(correction.witnessed_repaid_amount),
                token_cpi_vault_credit: factual_amount(witness.token_cpi_vault_credit),
                source_native_position_debt_shares_after: factual_amount_u128(
                    correction.source_native_position_debt_shares_after,
                ),
                event_position_debt_shares_after: factual_amount_u128(
                    witnessed_position_shares_after,
                ),
                source_native_total_debt_shares_after: factual_amount_u128(
                    correction.source_native_total_debt_shares_after,
                ),
                witnessed_total_debt_shares_after: factual_amount_u128(
                    correction.witnessed_total_debt_shares_after,
                ),
                source_native_total_debt_after: factual_amount(
                    correction.source_native_total_debt_after,
                ),
                witnessed_total_debt_after: factual_amount(correction.witnessed_total_debt_after),
                witnessed_position_debt_share_delta: factual_amount_u128(
                    correction.witnessed_position_debt_share_delta,
                ),
            }])
        }
        V1AppliedReceiptV1::ProtocolClaim(receipt) => {
            let Some(correction) = receipt.realization_correction else {
                return Ok(Vec::new());
            };
            let (amount0, amount1) = match &operation.payload {
                OperationPayloadV1::ClaimProtocolRevenue { amount0, amount1 } => (
                    parse_u64("claim.amount0", amount0)?,
                    parse_u64("claim.amount1", amount1)?,
                ),
                _ => unreachable!("protocol-claim receipt matched claim payload"),
            };
            if amount0 != correction.witnessed_realized0
                || amount1 != correction.witnessed_realized1
                || receipt.claimable0 != correction.witnessed_realized0
                || receipt.claimable1 != correction.witnessed_realized1
                || correction.source_native_claimable0
                    != correction
                        .witnessed_realized0
                        .checked_add(correction.modeled_residual0)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "protocol claim token0 modeled residual",
                        ))?
                || correction.source_native_claimable1
                    != correction
                        .witnessed_realized1
                        .checked_add(correction.modeled_residual1)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "protocol claim token1 modeled residual",
                        ))?
                || correction.reserve_vault0_after_claim != after.custody.reserve_vault0
                || correction.reserve_vault1_after_claim != after.custody.reserve_vault1
                || correction.reserve_vault0_before_claim
                    != after
                        .custody
                        .reserve_vault0
                        .checked_add(correction.witnessed_realized0)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "protocol claim token0 custody reconstruction",
                        ))?
                || correction.reserve_vault1_before_claim
                    != after
                        .custody
                        .reserve_vault1
                        .checked_add(correction.witnessed_realized1)
                        .ok_or(V1EngineError::ArithmeticOverflow(
                            "protocol claim token1 custody reconstruction",
                        ))?
            {
                return Err(V1EngineError::CheckpointMismatch.into());
            }
            Ok(vec![
                V1DecisionGradeCorrectionEvidenceV1::ProtocolClaimPartialRealization {
                    source_native_claimable0: factual_amount(correction.source_native_claimable0),
                    source_native_claimable1: factual_amount(correction.source_native_claimable1),
                    token_cpi_reserve_vault_debit0: factual_amount(correction.witnessed_realized0),
                    token_cpi_reserve_vault_debit1: factual_amount(correction.witnessed_realized1),
                    modeled_inventory_residual0: factual_amount(correction.modeled_residual0),
                    modeled_inventory_residual1: factual_amount(correction.modeled_residual1),
                    reserve_vault0_before_claim: factual_amount(
                        correction.reserve_vault0_before_claim,
                    ),
                    reserve_vault1_before_claim: factual_amount(
                        correction.reserve_vault1_before_claim,
                    ),
                    reserve_vault0_after_claim: factual_amount(
                        correction.reserve_vault0_after_claim,
                    ),
                    reserve_vault1_after_claim: factual_amount(
                        correction.reserve_vault1_after_claim,
                    ),
                },
            ])
        }
        _ => Ok(Vec::new()),
    }
}

fn retained_event_limitations(
    operation: &CanonicalOperationV1,
    decision_grade: bool,
    receipt: Option<&V1AppliedReceiptV1>,
) -> Vec<V1DecisionGradeRetainedEventLimitationV1> {
    if !decision_grade {
        return Vec::new();
    }
    let mut limitations: Vec<_> = operation
        .assertions
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayloadV1::LpPosition(payload) => Some(
                V1DecisionGradeRetainedEventLimitationV1::LpPositionDerivedEntitlement {
                    assertion_event_id: event.event_id.clone(),
                    assertion_raw_payload_sha256: event.raw_payload_sha256.clone(),
                    lp_amount: payload.lp_amount.clone(),
                    observed_amount0: payload.amount0.clone(),
                    observed_amount1: payload.amount1.clone(),
                },
            ),
            _ => None,
        })
        .collect();
    if let Some(V1AppliedReceiptV1::AddLiquidity(receipt)) = receipt {
        if let Some(correction) = receipt.cash_inventory_correction {
            if let Some(event) = operation.assertions.iter().find(|event| {
                matches!(
                    &event.payload,
                    EventPayloadV1::LpPosition(payload)
                        if payload.cash_reserve0.is_some() && payload.cash_reserve1.is_some()
                )
            }) {
                if correction.protocol_interest_inventory_rounding_atoms0 != 0
                    || correction.protocol_interest_inventory_rounding_atoms1 != 0
                {
                    limitations.push(
                        V1DecisionGradeRetainedEventLimitationV1::ProtocolInterestInventoryRounding {
                            assertion_event_id: event.event_id.clone(),
                            assertion_raw_payload_sha256: event.raw_payload_sha256.clone(),
                            interval_start_slot: receipt.accrual.slot_before,
                            checkpoint_slot: receipt.accrual.slot_after,
                            inventory_implied_protocol_interest0_after: factual_amount_u128(
                                correction.witnessed_protocol_interest0_after,
                            ),
                            inventory_implied_protocol_interest1_after: factual_amount_u128(
                                correction.witnessed_protocol_interest1_after,
                            ),
                            source_native_protocol_interest0_after: factual_amount_u128(
                                correction.source_native_protocol_interest0_after,
                            ),
                            source_native_protocol_interest1_after: factual_amount_u128(
                                correction.source_native_protocol_interest1_after,
                            ),
                            protocol_interest_inventory_rounding_atoms0:
                                factual_signed_amount(
                                    correction.protocol_interest_inventory_rounding_atoms0,
                                ),
                            protocol_interest_inventory_rounding_atoms1:
                                factual_signed_amount(
                                    correction.protocol_interest_inventory_rounding_atoms1,
                                ),
                        },
                    );
                } else {
                    limitations.push(
                        V1DecisionGradeRetainedEventLimitationV1::ProtocolInterestCheckpointCatchUpTiming {
                        assertion_event_id: event.event_id.clone(),
                        assertion_raw_payload_sha256: event.raw_payload_sha256.clone(),
                        interval_start_slot: receipt.accrual.slot_before,
                        checkpoint_slot: receipt.accrual.slot_after,
                        source_native_cash_covered_protocol_interest0: factual_amount(
                            correction.source_native_cash_covered_protocol_interest0,
                        ),
                        source_native_cash_covered_protocol_interest1: factual_amount(
                            correction.source_native_cash_covered_protocol_interest1,
                        ),
                        witnessed_cash_covered_protocol_interest0: factual_amount(
                            correction.witnessed_cash_covered_protocol_interest0,
                        ),
                        witnessed_cash_covered_protocol_interest1: factual_amount(
                            correction.witnessed_cash_covered_protocol_interest1,
                        ),
                        },
                    );
                }
            }
        }
    }
    if let Some(V1AppliedReceiptV1::RemoveLiquidity(receipt)) = receipt {
        if let Some(correction) = receipt.cash_inventory_correction {
            if let Some(event) = operation.assertions.iter().find(|event| {
                matches!(
                    &event.payload,
                    EventPayloadV1::LpPosition(payload)
                        if payload.cash_reserve0.is_some() && payload.cash_reserve1.is_some()
                )
            }) {
                limitations.push(
                    V1DecisionGradeRetainedEventLimitationV1::LpBurnCashCheckpointModelResidual {
                        cash_event_id: event.event_id.clone(),
                        cash_raw_payload_sha256: event.raw_payload_sha256.clone(),
                        interval_start_slot: receipt.accrual.slot_before,
                        checkpoint_slot: receipt.accrual.slot_after,
                        cash_checkpoint_rounding_atoms0: factual_signed_amount(
                            correction.cash_checkpoint_rounding_atoms0,
                        ),
                        cash_checkpoint_rounding_atoms1: factual_signed_amount(
                            correction.cash_checkpoint_rounding_atoms1,
                        ),
                    },
                );
            }
        }
    }
    if let Some(V1AppliedReceiptV1::Collateral(receipt)) = receipt {
        if let Some(correction) = receipt.withdrawal_factor_correction {
            if let Some(finding) = correction.post_capacity_exception {
                if let Some((event, payload)) =
                    operation
                        .assertions
                        .iter()
                        .find_map(|event| match &event.payload {
                            EventPayloadV1::Position(payload) => Some((event, payload)),
                            _ => None,
                        })
                {
                    limitations.push(
                        V1DecisionGradeRetainedEventLimitationV1::CollateralWithdrawalPostCapacityException {
                            position_event_id: event.event_id.clone(),
                            position_raw_payload_sha256: event.raw_payload_sha256.clone(),
                            position: payload.position.clone(),
                            requested_amount: factual_amount(correction.requested_amount),
                            witnessed_withdrawal_amount: factual_amount(
                                correction.witnessed_resolved_amount,
                            ),
                            post_withdraw_debt: factual_amount(finding.post_withdraw_debt),
                            admissible_max_debt: factual_amount(finding.admissible_max_debt),
                            excess_debt_atoms: factual_amount(finding.excess_debt_atoms),
                        },
                    );
                }
            }
        }
    }
    if let Some(V1AppliedReceiptV1::Liquidation(receipt)) = receipt {
        if let Some((event, payload)) =
            operation
                .assertions
                .iter()
                .find_map(|event| match &event.payload {
                    EventPayloadV1::Liquidation(payload) => Some((event, payload)),
                    _ => None,
                })
        {
            if payload.collateral_price_nad.as_str() != receipt.collateral_price_nad.to_string()
                || payload.k_before.as_str() != receipt.k_before.to_string()
                || payload.k_after.as_str() != receipt.k_after.to_string()
            {
                limitations.push(
                    V1DecisionGradeRetainedEventLimitationV1::LiquidationDerivedMetadata {
                        assertion_event_id: event.event_id.clone(),
                        assertion_raw_payload_sha256: event.raw_payload_sha256.clone(),
                        event_collateral_price_nad: payload.collateral_price_nad.clone(),
                        source_native_collateral_price_nad: factual_amount(
                            receipt.collateral_price_nad,
                        ),
                        event_k_before: payload.k_before.clone(),
                        source_native_k_before: factual_amount_u128(receipt.k_before),
                        event_k_after: payload.k_after.clone(),
                        source_native_k_after: factual_amount_u128(receipt.k_after),
                    },
                );
            }
        }
    }
    if let Some(V1AppliedReceiptV1::Swap(receipt)) = receipt {
        if let Some(correction) = receipt.transition_correction {
            if let Some(event) = operation
                .assertions
                .iter()
                .find(|event| matches!(&event.payload, EventPayloadV1::Swap(_)))
            {
                limitations.push(
                    V1DecisionGradeRetainedEventLimitationV1::SwapEventHistoricalFeeLabels {
                        assertion_event_id: event.event_id.clone(),
                        assertion_raw_payload_sha256: event.raw_payload_sha256.clone(),
                        event_lp_fee_label: factual_amount(correction.witnessed_lp_fee),
                        event_protocol_fee_label: factual_amount(correction.witnessed_protocol_fee),
                        config_accounted_lp_fee: factual_amount(correction.accounted_lp_fee),
                        config_accounted_protocol_fee: factual_amount(
                            correction.accounted_protocol_fee,
                        ),
                        reserve_model_inventory_delta: factual_amount(
                            correction.reserve_model_inventory_delta,
                        ),
                        output_reserve_model_inventory_delta: factual_amount(
                            correction.output_reserve_model_inventory_delta,
                        ),
                    },
                );
                if correction.k_delta < 0 {
                    limitations.push(
                        V1DecisionGradeRetainedEventLimitationV1::SwapEventKDecrease {
                            assertion_event_id: event.event_id.clone(),
                            assertion_raw_payload_sha256: event.raw_payload_sha256.clone(),
                            k_before: factual_amount_u128(correction.k_before),
                            k_after: factual_amount_u128(correction.k_after),
                            k_delta: factual_signed_amount(correction.k_delta),
                        },
                    );
                }
            }
        }
    }
    if let Some(V1AppliedReceiptV1::ProtocolClaim(receipt)) = receipt {
        if receipt.realization_correction.is_some()
            && !operation
                .assertions
                .iter()
                .any(|event| matches!(&event.payload, EventPayloadV1::ProtocolFeeClaim(_)))
        {
            limitations.push(
                V1DecisionGradeRetainedEventLimitationV1::ProtocolClaimRecipientDistributionAttribution {
                    token_cpi_realized0: factual_amount(receipt.claimable0),
                    token_cpi_realized1: factual_amount(receipt.claimable1),
                    modeled_futarchy_amount0: factual_amount(receipt.futarchy_amount0),
                    modeled_futarchy_amount1: factual_amount(receipt.futarchy_amount1),
                    modeled_buybacks_amount0: factual_amount(receipt.buybacks_amount0),
                    modeled_buybacks_amount1: factual_amount(receipt.buybacks_amount1),
                    modeled_team_amount0: factual_amount(receipt.team_amount0),
                    modeled_team_amount1: factual_amount(receipt.team_amount1),
                },
            );
        }
    }
    if matches!(operation.payload, OperationPayloadV1::ClockOnly) {
        let collateral_events: Vec<_> = operation
            .assertions
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    EventPayloadV1::CollateralDelta(payload)
                        if payload.amount0.as_str() == "0" && payload.amount1.as_str() == "0"
                )
            })
            .collect();
        let position_events: Vec<_> = operation
            .assertions
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayloadV1::Position(payload) => Some((event, payload)),
                _ => None,
            })
            .collect();
        if collateral_events.len() == 1 && position_events.len() == 1 {
            limitations.push(
                V1DecisionGradeRetainedEventLimitationV1::ClockOnlyZeroPositionLifecycleAssertion {
                    collateral_delta_event_id: collateral_events[0].event_id.clone(),
                    collateral_delta_raw_payload_sha256: collateral_events[0]
                        .raw_payload_sha256
                        .clone(),
                    position_event_id: position_events[0].0.event_id.clone(),
                    position_raw_payload_sha256: position_events[0].0.raw_payload_sha256.clone(),
                    position: position_events[0].1.position.clone(),
                },
            );
        }
        if let Some(V1AppliedReceiptV1::Liquidation(native)) = receipt {
            if validate_clock_zero_liquidation_assertions(operation).is_ok() {
                let debt_event = operation
                    .assertions
                    .iter()
                    .find(|event| matches!(&event.payload, EventPayloadV1::DebtDelta(_)))
                    .expect("validated unique debt event");
                let (position_event, position_payload) = operation
                    .assertions
                    .iter()
                    .find_map(|event| match &event.payload {
                        EventPayloadV1::Position(payload) => Some((event, payload)),
                        _ => None,
                    })
                    .expect("validated unique position event");
                let (liquidation_event, liquidation_payload) = operation
                    .assertions
                    .iter()
                    .find_map(|event| match &event.payload {
                        EventPayloadV1::Liquidation(payload) => Some((event, payload)),
                        _ => None,
                    })
                    .expect("validated unique liquidation event");
                limitations.push(
                V1DecisionGradeRetainedEventLimitationV1::ClockOnlyZeroLiquidationLifecycleAssertion {
                    debt_delta_event_id: debt_event.event_id.clone(),
                    debt_delta_raw_payload_sha256: debt_event.raw_payload_sha256.clone(),
                    position_event_id: position_event.event_id.clone(),
                    position_raw_payload_sha256: position_event.raw_payload_sha256.clone(),
                    liquidation_event_id: liquidation_event.event_id.clone(),
                    liquidation_raw_payload_sha256: liquidation_event
                        .raw_payload_sha256
                        .clone(),
                    position: position_payload.position.clone(),
                    debt_token: native.debt_token,
                    pre_debt_shares: factual_amount_u128(match native.debt_token {
                        V1TokenSideV1::Token0 => parse_u128(
                            "zero_liquidation.debt0_shares",
                            &position_payload.debt0_shares,
                        )
                        .unwrap_or_default()
                        .saturating_add(native.debt_shares_written_off),
                        V1TokenSideV1::Token1 => parse_u128(
                            "zero_liquidation.debt1_shares",
                            &position_payload.debt1_shares,
                        )
                        .unwrap_or_default()
                        .saturating_add(native.debt_shares_written_off),
                    }),
                    post_debt_shares: match native.debt_token {
                        V1TokenSideV1::Token0 => position_payload.debt0_shares.clone(),
                        V1TokenSideV1::Token1 => position_payload.debt1_shares.clone(),
                    },
                    debt_shares_written_off: factual_amount_u128(
                        native.debt_shares_written_off,
                    ),
                    observed_collateral_price_nad: liquidation_payload
                        .collateral_price_nad
                        .clone(),
                    unchanged_k: liquidation_payload.k_before.clone(),
                },
            );
            }
        }
    }
    if let OperationPayloadV1::CollateralDelta { position, .. } = &operation.payload {
        if let (Ok(pair), Ok(position_key)) = (
            parse_pubkey("operation.pair", &operation.pair),
            parse_pubkey("collateral.position", position),
        ) {
            if let Ok((event_owner, bump)) =
                decision_grade_position_creation_owner(operation, pair, position_key)
            {
                let outer_actor = operation.actor.clone().unwrap_or_default();
                let outer_derives_position = parse_pubkey("operation.actor", &outer_actor)
                    .ok()
                    .map(|owner| {
                        Pubkey::find_program_address(
                            &[POSITION_SEED_PREFIX, pair.as_ref(), owner.as_ref()],
                            &omnipair::ID,
                        )
                        .0 == position_key
                    })
                    .unwrap_or(false);
                if !outer_derives_position {
                    if let Some(event) = operation.assertions.iter().find(|event| {
                        matches!(
                            &event.payload,
                            EventPayloadV1::Config(fields)
                                if fields.get("event_kind").map(String::as_str)
                                    == Some("user_position_created")
                        )
                    }) {
                        limitations.push(
                            V1DecisionGradeRetainedEventLimitationV1::PositionOwnerEventAttribution {
                                creation_event_id: event.event_id.clone(),
                                creation_raw_payload_sha256: event.raw_payload_sha256.clone(),
                                outer_actor,
                                event_owner: event_owner.to_string(),
                                position: position_key.to_string(),
                                position_bump: bump,
                            },
                        );
                    }
                }
            }
        }
    }
    limitations
}

fn ensure_position(
    engine: &mut V1NativeEngine,
    operation: &CanonicalOperationV1,
    position_key: Pubkey,
    deposit: bool,
) -> Result<(), V1FactualReplayError> {
    if engine.position(&position_key).is_some() {
        return Ok(());
    }
    if !deposit {
        return Err(V1EngineError::MissingPosition(position_key).into());
    }
    let outer_owner = operation
        .actor
        .as_ref()
        .map(|owner| parse_pubkey("operation.actor", owner))
        .transpose()?;
    let outer_derivation = outer_owner.map(|owner| {
        let (expected, bump) = Pubkey::find_program_address(
            &[
                POSITION_SEED_PREFIX,
                engine.pair_key().as_ref(),
                owner.as_ref(),
            ],
            &omnipair::ID,
        );
        (owner, expected, bump)
    });
    let (owner, bump) = match outer_derivation {
        Some((owner, expected, bump)) if expected == position_key => (owner, bump),
        outer => {
            if engine.post_state_correction_policy()
                == V1PostStateCorrectionPolicyV1::DecisionGradeEventAndTokenCpi
            {
                decision_grade_position_creation_owner(operation, engine.pair_key(), position_key)?
            } else if let Some((_, expected, _)) = outer {
                return Err(V1FactualReplayError::PositionPdaMismatch {
                    expected,
                    actual: position_key,
                });
            } else {
                return Err(V1FactualReplayError::MissingPositionOwner {
                    position: position_key,
                });
            }
        }
    };
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
        .map_err(|error| V1FactualReplayError::PositionInitialization(error.to_string()))?;
    engine.insert_decoded_position(position_key, position)?;
    Ok(())
}

fn decision_grade_position_creation_owner(
    operation: &CanonicalOperationV1,
    pair: Pubkey,
    expected_position: Pubkey,
) -> Result<(Pubkey, u8), V1FactualReplayError> {
    let creation_events: Vec<_> = operation
        .assertions
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayloadV1::Config(fields)
                if fields.get("event_kind").map(String::as_str)
                    == Some("user_position_created") =>
            {
                Some((event, fields))
            }
            _ => None,
        })
        .collect();
    let position_events: Vec<_> = operation
        .assertions
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayloadV1::Position(payload) => Some((event, payload)),
            _ => None,
        })
        .collect();
    if creation_events.len() != 1 || position_events.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec![
                "unique_direct_child_user_position_created_assertion".to_owned(),
                "unique_direct_child_position_assertion".to_owned(),
            ],
        });
    }
    let (creation_event, fields) = creation_events[0];
    let (position_event, position_payload) = position_events[0];
    let creation_position = parse_pubkey(
        "user_position_created.position",
        required_field(fields, "position")?,
    )?;
    let asserted_position =
        parse_pubkey("position_assertion.position", &position_payload.position)?;
    let owner_text =
        creation_event
            .actor
            .as_ref()
            .ok_or(V1FactualReplayError::MissingPositionOwner {
                position: expected_position,
            })?;
    if creation_position != expected_position
        || asserted_position != expected_position
        || position_event.actor.as_ref() != Some(owner_text)
    {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: creation_event.event_id.clone(),
            field: "user_position_created/position join".to_owned(),
            expected: format!("position={expected_position},owner={owner_text}"),
            actual: format!(
                "creation_position={creation_position},asserted_position={asserted_position},position_actor={:?}",
                position_event.actor
            ),
        });
    }
    let owner = parse_pubkey("user_position_created.actor", owner_text)?;
    let (derived, bump) = Pubkey::find_program_address(
        &[POSITION_SEED_PREFIX, pair.as_ref(), owner.as_ref()],
        &omnipair::ID,
    );
    if derived != expected_position {
        return Err(V1FactualReplayError::PositionPdaMismatch {
            expected: derived,
            actual: expected_position,
        });
    }
    Ok((owner, bump))
}

fn unique_position_factor_witness(
    operation: &CanonicalOperationV1,
    expected_position: Pubkey,
) -> Result<V1PositionCollateralFactorWitnessV1, V1FactualReplayError> {
    let position_assertions: Vec<_> = operation
        .assertions
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayloadV1::Position(payload) => Some((event, payload)),
            _ => None,
        })
        .collect();
    if position_assertions.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec!["unique_direct_child_position_assertion".to_owned()],
        });
    }
    // The normalizer places only verified direct-child events in an
    // operation's assertions. Bind that unique event to the instruction's
    // exact position account before its CF fields may select a transition.
    let (event, payload) = position_assertions[0];
    let asserted_position = parse_pubkey("position_assertion.position", &payload.position)?;
    if asserted_position != expected_position {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: event.event_id.clone(),
            field: "position_assertion.position".to_owned(),
            expected: expected_position.to_string(),
            actual: asserted_position.to_string(),
        });
    }
    Ok(V1PositionCollateralFactorWitnessV1 {
        collateral0_max_cf_bps: payload.collateral0_max_cf_bps,
        collateral1_max_cf_bps: payload.collateral1_max_cf_bps,
        collateral0_liquidation_cf_bps: payload.collateral0_liquidation_cf_bps,
        collateral1_liquidation_cf_bps: payload.collateral1_liquidation_cf_bps,
    })
}

fn unique_collateral_withdrawal_witness(
    operation: &CanonicalOperationV1,
    expected_position: Pubkey,
    collateral_token: V1TokenSideV1,
    expected_amount: u64,
) -> Result<V1CollateralWithdrawalWitnessV1, V1FactualReplayError> {
    let OperationPayloadV1::CollateralDelta {
        vault_amount,
        counterparty_amount,
        ..
    } = &operation.payload
    else {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "collateral withdrawal witness requires collateral_delta".to_owned(),
        ));
    };
    let delta_events: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| matches!(&event.payload, EventPayloadV1::CollateralDelta(_)))
        .collect();
    let position_events: Vec<_> = operation
        .assertions
        .iter()
        .filter(|event| matches!(&event.payload, EventPayloadV1::Position(_)))
        .collect();
    if delta_events.len() != 1 || position_events.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec![
                "unique_direct_child_collateral_delta_assertion".to_owned(),
                "unique_direct_child_position_assertion".to_owned(),
            ],
        });
    }
    let delta_event = delta_events[0];
    let position_event = position_events[0];
    let EventPayloadV1::CollateralDelta(delta) = &delta_event.payload else {
        unreachable!("filtered to collateral delta")
    };
    let EventPayloadV1::Position(position) = &position_event.payload else {
        unreachable!("filtered to position")
    };
    let delta0 = parse_i128("collateral_delta.amount0", &delta.amount0)?;
    let delta1 = parse_i128("collateral_delta.amount1", &delta.amount1)?;
    let expected_signed = -i128::from(expected_amount);
    let delta_matches = match collateral_token {
        V1TokenSideV1::Token0 => delta0 == expected_signed && delta1 == 0,
        V1TokenSideV1::Token1 => delta0 == 0 && delta1 == expected_signed,
    };
    let event_position = parse_pubkey("position_assertion.position", &position.position)?;
    let token_cpi_collateral_vault_debit = parse_u64("collateral.vault_amount", vault_amount)?;
    let token_cpi_recipient_credit =
        parse_u64("collateral.counterparty_amount", counterparty_amount)?;
    if !delta_matches
        || event_position != expected_position
        || token_cpi_collateral_vault_debit != expected_amount
        || token_cpi_recipient_credit != expected_amount
    {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: delta_event.event_id.clone(),
            field: "collateral withdrawal event/position/token_cpi tuple".to_owned(),
            expected: format!(
                "position={expected_position},side={collateral_token:?},amount={expected_amount}"
            ),
            actual: format!(
                "position={event_position},delta0={delta0},delta1={delta1},vault={token_cpi_collateral_vault_debit},recipient={token_cpi_recipient_credit}"
            ),
        });
    }
    Ok(V1CollateralWithdrawalWitnessV1 {
        event_position,
        event_amount: expected_amount,
        token_cpi_collateral_vault_debit,
        token_cpi_recipient_credit,
        event_collateral0: parse_u64("position_assertion.collateral0", &position.collateral0)?,
        event_collateral1: parse_u64("position_assertion.collateral1", &position.collateral1)?,
        event_debt0_shares: parse_u128("position_assertion.debt0_shares", &position.debt0_shares)?,
        event_debt1_shares: parse_u128("position_assertion.debt1_shares", &position.debt1_shares)?,
        event_collateral0_max_cf_bps: position.collateral0_max_cf_bps,
        event_collateral1_max_cf_bps: position.collateral1_max_cf_bps,
        event_collateral0_liquidation_cf_bps: position.collateral0_liquidation_cf_bps,
        event_collateral1_liquidation_cf_bps: position.collateral1_liquidation_cf_bps,
    })
}

fn apply_config(
    engine: &mut V1NativeEngine,
    fields: &BTreeMap<String, String>,
) -> Result<(), V1FactualReplayError> {
    let kind = required_field(fields, "instruction_kind")?;
    match kind {
        "set_pair_reduce_only" => {
            engine.set_pair_reduce_only(parse_bool(required_field(fields, "args.reduce_only")?)?);
        }
        "set_global_reduce_only" => {
            engine.set_global_reduce_only(parse_bool(required_field(fields, "args.reduce_only")?)?);
        }
        "update_futarchy_authority" => {
            engine.set_authority(parse_config_pubkey(
                "args.new_authority",
                required_field(fields, "args.new_authority")?,
            )?);
        }
        "update_protocol_revenue" => {
            let swap_bps = optional_u16(fields, "args.swap_bps")?;
            let interest_bps = optional_u16(fields, "args.interest_bps")?;
            engine.set_revenue_share(swap_bps, interest_bps)?;
            let distribution_keys = [
                "args.revenue_distribution.futarchy_treasury_bps",
                "args.revenue_distribution.buybacks_vault_bps",
                "args.revenue_distribution.team_treasury_bps",
            ];
            if distribution_keys
                .iter()
                .any(|key| fields.contains_key(*key))
            {
                if !distribution_keys
                    .iter()
                    .all(|key| fields.contains_key(*key))
                {
                    return Err(V1FactualReplayError::MissingCanonicalFields {
                        operation_id: kind.to_owned(),
                        fields: distribution_keys
                            .iter()
                            .filter(|key| !fields.contains_key(**key))
                            .map(|key| (*key).to_owned())
                            .collect(),
                    });
                }
                engine.set_revenue_distribution(RevenueDistribution {
                    futarchy_treasury_bps: parse_u16_text(
                        distribution_keys[0],
                        required_field(fields, distribution_keys[0])?,
                    )?,
                    buybacks_vault_bps: parse_u16_text(
                        distribution_keys[1],
                        required_field(fields, distribution_keys[1])?,
                    )?,
                    team_treasury_bps: parse_u16_text(
                        distribution_keys[2],
                        required_field(fields, distribution_keys[2])?,
                    )?,
                })?;
            }
            if swap_bps.is_none()
                && interest_bps.is_none()
                && !distribution_keys
                    .iter()
                    .any(|key| fields.contains_key(*key))
            {
                return Err(V1FactualReplayError::UnsupportedConfig(kind.to_owned()));
            }
        }
        "update_revenue_recipients" => {
            let before = engine.checkpoint().authority;
            let futarchy_treasury =
                optional_pubkey(fields, "args.futarchy_treasury", before.futarchy_treasury)?;
            let buybacks_vault =
                optional_pubkey(fields, "args.buybacks_vault", before.buybacks_vault)?;
            let team_treasury =
                optional_pubkey(fields, "args.team_treasury", before.team_treasury)?;
            if futarchy_treasury == before.futarchy_treasury
                && buybacks_vault == before.buybacks_vault
                && team_treasury == before.team_treasury
                && !fields.keys().any(|key| key.starts_with("args."))
            {
                return Err(V1FactualReplayError::UnsupportedConfig(kind.to_owned()));
            }
            engine.set_revenue_recipients(RevenueRecipients {
                futarchy_treasury,
                buybacks_vault,
                team_treasury,
            });
        }
        // Initialization is imported as a decoded account checkpoint. Rate
        // model creation/selection requires a fully decoded native account.
        "init_futarchy_authority" | "create_rate_model" | "set_pair_rate_model" => {
            return Err(V1FactualReplayError::UnsupportedConfig(kind.to_owned()));
        }
        unsupported => {
            return Err(V1FactualReplayError::UnsupportedConfig(
                unsupported.to_owned(),
            ));
        }
    }
    Ok(())
}

fn reconcile_payload(
    operation: &CanonicalOperationV1,
    receipt: &V1AppliedReceiptV1,
    after: &V1NativeCheckpointV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
) -> Result<(), V1FactualReplayError> {
    let source = format!("operation:{}", operation.operation_id);
    match (&operation.payload, receipt) {
        (
            OperationPayloadV1::InitializeMarket {
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
                ..
            },
            V1AppliedReceiptV1::Initialize(native),
        ) => {
            push_text_eq(
                output,
                &source,
                "token0",
                token0,
                &native.token0.to_string(),
            )?;
            push_text_eq(
                output,
                &source,
                "token1",
                token1,
                &native.token1.to_string(),
            )?;
            push_u128_eq(
                output,
                &source,
                "token0_decimals",
                *token0_decimals as u128,
                after.pair.token0_decimals as u128,
            )?;
            push_u128_eq(
                output,
                &source,
                "token1_decimals",
                *token1_decimals as u128,
                after.pair.token1_decimals as u128,
            )?;
            push_token_eq(
                output,
                &source,
                "gross_amount0",
                gross_amount0,
                native.gross_amount0,
            )?;
            push_token_eq(
                output,
                &source,
                "gross_amount1",
                gross_amount1,
                native.gross_amount1,
            )?;
            push_token_eq(
                output,
                &source,
                "vault_credit0",
                vault_credit0,
                native.vault_credit0,
            )?;
            push_token_eq(
                output,
                &source,
                "vault_credit1",
                vault_credit1,
                native.vault_credit1,
            )?;
            push_token_eq(output, &source, "minted_lp", minted_lp, native.minted_lp)?;
            ensure_minimum_guard(
                output,
                &source,
                "min_liquidity_out_guard",
                min_liquidity_out,
                native.minted_lp,
            )?;
        }
        (
            OperationPayloadV1::AddLiquidity {
                minted_lp,
                vault_credit0,
                vault_credit1,
                min_liquidity_out,
                ..
            },
            V1AppliedReceiptV1::AddLiquidity(native),
        ) => {
            push_token_eq(
                output,
                &source,
                "minted_lp",
                minted_lp,
                native.liquidity_minted,
            )?;
            push_token_eq(
                output,
                &source,
                "vault_credit0",
                vault_credit0,
                native.vault_credit0,
            )?;
            push_token_eq(
                output,
                &source,
                "vault_credit1",
                vault_credit1,
                native.vault_credit1,
            )?;
            ensure_minimum_guard(
                output,
                &source,
                "min_liquidity_out_guard",
                min_liquidity_out,
                native.liquidity_minted,
            )?;
        }
        (
            OperationPayloadV1::RemoveLiquidity {
                requested_lp,
                burned_lp,
                vault_debit0,
                vault_debit1,
                recipient_credit0,
                recipient_credit1,
                min_amount0_out,
                min_amount1_out,
                ..
            },
            V1AppliedReceiptV1::RemoveLiquidity(native),
        ) => {
            push_token_eq(
                output,
                &source,
                "requested_lp",
                requested_lp,
                native.liquidity_burned,
            )?;
            push_token_eq(
                output,
                &source,
                "burned_lp",
                burned_lp,
                native.liquidity_burned,
            )?;
            push_token_eq(
                output,
                &source,
                "vault_debit0",
                vault_debit0,
                native.amount0_out,
            )?;
            push_token_eq(
                output,
                &source,
                "vault_debit1",
                vault_debit1,
                native.amount1_out,
            )?;
            push_token_eq(
                output,
                &source,
                "recipient_credit0",
                recipient_credit0,
                native.recipient_credit0,
            )?;
            push_token_eq(
                output,
                &source,
                "recipient_credit1",
                recipient_credit1,
                native.recipient_credit1,
            )?;
            ensure_minimum_guard(
                output,
                &source,
                "min_amount0_out_guard",
                min_amount0_out,
                native.amount0_out,
            )?;
            ensure_minimum_guard(
                output,
                &source,
                "min_amount1_out_guard",
                min_amount1_out,
                native.amount1_out,
            )?;
        }
        (
            OperationPayloadV1::SwapExactIn {
                gross_amount_in,
                vault_credit_in,
                vault_debit_out,
                recipient_credit_out,
                ..
            },
            V1AppliedReceiptV1::Swap(native),
        ) => {
            push_token_eq(
                output,
                &source,
                "gross_amount_in",
                gross_amount_in,
                native.gross_amount_in,
            )?;
            push_token_eq(
                output,
                &source,
                "vault_credit_in",
                vault_credit_in,
                native.vault_credit_in,
            )?;
            push_token_eq(
                output,
                &source,
                "vault_debit_out",
                vault_debit_out,
                native.vault_debit_out,
            )?;
            push_token_eq(
                output,
                &source,
                "recipient_credit_out",
                recipient_credit_out,
                native.recipient_credit_out,
            )?;
        }
        (
            OperationPayloadV1::CollateralDelta {
                amount0,
                amount1,
                gross_transfer_amount,
                vault_amount,
                counterparty_amount,
                requested_amount: _,
                ..
            },
            V1AppliedReceiptV1::Collateral(native),
        ) => {
            let (expected0, expected1) = signed_collateral_receipt(native);
            push_signed_eq(output, &source, "amount0", amount0, expected0)?;
            push_signed_eq(output, &source, "amount1", amount1, expected1)?;
            push_token_eq(
                output,
                &source,
                "gross_transfer_amount",
                gross_transfer_amount,
                native.accounted_amount,
            )?;
            push_guard_passed(output, &source, "requested_amount_resolution")?;
            push_token_eq(
                output,
                &source,
                "vault_amount",
                vault_amount,
                native.collateral_vault_amount,
            )?;
            push_token_eq(
                output,
                &source,
                "counterparty_amount",
                counterparty_amount,
                native.counterparty_amount,
            )?;
        }
        (
            OperationPayloadV1::Borrow {
                gross_amount,
                recipient_credit,
                ..
            },
            V1AppliedReceiptV1::Borrow(native),
        ) => {
            push_token_eq(
                output,
                &source,
                "gross_amount",
                gross_amount,
                native.borrowed_amount,
            )?;
            push_token_eq(
                output,
                &source,
                "recipient_credit",
                recipient_credit,
                native.recipient_credit,
            )?;
            push_guard_passed(output, &source, "requested_amount_resolution")?;
        }
        (
            OperationPayloadV1::Repay {
                gross_amount,
                vault_credit,
                ..
            },
            V1AppliedReceiptV1::Repay(native),
        ) => {
            push_token_eq(
                output,
                &source,
                "gross_amount",
                gross_amount,
                native.repaid_amount,
            )?;
            push_token_eq(
                output,
                &source,
                "vault_credit",
                vault_credit,
                native.vault_credit,
            )?;
            push_guard_passed(output, &source, "requested_amount_resolution")?;
        }
        (
            OperationPayloadV1::ClaimProtocolRevenue { amount0, amount1 },
            V1AppliedReceiptV1::ProtocolClaim(native),
        ) => {
            push_token_eq(output, &source, "amount0", amount0, native.claimable0)?;
            push_token_eq(output, &source, "amount1", amount1, native.claimable1)?;
        }
        (OperationPayloadV1::UpdateConfig { fields }, V1AppliedReceiptV1::Config) => {
            reconcile_config_payload(fields, after, output, &source)?;
        }
        (OperationPayloadV1::ClockOnly, V1AppliedReceiptV1::Clock(native)) => {
            push_u128_eq(
                output,
                &source,
                "slot",
                operation.order.slot as u128,
                native.slot_after as u128,
            )?;
        }
        (OperationPayloadV1::ClockOnly, V1AppliedReceiptV1::Liquidation(native)) => {
            push_u128_eq(
                output,
                &source,
                "slot",
                operation.order.slot as u128,
                native.accrual.slot_after as u128,
            )?;
            push_guard_passed(output, &source, "zero_raw_liquidation_resolution")?;
        }
        // Liquidation and flashloan currently fail closed before a receipt can
        // exist. These arms document the enriched-schema behavior.
        (
            OperationPayloadV1::Liquidate {
                requested_repay,
                collateral_vault_debit,
                reserve_vault_credit,
                liquidator_recipient_credit,
                ..
            },
            V1AppliedReceiptV1::Liquidation(native),
        ) => {
            push_token_eq(
                output,
                &source,
                "requested_repay",
                requested_repay,
                native.debt_written_off,
            )?;
            push_token_eq(
                output,
                &source,
                "collateral_vault_debit",
                collateral_vault_debit,
                native.collateral_seized,
            )?;
            push_token_eq(
                output,
                &source,
                "reserve_vault_credit",
                reserve_vault_credit,
                native.reserve_vault_credit,
            )?;
            push_token_eq(
                output,
                &source,
                "liquidator_recipient_credit",
                liquidator_recipient_credit,
                native.liquidator_recipient_credit,
            )?;
        }
        (
            OperationPayloadV1::Flashloan {
                amount0,
                amount1,
                fee0,
                fee1,
                net_vault_credit0,
                net_vault_credit1,
            },
            V1AppliedReceiptV1::Flashloan(native),
        ) => {
            push_token_eq(output, &source, "amount0", amount0, native.amount0)?;
            push_token_eq(output, &source, "amount1", amount1, native.amount1)?;
            push_token_eq(output, &source, "fee0", fee0, native.fee0)?;
            push_token_eq(output, &source, "fee1", fee1, native.fee1)?;
            push_token_eq(
                output,
                &source,
                "net_vault_credit0",
                net_vault_credit0,
                native.net_vault_credit0,
            )?;
            push_token_eq(
                output,
                &source,
                "net_vault_credit1",
                net_vault_credit1,
                native.net_vault_credit1,
            )?;
        }
        _ => {
            return Err(V1FactualReplayError::UnsupportedConfig(format!(
                "payload/receipt mismatch for {}",
                operation.operation_id
            )));
        }
    }
    Ok(())
}

fn reconcile_assertions(
    operation: &CanonicalOperationV1,
    before: Option<&V1NativeCheckpointV1>,
    after: &V1NativeCheckpointV1,
    receipt: &V1AppliedReceiptV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    retain_lp_derived_entitlements: bool,
) -> Result<(), V1FactualReplayError> {
    for event in &operation.assertions {
        let source = format!("event:{}", event.event_id);
        match &event.payload {
            EventPayloadV1::Swap(payload) => {
                let V1AppliedReceiptV1::Swap(native) = receipt else {
                    return unsupported_assertion(event, "swap");
                };
                push_u128_eq(
                    output,
                    &source,
                    "token0_in",
                    payload.token0_in as u128,
                    (native.token_in == V1TokenSideV1::Token0) as u128,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "amount_in",
                    &payload.amount_in,
                    native.gross_amount_in,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "amount_out",
                    &payload.amount_out,
                    native.amount_out,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "amount_in_after_fee",
                    &payload.amount_in_after_fee,
                    native.amount_in_after_fee,
                )?;
                push_token_eq(output, &source, "lp_fee", &payload.lp_fee, native.lp_fee)?;
                push_token_eq(
                    output,
                    &source,
                    "protocol_fee",
                    &payload.protocol_fee,
                    native.protocol_fee,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "reserve0_after",
                    &payload.reserve0_after,
                    native.reserve0_after,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "reserve1_after",
                    &payload.reserve1_after,
                    native.reserve1_after,
                )?;
            }
            EventPayloadV1::Liquidity(payload) => {
                reconcile_liquidity_event(payload, receipt, output, &source)?
            }
            EventPayloadV1::LpPosition(payload) => {
                if !retain_lp_derived_entitlements {
                    let supply = after.pair.total_supply as u128;
                    if supply == 0 {
                        return Err(V1FactualReplayError::UnsupportedAssertion {
                            event_id: event.event_id.clone(),
                            kind: "lp_position_with_zero_supply".to_owned(),
                        });
                    }
                    let lp = parse_u128("lp_position.lp_amount", &payload.lp_amount)?;
                    let amount0 = lp
                        .checked_mul(after.pair.reserve0 as u128)
                        .and_then(|v| v.checked_div(supply))
                        .ok_or(V1EngineError::ArithmeticOverflow("LP assertion token0"))?;
                    let amount1 = lp
                        .checked_mul(after.pair.reserve1 as u128)
                        .and_then(|v| v.checked_div(supply))
                        .ok_or(V1EngineError::ArithmeticOverflow("LP assertion token1"))?;
                    push_token_u128_eq(output, &source, "amount0", &payload.amount0, amount0)?;
                    push_token_u128_eq(output, &source, "amount1", &payload.amount1, amount1)?;
                }
                if let Some(cash) = &payload.cash_reserve0 {
                    push_token_eq(
                        output,
                        &source,
                        "cash_reserve0",
                        cash,
                        after.pair.cash_reserve0,
                    )?;
                }
                if let Some(cash) = &payload.cash_reserve1 {
                    push_token_eq(
                        output,
                        &source,
                        "cash_reserve1",
                        cash,
                        after.pair.cash_reserve1,
                    )?;
                }
            }
            EventPayloadV1::CollateralDelta(payload) => match receipt {
                V1AppliedReceiptV1::Collateral(native) => {
                    let (amount0, amount1) = signed_collateral_receipt(native);
                    reconcile_pair_delta(payload, amount0, amount1, output, &source)?;
                }
                V1AppliedReceiptV1::Clock(_) if retain_lp_derived_entitlements => {
                    let collateral_delta_count = operation
                        .assertions
                        .iter()
                        .filter(|candidate| {
                            matches!(&candidate.payload, EventPayloadV1::CollateralDelta(_))
                        })
                        .count();
                    let position_count = operation
                        .assertions
                        .iter()
                        .filter(|candidate| {
                            matches!(&candidate.payload, EventPayloadV1::Position(_))
                        })
                        .count();
                    if collateral_delta_count != 1 || position_count != 1 {
                        return unsupported_assertion(
                            event,
                            "clock_only_non_unique_collateral_lifecycle",
                        );
                    }
                    reconcile_pair_delta(payload, 0, 0, output, &source)?;
                }
                _ => return unsupported_assertion(event, "collateral_delta"),
            },
            EventPayloadV1::DebtDelta(payload) => {
                if matches!(receipt, V1AppliedReceiptV1::Clock(_)) && retain_lp_derived_entitlements
                {
                    validate_clock_zero_liquidation_assertions(operation)?;
                    reconcile_pair_delta(payload, 0, 0, output, &source)?;
                } else {
                    let (amount0, amount1) =
                        signed_debt_receipt(receipt, operation).ok_or_else(|| {
                            V1FactualReplayError::UnsupportedAssertion {
                                event_id: event.event_id.clone(),
                                kind: "debt_delta".to_owned(),
                            }
                        })?;
                    reconcile_pair_delta(payload, amount0, amount1, output, &source)?;
                }
            }
            EventPayloadV1::Position(payload) => {
                reconcile_position_event(payload, after, output, &source)?
            }
            EventPayloadV1::Liquidation(payload) => match receipt {
                V1AppliedReceiptV1::Liquidation(native) => reconcile_liquidation_event(
                    payload,
                    native,
                    output,
                    &source,
                    retain_lp_derived_entitlements,
                )?,
                V1AppliedReceiptV1::Clock(_) if retain_lp_derived_entitlements => {
                    validate_clock_zero_liquidation_assertions(operation)?;
                    push_token_eq(
                        output,
                        &source,
                        "collateral0_liquidated",
                        &payload.collateral0_liquidated,
                        0,
                    )?;
                    push_token_eq(
                        output,
                        &source,
                        "collateral1_liquidated",
                        &payload.collateral1_liquidated,
                        0,
                    )?;
                    push_token_eq(
                        output,
                        &source,
                        "debt0_liquidated",
                        &payload.debt0_liquidated,
                        0,
                    )?;
                    push_token_eq(
                        output,
                        &source,
                        "debt1_liquidated",
                        &payload.debt1_liquidated,
                        0,
                    )?;
                    push_token_eq(
                        output,
                        &source,
                        "liquidation_bonus",
                        &payload.liquidation_bonus,
                        0,
                    )?;
                    push_token_u128_eq(output, &source, "shortfall", &payload.shortfall, 0)?;
                    push_u128_eq(
                        output,
                        &source,
                        "k_unchanged",
                        parse_u128("zero_liquidation.k_before", &payload.k_before)?,
                        parse_u128("zero_liquidation.k_after", &payload.k_after)?,
                    )?;
                }
                _ => return unsupported_assertion(event, "liquidation"),
            },
            EventPayloadV1::PairUpdate(payload) => {
                let accrual = receipt.accrual().ok_or_else(|| {
                    V1FactualReplayError::UnsupportedAssertion {
                        event_id: event.event_id.clone(),
                        kind: "pair_update".to_owned(),
                    }
                })?;
                let before = before.ok_or_else(|| V1FactualReplayError::UnsupportedAssertion {
                    event_id: event.event_id.clone(),
                    kind: "pair_update_during_initialize".to_owned(),
                })?;
                reconcile_pair_update(payload, before, after, accrual, output, &source)?;
            }
            EventPayloadV1::Flashloan(payload) => {
                let V1AppliedReceiptV1::Flashloan(native) = receipt else {
                    return unsupported_assertion(event, "flashloan");
                };
                push_token_eq(output, &source, "amount0", &payload.amount0, native.amount0)?;
                push_token_eq(output, &source, "amount1", &payload.amount1, native.amount1)?;
                push_token_eq(output, &source, "fee0", &payload.fee0, native.fee0)?;
                push_token_eq(output, &source, "fee1", &payload.fee1, native.fee1)?;
            }
            EventPayloadV1::ProtocolFeeClaim(payload) => {
                let V1AppliedReceiptV1::ProtocolClaim(native) = receipt else {
                    return unsupported_assertion(event, "protocol_fee_claim");
                };
                push_text_eq(
                    output,
                    &source,
                    "token0",
                    &payload.token0,
                    &after.pair.token0.to_string(),
                )?;
                push_text_eq(
                    output,
                    &source,
                    "token1",
                    &payload.token1,
                    &after.pair.token1.to_string(),
                )?;
                push_token_eq(
                    output,
                    &source,
                    "futarchy_treasury_amount0",
                    &payload.futarchy_treasury_amount0,
                    native.futarchy_amount0,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "futarchy_treasury_amount1",
                    &payload.futarchy_treasury_amount1,
                    native.futarchy_amount1,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "buybacks_vault_amount0",
                    &payload.buybacks_vault_amount0,
                    native.buybacks_amount0,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "buybacks_vault_amount1",
                    &payload.buybacks_vault_amount1,
                    native.buybacks_amount1,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "team_treasury_amount0",
                    &payload.team_treasury_amount0,
                    native.team_amount0,
                )?;
                push_token_eq(
                    output,
                    &source,
                    "team_treasury_amount1",
                    &payload.team_treasury_amount1,
                    native.team_amount1,
                )?;
            }
            EventPayloadV1::Config(fields) => {
                reconcile_config_event(event, fields, operation, after, output, &source)?
            }
        }
    }
    Ok(())
}

fn reconcile_liquidity_event(
    payload: &crate::canonical::LiquidityPayloadV1,
    receipt: &V1AppliedReceiptV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    match (&payload.action, receipt) {
        (LiquidityAction::Mint, V1AppliedReceiptV1::Initialize(native)) => {
            push_token_eq(
                output,
                source,
                "amount0",
                &payload.amount0,
                native.gross_amount0,
            )?;
            push_token_eq(
                output,
                source,
                "amount1",
                &payload.amount1,
                native.gross_amount1,
            )?;
            push_token_eq(
                output,
                source,
                "liquidity",
                &payload.liquidity,
                native.minted_lp,
            )?;
        }
        (LiquidityAction::Mint, V1AppliedReceiptV1::AddLiquidity(native)) => {
            push_token_eq(
                output,
                source,
                "amount0",
                &payload.amount0,
                native.amount0_used,
            )?;
            push_token_eq(
                output,
                source,
                "amount1",
                &payload.amount1,
                native.amount1_used,
            )?;
            push_token_eq(
                output,
                source,
                "liquidity",
                &payload.liquidity,
                native.liquidity_minted,
            )?;
        }
        (LiquidityAction::Burn, V1AppliedReceiptV1::RemoveLiquidity(native)) => {
            push_token_eq(
                output,
                source,
                "amount0",
                &payload.amount0,
                native.amount0_out,
            )?;
            push_token_eq(
                output,
                source,
                "amount1",
                &payload.amount1,
                native.amount1_out,
            )?;
            push_token_eq(
                output,
                source,
                "liquidity",
                &payload.liquidity,
                native.liquidity_burned,
            )?;
        }
        _ => {
            return Err(V1FactualReplayError::UnsupportedAssertion {
                event_id: source.to_owned(),
                kind: "liquidity action does not match operation".to_owned(),
            });
        }
    }
    Ok(())
}

fn reconcile_pair_delta(
    payload: &PairDeltaPayloadV1,
    amount0: i128,
    amount1: i128,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    push_signed_eq(output, source, "amount0", &payload.amount0, amount0)?;
    push_signed_eq(output, source, "amount1", &payload.amount1, amount1)?;
    Ok(())
}

fn reconcile_position_event(
    payload: &crate::canonical::PositionPayloadV1,
    after: &V1NativeCheckpointV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    let key = parse_pubkey("position_assertion.position", &payload.position)?;
    let native = after
        .positions
        .get(&key)
        .ok_or(V1EngineError::MissingPosition(key))?;
    push_token_eq(
        output,
        source,
        "collateral0",
        &payload.collateral0,
        native.collateral0,
    )?;
    push_token_eq(
        output,
        source,
        "collateral1",
        &payload.collateral1,
        native.collateral1,
    )?;
    push_token_u128_eq(
        output,
        source,
        "debt0_shares",
        &payload.debt0_shares,
        native.debt0_shares,
    )?;
    push_token_u128_eq(
        output,
        source,
        "debt1_shares",
        &payload.debt1_shares,
        native.debt1_shares,
    )?;
    let max0 = max_cf(native.collateral0_liquidation_cf_bps);
    let max1 = max_cf(native.collateral1_liquidation_cf_bps);
    push_u128_eq(
        output,
        source,
        "collateral0_max_cf_bps",
        payload.collateral0_max_cf_bps as u128,
        max0 as u128,
    )?;
    push_u128_eq(
        output,
        source,
        "collateral1_max_cf_bps",
        payload.collateral1_max_cf_bps as u128,
        max1 as u128,
    )?;
    push_u128_eq(
        output,
        source,
        "collateral0_liquidation_cf_bps",
        payload.collateral0_liquidation_cf_bps as u128,
        native.collateral0_liquidation_cf_bps as u128,
    )?;
    push_u128_eq(
        output,
        source,
        "collateral1_liquidation_cf_bps",
        payload.collateral1_liquidation_cf_bps as u128,
        native.collateral1_liquidation_cf_bps as u128,
    )?;
    Ok(())
}

fn validate_clock_zero_liquidation_assertions(
    operation: &CanonicalOperationV1,
) -> Result<(), V1FactualReplayError> {
    if !matches!(operation.payload, OperationPayloadV1::ClockOnly) {
        return Err(V1FactualReplayError::UnsupportedConfig(
            "zero liquidation lifecycle requires clock_only".to_owned(),
        ));
    }
    let debt_events: Vec<_> = operation
        .assertions
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayloadV1::DebtDelta(payload) => Some((event, payload)),
            _ => None,
        })
        .collect();
    let position_events: Vec<_> = operation
        .assertions
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayloadV1::Position(payload) => Some((event, payload)),
            _ => None,
        })
        .collect();
    let liquidation_events: Vec<_> = operation
        .assertions
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayloadV1::Liquidation(payload) => Some((event, payload)),
            _ => None,
        })
        .collect();
    if debt_events.len() != 1 || position_events.len() != 1 || liquidation_events.len() != 1 {
        return Err(V1FactualReplayError::MissingCanonicalFields {
            operation_id: operation.operation_id.clone(),
            fields: vec![
                "unique_zero_debt_delta_assertion".to_owned(),
                "unique_position_assertion".to_owned(),
                "unique_zero_liquidation_assertion".to_owned(),
            ],
        });
    }
    let (debt_event, debt) = debt_events[0];
    let (_, position) = position_events[0];
    let (liquidation_event, liquidation) = liquidation_events[0];
    let amount0 = parse_i128("zero_debt_delta.amount0", &debt.amount0)?;
    let amount1 = parse_i128("zero_debt_delta.amount1", &debt.amount1)?;
    let position_key = parse_pubkey("zero_liquidation.position", &position.position)?;
    let liquidation_position =
        parse_pubkey("zero_liquidation.event_position", &liquidation.position)?;
    let k_before = parse_u128("zero_liquidation.k_before", &liquidation.k_before)?;
    let k_after = parse_u128("zero_liquidation.k_after", &liquidation.k_after)?;
    if amount0 != 0
        || amount1 != 0
        || position_key != liquidation_position
        || parse_u64(
            "zero_liquidation.collateral0",
            &liquidation.collateral0_liquidated,
        )? != 0
        || parse_u64(
            "zero_liquidation.collateral1",
            &liquidation.collateral1_liquidated,
        )? != 0
        || parse_u64("zero_liquidation.debt0", &liquidation.debt0_liquidated)? != 0
        || parse_u64("zero_liquidation.debt1", &liquidation.debt1_liquidated)? != 0
        || parse_u64("zero_liquidation.bonus", &liquidation.liquidation_bonus)? != 0
        || parse_u128("zero_liquidation.shortfall", &liquidation.shortfall)? != 0
        || k_before != k_after
    {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: liquidation_event.event_id.clone(),
            field: "clock_only zero-liquidation lifecycle tuple".to_owned(),
            expected: format!("position={position_key},zero deltas/bonus/shortfall,unchanged k"),
            actual: format!(
                "debt_delta={amount0}/{amount1},liquidation_position={liquidation_position},k={k_before}/{k_after}"
            ),
        });
    }
    let _ = debt_event;
    Ok(())
}

fn decision_grade_clock_zero_liquidation_input(
    operation: &CanonicalOperationV1,
    engine: &V1NativeEngine,
) -> Result<V1LiquidationInputV1, V1FactualReplayError> {
    validate_clock_zero_liquidation_assertions(operation)?;
    let position_payload = operation
        .assertions
        .iter()
        .find_map(|event| match &event.payload {
            EventPayloadV1::Position(payload) => Some(payload),
            _ => None,
        })
        .expect("validated unique position event");
    let position_key = parse_pubkey(
        "clock_zero_liquidation.position",
        &position_payload.position,
    )?;
    let before = engine
        .position(&position_key)
        .ok_or(V1EngineError::MissingPosition(position_key))?;
    let post_collateral0 = parse_u64(
        "clock_zero_liquidation.collateral0",
        &position_payload.collateral0,
    )?;
    let post_collateral1 = parse_u64(
        "clock_zero_liquidation.collateral1",
        &position_payload.collateral1,
    )?;
    let post_debt0_shares = parse_u128(
        "clock_zero_liquidation.debt0_shares",
        &position_payload.debt0_shares,
    )?;
    let post_debt1_shares = parse_u128(
        "clock_zero_liquidation.debt1_shares",
        &position_payload.debt1_shares,
    )?;
    if post_collateral0 != before.collateral0
        || post_collateral1 != before.collateral1
        || position_payload.collateral0_max_cf_bps != max_cf(before.collateral0_liquidation_cf_bps)
        || position_payload.collateral1_max_cf_bps != max_cf(before.collateral1_liquidation_cf_bps)
        || position_payload.collateral0_liquidation_cf_bps != before.collateral0_liquidation_cf_bps
        || position_payload.collateral1_liquidation_cf_bps != before.collateral1_liquidation_cf_bps
    {
        return Err(V1EngineError::CheckpointMismatchField(
            "clock_zero_liquidation.non_share_position_state",
        )
        .into());
    }
    let close_factor_burn = |shares: u128| -> Result<u128, V1FactualReplayError> {
        shares
            .checked_mul(CLOSE_FACTOR_BPS as u128)
            .and_then(|value| value.checked_add(BPS_DENOMINATOR as u128 - 1))
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or_else(|| {
                V1EngineError::ArithmeticOverflow("clock zero-liquidation close factor").into()
            })
    };
    let token0_matches = before.debt0_shares > 0
        && post_debt0_shares
            == before
                .debt0_shares
                .checked_sub(close_factor_burn(before.debt0_shares)?)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "clock zero-liquidation token0 shares",
                ))?
        && post_debt1_shares == before.debt1_shares;
    let token1_matches = before.debt1_shares > 0
        && post_debt1_shares
            == before
                .debt1_shares
                .checked_sub(close_factor_burn(before.debt1_shares)?)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "clock zero-liquidation token1 shares",
                ))?
        && post_debt0_shares == before.debt0_shares;
    let debt_token = match (token0_matches, token1_matches) {
        (true, false) => V1TokenSideV1::Token0,
        (false, true) => V1TokenSideV1::Token1,
        _ => {
            return Err(V1EngineError::CheckpointMismatchField(
                "clock_zero_liquidation.close_factor_share_delta",
            )
            .into());
        }
    };
    Ok(V1LiquidationInputV1 {
        slot: operation.order.slot,
        position: position_key,
        debt_token,
        reserve_vault_credit: 0,
        liquidator_recipient_credit: 0,
    })
}

fn reconcile_liquidation_event(
    payload: &crate::canonical::LiquidationPayloadV1,
    native: &V1LiquidationReceiptV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
    retain_derived_metadata: bool,
) -> Result<(), V1FactualReplayError> {
    let (collateral0, collateral1, debt0, debt1) = match native.debt_token {
        V1TokenSideV1::Token0 => (0, native.collateral_seized, native.debt_written_off, 0),
        V1TokenSideV1::Token1 => (native.collateral_seized, 0, 0, native.debt_written_off),
    };
    push_token_eq(
        output,
        source,
        "collateral0_liquidated",
        &payload.collateral0_liquidated,
        collateral0,
    )?;
    push_token_eq(
        output,
        source,
        "collateral1_liquidated",
        &payload.collateral1_liquidated,
        collateral1,
    )?;
    push_token_eq(
        output,
        source,
        "debt0_liquidated",
        &payload.debt0_liquidated,
        debt0,
    )?;
    push_token_eq(
        output,
        source,
        "debt1_liquidated",
        &payload.debt1_liquidated,
        debt1,
    )?;
    if !retain_derived_metadata {
        push_token_eq(
            output,
            source,
            "collateral_price_nad",
            &payload.collateral_price_nad,
            native.collateral_price_nad,
        )?;
    }
    push_token_u128_eq(
        output,
        source,
        "shortfall",
        &payload.shortfall,
        native.shortfall,
    )?;
    push_token_eq(
        output,
        source,
        "liquidation_bonus",
        &payload.liquidation_bonus,
        native.liquidator_incentive,
    )?;
    if !retain_derived_metadata {
        push_token_u128_eq(
            output,
            source,
            "k_before",
            &payload.k_before,
            native.k_before,
        )?;
        push_token_u128_eq(output, source, "k_after", &payload.k_after, native.k_after)?;
    }
    Ok(())
}

fn reconcile_pair_update(
    payload: &crate::canonical::PairUpdatePayloadV1,
    before: &V1NativeCheckpointV1,
    after: &V1NativeCheckpointV1,
    accrual: &V1InterestAccrualV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    push_token_eq(
        output,
        source,
        "price0_ema_nad",
        &payload.price0_ema_nad,
        after.pair.symmetric_price0_ema_nad,
    )?;
    push_token_eq(
        output,
        source,
        "price1_ema_nad",
        &payload.price1_ema_nad,
        after.pair.symmetric_price1_ema_nad,
    )?;
    push_token_eq(
        output,
        source,
        "rate0_nad",
        &payload.rate0_nad,
        accrual.rate0_nad,
    )?;
    push_token_eq(
        output,
        source,
        "rate1_nad",
        &payload.rate1_nad,
        accrual.rate1_nad,
    )?;
    push_token_u128_eq(
        output,
        source,
        "accrued_interest0",
        &payload.accrued_interest0,
        accrual.borrower_cost0 as u128,
    )?;
    push_token_u128_eq(
        output,
        source,
        "accrued_interest1",
        &payload.accrued_interest1,
        accrual.borrower_cost1 as u128,
    )?;
    push_token_eq(
        output,
        source,
        "lp_interest0",
        &payload.lp_interest0,
        accrual.lp_interest0,
    )?;
    push_token_eq(
        output,
        source,
        "lp_interest1",
        &payload.lp_interest1,
        accrual.lp_interest1,
    )?;
    push_token_eq(
        output,
        source,
        "protocol_interest0",
        &payload.protocol_interest0,
        accrual.protocol_interest0,
    )?;
    push_token_eq(
        output,
        source,
        "protocol_interest1",
        &payload.protocol_interest1,
        accrual.protocol_interest1,
    )?;
    let cash0 = before
        .pair
        .cash_reserve0
        .checked_sub(accrual.cash_covered_protocol_interest0)
        .ok_or(V1EngineError::ArithmeticOverflow("pair update cash0"))?;
    let cash1 = before
        .pair
        .cash_reserve1
        .checked_sub(accrual.cash_covered_protocol_interest1)
        .ok_or(V1EngineError::ArithmeticOverflow("pair update cash1"))?;
    let reserve0 = before
        .pair
        .reserve0
        .checked_add(accrual.lp_interest0)
        .and_then(|value| {
            value.checked_add(accrual.protocol_interest0 - accrual.cash_covered_protocol_interest0)
        })
        .ok_or(V1EngineError::ArithmeticOverflow("pair update reserve0"))?;
    let reserve1 = before
        .pair
        .reserve1
        .checked_add(accrual.lp_interest1)
        .and_then(|value| {
            value.checked_add(accrual.protocol_interest1 - accrual.cash_covered_protocol_interest1)
        })
        .ok_or(V1EngineError::ArithmeticOverflow("pair update reserve1"))?;
    push_token_eq(
        output,
        source,
        "cash_reserve0",
        &payload.cash_reserve0,
        cash0,
    )?;
    push_token_eq(
        output,
        source,
        "cash_reserve1",
        &payload.cash_reserve1,
        cash1,
    )?;
    push_token_eq(
        output,
        source,
        "reserve0_after_interest",
        &payload.reserve0_after_interest,
        reserve0,
    )?;
    push_token_eq(
        output,
        source,
        "reserve1_after_interest",
        &payload.reserve1_after_interest,
        reserve1,
    )?;
    Ok(())
}

fn reconcile_config_event(
    event: &CanonicalEventV1,
    fields: &BTreeMap<String, String>,
    operation: &CanonicalOperationV1,
    after: &V1NativeCheckpointV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    match fields.get("event_kind").map(String::as_str) {
        Some("user_position_created") => {
            let position = parse_pubkey(
                "user_position_created.position",
                required_field(fields, "position")?,
            )?;
            let native = after
                .positions
                .get(&position)
                .ok_or(V1EngineError::MissingPosition(position))?;
            if let Some(actor) = &event.actor {
                push_text_eq(output, source, "owner", actor, &native.owner.to_string())?;
            }
            push_text_eq(
                output,
                source,
                "position",
                &position.to_string(),
                &position.to_string(),
            )?;
        }
        Some("pair_created") => {
            let OperationPayloadV1::InitializeMarket {
                fields: initialize_fields,
                ..
            } = &operation.payload
            else {
                return Err(V1FactualReplayError::UnsupportedAssertion {
                    event_id: event.event_id.clone(),
                    kind: "pair_created outside initialize_market".to_owned(),
                });
            };
            reconcile_pair_created(fields, initialize_fields, after, output, source)?;
        }
        Some(kind) => {
            return Err(V1FactualReplayError::UnsupportedAssertion {
                event_id: event.event_id.clone(),
                kind: kind.to_owned(),
            });
        }
        None => {
            return Err(V1FactualReplayError::UnsupportedAssertion {
                event_id: event.event_id.clone(),
                kind: "config_without_event_kind".to_owned(),
            });
        }
    }
    Ok(())
}

fn reconcile_pair_created(
    fields: &BTreeMap<String, String>,
    initialize_fields: &BTreeMap<String, String>,
    after: &V1NativeCheckpointV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    push_text_eq(
        output,
        source,
        "token0",
        required_field(fields, "token0")?,
        &after.pair.token0.to_string(),
    )?;
    push_text_eq(
        output,
        source,
        "token1",
        required_field(fields, "token1")?,
        &after.pair.token1.to_string(),
    )?;
    push_text_eq(
        output,
        source,
        "lp_mint",
        required_field(fields, "lp_mint")?,
        &after.pair.lp_mint.to_string(),
    )?;
    push_text_eq(
        output,
        source,
        "rate_model",
        required_field(fields, "rate_model")?,
        &after.rate_model.key.to_string(),
    )?;
    reconcile_config_u64(
        fields,
        "token0_decimals",
        after.pair.token0_decimals as u64,
        output,
        source,
    )?;
    reconcile_config_u64(
        fields,
        "token1_decimals",
        after.pair.token1_decimals as u64,
        output,
        source,
    )?;
    reconcile_config_u64(
        fields,
        "swap_fee_bps",
        after.pair.swap_fee_bps as u64,
        output,
        source,
    )?;
    reconcile_config_u64(fields, "half_life", after.pair.half_life, output, source)?;
    reconcile_config_u64(fields, "version", after.pair.version as u64, output, source)?;
    let target_start_bps = nad_to_bps_exact(
        after.rate_model.target_util_start_nad,
        "target_util_start_bps",
    )?;
    let target_end_bps =
        nad_to_bps_exact(after.rate_model.target_util_end_nad, "target_util_end_bps")?;
    let min_rate_bps = nad_to_bps_exact(after.rate_model.min_rate_nad, "min_rate_bps")?;
    let max_rate_bps = nad_to_bps_exact(after.rate_model.max_rate_nad, "max_rate_bps")?;
    reconcile_optional_initialize_field(
        fields,
        initialize_fields,
        "target_util_start_bps",
        TARGET_UTIL_START_BPS,
        target_start_bps,
        output,
        source,
    )?;
    reconcile_optional_initialize_field(
        fields,
        initialize_fields,
        "target_util_end_bps",
        TARGET_UTIL_END_BPS,
        target_end_bps,
        output,
        source,
    )?;
    reconcile_optional_initialize_field(
        fields,
        initialize_fields,
        "rate_half_life_ms",
        DEFAULT_RATE_HALF_LIFE_MS,
        after.rate_model.half_life_ms,
        output,
        source,
    )?;
    reconcile_optional_initialize_field(
        fields,
        initialize_fields,
        "min_rate_bps",
        DEFAULT_MIN_RATE_BPS,
        min_rate_bps,
        output,
        source,
    )?;
    reconcile_optional_initialize_field(
        fields,
        initialize_fields,
        "max_rate_bps",
        DEFAULT_MAX_RATE_BPS,
        max_rate_bps,
        output,
        source,
    )?;
    reconcile_optional_fixed_cf(
        fields,
        initialize_fields,
        after.pair.fixed_cf_bps,
        output,
        source,
    )?;
    reconcile_initial_rate(
        initialize_fields,
        nad_to_bps_exact(after.rate_model.initial_rate_nad, "initial_rate_bps")?,
        output,
        source,
    )?;
    if let Some(params_hash) = fields.get("params_hash") {
        let decoded: Vec<u8> =
            serde_json::from_str(params_hash).map_err(|_| V1FactualReplayError::InvalidAmount {
                field: "params_hash".to_owned(),
                value: params_hash.clone(),
            })?;
        push_text_eq(
            output,
            source,
            "params_hash",
            &format!("{decoded:?}"),
            &format!("{:?}", after.pair.params_hash),
        )?;
    }
    Ok(())
}

fn reconcile_config_payload(
    fields: &BTreeMap<String, String>,
    after: &V1NativeCheckpointV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    let kind = required_field(fields, "instruction_kind")?;
    match kind {
        "set_pair_reduce_only" => push_text_eq(
            output,
            source,
            "args.reduce_only",
            required_field(fields, "args.reduce_only")?,
            &after.pair.reduce_only.to_string(),
        )?,
        "set_global_reduce_only" => push_text_eq(
            output,
            source,
            "args.reduce_only",
            required_field(fields, "args.reduce_only")?,
            &after.authority.global_reduce_only.to_string(),
        )?,
        "update_futarchy_authority" => push_config_pubkey_eq(
            output,
            source,
            "args.new_authority",
            required_field(fields, "args.new_authority")?,
            after.authority.authority,
        )?,
        "update_protocol_revenue" => {
            if let Some(value) = fields.get("args.swap_bps") {
                push_u128_eq(
                    output,
                    source,
                    "args.swap_bps",
                    parse_u16_text("args.swap_bps", value)? as u128,
                    after.authority.swap_bps as u128,
                )?;
            }
            if let Some(value) = fields.get("args.interest_bps") {
                push_u128_eq(
                    output,
                    source,
                    "args.interest_bps",
                    parse_u16_text("args.interest_bps", value)? as u128,
                    after.authority.interest_bps as u128,
                )?;
            }
            let distributions = [
                (
                    "args.revenue_distribution.futarchy_treasury_bps",
                    after.authority.futarchy_treasury_bps,
                ),
                (
                    "args.revenue_distribution.buybacks_vault_bps",
                    after.authority.buybacks_vault_bps,
                ),
                (
                    "args.revenue_distribution.team_treasury_bps",
                    after.authority.team_treasury_bps,
                ),
            ];
            for (field, actual) in distributions {
                if let Some(value) = fields.get(field) {
                    push_u128_eq(
                        output,
                        source,
                        field,
                        parse_u16_text(field, value)? as u128,
                        actual as u128,
                    )?;
                }
            }
        }
        "update_revenue_recipients" => {
            for (field, actual) in [
                ("args.futarchy_treasury", after.authority.futarchy_treasury),
                ("args.buybacks_vault", after.authority.buybacks_vault),
                ("args.team_treasury", after.authority.team_treasury),
            ] {
                if let Some(value) = fields.get(field) {
                    push_config_pubkey_eq(output, source, field, value, actual)?;
                }
            }
        }
        _ => return Err(V1FactualReplayError::UnsupportedConfig(kind.to_owned())),
    }
    Ok(())
}

fn reconcile_revenue_delta(
    receipt: &V1AppliedReceiptV1,
    actual: &V1RevenueDeltaV1,
    operation: &CanonicalOperationV1,
    output: &mut Vec<V1RawUnitReconciliationV1>,
) -> Result<(), V1FactualReplayError> {
    let mut expected = V1RevenueDeltaV1::default();
    if let Some(accrual) = receipt.accrual() {
        expected.lp_interest0 = u128::from(accrual.lp_interest0)
            + u128::from(accrual.protocol_interest0 - accrual.cash_covered_protocol_interest0);
        expected.lp_interest1 = u128::from(accrual.lp_interest1)
            + u128::from(accrual.protocol_interest1 - accrual.cash_covered_protocol_interest1);
        expected.protocol_interest0 = u128::from(accrual.cash_covered_protocol_interest0);
        expected.protocol_interest1 = u128::from(accrual.cash_covered_protocol_interest1);
        expected.nominal_protocol_interest0 = u128::from(accrual.protocol_interest0);
        expected.nominal_protocol_interest1 = u128::from(accrual.protocol_interest1);
        expected.cash_covered_protocol_interest0 =
            u128::from(accrual.cash_covered_protocol_interest0);
        expected.cash_covered_protocol_interest1 =
            u128::from(accrual.cash_covered_protocol_interest1);
    }
    match receipt {
        V1AppliedReceiptV1::Swap(swap) => match swap.token_in {
            V1TokenSideV1::Token0 => {
                expected.lp_swap_fee0 = swap.accounted_lp_fee as u128;
                expected.protocol_swap_fee0 = swap.accounted_protocol_fee as u128;
            }
            V1TokenSideV1::Token1 => {
                expected.lp_swap_fee1 = swap.accounted_lp_fee as u128;
                expected.protocol_swap_fee1 = swap.accounted_protocol_fee as u128;
            }
        },
        V1AppliedReceiptV1::Flashloan(flash) => {
            expected.flashloan_fee0 = flash.fee0 as u128;
            expected.flashloan_fee1 = flash.fee1 as u128;
        }
        V1AppliedReceiptV1::ProtocolClaim(claim) => {
            expected.protocol_claim_realized0 = claim.claimable0 as u128;
            expected.protocol_claim_realized1 = claim.claimable1 as u128;
        }
        _ => {}
    }
    let source = format!("revenue:{}", operation.operation_id);
    for (field, expected, actual) in revenue_fields(&expected, actual) {
        push_u128_eq(output, &source, field, expected, actual)?;
    }
    Ok(())
}

fn revenue_delta(
    before: &V1RevenueCheckpointV1,
    after: &V1RevenueCheckpointV1,
    receipt: &V1AppliedReceiptV1,
    operation: &CanonicalOperationV1,
) -> Result<V1RevenueDeltaV1, V1FactualReplayError> {
    macro_rules! delta {
        ($field:ident) => {
            after.$field.checked_sub(before.$field).ok_or_else(|| {
                V1FactualReplayError::ConservationViolation {
                    operation_id: operation.operation_id.clone(),
                    identity: concat!("monotonic revenue.", stringify!($field)).to_owned(),
                }
            })?
        };
    }
    let accrual = receipt.accrual();
    Ok(V1RevenueDeltaV1 {
        lp_swap_fee0: delta!(lp_swap_fee0),
        lp_swap_fee1: delta!(lp_swap_fee1),
        protocol_swap_fee0: delta!(protocol_swap_fee0),
        protocol_swap_fee1: delta!(protocol_swap_fee1),
        lp_interest0: delta!(lp_interest0),
        lp_interest1: delta!(lp_interest1),
        protocol_interest0: delta!(protocol_interest0),
        protocol_interest1: delta!(protocol_interest1),
        nominal_protocol_interest0: accrual
            .map(|value| u128::from(value.protocol_interest0))
            .unwrap_or(0),
        nominal_protocol_interest1: accrual
            .map(|value| u128::from(value.protocol_interest1))
            .unwrap_or(0),
        cash_covered_protocol_interest0: accrual
            .map(|value| u128::from(value.cash_covered_protocol_interest0))
            .unwrap_or(0),
        cash_covered_protocol_interest1: accrual
            .map(|value| u128::from(value.cash_covered_protocol_interest1))
            .unwrap_or(0),
        flashloan_fee0: delta!(flashloan_fee0),
        flashloan_fee1: delta!(flashloan_fee1),
        protocol_claim_realized0: delta!(protocol_claim_realized0),
        protocol_claim_realized1: delta!(protocol_claim_realized1),
    })
}

fn revenue_fields<'a>(
    expected: &'a V1RevenueDeltaV1,
    actual: &'a V1RevenueDeltaV1,
) -> [(&'static str, u128, u128); 16] {
    [
        ("lp_swap_fee0", expected.lp_swap_fee0, actual.lp_swap_fee0),
        ("lp_swap_fee1", expected.lp_swap_fee1, actual.lp_swap_fee1),
        (
            "protocol_swap_fee0",
            expected.protocol_swap_fee0,
            actual.protocol_swap_fee0,
        ),
        (
            "protocol_swap_fee1",
            expected.protocol_swap_fee1,
            actual.protocol_swap_fee1,
        ),
        ("lp_interest0", expected.lp_interest0, actual.lp_interest0),
        ("lp_interest1", expected.lp_interest1, actual.lp_interest1),
        (
            "protocol_interest0",
            expected.protocol_interest0,
            actual.protocol_interest0,
        ),
        (
            "protocol_interest1",
            expected.protocol_interest1,
            actual.protocol_interest1,
        ),
        (
            "nominal_protocol_interest0",
            expected.nominal_protocol_interest0,
            actual.nominal_protocol_interest0,
        ),
        (
            "nominal_protocol_interest1",
            expected.nominal_protocol_interest1,
            actual.nominal_protocol_interest1,
        ),
        (
            "cash_covered_protocol_interest0",
            expected.cash_covered_protocol_interest0,
            actual.cash_covered_protocol_interest0,
        ),
        (
            "cash_covered_protocol_interest1",
            expected.cash_covered_protocol_interest1,
            actual.cash_covered_protocol_interest1,
        ),
        (
            "flashloan_fee0",
            expected.flashloan_fee0,
            actual.flashloan_fee0,
        ),
        (
            "flashloan_fee1",
            expected.flashloan_fee1,
            actual.flashloan_fee1,
        ),
        (
            "protocol_claim_realized0",
            expected.protocol_claim_realized0,
            actual.protocol_claim_realized0,
        ),
        (
            "protocol_claim_realized1",
            expected.protocol_claim_realized1,
            actual.protocol_claim_realized1,
        ),
    ]
}

fn conservation_identities(
    checkpoint: &V1NativeCheckpointV1,
) -> Result<Vec<V1ConservationIdentityV1>, V1FactualReplayError> {
    let collateral0 = checkpoint
        .positions
        .values()
        .try_fold(0_u128, |sum, position| {
            sum.checked_add(position.collateral0 as u128)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "position collateral0 sum",
                ))
        })?;
    let collateral1 = checkpoint
        .positions
        .values()
        .try_fold(0_u128, |sum, position| {
            sum.checked_add(position.collateral1 as u128)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "position collateral1 sum",
                ))
        })?;
    let shares0 = checkpoint
        .positions
        .values()
        .try_fold(0_u128, |sum, position| {
            sum.checked_add(position.debt0_shares)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "position debt0 shares sum",
                ))
        })?;
    let shares1 = checkpoint
        .positions
        .values()
        .try_fold(0_u128, |sum, position| {
            sum.checked_add(position.debt1_shares)
                .ok_or(V1EngineError::ArithmeticOverflow(
                    "position debt1 shares sum",
                ))
        })?;
    Ok(vec![
        identity_equal(
            "reserve0 = cash0 + debt0",
            checkpoint.pair.reserve0 as u128,
            checkpoint.pair.cash_reserve0 as u128 + checkpoint.pair.total_debt0 as u128,
        ),
        identity_equal(
            "reserve1 = cash1 + debt1",
            checkpoint.pair.reserve1 as u128,
            checkpoint.pair.cash_reserve1 as u128 + checkpoint.pair.total_debt1 as u128,
        ),
        identity_equal(
            "total_collateral0 = sum(position.collateral0)",
            checkpoint.pair.total_collateral0 as u128,
            collateral0,
        ),
        identity_equal(
            "total_collateral1 = sum(position.collateral1)",
            checkpoint.pair.total_collateral1 as u128,
            collateral1,
        ),
        identity_equal(
            "total_debt0_shares = sum(position.debt0_shares)",
            checkpoint.pair.total_debt0_shares,
            shares0,
        ),
        identity_equal(
            "total_debt1_shares = sum(position.debt1_shares)",
            checkpoint.pair.total_debt1_shares,
            shares1,
        ),
    ])
}

fn identity_equal(name: &str, left: u128, right: u128) -> V1ConservationIdentityV1 {
    V1ConservationIdentityV1 {
        name: name.to_owned(),
        relation: V1IdentityRelationV1::Equal,
        left: left.to_string(),
        right: right.to_string(),
        raw_unit_difference: signed_difference(left, right),
        holds: left == right,
    }
}

fn ensure_conservation(
    operation: &CanonicalOperationV1,
    identities: &[V1ConservationIdentityV1],
) -> Result<(), V1FactualReplayError> {
    if let Some(identity) = identities.iter().find(|identity| !identity.holds) {
        return Err(V1FactualReplayError::ConservationViolation {
            operation_id: operation.operation_id.clone(),
            identity: identity.name.clone(),
        });
    }
    Ok(())
}

fn side(token0: bool) -> V1TokenSideV1 {
    if token0 {
        V1TokenSideV1::Token0
    } else {
        V1TokenSideV1::Token1
    }
}

fn collateral_delta(
    amount0: &SignedTokenAmount,
    amount1: &SignedTokenAmount,
) -> Result<(V1TokenSideV1, bool, u64), V1FactualReplayError> {
    let amount0 = parse_i128("collateral.amount0", amount0)?;
    let amount1 = parse_i128("collateral.amount1", amount1)?;
    let (side, signed) = match (amount0 != 0, amount1 != 0) {
        (true, false) => (V1TokenSideV1::Token0, amount0),
        (false, true) => (V1TokenSideV1::Token1, amount1),
        _ => {
            return Err(V1FactualReplayError::InvalidAmount {
                field: "collateral.amount0/amount1".to_owned(),
                value: format!("{amount0}/{amount1}"),
            });
        }
    };
    let magnitude =
        u64::try_from(signed.unsigned_abs()).map_err(|_| V1FactualReplayError::InvalidAmount {
            field: "collateral signed magnitude".to_owned(),
            value: signed.to_string(),
        })?;
    Ok((side, signed > 0, magnitude))
}

fn signed_collateral_receipt(receipt: &V1CollateralReceiptV1) -> (i128, i128) {
    let signed = if receipt.deposited {
        receipt.accounted_amount as i128
    } else {
        -(receipt.accounted_amount as i128)
    };
    match receipt.collateral_token {
        V1TokenSideV1::Token0 => (signed, 0),
        V1TokenSideV1::Token1 => (0, signed),
    }
}

fn signed_debt_receipt(
    receipt: &V1AppliedReceiptV1,
    operation: &CanonicalOperationV1,
) -> Option<(i128, i128)> {
    let (side, amount) = match receipt {
        V1AppliedReceiptV1::Borrow(receipt) => (
            receipt.capacity_before.debt_token,
            receipt.borrowed_amount as i128,
        ),
        V1AppliedReceiptV1::Repay(receipt) => match &operation.payload {
            OperationPayloadV1::Repay { debt_token0, .. } => {
                (side(*debt_token0), -(receipt.repaid_amount as i128))
            }
            _ => return None,
        },
        V1AppliedReceiptV1::Liquidation(receipt) => {
            (receipt.debt_token, -(receipt.debt_written_off as i128))
        }
        _ => return None,
    };
    Some(match side {
        V1TokenSideV1::Token0 => (amount, 0),
        V1TokenSideV1::Token1 => (0, amount),
    })
}

fn max_cf(liquidation_cf_bps: u16) -> u16 {
    ((liquidation_cf_bps as u32).saturating_mul((BPS_DENOMINATOR - LTV_BUFFER_BPS) as u32)
        / BPS_DENOMINATOR as u32) as u16
}

fn parse_pubkey(field: &str, value: &str) -> Result<Pubkey, V1FactualReplayError> {
    Pubkey::from_str(value).map_err(|_| V1FactualReplayError::InvalidPubkey {
        field: field.to_owned(),
        value: value.to_owned(),
    })
}

fn parse_config_pubkey(field: &str, value: &str) -> Result<Pubkey, V1FactualReplayError> {
    parse_schema_pubkey(value).ok_or_else(|| V1FactualReplayError::InvalidPubkey {
        field: field.to_owned(),
        value: value.to_owned(),
    })
}

fn parse_u128(field: &str, value: &TokenAmount) -> Result<u128, V1FactualReplayError> {
    value
        .as_str()
        .parse::<u128>()
        .map_err(|_| V1FactualReplayError::InvalidAmount {
            field: field.to_owned(),
            value: value.to_string(),
        })
}

fn parse_u64(field: &str, value: &TokenAmount) -> Result<u64, V1FactualReplayError> {
    value
        .as_str()
        .parse::<u64>()
        .map_err(|_| V1FactualReplayError::InvalidAmount {
            field: field.to_owned(),
            value: value.to_string(),
        })
}

fn parse_i128(field: &str, value: &SignedTokenAmount) -> Result<i128, V1FactualReplayError> {
    value
        .as_str()
        .parse::<i128>()
        .map_err(|_| V1FactualReplayError::InvalidAmount {
            field: field.to_owned(),
            value: value.to_string(),
        })
}

fn parse_u16_text(field: &str, value: &str) -> Result<u16, V1FactualReplayError> {
    value
        .parse::<u16>()
        .map_err(|_| V1FactualReplayError::InvalidAmount {
            field: field.to_owned(),
            value: value.to_owned(),
        })
}

fn parse_bool(value: &str) -> Result<bool, V1FactualReplayError> {
    value
        .parse::<bool>()
        .map_err(|_| V1FactualReplayError::InvalidAmount {
            field: "boolean config argument".to_owned(),
            value: value.to_owned(),
        })
}

fn required_field<'a>(
    fields: &'a BTreeMap<String, String>,
    field: &str,
) -> Result<&'a str, V1FactualReplayError> {
    fields.get(field).map(String::as_str).ok_or_else(|| {
        V1FactualReplayError::MissingCanonicalFields {
            operation_id: "config".to_owned(),
            fields: vec![field.to_owned()],
        }
    })
}

fn optional_u16(
    fields: &BTreeMap<String, String>,
    field: &str,
) -> Result<Option<u16>, V1FactualReplayError> {
    fields
        .get(field)
        .map(|value| parse_u16_text(field, value))
        .transpose()
}

fn optional_pubkey(
    fields: &BTreeMap<String, String>,
    field: &str,
    fallback: Pubkey,
) -> Result<Pubkey, V1FactualReplayError> {
    fields
        .get(field)
        .map(|value| parse_config_pubkey(field, value))
        .transpose()
        .map(|value| value.unwrap_or(fallback))
}

fn reconcile_config_u64(
    fields: &BTreeMap<String, String>,
    field: &str,
    actual: u64,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    let expected = required_field(fields, field)?;
    let expected = expected
        .parse::<u64>()
        .map_err(|_| V1FactualReplayError::InvalidAmount {
            field: field.to_owned(),
            value: expected.to_owned(),
        })?;
    push_u128_eq(output, source, field, expected as u128, actual as u128)
}

fn reconcile_optional_initialize_field(
    event_fields: &BTreeMap<String, String>,
    initialize_fields: &BTreeMap<String, String>,
    field: &str,
    native_default: u64,
    actual_resolved: u64,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    let instruction_field = format!("instruction.args.{field}");
    let decoded = initialize_fields
        .get(&instruction_field)
        .filter(|value| value.as_str() != "null")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| V1FactualReplayError::InvalidAmount {
                    field: instruction_field.clone(),
                    value: value.clone(),
                })
        })
        .transpose()?;
    let event_text = required_field(event_fields, field)?;
    let event_raw = event_text
        .parse::<u64>()
        .map_err(|_| V1FactualReplayError::InvalidAmount {
            field: field.to_owned(),
            value: event_text.to_owned(),
        })?;
    // PairCreatedEvent deliberately preserves the optional instruction
    // encoding and uses zero when the argument was absent. It does not emit
    // resolved rate-model state. Compare that lane to `None -> 0`, then
    // independently compare native state to `None -> revision default`.
    push_u128_eq(
        output,
        source,
        &format!("{field}.event_argument_encoding"),
        decoded.unwrap_or(0) as u128,
        event_raw as u128,
    )?;
    push_u128_eq(
        output,
        source,
        &format!("{field}.native_resolved"),
        decoded.unwrap_or(native_default) as u128,
        actual_resolved as u128,
    )
}

fn reconcile_optional_fixed_cf(
    event_fields: &BTreeMap<String, String>,
    initialize_fields: &BTreeMap<String, String>,
    actual: Option<u16>,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    let instruction_field = "instruction.args.fixed_cf_bps";
    let decoded = initialize_fields
        .get(instruction_field)
        .filter(|value| value.as_str() != "null")
        .map(|value| parse_u16_text(instruction_field, value))
        .transpose()?;
    let event = event_fields
        .get("fixed_cf_bps")
        .map(|value| parse_u16_text("fixed_cf_bps", value))
        .transpose()?;
    let expected = decoded
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned());
    let event = event
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned());
    let actual = actual
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned());
    push_text_eq(
        output,
        source,
        "fixed_cf_bps.event_argument_encoding",
        &expected,
        &event,
    )?;
    push_text_eq(
        output,
        source,
        "fixed_cf_bps.native_resolved",
        &expected,
        &actual,
    )
}

fn reconcile_initial_rate(
    initialize_fields: &BTreeMap<String, String>,
    actual_resolved: u64,
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
) -> Result<(), V1FactualReplayError> {
    let field = "instruction.args.initial_rate_bps";
    let decoded = initialize_fields
        .get(field)
        .filter(|value| value.as_str() != "null")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| V1FactualReplayError::InvalidAmount {
                    field: field.to_owned(),
                    value: value.clone(),
                })
        })
        .transpose()?;
    push_u128_eq(
        output,
        source,
        "initial_rate_bps.native_resolved",
        decoded.unwrap_or(omnipair::constants::DEFAULT_INITIAL_RATE_BPS) as u128,
        actual_resolved as u128,
    )
}

fn nad_to_bps_exact(value: u64, field: &str) -> Result<u64, V1FactualReplayError> {
    let numerator = (value as u128)
        .checked_mul(BPS_DENOMINATOR as u128)
        .ok_or(V1EngineError::ArithmeticOverflow("NAD to bps"))?;
    if numerator % NAD as u128 != 0 {
        return Err(V1FactualReplayError::InvalidAmount {
            field: field.to_owned(),
            value: value.to_string(),
        });
    }
    u64::try_from(numerator / NAD as u128)
        .map_err(|_| V1EngineError::ArithmeticOverflow("NAD to bps conversion").into())
}

fn push_token_eq(
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
    field: &str,
    expected: &TokenAmount,
    actual: u64,
) -> Result<(), V1FactualReplayError> {
    push_u128_eq(
        output,
        source,
        field,
        parse_u128(field, expected)?,
        actual as u128,
    )
}

fn ensure_minimum_guard(
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
    field: &str,
    minimum: &TokenAmount,
    actual: u64,
) -> Result<(), V1FactualReplayError> {
    let minimum = parse_u64(field, minimum)?;
    if actual < minimum {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: source.to_owned(),
            field: field.to_owned(),
            expected: format!(">={minimum}"),
            actual: actual.to_string(),
        });
    }
    push_guard_passed(output, source, field)
}

fn push_guard_passed(
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
    field: &str,
) -> Result<(), V1FactualReplayError> {
    push_u128_eq(output, source, field, 1, 1)
}

fn push_token_u128_eq(
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
    field: &str,
    expected: &TokenAmount,
    actual: u128,
) -> Result<(), V1FactualReplayError> {
    push_u128_eq(output, source, field, parse_u128(field, expected)?, actual)
}

fn push_signed_eq(
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
    field: &str,
    expected: &SignedTokenAmount,
    actual: i128,
) -> Result<(), V1FactualReplayError> {
    let expected = parse_i128(field, expected)?;
    let difference = expected.checked_sub(actual).map_or_else(
        || "overflow".to_owned(),
        |difference| difference.to_string(),
    );
    output.push(V1RawUnitReconciliationV1 {
        source: source.to_owned(),
        field: field.to_owned(),
        expected: expected.to_string(),
        actual: actual.to_string(),
        raw_unit_difference: difference,
    });
    if expected != actual {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: source.to_owned(),
            field: field.to_owned(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

fn push_u128_eq(
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
    field: &str,
    expected: u128,
    actual: u128,
) -> Result<(), V1FactualReplayError> {
    output.push(V1RawUnitReconciliationV1 {
        source: source.to_owned(),
        field: field.to_owned(),
        expected: expected.to_string(),
        actual: actual.to_string(),
        raw_unit_difference: signed_difference(expected, actual),
    });
    if expected != actual {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: source.to_owned(),
            field: field.to_owned(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

fn push_text_eq(
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
    field: &str,
    expected: &str,
    actual: &str,
) -> Result<(), V1FactualReplayError> {
    output.push(V1RawUnitReconciliationV1 {
        source: source.to_owned(),
        field: field.to_owned(),
        expected: expected.to_owned(),
        actual: actual.to_owned(),
        raw_unit_difference: if expected == actual { "0" } else { "n/a" }.to_owned(),
    });
    if expected != actual {
        return Err(V1FactualReplayError::AssertionMismatch {
            event_id: source.to_owned(),
            field: field.to_owned(),
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        });
    }
    Ok(())
}

fn push_config_pubkey_eq(
    output: &mut Vec<V1RawUnitReconciliationV1>,
    source: &str,
    field: &str,
    expected: &str,
    actual: Pubkey,
) -> Result<(), V1FactualReplayError> {
    let expected = parse_config_pubkey(field, expected)?.to_string();
    push_text_eq(output, source, field, &expected, &actual.to_string())
}

fn signed_difference(expected: u128, actual: u128) -> String {
    if expected >= actual {
        (expected - actual).to_string()
    } else {
        format!("-{}", actual - expected)
    }
}

fn unsupported_assertion<T>(
    event: &CanonicalEventV1,
    kind: &str,
) -> Result<T, V1FactualReplayError> {
    Err(V1FactualReplayError::UnsupportedAssertion {
        event_id: event.event_id.clone(),
        kind: kind.to_owned(),
    })
}

/// Canonical fields that must be extracted before the corresponding operation
/// can be accepted as event-exact. No default or event-derived substitute is
/// permitted for any entry in this list.
pub fn required_canonical_extensions() -> Vec<V1CanonicalFieldRequirementV1> {
    vec![
        V1CanonicalFieldRequirementV1 {
            operation_kind: "checkpoint_import".to_owned(),
            fields: vec![
                "raw reserve/collateral vault balances".to_owned(),
                "reconstructed cumulative revenue lanes".to_owned(),
                "all live UserPosition accounts".to_owned(),
            ],
            reason: "a mid-lifecycle import must prove custody, revenue, and aggregate share identities".to_owned(),
        },
        V1CanonicalFieldRequirementV1 {
            operation_kind: "initialize_market".to_owned(),
            fields: vec![
                "decoded instruction initial_rate_bps".to_owned(),
                "active FutarchyAuthority configuration epoch".to_owned(),
            ],
            reason: "PairCreatedEvent omits initial rate and global authority state; all PDAs and bumps are now derived exactly".to_owned(),
        },
        V1CanonicalFieldRequirementV1 {
            operation_kind: "add_liquidity".to_owned(),
            fields: vec!["pre user token balances".to_owned(), "same-transaction circuit-breaker evidence".to_owned()],
            reason: "successful state parity uses native amounts; validation parity also needs instruction guards".to_owned(),
        },
        V1CanonicalFieldRequirementV1 {
            operation_kind: "remove_liquidity".to_owned(),
            fields: vec!["pre user LP balance".to_owned(), "same-transaction circuit-breaker evidence".to_owned()],
            reason: "burn events contain realized amounts, not every original validation input".to_owned(),
        },
        V1CanonicalFieldRequirementV1 {
            operation_kind: "swap_exact_in".to_owned(),
            fields: vec!["pre user input-token balance".to_owned()],
            reason: "min-out and custody deltas are present, but balance validation is not".to_owned(),
        },
        V1CanonicalFieldRequirementV1 {
            operation_kind: "collateral_delta".to_owned(),
            fields: vec!["pre user balance on deposit".to_owned(), "same-transaction liquidity-delta evidence on withdrawal".to_owned()],
            reason: "these are native guards and cannot be inferred from the emitted delta".to_owned(),
        },
        V1CanonicalFieldRequirementV1 {
            operation_kind: "borrow_repay".to_owned(),
            fields: vec!["pre payer balance for repay".to_owned(), "same-transaction liquidity circuit evidence".to_owned()],
            reason: "requested sentinel amounts are now preserved; remaining fields are validation guards".to_owned(),
        },
        V1CanonicalFieldRequirementV1 {
            operation_kind: "flashloan".to_owned(),
            fields: vec!["callback program/data/account fingerprint".to_owned()],
            reason: "terminal net vault credits are now exact; callback provenance is still required for full instruction coverage".to_owned(),
        },
        V1CanonicalFieldRequirementV1 {
            operation_kind: "rate_model_config".to_owned(),
            fields: vec!["new rate-model account key".to_owned(), "complete decoded RateModel post-state".to_owned()],
            reason: "a versioned native rate account must be imported, never synthesized from partial config history".to_owned(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnipair::{
        ceil_div,
        constants::{MIN_LIQUIDITY, VERSION},
        state::{FutarchyAuthority, Pair, RateModel, VaultBumps},
        CPCurve,
    };

    use crate::canonical::{SwapPayloadV1, CANONICAL_SCHEMA_VERSION};

    const PAIR_KEY: Pubkey = Pubkey::new_from_array([8; 32]);
    const POSITION_KEY: Pubkey = Pubkey::new_from_array([9; 32]);
    const TOKEN0: Pubkey = Pubkey::new_from_array([1; 32]);
    const TOKEN1: Pubkey = Pubkey::new_from_array([2; 32]);
    const RATE_MODEL_KEY: Pubkey = Pubkey::new_from_array([4; 32]);

    fn amount(value: u128) -> TokenAmount {
        TokenAmount::new(value.to_string()).unwrap()
    }

    fn zero_revenue() -> V1RevenueCheckpointV1 {
        V1RevenueCheckpointV1 {
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
        }
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

    fn base_pair() -> Pair {
        let model = rate_model();
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
            model.initial_rate,
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

    fn decoded_accounts() -> V1DecodedAccountsV1 {
        V1DecodedAccountsV1 {
            program_version: VERSION,
            pair_key: PAIR_KEY,
            pair: base_pair(),
            rate_model_key: RATE_MODEL_KEY,
            rate_model: rate_model(),
            futarchy_authority: authority(),
            positions: vec![(
                POSITION_KEY,
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
                },
            )],
            custody: crate::v1_engine::V1CustodyCheckpointV1 {
                reserve_vault0: 1_000_000_000,
                reserve_vault1: 1_000_000_000,
                collateral_vault0: 0,
                collateral_vault1: 200_000_000,
            },
            revenue: zero_revenue(),
        }
    }

    fn order(slot: u64, event: bool) -> ChainOrder {
        ChainOrder {
            slot,
            transaction_index: 0,
            outer_instruction_index: 0,
            inner_execution_index: event.then_some(0),
            event_sequence: 0,
        }
    }

    fn swap_operation(wrong_output_delta: u64) -> CanonicalOperationV1 {
        let gross = 25_000_000_u64;
        let swap_fee = ceil_div(gross as u128 * 30, BPS_DENOMINATOR as u128).unwrap() as u64;
        let protocol_fee =
            ceil_div(swap_fee as u128 * 2_000, BPS_DENOMINATOR as u128).unwrap() as u64;
        let net = gross - swap_fee;
        let output = CPCurve::calculate_amount_out(1_000_000_000, 1_000_000_000, net).unwrap();
        let signature = "swap-signature".to_owned();
        let pair = PAIR_KEY.to_string();
        let assertion = CanonicalEventV1::new(
            "localnet".to_owned(),
            omnipair::ID.to_string(),
            Some(pair.clone()),
            Some(Pubkey::new_from_array([15; 32]).to_string()),
            signature.clone(),
            order(100, true),
            vec![0],
            1,
            "test-decoder".to_owned(),
            b"swap",
            None,
            EventPayloadV1::Swap(SwapPayloadV1 {
                token0_in: true,
                amount_in: amount(gross as u128),
                amount_out: amount((output + wrong_output_delta) as u128),
                amount_in_after_fee: amount(net as u128),
                lp_fee: amount((swap_fee - protocol_fee) as u128),
                protocol_fee: amount(protocol_fee as u128),
                reserve0_after: amount((1_000_000_000 + gross - protocol_fee) as u128),
                reserve1_after: amount((1_000_000_000 - output) as u128),
            }),
        );
        CanonicalOperationV1 {
            schema_version: crate::replay::REPLAY_SCHEMA_VERSION,
            operation_id: "swap-1".to_owned(),
            signature,
            pair,
            actor: Some(Pubkey::new_from_array([15; 32]).to_string()),
            order: order(100, false),
            block_time_unix_seconds: 1,
            decoder_version_id: "test-decoder".to_owned(),
            payload: OperationPayloadV1::SwapExactIn {
                token0_in: true,
                gross_amount_in: amount(gross as u128),
                vault_credit_in: amount(gross as u128),
                min_amount_out: amount(0),
                vault_debit_out: amount(output as u128),
                recipient_credit_out: amount(output as u128),
            },
            assertions: vec![assertion],
        }
    }

    #[test]
    fn factual_config_updates_preserve_legacy_pubkey_bytes_exactly() {
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

        let mut engine = V1NativeEngine::from_decoded_accounts(decoded_accounts()).unwrap();
        let authority = Pubkey::new_from_array([21; 32]);
        let authority_fields = BTreeMap::from([
            (
                "instruction_kind".to_owned(),
                "update_futarchy_authority".to_owned(),
            ),
            ("args.new_authority".to_owned(), legacy_pubkey(authority)),
        ]);
        apply_config(&mut engine, &authority_fields).unwrap();
        assert_eq!(engine.checkpoint().authority.authority, authority);
        let mut reconciliations = Vec::new();
        reconcile_config_payload(
            &authority_fields,
            &engine.checkpoint(),
            &mut reconciliations,
            "authority-update",
        )
        .unwrap();
        assert!(reconciliations
            .iter()
            .all(|item| item.raw_unit_difference == "0"));

        let futarchy_treasury = Pubkey::new_from_array([22; 32]);
        let buybacks_vault = Pubkey::new_from_array([23; 32]);
        let team_treasury = Pubkey::new_from_array([24; 32]);
        let recipient_fields = BTreeMap::from([
            (
                "instruction_kind".to_owned(),
                "update_revenue_recipients".to_owned(),
            ),
            (
                "args.futarchy_treasury".to_owned(),
                legacy_pubkey(futarchy_treasury),
            ),
            (
                "args.buybacks_vault".to_owned(),
                legacy_pubkey(buybacks_vault),
            ),
            (
                "args.team_treasury".to_owned(),
                legacy_pubkey(team_treasury),
            ),
        ]);
        apply_config(&mut engine, &recipient_fields).unwrap();
        let checkpoint = engine.checkpoint();
        assert_eq!(checkpoint.authority.futarchy_treasury, futarchy_treasury);
        assert_eq!(checkpoint.authority.buybacks_vault, buybacks_vault);
        assert_eq!(checkpoint.authority.team_treasury, team_treasury);
        let mut reconciliations = Vec::new();
        reconcile_config_payload(
            &recipient_fields,
            &checkpoint,
            &mut reconciliations,
            "recipient-update",
        )
        .unwrap();
        assert!(reconciliations
            .iter()
            .all(|item| item.raw_unit_difference == "0"));

        let error = apply_config(
            &mut engine,
            &BTreeMap::from([
                (
                    "instruction_kind".to_owned(),
                    "update_futarchy_authority".to_owned(),
                ),
                ("args.new_authority".to_owned(), "[\"21\"]".to_owned()),
            ]),
        )
        .unwrap_err();
        assert!(matches!(error, V1FactualReplayError::InvalidPubkey { .. }));
    }

    #[test]
    fn optional_initialize_fields_separate_event_sentinels_from_resolved_state() {
        for (field, native_default) in [
            ("target_util_start_bps", TARGET_UTIL_START_BPS),
            ("target_util_end_bps", TARGET_UTIL_END_BPS),
            ("rate_half_life_ms", DEFAULT_RATE_HALF_LIFE_MS),
            ("min_rate_bps", DEFAULT_MIN_RATE_BPS),
            ("max_rate_bps", DEFAULT_MAX_RATE_BPS),
        ] {
            let event = BTreeMap::from([(field.to_owned(), "0".to_owned())]);
            let initialize =
                BTreeMap::from([(format!("instruction.args.{field}"), "null".to_owned())]);
            let mut reconciliations = Vec::new();
            reconcile_optional_initialize_field(
                &event,
                &initialize,
                field,
                native_default,
                native_default,
                &mut reconciliations,
                "legacy-none",
            )
            .unwrap();
            assert_eq!(reconciliations.len(), 2);
            assert!(reconciliations
                .iter()
                .all(|item| item.raw_unit_difference == "0"));

            let explicit = native_default.saturating_add(17);
            let event = BTreeMap::from([(field.to_owned(), explicit.to_string())]);
            let initialize =
                BTreeMap::from([(format!("instruction.args.{field}"), explicit.to_string())]);
            reconcile_optional_initialize_field(
                &event,
                &initialize,
                field,
                native_default,
                explicit,
                &mut Vec::new(),
                "explicit",
            )
            .unwrap();

            let wrong_event = BTreeMap::from([(field.to_owned(), "0".to_owned())]);
            assert!(matches!(
                reconcile_optional_initialize_field(
                    &wrong_event,
                    &initialize,
                    field,
                    native_default,
                    explicit,
                    &mut Vec::new(),
                    "wrong-event",
                ),
                Err(V1FactualReplayError::AssertionMismatch { .. })
            ));
        }

        let absent = BTreeMap::new();
        let absent_arg = BTreeMap::from([(
            "instruction.args.fixed_cf_bps".to_owned(),
            "null".to_owned(),
        )]);
        reconcile_optional_fixed_cf(&absent, &absent_arg, None, &mut Vec::new(), "fixed-none")
            .unwrap();
        let fixed_event = BTreeMap::from([("fixed_cf_bps".to_owned(), "8000".to_owned())]);
        let fixed_arg = BTreeMap::from([(
            "instruction.args.fixed_cf_bps".to_owned(),
            "8000".to_owned(),
        )]);
        reconcile_optional_fixed_cf(
            &fixed_event,
            &fixed_arg,
            Some(8000),
            &mut Vec::new(),
            "fixed-explicit",
        )
        .unwrap();
        assert!(reconcile_optional_fixed_cf(
            &absent,
            &fixed_arg,
            Some(8000),
            &mut Vec::new(),
            "fixed-mismatch",
        )
        .is_err());

        let initial_none = BTreeMap::from([(
            "instruction.args.initial_rate_bps".to_owned(),
            "null".to_owned(),
        )]);
        reconcile_initial_rate(
            &initial_none,
            omnipair::constants::DEFAULT_INITIAL_RATE_BPS,
            &mut Vec::new(),
            "initial-none",
        )
        .unwrap();
        let initial_explicit = BTreeMap::from([(
            "instruction.args.initial_rate_bps".to_owned(),
            "777".to_owned(),
        )]);
        reconcile_initial_rate(&initial_explicit, 777, &mut Vec::new(), "initial-explicit")
            .unwrap();
        assert!(
            reconcile_initial_rate(&initial_none, 0, &mut Vec::new(), "initial-mismatch",).is_err()
        );
    }

    #[test]
    fn successful_swap_has_only_zero_raw_unit_differences() {
        let engine = V1NativeEngine::from_decoded_accounts(decoded_accounts()).unwrap();
        let mut replay = V1FactualReplay::from_checkpoint(engine, None).unwrap();
        let record = replay.apply_operation(&swap_operation(0)).unwrap();

        assert!(!record.reconciliations.is_empty());
        assert!(record
            .reconciliations
            .iter()
            .all(|reconciliation| reconciliation.raw_unit_difference == "0"));
        assert!(record.conservation.iter().all(|identity| identity.holds));
        assert!(record.revenue_delta.lp_swap_fee0 > 0);
        assert!(record.revenue_delta.protocol_swap_fee0 > 0);
        assert_eq!(replay.last_order(), Some(order(100, false)));
    }

    #[test]
    fn assertion_mismatch_rolls_back_state_and_order() {
        let engine = V1NativeEngine::from_decoded_accounts(decoded_accounts()).unwrap();
        let mut replay = V1FactualReplay::from_checkpoint(engine, None).unwrap();
        let before = replay.engine().checkpoint();

        let error = replay.apply_operation(&swap_operation(1)).unwrap_err();

        assert!(matches!(
            error,
            V1FactualReplayError::AssertionMismatch { .. }
        ));
        assert_eq!(replay.engine().checkpoint(), before);
        assert_eq!(replay.last_order(), None);
    }

    #[test]
    fn flashloan_with_inconsistent_terminal_vault_delta_rolls_back() {
        let engine = V1NativeEngine::from_decoded_accounts(decoded_accounts()).unwrap();
        let mut replay = V1FactualReplay::from_checkpoint(engine, None).unwrap();
        let before = replay.engine().checkpoint();
        let operation = CanonicalOperationV1 {
            schema_version: crate::replay::REPLAY_SCHEMA_VERSION,
            operation_id: "flash-1".to_owned(),
            signature: "flash-signature".to_owned(),
            pair: PAIR_KEY.to_string(),
            actor: None,
            order: order(100, false),
            block_time_unix_seconds: 1,
            decoder_version_id: "test-decoder".to_owned(),
            payload: OperationPayloadV1::Flashloan {
                amount0: amount(1_000),
                amount1: amount(0),
                fee0: amount(1),
                fee1: amount(0),
                net_vault_credit0: amount(0),
                net_vault_credit1: amount(0),
            },
            assertions: Vec::new(),
        };

        let error = replay.apply_operation(&operation).unwrap_err();

        assert!(matches!(error, V1FactualReplayError::Engine(_)));
        assert_eq!(replay.engine().checkpoint(), before);
        assert_eq!(replay.last_order(), None);
    }

    #[test]
    fn post_initialize_checkpoint_is_reconstructed_with_native_math() {
        let model = rate_model();
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
            model.initial_rate,
        );
        let gross = 1_000_000_u64;
        let minted = gross - MIN_LIQUIDITY;
        pair.reserve0 = gross;
        pair.reserve1 = gross;
        pair.cash_reserve0 = gross;
        pair.cash_reserve1 = gross;
        pair.total_supply = gross;
        pair.last_price0_ema.symmetric = NAD;
        pair.last_price0_ema.directional = NAD;
        pair.last_price1_ema.symmetric = NAD;
        pair.last_price1_ema.directional = NAD;
        let operation = CanonicalOperationV1 {
            schema_version: crate::replay::REPLAY_SCHEMA_VERSION,
            operation_id: "initialize-1".to_owned(),
            signature: "initialize-signature".to_owned(),
            pair: PAIR_KEY.to_string(),
            actor: None,
            order: order(100, false),
            block_time_unix_seconds: 1,
            decoder_version_id: "test-decoder".to_owned(),
            payload: OperationPayloadV1::InitializeMarket {
                token0: TOKEN0.to_string(),
                token1: TOKEN1.to_string(),
                token0_decimals: 6,
                token1_decimals: 6,
                gross_amount0: amount(gross as u128),
                gross_amount1: amount(gross as u128),
                vault_credit0: amount(gross as u128),
                vault_credit1: amount(gross as u128),
                minted_lp: amount(minted as u128),
                min_liquidity_out: amount(0),
                fields: BTreeMap::new(),
            },
            assertions: Vec::new(),
        };
        let accounts = V1DecodedAccountsV1 {
            program_version: VERSION,
            pair_key: PAIR_KEY,
            pair,
            rate_model_key: RATE_MODEL_KEY,
            rate_model: model,
            futarchy_authority: authority(),
            positions: Vec::new(),
            custody: crate::v1_engine::V1CustodyCheckpointV1 {
                reserve_vault0: gross,
                reserve_vault1: gross,
                collateral_vault0: 0,
                collateral_vault1: 0,
            },
            revenue: zero_revenue(),
        };

        let (replay, record) =
            V1FactualReplay::from_initialized_operation(&operation, accounts).unwrap();

        assert_eq!(replay.last_order(), Some(order(100, false)));
        assert!(record.conservation.iter().all(|identity| identity.holds));
        assert_eq!(record.checkpoint_after.pair.total_supply, gross);
        assert!(record
            .reconciliations
            .iter()
            .all(|reconciliation| reconciliation.raw_unit_difference == "0"));
        assert_eq!(CANONICAL_SCHEMA_VERSION, 1);
    }
}

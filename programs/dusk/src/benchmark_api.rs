//! Off-chain-only access to Dusk's native market transitions.
//!
//! This module is available only with the non-default `benchmark` feature.
//! It deliberately wraps the same integer state machines used by instruction
//! preview and execution; it does not provide an alternate economic model.

use anchor_lang::{prelude::*, AccountDeserialize, AccountSerialize};

use crate::{
    constants::{
        BPS_DENOMINATOR, LEVERAGE_INITIAL_MARGIN_BPS, LEVERAGE_MAINTENANCE_BUFFER_BPS, LEVERAGE_MAX_UNWIND_IMPACT_BPS,
        LIQUIDATION_AUCTION_DURATION_SECONDS, LIQUIDATION_BACKSTOP_CALLER_BPS, MAX_COLLATERAL_FACTOR_BPS,
        MAX_REFERRAL_INTEREST_SHARE_BPS, NAD, REFERRAL_ACCRUAL_SEED_PREFIX, REFERRAL_PARTNER_SEED_PREFIX,
        YIELD_ACCOUNT_SEED_PREFIX,
    },
    errors::ErrorCode,
    instructions::{
        leverage_entry_limit_satisfied, leverage_entry_price_nad, leverage_position_pda,
        rebalance_executes_token_changes, reconcile_live_hlp_supply, record_hlp_interest_credit,
        record_inline_hlp_interest_credit, BorrowCapacityPreview, PreparedSwap, SwapRequest,
    },
    market::{
        leverage_debt_from_margin,
        liquidity::{SingleSidedLiquidityReceipt, SwapCashPolicy},
        AmmSwapQuote, DynamicBorrowTerms, HlpRebalanceReceipt, LeverageCloseReceipt, LeverageOpenReceipt,
        LeverageSwapFeeCredit, LeverageSwapQuote, PreparedLeverageSwap,
    },
    math::{ceil_div, denormalize_from_nad_floor, health_bps, normalize_to_nad},
    state::{
        BorrowPosition, CollateralReceipt, Debt, DebtReceipt, HlpYieldEligibility, LeveragePosition, Market,
        MarketAsset, MarketConfig, MarketSide, ProtocolAuctionSplit, ReferralAccrual, ReferralInterestQuote,
        ReferralPartner, Risk, YieldAccount, YieldTokenKind,
    },
};

use crate::market::{LiquidationPricing, LiquidationReceipt, LiquidationTerms};

/// Canonical clock inputs frozen for one replay operation.
///
/// Slots drive protocol accrual and controller state. Unix timestamps are
/// used only by time-based launch fee policy and should come from the same
/// historical block as `slot`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkClock {
    pub slot: u64,
    pub unix_timestamp: i64,
}

/// Arguments for deterministic construction through [`Market::initialize`].
///
/// The caller supplies synthetic keys when replaying a market that never
/// existed on Dusk. Token amounts remain raw integer atoms throughout.
#[derive(Clone, Copy)]
pub struct BenchmarkMarketInit {
    pub ylp_mint: Pubkey,
    pub base_side: MarketSide,
    pub quote_side: MarketSide,
    pub config: MarketConfig,
    pub base_hlp_ylp_vault: Pubkey,
    pub quote_hlp_ylp_vault: Pubkey,
    pub base_insurance_vault: Pubkey,
    pub quote_insurance_vault: Pubkey,
    pub params_hash: [u8; 32],
    pub initial_liquidity_authority: Pubkey,
    pub bootstrap_price_nad: u64,
    pub launch_fee_progress_offset: u16,
    pub bump: u8,
}

/// Net reserve credit and protocol economics frozen for one native swap.
///
/// `reserve_credit` is the amount received by the reserve after any token
/// transfer fee. The historical extractor remains responsible for deriving
/// it from the gross user transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkSwapRequest {
    pub asset_in: MarketAsset,
    pub reserve_credit: u64,
    pub protocol_fee_bps: u16,
    pub protocol_auction_split: ProtocolAuctionSplit,
}

/// State-only result shared by pure preview and committed replay.
///
/// Token CPIs are intentionally outside this boundary. The rebalances expose
/// the exact token changes that an external replay ledger must settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkSwapExecution {
    pub quote: AmmSwapQuote,
    pub base_rebalance: HlpRebalanceReceipt,
    pub quote_rebalance: HlpRebalanceReceipt,
    pub fee_eligible_ylp_supply: u64,
    pub interest_eligibility: HlpYieldEligibility,
}

/// Minimal native curve state needed for quote-depth and conservation metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkCurveSnapshot {
    pub base_curve_reserve: u64,
    pub quote_curve_reserve: u64,
    pub spot_price_nad: u64,
    pub center_price_nad: u64,
    pub curve_depth_nad: u128,
    pub volatility_accumulator_nad: u64,
    pub last_observation_slot: u64,
}

/// Raw custody movements caused by one state transition. Positive and
/// negative directions remain separate so transfer-fee credits never get
/// confused with gross vault debits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkAssetCashFlow {
    pub reserve_vault_credit: u64,
    pub reserve_vault_debit: u64,
    pub collateral_vault_credit: u64,
    pub collateral_vault_debit: u64,
    pub interest_vault_credit: u64,
    pub insurance_vault_credit: u64,
    pub insurance_vault_debit: u64,
    pub recipient_credit: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkCashFlow {
    pub base: BenchmarkAssetCashFlow,
    pub quote: BenchmarkAssetCashFlow,
}

impl BenchmarkCashFlow {
    fn side_mut(&mut self, asset: MarketAsset) -> &mut BenchmarkAssetCashFlow {
        match asset {
            MarketAsset::Base => &mut self.base,
            MarketAsset::Quote => &mut self.quote,
        }
    }
}

/// Revenue ownership and custody for one token side at a replay checkpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkRevenueSideCheckpoint {
    pub swap_fee_growth_index_q64: u128,
    pub interest_growth_index_q64: u128,
    pub swap_fee_growth_remainder_scaled: u64,
    pub interest_growth_remainder_scaled: u64,
    pub hlp_funding_interest_growth_remainder_scaled: u64,
    pub lp_swap_fee_liability: u64,
    pub lp_interest_liability: u64,
    pub unallocated_lp_swap_fee_liability: u64,
    pub unallocated_lp_interest_liability: u64,
    pub protocol_swap_fee_liability: u64,
    pub protocol_interest_fee_liability: u64,
    pub buyback_swap_fee_liability: u64,
    pub buyback_interest_fee_liability: u64,
    pub referral_interest_liability: u64,
    pub swap_fee_custody_balance: u64,
    pub interest_vault_balance: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkRevenueCheckpoint {
    pub base: BenchmarkRevenueSideCheckpoint,
    pub quote: BenchmarkRevenueSideCheckpoint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkMarketSideCheckpoint {
    pub live_reserve: u64,
    pub cash_reserve: u64,
    pub protected_recenter_reserve: u64,
    pub base_hlp_backing_inventory: u64,
    pub quote_hlp_backing_inventory: u64,
    pub ylp_supply: u64,
    pub fixed_debt_shares: u128,
    pub fixed_debt: u128,
    pub fixed_debt_principal: u64,
    pub isolated_debt_shares: u128,
    pub isolated_debt: u128,
    pub isolated_debt_principal: u64,
    pub borrow_index_nad: u128,
    pub rate_at_target_nad: u128,
    pub last_accrual_slot: u64,
    pub daily_borrow_bucket: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkHlpCheckpoint {
    pub ylp_shares: u64,
    pub hlp_supply: u64,
    pub debt_shares: u128,
    pub debt_principal: u64,
    pub base_hlp_live_reserve: u64,
    pub quote_hlp_live_reserve: u64,
    pub residual_exposure: i128,
    pub last_nav_nad: u128,
    pub cached_settlement_price_nad: u128,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkMarketCheckpoint {
    pub market_key: Option<Pubkey>,
    pub clock: BenchmarkClock,
    pub version: u8,
    pub base: BenchmarkMarketSideCheckpoint,
    pub quote: BenchmarkMarketSideCheckpoint,
    pub base_hlp: BenchmarkHlpCheckpoint,
    pub quote_hlp: BenchmarkHlpCheckpoint,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub last_update_slot: u64,
    pub last_risk_snapshot_slot: u64,
    pub curve_revision: u64,
    pub risk_revision: u64,
    pub reduce_only: bool,
}

/// Aggregate unpaid public-borrow interest at one native market checkpoint.
///
/// Fixed-margin and isolated debt are intentionally kept in the same public
/// lane here. hLP funding debt is excluded because its entitlement is created
/// only when the hLP settlement pays the interest vault.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkPublicInterestCheckpoint {
    pub base: u128,
    pub quote: u128,
}

impl BenchmarkPublicInterestCheckpoint {
    pub const fn side(self, asset: MarketAsset) -> u128 {
        match asset {
            MarketAsset::Base => self.base,
            MarketAsset::Quote => self.quote,
        }
    }
}

/// Exact token-program outcome for public interest removed by one transition.
/// Principal transfers and hLP funding settlements must not be included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkPublicInterestPayment {
    pub gross_cash_interest_paid: u64,
    pub net_interest_vault_credit: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkPublicInterestPayments {
    pub base: BenchmarkPublicInterestPayment,
    pub quote: BenchmarkPublicInterestPayment,
}

/// Exact public-interest identity for one token side and one replay operation.
///
/// `transition_interest_created` is normally zero, but is required to make the
/// identity total for share-rounded borrows whose new indexed debt can exceed
/// newly recorded principal by one atom. `interest_written_off` is a shortfall
/// memo and must never be classified as cash or economic revenue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkPublicInterestSideTransition {
    pub outstanding_before_operation: u128,
    pub outstanding_after_clock_before_transition: u128,
    pub outstanding_after_transition: u128,
    pub clock_accrued: u128,
    pub transition_interest_created: u128,
    pub gross_cash_interest_paid: u128,
    pub net_interest_vault_credit: u128,
    pub total_interest_removed: u128,
    pub interest_written_off: u128,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkPublicInterestTransition {
    pub base: BenchmarkPublicInterestSideTransition,
    pub quote: BenchmarkPublicInterestSideTransition,
}

impl BenchmarkPublicInterestTransition {
    /// Constructs and proves the complete public-interest identity. All three
    /// checkpoints must belong to the same staged operation.
    pub fn checked(
        before_operation: BenchmarkPublicInterestCheckpoint,
        after_clock_before_transition: BenchmarkPublicInterestCheckpoint,
        after_transition: BenchmarkPublicInterestCheckpoint,
        payments: BenchmarkPublicInterestPayments,
    ) -> Result<Self> {
        Ok(Self {
            base: checked_public_interest_side_transition(
                before_operation.base,
                after_clock_before_transition.base,
                after_transition.base,
                payments.base,
            )?,
            quote: checked_public_interest_side_transition(
                before_operation.quote,
                after_clock_before_transition.quote,
                after_transition.quote,
                payments.quote,
            )?,
        })
    }

    pub fn identity(checkpoint: BenchmarkPublicInterestCheckpoint) -> Self {
        Self::checked(
            checkpoint,
            checkpoint,
            checkpoint,
            BenchmarkPublicInterestPayments::default(),
        )
        .expect("an identity public-interest transition is infallible")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkBorrowPositionCheckpoint {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position_id: Pubkey,
    pub base_collateral: u64,
    pub quote_collateral: u64,
    pub fixed_base_shares: u128,
    pub fixed_quote_shares: u128,
    pub fixed_base_debt: u128,
    pub fixed_quote_debt: u128,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
    pub base_referral_partner: Pubkey,
    pub quote_referral_partner: Pubkey,
    pub base_referral_interest_share_bps: u16,
    pub quote_referral_interest_share_bps: u16,
    pub auction_debt_asset: u8,
    pub auction_start_time: i64,
    pub auction_start_price_nad: u64,
    pub auction_floor_price_nad: u64,
    pub bump: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkMarketExecution<T> {
    pub receipt: T,
    pub cash: BenchmarkCashFlow,
    pub revenue_before: BenchmarkRevenueCheckpoint,
    pub revenue_after: BenchmarkRevenueCheckpoint,
    pub market_after: BenchmarkMarketCheckpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkPositionExecution<T> {
    pub market: BenchmarkMarketExecution<T>,
    pub position_after: BenchmarkBorrowPositionCheckpoint,
}

/// Exact token-account result supplied by the replay caller.
///
/// Dusk passes `source_debit` to the token program and observes
/// `destination_credit` after the CPI. The benchmark never derives one from
/// the other, so historical Token-2022 fees and hooks remain extractor-owned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkTokenTransferOutcome {
    pub source_debit: u64,
    pub destination_credit: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkYieldAccountCheckpoint {
    pub account_key: Pubkey,
    pub owner: Pubkey,
    pub market: Pubkey,
    pub lp_mint: Pubkey,
    pub asset_mint: Pubkey,
    pub token_kind: u8,
    pub recipient: Pubkey,
    pub swap_fee_checkpoint_q64: u128,
    pub interest_checkpoint_q64: u128,
    pub accrued_swap_fee_amount: u64,
    pub accrued_interest_amount: u64,
    pub swap_fee_remainder_q64: u64,
    pub interest_remainder_q64: u64,
    pub bump: u8,
}

/// Account-level state outside [`Market`] that one hLP mint transition owns.
///
/// `hlp_mint_supply` is kept separately from the holder balance because the
/// production instruction reconciles the live Token-2022 mint supply before
/// every entry and exit. `hlp_vault_ylp_balance` is the actual token-account
/// balance backing the native vault's `ylp_shares` ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkHlpOwnedCheckpoint {
    pub target_asset: MarketAsset,
    pub owner: Pubkey,
    pub hlp_mint: Pubkey,
    pub hlp_mint_supply: u64,
    pub holder_hlp_token_balance: u64,
    pub hlp_vault_ylp_balance: u64,
    pub measured_base_interest_vault_credits: u64,
    pub measured_quote_interest_vault_credits: u64,
    pub base_yield_account: BenchmarkYieldAccountCheckpoint,
    pub quote_yield_account: BenchmarkYieldAccountCheckpoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkHlpEntryRequest {
    pub clock: BenchmarkClock,
    /// Gross owner-token debit and exact reserve-vault credit.
    pub target_transfer: BenchmarkTokenTransferOutcome,
    pub min_hlp_amount: u64,
    pub global_reduce_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkHlpWithdrawRequest {
    pub clock: BenchmarkClock,
    pub hlp_amount: u64,
    /// Exact gross reserve debit and net owner credit for released principal.
    pub target_transfer: BenchmarkTokenTransferOutcome,
    /// Exact gross borrowed-reserve debit and net interest-vault credit.
    pub interest_transfer: BenchmarkTokenTransferOutcome,
    pub min_target_recipient_credit: u64,
    pub protocol_interest_fee_bps: u16,
    pub protocol_auction_split: ProtocolAuctionSplit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkHlpEntryReceipt {
    pub native: SingleSidedLiquidityReceipt,
    pub target_transfer: BenchmarkTokenTransferOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkHlpWithdrawReceipt {
    pub native: SingleSidedLiquidityReceipt,
    pub target_transfer: BenchmarkTokenTransferOutcome,
    pub interest_transfer: BenchmarkTokenTransferOutcome,
}

/// A native-feasible upper-bound plan. The caller must supply the exact
/// transfer outcome for `target_reserve_credit` before execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkMaximumHlpEntry {
    pub clock: BenchmarkClock,
    pub target_asset: MarketAsset,
    pub maximum_considered_reserve_credit: u64,
    pub target_reserve_credit: u64,
    pub native: SingleSidedLiquidityReceipt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkHlpExecution<T> {
    pub market: BenchmarkMarketExecution<T>,
    pub external_after: BenchmarkHlpOwnedCheckpoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkHlpAwareSwapRequest {
    pub swap: BenchmarkSwapRequest,
    /// Measured quote-interest-vault outcome for the base-target hLP.
    pub base_hlp_interest_transfer: BenchmarkTokenTransferOutcome,
    /// Measured base-interest-vault outcome for the quote-target hLP.
    pub quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome,
    pub protocol_interest_fee_bps: u16,
    pub protocol_auction_split: ProtocolAuctionSplit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkHlpAwareSwapExecution {
    pub swap: BenchmarkSwapExecution,
    pub hlp_interest_cash: BenchmarkCashFlow,
    pub revenue_before: BenchmarkRevenueCheckpoint,
    pub revenue_after: BenchmarkRevenueCheckpoint,
    pub market_after: BenchmarkMarketCheckpoint,
    pub base_external_after: BenchmarkHlpOwnedCheckpoint,
    pub quote_external_after: BenchmarkHlpOwnedCheckpoint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkYlpLiquidityReceipt {
    pub ylp_amount: u64,
    pub ylp_supply: u64,
    pub base_reserve_amount: u64,
    pub quote_reserve_amount: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkAddYlpRequest {
    pub clock: BenchmarkClock,
    pub owner: Pubkey,
    /// Exact base reserve-vault credit measured after token transfer fees.
    pub base_reserve_credit: u64,
    /// Exact quote reserve-vault credit measured after token transfer fees.
    pub quote_reserve_credit: u64,
    pub min_ylp_amount: u64,
    pub global_reduce_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkRemoveYlpRequest {
    pub clock: BenchmarkClock,
    pub ylp_amount: u64,
    pub owner_ylp_balance_before: u64,
    pub base_recipient_credit: u64,
    pub quote_recipient_credit: u64,
    pub min_base_recipient_credit: u64,
    pub min_quote_recipient_credit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkDepositCollateralRequest {
    pub clock: BenchmarkClock,
    pub collateral_asset: MarketAsset,
    /// Exact measured collateral-vault credit after token transfer fees.
    pub collateral_credit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkWithdrawCollateralRequest {
    pub clock: BenchmarkClock,
    pub collateral_asset: MarketAsset,
    pub collateral_debit: u64,
    pub collateral_vault_balance_before: u64,
    pub recipient_credit: u64,
    pub min_recipient_credit: u64,
    pub min_liquidation_cf_bps: u16,
    pub global_reduce_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkBorrowRequest {
    pub clock: BenchmarkClock,
    pub debt_asset: MarketAsset,
    pub borrow_amount: u64,
    pub recipient_credit: u64,
    pub min_recipient_credit: u64,
    pub min_liquidation_cf_bps: u16,
    pub global_reduce_only: bool,
    /// Exact binding selected by the decoded authority/referral accounts.
    pub referral_partner: Pubkey,
    pub referral_interest_share_bps: u16,
    pub referral_interest_share_cap_bps: u16,
}

/// Exact native bound that ties the actionable existing-position borrow
/// capacity. Multiple constraints may tie at the same raw atom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BenchmarkBorrowCapacityConstraint {
    Underwriting,
    GlobalHealthFloor,
    Cash,
    DailyBorrowBucket,
    ReduceOnly,
}

/// Existing-position borrow capacity after advancing a fork through the same
/// native clock/accrual order as [`BenchmarkMarket::execute_borrow`].
///
/// `gross_admissible_max_debt` applies the dynamic position-underwriting
/// predicate without cash, daily-bucket, global-floor, or reduce-only gates.
/// `actionable_max_debt` applies every native execution gate. Bounds ending in
/// `_additional` are requested transfer atoms, while the two max-debt fields
/// include debt already owned by the position and preserve share rounding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkExistingBorrowCapacity {
    pub debt_asset: MarketAsset,
    pub debt_before: u128,
    pub gross_admissible_max_debt: u128,
    pub actionable_max_debt: u128,
    pub underwriting_max_additional: u64,
    pub global_health_floor_max_additional: u64,
    pub cash_max_additional: u64,
    pub daily_bucket_max_additional: u64,
    /// `Some(0)` when reduce-only is active; `None` means this boolean gate is
    /// not an amount bound and avoids an integer sentinel.
    pub reduce_only_max_additional: Option<u64>,
    pub actionable_max_additional: u64,
    pub current_underwriting_satisfied: bool,
    pub current_global_health_floor_satisfied: bool,
    pub market_reduce_only: bool,
    pub global_reduce_only: bool,
    pub limiting_constraints: Vec<BenchmarkBorrowCapacityConstraint>,
}

/// Exact share-rounded request solving one desired post-borrow debt target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkBorrowTargetRequest {
    pub target_debt: u128,
    pub borrow_request: u64,
    pub projected_debt: u128,
    pub target_debt_gap: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkRepayRequest {
    pub clock: BenchmarkClock,
    /// Maximum exact reserve credit available after the user's transfer fee.
    pub max_repay_credit: u64,
    /// Exact credit measured in the interest vault after routing paid interest.
    pub interest_vault_credit: u64,
    pub protocol_interest_fee_bps: u16,
    pub protocol_auction_split: ProtocolAuctionSplit,
    pub referral_interest_amount: u64,
    pub debt_asset: MarketAsset,
}

const BENCHMARK_LIQUIDATION_START_PREMIUM_NUMERATOR: u64 = 105;
const BENCHMARK_LIQUIDATION_START_PREMIUM_DENOMINATOR: u64 = 100;
const BENCHMARK_LIQUIDATION_RESERVATION_FEE_BPS: u64 = 20;

/// The two executable branches of Dusk's native borrow-position auction.
///
/// A normal bid cannot draw insurance or socialize loss. Floor settlement is
/// admitted only after the explicit external-bid window expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchmarkLiquidationPhase {
    Bid,
    Floor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkStartLiquidationAuctionRequest {
    pub clock: BenchmarkClock,
    pub debt_asset: MarketAsset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLiquidationAuctionReceipt {
    pub debt_asset: MarketAsset,
    pub reference_price_nad: u64,
    pub start_price_nad: u64,
    pub floor_price_nad: u64,
    pub start_time: i64,
    /// Earliest whole-second clock accepted by the native floor predicate.
    pub first_floor_unix_timestamp: i64,
}

/// Exact state-independent inputs used to quote one auction transition.
///
/// `max_repay_credit` is the decoded caller cap after the debt mint's transfer
/// fee. The quote rounds it down with native debt-share math before any token
/// account is mutated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLiquidationPlanRequest {
    pub clock: BenchmarkClock,
    pub debt_asset: MarketAsset,
    pub phase: BenchmarkLiquidationPhase,
    /// External debt credit for `Bid`; must be zero for `Floor`.
    pub max_repay_credit: u64,
    /// Net collateral credit received by the reserve for `Floor`; must be zero
    /// for `Bid`.
    pub collateral_reserve_credit: u64,
    pub protocol_swap_fee_bps: u16,
    pub protocol_auction_split: ProtocolAuctionSplit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLiquidationPlan {
    pub phase: BenchmarkLiquidationPhase,
    pub debt_asset: MarketAsset,
    pub auction_price_nad: u64,
    pub first_floor_unix_timestamp: i64,
    pub terms: LiquidationTerms,
    pub repay_credit: u64,
    pub collateral_consumed: u64,
    pub caller_bounty: u64,
    pub collateral_swap_debit: u64,
    pub collateral_reserve_credit: u64,
    pub swap_output: u64,
    /// Gross insurance-vault debit requested by the native floor path. The
    /// corresponding reserve credit remains caller-supplied Token-2022 state.
    pub insurance_draw_debit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLiquidationPreviewRequest {
    pub plan: BenchmarkLiquidationPlanRequest,
    /// Measured reserve-vault credit for `insurance_draw_debit`.
    pub insurance_draw_credit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLiquidationPreview {
    pub plan: BenchmarkLiquidationPlan,
    pub native: LiquidationReceipt,
    pub owner_residual: u64,
}

/// Every fungible token balance touched by a borrow-position liquidation.
///
/// The wrapper owns this state separately from [`Market`] because production
/// token accounts roll back together with account data. Counterfactual callers
/// may initialize balances to the exact custody lower bound when history proves
/// there were no unsolicited donations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkLiquidationTokenBalances {
    pub liquidator_base_balance: u64,
    pub liquidator_quote_balance: u64,
    pub owner_base_balance: u64,
    pub owner_quote_balance: u64,
    pub base_reserve_vault_balance: u64,
    pub quote_reserve_vault_balance: u64,
    pub base_interest_vault_balance: u64,
    pub quote_interest_vault_balance: u64,
    pub base_collateral_vault_balance: u64,
    pub quote_collateral_vault_balance: u64,
    pub base_insurance_vault_balance: u64,
    pub quote_insurance_vault_balance: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLiquidationExecuteRequest {
    pub preview: BenchmarkLiquidationPreviewRequest,
    /// Gross instruction argument. It may exceed the actual source debit when
    /// debt-share rounding makes the exact native repayment smaller.
    pub max_repay_source_debit: u64,
    pub debt_transfer: BenchmarkTokenTransferOutcome,
    pub insurance_draw_transfer: BenchmarkTokenTransferOutcome,
    pub interest_transfer: BenchmarkTokenTransferOutcome,
    pub collateral_transfer: BenchmarkTokenTransferOutcome,
    pub collateral_swap_transfer: BenchmarkTokenTransferOutcome,
    pub owner_residual_transfer: BenchmarkTokenTransferOutcome,
    pub insurance_funding_transfer: BenchmarkTokenTransferOutcome,
    pub min_collateral_recipient_credit: u64,
    pub protocol_interest_fee_bps: u16,
    pub protocol_auction_split: ProtocolAuctionSplit,
    pub referral_interest_amount: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLiquidationExecutionReceipt {
    pub plan: BenchmarkLiquidationPlan,
    pub native: LiquidationReceipt,
    pub debt_transfer: BenchmarkTokenTransferOutcome,
    pub insurance_draw_transfer: BenchmarkTokenTransferOutcome,
    pub interest_transfer: BenchmarkTokenTransferOutcome,
    pub collateral_transfer: BenchmarkTokenTransferOutcome,
    pub collateral_swap_transfer: BenchmarkTokenTransferOutcome,
    pub owner_residual_transfer: BenchmarkTokenTransferOutcome,
    pub insurance_funding_transfer: BenchmarkTokenTransferOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkLiquidationExecution {
    pub market: BenchmarkMarketExecution<BenchmarkLiquidationExecutionReceipt>,
    pub position_after: BenchmarkBorrowPositionCheckpoint,
    pub token_balances_after: BenchmarkLiquidationTokenBalances,
}

/// Exact token-account balances owned by an isolated-leverage replay.
///
/// Reserve balances may include unsolicited donations, just like their
/// on-chain counterparts. Native custody checks therefore enforce the same
/// lower bound instead of requiring equality.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkLeverageTokenBalances {
    pub owner_base_balance: u64,
    pub owner_quote_balance: u64,
    pub base_reserve_vault_balance: u64,
    pub quote_reserve_vault_balance: u64,
    pub base_interest_vault_balance: u64,
    pub quote_interest_vault_balance: u64,
    pub base_leverage_collateral_vault_balance: u64,
    pub quote_leverage_collateral_vault_balance: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkReferralCheckpoint {
    pub partner_key: Pubkey,
    pub authority: Pubkey,
    pub recipient: Pubkey,
    pub configured_interest_share_bps: u16,
    pub active: bool,
    pub partner_bump: u8,
    pub accrual_key: Pubkey,
    pub market: Pubkey,
    pub asset_mint: Pubkey,
    pub accrued_amount: u64,
    pub accrual_bump: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkLeveragePositionCheckpoint {
    pub account_key: Pubkey,
    pub account_exists: bool,
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position_id: Pubkey,
    pub referral_partner: Pubkey,
    pub referral_interest_share_bps: u16,
    pub debt_asset: u8,
    pub collateral_amount: u64,
    pub margin_amount: u64,
    pub open_notional: u64,
    pub debt_principal: u128,
    pub debt_shares: u128,
    pub multiplier_bps: u64,
    pub opened_at: i64,
    pub opened_slot: u64,
    pub bump: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkLeverageOwnedCheckpoint {
    pub funding_owner: Pubkey,
    pub position_owner: Pubkey,
    pub position_id: Pubkey,
    pub position: BenchmarkLeveragePositionCheckpoint,
    pub token_balances: BenchmarkLeverageTokenBalances,
    pub referral: Option<BenchmarkReferralCheckpoint>,
}

/// Exact current-position health in debt-token atoms.
///
/// `minimum_healthy_closeout_value` is the first integer closeout value that
/// satisfies Dusk's strict maintenance predicate. It is deliberately not
/// presented as an oracle price: concentrated-curve unwind impact makes that
/// conversion state- and size-dependent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLeverageMetrics {
    pub debt_asset: MarketAsset,
    pub debt_amount: u64,
    pub collateral_amount: u64,
    pub closeout_value: u64,
    pub equity: u64,
    pub equity_bps: u128,
    pub initial_margin_bps: u16,
    pub maintenance_margin_bps: u16,
    pub minimum_healthy_closeout_value: u64,
    pub maintenance_shortfall: u64,
    pub spot_value: u64,
    pub unwind_impact_bps: u128,
    pub maximum_open_unwind_impact_bps: u16,
    pub liquidatable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLeveragePolicy {
    pub protocol_swap_fee_bps: u16,
    pub protocol_interest_fee_bps: u16,
    pub protocol_auction_split: ProtocolAuctionSplit,
    pub max_referral_interest_share_bps: u16,
    pub global_reduce_only: bool,
    /// Result of decoding the collateral mint extensions and applying the
    /// native leverage-risk mint validator.
    pub collateral_mint_is_leverage_eligible: bool,
    /// Required only while the launch same-transaction guard is active. The
    /// archival caller derives this from the containing transaction.
    pub launch_same_transaction_guard_satisfied: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkPrepareLeverageOpenRequest {
    pub clock: BenchmarkClock,
    pub debt_asset: MarketAsset,
    /// Gross debit from the funding owner's account and measured net reserve
    /// credit. The native debt calculation uses only the latter.
    pub margin_transfer: BenchmarkTokenTransferOutcome,
    pub multiplier_bps: u64,
    pub min_collateral_out: u64,
    pub limit_price_nad: u64,
    pub requested_referrer: Option<Pubkey>,
    pub policy: BenchmarkLeveragePolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkExecuteLeverageOpenRequest {
    /// Gross reserve debit and exact leverage-vault credit for the purchased
    /// collateral.
    pub collateral_transfer: BenchmarkTokenTransferOutcome,
    pub base_hlp_interest_transfer: BenchmarkTokenTransferOutcome,
    pub quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLeverageOpenQuote {
    pub borrowed_amount: u64,
    pub notional: u64,
    pub swap: LeverageSwapQuote,
    pub referral_partner: Pubkey,
    pub referral_interest_share_bps: u16,
    /// Exact native debits that execution must route through the token
    /// program. Destination credits remain caller-owned because they depend
    /// on the mint's historical Token-2022 configuration.
    pub settlement: BenchmarkLeverageSettlementRequirements,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkLeverageSettlementRequirements {
    /// Interest paid by the base hLP vault, denominated in quote atoms.
    pub base_hlp_interest_debit: u64,
    /// Interest paid by the quote hLP vault, denominated in base atoms.
    pub quote_hlp_interest_debit: u64,
    /// Position interest routed on close, denominated in debt-asset atoms.
    /// This is always zero for an open quote.
    pub position_interest_debit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkPrepareLeverageCloseRequest {
    pub clock: BenchmarkClock,
    /// Gross leverage-vault debit and exact reserve credit. Native Dusk only
    /// admits fee-free collateral mints, so these must also be equal.
    pub collateral_transfer: BenchmarkTokenTransferOutcome,
    pub min_residual_out: u64,
    pub policy: BenchmarkLeveragePolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkExecuteLeverageCloseRequest {
    /// Gross debt-reserve debit and exact owner-account credit.
    pub residual_transfer: BenchmarkTokenTransferOutcome,
    /// Gross debt-reserve debit and exact debt-interest-vault credit.
    pub interest_transfer: BenchmarkTokenTransferOutcome,
    pub base_hlp_interest_transfer: BenchmarkTokenTransferOutcome,
    pub quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLeverageCloseQuote {
    pub debt_amount: u64,
    pub collateral_sold: u64,
    pub swap: LeverageSwapQuote,
    pub expected_gross_residual: u64,
    pub metrics_before: BenchmarkLeverageMetrics,
    /// Exact native debits required by the prepared full-unwind settlement.
    pub settlement: BenchmarkLeverageSettlementRequirements,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLeverageOpenExecutionReceipt {
    pub native: LeverageOpenReceipt,
    pub margin_transfer: BenchmarkTokenTransferOutcome,
    pub collateral_transfer: BenchmarkTokenTransferOutcome,
    pub metrics: BenchmarkLeverageMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkLeverageCloseExecutionReceipt {
    pub native: LeverageCloseReceipt,
    pub collateral_transfer: BenchmarkTokenTransferOutcome,
    pub residual_transfer: BenchmarkTokenTransferOutcome,
    pub interest_transfer: BenchmarkTokenTransferOutcome,
    pub referral_interest: ReferralInterestQuote,
    pub metrics_before: BenchmarkLeverageMetrics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkLeverageExecution<T> {
    pub market: BenchmarkMarketExecution<T>,
    pub leverage_after: BenchmarkLeverageOwnedCheckpoint,
    pub base_hlp_after: BenchmarkHlpOwnedCheckpoint,
    pub quote_hlp_after: BenchmarkHlpOwnedCheckpoint,
}

/// External state that must be owned before a transition can be made atomic.
/// The benchmark deliberately publishes these requirements instead of
/// approximating account-level behavior with market-only math.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchmarkExternalStateRequirement {
    LeveragePositionAccount,
    PreparedLeverageSwapTracking,
    HlpHolderTokenBalance,
    HlpHolderBaseYieldAccount,
    HlpHolderQuoteYieldAccount,
    HlpVaultYlpTokenBalance,
    InterestVaultTransferCredits,
    Token2022MintAndHookState,
    ReferralAccountState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkDeferredTransitionRequirements {
    pub requirements: &'static [BenchmarkExternalStateRequirement],
}

// The owned leverage wrapper below closes this account-level boundary. Mint
// extension and hook behavior is supplied as exact typed transfer outcomes.
const LEVERAGE_EXTERNAL_REQUIREMENTS: &[BenchmarkExternalStateRequirement] = &[];

// hLP entry/exit now owns the complete account-level rollback boundary. Exact
// Token-2022 transfer results are typed operation inputs, not missing state.
const HLP_EXTERNAL_REQUIREMENTS: &[BenchmarkExternalStateRequirement] = &[];

/// A deterministic, forkable off-chain market state.
///
/// The wrapper owns no wall-clock source. Every operation uses the explicitly
/// supplied [`BenchmarkClock`], preventing accidental reads from `Clock` while
/// replaying historical slots.
pub struct BenchmarkMarket {
    market: Box<Market>,
    market_key: Option<Pubkey>,
    clock: BenchmarkClock,
}

/// An owned native borrow-position account. Market/position operations clone
/// and commit this object together with [`BenchmarkMarket`], preserving the
/// transaction-wide rollback boundary of the on-chain instruction.
pub struct BenchmarkBorrowPosition {
    position: Box<BorrowPosition>,
}

/// Forkable account-level state for one hLP mint and holder.
///
/// The market remains owned by [`BenchmarkMarket`]; hLP operations stage and
/// commit both objects together just as the on-chain instruction rolls back
/// all account writes when any later CPI or slippage check fails.
pub struct BenchmarkHlpOwnedState {
    target_asset: MarketAsset,
    owner: Pubkey,
    hlp_mint: Pubkey,
    hlp_mint_supply: u64,
    holder_hlp_token_balance: u64,
    hlp_vault_ylp_balance: u64,
    measured_base_interest_vault_credits: u64,
    measured_quote_interest_vault_credits: u64,
    base_yield_account_key: Pubkey,
    quote_yield_account_key: Pubkey,
    base_yield_account: Box<YieldAccount>,
    quote_yield_account: Box<YieldAccount>,
}

/// Native referral partner and claim ledger owned by the benchmark rollback
/// boundary. The account keys and bumps are validated against the same PDAs
/// used by the instructions before either account may influence a position.
pub struct BenchmarkReferralOwnedState {
    partner_key: Pubkey,
    partner: Box<ReferralPartner>,
    accrual_key: Pubkey,
    accrual: Box<ReferralAccrual>,
}

/// One isolated position plus every token/referral account mutated by its
/// open and full-unwind instructions.
pub struct BenchmarkLeverageOwnedState {
    funding_owner: Pubkey,
    position_owner: Pubkey,
    position_id: Pubkey,
    position_key: Pubkey,
    position_exists: bool,
    position: Box<LeveragePosition>,
    token_balances: BenchmarkLeverageTokenBalances,
    referral: Option<BenchmarkReferralOwnedState>,
}

/// Opaque prepared open. It owns the post-preparation native Market and
/// `PreparedLeverageSwap`; execution rejects it if any starting account has
/// changed since quote time.
pub struct BenchmarkPreparedLeverageOpen {
    clock_before: BenchmarkClock,
    market_before: Vec<u8>,
    leverage_before: BenchmarkLeverageOwnedCheckpoint,
    base_hlp_before: BenchmarkHlpOwnedCheckpoint,
    quote_hlp_before: BenchmarkHlpOwnedCheckpoint,
    prepared_market: Box<Market>,
    prepared_swap: PreparedLeverageSwap,
    request: BenchmarkPrepareLeverageOpenRequest,
    quote: BenchmarkLeverageOpenQuote,
}

impl BenchmarkPreparedLeverageOpen {
    pub const fn quote(&self) -> BenchmarkLeverageOpenQuote {
        self.quote
    }

    pub fn try_fork(&self) -> Result<Self> {
        Ok(Self {
            clock_before: self.clock_before,
            market_before: self.market_before.clone(),
            leverage_before: self.leverage_before,
            base_hlp_before: self.base_hlp_before.clone(),
            quote_hlp_before: self.quote_hlp_before.clone(),
            prepared_market: Box::new(clone_market(&self.prepared_market)?),
            prepared_swap: self.prepared_swap.clone(),
            request: self.request,
            quote: self.quote,
        })
    }
}

/// Opaque prepared full unwind with the same stale-quote protection as open.
pub struct BenchmarkPreparedLeverageClose {
    clock_before: BenchmarkClock,
    market_before: Vec<u8>,
    leverage_before: BenchmarkLeverageOwnedCheckpoint,
    base_hlp_before: BenchmarkHlpOwnedCheckpoint,
    quote_hlp_before: BenchmarkHlpOwnedCheckpoint,
    prepared_market: Box<Market>,
    prepared_swap: PreparedLeverageSwap,
    request: BenchmarkPrepareLeverageCloseRequest,
    quote: BenchmarkLeverageCloseQuote,
}

impl BenchmarkPreparedLeverageClose {
    pub const fn quote(&self) -> BenchmarkLeverageCloseQuote {
        self.quote
    }

    pub fn try_fork(&self) -> Result<Self> {
        Ok(Self {
            clock_before: self.clock_before,
            market_before: self.market_before.clone(),
            leverage_before: self.leverage_before,
            base_hlp_before: self.base_hlp_before.clone(),
            quote_hlp_before: self.quote_hlp_before.clone(),
            prepared_market: Box::new(clone_market(&self.prepared_market)?),
            prepared_swap: self.prepared_swap.clone(),
            request: self.request,
            quote: self.quote,
        })
    }
}

impl BenchmarkHlpOwnedState {
    /// Creates the same zero-holder YieldAccounts as the native initializer,
    /// checkpointed at the market's current hLP growth indexes.
    pub fn initialize(
        market: &BenchmarkMarket,
        target_asset: MarketAsset,
        owner: Pubkey,
        recipient: Pubkey,
    ) -> Result<Self> {
        require_keys_neq!(owner, Pubkey::default(), ErrorCode::InvalidArgument);
        let market_key = market.require_market_key()?;
        let hlp_mint = market.market.side(target_asset).hlp_mint;
        let (base_yield_account_key, base_bump) =
            derive_hlp_yield_account(market_key, owner, hlp_mint, market.market.base_side.asset_mint)?;
        let (quote_yield_account_key, quote_bump) =
            derive_hlp_yield_account(market_key, owner, hlp_mint, market.market.quote_side.asset_mint)?;
        let mut base_yield_account = Box::new(empty_yield_account());
        base_yield_account.initialize(
            owner,
            market_key,
            hlp_mint,
            market.market.base_side.asset_mint,
            YieldTokenKind::Hlp,
            recipient,
            base_bump,
        );
        let mut quote_yield_account = Box::new(empty_yield_account());
        quote_yield_account.initialize(
            owner,
            market_key,
            hlp_mint,
            market.market.quote_side.asset_mint,
            YieldTokenKind::Hlp,
            recipient,
            quote_bump,
        );
        let (base_swap_growth, base_interest_growth) =
            market.market.hlp_yield_growth_indexes(target_asset, MarketAsset::Base);
        let (quote_swap_growth, quote_interest_growth) =
            market.market.hlp_yield_growth_indexes(target_asset, MarketAsset::Quote);
        base_yield_account.accrue(0, base_swap_growth, base_interest_growth)?;
        quote_yield_account.accrue(0, quote_swap_growth, quote_interest_growth)?;
        let vault = hlp_vault(&market.market, target_asset);
        let state = Self {
            target_asset,
            owner,
            hlp_mint,
            hlp_mint_supply: vault.hlp_supply,
            holder_hlp_token_balance: 0,
            hlp_vault_ylp_balance: vault.ylp_shares,
            measured_base_interest_vault_credits: 0,
            measured_quote_interest_vault_credits: 0,
            base_yield_account_key,
            quote_yield_account_key,
            base_yield_account,
            quote_yield_account,
        };
        state.validate(market)?;
        Ok(state)
    }

    pub const fn target_asset(&self) -> MarketAsset {
        self.target_asset
    }

    pub const fn holder_hlp_token_balance(&self) -> u64 {
        self.holder_hlp_token_balance
    }

    pub const fn hlp_mint_supply(&self) -> u64 {
        self.hlp_mint_supply
    }

    pub const fn hlp_vault_ylp_balance(&self) -> u64 {
        self.hlp_vault_ylp_balance
    }

    pub fn checkpoint(&self) -> BenchmarkHlpOwnedCheckpoint {
        BenchmarkHlpOwnedCheckpoint {
            target_asset: self.target_asset,
            owner: self.owner,
            hlp_mint: self.hlp_mint,
            hlp_mint_supply: self.hlp_mint_supply,
            holder_hlp_token_balance: self.holder_hlp_token_balance,
            hlp_vault_ylp_balance: self.hlp_vault_ylp_balance,
            measured_base_interest_vault_credits: self.measured_base_interest_vault_credits,
            measured_quote_interest_vault_credits: self.measured_quote_interest_vault_credits,
            base_yield_account: yield_account_checkpoint(self.base_yield_account_key, &self.base_yield_account),
            quote_yield_account: yield_account_checkpoint(self.quote_yield_account_key, &self.quote_yield_account),
        }
    }

    pub fn try_fork(&self) -> Result<Self> {
        Ok(Self {
            target_asset: self.target_asset,
            owner: self.owner,
            hlp_mint: self.hlp_mint,
            hlp_mint_supply: self.hlp_mint_supply,
            holder_hlp_token_balance: self.holder_hlp_token_balance,
            hlp_vault_ylp_balance: self.hlp_vault_ylp_balance,
            measured_base_interest_vault_credits: self.measured_base_interest_vault_credits,
            measured_quote_interest_vault_credits: self.measured_quote_interest_vault_credits,
            base_yield_account_key: self.base_yield_account_key,
            quote_yield_account_key: self.quote_yield_account_key,
            base_yield_account: Box::new(clone_yield_account(&self.base_yield_account)?),
            quote_yield_account: Box::new(clone_yield_account(&self.quote_yield_account)?),
        })
    }

    fn validate(&self, market: &BenchmarkMarket) -> Result<()> {
        validate_hlp_owned_state(self, &market.market, market.require_market_key()?)
    }
}

impl BenchmarkReferralOwnedState {
    pub fn initialize(
        market: &BenchmarkMarket,
        debt_asset: MarketAsset,
        authority: Pubkey,
        recipient: Pubkey,
        interest_share_bps: u16,
        active: bool,
    ) -> Result<Self> {
        require_keys_neq!(authority, Pubkey::default(), ErrorCode::InvalidReferralPartner);
        require_keys_neq!(recipient, Pubkey::default(), ErrorCode::InvalidRecipient);
        require_gte!(
            MAX_REFERRAL_INTEREST_SHARE_BPS,
            interest_share_bps,
            ErrorCode::InvalidReferralInterestShareBps
        );
        let market_key = market.require_market_key()?;
        let (partner_key, partner_bump) =
            Pubkey::try_find_program_address(&[REFERRAL_PARTNER_SEED_PREFIX, authority.as_ref()], &crate::ID)
                .ok_or(ErrorCode::InvalidReferralPartner)?;
        let asset_mint = market.market.side(debt_asset).asset_mint;
        let (accrual_key, accrual_bump) = Pubkey::try_find_program_address(
            &[
                REFERRAL_ACCRUAL_SEED_PREFIX,
                partner_key.as_ref(),
                market_key.as_ref(),
                asset_mint.as_ref(),
            ],
            &crate::ID,
        )
        .ok_or(ErrorCode::InvalidReferralAccrual)?;
        let mut partner = Box::new(ReferralPartner {
            authority: Pubkey::default(),
            recipient: Pubkey::default(),
            interest_share_bps: 0,
            active: false,
            bump: 0,
        });
        partner.initialize(authority, interest_share_bps, active, partner_bump)?;
        partner.set_recipient(authority, recipient)?;
        let mut accrual = Box::new(ReferralAccrual {
            referral_partner: Pubkey::default(),
            market: Pubkey::default(),
            asset_mint: Pubkey::default(),
            amount: 0,
            bump: 0,
        });
        accrual.initialize(partner_key, market_key, asset_mint, accrual_bump)?;
        let state = Self {
            partner_key,
            partner,
            accrual_key,
            accrual,
        };
        validate_referral_owned_state(&state, market_key, asset_mint)?;
        Ok(state)
    }

    pub fn checkpoint(&self) -> BenchmarkReferralCheckpoint {
        BenchmarkReferralCheckpoint {
            partner_key: self.partner_key,
            authority: self.partner.authority,
            recipient: self.partner.recipient,
            configured_interest_share_bps: self.partner.interest_share_bps,
            active: self.partner.active,
            partner_bump: self.partner.bump,
            accrual_key: self.accrual_key,
            market: self.accrual.market,
            asset_mint: self.accrual.asset_mint,
            accrued_amount: self.accrual.amount,
            accrual_bump: self.accrual.bump,
        }
    }

    pub fn try_fork(&self) -> Result<Self> {
        let mut partner_bytes = Vec::new();
        self.partner.try_serialize(&mut partner_bytes)?;
        let mut partner_input = partner_bytes.as_slice();
        let partner = ReferralPartner::try_deserialize(&mut partner_input)?;
        let mut accrual_bytes = Vec::new();
        self.accrual.try_serialize(&mut accrual_bytes)?;
        let mut accrual_input = accrual_bytes.as_slice();
        let accrual = ReferralAccrual::try_deserialize(&mut accrual_input)?;
        Ok(Self {
            partner_key: self.partner_key,
            partner: Box::new(partner),
            accrual_key: self.accrual_key,
            accrual: Box::new(accrual),
        })
    }
}

impl BenchmarkLeverageOwnedState {
    pub fn initialize_for_open(
        market: &BenchmarkMarket,
        funding_owner: Pubkey,
        position_owner: Pubkey,
        position_id: Pubkey,
        token_balances: BenchmarkLeverageTokenBalances,
        referral: Option<BenchmarkReferralOwnedState>,
    ) -> Result<Self> {
        require_keys_neq!(funding_owner, Pubkey::default(), ErrorCode::InvalidSigner);
        require_keys_neq!(position_owner, Pubkey::default(), ErrorCode::InvalidSigner);
        let market_key = market.require_market_key()?;
        let (position_key, _) = leverage_position_pda(market_key, position_id)?;
        let state = Self {
            funding_owner,
            position_owner,
            position_id,
            position_key,
            position_exists: false,
            position: Box::new(empty_leverage_position()),
            token_balances,
            referral,
        };
        validate_leverage_token_custody(&market.market, &state.token_balances)?;
        Ok(state)
    }

    pub fn checkpoint(&self) -> BenchmarkLeverageOwnedCheckpoint {
        BenchmarkLeverageOwnedCheckpoint {
            funding_owner: self.funding_owner,
            position_owner: self.position_owner,
            position_id: self.position_id,
            position: BenchmarkLeveragePositionCheckpoint {
                account_key: self.position_key,
                account_exists: self.position_exists,
                owner: self.position.owner,
                market: self.position.market,
                position_id: self.position.position_id,
                referral_partner: self.position.referral_partner,
                referral_interest_share_bps: self.position.referral_interest_share_bps,
                debt_asset: self.position.debt_asset,
                collateral_amount: self.position.collateral_amount,
                margin_amount: self.position.margin_amount,
                open_notional: self.position.open_notional,
                debt_principal: self.position.debt_principal,
                debt_shares: self.position.debt_shares,
                multiplier_bps: self.position.multiplier_bps,
                opened_at: self.position.opened_at,
                opened_slot: self.position.opened_slot,
                bump: self.position.bump,
            },
            token_balances: self.token_balances,
            referral: self.referral.as_ref().map(BenchmarkReferralOwnedState::checkpoint),
        }
    }

    pub fn position(&self) -> Option<&LeveragePosition> {
        self.position_exists.then_some(&self.position)
    }

    pub fn token_balances(&self) -> BenchmarkLeverageTokenBalances {
        self.token_balances
    }

    pub fn try_fork(&self) -> Result<Self> {
        Ok(Self {
            funding_owner: self.funding_owner,
            position_owner: self.position_owner,
            position_id: self.position_id,
            position_key: self.position_key,
            position_exists: self.position_exists,
            position: Box::new(clone_leverage_position(&self.position)?),
            token_balances: self.token_balances,
            referral: self
                .referral
                .as_ref()
                .map(BenchmarkReferralOwnedState::try_fork)
                .transpose()?,
        })
    }
}

impl BenchmarkBorrowPosition {
    pub fn initialize(owner: Pubkey, market: Pubkey, position_id: Pubkey, bump: u8) -> Self {
        let mut position = Box::new(BorrowPosition {
            owner: Pubkey::default(),
            market: Pubkey::default(),
            position_id: Pubkey::default(),
            base_collateral: 0,
            quote_collateral: 0,
            global_health_base_contribution_for_quote_debt: 0,
            global_health_quote_contribution_for_base_debt: 0,
            base_liquidation_cf_bps: 0,
            quote_liquidation_cf_bps: 0,
            base_referral_partner: Pubkey::default(),
            quote_referral_partner: Pubkey::default(),
            base_referral_interest_share_bps: 0,
            quote_referral_interest_share_bps: 0,
            fixed_base_shares: 0,
            fixed_quote_shares: 0,
            auction_debt_asset: u8::MAX,
            auction_start_time: 0,
            auction_start_price_nad: 0,
            auction_floor_price_nad: 0,
            bump: 0,
        });
        position.initialize(owner, market, position_id, bump);
        Self { position }
    }

    pub fn from_position_state(position: BorrowPosition) -> Result<Self> {
        require!(position.is_initialized(), ErrorCode::InvalidPositionMarket);
        Ok(Self {
            position: Box::new(position),
        })
    }

    pub fn position(&self) -> &BorrowPosition {
        &self.position
    }

    pub fn into_position(self) -> BorrowPosition {
        *self.position
    }

    pub fn try_fork(&self) -> Result<Self> {
        Ok(Self {
            position: Box::new(clone_borrow_position(&self.position)?),
        })
    }

    pub fn checkpoint(&self, market: &BenchmarkMarket) -> Result<BenchmarkBorrowPositionCheckpoint> {
        let market_key = market.require_market_key()?;
        self.position.assert_position(self.position.owner, market_key)?;
        borrow_position_checkpoint(&self.position, &market.market.debt)
    }

    pub fn first_liquidation_floor_unix_timestamp(&self) -> Result<i64> {
        self.position
            .active_liquidation_auction_asset()?
            .ok_or(ErrorCode::PositionNotLiquidatable)?;
        first_liquidation_floor_timestamp(
            self.position.auction_start_time,
            self.position.auction_start_price_nad,
            self.position.auction_floor_price_nad,
        )
    }
}

impl BenchmarkMarket {
    /// Constructs an empty Dusk market through the production initializer.
    pub fn initialize(init: BenchmarkMarketInit, clock: BenchmarkClock) -> Result<Self> {
        Self::initialize_inner(None, init, clock)
    }

    /// Constructs a market and binds the account key needed to validate owned
    /// position accounts. Position transitions fail closed on an unbound
    /// market; market-only quote and swap use remains backward-compatible.
    pub fn initialize_at(market_key: Pubkey, init: BenchmarkMarketInit, clock: BenchmarkClock) -> Result<Self> {
        Self::initialize_inner(Some(market_key), init, clock)
    }

    fn initialize_inner(market_key: Option<Pubkey>, init: BenchmarkMarketInit, clock: BenchmarkClock) -> Result<Self> {
        let mut market = Box::<Market>::default();
        market.initialize(
            init.ylp_mint,
            init.base_side,
            init.quote_side,
            init.config,
            init.base_hlp_ylp_vault,
            init.quote_hlp_ylp_vault,
            init.base_insurance_vault,
            init.quote_insurance_vault,
            init.params_hash,
            init.initial_liquidity_authority,
            init.bootstrap_price_nad,
            init.launch_fee_progress_offset,
            clock.slot,
            init.bump,
        )?;
        Ok(Self {
            market,
            market_key,
            clock,
        })
    }

    /// Wraps a decoded market checkpoint without changing it.
    pub fn from_market_state(market: Market, clock: BenchmarkClock) -> Result<Self> {
        Self::from_market_state_inner(None, market, clock)
    }

    pub fn from_market_state_at(market_key: Pubkey, market: Market, clock: BenchmarkClock) -> Result<Self> {
        Self::from_market_state_inner(Some(market_key), market, clock)
    }

    fn from_market_state_inner(market_key: Option<Pubkey>, market: Market, clock: BenchmarkClock) -> Result<Self> {
        market.assert_current_version()?;
        market.config.validate()?;
        let minimum_slot = market
            .last_update_slot
            .max(market.risk.last_snapshot_slot)
            .max(market.amm.last_observation_slot)
            .max(market.debt.base_last_accrual_slot)
            .max(market.debt.quote_last_accrual_slot);
        require_gte!(clock.slot, minimum_slot, ErrorCode::InvalidArgument);
        Ok(Self {
            market: Box::new(market),
            market_key,
            clock,
        })
    }

    pub const fn clock(&self) -> BenchmarkClock {
        self.clock
    }

    pub fn market(&self) -> &Market {
        &self.market
    }

    pub const fn market_key(&self) -> Option<Pubkey> {
        self.market_key
    }

    /// Direct mutable access for public native transitions not otherwise
    /// wrapped here. Use [`Self::transact`] when a failed transition must leave
    /// the market checkpoint unchanged.
    pub fn market_mut(&mut self) -> &mut Market {
        &mut self.market
    }

    pub fn into_market(self) -> Market {
        *self.market
    }

    /// Creates an independent branch through canonical account serialization.
    pub fn try_fork(&self) -> Result<Self> {
        Ok(Self {
            market: Box::new(clone_market(&self.market)?),
            market_key: self.market_key,
            clock: self.clock,
        })
    }

    /// Applies an arbitrary public native transition atomically to the market.
    ///
    /// Caller-owned position objects are outside this rollback boundary and
    /// should be staged alongside the market when an operation may fail.
    pub fn transact<T>(&mut self, operation: impl FnOnce(&mut Market, BenchmarkClock) -> Result<T>) -> Result<T> {
        let mut next = clone_market(&self.market)?;
        let output = operation(&mut next, self.clock)?;
        *self.market = next;
        Ok(output)
    }

    /// Advances interest, AMM clocks, hLP accounting, and risk to an explicit
    /// slot using the same ordering as [`Market::update`].
    pub fn advance_to(&mut self, clock: BenchmarkClock) -> Result<()> {
        require_gte!(clock.slot, self.clock.slot, ErrorCode::InvalidArgument);
        let mut next = clone_market(&self.market)?;
        advance_market_to_slot(&mut next, clock.slot)?;
        *self.market = next;
        self.clock = clock;
        Ok(())
    }

    /// Runs the complete native preview/finalization path on a fork and leaves
    /// this checkpoint byte-for-byte unchanged.
    pub fn preview_swap(&self, request: BenchmarkSwapRequest) -> Result<BenchmarkSwapExecution> {
        let mut preview = clone_market(&self.market)?;
        execute_swap(&mut preview, self.clock, request)
    }

    /// Atomically commits the same state-only path used by the swap instruction.
    pub fn execute_swap(&mut self, request: BenchmarkSwapRequest) -> Result<BenchmarkSwapExecution> {
        let mut next = clone_market(&self.market)?;
        let execution = execute_swap(&mut next, self.clock, request)?;
        *self.market = next;
        Ok(execution)
    }

    /// Executes a swap and both native hLP token settlements in one rollback
    /// boundary. Use this path whenever hLP state is active; it keeps the two
    /// yLP custody accounts and measured funding-interest credits synchronized
    /// with the market-side rebalance receipts.
    pub fn execute_swap_with_hlp(
        &mut self,
        base_external: &mut BenchmarkHlpOwnedState,
        quote_external: &mut BenchmarkHlpOwnedState,
        request: BenchmarkHlpAwareSwapRequest,
    ) -> Result<BenchmarkHlpAwareSwapExecution> {
        require!(base_external.target_asset == MarketAsset::Base, ErrorCode::InvalidMint);
        require!(
            quote_external.target_asset == MarketAsset::Quote,
            ErrorCode::InvalidMint
        );
        base_external.validate(self)?;
        quote_external.validate(self)?;
        require!(
            request.protocol_auction_split.is_valid(),
            ErrorCode::InvalidAuctionConfig
        );

        let revenue_before = self.revenue_checkpoint();
        let market_key = self.require_market_key()?;
        let mut next_market = clone_market(&self.market)?;
        let mut next_base = base_external.try_fork()?;
        let mut next_quote = quote_external.try_fork()?;
        let swap = execute_swap(&mut next_market, self.clock, request.swap)?;
        let mut hlp_interest_cash = BenchmarkCashFlow::default();
        apply_hlp_rebalance_settlement(
            &mut next_market,
            &mut next_base,
            swap.base_rebalance,
            swap.interest_eligibility,
            request.base_hlp_interest_transfer,
            request.protocol_interest_fee_bps,
            request.protocol_auction_split,
            &mut hlp_interest_cash,
        )?;
        apply_hlp_rebalance_settlement(
            &mut next_market,
            &mut next_quote,
            swap.quote_rebalance,
            swap.interest_eligibility,
            request.quote_hlp_interest_transfer,
            request.protocol_interest_fee_bps,
            request.protocol_auction_split,
            &mut hlp_interest_cash,
        )?;
        validate_hlp_owned_state(&next_base, &next_market, market_key)?;
        validate_hlp_owned_state(&next_quote, &next_market, market_key)?;

        let revenue_after = revenue_checkpoint(&next_market);
        let market_after = market_checkpoint(&next_market, self.market_key, self.clock)?;
        let base_external_after = next_base.checkpoint();
        let quote_external_after = next_quote.checkpoint();
        *self.market = next_market;
        *base_external = next_base;
        *quote_external = next_quote;
        Ok(BenchmarkHlpAwareSwapExecution {
            swap,
            hlp_interest_cash,
            revenue_before,
            revenue_after,
            market_after,
            base_external_after,
            quote_external_after,
        })
    }

    /// Reproduces the native new-position capacity preview at the stored slot.
    ///
    /// The returned health, cash, and daily-limit bounds are kept separate so
    /// the benchmark can attribute rejected demand without recomputing policy.
    pub fn preview_borrow_capacity(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        projected_borrow_amount: Option<u64>,
    ) -> Result<BorrowCapacityPreview> {
        let mut preview = clone_market(&self.market)?;
        advance_market_to_slot(&mut preview, self.clock.slot)?;
        require!(collateral_amount > 0, ErrorCode::AmountZero);
        let debt_asset = collateral_asset.opposite();
        let collateral_side = preview.side(collateral_asset);
        let debt_side = preview.side(debt_asset);
        let risk = preview.risk;
        let collateral_value_nad = preview.collateral_value_nad(collateral_asset, collateral_amount, &risk)?;
        let max_debt_by_cash = debt_side.reserves.cash_reserve;
        let daily_limit = preview.daily_limit_for_side(debt_asset, preview.config.max_daily_borrow_bps)?;
        let max_debt_by_daily_limit = debt_side.daily_borrow_bucket.remaining(daily_limit, self.clock.slot)?;
        let preview_context = NewPositionCapacityContext {
            market: &preview,
            debt_asset,
            collateral_amount,
            risk: &risk,
            existing_total_debt_nad: preview.total_fixed_debt_nad(debt_asset)?,
            current_aggregate_contribution: match debt_asset {
                MarketAsset::Base => preview.debt.global_health_quote_contribution_for_base_debt,
                MarketAsset::Quote => preview.debt.global_health_base_contribution_for_quote_debt,
            },
        };
        let max_debt_by_health = {
            let current_health = preview.market_health_from_risk(&risk)?;
            if preview.assert_market_health_snapshot(&current_health).is_err() {
                0
            } else {
                let mut low = 0_u64;
                let mut high = debt_side.reserves.live_reserve;
                while low < high {
                    let midpoint = low + (high - low) / 2 + 1;
                    let (terms, _) = preview_context.terms(midpoint)?;
                    let accepted = terms.max_debt >= midpoint
                        && terms.projected_market_health_bps >= preview.config.borrow_market_health_floor_bps as u64;
                    if accepted {
                        low = midpoint;
                    } else {
                        high = midpoint - 1;
                    }
                }
                low
            }
        };
        let max_debt = max_debt_by_health.min(max_debt_by_cash).min(max_debt_by_daily_limit);
        let projected_borrow_amount = projected_borrow_amount.unwrap_or(max_debt);
        let (projected_terms, projected_global_health_contribution) = preview_context.terms(projected_borrow_amount)?;
        let projected_debt_nad = normalize_to_nad(projected_borrow_amount as u128, debt_side.asset_decimals)?;
        let projected_health_bps = if projected_debt_nad == 0 {
            u64::MAX
        } else {
            health_bps(collateral_value_nad, projected_debt_nad)?
        };
        let liquidation_debt_per_collateral_price_nad =
            if projected_borrow_amount == 0 || projected_terms.liquidation_cf_bps == 0 {
                0
            } else {
                let collateral_nad = normalize_to_nad(collateral_amount as u128, collateral_side.asset_decimals)?;
                let debt_nad = normalize_to_nad(projected_borrow_amount as u128, debt_side.asset_decimals)?;
                let price = ceil_div(
                    debt_nad
                        .checked_mul(BPS_DENOMINATOR as u128)
                        .and_then(|value| value.checked_mul(NAD as u128))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    collateral_nad
                        .checked_mul(projected_terms.liquidation_cf_bps as u128)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                )
                .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow)?
            };

        Ok(BorrowCapacityPreview {
            collateral_asset,
            debt_asset,
            collateral_amount,
            collateral_value_nad,
            max_debt_by_health,
            max_debt_by_cash,
            max_debt_by_daily_limit,
            max_debt,
            max_borrow_amount: max_debt,
            borrow_market_health_floor_bps: preview.config.borrow_market_health_floor_bps,
            global_health_contribution_cap_bps: preview.config.global_health_contribution_cap_bps,
            projected_borrow_amount,
            projected_debt_amount: projected_borrow_amount,
            projected_health_bps,
            projected_global_market_health_bps: projected_terms.projected_market_health_bps,
            projected_global_health_contribution,
            projected_effective_existing_debt_nad: projected_terms.effective_existing_debt_nad,
            max_cf_bps: projected_terms.max_cf_bps,
            liquidation_cf_bps: projected_terms.liquidation_cf_bps,
            liquidation_debt_per_collateral_price_nad,
        })
    }

    /// Reproduces every native capacity predicate for an already-owned borrow
    /// position without mutating the market, position, or stored clock.
    pub fn preview_existing_borrow_capacity(
        &self,
        position: &BenchmarkBorrowPosition,
        debt_asset: MarketAsset,
        clock: BenchmarkClock,
        global_reduce_only: bool,
    ) -> Result<BenchmarkExistingBorrowCapacity> {
        self.require_monotonic_clock(clock)?;
        self.assert_position_market(position)?;
        let mut preview = clone_market(&self.market)?;
        advance_market_to_slot(&mut preview, clock.slot)?;
        preview.assert_started_at(clock.unix_timestamp)?;
        existing_borrow_capacity_preview(&preview, &position.position, debt_asset, clock.slot, global_reduce_only)
    }

    /// Solves the largest raw request whose share-rounded post-debt is no
    /// greater than `target_debt`. Execution limits are deliberately not
    /// applied; callers cap this intent by `actionable_max_additional`.
    pub fn preview_existing_borrow_request_for_target(
        &self,
        position: &BenchmarkBorrowPosition,
        debt_asset: MarketAsset,
        clock: BenchmarkClock,
        global_reduce_only: bool,
        target_debt: u128,
    ) -> Result<BenchmarkBorrowTargetRequest> {
        self.require_monotonic_clock(clock)?;
        self.assert_position_market(position)?;
        let mut preview = clone_market(&self.market)?;
        advance_market_to_slot(&mut preview, clock.slot)?;
        preview.assert_started_at(clock.unix_timestamp)?;
        let capacity =
            existing_borrow_capacity_preview(&preview, &position.position, debt_asset, clock.slot, global_reduce_only)?;
        require_gte!(
            capacity.gross_admissible_max_debt,
            target_debt,
            ErrorCode::InvalidArgument
        );
        let risk = preview.risk;
        let context = ExistingPositionCapacityContext {
            market: &preview,
            position: &position.position,
            debt_asset,
            risk: &risk,
            external_debt_nad: preview.external_fixed_debt_nad(&position.position, debt_asset)?,
        };
        let borrow_request = if target_debt <= capacity.debt_before {
            0
        } else {
            maximum_monotone_capacity(capacity.underwriting_max_additional, |amount| {
                Ok(context.project(amount)?.projected_position_debt <= target_debt)
            })?
        };
        let projected_debt = context.project(borrow_request)?.projected_position_debt;
        Ok(BenchmarkBorrowTargetRequest {
            target_debt,
            borrow_request,
            projected_debt,
            target_debt_gap: target_debt.saturating_sub(projected_debt),
        })
    }

    pub fn curve_snapshot(&self) -> Result<BenchmarkCurveSnapshot> {
        let spot_price_nad = self
            .market
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        let cache = self.market.amm.concentrated_curve_cache;
        Ok(BenchmarkCurveSnapshot {
            base_curve_reserve: self.market.curve_reserve(MarketAsset::Base)?,
            quote_curve_reserve: self.market.curve_reserve(MarketAsset::Quote)?,
            spot_price_nad,
            center_price_nad: self.market.amm.center_price_nad,
            curve_depth_nad: cache
                .tail_liquidity
                .checked_add(cache.concentrated_liquidity)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            volatility_accumulator_nad: self.market.amm.volatility_accumulator_nad,
            last_observation_slot: self.market.amm.last_observation_slot,
        })
    }

    pub fn checkpoint(&self) -> Result<BenchmarkMarketCheckpoint> {
        market_checkpoint(&self.market, self.market_key, self.clock)
    }

    /// Returns the exact unpaid public-interest coordinate without advancing
    /// the stored market clock or mutating any account state.
    pub fn public_interest_checkpoint(&self) -> Result<BenchmarkPublicInterestCheckpoint> {
        let side = |asset: MarketAsset| -> Result<u128> {
            let (fixed_debt, fixed_principal, isolated_debt, isolated_principal) = match asset {
                MarketAsset::Base => (
                    self.market.debt.fixed_base_debt()?,
                    u128::from(self.market.debt.fixed_base_principal),
                    self.market.debt.isolated_debt(MarketAsset::Base)?,
                    u128::from(self.market.debt.isolated_base_principal),
                ),
                MarketAsset::Quote => (
                    self.market.debt.fixed_quote_debt()?,
                    u128::from(self.market.debt.fixed_quote_principal),
                    self.market.debt.isolated_debt(MarketAsset::Quote)?,
                    u128::from(self.market.debt.isolated_quote_principal),
                ),
            };
            fixed_debt
                .checked_sub(fixed_principal.min(fixed_debt))
                .ok_or(ErrorCode::DebtMathOverflow)?
                .checked_add(
                    isolated_debt
                        .checked_sub(isolated_principal.min(isolated_debt))
                        .ok_or(ErrorCode::DebtMathOverflow)?,
                )
                .ok_or_else(|| ErrorCode::DebtMathOverflow.into())
        };
        Ok(BenchmarkPublicInterestCheckpoint {
            base: side(MarketAsset::Base)?,
            quote: side(MarketAsset::Quote)?,
        })
    }

    pub fn revenue_checkpoint(&self) -> BenchmarkRevenueCheckpoint {
        revenue_checkpoint(&self.market)
    }

    pub const fn leverage_external_requirements() -> BenchmarkDeferredTransitionRequirements {
        BenchmarkDeferredTransitionRequirements {
            requirements: LEVERAGE_EXTERNAL_REQUIREMENTS,
        }
    }

    pub const fn hlp_external_requirements() -> BenchmarkDeferredTransitionRequirements {
        BenchmarkDeferredTransitionRequirements {
            requirements: HLP_EXTERNAL_REQUIREMENTS,
        }
    }

    /// Freezes the native open quote and every mutable account checkpoint.
    /// No caller state is changed until the returned object is consumed by
    /// [`Self::execute_prepared_leverage_open`].
    pub fn prepare_leverage_open(
        &self,
        leverage: &BenchmarkLeverageOwnedState,
        base_hlp: &BenchmarkHlpOwnedState,
        quote_hlp: &BenchmarkHlpOwnedState,
        request: BenchmarkPrepareLeverageOpenRequest,
    ) -> Result<BenchmarkPreparedLeverageOpen> {
        self.require_monotonic_clock(request.clock)?;
        validate_leverage_policy(&self.market, request.debt_asset, request.clock, request.policy, true)?;
        require!(!leverage.position_exists, ErrorCode::InvalidLeveragePosition);
        require!(!leverage.position.is_initialized(), ErrorCode::InvalidLeveragePosition);
        let market_key = self.require_market_key()?;
        let (position_key, _) = leverage_position_pda(market_key, leverage.position_id)?;
        require_keys_eq!(leverage.position_key, position_key, ErrorCode::InvalidLeveragePosition);
        validate_leverage_token_custody(&self.market, &leverage.token_balances)?;
        if let Some(referral) = leverage.referral.as_ref() {
            validate_referral_owned_state(referral, market_key, self.market.side(request.debt_asset).asset_mint)?;
        }
        validate_leverage_hlp_pair(self, base_hlp, quote_hlp)?;
        require!(
            request.margin_transfer.source_debit > 0 && request.margin_transfer.destination_credit > 0,
            ErrorCode::AmountZero
        );
        require_gte!(
            leverage_token_balance(&leverage.token_balances, request.debt_asset),
            request.margin_transfer.source_debit,
            ErrorCode::InsufficientBalance
        );
        require_gte!(
            MAX_REFERRAL_INTEREST_SHARE_BPS,
            request.policy.max_referral_interest_share_bps,
            ErrorCode::InvalidReferralInterestShareBps
        );
        let (referral_partner, referral_interest_share_bps) =
            match (request.requested_referrer, leverage.referral.as_ref()) {
                (None, None) => (Pubkey::default(), 0),
                (Some(referrer), Some(referral)) => {
                    validate_referral_owned_state(
                        referral,
                        market_key,
                        self.market.side(request.debt_asset).asset_mint,
                    )?;
                    require_keys_eq!(referral.partner.authority, referrer, ErrorCode::InvalidReferralPartner);
                    (
                        referral.partner_key,
                        referral
                            .partner
                            .binding_interest_share_bps(request.policy.max_referral_interest_share_bps)?,
                    )
                }
                (None, Some(_)) | (Some(_), None) => return err!(ErrorCode::InvalidReferralPartner),
            };
        let borrowed_amount =
            leverage_debt_from_margin(request.margin_transfer.destination_credit, request.multiplier_bps)?;
        let notional = request
            .margin_transfer
            .destination_credit
            .checked_add(borrowed_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let mut prepared_market = clone_market(&self.market)?;
        let prepared_swap = prepare_benchmark_leverage_swap(
            &mut prepared_market,
            SwapRequest {
                current_slot: request.clock.slot,
                current_unix_timestamp: request.clock.unix_timestamp,
                asset_in: request.debt_asset,
                reserve_credit: notional,
                protocol_fee_bps: request.policy.protocol_swap_fee_bps,
            },
            SwapCashPolicy::Borrow {
                asset: request.debt_asset,
                amount: borrowed_amount,
            },
        )?;
        // Derive settlement debits by running the same native transition on a
        // disposable fork. The prepared state itself remains untouched and
        // execution will independently reproduce these receipts.
        let mut settlement_market = clone_market(&prepared_market)?;
        let mut settlement_position = empty_leverage_position();
        let (_, position_bump) = leverage_position_pda(self.require_market_key()?, leverage.position_id)?;
        let settlement_preview = settlement_market.open_leverage(
            &mut settlement_position,
            leverage.position_owner,
            self.require_market_key()?,
            leverage.position_id,
            referral_partner,
            referral_interest_share_bps,
            request.debt_asset,
            request.margin_transfer.destination_credit,
            request.multiplier_bps,
            prepared_swap.swap.amount_out,
            prepared_swap.clone(),
            full_leverage_swap_fee_credit(prepared_swap.swap)?,
            request.clock.unix_timestamp,
            request.clock.slot,
            position_bump,
            request.policy.protocol_swap_fee_bps,
            request.policy.protocol_auction_split,
        )?;
        let quote = BenchmarkLeverageOpenQuote {
            borrowed_amount,
            notional,
            swap: prepared_swap.swap,
            referral_partner,
            referral_interest_share_bps,
            settlement: BenchmarkLeverageSettlementRequirements {
                base_hlp_interest_debit: settlement_preview.base_hlp_rebalance.interest_paid,
                quote_hlp_interest_debit: settlement_preview.quote_hlp_rebalance.interest_paid,
                position_interest_debit: 0,
            },
        };
        Ok(BenchmarkPreparedLeverageOpen {
            clock_before: self.clock,
            market_before: serialize_market(&self.market)?,
            leverage_before: leverage.checkpoint(),
            base_hlp_before: base_hlp.checkpoint(),
            quote_hlp_before: quote_hlp.checkpoint(),
            prepared_market: Box::new(prepared_market),
            prepared_swap,
            request,
            quote,
        })
    }

    /// Consumes an identity-bound prepared quote and atomically commits the
    /// native market, position, reserve, collateral, interest, hLP, and
    /// referral state transitions.
    pub fn execute_prepared_leverage_open(
        &mut self,
        leverage: &mut BenchmarkLeverageOwnedState,
        base_hlp: &mut BenchmarkHlpOwnedState,
        quote_hlp: &mut BenchmarkHlpOwnedState,
        prepared: BenchmarkPreparedLeverageOpen,
        execution: BenchmarkExecuteLeverageOpenRequest,
    ) -> Result<BenchmarkLeverageExecution<BenchmarkLeverageOpenExecutionReceipt>> {
        require_prepared_identity(
            self,
            leverage,
            base_hlp,
            quote_hlp,
            prepared.clock_before,
            &prepared.market_before,
            prepared.leverage_before,
            prepared.base_hlp_before,
            prepared.quote_hlp_before,
        )?;
        require_eq!(
            execution.collateral_transfer.source_debit,
            prepared.quote.swap.amount_out,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            execution.collateral_transfer.destination_credit,
            execution.collateral_transfer.source_debit,
            ErrorCode::InvalidLeverageCollateralMint
        );
        require_gte!(
            execution.collateral_transfer.destination_credit,
            prepared.request.min_collateral_out,
            ErrorCode::SlippageExceeded
        );
        if prepared.request.limit_price_nad != 0 {
            let execution_price_nad = leverage_entry_price_nad(
                &prepared.prepared_market,
                prepared.request.debt_asset,
                prepared.quote.notional,
                execution.collateral_transfer.destination_credit,
            )?;
            require!(
                leverage_entry_limit_satisfied(
                    prepared.request.debt_asset,
                    execution_price_nad,
                    prepared.request.limit_price_nad,
                ),
                ErrorCode::SlippageExceeded
            );
        }

        let revenue_before = self.revenue_checkpoint();
        let mut next_market = *prepared.prepared_market;
        let mut next_leverage = leverage.try_fork()?;
        let mut next_base_hlp = base_hlp.try_fork()?;
        let mut next_quote_hlp = quote_hlp.try_fork()?;
        let debt_asset = prepared.request.debt_asset;
        let margin = prepared.request.margin_transfer;
        let collateral = execution.collateral_transfer;
        *leverage_token_balance_mut(&mut next_leverage.token_balances, debt_asset) =
            leverage_token_balance(&next_leverage.token_balances, debt_asset)
                .checked_sub(margin.source_debit)
                .ok_or(ErrorCode::InsufficientBalance)?;
        *leverage_reserve_vault_balance_mut(&mut next_leverage.token_balances, debt_asset) =
            leverage_reserve_vault_balance(&next_leverage.token_balances, debt_asset)
                .checked_add(margin.destination_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        let collateral_asset = debt_asset.opposite();
        *leverage_reserve_vault_balance_mut(&mut next_leverage.token_balances, collateral_asset) =
            leverage_reserve_vault_balance(&next_leverage.token_balances, collateral_asset)
                .checked_sub(collateral.source_debit)
                .ok_or(ErrorCode::InsufficientLiquidity)?;
        *leverage_collateral_vault_balance_mut(&mut next_leverage.token_balances, collateral_asset) =
            leverage_collateral_vault_balance(&next_leverage.token_balances, collateral_asset)
                .checked_add(collateral.destination_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        let fee_credit = full_leverage_swap_fee_credit(prepared.quote.swap)?;
        let interest_eligibility = prepared.prepared_swap.interest_eligibility;
        let (_, position_bump) = leverage_position_pda(self.require_market_key()?, next_leverage.position_id)?;
        let native = next_market.open_leverage(
            &mut next_leverage.position,
            next_leverage.position_owner,
            self.require_market_key()?,
            next_leverage.position_id,
            prepared.quote.referral_partner,
            prepared.quote.referral_interest_share_bps,
            prepared.request.debt_asset,
            prepared.request.margin_transfer.destination_credit,
            prepared.request.multiplier_bps,
            execution.collateral_transfer.destination_credit,
            prepared.prepared_swap,
            fee_credit,
            prepared.request.clock.unix_timestamp,
            prepared.request.clock.slot,
            position_bump,
            prepared.request.policy.protocol_swap_fee_bps,
            prepared.request.policy.protocol_auction_split,
        )?;
        require_eq!(
            native.base_hlp_rebalance.interest_paid,
            prepared.quote.settlement.base_hlp_interest_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            native.quote_hlp_rebalance.interest_paid,
            prepared.quote.settlement.quote_hlp_interest_debit,
            ErrorCode::BrokenInvariant
        );
        next_leverage.position_exists = true;
        let mut cash = BenchmarkCashFlow::default();
        cash.side_mut(prepared.request.debt_asset).reserve_vault_credit =
            prepared.request.margin_transfer.destination_credit;
        let collateral_cash = cash.side_mut(prepared.request.debt_asset.opposite());
        collateral_cash.reserve_vault_debit = execution.collateral_transfer.source_debit;
        collateral_cash.collateral_vault_credit = execution.collateral_transfer.destination_credit;
        apply_leverage_hlp_settlement(
            &mut next_market,
            &mut next_base_hlp,
            native.base_hlp_rebalance,
            interest_eligibility,
            execution.base_hlp_interest_transfer,
            prepared.request.policy,
            &mut next_leverage.token_balances,
            &mut cash,
        )?;
        apply_leverage_hlp_settlement(
            &mut next_market,
            &mut next_quote_hlp,
            native.quote_hlp_rebalance,
            interest_eligibility,
            execution.quote_hlp_interest_transfer,
            prepared.request.policy,
            &mut next_leverage.token_balances,
            &mut cash,
        )?;
        let metrics = leverage_metrics(&next_market, &next_leverage.position, prepared.request.clock)?;
        validate_leverage_post_state(
            &next_market,
            self.require_market_key()?,
            &next_leverage,
            &next_base_hlp,
            &next_quote_hlp,
        )?;
        let receipt = BenchmarkLeverageOpenExecutionReceipt {
            native,
            margin_transfer: prepared.request.margin_transfer,
            collateral_transfer: execution.collateral_transfer,
            metrics,
        };
        let market_after = market_checkpoint(&next_market, self.market_key, prepared.request.clock)?;
        let revenue_after = revenue_checkpoint(&next_market);
        *self.market = next_market;
        self.clock = prepared.request.clock;
        *leverage = next_leverage;
        *base_hlp = next_base_hlp;
        *quote_hlp = next_quote_hlp;
        Ok(BenchmarkLeverageExecution {
            market: BenchmarkMarketExecution {
                receipt,
                cash,
                revenue_before,
                revenue_after,
                market_after,
            },
            leverage_after: leverage.checkpoint(),
            base_hlp_after: base_hlp.checkpoint(),
            quote_hlp_after: quote_hlp.checkpoint(),
        })
    }

    /// Prepares the exact owner-authorized full unwind path. Partial delegated
    /// closes deliberately remain outside this narrow isolated-position API.
    pub fn prepare_leverage_close(
        &self,
        leverage: &BenchmarkLeverageOwnedState,
        base_hlp: &BenchmarkHlpOwnedState,
        quote_hlp: &BenchmarkHlpOwnedState,
        request: BenchmarkPrepareLeverageCloseRequest,
    ) -> Result<BenchmarkPreparedLeverageClose> {
        self.require_monotonic_clock(request.clock)?;
        require!(leverage.position_exists, ErrorCode::InvalidLeveragePosition);
        leverage.position.assert_position(
            leverage.position_owner,
            self.require_market_key()?,
            leverage.position.debt_asset()?,
        )?;
        validate_leverage_policy(
            &self.market,
            leverage.position.collateral_asset()?,
            request.clock,
            request.policy,
            false,
        )?;
        validate_leverage_hlp_pair(self, base_hlp, quote_hlp)?;
        require_gte!(
            MAX_REFERRAL_INTEREST_SHARE_BPS,
            request.policy.max_referral_interest_share_bps,
            ErrorCode::InvalidReferralInterestShareBps
        );
        if leverage.position.referral_partner == Pubkey::default() {
            require_eq!(
                leverage.position.referral_interest_share_bps,
                0,
                ErrorCode::BrokenInvariant
            );
            require!(leverage.referral.is_none(), ErrorCode::InvalidReferralPartner);
        } else {
            require_gte!(
                MAX_REFERRAL_INTEREST_SHARE_BPS,
                leverage.position.referral_interest_share_bps,
                ErrorCode::InvalidReferralInterestShareBps
            );
            let referral = leverage.referral.as_ref().ok_or(ErrorCode::InvalidReferralPartner)?;
            validate_referral_owned_state(
                referral,
                self.require_market_key()?,
                self.market.side(leverage.position.debt_asset()?).asset_mint,
            )?;
            require_keys_eq!(
                referral.partner_key,
                leverage.position.referral_partner,
                ErrorCode::InvalidReferralPartner
            );
        }
        let mut prepared_market = clone_market(&self.market)?;
        prepared_market.accrue_interest_to_slot(request.clock.slot)?;
        let slice = prepared_market.leverage_close_slice(&leverage.position, BPS_DENOMINATOR)?;
        require_eq!(
            request.collateral_transfer.source_debit,
            slice.collateral_amount,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            request.collateral_transfer.destination_credit,
            request.collateral_transfer.source_debit,
            ErrorCode::InvalidLeverageCollateralMint
        );
        require_gte!(
            leverage_collateral_vault_balance(&leverage.token_balances, leverage.position.collateral_asset()?),
            request.collateral_transfer.source_debit,
            ErrorCode::InsufficientBalance
        );
        prepared_market.prepare_amm_for_swap(request.clock.slot)?;
        prepared_market.advance_one_amm_controller_target(request.clock.slot)?;
        prepared_market.observe_current_risk(request.clock.slot)?;
        let debt_asset = leverage.position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_amount = prepared_market
            .debt
            .isolated_repayment_for_max(debt_asset, slice.debt_shares, u64::MAX)?
            .cash_repaid;
        let metrics_before = leverage_metrics(&prepared_market, &leverage.position, request.clock)?;
        let prepared_swap = prepare_benchmark_leverage_swap(
            &mut prepared_market,
            SwapRequest {
                current_slot: request.clock.slot,
                current_unix_timestamp: request.clock.unix_timestamp,
                asset_in: collateral_asset,
                reserve_credit: request.collateral_transfer.destination_credit,
                protocol_fee_bps: request.policy.protocol_swap_fee_bps,
            },
            SwapCashPolicy::Close {
                debt_asset,
                debt_shares: slice.debt_shares,
                debt_principal: slice.debt_principal,
            },
        )?;
        require_gte!(
            prepared_swap.swap.amount_out,
            debt_amount,
            ErrorCode::InsufficientAmount
        );
        let expected_gross_residual = prepared_swap
            .swap
            .amount_out
            .checked_sub(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(
            expected_gross_residual,
            request.min_residual_out,
            ErrorCode::SlippageExceeded
        );
        let mut settlement_market = clone_market(&prepared_market)?;
        let mut settlement_position = clone_leverage_position(&leverage.position)?;
        let settlement_preview = *settlement_market.close_leverage(
            &mut settlement_position,
            request.min_residual_out,
            prepared_swap.clone(),
            full_leverage_swap_fee_credit(prepared_swap.swap)?,
            request.policy.protocol_swap_fee_bps,
            request.policy.protocol_auction_split,
            request.clock.slot,
        )?;
        require_eq!(
            settlement_preview.residual,
            expected_gross_residual,
            ErrorCode::BrokenInvariant
        );
        let quote = BenchmarkLeverageCloseQuote {
            debt_amount,
            collateral_sold: slice.collateral_amount,
            swap: prepared_swap.swap,
            expected_gross_residual,
            metrics_before,
            settlement: BenchmarkLeverageSettlementRequirements {
                base_hlp_interest_debit: settlement_preview.base_hlp_rebalance.interest_paid,
                quote_hlp_interest_debit: settlement_preview.quote_hlp_rebalance.interest_paid,
                position_interest_debit: settlement_preview.interest_paid,
            },
        };
        Ok(BenchmarkPreparedLeverageClose {
            clock_before: self.clock,
            market_before: serialize_market(&self.market)?,
            leverage_before: leverage.checkpoint(),
            base_hlp_before: base_hlp.checkpoint(),
            quote_hlp_before: quote_hlp.checkpoint(),
            prepared_market: Box::new(prepared_market),
            prepared_swap,
            request,
            quote,
        })
    }

    pub fn execute_prepared_leverage_close(
        &mut self,
        leverage: &mut BenchmarkLeverageOwnedState,
        base_hlp: &mut BenchmarkHlpOwnedState,
        quote_hlp: &mut BenchmarkHlpOwnedState,
        prepared: BenchmarkPreparedLeverageClose,
        execution: BenchmarkExecuteLeverageCloseRequest,
    ) -> Result<BenchmarkLeverageExecution<BenchmarkLeverageCloseExecutionReceipt>> {
        require_prepared_identity(
            self,
            leverage,
            base_hlp,
            quote_hlp,
            prepared.clock_before,
            &prepared.market_before,
            prepared.leverage_before,
            prepared.base_hlp_before,
            prepared.quote_hlp_before,
        )?;
        let revenue_before = self.revenue_checkpoint();
        let mut next_market = *prepared.prepared_market;
        let mut next_leverage = leverage.try_fork()?;
        let mut next_base_hlp = base_hlp.try_fork()?;
        let mut next_quote_hlp = quote_hlp.try_fork()?;
        let close_collateral_asset = next_leverage.position.collateral_asset()?;
        let close_collateral_transfer = prepared.request.collateral_transfer;
        *leverage_collateral_vault_balance_mut(&mut next_leverage.token_balances, close_collateral_asset) =
            leverage_collateral_vault_balance(&next_leverage.token_balances, close_collateral_asset)
                .checked_sub(close_collateral_transfer.source_debit)
                .ok_or(ErrorCode::InsufficientBalance)?;
        *leverage_reserve_vault_balance_mut(&mut next_leverage.token_balances, close_collateral_asset) =
            leverage_reserve_vault_balance(&next_leverage.token_balances, close_collateral_asset)
                .checked_add(close_collateral_transfer.destination_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        let interest_eligibility = prepared.prepared_swap.interest_eligibility;
        let native = *next_market.close_leverage(
            &mut next_leverage.position,
            prepared.request.min_residual_out,
            prepared.prepared_swap,
            full_leverage_swap_fee_credit(prepared.quote.swap)?,
            prepared.request.policy.protocol_swap_fee_bps,
            prepared.request.policy.protocol_auction_split,
            prepared.request.clock.slot,
        )?;
        require_eq!(
            native.base_hlp_rebalance.interest_paid,
            prepared.quote.settlement.base_hlp_interest_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            native.quote_hlp_rebalance.interest_paid,
            prepared.quote.settlement.quote_hlp_interest_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            native.interest_paid,
            prepared.quote.settlement.position_interest_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            execution.residual_transfer.source_debit,
            native.residual,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            execution.residual_transfer.source_debit,
            execution.residual_transfer.destination_credit,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            execution.residual_transfer.destination_credit,
            prepared.request.min_residual_out,
            ErrorCode::SlippageExceeded
        );
        require_eq!(
            execution.interest_transfer.source_debit,
            native.interest_paid,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            execution.interest_transfer.source_debit,
            execution.interest_transfer.destination_credit,
            ErrorCode::BrokenInvariant
        );
        let mut cash = BenchmarkCashFlow::default();
        let collateral_asset = next_leverage.position.collateral_asset()?;
        let collateral_cash = cash.side_mut(collateral_asset);
        collateral_cash.collateral_vault_debit = prepared.request.collateral_transfer.source_debit;
        collateral_cash.reserve_vault_credit = prepared.request.collateral_transfer.destination_credit;
        apply_leverage_hlp_settlement(
            &mut next_market,
            &mut next_base_hlp,
            native.base_hlp_rebalance,
            interest_eligibility,
            execution.base_hlp_interest_transfer,
            prepared.request.policy,
            &mut next_leverage.token_balances,
            &mut cash,
        )?;
        apply_leverage_hlp_settlement(
            &mut next_market,
            &mut next_quote_hlp,
            native.quote_hlp_rebalance,
            interest_eligibility,
            execution.quote_hlp_interest_transfer,
            prepared.request.policy,
            &mut next_leverage.token_balances,
            &mut cash,
        )?;
        let debt_asset = MarketAsset::try_from_code(prepared.quote.swap.asset_in)?.opposite();
        *leverage_reserve_vault_balance_mut(&mut next_leverage.token_balances, debt_asset) =
            leverage_reserve_vault_balance(&next_leverage.token_balances, debt_asset)
                .checked_sub(execution.residual_transfer.source_debit)
                .and_then(|value| value.checked_sub(execution.interest_transfer.source_debit))
                .ok_or(ErrorCode::InsufficientLiquidity)?;
        *leverage_token_balance_mut(&mut next_leverage.token_balances, debt_asset) =
            leverage_token_balance(&next_leverage.token_balances, debt_asset)
                .checked_add(execution.residual_transfer.destination_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        *leverage_interest_vault_balance_mut(&mut next_leverage.token_balances, debt_asset) =
            leverage_interest_vault_balance(&next_leverage.token_balances, debt_asset)
                .checked_add(execution.interest_transfer.destination_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        let expected_referral_partner = next_leverage.position.referral_partner;
        let frozen_share_bps = next_leverage.position.referral_interest_share_bps;
        let asset_mint = next_market.side(debt_asset).asset_mint;
        let referral = next_leverage.referral.as_mut();
        let referral_share = if expected_referral_partner == Pubkey::default() {
            require_eq!(frozen_share_bps, 0, ErrorCode::BrokenInvariant);
            require!(referral.is_none(), ErrorCode::InvalidReferralPartner);
            None
        } else {
            let referral_state = referral.as_deref().ok_or(ErrorCode::InvalidReferralPartner)?;
            validate_referral_owned_state(referral_state, self.require_market_key()?, asset_mint)?;
            require_keys_eq!(
                referral_state.partner_key,
                expected_referral_partner,
                ErrorCode::InvalidReferralPartner
            );
            Some(frozen_share_bps)
        };
        let referral_interest = ReferralInterestQuote::new(
            execution.interest_transfer.source_debit,
            execution.interest_transfer.destination_credit,
            prepared.request.policy.protocol_interest_fee_bps,
            referral_share,
        )?;
        if referral_interest.referral_amount > 0 {
            referral
                .ok_or(ErrorCode::InvalidReferralAccrual)?
                .accrual
                .accrue(referral_interest.referral_amount)?;
        }
        if execution.interest_transfer.source_debit > 0 {
            next_market.side_mut(debt_asset).record_interest_credit_with_supply(
                execution.interest_transfer.destination_credit,
                prepared.request.policy.protocol_interest_fee_bps,
                prepared.request.policy.protocol_auction_split,
                referral_interest.referral_amount,
                interest_eligibility.ylp_supply,
            )?;
            next_market
                .checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, interest_eligibility.base_hlp_ylp_shares)?;
            next_market
                .checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, interest_eligibility.quote_hlp_ylp_shares)?;
        }
        let debt_cash = cash.side_mut(debt_asset);
        debt_cash.reserve_vault_debit = debt_cash
            .reserve_vault_debit
            .checked_add(execution.residual_transfer.source_debit)
            .and_then(|value| value.checked_add(execution.interest_transfer.source_debit))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        debt_cash.recipient_credit = execution.residual_transfer.destination_credit;
        debt_cash.interest_vault_credit = debt_cash
            .interest_vault_credit
            .checked_add(execution.interest_transfer.destination_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        next_leverage.position_exists = false;
        next_leverage.position = Box::new(empty_leverage_position());
        validate_leverage_post_state(
            &next_market,
            self.require_market_key()?,
            &next_leverage,
            &next_base_hlp,
            &next_quote_hlp,
        )?;
        let receipt = BenchmarkLeverageCloseExecutionReceipt {
            native,
            collateral_transfer: prepared.request.collateral_transfer,
            residual_transfer: execution.residual_transfer,
            interest_transfer: execution.interest_transfer,
            referral_interest,
            metrics_before: prepared.quote.metrics_before,
        };
        let market_after = market_checkpoint(&next_market, self.market_key, prepared.request.clock)?;
        let revenue_after = revenue_checkpoint(&next_market);
        *self.market = next_market;
        self.clock = prepared.request.clock;
        *leverage = next_leverage;
        *base_hlp = next_base_hlp;
        *quote_hlp = next_quote_hlp;
        Ok(BenchmarkLeverageExecution {
            market: BenchmarkMarketExecution {
                receipt,
                cash,
                revenue_before,
                revenue_after,
                market_after,
            },
            leverage_after: leverage.checkpoint(),
            base_hlp_after: base_hlp.checkpoint(),
            quote_hlp_after: quote_hlp.checkpoint(),
        })
    }

    /// Finds the greatest net target-asset credit admitted by the native hLP
    /// entry path up to `maximum_target_reserve_credit`.
    ///
    /// Borrow headroom is monotone in the same-ratio borrowed leg, so the
    /// helper first locates its exact integer boundary. It then executes the
    /// complete native entry, settlement, health, yield-account, and external
    /// custody path on a fork. If a non-capacity guard rejects that boundary,
    /// the error is returned rather than approximating a smaller allocation.
    pub fn maximize_hlp_entry(
        &self,
        external: &BenchmarkHlpOwnedState,
        clock: BenchmarkClock,
        maximum_target_reserve_credit: u64,
        global_reduce_only: bool,
    ) -> Result<Option<BenchmarkMaximumHlpEntry>> {
        self.require_monotonic_clock(clock)?;
        external.validate(self)?;
        if maximum_target_reserve_credit == 0 {
            return Ok(None);
        }

        let mut prepared = clone_market(&self.market)?;
        prepare_hlp_entry_market(
            &mut prepared,
            external.target_asset,
            external.hlp_mint_supply,
            clock,
            global_reduce_only,
        )?;
        let borrowed_asset = external.target_asset.opposite();
        let mut low = 0_u64;
        let mut high = maximum_target_reserve_credit;
        while low < high {
            let midpoint = low + (high - low) / 2 + 1;
            let borrowed_amount = hlp_borrowed_amount(&prepared, external.target_asset, midpoint)?;
            let vault = hlp_vault(&prepared, external.target_asset);
            let borrow_index_nad = prepared.debt.borrow_index(borrowed_asset);
            let added_shares = Debt::debt_to_shares(borrowed_amount, borrow_index_nad)?;
            let fits = if let Some(projected_shares) = vault.debt_shares.checked_add(added_shares) {
                Debt::shares_to_debt(projected_shares, borrow_index_nad)?
                    <= prepared.side(borrowed_asset).reserves.cash_reserve as u128
            } else {
                false
            };
            if fits {
                low = midpoint;
            } else {
                high = midpoint - 1;
            }
        }
        let target_reserve_credit = if low > 0 && hlp_borrowed_amount(&prepared, external.target_asset, low)? == 0 {
            0
        } else {
            low
        };
        if target_reserve_credit == 0 {
            return Ok(None);
        }

        let request = BenchmarkHlpEntryRequest {
            clock,
            // This is a state-only probe. The caller supplies the historical
            // gross debit after receiving the selected net credit.
            target_transfer: BenchmarkTokenTransferOutcome {
                source_debit: target_reserve_credit,
                destination_credit: target_reserve_credit,
            },
            min_hlp_amount: 1,
            global_reduce_only,
        };
        let mut next_external = external.try_fork()?;
        let (native, _) = apply_hlp_entry_after_prepare(&mut prepared, &mut next_external, request)?;
        validate_hlp_owned_state(&next_external, &prepared, self.require_market_key()?)?;
        Ok(Some(BenchmarkMaximumHlpEntry {
            clock,
            target_asset: external.target_asset,
            maximum_considered_reserve_credit: maximum_target_reserve_credit,
            target_reserve_credit,
            native,
        }))
    }

    /// Commits a previously selected maximum-feasible entry. The plan is
    /// revalidated against the current market, while the caller supplies the
    /// exact gross debit/net credit Token-2022 outcome.
    pub fn execute_maximum_hlp_entry(
        &mut self,
        external: &mut BenchmarkHlpOwnedState,
        plan: BenchmarkMaximumHlpEntry,
        target_transfer: BenchmarkTokenTransferOutcome,
        global_reduce_only: bool,
    ) -> Result<BenchmarkHlpExecution<BenchmarkHlpEntryReceipt>> {
        require!(plan.target_asset == external.target_asset, ErrorCode::InvalidMint);
        require_eq!(
            plan.target_reserve_credit,
            target_transfer.destination_credit,
            ErrorCode::BrokenInvariant
        );
        let current_plan = self
            .maximize_hlp_entry(
                external,
                plan.clock,
                plan.maximum_considered_reserve_credit,
                global_reduce_only,
            )?
            .ok_or(ErrorCode::InsufficientBorrowHeadroom)?;
        require_eq!(
            current_plan.target_reserve_credit,
            plan.target_reserve_credit,
            ErrorCode::BrokenInvariant
        );
        require!(current_plan.native == plan.native, ErrorCode::BrokenInvariant);
        self.execute_hlp_entry(
            external,
            BenchmarkHlpEntryRequest {
                clock: plan.clock,
                target_transfer,
                min_hlp_amount: plan.native.hlp_amount,
                global_reduce_only,
            },
        )
    }

    /// Atomically mirrors `DepositSingleSided`: clock/reconciliation, native
    /// entry, risk finalization, holder yield checkpoint, and both token mints.
    pub fn execute_hlp_entry(
        &mut self,
        external: &mut BenchmarkHlpOwnedState,
        request: BenchmarkHlpEntryRequest,
    ) -> Result<BenchmarkHlpExecution<BenchmarkHlpEntryReceipt>> {
        self.require_monotonic_clock(request.clock)?;
        external.validate(self)?;
        let revenue_before = self.revenue_checkpoint();
        let market_key = self.require_market_key()?;
        let mut next_market = clone_market(&self.market)?;
        let mut next_external = external.try_fork()?;
        prepare_hlp_entry_market(
            &mut next_market,
            next_external.target_asset,
            next_external.hlp_mint_supply,
            request.clock,
            request.global_reduce_only,
        )?;
        let (native, cash) = apply_hlp_entry_after_prepare(&mut next_market, &mut next_external, request)?;
        validate_hlp_owned_state(&next_external, &next_market, market_key)?;
        let receipt = BenchmarkHlpEntryReceipt {
            native,
            target_transfer: request.target_transfer,
        };
        let external_after = next_external.checkpoint();
        let market = self.commit_market_execution(next_market, request.clock, revenue_before, cash, receipt)?;
        *external = next_external;
        Ok(BenchmarkHlpExecution { market, external_after })
    }

    /// Quotes the exact native principal/yLP/debt retirement at an explicit
    /// clock without committing market or holder state. The returned gross
    /// debits let the caller obtain the corresponding historical Token-2022
    /// credits before calling [`Self::execute_hlp_withdraw`].
    pub fn preview_hlp_withdrawal(
        &self,
        external: &BenchmarkHlpOwnedState,
        clock: BenchmarkClock,
        hlp_amount: u64,
    ) -> Result<SingleSidedLiquidityReceipt> {
        self.require_monotonic_clock(clock)?;
        external.validate(self)?;
        require_gte!(
            external.holder_hlp_token_balance,
            hlp_amount,
            ErrorCode::InsufficientBalance
        );
        let mut preview = clone_market(&self.market)?;
        prepare_hlp_withdraw_market(&mut preview, external.target_asset, external.hlp_mint_supply, clock)?;
        preview.checkpoint_hlp_yield_from_ylp(external.target_asset)?;
        preview.withdraw_single_sided(external.target_asset, hlp_amount)
    }

    /// Atomically mirrors `WithdrawSingleSided`, including pro-rata native
    /// debt/yLP retirement, final-holder yield drain, measured funding-interest
    /// credit, backing-yLP burn, and the exact Token-2022 recipient outcome.
    pub fn execute_hlp_withdraw(
        &mut self,
        external: &mut BenchmarkHlpOwnedState,
        request: BenchmarkHlpWithdrawRequest,
    ) -> Result<BenchmarkHlpExecution<BenchmarkHlpWithdrawReceipt>> {
        self.require_monotonic_clock(request.clock)?;
        external.validate(self)?;
        require!(
            request.protocol_auction_split.is_valid(),
            ErrorCode::InvalidAuctionConfig
        );
        let revenue_before = self.revenue_checkpoint();
        let market_key = self.require_market_key()?;
        let mut next_market = clone_market(&self.market)?;
        let mut next_external = external.try_fork()?;
        prepare_hlp_withdraw_market(
            &mut next_market,
            next_external.target_asset,
            next_external.hlp_mint_supply,
            request.clock,
        )?;
        require!(request.hlp_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            next_external.holder_hlp_token_balance,
            request.hlp_amount,
            ErrorCode::InsufficientBalance
        );
        let eligibility = HlpYieldEligibility {
            ylp_supply: next_market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: next_market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: next_market.quote_hlp_vault.ylp_shares,
        };
        require_eq!(
            eligibility.ylp_supply,
            next_market.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        next_market.checkpoint_hlp_yield_from_ylp(next_external.target_asset)?;
        let (base_swap_growth, base_interest_growth) =
            next_market.hlp_yield_growth_indexes(next_external.target_asset, MarketAsset::Base);
        let (quote_swap_growth, quote_interest_growth) =
            next_market.hlp_yield_growth_indexes(next_external.target_asset, MarketAsset::Quote);
        next_external.base_yield_account.accrue(
            next_external.holder_hlp_token_balance,
            base_swap_growth,
            base_interest_growth,
        )?;
        next_external.quote_yield_account.accrue(
            next_external.holder_hlp_token_balance,
            quote_swap_growth,
            quote_interest_growth,
        )?;

        // Token-2022 burns occur before the market transition in production.
        next_external.holder_hlp_token_balance = next_external
            .holder_hlp_token_balance
            .checked_sub(request.hlp_amount)
            .ok_or(ErrorCode::InsufficientBalance)?;
        next_external.hlp_mint_supply = next_external
            .hlp_mint_supply
            .checked_sub(request.hlp_amount)
            .ok_or(ErrorCode::InvalidHlpMintSupply)?;
        let native = next_market.withdraw_single_sided(next_external.target_asset, request.hlp_amount)?;

        require_eq!(
            request.interest_transfer.source_debit,
            native.interest_paid,
            ErrorCode::BrokenInvariant
        );
        let borrowed_asset = next_external.target_asset.opposite();
        if native.interest_paid == 0 {
            require_eq!(
                request.interest_transfer.destination_credit,
                0,
                ErrorCode::BrokenInvariant
            );
        } else {
            record_hlp_interest_credit(
                &mut next_market,
                borrowed_asset,
                request.interest_transfer.destination_credit,
                request.protocol_interest_fee_bps,
                request.protocol_auction_split,
                eligibility,
            )?;
            let measured_credits = match borrowed_asset {
                MarketAsset::Base => &mut next_external.measured_base_interest_vault_credits,
                MarketAsset::Quote => &mut next_external.measured_quote_interest_vault_credits,
            };
            *measured_credits = measured_credits
                .checked_add(request.interest_transfer.destination_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        if native.hlp_supply == 0 {
            next_market.drain_hlp_unallocated_yield(
                next_external.target_asset,
                &mut next_external.base_yield_account,
                &mut next_external.quote_yield_account,
            )?;
        }
        next_market.finalize_amm_transition_and_observe_risk(request.clock.slot)?;
        next_market.assert_market_health()?;

        next_external.hlp_vault_ylp_balance = next_external
            .hlp_vault_ylp_balance
            .checked_sub(native.ylp_amount)
            .ok_or(ErrorCode::InsufficientBalance)?;
        require_eq!(
            request.target_transfer.source_debit,
            native.target_amount_out,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            request.target_transfer.destination_credit,
            request.min_target_recipient_credit,
            ErrorCode::SlippageExceeded
        );

        let mut cash = BenchmarkCashFlow::default();
        let target_cash = cash.side_mut(next_external.target_asset);
        target_cash.reserve_vault_debit = native.target_amount_out;
        target_cash.recipient_credit = request.target_transfer.destination_credit;
        let borrowed_cash = cash.side_mut(borrowed_asset);
        borrowed_cash.reserve_vault_debit = native.interest_paid;
        borrowed_cash.interest_vault_credit = request.interest_transfer.destination_credit;
        validate_hlp_owned_state(&next_external, &next_market, market_key)?;
        let receipt = BenchmarkHlpWithdrawReceipt {
            native,
            target_transfer: request.target_transfer,
            interest_transfer: request.interest_transfer,
        };
        let external_after = next_external.checkpoint();
        let market = self.commit_market_execution(next_market, request.clock, revenue_before, cash, receipt)?;
        *external = next_external;
        Ok(BenchmarkHlpExecution { market, external_after })
    }

    /// Atomically applies the balanced-yLP instruction's market-side ordering:
    /// update, distribute already-backed yield, native add, then exact curve
    /// and risk finalization. Holder YieldAccounts remain external assertions.
    pub fn execute_add_ylp(
        &mut self,
        request: BenchmarkAddYlpRequest,
    ) -> Result<BenchmarkMarketExecution<BenchmarkYlpLiquidityReceipt>> {
        self.require_monotonic_clock(request.clock)?;
        let revenue_before = self.revenue_checkpoint();
        let mut next = clone_market(&self.market)?;
        advance_market_to_slot(&mut next, request.clock.slot)?;
        require!(
            !request.global_reduce_only && !next.reduce_only,
            ErrorCode::ReduceOnlyMode
        );
        next.require_initial_liquidity_authority(request.owner)?;
        carry_forward_all_ylp_revenue(&mut next)?;
        let receipt = next.add_liquidity(request.base_reserve_credit, request.quote_reserve_credit)?;
        require_gte!(receipt.ylp_amount, request.min_ylp_amount, ErrorCode::SlippageExceeded);
        next.finalize_amm_transition_and_observe_risk(request.clock.slot)?;

        let mut cash = BenchmarkCashFlow::default();
        cash.base.reserve_vault_credit = receipt.base_reserve_credit;
        cash.quote.reserve_vault_credit = receipt.quote_reserve_credit;
        let output = BenchmarkYlpLiquidityReceipt {
            ylp_amount: receipt.ylp_amount,
            ylp_supply: receipt.ylp_supply,
            base_reserve_amount: receipt.base_reserve_credit,
            quote_reserve_amount: receipt.quote_reserve_credit,
        };
        self.commit_market_execution(next, request.clock, revenue_before, cash, output)
    }

    /// Atomically applies balanced-yLP removal. Recipient credits are required
    /// inputs because Token-2022 fees live outside Market state.
    pub fn execute_remove_ylp(
        &mut self,
        request: BenchmarkRemoveYlpRequest,
    ) -> Result<BenchmarkMarketExecution<BenchmarkYlpLiquidityReceipt>> {
        self.require_monotonic_clock(request.clock)?;
        let revenue_before = self.revenue_checkpoint();
        let mut next = clone_market(&self.market)?;
        advance_market_to_slot(&mut next, request.clock.slot)?;
        next.assert_started_at(request.clock.unix_timestamp)?;
        require_gte!(
            request.owner_ylp_balance_before,
            request.ylp_amount,
            ErrorCode::InsufficientBalance
        );
        carry_forward_all_ylp_revenue(&mut next)?;
        let receipt = next.remove_liquidity(request.ylp_amount)?;
        require_gte!(
            request.base_recipient_credit,
            request.min_base_recipient_credit,
            ErrorCode::SlippageExceeded
        );
        require_gte!(
            request.quote_recipient_credit,
            request.min_quote_recipient_credit,
            ErrorCode::SlippageExceeded
        );
        next.finalize_amm_transition_and_observe_risk(request.clock.slot)?;
        next.assert_market_health()?;

        let mut cash = BenchmarkCashFlow::default();
        cash.base.reserve_vault_debit = receipt.base_amount_out;
        cash.quote.reserve_vault_debit = receipt.quote_amount_out;
        cash.base.recipient_credit = request.base_recipient_credit;
        cash.quote.recipient_credit = request.quote_recipient_credit;
        let output = BenchmarkYlpLiquidityReceipt {
            ylp_amount: receipt.ylp_amount,
            ylp_supply: receipt.ylp_supply,
            base_reserve_amount: receipt.base_amount_out,
            quote_reserve_amount: receipt.quote_amount_out,
        };
        self.commit_market_execution(next, request.clock, revenue_before, cash, output)
    }

    pub fn execute_deposit_collateral(
        &mut self,
        position: &mut BenchmarkBorrowPosition,
        request: BenchmarkDepositCollateralRequest,
    ) -> Result<BenchmarkPositionExecution<CollateralReceipt>> {
        self.require_monotonic_clock(request.clock)?;
        self.assert_position_market(position)?;
        let revenue_before = self.revenue_checkpoint();
        let mut next_market = clone_market(&self.market)?;
        let mut next_position = clone_borrow_position(&position.position)?;
        advance_market_to_slot(&mut next_market, request.clock.slot)?;
        next_market.assert_started_at(request.clock.unix_timestamp)?;
        let receipt =
            next_market.deposit_collateral(&mut next_position, request.collateral_asset, request.collateral_credit)?;
        let mut cash = BenchmarkCashFlow::default();
        cash.side_mut(request.collateral_asset).collateral_vault_credit = request.collateral_credit;
        self.commit_position_execution(
            position,
            next_market,
            next_position,
            request.clock,
            revenue_before,
            cash,
            receipt,
        )
    }

    pub fn execute_withdraw_collateral(
        &mut self,
        position: &mut BenchmarkBorrowPosition,
        request: BenchmarkWithdrawCollateralRequest,
    ) -> Result<BenchmarkPositionExecution<CollateralReceipt>> {
        self.require_monotonic_clock(request.clock)?;
        self.assert_position_market(position)?;
        let revenue_before = self.revenue_checkpoint();
        let mut next_market = clone_market(&self.market)?;
        let mut next_position = clone_borrow_position(&position.position)?;
        advance_market_to_slot(&mut next_market, request.clock.slot)?;
        next_market.assert_started_at(request.clock.unix_timestamp)?;
        require_gte!(
            request.collateral_vault_balance_before,
            request.collateral_debit,
            ErrorCode::InsufficientBalance
        );
        if request.global_reduce_only || next_market.reduce_only {
            let debt = next_position
                .fixed_base_debt(&next_market.debt)?
                .checked_add(next_position.fixed_quote_debt(&next_market.debt)?)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            require!(debt == 0, ErrorCode::ReduceOnlyHasDebt);
        }
        require_gte!(
            request.recipient_credit,
            request.min_recipient_credit,
            ErrorCode::SlippageExceeded
        );
        let receipt = next_market.withdraw_collateral(
            &mut next_position,
            request.collateral_asset,
            request.collateral_debit,
            request.min_liquidation_cf_bps,
        )?;
        let mut cash = BenchmarkCashFlow::default();
        let side = cash.side_mut(request.collateral_asset);
        side.collateral_vault_debit = request.collateral_debit;
        side.recipient_credit = request.recipient_credit;
        self.commit_position_execution(
            position,
            next_market,
            next_position,
            request.clock,
            revenue_before,
            cash,
            receipt,
        )
    }

    pub fn execute_borrow(
        &mut self,
        position: &mut BenchmarkBorrowPosition,
        request: BenchmarkBorrowRequest,
    ) -> Result<BenchmarkPositionExecution<DebtReceipt>> {
        self.require_monotonic_clock(request.clock)?;
        self.assert_position_market(position)?;
        let revenue_before = self.revenue_checkpoint();
        let mut next_market = clone_market(&self.market)?;
        let mut next_position = clone_borrow_position(&position.position)?;
        advance_market_to_slot(&mut next_market, request.clock.slot)?;
        next_market.assert_started_at(request.clock.unix_timestamp)?;
        require!(
            !request.global_reduce_only && !next_market.reduce_only,
            ErrorCode::ReduceOnlyMode
        );
        require_gte!(
            request.recipient_credit,
            request.min_recipient_credit,
            ErrorCode::SlippageExceeded
        );
        require_gte!(
            BPS_DENOMINATOR,
            request.referral_interest_share_cap_bps,
            ErrorCode::InvalidReferralInterestShareBps
        );
        require_gte!(
            request.referral_interest_share_cap_bps,
            request.referral_interest_share_bps,
            ErrorCode::InvalidReferralInterestShareBps
        );
        if request.referral_partner == Pubkey::default() {
            require_eq!(request.referral_interest_share_bps, 0, ErrorCode::BrokenInvariant);
        }
        let debt_before = match request.debt_asset {
            MarketAsset::Base => next_position.fixed_base_debt(&next_market.debt)?,
            MarketAsset::Quote => next_position.fixed_quote_debt(&next_market.debt)?,
        };
        if debt_before == 0 {
            require_keys_eq!(
                next_position.referral_partner(request.debt_asset),
                Pubkey::default(),
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                next_position.referral_interest_share_bps(request.debt_asset),
                0,
                ErrorCode::BrokenInvariant
            );
            next_position.set_referral_binding(
                request.debt_asset,
                request.referral_partner,
                request.referral_interest_share_bps,
            );
        } else {
            require_keys_eq!(
                next_position.referral_partner(request.debt_asset),
                request.referral_partner,
                ErrorCode::InvalidReferralPartner
            );
            require_eq!(
                next_position.referral_interest_share_bps(request.debt_asset),
                request.referral_interest_share_bps,
                ErrorCode::InvalidReferralInterestShareBps
            );
        }
        let receipt = next_market.borrow(
            &mut next_position,
            request.debt_asset,
            request.borrow_amount,
            request.min_liquidation_cf_bps,
            request.clock.slot,
        )?;
        next_market.finalize_amm_transition(request.clock.slot)?;
        next_market.refresh_risk_at_slot(request.clock.slot)?;

        let mut cash = BenchmarkCashFlow::default();
        let side = cash.side_mut(request.debt_asset);
        side.reserve_vault_debit = request.borrow_amount;
        side.recipient_credit = request.recipient_credit;
        self.commit_position_execution(
            position,
            next_market,
            next_position,
            request.clock,
            revenue_before,
            cash,
            receipt,
        )
    }

    pub fn execute_repay(
        &mut self,
        position: &mut BenchmarkBorrowPosition,
        request: BenchmarkRepayRequest,
    ) -> Result<BenchmarkPositionExecution<DebtReceipt>> {
        self.require_monotonic_clock(request.clock)?;
        self.assert_position_market(position)?;
        require!(
            request.protocol_auction_split.is_valid(),
            ErrorCode::InvalidAuctionConfig
        );
        let revenue_before = self.revenue_checkpoint();
        let mut next_market = clone_market(&self.market)?;
        let mut next_position = clone_borrow_position(&position.position)?;
        advance_market_to_slot(&mut next_market, request.clock.slot)?;
        next_market.assert_started_at(request.clock.unix_timestamp)?;
        let referral_binding = referral_binding(&next_position, request.debt_asset);
        let repayment =
            next_market.fixed_repayment_for_max(&next_position, request.debt_asset, request.max_repay_credit)?;
        let receipt = next_market.repay(&mut next_position, request.debt_asset, repayment.cash_repaid)?;
        require_eq!(receipt.cash_repaid, repayment.cash_repaid, ErrorCode::BrokenInvariant);
        require_gte!(
            receipt.interest_paid,
            request.interest_vault_credit,
            ErrorCode::FeeMathOverflow
        );
        let expected_referral = referral_interest_amount(
            referral_binding,
            receipt.interest_paid,
            request.interest_vault_credit,
            request.protocol_interest_fee_bps,
        )?;
        require_eq!(
            expected_referral,
            request.referral_interest_amount,
            ErrorCode::BrokenInvariant
        );
        if receipt.interest_paid == 0 {
            require_eq!(request.interest_vault_credit, 0, ErrorCode::BrokenInvariant);
        } else {
            next_market.side_mut(request.debt_asset).record_interest_credit(
                request.interest_vault_credit,
                request.protocol_interest_fee_bps,
                request.protocol_auction_split,
                request.referral_interest_amount,
            )?;
        }
        next_market.finalize_amm_transition(request.clock.slot)?;
        next_market.refresh_risk_at_slot(request.clock.slot)?;

        let mut cash = BenchmarkCashFlow::default();
        let side = cash.side_mut(request.debt_asset);
        side.reserve_vault_credit = receipt.cash_repaid;
        side.reserve_vault_debit = receipt.interest_paid;
        side.interest_vault_credit = request.interest_vault_credit;
        self.commit_position_execution(
            position,
            next_market,
            next_position,
            request.clock,
            revenue_before,
            cash,
            receipt,
        )
    }

    /// Opens the native liquidation auction and atomically owns both account
    /// writes. A failed health check or late checkpoint failure leaves market
    /// and position byte-for-byte unchanged.
    pub fn execute_start_liquidation_auction(
        &mut self,
        position: &mut BenchmarkBorrowPosition,
        request: BenchmarkStartLiquidationAuctionRequest,
    ) -> Result<BenchmarkPositionExecution<BenchmarkLiquidationAuctionReceipt>> {
        self.require_monotonic_clock(request.clock)?;
        self.assert_position_market(position)?;
        let revenue_before = self.revenue_checkpoint();
        let mut next_market = clone_market(&self.market)?;
        let mut next_position = clone_borrow_position(&position.position)?;
        advance_market_to_slot(&mut next_market, request.clock.slot)?;
        next_market.assert_started_at(request.clock.unix_timestamp)?;
        let reference_price_nad = next_market.liquidation_reference_price_nad(&next_position, request.debt_asset)?;
        require!(
            next_market.is_position_liquidatable(&next_position, request.debt_asset)?,
            ErrorCode::PositionNotLiquidatable
        );
        require!(
            !next_position.has_active_liquidation_auction(),
            ErrorCode::PositionNotLiquidatable
        );
        let start_price_nad = reference_price_nad
            .checked_mul(BENCHMARK_LIQUIDATION_START_PREMIUM_NUMERATOR)
            .and_then(|value| value.checked_div(BENCHMARK_LIQUIDATION_START_PREMIUM_DENOMINATOR))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        next_position.start_liquidation_auction(
            request.debt_asset,
            request.clock.unix_timestamp,
            start_price_nad,
            reference_price_nad,
        );
        let receipt = BenchmarkLiquidationAuctionReceipt {
            debt_asset: request.debt_asset,
            reference_price_nad,
            start_price_nad,
            floor_price_nad: reference_price_nad,
            start_time: request.clock.unix_timestamp,
            first_floor_unix_timestamp: first_liquidation_floor_timestamp(
                request.clock.unix_timestamp,
                start_price_nad,
                reference_price_nad,
            )?,
        };
        self.commit_position_execution(
            position,
            next_market,
            next_position,
            request.clock,
            revenue_before,
            BenchmarkCashFlow::default(),
            receipt,
        )
    }

    /// Quotes exact native repayment, auction price, and insurance-vault debit
    /// without committing any account. Callers use the result to evaluate the
    /// historical Token-2022 schedules before requesting a receipt preview.
    pub fn preview_liquidation_plan(
        &self,
        position: &BenchmarkBorrowPosition,
        request: BenchmarkLiquidationPlanRequest,
    ) -> Result<BenchmarkLiquidationPlan> {
        self.require_monotonic_clock(request.clock)?;
        self.assert_position_market(position)?;
        let mut next_market = clone_market(&self.market)?;
        let mut next_position = clone_borrow_position(&position.position)?;
        advance_market_to_slot(&mut next_market, request.clock.slot)?;
        next_market.assert_started_at(request.clock.unix_timestamp)?;
        liquidation_plan(&mut next_market, &mut next_position, request)
    }

    /// Constructs the exact minimum token-account state consistent with the
    /// market ledgers. Callers supply borrower collateral custody and the
    /// historical liquidator's actual spendable balances; no donation or
    /// bidder capital is inferred.
    pub fn liquidation_token_balances_at_custody_floor(
        &self,
        liquidator_base_balance: u64,
        liquidator_quote_balance: u64,
        base_collateral_vault_balance: u64,
        quote_collateral_vault_balance: u64,
    ) -> Result<BenchmarkLiquidationTokenBalances> {
        Ok(BenchmarkLiquidationTokenBalances {
            liquidator_base_balance,
            liquidator_quote_balance,
            owner_base_balance: 0,
            owner_quote_balance: 0,
            base_reserve_vault_balance: required_reserve_custody(&self.market.base_side)?,
            quote_reserve_vault_balance: required_reserve_custody(&self.market.quote_side)?,
            base_interest_vault_balance: self.market.base_side.fees.interest_vault_balance,
            quote_interest_vault_balance: self.market.quote_side.fees.interest_vault_balance,
            base_collateral_vault_balance,
            quote_collateral_vault_balance,
            base_insurance_vault_balance: self.market.insurance.base_available,
            quote_insurance_vault_balance: self.market.insurance.quote_available,
        })
    }

    /// Applies the native state transition on a fork so downstream transfer
    /// debits can be converted through the exact historical mint schedule.
    pub fn preview_liquidation(
        &self,
        position: &BenchmarkBorrowPosition,
        request: BenchmarkLiquidationPreviewRequest,
    ) -> Result<BenchmarkLiquidationPreview> {
        self.require_monotonic_clock(request.plan.clock)?;
        self.assert_position_market(position)?;
        let mut next_market = clone_market(&self.market)?;
        let mut next_position = clone_borrow_position(&position.position)?;
        advance_market_to_slot(&mut next_market, request.plan.clock.slot)?;
        next_market.assert_started_at(request.plan.clock.unix_timestamp)?;
        preview_liquidation_on_state(&mut next_market, &mut next_position, request)
    }

    /// Executes bid or floor settlement with the same account-wide rollback
    /// boundary as the instruction: Market, BorrowPosition, and every touched
    /// token balance are forked first and committed only after custody, fee,
    /// slippage, and finalization checks all succeed.
    pub fn execute_liquidation(
        &mut self,
        position: &mut BenchmarkBorrowPosition,
        token_balances: &mut BenchmarkLiquidationTokenBalances,
        request: BenchmarkLiquidationExecuteRequest,
    ) -> Result<BenchmarkLiquidationExecution> {
        self.require_monotonic_clock(request.preview.plan.clock)?;
        self.assert_position_market(position)?;
        require!(
            request.protocol_auction_split.is_valid(),
            ErrorCode::InvalidAuctionConfig
        );
        let revenue_before = self.revenue_checkpoint();
        let mut next_market = clone_market(&self.market)?;
        let mut next_position = clone_borrow_position(&position.position)?;
        let mut next_balances = *token_balances;
        let clock = request.preview.plan.clock;
        advance_market_to_slot(&mut next_market, clock.slot)?;
        next_market.assert_started_at(clock.unix_timestamp)?;
        let preview = preview_liquidation_on_state(&mut next_market, &mut next_position, request.preview)?;
        match preview.plan.phase {
            BenchmarkLiquidationPhase::Bid => {
                require!(request.max_repay_source_debit > 0, ErrorCode::AmountZero);
                require_gte!(
                    request.max_repay_source_debit,
                    request.debt_transfer.source_debit,
                    ErrorCode::BrokenInvariant
                );
                require_eq!(
                    request.debt_transfer.destination_credit,
                    preview.plan.repay_credit,
                    ErrorCode::BrokenInvariant
                );
                require_gte!(
                    request.debt_transfer.source_debit,
                    request.debt_transfer.destination_credit,
                    ErrorCode::BrokenInvariant
                );
                require_eq!(
                    request.collateral_transfer.source_debit,
                    preview.native.collateral_to_liquidator,
                    ErrorCode::BrokenInvariant
                );
                require!(
                    request.collateral_swap_transfer == BenchmarkTokenTransferOutcome::default(),
                    ErrorCode::BrokenInvariant
                );
                require!(
                    request.owner_residual_transfer == BenchmarkTokenTransferOutcome::default(),
                    ErrorCode::BrokenInvariant
                );
                require_eq!(preview.owner_residual, 0, ErrorCode::BrokenInvariant);
            }
            BenchmarkLiquidationPhase::Floor => {
                require_eq!(request.max_repay_source_debit, 0, ErrorCode::InvalidArgument);
                require!(
                    request.debt_transfer == BenchmarkTokenTransferOutcome::default(),
                    ErrorCode::BrokenInvariant
                );
                require_eq!(
                    request.collateral_transfer.source_debit,
                    preview.plan.caller_bounty,
                    ErrorCode::BrokenInvariant
                );
                require_eq!(
                    request.collateral_swap_transfer.source_debit,
                    preview.plan.collateral_swap_debit,
                    ErrorCode::BrokenInvariant
                );
                require_eq!(
                    request.collateral_swap_transfer.destination_credit,
                    preview.plan.collateral_reserve_credit,
                    ErrorCode::BrokenInvariant
                );
                require_gte!(
                    request.collateral_swap_transfer.source_debit,
                    request.collateral_swap_transfer.destination_credit,
                    ErrorCode::BrokenInvariant
                );
                require_eq!(
                    request.owner_residual_transfer.source_debit,
                    preview.owner_residual,
                    ErrorCode::BrokenInvariant
                );
                require_gte!(
                    request.owner_residual_transfer.source_debit,
                    request.owner_residual_transfer.destination_credit,
                    ErrorCode::BrokenInvariant
                );
                require!(
                    request.insurance_funding_transfer == BenchmarkTokenTransferOutcome::default(),
                    ErrorCode::BrokenInvariant
                );
            }
        }
        require_eq!(
            request.insurance_draw_transfer.source_debit,
            preview.plan.insurance_draw_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            request.insurance_draw_transfer.destination_credit,
            request.preview.insurance_draw_credit,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            request.insurance_draw_transfer.source_debit,
            request.insurance_draw_transfer.destination_credit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            request.interest_transfer.source_debit,
            preview.native.interest_paid,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            request.interest_transfer.source_debit,
            request.interest_transfer.destination_credit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            request.collateral_transfer.source_debit,
            preview.native.collateral_to_liquidator,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            request.collateral_transfer.source_debit,
            request.collateral_transfer.destination_credit,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            request.collateral_transfer.destination_credit,
            request.min_collateral_recipient_credit,
            ErrorCode::SlippageExceeded
        );
        require_eq!(
            request.insurance_funding_transfer.source_debit,
            preview.native.insurance_funded,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            request.insurance_funding_transfer.source_debit,
            request.insurance_funding_transfer.destination_credit,
            ErrorCode::BrokenInvariant
        );

        if preview.plan.phase == BenchmarkLiquidationPhase::Bid {
            let liquidator_debt_balance = match preview.plan.debt_asset {
                MarketAsset::Base => next_balances.liquidator_base_balance,
                MarketAsset::Quote => next_balances.liquidator_quote_balance,
            };
            require_gte!(
                liquidator_debt_balance,
                request.debt_transfer.source_debit,
                ErrorCode::InsufficientBalance
            );
            debit_balance(
                liquidation_liquidator_balance_mut(&mut next_balances, preview.plan.debt_asset),
                request.debt_transfer.source_debit,
            )?;
            credit_balance(
                liquidation_reserve_balance_mut(&mut next_balances, preview.plan.debt_asset),
                request.debt_transfer.destination_credit,
            )?;
        }
        require_gte!(
            liquidation_insurance_balance(&next_balances, preview.plan.debt_asset),
            request.insurance_draw_transfer.source_debit,
            ErrorCode::InsufficientInsurance
        );
        debit_balance(
            liquidation_insurance_balance_mut(&mut next_balances, preview.plan.debt_asset),
            request.insurance_draw_transfer.source_debit,
        )?;
        credit_balance(
            liquidation_reserve_balance_mut(&mut next_balances, preview.plan.debt_asset),
            request.insurance_draw_transfer.destination_credit,
        )?;

        // `preview_liquidation_on_state` already applied the native settlement
        // to the staged account pair. Reconcile token-program outcomes and
        // revenue ownership exactly as the instruction does afterward.
        let referral_binding = referral_binding(&position.position, preview.plan.debt_asset);
        let expected_referral = referral_interest_amount(
            referral_binding,
            preview.native.interest_paid,
            request.interest_transfer.destination_credit,
            request.protocol_interest_fee_bps,
        )?;
        require_eq!(
            expected_referral,
            request.referral_interest_amount,
            ErrorCode::BrokenInvariant
        );
        if preview.native.interest_paid > 0 {
            next_market.side_mut(preview.plan.debt_asset).record_interest_credit(
                request.interest_transfer.destination_credit,
                request.protocol_interest_fee_bps,
                request.protocol_auction_split,
                request.referral_interest_amount,
            )?;
        } else {
            require_eq!(
                request.interest_transfer.destination_credit,
                0,
                ErrorCode::BrokenInvariant
            );
        }
        let collateral_asset = preview.plan.debt_asset.opposite();
        debit_balance(
            liquidation_reserve_balance_mut(&mut next_balances, preview.plan.debt_asset),
            request.interest_transfer.source_debit,
        )?;
        let interest_balance = match preview.plan.debt_asset {
            MarketAsset::Base => &mut next_balances.base_interest_vault_balance,
            MarketAsset::Quote => &mut next_balances.quote_interest_vault_balance,
        };
        credit_balance(interest_balance, request.interest_transfer.destination_credit)?;
        debit_balance(
            liquidation_collateral_balance_mut(&mut next_balances, collateral_asset),
            request.collateral_transfer.source_debit,
        )?;
        credit_balance(
            liquidation_liquidator_balance_mut(&mut next_balances, collateral_asset),
            request.collateral_transfer.destination_credit,
        )?;
        debit_balance(
            liquidation_collateral_balance_mut(&mut next_balances, collateral_asset),
            request.collateral_swap_transfer.source_debit,
        )?;
        credit_balance(
            liquidation_reserve_balance_mut(&mut next_balances, collateral_asset),
            request.collateral_swap_transfer.destination_credit,
        )?;
        debit_balance(
            liquidation_collateral_balance_mut(&mut next_balances, collateral_asset),
            request.insurance_funding_transfer.source_debit,
        )?;
        credit_balance(
            liquidation_insurance_balance_mut(&mut next_balances, collateral_asset),
            request.insurance_funding_transfer.destination_credit,
        )?;
        if preview.native.insurance_funded > 0 {
            next_market.insurance.reconcile_credit(
                preview.plan.debt_asset.opposite(),
                preview.native.insurance_funded,
                request.insurance_funding_transfer.destination_credit,
            )?;
        }
        debit_balance(
            liquidation_reserve_balance_mut(&mut next_balances, preview.plan.debt_asset),
            request.owner_residual_transfer.source_debit,
        )?;
        let owner_balance = match preview.plan.debt_asset {
            MarketAsset::Base => &mut next_balances.owner_base_balance,
            MarketAsset::Quote => &mut next_balances.owner_quote_balance,
        };
        credit_balance(owner_balance, request.owner_residual_transfer.destination_credit)?;
        if preview.plan.phase == BenchmarkLiquidationPhase::Bid {
            next_market.finalize_amm_transition(clock.slot)?;
            next_market.refresh_risk_at_slot(clock.slot)?;
        }
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let reserve_balance = match asset {
                MarketAsset::Base => next_balances.base_reserve_vault_balance,
                MarketAsset::Quote => next_balances.quote_reserve_vault_balance,
            };
            let interest_balance = match asset {
                MarketAsset::Base => next_balances.base_interest_vault_balance,
                MarketAsset::Quote => next_balances.quote_interest_vault_balance,
            };
            require_gte!(
                reserve_balance,
                required_reserve_custody(next_market.side(asset))?,
                ErrorCode::UnbackedFeeLiability
            );
            require_gte!(
                interest_balance,
                next_market.side(asset).fees.interest_vault_balance,
                ErrorCode::UnbackedFeeLiability
            );
            require_gte!(
                liquidation_insurance_balance(&next_balances, asset),
                next_market.insurance.available(asset),
                ErrorCode::InsufficientInsurance
            );
        }

        let mut cash = BenchmarkCashFlow::default();
        let debt_cash = cash.side_mut(preview.plan.debt_asset);
        debt_cash.reserve_vault_credit = request
            .debt_transfer
            .destination_credit
            .checked_add(request.insurance_draw_transfer.destination_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        debt_cash.reserve_vault_debit = request
            .interest_transfer
            .source_debit
            .checked_add(request.owner_residual_transfer.source_debit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        debt_cash.interest_vault_credit = request.interest_transfer.destination_credit;
        debt_cash.insurance_vault_debit = request.insurance_draw_transfer.source_debit;
        debt_cash.recipient_credit = request.owner_residual_transfer.destination_credit;
        let collateral_cash = cash.side_mut(preview.plan.debt_asset.opposite());
        collateral_cash.reserve_vault_credit = request.collateral_swap_transfer.destination_credit;
        collateral_cash.collateral_vault_debit = preview.native.collateral_seized;
        collateral_cash.insurance_vault_credit = request.insurance_funding_transfer.destination_credit;
        collateral_cash.recipient_credit = request.collateral_transfer.destination_credit;

        let position_after = borrow_position_checkpoint(&next_position, &next_market.debt)?;
        let receipt = BenchmarkLiquidationExecutionReceipt {
            plan: preview.plan,
            native: preview.native,
            debt_transfer: request.debt_transfer,
            insurance_draw_transfer: request.insurance_draw_transfer,
            interest_transfer: request.interest_transfer,
            collateral_transfer: request.collateral_transfer,
            collateral_swap_transfer: request.collateral_swap_transfer,
            owner_residual_transfer: request.owner_residual_transfer,
            insurance_funding_transfer: request.insurance_funding_transfer,
        };
        let market = self.commit_market_execution(next_market, clock, revenue_before, cash, receipt)?;
        *position.position = next_position;
        *token_balances = next_balances;
        Ok(BenchmarkLiquidationExecution {
            market,
            position_after,
            token_balances_after: next_balances,
        })
    }

    fn require_market_key(&self) -> Result<Pubkey> {
        self.market_key.ok_or_else(|| ErrorCode::InvalidPositionMarket.into())
    }

    fn require_monotonic_clock(&self, clock: BenchmarkClock) -> Result<()> {
        require_gte!(clock.slot, self.clock.slot, ErrorCode::InvalidArgument);
        Ok(())
    }

    fn assert_position_market(&self, position: &BenchmarkBorrowPosition) -> Result<()> {
        position
            .position
            .assert_position(position.position.owner, self.require_market_key()?)
    }

    fn commit_market_execution<T>(
        &mut self,
        next: Market,
        clock: BenchmarkClock,
        revenue_before: BenchmarkRevenueCheckpoint,
        cash: BenchmarkCashFlow,
        receipt: T,
    ) -> Result<BenchmarkMarketExecution<T>> {
        let revenue_after = revenue_checkpoint(&next);
        let market_after = market_checkpoint(&next, self.market_key, clock)?;
        *self.market = next;
        self.clock = clock;
        Ok(BenchmarkMarketExecution {
            receipt,
            cash,
            revenue_before,
            revenue_after,
            market_after,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_position_execution<T>(
        &mut self,
        position: &mut BenchmarkBorrowPosition,
        next_market: Market,
        next_position: BorrowPosition,
        clock: BenchmarkClock,
        revenue_before: BenchmarkRevenueCheckpoint,
        cash: BenchmarkCashFlow,
        receipt: T,
    ) -> Result<BenchmarkPositionExecution<T>> {
        let position_after = borrow_position_checkpoint(&next_position, &next_market.debt)?;
        let market = self.commit_market_execution(next_market, clock, revenue_before, cash, receipt)?;
        *position.position = next_position;
        Ok(BenchmarkPositionExecution { market, position_after })
    }
}

fn first_liquidation_floor_timestamp(start_time: i64, start_price_nad: u64, floor_price_nad: u64) -> Result<i64> {
    require!(start_time > 0, ErrorCode::InvalidArgument);
    require!(
        start_price_nad >= floor_price_nad && floor_price_nad > 0,
        ErrorCode::InvalidSettlementPrice
    );
    start_time
        .checked_add(LIQUIDATION_AUCTION_DURATION_SECONDS)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn liquidation_plan(
    market: &mut Market,
    position: &mut BorrowPosition,
    request: BenchmarkLiquidationPlanRequest,
) -> Result<BenchmarkLiquidationPlan> {
    if request.phase == BenchmarkLiquidationPhase::Floor {
        return prepare_floor_liquidation_plan(market, position, request).map(|(plan, _)| plan);
    }
    require!(request.max_repay_credit > 0, ErrorCode::AmountZero);
    require_eq!(request.collateral_reserve_credit, 0, ErrorCode::InvalidArgument);
    market.reconcile_liquidation_auction(position)?;
    position.assert_liquidation_auction(request.debt_asset)?;
    require!(
        !position.liquidation_auction_expired(request.clock.unix_timestamp)?,
        ErrorCode::PositionNotLiquidatable
    );
    let price_before_fee = position.liquidation_auction_price_nad(request.clock.unix_timestamp)?;
    let reservation_fee = price_before_fee
        .checked_mul(BENCHMARK_LIQUIDATION_RESERVATION_FEE_BPS)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u64))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let auction_price_nad = price_before_fee
        .checked_add(reservation_fee)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: auction_price_nad,
    };
    let terms = market.liquidation_terms_with_pricing(position, request.debt_asset, pricing)?;
    let repay_credit = market
        .fixed_repayment_for_max(position, request.debt_asset, request.max_repay_credit)?
        .cash_repaid;
    require_gte!(
        terms.max_repay_amount,
        repay_credit,
        ErrorCode::LiquidationRepayTooLarge
    );
    Ok(BenchmarkLiquidationPlan {
        phase: BenchmarkLiquidationPhase::Bid,
        debt_asset: request.debt_asset,
        auction_price_nad,
        first_floor_unix_timestamp: first_liquidation_floor_timestamp(
            position.auction_start_time,
            position.auction_start_price_nad,
            position.auction_floor_price_nad,
        )?,
        terms,
        repay_credit,
        collateral_consumed: 0,
        caller_bounty: 0,
        collateral_swap_debit: 0,
        collateral_reserve_credit: 0,
        swap_output: 0,
        insurance_draw_debit: 0,
    })
}

fn prepare_floor_liquidation_plan(
    market: &mut Market,
    position: &mut BorrowPosition,
    request: BenchmarkLiquidationPlanRequest,
) -> Result<(BenchmarkLiquidationPlan, Option<PreparedSwap>)> {
    require!(
        request.phase == BenchmarkLiquidationPhase::Floor,
        ErrorCode::InvalidArgument
    );
    require_eq!(request.max_repay_credit, 0, ErrorCode::InvalidArgument);
    require!(
        request.protocol_auction_split.is_valid(),
        ErrorCode::InvalidAuctionConfig
    );
    market.reconcile_liquidation_auction(position)?;
    position.assert_liquidation_auction(request.debt_asset)?;
    require!(
        position.liquidation_auction_expired(request.clock.unix_timestamp)?,
        ErrorCode::PositionNotLiquidatable
    );
    let collateral_asset = request.debt_asset.opposite();
    let collateral_consumed = position.collateral(collateral_asset);
    require!(collateral_consumed > 0, ErrorCode::InsufficientBalance);
    let caller_bounty = u64::try_from(
        (collateral_consumed as u128)
            .checked_mul(LIQUIDATION_BACKSTOP_CALLER_BPS as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let collateral_swap_debit = collateral_consumed
        .checked_sub(caller_bounty)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require_gte!(
        collateral_swap_debit,
        request.collateral_reserve_credit,
        ErrorCode::BrokenInvariant
    );

    let prepared = if request.collateral_reserve_credit > 0 {
        Some(
            SwapRequest {
                current_slot: request.clock.slot,
                current_unix_timestamp: request.clock.unix_timestamp,
                asset_in: collateral_asset,
                reserve_credit: request.collateral_reserve_credit,
                protocol_fee_bps: request.protocol_swap_fee_bps,
            }
            .prepare_with_cash_policy(
                market,
                SwapCashPolicy::Liquidate {
                    debt_asset: request.debt_asset,
                    debt_shares: 0,
                    debt_principal: 0,
                },
            )?,
        )
    } else {
        None
    };
    let swap_output = prepared.as_ref().map(|swap| swap.quote.amount_out).unwrap_or(0);
    let full_repayment = market
        .fixed_repayment_for_max(position, request.debt_asset, u64::MAX)?
        .cash_repaid;
    let repay_credit = swap_output.min(full_repayment);
    let remaining_debt = full_repayment
        .checked_sub(repay_credit)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let insurance_draw_debit = if remaining_debt == 0 {
        0
    } else {
        market
            .insurance
            .draw_capacity(request.debt_asset, request.clock.slot)?
            .min(remaining_debt)
    };
    let plan = BenchmarkLiquidationPlan {
        phase: BenchmarkLiquidationPhase::Floor,
        debt_asset: request.debt_asset,
        auction_price_nad: position.auction_floor_price_nad,
        first_floor_unix_timestamp: first_liquidation_floor_timestamp(
            position.auction_start_time,
            position.auction_start_price_nad,
            position.auction_floor_price_nad,
        )?,
        terms: LiquidationTerms {
            liquidation_incentive_bps: LIQUIDATION_BACKSTOP_CALLER_BPS,
            insurance_funding_bps: 0,
            total_penalty_bps: LIQUIDATION_BACKSTOP_CALLER_BPS,
            max_repay_amount: full_repayment,
        },
        repay_credit,
        collateral_consumed,
        caller_bounty,
        collateral_swap_debit,
        collateral_reserve_credit: request.collateral_reserve_credit,
        swap_output,
        insurance_draw_debit,
    };
    Ok((plan, prepared))
}

fn preview_liquidation_on_state(
    market: &mut Market,
    position: &mut BorrowPosition,
    request: BenchmarkLiquidationPreviewRequest,
) -> Result<BenchmarkLiquidationPreview> {
    if request.plan.phase == BenchmarkLiquidationPhase::Floor {
        let (plan, prepared) = prepare_floor_liquidation_plan(market, position, request.plan)?;
        require_gte!(
            plan.insurance_draw_debit,
            request.insurance_draw_credit,
            ErrorCode::MarketMathOverflow
        );
        if let Some(prepared) = prepared {
            let finalized = prepared.finalize_lending_liquidation_state(
                market,
                request.plan.clock.slot,
                request.plan.protocol_swap_fee_bps,
                request.plan.protocol_auction_split,
            )?;
            require!(
                !rebalance_executes_token_changes(&finalized.base_rebalance)
                    && !rebalance_executes_token_changes(&finalized.quote_rebalance),
                ErrorCode::InvalidArgument
            );
        }
        let internal = market.settle_internal_liquidation(
            position,
            plan.debt_asset,
            plan.swap_output,
            plan.insurance_draw_debit,
            request.insurance_draw_credit,
            plan.collateral_consumed,
            plan.caller_bounty,
        )?;
        if internal.liquidation.socialized_loss > 0 {
            market.finalize_amm_socialized_loss_and_observe_risk(request.plan.clock.slot)?;
        } else {
            market.finalize_amm_transition(request.plan.clock.slot)?;
            market.refresh_risk_at_slot(request.plan.clock.slot)?;
        }
        return Ok(BenchmarkLiquidationPreview {
            plan,
            native: internal.liquidation,
            owner_residual: internal.owner_residual,
        });
    }
    let plan = liquidation_plan(market, position, request.plan)?;
    require_gte!(
        plan.insurance_draw_debit,
        request.insurance_draw_credit,
        ErrorCode::MarketMathOverflow
    );
    require_eq!(request.insurance_draw_credit, 0, ErrorCode::InvalidArgument);
    let native = market.settle_liquidation(
        position,
        plan.debt_asset,
        plan.repay_credit,
        plan.insurance_draw_debit,
        request.insurance_draw_credit,
        0,
        plan.terms,
        LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad: plan.auction_price_nad,
        },
    )?;
    Ok(BenchmarkLiquidationPreview {
        plan,
        native,
        owner_residual: 0,
    })
}

fn liquidation_liquidator_balance_mut(
    balances: &mut BenchmarkLiquidationTokenBalances,
    asset: MarketAsset,
) -> &mut u64 {
    match asset {
        MarketAsset::Base => &mut balances.liquidator_base_balance,
        MarketAsset::Quote => &mut balances.liquidator_quote_balance,
    }
}

fn liquidation_reserve_balance_mut(balances: &mut BenchmarkLiquidationTokenBalances, asset: MarketAsset) -> &mut u64 {
    match asset {
        MarketAsset::Base => &mut balances.base_reserve_vault_balance,
        MarketAsset::Quote => &mut balances.quote_reserve_vault_balance,
    }
}

fn liquidation_collateral_balance_mut(
    balances: &mut BenchmarkLiquidationTokenBalances,
    asset: MarketAsset,
) -> &mut u64 {
    match asset {
        MarketAsset::Base => &mut balances.base_collateral_vault_balance,
        MarketAsset::Quote => &mut balances.quote_collateral_vault_balance,
    }
}

fn liquidation_insurance_balance(balances: &BenchmarkLiquidationTokenBalances, asset: MarketAsset) -> u64 {
    match asset {
        MarketAsset::Base => balances.base_insurance_vault_balance,
        MarketAsset::Quote => balances.quote_insurance_vault_balance,
    }
}

fn liquidation_insurance_balance_mut(balances: &mut BenchmarkLiquidationTokenBalances, asset: MarketAsset) -> &mut u64 {
    match asset {
        MarketAsset::Base => &mut balances.base_insurance_vault_balance,
        MarketAsset::Quote => &mut balances.quote_insurance_vault_balance,
    }
}

fn debit_balance(balance: &mut u64, amount: u64) -> Result<()> {
    *balance = balance.checked_sub(amount).ok_or(ErrorCode::InsufficientBalance)?;
    Ok(())
}

fn credit_balance(balance: &mut u64, amount: u64) -> Result<()> {
    *balance = balance.checked_add(amount).ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(())
}

fn prepare_hlp_entry_market(
    market: &mut Market,
    target_asset: MarketAsset,
    live_hlp_mint_supply: u64,
    clock: BenchmarkClock,
    global_reduce_only: bool,
) -> Result<()> {
    market.assert_current_version()?;
    market.assert_started_at(clock.unix_timestamp)?;
    require!(!global_reduce_only && !market.reduce_only, ErrorCode::ReduceOnlyMode);
    market.accrue_interest_to_slot(clock.slot)?;
    reconcile_live_hlp_supply(market, target_asset, live_hlp_mint_supply)?;
    if market.base_side.reserves.live_reserve > 0 && market.quote_side.reserves.live_reserve > 0 {
        market.advance_amm_clock(clock.slot)?;
        market.checkpoint_hlp_vaults()?;
        market.assert_hlp_entry_available(target_asset)?;
        market.observe_current_risk(clock.slot)?;
    }
    Ok(())
}

fn apply_hlp_entry_after_prepare(
    market: &mut Market,
    external: &mut BenchmarkHlpOwnedState,
    request: BenchmarkHlpEntryRequest,
) -> Result<(SingleSidedLiquidityReceipt, BenchmarkCashFlow)> {
    require!(request.target_transfer.source_debit > 0, ErrorCode::AmountZero);
    require!(request.target_transfer.destination_credit > 0, ErrorCode::AmountZero);
    let native = market.deposit_single_sided(
        external.target_asset,
        request.target_transfer.destination_credit,
        request.min_hlp_amount,
    )?;
    market.finalize_amm_transition_and_observe_risk(request.clock.slot)?;

    let (base_swap_growth, base_interest_growth) =
        market.hlp_yield_growth_indexes(external.target_asset, MarketAsset::Base);
    let (quote_swap_growth, quote_interest_growth) =
        market.hlp_yield_growth_indexes(external.target_asset, MarketAsset::Quote);
    external.base_yield_account.accrue(
        external.holder_hlp_token_balance,
        base_swap_growth,
        base_interest_growth,
    )?;
    external.quote_yield_account.accrue(
        external.holder_hlp_token_balance,
        quote_swap_growth,
        quote_interest_growth,
    )?;
    external.hlp_vault_ylp_balance = external
        .hlp_vault_ylp_balance
        .checked_add(native.ylp_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    external.hlp_mint_supply = external
        .hlp_mint_supply
        .checked_add(native.hlp_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    external.holder_hlp_token_balance = external
        .holder_hlp_token_balance
        .checked_add(native.hlp_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;

    let mut cash = BenchmarkCashFlow::default();
    cash.side_mut(external.target_asset).reserve_vault_credit = native.deposit_amount;
    Ok((native, cash))
}

fn prepare_hlp_withdraw_market(
    market: &mut Market,
    target_asset: MarketAsset,
    live_hlp_mint_supply: u64,
    clock: BenchmarkClock,
) -> Result<()> {
    market.assert_current_version()?;
    market.assert_started_at(clock.unix_timestamp)?;
    market.accrue_interest_to_slot(clock.slot)?;
    reconcile_live_hlp_supply(market, target_asset, live_hlp_mint_supply)?;
    if market.hlp_terminally_closed(target_asset) {
        market.advance_amm_clock(clock.slot)?;
    } else if market.base_side.reserves.live_reserve > 0 && market.quote_side.reserves.live_reserve > 0 {
        market.advance_amm_clock(clock.slot)?;
        market.checkpoint_hlp_vaults()?;
        market.refresh_risk_at_slot(clock.slot)?;
    }
    Ok(())
}

fn hlp_borrowed_amount(market: &Market, target_asset: MarketAsset, target_amount: u64) -> Result<u64> {
    let target_reserve = market.curve_reserve(target_asset)?;
    let opposite_reserve = market.curve_reserve(target_asset.opposite())?;
    require!(
        target_reserve > 0 && opposite_reserve > 0,
        ErrorCode::InsufficientLiquidity
    );
    u64::try_from(
        (target_amount as u128)
            .checked_mul(opposite_reserve as u128)
            .and_then(|value| value.checked_div(target_reserve as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn hlp_vault(market: &Market, target_asset: MarketAsset) -> &crate::state::HlpVault {
    match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    }
}

fn validate_hlp_owned_state(external: &BenchmarkHlpOwnedState, market: &Market, market_key: Pubkey) -> Result<()> {
    require_keys_eq!(
        external.hlp_mint,
        market.side(external.target_asset).hlp_mint,
        ErrorCode::InvalidMint
    );
    external.base_yield_account.assert_account(
        external.owner,
        market_key,
        external.hlp_mint,
        market.base_side.asset_mint,
        YieldTokenKind::Hlp,
    )?;
    external.quote_yield_account.assert_account(
        external.owner,
        market_key,
        external.hlp_mint,
        market.quote_side.asset_mint,
        YieldTokenKind::Hlp,
    )?;
    let (expected_base, expected_base_bump) = derive_hlp_yield_account(
        market_key,
        external.owner,
        external.hlp_mint,
        market.base_side.asset_mint,
    )?;
    let (expected_quote, expected_quote_bump) = derive_hlp_yield_account(
        market_key,
        external.owner,
        external.hlp_mint,
        market.quote_side.asset_mint,
    )?;
    require_keys_eq!(
        external.base_yield_account_key,
        expected_base,
        ErrorCode::InvalidYieldAccount
    );
    require_keys_eq!(
        external.quote_yield_account_key,
        expected_quote,
        ErrorCode::InvalidYieldAccount
    );
    require_eq!(
        external.base_yield_account.bump,
        expected_base_bump,
        ErrorCode::InvalidYieldAccount
    );
    require_eq!(
        external.quote_yield_account.bump,
        expected_quote_bump,
        ErrorCode::InvalidYieldAccount
    );
    let vault = hlp_vault(market, external.target_asset);
    require_eq!(
        external.hlp_mint_supply,
        vault.hlp_supply,
        ErrorCode::InvalidHlpMintSupply
    );
    require_eq!(
        external.hlp_vault_ylp_balance,
        vault.ylp_shares,
        ErrorCode::InvalidVault
    );
    require_gte!(
        external.hlp_mint_supply,
        external.holder_hlp_token_balance,
        ErrorCode::InsufficientBalance
    );
    Ok(())
}

fn derive_hlp_yield_account(
    market_key: Pubkey,
    owner: Pubkey,
    hlp_mint: Pubkey,
    asset_mint: Pubkey,
) -> Result<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[
            YIELD_ACCOUNT_SEED_PREFIX,
            market_key.as_ref(),
            owner.as_ref(),
            hlp_mint.as_ref(),
            asset_mint.as_ref(),
            &[YieldTokenKind::Hlp.code()],
        ],
        &crate::ID,
    )
    .ok_or_else(|| ErrorCode::InvalidYieldAccount.into())
}

fn validate_referral_owned_state(
    referral: &BenchmarkReferralOwnedState,
    market_key: Pubkey,
    asset_mint: Pubkey,
) -> Result<()> {
    let (expected_partner, partner_bump) = Pubkey::try_find_program_address(
        &[REFERRAL_PARTNER_SEED_PREFIX, referral.partner.authority.as_ref()],
        &crate::ID,
    )
    .ok_or(ErrorCode::InvalidReferralPartner)?;
    require_keys_eq!(
        referral.partner_key,
        expected_partner,
        ErrorCode::InvalidReferralPartner
    );
    require_eq!(referral.partner.bump, partner_bump, ErrorCode::InvalidReferralPartner);
    require_gte!(
        MAX_REFERRAL_INTEREST_SHARE_BPS,
        referral.partner.interest_share_bps,
        ErrorCode::InvalidReferralInterestShareBps
    );
    let (expected_accrual, accrual_bump) = Pubkey::try_find_program_address(
        &[
            REFERRAL_ACCRUAL_SEED_PREFIX,
            referral.partner_key.as_ref(),
            market_key.as_ref(),
            asset_mint.as_ref(),
        ],
        &crate::ID,
    )
    .ok_or(ErrorCode::InvalidReferralAccrual)?;
    require_keys_eq!(
        referral.accrual_key,
        expected_accrual,
        ErrorCode::InvalidReferralAccrual
    );
    require_eq!(referral.accrual.bump, accrual_bump, ErrorCode::InvalidReferralAccrual);
    require_keys_eq!(
        referral.accrual.referral_partner,
        referral.partner_key,
        ErrorCode::InvalidReferralAccrual
    );
    require_keys_eq!(referral.accrual.market, market_key, ErrorCode::InvalidReferralAccrual);
    require_keys_eq!(
        referral.accrual.asset_mint,
        asset_mint,
        ErrorCode::InvalidReferralAccrual
    );
    Ok(())
}

fn empty_leverage_position() -> LeveragePosition {
    LeveragePosition {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        position_id: Pubkey::default(),
        referral_partner: Pubkey::default(),
        referral_interest_share_bps: 0,
        debt_asset: 0,
        collateral_amount: 0,
        margin_amount: 0,
        open_notional: 0,
        debt_principal: 0,
        debt_shares: 0,
        multiplier_bps: 0,
        opened_at: 0,
        opened_slot: 0,
        bump: 0,
    }
}

fn clone_leverage_position(position: &LeveragePosition) -> Result<LeveragePosition> {
    let mut bytes = Vec::new();
    position.try_serialize(&mut bytes)?;
    let mut input = bytes.as_slice();
    LeveragePosition::try_deserialize(&mut input)
}

fn serialize_market(market: &Market) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    market.try_serialize(&mut bytes)?;
    Ok(bytes)
}

fn validate_leverage_policy(
    market: &Market,
    price_moving_asset: MarketAsset,
    clock: BenchmarkClock,
    policy: BenchmarkLeveragePolicy,
    risk_increasing: bool,
) -> Result<()> {
    market.assert_current_version()?;
    market.assert_started_at(clock.unix_timestamp)?;
    require_gte!(
        BPS_DENOMINATOR,
        policy.protocol_swap_fee_bps,
        ErrorCode::InvalidMarketConfig
    );
    require_gte!(
        BPS_DENOMINATOR,
        policy.protocol_interest_fee_bps,
        ErrorCode::InvalidInterestFeeBps
    );
    require!(
        policy.protocol_auction_split.is_valid(),
        ErrorCode::InvalidAuctionConfig
    );
    require_gte!(
        MAX_REFERRAL_INTEREST_SHARE_BPS,
        policy.max_referral_interest_share_bps,
        ErrorCode::InvalidReferralInterestShareBps
    );
    require!(
        policy.collateral_mint_is_leverage_eligible,
        ErrorCode::InvalidLeverageCollateralMint
    );
    if market
        .config
        .launch_rate_limit_active_for_swap(price_moving_asset, clock.unix_timestamp)
    {
        require!(
            policy.launch_same_transaction_guard_satisfied,
            ErrorCode::LaunchRateLimitSplitTransaction
        );
    }
    if risk_increasing {
        require!(
            !policy.global_reduce_only && !market.reduce_only,
            ErrorCode::ReduceOnlyMode
        );
    }
    Ok(())
}

fn validate_leverage_hlp_pair(
    market: &BenchmarkMarket,
    base_hlp: &BenchmarkHlpOwnedState,
    quote_hlp: &BenchmarkHlpOwnedState,
) -> Result<()> {
    require!(base_hlp.target_asset == MarketAsset::Base, ErrorCode::InvalidMint);
    require!(quote_hlp.target_asset == MarketAsset::Quote, ErrorCode::InvalidMint);
    base_hlp.validate(market)?;
    quote_hlp.validate(market)
}

#[allow(clippy::too_many_arguments)]
fn require_prepared_identity(
    market: &BenchmarkMarket,
    leverage: &BenchmarkLeverageOwnedState,
    base_hlp: &BenchmarkHlpOwnedState,
    quote_hlp: &BenchmarkHlpOwnedState,
    clock_before: BenchmarkClock,
    market_before: &[u8],
    leverage_before: BenchmarkLeverageOwnedCheckpoint,
    base_hlp_before: BenchmarkHlpOwnedCheckpoint,
    quote_hlp_before: BenchmarkHlpOwnedCheckpoint,
) -> Result<()> {
    require!(market.clock == clock_before, ErrorCode::BrokenInvariant);
    require!(
        serialize_market(&market.market)?.as_slice() == market_before,
        ErrorCode::BrokenInvariant
    );
    require!(leverage.checkpoint() == leverage_before, ErrorCode::BrokenInvariant);
    require!(base_hlp.checkpoint() == base_hlp_before, ErrorCode::BrokenInvariant);
    require!(quote_hlp.checkpoint() == quote_hlp_before, ErrorCode::BrokenInvariant);
    Ok(())
}

fn prepare_benchmark_leverage_swap(
    market: &mut Market,
    request: SwapRequest,
    cash_policy: SwapCashPolicy,
) -> Result<PreparedLeverageSwap> {
    let current_slot = request.current_slot;
    let PreparedSwap {
        quote,
        base_pre_rebalance,
        quote_pre_rebalance,
        fee_eligible_ylp_supply,
        interest_eligibility,
        cash_policy,
        concentrated_transition,
    } = request.prepare_with_cash_policy(market, cash_policy)?;
    market.observe_current_risk(current_slot)?;
    Ok(PreparedLeverageSwap {
        swap: LeverageSwapQuote::from_amm(quote, current_slot),
        base_pre_rebalance,
        quote_pre_rebalance,
        fee_eligible_ylp_supply,
        interest_eligibility,
        cash_policy,
        concentrated_transition,
    })
}

fn full_leverage_swap_fee_credit(quote: LeverageSwapQuote) -> Result<LeverageSwapFeeCredit> {
    require_eq!(
        quote.fee_breakdown.claimable_fee_debit,
        quote.fee_credit,
        ErrorCode::BrokenInvariant
    );
    LeverageSwapFeeCredit::from_total_actual_credit(&quote, quote.fee_credit)
}

fn leverage_token_balance(balances: &BenchmarkLeverageTokenBalances, asset: MarketAsset) -> u64 {
    match asset {
        MarketAsset::Base => balances.owner_base_balance,
        MarketAsset::Quote => balances.owner_quote_balance,
    }
}

fn leverage_token_balance_mut(balances: &mut BenchmarkLeverageTokenBalances, asset: MarketAsset) -> &mut u64 {
    match asset {
        MarketAsset::Base => &mut balances.owner_base_balance,
        MarketAsset::Quote => &mut balances.owner_quote_balance,
    }
}

fn leverage_reserve_vault_balance(balances: &BenchmarkLeverageTokenBalances, asset: MarketAsset) -> u64 {
    match asset {
        MarketAsset::Base => balances.base_reserve_vault_balance,
        MarketAsset::Quote => balances.quote_reserve_vault_balance,
    }
}

fn leverage_reserve_vault_balance_mut(balances: &mut BenchmarkLeverageTokenBalances, asset: MarketAsset) -> &mut u64 {
    match asset {
        MarketAsset::Base => &mut balances.base_reserve_vault_balance,
        MarketAsset::Quote => &mut balances.quote_reserve_vault_balance,
    }
}

fn leverage_interest_vault_balance(balances: &BenchmarkLeverageTokenBalances, asset: MarketAsset) -> u64 {
    match asset {
        MarketAsset::Base => balances.base_interest_vault_balance,
        MarketAsset::Quote => balances.quote_interest_vault_balance,
    }
}

fn leverage_interest_vault_balance_mut(balances: &mut BenchmarkLeverageTokenBalances, asset: MarketAsset) -> &mut u64 {
    match asset {
        MarketAsset::Base => &mut balances.base_interest_vault_balance,
        MarketAsset::Quote => &mut balances.quote_interest_vault_balance,
    }
}

fn leverage_collateral_vault_balance(balances: &BenchmarkLeverageTokenBalances, asset: MarketAsset) -> u64 {
    match asset {
        MarketAsset::Base => balances.base_leverage_collateral_vault_balance,
        MarketAsset::Quote => balances.quote_leverage_collateral_vault_balance,
    }
}

fn leverage_collateral_vault_balance_mut(
    balances: &mut BenchmarkLeverageTokenBalances,
    asset: MarketAsset,
) -> &mut u64 {
    match asset {
        MarketAsset::Base => &mut balances.base_leverage_collateral_vault_balance,
        MarketAsset::Quote => &mut balances.quote_leverage_collateral_vault_balance,
    }
}

fn required_reserve_custody(side: &MarketSide) -> Result<u64> {
    let hlp_backing_inventory = side.reserves.total_hlp_backing_inventory()?;
    side.reserves
        .cash_reserve
        .checked_add(side.fees.swap_fee_custody_balance)
        .and_then(|value| value.checked_add(hlp_backing_inventory))
        .and_then(|value| value.checked_add(side.reserves.protected_recenter_reserve))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn validate_leverage_token_custody(market: &Market, balances: &BenchmarkLeverageTokenBalances) -> Result<()> {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        require_gte!(
            leverage_reserve_vault_balance(balances, asset),
            required_reserve_custody(market.side(asset))?,
            ErrorCode::UnbackedFeeLiability
        );
        require_gte!(
            leverage_interest_vault_balance(balances, asset),
            market.side(asset).fees.interest_vault_balance,
            ErrorCode::UnbackedFeeLiability
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_leverage_hlp_settlement(
    market: &mut Market,
    external: &mut BenchmarkHlpOwnedState,
    receipt: HlpRebalanceReceipt,
    interest_eligibility: HlpYieldEligibility,
    interest_transfer: BenchmarkTokenTransferOutcome,
    policy: BenchmarkLeveragePolicy,
    balances: &mut BenchmarkLeverageTokenBalances,
    cash: &mut BenchmarkCashFlow,
) -> Result<()> {
    let borrowed_asset = external.target_asset.opposite();
    *leverage_reserve_vault_balance_mut(balances, borrowed_asset) =
        leverage_reserve_vault_balance(balances, borrowed_asset)
            .checked_sub(interest_transfer.source_debit)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
    *leverage_interest_vault_balance_mut(balances, borrowed_asset) =
        leverage_interest_vault_balance(balances, borrowed_asset)
            .checked_add(interest_transfer.destination_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    apply_hlp_rebalance_settlement(
        market,
        external,
        receipt,
        interest_eligibility,
        interest_transfer,
        policy.protocol_interest_fee_bps,
        policy.protocol_auction_split,
        cash,
    )
}

fn leverage_metrics(
    market: &Market,
    position: &LeveragePosition,
    clock: BenchmarkClock,
) -> Result<BenchmarkLeverageMetrics> {
    position.require_open()?;
    let debt_asset = position.debt_asset()?;
    let collateral_asset = debt_asset.opposite();
    let debt_amount = position.debt_amount(&market.debt)?;
    let closeout_quote = market.quote_leverage_swap_at_time(
        collateral_asset,
        position.collateral_amount,
        clock.slot,
        clock.unix_timestamp,
    )?;
    let closeout_value = closeout_quote.amount_out;
    let equity = closeout_value.saturating_sub(debt_amount);
    let equity_bps = if closeout_value == 0 {
        0
    } else {
        (equity as u128)
            .checked_mul(BPS_DENOMINATOR as u128)
            .and_then(|value| value.checked_div(closeout_value as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?
    };
    let collateral_nad = normalize_to_nad(
        position.collateral_amount as u128,
        market.side(collateral_asset).asset_decimals,
    )?;
    let spot_value_nad = match collateral_asset {
        MarketAsset::Base => collateral_nad
            .checked_mul(closeout_quote.start_price_nad as u128)
            .and_then(|value| value.checked_div(NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
        MarketAsset::Quote => collateral_nad
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(closeout_quote.start_price_nad as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
    };
    let spot_value = denormalize_from_nad_floor(spot_value_nad, market.side(debt_asset).asset_decimals)?;
    let unwind_impact_bps = if closeout_value >= spot_value || spot_value == 0 {
        0
    } else {
        (spot_value - closeout_value) as u128 * BPS_DENOMINATOR as u128 / spot_value as u128
    };
    let healthy = |candidate: u64| -> Result<bool> {
        if candidate <= debt_amount || candidate == 0 {
            return Ok(false);
        }
        let bps = (candidate.saturating_sub(debt_amount) as u128)
            .checked_mul(BPS_DENOMINATOR as u128)
            .and_then(|value| value.checked_div(candidate as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(bps > LEVERAGE_MAINTENANCE_BUFFER_BPS as u128)
    };
    require!(healthy(u64::MAX)?, ErrorCode::DebtMathOverflow);
    let mut low = debt_amount.saturating_add(1);
    let mut high = u64::MAX;
    while low < high {
        let middle = low + (high - low) / 2;
        if healthy(middle)? {
            high = middle;
        } else {
            low = middle.checked_add(1).ok_or(ErrorCode::DebtMathOverflow)?;
        }
    }
    let minimum_healthy_closeout_value = low;
    Ok(BenchmarkLeverageMetrics {
        debt_asset,
        debt_amount,
        collateral_amount: position.collateral_amount,
        closeout_value,
        equity,
        equity_bps,
        initial_margin_bps: LEVERAGE_INITIAL_MARGIN_BPS,
        maintenance_margin_bps: LEVERAGE_MAINTENANCE_BUFFER_BPS,
        minimum_healthy_closeout_value,
        maintenance_shortfall: minimum_healthy_closeout_value.saturating_sub(closeout_value),
        spot_value,
        unwind_impact_bps,
        maximum_open_unwind_impact_bps: LEVERAGE_MAX_UNWIND_IMPACT_BPS,
        liquidatable: closeout_value <= debt_amount || equity_bps <= LEVERAGE_MAINTENANCE_BUFFER_BPS as u128,
    })
}

fn validate_leverage_post_state(
    market: &Market,
    market_key: Pubkey,
    leverage: &BenchmarkLeverageOwnedState,
    base_hlp: &BenchmarkHlpOwnedState,
    quote_hlp: &BenchmarkHlpOwnedState,
) -> Result<()> {
    validate_leverage_token_custody(market, &leverage.token_balances)?;
    validate_hlp_owned_state(base_hlp, market, market_key)?;
    validate_hlp_owned_state(quote_hlp, market, market_key)?;
    if leverage.position_exists {
        leverage
            .position
            .assert_position(leverage.position_owner, market_key, leverage.position.debt_asset()?)?;
        leverage.position.require_open()?;
    } else {
        require!(!leverage.position.is_initialized(), ErrorCode::InvalidLeveragePosition);
    }
    market.assert_market_invariants()
}

fn empty_yield_account() -> YieldAccount {
    YieldAccount {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        lp_mint: Pubkey::default(),
        asset_mint: Pubkey::default(),
        token_kind: YieldTokenKind::Ylp.code(),
        recipient: Pubkey::default(),
        swap_fee_checkpoint_q64: 0,
        interest_checkpoint_q64: 0,
        accrued_swap_fee_amount: 0,
        accrued_interest_amount: 0,
        swap_fee_remainder_q64: 0,
        interest_remainder_q64: 0,
        bump: 0,
    }
}

fn clone_yield_account(account: &YieldAccount) -> Result<YieldAccount> {
    let mut bytes = Vec::new();
    account.try_serialize(&mut bytes)?;
    let mut input = bytes.as_slice();
    YieldAccount::try_deserialize(&mut input)
}

fn yield_account_checkpoint(account_key: Pubkey, account: &YieldAccount) -> BenchmarkYieldAccountCheckpoint {
    BenchmarkYieldAccountCheckpoint {
        account_key,
        owner: account.owner,
        market: account.market,
        lp_mint: account.lp_mint,
        asset_mint: account.asset_mint,
        token_kind: account.token_kind,
        recipient: account.recipient,
        swap_fee_checkpoint_q64: account.swap_fee_checkpoint_q64,
        interest_checkpoint_q64: account.interest_checkpoint_q64,
        accrued_swap_fee_amount: account.accrued_swap_fee_amount,
        accrued_interest_amount: account.accrued_interest_amount,
        swap_fee_remainder_q64: account.swap_fee_remainder_q64,
        interest_remainder_q64: account.interest_remainder_q64,
        bump: account.bump,
    }
}

fn clone_borrow_position(position: &BorrowPosition) -> Result<BorrowPosition> {
    let mut bytes = Vec::new();
    position.try_serialize(&mut bytes)?;
    let mut input = bytes.as_slice();
    BorrowPosition::try_deserialize(&mut input)
}

fn clone_market(market: &Market) -> Result<Market> {
    let mut bytes = Vec::new();
    market.try_serialize(&mut bytes)?;
    let mut input = bytes.as_slice();
    Market::try_deserialize(&mut input)
}

fn carry_forward_all_ylp_revenue(market: &mut Market) -> Result<()> {
    market.base_side.carry_forward_swap_fees()?;
    market.base_side.carry_forward_interest()?;
    market.quote_side.carry_forward_swap_fees()?;
    market.quote_side.carry_forward_interest()?;
    Ok(())
}

fn referral_binding(position: &BorrowPosition, debt_asset: MarketAsset) -> (Pubkey, u16) {
    (
        position.referral_partner(debt_asset),
        position.referral_interest_share_bps(debt_asset),
    )
}

fn referral_interest_amount(
    binding: (Pubkey, u16),
    interest_paid: u64,
    interest_vault_credit: u64,
    protocol_interest_fee_bps: u16,
) -> Result<u64> {
    let interest_share_bps = if binding.0 == Pubkey::default() {
        require_eq!(binding.1, 0, ErrorCode::BrokenInvariant);
        None
    } else {
        Some(binding.1)
    };
    Ok(ReferralInterestQuote::new(
        interest_paid,
        interest_vault_credit,
        protocol_interest_fee_bps,
        interest_share_bps,
    )?
    .referral_amount)
}

fn revenue_side_checkpoint(side: &MarketSide) -> BenchmarkRevenueSideCheckpoint {
    BenchmarkRevenueSideCheckpoint {
        swap_fee_growth_index_q64: side.fees.swap_fee_growth_index_q64,
        interest_growth_index_q64: side.fees.interest_growth_index_q64,
        swap_fee_growth_remainder_scaled: side.fees.swap_fee_growth_remainder_scaled,
        interest_growth_remainder_scaled: side.fees.interest_growth_remainder_scaled,
        hlp_funding_interest_growth_remainder_scaled: side.fees.hlp_funding_interest_growth_remainder_scaled,
        lp_swap_fee_liability: side.fees.swap_fee_liability,
        lp_interest_liability: side.fees.interest_liability,
        unallocated_lp_swap_fee_liability: side.fees.unallocated_swap_fee_liability,
        unallocated_lp_interest_liability: side.fees.unallocated_interest_liability,
        protocol_swap_fee_liability: side.fees.swap_protocol_fee_liability,
        protocol_interest_fee_liability: side.fees.interest_protocol_fee_liability,
        buyback_swap_fee_liability: side.fees.swap_buyback_fee_liability,
        buyback_interest_fee_liability: side.fees.interest_buyback_fee_liability,
        referral_interest_liability: side.fees.referral_interest_liability,
        swap_fee_custody_balance: side.fees.swap_fee_custody_balance,
        interest_vault_balance: side.fees.interest_vault_balance,
    }
}

fn revenue_checkpoint(market: &Market) -> BenchmarkRevenueCheckpoint {
    BenchmarkRevenueCheckpoint {
        base: revenue_side_checkpoint(&market.base_side),
        quote: revenue_side_checkpoint(&market.quote_side),
    }
}

fn checked_public_interest_side_transition(
    outstanding_before_operation: u128,
    outstanding_after_clock_before_transition: u128,
    outstanding_after_transition: u128,
    payment: BenchmarkPublicInterestPayment,
) -> Result<BenchmarkPublicInterestSideTransition> {
    require_gte!(
        outstanding_after_clock_before_transition,
        outstanding_before_operation,
        ErrorCode::BrokenInvariant
    );
    require_gte!(
        payment.gross_cash_interest_paid,
        payment.net_interest_vault_credit,
        ErrorCode::FeeMathOverflow
    );
    let clock_accrued = outstanding_after_clock_before_transition
        .checked_sub(outstanding_before_operation)
        .ok_or(ErrorCode::DebtMathOverflow)?;
    // Native fixed repayment/liquidation first computes one aggregate debt
    // reduction and passes that same reduction through
    // `realize_margin_clearance`; its proportional principal reduction makes
    // public interest monotone-decreasing for those transitions. Native borrow
    // can instead create at most the share-rounded increase. A row claiming
    // cash-paid interest while the net public-interest balance increased is
    // therefore impossible and is rejected below rather than guessed.
    let (transition_interest_created, total_interest_removed) =
        if outstanding_after_transition >= outstanding_after_clock_before_transition {
            (
                outstanding_after_transition
                    .checked_sub(outstanding_after_clock_before_transition)
                    .ok_or(ErrorCode::DebtMathOverflow)?,
                0,
            )
        } else {
            (
                0,
                outstanding_after_clock_before_transition
                    .checked_sub(outstanding_after_transition)
                    .ok_or(ErrorCode::DebtMathOverflow)?,
            )
        };
    let gross_cash_interest_paid = u128::from(payment.gross_cash_interest_paid);
    require_gte!(
        total_interest_removed,
        gross_cash_interest_paid,
        ErrorCode::BrokenInvariant
    );
    let interest_written_off = total_interest_removed
        .checked_sub(gross_cash_interest_paid)
        .ok_or(ErrorCode::DebtMathOverflow)?;
    require_eq!(
        outstanding_before_operation
            .checked_add(clock_accrued)
            .ok_or(ErrorCode::DebtMathOverflow)?,
        outstanding_after_clock_before_transition,
        ErrorCode::BrokenInvariant
    );
    require_eq!(
        outstanding_after_clock_before_transition
            .checked_add(transition_interest_created)
            .ok_or(ErrorCode::DebtMathOverflow)?,
        outstanding_after_transition
            .checked_add(total_interest_removed)
            .ok_or(ErrorCode::DebtMathOverflow)?,
        ErrorCode::BrokenInvariant
    );
    require_eq!(
        gross_cash_interest_paid
            .checked_add(interest_written_off)
            .ok_or(ErrorCode::DebtMathOverflow)?,
        total_interest_removed,
        ErrorCode::BrokenInvariant
    );
    Ok(BenchmarkPublicInterestSideTransition {
        outstanding_before_operation,
        outstanding_after_clock_before_transition,
        outstanding_after_transition,
        clock_accrued,
        transition_interest_created,
        gross_cash_interest_paid,
        net_interest_vault_credit: u128::from(payment.net_interest_vault_credit),
        total_interest_removed,
        interest_written_off,
    })
}

fn market_side_checkpoint(side: &MarketSide, debt: &Debt, asset: MarketAsset) -> Result<BenchmarkMarketSideCheckpoint> {
    let (
        fixed_debt_shares,
        fixed_debt,
        fixed_debt_principal,
        isolated_debt_shares,
        isolated_debt_principal,
        borrow_index_nad,
        rate_at_target_nad,
        last_accrual_slot,
    ) = match asset {
        MarketAsset::Base => (
            debt.fixed_base_shares,
            debt.fixed_base_debt()?,
            debt.fixed_base_principal,
            debt.isolated_base_shares,
            debt.isolated_base_principal,
            debt.base_borrow_index_nad,
            debt.base_rate_at_target_nad,
            debt.base_last_accrual_slot,
        ),
        MarketAsset::Quote => (
            debt.fixed_quote_shares,
            debt.fixed_quote_debt()?,
            debt.fixed_quote_principal,
            debt.isolated_quote_shares,
            debt.isolated_quote_principal,
            debt.quote_borrow_index_nad,
            debt.quote_rate_at_target_nad,
            debt.quote_last_accrual_slot,
        ),
    };
    Ok(BenchmarkMarketSideCheckpoint {
        live_reserve: side.reserves.live_reserve,
        cash_reserve: side.reserves.cash_reserve,
        protected_recenter_reserve: side.reserves.protected_recenter_reserve,
        base_hlp_backing_inventory: side.reserves.base_hlp_backing_inventory,
        quote_hlp_backing_inventory: side.reserves.quote_hlp_backing_inventory,
        ylp_supply: side.shares.ylp_supply,
        fixed_debt_shares,
        fixed_debt,
        fixed_debt_principal,
        isolated_debt_shares,
        isolated_debt: debt.isolated_debt(asset)?,
        isolated_debt_principal,
        borrow_index_nad,
        rate_at_target_nad,
        last_accrual_slot,
        daily_borrow_bucket: side.daily_borrow_bucket.borrowed_bucket,
    })
}

fn hlp_checkpoint(vault: &crate::state::HlpVault) -> BenchmarkHlpCheckpoint {
    BenchmarkHlpCheckpoint {
        ylp_shares: vault.ylp_shares,
        hlp_supply: vault.hlp_supply,
        debt_shares: vault.debt_shares,
        debt_principal: vault.debt_principal,
        base_hlp_live_reserve: vault.base_hlp_live_reserve,
        quote_hlp_live_reserve: vault.quote_hlp_live_reserve,
        residual_exposure: vault.residual_exposure,
        last_nav_nad: vault.last_nav_nad,
        cached_settlement_price_nad: vault.cached_settlement_price_nad,
    }
}

fn market_checkpoint(
    market: &Market,
    market_key: Option<Pubkey>,
    clock: BenchmarkClock,
) -> Result<BenchmarkMarketCheckpoint> {
    Ok(BenchmarkMarketCheckpoint {
        market_key,
        clock,
        version: market.version,
        base: market_side_checkpoint(&market.base_side, &market.debt, MarketAsset::Base)?,
        quote: market_side_checkpoint(&market.quote_side, &market.debt, MarketAsset::Quote)?,
        base_hlp: hlp_checkpoint(&market.base_hlp_vault),
        quote_hlp: hlp_checkpoint(&market.quote_hlp_vault),
        global_health_base_contribution_for_quote_debt: market.debt.global_health_base_contribution_for_quote_debt,
        global_health_quote_contribution_for_base_debt: market.debt.global_health_quote_contribution_for_base_debt,
        last_update_slot: market.last_update_slot,
        last_risk_snapshot_slot: market.risk.last_snapshot_slot,
        curve_revision: market.curve_revision,
        risk_revision: market.risk_revision,
        reduce_only: market.reduce_only,
    })
}

fn borrow_position_checkpoint(position: &BorrowPosition, debt: &Debt) -> Result<BenchmarkBorrowPositionCheckpoint> {
    Ok(BenchmarkBorrowPositionCheckpoint {
        owner: position.owner,
        market: position.market,
        position_id: position.position_id,
        base_collateral: position.base_collateral,
        quote_collateral: position.quote_collateral,
        fixed_base_shares: position.fixed_base_shares,
        fixed_quote_shares: position.fixed_quote_shares,
        fixed_base_debt: position.fixed_base_debt(debt)?,
        fixed_quote_debt: position.fixed_quote_debt(debt)?,
        global_health_base_contribution_for_quote_debt: position.global_health_base_contribution_for_quote_debt,
        global_health_quote_contribution_for_base_debt: position.global_health_quote_contribution_for_base_debt,
        base_liquidation_cf_bps: position.base_liquidation_cf_bps,
        quote_liquidation_cf_bps: position.quote_liquidation_cf_bps,
        base_referral_partner: position.base_referral_partner,
        quote_referral_partner: position.quote_referral_partner,
        base_referral_interest_share_bps: position.base_referral_interest_share_bps,
        quote_referral_interest_share_bps: position.quote_referral_interest_share_bps,
        auction_debt_asset: position.auction_debt_asset,
        auction_start_time: position.auction_start_time,
        auction_start_price_nad: position.auction_start_price_nad,
        auction_floor_price_nad: position.auction_floor_price_nad,
        bump: position.bump,
    })
}

fn advance_market_to_slot(market: &mut Market, current_slot: u64) -> Result<()> {
    market.assert_current_version()?;
    market.accrue_interest_to_slot(current_slot)?;
    if market.base_side.reserves.live_reserve > 0 && market.quote_side.reserves.live_reserve > 0 {
        market.advance_amm_clock(current_slot)?;
        market.checkpoint_hlp_vaults()?;
        market.refresh_risk_at_slot(current_slot)?;
    }
    Ok(())
}

fn execute_swap(
    market: &mut Market,
    clock: BenchmarkClock,
    request: BenchmarkSwapRequest,
) -> Result<BenchmarkSwapExecution> {
    require!(
        request.protocol_auction_split.is_valid(),
        ErrorCode::InvalidAuctionConfig
    );
    let prepared = SwapRequest {
        current_slot: clock.slot,
        current_unix_timestamp: clock.unix_timestamp,
        asset_in: request.asset_in,
        reserve_credit: request.reserve_credit,
        protocol_fee_bps: request.protocol_fee_bps,
    }
    .prepare(market)?;
    let quote = prepared.quote;
    let fee_eligible_ylp_supply = prepared.fee_eligible_ylp_supply;
    let interest_eligibility = prepared.interest_eligibility;
    let finalized = prepared.finalize_state(
        market,
        clock.slot,
        request.protocol_fee_bps,
        request.protocol_auction_split,
    )?;
    Ok(BenchmarkSwapExecution {
        quote,
        base_rebalance: finalized.base_rebalance,
        quote_rebalance: finalized.quote_rebalance,
        fee_eligible_ylp_supply,
        interest_eligibility,
    })
}

#[allow(clippy::too_many_arguments)]
fn apply_hlp_rebalance_settlement(
    market: &mut Market,
    external: &mut BenchmarkHlpOwnedState,
    receipt: HlpRebalanceReceipt,
    interest_eligibility: HlpYieldEligibility,
    interest_transfer: BenchmarkTokenTransferOutcome,
    protocol_interest_fee_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
    cash: &mut BenchmarkCashFlow,
) -> Result<()> {
    require!(receipt.target_asset == external.target_asset, ErrorCode::InvalidMint);
    require!(
        receipt.ylp_mint_amount == 0 || receipt.ylp_burn_amount == 0,
        ErrorCode::BrokenInvariant
    );
    external.hlp_vault_ylp_balance = external
        .hlp_vault_ylp_balance
        .checked_add(receipt.ylp_mint_amount)
        .and_then(|value| value.checked_sub(receipt.ylp_burn_amount))
        .ok_or(ErrorCode::InsufficientBalance)?;
    require_eq!(
        interest_transfer.source_debit,
        receipt.interest_paid,
        ErrorCode::BrokenInvariant
    );
    let borrowed_asset = external.target_asset.opposite();
    if receipt.interest_paid == 0 {
        require_eq!(interest_transfer.destination_credit, 0, ErrorCode::BrokenInvariant);
    } else {
        record_inline_hlp_interest_credit(
            market,
            borrowed_asset,
            interest_transfer.destination_credit,
            protocol_interest_fee_bps,
            protocol_auction_split,
            interest_eligibility,
        )?;
        let measured_credits = match borrowed_asset {
            MarketAsset::Base => &mut external.measured_base_interest_vault_credits,
            MarketAsset::Quote => &mut external.measured_quote_interest_vault_credits,
        };
        *measured_credits = measured_credits
            .checked_add(interest_transfer.destination_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    }
    let borrowed_cash = cash.side_mut(borrowed_asset);
    borrowed_cash.reserve_vault_debit = borrowed_cash
        .reserve_vault_debit
        .checked_add(receipt.interest_paid)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    borrowed_cash.interest_vault_credit = borrowed_cash
        .interest_vault_credit
        .checked_add(interest_transfer.destination_credit)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(())
}

struct ExistingPositionCapacityContext<'a> {
    market: &'a Market,
    position: &'a BorrowPosition,
    debt_asset: MarketAsset,
    risk: &'a Risk,
    external_debt_nad: u128,
}

struct ExistingPositionCapacityProjection {
    projected_position_debt: u128,
    terms: DynamicBorrowTerms,
}

impl ExistingPositionCapacityContext<'_> {
    fn project(&self, additional_amount: u64) -> Result<ExistingPositionCapacityProjection> {
        let (position_shares, aggregate_shares, borrow_index_nad) = match self.debt_asset {
            MarketAsset::Base => (
                self.position.fixed_base_shares,
                self.market.debt.fixed_base_shares,
                self.market.debt.base_borrow_index_nad,
            ),
            MarketAsset::Quote => (
                self.position.fixed_quote_shares,
                self.market.debt.fixed_quote_shares,
                self.market.debt.quote_borrow_index_nad,
            ),
        };
        let added_shares = if additional_amount == 0 {
            0
        } else {
            Debt::debt_to_shares(additional_amount, borrow_index_nad)?
        };
        let projected_position_debt = Debt::shares_to_debt(
            position_shares
                .checked_add(added_shares)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            borrow_index_nad,
        )?;
        let projected_total_debt = Debt::shares_to_debt(
            aggregate_shares
                .checked_add(added_shares)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            borrow_index_nad,
        )?;
        let collateral_amount = self.position.collateral(self.debt_asset.opposite());
        let target_contribution = self.market.debt_capped_global_health_contribution(
            self.debt_asset,
            projected_position_debt,
            collateral_amount,
            self.risk,
        )?;
        let projected_aggregate = self.market.projected_aggregate_global_health_contribution(
            self.position,
            self.debt_asset,
            target_contribution,
        )?;
        let projected_total_debt_nad =
            normalize_to_nad(projected_total_debt, self.market.side(self.debt_asset).asset_decimals)?;
        let terms = self.market.dynamic_borrow_terms(
            self.debt_asset,
            collateral_amount,
            self.external_debt_nad,
            projected_total_debt_nad,
            projected_aggregate,
            self.risk,
        )?;
        Ok(ExistingPositionCapacityProjection {
            projected_position_debt,
            terms,
        })
    }
}

fn existing_borrow_capacity_preview(
    market: &Market,
    position: &BorrowPosition,
    debt_asset: MarketAsset,
    current_slot: u64,
    global_reduce_only: bool,
) -> Result<BenchmarkExistingBorrowCapacity> {
    let risk = market.risk;
    let context = ExistingPositionCapacityContext {
        market,
        position,
        debt_asset,
        risk: &risk,
        external_debt_nad: market.external_fixed_debt_nad(position, debt_asset)?,
    };
    let debt_before = match debt_asset {
        MarketAsset::Base => position.fixed_base_debt(&market.debt)?,
        MarketAsset::Quote => position.fixed_quote_debt(&market.debt)?,
    };
    let baseline = context.project(0)?;
    let current_underwriting_satisfied = baseline.terms.max_debt as u128 >= debt_before;
    let collateral_value_nad =
        market.collateral_value_nad(debt_asset.opposite(), position.collateral(debt_asset.opposite()), &risk)?;
    let maximum_underwriting_debt_nad = collateral_value_nad
        .checked_mul(MAX_COLLATERAL_FACTOR_BPS as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // Dynamic underwriting's incremental curve value cannot exceed the same
    // pessimistic collateral quote at the protocol maximum CF. Using this
    // health-domain ceiling avoids importing live/cash capital into gross
    // admissibility.
    let search_upper =
        denormalize_from_nad_floor(maximum_underwriting_debt_nad, market.side(debt_asset).asset_decimals)?;
    let underwriting_max_additional = if current_underwriting_satisfied {
        maximum_monotone_capacity(search_upper, |amount| {
            let projected = context.project(amount)?;
            Ok(projected.terms.max_debt as u128 >= projected.projected_position_debt)
        })?
    } else {
        0
    };

    let current_health = market.market_health_from_risk(&risk)?;
    let current_global_health_floor_satisfied = market.assert_market_health_snapshot(&current_health).is_ok();
    let global_health_floor_max_additional = if current_global_health_floor_satisfied {
        maximum_monotone_capacity(search_upper, |amount| {
            Ok(context.project(amount)?.terms.projected_market_health_bps
                >= market.config.borrow_market_health_floor_bps as u64)
        })?
    } else {
        0
    };
    let cash_max_additional = market.side(debt_asset).reserves.cash_reserve;
    let daily_limit = market.daily_limit_for_side(debt_asset, market.config.max_daily_borrow_bps)?;
    let daily_bucket_max_additional = market
        .side(debt_asset)
        .daily_borrow_bucket
        .remaining(daily_limit, current_slot)?;
    let market_reduce_only = market.reduce_only;
    let reduce_only_max_additional = if global_reduce_only || market_reduce_only {
        Some(0)
    } else {
        None
    };
    let mut actionable_max_additional = underwriting_max_additional
        .min(global_health_floor_max_additional)
        .min(cash_max_additional)
        .min(daily_bucket_max_additional);
    if let Some(reduce_only_bound) = reduce_only_max_additional {
        actionable_max_additional = actionable_max_additional.min(reduce_only_bound);
    }
    let mut limiting_constraints = Vec::new();
    for (bound, constraint) in [
        (
            underwriting_max_additional,
            BenchmarkBorrowCapacityConstraint::Underwriting,
        ),
        (
            global_health_floor_max_additional,
            BenchmarkBorrowCapacityConstraint::GlobalHealthFloor,
        ),
        (cash_max_additional, BenchmarkBorrowCapacityConstraint::Cash),
        (
            daily_bucket_max_additional,
            BenchmarkBorrowCapacityConstraint::DailyBorrowBucket,
        ),
    ] {
        if bound == actionable_max_additional {
            limiting_constraints.push(constraint);
        }
    }
    if reduce_only_max_additional == Some(actionable_max_additional) {
        limiting_constraints.push(BenchmarkBorrowCapacityConstraint::ReduceOnly);
    }
    let gross_admissible_max_debt = if current_underwriting_satisfied {
        context.project(underwriting_max_additional)?.projected_position_debt
    } else {
        u128::from(baseline.terms.max_debt)
    };
    Ok(BenchmarkExistingBorrowCapacity {
        debt_asset,
        debt_before,
        gross_admissible_max_debt,
        actionable_max_debt: context.project(actionable_max_additional)?.projected_position_debt,
        underwriting_max_additional,
        global_health_floor_max_additional,
        cash_max_additional,
        daily_bucket_max_additional,
        reduce_only_max_additional,
        actionable_max_additional,
        current_underwriting_satisfied,
        current_global_health_floor_satisfied,
        market_reduce_only,
        global_reduce_only,
        limiting_constraints,
    })
}

fn maximum_monotone_capacity(upper: u64, mut accepted: impl FnMut(u64) -> Result<bool>) -> Result<u64> {
    let mut low = 0_u64;
    let mut high = upper;
    while low < high {
        let midpoint = low + (high - low) / 2 + 1;
        if accepted(midpoint)? {
            low = midpoint;
        } else {
            high = midpoint - 1;
        }
    }
    Ok(low)
}

struct NewPositionCapacityContext<'a> {
    market: &'a Market,
    debt_asset: MarketAsset,
    collateral_amount: u64,
    risk: &'a Risk,
    existing_total_debt_nad: u128,
    current_aggregate_contribution: u64,
}

impl NewPositionCapacityContext<'_> {
    fn terms(&self, projected_debt_amount: u64) -> Result<(DynamicBorrowTerms, u64)> {
        let debt_decimals = self.market.side(self.debt_asset).asset_decimals;
        let projected_debt_nad = normalize_to_nad(projected_debt_amount as u128, debt_decimals)?;
        let projected_total_debt_nad = self
            .existing_total_debt_nad
            .checked_add(projected_debt_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let contribution = self.market.debt_capped_global_health_contribution(
            self.debt_asset,
            projected_debt_amount as u128,
            self.collateral_amount,
            self.risk,
        )?;
        let projected_aggregate = self
            .current_aggregate_contribution
            .checked_add(contribution)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let terms = self.market.dynamic_borrow_terms(
            self.debt_asset,
            self.collateral_amount,
            self.existing_total_debt_nad,
            projected_total_debt_nad,
            projected_aggregate,
            self.risk,
        )?;
        Ok((terms, contribution))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS},
        state::{AmmConfig, BorrowPosition, IrmConfig, DEFAULT_DAILY_BORROW_BPS},
    };

    fn valid_config() -> MarketConfig {
        MarketConfig {
            swap_fee_bps: 30,
            divergence_fee_share_cap_bps: 0,
            volatility_fee_share_cap_bps: 0,
            target_hlp_leverage_bps: 2 * BPS_DENOMINATOR,
            settlement_divergence_bps: BPS_DENOMINATOR,
            ema_half_life_ms: MIN_HALF_LIFE_MS,
            directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
            curve_depth_ema_half_life_ms: MIN_HALF_LIFE_MS,
            max_daily_borrow_bps: DEFAULT_DAILY_BORROW_BPS,
            global_health_contribution_cap_bps: 15_000,
            borrow_market_health_floor_bps: BPS_DENOMINATOR,
            amm: AmmConfig::default(),
            irm: IrmConfig::default(),
            start_time: 0,
        }
    }

    fn initialized_market() -> BenchmarkMarket {
        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let base_side = MarketSide {
            asset_mint: base_mint,
            asset_decimals: 6,
            hlp_mint: Pubkey::new_unique(),
            reserve_vault: Pubkey::new_unique(),
            collateral_vault: Pubkey::new_unique(),
            interest_vault: Pubkey::new_unique(),
            ..MarketSide::default()
        };
        let quote_side = MarketSide {
            asset_mint: quote_mint,
            asset_decimals: 6,
            hlp_mint: Pubkey::new_unique(),
            reserve_vault: Pubkey::new_unique(),
            collateral_vault: Pubkey::new_unique(),
            interest_vault: Pubkey::new_unique(),
            ..MarketSide::default()
        };
        let mut benchmark = BenchmarkMarket::initialize(
            BenchmarkMarketInit {
                ylp_mint: Pubkey::new_unique(),
                base_side,
                quote_side,
                config: valid_config(),
                base_hlp_ylp_vault: Pubkey::new_unique(),
                quote_hlp_ylp_vault: Pubkey::new_unique(),
                base_insurance_vault: Pubkey::new_unique(),
                quote_insurance_vault: Pubkey::new_unique(),
                params_hash: [7; 32],
                initial_liquidity_authority: Pubkey::new_unique(),
                bootstrap_price_nad: 0,
                launch_fee_progress_offset: 0,
                bump: 255,
            },
            BenchmarkClock {
                slot: 0,
                unix_timestamp: 0,
            },
        )
        .unwrap();
        benchmark.market_mut().add_liquidity(1_000_000, 2_000_000).unwrap();
        benchmark
            .advance_to(BenchmarkClock {
                slot: 1,
                unix_timestamp: 1,
            })
            .unwrap();
        benchmark
    }

    fn market_bytes(market: &Market) -> Vec<u8> {
        let mut bytes = Vec::new();
        market.try_serialize(&mut bytes).unwrap();
        bytes
    }

    fn position_bytes(position: &BorrowPosition) -> Vec<u8> {
        let mut bytes = Vec::new();
        position.try_serialize(&mut bytes).unwrap();
        bytes
    }

    fn initialized_keyed_market() -> (BenchmarkMarket, Pubkey) {
        let benchmark = initialized_market();
        let market_key = Pubkey::new_unique();
        let keyed = BenchmarkMarket::from_market_state_at(
            market_key,
            clone_market(benchmark.market()).unwrap(),
            benchmark.clock(),
        )
        .unwrap();
        (keyed, market_key)
    }

    fn borrow_position_with_collateral(collateral_asset: MarketAsset, collateral_amount: u64) -> BorrowPosition {
        let mut position = BorrowPosition {
            owner: Pubkey::new_unique(),
            market: Pubkey::new_unique(),
            position_id: Pubkey::new_unique(),
            base_collateral: 0,
            quote_collateral: 0,
            global_health_base_contribution_for_quote_debt: 0,
            global_health_quote_contribution_for_base_debt: 0,
            base_liquidation_cf_bps: 0,
            quote_liquidation_cf_bps: 0,
            base_referral_partner: Pubkey::default(),
            quote_referral_partner: Pubkey::default(),
            base_referral_interest_share_bps: 0,
            quote_referral_interest_share_bps: 0,
            fixed_base_shares: 0,
            fixed_quote_shares: 0,
            auction_debt_asset: u8::MAX,
            auction_start_time: 0,
            auction_start_price_nad: 0,
            auction_floor_price_nad: 0,
            bump: 255,
        };
        match collateral_asset {
            MarketAsset::Base => position.base_collateral = collateral_amount,
            MarketAsset::Quote => position.quote_collateral = collateral_amount,
        }
        position
    }

    fn borrowed_liquidation_fixture(collateral_after: u64) -> (BenchmarkMarket, BenchmarkBorrowPosition, u64) {
        let (mut benchmark, market_key) = initialized_keyed_market();
        let mut position =
            BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, Pubkey::new_unique(), 251);
        benchmark
            .execute_deposit_collateral(
                &mut position,
                BenchmarkDepositCollateralRequest {
                    clock: BenchmarkClock {
                        slot: 2,
                        unix_timestamp: 2,
                    },
                    collateral_asset: MarketAsset::Quote,
                    collateral_credit: 300_000,
                },
            )
            .unwrap();
        let capacity = benchmark
            .preview_borrow_capacity(MarketAsset::Quote, 300_000, None)
            .unwrap()
            .max_borrow_amount;
        let borrowed = capacity / 2;
        benchmark
            .execute_borrow(
                &mut position,
                BenchmarkBorrowRequest {
                    clock: BenchmarkClock {
                        slot: 3,
                        unix_timestamp: 3,
                    },
                    debt_asset: MarketAsset::Base,
                    borrow_amount: borrowed,
                    recipient_credit: borrowed,
                    min_recipient_credit: 0,
                    min_liquidation_cf_bps: 0,
                    global_reduce_only: false,
                    referral_partner: Pubkey::default(),
                    referral_interest_share_bps: 0,
                    referral_interest_share_cap_bps: 0,
                },
            )
            .unwrap();
        // Model an adverse collateral-price path by reducing only this
        // position's claim. The vault retains the historical gross custody;
        // unrelated atoms are inert donations at this unit-test boundary.
        position.position.quote_collateral = collateral_after;
        assert!(benchmark
            .market()
            .is_position_liquidatable(position.position(), MarketAsset::Base)
            .unwrap());
        (benchmark, position, borrowed)
    }

    fn exact_transfer(amount: u64) -> BenchmarkTokenTransferOutcome {
        BenchmarkTokenTransferOutcome {
            source_debit: amount,
            destination_credit: amount,
        }
    }

    #[test]
    fn explicit_clock_advance_matches_native_update_order() {
        let mut benchmark = initialized_market();
        let mut native = clone_market(benchmark.market()).unwrap();
        let clock = BenchmarkClock {
            slot: 10_000,
            unix_timestamp: 500,
        };

        native.accrue_interest_to_slot(clock.slot).unwrap();
        native.advance_amm_clock(clock.slot).unwrap();
        native.checkpoint_hlp_vaults().unwrap();
        native.refresh_risk_at_slot(clock.slot).unwrap();
        benchmark.advance_to(clock).unwrap();

        assert_eq!(benchmark.clock(), clock);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&native));
        assert_eq!(benchmark.market().version, MARKET_LAYOUT_VERSION);
        assert_ne!(
            benchmark.market().debt.base_rate_at_target_nad,
            INTEREST_INITIAL_RATE_AT_TARGET_NAD
        );
    }

    #[test]
    fn pure_preview_and_commit_match_the_native_swap_pipeline_exactly() {
        let mut benchmark = initialized_market();
        let request = BenchmarkSwapRequest {
            asset_in: MarketAsset::Base,
            reserve_credit: 10_000,
            protocol_fee_bps: 1_000,
            protocol_auction_split: ProtocolAuctionSplit::default(),
        };
        let before = market_bytes(benchmark.market());
        let preview = benchmark.preview_swap(request).unwrap();
        assert_eq!(market_bytes(benchmark.market()), before);

        let mut native = clone_market(benchmark.market()).unwrap();
        let prepared = SwapRequest {
            current_slot: benchmark.clock().slot,
            current_unix_timestamp: benchmark.clock().unix_timestamp,
            asset_in: request.asset_in,
            reserve_credit: request.reserve_credit,
            protocol_fee_bps: request.protocol_fee_bps,
        }
        .prepare(&mut native)
        .unwrap();
        let native_quote = prepared.quote;
        let native_fee_supply = prepared.fee_eligible_ylp_supply;
        let native_interest_eligibility = prepared.interest_eligibility;
        let native_finalized = prepared
            .finalize_state(
                &mut native,
                benchmark.clock().slot,
                request.protocol_fee_bps,
                request.protocol_auction_split,
            )
            .unwrap();

        assert_eq!(preview.quote, native_quote);
        assert_eq!(preview.base_rebalance, native_finalized.base_rebalance);
        assert_eq!(preview.quote_rebalance, native_finalized.quote_rebalance);
        assert_eq!(preview.fee_eligible_ylp_supply, native_fee_supply);
        assert_eq!(preview.interest_eligibility, native_interest_eligibility);

        let execution = benchmark.execute_swap(request).unwrap();
        assert_eq!(execution, preview);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&native));
    }

    #[test]
    fn borrow_capacity_bound_is_accepted_and_the_next_atom_is_rejected() {
        let benchmark = initialized_market();
        let collateral_asset = MarketAsset::Quote;
        let collateral_amount = 200_000;
        let capacity = benchmark
            .preview_borrow_capacity(collateral_asset, collateral_amount, None)
            .unwrap();
        assert!(capacity.max_borrow_amount > 0);

        let mut accepted = benchmark.try_fork().unwrap();
        let mut accepted_position = borrow_position_with_collateral(collateral_asset, collateral_amount);
        accepted
            .transact(|market, clock| {
                market.borrow(
                    &mut accepted_position,
                    collateral_asset.opposite(),
                    capacity.max_borrow_amount,
                    0,
                    clock.slot,
                )
            })
            .unwrap();

        let mut rejected = benchmark.try_fork().unwrap();
        let mut rejected_position = borrow_position_with_collateral(collateral_asset, collateral_amount);
        assert!(rejected
            .transact(|market, clock| {
                market.borrow(
                    &mut rejected_position,
                    collateral_asset.opposite(),
                    capacity.max_borrow_amount + 1,
                    0,
                    clock.slot,
                )
            })
            .is_err());
        assert_eq!(market_bytes(rejected.market()), market_bytes(benchmark.market()));
    }

    #[test]
    fn existing_position_capacity_decomposes_native_bounds_without_mutation() {
        let (mut benchmark, market_key) = initialized_keyed_market();
        let mut position =
            BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, Pubkey::new_unique(), 250);
        benchmark
            .execute_deposit_collateral(
                &mut position,
                BenchmarkDepositCollateralRequest {
                    clock: BenchmarkClock {
                        slot: 2,
                        unix_timestamp: 2,
                    },
                    collateral_asset: MarketAsset::Quote,
                    collateral_credit: 300_000,
                },
            )
            .unwrap();
        let initial = benchmark
            .preview_existing_borrow_capacity(
                &position,
                MarketAsset::Base,
                BenchmarkClock {
                    slot: 3,
                    unix_timestamp: 3,
                },
                false,
            )
            .unwrap();
        assert!(initial.actionable_max_additional > 2);
        let first_borrow = initial.actionable_max_additional / 3;
        benchmark
            .execute_borrow(
                &mut position,
                BenchmarkBorrowRequest {
                    clock: BenchmarkClock {
                        slot: 3,
                        unix_timestamp: 3,
                    },
                    debt_asset: MarketAsset::Base,
                    borrow_amount: first_borrow,
                    recipient_credit: first_borrow,
                    min_recipient_credit: 0,
                    min_liquidation_cf_bps: 0,
                    global_reduce_only: false,
                    referral_partner: Pubkey::default(),
                    referral_interest_share_bps: 0,
                    referral_interest_share_cap_bps: 0,
                },
            )
            .unwrap();

        let market_before = market_bytes(benchmark.market());
        let position_before = position_bytes(position.position());
        let clock_before = benchmark.clock();
        let clock = BenchmarkClock {
            slot: 4,
            unix_timestamp: 4,
        };
        let capacity = benchmark
            .preview_existing_borrow_capacity(&position, MarketAsset::Base, clock, false)
            .unwrap();
        assert!(capacity.debt_before > 0);
        assert!(capacity.gross_admissible_max_debt >= capacity.actionable_max_debt);
        assert_eq!(
            capacity.actionable_max_additional,
            capacity
                .underwriting_max_additional
                .min(capacity.global_health_floor_max_additional)
                .min(capacity.cash_max_additional)
                .min(capacity.daily_bucket_max_additional)
        );
        assert!(!capacity.limiting_constraints.is_empty());
        assert_eq!(market_bytes(benchmark.market()), market_before);
        assert_eq!(position_bytes(position.position()), position_before);
        assert_eq!(benchmark.clock(), clock_before);

        if capacity.actionable_max_additional > 0 {
            let mut accepted_market = benchmark.try_fork().unwrap();
            let mut accepted_position = position.try_fork().unwrap();
            accepted_market
                .execute_borrow(
                    &mut accepted_position,
                    BenchmarkBorrowRequest {
                        clock,
                        debt_asset: MarketAsset::Base,
                        borrow_amount: capacity.actionable_max_additional,
                        recipient_credit: capacity.actionable_max_additional,
                        min_recipient_credit: 0,
                        min_liquidation_cf_bps: 0,
                        global_reduce_only: false,
                        referral_partner: Pubkey::default(),
                        referral_interest_share_bps: 0,
                        referral_interest_share_cap_bps: 0,
                    },
                )
                .unwrap();

            if capacity.actionable_max_additional < u64::MAX {
                let mut rejected_market = benchmark.try_fork().unwrap();
                let mut rejected_position = position.try_fork().unwrap();
                let next = capacity.actionable_max_additional + 1;
                assert!(rejected_market
                    .execute_borrow(
                        &mut rejected_position,
                        BenchmarkBorrowRequest {
                            clock,
                            debt_asset: MarketAsset::Base,
                            borrow_amount: next,
                            recipient_credit: next,
                            min_recipient_credit: 0,
                            min_liquidation_cf_bps: 0,
                            global_reduce_only: false,
                            referral_partner: Pubkey::default(),
                            referral_interest_share_bps: 0,
                            referral_interest_share_cap_bps: 0,
                        },
                    )
                    .is_err());
            }
        }
    }

    #[test]
    fn existing_position_capacity_reduce_only_is_exact_zero_actionable() {
        let (mut benchmark, market_key) = initialized_keyed_market();
        let mut position =
            BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, Pubkey::new_unique(), 249);
        benchmark
            .execute_deposit_collateral(
                &mut position,
                BenchmarkDepositCollateralRequest {
                    clock: BenchmarkClock {
                        slot: 2,
                        unix_timestamp: 2,
                    },
                    collateral_asset: MarketAsset::Quote,
                    collateral_credit: 300_000,
                },
            )
            .unwrap();
        let capacity = benchmark
            .preview_existing_borrow_capacity(
                &position,
                MarketAsset::Base,
                BenchmarkClock {
                    slot: 3,
                    unix_timestamp: 3,
                },
                true,
            )
            .unwrap();
        assert_eq!(capacity.actionable_max_additional, 0);
        assert_eq!(capacity.actionable_max_debt, capacity.debt_before);
        assert!(capacity.gross_admissible_max_debt > capacity.debt_before);
        assert!(capacity
            .limiting_constraints
            .contains(&BenchmarkBorrowCapacityConstraint::ReduceOnly));
    }

    #[test]
    fn existing_position_target_solver_preserves_post_debt_atoms_across_share_rounding() {
        let (mut benchmark, market_key) = initialized_keyed_market();
        let mut position =
            BenchmarkBorrowPosition::initialize(Pubkey::new_unique(), market_key, Pubkey::new_unique(), 248);
        benchmark
            .execute_deposit_collateral(
                &mut position,
                BenchmarkDepositCollateralRequest {
                    clock: BenchmarkClock {
                        slot: 2,
                        unix_timestamp: 2,
                    },
                    collateral_asset: MarketAsset::Quote,
                    collateral_credit: 300_000,
                },
            )
            .unwrap();
        benchmark
            .execute_borrow(
                &mut position,
                BenchmarkBorrowRequest {
                    clock: BenchmarkClock {
                        slot: 3,
                        unix_timestamp: 3,
                    },
                    debt_asset: MarketAsset::Base,
                    borrow_amount: 10_001,
                    recipient_credit: 10_001,
                    min_recipient_credit: 0,
                    min_liquidation_cf_bps: 0,
                    global_reduce_only: false,
                    referral_partner: Pubkey::default(),
                    referral_interest_share_bps: 0,
                    referral_interest_share_cap_bps: 0,
                },
            )
            .unwrap();
        // One new debt share is worth either one or two debt atoms at this
        // non-NAD index. The raw transfer request and post-debt delta therefore
        // cannot be treated as interchangeable units.
        benchmark.market_mut().debt.base_borrow_index_nad = (NAD as u128) * 3 / 2;
        benchmark.market_mut().config.borrow_market_health_floor_bps = 9_000;
        let clock = BenchmarkClock {
            slot: 3,
            unix_timestamp: 3,
        };
        let capacity = benchmark
            .preview_existing_borrow_capacity(&position, MarketAsset::Base, clock, false)
            .unwrap();
        let target = capacity.debt_before + 2;
        let solved = benchmark
            .preview_existing_borrow_request_for_target(&position, MarketAsset::Base, clock, false, target)
            .unwrap();
        assert_eq!(solved.borrow_request, 1);
        assert_eq!(solved.projected_debt, target);
        assert_eq!(solved.target_debt_gap, 0);
        assert_ne!(
            u128::from(solved.borrow_request),
            solved.projected_debt - capacity.debt_before
        );

        let unreachable = benchmark
            .preview_existing_borrow_request_for_target(
                &position,
                MarketAsset::Base,
                clock,
                false,
                capacity.debt_before + 1,
            )
            .unwrap();
        assert_eq!(unreachable.borrow_request, 0);
        assert_eq!(unreachable.target_debt_gap, 1);

        let receipt = benchmark
            .execute_borrow(
                &mut position,
                BenchmarkBorrowRequest {
                    clock,
                    debt_asset: MarketAsset::Base,
                    borrow_amount: solved.borrow_request,
                    recipient_credit: solved.borrow_request,
                    min_recipient_credit: 0,
                    min_liquidation_cf_bps: 0,
                    global_reduce_only: false,
                    referral_partner: Pubkey::default(),
                    referral_interest_share_bps: 0,
                    referral_interest_share_cap_bps: 0,
                },
            )
            .unwrap();
        assert_eq!(receipt.position_after.fixed_base_debt, solved.projected_debt);
    }

    #[test]
    fn balanced_ylp_wrappers_match_native_transition_order_exactly() {
        let mut benchmark = initialized_market();
        let mut native = clone_market(benchmark.market()).unwrap();
        let add_clock = BenchmarkClock {
            slot: 20,
            unix_timestamp: 20,
        };
        let add_request = BenchmarkAddYlpRequest {
            clock: add_clock,
            owner: Pubkey::new_unique(),
            base_reserve_credit: 100_000,
            quote_reserve_credit: 200_000,
            min_ylp_amount: 100_000,
            global_reduce_only: false,
        };

        advance_market_to_slot(&mut native, add_clock.slot).unwrap();
        carry_forward_all_ylp_revenue(&mut native).unwrap();
        let native_add = native
            .add_liquidity(add_request.base_reserve_credit, add_request.quote_reserve_credit)
            .unwrap();
        native.finalize_amm_transition_and_observe_risk(add_clock.slot).unwrap();

        let add = benchmark.execute_add_ylp(add_request).unwrap();
        assert_eq!(add.receipt.ylp_amount, native_add.ylp_amount);
        assert_eq!(add.receipt.ylp_supply, native_add.ylp_supply);
        assert_eq!(add.receipt.base_reserve_amount, native_add.base_reserve_credit);
        assert_eq!(add.receipt.quote_reserve_amount, native_add.quote_reserve_credit);
        assert_eq!(add.cash.base.reserve_vault_credit, native_add.base_reserve_credit);
        assert_eq!(add.cash.quote.reserve_vault_credit, native_add.quote_reserve_credit);
        assert_eq!(add.market_after, benchmark.checkpoint().unwrap());
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&native));

        let remove_clock = BenchmarkClock {
            slot: 21,
            unix_timestamp: 21,
        };
        let ylp_amount = native_add.ylp_amount / 2;
        advance_market_to_slot(&mut native, remove_clock.slot).unwrap();
        carry_forward_all_ylp_revenue(&mut native).unwrap();
        let native_remove = native.remove_liquidity(ylp_amount).unwrap();
        native
            .finalize_amm_transition_and_observe_risk(remove_clock.slot)
            .unwrap();
        native.assert_market_health().unwrap();

        let remove = benchmark
            .execute_remove_ylp(BenchmarkRemoveYlpRequest {
                clock: remove_clock,
                ylp_amount,
                owner_ylp_balance_before: native_add.ylp_amount,
                base_recipient_credit: native_remove.base_amount_out,
                quote_recipient_credit: native_remove.quote_amount_out,
                min_base_recipient_credit: native_remove.base_amount_out,
                min_quote_recipient_credit: native_remove.quote_amount_out,
            })
            .unwrap();
        assert_eq!(remove.receipt.ylp_amount, native_remove.ylp_amount);
        assert_eq!(remove.receipt.ylp_supply, native_remove.ylp_supply);
        assert_eq!(remove.cash.base.reserve_vault_debit, native_remove.base_amount_out);
        assert_eq!(remove.cash.quote.reserve_vault_debit, native_remove.quote_amount_out);
        assert_eq!(remove.cash.base.recipient_credit, native_remove.base_amount_out);
        assert_eq!(remove.cash.quote.recipient_credit, native_remove.quote_amount_out);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&native));
    }

    #[test]
    fn position_wrappers_match_native_deposit_borrow_and_repay() {
        let (mut benchmark, market_key) = initialized_keyed_market();
        let owner = Pubkey::new_unique();
        let position_id = Pubkey::new_unique();
        let mut position = BenchmarkBorrowPosition::initialize(owner, market_key, position_id, 254);
        let mut native_market = clone_market(benchmark.market()).unwrap();
        let mut native_position =
            BenchmarkBorrowPosition::initialize(owner, market_key, position_id, 254).into_position();

        let deposit_clock = BenchmarkClock {
            slot: 2,
            unix_timestamp: 2,
        };
        advance_market_to_slot(&mut native_market, deposit_clock.slot).unwrap();
        native_market.assert_started_at(deposit_clock.unix_timestamp).unwrap();
        let native_deposit = native_market
            .deposit_collateral(&mut native_position, MarketAsset::Quote, 300_000)
            .unwrap();
        let deposit = benchmark
            .execute_deposit_collateral(
                &mut position,
                BenchmarkDepositCollateralRequest {
                    clock: deposit_clock,
                    collateral_asset: MarketAsset::Quote,
                    collateral_credit: 300_000,
                },
            )
            .unwrap();
        assert_eq!(deposit.market.receipt, native_deposit);
        assert_eq!(deposit.market.cash.quote.collateral_vault_credit, 300_000);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&native_market));
        assert_eq!(position_bytes(position.position()), position_bytes(&native_position));

        let capacity = benchmark
            .preview_borrow_capacity(MarketAsset::Quote, 300_000, None)
            .unwrap();
        assert!(capacity.max_borrow_amount >= 2);
        let borrow_amount = capacity.max_borrow_amount / 2;
        let borrow_clock = BenchmarkClock {
            slot: 3,
            unix_timestamp: 3,
        };
        advance_market_to_slot(&mut native_market, borrow_clock.slot).unwrap();
        native_market.assert_started_at(borrow_clock.unix_timestamp).unwrap();
        native_position.set_referral_binding(MarketAsset::Base, Pubkey::default(), 0);
        let native_borrow = native_market
            .borrow(
                &mut native_position,
                MarketAsset::Base,
                borrow_amount,
                0,
                borrow_clock.slot,
            )
            .unwrap();
        native_market.finalize_amm_transition(borrow_clock.slot).unwrap();
        native_market.refresh_risk_at_slot(borrow_clock.slot).unwrap();
        let borrow = benchmark
            .execute_borrow(
                &mut position,
                BenchmarkBorrowRequest {
                    clock: borrow_clock,
                    debt_asset: MarketAsset::Base,
                    borrow_amount,
                    recipient_credit: borrow_amount,
                    min_recipient_credit: borrow_amount,
                    min_liquidation_cf_bps: 0,
                    global_reduce_only: false,
                    referral_partner: Pubkey::default(),
                    referral_interest_share_bps: 0,
                    referral_interest_share_cap_bps: BPS_DENOMINATOR,
                },
            )
            .unwrap();
        assert_eq!(borrow.market.receipt, native_borrow);
        assert_eq!(borrow.market.cash.base.reserve_vault_debit, borrow_amount);
        assert_eq!(borrow.market.cash.base.recipient_credit, borrow_amount);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&native_market));
        assert_eq!(position_bytes(position.position()), position_bytes(&native_position));

        let repay_clock = BenchmarkClock {
            slot: 100_003,
            unix_timestamp: 100_003,
        };
        advance_market_to_slot(&mut native_market, repay_clock.slot).unwrap();
        native_market.assert_started_at(repay_clock.unix_timestamp).unwrap();
        let repayment = native_market
            .fixed_repayment_for_max(&native_position, MarketAsset::Base, u64::MAX)
            .unwrap();
        let native_repay = native_market
            .repay(&mut native_position, MarketAsset::Base, repayment.cash_repaid)
            .unwrap();
        if native_repay.interest_paid > 0 {
            native_market
                .base_side
                .record_interest_credit(native_repay.interest_paid, 2_000, ProtocolAuctionSplit::default(), 0)
                .unwrap();
        }
        native_market.finalize_amm_transition(repay_clock.slot).unwrap();
        native_market.refresh_risk_at_slot(repay_clock.slot).unwrap();

        let repay = benchmark
            .execute_repay(
                &mut position,
                BenchmarkRepayRequest {
                    clock: repay_clock,
                    max_repay_credit: u64::MAX,
                    interest_vault_credit: native_repay.interest_paid,
                    protocol_interest_fee_bps: 2_000,
                    protocol_auction_split: ProtocolAuctionSplit::default(),
                    referral_interest_amount: 0,
                    debt_asset: MarketAsset::Base,
                },
            )
            .unwrap();
        assert_eq!(repay.market.receipt, native_repay);
        assert_eq!(repay.market.cash.base.reserve_vault_credit, native_repay.cash_repaid);
        assert_eq!(repay.market.cash.base.reserve_vault_debit, native_repay.interest_paid);
        assert_eq!(repay.market.cash.base.interest_vault_credit, native_repay.interest_paid);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&native_market));
        assert_eq!(position_bytes(position.position()), position_bytes(&native_position));
        assert_eq!(repay.position_after, position.checkpoint(&benchmark).unwrap());
    }

    #[test]
    fn failed_position_transition_rolls_back_market_position_and_clock() {
        let (mut benchmark, market_key) = initialized_keyed_market();
        let owner = Pubkey::new_unique();
        let mut position = BenchmarkBorrowPosition::initialize(owner, market_key, Pubkey::new_unique(), 253);
        benchmark
            .execute_deposit_collateral(
                &mut position,
                BenchmarkDepositCollateralRequest {
                    clock: BenchmarkClock {
                        slot: 2,
                        unix_timestamp: 2,
                    },
                    collateral_asset: MarketAsset::Quote,
                    collateral_credit: 300_000,
                },
            )
            .unwrap();
        let market_before = market_bytes(benchmark.market());
        let position_before = position_bytes(position.position());
        let clock_before = benchmark.clock();
        let impossible_amount = benchmark.market().base_side.reserves.cash_reserve + 1;

        assert!(benchmark
            .execute_borrow(
                &mut position,
                BenchmarkBorrowRequest {
                    clock: BenchmarkClock {
                        slot: 50_000,
                        unix_timestamp: 50_000,
                    },
                    debt_asset: MarketAsset::Base,
                    borrow_amount: impossible_amount,
                    recipient_credit: impossible_amount,
                    min_recipient_credit: impossible_amount,
                    min_liquidation_cf_bps: 0,
                    global_reduce_only: false,
                    referral_partner: Pubkey::new_unique(),
                    referral_interest_share_bps: 100,
                    referral_interest_share_cap_bps: 100,
                },
            )
            .is_err());
        assert_eq!(benchmark.clock(), clock_before);
        assert_eq!(market_bytes(benchmark.market()), market_before);
        assert_eq!(position_bytes(position.position()), position_before);
    }

    #[test]
    fn leverage_and_hlp_external_state_are_fully_owned() {
        assert_eq!(
            BenchmarkMarket::leverage_external_requirements().requirements,
            LEVERAGE_EXTERNAL_REQUIREMENTS
        );
        assert_eq!(
            BenchmarkMarket::hlp_external_requirements().requirements,
            HLP_EXTERNAL_REQUIREMENTS
        );
        assert!(BenchmarkMarket::leverage_external_requirements()
            .requirements
            .is_empty());
        assert!(BenchmarkMarket::hlp_external_requirements().requirements.is_empty());
    }

    #[test]
    fn owned_hlp_entry_matches_native_market_and_account_ordering() {
        let (mut benchmark, market_key) = initialized_keyed_market();
        let owner = Pubkey::new_unique();
        let mut external = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
        let mut expected_market = clone_market(benchmark.market()).unwrap();
        let mut expected_external = external.try_fork().unwrap();
        let request = BenchmarkHlpEntryRequest {
            clock: BenchmarkClock {
                slot: 2,
                unix_timestamp: 2,
            },
            target_transfer: BenchmarkTokenTransferOutcome {
                source_debit: 100_100,
                destination_credit: 100_000,
            },
            min_hlp_amount: 1,
            global_reduce_only: false,
        };

        expected_market.accrue_interest_to_slot(request.clock.slot).unwrap();
        reconcile_live_hlp_supply(
            &mut expected_market,
            MarketAsset::Base,
            expected_external.hlp_mint_supply,
        )
        .unwrap();
        expected_market.advance_amm_clock(request.clock.slot).unwrap();
        expected_market.checkpoint_hlp_vaults().unwrap();
        expected_market.assert_hlp_entry_available(MarketAsset::Base).unwrap();
        expected_market.observe_current_risk(request.clock.slot).unwrap();
        let expected_receipt = expected_market
            .deposit_single_sided(MarketAsset::Base, request.target_transfer.destination_credit, 1)
            .unwrap();
        expected_market
            .finalize_amm_transition_and_observe_risk(request.clock.slot)
            .unwrap();
        let (base_swap, base_interest) = expected_market.hlp_yield_growth_indexes(MarketAsset::Base, MarketAsset::Base);
        let (quote_swap, quote_interest) =
            expected_market.hlp_yield_growth_indexes(MarketAsset::Base, MarketAsset::Quote);
        expected_external
            .base_yield_account
            .accrue(0, base_swap, base_interest)
            .unwrap();
        expected_external
            .quote_yield_account
            .accrue(0, quote_swap, quote_interest)
            .unwrap();
        expected_external.hlp_vault_ylp_balance += expected_receipt.ylp_amount;
        expected_external.hlp_mint_supply += expected_receipt.hlp_amount;
        expected_external.holder_hlp_token_balance += expected_receipt.hlp_amount;
        validate_hlp_owned_state(&expected_external, &expected_market, market_key).unwrap();

        let execution = benchmark.execute_hlp_entry(&mut external, request).unwrap();
        assert_eq!(execution.market.receipt.native, expected_receipt);
        assert_eq!(execution.market.cash.base.reserve_vault_credit, 100_000);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&expected_market));
        assert_eq!(external.checkpoint(), expected_external.checkpoint());
    }

    #[test]
    fn failed_hlp_entry_rolls_back_market_external_state_and_clock() {
        let (mut benchmark, _) = initialized_keyed_market();
        let owner = Pubkey::new_unique();
        let mut external = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
        let market_before = market_bytes(benchmark.market());
        let external_before = external.checkpoint();
        let clock_before = benchmark.clock();

        assert!(benchmark
            .execute_hlp_entry(
                &mut external,
                BenchmarkHlpEntryRequest {
                    clock: BenchmarkClock {
                        slot: 50_000,
                        unix_timestamp: 50_000,
                    },
                    target_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: 100_000,
                        destination_credit: 100_000,
                    },
                    min_hlp_amount: u64::MAX,
                    global_reduce_only: false,
                },
            )
            .is_err());
        assert_eq!(market_bytes(benchmark.market()), market_before);
        assert_eq!(external.checkpoint(), external_before);
        assert_eq!(benchmark.clock(), clock_before);
    }

    #[test]
    fn hlp_aware_swap_atomically_settles_both_ylp_vaults() {
        let (mut benchmark, _) = initialized_keyed_market();
        let owner = Pubkey::new_unique();
        let mut base_external =
            BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
        let mut quote_external =
            BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
        benchmark
            .execute_hlp_entry(
                &mut base_external,
                BenchmarkHlpEntryRequest {
                    clock: BenchmarkClock {
                        slot: 2,
                        unix_timestamp: 2,
                    },
                    target_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: 100_000,
                        destination_credit: 100_000,
                    },
                    min_hlp_amount: 1,
                    global_reduce_only: false,
                },
            )
            .unwrap();
        benchmark
            .execute_hlp_entry(
                &mut quote_external,
                BenchmarkHlpEntryRequest {
                    clock: BenchmarkClock {
                        slot: 3,
                        unix_timestamp: 3,
                    },
                    target_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: 200_000,
                        destination_credit: 200_000,
                    },
                    min_hlp_amount: 1,
                    global_reduce_only: false,
                },
            )
            .unwrap();
        let swap_request = BenchmarkSwapRequest {
            asset_in: MarketAsset::Base,
            reserve_credit: 100_000,
            protocol_fee_bps: 0,
            protocol_auction_split: ProtocolAuctionSplit::default(),
        };
        let preview = benchmark.preview_swap(swap_request).unwrap();
        assert!(
            preview.base_rebalance.ylp_mint_amount > 0
                || preview.base_rebalance.ylp_burn_amount > 0
                || preview.quote_rebalance.ylp_mint_amount > 0
                || preview.quote_rebalance.ylp_burn_amount > 0
        );
        let execution = benchmark
            .execute_swap_with_hlp(
                &mut base_external,
                &mut quote_external,
                BenchmarkHlpAwareSwapRequest {
                    swap: swap_request,
                    base_hlp_interest_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: preview.base_rebalance.interest_paid,
                        destination_credit: preview.base_rebalance.interest_paid,
                    },
                    quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: preview.quote_rebalance.interest_paid,
                        destination_credit: preview.quote_rebalance.interest_paid,
                    },
                    protocol_interest_fee_bps: 2_000,
                    protocol_auction_split: ProtocolAuctionSplit::default(),
                },
            )
            .unwrap();
        assert_eq!(execution.swap, preview);
        assert_eq!(
            base_external.hlp_vault_ylp_balance(),
            benchmark.market().base_hlp_vault.ylp_shares
        );
        assert_eq!(
            quote_external.hlp_vault_ylp_balance(),
            benchmark.market().quote_hlp_vault.ylp_shares
        );
    }

    #[test]
    fn maximum_hlp_entry_and_pro_rata_withdrawal_exhaust_owned_state() {
        let (mut benchmark, _) = initialized_keyed_market();
        let owner = Pubkey::new_unique();
        let mut external = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
        let entry_clock = BenchmarkClock {
            slot: 2,
            unix_timestamp: 2,
        };
        let plan = benchmark
            .maximize_hlp_entry(&external, entry_clock, 100_000, false)
            .unwrap()
            .unwrap();
        assert_eq!(plan.target_reserve_credit, 100_000);
        let entry = benchmark
            .execute_maximum_hlp_entry(
                &mut external,
                plan,
                BenchmarkTokenTransferOutcome {
                    source_debit: 100_321,
                    destination_credit: plan.target_reserve_credit,
                },
                false,
            )
            .unwrap();
        assert_eq!(
            entry.external_after.hlp_vault_ylp_balance,
            entry.market.receipt.native.ylp_amount
        );
        let initial_hlp = external.holder_hlp_token_balance();
        let initial_ylp = external.hlp_vault_ylp_balance();
        let first_burn = (initial_hlp / 3).max(1);
        let first_clock = BenchmarkClock {
            slot: 1_000_002,
            unix_timestamp: 1_000_002,
        };
        let first_preview = benchmark
            .preview_hlp_withdrawal(&external, first_clock, first_burn)
            .unwrap();
        assert!(first_preview.interest_paid > 0);
        let first = benchmark
            .execute_hlp_withdraw(
                &mut external,
                BenchmarkHlpWithdrawRequest {
                    clock: first_clock,
                    hlp_amount: first_burn,
                    target_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: first_preview.target_amount_out,
                        destination_credit: first_preview.target_amount_out.saturating_sub(1),
                    },
                    interest_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: first_preview.interest_paid,
                        destination_credit: first_preview.interest_paid,
                    },
                    min_target_recipient_credit: first_preview.target_amount_out.saturating_sub(1),
                    protocol_interest_fee_bps: 2_000,
                    protocol_auction_split: ProtocolAuctionSplit::default(),
                },
            )
            .unwrap();
        assert_eq!(first.market.receipt.native, first_preview);
        assert_eq!(external.holder_hlp_token_balance(), initial_hlp - first_burn);
        assert_eq!(external.hlp_vault_ylp_balance(), initial_ylp - first_preview.ylp_amount);
        assert_eq!(
            external.checkpoint().measured_quote_interest_vault_credits,
            first_preview.interest_paid
        );

        let final_burn = external.holder_hlp_token_balance();
        let final_clock = BenchmarkClock {
            slot: 1_000_003,
            unix_timestamp: 1_000_003,
        };
        let final_preview = benchmark
            .preview_hlp_withdrawal(&external, final_clock, final_burn)
            .unwrap();
        let final_exit = benchmark
            .execute_hlp_withdraw(
                &mut external,
                BenchmarkHlpWithdrawRequest {
                    clock: final_clock,
                    hlp_amount: final_burn,
                    target_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: final_preview.target_amount_out,
                        destination_credit: final_preview.target_amount_out,
                    },
                    interest_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: final_preview.interest_paid,
                        destination_credit: final_preview.interest_paid,
                    },
                    min_target_recipient_credit: final_preview.target_amount_out,
                    protocol_interest_fee_bps: 2_000,
                    protocol_auction_split: ProtocolAuctionSplit::default(),
                },
            )
            .unwrap();
        assert_eq!(final_exit.market.receipt.native, final_preview);
        assert_eq!(external.holder_hlp_token_balance(), 0);
        assert_eq!(external.hlp_mint_supply(), 0);
        assert_eq!(external.hlp_vault_ylp_balance(), 0);
        assert_eq!(benchmark.market().base_hlp_vault.hlp_supply, 0);
        assert_eq!(benchmark.market().base_hlp_vault.ylp_shares, 0);
    }

    #[test]
    fn prepared_leverage_open_and_full_unwind_match_native_transitions_exactly() {
        let (mut benchmark, market_key) = initialized_keyed_market();
        let owner = Pubkey::new_unique();
        let position_id = Pubkey::new_unique();
        let referrer = Pubkey::new_unique();
        let referral =
            BenchmarkReferralOwnedState::initialize(&benchmark, MarketAsset::Base, referrer, referrer, 2_500, true)
                .unwrap();
        let token_balances = BenchmarkLeverageTokenBalances {
            owner_base_balance: 100_000,
            owner_quote_balance: 100_000,
            base_reserve_vault_balance: required_reserve_custody(&benchmark.market().base_side).unwrap(),
            quote_reserve_vault_balance: required_reserve_custody(&benchmark.market().quote_side).unwrap(),
            base_interest_vault_balance: benchmark.market().base_side.fees.interest_vault_balance,
            quote_interest_vault_balance: benchmark.market().quote_side.fees.interest_vault_balance,
            base_leverage_collateral_vault_balance: 0,
            quote_leverage_collateral_vault_balance: 0,
        };
        let mut leverage = BenchmarkLeverageOwnedState::initialize_for_open(
            &benchmark,
            owner,
            owner,
            position_id,
            token_balances,
            Some(referral),
        )
        .unwrap();
        let mut base_hlp = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
        let mut quote_hlp = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
        let policy = BenchmarkLeveragePolicy {
            protocol_swap_fee_bps: 0,
            protocol_interest_fee_bps: 2_000,
            protocol_auction_split: ProtocolAuctionSplit::default(),
            max_referral_interest_share_bps: 5_000,
            global_reduce_only: false,
            collateral_mint_is_leverage_eligible: true,
            launch_same_transaction_guard_satisfied: true,
        };
        let open_request = BenchmarkPrepareLeverageOpenRequest {
            clock: BenchmarkClock {
                slot: 2,
                unix_timestamp: 2,
            },
            debt_asset: MarketAsset::Base,
            margin_transfer: BenchmarkTokenTransferOutcome {
                source_debit: 1_000,
                destination_credit: 1_000,
            },
            multiplier_bps: 20_000,
            min_collateral_out: 1,
            limit_price_nad: 0,
            requested_referrer: Some(referrer),
            policy,
        };
        let prepared = benchmark
            .prepare_leverage_open(&leverage, &base_hlp, &quote_hlp, open_request)
            .unwrap();
        let open_quote = prepared.quote();
        let mut expected_market = clone_market(&prepared.prepared_market).unwrap();
        let mut expected_position = empty_leverage_position();
        let (_, position_bump) = leverage_position_pda(market_key, position_id).unwrap();
        let expected_open = expected_market
            .open_leverage(
                &mut expected_position,
                owner,
                market_key,
                position_id,
                open_quote.referral_partner,
                open_quote.referral_interest_share_bps,
                MarketAsset::Base,
                1_000,
                20_000,
                open_quote.swap.amount_out,
                prepared.prepared_swap.clone(),
                full_leverage_swap_fee_credit(open_quote.swap).unwrap(),
                2,
                2,
                position_bump,
                0,
                ProtocolAuctionSplit::default(),
            )
            .unwrap();
        let open = benchmark
            .execute_prepared_leverage_open(
                &mut leverage,
                &mut base_hlp,
                &mut quote_hlp,
                prepared,
                BenchmarkExecuteLeverageOpenRequest {
                    collateral_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: open_quote.swap.amount_out,
                        destination_credit: open_quote.swap.amount_out,
                    },
                    base_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
                    quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
                },
            )
            .unwrap();
        assert_eq!(open.market.receipt.native, expected_open);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&expected_market));
        assert_eq!(
            leverage.position().unwrap().try_to_vec().unwrap(),
            expected_position.try_to_vec().unwrap()
        );
        assert_eq!(open.market.receipt.metrics.debt_amount, expected_open.debt_amount);
        assert!(!open.market.receipt.metrics.liquidatable);
        assert_eq!(
            leverage.checkpoint().referral.unwrap().configured_interest_share_bps,
            2_500
        );

        let collateral_amount = leverage.position().unwrap().collateral_amount;
        let close_request = BenchmarkPrepareLeverageCloseRequest {
            clock: open_request.clock,
            collateral_transfer: BenchmarkTokenTransferOutcome {
                source_debit: collateral_amount,
                destination_credit: collateral_amount,
            },
            min_residual_out: 0,
            policy,
        };
        let prepared_close = benchmark
            .prepare_leverage_close(&leverage, &base_hlp, &quote_hlp, close_request)
            .unwrap();
        let close_quote = prepared_close.quote();
        let mut expected_close_market = clone_market(&prepared_close.prepared_market).unwrap();
        let mut expected_close_position = clone_leverage_position(leverage.position().unwrap()).unwrap();
        let expected_close = *expected_close_market
            .close_leverage(
                &mut expected_close_position,
                0,
                prepared_close.prepared_swap.clone(),
                full_leverage_swap_fee_credit(close_quote.swap).unwrap(),
                0,
                ProtocolAuctionSplit::default(),
                close_request.clock.slot,
            )
            .unwrap();
        assert_eq!(expected_close.interest_paid, 0);
        let close = benchmark
            .execute_prepared_leverage_close(
                &mut leverage,
                &mut base_hlp,
                &mut quote_hlp,
                prepared_close,
                BenchmarkExecuteLeverageCloseRequest {
                    residual_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: expected_close.residual,
                        destination_credit: expected_close.residual,
                    },
                    interest_transfer: BenchmarkTokenTransferOutcome::default(),
                    base_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
                    quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
                },
            )
            .unwrap();
        assert_eq!(close.market.receipt.native, expected_close);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&expected_close_market));
        assert!(leverage.position().is_none());
        assert!(!close.leverage_after.position.account_exists);
        assert_eq!(close.market.receipt.referral_interest.referral_amount, 0);
        assert_eq!(
            close.market.receipt.residual_transfer.destination_credit,
            expected_close.residual
        );
    }

    #[test]
    fn failed_prepared_leverage_execution_rolls_back_every_owned_account() {
        let (mut benchmark, _) = initialized_keyed_market();
        let owner = Pubkey::new_unique();
        let token_balances = BenchmarkLeverageTokenBalances {
            owner_base_balance: 100_000,
            owner_quote_balance: 100_000,
            base_reserve_vault_balance: required_reserve_custody(&benchmark.market().base_side).unwrap(),
            quote_reserve_vault_balance: required_reserve_custody(&benchmark.market().quote_side).unwrap(),
            base_interest_vault_balance: benchmark.market().base_side.fees.interest_vault_balance,
            quote_interest_vault_balance: benchmark.market().quote_side.fees.interest_vault_balance,
            ..BenchmarkLeverageTokenBalances::default()
        };
        let mut leverage = BenchmarkLeverageOwnedState::initialize_for_open(
            &benchmark,
            owner,
            owner,
            Pubkey::new_unique(),
            token_balances,
            None,
        )
        .unwrap();
        let mut base_hlp = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Base, owner, owner).unwrap();
        let mut quote_hlp = BenchmarkHlpOwnedState::initialize(&benchmark, MarketAsset::Quote, owner, owner).unwrap();
        let request = BenchmarkPrepareLeverageOpenRequest {
            clock: BenchmarkClock {
                slot: 2,
                unix_timestamp: 2,
            },
            debt_asset: MarketAsset::Base,
            margin_transfer: BenchmarkTokenTransferOutcome {
                source_debit: 1_000,
                destination_credit: 1_000,
            },
            multiplier_bps: 20_000,
            min_collateral_out: 1,
            limit_price_nad: 0,
            requested_referrer: None,
            policy: BenchmarkLeveragePolicy {
                protocol_swap_fee_bps: 0,
                protocol_interest_fee_bps: 0,
                protocol_auction_split: ProtocolAuctionSplit::default(),
                max_referral_interest_share_bps: BPS_DENOMINATOR,
                global_reduce_only: false,
                collateral_mint_is_leverage_eligible: true,
                launch_same_transaction_guard_satisfied: true,
            },
        };
        let prepared = benchmark
            .prepare_leverage_open(&leverage, &base_hlp, &quote_hlp, request)
            .unwrap();
        let quote = prepared.quote();
        let market_before = market_bytes(benchmark.market());
        let clock_before = benchmark.clock();
        let leverage_before = leverage.checkpoint();
        let base_before = base_hlp.checkpoint();
        let quote_before = quote_hlp.checkpoint();
        assert!(benchmark
            .execute_prepared_leverage_open(
                &mut leverage,
                &mut base_hlp,
                &mut quote_hlp,
                prepared,
                BenchmarkExecuteLeverageOpenRequest {
                    collateral_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: quote.swap.amount_out.saturating_sub(1),
                        destination_credit: quote.swap.amount_out.saturating_sub(1),
                    },
                    base_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
                    quote_hlp_interest_transfer: BenchmarkTokenTransferOutcome::default(),
                },
            )
            .is_err());
        assert_eq!(market_bytes(benchmark.market()), market_before);
        assert_eq!(benchmark.clock(), clock_before);
        assert_eq!(leverage.checkpoint(), leverage_before);
        assert_eq!(base_hlp.checkpoint(), base_before);
        assert_eq!(quote_hlp.checkpoint(), quote_before);
    }

    #[test]
    fn liquidation_bid_matches_native_transition_and_keeps_partial_collateral() {
        let (mut benchmark, mut position, _) = borrowed_liquidation_fixture(50_000);
        let clock = BenchmarkClock {
            slot: 4,
            unix_timestamp: 4,
        };
        benchmark
            .execute_start_liquidation_auction(
                &mut position,
                BenchmarkStartLiquidationAuctionRequest {
                    clock,
                    debt_asset: MarketAsset::Base,
                },
            )
            .unwrap();
        let plan_request = BenchmarkLiquidationPlanRequest {
            clock,
            debt_asset: MarketAsset::Base,
            phase: BenchmarkLiquidationPhase::Bid,
            max_repay_credit: 1_000,
            collateral_reserve_credit: 0,
            protocol_swap_fee_bps: 0,
            protocol_auction_split: ProtocolAuctionSplit::default(),
        };
        let preview_request = BenchmarkLiquidationPreviewRequest {
            plan: plan_request,
            insurance_draw_credit: 0,
        };
        let preview = benchmark.preview_liquidation(&position, preview_request).unwrap();
        assert_eq!(preview.native.socialized_loss, 0);
        assert!(preview.native.collateral_seized < 50_000);

        let mut native_market = clone_market(benchmark.market()).unwrap();
        let mut native_position = clone_borrow_position(position.position()).unwrap();
        advance_market_to_slot(&mut native_market, clock.slot).unwrap();
        let native_plan = liquidation_plan(&mut native_market, &mut native_position, plan_request).unwrap();
        let native_receipt = native_market
            .settle_liquidation(
                &mut native_position,
                MarketAsset::Base,
                native_plan.repay_credit,
                0,
                0,
                0,
                native_plan.terms,
                LiquidationPricing::ReferencePrice {
                    debt_per_collateral_price_nad: native_plan.auction_price_nad,
                },
            )
            .unwrap();
        if native_receipt.interest_paid > 0 {
            native_market
                .base_side
                .record_interest_credit(native_receipt.interest_paid, 0, ProtocolAuctionSplit::default(), 0)
                .unwrap();
        }
        native_market.finalize_amm_transition(clock.slot).unwrap();
        native_market.refresh_risk_at_slot(clock.slot).unwrap();

        let mut balances = benchmark
            .liquidation_token_balances_at_custody_floor(1_000, 0, 0, 300_000)
            .unwrap();
        let execution = benchmark
            .execute_liquidation(
                &mut position,
                &mut balances,
                BenchmarkLiquidationExecuteRequest {
                    preview: preview_request,
                    max_repay_source_debit: 1_000,
                    debt_transfer: exact_transfer(preview.plan.repay_credit),
                    insurance_draw_transfer: BenchmarkTokenTransferOutcome::default(),
                    interest_transfer: exact_transfer(preview.native.interest_paid),
                    collateral_transfer: exact_transfer(preview.native.collateral_to_liquidator),
                    collateral_swap_transfer: BenchmarkTokenTransferOutcome::default(),
                    owner_residual_transfer: BenchmarkTokenTransferOutcome::default(),
                    insurance_funding_transfer: exact_transfer(preview.native.insurance_funded),
                    min_collateral_recipient_credit: 0,
                    protocol_interest_fee_bps: 0,
                    protocol_auction_split: ProtocolAuctionSplit::default(),
                    referral_interest_amount: 0,
                },
            )
            .unwrap();
        assert_eq!(execution.market.receipt.native, native_receipt);
        assert_eq!(market_bytes(benchmark.market()), market_bytes(&native_market));
        assert_eq!(position_bytes(position.position()), position_bytes(&native_position));
        assert!(execution.position_after.quote_collateral > 0);
    }

    #[test]
    fn floor_shortfall_closes_automatically_and_late_failure_rolls_back() {
        let (mut benchmark, mut position, _) = borrowed_liquidation_fixture(1_000);
        let trigger = benchmark
            .execute_start_liquidation_auction(
                &mut position,
                BenchmarkStartLiquidationAuctionRequest {
                    clock: BenchmarkClock {
                        slot: 4,
                        unix_timestamp: 4,
                    },
                    debt_asset: MarketAsset::Base,
                },
            )
            .unwrap();
        let floor_clock = BenchmarkClock {
            slot: 100,
            unix_timestamp: trigger.market.receipt.first_floor_unix_timestamp,
        };
        let plan_request = BenchmarkLiquidationPlanRequest {
            clock: floor_clock,
            debt_asset: MarketAsset::Base,
            phase: BenchmarkLiquidationPhase::Floor,
            max_repay_credit: 0,
            collateral_reserve_credit: 995,
            protocol_swap_fee_bps: 0,
            protocol_auction_split: ProtocolAuctionSplit::default(),
        };
        let preview_request = BenchmarkLiquidationPreviewRequest {
            plan: plan_request,
            insurance_draw_credit: 0,
        };
        let preview = benchmark.preview_liquidation(&position, preview_request).unwrap();
        assert_eq!(preview.plan.caller_bounty, 5);
        assert_eq!(preview.plan.collateral_swap_debit, 995);
        assert_eq!(preview.native.collateral_seized, 1_000);
        assert!(preview.native.socialized_loss > 0);
        let mut balances = benchmark
            .liquidation_token_balances_at_custody_floor(0, 0, 0, 300_000)
            .unwrap();
        let valid_request = BenchmarkLiquidationExecuteRequest {
            preview: preview_request,
            max_repay_source_debit: 0,
            debt_transfer: BenchmarkTokenTransferOutcome::default(),
            insurance_draw_transfer: BenchmarkTokenTransferOutcome::default(),
            interest_transfer: exact_transfer(preview.native.interest_paid),
            collateral_transfer: exact_transfer(preview.plan.caller_bounty),
            collateral_swap_transfer: exact_transfer(preview.plan.collateral_swap_debit),
            owner_residual_transfer: exact_transfer(preview.owner_residual),
            insurance_funding_transfer: BenchmarkTokenTransferOutcome::default(),
            min_collateral_recipient_credit: 0,
            protocol_interest_fee_bps: 0,
            protocol_auction_split: ProtocolAuctionSplit::default(),
            referral_interest_amount: 0,
        };
        let market_before = market_bytes(benchmark.market());
        let position_before = position_bytes(position.position());
        let balances_before = balances;
        assert!(benchmark
            .execute_liquidation(
                &mut position,
                &mut balances,
                BenchmarkLiquidationExecuteRequest {
                    min_collateral_recipient_credit: preview.plan.caller_bounty.saturating_add(1),
                    ..valid_request
                },
            )
            .is_err());
        assert_eq!(market_bytes(benchmark.market()), market_before);
        assert_eq!(position_bytes(position.position()), position_before);
        assert_eq!(balances, balances_before);

        let execution = benchmark
            .execute_liquidation(&mut position, &mut balances, valid_request)
            .unwrap();
        assert_eq!(
            execution.market.receipt.native.socialized_loss,
            preview.native.socialized_loss
        );
        assert_eq!(execution.market.receipt.native.remaining_debt, 0);
        assert_eq!(execution.position_after.quote_collateral, 0);
        assert_eq!(execution.position_after.auction_debt_asset, u8::MAX);
    }

    #[test]
    fn floor_closes_when_transfer_fees_leave_no_collateral_for_the_amm() {
        let (mut benchmark, mut position, _) = borrowed_liquidation_fixture(1);
        let trigger = benchmark
            .execute_start_liquidation_auction(
                &mut position,
                BenchmarkStartLiquidationAuctionRequest {
                    clock: BenchmarkClock {
                        slot: 4,
                        unix_timestamp: 4,
                    },
                    debt_asset: MarketAsset::Base,
                },
            )
            .unwrap();
        let floor_clock = BenchmarkClock {
            slot: 100,
            unix_timestamp: trigger.market.receipt.first_floor_unix_timestamp,
        };
        let preview_request = BenchmarkLiquidationPreviewRequest {
            plan: BenchmarkLiquidationPlanRequest {
                clock: floor_clock,
                debt_asset: MarketAsset::Base,
                phase: BenchmarkLiquidationPhase::Floor,
                max_repay_credit: 0,
                collateral_reserve_credit: 0,
                protocol_swap_fee_bps: 0,
                protocol_auction_split: ProtocolAuctionSplit::default(),
            },
            insurance_draw_credit: 0,
        };
        let preview = benchmark.preview_liquidation(&position, preview_request).unwrap();
        assert_eq!(preview.plan.collateral_consumed, 1);
        assert_eq!(preview.plan.caller_bounty, 0);
        assert_eq!(preview.plan.swap_output, 0);
        assert_eq!(preview.native.repaid_amount, 0);
        assert!(preview.native.socialized_loss > 0);

        let mut balances = benchmark
            .liquidation_token_balances_at_custody_floor(0, 0, 0, 300_000)
            .unwrap();
        let execution = benchmark
            .execute_liquidation(
                &mut position,
                &mut balances,
                BenchmarkLiquidationExecuteRequest {
                    preview: preview_request,
                    max_repay_source_debit: 0,
                    debt_transfer: BenchmarkTokenTransferOutcome::default(),
                    insurance_draw_transfer: BenchmarkTokenTransferOutcome::default(),
                    interest_transfer: BenchmarkTokenTransferOutcome::default(),
                    collateral_transfer: BenchmarkTokenTransferOutcome::default(),
                    collateral_swap_transfer: BenchmarkTokenTransferOutcome {
                        source_debit: 1,
                        destination_credit: 0,
                    },
                    owner_residual_transfer: BenchmarkTokenTransferOutcome::default(),
                    insurance_funding_transfer: BenchmarkTokenTransferOutcome::default(),
                    min_collateral_recipient_credit: 0,
                    protocol_interest_fee_bps: 0,
                    protocol_auction_split: ProtocolAuctionSplit::default(),
                    referral_interest_amount: 0,
                },
            )
            .unwrap();
        assert_eq!(execution.market.receipt.native.remaining_debt, 0);
        assert_eq!(execution.position_after.quote_collateral, 0);
        assert_eq!(execution.position_after.auction_debt_asset, u8::MAX);
    }

    #[test]
    fn account_round_trip_preserves_every_market_atom() {
        let benchmark = initialized_market();
        let fork = benchmark.try_fork().unwrap();
        assert_eq!(market_bytes(benchmark.market()), market_bytes(fork.market()));
        assert_eq!(benchmark.clock(), fork.clock());
    }

    #[test]
    fn public_interest_transition_witness_covers_zero_repay_and_writeoff() {
        let zero = BenchmarkPublicInterestTransition::checked(
            BenchmarkPublicInterestCheckpoint { base: 7, quote: 9 },
            BenchmarkPublicInterestCheckpoint { base: 7, quote: 9 },
            BenchmarkPublicInterestCheckpoint { base: 7, quote: 9 },
            BenchmarkPublicInterestPayments::default(),
        )
        .unwrap();
        assert_eq!(
            zero,
            BenchmarkPublicInterestTransition::identity(BenchmarkPublicInterestCheckpoint { base: 7, quote: 9 })
        );

        let repay = BenchmarkPublicInterestTransition::checked(
            BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
            BenchmarkPublicInterestCheckpoint { base: 120, quote: 0 },
            BenchmarkPublicInterestCheckpoint { base: 70, quote: 0 },
            BenchmarkPublicInterestPayments {
                base: BenchmarkPublicInterestPayment {
                    gross_cash_interest_paid: 50,
                    net_interest_vault_credit: 49,
                },
                quote: BenchmarkPublicInterestPayment::default(),
            },
        )
        .unwrap();
        assert_eq!(repay.base.clock_accrued, 20);
        assert_eq!(repay.base.total_interest_removed, 50);
        assert_eq!(repay.base.interest_written_off, 0);
        assert_eq!(repay.base.net_interest_vault_credit, 49);

        let liquidation = BenchmarkPublicInterestTransition::checked(
            BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
            BenchmarkPublicInterestCheckpoint { base: 120, quote: 0 },
            BenchmarkPublicInterestCheckpoint { base: 10, quote: 0 },
            BenchmarkPublicInterestPayments {
                base: BenchmarkPublicInterestPayment {
                    gross_cash_interest_paid: 40,
                    net_interest_vault_credit: 40,
                },
                quote: BenchmarkPublicInterestPayment::default(),
            },
        )
        .unwrap();
        assert_eq!(liquidation.base.total_interest_removed, 110);
        assert_eq!(liquidation.base.interest_written_off, 70);
        assert_eq!(
            liquidation.base.gross_cash_interest_paid + liquidation.base.interest_written_off,
            liquidation.base.total_interest_removed
        );
    }

    #[test]
    fn public_interest_transition_exposes_share_rounding_and_fails_closed() {
        let rounded = BenchmarkPublicInterestTransition::checked(
            BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
            BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
            BenchmarkPublicInterestCheckpoint { base: 101, quote: 0 },
            BenchmarkPublicInterestPayments::default(),
        )
        .unwrap();
        assert_eq!(rounded.base.transition_interest_created, 1);
        assert_eq!(rounded.base.total_interest_removed, 0);

        assert!(BenchmarkPublicInterestTransition::checked(
            BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
            BenchmarkPublicInterestCheckpoint { base: 100, quote: 0 },
            BenchmarkPublicInterestCheckpoint { base: 99, quote: 0 },
            BenchmarkPublicInterestPayments {
                base: BenchmarkPublicInterestPayment {
                    gross_cash_interest_paid: 2,
                    net_interest_vault_credit: 2,
                },
                quote: BenchmarkPublicInterestPayment::default(),
            },
        )
        .is_err());
    }

    #[test]
    fn public_interest_checkpoint_clamps_rounding_principal_above_debt() {
        let mut benchmark = initialized_market();
        benchmark.market.debt.fixed_base_shares = 1;
        benchmark.market.debt.fixed_base_principal = 2;
        benchmark.market.debt.isolated_base_shares = 1;
        benchmark.market.debt.isolated_base_principal = 2;
        let checkpoint = benchmark.public_interest_checkpoint().unwrap();
        assert_eq!(checkpoint.base, 0);
        assert_eq!(checkpoint.quote, 0);
    }
}

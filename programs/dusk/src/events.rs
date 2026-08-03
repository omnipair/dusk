use crate::state::{LeverageSwapFeeCredit, LeverageSwapQuote, MarketConfig};
use anchor_lang::prelude::*;

pub mod log;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct MarketEventMetadata {
    pub signer: Pubkey,
    pub market: Pubkey,
    pub slot: u64,
}

impl MarketEventMetadata {
    pub fn new(signer: Pubkey, market: Pubkey) -> Result<Self> {
        Ok(Self {
            signer,
            market,
            slot: Clock::get()?.slot,
        })
    }
}

#[event]
pub struct MarketCreated {
    pub market: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub ylp_mint: Pubkey,
    pub base_collateral_vault: Pubkey,
    pub quote_collateral_vault: Pubkey,
    pub base_insurance_vault: Pubkey,
    pub quote_insurance_vault: Pubkey,
    pub base_hlp_mint: Pubkey,
    pub quote_hlp_mint: Pubkey,
    pub operator: Pubkey,
    pub manager: Pubkey,
    pub target_hlp_leverage_bps: u16,
    pub swap_fee_bps: u16,
    pub manager_fee_bps: u16,
    pub protocol_fee_bps: u16,
    pub config: MarketConfig,
    pub params_hash: [u8; 32],
    pub version: u8,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketUpdated {
    pub market: Pubkey,
    pub reduce_only: bool,
    pub target_hlp_leverage_bps: u16,
    pub swap_fee_bps: u16,
    pub manager_fee_bps: u16,
    pub protocol_fee_bps: u16,
    pub config: MarketConfig,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketConfigUpdateScheduled {
    pub market: Pubkey,
    pub execute_after_slot: u64,
    pub target_hlp_leverage_bps: u16,
    pub swap_fee_bps: u16,
    pub manager_fee_bps: u16,
    pub protocol_fee_bps: u16,
    pub config: MarketConfig,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketAuthorityUpdated {
    pub market: Pubkey,
    pub manager: Pubkey,
    pub operator: Pubkey,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketAuthorityUpdateScheduled {
    pub market: Pubkey,
    pub role: u8,
    pub pending_authority: Pubkey,
    pub execute_after_slot: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketHealthUpdated {
    pub market: Pubkey,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub effective_base_debt_nad: u128,
    pub effective_quote_debt_nad: u128,
    pub base_debt_health_bps: u64,
    pub quote_debt_health_bps: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct LiquidityAdded {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub base_reserve_credit: u64,
    pub quote_reserve_credit: u64,
    pub ylp_amount: u64,
    pub ylp_supply: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct LiquidityRemoved {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub ylp_amount: u64,
    pub base_amount_out: u64,
    pub quote_amount_out: u64,
    pub ylp_supply: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct YieldRecipientUpdated {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub asset_mint: Pubkey,
    pub token_kind: u8,
    pub recipient: Pubkey,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct YieldClaimed {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub asset_mint: Pubkey,
    pub token_kind: u8,
    pub recipient: Pubkey,
    pub swap_fee_amount: u64,
    pub interest_amount: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketFeeLiabilityClaimed {
    pub market: Pubkey,
    pub authority: Pubkey,
    pub asset_mint: Pubkey,
    pub claim_kind: u8,
    pub fee_amount: u64,
    pub remaining_fee_liability: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct ManagerFeesClaimed {
    pub market: Pubkey,
    pub manager: Pubkey,
    pub asset_mint: Pubkey,
    pub swap_fee_amount: u64,
    pub interest_fee_amount: u64,
    pub remaining_manager_swap_fee_liability: u64,
    pub remaining_manager_interest_fee_liability: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct ProtocolAuctionConfigUpdated {
    pub authority: Pubkey,
    pub lane: u8,
    pub accepted_mint: Pubkey,
    pub start_multiplier_bps: u16,
    pub floor_multiplier_bps: u16,
    pub duration_slots: u64,
    pub max_reference_age_slots: u64,
    pub signer: Pubkey,
}

#[event]
pub struct ProtocolAuctionRecipientsUpdated {
    pub authority: Pubkey,
    pub lane: u8,
    pub treasury: Pubkey,
    pub staking_vault: Pubkey,
    pub treasury_bps: u16,
    pub staking_vault_bps: u16,
    pub signer: Pubkey,
}

#[event]
pub struct ProtocolAuctionSplitUpdated {
    pub authority: Pubkey,
    pub fee_auction_bps: u16,
    pub buyback_auction_bps: u16,
    pub signer: Pubkey,
}

#[event]
pub struct ReferralInterestShareCapUpdated {
    pub authority: Pubkey,
    pub max_referral_interest_share_bps: u16,
    pub signer: Pubkey,
}

#[event]
pub struct ReferralPartnerConfigured {
    pub referral_partner: Pubkey,
    pub authority: Pubkey,
    pub recipient: Pubkey,
    pub interest_share_bps: u16,
    pub active: bool,
    pub signer: Pubkey,
}

#[event]
pub struct ReferralRecipientUpdated {
    pub referral_partner: Pubkey,
    pub authority: Pubkey,
    pub recipient: Pubkey,
}

#[event]
pub struct ReferralInterestClaimed {
    pub market: Pubkey,
    pub referral_partner: Pubkey,
    pub referral_accrual: Pubkey,
    pub authority: Pubkey,
    pub recipient: Pubkey,
    pub asset_mint: Pubkey,
    pub vault_debit: u64,
    pub recipient_credit: u64,
    pub remaining_accrual: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct ReferralInterestAccrued {
    pub market: Pubkey,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub referrer: Pubkey,
    pub referral_partner: Pubkey,
    pub referral_accrual: Pubkey,
    pub asset_mint: Pubkey,
    pub interest_paid: u64,
    pub interest_vault_credit: u64,
    pub protocol_interest_revenue: u64,
    pub interest_share_bps: u16,
    pub accrued_amount: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct ReferralBound {
    pub market: Pubkey,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub referrer: Pubkey,
    pub referral_partner: Pubkey,
    pub asset_mint: Pubkey,
    pub interest_share_bps: u16,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct ProtocolAuctionSettled {
    pub market: Pubkey,
    pub reference_market: Pubkey,
    pub lane: u8,
    pub side: u8,
    pub bidder: Pubkey,
    pub sold_mint: Pubkey,
    pub accepted_mint: Pubkey,
    pub sold_amount: u64,
    pub payment_amount: u64,
    pub treasury_amount: u64,
    pub staking_vault_amount: u64,
    pub reference_price_nad: u64,
    pub auction_price_nad: u64,
    pub remaining_fee_liability: u64,
    pub remaining_buyback_liability: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct SwapExecuted {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub asset_in_mint: Pubkey,
    pub asset_out_mint: Pubkey,
    pub reserve_credit: u64,
    pub amount_in_after_fee: u64,
    pub amount_out: u64,
    pub fee_credit: u64,
    pub base_hlp_pending_rebalance: i128,
    pub quote_hlp_pending_rebalance: i128,
    pub metadata: MarketEventMetadata,
    pub fee_breakdown: SwapFeeBreakdownEvent,
    pub start_price_nad: u64,
    /// Legacy name for the invariant-preserving trade endpoint.
    pub end_price_nad: u64,
    /// Final pool marginal price after retained surcharge enters reserves.
    pub reserve_end_price_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    pub base_fee_credit: u64,
    pub distributed_surcharge_credit: u64,
}

#[event]
pub struct SwapSettled {
    pub market: Pubkey,
    pub trader: Pubkey,
    pub asset_in_side: u8,
    pub reserve_credit: u64,
    pub amount_in_after_fee: u64,
    pub amount_out: u64,
    pub fee_credit: u64,
    pub base_hlp_pending_rebalance: i128,
    pub quote_hlp_pending_rebalance: i128,
    pub fee_breakdown: SwapFeeBreakdownEvent,
    pub start_price_nad: u64,
    /// Legacy name for the invariant-preserving trade endpoint.
    pub end_price_nad: u64,
    /// Final pool marginal price after retained surcharge enters reserves.
    pub reserve_end_price_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    pub base_fee_credit: u64,
    pub distributed_surcharge_credit: u64,
}

/// Full quote-time fee accounting embedded in both swap events. Legacy event
/// fields remain in place so existing consumers can migrate incrementally.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwapFeeBreakdownEvent {
    pub reserve_credit: u64,
    pub base_fee_debit: u64,
    pub divergence_surcharge_debit: u64,
    pub volatility_surcharge_debit: u64,
    pub dynamic_surcharge_debit: u64,
    pub total_fee_debit: u64,
    pub retained_surcharge: u64,
    pub distributed_surcharge_debit: u64,
    pub amount_in_for_quote: u64,
    pub reserve_input_credit: u64,
    pub claimable_fee_debit: u64,
    pub base_fee_rate_nad: u64,
    pub divergence_fee_rate_nad: u64,
    pub volatility_fee_rate_nad: u64,
    pub total_fee_rate_nad: u64,
}

/// Fee and endpoint telemetry for an AMM leg embedded in a leverage action.
/// `None` on `LeveragePositionUpdated` means the action was margin-only.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeverageSwapEvent {
    pub asset_in_side: u8,
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_breakdown: SwapFeeBreakdownEvent,
    pub start_price_nad: u64,
    /// Invariant-preserving trade endpoint; retained principal is excluded.
    pub end_price_nad: u64,
    /// Final executable-reserve marginal price after retained principal.
    pub reserve_end_price_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    pub base_fee_credit: u64,
    pub distributed_surcharge_credit: u64,
}

impl LeverageSwapEvent {
    pub fn new(quote: LeverageSwapQuote, credit: LeverageSwapFeeCredit) -> Self {
        Self {
            asset_in_side: quote.asset_in,
            amount_in: quote.amount_in,
            amount_out: quote.amount_out,
            fee_breakdown: quote.fee_breakdown.into(),
            start_price_nad: quote.start_price_nad,
            end_price_nad: quote.end_price_nad,
            reserve_end_price_nad: quote.reserve_end_price_nad,
            decayed_volatility_nad: quote.decayed_volatility_nad,
            post_success_volatility_nad: quote.post_success_volatility_nad,
            base_fee_credit: credit.base,
            distributed_surcharge_credit: credit.distributed_surcharge,
        }
    }
}

#[event]
pub struct LeveragePositionOpened {
    pub market: Pubkey,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub debt_asset_mint: Pubkey,
    pub collateral_asset_mint: Pubkey,
    pub margin_amount: u64,
    pub borrowed_amount: u64,
    pub debt_amount: u64,
    pub debt_shares: u128,
    pub collateral_amount: u64,
    pub closeout_value: u64,
    pub equity: u64,
    pub multiplier_bps: u64,
    pub swap: LeverageSwapEvent,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct LeveragePositionClosed {
    pub market: Pubkey,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub debt_asset_mint: Pubkey,
    pub collateral_asset_mint: Pubkey,
    pub debt_repaid: u64,
    pub interest_paid: u64,
    pub collateral_sold: u64,
    pub closeout_value: u64,
    pub residual: u64,
    pub swap: LeverageSwapEvent,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct LeveragePositionUpdated {
    pub market: Pubkey,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub debt_asset_mint: Pubkey,
    pub collateral_asset_mint: Pubkey,
    pub borrowed_amount: u64,
    pub debt_delta: i64,
    pub collateral_delta: i64,
    pub debt_amount: u64,
    pub debt_shares: u128,
    pub collateral_amount: u64,
    pub closeout_value: u64,
    pub swap: Option<LeverageSwapEvent>,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct LeveragePositionLiquidated {
    pub market: Pubkey,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub liquidator: Pubkey,
    pub debt_asset_mint: Pubkey,
    pub collateral_asset_mint: Pubkey,
    pub debt_repaid: u64,
    pub interest_paid: u64,
    pub principal_written_off: u64,
    pub collateral_sold: u64,
    pub closeout_value: u64,
    pub liquidator_amount: u64,
    pub owner_residual: u64,
    pub swap: LeverageSwapEvent,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct LeverageDelegationUpdated {
    pub market: Pubkey,
    pub delegation: Pubkey,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub delegated_program: Pubkey,
    pub approved_actions: u32,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketCollateralDeposited {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub asset_mint: Pubkey,
    pub collateral_credit: u64,
    pub base_collateral: u64,
    pub quote_collateral: u64,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketCollateralWithdrawn {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub asset_mint: Pubkey,
    pub collateral_debit: u64,
    pub asset_credit: u64,
    pub base_collateral: u64,
    pub quote_collateral: u64,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketDebtUpdated {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub debt_asset_mint: Pubkey,
    pub debt_delta: i64,
    pub fixed_base_debt: u128,
    pub fixed_quote_debt: u128,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
    pub base_debt_health_bps: u64,
    pub quote_debt_health_bps: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct PositionLiquidated {
    pub market: Pubkey,
    pub borrow_position: Pubkey,
    pub borrower: Pubkey,
    pub liquidator: Pubkey,
    pub debt_asset_mint: Pubkey,
    pub collateral_asset_mint: Pubkey,
    pub repaid_amount: u64,
    pub collateral_seized: u64,
    pub collateral_to_liquidator: u64,
    pub insurance_funded: u64,
    pub insurance_drawn: u64,
    pub socialized_loss: u64,
    pub remaining_debt: u128,
    pub remaining_global_health_contribution: u64,
    pub remaining_liquidation_cf_bps: u16,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct HlpOpened {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub asset_mint: Pubkey,
    pub deposit_amount: u64,
    pub borrowed_amount: u64,
    pub ylp_amount: u64,
    pub hlp_amount: u64,
    pub hlp_supply: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct HlpClosed {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub asset_mint: Pubkey,
    pub hlp_amount: u64,
    pub ylp_amount: u64,
    pub target_amount_out: u64,
    pub debt_repaid: u64,
    pub interest_paid: u64,
    pub hlp_supply: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct HlpRebalanced {
    pub market: Pubkey,
    pub target_side: u8,
    pub ideal_delta: i128,
    pub executed_delta: i128,
    pub pending_rebalance: i128,
    pub nav_nad: u128,
    pub metadata: MarketEventMetadata,
}

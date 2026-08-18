use crate::{
    errors::ErrorCode,
    market::{LeverageSwapFeeCredit, LeverageSwapQuote},
    state::MarketConfig,
};
use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct MarketEventMetadata {
    pub signer: Pubkey,
    pub market: Pubkey,
    pub slot: u64,
}

impl MarketEventMetadata {
    pub fn new(signer: Pubkey, market: Pubkey) -> Result<Self> {
        Ok(Self::at_slot(signer, market, Clock::get()?.slot))
    }

    pub const fn at_slot(signer: Pubkey, market: Pubkey, slot: u64) -> Self {
        Self { signer, market, slot }
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
    pub target_hlp_leverage_bps: u16,
    pub swap_fee_bps: u16,
    pub config: MarketConfig,
    pub params_hash: [u8; 32],
    pub initial_liquidity_authority: Pubkey,
    pub launch_reference_price_nad: u64,
    pub launch_fee_progress_offset: u16,
    pub version: u8,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct MarketReduceOnlyUpdated {
    pub market: Pubkey,
    pub reduce_only: bool,
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
pub struct InsuranceDonated {
    pub market: Pubkey,
    pub donor: Pubkey,
    /// `0` for Base and `1` for Quote.
    pub asset: u8,
    /// Gross amount requested from the donor's token account.
    pub requested_amount: u64,
    /// Net amount received after any Token-2022 transfer fee.
    pub credited_amount: u64,
    pub available_after: u64,
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
    pub base_live_reserve: u64,
    pub quote_live_reserve: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct LiquidityRemoved {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub ylp_amount: u64,
    /// Gross amounts debited from the reserve vaults.
    pub base_reserve_debit: u64,
    pub quote_reserve_debit: u64,
    /// Net amounts credited to the owner after any Token-2022 transfer fee.
    pub base_owner_credit: u64,
    pub quote_owner_credit: u64,
    pub ylp_supply: u64,
    pub base_live_reserve: u64,
    pub quote_live_reserve: u64,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct YieldRecipientUpdated {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub lp_mint: Pubkey,
    pub asset_mint: Pubkey,
    pub token_kind: u8,
    pub recipient: Pubkey,
    pub metadata: MarketEventMetadata,
}

#[event]
pub struct YieldClaimed {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub lp_mint: Pubkey,
    pub asset_mint: Pubkey,
    pub token_kind: u8,
    pub recipient: Pubkey,
    pub swap_fee_amount: u64,
    pub interest_amount: u64,
    /// Total amount credited to the recipient after transfer fees.
    pub recipient_credit: u64,
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
pub struct ProtocolAuctionRouteUpdated {
    pub authority: Pubkey,
    pub market: Pubkey,
    pub lane: u8,
    pub side: u8,
    pub sold_mint: Pubkey,
    pub accepted_mint: Pubkey,
    /// `Pubkey::default()` restores the direct-market-only policy.
    pub reference_market: Pubkey,
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
    pub source: u8,
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
    /// `0` for base input and `1` for quote input.
    pub asset_in_side: u8,
    /// Exact amount debited from the trader's input account.
    pub amount_in: u64,
    /// Amount credited to the trader after any output transfer fee.
    pub amount_out: u64,
    /// Curve output before an output-denominated fee and transfer fee.
    pub gross_amount_out: u64,
    /// `0` for Base and `1` for Quote.
    pub fee_asset_side: u8,
    /// Input applied to the invariant after all swap fees.
    pub amount_in_after_fee: u64,
    pub base_fee: u64,
    pub divergence_fee: u64,
    pub volatility_fee: u64,
    /// Dynamic surcharge retained as executable principal.
    pub retained_fee: u64,
    /// LP-owned fee compounded into reserve principal.
    pub compounded_fee: u64,
    /// Extra output funded by deleveraging the stressed hLP.
    pub hlp_recovery_target_asset: u8,
    pub hlp_recovery_funding_gap: u64,
    pub hlp_recovery_matched_input: u64,
    pub hlp_recovery_bonus_output: u64,
    pub hlp_recovery_discount_bps: u16,
    pub hlp_recovery_critical: bool,
    /// Final executable reserves after retention and inline hLP correction.
    pub base_live_reserve: u64,
    pub quote_live_reserve: u64,
}

/// Actual AMM receipt embedded in a leverage action.
/// `None` on `LeveragePositionUpdated` means the action was margin-only.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeverageSwapReceipt {
    pub asset_in_side: u8,
    pub fee_asset_side: u8,
    pub amount_in: u64,
    pub amount_out: u64,
    pub gross_amount_out: u64,
    pub amount_in_after_fee: u64,
    pub base_fee: u64,
    pub divergence_fee: u64,
    pub volatility_fee: u64,
    pub retained_fee: u64,
    pub compounded_fee: u64,
    /// Actual reserve-vault credit after any Token-2022 transfer fee.
    pub claimable_fee_credit: u64,
    /// Final executable reserves after retention and inline hLP correction.
    pub base_live_reserve: u64,
    pub quote_live_reserve: u64,
}

impl LeverageSwapReceipt {
    pub fn new(
        quote: LeverageSwapQuote,
        credit: LeverageSwapFeeCredit,
        base_live_reserve: u64,
        quote_live_reserve: u64,
    ) -> Result<Self> {
        Ok(Self {
            asset_in_side: quote.asset_in,
            fee_asset_side: quote.fee_breakdown.fee_asset,
            amount_in: quote.amount_in,
            amount_out: quote.amount_out,
            gross_amount_out: quote.gross_amount_out,
            amount_in_after_fee: quote.amount_in_after_fee,
            base_fee: quote.fee_breakdown.base_fee_debit,
            divergence_fee: quote.fee_breakdown.divergence_surcharge_debit,
            volatility_fee: quote.fee_breakdown.volatility_surcharge_debit,
            retained_fee: quote.fee_breakdown.retained_surcharge,
            compounded_fee: quote.fee_breakdown.compounded_fee_debit,
            claimable_fee_credit: credit
                .base
                .checked_add(credit.distributed_surcharge)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            base_live_reserve,
            quote_live_reserve,
        })
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
    pub swap: LeverageSwapReceipt,
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
    pub swap: LeverageSwapReceipt,
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
    /// Net tokens paid to the owner by this update, if any.
    pub owner_credit: u64,
    pub swap: Option<LeverageSwapReceipt>,
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
    pub swap: LeverageSwapReceipt,
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
    pub position: Pubkey,
    pub owner: Pubkey,
    pub debt_asset_mint: Pubkey,
    pub debt_delta: i64,
    /// Gross source-account debit and net destination-account credit.
    pub cash_debit: u64,
    pub cash_credit: u64,
    pub interest_paid: u64,
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
pub struct BorrowPositionLiquidated {
    pub market: Pubkey,
    pub borrow_position: Pubkey,
    pub borrower: Pubkey,
    pub liquidator: Pubkey,
    /// `0` for base debt and `1` for quote debt. The collateral is the other side.
    pub debt_asset_side: u8,
    pub repaid_amount: u64,
    pub collateral_seized: u64,
    /// Gross collateral debited for the liquidator's transfer. Token-2022 may
    /// reduce the liquidator's net account credit.
    pub collateral_to_liquidator: u64,
    /// Net collateral credited to the liquidator after any transfer fee.
    pub collateral_credit: u64,
    pub insurance_drawn: u64,
    pub socialized_loss: u64,
    pub remaining_debt: u128,
}

#[event]
pub struct HlpOpened {
    pub market: Pubkey,
    pub owner: Pubkey,
    /// `0` for base hLP and `1` for quote hLP.
    pub asset_side: u8,
    /// Net reserve-vault credit after any input transfer fee.
    pub deposit_amount: u64,
    pub borrowed_amount: u64,
    pub ylp_amount: u64,
    pub hlp_amount: u64,
    pub ylp_supply: u64,
    pub hlp_supply: u64,
    pub base_live_reserve: u64,
    pub quote_live_reserve: u64,
}

#[event]
pub struct HlpClosed {
    pub market: Pubkey,
    pub owner: Pubkey,
    /// `0` for base hLP and `1` for quote hLP.
    pub asset_side: u8,
    pub hlp_amount: u64,
    pub ylp_amount: u64,
    /// Amount credited to the owner after any output transfer fee.
    pub amount_out: u64,
    pub debt_repaid: u64,
    pub interest_paid: u64,
    pub ylp_supply: u64,
    pub hlp_supply: u64,
    pub base_live_reserve: u64,
    pub quote_live_reserve: u64,
}

#[event]
pub struct HlpTerminalLiquidated {
    pub market: Pubkey,
    pub caller: Pubkey,
    pub target_asset: u8,
    pub debt_closed: u64,
    pub ylp_burned: u64,
    pub interest_paid: u64,
    pub insurance_drawn: u64,
    pub socialized_loss: u64,
    /// Existing holder tokens remain burnable for zero principal while their
    /// already-checkpointed fee claims remain independently claimable.
    pub remaining_hlp_supply: u64,
}

#[event]
pub struct ParameterProposalCreated {
    pub proposal: Pubkey,
    pub market: Pubkey,
    pub proposer: Pubkey,
    pub nonce: u64,
    pub family: u8,
    pub family_revision: u64,
    pub digest: [u8; 32],
    pub sponsorship_floor: u64,
    pub initial_support: u64,
    pub status: u8,
}

#[event]
pub struct ParameterProposalSupported {
    pub proposal: Pubkey,
    pub supporter: Pubkey,
    pub amount: u64,
    pub supporter_locked: u64,
    pub total_locked: u64,
    pub status: u8,
}

#[event]
pub struct ParameterProposalQueued {
    pub proposal: Pubkey,
    pub total_locked: u64,
    pub eligible_supply: u64,
    pub queued_at: i64,
    pub execute_after: i64,
    pub execution_deadline: i64,
}

#[event]
pub struct ParameterProposalExecuted {
    pub proposal: Pubkey,
    pub market: Pubkey,
    pub family: u8,
    pub new_family_revision: u64,
    pub executed_at: i64,
}

#[event]
pub struct ParameterProposalSupportWithdrawn {
    pub proposal: Pubkey,
    pub supporter: Pubkey,
    pub amount: u64,
    pub total_locked: u64,
    pub status: u8,
}

#[cfg(test)]
mod tests {
    include!("tests/events.rs");
}

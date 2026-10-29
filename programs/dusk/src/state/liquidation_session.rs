use anchor_lang::prelude::*;

use super::{LiquidationFeeAllocation, LiquidationHealth, LiquidationRates};

pub const LIQUIDATION_SESSION_SEED: &[u8] = b"liquidation-session";
pub const LIQUIDATION_PAYMENT_SEED: &[u8] = b"liquidation-payment";

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct FlashLiquidationQuote {
    pub full: bool,
    pub collateral_debit: u64,
    pub collateral_credit: u64,
    /// Bound net spendable payment before distribution, including repayment,
    /// liquidation fee and any full-purchase proceeds belonging to the owner.
    pub payment: u64,
    pub repayment: u64,
    pub owner_surplus: u64,
    pub fee: LiquidationFeeAllocation,
    pub debt_shares_to_burn: u128,
    pub health_before: LiquidationHealth,
    pub health_after: LiquidationHealth,
    pub rates: LiquidationRates,
}

/// Complete quote for the optional Dusk-AMM settlement route. The buyer first
/// receives `liquidation.collateral_credit`, returns that amount to Dusk for
/// the paired swap, and finally receives `buyer_credit` debt tokens after the
/// liquidation obligation and debt-token transfer fee.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct FlashAmmLiquidationQuote {
    pub liquidation: FlashLiquidationQuote,
    pub swap_output: u64,
    pub obligation: u64,
    pub buyer_debit: u64,
    pub buyer_credit: u64,
}

/// Exists only from begin through the exact paired settle in one transaction.
/// Original debt and collateral allocations remain on the locked position.
#[account]
#[derive(InitSpace)]
pub struct LiquidationSession {
    pub market: Pubkey,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub buyer: Pubkey,
    pub collateral_mint: Pubkey,
    pub debt_mint: Pubkey,
    pub collateral_vault: Pubkey,
    pub buyer_collateral_account: Pubkey,
    pub repayment_vault: Pubkey,
    pub buyer_refund_account: Pubkey,
    pub owner_debt_account: Pubkey,
    pub original_collateral: u64,
    pub original_debt_shares: u128,
    pub original_principal: u128,
    pub borrow_index: u128,
    pub payment_balance_before: u64,
    pub insurance_balance_at_quote: u64,
    pub insurance_target_at_quote: u64,
    /// Minimum final debt-token credit when the paired Dusk AMM funds settle.
    /// External settlement routes bind their own output and leave this zero.
    pub min_buyer_credit: u64,
    pub quote: FlashLiquidationQuote,
    pub debt_asset: u8,
    pub leverage: bool,
    pub begin_index: u16,
    pub settle_index: u16,
    pub slot: u64,
    pub bump: u8,
}

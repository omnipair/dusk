use anchor_lang::prelude::*;

use crate::errors::LeverageDelegateError;

#[account]
#[derive(InitSpace)]
pub struct LeverageEntryOrder {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position: Pubkey,
    pub position_id: Pubkey,
    pub debt_mint: Pubkey,
    pub collateral_mint: Pubkey,
    pub order_id: u64,
    pub debt_asset: u8,
    /// Measured vault credit of `deposit_amount`, which funds the margin and
    /// the executor's gross bounty. The protocol fee deposit is not included.
    pub deposit_credit: u64,
    /// Gross vault debit forwarded to Dusk as margin at the creation epoch's
    /// transfer fee. Execution forwards less when a later fee increase raises
    /// the bounty's gross debit.
    pub margin_amount: u64,
    /// Minimum margin Dusk must record for the opened position, after every
    /// transfer fee.
    pub min_margin_amount: u64,
    /// Minimum net credit paid to the successful executor.
    pub executor_bounty: u64,
    pub multiplier_bps: u64,
    pub limit_price_nad: u64,
    pub min_collateral_out: u64,
    pub expiry_unix_timestamp: i64,
    pub referrer: Option<Pubkey>,
    pub bump: u8,
}

impl LeverageEntryOrder {
    /// Gross margin to forward when the executor's net bounty costs
    /// `bounty_debit` at the current epoch's transfer fee. The bounty comes
    /// out of the deposit first, so a fee increase since creation reduces the
    /// margin and never the executor's net bounty. A fee decrease leaves the
    /// margin at its recorded amount.
    pub fn execution_margin(&self, bounty_debit: u64) -> Result<u64> {
        let margin = self
            .deposit_credit
            .checked_sub(bounty_debit)
            .ok_or(LeverageDelegateError::MinimumMarginNotMet)?
            .min(self.margin_amount);
        require_gte!(
            margin,
            self.min_margin_amount,
            LeverageDelegateError::MinimumMarginNotMet
        );
        Ok(margin)
    }
}

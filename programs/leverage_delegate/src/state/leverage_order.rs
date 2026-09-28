use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct LeverageOrder {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position: Pubkey,
    pub open_curve_revision: u64,
    pub order_id: u64,
    pub kind: u8,
    pub trigger_closeout_price_nad: u64,
    pub close_bps: u16,
    pub staged_margin: u64,
    pub staged_collateral_amount: u64,
    pub staged_remaining_collateral_amount: u64,
    pub staged_remaining_debt_shares: u128,
    pub staged_remaining_debt_principal: u128,
    pub staged_custody_token_account: Pubkey,
    pub staged_output_mint: Pubkey,
    pub staged_output_amount: u64,
    /// Executed collateral-sale value in debt tokens, before debt repayment.
    pub staged_execution_value: u64,
    pub bump: u8,
}

impl LeverageOrder {
    pub fn assert_position_generation(
        &self,
        position: &dusk::state::LeveragePosition,
    ) -> Result<()> {
        require_eq!(
            self.open_curve_revision,
            position.open_curve_revision,
            crate::errors::LeverageDelegateError::InvalidOrder
        );
        Ok(())
    }
}

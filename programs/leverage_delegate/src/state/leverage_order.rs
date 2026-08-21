use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct LeverageOrder {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position: Pubkey,
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
    pub bump: u8,
}

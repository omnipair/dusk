use anchor_lang::prelude::*;

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
    /// Gross vault debit forwarded to Dusk as margin.
    pub margin_amount: u64,
    /// Gross vault debit paid to the successful executor.
    pub executor_bounty: u64,
    pub multiplier_bps: u64,
    pub limit_price_nad: u64,
    pub min_collateral_out: u64,
    pub expiry_unix_timestamp: i64,
    pub referrer: Option<Pubkey>,
    pub bump: u8,
}

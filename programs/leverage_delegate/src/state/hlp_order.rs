use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct HlpOrder {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub target_hlp_mint: Pubkey,
    pub custody_hlp_account: Pubkey,
    pub order_id: u64,
    pub kind: u8,
    pub status: u8,
    pub hlp_amount: u64,
    pub trigger_nad: u64,
    pub min_target_amount_out: u64,
    pub bump: u8,
}

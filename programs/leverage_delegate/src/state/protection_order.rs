use anchor_lang::prelude::*;

/// A prepaid, recurring authorization. Future debt on this position is covered
/// until expiry or cancellation, subject to the remaining LP budget.
#[account]
#[derive(InitSpace)]
pub struct ProtectionOrder {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position: Pubkey,
    pub position_owner: Pubkey,
    pub lp_mint: Pubkey,
    pub custody_lp_account: Pubkey,
    pub order_id: u64,
    /// 0: borrow repayment; 1: borrow collateral donation; 2: leverage repayment.
    pub action: u8,
    pub debt_asset: u8,
    pub active: bool,
    pub remaining_lp: u64,
    pub max_lp_per_execution: u64,
    pub max_payment_per_execution: u64,
    pub trigger_health_bps: u64,
    pub target_health_bps: u64,
    /// Minimum gross payment per LP smallest unit, scaled by 1e9.
    pub min_payment_per_lp_nad: u64,
    pub keeper_fee_bps: u16,
    pub expires_at: i64,
    pub bump: u8,
}

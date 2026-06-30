use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Reserves {
    // Virtual Reserves (r_virtual = r_cash + r_cash_backed_debt + r_hlp_live)
    pub live_reserve: u64,
    // Cash Reserves (r_cash)
    pub cash_reserve: u64,
    pub reserved_liability: u64,
}

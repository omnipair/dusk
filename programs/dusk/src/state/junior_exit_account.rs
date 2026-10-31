use crate::state::JuniorExitTicket;
use anchor_lang::prelude::*;

/// Burned receipt units in this account remain junior claims. They earn income
/// through the owner's yield accounts and cannot be transferred or spent twice.
#[account]
#[derive(InitSpace, Default)]
pub struct JuniorExitAccount {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub ticket: JuniorExitTicket,
    pub bump: u8,
}

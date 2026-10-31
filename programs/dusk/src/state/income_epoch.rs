use crate::state::IncomeReceivableBook;
use anchor_lang::prelude::*;

/// Immutable closing state of one unpaid-income denomination. Holders use it
/// to carry their existing units and unpaid cash fractions into the next epoch.
#[account]
#[derive(InitSpace, Default)]
pub struct IncomeEpoch {
    pub market: Pubkey,
    pub book_index: u8,
    pub book: IncomeReceivableBook,
    pub bump: u8,
}

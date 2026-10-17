use anchor_lang::prelude::*;

/// What remains of a parameter proposal once its last supporter withdraws:
/// the account discriminator alone, kept rent-exempt so the proposal's
/// address can never be initialized again. The rest of the proposal's rent
/// returns to its proposer.
#[account]
#[derive(InitSpace)]
pub struct ParameterProposalTombstone {}

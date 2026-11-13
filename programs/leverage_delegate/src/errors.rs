use anchor_lang::prelude::*;

#[error_code]
pub enum LeverageDelegateError {
    #[msg("Invalid leverage order")]
    InvalidOrder,
    #[msg("Order trigger is not met")]
    TriggerNotMet,
    #[msg("Invalid token account")]
    InvalidTokenAccount,
    #[msg("Math overflow")]
    MathOverflow,
    #[msg("Approval serialization failed")]
    ApprovalSerializationFailed,
    #[msg("Unsupported Dusk market version")]
    InvalidMarketVersion,
    #[msg("Net close payout is below the owner's minimum")]
    MinimumPayoutNotMet,
    #[msg("The order's hLP cohort was retired; only cancellation remains")]
    HlpCohortRetired,
    #[msg("Opened margin is below the owner's minimum")]
    MinimumMarginNotMet,
    #[msg("Harvest the order's earned yield before closing it")]
    UnclaimedYield,
}

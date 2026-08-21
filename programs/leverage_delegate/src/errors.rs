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
}

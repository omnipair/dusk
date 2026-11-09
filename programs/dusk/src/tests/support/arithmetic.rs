use crate::constants::NAD_DECIMALS;

pub fn accrue_fee_liability(shares: u64, fee_growth_index_q64: u128, fee_growth_checkpoint_q64: u128) -> Result<u64> {
    accrue_fee_liability_with_remainder(shares, fee_growth_index_q64, fee_growth_checkpoint_q64, 0)
        .map(|(amount, _)| amount)
}

pub(crate) fn normalize_to_nad(amount: u128, decimals: u8) -> Result<u128> {
    match decimals.cmp(&NAD_DECIMALS) {
        std::cmp::Ordering::Equal => Ok(amount),
        std::cmp::Ordering::Less => amount
            .checked_mul(
                10_u128
                    .checked_pow((NAD_DECIMALS - decimals) as u32)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            )
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into()),
        std::cmp::Ordering::Greater => Ok(amount
            .checked_div(
                10_u128
                    .checked_pow((decimals - NAD_DECIMALS) as u32)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?),
    }
}

pub(crate) fn denormalize_from_nad_ceil(amount_nad: u128, decimals: u8) -> Result<u64> {
    let value = match decimals.cmp(&NAD_DECIMALS) {
        std::cmp::Ordering::Equal => amount_nad,
        std::cmp::Ordering::Less => ceil_div(
            amount_nad,
            10_u128
                .checked_pow((NAD_DECIMALS - decimals) as u32)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .ok_or(ErrorCode::MarketMathOverflow)?,
        std::cmp::Ordering::Greater => amount_nad
            .checked_mul(
                10_u128
                    .checked_pow((decimals - NAD_DECIMALS) as u32)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?,
    };
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

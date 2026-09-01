use anchor_lang::prelude::*;

use crate::{
    constants::{NAD, NAD_DECIMALS, TARGET_MS_PER_SLOT, YIELD_GROWTH_FRACTION_MASK_Q64, YIELD_GROWTH_SCALE_Q64},
    errors::ErrorCode,
};

/// Converts backed token atoms and prior carry into Q64 per-share growth while
/// preserving `amount * 2^64 + prior = delta * supply + remainder` exactly.
pub(crate) fn distribute_growth_q64(amount: u64, supply: u64, prior_remainder_scaled: u64) -> Result<(u128, u64)> {
    require!(supply > 0, ErrorCode::SupplyUnderflow);
    // Maximum numerator is `(2^64 - 1) * 2^64 + (2^64 - 1)`, exactly
    // `u128::MAX`. No wider production integer is required.
    let scaled = (amount as u128)
        .checked_mul(YIELD_GROWTH_SCALE_Q64)
        .and_then(|value| value.checked_add(prior_remainder_scaled as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let remainder = u64::try_from(scaled % supply as u128).map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((scaled / supply as u128, remainder))
}

#[cfg(test)]
pub fn accrue_fee_liability(shares: u64, fee_growth_index_q64: u128, fee_growth_checkpoint_q64: u128) -> Result<u64> {
    accrue_fee_liability_with_remainder(shares, fee_growth_index_q64, fee_growth_checkpoint_q64, 0)
        .map(|(amount, _)| amount)
}

pub fn accrue_fee_liability_with_remainder(
    shares: u64,
    fee_growth_index_q64: u128,
    fee_growth_checkpoint_q64: u128,
    prior_remainder_q64: u64,
) -> Result<(u64, u64)> {
    if shares == 0 || fee_growth_index_q64 <= fee_growth_checkpoint_q64 {
        return Ok((0, prior_remainder_q64));
    }
    let delta = fee_growth_index_q64
        .checked_sub(fee_growth_checkpoint_q64)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // Never multiply a u64 balance by the accumulated u128 index directly.
    // Split the delta into whole and fractional Q64 limbs. Each product is at
    // most `(2^64 - 1)^2`; the fractional product plus its prior remainder is
    // at most `u128::MAX`.
    let whole_per_share = u64::try_from(delta >> 64).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let whole_accrual = (shares as u128)
        .checked_mul(whole_per_share as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let fractional_scaled = (shares as u128)
        .checked_mul(delta & YIELD_GROWTH_FRACTION_MASK_Q64)
        .and_then(|value| value.checked_add(prior_remainder_q64 as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let accrued = whole_accrual
        .checked_add(fractional_scaled >> 64)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let remainder =
        u64::try_from(fractional_scaled & YIELD_GROWTH_FRACTION_MASK_Q64).map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((
        u64::try_from(accrued).map_err(|_| ErrorCode::MarketMathOverflow)?,
        remainder,
    ))
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

pub(crate) fn denormalize_from_nad_floor(amount_nad: u128, decimals: u8) -> Result<u64> {
    let value = match decimals.cmp(&NAD_DECIMALS) {
        std::cmp::Ordering::Equal => amount_nad,
        std::cmp::Ordering::Less => amount_nad
            .checked_div(
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

pub(crate) fn observed_or_current_u64(cached_observation: u64, current_observation: u64) -> u64 {
    if cached_observation == 0 {
        current_observation
    } else {
        cached_observation
    }
}

/// Exact `floor(value * numerator / denominator)` with a checked u128 result.
pub(crate) fn mul_div_rem_u128(value: u128, numerator: u128, denominator: u128) -> Result<(u128, u128)> {
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    if value == 0 || numerator == 0 {
        return Ok((0, 0));
    }
    if let Some(product) = value.checked_mul(numerator) {
        return Ok((product / denominator, product % denominator));
    }

    let whole = value / denominator;
    let base_quotient = whole.checked_mul(numerator).ok_or(ErrorCode::MarketMathOverflow)?;
    let addend = value % denominator;
    let mut quotient = 0_u128;
    let mut remainder = 0_u128;

    for bit in (0..128).rev() {
        let carry = if remainder >= denominator - remainder {
            remainder -= denominator - remainder;
            1_u128
        } else {
            remainder += remainder;
            0_u128
        };
        quotient = quotient
            .checked_mul(2)
            .and_then(|result| result.checked_add(carry))
            .ok_or(ErrorCode::MarketMathOverflow)?;

        if (numerator >> bit) & 1 == 1 {
            let carry = if remainder >= denominator - addend {
                remainder -= denominator - addend;
                1_u128
            } else {
                remainder += addend;
                0_u128
            };
            quotient = quotient.checked_add(carry).ok_or(ErrorCode::MarketMathOverflow)?;
        }
    }
    let quotient = base_quotient
        .checked_add(quotient)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((quotient, remainder))
}

pub(crate) fn mul_div_u128(value: u128, numerator: u128, denominator: u128) -> Result<u128> {
    Ok(mul_div_rem_u128(value, numerator, denominator)?.0)
}

pub(crate) fn mul_div_ceil_u128(value: u128, numerator: u128, denominator: u128) -> Result<u128> {
    let (quotient, remainder) = mul_div_rem_u128(value, numerator, denominator)?;
    quotient
        .checked_add(u128::from(remainder != 0))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

/// Compare two unsigned ratios without overflowing their u128 cross-products.
pub(crate) fn ratio_lte_full_width(
    left_numerator: u128,
    left_denominator: u128,
    right_numerator: u128,
    right_denominator: u128,
) -> Result<bool> {
    require!(
        left_denominator > 0 && right_denominator > 0,
        ErrorCode::DenominatorOverflow
    );
    if let (Some(left), Some(right)) = (
        left_numerator.checked_mul(right_denominator),
        right_numerator.checked_mul(left_denominator),
    ) {
        return Ok(left <= right);
    }

    let (mut left_n, mut left_d) = (left_numerator, left_denominator);
    let (mut right_n, mut right_d) = (right_numerator, right_denominator);
    let mut reversed = false;
    loop {
        let left_whole = left_n / left_d;
        let right_whole = right_n / right_d;
        if left_whole != right_whole {
            return Ok(if reversed {
                left_whole > right_whole
            } else {
                left_whole < right_whole
            });
        }

        let left_remainder = left_n % left_d;
        let right_remainder = right_n % right_d;
        match (left_remainder == 0, right_remainder == 0) {
            (true, true) => return Ok(true),
            (true, false) => return Ok(!reversed),
            (false, true) => return Ok(reversed),
            (false, false) => {
                (left_n, left_d) = (left_d, left_remainder);
                (right_n, right_d) = (right_d, right_remainder);
                reversed = !reversed;
            }
        }
    }
}

/// Integer square root (floor), Newton's method on u128.
pub fn isqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let mut x = 1u128 << ((128 - value.leading_zeros()).div_ceil(2));
    loop {
        let next = (x + value / x) / 2;
        if next >= x {
            return x;
        }
        x = next;
    }
}

/// `sqrt(r)` in NAD, where `r_nad = r * NAD`. Returns `sqrt(r) * NAD`.
pub fn sqrt_ratio_nad(r_nad: u128) -> Result<u128> {
    let scaled = r_nad.checked_mul(NAD as u128).ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(isqrt(scaled))
}

/// Approximates the elapsed time in milliseconds between two slots.
pub fn slots_to_ms(start_slot: u64, end_slot: u64) -> Option<u64> {
    end_slot.checked_sub(start_slot)?.checked_mul(TARGET_MS_PER_SLOT)
}

pub fn taylor_exp(x: i64, scale: u64, precision: u64) -> u64 {
    // For negative x, we calculate exp(-x) and take reciprocal
    let is_negative = x < 0;
    let abs_x = if is_negative { -x } else { x };

    // Choose a suitable n for range reduction
    let n = 10u64;
    // Reduce x by n
    let reduced_x = abs_x / (n as i64);

    // Start with 1 (scaled by `scale`)
    let mut term = scale as u128;
    // Initialize sum with 1 (scaled by `scale`)
    let mut sum = scale as u128;

    // Compute Taylor series terms
    for i in 1..=precision {
        // Compute the next term (scaled) with overflow protection
        term = term
            .checked_mul(reduced_x as u128)
            .and_then(|t| t.checked_div(i as u128 * scale as u128))
            .unwrap_or(0);
        // Add the term to the sum with overflow protection
        sum = sum.saturating_add(term);
    }

    // Start with 1 (scaled by `scale`)
    let mut result = scale as u128;
    // Raise the result to the power of n with overflow protection
    for _i in 0..n {
        result = result
            .checked_mul(sum)
            .and_then(|r| r.checked_div(scale as u128))
            .unwrap_or(u128::MAX);
    }

    // If x was negative, take reciprocal
    if is_negative {
        result = (scale as u128 * scale as u128) / result;
    }

    result as u64
}

// Babylonian (Newton's) method (https://en.wikipedia.org/wiki/Methods_of_computing_square_roots#Babylonian_method)
// Safe sqrt function that returns None if the input is negative
pub trait SqrtU128 {
    fn sqrt(&self) -> Option<u128>;
}

impl SqrtU128 for u128 {
    fn sqrt(&self) -> Option<u128> {
        let y = *self;
        if y > 3 {
            let mut z = y;
            let mut x = y.checked_div(2)?.checked_add(1)?;
            while x < z {
                z = x;
                x = (y.checked_div(x)?.checked_add(x)?).checked_div(2)?;
            }
            Some(z)
        } else if y != 0 {
            Some(1)
        } else {
            Some(0)
        }
    }
}

/// Ceiling division: rounds up to the nearest integer
/// Formula: ceil(a / b) = (a + b - 1) / b
/// Returns None on overflow
pub fn ceil_div(a: u128, b: u128) -> Option<u128> {
    if b == 0 {
        return None;
    }
    a.checked_add(b - 1)?.checked_div(b)
}

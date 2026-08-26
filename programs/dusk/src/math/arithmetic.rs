use anchor_lang::prelude::*;

use crate::{
    constants::{NAD_DECIMALS, TARGET_MS_PER_SLOT},
    errors::ErrorCode,
};

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

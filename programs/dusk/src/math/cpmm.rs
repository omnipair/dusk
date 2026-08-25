use anchor_lang::prelude::*;

use crate::errors::ErrorCode;

#[cfg(test)]
use crate::{
    constants::{MIN_LIQUIDITY, NAD},
    math::SqrtU128,
    state::MarketSide,
};

use super::{isqrt, mul_div_ceil_u128, mul_div_u128, ratio_lte_full_width};

#[cfg(test)]
use super::fixed_point::normalize_to_nad;

/// Exact `floor(sqrt(x * y))` without requiring `x * y` to fit in `u128`.
pub(crate) fn geometric_mean_floor(x: u128, y: u128) -> Result<u128> {
    let root = if let Some(product) = x.checked_mul(y) {
        isqrt(product)
    } else {
        let x_bits = u128::BITS - x.leading_zeros();
        let y_bits = u128::BITS - y.leading_zeros();
        let mut x_shift = x_bits.saturating_sub(64);
        let mut y_shift = y_bits.saturating_sub(64);
        if (x_shift + y_shift) & 1 == 1 {
            if x_shift > 0 {
                x_shift += 1;
            } else {
                y_shift += 1;
            }
        }
        let mantissa_product = (x >> x_shift)
            .checked_mul(y >> y_shift)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let seed = isqrt(mantissa_product)
            .checked_shl((x_shift + y_shift) / 2)
            .ok_or(ErrorCode::InvariantOverflow)?;
        require!(seed > 0, ErrorCode::InvariantOverflow);
        let reciprocal = mul_div_u128(x, y, seed).map_err(|_| ErrorCode::InvariantOverflow)?;
        require_gte!(reciprocal, seed, ErrorCode::InvariantOverflow);
        let mut candidate = seed
            .checked_add((reciprocal - seed) / 2)
            .ok_or(ErrorCode::InvariantOverflow)?;

        for _ in 0..32 {
            if !ratio_lte_full_width(candidate, x, y, candidate)? {
                candidate = candidate.checked_sub(1).ok_or(ErrorCode::InvariantOverflow)?;
                continue;
            }
            let successor = candidate.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
            if ratio_lte_full_width(successor, x, y, successor)? {
                candidate = successor;
                continue;
            }
            return Ok(candidate);
        }
        return err!(ErrorCode::InvariantOverflow);
    };
    Ok(root)
}

/// Reconstructs both normalized reserve depths from a conservative K while
/// preserving the current spot reserve ratio. Flooring can only make the
/// reconstructed product more conservative than `k_nad`.
#[cfg(test)]
pub(crate) fn cpmm_reserves_from_invariant_at_spot_ratio(
    x_spot: u128,
    y_spot: u128,
    k_nad: u128,
) -> Result<(u128, u128)> {
    if x_spot == 0 || y_spot == 0 || k_nad == 0 {
        return Ok((0, 0));
    }
    let spot_k = x_spot.checked_mul(y_spot).ok_or(ErrorCode::MarketMathOverflow)?;
    let conservative_k = k_nad.min(spot_k);
    if conservative_k == spot_k {
        return Ok((x_spot, y_spot));
    }

    let conservative_sqrt = conservative_k.sqrt().ok_or(ErrorCode::MarketMathOverflow)?;
    let spot_sqrt = spot_k.sqrt().ok_or(ErrorCode::MarketMathOverflow)?;
    require!(spot_sqrt > 0, ErrorCode::DenominatorOverflow);

    let x = x_spot
        .checked_mul(conservative_sqrt)
        .and_then(|value| value.checked_div(spot_sqrt))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let y = y_spot
        .checked_mul(conservative_sqrt)
        .and_then(|value| value.checked_div(spot_sqrt))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((x.min(x_spot), y.min(y_spot)))
}

/// Constructs virtual reserves at pessimistic price = min(P_directional_ema, P_symmetric_ema).
/// - x_virt = sqrt(k * NAD / P_pessimistic)
/// - y_virt = sqrt(k * P_pessimistic / NAD)
#[cfg(test)]
pub(crate) fn cpmm_virtual_reserves_at_pessimistic_price(
    x_spot: u128,
    y_spot: u128,
    x_price_nad: u64,
    x_directional_price_nad: u64,
) -> Result<(u128, u128)> {
    // Minimum liquidity check to prevent sqrt precision loss
    if x_spot < MIN_LIQUIDITY as u128 || y_spot < MIN_LIQUIDITY as u128 {
        return Ok((0, 0));
    }
    let pessimistic_price_nad = x_price_nad.min(x_directional_price_nad) as u128;
    if pessimistic_price_nad == 0 {
        return Ok((x_spot, y_spot));
    }

    let k = x_spot.checked_mul(y_spot).ok_or(ErrorCode::MarketMathOverflow)?;

    // k * NAD / P_pessimistic
    // Try direct multiplication first; on overflow, split as (x * NAD / P) * y
    // to keep intermediates within u128 (at a small precision cost).
    let x_virt_squared = match k.checked_mul(NAD as u128) {
        Some(value) => value
            .checked_div(pessimistic_price_nad)
            .ok_or(ErrorCode::DenominatorOverflow)?,
        None => {
            let partial = x_spot
                .checked_mul(NAD as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?
                .checked_div(pessimistic_price_nad)
                .ok_or(ErrorCode::DenominatorOverflow)?;
            partial.checked_mul(y_spot).ok_or(ErrorCode::MarketMathOverflow)?
        }
    };
    // sqrt(k * NAD / P_pessimistic)
    let x_virt = x_virt_squared.sqrt().ok_or(ErrorCode::MarketMathOverflow)?;

    // k * P_pessimistic / NAD
    // Try direct multiplication first; on overflow, split as (y * P / NAD) * x.
    let y_virt_squared = match k.checked_mul(pessimistic_price_nad) {
        Some(value) => value.checked_div(NAD as u128).ok_or(ErrorCode::DenominatorOverflow)?,
        None => {
            let partial = y_spot
                .checked_mul(pessimistic_price_nad)
                .ok_or(ErrorCode::MarketMathOverflow)?
                .checked_div(NAD as u128)
                .ok_or(ErrorCode::DenominatorOverflow)?;
            partial.checked_mul(x_spot).ok_or(ErrorCode::MarketMathOverflow)?
        }
    };
    // sqrt(k * P_pessimistic / NAD)
    let y_virt = y_virt_squared.sqrt().ok_or(ErrorCode::MarketMathOverflow)?;

    Ok((x_virt, y_virt))
}

/// Calculate dy for adding dx to a constant-product coordinate.
/// ```text
/// Δy = (Δx * y) / (x + Δx)
/// ```
pub(crate) fn cpmm_amount_out_nad(x: u128, y: u128, dx: u128) -> Result<u128> {
    let denominator = x.checked_add(dx).ok_or(ErrorCode::DenominatorOverflow)?;
    require!(denominator > 0, ErrorCode::OutputAmountOverflow);
    mul_div_u128(dx, y, denominator).map_err(|_| ErrorCode::OutputAmountOverflow.into())
}

#[cfg(test)]
pub(crate) fn cpmm_amount_out(x: u64, y: u64, dx: u64) -> Result<u64> {
    let dy = cpmm_amount_out_nad(x as u128, y as u128, dx as u128)?;
    u64::try_from(dy).map_err(|_| ErrorCode::OutputAmountOverflow.into())
}

/// Calculate dx required to remove dy from a constant-product coordinate.
/// ```text
/// Δx = (Δy * x) / (y - Δy)
/// ```
pub(crate) fn cpmm_amount_in_nad(x: u128, y: u128, dy: u128) -> Result<u128> {
    let denominator = y.checked_sub(dy).ok_or(ErrorCode::DenominatorOverflow)?;
    require!(denominator > 0, ErrorCode::OutputAmountOverflow);
    mul_div_ceil_u128(dy, x, denominator).map_err(|_| ErrorCode::OutputAmountOverflow.into())
}

#[cfg(test)]
mod tests {
    include!("../tests/math/cpmm.rs");
}

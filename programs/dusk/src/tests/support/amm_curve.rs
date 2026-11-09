use crate::{constants::MIN_LIQUIDITY, math::SqrtU128, state::MarketSide};

use crate::math::arithmetic::normalize_to_nad;

/// Builds the initial concentrated geometry when the sticky center equals the
/// current ordinary-reserve spot. This is the positive closed-form solution
/// for total liquidity; no invariant root or finite-difference cell is used.
pub(crate) fn prepare_centered_concentrated_geometry(
    base_reserve: u128,
    quote_reserve: u128,
    center_price_nad: u64,
    parameters: ConcentratedCurveParameters,
) -> Result<ConcentratedCurveGeometry> {
    if parameters.is_cpmm() {
        parameters.validate(u64::MAX)?;
        return Ok(ConcentratedCurveGeometry::cpmm());
    }
    prepare_centered_concentrated_cache(base_reserve, quote_reserve, center_price_nad, parameters)?.geometry()
}

pub(crate) fn prepare_centered_concentrated_cache(
    base_reserve: u128,
    quote_reserve: u128,
    center_price_nad: u64,
    parameters: ConcentratedCurveParameters,
) -> Result<ConcentratedCurveCache> {
    parameters.validate(u64::MAX)?;
    if parameters.is_cpmm() {
        return err!(ErrorCode::InvalidMarketConfig);
    }
    require!(base_reserve > 0 && quote_reserve > 0, ErrorCode::InsufficientLiquidity);
    let spot = mul_div_u128(quote_reserve, NAD as u128, base_reserve)?;
    require!(
        spot.abs_diff(center_price_nad as u128) <= 1,
        ErrorCode::InvalidMarketConfig
    );

    let sqrt_center_nad = sqrt_ratio_nad(center_price_nad as u128)?;
    let (core_sqrt_width, outer_sqrt_width) = parameters.sqrt_widths_nad()?;
    let core_lower_sqrt_price_nad = mul_div_u128(sqrt_center_nad, NAD as u128, core_sqrt_width)?;
    let core_upper_sqrt_price_nad = mul_div_u128(sqrt_center_nad, core_sqrt_width, NAD as u128)?;
    let outer_lower_sqrt_price_nad = mul_div_u128(sqrt_center_nad, NAD as u128, outer_sqrt_width)?;
    let outer_upper_sqrt_price_nad = mul_div_u128(sqrt_center_nad, outer_sqrt_width, NAD as u128)?;
    let (_, concentrated_share) = parameters.liquidity_shares_nad()?;
    let core_share = if parameters.fade_width_bps == 0 {
        concentrated_share
    } else {
        concentrated_share.div_ceil(2)
    };
    let shoulder_share = concentrated_share
        .checked_sub(core_share)
        .ok_or(ErrorCode::BrokenInvariant)?;
    let core_over_sqrt_width = mul_div_u128(core_share, NAD as u128, core_sqrt_width)?;
    let shoulder_over_sqrt_width = mul_div_u128(shoulder_share, NAD as u128, outer_sqrt_width)?;
    let effective_share_denominator = (NAD as u128)
        .checked_sub(core_over_sqrt_width)
        .and_then(|value| value.checked_sub(shoulder_over_sqrt_width))
        .ok_or(ErrorCode::InvalidMarketConfig)?;
    require!(effective_share_denominator > 0, ErrorCode::InvalidMarketConfig);
    let total_liquidity = mul_div_u128(
        geometric_mean_floor(base_reserve, quote_reserve)?,
        NAD as u128,
        effective_share_denominator,
    )?;
    let concentrated_liquidity = mul_div_u128(total_liquidity, concentrated_share, NAD as u128)?;
    let tail_liquidity = total_liquidity
        .checked_sub(concentrated_liquidity)
        .ok_or(ErrorCode::BrokenInvariant)?;
    complete_concentrated_cache(
        parameters,
        tail_liquidity,
        concentrated_liquidity,
        core_lower_sqrt_price_nad,
        core_upper_sqrt_price_nad,
        outer_lower_sqrt_price_nad,
        outer_upper_sqrt_price_nad,
        sqrt_center_nad,
    )
    .map(|(cache, _)| cache)
}

/// Reconstructs both normalized reserve depths from a conservative K while
/// preserving the current spot reserve ratio. Flooring can only make the
/// reconstructed product more conservative than `k_nad`.
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

pub(crate) fn cpmm_amount_out(x: u64, y: u64, dx: u64) -> Result<u64> {
    let dy = cpmm_amount_out_nad(x as u128, y as u128, dx as u128)?;
    u64::try_from(dy).map_err(|_| ErrorCode::OutputAmountOverflow.into())
}

impl ConcentratedCurveCache {
    pub(crate) fn center_point(self, center_price_nad: u64) -> Result<ConcentratedCurvePoint> {
        self.center_point_with_geometry(center_price_nad, self.geometry()?)
    }
}

impl ConcentratedCurveParameters {
    pub(crate) const fn cpmm() -> Self {
        Self {
            peak_amplification_nad: NAD,
            core_half_width_bps: 0,
            fade_width_bps: 0,
        }
    }

    pub(crate) fn price_bounds_nad(self, center_price_nad: u64) -> Result<Option<(u128, u128)>> {
        require!(center_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        if self.is_cpmm() {
            return Ok(None);
        }
        let outer_width = Self::width_ratio_nad(u32::from(self.core_half_width_bps) + u32::from(self.fade_width_bps))?;
        let lower = mul_div_u128(center_price_nad as u128, NAD as u128, outer_width)?;
        let upper = mul_div_u128(center_price_nad as u128, outer_width, NAD as u128)?;
        require!(
            lower > 0 && lower < center_price_nad as u128 && upper > center_price_nad as u128,
            ErrorCode::InvalidMarketConfig
        );
        Ok(Some((lower, upper)))
    }
}

impl ConcentratedCurveGeometry {
    /// Marginal Quote-per-Base price of the active concentrated segment.
    pub(crate) fn spot_price_nad(self, point: ConcentratedCurvePoint) -> Result<u128> {
        self.validate()?;
        self.spot_price_nad_prevalidated(point)
    }
}

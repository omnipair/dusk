//! Explicit CPMM-tail plus one-band concentrated curve.
//!
//! A prepared curve is a continuous three-segment reserve path:
//! lower full-range CPMM tail, one shifted-CPMM concentrated band, and upper
//! full-range CPMM tail. Quotes cross at most two precomputed boundaries and
//! never solve an invariant root.

use anchor_lang::prelude::*;

use crate::errors::ErrorCode;

use super::{
    cpmm_amount_in_nad, cpmm_amount_out_nad, geometric_mean_floor, mul_div_u128, ratio_lte_full_width, sqrt_ratio_nad,
};
use crate::constants::NAD;

#[allow(clippy::assign_op_pattern, clippy::manual_div_ceil)]
mod wide {
    use uint::construct_uint;

    construct_uint! {
        pub struct U512(8);
    }
}

use wide::U512;

const EXPLICIT_SQRT_MAX_ITERS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExplicitCurveDirection {
    BaseToQuote,
    QuoteToBase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExplicitCurveBranch {
    LowerTail,
    Inner,
    UpperTail,
}

impl ExplicitCurveBranch {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::LowerTail => 0,
            Self::Inner => 1,
            Self::UpperTail => 2,
        }
    }
}

/// Serialized curve-cache math revision. Pre-deployment iterations remain at
/// revision 1; increment only when supporting an already-deployed cache whose
/// mathematical interpretation must remain distinguishable.
pub(crate) const EXPLICIT_CURVE_MATH_REVISION: u8 = 1;

/// Governance surface for the explicit curve. `range_width_nad` is the
/// multiplicative upper-price width around the sticky center; the lower bound
/// is its reciprocal. A zero concentrated share is the exact CPMM mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExplicitCurveParameters {
    pub range_width_nad: u64,
    pub concentrated_liquidity_share_nad: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ExplicitCurveCache {
    pub math_revision: u8,
    pub range_width_nad: u64,
    pub concentrated_liquidity_share_nad: u64,
    pub tail_liquidity: u128,
    pub concentrated_liquidity: u128,
    pub lower_sqrt_price_nad: u128,
    pub upper_sqrt_price_nad: u128,
}

impl ExplicitCurveCache {
    pub(crate) fn geometry(self) -> Result<ExplicitCurveGeometry> {
        require_eq!(
            self.math_revision,
            EXPLICIT_CURVE_MATH_REVISION,
            ErrorCode::BrokenInvariant
        );
        ExplicitCurveGeometry::from_liquidity_range(
            self.tail_liquidity,
            self.concentrated_liquidity,
            self.lower_sqrt_price_nad,
            self.upper_sqrt_price_nad,
            NAD as u128,
        )
    }

    pub(crate) const fn parameters(self) -> ExplicitCurveParameters {
        ExplicitCurveParameters {
            range_width_nad: self.range_width_nad,
            concentrated_liquidity_share_nad: self.concentrated_liquidity_share_nad,
        }
    }

    #[cfg(test)]
    pub(crate) fn center_point(self, center_price_nad: u64) -> Result<ExplicitCurvePoint> {
        self.center_point_with_geometry(center_price_nad, self.geometry()?)
    }

    pub(crate) fn center_point_with_geometry(
        self,
        center_price_nad: u64,
        geometry: ExplicitCurveGeometry,
    ) -> Result<ExplicitCurvePoint> {
        require!(center_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        require_eq!(
            geometry.inner_liquidity,
            self.concentrated_liquidity,
            ErrorCode::BrokenInvariant
        );
        let sqrt_center_nad = sqrt_ratio_nad(center_price_nad as u128)?;
        let total_liquidity = self
            .tail_liquidity
            .checked_add(self.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let center_point = ExplicitCurvePoint {
            base_reserve: mul_div_u128(total_liquidity, NAD as u128, sqrt_center_nad)?
                .checked_sub(geometry.inner_base_amplification_offset)
                .ok_or(ErrorCode::BrokenInvariant)?,
            quote_reserve: mul_div_u128(total_liquidity, sqrt_center_nad, NAD as u128)?
                .checked_sub(geometry.inner_quote_amplification_offset)
                .ok_or(ErrorCode::BrokenInvariant)?,
        };
        require!(
            center_point.base_reserve > 0 && center_point.quote_reserve > 0,
            ErrorCode::BrokenInvariant
        );
        Ok(center_point)
    }
}

fn complete_explicit_cache(
    parameters: ExplicitCurveParameters,
    tail_liquidity: u128,
    concentrated_liquidity: u128,
    lower_sqrt_price_nad: u128,
    upper_sqrt_price_nad: u128,
    sqrt_center_nad: u128,
) -> Result<ExplicitCurveCache> {
    let geometry = ExplicitCurveGeometry::from_liquidity_range(
        tail_liquidity,
        concentrated_liquidity,
        lower_sqrt_price_nad,
        upper_sqrt_price_nad,
        NAD as u128,
    )?;
    let total_liquidity = tail_liquidity
        .checked_add(concentrated_liquidity)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let center_point = ExplicitCurvePoint {
        base_reserve: mul_div_u128(total_liquidity, NAD as u128, sqrt_center_nad)?
            .checked_sub(geometry.inner_base_amplification_offset)
            .ok_or(ErrorCode::BrokenInvariant)?,
        quote_reserve: mul_div_u128(total_liquidity, sqrt_center_nad, NAD as u128)?
            .checked_sub(geometry.inner_quote_amplification_offset)
            .ok_or(ErrorCode::BrokenInvariant)?,
    };
    require!(
        center_point.base_reserve > 0 && center_point.quote_reserve > 0,
        ErrorCode::BrokenInvariant
    );
    Ok(ExplicitCurveCache {
        math_revision: EXPLICIT_CURVE_MATH_REVISION,
        range_width_nad: parameters.range_width_nad,
        concentrated_liquidity_share_nad: parameters.concentrated_liquidity_share_nad,
        tail_liquidity,
        concentrated_liquidity,
        lower_sqrt_price_nad,
        upper_sqrt_price_nad,
    })
}

impl ExplicitCurveParameters {
    #[cfg(test)]
    pub(crate) const fn cpmm() -> Self {
        Self {
            range_width_nad: 0,
            concentrated_liquidity_share_nad: 0,
        }
    }

    pub(crate) const fn is_cpmm(self) -> bool {
        self.concentrated_liquidity_share_nad == 0
    }

    /// Preserves the existing maximum-amplification policy. For
    /// `rho=Lc/(Lt+Lc)`, amplification is `rho/(1-rho)`.
    pub(crate) fn validate(self, max_amplification_nad: u64) -> Result<()> {
        if self.is_cpmm() {
            require_eq!(self.range_width_nad, 0, ErrorCode::InvalidMarketConfig);
            return Ok(());
        }
        require!(
            self.range_width_nad > NAD && self.concentrated_liquidity_share_nad < NAD && max_amplification_nad > 0,
            ErrorCode::InvalidMarketConfig
        );
        let denominator = (NAD - self.concentrated_liquidity_share_nad) as u128;
        require!(
            ratio_lte_full_width(
                self.concentrated_liquidity_share_nad as u128,
                denominator,
                max_amplification_nad as u128,
                NAD as u128,
            )?,
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn price_bounds_nad(self, center_price_nad: u64) -> Result<Option<(u128, u128)>> {
        require!(center_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        if self.is_cpmm() {
            return Ok(None);
        }
        let lower = mul_div_u128(center_price_nad as u128, NAD as u128, self.range_width_nad as u128)?;
        let upper = mul_div_u128(center_price_nad as u128, self.range_width_nad as u128, NAD as u128)?;
        require!(
            lower > 0 && lower < center_price_nad as u128 && upper > center_price_nad as u128,
            ErrorCode::InvalidMarketConfig
        );
        Ok(Some((lower, upper)))
    }
}

/// Builds the initial explicit geometry when the sticky center equals the
/// current ordinary-reserve spot. This is the positive closed-form solution
/// for total liquidity; no invariant root or finite-difference cell is used.
#[cfg(test)]
pub(crate) fn prepare_centered_explicit_geometry(
    base_reserve: u128,
    quote_reserve: u128,
    center_price_nad: u64,
    parameters: ExplicitCurveParameters,
) -> Result<ExplicitCurveGeometry> {
    if parameters.is_cpmm() {
        parameters.validate(u64::MAX)?;
        return Ok(ExplicitCurveGeometry::cpmm());
    }
    prepare_centered_explicit_cache(base_reserve, quote_reserve, center_price_nad, parameters)?.geometry()
}

#[cfg(test)]
pub(crate) fn prepare_centered_explicit_cache(
    base_reserve: u128,
    quote_reserve: u128,
    center_price_nad: u64,
    parameters: ExplicitCurveParameters,
) -> Result<ExplicitCurveCache> {
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
    let sqrt_width_nad = sqrt_ratio_nad(parameters.range_width_nad as u128)?;
    let lower_sqrt_price_nad = mul_div_u128(sqrt_center_nad, NAD as u128, sqrt_width_nad)?;
    let upper_sqrt_price_nad = mul_div_u128(sqrt_center_nad, sqrt_width_nad, NAD as u128)?;
    let rho_over_sqrt_width = mul_div_u128(
        parameters.concentrated_liquidity_share_nad as u128,
        NAD as u128,
        sqrt_width_nad,
    )?;
    let effective_share_denominator = (NAD as u128)
        .checked_sub(rho_over_sqrt_width)
        .ok_or(ErrorCode::InvalidMarketConfig)?;
    require!(effective_share_denominator > 0, ErrorCode::InvalidMarketConfig);
    let total_liquidity = mul_div_u128(
        geometric_mean_floor(base_reserve, quote_reserve)?,
        NAD as u128,
        effective_share_denominator,
    )?;
    let concentrated_liquidity = mul_div_u128(
        total_liquidity,
        parameters.concentrated_liquidity_share_nad as u128,
        NAD as u128,
    )?;
    let tail_liquidity = total_liquidity
        .checked_sub(concentrated_liquidity)
        .ok_or(ErrorCode::BrokenInvariant)?;
    complete_explicit_cache(
        parameters,
        tail_liquidity,
        concentrated_liquidity,
        lower_sqrt_price_nad,
        upper_sqrt_price_nad,
        sqrt_center_nad,
    )
}

/// Reconstructs the unique positive curve scale through an arbitrary reserve
/// point for a sticky center and concentration policy. Each of the three
/// possible branches has one quadratic positive root; we evaluate those
/// closed forms and accept the unique root whose geometry classifies the
/// supplied point into the same branch. No invariant search is performed.
pub(crate) fn prepare_explicit_cache_at_point(
    base_reserve: u128,
    quote_reserve: u128,
    center_price_nad: u64,
    parameters: ExplicitCurveParameters,
) -> Result<ExplicitCurveCache> {
    parameters.validate(u64::MAX)?;
    require!(base_reserve > 0 && quote_reserve > 0, ErrorCode::InsufficientLiquidity);

    if parameters.is_cpmm() {
        let tail_liquidity = geometric_mean_floor(base_reserve, quote_reserve)?;
        require!(tail_liquidity > 0, ErrorCode::InsufficientLiquidity);
        let sqrt_center_nad = sqrt_ratio_nad(center_price_nad as u128)?;
        return complete_explicit_cache(parameters, tail_liquidity, 0, 0, 0, sqrt_center_nad);
    }

    let sqrt_center_nad = sqrt_ratio_nad(center_price_nad as u128)?;
    let sqrt_width_nad = sqrt_ratio_nad(parameters.range_width_nad as u128)?;
    let lower_sqrt_price_nad = mul_div_u128(sqrt_center_nad, NAD as u128, sqrt_width_nad)?;
    let upper_sqrt_price_nad = mul_div_u128(sqrt_center_nad, sqrt_width_nad, NAD as u128)?;
    let point = ExplicitCurvePoint {
        base_reserve,
        quote_reserve,
    };

    for branch in [
        ExplicitCurveBranch::Inner,
        ExplicitCurveBranch::LowerTail,
        ExplicitCurveBranch::UpperTail,
    ] {
        let total_liquidity = explicit_total_liquidity_root(
            point,
            parameters.concentrated_liquidity_share_nad as u128,
            lower_sqrt_price_nad,
            upper_sqrt_price_nad,
            branch,
        )?;
        if total_liquidity == 0 {
            continue;
        }
        let concentrated_liquidity = mul_div_u128(
            total_liquidity,
            parameters.concentrated_liquidity_share_nad as u128,
            NAD as u128,
        )?;
        let tail_liquidity = total_liquidity
            .checked_sub(concentrated_liquidity)
            .ok_or(ErrorCode::BrokenInvariant)?;
        if tail_liquidity == 0 || concentrated_liquidity == 0 {
            continue;
        }
        let cache = complete_explicit_cache(
            parameters,
            tail_liquidity,
            concentrated_liquidity,
            lower_sqrt_price_nad,
            upper_sqrt_price_nad,
            sqrt_center_nad,
        )?;
        let geometry = cache.geometry()?;
        if geometry.branch(point) == branch {
            return Ok(cache);
        }
    }
    err!(ErrorCode::BrokenInvariant)
}

fn explicit_total_liquidity_root(
    point: ExplicitCurvePoint,
    share_nad: u128,
    lower_sqrt_price_nad: u128,
    upper_sqrt_price_nad: u128,
    branch: ExplicitCurveBranch,
) -> Result<u128> {
    let scale = NAD as u128;
    let scale_squared = U512::from(scale)
        .checked_mul(U512::from(scale))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let x = U512::from(point.base_reserve);
    let y = U512::from(point.quote_reserve);
    let rho = U512::from(share_nad);
    let tail_share = U512::from(scale.checked_sub(share_nad).ok_or(ErrorCode::InvalidMarketConfig)?);
    let lower = U512::from(lower_sqrt_price_nad);
    let upper = U512::from(upper_sqrt_price_nad);
    let width = upper.checked_sub(lower).ok_or(ErrorCode::InvalidMarketConfig)?;

    let (a, b, c, negative_linear) = match branch {
        ExplicitCurveBranch::Inner => {
            let a = upper
                .checked_mul(scale_squared)
                .and_then(|value| value.checked_sub(rho.checked_mul(rho)?.checked_mul(lower)?))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let linear_shape = x
                .checked_mul(lower)
                .and_then(|value| value.checked_mul(upper))
                .and_then(|value| value.checked_add(y.checked_mul(scale_squared)?))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let b = rho.checked_mul(linear_shape).ok_or(ErrorCode::InvariantOverflow)?;
            let c = x
                .checked_mul(y)
                .and_then(|value| value.checked_mul(upper))
                .and_then(|value| value.checked_mul(scale_squared))
                .ok_or(ErrorCode::InvariantOverflow)?;
            (a, b, c, true)
        }
        ExplicitCurveBranch::LowerTail => {
            let a = tail_share
                .checked_mul(tail_share)
                .and_then(|value| value.checked_mul(lower))
                .and_then(|value| value.checked_mul(upper))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let b = rho
                .checked_mul(width)
                .and_then(|value| value.checked_mul(y))
                .and_then(|value| value.checked_mul(scale_squared))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let c = x
                .checked_mul(y)
                .and_then(|value| value.checked_mul(scale_squared))
                .and_then(|value| value.checked_mul(lower))
                .and_then(|value| value.checked_mul(upper))
                .ok_or(ErrorCode::InvariantOverflow)?;
            (a, b, c, false)
        }
        ExplicitCurveBranch::UpperTail => {
            let a = tail_share.checked_mul(tail_share).ok_or(ErrorCode::InvariantOverflow)?;
            let b = x
                .checked_mul(rho)
                .and_then(|value| value.checked_mul(width))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let c = x
                .checked_mul(y)
                .and_then(|value| value.checked_mul(scale_squared))
                .ok_or(ErrorCode::InvariantOverflow)?;
            (a, b, c, false)
        }
    };
    require!(!a.is_zero(), ErrorCode::InvalidMarketConfig);
    let discriminant = b
        .checked_mul(b)
        .and_then(|value| value.checked_add(a.checked_mul(c)?.checked_mul(U512::from(4_u8))?))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let root = sqrt_floor_u512(discriminant)?;
    let numerator = if negative_linear {
        root.checked_add(b).ok_or(ErrorCode::InvariantOverflow)?
    } else {
        root.checked_sub(b).ok_or(ErrorCode::BrokenInvariant)?
    };
    let denominator = a.checked_mul(U512::from(2_u8)).ok_or(ErrorCode::InvariantOverflow)?;
    let liquidity = numerator / denominator;
    require!(liquidity <= U512::from(u128::MAX), ErrorCode::InvariantOverflow);
    Ok(liquidity.as_u128())
}

fn sqrt_floor_u512(value: U512) -> Result<U512> {
    if value.is_zero() {
        return Ok(U512::zero());
    }
    let mut root = U512::one() << value.bits().div_ceil(2);
    for _ in 0..EXPLICIT_SQRT_MAX_ITERS {
        let next = root.checked_add(value / root).ok_or(ErrorCode::InvariantOverflow)? >> 1;
        if next >= root {
            return Ok(root);
        }
        root = next;
    }
    err!(ErrorCode::InvariantOverflow)
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ExplicitCurvePoint {
    pub base_reserve: u128,
    pub quote_reserve: u128,
}

/// Precomputed geometry for one full-range CPMM tail plus one concentrated
/// band. Offsets are reserve coordinates, not transferable balances.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ExplicitCurveGeometry {
    /// Zero selects exact CPMM and requires every other geometry field to be
    /// zero. Nonzero selects the three-segment concentrated curve.
    pub inner_liquidity: u128,
    /// Positive offsets used by the shifted constant product inside the band.
    pub inner_base_amplification_offset: u128,
    pub inner_quote_amplification_offset: u128,
    /// Fixed concentrated inventory left behind in the corresponding tail.
    pub lower_tail_base_inventory: u128,
    pub upper_tail_quote_inventory: u128,
    pub lower_boundary: ExplicitCurvePoint,
    pub upper_boundary: ExplicitCurvePoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExplicitCurveQuote {
    pub amount_in: u128,
    pub amount_out: u128,
    pub end: ExplicitCurvePoint,
    pub end_branch: ExplicitCurveBranch,
    pub boundary_crossings: u8,
}

impl ExplicitCurveGeometry {
    pub(crate) const fn cpmm() -> Self {
        Self {
            inner_liquidity: 0,
            inner_base_amplification_offset: 0,
            inner_quote_amplification_offset: 0,
            lower_tail_base_inventory: 0,
            upper_tail_quote_inventory: 0,
            lower_boundary: ExplicitCurvePoint {
                base_reserve: 0,
                quote_reserve: 0,
            },
            upper_boundary: ExplicitCurvePoint {
                base_reserve: 0,
                quote_reserve: 0,
            },
        }
    }

    pub(crate) const fn is_cpmm(self) -> bool {
        self.inner_liquidity == 0
    }

    /// Constructs reserve-coordinate offsets and boundaries directly from
    /// tail liquidity, concentrated liquidity, and two square-root prices.
    pub(crate) fn from_liquidity_range(
        tail_liquidity: u128,
        concentrated_liquidity: u128,
        lower_sqrt_price: u128,
        upper_sqrt_price: u128,
        sqrt_price_scale: u128,
    ) -> Result<Self> {
        if concentrated_liquidity == 0 {
            require!(tail_liquidity > 0, ErrorCode::InsufficientLiquidity);
            return Ok(Self::cpmm());
        }
        require!(
            tail_liquidity > 0 && lower_sqrt_price > 0 && lower_sqrt_price < upper_sqrt_price && sqrt_price_scale > 0,
            ErrorCode::InvalidMarketConfig
        );
        let inner_base_amplification_offset = mul_div_u128(concentrated_liquidity, sqrt_price_scale, upper_sqrt_price)?;
        let inner_quote_amplification_offset =
            mul_div_u128(concentrated_liquidity, lower_sqrt_price, sqrt_price_scale)?;
        let lower_tail_base_inventory = mul_div_u128(concentrated_liquidity, sqrt_price_scale, lower_sqrt_price)?
            .checked_sub(inner_base_amplification_offset)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let lower_boundary = ExplicitCurvePoint {
            base_reserve: mul_div_u128(tail_liquidity, sqrt_price_scale, lower_sqrt_price)?
                .checked_add(lower_tail_base_inventory)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            quote_reserve: mul_div_u128(tail_liquidity, lower_sqrt_price, sqrt_price_scale)?,
        };
        let upper_base_reserve = mul_div_u128(tail_liquidity, sqrt_price_scale, upper_sqrt_price)?;
        let lower_inner_base = lower_boundary
            .base_reserve
            .checked_add(inner_base_amplification_offset)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let lower_inner_quote = lower_boundary
            .quote_reserve
            .checked_add(inner_quote_amplification_offset)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let upper_inner_base = upper_base_reserve
            .checked_add(inner_base_amplification_offset)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let upper_inner_quote = mul_div_u128(lower_inner_base, lower_inner_quote, upper_inner_base)?;
        let upper_quote_reserve = upper_inner_quote
            .checked_sub(inner_quote_amplification_offset)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let lower_tail_base = lower_boundary
            .base_reserve
            .checked_sub(lower_tail_base_inventory)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let upper_tail_quote = mul_div_u128(lower_tail_base, lower_boundary.quote_reserve, upper_base_reserve)?;
        let upper_tail_quote_inventory = upper_quote_reserve
            .checked_sub(upper_tail_quote)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let geometry = Self {
            inner_liquidity: concentrated_liquidity,
            inner_base_amplification_offset,
            inner_quote_amplification_offset,
            lower_tail_base_inventory,
            upper_tail_quote_inventory,
            lower_boundary,
            upper_boundary: ExplicitCurvePoint {
                base_reserve: upper_base_reserve,
                quote_reserve: upper_quote_reserve,
            },
        };
        geometry.validate()?;
        Ok(geometry)
    }

    pub(crate) fn validate(self) -> Result<()> {
        if self.is_cpmm() {
            require!(self == Self::cpmm(), ErrorCode::InvalidMarketConfig);
            return Ok(());
        }

        require!(
            self.inner_base_amplification_offset > 0
                && self.inner_quote_amplification_offset > 0
                && self.lower_boundary.base_reserve > self.upper_boundary.base_reserve
                && self.lower_boundary.quote_reserve < self.upper_boundary.quote_reserve
                && self.lower_boundary.base_reserve > self.lower_tail_base_inventory
                && self.upper_boundary.quote_reserve > self.upper_tail_quote_inventory,
            ErrorCode::InvalidMarketConfig
        );

        let lower_inner_base = self
            .lower_boundary
            .base_reserve
            .checked_add(self.inner_base_amplification_offset)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let lower_inner_quote = self
            .lower_boundary
            .quote_reserve
            .checked_add(self.inner_quote_amplification_offset)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let upper_inner_base = self
            .upper_boundary
            .base_reserve
            .checked_add(self.inner_base_amplification_offset)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let upper_inner_quote = self
            .upper_boundary
            .quote_reserve
            .checked_add(self.inner_quote_amplification_offset)
            .ok_or(ErrorCode::InvariantOverflow)?;

        require!(
            products_rounding_compatible(lower_inner_base, lower_inner_quote, upper_inner_base, upper_inner_quote,)?,
            ErrorCode::InvalidMarketConfig
        );

        let lower_tail_base = self
            .lower_boundary
            .base_reserve
            .checked_sub(self.lower_tail_base_inventory)
            .ok_or(ErrorCode::InvalidMarketConfig)?;
        let upper_tail_quote = self
            .upper_boundary
            .quote_reserve
            .checked_sub(self.upper_tail_quote_inventory)
            .ok_or(ErrorCode::InvalidMarketConfig)?;
        require!(
            products_rounding_compatible(
                lower_tail_base,
                self.lower_boundary.quote_reserve,
                self.upper_boundary.base_reserve,
                upper_tail_quote,
            )?,
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }

    pub(crate) fn branch(self, point: ExplicitCurvePoint) -> ExplicitCurveBranch {
        if self.is_cpmm() || point.base_reserve >= self.lower_boundary.base_reserve {
            ExplicitCurveBranch::LowerTail
        } else if point.base_reserve <= self.upper_boundary.base_reserve {
            ExplicitCurveBranch::UpperTail
        } else {
            ExplicitCurveBranch::Inner
        }
    }

    /// Marginal Quote-per-Base price of the active explicit segment.
    #[cfg(test)]
    pub(crate) fn spot_price_nad(self, point: ExplicitCurvePoint) -> Result<u128> {
        self.validate()?;
        self.spot_price_nad_prevalidated(point)
    }

    /// Hot-path variant for geometry loaded from `ExplicitCurveCache`, whose
    /// constructor already performed the complete geometry validation.
    pub(crate) fn spot_price_nad_prevalidated(self, point: ExplicitCurvePoint) -> Result<u128> {
        let branch = self.branch(point);
        let (base_curve_reserve, quote_curve_reserve) = self.effective_reserves(point, branch)?;
        mul_div_u128(quote_curve_reserve, NAD as u128, base_curve_reserve)
    }

    pub(crate) fn range_prices_nad(self) -> Result<Option<(u128, u128)>> {
        self.validate()?;
        if self.is_cpmm() {
            return Ok(None);
        }
        let (lower_base, lower_quote) = self.effective_reserves(self.lower_boundary, ExplicitCurveBranch::Inner)?;
        let (upper_base, upper_quote) = self.effective_reserves(self.upper_boundary, ExplicitCurveBranch::Inner)?;
        Ok(Some((
            mul_div_u128(lower_quote, NAD as u128, lower_base)?,
            mul_div_u128(upper_quote, NAD as u128, upper_base)?,
        )))
    }

    /// Closed-form reserve point at a requested marginal price. Lending and
    /// liquidation use this inverse to materialize a pessimistic executable
    /// shape without an invariant root solve.
    pub(crate) fn point_at_price_nad(self, price_nad: u128, tail_liquidity: u128) -> Result<ExplicitCurvePoint> {
        self.validate()?;
        require!(price_nad > 0 && tail_liquidity > 0, ErrorCode::InvalidSettlementPrice);
        let sqrt_price_nad = sqrt_ratio_nad(price_nad)?;
        require!(sqrt_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        let branch = if self.is_cpmm() {
            ExplicitCurveBranch::LowerTail
        } else {
            let (lower_price, upper_price) = self.range_prices_nad()?.ok_or(ErrorCode::BrokenInvariant)?;
            if price_nad <= lower_price {
                ExplicitCurveBranch::LowerTail
            } else if price_nad >= upper_price {
                ExplicitCurveBranch::UpperTail
            } else {
                ExplicitCurveBranch::Inner
            }
        };
        let liquidity = match branch {
            ExplicitCurveBranch::Inner => tail_liquidity
                .checked_add(self.inner_liquidity)
                .ok_or(ErrorCode::InvariantOverflow)?,
            ExplicitCurveBranch::LowerTail | ExplicitCurveBranch::UpperTail => tail_liquidity,
        };
        let (effective_base, effective_quote) = (
            mul_div_u128(liquidity, NAD as u128, sqrt_price_nad)?,
            mul_div_u128(liquidity, sqrt_price_nad, NAD as u128)?,
        );
        match branch {
            ExplicitCurveBranch::LowerTail => Ok(ExplicitCurvePoint {
                base_reserve: effective_base
                    .checked_add(self.lower_tail_base_inventory)
                    .ok_or(ErrorCode::InvariantOverflow)?,
                quote_reserve: effective_quote,
            }),
            ExplicitCurveBranch::Inner => Ok(ExplicitCurvePoint {
                base_reserve: effective_base
                    .checked_sub(self.inner_base_amplification_offset)
                    .ok_or(ErrorCode::InsufficientLiquidity)?,
                quote_reserve: effective_quote
                    .checked_sub(self.inner_quote_amplification_offset)
                    .ok_or(ErrorCode::InsufficientLiquidity)?,
            }),
            ExplicitCurveBranch::UpperTail => Ok(ExplicitCurvePoint {
                base_reserve: effective_base,
                quote_reserve: effective_quote
                    .checked_add(self.upper_tail_quote_inventory)
                    .ok_or(ErrorCode::InvariantOverflow)?,
            }),
        }
    }

    fn branch_for_direction(self, point: ExplicitCurvePoint, direction: ExplicitCurveDirection) -> ExplicitCurveBranch {
        if self.is_cpmm() {
            return ExplicitCurveBranch::LowerTail;
        }
        match direction {
            // Input reserve is the canonical branch coordinate. The opposite
            // reserve may differ from its continuous boundary reference by an
            // atom after conservative output rounding.
            ExplicitCurveDirection::BaseToQuote => {
                if point.base_reserve >= self.lower_boundary.base_reserve {
                    ExplicitCurveBranch::LowerTail
                } else if point.base_reserve < self.upper_boundary.base_reserve {
                    ExplicitCurveBranch::UpperTail
                } else {
                    ExplicitCurveBranch::Inner
                }
            }
            ExplicitCurveDirection::QuoteToBase => {
                if point.quote_reserve < self.lower_boundary.quote_reserve {
                    ExplicitCurveBranch::LowerTail
                } else if point.quote_reserve >= self.upper_boundary.quote_reserve {
                    ExplicitCurveBranch::UpperTail
                } else {
                    ExplicitCurveBranch::Inner
                }
            }
        }
    }

    fn effective_reserves(self, point: ExplicitCurvePoint, branch: ExplicitCurveBranch) -> Result<(u128, u128)> {
        let pair = match branch {
            ExplicitCurveBranch::LowerTail => (
                point
                    .base_reserve
                    .checked_sub(self.lower_tail_base_inventory)
                    .ok_or(ErrorCode::BrokenInvariant)?,
                point.quote_reserve,
            ),
            ExplicitCurveBranch::Inner => (
                point
                    .base_reserve
                    .checked_add(self.inner_base_amplification_offset)
                    .ok_or(ErrorCode::InvariantOverflow)?,
                point
                    .quote_reserve
                    .checked_add(self.inner_quote_amplification_offset)
                    .ok_or(ErrorCode::InvariantOverflow)?,
            ),
            ExplicitCurveBranch::UpperTail => (
                point.base_reserve,
                point
                    .quote_reserve
                    .checked_sub(self.upper_tail_quote_inventory)
                    .ok_or(ErrorCode::BrokenInvariant)?,
            ),
        };
        require!(pair.0 > 0 && pair.1 > 0, ErrorCode::InsufficientLiquidity);
        Ok(pair)
    }

    fn next_boundary(
        self,
        point: ExplicitCurvePoint,
        branch: ExplicitCurveBranch,
        direction: ExplicitCurveDirection,
    ) -> Option<ExplicitCurvePoint> {
        if self.is_cpmm() {
            return None;
        }
        match (direction, branch) {
            (ExplicitCurveDirection::BaseToQuote, ExplicitCurveBranch::UpperTail) => Some(self.upper_boundary),
            (ExplicitCurveDirection::BaseToQuote, ExplicitCurveBranch::Inner) => Some(self.lower_boundary),
            (ExplicitCurveDirection::QuoteToBase, ExplicitCurveBranch::LowerTail) => Some(self.lower_boundary),
            (ExplicitCurveDirection::QuoteToBase, ExplicitCurveBranch::Inner) => Some(self.upper_boundary),
            _ => {
                let _ = point;
                None
            }
        }
    }

    pub(crate) fn quote_exact_in(
        self,
        start: ExplicitCurvePoint,
        amount_in: u128,
        direction: ExplicitCurveDirection,
    ) -> Result<ExplicitCurveQuote> {
        self.validate()?;
        self.quote_exact_in_prevalidated(start, amount_in, direction)
    }

    /// Hot-path variant for geometry loaded from `ExplicitCurveCache`.
    pub(crate) fn quote_exact_in_prevalidated(
        self,
        start: ExplicitCurvePoint,
        amount_in: u128,
        direction: ExplicitCurveDirection,
    ) -> Result<ExplicitCurveQuote> {
        require!(
            start.base_reserve > 0 && start.quote_reserve > 0,
            ErrorCode::InsufficientLiquidity
        );
        require!(amount_in > 0, ErrorCode::AmountZero);

        let mut point = start;
        let mut remaining = amount_in;
        let mut total_out = 0_u128;
        let mut crossings = 0_u8;

        for _ in 0..3 {
            let branch = self.branch_for_direction(point, direction);
            let boundary = self.next_boundary(point, branch, direction);
            let input_to_boundary = boundary.map(|target| match direction {
                ExplicitCurveDirection::BaseToQuote => target.base_reserve.saturating_sub(point.base_reserve),
                ExplicitCurveDirection::QuoteToBase => target.quote_reserve.saturating_sub(point.quote_reserve),
            });

            if let (Some(target), Some(to_boundary)) = (boundary, input_to_boundary) {
                if to_boundary > 0 && remaining >= to_boundary {
                    // Pay for the crossing through the branch invariant. The
                    // configured boundary is an input-coordinate threshold;
                    // its opposite reserve is only the continuous reference.
                    // This matters after prior integer rounding has increased
                    // K by dust: assigning the reference point directly would
                    // silently give that dust away.
                    let (effective_base, effective_quote) = self.effective_reserves(point, branch)?;
                    let boundary_out = match direction {
                        ExplicitCurveDirection::BaseToQuote => {
                            cpmm_amount_out_nad(effective_base, effective_quote, to_boundary)?
                        }
                        ExplicitCurveDirection::QuoteToBase => {
                            cpmm_amount_out_nad(effective_quote, effective_base, to_boundary)?
                        }
                    };
                    match direction {
                        ExplicitCurveDirection::BaseToQuote => {
                            point.base_reserve = target.base_reserve;
                            point.quote_reserve = point
                                .quote_reserve
                                .checked_sub(boundary_out)
                                .ok_or(ErrorCode::InsufficientLiquidity)?;
                        }
                        ExplicitCurveDirection::QuoteToBase => {
                            point.quote_reserve = target.quote_reserve;
                            point.base_reserve = point
                                .base_reserve
                                .checked_sub(boundary_out)
                                .ok_or(ErrorCode::InsufficientLiquidity)?;
                        }
                    }
                    remaining = remaining.checked_sub(to_boundary).ok_or(ErrorCode::BrokenInvariant)?;
                    total_out = total_out
                        .checked_add(boundary_out)
                        .ok_or(ErrorCode::OutputAmountOverflow)?;
                    crossings = crossings.checked_add(1).ok_or(ErrorCode::BrokenInvariant)?;
                    if remaining == 0 {
                        return Ok(ExplicitCurveQuote {
                            amount_in,
                            amount_out: total_out,
                            end: point,
                            end_branch: self.branch(point),
                            boundary_crossings: crossings,
                        });
                    }
                    continue;
                }
            }

            let (effective_base, effective_quote) = self.effective_reserves(point, branch)?;
            let output = match direction {
                ExplicitCurveDirection::BaseToQuote => cpmm_amount_out_nad(effective_base, effective_quote, remaining)?,
                ExplicitCurveDirection::QuoteToBase => cpmm_amount_out_nad(effective_quote, effective_base, remaining)?,
            };
            require!(output > 0, ErrorCode::InsufficientOutputAmount);
            match direction {
                ExplicitCurveDirection::BaseToQuote => {
                    point.base_reserve = point
                        .base_reserve
                        .checked_add(remaining)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    point.quote_reserve = point
                        .quote_reserve
                        .checked_sub(output)
                        .ok_or(ErrorCode::InsufficientLiquidity)?;
                }
                ExplicitCurveDirection::QuoteToBase => {
                    point.quote_reserve = point
                        .quote_reserve
                        .checked_add(remaining)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    point.base_reserve = point
                        .base_reserve
                        .checked_sub(output)
                        .ok_or(ErrorCode::InsufficientLiquidity)?;
                }
            }
            total_out = total_out.checked_add(output).ok_or(ErrorCode::OutputAmountOverflow)?;
            return Ok(ExplicitCurveQuote {
                amount_in,
                amount_out: total_out,
                end: point,
                end_branch: self.branch(point),
                boundary_crossings: crossings,
            });
        }
        err!(ErrorCode::BrokenInvariant)
    }

    pub(crate) fn quote_exact_out(
        self,
        start: ExplicitCurvePoint,
        amount_out: u128,
        direction: ExplicitCurveDirection,
    ) -> Result<ExplicitCurveQuote> {
        self.validate()?;
        require!(
            start.base_reserve > 0 && start.quote_reserve > 0,
            ErrorCode::InsufficientLiquidity
        );
        require!(amount_out > 0, ErrorCode::AmountZero);

        let mut point = start;
        let mut remaining = amount_out;
        let mut total_in = 0_u128;
        let mut crossings = 0_u8;

        for _ in 0..3 {
            let branch = self.branch_for_direction(point, direction);
            let boundary = self.next_boundary(point, branch, direction);
            let boundary_segment = if let Some(target) = boundary {
                let input = match direction {
                    ExplicitCurveDirection::BaseToQuote => target.base_reserve.saturating_sub(point.base_reserve),
                    ExplicitCurveDirection::QuoteToBase => target.quote_reserve.saturating_sub(point.quote_reserve),
                };
                if input == 0 {
                    None
                } else {
                    let (effective_base, effective_quote) = self.effective_reserves(point, branch)?;
                    let output = match direction {
                        ExplicitCurveDirection::BaseToQuote => {
                            cpmm_amount_out_nad(effective_base, effective_quote, input)?
                        }
                        ExplicitCurveDirection::QuoteToBase => {
                            cpmm_amount_out_nad(effective_quote, effective_base, input)?
                        }
                    };
                    Some((target, input, output))
                }
            } else {
                None
            };

            if let Some((target, boundary_in, boundary_out)) = boundary_segment {
                if remaining >= boundary_out {
                    match direction {
                        ExplicitCurveDirection::BaseToQuote => {
                            point.base_reserve = target.base_reserve;
                            point.quote_reserve = point
                                .quote_reserve
                                .checked_sub(boundary_out)
                                .ok_or(ErrorCode::InsufficientLiquidity)?;
                        }
                        ExplicitCurveDirection::QuoteToBase => {
                            point.quote_reserve = target.quote_reserve;
                            point.base_reserve = point
                                .base_reserve
                                .checked_sub(boundary_out)
                                .ok_or(ErrorCode::InsufficientLiquidity)?;
                        }
                    }
                    remaining = remaining.checked_sub(boundary_out).ok_or(ErrorCode::BrokenInvariant)?;
                    total_in = total_in
                        .checked_add(boundary_in)
                        .ok_or(ErrorCode::OutputAmountOverflow)?;
                    crossings = crossings.checked_add(1).ok_or(ErrorCode::BrokenInvariant)?;
                    if remaining == 0 {
                        return Ok(ExplicitCurveQuote {
                            amount_in: total_in,
                            amount_out,
                            end: point,
                            end_branch: self.branch(point),
                            boundary_crossings: crossings,
                        });
                    }
                    continue;
                }
            }

            let (effective_base, effective_quote) = self.effective_reserves(point, branch)?;
            let input = match direction {
                ExplicitCurveDirection::BaseToQuote => cpmm_amount_in_nad(effective_base, effective_quote, remaining)?,
                ExplicitCurveDirection::QuoteToBase => cpmm_amount_in_nad(effective_quote, effective_base, remaining)?,
            };
            require!(input > 0, ErrorCode::InsufficientOutputAmount);
            match direction {
                ExplicitCurveDirection::BaseToQuote => {
                    point.base_reserve = point
                        .base_reserve
                        .checked_add(input)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    point.quote_reserve = point
                        .quote_reserve
                        .checked_sub(remaining)
                        .ok_or(ErrorCode::InsufficientLiquidity)?;
                }
                ExplicitCurveDirection::QuoteToBase => {
                    point.quote_reserve = point
                        .quote_reserve
                        .checked_add(input)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    point.base_reserve = point
                        .base_reserve
                        .checked_sub(remaining)
                        .ok_or(ErrorCode::InsufficientLiquidity)?;
                }
            }
            total_in = total_in.checked_add(input).ok_or(ErrorCode::OutputAmountOverflow)?;
            return Ok(ExplicitCurveQuote {
                amount_in: total_in,
                amount_out,
                end: point,
                end_branch: self.branch(point),
                boundary_crossings: crossings,
            });
        }
        err!(ErrorCode::BrokenInvariant)
    }
}

fn products_rounding_compatible(a: u128, b: u128, c: u128, d: u128) -> Result<bool> {
    let b_plus_one = b.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
    let d_plus_one = d.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
    Ok(ratio_lte_full_width(a, c, d_plus_one, b)? && ratio_lte_full_width(c, a, b_plus_one, d)?)
}

#[cfg(test)]
mod tests {
    include!("../tests/math/explicit_curve.rs");
}

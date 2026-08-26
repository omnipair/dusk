//! Explicit CPMM-tail plus nested core-and-shoulder concentrated curve.
//!
//! A prepared curve is a continuous five-segment reserve path: lower tail,
//! lower shoulder, full-depth core, upper shoulder, and upper tail. Quotes
//! cross at most four precomputed boundaries and never solve an invariant root.

use anchor_lang::prelude::*;

use crate::errors::ErrorCode;

use crate::constants::NAD;
use crate::math::{
    cpmm_amount_in_nad, cpmm_amount_out_nad, geometric_mean_floor, mul_div_u128, ratio_lte_full_width, sqrt_ratio_nad,
};

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
    LowerShoulder,
    Inner,
    UpperShoulder,
    UpperTail,
}

impl ExplicitCurveBranch {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::LowerTail => 0,
            Self::LowerShoulder => 1,
            Self::Inner => 2,
            Self::UpperShoulder => 3,
            Self::UpperTail => 4,
        }
    }
}

fn nested_branch_from_region(region: usize) -> ExplicitCurveBranch {
    match region {
        0 => ExplicitCurveBranch::UpperTail,
        1 => ExplicitCurveBranch::UpperShoulder,
        2 => ExplicitCurveBranch::Inner,
        3 => ExplicitCurveBranch::LowerShoulder,
        _ => ExplicitCurveBranch::LowerTail,
    }
}

fn nested_region_from_branch(branch: ExplicitCurveBranch) -> usize {
    match branch {
        ExplicitCurveBranch::UpperTail => 0,
        ExplicitCurveBranch::UpperShoulder => 1,
        ExplicitCurveBranch::Inner => 2,
        ExplicitCurveBranch::LowerShoulder => 3,
        ExplicitCurveBranch::LowerTail => 4,
    }
}

/// Serialized curve-cache math revision. Pre-deployment iterations remain at
/// revision 1; increment only when supporting an already-deployed cache whose
/// mathematical interpretation must remain distinguishable.
pub(crate) const EXPLICIT_CURVE_MATH_REVISION: u8 = 1;

/// Product-facing governance surface for the explicit curve.
///
/// `peak_amplification_nad` is center liquidity depth relative to a CPMM with
/// the same center reserves. `core_half_width_bps` keeps that complete depth
/// active around the sticky center. `fade_width_bps` adds one deterministic
/// half-depth shoulder before the nonzero full-range CPMM tail. One-times
/// amplification with zero widths is exact CPMM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExplicitCurveParameters {
    pub peak_amplification_nad: u64,
    pub core_half_width_bps: u16,
    pub fade_width_bps: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ExplicitCurveCache {
    pub math_revision: u8,
    pub peak_amplification_nad: u64,
    pub core_half_width_bps: u16,
    pub fade_width_bps: u16,
    pub tail_liquidity: u128,
    pub concentrated_liquidity: u128,
    pub core_lower_sqrt_price_nad: u128,
    pub core_upper_sqrt_price_nad: u128,
    pub outer_lower_sqrt_price_nad: u128,
    pub outer_upper_sqrt_price_nad: u128,
}

impl ExplicitCurveCache {
    pub(crate) fn geometry(self) -> Result<ExplicitCurveGeometry> {
        require_eq!(
            self.math_revision,
            EXPLICIT_CURVE_MATH_REVISION,
            ErrorCode::BrokenInvariant
        );
        let sqrt_price_scale = NAD as u128;
        if self.fade_width_bps == 0 {
            if self.concentrated_liquidity == 0 {
                require!(self.tail_liquidity > 0, ErrorCode::InsufficientLiquidity);
                return Ok(ExplicitCurveGeometry::cpmm());
            }
            require!(
                self.tail_liquidity > 0
                    && self.core_lower_sqrt_price_nad > 0
                    && self.core_lower_sqrt_price_nad < self.core_upper_sqrt_price_nad,
                ErrorCode::InvalidMarketConfig
            );
            let inner_base_amplification_offset = mul_div_u128(
                self.concentrated_liquidity,
                sqrt_price_scale,
                self.core_upper_sqrt_price_nad,
            )?;
            let inner_quote_amplification_offset = mul_div_u128(
                self.concentrated_liquidity,
                self.core_lower_sqrt_price_nad,
                sqrt_price_scale,
            )?;
            let lower_tail_base_inventory = mul_div_u128(
                self.concentrated_liquidity,
                sqrt_price_scale,
                self.core_lower_sqrt_price_nad,
            )?
            .checked_sub(inner_base_amplification_offset)
            .ok_or(ErrorCode::BrokenInvariant)?;
            let lower_boundary = ExplicitCurvePoint {
                base_reserve: mul_div_u128(self.tail_liquidity, sqrt_price_scale, self.core_lower_sqrt_price_nad)?
                    .checked_add(lower_tail_base_inventory)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                quote_reserve: mul_div_u128(self.tail_liquidity, self.core_lower_sqrt_price_nad, sqrt_price_scale)?,
            };
            let upper_base_reserve =
                mul_div_u128(self.tail_liquidity, sqrt_price_scale, self.core_upper_sqrt_price_nad)?;
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
            let geometry = ExplicitCurveGeometry {
                inner_liquidity: self.concentrated_liquidity,
                inner_base_amplification_offset,
                inner_quote_amplification_offset,
                lower_tail_base_inventory,
                upper_tail_quote_inventory,
                lower_boundary,
                upper_boundary: ExplicitCurvePoint {
                    base_reserve: upper_base_reserve,
                    quote_reserve: upper_quote_reserve,
                },
                ..ExplicitCurveGeometry::cpmm()
            };
            geometry.validate()?;
            Ok(geometry)
        } else {
            require!(
                self.tail_liquidity > 0
                    && self.concentrated_liquidity >= 2
                    && self.outer_lower_sqrt_price_nad < self.core_lower_sqrt_price_nad
                    && self.core_lower_sqrt_price_nad < self.core_upper_sqrt_price_nad
                    && self.core_upper_sqrt_price_nad < self.outer_upper_sqrt_price_nad,
                ErrorCode::InvalidMarketConfig
            );
            let core_liquidity = self.concentrated_liquidity.div_ceil(2);
            let shoulder_liquidity = self
                .concentrated_liquidity
                .checked_sub(core_liquidity)
                .ok_or(ErrorCode::BrokenInvariant)?;
            let layers = [
                ExplicitCurveLayer {
                    liquidity: core_liquidity,
                    lower_sqrt_price: self.core_lower_sqrt_price_nad,
                    upper_sqrt_price: self.core_upper_sqrt_price_nad,
                },
                ExplicitCurveLayer {
                    liquidity: shoulder_liquidity,
                    lower_sqrt_price: self.outer_lower_sqrt_price_nad,
                    upper_sqrt_price: self.outer_upper_sqrt_price_nad,
                },
            ];
            let mut geometry = ExplicitCurveGeometry {
                inner_liquidity: self.concentrated_liquidity,
                shoulder_liquidity,
                nested_boundary_count: 4,
                ..ExplicitCurveGeometry::cpmm()
            };
            geometry.nested_boundaries = [
                nested_point_at_sqrt_price(
                    self.tail_liquidity,
                    layers,
                    self.outer_upper_sqrt_price_nad,
                    sqrt_price_scale,
                )?,
                nested_point_at_sqrt_price(
                    self.tail_liquidity,
                    layers,
                    self.core_upper_sqrt_price_nad,
                    sqrt_price_scale,
                )?,
                nested_point_at_sqrt_price(
                    self.tail_liquidity,
                    layers,
                    self.core_lower_sqrt_price_nad,
                    sqrt_price_scale,
                )?,
                nested_point_at_sqrt_price(
                    self.tail_liquidity,
                    layers,
                    self.outer_lower_sqrt_price_nad,
                    sqrt_price_scale,
                )?,
            ];
            for region in 0..5 {
                let (base, quote) = nested_region_constants(layers, region, sqrt_price_scale)?;
                geometry.nested_base_constants[region] = base;
                geometry.nested_quote_constants[region] = quote;
            }
            geometry.validate()?;
            Ok(geometry)
        }
    }

    pub(crate) const fn parameters(self) -> ExplicitCurveParameters {
        ExplicitCurveParameters {
            peak_amplification_nad: self.peak_amplification_nad,
            core_half_width_bps: self.core_half_width_bps,
            fade_width_bps: self.fade_width_bps,
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
        let center_point = geometry.point_at_price_nad(center_price_nad as u128, self.tail_liquidity)?;
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
    core_lower_sqrt_price_nad: u128,
    core_upper_sqrt_price_nad: u128,
    outer_lower_sqrt_price_nad: u128,
    outer_upper_sqrt_price_nad: u128,
    _sqrt_center_nad: u128,
) -> Result<ExplicitCurveCache> {
    let cache = ExplicitCurveCache {
        math_revision: EXPLICIT_CURVE_MATH_REVISION,
        peak_amplification_nad: parameters.peak_amplification_nad,
        core_half_width_bps: parameters.core_half_width_bps,
        fade_width_bps: parameters.fade_width_bps,
        tail_liquidity,
        concentrated_liquidity,
        core_lower_sqrt_price_nad,
        core_upper_sqrt_price_nad,
        outer_lower_sqrt_price_nad,
        outer_upper_sqrt_price_nad,
    };
    cache.geometry()?;
    Ok(cache)
}

impl ExplicitCurveParameters {
    #[cfg(test)]
    pub(crate) const fn cpmm() -> Self {
        Self {
            peak_amplification_nad: NAD,
            core_half_width_bps: 0,
            fade_width_bps: 0,
        }
    }

    pub(crate) const fn is_cpmm(self) -> bool {
        self.peak_amplification_nad == NAD
    }

    pub(crate) fn validate(self, max_amplification_nad: u64) -> Result<()> {
        if self.is_cpmm() {
            require!(
                self.core_half_width_bps == 0 && self.fade_width_bps == 0,
                ErrorCode::InvalidMarketConfig
            );
            return Ok(());
        }
        require!(
            self.peak_amplification_nad > NAD
                && self.peak_amplification_nad <= max_amplification_nad
                && self.core_half_width_bps > 0,
            ErrorCode::InvalidMarketConfig
        );
        require!(
            u32::from(self.core_half_width_bps) + u32::from(self.fade_width_bps) <= u16::MAX as u32,
            ErrorCode::InvalidMarketConfig
        );
        let shares = self.liquidity_shares_nad()?;
        require!(shares.0 > 0 && shares.1 > 0, ErrorCode::InvalidMarketConfig);
        if self.fade_width_bps > 0 {
            require!(shares.1 >= 2, ErrorCode::InvalidMarketConfig);
        }
        Ok(())
    }

    fn width_ratio_nad(width_bps: u32) -> Result<u128> {
        (NAD as u128)
            .checked_add(mul_div_u128(NAD as u128, width_bps as u128, 10_000)?)
            .ok_or_else(|| ErrorCode::InvariantOverflow.into())
    }

    fn sqrt_widths_nad(self) -> Result<(u128, u128)> {
        let core = Self::width_ratio_nad(u32::from(self.core_half_width_bps))?;
        let outer = Self::width_ratio_nad(
            u32::from(self.core_half_width_bps)
                .checked_add(u32::from(self.fade_width_bps))
                .ok_or(ErrorCode::InvalidMarketConfig)?,
        )?;
        Ok((sqrt_ratio_nad(core)?, sqrt_ratio_nad(outer)?))
    }

    /// Returns `(tail, concentrated)` shares of peak active liquidity. The
    /// concentrated share is derived so the configured peak amplification is
    /// exact at the centered reserve point; it is never a governance input.
    pub(crate) fn liquidity_shares_nad(self) -> Result<(u128, u128)> {
        if self.is_cpmm() {
            return Ok((NAD as u128, 0));
        }
        let (core_sqrt_width, outer_sqrt_width) = self.sqrt_widths_nad()?;
        let core_inverse = mul_div_u128(NAD as u128, NAD as u128, core_sqrt_width)?;
        let average_inverse = if self.fade_width_bps == 0 {
            core_inverse
        } else {
            let outer_inverse = mul_div_u128(NAD as u128, NAD as u128, outer_sqrt_width)?;
            core_inverse
                .checked_add(outer_inverse)
                .ok_or(ErrorCode::InvariantOverflow)?
                / 2
        };
        let inverse_amplification = mul_div_u128(NAD as u128, NAD as u128, self.peak_amplification_nad as u128)?;
        let concentrated_share = mul_div_u128(
            (NAD as u128)
                .checked_sub(inverse_amplification)
                .ok_or(ErrorCode::InvalidMarketConfig)?,
            NAD as u128,
            average_inverse,
        )?;
        require!(concentrated_share < NAD as u128, ErrorCode::InvalidMarketConfig);
        Ok((NAD as u128 - concentrated_share, concentrated_share))
    }

    #[cfg(test)]
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
    complete_explicit_cache(
        parameters,
        tail_liquidity,
        concentrated_liquidity,
        core_lower_sqrt_price_nad,
        core_upper_sqrt_price_nad,
        outer_lower_sqrt_price_nad,
        outer_upper_sqrt_price_nad,
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
        return complete_explicit_cache(parameters, tail_liquidity, 0, 0, 0, 0, 0, sqrt_center_nad);
    }

    let sqrt_center_nad = sqrt_ratio_nad(center_price_nad as u128)?;
    let (core_sqrt_width, outer_sqrt_width) = parameters.sqrt_widths_nad()?;
    let core_lower_sqrt_price_nad = mul_div_u128(sqrt_center_nad, NAD as u128, core_sqrt_width)?;
    let core_upper_sqrt_price_nad = mul_div_u128(sqrt_center_nad, core_sqrt_width, NAD as u128)?;
    let outer_lower_sqrt_price_nad = mul_div_u128(sqrt_center_nad, NAD as u128, outer_sqrt_width)?;
    let outer_upper_sqrt_price_nad = mul_div_u128(sqrt_center_nad, outer_sqrt_width, NAD as u128)?;
    let (_, concentrated_share) = parameters.liquidity_shares_nad()?;
    let point = ExplicitCurvePoint {
        base_reserve,
        quote_reserve,
    };

    for branch in [
        ExplicitCurveBranch::Inner,
        ExplicitCurveBranch::UpperShoulder,
        ExplicitCurveBranch::LowerShoulder,
        ExplicitCurveBranch::LowerTail,
        ExplicitCurveBranch::UpperTail,
    ] {
        if parameters.fade_width_bps == 0
            && matches!(
                branch,
                ExplicitCurveBranch::UpperShoulder | ExplicitCurveBranch::LowerShoulder
            )
        {
            continue;
        }
        let total_liquidity = if parameters.fade_width_bps == 0 {
            explicit_total_liquidity_root(
                point,
                concentrated_share,
                core_lower_sqrt_price_nad,
                core_upper_sqrt_price_nad,
                branch,
            )?
        } else {
            let region = nested_region_from_branch(branch);
            require!(region <= 4, ErrorCode::InvalidMarketConfig);
            let (tail_share_nad, concentrated_share_nad) = parameters.liquidity_shares_nad()?;
            let core_share_nad = concentrated_share_nad.div_ceil(2);
            let shoulder_share_nad = concentrated_share_nad
                .checked_sub(core_share_nad)
                .ok_or(ErrorCode::BrokenInvariant)?;
            let core_liquidity = mul_div_u128(NESTED_SHAPE_SCALE, core_share_nad, NAD as u128)?;
            let shoulder_liquidity = mul_div_u128(NESTED_SHAPE_SCALE, shoulder_share_nad, NAD as u128)?;
            let tail_liquidity = NESTED_SHAPE_SCALE
                .checked_sub(core_liquidity)
                .and_then(|value| value.checked_sub(shoulder_liquidity))
                .ok_or(ErrorCode::BrokenInvariant)?;
            let expected_tail = mul_div_u128(NESTED_SHAPE_SCALE, tail_share_nad, NAD as u128)?;
            require!(tail_liquidity.abs_diff(expected_tail) <= 2, ErrorCode::BrokenInvariant);
            let layers = [
                ExplicitCurveLayer {
                    liquidity: core_liquidity,
                    lower_sqrt_price: core_lower_sqrt_price_nad,
                    upper_sqrt_price: core_upper_sqrt_price_nad,
                },
                ExplicitCurveLayer {
                    liquidity: shoulder_liquidity,
                    lower_sqrt_price: outer_lower_sqrt_price_nad,
                    upper_sqrt_price: outer_upper_sqrt_price_nad,
                },
            ];
            let (base_constant, quote_constant) = nested_region_constants(layers, region, NAD as u128)?;
            let active_liquidity = match region {
                0 | 4 => tail_liquidity,
                1 | 3 => tail_liquidity
                    .checked_add(shoulder_liquidity)
                    .ok_or(ErrorCode::InvariantOverflow)?,
                2 => NESTED_SHAPE_SCALE,
                _ => unreachable!(),
            };
            let active_squared = U512::from(active_liquidity)
                .checked_mul(U512::from(active_liquidity))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let constant_product = U512::from(base_constant.magnitude)
                .checked_mul(U512::from(quote_constant.magnitude))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let constants_negative = base_constant.negative ^ quote_constant.negative;
            let quadratic = if constants_negative {
                active_squared
                    .checked_add(constant_product)
                    .ok_or(ErrorCode::InvariantOverflow)?
            } else {
                active_squared
                    .checked_sub(constant_product)
                    .ok_or(ErrorCode::InvalidMarketConfig)?
            };
            require!(!quadratic.is_zero(), ErrorCode::InvalidMarketConfig);

            let lhs = SignedWide::from_product(quote_constant.negative, point.base_reserve, quote_constant.magnitude)?;
            let rhs = SignedWide::from_product(base_constant.negative, point.quote_reserve, base_constant.magnitude)?;
            let linear = if lhs.negative == rhs.negative {
                let magnitude = lhs
                    .magnitude
                    .checked_add(rhs.magnitude)
                    .ok_or(ErrorCode::InvariantOverflow)?;
                SignedWide {
                    negative: lhs.negative && !magnitude.is_zero(),
                    magnitude,
                }
            } else if lhs.magnitude >= rhs.magnitude {
                let magnitude = lhs
                    .magnitude
                    .checked_sub(rhs.magnitude)
                    .ok_or(ErrorCode::BrokenInvariant)?;
                SignedWide {
                    negative: lhs.negative && !magnitude.is_zero(),
                    magnitude,
                }
            } else {
                let magnitude = rhs
                    .magnitude
                    .checked_sub(lhs.magnitude)
                    .ok_or(ErrorCode::BrokenInvariant)?;
                SignedWide {
                    negative: rhs.negative && !magnitude.is_zero(),
                    magnitude,
                }
            };
            let scaled_linear = linear
                .magnitude
                .checked_mul(U512::from(NESTED_SHAPE_SCALE))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let scale_squared = U512::from(NESTED_SHAPE_SCALE)
                .checked_mul(U512::from(NESTED_SHAPE_SCALE))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let constant = U512::from(point.base_reserve)
                .checked_mul(U512::from(point.quote_reserve))
                .and_then(|value| value.checked_mul(scale_squared))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let discriminant = scaled_linear
                .checked_mul(scaled_linear)
                .and_then(|value| {
                    quadratic
                        .checked_mul(constant)?
                        .checked_mul(U512::from(4_u8))?
                        .checked_add(value)
                })
                .ok_or(ErrorCode::InvariantOverflow)?;
            let root = sqrt_floor_u512(discriminant)?;
            let numerator = if linear.negative {
                root.checked_add(scaled_linear).ok_or(ErrorCode::InvariantOverflow)?
            } else {
                root.checked_sub(scaled_linear).ok_or(ErrorCode::BrokenInvariant)?
            };
            let denominator = quadratic
                .checked_mul(U512::from(2_u8))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let liquidity = numerator / denominator;
            require!(liquidity <= U512::from(u128::MAX), ErrorCode::InvariantOverflow);
            liquidity.as_u128()
        };
        if total_liquidity == 0 {
            continue;
        }
        let concentrated_liquidity = mul_div_u128(total_liquidity, concentrated_share, NAD as u128)?;
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
            core_lower_sqrt_price_nad,
            core_upper_sqrt_price_nad,
            outer_lower_sqrt_price_nad,
            outer_upper_sqrt_price_nad,
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
        ExplicitCurveBranch::LowerShoulder | ExplicitCurveBranch::UpperShoulder => {
            return err!(ErrorCode::InvalidMarketConfig)
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

const NESTED_SHAPE_SCALE: u128 = 1_000_000_000_000_000_000;

#[derive(Clone, Debug)]
struct SignedWide {
    negative: bool,
    magnitude: U512,
}

impl SignedWide {
    fn from_product(negative: bool, lhs: u128, rhs: u128) -> Result<Self> {
        let magnitude = U512::from(lhs)
            .checked_mul(U512::from(rhs))
            .ok_or(ErrorCode::InvariantOverflow)?;
        Ok(Self {
            negative: negative && !magnitude.is_zero(),
            magnitude,
        })
    }
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
pub struct ExplicitSignedReserve {
    pub negative: bool,
    pub magnitude: u128,
}

impl ExplicitSignedReserve {
    fn add_positive(&mut self, value: u128) -> Result<()> {
        if self.negative {
            if value >= self.magnitude {
                self.magnitude = value.checked_sub(self.magnitude).ok_or(ErrorCode::BrokenInvariant)?;
                self.negative = false;
            } else {
                self.magnitude = self.magnitude.checked_sub(value).ok_or(ErrorCode::BrokenInvariant)?;
            }
        } else {
            self.magnitude = self.magnitude.checked_add(value).ok_or(ErrorCode::InvariantOverflow)?;
        }
        Ok(())
    }

    fn subtract_positive(&mut self, value: u128) -> Result<()> {
        if self.negative {
            self.magnitude = self.magnitude.checked_add(value).ok_or(ErrorCode::InvariantOverflow)?;
        } else if self.magnitude >= value {
            self.magnitude = self.magnitude.checked_sub(value).ok_or(ErrorCode::BrokenInvariant)?;
        } else {
            self.magnitude = value.checked_sub(self.magnitude).ok_or(ErrorCode::BrokenInvariant)?;
            self.negative = true;
        }
        Ok(())
    }

    fn subtract_from(self, value: u128) -> Result<u128> {
        if self.negative {
            value
                .checked_add(self.magnitude)
                .ok_or_else(|| ErrorCode::InvariantOverflow.into())
        } else {
            value
                .checked_sub(self.magnitude)
                .ok_or_else(|| ErrorCode::BrokenInvariant.into())
        }
    }

    fn add_to(self, value: u128) -> Result<u128> {
        if self.negative {
            value
                .checked_sub(self.magnitude)
                .ok_or_else(|| ErrorCode::BrokenInvariant.into())
        } else {
            value
                .checked_add(self.magnitude)
                .ok_or_else(|| ErrorCode::InvariantOverflow.into())
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ExplicitCurveGeometry {
    /// Zero selects exact CPMM and requires every other geometry field to be
    /// zero. Nonzero supplies the compact three-segment base representation;
    /// a nonzero shoulder below extends it to the five-segment product curve.
    pub inner_liquidity: u128,
    /// Positive offsets used by the shifted constant product inside the band.
    pub inner_base_amplification_offset: u128,
    pub inner_quote_amplification_offset: u128,
    /// Fixed concentrated inventory left behind in the corresponding tail.
    pub lower_tail_base_inventory: u128,
    pub upper_tail_quote_inventory: u128,
    pub lower_boundary: ExplicitCurvePoint,
    pub upper_boundary: ExplicitCurvePoint,
    /// A nonzero shoulder selects the five-segment nested curve. The original
    /// one-band fields above remain the compact representation when this is
    /// zero.
    pub shoulder_liquidity: u128,
    pub nested_boundary_count: u8,
    pub nested_boundaries: [ExplicitCurvePoint; 4],
    pub nested_base_constants: [ExplicitSignedReserve; 5],
    pub nested_quote_constants: [ExplicitSignedReserve; 5],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExplicitCurveQuote {
    pub amount_in: u128,
    pub amount_out: u128,
    pub end: ExplicitCurvePoint,
    pub end_branch: ExplicitCurveBranch,
    pub boundary_crossings: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ExplicitCurveLayer {
    liquidity: u128,
    lower_sqrt_price: u128,
    upper_sqrt_price: u128,
}

fn nested_point_at_sqrt_price(
    tail_liquidity: u128,
    layers: [ExplicitCurveLayer; 2],
    sqrt_price: u128,
    sqrt_price_scale: u128,
) -> Result<ExplicitCurvePoint> {
    require!(sqrt_price > 0, ErrorCode::InvalidSettlementPrice);
    let mut base_reserve = mul_div_u128(tail_liquidity, sqrt_price_scale, sqrt_price)?;
    let mut quote_reserve = mul_div_u128(tail_liquidity, sqrt_price, sqrt_price_scale)?;
    for layer in layers {
        let base_at_lower = mul_div_u128(layer.liquidity, sqrt_price_scale, layer.lower_sqrt_price)?;
        let base_at_upper = mul_div_u128(layer.liquidity, sqrt_price_scale, layer.upper_sqrt_price)?;
        let quote_at_lower = mul_div_u128(layer.liquidity, layer.lower_sqrt_price, sqrt_price_scale)?;
        let quote_at_upper = mul_div_u128(layer.liquidity, layer.upper_sqrt_price, sqrt_price_scale)?;
        let (base_claim, quote_claim) = if sqrt_price <= layer.lower_sqrt_price {
            (
                base_at_lower
                    .checked_sub(base_at_upper)
                    .ok_or(ErrorCode::BrokenInvariant)?,
                0,
            )
        } else if sqrt_price >= layer.upper_sqrt_price {
            (
                0,
                quote_at_upper
                    .checked_sub(quote_at_lower)
                    .ok_or(ErrorCode::BrokenInvariant)?,
            )
        } else {
            (
                mul_div_u128(layer.liquidity, sqrt_price_scale, sqrt_price)?
                    .checked_sub(base_at_upper)
                    .ok_or(ErrorCode::BrokenInvariant)?,
                mul_div_u128(layer.liquidity, sqrt_price, sqrt_price_scale)?
                    .checked_sub(quote_at_lower)
                    .ok_or(ErrorCode::BrokenInvariant)?,
            )
        };
        base_reserve = base_reserve
            .checked_add(base_claim)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        quote_reserve = quote_reserve
            .checked_add(quote_claim)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    }
    Ok(ExplicitCurvePoint {
        base_reserve,
        quote_reserve,
    })
}

fn nested_region_constants(
    layers: [ExplicitCurveLayer; 2],
    region: usize,
    sqrt_price_scale: u128,
) -> Result<(ExplicitSignedReserve, ExplicitSignedReserve)> {
    require!(region <= 4, ErrorCode::BrokenInvariant);
    let states = match region {
        0 => [2_u8, 2_u8],
        1 => [2, 1],
        2 => [1, 1],
        3 => [0, 1],
        4 => [0, 0],
        _ => unreachable!(),
    };
    let mut base = ExplicitSignedReserve::default();
    let mut quote = ExplicitSignedReserve::default();
    for (layer, state) in layers.into_iter().zip(states) {
        let base_at_lower = mul_div_u128(layer.liquidity, sqrt_price_scale, layer.lower_sqrt_price)?;
        let base_at_upper = mul_div_u128(layer.liquidity, sqrt_price_scale, layer.upper_sqrt_price)?;
        let quote_at_lower = mul_div_u128(layer.liquidity, layer.lower_sqrt_price, sqrt_price_scale)?;
        let quote_at_upper = mul_div_u128(layer.liquidity, layer.upper_sqrt_price, sqrt_price_scale)?;
        match state {
            0 => base.add_positive(
                base_at_lower
                    .checked_sub(base_at_upper)
                    .ok_or(ErrorCode::BrokenInvariant)?,
            )?,
            1 => {
                base.subtract_positive(base_at_upper)?;
                quote.subtract_positive(quote_at_lower)?;
            }
            2 => quote.add_positive(
                quote_at_upper
                    .checked_sub(quote_at_lower)
                    .ok_or(ErrorCode::BrokenInvariant)?,
            )?,
            _ => unreachable!(),
        }
    }
    Ok((base, quote))
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
            shoulder_liquidity: 0,
            nested_boundary_count: 0,
            nested_boundaries: [ExplicitCurvePoint {
                base_reserve: 0,
                quote_reserve: 0,
            }; 4],
            nested_base_constants: [ExplicitSignedReserve {
                negative: false,
                magnitude: 0,
            }; 5],
            nested_quote_constants: [ExplicitSignedReserve {
                negative: false,
                magnitude: 0,
            }; 5],
        }
    }

    pub(crate) const fn is_cpmm(self) -> bool {
        self.inner_liquidity == 0
    }

    pub(crate) const fn is_nested(self) -> bool {
        self.shoulder_liquidity > 0
    }

    pub(crate) fn validate(self) -> Result<()> {
        if self.is_cpmm() {
            require!(self == Self::cpmm(), ErrorCode::InvalidMarketConfig);
            return Ok(());
        }

        if self.is_nested() {
            require_eq!(self.nested_boundary_count, 4, ErrorCode::InvalidMarketConfig);
            require!(
                self.inner_liquidity > self.shoulder_liquidity
                    && self.inner_base_amplification_offset == 0
                    && self.inner_quote_amplification_offset == 0
                    && self.lower_tail_base_inventory == 0
                    && self.upper_tail_quote_inventory == 0
                    && self.lower_boundary == ExplicitCurvePoint::default()
                    && self.upper_boundary == ExplicitCurvePoint::default(),
                ErrorCode::InvalidMarketConfig
            );
            require!(
                self.nested_boundaries
                    .windows(2)
                    .all(|window| window[0].base_reserve < window[1].base_reserve
                        && window[0].quote_reserve > window[1].quote_reserve),
                ErrorCode::InvalidMarketConfig
            );
            for (region, boundary) in self.nested_boundaries.into_iter().enumerate() {
                let left = self.nested_effective_reserves(boundary, region)?;
                let right = self.nested_effective_reserves(boundary, region + 1)?;
                let left_price = mul_div_u128(left.1, NAD as u128, left.0)?;
                let right_price = mul_div_u128(right.1, NAD as u128, right.0)?;
                require!(left_price.abs_diff(right_price) <= 2, ErrorCode::InvalidMarketConfig);
            }
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
        if self.is_nested() {
            let region = self
                .nested_boundaries
                .iter()
                .take(self.nested_boundary_count as usize)
                .position(|boundary| point.base_reserve <= boundary.base_reserve)
                .unwrap_or(self.nested_boundary_count as usize);
            return nested_branch_from_region(region);
        }
        if self.is_cpmm() || point.base_reserve >= self.lower_boundary.base_reserve {
            ExplicitCurveBranch::LowerTail
        } else if point.base_reserve <= self.upper_boundary.base_reserve {
            ExplicitCurveBranch::UpperTail
        } else {
            ExplicitCurveBranch::Inner
        }
    }

    fn nested_effective_reserves(self, point: ExplicitCurvePoint, region: usize) -> Result<(u128, u128)> {
        require!(
            region <= self.nested_boundary_count as usize,
            ErrorCode::BrokenInvariant
        );
        let base = self.nested_base_constants[region].subtract_from(point.base_reserve)?;
        let quote = self.nested_quote_constants[region].subtract_from(point.quote_reserve)?;
        require!(base > 0 && quote > 0, ErrorCode::InsufficientLiquidity);
        Ok((base, quote))
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
        if self.is_nested() {
            let upper_boundary = self.nested_boundaries[0];
            let lower_boundary = self.nested_boundaries[3];
            let (upper_base, upper_quote) = self.nested_effective_reserves(upper_boundary, 0)?;
            let (lower_base, lower_quote) = self.nested_effective_reserves(lower_boundary, 4)?;
            return Ok(Some((
                mul_div_u128(lower_quote, NAD as u128, lower_base)?,
                mul_div_u128(upper_quote, NAD as u128, upper_base)?,
            )));
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
        if self.is_nested() {
            let mut region = self.nested_boundary_count as usize;
            for index in 0..self.nested_boundary_count as usize {
                let boundary = self.nested_boundaries[index];
                let (base, quote) = self.nested_effective_reserves(boundary, index)?;
                let boundary_price = mul_div_u128(quote, NAD as u128, base)?;
                if price_nad >= boundary_price {
                    region = index;
                    break;
                }
            }
            let core_liquidity = self
                .inner_liquidity
                .checked_sub(self.shoulder_liquidity)
                .ok_or(ErrorCode::BrokenInvariant)?;
            let active_concentrated = match region {
                0 | 4 => 0,
                1 | 3 => self.shoulder_liquidity,
                2 => self
                    .shoulder_liquidity
                    .checked_add(core_liquidity)
                    .ok_or(ErrorCode::InvariantOverflow)?,
                _ => return err!(ErrorCode::BrokenInvariant),
            };
            let liquidity = tail_liquidity
                .checked_add(active_concentrated)
                .ok_or(ErrorCode::InvariantOverflow)?;
            let effective_base = mul_div_u128(liquidity, NAD as u128, sqrt_price_nad)?;
            let effective_quote = mul_div_u128(liquidity, sqrt_price_nad, NAD as u128)?;
            return Ok(ExplicitCurvePoint {
                base_reserve: self.nested_base_constants[region].add_to(effective_base)?,
                quote_reserve: self.nested_quote_constants[region].add_to(effective_quote)?,
            });
        }
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
            ExplicitCurveBranch::LowerShoulder | ExplicitCurveBranch::UpperShoulder => {
                return err!(ErrorCode::BrokenInvariant)
            }
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
            ExplicitCurveBranch::LowerShoulder | ExplicitCurveBranch::UpperShoulder => {
                err!(ErrorCode::BrokenInvariant)
            }
        }
    }

    fn branch_for_direction(self, point: ExplicitCurvePoint, direction: ExplicitCurveDirection) -> ExplicitCurveBranch {
        if self.is_nested() {
            let region = match direction {
                ExplicitCurveDirection::BaseToQuote => self
                    .nested_boundaries
                    .iter()
                    .take(self.nested_boundary_count as usize)
                    .position(|boundary| point.base_reserve < boundary.base_reserve)
                    .unwrap_or(self.nested_boundary_count as usize),
                ExplicitCurveDirection::QuoteToBase => self
                    .nested_boundaries
                    .iter()
                    .take(self.nested_boundary_count as usize)
                    .position(|boundary| point.quote_reserve >= boundary.quote_reserve)
                    .unwrap_or(self.nested_boundary_count as usize),
            };
            return nested_branch_from_region(region);
        }
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
        if self.is_nested() {
            return self.nested_effective_reserves(point, nested_region_from_branch(branch));
        }
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
            ExplicitCurveBranch::LowerShoulder | ExplicitCurveBranch::UpperShoulder => {
                return err!(ErrorCode::BrokenInvariant)
            }
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
        if self.is_nested() {
            let region = nested_region_from_branch(branch);
            return match direction {
                ExplicitCurveDirection::BaseToQuote => {
                    if region < self.nested_boundary_count as usize {
                        Some(self.nested_boundaries[region])
                    } else {
                        None
                    }
                }
                ExplicitCurveDirection::QuoteToBase => {
                    if region > 0 {
                        Some(self.nested_boundaries[region - 1])
                    } else {
                        None
                    }
                }
            };
        }
        if self.is_cpmm() {
            return None;
        }
        match (direction, branch) {
            (ExplicitCurveDirection::BaseToQuote, ExplicitCurveBranch::UpperTail) => Some(self.upper_boundary),
            (ExplicitCurveDirection::BaseToQuote, ExplicitCurveBranch::Inner) => Some(self.lower_boundary),
            (ExplicitCurveDirection::QuoteToBase, ExplicitCurveBranch::LowerTail) => Some(self.lower_boundary),
            (ExplicitCurveDirection::QuoteToBase, ExplicitCurveBranch::Inner) => Some(self.upper_boundary),
            (_, ExplicitCurveBranch::LowerShoulder | ExplicitCurveBranch::UpperShoulder) => None,
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

        for _ in 0..5 {
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

        for _ in 0..5 {
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
    include!("../../tests/math/explicit_curve.rs");
}

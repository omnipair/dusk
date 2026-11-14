//! Concentrated CPMM-tail plus nested core-and-shoulder concentrated curve.
//!
//! A prepared curve is a continuous five-segment reserve path: lower tail,
//! lower shoulder, full-depth core, upper shoulder, and upper tail. Quotes
//! cross at most four precomputed boundaries and never solve an invariant root.

use anchor_lang::prelude::*;

pub use crate::state::ConcentratedCurveParameters;
use crate::{errors::ErrorCode, state::ConcentratedCurveCache};

use crate::constants::NAD;
use crate::math::{
    div_rem_native, isqrt, mul_div_ceil_u128, mul_div_u128, ratio_lte_full_width, sqrt_ratio_nad, widening_mul_u64,
};

// uint 0.9's construct_uint! expansion uses legacy integer APIs and an
// expression-position semicolon. Rust 1.99 reports these at the call site;
// confine the compatibility allowances to the generated wide integer module.
#[allow(unknown_lints)] // The SBF toolchain predates Rust 1.99's renamed lint.
#[allow(
    clippy::assign_op_pattern,
    clippy::manual_div_ceil,
    deprecated,
    semicolon_in_expressions_from_non_local_macros
)]
mod wide {
    use uint::construct_uint;

    construct_uint! {
        pub struct U512(8);
    }
}

use wide::U512;

const CONCENTRATED_SQRT_MAX_ITERS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConcentratedCurveDirection {
    BaseToQuote,
    QuoteToBase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConcentratedCurveBranch {
    LowerTail,
    LowerShoulder,
    Inner,
    UpperShoulder,
    UpperTail,
}

impl ConcentratedCurveBranch {
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

fn nested_branch_from_region(region: usize) -> ConcentratedCurveBranch {
    match region {
        0 => ConcentratedCurveBranch::UpperTail,
        1 => ConcentratedCurveBranch::UpperShoulder,
        2 => ConcentratedCurveBranch::Inner,
        3 => ConcentratedCurveBranch::LowerShoulder,
        _ => ConcentratedCurveBranch::LowerTail,
    }
}

fn nested_region_from_branch(branch: ConcentratedCurveBranch) -> usize {
    match branch {
        ConcentratedCurveBranch::UpperTail => 0,
        ConcentratedCurveBranch::UpperShoulder => 1,
        ConcentratedCurveBranch::Inner => 2,
        ConcentratedCurveBranch::LowerShoulder => 3,
        ConcentratedCurveBranch::LowerTail => 4,
    }
}

/// Serialized curve-cache math revision. Pre-deployment iterations remain at
/// revision 1; increment only when supporting an already-deployed cache whose
/// mathematical interpretation must remain distinguishable.
pub(crate) const CONCENTRATED_CURVE_MATH_REVISION: u8 = 1;

impl ConcentratedCurveCache {
    #[inline(never)]
    pub(crate) fn geometry(self) -> Result<ConcentratedCurveGeometry> {
        require_eq!(
            self.math_revision,
            CONCENTRATED_CURVE_MATH_REVISION,
            ErrorCode::BrokenInvariant
        );
        let sqrt_price_scale = NAD as u128;
        if self.fade_width_bps == 0 {
            if self.concentrated_liquidity == 0 {
                require!(self.tail_liquidity > 0, ErrorCode::InsufficientLiquidity);
                return Ok(ConcentratedCurveGeometry::cpmm());
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
            let lower_boundary = ConcentratedCurvePoint {
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
            let geometry = ConcentratedCurveGeometry {
                inner_liquidity: self.concentrated_liquidity,
                inner_base_amplification_offset,
                inner_quote_amplification_offset,
                lower_tail_base_inventory,
                upper_tail_quote_inventory,
                lower_boundary,
                upper_boundary: ConcentratedCurvePoint {
                    base_reserve: upper_base_reserve,
                    quote_reserve: upper_quote_reserve,
                },
                ..ConcentratedCurveGeometry::cpmm()
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
                ConcentratedCurveLayer {
                    liquidity: core_liquidity,
                    lower_sqrt_price: self.core_lower_sqrt_price_nad,
                    upper_sqrt_price: self.core_upper_sqrt_price_nad,
                },
                ConcentratedCurveLayer {
                    liquidity: shoulder_liquidity,
                    lower_sqrt_price: self.outer_lower_sqrt_price_nad,
                    upper_sqrt_price: self.outer_upper_sqrt_price_nad,
                },
            ];
            // Every boundary and region uses the same four floored values
            // per layer. Compute them once, retaining identical rounding.
            let prepared_layers = [layers[0].prepare()?, layers[1].prepare()?];
            let mut geometry = ConcentratedCurveGeometry {
                inner_liquidity: self.concentrated_liquidity,
                shoulder_liquidity,
                nested_boundary_count: 4,
                ..ConcentratedCurveGeometry::cpmm()
            };
            geometry.nested_boundaries = [
                nested_point_at_sqrt_price(
                    self.tail_liquidity,
                    prepared_layers,
                    self.outer_upper_sqrt_price_nad,
                    sqrt_price_scale,
                )?,
                nested_point_at_sqrt_price(
                    self.tail_liquidity,
                    prepared_layers,
                    self.core_upper_sqrt_price_nad,
                    sqrt_price_scale,
                )?,
                nested_point_at_sqrt_price(
                    self.tail_liquidity,
                    prepared_layers,
                    self.core_lower_sqrt_price_nad,
                    sqrt_price_scale,
                )?,
                nested_point_at_sqrt_price(
                    self.tail_liquidity,
                    prepared_layers,
                    self.outer_lower_sqrt_price_nad,
                    sqrt_price_scale,
                )?,
            ];
            // Per layer, outermost upper region first: 2 above the layer's
            // range, 1 inside it, 0 below it.
            let region_states = [[2_u8, 2], [2, 1], [1, 1], [0, 1], [0, 0]];
            for (region, states) in region_states.into_iter().enumerate() {
                let mut base = ConcentratedSignedReserve::default();
                let mut quote = ConcentratedSignedReserve::default();
                for (layer, state) in prepared_layers.iter().zip(states) {
                    match state {
                        0 => base.add_positive(
                            layer
                                .lower
                                .base_reserve
                                .checked_sub(layer.upper.base_reserve)
                                .ok_or(ErrorCode::BrokenInvariant)?,
                        )?,
                        1 => {
                            base.subtract_positive(layer.upper.base_reserve)?;
                            quote.subtract_positive(layer.lower.quote_reserve)?;
                        }
                        _ => quote.add_positive(
                            layer
                                .upper
                                .quote_reserve
                                .checked_sub(layer.lower.quote_reserve)
                                .ok_or(ErrorCode::BrokenInvariant)?,
                        )?,
                    }
                }
                geometry.nested_base_constants[region] = base;
                geometry.nested_quote_constants[region] = quote;
            }
            geometry.validate()?;
            Ok(geometry)
        }
    }

    pub(crate) const fn parameters(self) -> ConcentratedCurveParameters {
        ConcentratedCurveParameters {
            peak_amplification_nad: self.peak_amplification_nad,
            core_half_width_bps: self.core_half_width_bps,
            fade_width_bps: self.fade_width_bps,
        }
    }

    pub(crate) fn center_point_with_geometry(
        self,
        center_price_nad: u64,
        geometry: ConcentratedCurveGeometry,
    ) -> Result<ConcentratedCurvePoint> {
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

fn complete_concentrated_cache(
    parameters: ConcentratedCurveParameters,
    tail_liquidity: u128,
    concentrated_liquidity: u128,
    core_lower_sqrt_price_nad: u128,
    core_upper_sqrt_price_nad: u128,
    outer_lower_sqrt_price_nad: u128,
    outer_upper_sqrt_price_nad: u128,
    _sqrt_center_nad: u128,
) -> Result<(ConcentratedCurveCache, ConcentratedCurveGeometry)> {
    let cache = ConcentratedCurveCache {
        math_revision: CONCENTRATED_CURVE_MATH_REVISION,
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
    let geometry = cache.geometry()?;
    Ok((cache, geometry))
}

impl ConcentratedCurveParameters {
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
        let (tail_share, concentrated_share) = self.liquidity_shares_nad()?;
        // Every transition rebuilds the curve from rounded reserves. The
        // round-trip excess that rebuild can leave grows as the tail thins.
        require!(
            tail_share >= u128::from(crate::state::MIN_AMM_TAIL_SHARE_NAD),
            ErrorCode::CurveTailShareBelowMinimum
        );
        require!(concentrated_share > 0, ErrorCode::InvalidMarketConfig);
        if self.fade_width_bps > 0 {
            require!(concentrated_share >= 2, ErrorCode::InvalidMarketConfig);
        }
        Ok(())
    }

    fn width_ratio_nad(width_bps: u32) -> Result<u128> {
        (NAD as u128)
            .checked_add(mul_div_u128(NAD as u128, width_bps as u128, 10_000)?)
            .ok_or_else(|| ErrorCode::InvariantOverflow.into())
    }

    pub(crate) fn sqrt_widths_nad(self) -> Result<(u128, u128)> {
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
    /// exact at the centered reserve point; it is never an operator input.
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
        // Amplification beyond what the widths can carry leaves no tail.
        require!(concentrated_share < NAD as u128, ErrorCode::CurveTailShareBelowMinimum);
        Ok((NAD as u128 - concentrated_share, concentrated_share))
    }
}

fn reserve_ratio_cmp(point: ConcentratedCurvePoint, boundary: ConcentratedCurvePoint) -> core::cmp::Ordering {
    wide_product(point.quote_reserve, boundary.base_reserve)
        .cmp(&wide_product(boundary.quote_reserve, point.base_reserve))
}

// Build the same tail/band geometry from independent oracle inputs. Unlike
// reconstruction at a reserve point, this never reads the live inventory.
// The validated geometry is returned rather than rebuilt by the caller.
pub(crate) fn prepare_concentrated_cache_and_geometry_at_depth(
    center_price_nad: u64,
    depth_nad: u128,
    parameters: ConcentratedCurveParameters,
) -> Result<(ConcentratedCurveCache, ConcentratedCurveGeometry)> {
    parameters.validate(u64::MAX)?;
    require!(center_price_nad > 0 && depth_nad > 0, ErrorCode::InvalidSettlementPrice);
    let sqrt_center = sqrt_ratio_nad(u128::from(center_price_nad))?;
    let (core_width, outer_width) = parameters.sqrt_widths_nad()?;
    let (_, concentrated_share) = parameters.liquidity_shares_nad()?;
    let concentrated = mul_div_u128(depth_nad, concentrated_share, u128::from(NAD))?;
    complete_concentrated_cache(
        parameters,
        depth_nad - concentrated,
        concentrated,
        mul_div_u128(sqrt_center, u128::from(NAD), core_width)?,
        mul_div_u128(sqrt_center, core_width, u128::from(NAD))?,
        mul_div_u128(sqrt_center, u128::from(NAD), outer_width)?,
        mul_div_u128(sqrt_center, outer_width, u128::from(NAD))?,
        sqrt_center,
    )
}

/// Reconstructs the unique positive curve scale through an arbitrary reserve
/// point for a sticky center and concentration policy. Homothetic boundary
/// ratios select the expected branch first, so the normal path evaluates one
/// quadratic positive root. Remaining branches are rounding-boundary
/// fallbacks, not an invariant search.
pub(crate) fn prepare_concentrated_cache_at_point(
    base_reserve: u128,
    quote_reserve: u128,
    center_price_nad: u64,
    parameters: ConcentratedCurveParameters,
) -> Result<ConcentratedCurveCache> {
    prepare_concentrated_cache_and_geometry_at_point(
        &mut None,
        base_reserve,
        quote_reserve,
        center_price_nad,
        parameters,
    )
    .map(|(cache, _)| cache)
}

// Return the already validated geometry to quote callers. Reconstructing it
// again would repeat all nested-boundary divisions and continuity checks.
// `solver` keeps the point-independent work: a later call with the same
// center and policy reuses it, with the results and errors of a call given
// `None`. The caller's stack holds the solver, since Solana's bump allocator
// would never reclaim a heap copy.
pub(crate) fn prepare_concentrated_cache_and_geometry_at_point(
    solver: &mut Option<ConcentratedCurveSolver>,
    base_reserve: u128,
    quote_reserve: u128,
    center_price_nad: u64,
    parameters: ConcentratedCurveParameters,
) -> Result<(ConcentratedCurveCache, ConcentratedCurveGeometry)> {
    // A kept solver was built for these inputs after this validation passed.
    // Validation is deterministic, so repeating it cannot fail.
    let reusable = solver
        .as_ref()
        .is_some_and(|kept| kept.center_price_nad == center_price_nad && kept.parameters == parameters);
    if !reusable {
        parameters.validate(u64::MAX)?;
    }
    require!(base_reserve > 0 && quote_reserve > 0, ErrorCode::InsufficientLiquidity);

    if parameters.is_cpmm() {
        let tail_liquidity = geometric_mean_floor(base_reserve, quote_reserve)?;
        require!(tail_liquidity > 0, ErrorCode::InsufficientLiquidity);
        let sqrt_center_nad = sqrt_ratio_nad(center_price_nad as u128)?;
        return complete_concentrated_cache(parameters, tail_liquidity, 0, 0, 0, 0, 0, sqrt_center_nad);
    }

    let solver = match solver {
        Some(kept) if reusable => &*kept,
        slot => ConcentratedCurveSolver::build_in(slot, center_price_nad, parameters)?,
    };
    let point = ConcentratedCurvePoint {
        base_reserve,
        quote_reserve,
    };
    let shape = &solver.shape;

    // Curve geometry is homothetic: changing total liquidity scales every
    // boundary coordinate by the same amount. Reserve-ratio comparisons can
    // therefore select the branch before the scale is known.
    let preferred_branch = if shape.is_nested() {
        let region = shape
            .nested_boundaries
            .iter()
            .take(shape.nested_boundary_count as usize)
            .position(|boundary| reserve_ratio_cmp(point, *boundary) != core::cmp::Ordering::Less)
            .unwrap_or(shape.nested_boundary_count as usize);
        nested_branch_from_region(region)
    } else if reserve_ratio_cmp(point, shape.upper_boundary) != core::cmp::Ordering::Less {
        ConcentratedCurveBranch::UpperTail
    } else if reserve_ratio_cmp(point, shape.lower_boundary) == core::cmp::Ordering::Greater {
        ConcentratedCurveBranch::Inner
    } else {
        ConcentratedCurveBranch::LowerTail
    };
    let branch_candidates = match preferred_branch {
        ConcentratedCurveBranch::UpperTail => [
            ConcentratedCurveBranch::UpperTail,
            ConcentratedCurveBranch::UpperShoulder,
            ConcentratedCurveBranch::Inner,
            ConcentratedCurveBranch::LowerShoulder,
            ConcentratedCurveBranch::LowerTail,
        ],
        ConcentratedCurveBranch::UpperShoulder => [
            ConcentratedCurveBranch::UpperShoulder,
            ConcentratedCurveBranch::UpperTail,
            ConcentratedCurveBranch::Inner,
            ConcentratedCurveBranch::LowerShoulder,
            ConcentratedCurveBranch::LowerTail,
        ],
        ConcentratedCurveBranch::Inner => [
            ConcentratedCurveBranch::Inner,
            ConcentratedCurveBranch::UpperShoulder,
            ConcentratedCurveBranch::LowerShoulder,
            ConcentratedCurveBranch::UpperTail,
            ConcentratedCurveBranch::LowerTail,
        ],
        ConcentratedCurveBranch::LowerShoulder => [
            ConcentratedCurveBranch::LowerShoulder,
            ConcentratedCurveBranch::Inner,
            ConcentratedCurveBranch::LowerTail,
            ConcentratedCurveBranch::UpperShoulder,
            ConcentratedCurveBranch::UpperTail,
        ],
        ConcentratedCurveBranch::LowerTail => [
            ConcentratedCurveBranch::LowerTail,
            ConcentratedCurveBranch::LowerShoulder,
            ConcentratedCurveBranch::Inner,
            ConcentratedCurveBranch::UpperShoulder,
            ConcentratedCurveBranch::UpperTail,
        ],
    };

    // Dust reserves can floor the tail or concentrated liquidity of every
    // candidate to zero. That is too little liquidity for the configured
    // shape, not an internal inconsistency.
    let mut solved_candidate = false;
    // The solve scales region constants from a fixed shape, while the
    // geometry floors every layer at the solved liquidity. Within that
    // rounding of a boundary, the two neighbouring solutions can each place
    // the point in the other's region: the point lies between their boundary
    // positions. Either solution is then exact up to rounding. The smaller
    // one is kept, the same direction in which the root solve rounds.
    // Single-band curves have no shoulder regions between neighbours.
    // Misplaced candidates are rare; the list allocates only for them and
    // keeps this frame within the SBF stack limit.
    let neighbour_distance = if parameters.fade_width_bps == 0 { 2 } else { 1 };
    let mut misplaced: Vec<(usize, u128, usize)> = Vec::new();
    for branch in branch_candidates {
        if parameters.fade_width_bps == 0
            && matches!(
                branch,
                ConcentratedCurveBranch::UpperShoulder | ConcentratedCurveBranch::LowerShoulder
            )
        {
            continue;
        }
        let total_liquidity = if parameters.fade_width_bps == 0 {
            concentrated_total_liquidity_root(
                point,
                solver.concentrated_share,
                solver.core_lower_sqrt_price_nad,
                solver.core_upper_sqrt_price_nad,
                branch,
            )?
        } else {
            solver.nested_total_liquidity_root(point, branch)?
        };
        if total_liquidity == 0 {
            continue;
        }
        let concentrated_liquidity = mul_div_u128(total_liquidity, solver.concentrated_share, NAD as u128)?;
        let tail_liquidity = total_liquidity
            .checked_sub(concentrated_liquidity)
            .ok_or(ErrorCode::BrokenInvariant)?;
        if tail_liquidity == 0 || concentrated_liquidity == 0 {
            continue;
        }
        solved_candidate = true;
        let (cache, geometry) = complete_concentrated_cache(
            parameters,
            tail_liquidity,
            concentrated_liquidity,
            solver.core_lower_sqrt_price_nad,
            solver.core_upper_sqrt_price_nad,
            solver.outer_lower_sqrt_price_nad,
            solver.outer_upper_sqrt_price_nad,
            solver.sqrt_center_nad,
        )?;
        let region = nested_region_from_branch(branch);
        let point_region = nested_region_from_branch(geometry.branch(point));
        if point_region == region {
            return Ok((cache, geometry));
        }
        let bracketing_total = misplaced
            .iter()
            .find(|&&(neighbour_region, _, neighbour_point_region)| {
                neighbour_region == point_region
                    && neighbour_point_region == region
                    && region.abs_diff(point_region) == neighbour_distance
            })
            .map(|&(_, neighbour_total, _)| neighbour_total);
        if let Some(neighbour_total) = bracketing_total {
            if total_liquidity <= neighbour_total {
                return Ok((cache, geometry));
            }
            let neighbour_concentrated = mul_div_u128(neighbour_total, solver.concentrated_share, NAD as u128)?;
            let neighbour_tail = neighbour_total
                .checked_sub(neighbour_concentrated)
                .ok_or(ErrorCode::BrokenInvariant)?;
            return complete_concentrated_cache(
                parameters,
                neighbour_tail,
                neighbour_concentrated,
                solver.core_lower_sqrt_price_nad,
                solver.core_upper_sqrt_price_nad,
                solver.outer_lower_sqrt_price_nad,
                solver.outer_upper_sqrt_price_nad,
                solver.sqrt_center_nad,
            );
        }
        misplaced.push((region, total_liquidity, point_region));
    }
    require!(solved_candidate, ErrorCode::InsufficientLiquidity);
    err!(ErrorCode::BrokenInvariant)
}

/// The point-independent part of a reserve-point reconstruction for one
/// sticky center and concentration policy: the layer prices and the shape at
/// `NESTED_SHAPE_SCALE`, whose boundaries select the branch and whose region
/// constants define each branch's quadratic.
pub(crate) struct ConcentratedCurveSolver {
    center_price_nad: u64,
    parameters: ConcentratedCurveParameters,
    sqrt_center_nad: u128,
    core_lower_sqrt_price_nad: u128,
    core_upper_sqrt_price_nad: u128,
    outer_lower_sqrt_price_nad: u128,
    outer_upper_sqrt_price_nad: u128,
    concentrated_share: u128,
    shape: ConcentratedCurveGeometry,
}

impl ConcentratedCurveSolver {
    // Builds the solver into the caller's slot from this SBF frame. A solver
    // returned by value into the reconstruction's frame overflows it.
    #[inline(never)]
    fn build_in(
        slot: &mut Option<Self>,
        center_price_nad: u64,
        parameters: ConcentratedCurveParameters,
    ) -> Result<&Self> {
        let sqrt_center_nad = sqrt_ratio_nad(center_price_nad as u128)?;
        let (core_sqrt_width, outer_sqrt_width) = parameters.sqrt_widths_nad()?;
        let core_lower_sqrt_price_nad = mul_div_u128(sqrt_center_nad, NAD as u128, core_sqrt_width)?;
        let core_upper_sqrt_price_nad = mul_div_u128(sqrt_center_nad, core_sqrt_width, NAD as u128)?;
        let outer_lower_sqrt_price_nad = mul_div_u128(sqrt_center_nad, NAD as u128, outer_sqrt_width)?;
        let outer_upper_sqrt_price_nad = mul_div_u128(sqrt_center_nad, outer_sqrt_width, NAD as u128)?;
        let (_, concentrated_share) = parameters.liquidity_shares_nad()?;
        let shape_concentrated_liquidity = mul_div_u128(NESTED_SHAPE_SCALE, concentrated_share, NAD as u128)?;
        let shape_tail_liquidity = NESTED_SHAPE_SCALE
            .checked_sub(shape_concentrated_liquidity)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let shape = ConcentratedCurveCache {
            math_revision: CONCENTRATED_CURVE_MATH_REVISION,
            peak_amplification_nad: parameters.peak_amplification_nad,
            core_half_width_bps: parameters.core_half_width_bps,
            fade_width_bps: parameters.fade_width_bps,
            tail_liquidity: shape_tail_liquidity,
            concentrated_liquidity: shape_concentrated_liquidity,
            core_lower_sqrt_price_nad,
            core_upper_sqrt_price_nad,
            outer_lower_sqrt_price_nad,
            outer_upper_sqrt_price_nad,
        }
        .geometry()?;
        Ok(slot.insert(Self {
            center_price_nad,
            parameters,
            sqrt_center_nad,
            core_lower_sqrt_price_nad,
            core_upper_sqrt_price_nad,
            outer_lower_sqrt_price_nad,
            outer_upper_sqrt_price_nad,
            concentrated_share,
            shape,
        }))
    }

    // The wide quadratic of one nested branch, in its own SBF frame. The
    // solve's frame also holds a complete cache and geometry.
    #[inline(never)]
    fn nested_total_liquidity_root(
        &self,
        point: ConcentratedCurvePoint,
        branch: ConcentratedCurveBranch,
    ) -> Result<u128> {
        let shape = &self.shape;
        let region = nested_region_from_branch(branch);
        require!(region <= 4, ErrorCode::InvalidMarketConfig);
        // The shape splits its liquidity exactly as the cache splits
        // concentrated liquidity: the core takes the ceiling half.
        // Halving the share in NAD units instead moves up to 5e-10 of
        // the liquidity between core and shoulder, and the solved scale
        // then misses the built curve. In a thin tail region that miss
        // is a large fraction of the effective reserves, and round trips
        // through the boundary returned more than their input. The
        // shape's region constants are those of its own floored layers.
        let base_constant = shape.nested_base_constants[region];
        let quote_constant = shape.nested_quote_constants[region];
        let tail_liquidity = NESTED_SHAPE_SCALE
            .checked_sub(shape.inner_liquidity)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let active_liquidity = match region {
            0 | 4 => tail_liquidity,
            1 | 3 => tail_liquidity
                .checked_add(shape.shoulder_liquidity)
                .ok_or(ErrorCode::InvariantOverflow)?,
            2 => NESTED_SHAPE_SCALE,
            _ => unreachable!(),
        };
        let active_squared = wide_product(active_liquidity, active_liquidity);
        let constant_product = wide_product(base_constant.magnitude, quote_constant.magnitude);
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

        let lhs = SignedWide::from_product(quote_constant.negative, point.base_reserve, quote_constant.magnitude);
        let rhs = SignedWide::from_product(base_constant.negative, point.quote_reserve, base_constant.magnitude);
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
        let scaled_linear =
            mul_u512(&linear.magnitude, &U512::from(NESTED_SHAPE_SCALE)).ok_or(ErrorCode::InvariantOverflow)?;
        // 10^36 fits a u128 constant.
        let scale_squared = U512::from(NESTED_SHAPE_SCALE * NESTED_SHAPE_SCALE);
        let constant = mul_u512(&wide_product(point.base_reserve, point.quote_reserve), &scale_squared)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let liquidity = quadratic_root_floor(quadratic, scaled_linear, linear.negative, constant)?;
        require!(liquidity <= U512::from(u128::MAX), ErrorCode::InvariantOverflow);
        Ok(liquidity.as_u128())
    }
}

fn concentrated_total_liquidity_root(
    point: ConcentratedCurvePoint,
    share_nad: u128,
    lower_sqrt_price_nad: u128,
    upper_sqrt_price_nad: u128,
    branch: ConcentratedCurveBranch,
) -> Result<u128> {
    let scale = NAD as u128;
    // NAD^2 = 10^18 fits a u128 constant.
    let scale_squared = U512::from(NAD as u128 * NAD as u128);
    let x = U512::from(point.base_reserve);
    let y = U512::from(point.quote_reserve);
    let rho = U512::from(share_nad);
    let tail_share = U512::from(scale.checked_sub(share_nad).ok_or(ErrorCode::InvalidMarketConfig)?);
    let lower = U512::from(lower_sqrt_price_nad);
    let upper = U512::from(upper_sqrt_price_nad);
    let width = upper.checked_sub(lower).ok_or(ErrorCode::InvalidMarketConfig)?;

    let (a, b, c, negative_linear) = match branch {
        ConcentratedCurveBranch::Inner => {
            let a = mul_u512(&upper, &scale_squared)
                .and_then(|value| value.checked_sub(mul_u512(&mul_u512(&rho, &rho)?, &lower)?))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let linear_shape = mul_u512(&x, &lower)
                .and_then(|value| mul_u512(&value, &upper))
                .and_then(|value| value.checked_add(mul_u512(&y, &scale_squared)?))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let b = mul_u512(&rho, &linear_shape).ok_or(ErrorCode::InvariantOverflow)?;
            let c = mul_u512(&x, &y)
                .and_then(|value| mul_u512(&value, &upper))
                .and_then(|value| mul_u512(&value, &scale_squared))
                .ok_or(ErrorCode::InvariantOverflow)?;
            (a, b, c, true)
        }
        ConcentratedCurveBranch::LowerTail => {
            let a = mul_u512(&tail_share, &tail_share)
                .and_then(|value| mul_u512(&value, &lower))
                .and_then(|value| mul_u512(&value, &upper))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let b = mul_u512(&rho, &width)
                .and_then(|value| mul_u512(&value, &y))
                .and_then(|value| mul_u512(&value, &scale_squared))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let c = mul_u512(&x, &y)
                .and_then(|value| mul_u512(&value, &scale_squared))
                .and_then(|value| mul_u512(&value, &lower))
                .and_then(|value| mul_u512(&value, &upper))
                .ok_or(ErrorCode::InvariantOverflow)?;
            (a, b, c, false)
        }
        ConcentratedCurveBranch::UpperTail => {
            let a = mul_u512(&tail_share, &tail_share).ok_or(ErrorCode::InvariantOverflow)?;
            let b = mul_u512(&x, &rho)
                .and_then(|value| mul_u512(&value, &width))
                .ok_or(ErrorCode::InvariantOverflow)?;
            let c = mul_u512(&x, &y)
                .and_then(|value| mul_u512(&value, &scale_squared))
                .ok_or(ErrorCode::InvariantOverflow)?;
            (a, b, c, false)
        }
        ConcentratedCurveBranch::LowerShoulder | ConcentratedCurveBranch::UpperShoulder => {
            return err!(ErrorCode::InvalidMarketConfig)
        }
    };
    require!(!a.is_zero(), ErrorCode::InvalidMarketConfig);
    let liquidity = quadratic_root_floor(a, b, negative_linear, c)?;
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
    fn from_product(negative: bool, lhs: u128, rhs: u128) -> Self {
        let magnitude = wide_product(lhs, rhs);
        Self {
            negative: negative && !magnitude.is_zero(),
            magnitude,
        }
    }
}

fn significant_limbs(value: &U512) -> usize {
    value.0.iter().rposition(|&limb| limb != 0).map_or(0, |index| index + 1)
}

/// Exact product of two u128 values. It is below 2^256, so it always fits.
fn wide_product(lhs: u128, rhs: u128) -> U512 {
    if lhs <= u128::from(u64::MAX) && rhs <= u128::from(u64::MAX) {
        let product = widening_mul_u64(lhs as u64, rhs as u64);
        return U512([product as u64, (product >> 64) as u64, 0, 0, 0, 0, 0, 0]);
    }
    let lhs = [lhs as u64, (lhs >> 64) as u64];
    let rhs = [rhs as u64, (rhs >> 64) as u64];
    let mut limbs = [0_u64; 8];
    for (i, &left) in lhs.iter().enumerate() {
        let mut carry = 0_u64;
        for (j, &right) in rhs.iter().enumerate() {
            let wide = widening_mul_u64(left, right);
            let (low, high) = (wide as u64, (wide >> 64) as u64);
            let (sum, first) = low.overflowing_add(limbs[i + j]);
            let (sum, second) = sum.overflowing_add(carry);
            limbs[i + j] = sum;
            carry = high + u64::from(first) + u64::from(second);
        }
        limbs[i + 2] = carry;
    }
    U512(limbs)
}

/// Exact product, or `None` exactly when `U512::checked_mul` reports an
/// overflow. The generic multiply visits all 64 limb pairs and multiplies
/// every nonzero limb by each zero limb of the other factor; on SBF that was
/// most of the cost of a curve solve. This multiplies significant limbs only.
fn mul_u512(lhs: &U512, rhs: &U512) -> Option<U512> {
    let lhs_limbs = significant_limbs(lhs);
    let rhs_limbs = significant_limbs(rhs);
    if lhs_limbs == 0 || rhs_limbs == 0 {
        return Some(U512::zero());
    }
    // Both leading limbs are nonzero, so the product is at least
    // 2^(64 * (lhs_limbs + rhs_limbs - 2)).
    if lhs_limbs + rhs_limbs > 9 {
        return None;
    }
    let mut product = [0_u64; 9];
    for (i, &left) in lhs.0.iter().take(lhs_limbs).enumerate() {
        let mut carry = 0_u64;
        for (j, &right) in rhs.0.iter().take(rhs_limbs).enumerate() {
            let wide = widening_mul_u64(left, right);
            let (low, high) = (wide as u64, (wide >> 64) as u64);
            let (sum, first) = low.overflowing_add(product[i + j]);
            let (sum, second) = sum.overflowing_add(carry);
            product[i + j] = sum;
            // low + high * 2^64 + two words is at most 2^128 - 1, so the
            // high word with both carries still fits.
            carry = high + u64::from(first) + u64::from(second);
        }
        product[i + rhs_limbs] = carry;
    }
    if product[8] != 0 {
        return None;
    }
    let mut limbs = [0_u64; 8];
    limbs.copy_from_slice(&product[..8]);
    Some(U512(limbs))
}

/// The low 128 bits of `value >> shift`, reading only the limbs they need.
fn shifted_low_u128(value: &U512, shift: usize) -> u128 {
    let word = |index: usize| u128::from(value.0.get(index).copied().unwrap_or(0));
    let limb = shift / 64;
    let bit = shift % 64;
    let low = word(limb) | (word(limb + 1) << 64);
    if bit == 0 {
        low
    } else {
        (low >> bit) | (word(limb + 2) << (128 - bit))
    }
}

/// Both sides of `q t^2 + b t <= c` at `t`, where `b` is `-linear` when
/// `negative_linear` and `+linear` otherwise: `(q t^2 + linear t, c)` or
/// `(q t^2, c + linear t)`. The polynomial is positive at `t` exactly when
/// the first exceeds the second. `None` means a term needs more than 512
/// bits.
fn quadratic_sides(
    quadratic: &U512,
    linear: &U512,
    negative_linear: bool,
    constant: &U512,
    t: &U512,
) -> Option<(U512, U512)> {
    let quadratic_term = mul_u512(quadratic, &mul_u512(t, t)?)?;
    let linear_term = mul_u512(linear, t)?;
    if negative_linear {
        Some((quadratic_term, constant.checked_add(linear_term)?))
    } else {
        Some((quadratic_term.checked_add(linear_term)?, *constant))
    }
}

/// About 62 leading bits of `numerator / denominator`. Only the candidate
/// that exact arithmetic then confirms depends on it. `None` when the
/// quotient would not fit.
fn approximate_quotient(numerator: &U512, denominator: &U512) -> Option<U512> {
    if denominator.is_zero() {
        return None;
    }
    let numerator_bits = numerator.bits();
    let denominator_bits = denominator.bits();
    if numerator_bits < denominator_bits {
        return Some(U512::zero());
    }
    // A 64-bit divisor and a dividend below 2^127 keep the division on the
    // native two-digit path.
    let denominator_shift = denominator_bits.saturating_sub(64);
    let numerator_shift = numerator_bits.saturating_sub(127);
    let (quotient, _) = div_rem_native(
        shifted_low_u128(numerator, numerator_shift),
        shifted_low_u128(denominator, denominator_shift),
    );
    let quotient = U512::from(quotient);
    if numerator_shift >= denominator_shift {
        let shift = numerator_shift - denominator_shift;
        (quotient.bits() + shift <= 512).then(|| quotient << shift)
    } else {
        Some(quotient >> (denominator_shift - numerator_shift))
    }
}

/// Floor of the positive root of `q l^2 + b l - c`, where `b` is `-linear`
/// when `negative_linear` and `+linear` otherwise, `q > 0` and `c > 0`.
/// The fallback evaluates floor((floor(sqrt(D)) - b) / (2q)) with
/// D = linear^2 + 4qc. Since `b` and `2q` are integers, flooring sqrt(D)
/// first does not change that floor: it is the largest `l >= 0` with
/// `q l^2 + b l <= c`. A closed-form estimate is therefore accepted exactly
/// when `l` meets that bound and `l + 1` does not. Overflow and error results
/// are those of the fallback, which runs whenever no estimate is confirmed.
#[inline(never)]
fn quadratic_root_floor(quadratic: U512, linear: U512, negative_linear: bool, constant: U512) -> Result<U512> {
    let discriminant = mul_u512(&quadratic, &constant)
        .and_then(|value| value.checked_add(value))
        .and_then(|value| value.checked_add(value))
        .and_then(|value| value.checked_add(mul_u512(&linear, &linear)?))
        .ok_or(ErrorCode::InvariantOverflow)?;
    if let Some(root) = confirmed_quadratic_root(&quadratic, &linear, negative_linear, &constant, &discriminant) {
        return Ok(root);
    }
    let root = sqrt_floor_u512(discriminant)?;
    let numerator = if negative_linear {
        root.checked_add(linear).ok_or(ErrorCode::InvariantOverflow)?
    } else {
        root.checked_sub(linear).ok_or(ErrorCode::BrokenInvariant)?
    };
    let denominator = quadratic.checked_add(quadratic).ok_or(ErrorCode::InvariantOverflow)?;
    div_floor_u512(numerator, denominator)
}

/// The root `quadratic_root_floor` returns, estimated in closed form and
/// confirmed with exact arithmetic. `None` leaves it to the exact fallback.
fn confirmed_quadratic_root(
    quadratic: &U512,
    linear: &U512,
    negative_linear: bool,
    constant: &U512,
    discriminant: &U512,
) -> Option<U512> {
    // sqrt(D) to about 62 bits, as the fallback's Newton seed.
    let shift = discriminant.bits().saturating_sub(128).div_ceil(2) * 2;
    let root = U512::from(isqrt(shifted_low_u128(discriminant, shift))) << (shift / 2);
    // Cancellation-free forms of the same root.
    let sum = linear.checked_add(root)?;
    let mut candidate = if negative_linear {
        approximate_quotient(&sum, &quadratic.checked_add(*quadratic)?)?
    } else {
        approximate_quotient(&constant.checked_add(*constant)?, &sum)?
    };
    // Before flooring, the estimate is within 2^-60 of the root, or within
    // one when D has at most 128 bits and its square root is exact to the
    // integer below. Below 2^58 the floored candidate is then within two of
    // the root. Above, one Newton step on the exact polynomial leaves it
    // within two while the root is below 2^118. Without a positive slope the
    // candidate is near zero and is kept for the search below.
    if candidate.bits() > 58 {
        let (positive, negative) = quadratic_sides(quadratic, linear, negative_linear, constant, &candidate)?;
        let above = positive > negative;
        let excess = if above {
            positive - negative
        } else {
            negative - positive
        };
        let slope = mul_u512(&quadratic.checked_add(*quadratic)?, &candidate)?;
        let slope = if negative_linear {
            slope.checked_sub(*linear)
        } else {
            slope.checked_add(*linear)
        };
        if let Some(step) = slope.and_then(|slope| approximate_quotient(&excess, &slope)) {
            candidate = if above {
                candidate.saturating_sub(step)
            } else {
                candidate.checked_add(step)?
            };
        }
    }
    for _ in 0..4 {
        let (positive, negative) = quadratic_sides(quadratic, linear, negative_linear, constant, &candidate)?;
        if positive > negative {
            candidate = candidate.checked_sub(U512::one())?;
            continue;
        }
        // The polynomial at t + 1 adds q (2t + 1) + b.
        let rise = mul_u512(quadratic, &candidate.checked_add(candidate)?.checked_add(U512::one())?)?;
        let (next_positive, next_negative) = if negative_linear {
            (positive.checked_add(rise)?, negative.checked_add(*linear)?)
        } else {
            (positive.checked_add(rise)?.checked_add(*linear)?, negative)
        };
        if next_positive <= next_negative {
            candidate = candidate.checked_add(U512::one())?;
            continue;
        }
        return Some(candidate);
    }
    None
}

// Knuth division in base 2^32. Its quotient estimates use native u64
// division on SBF; the generic U512 implementation estimates in base 2^64
// and invokes software u128 division. Both return exactly floor(n / d).
#[inline(never)]
fn div_floor_u512(numerator: U512, denominator: U512) -> Result<U512> {
    require!(!denominator.is_zero(), ErrorCode::InvariantOverflow);
    if numerator < denominator {
        return Ok(U512::zero());
    }
    let n = denominator.bits().div_ceil(32);
    let m = numerator.bits().div_ceil(32);
    let mut u = [0_u32; 17];
    let mut v = [0_u32; 16];
    let mut q = [0_u32; 16];
    for (i, digit) in u.iter_mut().take(m).enumerate() {
        *digit = (numerator.0[i / 2] >> (32 * (i % 2))) as u32;
    }
    for (i, digit) in v.iter_mut().take(n).enumerate() {
        *digit = (denominator.0[i / 2] >> (32 * (i % 2))) as u32;
    }
    if n == 1 {
        let mut remainder = 0_u64;
        for i in (0..m).rev() {
            let dividend = (remainder << 32) | u[i] as u64;
            q[i] = (dividend / v[0] as u64) as u32;
            remainder = dividend % v[0] as u64;
        }
    } else {
        let shift = v[n - 1].leading_zeros();
        if shift > 0 {
            let mut carry = 0;
            for digit in u.iter_mut().take(m + 1) {
                let next = *digit >> (32 - shift);
                *digit = (*digit << shift) | carry;
                carry = next;
            }
            carry = 0;
            for digit in v.iter_mut().take(n) {
                let next = *digit >> (32 - shift);
                *digit = (*digit << shift) | carry;
                carry = next;
            }
        }
        const BASE: u64 = 1_u64 << 32;
        let divisor = v[n - 1] as u64;
        for j in (0..=m - n).rev() {
            let dividend = ((u[j + n] as u64) << 32) | u[j + n - 1] as u64;
            let mut estimate = (dividend / divisor).min(BASE - 1);
            let mut remainder = dividend - estimate * divisor;
            while remainder < BASE && estimate * v[n - 2] as u64 > (remainder << 32) | u[j + n - 2] as u64 {
                estimate -= 1;
                remainder += divisor;
            }
            let mut borrow = 0_u64;
            for i in 0..n {
                let product = estimate * v[i] as u64 + borrow;
                let low = product as u32;
                borrow = (product >> 32) + u64::from(u[j + i] < low);
                u[j + i] = u[j + i].wrapping_sub(low);
            }
            let negative = (u[j + n] as u64) < borrow;
            u[j + n] = u[j + n].wrapping_sub(borrow as u32);
            if negative {
                estimate -= 1;
                let mut carry = 0_u64;
                for i in 0..n {
                    let sum = u[j + i] as u64 + v[i] as u64 + carry;
                    u[j + i] = sum as u32;
                    carry = sum >> 32;
                }
                u[j + n] = u[j + n].wrapping_add(carry as u32);
            }
            q[j] = estimate as u32;
        }
    }
    let mut out = U512::zero();
    for i in 0..8 {
        out.0[i] = q[2 * i] as u64 | ((q[2 * i + 1] as u64) << 32);
    }
    Ok(out)
}

fn sqrt_floor_u512(value: U512) -> Result<U512> {
    if value.is_zero() {
        return Ok(U512::zero());
    }
    if value.bits() <= 128 {
        return Ok(U512::from(isqrt(value.as_u128())));
    }
    // An even shift preserves square scaling. The top 128 bits provide a
    // strict upper seed: value < (top + 1) * 2^shift <= seed^2 * 2^shift.
    // Newton still finishes with exact wide division; the seed only avoids
    // repeatedly dividing a 512-bit value to discover its leading digits.
    let shift = (value.bits() - 128).div_ceil(2) * 2;
    let top = (value >> shift).as_u128();
    let mut root = U512::from(isqrt(top) + 1) << (shift / 2);
    for _ in 0..CONCENTRATED_SQRT_MAX_ITERS {
        let next = root
            .checked_add(div_floor_u512(value, root)?)
            .ok_or(ErrorCode::InvariantOverflow)?
            >> 1;
        if next >= root {
            return Ok(root);
        }
        root = next;
    }
    err!(ErrorCode::InvariantOverflow)
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ConcentratedCurvePoint {
    pub base_reserve: u128,
    pub quote_reserve: u128,
}

impl ConcentratedCurvePoint {
    pub(crate) fn shifted(self, base: i128, quote: i128) -> Result<Self> {
        let base_reserve = self
            .base_reserve
            .checked_add_signed(base)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let quote_reserve = self
            .quote_reserve
            .checked_add_signed(quote)
            .ok_or(ErrorCode::BrokenInvariant)?;
        require!(base_reserve > 0 && quote_reserve > 0, ErrorCode::InsufficientLiquidity);
        Ok(Self {
            base_reserve,
            quote_reserve,
        })
    }
}

/// Precomputed geometry for one full-range CPMM tail plus one concentrated
/// band. Offsets are reserve coordinates, not transferable balances.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ConcentratedSignedReserve {
    pub negative: bool,
    pub magnitude: u128,
}

impl ConcentratedSignedReserve {
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
pub struct ConcentratedCurveGeometry {
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
    pub lower_boundary: ConcentratedCurvePoint,
    pub upper_boundary: ConcentratedCurvePoint,
    /// A nonzero shoulder selects the five-segment nested curve. The original
    /// one-band fields above remain the compact representation when this is
    /// zero.
    pub shoulder_liquidity: u128,
    pub nested_boundary_count: u8,
    pub nested_boundaries: [ConcentratedCurvePoint; 4],
    pub nested_base_constants: [ConcentratedSignedReserve; 5],
    pub nested_quote_constants: [ConcentratedSignedReserve; 5],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConcentratedCurveQuote {
    pub amount_in: u128,
    pub amount_out: u128,
    pub end: ConcentratedCurvePoint,
    pub end_branch: ConcentratedCurveBranch,
    pub boundary_crossings: u8,
}

// Fixed-start quotes share every complete boundary crossing. Cache those
// exact canonical crossings once for the liquidation proof's many intervals.
// Within a segment only one constant-product division remains.
pub(crate) struct ConcentratedQuotePath {
    segments: Vec<ConcentratedQuoteSegment>,
    pub max_price_nad: u128,
}

struct ConcentratedQuoteSegment {
    input_before: u128,
    input_limit: u128,
    output_before: u128,
    effective_input: u128,
    effective_output: u128,
    real_input: u128,
    real_output: u128,
}

impl ConcentratedQuotePath {
    pub(crate) fn amount_out(&self, input: u128) -> Result<u128> {
        require!(input > 0, ErrorCode::AmountZero);
        for segment in &self.segments {
            if input <= segment.input_limit {
                let remaining = input
                    .checked_sub(segment.input_before)
                    .ok_or(ErrorCode::BrokenInvariant)?;
                segment
                    .real_input
                    .checked_add(remaining)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let amount = cpmm_amount_out_nad(segment.effective_input, segment.effective_output, remaining)?;
                require!(amount <= segment.real_output, ErrorCode::InsufficientLiquidity);
                require!(
                    amount > 0 || segment.output_before > 0 || input == segment.input_limit,
                    ErrorCode::InsufficientOutputAmount
                );
                return segment
                    .output_before
                    .checked_add(amount)
                    .ok_or_else(|| ErrorCode::OutputAmountOverflow.into());
            }
        }
        err!(ErrorCode::BrokenInvariant)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ConcentratedCurveLayer {
    liquidity: u128,
    lower_sqrt_price: u128,
    upper_sqrt_price: u128,
}

#[derive(Clone, Copy)]
struct PreparedCurveLayer {
    layer: ConcentratedCurveLayer,
    lower: ConcentratedCurvePoint,
    upper: ConcentratedCurvePoint,
}

impl ConcentratedCurveLayer {
    fn prepare(self) -> Result<PreparedCurveLayer> {
        let scale = NAD as u128;
        Ok(PreparedCurveLayer {
            layer: self,
            lower: ConcentratedCurvePoint {
                base_reserve: mul_div_u128(self.liquidity, scale, self.lower_sqrt_price)?,
                quote_reserve: mul_div_u128(self.liquidity, self.lower_sqrt_price, scale)?,
            },
            upper: ConcentratedCurvePoint {
                base_reserve: mul_div_u128(self.liquidity, scale, self.upper_sqrt_price)?,
                quote_reserve: mul_div_u128(self.liquidity, self.upper_sqrt_price, scale)?,
            },
        })
    }
}

fn nested_point_at_sqrt_price(
    tail_liquidity: u128,
    layers: [PreparedCurveLayer; 2],
    sqrt_price: u128,
    sqrt_price_scale: u128,
) -> Result<ConcentratedCurvePoint> {
    require!(sqrt_price > 0, ErrorCode::InvalidSettlementPrice);
    let mut base_reserve = mul_div_u128(tail_liquidity, sqrt_price_scale, sqrt_price)?;
    let mut quote_reserve = mul_div_u128(tail_liquidity, sqrt_price, sqrt_price_scale)?;
    for layer in layers {
        let base_at_lower = layer.lower.base_reserve;
        let base_at_upper = layer.upper.base_reserve;
        let quote_at_lower = layer.lower.quote_reserve;
        let quote_at_upper = layer.upper.quote_reserve;
        let layer = layer.layer;
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
    Ok(ConcentratedCurvePoint {
        base_reserve,
        quote_reserve,
    })
}

impl ConcentratedCurveGeometry {
    pub(crate) const fn cpmm() -> Self {
        Self {
            inner_liquidity: 0,
            inner_base_amplification_offset: 0,
            inner_quote_amplification_offset: 0,
            lower_tail_base_inventory: 0,
            upper_tail_quote_inventory: 0,
            lower_boundary: ConcentratedCurvePoint {
                base_reserve: 0,
                quote_reserve: 0,
            },
            upper_boundary: ConcentratedCurvePoint {
                base_reserve: 0,
                quote_reserve: 0,
            },
            shoulder_liquidity: 0,
            nested_boundary_count: 0,
            nested_boundaries: [ConcentratedCurvePoint {
                base_reserve: 0,
                quote_reserve: 0,
            }; 4],
            nested_base_constants: [ConcentratedSignedReserve {
                negative: false,
                magnitude: 0,
            }; 5],
            nested_quote_constants: [ConcentratedSignedReserve {
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
                    && self.lower_boundary == ConcentratedCurvePoint::default()
                    && self.upper_boundary == ConcentratedCurvePoint::default(),
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
                // Inactive claims cancel against the region constants. Each
                // effective reserve is the sum of floor(L / sqrt(p)) or
                // floor(L * sqrt(p)) for the active tail/core/shoulder layers.
                // With k active layers its exact value lies in [v, v + k),
                // so the common boundary price lies strictly between
                // y / (x + k) and (y + k) / x. Compare these intervals exactly;
                // a fixed NAD-price tolerance rejects valid small reserves.
                let active_layers = [1_u128, 2, 3, 2, 1];
                let left_base_upper = left
                    .0
                    .checked_add(active_layers[region])
                    .ok_or(ErrorCode::InvariantOverflow)?;
                let left_quote_upper = left
                    .1
                    .checked_add(active_layers[region])
                    .ok_or(ErrorCode::InvariantOverflow)?;
                let right_base_upper = right
                    .0
                    .checked_add(active_layers[region + 1])
                    .ok_or(ErrorCode::InvariantOverflow)?;
                let right_quote_upper = right
                    .1
                    .checked_add(active_layers[region + 1])
                    .ok_or(ErrorCode::InvariantOverflow)?;
                require!(
                    !ratio_lte_full_width(left_quote_upper, left.0, right.1, right_base_upper)?
                        && !ratio_lte_full_width(right_quote_upper, right.0, left.1, left_base_upper)?,
                    ErrorCode::InvalidMarketConfig
                );
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

    pub(crate) fn branch(self, point: ConcentratedCurvePoint) -> ConcentratedCurveBranch {
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
            ConcentratedCurveBranch::LowerTail
        } else if point.base_reserve <= self.upper_boundary.base_reserve {
            ConcentratedCurveBranch::UpperTail
        } else {
            ConcentratedCurveBranch::Inner
        }
    }

    fn nested_effective_reserves(self, point: ConcentratedCurvePoint, region: usize) -> Result<(u128, u128)> {
        require!(
            region <= self.nested_boundary_count as usize,
            ErrorCode::BrokenInvariant
        );
        let base = self.nested_base_constants[region].subtract_from(point.base_reserve)?;
        let quote = self.nested_quote_constants[region].subtract_from(point.quote_reserve)?;
        require!(base > 0 && quote > 0, ErrorCode::InsufficientLiquidity);
        Ok((base, quote))
    }

    /// Hot-path variant for geometry loaded from `ConcentratedCurveCache`, whose
    /// constructor already performed the complete geometry validation.
    pub(crate) fn spot_price_nad_prevalidated(self, point: ConcentratedCurvePoint) -> Result<u128> {
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
        let (lower_base, lower_quote) = self.effective_reserves(self.lower_boundary, ConcentratedCurveBranch::Inner)?;
        let (upper_base, upper_quote) = self.effective_reserves(self.upper_boundary, ConcentratedCurveBranch::Inner)?;
        Ok(Some((
            mul_div_u128(lower_quote, NAD as u128, lower_base)?,
            mul_div_u128(upper_quote, NAD as u128, upper_base)?,
        )))
    }

    /// Closed-form reserve point at a requested marginal price. Lending and
    /// liquidation use this inverse to materialize a pessimistic executable
    /// shape without an invariant root solve.
    pub(crate) fn point_at_price_nad(self, price_nad: u128, tail_liquidity: u128) -> Result<ConcentratedCurvePoint> {
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
            return Ok(ConcentratedCurvePoint {
                base_reserve: self.nested_base_constants[region].add_to(effective_base)?,
                quote_reserve: self.nested_quote_constants[region].add_to(effective_quote)?,
            });
        }
        let branch = if self.is_cpmm() {
            ConcentratedCurveBranch::LowerTail
        } else {
            let (lower_price, upper_price) = self.range_prices_nad()?.ok_or(ErrorCode::BrokenInvariant)?;
            if price_nad <= lower_price {
                ConcentratedCurveBranch::LowerTail
            } else if price_nad >= upper_price {
                ConcentratedCurveBranch::UpperTail
            } else {
                ConcentratedCurveBranch::Inner
            }
        };
        let liquidity = match branch {
            ConcentratedCurveBranch::Inner => tail_liquidity
                .checked_add(self.inner_liquidity)
                .ok_or(ErrorCode::InvariantOverflow)?,
            ConcentratedCurveBranch::LowerTail | ConcentratedCurveBranch::UpperTail => tail_liquidity,
            ConcentratedCurveBranch::LowerShoulder | ConcentratedCurveBranch::UpperShoulder => {
                return err!(ErrorCode::BrokenInvariant)
            }
        };
        let (effective_base, effective_quote) = (
            mul_div_u128(liquidity, NAD as u128, sqrt_price_nad)?,
            mul_div_u128(liquidity, sqrt_price_nad, NAD as u128)?,
        );
        match branch {
            ConcentratedCurveBranch::LowerTail => Ok(ConcentratedCurvePoint {
                base_reserve: effective_base
                    .checked_add(self.lower_tail_base_inventory)
                    .ok_or(ErrorCode::InvariantOverflow)?,
                quote_reserve: effective_quote,
            }),
            ConcentratedCurveBranch::Inner => Ok(ConcentratedCurvePoint {
                base_reserve: effective_base
                    .checked_sub(self.inner_base_amplification_offset)
                    .ok_or(ErrorCode::InsufficientLiquidity)?,
                quote_reserve: effective_quote
                    .checked_sub(self.inner_quote_amplification_offset)
                    .ok_or(ErrorCode::InsufficientLiquidity)?,
            }),
            ConcentratedCurveBranch::UpperTail => Ok(ConcentratedCurvePoint {
                base_reserve: effective_base,
                quote_reserve: effective_quote
                    .checked_add(self.upper_tail_quote_inventory)
                    .ok_or(ErrorCode::InvariantOverflow)?,
            }),
            ConcentratedCurveBranch::LowerShoulder | ConcentratedCurveBranch::UpperShoulder => {
                err!(ErrorCode::BrokenInvariant)
            }
        }
    }

    fn branch_for_direction(
        self,
        point: ConcentratedCurvePoint,
        direction: ConcentratedCurveDirection,
    ) -> ConcentratedCurveBranch {
        if self.is_nested() {
            let region = match direction {
                ConcentratedCurveDirection::BaseToQuote => self
                    .nested_boundaries
                    .iter()
                    .take(self.nested_boundary_count as usize)
                    .position(|boundary| point.base_reserve < boundary.base_reserve)
                    .unwrap_or(self.nested_boundary_count as usize),
                ConcentratedCurveDirection::QuoteToBase => self
                    .nested_boundaries
                    .iter()
                    .take(self.nested_boundary_count as usize)
                    .position(|boundary| point.quote_reserve >= boundary.quote_reserve)
                    .unwrap_or(self.nested_boundary_count as usize),
            };
            return nested_branch_from_region(region);
        }
        if self.is_cpmm() {
            return ConcentratedCurveBranch::LowerTail;
        }
        match direction {
            // Input reserve is the canonical branch coordinate. The opposite
            // reserve may differ from its continuous boundary reference by an
            // atom after conservative output rounding.
            ConcentratedCurveDirection::BaseToQuote => {
                if point.base_reserve >= self.lower_boundary.base_reserve {
                    ConcentratedCurveBranch::LowerTail
                } else if point.base_reserve < self.upper_boundary.base_reserve {
                    ConcentratedCurveBranch::UpperTail
                } else {
                    ConcentratedCurveBranch::Inner
                }
            }
            ConcentratedCurveDirection::QuoteToBase => {
                if point.quote_reserve < self.lower_boundary.quote_reserve {
                    ConcentratedCurveBranch::LowerTail
                } else if point.quote_reserve >= self.upper_boundary.quote_reserve {
                    ConcentratedCurveBranch::UpperTail
                } else {
                    ConcentratedCurveBranch::Inner
                }
            }
        }
    }

    fn effective_reserves(
        self,
        point: ConcentratedCurvePoint,
        branch: ConcentratedCurveBranch,
    ) -> Result<(u128, u128)> {
        if self.is_nested() {
            return self.nested_effective_reserves(point, nested_region_from_branch(branch));
        }
        let pair = match branch {
            ConcentratedCurveBranch::LowerTail => (
                point
                    .base_reserve
                    .checked_sub(self.lower_tail_base_inventory)
                    .ok_or(ErrorCode::BrokenInvariant)?,
                point.quote_reserve,
            ),
            ConcentratedCurveBranch::Inner => (
                point
                    .base_reserve
                    .checked_add(self.inner_base_amplification_offset)
                    .ok_or(ErrorCode::InvariantOverflow)?,
                point
                    .quote_reserve
                    .checked_add(self.inner_quote_amplification_offset)
                    .ok_or(ErrorCode::InvariantOverflow)?,
            ),
            ConcentratedCurveBranch::UpperTail => (
                point.base_reserve,
                point
                    .quote_reserve
                    .checked_sub(self.upper_tail_quote_inventory)
                    .ok_or(ErrorCode::BrokenInvariant)?,
            ),
            ConcentratedCurveBranch::LowerShoulder | ConcentratedCurveBranch::UpperShoulder => {
                return err!(ErrorCode::BrokenInvariant)
            }
        };
        require!(pair.0 > 0 && pair.1 > 0, ErrorCode::InsufficientLiquidity);
        Ok(pair)
    }

    fn next_boundary(
        self,
        point: ConcentratedCurvePoint,
        branch: ConcentratedCurveBranch,
        direction: ConcentratedCurveDirection,
    ) -> Option<ConcentratedCurvePoint> {
        if self.is_nested() {
            let region = nested_region_from_branch(branch);
            return match direction {
                ConcentratedCurveDirection::BaseToQuote => {
                    if region < self.nested_boundary_count as usize {
                        Some(self.nested_boundaries[region])
                    } else {
                        None
                    }
                }
                ConcentratedCurveDirection::QuoteToBase => {
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
            (ConcentratedCurveDirection::BaseToQuote, ConcentratedCurveBranch::UpperTail) => Some(self.upper_boundary),
            (ConcentratedCurveDirection::BaseToQuote, ConcentratedCurveBranch::Inner) => Some(self.lower_boundary),
            (ConcentratedCurveDirection::QuoteToBase, ConcentratedCurveBranch::LowerTail) => Some(self.lower_boundary),
            (ConcentratedCurveDirection::QuoteToBase, ConcentratedCurveBranch::Inner) => Some(self.upper_boundary),
            (_, ConcentratedCurveBranch::LowerShoulder | ConcentratedCurveBranch::UpperShoulder) => None,
            _ => {
                let _ = point;
                None
            }
        }
    }

    pub(crate) fn quote_exact_in(
        self,
        start: ConcentratedCurvePoint,
        amount_in: u128,
        direction: ConcentratedCurveDirection,
    ) -> Result<ConcentratedCurveQuote> {
        self.validate()?;
        self.quote_exact_in_prevalidated(start, amount_in, direction)
    }

    pub(crate) fn prepare_quote_path(
        self,
        start: ConcentratedCurvePoint,
        direction: ConcentratedCurveDirection,
    ) -> Result<ConcentratedQuotePath> {
        let mut point = start;
        let mut input = 0_u128;
        let mut output = 0_u128;
        let mut segments = Vec::with_capacity(5);
        let mut max_price_nad = 0_u128;
        for _ in 0..5 {
            let branch = self.branch_for_direction(point, direction);
            let (base, quote) = self.effective_reserves(point, branch)?;
            let (effective_input, effective_output, real_input, real_output) = match direction {
                ConcentratedCurveDirection::BaseToQuote => (base, quote, point.base_reserve, point.quote_reserve),
                ConcentratedCurveDirection::QuoteToBase => (quote, base, point.quote_reserve, point.base_reserve),
            };
            // Each segment's marginal price only falls with input. Include
            // every exact, dust-bearing crossing so even a rounding jump is
            // covered by this global upper slope.
            max_price_nad = max_price_nad.max(
                mul_div_u128(effective_output, NAD as u128, effective_input)?
                    .checked_add(1)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            );
            let distance = self
                .next_boundary(point, branch, direction)
                .map(|target| match direction {
                    ConcentratedCurveDirection::BaseToQuote => target.base_reserve.saturating_sub(point.base_reserve),
                    ConcentratedCurveDirection::QuoteToBase => target.quote_reserve.saturating_sub(point.quote_reserve),
                });
            let limit = match distance {
                Some(distance) if distance > 0 => input.checked_add(distance).ok_or(ErrorCode::MarketMathOverflow)?,
                _ => u128::MAX,
            };
            segments.push(ConcentratedQuoteSegment {
                input_before: input,
                input_limit: limit,
                output_before: output,
                effective_input,
                effective_output,
                real_input,
                real_output,
            });
            if limit == u128::MAX {
                return Ok(ConcentratedQuotePath {
                    segments,
                    max_price_nad,
                });
            }
            // Let the existing quote engine perform the exact crossing,
            // including floor rounding and any reserve dust carried forward.
            let crossing = self.quote_exact_in_prevalidated(point, limit - input, direction)?;
            point = crossing.end;
            output = output
                .checked_add(crossing.amount_out)
                .ok_or(ErrorCode::OutputAmountOverflow)?;
            input = limit;
        }
        err!(ErrorCode::BrokenInvariant)
    }

    /// Hot-path variant for geometry loaded from `ConcentratedCurveCache`.
    pub(crate) fn quote_exact_in_prevalidated(
        self,
        start: ConcentratedCurvePoint,
        amount_in: u128,
        direction: ConcentratedCurveDirection,
    ) -> Result<ConcentratedCurveQuote> {
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
                ConcentratedCurveDirection::BaseToQuote => target.base_reserve.saturating_sub(point.base_reserve),
                ConcentratedCurveDirection::QuoteToBase => target.quote_reserve.saturating_sub(point.quote_reserve),
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
                        ConcentratedCurveDirection::BaseToQuote => {
                            cpmm_amount_out_nad(effective_base, effective_quote, to_boundary)?
                        }
                        ConcentratedCurveDirection::QuoteToBase => {
                            cpmm_amount_out_nad(effective_quote, effective_base, to_boundary)?
                        }
                    };
                    match direction {
                        ConcentratedCurveDirection::BaseToQuote => {
                            point.base_reserve = target.base_reserve;
                            point.quote_reserve = point
                                .quote_reserve
                                .checked_sub(boundary_out)
                                .ok_or(ErrorCode::InsufficientLiquidity)?;
                        }
                        ConcentratedCurveDirection::QuoteToBase => {
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
                        return Ok(ConcentratedCurveQuote {
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
                ConcentratedCurveDirection::BaseToQuote => {
                    cpmm_amount_out_nad(effective_base, effective_quote, remaining)?
                }
                ConcentratedCurveDirection::QuoteToBase => {
                    cpmm_amount_out_nad(effective_quote, effective_base, remaining)?
                }
            };
            // A remainder of a few atoms past the last crossing can floor to
            // zero output. The pool keeps it, as it keeps the floor of every
            // segment; only a swap that pays nothing at all is rejected.
            require!(output > 0 || total_out > 0, ErrorCode::InsufficientOutputAmount);
            match direction {
                ConcentratedCurveDirection::BaseToQuote => {
                    point.base_reserve = point
                        .base_reserve
                        .checked_add(remaining)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    point.quote_reserve = point
                        .quote_reserve
                        .checked_sub(output)
                        .ok_or(ErrorCode::InsufficientLiquidity)?;
                }
                ConcentratedCurveDirection::QuoteToBase => {
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
            return Ok(ConcentratedCurveQuote {
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
        start: ConcentratedCurvePoint,
        amount_out: u128,
        direction: ConcentratedCurveDirection,
    ) -> Result<ConcentratedCurveQuote> {
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
                    ConcentratedCurveDirection::BaseToQuote => target.base_reserve.saturating_sub(point.base_reserve),
                    ConcentratedCurveDirection::QuoteToBase => target.quote_reserve.saturating_sub(point.quote_reserve),
                };
                if input == 0 {
                    None
                } else {
                    let (effective_base, effective_quote) = self.effective_reserves(point, branch)?;
                    let output = match direction {
                        ConcentratedCurveDirection::BaseToQuote => {
                            cpmm_amount_out_nad(effective_base, effective_quote, input)?
                        }
                        ConcentratedCurveDirection::QuoteToBase => {
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
                        ConcentratedCurveDirection::BaseToQuote => {
                            point.base_reserve = target.base_reserve;
                            point.quote_reserve = point
                                .quote_reserve
                                .checked_sub(boundary_out)
                                .ok_or(ErrorCode::InsufficientLiquidity)?;
                        }
                        ConcentratedCurveDirection::QuoteToBase => {
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
                        return Ok(ConcentratedCurveQuote {
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
                ConcentratedCurveDirection::BaseToQuote => {
                    cpmm_amount_in_nad(effective_base, effective_quote, remaining)?
                }
                ConcentratedCurveDirection::QuoteToBase => {
                    cpmm_amount_in_nad(effective_quote, effective_base, remaining)?
                }
            };
            require!(input > 0, ErrorCode::InsufficientOutputAmount);
            match direction {
                ConcentratedCurveDirection::BaseToQuote => {
                    point.base_reserve = point
                        .base_reserve
                        .checked_add(input)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    point.quote_reserve = point
                        .quote_reserve
                        .checked_sub(remaining)
                        .ok_or(ErrorCode::InsufficientLiquidity)?;
                }
                ConcentratedCurveDirection::QuoteToBase => {
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
            return Ok(ConcentratedCurveQuote {
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
include!("../../tests/support/amm_curve.rs");

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/amm_curve.rs");
}

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

/// Calculate dy for adding dx to a constant-product coordinate.
/// ```text
/// Δy = (Δx * y) / (x + Δx)
/// ```
pub(crate) fn cpmm_amount_out_nad(x: u128, y: u128, dx: u128) -> Result<u128> {
    let denominator = x.checked_add(dx).ok_or(ErrorCode::DenominatorOverflow)?;
    require!(denominator > 0, ErrorCode::OutputAmountOverflow);
    mul_div_u128(dx, y, denominator).map_err(|_| ErrorCode::OutputAmountOverflow.into())
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
mod cpmm_tests {
    include!("../../tests/transitions/amm_cpmm.rs");
}

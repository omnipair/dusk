//! Dusk's two-asset finite-C1 concentrated invariant.
//!
//! The executable scalar uses dimensionless fixed-point coordinates:
//! ```text
//! q = 4*x*y/D^2
//! delta = 1-q
//! w = (s/(s+delta))^2
//! G = 2*P*q*w*((x+y-D)/D) + q - 1
//! ```
//! `P` is the extra center-depth multiplier and `s` is the fade scale. The
//! protocol follows that inner equation until `delta=s/4`, then joins it to
//! the exact constant-product level `x*y = D^2*(1-s)/4` through a fixed cubic
//! transition. The transition width is protocol-defined rather than an
//! operator parameter. Both joins preserve the reserve level and its first
//! derivative, so executable quotes and marginal prices are continuous. The
//! second derivative may change at the joins.
//!
//! Positive-concentration reserves use an adaptive common numeraire: quote
//! units when the center is at least one, base units below one. This makes
//! every raw normalized input atom advance by at least one common atom.
//! Production arithmetic uses raw `u128` operations only. Positive-
//! concentration common reserves are capped at `u64::MAX`; exact CPMM mode
//! supports a wider normalized-reserve domain. Authoritative geometry
//! is derived in Q80; transition targets and ordinary residuals use its cached
//! Q64 projection. A Q80 sign calculation runs only when the Q64 residual is
//! within eight ulps of zero. Inner/tail
//! classification is a reserve-ratio comparison, so ordinary transition
//! probes take only a software Q64 square root. Fixed-point products are
//! decomposed into bounded limbs; no live multiplication requires a wider
//! runtime integer.

use anchor_lang::prelude::*;
#[cfg(test)]
use std::cell::Cell;

use crate::{constants::NAD, errors::ErrorCode};

use super::{
    cpmm::{cpmm_amount_in_nad, cpmm_amount_out_nad},
    isqrt, mul_div_ceil_u128, mul_div_rem_u128, mul_div_u128, ratio_lte_full_width,
};

const Q48_BITS: u32 = 48;
pub(super) const Q48_ONE: u128 = 1_u128 << Q48_BITS;
const Q64_BITS: u32 = 64;
const Q64_ONE: u128 = 1_u128 << Q64_BITS;
const Q80_BITS: u32 = 80;
const Q80_ONE: u128 = 1_u128 << Q80_BITS;
const PRICE_BITS: u32 = 32;
const PRICE_ONE: u128 = 1_u128 << PRICE_BITS;
const Q64_RESIDUAL_AMBIGUITY_ULPS: u128 = 8;

pub(crate) const CONCENTRATED_MAX_PEAK_DEPTH_NAD: u128 = 2_000 * NAD as u128;
pub(crate) const CONCENTRATED_MAX_FADE_SCALE_NAD: u128 = 199_000_000;
/// Persisted cache/invariant identity. Increment this only when executable
/// curve mathematics changes; stale cached roots then become cold hints rather
/// than requiring a market-state migration.
pub(crate) const CONCENTRATED_MATH_REVISION: u8 = 2;
pub(crate) const CONCENTRATED_INVARIANT_MAX_ITERS: usize = 65;
pub(crate) const CONCENTRATED_RESERVE_MAX_ITERS: usize = 65;
pub(crate) const CONCENTRATED_MIN_PEAK_DEPTH_NAD: u128 = 2 * NAD as u128;
pub(crate) const MAX_COMMON_RESERVE: u128 = u64::MAX as u128;
pub(crate) const MIN_INNER_COMMON_RESERVE: u128 = NAD as u128;
const C1_TRANSITION_START_SHIFT: u32 = 2;

#[cfg(test)]
thread_local! {
    static RESIDUAL_EVALUATIONS: Cell<usize> = const { Cell::new(0) };
    static SQRT_Q64_EVALUATIONS: Cell<usize> = const { Cell::new(0) };
    static SQRT_Q80_EVALUATIONS: Cell<usize> = const { Cell::new(0) };
    static Q80_FALLBACK_EVALUATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_residual_evaluations() {
    RESIDUAL_EVALUATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn residual_evaluations() -> usize {
    RESIDUAL_EVALUATIONS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_sqrt_q64_evaluations() {
    SQRT_Q64_EVALUATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn sqrt_q64_evaluations() -> usize {
    SQRT_Q64_EVALUATIONS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_sqrt_q80_evaluations() {
    SQRT_Q80_EVALUATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn sqrt_q80_evaluations() -> usize {
    SQRT_Q80_EVALUATIONS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_q80_fallback_evaluations() {
    Q80_FALLBACK_EVALUATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn q80_fallback_evaluations() -> usize {
    Q80_FALLBACK_EVALUATIONS.with(Cell::get)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcentratedSwapDirection {
    BaseToQuote,
    QuoteToBase,
}

/// Homogeneous unit used by the dimensionless invariant.
///
/// The selector always converts the higher-valued asset into the
/// lower-valued asset's unit. Consequently one raw normalized input atom
/// advances by at least one common atom on either side of a unit center.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcentratedCommonNumeraire {
    Quote,
    Base,
}

/// Exact rational conversion `common = asset * numerator / denominator`.
/// The ratio is intentionally not collapsed to one NAD-scaled rate: doing so
/// would add a second rounding layer when quote is converted into base units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedCommonScale {
    numerator: u128,
    denominator: u128,
}

impl ConcentratedCommonScale {
    #[cfg(test)]
    pub(crate) const fn numerator(self) -> u128 {
        self.numerator
    }

    #[cfg(test)]
    pub(crate) const fn denominator(self) -> u128 {
        self.denominator
    }

    pub(crate) fn to_common_floor(self, amount_nad: u128) -> Result<u128> {
        mul_div_floor(amount_nad, self.numerator, self.denominator)
    }

    pub(crate) fn common_to_raw_floor(self, common_nad: u128) -> Result<u128> {
        mul_div_floor(common_nad, self.denominator, self.numerator)
    }

    pub(crate) fn common_to_raw_ceil(self, common_nad: u128) -> Result<u128> {
        mul_div_ceil_u128(common_nad, self.denominator, self.numerator).map_err(|_| ErrorCode::InvariantOverflow.into())
    }
}

impl ConcentratedCommonNumeraire {
    pub(crate) fn for_center(center_price_nad: u128) -> Result<Self> {
        require!(center_price_nad > 0, ErrorCode::InvalidArgument);
        Ok(if center_price_nad >= NAD as u128 {
            Self::Quote
        } else {
            Self::Base
        })
    }

    pub(crate) fn base_scale(self, center_price_nad: u128) -> Result<ConcentratedCommonScale> {
        require!(center_price_nad > 0, ErrorCode::InvalidArgument);
        Ok(match self {
            Self::Quote => ConcentratedCommonScale {
                numerator: center_price_nad,
                denominator: NAD as u128,
            },
            Self::Base => ConcentratedCommonScale {
                numerator: 1,
                denominator: 1,
            },
        })
    }

    pub(crate) fn quote_scale(self, center_price_nad: u128) -> Result<ConcentratedCommonScale> {
        require!(center_price_nad > 0, ErrorCode::InvalidArgument);
        Ok(match self {
            Self::Quote => ConcentratedCommonScale {
                numerator: 1,
                denominator: 1,
            },
            Self::Base => ConcentratedCommonScale {
                numerator: NAD as u128,
                denominator: center_price_nad,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcentratedHybridBranch {
    Inner,
    BaseScarceTransition,
    QuoteScarceTransition,
    BaseScarceTail,
    QuoteScarceTail,
}

impl ConcentratedHybridBranch {
    pub(crate) const fn is_exact_tail(self) -> bool {
        matches!(self, Self::BaseScarceTail | Self::QuoteScarceTail)
    }

    pub(crate) const fn same_exact_tail(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::BaseScarceTail, Self::BaseScarceTail) | (Self::QuoteScarceTail, Self::QuoteScarceTail)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedC1Geometry {
    peak_q64: u128,
    scale_q64: u128,
    peak_q80: u128,
    scale_q80: u128,
    pub(super) q_start_q48: u128,
    pub(super) q_tail_q48: u128,
    q_start_q64: u128,
    q_tail_q64: u128,
    q_start_q80: u128,
    q_tail_q80: u128,
    v_start_q64: u128,
    v_tail_q64: u128,
    v_start_q80: u128,
    v_tail_q80: u128,
    pub(super) v_start_q48: u128,
    pub(super) v_tail_q48: u128,
    pub(super) reserve_ratio_start_q48: u128,
    pub(super) reserve_ratio_tail_q48: u128,
    reserve_ratio_start_q80: u128,
    reserve_ratio_tail_q80: u128,
    negative_q_prime_start_q64: u128,
    negative_q_prime_start_q80: u128,
    pub(super) negative_q_prime_start_q48: u128,
}

/// Parameter-bound authoritative geometry persisted by market state.
///
/// Q80 derivation is intentionally paid only when the applied shape changes.
/// Ordinary quotes reconstruct every Q64/Q48 projection with shifts.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, Eq, InitSpace, PartialEq)]
pub struct ConcentratedGeometryCache {
    math_revision: u8,
    peak_depth_nad: u64,
    fade_scale_nad: u64,
    peak_q80: u128,
    scale_q80: u128,
    v_start_q80: u128,
    v_tail_q80: u128,
    reserve_ratio_start_q80: u128,
    reserve_ratio_tail_q80: u128,
    negative_q_prime_start_q80: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConcentratedResidualContext {
    branch: ConcentratedHybridBranch,
    target_q64: u128,
    transition_cosh_q64: u128,
    transition_negative_q_prime_q64: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConcentratedResidualEvaluation {
    positive: bool,
    magnitude: u128,
    q64: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedEvaluation {
    pub invariant_d: u128,
    pub balanced_equivalent_q: u128,
    pub marginal_price_nad: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedPreparedCurve {
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    pub(crate) base_common: u128,
    pub(crate) quote_common: u128,
    pub(crate) center_price_nad: u128,
    pub(crate) peak_depth_nad: u128,
    pub(crate) fade_scale_nad: u128,
    invariant_d: u128,
    common_numeraire: ConcentratedCommonNumeraire,
    pub(crate) geometry: Option<ConcentratedC1Geometry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcentratedInvariantSeed {
    Hint(u128),
    #[cfg(test)]
    Exact(u128),
}

impl ConcentratedPreparedCurve {
    /// Canonical integer invariant: the first atom at which the residual has
    /// crossed from its lower-side sign. Adjacent-atom proof state remains
    /// local to the solver and is never stored in the prepared curve.
    pub(crate) const fn invariant_d(self) -> u128 {
        self.invariant_d
    }

    pub(crate) fn balanced_equivalent_q(self) -> Result<u128> {
        if self.peak_depth_nad == 0 {
            // CPMM's balanced-equivalent depth is exactly sqrt(x*y) in the
            // assets' own normalized coordinates. It is independent of the
            // controller center; deriving it through center-normalized D
            // would introduce two unrelated integer-rounding losses.
            return geometric_mean_floor(self.base_reserve_nad, self.quote_reserve_nad);
        }

        let invariant_d = self.invariant_d();
        require!(invariant_d > 0 && self.center_price_nad > 0, ErrorCode::InvalidArgument);
        let (mut normalized_numerator, mut normalized_denominator) = match self.common_numeraire {
            // D is in quote units: Q = D/2 * sqrt(NAD/center).
            ConcentratedCommonNumeraire::Quote => (
                NAD as u128,
                self.center_price_nad
                    .checked_mul(4)
                    .ok_or(ErrorCode::InvariantOverflow)?,
            ),
            // D is in base units: Q = D/2 * sqrt(center/NAD).
            ConcentratedCommonNumeraire::Base => (
                self.center_price_nad,
                (NAD as u128).checked_mul(4).ok_or(ErrorCode::InvariantOverflow)?,
            ),
        };

        // Seed the orientation-aware physical Q ratio in [1/4, 4].
        // Powers of four become exact powers of two after the square root, so
        // this retains 48 fractional bits without constructing the potentially
        // 160-bit radicand.
        let exact_ratio_numerator = normalized_numerator;
        let exact_ratio_denominator = normalized_denominator;
        let mut numerator_scale = 0_u32;
        let mut denominator_scale = 0_u32;
        if normalized_numerator < normalized_denominator {
            for _ in 0..64 {
                if normalized_numerator >= normalized_denominator {
                    break;
                }
                normalized_numerator = normalized_numerator
                    .checked_mul(4)
                    .ok_or(ErrorCode::InvariantOverflow)?;
                numerator_scale += 1;
            }
        } else {
            for _ in 0..64 {
                let four_denominator = normalized_denominator
                    .checked_mul(4)
                    .ok_or(ErrorCode::InvariantOverflow)?;
                if normalized_numerator <= four_denominator {
                    break;
                }
                normalized_denominator = four_denominator;
                denominator_scale += 1;
            }
        }
        require!(
            normalized_numerator
                .checked_mul(4)
                .ok_or(ErrorCode::InvariantOverflow)?
                >= normalized_denominator,
            ErrorCode::InvariantOverflow
        );
        require!(
            normalized_numerator
                <= normalized_denominator
                    .checked_mul(4)
                    .ok_or(ErrorCode::InvariantOverflow)?,
            ErrorCode::InvariantOverflow
        );
        let ratio_q48 = mul_div_u128(normalized_numerator, Q48_ONE, normalized_denominator)
            .map_err(|_| ErrorCode::InvariantOverflow)?;
        let sqrt_ratio_q48 = isqrt(ratio_q48.checked_mul(Q48_ONE).ok_or(ErrorCode::InvariantOverflow)?);
        let mut candidate = if numerator_scale > 0 {
            mul_div_u128(
                invariant_d,
                sqrt_ratio_q48,
                Q48_ONE
                    .checked_shl(numerator_scale)
                    .ok_or(ErrorCode::InvariantOverflow)?,
            )
        } else {
            mul_div_u128(
                invariant_d,
                sqrt_ratio_q48
                    .checked_shl(denominator_scale)
                    .ok_or(ErrorCode::InvariantOverflow)?,
                Q48_ONE,
            )
        }
        .map_err(|_| ErrorCode::InvariantOverflow)?
        .max(1);

        // Exact floor(D^2*NAD/(denominator*probe)) without a wide integer.
        // Each quotient is root-sized; every discarded remainder is carried
        // into the next stage before the final one-bit comparison.
        let (scaled_d, scaled_d_remainder) =
            mul_div_rem_u128(invariant_d, exact_ratio_numerator, exact_ratio_denominator)
                .map_err(|_| ErrorCode::InvariantOverflow)?;
        let quotient_at = |probe: u128| -> Result<u128> {
            require!(probe > 0, ErrorCode::DenominatorOverflow);
            let (whole, whole_remainder) =
                mul_div_rem_u128(invariant_d, scaled_d, probe).map_err(|_| ErrorCode::InvariantOverflow)?;
            let (fractional, fractional_remainder) =
                mul_div_rem_u128(invariant_d, scaled_d_remainder, probe).map_err(|_| ErrorCode::InvariantOverflow)?;
            let fractional_whole = fractional / exact_ratio_denominator;
            let fractional_modulus = fractional % exact_ratio_denominator;
            let (cross_whole, cross_remainder) = mul_div_rem_u128(fractional_modulus, probe, exact_ratio_denominator)
                .map_err(|_| ErrorCode::InvariantOverflow)?;
            let cross_fraction = cross_remainder
                .checked_add(fractional_remainder)
                .ok_or(ErrorCode::InvariantOverflow)?
                / exact_ratio_denominator;
            let carried = cross_whole
                .checked_add(cross_fraction)
                .ok_or(ErrorCode::InvariantOverflow)?;
            let carry = u128::from(carried >= probe.checked_sub(whole_remainder).ok_or(ErrorCode::InvariantOverflow)?);
            whole
                .checked_add(fractional_whole)
                .and_then(|value| value.checked_add(carry))
                .ok_or_else(|| ErrorCode::InvariantOverflow.into())
        };

        let reciprocal = quotient_at(candidate)?;
        candidate = if reciprocal >= candidate {
            candidate
                .checked_add((reciprocal - candidate) / 2)
                .ok_or(ErrorCode::InvariantOverflow)?
        } else {
            reciprocal + (candidate - reciprocal) / 2
        }
        .max(1);

        // The normalized seed followed by one exact Newton step is adjacent
        // over the configured domain. Fail closed if that bound ever changes.
        for _ in 0..4 {
            let quotient = quotient_at(candidate)?;
            if quotient < candidate {
                candidate = candidate.checked_sub(1).ok_or(ErrorCode::InvariantOverflow)?;
                if candidate == 0 {
                    return Ok(0);
                }
                continue;
            }
            let successor = candidate.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
            if quotient_at(successor)? >= successor {
                candidate = successor;
                continue;
            }
            return Ok(candidate);
        }
        err!(ErrorCode::InvariantOverflow)
    }

    pub(crate) fn marginal_price_nad(self) -> Result<u128> {
        if self.peak_depth_nad == 0 {
            return mul_div_floor(self.quote_reserve_nad, NAD as u128, self.base_reserve_nad);
        }
        concentrated_marginal_price_from_common_with_geometry(
            self.base_common,
            self.quote_common,
            self.invariant_d,
            self.center_price_nad,
            self.peak_depth_nad,
            self.fade_scale_nad,
            self.geometry.ok_or(ErrorCode::BrokenInvariant)?,
        )
    }

    pub(crate) fn evaluation(self) -> Result<ConcentratedEvaluation> {
        Ok(ConcentratedEvaluation {
            invariant_d: self.invariant_d(),
            balanced_equivalent_q: self.balanced_equivalent_q()?,
            marginal_price_nad: self.marginal_price_nad()?,
        })
    }

    pub(crate) fn quote_exact_in(self, amount_in_nad: u128, direction: ConcentratedSwapDirection) -> Result<u128> {
        if amount_in_nad == 0 {
            return Ok(0);
        }
        if self.peak_depth_nad == 0 {
            return match direction {
                ConcentratedSwapDirection::BaseToQuote => {
                    cpmm_amount_out_nad(self.base_reserve_nad, self.quote_reserve_nad, amount_in_nad)
                }
                ConcentratedSwapDirection::QuoteToBase => {
                    cpmm_amount_out_nad(self.quote_reserve_nad, self.base_reserve_nad, amount_in_nad)
                }
            };
        }
        let geometry = self.geometry.ok_or(ErrorCode::BrokenInvariant)?;
        let start_branch = geometry.branch(self.base_common, self.quote_common)?;
        if start_branch.is_exact_tail() {
            let output = match direction {
                ConcentratedSwapDirection::BaseToQuote => {
                    cpmm_amount_out_nad(self.base_reserve_nad, self.quote_reserve_nad, amount_in_nad)?
                }
                ConcentratedSwapDirection::QuoteToBase => {
                    cpmm_amount_out_nad(self.quote_reserve_nad, self.base_reserve_nad, amount_in_nad)?
                }
            };
            let (base_after, quote_after) = match direction {
                ConcentratedSwapDirection::BaseToQuote => (
                    self.base_reserve_nad
                        .checked_add(amount_in_nad)
                        .ok_or(ErrorCode::InvariantOverflow)?,
                    self.quote_reserve_nad
                        .checked_sub(output)
                        .ok_or(ErrorCode::OutputAmountOverflow)?,
                ),
                ConcentratedSwapDirection::QuoteToBase => (
                    self.base_reserve_nad
                        .checked_sub(output)
                        .ok_or(ErrorCode::OutputAmountOverflow)?,
                    self.quote_reserve_nad
                        .checked_add(amount_in_nad)
                        .ok_or(ErrorCode::InvariantOverflow)?,
                ),
            };
            let (base_after_common, quote_after_common) =
                normalize_reserves(base_after, quote_after, self.center_price_nad)?;
            if start_branch.same_exact_tail(geometry.branch(base_after_common, quote_after_common)?) {
                return Ok(output);
            }
        }
        let (input_reserve_nad, output_reserve_nad, input_common, output_common) = match direction {
            ConcentratedSwapDirection::BaseToQuote => (
                self.base_reserve_nad,
                self.quote_reserve_nad,
                self.base_common,
                self.quote_common,
            ),
            ConcentratedSwapDirection::QuoteToBase => (
                self.quote_reserve_nad,
                self.base_reserve_nad,
                self.quote_common,
                self.base_common,
            ),
        };
        // Normalize the complete post-trade reserve. In general,
        // floor((R+dR)*a/b) != floor(R*a/b)+floor(dR*a/b); composing rounded
        // deltas would create a low-center quote bucket.
        let input_after_nad = input_reserve_nad
            .checked_add(amount_in_nad)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let input_after_common = self.input_common_scale(direction)?.to_common_floor(input_after_nad)?;
        if input_after_common <= input_common {
            return Ok(0);
        }
        validate_common_reserves(input_after_common, output_common)?;
        let output_common_delta =
            if !hybrid_residual(input_after_common, output_common, self.invariant_d, Some(geometry))?.0 {
                0
            } else {
                let (_, output_after_common) =
                    solve_variable_reserve(input_after_common, self.invariant_d, Some(geometry), 1, output_common)?;
                // The solver returns the smallest valid reserve atom, so the
                // corresponding output is maximal in common coordinates.
                output_common
                    .checked_sub(output_after_common)
                    .ok_or(ErrorCode::OutputAmountOverflow)?
            };
        let output_after_common = output_common
            .checked_sub(output_common_delta)
            .ok_or(ErrorCode::OutputAmountOverflow)?;
        let output_after_nad = self
            .output_common_scale(direction)?
            .common_to_raw_ceil(output_after_common)?;
        require!(
            output_after_nad > 0 && output_after_nad <= output_reserve_nad,
            ErrorCode::OutputAmountOverflow
        );
        let output = output_reserve_nad - output_after_nad;
        // The common-coordinate solver already certifies that its returned
        // reserve is the smallest valid atom. Every adaptive raw-to-common
        // scale is >= 1, so ceil(common * denominator / numerator) is the
        // smallest raw reserve mapping to that endpoint or above; its raw
        // predecessor maps strictly below it. The inverse-floor lemma is
        // exhaustively checked at the supported center boundaries in tests.
        Ok(output)
    }

    /// Returns `(largest_known_insufficient_input, smallest_sufficient_input)`
    /// in raw normalized input units for one exact-output request.
    pub(crate) fn quote_exact_out_input_bracket(
        self,
        amount_out_nad: u128,
        direction: ConcentratedSwapDirection,
    ) -> Result<(u128, u128)> {
        let (input_reserve_nad, output_reserve_nad, input_common, output_common) = match direction {
            ConcentratedSwapDirection::BaseToQuote => (
                self.base_reserve_nad,
                self.quote_reserve_nad,
                self.base_common,
                self.quote_common,
            ),
            ConcentratedSwapDirection::QuoteToBase => (
                self.quote_reserve_nad,
                self.base_reserve_nad,
                self.quote_common,
                self.base_common,
            ),
        };
        require!(
            amount_out_nad > 0 && amount_out_nad < output_reserve_nad,
            ErrorCode::InsufficientLiquidity
        );
        if self.peak_depth_nad == 0 {
            let input = cpmm_amount_in_nad(input_reserve_nad, output_reserve_nad, amount_out_nad)?;
            return Ok((input.saturating_sub(1), input));
        }
        let geometry = self.geometry.ok_or(ErrorCode::BrokenInvariant)?;
        let start_branch = geometry.branch(self.base_common, self.quote_common)?;
        if start_branch.is_exact_tail() {
            let input = match direction {
                ConcentratedSwapDirection::BaseToQuote => {
                    cpmm_amount_in_nad(self.base_reserve_nad, self.quote_reserve_nad, amount_out_nad)?
                }
                ConcentratedSwapDirection::QuoteToBase => {
                    cpmm_amount_in_nad(self.quote_reserve_nad, self.base_reserve_nad, amount_out_nad)?
                }
            };
            let (base_after, quote_after) = match direction {
                ConcentratedSwapDirection::BaseToQuote => (
                    self.base_reserve_nad
                        .checked_add(input)
                        .ok_or(ErrorCode::InvariantOverflow)?,
                    self.quote_reserve_nad
                        .checked_sub(amount_out_nad)
                        .ok_or(ErrorCode::InsufficientLiquidity)?,
                ),
                ConcentratedSwapDirection::QuoteToBase => (
                    self.base_reserve_nad
                        .checked_sub(amount_out_nad)
                        .ok_or(ErrorCode::InsufficientLiquidity)?,
                    self.quote_reserve_nad
                        .checked_add(input)
                        .ok_or(ErrorCode::InvariantOverflow)?,
                ),
            };
            let (base_after_common, quote_after_common) =
                normalize_reserves(base_after, quote_after, self.center_price_nad)?;
            if start_branch.same_exact_tail(geometry.branch(base_after_common, quote_after_common)?) {
                return Ok((input.saturating_sub(1), input));
            }
        }

        let output_after_nad = output_reserve_nad
            .checked_sub(amount_out_nad)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        let output_after_common = self.output_common_scale(direction)?.to_common_floor(output_after_nad)?;
        let common_output = output_common
            .checked_sub(output_after_common)
            .ok_or(ErrorCode::BrokenInvariant)?;
        require!(common_output > 0, ErrorCode::InsufficientLiquidity);
        require!(input_common < MAX_COMMON_RESERVE, ErrorCode::InsufficientLiquidity);
        require!(
            hybrid_residual(output_after_common, MAX_COMMON_RESERVE, self.invariant_d, self.geometry,)?.0,
            ErrorCode::InsufficientLiquidity
        );
        let (_, sufficient_input_common) = solve_variable_reserve(
            output_after_common,
            self.invariant_d,
            self.geometry,
            input_common,
            MAX_COMMON_RESERVE,
        )?;
        let sufficient_common_delta = sufficient_input_common
            .checked_sub(input_common)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let sufficient_input_common = input_common
            .checked_add(sufficient_common_delta)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let sufficient_input_after_nad = self
            .input_common_scale(direction)?
            .common_to_raw_ceil(sufficient_input_common)?;
        let sufficient_input = sufficient_input_after_nad
            .checked_sub(input_reserve_nad)
            .ok_or(ErrorCode::BrokenInvariant)?;
        require!(sufficient_input > 0, ErrorCode::InsufficientLiquidity);
        Ok((sufficient_input - 1, sufficient_input))
    }

    pub(crate) const fn common_numeraire(self) -> ConcentratedCommonNumeraire {
        self.common_numeraire
    }

    pub(crate) fn input_common_scale(self, direction: ConcentratedSwapDirection) -> Result<ConcentratedCommonScale> {
        match direction {
            ConcentratedSwapDirection::BaseToQuote => self.common_numeraire.base_scale(self.center_price_nad),
            ConcentratedSwapDirection::QuoteToBase => self.common_numeraire.quote_scale(self.center_price_nad),
        }
    }

    pub(crate) fn output_common_scale(self, direction: ConcentratedSwapDirection) -> Result<ConcentratedCommonScale> {
        match direction {
            ConcentratedSwapDirection::BaseToQuote => self.common_numeraire.quote_scale(self.center_price_nad),
            ConcentratedSwapDirection::QuoteToBase => self.common_numeraire.base_scale(self.center_price_nad),
        }
    }

    pub(crate) const fn base_reserve_nad(self) -> u128 {
        self.base_reserve_nad
    }

    pub(crate) const fn quote_reserve_nad(self) -> u128 {
        self.quote_reserve_nad
    }

    pub(crate) fn geometry_cache(self) -> Option<ConcentratedGeometryCache> {
        self.geometry.map(|geometry| ConcentratedGeometryCache {
            math_revision: CONCENTRATED_MATH_REVISION,
            peak_depth_nad: self.peak_depth_nad as u64,
            fade_scale_nad: self.fade_scale_nad as u64,
            peak_q80: geometry.peak_q80,
            scale_q80: geometry.scale_q80,
            v_start_q80: geometry.v_start_q80,
            v_tail_q80: geometry.v_tail_q80,
            reserve_ratio_start_q80: geometry.reserve_ratio_start_q80,
            reserve_ratio_tail_q80: geometry.reserve_ratio_tail_q80,
            negative_q_prime_start_q80: geometry.negative_q_prime_start_q80,
        })
    }

    /// Prepare a same-parameter endpoint without deriving geometry again.
    /// Raw-rounded trade and retained-surcharge endpoints both use this path.
    pub(crate) fn prepare_successor(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
        invariant_seed: ConcentratedInvariantSeed,
    ) -> Result<Self> {
        prepare_curve_internal(
            base_reserve_nad,
            quote_reserve_nad,
            self.center_price_nad,
            self.peak_depth_nad,
            self.fade_scale_nad,
            self.geometry_cache(),
            Some(invariant_seed),
        )
    }
}

fn mul_div_floor(a: u128, b: u128, denominator: u128) -> Result<u128> {
    mul_div_u128(a, b, denominator).map_err(|_| ErrorCode::InvariantOverflow.into())
}

fn to_q48_nad(value_nad: u128) -> Result<u128> {
    mul_div_floor(value_nad, Q48_ONE, NAD as u128)
}

fn mul_q48(a: u128, b: u128) -> Result<u128> {
    a.checked_mul(b)
        .map(|product| product >> Q48_BITS)
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

fn mul_q64(a: u128, b: u128) -> Result<u128> {
    let (a_hi, a_lo) = (a >> Q64_BITS, a & (Q64_ONE - 1));
    let (b_hi, b_lo) = (b >> Q64_BITS, b & (Q64_ONE - 1));
    a_hi.checked_mul(b_hi)
        .and_then(|value| value.checked_mul(Q64_ONE))
        .and_then(|value| value.checked_add(a_hi.checked_mul(b_lo)?))
        .and_then(|value| value.checked_add(b_hi.checked_mul(a_lo)?))
        .and_then(|value| value.checked_add(a_lo.checked_mul(b_lo)? >> Q64_BITS))
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

fn div_q64(a: u128, b: u128) -> Result<u128> {
    require!(b > 0, ErrorCode::DenominatorOverflow);
    mul_div_floor(a, Q64_ONE, b)
}

fn sqrt_q64(value_q64: u128) -> Result<u128> {
    #[cfg(test)]
    SQRT_Q64_EVALUATIONS.with(|count| count.set(count.get() + 1));
    if value_q64 == 0 {
        return Ok(0);
    }
    let mut normalized_q64 = value_q64;
    let mut scale_up = 0_u32;
    let mut scale_down = 0_u32;
    while normalized_q64 > Q64_ONE {
        normalized_q64 >>= 2;
        scale_up = scale_up.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
    }
    while normalized_q64 < (Q64_ONE >> 2) {
        normalized_q64 = normalized_q64.checked_shl(2).ok_or(ErrorCode::InvariantOverflow)?;
        scale_down = scale_down.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
    }
    if normalized_q64 == Q64_ONE {
        return Q64_ONE
            .checked_shl(scale_up)
            .map(|value| value >> scale_down)
            .ok_or_else(|| ErrorCode::InvariantOverflow.into());
    }
    let mut candidate = isqrt(
        normalized_q64
            .checked_mul(Q64_ONE)
            .ok_or(ErrorCode::InvariantOverflow)?,
    )
    .checked_shl(scale_up)
    .map(|value| value >> scale_down)
    .ok_or(ErrorCode::InvariantOverflow)?;
    // Power-of-four normalization is exact below unity. Above unity its
    // right shift may discard low bits, so certify the canonical floor against
    // the original radicand with a bounded adjacent correction.
    for _ in 0..4 {
        if !ratio_lte_full_width(candidate, value_q64, Q64_ONE, candidate)? {
            candidate = candidate.checked_sub(1).ok_or(ErrorCode::InvariantOverflow)?;
            continue;
        }
        let successor = candidate.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
        if ratio_lte_full_width(successor, value_q64, Q64_ONE, successor)? {
            candidate = successor;
            continue;
        }
        return Ok(candidate);
    }
    err!(ErrorCode::InvariantOverflow)
}

fn mul_q80(a: u128, b: u128) -> Result<u128> {
    const LIMB_BITS: u32 = 40;
    const LIMB: u128 = 1_u128 << LIMB_BITS;
    let (a_hi, a_lo) = (a >> LIMB_BITS, a & (LIMB - 1));
    let (b_hi, b_lo) = (b >> LIMB_BITS, b & (LIMB - 1));
    let high = a_hi.checked_mul(b_hi).ok_or(ErrorCode::InvariantOverflow)?;
    let cross = a_hi
        .checked_mul(b_lo)
        .and_then(|value| value.checked_add(a_lo.checked_mul(b_hi)?))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let low = a_lo.checked_mul(b_lo).ok_or(ErrorCode::InvariantOverflow)?;
    high.checked_add(cross >> LIMB_BITS)
        .and_then(|value| {
            (cross & (LIMB - 1))
                .checked_shl(LIMB_BITS)
                .and_then(|fraction| fraction.checked_add(low))
                .and_then(|fraction| value.checked_add(fraction >> Q80_BITS))
        })
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

fn div_q80(a: u128, b: u128) -> Result<u128> {
    require!(b > 0, ErrorCode::DenominatorOverflow);
    mul_div_floor(a, Q80_ONE, b)
}

fn sqrt_q80(value_q80: u128) -> Result<u128> {
    #[cfg(test)]
    SQRT_Q80_EVALUATIONS.with(|count| count.set(count.get() + 1));
    if value_q80 == 0 {
        return Ok(0);
    }
    let mut normalized_q80 = value_q80;
    let mut scale_up = 0_u32;
    let mut scale_down = 0_u32;
    while normalized_q80 > Q80_ONE {
        normalized_q80 >>= 2;
        scale_up = scale_up.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
    }
    while normalized_q80 < (Q80_ONE >> 2) {
        normalized_q80 = normalized_q80.checked_shl(2).ok_or(ErrorCode::InvariantOverflow)?;
        scale_down = scale_down.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
    }
    if normalized_q80 == Q80_ONE {
        return Q80_ONE
            .checked_shl(scale_up)
            .map(|value| value >> scale_down)
            .ok_or_else(|| ErrorCode::InvariantOverflow.into());
    }
    let normalized_q64 = normalized_q80 >> (Q80_BITS - Q64_BITS);
    require!(normalized_q64 > 0, ErrorCode::InvariantOverflow);
    let seed_q64 = sqrt_q64(normalized_q64)?;
    let seed_q80 = seed_q64
        .checked_shl(Q80_BITS - Q64_BITS)
        .and_then(|value| value.checked_shl(scale_up))
        .map(|value| value >> scale_down)
        .ok_or(ErrorCode::InvariantOverflow)?;
    require!(seed_q80 > 0, ErrorCode::InvariantOverflow);
    // A Q64 root shifted into Q80 is within one Q64 quantum. One exact Newton
    // step squares that relative error; the adjacent proof below then selects
    // the canonical floor without an open-ended fixed-point iteration.
    let reciprocal = mul_div_floor(value_q80, Q80_ONE, seed_q80)?;
    let mut candidate = seed_q80.checked_add(reciprocal).ok_or(ErrorCode::InvariantOverflow)? / 2;
    for _ in 0..4 {
        if !ratio_lte_full_width(candidate, value_q80, Q80_ONE, candidate)? {
            candidate = candidate.checked_sub(1).ok_or(ErrorCode::InvariantOverflow)?;
            continue;
        }
        let successor = candidate.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
        if ratio_lte_full_width(successor, value_q80, Q80_ONE, successor)? {
            candidate = successor;
            continue;
        }
        return Ok(candidate);
    }
    err!(ErrorCode::InvariantOverflow)
}

fn div_q48(a: u128, b: u128) -> Result<u128> {
    require!(b > 0, ErrorCode::DenominatorOverflow);
    a.checked_mul(Q48_ONE)
        .and_then(|numerator| numerator.checked_div(b))
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

pub(super) fn mul_scalar_q48(value: u128, factor_q48: u128) -> Result<u128> {
    let whole = value
        .checked_mul(factor_q48 >> Q48_BITS)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let fractional = value
        .checked_mul(factor_q48 & (Q48_ONE - 1))
        .ok_or(ErrorCode::InvariantOverflow)?
        >> Q48_BITS;
    whole
        .checked_add(fractional)
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

fn ratio_q48(numerator: u128, denominator: u128) -> Result<u128> {
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    numerator
        .checked_mul(Q48_ONE)
        .and_then(|value| value.checked_div(denominator))
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

fn ratio_q32(numerator: u128, denominator: u128) -> Result<u128> {
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    numerator
        .checked_mul(PRICE_ONE)
        .and_then(|value| value.checked_div(denominator))
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

pub(super) fn validate_parameters(center_price_nad: u128, peak_depth_nad: u128, fade_scale_nad: u128) -> Result<()> {
    require!(
        center_price_nad > 0 && center_price_nad <= u64::MAX as u128,
        ErrorCode::InvalidArgument
    );
    require!(
        peak_depth_nad <= CONCENTRATED_MAX_PEAK_DEPTH_NAD,
        ErrorCode::InvalidArgument
    );
    if peak_depth_nad == 0 || fade_scale_nad == 0 {
        require!(peak_depth_nad == 0 && fade_scale_nad == 0, ErrorCode::InvalidArgument);
    } else {
        require!(
            fade_scale_nad <= CONCENTRATED_MAX_FADE_SCALE_NAD && fade_scale_nad <= peak_depth_nad.saturating_mul(100),
            ErrorCode::InvalidArgument
        );
    }
    Ok(())
}

pub(super) fn validate_common_reserves(x: u128, y: u128) -> Result<()> {
    require!(x > 0 && y > 0, ErrorCode::InvalidArgument);
    Ok(())
}

fn validate_bounded_common_reserves(x: u128, y: u128) -> Result<()> {
    validate_common_reserves(x, y)?;
    require!(
        x <= MAX_COMMON_RESERVE && y <= MAX_COMMON_RESERVE,
        ErrorCode::InvalidArgument
    );
    Ok(())
}

pub(super) fn normalize_reserves(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
) -> Result<(u128, u128)> {
    let numeraire = ConcentratedCommonNumeraire::for_center(center_price_nad)?;
    let base_common = numeraire
        .base_scale(center_price_nad)?
        .to_common_floor(base_reserve_nad)?;
    let quote_common = numeraire
        .quote_scale(center_price_nad)?
        .to_common_floor(quote_reserve_nad)?;
    validate_bounded_common_reserves(base_common, quote_common)?;
    Ok((base_common, quote_common))
}

fn balance_factor_q48(x: u128, y: u128, d: u128) -> Result<u128> {
    validate_bounded_common_reserves(x, y)?;
    require!(d > 0, ErrorCode::DenominatorOverflow);
    let twice_x = x.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    let twice_y = y.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    let x_ratio = ratio_q48(twice_x, d)?;
    let y_ratio = ratio_q48(twice_y, d)?;
    mul_q48(x_ratio, y_ratio)
}

fn low_high_ratio_q64(x: u128, y: u128) -> Result<u128> {
    validate_bounded_common_reserves(x, y)?;
    // Both common reserves are at most u64::MAX, so this product fits u128
    // directly and avoids the general full-width quotient path.
    x.min(y)
        .checked_mul(Q64_ONE)
        .and_then(|value| value.checked_div(x.max(y)))
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

fn balance_factor_q64(x: u128, y: u128, d: u128) -> Result<u128> {
    balance_factor_fixed(x, y, d, Q64_ONE)
}

fn balance_factor_fixed(x: u128, y: u128, d: u128, fixed_one: u128) -> Result<u128> {
    validate_bounded_common_reserves(x, y)?;
    require!(d > 0, ErrorCode::DenominatorOverflow);
    let twice_x = x.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    let twice_y = y.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    // Exact floor(4*x*y*Q64/D^2), retaining both division remainders. If
    // 2*x*Q64 = a*D+r, a*2*y = b*D+s, and r*2*y = c*D+t, then the answer is
    // b + floor((s+c)/D); the remaining (s+c)%D*D+t is strictly below D^2.
    let (scaled_x, scaled_x_remainder) =
        mul_div_rem_u128(twice_x, fixed_one, d).map_err(|_| ErrorCode::InvariantOverflow)?;
    let (whole, whole_remainder) = mul_div_rem_u128(scaled_x, twice_y, d).map_err(|_| ErrorCode::InvariantOverflow)?;
    let (carried, _) = mul_div_rem_u128(scaled_x_remainder, twice_y, d).map_err(|_| ErrorCode::InvariantOverflow)?;
    whole
        .checked_add(
            whole_remainder
                .checked_add(carried)
                .ok_or(ErrorCode::InvariantOverflow)?
                / d,
        )
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

impl ConcentratedC1Geometry {
    pub(crate) fn from_cache(
        cache: ConcentratedGeometryCache,
        peak_depth_nad: u128,
        fade_scale_nad: u128,
    ) -> Result<Self> {
        require!(
            cache.matches(peak_depth_nad, fade_scale_nad),
            ErrorCode::BrokenInvariant
        );
        let q_start_q80 = Q80_ONE
            .checked_sub(cache.scale_q80 >> C1_TRANSITION_START_SHIFT)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let q_tail_q80 = Q80_ONE
            .checked_sub(cache.scale_q80)
            .ok_or(ErrorCode::InvariantOverflow)?;
        require!(
            cache.peak_q80 > 0
                && cache.scale_q80 > 0
                && cache.scale_q80 < Q80_ONE
                && q_start_q80 > q_tail_q80
                && cache.v_start_q80 < cache.v_tail_q80
                && cache.reserve_ratio_start_q80 > cache.reserve_ratio_tail_q80
                && cache.reserve_ratio_start_q80 <= Q80_ONE
                && cache.reserve_ratio_tail_q80 > 0
                && cache.negative_q_prime_start_q80 > 0,
            ErrorCode::BrokenInvariant
        );
        let q_start_q64 = q_start_q80 >> (Q80_BITS - Q64_BITS);
        let q_tail_q64 = q_tail_q80 >> (Q80_BITS - Q64_BITS);
        let v_start_q64 = cache.v_start_q80 >> (Q80_BITS - Q64_BITS);
        let v_tail_q64 = cache.v_tail_q80 >> (Q80_BITS - Q64_BITS);
        let negative_q_prime_start_q64 = cache.negative_q_prime_start_q80 >> (Q80_BITS - Q64_BITS);
        require!(
            q_start_q64 > q_tail_q64 && v_start_q64 < v_tail_q64 && negative_q_prime_start_q64 > 0,
            ErrorCode::BrokenInvariant
        );
        Ok(Self {
            peak_q64: cache.peak_q80 >> (Q80_BITS - Q64_BITS),
            scale_q64: cache.scale_q80 >> (Q80_BITS - Q64_BITS),
            peak_q80: cache.peak_q80,
            scale_q80: cache.scale_q80,
            q_start_q48: q_start_q64 >> (Q64_BITS - Q48_BITS),
            q_tail_q48: q_tail_q64 >> (Q64_BITS - Q48_BITS),
            q_start_q64,
            q_tail_q64,
            q_start_q80,
            q_tail_q80,
            v_start_q64,
            v_tail_q64,
            v_start_q80: cache.v_start_q80,
            v_tail_q80: cache.v_tail_q80,
            v_start_q48: v_start_q64 >> (Q64_BITS - Q48_BITS),
            v_tail_q48: v_tail_q64 >> (Q64_BITS - Q48_BITS),
            reserve_ratio_start_q48: cache.reserve_ratio_start_q80 >> (Q80_BITS - Q48_BITS),
            reserve_ratio_tail_q48: cache.reserve_ratio_tail_q80 >> (Q80_BITS - Q48_BITS),
            reserve_ratio_start_q80: cache.reserve_ratio_start_q80,
            reserve_ratio_tail_q80: cache.reserve_ratio_tail_q80,
            negative_q_prime_start_q64,
            negative_q_prime_start_q80: cache.negative_q_prime_start_q80,
            negative_q_prime_start_q48: negative_q_prime_start_q64 >> (Q64_BITS - Q48_BITS),
        })
    }

    pub(super) fn derive(peak_depth_nad: u128, fade_scale_nad: u128) -> Result<Self> {
        let peak_q80 = mul_div_floor(peak_depth_nad, Q80_ONE, NAD as u128)?;
        let scale_q80 = mul_div_floor(fade_scale_nad, Q80_ONE, NAD as u128)?;
        require!(
            peak_q80 > 0 && scale_q80 > 0 && scale_q80 < Q80_ONE,
            ErrorCode::InvalidArgument
        );
        // The protocol fixes the first join at delta=s/4. Solving the inner
        // equation for its reserve-shape coordinate avoids a 30-step Q80 root
        // search during every geometry construction.
        let delta_start_q80 = scale_q80 >> C1_TRANSITION_START_SHIFT;
        require!(delta_start_q80 > 0, ErrorCode::InvalidArgument);
        let q_start_q80 = Q80_ONE
            .checked_sub(delta_start_q80)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let q_tail_q80 = Q80_ONE.checked_sub(scale_q80).ok_or(ErrorCode::InvariantOverflow)?;
        require!(q_start_q80 > q_tail_q80, ErrorCode::BrokenInvariant);
        let weight_base_q80 = div_q80(
            scale_q80,
            scale_q80
                .checked_add(delta_start_q80)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )?;
        let weight_q80 = mul_q80(weight_base_q80, weight_base_q80)?;
        let coefficient_q80 = mul_q80(
            peak_q80.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
            mul_q80(q_start_q80, weight_q80)?,
        )?;
        require!(coefficient_q80 > 0, ErrorCode::DenominatorOverflow);
        let h_start_q80 = div_q80(delta_start_q80, coefficient_q80)?;
        let sqrt_q_start_q80 = sqrt_q80(q_start_q80)?;
        let cosh_start_q80 = div_q80(
            Q80_ONE.checked_add(h_start_q80).ok_or(ErrorCode::InvariantOverflow)?,
            sqrt_q_start_q80,
        )?;
        let cosh_start_squared_q80 = mul_q80(cosh_start_q80, cosh_start_q80)?;
        let v_start_q80 = sqrt_q80(
            cosh_start_squared_q80
                .checked_sub(Q80_ONE)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )?;

        // Differentiate the inner scalar equation at the first join. The
        // transition is q(v)=q_tail+(q_start-q_tail)*(1-z)^3, so matching its
        // initial slope fixes the whole transition length without a third
        // operator parameter.
        let inverse_q80 = div_q80(Q80_ONE, q_start_q80)?;
        let two_over_denominator_q80 = div_q80(
            Q80_ONE.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
            scale_q80
                .checked_add(delta_start_q80)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )?;
        let coefficient_derivative_q80 = mul_q80(
            coefficient_q80,
            inverse_q80
                .checked_add(two_over_denominator_q80)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )?;
        let residual_q_q80 = mul_q80(coefficient_derivative_q80, h_start_q80)?
            .checked_add(div_q80(
                mul_q80(coefficient_q80, cosh_start_q80)?,
                sqrt_q_start_q80.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
            )?)
            .and_then(|value| value.checked_add(Q80_ONE))
            .ok_or(ErrorCode::InvariantOverflow)?;
        let residual_v_q80 = div_q80(
            mul_q80(mul_q80(coefficient_q80, sqrt_q_start_q80)?, v_start_q80)?,
            cosh_start_q80,
        )?;
        let negative_q_prime_start_q80 = div_q80(residual_v_q80, residual_q_q80)?;
        require!(negative_q_prime_start_q80 > 0, ErrorCode::DenominatorOverflow);
        let transition_drop_q80 = q_start_q80
            .checked_sub(q_tail_q80)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let transition_length_q80 = div_q80(
            transition_drop_q80.checked_mul(3).ok_or(ErrorCode::InvariantOverflow)?,
            negative_q_prime_start_q80,
        )?;
        require!(transition_length_q80 > 0, ErrorCode::DenominatorOverflow);
        let v_tail_q80 = v_start_q80
            .checked_add(transition_length_q80)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let start_ratio_factor_q80 = cosh_start_q80
            .checked_sub(v_start_q80)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let reserve_ratio_start_q80 = mul_q80(start_ratio_factor_q80, start_ratio_factor_q80)?;
        let cosh_tail_q80 = sqrt_q80(
            Q80_ONE
                .checked_add(mul_q80(v_tail_q80, v_tail_q80)?)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )?;
        let tail_ratio_factor_q80 = cosh_tail_q80
            .checked_sub(v_tail_q80)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let reserve_ratio_tail_q80 = mul_q80(tail_ratio_factor_q80, tail_ratio_factor_q80)?;
        require!(
            reserve_ratio_start_q80 > reserve_ratio_tail_q80,
            ErrorCode::BrokenInvariant
        );
        // The variable-reserve scalar is globally one-to-one if
        // r(v)=(-q'(v)*cosh(v))/(2q(v))<1. Across the cubic transition,
        // -q'(v)<=m_start, q(v)>=q_tail, and
        // cosh(v)<=1+v<=1+v_tail.  Check the resulting sufficient bound with
        // an exact ratio comparison so fixed-point rounding at the join
        // cannot invalidate the continuous proof.
        let twice_q_tail_minus_one = q_tail_q80
            .checked_mul(2)
            .and_then(|value| value.checked_sub(1))
            .ok_or(ErrorCode::InvariantOverflow)?;
        require!(
            ratio_lte_full_width(
                negative_q_prime_start_q80,
                Q80_ONE,
                twice_q_tail_minus_one,
                Q80_ONE.checked_add(v_tail_q80).ok_or(ErrorCode::InvariantOverflow)?,
            )?,
            ErrorCode::BrokenInvariant
        );
        let q_start_q64 = q_start_q80 >> (Q80_BITS - Q64_BITS);
        let q_tail_q64 = q_tail_q80 >> (Q80_BITS - Q64_BITS);
        let v_start_q64 = v_start_q80 >> (Q80_BITS - Q64_BITS);
        let v_tail_q64 = v_tail_q80 >> (Q80_BITS - Q64_BITS);
        let negative_q_prime_start_q64 = negative_q_prime_start_q80 >> (Q80_BITS - Q64_BITS);
        let q_start_q48 = q_start_q64 >> (Q64_BITS - Q48_BITS);
        let q_tail_q48 = q_tail_q64 >> (Q64_BITS - Q48_BITS);
        let v_start_q48 = v_start_q64 >> (Q64_BITS - Q48_BITS);
        let v_tail_q48 = v_tail_q64 >> (Q64_BITS - Q48_BITS);
        let negative_q_prime_start_q48 = negative_q_prime_start_q64 >> (Q64_BITS - Q48_BITS);
        Ok(Self {
            peak_q64: peak_q80 >> (Q80_BITS - Q64_BITS),
            scale_q64: scale_q80 >> (Q80_BITS - Q64_BITS),
            peak_q80,
            scale_q80,
            q_start_q48,
            q_tail_q48,
            q_start_q64,
            q_tail_q64,
            q_start_q80,
            q_tail_q80,
            v_start_q64,
            v_tail_q64,
            v_start_q80,
            v_tail_q80,
            v_start_q48,
            v_tail_q48,
            reserve_ratio_start_q48: reserve_ratio_start_q80 >> (Q80_BITS - Q48_BITS),
            reserve_ratio_tail_q48: reserve_ratio_tail_q80 >> (Q80_BITS - Q48_BITS),
            reserve_ratio_start_q80,
            reserve_ratio_tail_q80,
            negative_q_prime_start_q64,
            negative_q_prime_start_q80,
            negative_q_prime_start_q48,
        })
    }

    pub(crate) fn branch(self, x: u128, y: u128) -> Result<ConcentratedHybridBranch> {
        self.branch_from_ratio_q64(x, y, low_high_ratio_q64(x, y)?)
    }

    fn branch_from_ratio_q64(self, x: u128, y: u128, ratio_q64: u128) -> Result<ConcentratedHybridBranch> {
        let base_is_scarce = x < y;
        let ratio_tail_q64 = self.reserve_ratio_tail_q80 >> (Q80_BITS - Q64_BITS);
        let ratio_start_q64 = self.reserve_ratio_start_q80 >> (Q80_BITS - Q64_BITS);
        let tail_side = ratio_q64 < ratio_tail_q64
            || (ratio_q64 == ratio_tail_q64
                && ratio_lte_full_width(x.min(y), x.max(y), self.reserve_ratio_tail_q80, Q80_ONE)?);
        if tail_side {
            return Ok(if base_is_scarce {
                ConcentratedHybridBranch::BaseScarceTail
            } else {
                ConcentratedHybridBranch::QuoteScarceTail
            });
        }
        let transition_side = ratio_q64 < ratio_start_q64
            || (ratio_q64 == ratio_start_q64
                && ratio_lte_full_width(x.min(y), x.max(y), self.reserve_ratio_start_q80, Q80_ONE)?);
        Ok(if transition_side {
            if base_is_scarce {
                ConcentratedHybridBranch::BaseScarceTransition
            } else {
                ConcentratedHybridBranch::QuoteScarceTransition
            }
        } else {
            ConcentratedHybridBranch::Inner
        })
    }

    pub(super) fn transition_q_and_slope_at_v(self, v_q48: u128) -> Result<(u128, u128)> {
        if v_q48 <= self.v_start_q48 {
            return Ok((self.q_start_q48, self.negative_q_prime_start_q48));
        }
        if v_q48 >= self.v_tail_q48 {
            return Ok((self.q_tail_q48, 0));
        }
        let (q_q64, slope_q64) = self.transition_q_and_slope_at_v_q64(
            v_q48
                .checked_shl(Q64_BITS - Q48_BITS)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )?;
        Ok((q_q64 >> (Q64_BITS - Q48_BITS), slope_q64 >> (Q64_BITS - Q48_BITS)))
    }

    fn transition_q_and_slope_at_v_q64(self, v_q64: u128) -> Result<(u128, u128)> {
        require!(
            v_q64 >= self.v_start_q64 && v_q64 <= self.v_tail_q64,
            ErrorCode::BrokenInvariant
        );
        let z = mul_div_floor(
            v_q64
                .checked_sub(self.v_start_q64)
                .ok_or(ErrorCode::InvariantOverflow)?,
            Q64_ONE,
            self.v_tail_q64
                .checked_sub(self.v_start_q64)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )?
        .min(Q64_ONE);
        let one_minus_z = Q64_ONE - z;
        let one_minus_z_squared = mul_q64(one_minus_z, one_minus_z)?;
        let one_minus_z_cubed = mul_q64(one_minus_z_squared, one_minus_z)?;
        let transition_drop = self
            .q_start_q64
            .checked_sub(self.q_tail_q64)
            .ok_or(ErrorCode::InvariantOverflow)?;
        Ok((
            self.q_tail_q64
                .checked_add(mul_q64(transition_drop, one_minus_z_cubed)?)
                .ok_or(ErrorCode::InvariantOverflow)?,
            mul_q64(self.negative_q_prime_start_q64, one_minus_z_squared)?,
        ))
    }

    #[cfg(test)]
    fn transition_q_and_slope_at_v_q80(self, v_q80: u128) -> Result<(u128, u128)> {
        require!(
            v_q80 >= self.v_start_q80 && v_q80 <= self.v_tail_q80,
            ErrorCode::BrokenInvariant
        );
        let z = mul_div_floor(
            v_q80
                .checked_sub(self.v_start_q80)
                .ok_or(ErrorCode::InvariantOverflow)?,
            Q80_ONE,
            self.v_tail_q80
                .checked_sub(self.v_start_q80)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )?
        .min(Q80_ONE);
        let one_minus_z = Q80_ONE - z;
        let one_minus_z_squared = mul_q80(one_minus_z, one_minus_z)?;
        let one_minus_z_cubed = mul_q80(one_minus_z_squared, one_minus_z)?;
        let transition_drop = self
            .q_start_q80
            .checked_sub(self.q_tail_q80)
            .ok_or(ErrorCode::InvariantOverflow)?;
        Ok((
            self.q_tail_q80
                .checked_add(mul_q80(transition_drop, one_minus_z_cubed)?)
                .ok_or(ErrorCode::InvariantOverflow)?,
            mul_q80(self.negative_q_prime_start_q80, one_minus_z_squared)?,
        ))
    }
}

impl ConcentratedGeometryCache {
    pub(crate) fn derive(peak_depth_nad: u128, fade_scale_nad: u128) -> Result<Self> {
        validate_parameters(NAD as u128, peak_depth_nad, fade_scale_nad)?;
        require!(peak_depth_nad > 0, ErrorCode::InvalidArgument);
        let geometry = ConcentratedC1Geometry::derive(peak_depth_nad, fade_scale_nad)?;
        Ok(Self {
            math_revision: CONCENTRATED_MATH_REVISION,
            peak_depth_nad: u64::try_from(peak_depth_nad).map_err(|_| ErrorCode::InvalidArgument)?,
            fade_scale_nad: u64::try_from(fade_scale_nad).map_err(|_| ErrorCode::InvalidArgument)?,
            peak_q80: geometry.peak_q80,
            scale_q80: geometry.scale_q80,
            v_start_q80: geometry.v_start_q80,
            v_tail_q80: geometry.v_tail_q80,
            reserve_ratio_start_q80: geometry.reserve_ratio_start_q80,
            reserve_ratio_tail_q80: geometry.reserve_ratio_tail_q80,
            negative_q_prime_start_q80: geometry.negative_q_prime_start_q80,
        })
    }

    pub(crate) fn matches(self, peak_depth_nad: u128, fade_scale_nad: u128) -> bool {
        self.math_revision == CONCENTRATED_MATH_REVISION
            && u128::from(self.peak_depth_nad) == peak_depth_nad
            && u128::from(self.fade_scale_nad) == fade_scale_nad
    }
}

impl ConcentratedResidualContext {
    fn derive(geometry: ConcentratedC1Geometry, x: u128, y: u128) -> Result<Self> {
        let ratio_q64 = low_high_ratio_q64(x, y)?;
        let branch = geometry.branch_from_ratio_q64(x, y, ratio_q64)?;
        if branch.is_exact_tail() {
            return Ok(Self {
                branch,
                target_q64: geometry.q_tail_q64,
                transition_cosh_q64: 0,
                transition_negative_q_prime_q64: 0,
            });
        }
        if branch != ConcentratedHybridBranch::Inner {
            // Only transition probes need the radial coordinate. Inner and
            // exact CPMM-tail classification is a single low/high ratio
            // comparison. The cached Q80 geometry is projected to Q64 once;
            // ordinary probes never execute a Q80 square root.
            require!(ratio_q64 > 0 && ratio_q64 <= Q64_ONE, ErrorCode::InvalidArgument);
            let sqrt_ratio_q64 = sqrt_q64(ratio_q64)?;
            let denominator = sqrt_ratio_q64.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
            let v_q64 = div_q64(Q64_ONE - ratio_q64, denominator)?;
            let cosh_q64 = div_q64(
                Q64_ONE.checked_add(ratio_q64).ok_or(ErrorCode::InvariantOverflow)?,
                denominator,
            )?;
            let v_q64 = v_q64.clamp(geometry.v_start_q64, geometry.v_tail_q64);
            let (target_q64, transition_negative_q_prime_q64) = geometry.transition_q_and_slope_at_v_q64(v_q64)?;
            Ok(Self {
                branch,
                target_q64,
                transition_cosh_q64: cosh_q64,
                transition_negative_q_prime_q64,
            })
        } else {
            Ok(Self {
                branch: ConcentratedHybridBranch::Inner,
                target_q64: Q64_ONE,
                transition_cosh_q64: 0,
                transition_negative_q_prime_q64: 0,
            })
        }
    }
}

#[cfg(test)]
pub(crate) fn concentrated_hybrid_branch_from_common(
    x: u128,
    y: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<ConcentratedHybridBranch> {
    if peak_depth_nad == 0 {
        validate_common_reserves(x, y)?;
        return Ok(ConcentratedHybridBranch::Inner);
    }
    ConcentratedC1Geometry::derive(peak_depth_nad, fade_scale_nad)?.branch(x, y)
}

pub(crate) fn concentrated_hybrid_branch(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<ConcentratedHybridBranch> {
    validate_parameters(center_price_nad, peak_depth_nad, fade_scale_nad)?;
    let (base_common, quote_common) = normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)?;
    if peak_depth_nad == 0 {
        validate_common_reserves(base_common, quote_common)?;
        return Ok(ConcentratedHybridBranch::Inner);
    }
    ConcentratedC1Geometry::derive(peak_depth_nad, fade_scale_nad)?.branch(base_common, quote_common)
}

pub(crate) fn concentrated_hybrid_branch_cached(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
    geometry_cache: ConcentratedGeometryCache,
) -> Result<ConcentratedHybridBranch> {
    validate_parameters(center_price_nad, peak_depth_nad, fade_scale_nad)?;
    require!(peak_depth_nad > 0, ErrorCode::InvalidArgument);
    let geometry = ConcentratedC1Geometry::from_cache(geometry_cache, peak_depth_nad, fade_scale_nad)?;
    let (base_common, quote_common) = normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)?;
    geometry.branch(base_common, quote_common)
}

fn hybrid_residual_q80(
    x: u128,
    y: u128,
    d: u128,
    geometry: ConcentratedC1Geometry,
    context: ConcentratedResidualContext,
) -> Result<(bool, u128)> {
    #[cfg(test)]
    Q80_FALLBACK_EVALUATIONS.with(|count| count.set(count.get() + 1));

    let q = balance_factor_fixed(x, y, d, Q80_ONE)?;
    let result = if context.branch == ConcentratedHybridBranch::Inner {
        let delta = Q80_ONE.saturating_sub(q.min(Q80_ONE));
        let weight_base = div_q80(
            geometry.scale_q80,
            geometry
                .scale_q80
                .checked_add(delta)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )?;
        let weight = mul_q80(weight_base, weight_base)?;
        let coefficient = mul_q80(
            geometry.peak_q80.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
            mul_q80(q, weight)?,
        )?;
        let sum = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
        let concentration = mul_div_floor(coefficient, sum.abs_diff(d), d)?;
        let q_positive = q >= Q80_ONE;
        let q_magnitude = q.abs_diff(Q80_ONE);
        let concentration_positive = sum >= d;
        if concentration_positive == q_positive {
            (
                concentration_positive,
                concentration
                    .checked_add(q_magnitude)
                    .ok_or(ErrorCode::InvariantOverflow)?,
            )
        } else if concentration >= q_magnitude {
            (concentration_positive, concentration - q_magnitude)
        } else {
            (q_positive, q_magnitude - concentration)
        }
    } else {
        let target_q80 = if context.branch.is_exact_tail() {
            geometry.q_tail_q80
        } else {
            // The high-precision radial coordinate exists only in the
            // ambiguity fallback. Dividing by the larger reserve first gives
            // v=(1-r)/(2*sqrt(r)), r=min/max, without an ill-conditioned
            // subtraction from unity.
            validate_bounded_common_reserves(x, y)?;
            let ratio_q80 = mul_div_floor(x.min(y), Q80_ONE, x.max(y))?;
            let sqrt_ratio_q80 = sqrt_q80(ratio_q80)?;
            let denominator = sqrt_ratio_q80.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
            let v_q80 = div_q80(Q80_ONE - ratio_q80, denominator)?.clamp(geometry.v_start_q80, geometry.v_tail_q80);
            let z_q80 = mul_div_floor(
                v_q80
                    .checked_sub(geometry.v_start_q80)
                    .ok_or(ErrorCode::InvariantOverflow)?,
                Q80_ONE,
                geometry
                    .v_tail_q80
                    .checked_sub(geometry.v_start_q80)
                    .ok_or(ErrorCode::InvariantOverflow)?,
            )?
            .min(Q80_ONE);
            let one_minus_z_q80 = Q80_ONE - z_q80;
            let one_minus_z_squared_q80 = mul_q80(one_minus_z_q80, one_minus_z_q80)?;
            let one_minus_z_cubed_q80 = mul_q80(one_minus_z_squared_q80, one_minus_z_q80)?;
            geometry
                .q_tail_q80
                .checked_add(mul_q80(
                    geometry
                        .q_start_q80
                        .checked_sub(geometry.q_tail_q80)
                        .ok_or(ErrorCode::InvariantOverflow)?,
                    one_minus_z_cubed_q80,
                )?)
                .ok_or(ErrorCode::InvariantOverflow)?
        };
        if q >= target_q80 {
            (true, q - target_q80)
        } else {
            (false, target_q80 - q)
        }
    };
    let magnitude_q64 = if result.1 == 0 {
        0
    } else {
        result
            .1
            .checked_add((1_u128 << (Q80_BITS - Q64_BITS)) - 1)
            .ok_or(ErrorCode::InvariantOverflow)?
            >> (Q80_BITS - Q64_BITS)
    };
    Ok((result.0, magnitude_q64))
}

fn hybrid_residual(x: u128, y: u128, d: u128, geometry: Option<ConcentratedC1Geometry>) -> Result<(bool, u128)> {
    let context = geometry
        .map(|geometry| ConcentratedResidualContext::derive(geometry, x, y))
        .transpose()?;
    hybrid_residual_with_context(x, y, d, geometry, context)
}

fn hybrid_residual_with_context(
    x: u128,
    y: u128,
    d: u128,
    geometry: Option<ConcentratedC1Geometry>,
    context: Option<ConcentratedResidualContext>,
) -> Result<(bool, u128)> {
    let evaluation = hybrid_residual_evaluation_with_context(x, y, d, geometry, context)?;
    Ok((evaluation.positive, evaluation.magnitude))
}

fn hybrid_residual_evaluation_with_context(
    x: u128,
    y: u128,
    d: u128,
    geometry: Option<ConcentratedC1Geometry>,
    context: Option<ConcentratedResidualContext>,
) -> Result<ConcentratedResidualEvaluation> {
    #[cfg(test)]
    RESIDUAL_EVALUATIONS.with(|count| count.set(count.get() + 1));

    validate_common_reserves(x, y)?;
    require!(d > 0, ErrorCode::InvalidArgument);
    if geometry.is_none() {
        let q = balance_factor_q48(x, y, d)?;
        return Ok(if q >= Q48_ONE {
            ConcentratedResidualEvaluation {
                positive: true,
                magnitude: q - Q48_ONE,
                q64: q << (Q64_BITS - Q48_BITS),
            }
        } else {
            ConcentratedResidualEvaluation {
                positive: false,
                magnitude: Q48_ONE - q,
                q64: q << (Q64_BITS - Q48_BITS),
            }
        });
    }
    let context = context.ok_or(ErrorCode::BrokenInvariant)?;
    let q = balance_factor_q64(x, y, d)?;
    if context.branch != ConcentratedHybridBranch::Inner {
        let coarse = if q >= context.target_q64 {
            (true, q - context.target_q64)
        } else {
            (false, context.target_q64 - q)
        };
        let (positive, magnitude) = if coarse.1 <= Q64_RESIDUAL_AMBIGUITY_ULPS {
            hybrid_residual_q80(x, y, d, geometry.ok_or(ErrorCode::BrokenInvariant)?, context)?
        } else {
            coarse
        };
        return Ok(ConcentratedResidualEvaluation {
            positive,
            magnitude,
            q64: q,
        });
    }
    let geometry = geometry.ok_or(ErrorCode::BrokenInvariant)?;
    let (peak, scale) = (geometry.peak_q64, geometry.scale_q64);
    let (delta, q_positive, q_magnitude) = if q >= Q64_ONE {
        (0, true, q - Q64_ONE)
    } else {
        (Q64_ONE - q, false, Q64_ONE - q)
    };
    let weight_base = div_q64(scale, scale.checked_add(delta).ok_or(ErrorCode::InvariantOverflow)?)?;
    let weight = mul_q64(weight_base, weight_base)?;
    let concentration_coefficient = mul_q64(
        peak.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
        mul_q64(q, weight)?,
    )?;
    // Retain the raw numerator/remainder of h=(x+y-D)/D through the last
    // multiplication. Materializing h first has a two-reserve-atom quantum
    // when D approaches 2*u64::MAX.
    let sum = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
    let concentration = mul_div_floor(concentration_coefficient, sum.abs_diff(d), d)?;
    let first_positive = sum >= d;
    let coarse = if first_positive == q_positive {
        (
            first_positive,
            concentration
                .checked_add(q_magnitude)
                .ok_or(ErrorCode::InvariantOverflow)?,
        )
    } else if concentration >= q_magnitude {
        (first_positive, concentration - q_magnitude)
    } else {
        (q_positive, q_magnitude - concentration)
    };
    let (positive, magnitude) = if coarse.1 <= Q64_RESIDUAL_AMBIGUITY_ULPS {
        hybrid_residual_q80(x, y, d, geometry, context)?
    } else {
        coarse
    };
    Ok(ConcentratedResidualEvaluation {
        positive,
        magnitude,
        q64: q,
    })
}

/// Exact negative-side residual magnitude at the structural upper endpoint
/// `D = x + y`. Here `h = 0`, so the concentration term vanishes and only the
/// applicable fixed-point balance threshold remains. Keeping this closed form avoids
/// a full hybrid residual evaluation while preserving the solver bracket.
fn invariant_sum_endpoint_magnitude(
    x: u128,
    y: u128,
    d: u128,
    peak_depth_nad: u128,
    context: Option<ConcentratedResidualContext>,
) -> Result<u128> {
    require_eq!(
        d,
        x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?,
        ErrorCode::BrokenInvariant
    );
    let (q, threshold) = if peak_depth_nad == 0 {
        (balance_factor_q48(x, y, d)?, Q48_ONE)
    } else {
        let context = context.ok_or(ErrorCode::BrokenInvariant)?;
        (balance_factor_q64(x, y, d)?, context.target_q64)
    };
    require_gte!(threshold, q, ErrorCode::InvariantOverflow);
    Ok(threshold - q)
}

fn transition_newton_probe(
    fixed: u128,
    variable: u128,
    evaluation: ConcentratedResidualEvaluation,
    context: Option<ConcentratedResidualContext>,
) -> Result<Option<u128>> {
    let Some(context) =
        context.filter(|context| context.branch != ConcentratedHybridBranch::Inner && !context.branch.is_exact_tail())
    else {
        return Ok(None);
    };
    // In the transition, R(y)=4xy/D^2-q(v(y)). Its exact continuous
    // derivative satisfies
    //
    //   y R'(y) = q - m*cosh(v)/2,  y < x
    //             q + m*cosh(v)/2,  y > x,
    //
    // where m=-dq/dv. This is only an accelerator: the caller's sign bracket
    // and final adjacent-atom condition remain authoritative under rounding.
    let half_slope_cosh = mul_q64(context.transition_negative_q_prime_q64, context.transition_cosh_q64)? / 2;
    let derivative_times_variable = if variable < fixed {
        evaluation.q64.checked_sub(half_slope_cosh)
    } else {
        evaluation.q64.checked_add(half_slope_cosh)
    };
    let Some(derivative_times_variable) = derivative_times_variable.filter(|value| *value > 0) else {
        return Ok(None);
    };
    let step = mul_div_ceil_u128(evaluation.magnitude, variable, derivative_times_variable)
        .map_err(|_| ErrorCode::InvariantOverflow)?
        .max(1);
    Ok(if evaluation.positive {
        variable.checked_sub(step)
    } else {
        variable.checked_add(step)
    })
}

fn geometric_mean_floor(x: u128, y: u128) -> Result<u128> {
    let root = if let Some(product) = x.checked_mul(y) {
        // A leading-bit seed reaches the exact floor root in logarithmically
        // fewer u128 divisions than starting Babylonian iteration at y/2.
        isqrt(product)
    } else {
        // Retain the top 63-64 bits of both factors. Their product fits u128,
        // and choosing an even total shift makes the square-root rescaling
        // exact. One full-width Newton step then recovers all discarded bits.
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

        // At the configured raw-reserve/decimal bounds the Newton candidate is
        // adjacent to the exact root. The fixed correction bound fails closed
        // if those normalization bounds are ever widened.
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

#[cfg(test)]
fn geometric_lower_d(x: u128, y: u128) -> Result<u128> {
    geometric_mean_floor(x, y)?
        .checked_mul(2)
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

fn prepare_curve_internal(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
    geometry_cache: Option<ConcentratedGeometryCache>,
    invariant_seed: Option<ConcentratedInvariantSeed>,
) -> Result<ConcentratedPreparedCurve> {
    validate_parameters(center_price_nad, peak_depth_nad, fade_scale_nad)?;
    let common_numeraire = ConcentratedCommonNumeraire::for_center(center_price_nad)?;
    let (base_common, quote_common) = if peak_depth_nad > 0 {
        normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)?
    } else {
        // CPMM quotes do not use concentrated residual arithmetic and therefore
        // support the wider normalized-reserve domain.
        let base_common = common_numeraire
            .base_scale(center_price_nad)?
            .to_common_floor(base_reserve_nad)?;
        let quote_common = common_numeraire
            .quote_scale(center_price_nad)?
            .to_common_floor(quote_reserve_nad)?;
        validate_common_reserves(base_common, quote_common)?;
        (base_common, quote_common)
    };
    let geometry = if peak_depth_nad == 0 {
        require!(geometry_cache.is_none(), ErrorCode::InvalidArgument);
        None
    } else {
        Some(if let Some(cache) = geometry_cache {
            ConcentratedC1Geometry::from_cache(cache, peak_depth_nad, fade_scale_nad)?
        } else {
            ConcentratedC1Geometry::derive(peak_depth_nad, fade_scale_nad)?
        })
    };
    let residual_context = geometry
        .map(|geometry| ConcentratedResidualContext::derive(geometry, base_common, quote_common))
        .transpose()?;
    let invariant_d = match invariant_seed {
        #[cfg(test)]
        Some(ConcentratedInvariantSeed::Exact(invariant_d)) => {
            require!(invariant_d > 0, ErrorCode::BrokenInvariant);
            if peak_depth_nad == 0 {
                require_eq!(
                    invariant_d,
                    geometric_lower_d(base_common, quote_common)?,
                    ErrorCode::BrokenInvariant
                );
            } else {
                let (canonical_sign, canonical_magnitude) =
                    hybrid_residual_with_context(base_common, quote_common, invariant_d, geometry, residual_context)?;
                let adjacent = invariant_d.checked_sub(1).ok_or(ErrorCode::BrokenInvariant)?;
                require!(
                    hybrid_residual_with_context(base_common, quote_common, adjacent, geometry, residual_context,)?.0,
                    ErrorCode::BrokenInvariant
                );
                if canonical_sign {
                    require_eq!(canonical_magnitude, 0, ErrorCode::BrokenInvariant);
                    require_eq!(
                        invariant_d,
                        geometric_lower_d(base_common, quote_common)?,
                        ErrorCode::BrokenInvariant
                    );
                } else {
                    require!(canonical_magnitude > 0, ErrorCode::BrokenInvariant);
                }
            }
            invariant_d
        }
        seed => {
            let hint = match seed {
                Some(ConcentratedInvariantSeed::Hint(value)) => Some(value),
                _ => None,
            };
            let branch = residual_context
                .map(|context| context.branch)
                .unwrap_or(ConcentratedHybridBranch::Inner);
            if peak_depth_nad > 0 && !branch.is_exact_tail() {
                require!(
                    base_common >= MIN_INNER_COMMON_RESERVE && quote_common >= MIN_INNER_COMMON_RESERVE,
                    ErrorCode::InsufficientLiquidity
                );
            }
            let mut low = geometric_mean_floor(base_common, quote_common)?
                .checked_mul(2)
                .ok_or(ErrorCode::InvariantOverflow)?;
            let mut high = base_common
                .checked_add(quote_common)
                .ok_or(ErrorCode::InvariantOverflow)?;
            if low == high || peak_depth_nad == 0 {
                low
            } else {
                let (mut low_magnitude, mut high_magnitude);
                if let Some(hint) = hint.filter(|hint| *hint > low && *hint < high) {
                    let (hint_sign, hint_magnitude) =
                        hybrid_residual_with_context(base_common, quote_common, hint, geometry, residual_context)?;
                    if hint_sign {
                        low = hint;
                        low_magnitude = hint_magnitude;
                        high_magnitude = invariant_sum_endpoint_magnitude(
                            base_common,
                            quote_common,
                            high,
                            peak_depth_nad,
                            residual_context,
                        )?;
                    } else {
                        high = hint;
                        high_magnitude = hint_magnitude;
                        let (low_sign, magnitude) =
                            hybrid_residual_with_context(base_common, quote_common, low, geometry, residual_context)?;
                        require!(low_sign, ErrorCode::InvariantOverflow);
                        low_magnitude = magnitude;
                    }
                } else {
                    let (low_sign, low_endpoint_magnitude) =
                        hybrid_residual_with_context(base_common, quote_common, low, geometry, residual_context)?;
                    let high_endpoint_magnitude = invariant_sum_endpoint_magnitude(
                        base_common,
                        quote_common,
                        high,
                        peak_depth_nad,
                        residual_context,
                    )?;
                    require!(low_sign, ErrorCode::InvariantOverflow);
                    low_magnitude = low_endpoint_magnitude;
                    high_magnitude = high_endpoint_magnitude;
                }
                for iteration in 0..CONCENTRATED_INVARIANT_MAX_ITERS {
                    if high - low <= 1 {
                        break;
                    }
                    let width = high - low;
                    let magnitude_sum = low_magnitude
                        .checked_add(high_magnitude)
                        .ok_or(ErrorCode::InvariantOverflow)?;
                    let secant_offset = if magnitude_sum == 0 {
                        width / 2
                    } else {
                        mul_div_u128(width, low_magnitude, magnitude_sum).map_err(|_| ErrorCode::InvariantOverflow)?
                    };
                    let secant_probe = low
                        .checked_add(secant_offset)
                        .filter(|probe| *probe > low && *probe < high)
                        .unwrap_or(low + width / 2);
                    let (anchor, anchor_sign, anchor_magnitude) = if low_magnitude <= high_magnitude {
                        (low, true, low_magnitude)
                    } else {
                        (high, false, high_magnitude)
                    };
                    let negative_derivative_times_d = if branch == ConcentratedHybridBranch::Inner {
                        let q = balance_factor_q64(base_common, quote_common, anchor)?;
                        let geometry = geometry.ok_or(ErrorCode::BrokenInvariant)?;
                        let (peak, scale) = (geometry.peak_q64, geometry.scale_q64);
                        let delta = Q64_ONE.saturating_sub(q.min(Q64_ONE));
                        let denominator = scale.checked_add(delta).ok_or(ErrorCode::InvariantOverflow)?;
                        let weight_base = div_q64(scale, denominator)?;
                        let weight = mul_q64(weight_base, weight_base)?;
                        let sum = base_common
                            .checked_add(quote_common)
                            .ok_or(ErrorCode::InvariantOverflow)?;
                        let h = div_q64(sum.abs_diff(anchor), anchor)?;
                        let four_q_h_over_denominator = div_q64(mul_q64(q, h)?, denominator)?
                            .checked_mul(4)
                            .ok_or(ErrorCode::InvariantOverflow)?;
                        let bracket = h
                            .checked_mul(3)
                            .and_then(|value| value.checked_add(Q64_ONE))
                            .and_then(|value| value.checked_add(four_q_h_over_denominator))
                            .ok_or(ErrorCode::InvariantOverflow)?;
                        mul_q64(
                            peak.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
                            mul_q64(q, mul_q64(weight, bracket)?)?,
                        )?
                        .checked_add(q.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?)
                        .ok_or(ErrorCode::InvariantOverflow)?
                    } else {
                        balance_factor_q64(base_common, quote_common, anchor)?
                            .checked_mul(2)
                            .ok_or(ErrorCode::InvariantOverflow)?
                    };
                    let newton_step = mul_div_ceil_u128(anchor_magnitude, anchor, negative_derivative_times_d)
                        .map_err(|_| ErrorCode::InvariantOverflow)?
                        .max(1);
                    let newton_probe = if anchor_sign {
                        anchor.checked_add(newton_step)
                    } else {
                        anchor.checked_sub(newton_step)
                    };
                    let accelerated_probe = newton_probe
                        .filter(|probe| *probe > low && *probe < high)
                        .unwrap_or(secant_probe);
                    let remaining_iterations = CONCENTRATED_INVARIANT_MAX_ITERS - iteration;
                    let bisections_required = u128::BITS as usize - (width - 1).leading_zeros() as usize;
                    let probe = if bisections_required >= remaining_iterations {
                        low + width / 2
                    } else {
                        accelerated_probe
                    };
                    let (probe_sign, probe_magnitude) =
                        hybrid_residual_with_context(base_common, quote_common, probe, geometry, residual_context)?;
                    if probe_sign {
                        low = probe;
                        low_magnitude = probe_magnitude;
                    } else {
                        high = probe;
                        high_magnitude = probe_magnitude;
                    }
                }
                require!(high - low <= 1, ErrorCode::InvariantOverflow);
                high
            }
        }
    };
    Ok(ConcentratedPreparedCurve {
        base_reserve_nad,
        quote_reserve_nad,
        base_common,
        quote_common,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
        invariant_d,
        common_numeraire,
        geometry,
    })
}

pub(crate) fn concentrated_prepare_curve(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<ConcentratedPreparedCurve> {
    prepare_curve_internal(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
        None,
        None,
    )
}

pub(crate) fn concentrated_prepare_curve_seeded_cached(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
    geometry_cache: ConcentratedGeometryCache,
    invariant_seed: ConcentratedInvariantSeed,
) -> Result<ConcentratedPreparedCurve> {
    prepare_curve_internal(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
        Some(geometry_cache),
        Some(invariant_seed),
    )
}

pub(crate) fn concentrated_prepare_curve_cached(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
    geometry_cache: ConcentratedGeometryCache,
) -> Result<ConcentratedPreparedCurve> {
    prepare_curve_internal(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
        Some(geometry_cache),
        None,
    )
}

#[cfg(test)]
fn exact_cpmm_tail_in_with_geometry(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_in_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    base_common: u128,
    quote_common: u128,
    geometry: ConcentratedC1Geometry,
) -> Result<Option<u128>> {
    let start = geometry.branch(base_common, quote_common)?;
    if !start.is_exact_tail() {
        return Ok(None);
    }
    let output = match direction {
        ConcentratedSwapDirection::BaseToQuote => {
            cpmm_amount_out_nad(base_reserve_nad, quote_reserve_nad, amount_in_nad)?
        }
        ConcentratedSwapDirection::QuoteToBase => {
            cpmm_amount_out_nad(quote_reserve_nad, base_reserve_nad, amount_in_nad)?
        }
    };
    let (base_after, quote_after) = match direction {
        ConcentratedSwapDirection::BaseToQuote => (
            base_reserve_nad
                .checked_add(amount_in_nad)
                .ok_or(ErrorCode::InvariantOverflow)?,
            quote_reserve_nad
                .checked_sub(output)
                .ok_or(ErrorCode::OutputAmountOverflow)?,
        ),
        ConcentratedSwapDirection::QuoteToBase => (
            base_reserve_nad
                .checked_sub(output)
                .ok_or(ErrorCode::OutputAmountOverflow)?,
            quote_reserve_nad
                .checked_add(amount_in_nad)
                .ok_or(ErrorCode::InvariantOverflow)?,
        ),
    };
    let (base_after_common, quote_after_common) = normalize_reserves(base_after, quote_after, center_price_nad)?;
    let end = geometry.branch(base_after_common, quote_after_common)?;
    Ok(start.same_exact_tail(end).then_some(output))
}

#[cfg(test)]
pub(super) fn exact_cpmm_tail_in_raw(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_in_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<Option<u128>> {
    validate_parameters(center_price_nad, peak_depth_nad, fade_scale_nad)?;
    let geometry = ConcentratedC1Geometry::derive(peak_depth_nad, fade_scale_nad)?;
    let (base_common, quote_common) = normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)?;
    exact_cpmm_tail_in_with_geometry(
        base_reserve_nad,
        quote_reserve_nad,
        amount_in_nad,
        direction,
        center_price_nad,
        base_common,
        quote_common,
        geometry,
    )
}

/// Brackets the smallest variable reserve with a non-negative residual.
fn solve_variable_reserve(
    fixed: u128,
    d: u128,
    geometry: Option<ConcentratedC1Geometry>,
    mut low: u128,
    mut high: u128,
) -> Result<(u128, u128)> {
    require!(low < high, ErrorCode::InvalidArgument);
    let (low_valid, mut low_magnitude) = hybrid_residual(fixed, low, d, geometry)?;
    require!(!low_valid, ErrorCode::InvariantOverflow);
    let (high_valid, mut high_magnitude) = hybrid_residual(fixed, high, d, geometry)?;
    require!(high_valid, ErrorCode::InvariantOverflow);
    // Every valid invariant state satisfies x+y>=D. Intersecting the caller's
    // generic bracket with y>=D-x removes most of the empty search interval
    // for ordinary near-center trades before the safeguarded secant starts.
    let mut safeguarded_newton_probe = None;
    let structural_probe = d.saturating_sub(fixed).max(low);
    if structural_probe > low && structural_probe < high {
        let context = geometry
            .map(|geometry| ConcentratedResidualContext::derive(geometry, fixed, structural_probe))
            .transpose()?;
        let evaluation = hybrid_residual_evaluation_with_context(fixed, structural_probe, d, geometry, context)?;
        if evaluation.positive {
            high = structural_probe;
            high_magnitude = evaluation.magnitude;
        } else {
            low = structural_probe;
            low_magnitude = evaluation.magnitude;
        }
        safeguarded_newton_probe = transition_newton_probe(fixed, structural_probe, evaluation, context)?
            .filter(|candidate| *candidate > low && *candidate < high);
    }
    let mut previous_probe_was_valid = None;
    for iteration in 0..CONCENTRATED_RESERVE_MAX_ITERS {
        let width = high - low;
        if width <= 1 {
            break;
        }
        let midpoint = low + width / 2;
        let remaining_iterations = CONCENTRATED_RESERVE_MAX_ITERS - iteration;
        let bisection_iterations_needed = (u128::BITS - (width - 1).leading_zeros()) as usize;
        let probe = if remaining_iterations <= bisection_iterations_needed {
            midpoint
        } else {
            let secant = low_magnitude
                .checked_add(high_magnitude)
                .filter(|sum| *sum > 0)
                .and_then(|sum| mul_div_floor(width, low_magnitude, sum).ok())
                .and_then(|offset| low.checked_add(offset))
                .filter(|candidate| *candidate > low && *candidate < high);
            safeguarded_newton_probe
                .take()
                .filter(|candidate| *candidate > low && *candidate < high)
                .or(secant)
                .unwrap_or(midpoint)
        };
        let context = geometry
            .map(|geometry| ConcentratedResidualContext::derive(geometry, fixed, probe))
            .transpose()?;
        let evaluation = hybrid_residual_evaluation_with_context(fixed, probe, d, geometry, context)?;
        let valid = evaluation.positive;
        let magnitude = evaluation.magnitude;
        if valid {
            high = probe;
            high_magnitude = magnitude;
            if previous_probe_was_valid == Some(true) {
                low_magnitude = low_magnitude.div_ceil(2);
            }
        } else {
            low = probe;
            low_magnitude = magnitude;
            if previous_probe_was_valid == Some(false) {
                high_magnitude = high_magnitude.div_ceil(2);
            }
        }
        safeguarded_newton_probe = transition_newton_probe(fixed, probe, evaluation, context)?
            .filter(|candidate| *candidate > low && *candidate < high);
        previous_probe_was_valid = Some(valid);
    }
    require!(high - low <= 1, ErrorCode::InvariantOverflow);
    Ok((low, high))
}

#[cfg(test)]
pub(crate) fn concentrated_quote_exact_in(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_in_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<u128> {
    validate_parameters(center_price_nad, peak_depth_nad, fade_scale_nad)?;
    if amount_in_nad == 0 {
        return Ok(0);
    }
    if peak_depth_nad == 0 {
        return match direction {
            ConcentratedSwapDirection::BaseToQuote => {
                cpmm_amount_out_nad(base_reserve_nad, quote_reserve_nad, amount_in_nad)
            }
            ConcentratedSwapDirection::QuoteToBase => {
                cpmm_amount_out_nad(quote_reserve_nad, base_reserve_nad, amount_in_nad)
            }
        };
    }
    let prepared = concentrated_prepare_curve(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
    )?;
    prepared.quote_exact_in(amount_in_nad, direction)
}

pub(crate) fn concentrated_quote_exact_out(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_out_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<u128> {
    if amount_out_nad == 0 {
        return Ok(0);
    }
    Ok(concentrated_prepare_curve(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
    )?
    .quote_exact_out_input_bracket(amount_out_nad, direction)?
    .1)
}

#[cfg(test)]
pub(crate) fn concentrated_quote_exact_out_input_lower_bound(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_out_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<u128> {
    if amount_out_nad == 0 {
        return Ok(0);
    }
    Ok(concentrated_prepare_curve(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
    )?
    .quote_exact_out_input_bracket(amount_out_nad, direction)?
    .0)
}

fn scale_price_ratio_q32(center_price_nad: u128, price_ratio_q32: u128) -> Result<u128> {
    let whole = center_price_nad
        .checked_mul(price_ratio_q32 >> PRICE_BITS)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let fractional = center_price_nad
        .checked_mul(price_ratio_q32 & (PRICE_ONE - 1))
        .ok_or(ErrorCode::InvariantOverflow)?
        >> PRICE_BITS;
    whole
        .checked_add(fractional)
        .ok_or_else(|| ErrorCode::InvariantOverflow.into())
}

fn concentrated_marginal_price_from_common_with_geometry(
    x: u128,
    y: u128,
    d: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
    geometry: ConcentratedC1Geometry,
) -> Result<u128> {
    let context = ConcentratedResidualContext::derive(geometry, x, y)?;
    match context.branch {
        ConcentratedHybridBranch::Inner => {
            validate_common_reserves(x, y)?;
            let q = balance_factor_q48(x, y, d)?;
            let peak = to_q48_nad(peak_depth_nad)?;
            let scale = to_q48_nad(fade_scale_nad)?;
            let delta = Q48_ONE.saturating_sub(q.min(Q48_ONE));
            let weight_base = div_q48(scale, scale.checked_add(delta).ok_or(ErrorCode::InvariantOverflow)?)?;
            let weight = mul_q48(weight_base, weight_base)?;
            let coefficient = mul_q48(
                peak.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
                mul_q48(q, weight)?,
            )?;
            let interaction = div_q48(
                q.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
                scale.checked_add(delta).ok_or(ErrorCode::InvariantOverflow)?,
            )?;
            let shape = Q48_ONE.checked_add(interaction).ok_or(ErrorCode::InvariantOverflow)?;
            let sum = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
            let h_d = sum.saturating_sub(d);
            let shared = mul_scalar_q48(h_d, shape)?;
            let x_shape = shared.checked_add(x).ok_or(ErrorCode::InvariantOverflow)?;
            let y_shape = shared.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
            let q_d = mul_scalar_q48(d, q)?;
            let x_core = mul_scalar_q48(x_shape, coefficient)?
                .checked_add(q_d)
                .ok_or(ErrorCode::InvariantOverflow)?;
            let y_core = mul_scalar_q48(y_shape, coefficient)?
                .checked_add(q_d)
                .ok_or(ErrorCode::InvariantOverflow)?;
            require!(x_core > 0 && y_core > 0, ErrorCode::InvariantOverflow);

            // In the inner branch y/x and x_core/y_core move in opposite
            // directions. Q32 is sufficient for the protocol's 25 ppm price
            // budget and keeps their product below u128.
            let reserve_ratio = ratio_q32(y, x)?;
            let core_ratio = ratio_q32(x_core, y_core)?;
            let price_ratio = reserve_ratio
                .checked_mul(core_ratio)
                .map(|value| value >> PRICE_BITS)
                .ok_or(ErrorCode::InvariantOverflow)?;
            scale_price_ratio_q32(center_price_nad, price_ratio)
        }
        ConcentratedHybridBranch::BaseScarceTail | ConcentratedHybridBranch::QuoteScarceTail => {
            mul_div_floor(y, center_price_nad, x)
        }
        ConcentratedHybridBranch::BaseScarceTransition | ConcentratedHybridBranch::QuoteScarceTransition => {
            let q = context.target_q64 >> (Q64_BITS - Q48_BITS);
            let negative_q_prime = context.transition_negative_q_prime_q64 >> (Q64_BITS - Q48_BITS);
            let cosh = context.transition_cosh_q64 >> (Q64_BITS - Q48_BITS);
            let slope_ratio = div_q48(
                mul_q48(negative_q_prime, cosh)?,
                q.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
            )?;
            require!(slope_ratio < Q48_ONE, ErrorCode::InvariantOverflow);
            let lower = Q48_ONE.checked_sub(slope_ratio).ok_or(ErrorCode::InvariantOverflow)?;
            let upper = Q48_ONE.checked_add(slope_ratio).ok_or(ErrorCode::InvariantOverflow)?;
            let reserve_ratio_q32 = ratio_q32(y, x)?;
            let price_ratio_q32 = if y >= x {
                mul_div_floor(reserve_ratio_q32, lower, upper)?
            } else {
                mul_div_floor(reserve_ratio_q32, upper, lower)?
            };
            scale_price_ratio_q32(center_price_nad, price_ratio_q32)
        }
    }
}

pub(crate) fn concentrated_marginal_price_from_common(
    x: u128,
    y: u128,
    d: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<u128> {
    concentrated_marginal_price_from_common_with_geometry(
        x,
        y,
        d,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
        ConcentratedC1Geometry::derive(peak_depth_nad, fade_scale_nad)?,
    )
}

pub(crate) fn concentrated_marginal_price_nad(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<u128> {
    validate_parameters(center_price_nad, peak_depth_nad, fade_scale_nad)?;
    require!(
        base_reserve_nad > 0 && quote_reserve_nad > 0,
        ErrorCode::InvalidArgument
    );
    if peak_depth_nad == 0 {
        return mul_div_floor(quote_reserve_nad, NAD as u128, base_reserve_nad);
    }
    concentrated_prepare_curve(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
    )?
    .marginal_price_nad()
}

#[cfg(test)]
pub(crate) fn concentrated_evaluate(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    fade_scale_nad: u128,
) -> Result<ConcentratedEvaluation> {
    concentrated_prepare_curve(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        fade_scale_nad,
    )?
    .evaluation()
}

#[cfg(test)]
mod tests {
    include!("../tests/math/concentrated.rs");

    mod high_precision_reference {
        include!("../tests/math/concentrated_reference.rs");
    }
}

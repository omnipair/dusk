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
    cpmm::{cpmm_amount_in_nad, cpmm_amount_out_nad, geometric_mean_floor},
    isqrt, mul_div_ceil_u128, mul_div_rem_u128, mul_div_u128, ratio_lte_full_width,
};

const Q48_BITS: u32 = 48;
pub(super) const Q48_ONE: u128 = 1_u128 << Q48_BITS;
const BOUNDED_Q48_RECIPROCAL_BITS: u32 = 112;
const BOUNDED_Q48_RECIPROCAL_ONE: u128 = 1_u128 << BOUNDED_Q48_RECIPROCAL_BITS;
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
pub(crate) const MAX_RESIDUAL_PROBES_PER_VARIABLE_LEG: usize = 5;
pub(crate) const MAX_RESIDUAL_PROBES_PER_BOUNDED_EXACT_OUT_LEG: usize = 3;
const _: [(); 3] = [(); MAX_RESIDUAL_PROBES_PER_BOUNDED_EXACT_OUT_LEG];
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
    static CERTIFIED_Q48_RESIDUAL_EVALUATIONS: Cell<usize> = const { Cell::new(0) };
    static CERTIFIED_Q48_EXACT_FALLBACK_EVALUATIONS: Cell<usize> = const { Cell::new(0) };
    static BOUNDED_Q48_RECIPROCAL_BUILDS: Cell<usize> = const { Cell::new(0) };
    static BOUNDED_Q48_RECIPROCAL_QUOTIENTS: Cell<usize> = const { Cell::new(0) };
    static BOUNDED_Q48_RECIPROCAL_FALLBACKS: Cell<usize> = const { Cell::new(0) };
    static CERTIFIED_Q48_RESIDUALS_ENABLED: Cell<bool> = const { Cell::new(true) };
    static BOUNDED_EXACT_IN_FIRST_RAW_FALLBACKS: Cell<usize> = const { Cell::new(0) };
    static BOUNDED_EXACT_IN_FIRST_RAW_PROBE_ORDINAL: Cell<usize> = const { Cell::new(0) };
    static BOUNDED_EXACT_IN_FIRST_RAW_LOW: Cell<u128> = const { Cell::new(0) };
    static BOUNDED_EXACT_IN_FIRST_RAW_SELECTED: Cell<u128> = const { Cell::new(0) };
    static CANONICAL_D_HINT_HITS: Cell<usize> = const { Cell::new(0) };
    static CANONICAL_D_HINT_MISSES: Cell<usize> = const { Cell::new(0) };
    static CANONICAL_D_HINT_RESIDUAL_EVALUATIONS: Cell<usize> = const { Cell::new(0) };
    // Differential tests can disable only the Inner Newton hint while
    // retaining every exact bracket/sign check used by production.
    static INNER_NEWTON_ACCELERATION_ENABLED: Cell<bool> = const { Cell::new(true) };
}

#[cfg(test)]
pub(crate) fn set_inner_newton_acceleration_enabled(enabled: bool) -> bool {
    INNER_NEWTON_ACCELERATION_ENABLED.with(|state| {
        let previous = state.get();
        state.set(enabled);
        previous
    })
}

#[cfg(test)]
pub(crate) fn reset_residual_evaluations() {
    RESIDUAL_EVALUATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn reset_bounded_exact_in_first_raw_fallbacks() {
    BOUNDED_EXACT_IN_FIRST_RAW_FALLBACKS.with(|count| count.set(0));
    BOUNDED_EXACT_IN_FIRST_RAW_PROBE_ORDINAL.with(|ordinal| ordinal.set(0));
    BOUNDED_EXACT_IN_FIRST_RAW_LOW.with(|reserve| reserve.set(0));
    BOUNDED_EXACT_IN_FIRST_RAW_SELECTED.with(|reserve| reserve.set(0));
}

#[cfg(test)]
pub(crate) fn bounded_exact_in_first_raw_fallbacks() -> usize {
    BOUNDED_EXACT_IN_FIRST_RAW_FALLBACKS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn bounded_exact_in_first_raw_trace() -> (usize, u128, u128) {
    (
        BOUNDED_EXACT_IN_FIRST_RAW_PROBE_ORDINAL.with(Cell::get),
        BOUNDED_EXACT_IN_FIRST_RAW_LOW.with(Cell::get),
        BOUNDED_EXACT_IN_FIRST_RAW_SELECTED.with(Cell::get),
    )
}

#[cfg(test)]
pub(crate) fn residual_evaluations() -> usize {
    RESIDUAL_EVALUATIONS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_canonical_d_hint_counters() {
    CANONICAL_D_HINT_HITS.with(|count| count.set(0));
    CANONICAL_D_HINT_MISSES.with(|count| count.set(0));
    CANONICAL_D_HINT_RESIDUAL_EVALUATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn canonical_d_hint_counters() -> (usize, usize, usize) {
    (
        CANONICAL_D_HINT_HITS.with(Cell::get),
        CANONICAL_D_HINT_MISSES.with(Cell::get),
        CANONICAL_D_HINT_RESIDUAL_EVALUATIONS.with(Cell::get),
    )
}

#[cfg(test)]
pub(crate) fn set_certified_q48_residuals_enabled(enabled: bool) -> bool {
    CERTIFIED_Q48_RESIDUALS_ENABLED.with(|state| {
        let previous = state.get();
        state.set(enabled);
        previous
    })
}

#[cfg(test)]
pub(crate) fn reset_certified_q48_residual_evaluations() {
    CERTIFIED_Q48_RESIDUAL_EVALUATIONS.with(|count| count.set(0));
    CERTIFIED_Q48_EXACT_FALLBACK_EVALUATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn certified_q48_residual_evaluations() -> usize {
    CERTIFIED_Q48_RESIDUAL_EVALUATIONS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn certified_q48_exact_fallback_evaluations() -> usize {
    CERTIFIED_Q48_EXACT_FALLBACK_EVALUATIONS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_bounded_q48_reciprocal_counters() {
    BOUNDED_Q48_RECIPROCAL_BUILDS.with(|count| count.set(0));
    BOUNDED_Q48_RECIPROCAL_QUOTIENTS.with(|count| count.set(0));
    BOUNDED_Q48_RECIPROCAL_FALLBACKS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn bounded_q48_reciprocal_counters() -> (usize, usize, usize) {
    (
        BOUNDED_Q48_RECIPROCAL_BUILDS.with(Cell::get),
        BOUNDED_Q48_RECIPROCAL_QUOTIENTS.with(Cell::get),
        BOUNDED_Q48_RECIPROCAL_FALLBACKS.with(Cell::get),
    )
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

/// Non-authoritative residual evidence used only by bounded guidance probes.
/// The sign is certified against the exact Q64/Q80 residual. The magnitude
/// and q values are accelerator hints and must never be consumed as exact
/// residual data or persisted into an authoritative curve.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConcentratedGuidanceResidualEvaluation {
    positive: bool,
    magnitude_hint: u128,
    q64_hint: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConcentratedResidualAccelerator {
    positive: bool,
    magnitude_hint: u128,
    q64_hint: u128,
}

impl ConcentratedResidualEvaluation {
    const fn accelerator(self) -> ConcentratedResidualAccelerator {
        ConcentratedResidualAccelerator {
            positive: self.positive,
            magnitude_hint: self.magnitude,
            q64_hint: self.q64,
        }
    }
}

impl ConcentratedGuidanceResidualEvaluation {
    const fn accelerator(self) -> ConcentratedResidualAccelerator {
        ConcentratedResidualAccelerator {
            positive: self.positive,
            magnitude_hint: self.magnitude_hint,
            q64_hint: self.q64_hint,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConcentratedBoundedExactInProbe {
    reserve_nad: u128,
    reserve_common: u128,
    context: ConcentratedResidualContext,
    evaluation: ConcentratedGuidanceResidualEvaluation,
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

/// Opaque non-authoritative curve projection. Its inner prepared curve is
/// intentionally inaccessible outside this module, so same-invariant
/// guidance can never be converted into a persisted market checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedGuidanceCurve(ConcentratedPreparedCurve);

/// Private guidance-function fingerprint for one reserve anchor. The action
/// records which side of the checked radial/sum enclosure selected the opaque
/// guidance invariant; it never carries checkpoint authority.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ConcentratedGuidanceDAction {
    RaisedRadial,
    #[default]
    Unchanged,
    LoweredSum,
}

/// Non-authoritative exact-input projection regime. `StructuralGap` is an
/// executable token-lattice certificate between two consecutive reserve
/// atoms; it performs no invariant-residual probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcentratedGuidanceExactInMode {
    Bracket,
    StructuralGap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedGuidanceExactInQuote {
    pub(crate) amount_out_nad: u128,
    pub(crate) mode: ConcentratedGuidanceExactInMode,
}

impl ConcentratedGuidanceExactInQuote {
    const fn bracket(amount_out_nad: u128) -> Self {
        Self {
            amount_out_nad,
            mode: ConcentratedGuidanceExactInMode::Bracket,
        }
    }

    const fn structural_gap(amount_out_nad: u128) -> Self {
        Self {
            amount_out_nad,
            mode: ConcentratedGuidanceExactInMode::StructuralGap,
        }
    }
}

/// Non-authoritative exact-output projection regime. Concentrated guidance
/// consumes either the verified q=1 high after two probes or one additional
/// token-aligned false-position probe. CPMM remains analytic and consumes no
/// invariant-residual probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcentratedGuidanceExactOutMode {
    AnalyticCpmm,
    BoundedP2High,
    BoundedP3Positive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedGuidanceExactOutQuote {
    pub(crate) amount_in_nad: u128,
    pub(crate) mode: ConcentratedGuidanceExactOutMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CanonicalGuidanceAnchorSeal;

/// Operation-start canonical invariant enclosure. Construction consumes only
/// a canonical `ConcentratedPreparedCurve`; the wrapped guidance curve cannot
/// be unwrapped into a checkpoint or any other authority capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedCanonicalGuidanceAnchor {
    start: ConcentratedGuidanceCurve,
    predecessor_invariant_d: u128,
    _seal: CanonicalGuidanceAnchorSeal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcentratedInvariantSeed {
    Hint(u128),
    #[cfg(test)]
    Exact(u128),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreparedCurveInvariantSeed {
    Canonical(ConcentratedInvariantSeed),
    Guidance(u128),
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

    /// Five-probe, conservative-side guidance for an exact-input quote. This
    /// is intentionally weaker than `quote_exact_in`: it neither constructs a
    /// canonical successor invariant nor proves the adjacent output atom.
    fn quote_bounded_guidance_exact_in(
        self,
        amount_in_nad: u128,
        direction: ConcentratedSwapDirection,
        output_atom_nad: u128,
    ) -> Result<ConcentratedGuidanceExactInQuote> {
        self.quote_bounded_guidance_exact_in_with_probe_limit::<MAX_RESIDUAL_PROBES_PER_VARIABLE_LEG>(
            amount_in_nad,
            direction,
            output_atom_nad,
        )
    }

    fn quote_bounded_guidance_exact_in_with_probe_limit<const MAX_PROBES: usize>(
        self,
        amount_in_nad: u128,
        direction: ConcentratedSwapDirection,
        output_atom_nad: u128,
    ) -> Result<ConcentratedGuidanceExactInQuote> {
        require!(output_atom_nad > 0, ErrorCode::InvalidArgument);
        if amount_in_nad == 0 {
            return Ok(ConcentratedGuidanceExactInQuote::bracket(0));
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
        require_eq!(output_reserve_nad % output_atom_nad, 0, ErrorCode::BrokenInvariant);
        if self.peak_depth_nad == 0 {
            let output = self.quote_exact_in(amount_in_nad, direction)?;
            return Ok(ConcentratedGuidanceExactInQuote::bracket(
                output - output % output_atom_nad,
            ));
        }
        let geometry = self.geometry.ok_or(ErrorCode::BrokenInvariant)?;
        validate_bounded_guidance_basis(self.base_common, self.quote_common, self.invariant_d, geometry)?;
        let q48_reciprocal = BoundedQ48Reciprocal::new(self.invariant_d);
        let input_after_nad = input_reserve_nad
            .checked_add(amount_in_nad)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let input_scale = self.input_common_scale(direction)?;
        let output_scale = self.output_common_scale(direction)?;
        let input_after_common = input_scale.to_common_floor(input_after_nad)?;
        if input_after_common <= input_common {
            return Ok(ConcentratedGuidanceExactInQuote::bracket(0));
        }
        validate_bounded_common_reserves(input_after_common, output_common)?;

        // For fixed post-input x, every trade-only root lies in the structural
        // interval D-x <= y <= floor(D^2/(4*x)). P1 is the greatest final
        // output-token atom in that interval which still emits nonzero output.
        // If the current output reserve already lies below q=1, cap U there and
        // avoid computing an irrelevant wider quotient.
        let structural_low_common = self.invariant_d.saturating_sub(input_after_common).max(1);
        let four_output_common = output_common.checked_mul(4).ok_or(ErrorCode::InvariantOverflow)?;
        let structural_high_common = if ratio_lte_full_width(
            input_after_common,
            self.invariant_d,
            self.invariant_d,
            four_output_common,
        )? {
            output_common
        } else {
            let four_input_common = input_after_common.checked_mul(4).ok_or(ErrorCode::InvariantOverflow)?;
            mul_div_floor(self.invariant_d, self.invariant_d, four_input_common)?.min(output_common)
        };
        if structural_high_common < structural_low_common {
            return Ok(ConcentratedGuidanceExactInQuote::bracket(0));
        }
        let p1_raw = bounded_exact_in_structural_high_raw(
            output_scale,
            output_atom_nad,
            output_reserve_nad,
            structural_low_common,
            structural_high_common,
        )?;
        let Some(p1_raw) = p1_raw else {
            return Ok(bounded_exact_in_structural_gap_output(
                output_scale,
                output_atom_nad,
                output_reserve_nad,
                structural_low_common,
                structural_high_common,
            )?
            .map(ConcentratedGuidanceExactInQuote::structural_gap)
            .unwrap_or_else(|| ConcentratedGuidanceExactInQuote::bracket(0)));
        };
        let p1 = bounded_exact_in_probe(
            input_after_common,
            self.invariant_d,
            geometry,
            output_scale,
            p1_raw,
            q48_reciprocal.as_ref(),
        )?;
        if p1.reserve_common < structural_low_common
            || p1.reserve_common > structural_high_common
            || !p1.evaluation.positive
        {
            return Ok(ConcentratedGuidanceExactInQuote::bracket(0));
        }

        // P2-P5 maintain one raw-token-lattice bracket. Without a verified
        // nonpositive low, take a full Newton step from H. Once a low exists,
        // use checked false position. Every accelerator is rounded back onto
        // the strict executable lattice and re-evaluated with its own branch;
        // duplicate/no-interior accelerators retain the prior verified H.
        let mut verified_low = None;
        let mut verified_high = p1;
        for _ in 1..MAX_PROBES {
            let Some(probe) = bounded_exact_in_next_bracket_probe(
                input_after_common,
                self.invariant_d,
                geometry,
                output_scale,
                output_atom_nad,
                structural_low_common,
                verified_low,
                verified_high,
                q48_reciprocal.as_ref(),
            )?
            else {
                break;
            };
            if probe.evaluation.positive {
                verified_high = probe;
            } else {
                verified_low = Some(probe);
            }
        }
        bounded_exact_in_emit(output_reserve_nad, output_atom_nad, verified_high)
            .map(ConcentratedGuidanceExactInQuote::bracket)
    }

    /// Three-probe, sufficient-side guidance for an exact-output request. P1
    /// checks the actual input reserve, P2 proves one token-aligned q=1 high,
    /// and P3 optionally tightens that bracket with one token-aligned false-
    /// position probe. Only an exactly re-probed positive point is emitted.
    fn quote_bounded_guidance_exact_out_input(
        self,
        amount_out_nad: u128,
        direction: ConcentratedSwapDirection,
        input_atom_nad: u128,
    ) -> Result<ConcentratedGuidanceExactOutQuote> {
        require!(input_atom_nad > 0, ErrorCode::InvalidArgument);
        if amount_out_nad == 0 {
            return Ok(ConcentratedGuidanceExactOutQuote {
                amount_in_nad: 0,
                mode: ConcentratedGuidanceExactOutMode::AnalyticCpmm,
            });
        }
        if self.peak_depth_nad == 0 {
            let input = self.quote_exact_out_input_bracket(amount_out_nad, direction)?.1;
            return Ok(ConcentratedGuidanceExactOutQuote {
                amount_in_nad: bounded_exact_in_align_raw_up(input, input_atom_nad)?,
                mode: ConcentratedGuidanceExactOutMode::AnalyticCpmm,
            });
        }
        let geometry = self.geometry.ok_or(ErrorCode::BrokenInvariant)?;
        validate_bounded_guidance_basis(self.base_common, self.quote_common, self.invariant_d, geometry)?;
        let q48_reciprocal = BoundedQ48Reciprocal::new(self.invariant_d);
        let (input_reserve_nad, output_reserve_nad, input_common, _output_common) = match direction {
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
        require_eq!(input_reserve_nad % input_atom_nad, 0, ErrorCode::BrokenInvariant);
        require!(
            amount_out_nad > 0 && amount_out_nad < output_reserve_nad,
            ErrorCode::InsufficientLiquidity
        );
        let input_scale = self.input_common_scale(direction)?;
        let output_scale = self.output_common_scale(direction)?;
        let output_after_nad = output_reserve_nad
            .checked_sub(amount_out_nad)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        let output_after_common = output_scale.to_common_floor(output_after_nad)?;
        require!(output_after_common > 0, ErrorCode::InsufficientLiquidity);

        // The low probe is the actual current input reserve. It is allowed to
        // sit below x+y=D: that is an ordinary off-curve insufficient point,
        // not an invalid guidance basis.
        let low_context = bounded_guidance_residual_context(output_after_common, input_common, geometry)?;
        let low_evaluation = bounded_guidance_residual_evaluation_with_context(
            output_after_common,
            input_common,
            self.invariant_d,
            Some(geometry),
            Some(low_context),
            q48_reciprocal.as_ref(),
        )?;
        if low_evaluation.positive {
            // The compact I>B settlement has no economically valid zero-input
            // branch: it must debit a positive retained target amount. A
            // positive current-input residual under an opaque scaled D is
            // therefore a guidance miss, never a free settlement estimate.
            return err!(ErrorCode::InsufficientLiquidity);
        }

        // q=4*x*y/D^2=1 is residual-positive for every concentrated branch.
        // Combine it with the structural sum floor, then round the complete
        // reserve up to the executable input-token lattice before P2.
        let four_fixed = output_after_common.checked_mul(4).ok_or(ErrorCode::InvariantOverflow)?;
        let q_one_input_common = mul_div_ceil_u128(self.invariant_d, self.invariant_d, four_fixed)?;
        let structural_input_common = self.invariant_d.saturating_sub(output_after_common);
        let sufficient_seed_common = q_one_input_common
            .max(structural_input_common)
            .max(input_common.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?)
            .min(MAX_COMMON_RESERVE);
        require!(sufficient_seed_common > input_common, ErrorCode::InsufficientLiquidity);
        let sufficient_input_after_nad =
            bounded_exact_in_align_raw_up(input_scale.common_to_raw_ceil(sufficient_seed_common)?, input_atom_nad)?;
        require_gte!(
            sufficient_input_after_nad,
            input_reserve_nad,
            ErrorCode::BrokenInvariant
        );
        let rounded_common = input_scale.to_common_floor(sufficient_input_after_nad)?;
        require!(
            rounded_common >= sufficient_seed_common && rounded_common <= MAX_COMMON_RESERVE,
            ErrorCode::InvariantOverflow
        );
        let rounded_context = bounded_guidance_residual_context(output_after_common, rounded_common, geometry)?;
        let rounded_evaluation = bounded_guidance_residual_evaluation_with_context(
            output_after_common,
            rounded_common,
            self.invariant_d,
            Some(geometry),
            Some(rounded_context),
            q48_reciprocal.as_ref(),
        )?;
        require!(rounded_evaluation.positive, ErrorCode::InsufficientLiquidity);

        // P3 is a single checked false-position accelerator. Failure to form
        // a strict executable-lattice seed retains the already-proven P2 high;
        // once a seed is emitted, its context and residual evaluation are
        // authoritative for this guidance result and any error propagates.
        let false_position_after_nad = (|| -> Option<u128> {
            let magnitude_sum = low_evaluation
                .magnitude_hint
                .checked_add(rounded_evaluation.magnitude_hint)
                .filter(|sum| *sum > 0)?;
            let width = rounded_common.checked_sub(input_common)?;
            let offset = mul_div_floor(width, low_evaluation.magnitude_hint, magnitude_sum).ok()?;
            let candidate_common = input_common.checked_add(offset)?;
            if candidate_common <= input_common || candidate_common >= rounded_common {
                return None;
            }
            let candidate_after_nad =
                bounded_exact_in_align_raw_up(input_scale.common_to_raw_ceil(candidate_common).ok()?, input_atom_nad)
                    .ok()?;
            if candidate_after_nad <= input_reserve_nad || candidate_after_nad >= sufficient_input_after_nad {
                return None;
            }
            let replay_common = input_scale.to_common_floor(candidate_after_nad).ok()?;
            (replay_common > input_common && replay_common < rounded_common).then_some(candidate_after_nad)
        })();
        let (selected_input_after_nad, mode) = if let Some(false_position_after_nad) = false_position_after_nad {
            let false_position_common = input_scale.to_common_floor(false_position_after_nad)?;
            let false_position_context =
                bounded_guidance_residual_context(output_after_common, false_position_common, geometry)?;
            let false_position_evaluation = bounded_guidance_residual_evaluation_with_context(
                output_after_common,
                false_position_common,
                self.invariant_d,
                Some(geometry),
                Some(false_position_context),
                q48_reciprocal.as_ref(),
            )?;
            if false_position_evaluation.positive {
                (
                    false_position_after_nad,
                    ConcentratedGuidanceExactOutMode::BoundedP3Positive,
                )
            } else {
                (
                    sufficient_input_after_nad,
                    ConcentratedGuidanceExactOutMode::BoundedP2High,
                )
            }
        } else {
            (
                sufficient_input_after_nad,
                ConcentratedGuidanceExactOutMode::BoundedP2High,
            )
        };
        let selected_input_nad = selected_input_after_nad
            .checked_sub(input_reserve_nad)
            .filter(|input| *input > 0)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        require_eq!(selected_input_nad % input_atom_nad, 0, ErrorCode::BrokenInvariant);
        require_eq!(
            input_reserve_nad
                .checked_add(selected_input_nad)
                .ok_or(ErrorCode::InvariantOverflow)?,
            selected_input_after_nad,
            ErrorCode::BrokenInvariant
        );
        Ok(ConcentratedGuidanceExactOutQuote {
            amount_in_nad: selected_input_nad,
            mode,
        })
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
            Some(PreparedCurveInvariantSeed::Canonical(invariant_seed)),
        )
    }

    /// Erases a canonical operation-start curve into a guidance-only adjacent
    /// invariant enclosure. Canonical preparation defines `D0` as the upper
    /// endpoint and proves `D0 - 1` as its predecessor; no derived guidance
    /// curve can call this constructor because its prepared curve is private.
    pub(crate) fn seal_canonical_guidance_anchor(self) -> Result<ConcentratedCanonicalGuidanceAnchor> {
        let predecessor_invariant_d = self.invariant_d.checked_sub(1).ok_or(ErrorCode::BrokenInvariant)?;
        let start = self.prepare_guidance_successor_with_invariant(
            self.base_reserve_nad,
            self.quote_reserve_nad,
            self.invariant_d,
        )?;
        Ok(ConcentratedCanonicalGuidanceAnchor {
            start,
            predecessor_invariant_d,
            _seal: CanonicalGuidanceAnchorSeal,
        })
    }

    /// Guidance-only endpoint at the already-proven start invariant. Raw
    /// output flooring can move the canonical endpoint invariant by an atom;
    /// callers must never persist this projection or use it as the executable
    /// quote. It exists only to guide a later fully authoritative solve.
    pub(crate) fn prepare_guidance_successor(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
    ) -> Result<ConcentratedGuidanceCurve> {
        self.prepare_guidance_successor_with_invariant(base_reserve_nad, quote_reserve_nad, self.invariant_d)
    }

    pub(crate) fn prepare_guidance_successor_with_invariant(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
        invariant_d: u128,
    ) -> Result<ConcentratedGuidanceCurve> {
        prepare_curve_internal(
            base_reserve_nad,
            quote_reserve_nad,
            self.center_price_nad,
            self.peak_depth_nad,
            self.fade_scale_nad,
            self.geometry_cache(),
            Some(PreparedCurveInvariantSeed::Guidance(invariant_d)),
        )
        .map(ConcentratedGuidanceCurve)
    }
}

fn validate_bounded_guidance_basis(
    base_common: u128,
    quote_common: u128,
    invariant_d: u128,
    geometry: ConcentratedC1Geometry,
) -> Result<()> {
    validate_bounded_common_reserves(base_common, quote_common)?;
    require!(invariant_d > 0, ErrorCode::InvalidArgument);
    require_gte!(
        base_common
            .checked_add(quote_common)
            .ok_or(ErrorCode::InvariantOverflow)?,
        invariant_d,
        ErrorCode::InvariantOverflow
    );
    // Full-width `D^2 >= 4*x*y`, expressed as x/D <= D/(4*y)
    // so neither cross-product is materialized in u128.
    let four_quote = quote_common.checked_mul(4).ok_or(ErrorCode::InvariantOverflow)?;
    require!(
        ratio_lte_full_width(base_common, invariant_d, invariant_d, four_quote)?,
        ErrorCode::InvariantOverflow
    );
    let context = ConcentratedResidualContext::derive(geometry, base_common, quote_common)?;
    if context.branch == ConcentratedHybridBranch::Inner {
        require!(
            base_common >= MIN_INNER_COMMON_RESERVE && quote_common >= MIN_INNER_COMMON_RESERVE,
            ErrorCode::InsufficientLiquidity
        );
    }
    Ok(())
}

/// Exact `ceil(sqrt(4*x*y))` without materializing the potentially wider
/// product. `2*floor(sqrt(x*y))` is at most two atoms below the answer; each
/// candidate and the final predecessor are certified with a full-width ratio
/// comparison.
fn concentrated_guidance_radial_floor_ceil(x: u128, y: u128) -> Result<u128> {
    validate_bounded_common_reserves(x, y)?;
    let four_y = y.checked_mul(4).ok_or(ErrorCode::InvariantOverflow)?;
    let floor = geometric_mean_floor(x, y)?
        .checked_mul(2)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let covers_product = |candidate: u128| ratio_lte_full_width(x, candidate, candidate, four_y);
    let candidate = if covers_product(floor)? {
        floor
    } else {
        let successor = floor.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?;
        if covers_product(successor)? {
            successor
        } else {
            successor.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?
        }
    };
    require!(covers_product(candidate)?, ErrorCode::InvariantOverflow);
    let predecessor = candidate.checked_sub(1).ok_or(ErrorCode::InvariantOverflow)?;
    require!(!covers_product(predecessor)?, ErrorCode::BrokenInvariant);
    Ok(candidate)
}

const fn concentrated_guidance_d_action(
    candidate_invariant_d: u128,
    guidance_invariant_d: u128,
    radial_floor_d: u128,
    reserve_sum_d: u128,
) -> ConcentratedGuidanceDAction {
    if guidance_invariant_d > candidate_invariant_d || candidate_invariant_d <= radial_floor_d {
        ConcentratedGuidanceDAction::RaisedRadial
    } else if guidance_invariant_d < candidate_invariant_d || candidate_invariant_d >= reserve_sum_d {
        ConcentratedGuidanceDAction::LoweredSum
    } else {
        ConcentratedGuidanceDAction::Unchanged
    }
}

fn bounded_exact_in_align_raw_up(value: u128, atom_nad: u128) -> Result<u128> {
    require!(atom_nad > 0, ErrorCode::InvalidArgument);
    let remainder = value % atom_nad;
    if remainder == 0 {
        Ok(value)
    } else {
        value
            .checked_add(atom_nad - remainder)
            .ok_or_else(|| ErrorCode::InvariantOverflow.into())
    }
}

/// Greatest output-token-lattice reserve inside the structural interval and
/// strictly below the current reserve. `floor(raw*scale) <= U` is inverted as
/// `raw < ceil((U+1)/scale)`, retaining every raw atom which maps to U.
fn bounded_exact_in_structural_high_raw(
    output_scale: ConcentratedCommonScale,
    output_atom_nad: u128,
    output_reserve_nad: u128,
    structural_low_common: u128,
    structural_high_common: u128,
) -> Result<Option<u128>> {
    require!(output_atom_nad > 0, ErrorCode::InvalidArgument);
    require_eq!(output_reserve_nad % output_atom_nad, 0, ErrorCode::BrokenInvariant);
    let Some(nonzero_output_cap) = output_reserve_nad.checked_sub(output_atom_nad) else {
        return Ok(None);
    };
    let first_raw_above_high = output_scale.common_to_raw_ceil(
        structural_high_common
            .checked_add(1)
            .ok_or(ErrorCode::InvariantOverflow)?,
    )?;
    let Some(structural_raw_cap) = first_raw_above_high.checked_sub(1) else {
        return Ok(None);
    };
    let raw_cap = structural_raw_cap.min(nonzero_output_cap);
    let reserve_nad = raw_cap - raw_cap % output_atom_nad;
    if reserve_nad == 0 {
        return Ok(None);
    }
    let reserve_common = output_scale.to_common_floor(reserve_nad)?;
    if reserve_common < structural_low_common || reserve_common > structural_high_common {
        return Ok(None);
    }
    let next_raw = reserve_nad
        .checked_add(output_atom_nad)
        .ok_or(ErrorCode::InvariantOverflow)?;
    if next_raw < output_reserve_nad {
        require!(
            output_scale.to_common_floor(next_raw)? > structural_high_common,
            ErrorCode::BrokenInvariant
        );
    }
    Ok(Some(reserve_nad))
}

/// Certifies that the same-D structural interval contains no executable
/// output-token reserve atom. `r_lo` and `r_hi` are consecutive lattice atoms
/// immediately below the structural low and strictly above the structural
/// high. Emitting `R-r_hi` is conservative and consumes no residual probe.
fn bounded_exact_in_structural_gap_output(
    output_scale: ConcentratedCommonScale,
    output_atom_nad: u128,
    output_reserve_nad: u128,
    structural_low_common: u128,
    structural_high_common: u128,
) -> Result<Option<u128>> {
    require!(output_atom_nad > 0, ErrorCode::InvalidArgument);
    require_eq!(output_reserve_nad % output_atom_nad, 0, ErrorCode::BrokenInvariant);
    let raw_above_high = output_scale.common_to_raw_ceil(
        structural_high_common
            .checked_add(1)
            .ok_or(ErrorCode::InvariantOverflow)?,
    )?;
    let r_hi = bounded_exact_in_align_raw_up(raw_above_high, output_atom_nad)?;
    let Some(maximum_emitting_reserve) = output_reserve_nad.checked_sub(output_atom_nad) else {
        return Ok(None);
    };
    if r_hi < output_atom_nad || r_hi > maximum_emitting_reserve {
        return Ok(None);
    }
    let y_hi = output_scale.to_common_floor(r_hi)?;
    require!(y_hi > structural_high_common, ErrorCode::BrokenInvariant);
    let r_lo = r_hi.checked_sub(output_atom_nad).ok_or(ErrorCode::BrokenInvariant)?;
    let y_lo = output_scale.to_common_floor(r_lo)?;
    if y_lo >= structural_low_common {
        return Ok(None);
    }
    let output = output_reserve_nad
        .checked_sub(r_hi)
        .filter(|output| *output >= output_atom_nad && *output % output_atom_nad == 0)
        .ok_or(ErrorCode::OutputAmountOverflow)?;
    require_eq!(
        output_reserve_nad
            .checked_sub(output)
            .ok_or(ErrorCode::OutputAmountOverflow)?,
        r_hi,
        ErrorCode::BrokenInvariant
    );
    Ok(Some(output))
}

fn bounded_exact_in_probe(
    fixed_common: u128,
    invariant_d: u128,
    geometry: ConcentratedC1Geometry,
    output_scale: ConcentratedCommonScale,
    reserve_nad: u128,
    reciprocal: Option<&BoundedQ48Reciprocal>,
) -> Result<ConcentratedBoundedExactInProbe> {
    let reserve_common = output_scale.to_common_floor(reserve_nad)?;
    let context = bounded_guidance_residual_context(fixed_common, reserve_common, geometry)?;
    let evaluation = bounded_guidance_residual_evaluation_with_context(
        fixed_common,
        reserve_common,
        invariant_d,
        Some(geometry),
        Some(context),
        reciprocal,
    )?;
    Ok(ConcentratedBoundedExactInProbe {
        reserve_nad,
        reserve_common,
        context,
        evaluation,
    })
}

fn bounded_exact_in_seeded_interior_probe(
    fixed_common: u128,
    invariant_d: u128,
    geometry: ConcentratedC1Geometry,
    output_scale: ConcentratedCommonScale,
    output_atom_nad: u128,
    low_common: u128,
    low_raw: Option<u128>,
    verified_high: ConcentratedBoundedExactInProbe,
    seed: Option<BoundedExactInInteriorSeed>,
    reciprocal: Option<&BoundedQ48Reciprocal>,
) -> Result<Option<ConcentratedBoundedExactInProbe>> {
    let Some(reserve_nad) =
        bounded_exact_in_interior_raw(output_scale, output_atom_nad, low_common, low_raw, verified_high, seed)?
    else {
        return Ok(None);
    };
    bounded_exact_in_probe(
        fixed_common,
        invariant_d,
        geometry,
        output_scale,
        reserve_nad,
        reciprocal,
    )
    .map(Some)
}

/// One safeguarded executable-lattice continuation of a verified sign bracket.
/// Newton is used while only H is known; a checked false-position seed is used
/// once a nonpositive L exists. Arithmetic failure in either accelerator drops
/// only the seed, so the exact atom midpoint remains the deterministic fallback.
fn bounded_exact_in_next_bracket_probe(
    fixed_common: u128,
    invariant_d: u128,
    geometry: ConcentratedC1Geometry,
    output_scale: ConcentratedCommonScale,
    output_atom_nad: u128,
    structural_low_common: u128,
    verified_low: Option<ConcentratedBoundedExactInProbe>,
    verified_high: ConcentratedBoundedExactInProbe,
    reciprocal: Option<&BoundedQ48Reciprocal>,
) -> Result<Option<ConcentratedBoundedExactInProbe>> {
    require!(verified_high.evaluation.positive, ErrorCode::BrokenInvariant);
    let (low_common, low_raw, proposed_common) = if let Some(low) = verified_low {
        require!(!low.evaluation.positive, ErrorCode::BrokenInvariant);
        require!(
            low.reserve_common < verified_high.reserve_common,
            ErrorCode::BrokenInvariant
        );
        require!(low.reserve_nad < verified_high.reserve_nad, ErrorCode::BrokenInvariant);
        (
            low.reserve_common,
            Some(low.reserve_nad),
            bounded_exact_in_false_position(low, verified_high),
        )
    } else {
        let proposed = bounded_guidance_variable_reserve_newton_probe(
            fixed_common,
            verified_high.reserve_common,
            invariant_d,
            verified_high.evaluation,
            Some(geometry),
            Some(verified_high.context),
        );
        (
            structural_low_common,
            None,
            proposed.map(BoundedExactInInteriorSeed::Common),
        )
    };
    bounded_exact_in_seeded_interior_probe(
        fixed_common,
        invariant_d,
        geometry,
        output_scale,
        output_atom_nad,
        low_common,
        low_raw,
        verified_high,
        proposed_common,
        reciprocal,
    )
}

/// Maps a continuous accelerator to a strict raw-token-lattice bracket. If the
/// proposed point leaves the bracket or rounds onto an endpoint, use the exact
/// atom midpoint. No probe is returned when the lattice has no interior atom.
fn bounded_exact_in_interior_raw(
    output_scale: ConcentratedCommonScale,
    output_atom_nad: u128,
    low_common: u128,
    low_raw: Option<u128>,
    verified_high: ConcentratedBoundedExactInProbe,
    seed: Option<BoundedExactInInteriorSeed>,
) -> Result<Option<u128>> {
    require!(output_atom_nad > 0, ErrorCode::InvalidArgument);
    require_eq!(
        verified_high.reserve_nad % output_atom_nad,
        0,
        ErrorCode::BrokenInvariant
    );
    if let Some(low_raw) = low_raw {
        require_eq!(low_raw % output_atom_nad, 0, ErrorCode::BrokenInvariant);
        require!(low_raw < verified_high.reserve_nad, ErrorCode::BrokenInvariant);
    }
    require!(low_common < verified_high.reserve_common, ErrorCode::BrokenInvariant);

    let first_from_common = bounded_exact_in_align_raw_up(
        output_scale.common_to_raw_ceil(low_common.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?)?,
        output_atom_nad,
    )?;
    let first_raw = match low_raw {
        Some(raw) => first_from_common.max(raw.checked_add(output_atom_nad).ok_or(ErrorCode::InvariantOverflow)?),
        None => first_from_common,
    };
    let raw_before_high_common = output_scale
        .common_to_raw_ceil(verified_high.reserve_common)?
        .checked_sub(1)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let last_from_common = raw_before_high_common - raw_before_high_common % output_atom_nad;
    let last_raw = verified_high
        .reserve_nad
        .checked_sub(output_atom_nad)
        .ok_or(ErrorCode::InvariantOverflow)?
        .min(last_from_common);
    if first_raw > last_raw {
        return Ok(None);
    }

    let proposed_raw = match seed {
        Some(BoundedExactInInteriorSeed::FirstRawInterior) => {
            require!(low_raw.is_some(), ErrorCode::BrokenInvariant);
            #[cfg(test)]
            {
                BOUNDED_EXACT_IN_FIRST_RAW_LOW.with(|reserve| reserve.set(low_raw.unwrap_or_default()));
                BOUNDED_EXACT_IN_FIRST_RAW_SELECTED.with(|reserve| reserve.set(first_raw));
            }
            Some(first_raw)
        }
        Some(BoundedExactInInteriorSeed::Common(candidate)) => (candidate > low_common
            && candidate < verified_high.reserve_common)
            .then_some(candidate)
            .and_then(|candidate| output_scale.common_to_raw_ceil(candidate).ok())
            .and_then(|raw| bounded_exact_in_align_raw_up(raw, output_atom_nad).ok())
            .filter(|raw| *raw >= first_raw && *raw <= last_raw),
        None => None,
    };
    let reserve_nad = proposed_raw.unwrap_or_else(|| {
        let atom_span = (last_raw - first_raw) / output_atom_nad;
        first_raw + (atom_span / 2) * output_atom_nad
    });
    let reserve_common = output_scale.to_common_floor(reserve_nad)?;
    require!(
        reserve_common > low_common && reserve_common < verified_high.reserve_common,
        ErrorCode::BrokenInvariant
    );
    if let Some(low_raw) = low_raw {
        require!(
            low_raw < reserve_nad && reserve_nad < verified_high.reserve_nad,
            ErrorCode::BrokenInvariant
        );
    } else {
        require!(reserve_nad < verified_high.reserve_nad, ErrorCode::BrokenInvariant);
    }
    Ok(Some(reserve_nad))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundedExactInInteriorSeed {
    Common(u128),
    FirstRawInterior,
}

fn bounded_exact_in_false_position(
    verified_low: ConcentratedBoundedExactInProbe,
    verified_high: ConcentratedBoundedExactInProbe,
) -> Option<BoundedExactInInteriorSeed> {
    if verified_low.evaluation.positive
        || !verified_high.evaluation.positive
        || verified_low.reserve_common >= verified_high.reserve_common
    {
        return None;
    }
    (|| -> Result<BoundedExactInInteriorSeed> {
        let magnitude_sum = verified_low
            .evaluation
            .magnitude_hint
            .checked_add(verified_high.evaluation.magnitude_hint)
            .filter(|sum| *sum > 0)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let width = verified_high.reserve_common - verified_low.reserve_common;
        let offset = mul_div_floor(width, verified_low.evaluation.magnitude_hint, magnitude_sum)?;
        if offset == 0 {
            #[cfg(test)]
            {
                BOUNDED_EXACT_IN_FIRST_RAW_FALLBACKS.with(|count| count.set(count.get().saturating_add(1)));
                let ordinal = RESIDUAL_EVALUATIONS.with(|count| count.get().saturating_add(1));
                BOUNDED_EXACT_IN_FIRST_RAW_PROBE_ORDINAL.with(|probe| probe.set(ordinal));
            }
            return Ok(BoundedExactInInteriorSeed::FirstRawInterior);
        }
        verified_low
            .reserve_common
            .checked_add(offset)
            .filter(|candidate| *candidate > verified_low.reserve_common && *candidate < verified_high.reserve_common)
            .map(BoundedExactInInteriorSeed::Common)
            .ok_or_else(|| ErrorCode::InvariantOverflow.into())
    })()
    .ok()
}

fn bounded_exact_in_emit(
    output_reserve_nad: u128,
    output_atom_nad: u128,
    verified_high: ConcentratedBoundedExactInProbe,
) -> Result<u128> {
    require!(output_atom_nad > 0, ErrorCode::InvalidArgument);
    require!(verified_high.evaluation.positive, ErrorCode::BrokenInvariant);
    require_eq!(
        verified_high.reserve_nad % output_atom_nad,
        0,
        ErrorCode::BrokenInvariant
    );
    let output = output_reserve_nad
        .checked_sub(verified_high.reserve_nad)
        .filter(|output| *output >= output_atom_nad && *output % output_atom_nad == 0)
        .ok_or(ErrorCode::OutputAmountOverflow)?;
    Ok(output)
}

fn bounded_guidance_residual_context(
    fixed: u128,
    variable: u128,
    geometry: ConcentratedC1Geometry,
) -> Result<ConcentratedResidualContext> {
    validate_bounded_common_reserves(fixed, variable)?;
    let context = ConcentratedResidualContext::derive(geometry, fixed, variable)?;
    if context.branch == ConcentratedHybridBranch::Inner {
        require!(
            fixed >= MIN_INNER_COMMON_RESERVE && variable >= MIN_INNER_COMMON_RESERVE,
            ErrorCode::InsufficientLiquidity
        );
    }
    Ok(context)
}

impl ConcentratedCanonicalGuidanceAnchor {
    pub(crate) const fn guidance(&self) -> &ConcentratedGuidanceCurve {
        &self.start
    }

    /// Projects the operation-start adjacent canonical bracket onto one actual
    /// candidate reserve pair. The lower homothetic bound is deliberately
    /// loose; it makes no successor-sign claim. The selected same-D guidance
    /// is the independently supply-scaled D clamped into the reserve-ratio,
    /// radial, and sum enclosure.
    pub(crate) fn prepare_candidate_guidance(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
        from_ylp_supply: u64,
        to_ylp_supply: u64,
    ) -> Result<ConcentratedGuidanceCurve> {
        self.prepare_candidate_guidance_with_action(base_reserve_nad, quote_reserve_nad, from_ylp_supply, to_ylp_supply)
            .map(|(guidance, _)| guidance)
    }

    pub(crate) fn prepare_candidate_guidance_with_action(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
        from_ylp_supply: u64,
        to_ylp_supply: u64,
    ) -> Result<(ConcentratedGuidanceCurve, ConcentratedGuidanceDAction)> {
        require!(from_ylp_supply > 0 && to_ylp_supply > 0, ErrorCode::SupplyUnderflow);
        if self.start.0.peak_depth_nad == 0 {
            let invariant_d =
                mul_div_u128(self.start.0.invariant_d, to_ylp_supply as u128, from_ylp_supply as u128)?.max(1);
            return Ok((
                self.start.prepare_guidance_successor_with_invariant(
                    base_reserve_nad,
                    quote_reserve_nad,
                    invariant_d,
                )?,
                ConcentratedGuidanceDAction::Unchanged,
            ));
        }

        let anchor_base_common = self.start.0.base_common;
        let anchor_quote_common = self.start.0.quote_common;
        let (base_common, quote_common) =
            normalize_reserves(base_reserve_nad, quote_reserve_nad, self.start.0.center_price_nad)?;
        let lower_homogeneous = mul_div_u128(self.predecessor_invariant_d, base_common, anchor_base_common)?.min(
            mul_div_u128(self.predecessor_invariant_d, quote_common, anchor_quote_common)?,
        );
        let upper_homogeneous = mul_div_ceil_u128(self.start.0.invariant_d, base_common, anchor_base_common)?.max(
            mul_div_ceil_u128(self.start.0.invariant_d, quote_common, anchor_quote_common)?,
        );
        let radial_floor = concentrated_guidance_radial_floor_ceil(base_common, quote_common)?;
        let reserve_sum = base_common
            .checked_add(quote_common)
            .ok_or(ErrorCode::InvariantOverflow)?;
        require!(radial_floor <= reserve_sum, ErrorCode::BrokenInvariant);
        let lower = lower_homogeneous.max(radial_floor);
        let upper = upper_homogeneous.min(reserve_sum);
        require!(lower <= upper, ErrorCode::InsufficientLiquidity);
        let supply_scaled = mul_div_u128(self.start.0.invariant_d, to_ylp_supply as u128, from_ylp_supply as u128)?;
        let invariant_d = supply_scaled.clamp(lower, upper);
        let action = concentrated_guidance_d_action(supply_scaled, invariant_d, radial_floor, reserve_sum);
        let guidance =
            self.start
                .prepare_guidance_successor_with_invariant(base_reserve_nad, quote_reserve_nad, invariant_d)?;
        validate_bounded_guidance_basis(
            guidance.0.base_common,
            guidance.0.quote_common,
            guidance.0.invariant_d,
            guidance.0.geometry.ok_or(ErrorCode::BrokenInvariant)?,
        )?;
        Ok((guidance, action))
    }
}

impl ConcentratedGuidanceCurve {
    pub(crate) const fn is_concentrated(self) -> bool {
        self.0.peak_depth_nad > 0
    }

    pub(crate) const fn invariant_d(self) -> u128 {
        self.0.invariant_d
    }

    pub(crate) const fn base_reserve_nad(self) -> u128 {
        self.0.base_reserve_nad
    }

    pub(crate) const fn quote_reserve_nad(self) -> u128 {
        self.0.quote_reserve_nad
    }

    pub(crate) const fn common_numeraire(self) -> ConcentratedCommonNumeraire {
        self.0.common_numeraire
    }

    pub(crate) fn marginal_price_nad(self) -> Result<u128> {
        self.0.marginal_price_nad()
    }

    pub(crate) fn evaluation(self) -> Result<ConcentratedEvaluation> {
        self.0.evaluation()
    }

    pub(crate) fn quote_exact_in(self, amount_in_nad: u128, direction: ConcentratedSwapDirection) -> Result<u128> {
        self.0.quote_exact_in(amount_in_nad, direction)
    }

    /// Bounded, non-authoritative exact-in predictor used only by the compact
    /// hLP planner differential.  Unlike the canonical quote, this performs
    /// at most five residual evaluations and returns an output only from a
    /// residual-positive (conservative remaining-reserve) point.  It carries
    /// no adjacent-atom proof and can never be converted into a checkpoint.
    pub(crate) fn quote_bounded_exact_in(
        self,
        amount_in_nad: u128,
        direction: ConcentratedSwapDirection,
        output_atom_nad: u128,
    ) -> Result<u128> {
        self.0
            .quote_bounded_guidance_exact_in(amount_in_nad, direction, output_atom_nad)
            .map(|quote| quote.amount_out_nad)
    }

    pub(crate) fn quote_bounded_exact_in_with_mode(
        self,
        amount_in_nad: u128,
        direction: ConcentratedSwapDirection,
        output_atom_nad: u128,
    ) -> Result<ConcentratedGuidanceExactInQuote> {
        self.0
            .quote_bounded_guidance_exact_in(amount_in_nad, direction, output_atom_nad)
    }

    #[cfg(test)]
    pub(crate) fn quote_bounded_exact_in_with_four_probes(
        self,
        amount_in_nad: u128,
        direction: ConcentratedSwapDirection,
        output_atom_nad: u128,
    ) -> Result<ConcentratedGuidanceExactInQuote> {
        self.0
            .quote_bounded_guidance_exact_in_with_probe_limit::<4>(amount_in_nad, direction, output_atom_nad)
    }

    /// Bounded, non-authoritative exact-out predictor. The complete input
    /// reserve is rounded to the caller's executable token lattice before
    /// every emitted probe. Concentrated execution consumes at most three
    /// residual evaluations; CPMM remains analytic.
    pub(crate) fn quote_bounded_exact_out_input(
        self,
        amount_out_nad: u128,
        direction: ConcentratedSwapDirection,
        input_atom_nad: u128,
    ) -> Result<ConcentratedGuidanceExactOutQuote> {
        self.0
            .quote_bounded_guidance_exact_out_input(amount_out_nad, direction, input_atom_nad)
    }

    /// Rare liveness backstop for bounded exact-out guidance. Canonical
    /// preparation and its adjacent exact-out bracket remain sealed inside
    /// this scalar-only method; callers receive neither a prepared curve nor a
    /// checkpoint capability.
    pub(crate) fn quote_hint_successor_exact_out_input_upper(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
        amount_out_nad: u128,
        direction: ConcentratedSwapDirection,
        input_atom_nad: u128,
    ) -> Result<u128> {
        require!(input_atom_nad > 0, ErrorCode::InvalidArgument);
        let prepared = self.prepare_hint_successor(base_reserve_nad, quote_reserve_nad)?;
        let input = prepared.0.quote_exact_out_input_bracket(amount_out_nad, direction)?.1;
        bounded_exact_in_align_raw_up(input, input_atom_nad)
    }

    /// Exact scalar reference for compact-planner differential tests.  The
    /// result remains guidance-only because this type cannot expose the inner
    /// prepared curve or construct a `CurveCheckpoint`.
    #[cfg(test)]
    pub(crate) fn quote_exact_out_input_bracket(
        self,
        amount_out_nad: u128,
        direction: ConcentratedSwapDirection,
    ) -> Result<(u128, u128)> {
        self.0.quote_exact_out_input_bracket(amount_out_nad, direction)
    }

    pub(crate) fn prepare_guidance_successor(self, base_reserve_nad: u128, quote_reserve_nad: u128) -> Result<Self> {
        self.prepare_guidance_successor_with_invariant(base_reserve_nad, quote_reserve_nad, self.0.invariant_d)
    }

    pub(crate) fn prepare_guidance_successor_with_invariant(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
        invariant_d: u128,
    ) -> Result<Self> {
        self.0
            .prepare_guidance_successor_with_invariant(base_reserve_nad, quote_reserve_nad, invariant_d)
    }

    /// Reuses a homogeneous, supply-scaled invariant only as opaque planner
    /// guidance. Raw reserve rounding can leave the scaled value one atom below
    /// the radial domain floor, so raise it to `ceil(sqrt(4*x*y))`, prove the
    /// complete guidance domain again, and never expose a canonical curve or
    /// checkpoint capability.
    pub(crate) fn prepare_supply_scaled_guidance_successor(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
        from_ylp_supply: u64,
        to_ylp_supply: u64,
    ) -> Result<Self> {
        self.prepare_supply_scaled_guidance_successor_with_action(
            base_reserve_nad,
            quote_reserve_nad,
            from_ylp_supply,
            to_ylp_supply,
        )
        .map(|(guidance, _)| guidance)
    }

    pub(crate) fn prepare_supply_scaled_guidance_successor_with_action(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
        from_ylp_supply: u64,
        to_ylp_supply: u64,
    ) -> Result<(Self, ConcentratedGuidanceDAction)> {
        require!(from_ylp_supply > 0 && to_ylp_supply > 0, ErrorCode::SupplyUnderflow);
        let scaled_invariant_d = mul_div_u128(self.0.invariant_d, to_ylp_supply as u128, from_ylp_supply as u128)?;
        self.prepare_enclosed_guidance_successor(base_reserve_nad, quote_reserve_nad, scaled_invariant_d)
    }

    /// Retained surcharge changes only one reserve after the trade endpoint.
    /// Re-establish the radial floor from those final, executable raw reserves
    /// while preserving the guidance-only capability boundary.
    pub(crate) fn prepare_locally_floored_guidance_successor(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
    ) -> Result<Self> {
        self.prepare_locally_floored_guidance_successor_with_action(base_reserve_nad, quote_reserve_nad)
            .map(|(guidance, _)| guidance)
    }

    pub(crate) fn prepare_locally_floored_guidance_successor_with_action(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
    ) -> Result<(Self, ConcentratedGuidanceDAction)> {
        self.prepare_enclosed_guidance_successor(base_reserve_nad, quote_reserve_nad, self.0.invariant_d)
    }

    fn prepare_enclosed_guidance_successor(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
        candidate_invariant_d: u128,
    ) -> Result<(Self, ConcentratedGuidanceDAction)> {
        // CPMM endpoint marks are analytic and do not consume concentrated
        // geometry or its radial-domain proof. Preserve the opaque guidance D
        // solely as bookkeeping; there is no geometry to revalidate here.
        if !self.is_concentrated() {
            return Ok((
                self.prepare_guidance_successor_with_invariant(
                    base_reserve_nad,
                    quote_reserve_nad,
                    candidate_invariant_d,
                )?,
                ConcentratedGuidanceDAction::Unchanged,
            ));
        }
        let (base_common, quote_common) =
            normalize_reserves(base_reserve_nad, quote_reserve_nad, self.0.center_price_nad)?;
        let radial_floor_d = concentrated_guidance_radial_floor_ceil(base_common, quote_common)?;
        let reserve_sum_d = base_common
            .checked_add(quote_common)
            .ok_or(ErrorCode::InvariantOverflow)?;
        require!(radial_floor_d <= reserve_sum_d, ErrorCode::BrokenInvariant);
        let guidance_invariant_d = candidate_invariant_d.clamp(radial_floor_d, reserve_sum_d);
        let action = concentrated_guidance_d_action(
            candidate_invariant_d,
            guidance_invariant_d,
            radial_floor_d,
            reserve_sum_d,
        );
        let guidance =
            self.prepare_guidance_successor_with_invariant(base_reserve_nad, quote_reserve_nad, guidance_invariant_d)?;
        validate_bounded_guidance_basis(
            guidance.0.base_common,
            guidance.0.quote_common,
            guidance.0.invariant_d,
            guidance.0.geometry.ok_or(ErrorCode::BrokenInvariant)?,
        )?;
        Ok((guidance, action))
    }

    /// Even a canonically re-solved successor remains guidance-typed so no
    /// downstream code can accidentally turn a planner state into authority.
    pub(crate) fn prepare_hint_successor(self, base_reserve_nad: u128, quote_reserve_nad: u128) -> Result<Self> {
        self.0
            .prepare_successor(
                base_reserve_nad,
                quote_reserve_nad,
                ConcentratedInvariantSeed::Hint(self.0.invariant_d),
            )
            .map(Self)
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

/// Inclusive Q48 enclosure of one value computed by the existing Q64
/// residual. `lower << 16 <= value_q64 <= upper << 16` is the representation
/// invariant.  The interval is deliberately coarser than the authoritative
/// Q64/Q80 arithmetic; it is used only to certify a sign that is far enough
/// from zero that the exact ambiguity fallback cannot change it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Q48Interval {
    lower: u128,
    upper: u128,
}

impl Q48Interval {
    fn from_q64(value_q64: u128) -> Option<Self> {
        let lower = value_q64 >> (Q64_BITS - Q48_BITS);
        let upper = lower.checked_add(u128::from(value_q64 & ((1_u128 << (Q64_BITS - Q48_BITS)) - 1) != 0))?;
        Some(Self { lower, upper })
    }

    fn midpoint(self) -> u128 {
        self.lower + (self.upper - self.lower) / 2
    }

    fn checked_mul(self, other: Self) -> Option<Self> {
        let lower = self.lower.checked_mul(other.lower)? >> Q48_BITS;
        let upper_product = self.upper.checked_mul(other.upper)?;
        let upper = (upper_product >> Q48_BITS).checked_add(u128::from(upper_product & (Q48_ONE - 1) != 0))?;
        Some(Self { lower, upper })
    }
}

/// One bounded-denominator reciprocal shared by every certified residual
/// probe in a guidance quote. For `n,D <= 2*u64::MAX`,
///
/// `floor(n * floor(2^112 / D) / 2^64)`
///
/// underestimates `floor(n * 2^48 / D)` by at most two. The high product is
/// assembled from 64-bit limbs so no live `n * reciprocal` needs 256 bits.
/// Two checked remainder corrections then recover the exact quotient used by
/// the pre-existing division path. Any failed bound or identity check selects
/// that division path unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BoundedQ48Reciprocal {
    denominator: u128,
    reciprocal_q64: u128,
}

impl BoundedQ48Reciprocal {
    fn new(denominator: u128) -> Option<Self> {
        let maximum = MAX_COMMON_RESERVE.checked_mul(2)?;
        if denominator == 0 || denominator > maximum {
            return None;
        }
        let reciprocal_q64 = BOUNDED_Q48_RECIPROCAL_ONE.checked_div(denominator)?;
        if reciprocal_q64 == 0 {
            return None;
        }
        #[cfg(test)]
        BOUNDED_Q48_RECIPROCAL_BUILDS.with(|count| count.set(count.get() + 1));
        Some(Self {
            denominator,
            reciprocal_q64,
        })
    }

    fn approximate_floor(&self, numerator: u128) -> Option<u128> {
        let maximum = MAX_COMMON_RESERVE.checked_mul(2)?;
        if numerator > maximum {
            return None;
        }
        let numerator_high = numerator >> Q64_BITS;
        let numerator_low = numerator as u64 as u128;
        let reciprocal_high = self.reciprocal_q64 >> Q64_BITS;
        let reciprocal_low = self.reciprocal_q64 as u64 as u128;
        let low_product_high = numerator_low.checked_mul(reciprocal_low)? >> Q64_BITS;
        numerator_high
            .checked_mul(self.reciprocal_q64)?
            .checked_add(numerator_low.checked_mul(reciprocal_high)?)?
            .checked_add(low_product_high)
    }

    fn exact_floor_rem(&self, numerator: u128, denominator: u128) -> Option<(u128, u128)> {
        if denominator != self.denominator {
            return None;
        }
        let scaled = numerator.checked_mul(Q48_ONE)?;
        let mut quotient = self.approximate_floor(numerator)?;
        let product = quotient.checked_mul(denominator)?;
        let mut remainder = scaled.checked_sub(product)?;

        if remainder >= denominator {
            quotient = quotient.checked_add(1)?;
            remainder -= denominator;
        }
        if remainder >= denominator {
            quotient = quotient.checked_add(1)?;
            remainder -= denominator;
        }
        if remainder >= denominator {
            return None;
        }
        let recomposed = quotient.checked_mul(denominator)?.checked_add(remainder)?;
        (recomposed == scaled).then_some((quotient, remainder))
    }
}

fn ratio_q48_interval(
    numerator: u128,
    denominator: u128,
    reciprocal: Option<&BoundedQ48Reciprocal>,
) -> Result<Q48Interval> {
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    // Bounded common reserves make this product at most 113 bits. Keeping the
    // quotient native is the point of the guidance-only Q48 front-end.
    let scaled = numerator.checked_mul(Q48_ONE).ok_or(ErrorCode::InvariantOverflow)?;
    let accelerated = reciprocal.and_then(|value| value.exact_floor_rem(numerator, denominator));
    #[cfg(test)]
    if accelerated.is_some() {
        BOUNDED_Q48_RECIPROCAL_QUOTIENTS.with(|count| count.set(count.get() + 1));
    } else {
        BOUNDED_Q48_RECIPROCAL_FALLBACKS.with(|count| count.set(count.get() + 1));
    }
    let (lower, remainder) = accelerated.unwrap_or_else(|| (scaled / denominator, scaled % denominator));
    let upper = lower
        .checked_add(u128::from(remainder != 0))
        .ok_or(ErrorCode::InvariantOverflow)?;
    Ok(Q48Interval { lower, upper })
}

/// Encloses exact `floor(4*x*y*Q64/D^2)` without forming a Q64 product.
/// The two ratio products are at most 113 bits. The final interval product is
/// attempted directly; an off-domain product wider than u128 simply selects
/// the unchanged exact evaluator.
fn balance_factor_q48_interval(
    x: u128,
    y: u128,
    d: u128,
    reciprocal: Option<&BoundedQ48Reciprocal>,
) -> Result<Option<Q48Interval>> {
    validate_bounded_common_reserves(x, y)?;
    require!(d > 0, ErrorCode::DenominatorOverflow);
    let twice_x = x.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    let twice_y = y.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    Ok(ratio_q48_interval(twice_x, d, reciprocal)?.checked_mul(ratio_q48_interval(twice_y, d, reciprocal)?))
}

fn q48_div_interval_correlated_scale(scale: Q48Interval, delta: Q48Interval) -> Option<Q48Interval> {
    let lower_denominator = scale.lower.checked_add(delta.upper)?;
    let upper_denominator = scale.upper.checked_add(delta.lower)?;
    if lower_denominator == 0 || upper_denominator == 0 {
        return None;
    }
    let lower_product = scale.lower.checked_mul(Q48_ONE)?;
    let upper_product = scale.upper.checked_mul(Q48_ONE)?;
    let lower = lower_product / lower_denominator;
    let upper = (upper_product / upper_denominator).checked_add(u128::from(upper_product % upper_denominator != 0))?;
    Some(Q48Interval { lower, upper })
}

fn q48_mul_ratio_interval(value: Q48Interval, numerator: u128, denominator: u128) -> Option<Q48Interval> {
    if denominator == 0 {
        return None;
    }
    let lower_product = value.lower.checked_mul(numerator)?;
    let upper_product = value.upper.checked_mul(numerator)?;
    let lower = lower_product / denominator;
    let upper = (upper_product / denominator).checked_add(u128::from(upper_product % denominator != 0))?;
    Some(Q48Interval { lower, upper })
}

/// Fast residual front-end for the monotone inner scalar and exact tail.
///
/// A returned evaluation has the same sign as the existing Q64/Q80 path. Its
/// magnitude and q are only accelerator hints. `None` means that the interval
/// crosses zero, the point is in the cubic transition, or a direct u128 bound
/// is not representable; callers then execute the byte-identical exact path.
#[inline(never)]
fn certified_q48_residual_evaluation(
    x: u128,
    y: u128,
    d: u128,
    geometry: ConcentratedC1Geometry,
    context: ConcentratedResidualContext,
    reciprocal: Option<&BoundedQ48Reciprocal>,
) -> Result<Option<ConcentratedGuidanceResidualEvaluation>> {
    let Some(q) = balance_factor_q48_interval(x, y, d, reciprocal)? else {
        return Ok(None);
    };
    let (positive, magnitude_q48) = if context.branch.is_exact_tail() {
        let Some(target) = Q48Interval::from_q64(context.target_q64) else {
            return Ok(None);
        };
        if q.lower > target.upper {
            (true, q.midpoint().abs_diff(target.midpoint()))
        } else if q.upper < target.lower {
            (false, q.midpoint().abs_diff(target.midpoint()))
        } else {
            return Ok(None);
        }
    } else {
        if context.branch != ConcentratedHybridBranch::Inner {
            return Ok(None);
        }
        let sum = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
        if sum < d {
            return Ok(None);
        }
        let Some(scale) = Q48Interval::from_q64(geometry.scale_q64) else {
            return Ok(None);
        };
        let Some(peak) = Q48Interval::from_q64(geometry.peak_q64) else {
            return Ok(None);
        };
        let delta = Q48Interval {
            lower: Q48_ONE.saturating_sub(q.upper.min(Q48_ONE)),
            upper: Q48_ONE.saturating_sub(q.lower.min(Q48_ONE)),
        };
        let Some(weight_base) = q48_div_interval_correlated_scale(scale, delta) else {
            return Ok(None);
        };
        let Some(weight) = weight_base.checked_mul(weight_base) else {
            return Ok(None);
        };
        let Some(q_weight) = q.checked_mul(weight) else {
            return Ok(None);
        };
        let Some(twice_peak) = peak
            .lower
            .checked_mul(2)
            .zip(peak.upper.checked_mul(2))
            .map(|(lower, upper)| Q48Interval { lower, upper })
        else {
            return Ok(None);
        };
        let Some(coefficient) = twice_peak.checked_mul(q_weight) else {
            return Ok(None);
        };
        let Some(concentration) = q48_mul_ratio_interval(coefficient, sum - d, d) else {
            return Ok(None);
        };
        let Some(lower_total) = concentration.lower.checked_add(q.lower) else {
            return Ok(None);
        };
        let Some(upper_total) = concentration.upper.checked_add(q.upper) else {
            return Ok(None);
        };
        if lower_total > Q48_ONE {
            let midpoint = lower_total + (upper_total - lower_total) / 2;
            (true, midpoint - Q48_ONE)
        } else if upper_total < Q48_ONE {
            let midpoint = lower_total + (upper_total - lower_total) / 2;
            (false, Q48_ONE - midpoint)
        } else {
            return Ok(None);
        }
    };
    let magnitude = magnitude_q48.checked_shl(Q64_BITS - Q48_BITS);
    let q64 = q.midpoint().checked_shl(Q64_BITS - Q48_BITS);
    Ok(magnitude
        .zip(q64)
        .map(|(magnitude_hint, q64_hint)| ConcentratedGuidanceResidualEvaluation {
            positive,
            magnitude_hint,
            q64_hint,
        }))
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

    // Authoritative concentration only reaches this function with Q64/Q80
    // scaling and a two-reserve invariant no wider than 65 bits. Divide the
    // exact 210-bit numerator by the exact 130-bit squared denominator in
    // base-2^32 limbs. This replaces three general 128-step mul-div loops.
    // Keep the staged evaluator unchanged for every off-domain call.
    let maximum_d = MAX_COMMON_RESERVE.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    let fixed_bits = if fixed_one == Q64_ONE {
        Some(Q64_BITS)
    } else if fixed_one == Q80_ONE {
        Some(Q80_BITS)
    } else {
        None
    };
    if d <= maximum_d {
        if let Some(fixed_bits) = fixed_bits {
            return balance_factor_fixed_bounded(x, y, d, fixed_bits);
        }
    }

    balance_factor_fixed_staged(x, y, d, fixed_one)
}

/// Legacy staged identity retained byte-for-byte for off-domain calls and
/// exact differential tests.
fn balance_factor_fixed_staged(x: u128, y: u128, d: u128, fixed_one: u128) -> Result<u128> {
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

/// Exact `floor((4*x*y*2^fixed_bits) / d^2)` for `x,y < 2^64`,
/// `d < 2^65`, and `fixed_bits` equal to 64 or 80.
///
/// The numerator occupies at most seven base-2^32 limbs and the squared
/// denominator at most five. Knuth's normalized quotient-digit division uses
/// only u64 products/divisions. The returned quotient is rejected if it is
/// wider than u128. The explicit first-stage check preserves the legacy error
/// when `floor(2*x*2^fixed_bits/d)` itself is wider than u128 even if the final
/// symmetric expression would fit.
fn balance_factor_fixed_bounded(x: u128, y: u128, d: u128, fixed_bits: u32) -> Result<u128> {
    debug_assert!(x <= MAX_COMMON_RESERVE && y <= MAX_COMMON_RESERVE);
    debug_assert!(d > 0 && d <= MAX_COMMON_RESERVE * 2);
    debug_assert!(fixed_bits == Q64_BITS || fixed_bits == Q80_BITS);

    let twice_x = x.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    let first_quotient_high = twice_x >> (128 - fixed_bits);
    if first_quotient_high >= d {
        return Err(ErrorCode::InvariantOverflow.into());
    }

    let product = multiply_u65_to_u32_limbs(x, y);
    let mut numerator = [0_u32; 8];
    let shift = fixed_bits + 2;
    let word_shift = (shift / 32) as usize;
    let bit_shift = shift % 32;
    let mut carry = 0_u32;
    for (index, limb) in product[..4].iter().copied().enumerate() {
        let shifted = ((limb as u64) << bit_shift) | carry as u64;
        numerator[word_shift + index] = shifted as u32;
        carry = (shifted >> 32) as u32;
    }
    numerator[word_shift + 4] = carry;

    let denominator = multiply_u65_to_u32_limbs(d, d);
    divide_u32_limbs_to_u128(numerator, denominator)
}

/// Schoolbook product for two values no wider than 65 bits. Each inner sum is
/// strictly below 2^64: one 32x32 product, one output limb, and one carry.
fn multiply_u65_to_u32_limbs(lhs: u128, rhs: u128) -> [u32; 6] {
    debug_assert!(lhs < (1_u128 << 65) && rhs < (1_u128 << 65));
    let lhs = [lhs as u32, (lhs >> 32) as u32, (lhs >> 64) as u32];
    let rhs = [rhs as u32, (rhs >> 32) as u32, (rhs >> 64) as u32];
    let mut product = [0_u32; 6];
    for (lhs_index, lhs_limb) in lhs.iter().copied().enumerate() {
        let mut carry = 0_u64;
        for (rhs_index, rhs_limb) in rhs.iter().copied().enumerate() {
            let index = lhs_index + rhs_index;
            let accumulated = (lhs_limb as u64) * (rhs_limb as u64) + product[index] as u64 + carry;
            product[index] = accumulated as u32;
            carry = accumulated >> 32;
        }
        product[lhs_index + 3] = carry as u32;
    }
    product
}

fn u32_limb_len(limbs: &[u32]) -> usize {
    limbs.iter().rposition(|limb| *limb != 0).map_or(0, |index| index + 1)
}

/// Divide an eight-limb numerator by a six-limb denominator and return the
/// exact quotient when it fits u128. This is Knuth Algorithm D in base 2^32.
fn divide_u32_limbs_to_u128(numerator: [u32; 8], denominator: [u32; 6]) -> Result<u128> {
    const BASE: u64 = 1_u64 << 32;

    let numerator_len = u32_limb_len(&numerator);
    let denominator_len = u32_limb_len(&denominator);
    require!(denominator_len > 0, ErrorCode::DenominatorOverflow);
    if numerator_len < denominator_len {
        return Ok(0);
    }

    let mut quotient = [0_u32; 8];
    if denominator_len == 1 {
        let divisor = denominator[0] as u64;
        let mut remainder = 0_u64;
        for index in (0..numerator_len).rev() {
            let dividend = (remainder << 32) | numerator[index] as u64;
            quotient[index] = (dividend / divisor) as u32;
            remainder = dividend % divisor;
        }
    } else {
        let normalization_shift = denominator[denominator_len - 1].leading_zeros();
        let mut normalized_denominator = [0_u32; 6];
        let mut normalized_numerator = [0_u32; 9];

        let mut carry = 0_u32;
        for index in 0..denominator_len {
            let shifted = ((denominator[index] as u64) << normalization_shift) | carry as u64;
            normalized_denominator[index] = shifted as u32;
            carry = (shifted >> 32) as u32;
        }
        debug_assert_eq!(carry, 0);

        carry = 0;
        for index in 0..numerator_len {
            let shifted = ((numerator[index] as u64) << normalization_shift) | carry as u64;
            normalized_numerator[index] = shifted as u32;
            carry = (shifted >> 32) as u32;
        }
        normalized_numerator[numerator_len] = carry;

        let high_divisor = normalized_denominator[denominator_len - 1] as u64;
        let quotient_digits = numerator_len - denominator_len;
        for quotient_index in (0..=quotient_digits).rev() {
            let high_dividend = normalized_numerator[quotient_index + denominator_len] as u64;
            let low_dividend = normalized_numerator[quotient_index + denominator_len - 1] as u64;
            let dividend = (high_dividend << 32) | low_dividend;
            let (mut estimate, mut estimate_remainder) = if high_dividend == high_divisor {
                (BASE - 1, low_dividend + high_divisor)
            } else {
                (dividend / high_divisor, dividend % high_divisor)
            };

            let next_divisor = normalized_denominator[denominator_len - 2] as u64;
            let next_dividend = normalized_numerator[quotient_index + denominator_len - 2] as u64;
            while estimate_remainder < BASE && estimate * next_divisor > (estimate_remainder << 32) + next_dividend {
                estimate -= 1;
                estimate_remainder += high_divisor;
            }

            let mut borrow = 0_u64;
            for divisor_index in 0..denominator_len {
                let product = estimate * normalized_denominator[divisor_index] as u64 + borrow;
                let product_low = product as u32 as u64;
                let minuend = normalized_numerator[quotient_index + divisor_index] as u64;
                normalized_numerator[quotient_index + divisor_index] = minuend.wrapping_sub(product_low) as u32;
                borrow = (product >> 32) + u64::from(minuend < product_low);
            }
            let top_index = quotient_index + denominator_len;
            let top = normalized_numerator[top_index] as u64;
            let negative = top < borrow;
            normalized_numerator[top_index] = top.wrapping_sub(borrow) as u32;

            if negative {
                estimate -= 1;
                let mut add_carry = 0_u64;
                for divisor_index in 0..denominator_len {
                    let index = quotient_index + divisor_index;
                    let sum =
                        normalized_numerator[index] as u64 + normalized_denominator[divisor_index] as u64 + add_carry;
                    normalized_numerator[index] = sum as u32;
                    add_carry = sum >> 32;
                }
                normalized_numerator[top_index] = normalized_numerator[top_index].wrapping_add(add_carry as u32);
            }
            quotient[quotient_index] = estimate as u32;
        }
    }

    if quotient[4..].iter().any(|limb| *limb != 0) {
        return Err(ErrorCode::InvariantOverflow.into());
    }
    Ok((quotient[0] as u128)
        | ((quotient[1] as u128) << 32)
        | ((quotient[2] as u128) << 64)
        | ((quotient[3] as u128) << 96))
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CanonicalDHintEvidence {
    hit: Option<u128>,
    low: Option<(u128, bool, u128)>,
    high: Option<(u128, u128)>,
}

fn canonical_d_hint_residual(
    x: u128,
    y: u128,
    d: u128,
    geometry: Option<ConcentratedC1Geometry>,
    context: Option<ConcentratedResidualContext>,
) -> Result<(bool, u128)> {
    #[cfg(test)]
    CANONICAL_D_HINT_RESIDUAL_EVALUATIONS.with(|count| count.set(count.get() + 1));

    hybrid_residual_with_context(x, y, d, geometry, context)
}

/// Attempts the complete adjacent-atom proof around one caller-provided D
/// hint. Extra predecessor work is optional: an arithmetic failure there
/// cannot reduce the domain accepted by the pre-existing hinted solver. The
/// strict-interior hint residual retains the old propagated-error behavior.
fn canonical_d_hint_evidence(
    x: u128,
    y: u128,
    hint: u128,
    low: u128,
    high: u128,
    geometry: Option<ConcentratedC1Geometry>,
    context: Option<ConcentratedResidualContext>,
) -> Result<CanonicalDHintEvidence> {
    let mut evidence = CanonicalDHintEvidence::default();
    if hint < low || hint > high {
        #[cfg(test)]
        CANONICAL_D_HINT_MISSES.with(|count| count.set(count.get() + 1));
        return Ok(evidence);
    }

    if hint == low {
        // At the geometric lower endpoint there is no in-bracket
        // predecessor. A zero residual is the canonical boundary root.
        if let Ok((positive, magnitude)) = canonical_d_hint_residual(x, y, hint, geometry, context) {
            if positive && magnitude == 0 {
                evidence.hit = Some(hint);
            } else {
                evidence.low = Some((hint, positive, magnitude));
            }
        }
    } else {
        let predecessor = hint - 1;
        let hint_evaluation = if hint < high {
            // Strict-interior hints were already evaluated by the old path;
            // preserve its exact error boundary.
            Some(canonical_d_hint_residual(x, y, hint, geometry, context)?)
        } else {
            // A boundary hint is new accelerator work, so failure falls back.
            canonical_d_hint_residual(x, y, hint, geometry, context).ok()
        };

        if let Some((hint_positive, hint_magnitude)) = hint_evaluation {
            if hint_positive {
                // The root cannot be at a residual-positive non-boundary
                // hint, so predecessor work cannot complete an adjacent
                // proof. Reuse the exact hint as the lower bracket exactly as
                // the old hinted solver did.
                evidence.low = Some((hint, true, hint_magnitude));
            } else {
                let predecessor_evaluation = canonical_d_hint_residual(x, y, predecessor, geometry, context).ok();
                if matches!(predecessor_evaluation, Some((true, _))) {
                    evidence.hit = Some(hint);
                } else {
                    evidence.high = match predecessor_evaluation {
                        Some((false, predecessor_magnitude)) => Some((predecessor, predecessor_magnitude)),
                        _ => Some((hint, hint_magnitude)),
                    };
                }
            }
        }
    }

    #[cfg(test)]
    if evidence.hit.is_some() {
        CANONICAL_D_HINT_HITS.with(|count| count.set(count.get() + 1));
    } else {
        CANONICAL_D_HINT_MISSES.with(|count| count.set(count.get() + 1));
    }
    Ok(evidence)
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

    hybrid_residual_evaluation_with_context_uncounted(x, y, d, geometry, context)
}

fn hybrid_residual_evaluation_with_context_uncounted(
    x: u128,
    y: u128,
    d: u128,
    geometry: Option<ConcentratedC1Geometry>,
    context: Option<ConcentratedResidualContext>,
) -> Result<ConcentratedResidualEvaluation> {
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

/// Guidance-only residual front-end. Certified Q48 evidence never enters the
/// canonical prepare/quote/checkpoint path: bounded exact-in/out probes may use
/// its sign and optional accelerator hints, and every proposed coordinate is
/// re-evaluated on its own branch before emission. Any fast-path arithmetic
/// failure falls through to the unchanged exact evaluator.
fn bounded_guidance_residual_evaluation_with_context(
    x: u128,
    y: u128,
    d: u128,
    geometry: Option<ConcentratedC1Geometry>,
    context: Option<ConcentratedResidualContext>,
    reciprocal: Option<&BoundedQ48Reciprocal>,
) -> Result<ConcentratedGuidanceResidualEvaluation> {
    #[cfg(test)]
    RESIDUAL_EVALUATIONS.with(|count| count.set(count.get() + 1));

    // Preserve the shared evaluator's validation and error order before the
    // optional accelerator is considered.
    validate_common_reserves(x, y)?;
    require!(d > 0, ErrorCode::InvalidArgument);
    let Some(geometry) = geometry else {
        let exact = hybrid_residual_evaluation_with_context_uncounted(x, y, d, None, context)?;
        return Ok(ConcentratedGuidanceResidualEvaluation {
            positive: exact.positive,
            magnitude_hint: exact.magnitude,
            q64_hint: exact.q64,
        });
    };
    let context = context.ok_or(ErrorCode::BrokenInvariant)?;

    #[cfg(test)]
    let certified_enabled = CERTIFIED_Q48_RESIDUALS_ENABLED.with(Cell::get);
    #[cfg(not(test))]
    let certified_enabled = true;
    if certified_enabled {
        if let Ok(Some(evaluation)) = certified_q48_residual_evaluation(x, y, d, geometry, context, reciprocal) {
            #[cfg(test)]
            CERTIFIED_Q48_RESIDUAL_EVALUATIONS.with(|count| count.set(count.get() + 1));
            return Ok(evaluation);
        }
        #[cfg(test)]
        CERTIFIED_Q48_EXACT_FALLBACK_EVALUATIONS.with(|count| count.set(count.get() + 1));
    }

    let exact = hybrid_residual_evaluation_with_context_uncounted(x, y, d, Some(geometry), Some(context))?;
    Ok(ConcentratedGuidanceResidualEvaluation {
        positive: exact.positive,
        magnitude_hint: exact.magnitude,
        q64_hint: exact.q64,
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
    accelerator: ConcentratedResidualAccelerator,
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
        accelerator.q64_hint.checked_sub(half_slope_cosh)
    } else {
        accelerator.q64_hint.checked_add(half_slope_cosh)
    };
    let Some(derivative_times_variable) = derivative_times_variable.filter(|value| *value > 0) else {
        return Ok(None);
    };
    let step = mul_div_ceil_u128(accelerator.magnitude_hint, variable, derivative_times_variable)
        .map_err(|_| ErrorCode::InvariantOverflow)?
        .max(1);
    Ok(if accelerator.positive {
        variable.checked_sub(step)
    } else {
        variable.checked_add(step)
    })
}

/// Continuous Newton accelerator for the inner concentrated branch.
///
/// With `x` and `D` fixed, the executable scalar is
///
///   R(y) = 2*P*q*w*h + q - 1,
///   q = 4*x*y/D^2,
///   w = (s/(s + max(1-q, 0)))^2,
///   h = (x+y-D)/D.
///
/// Its exact continuous derivative satisfies
///
///   y*R'(y) = q + 2*P*q*w*(h + y/D + 2*q*h/(s+1-q)), q < 1,
///             q + 2*P*q*w*(h + y/D),               q >= 1.
///
/// Fixed-point rounding means this is guidance only. The caller retains the
/// authoritative sign bracket and adjacent-atom proof. Arithmetic failure is
/// deliberately ignored so the accelerator cannot narrow the quote domain.
fn inner_newton_probe(
    fixed: u128,
    variable: u128,
    d: u128,
    accelerator: ConcentratedResidualAccelerator,
    geometry: ConcentratedC1Geometry,
) -> Option<u128> {
    (|| -> Result<Option<u128>> {
        let sum = fixed.checked_add(variable).ok_or(ErrorCode::InvariantOverflow)?;
        if sum < d {
            return Ok(None);
        }
        let q = accelerator.q64_hint;
        if q == Q64_ONE {
            return Ok(None);
        }
        let delta = Q64_ONE.saturating_sub(q.min(Q64_ONE));
        let denominator = geometry
            .scale_q64
            .checked_add(delta)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let weight_base = div_q64(geometry.scale_q64, denominator)?;
        let weight = mul_q64(weight_base, weight_base)?;
        let coefficient = mul_q64(
            geometry.peak_q64.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
            mul_q64(q, weight)?,
        )?;
        let h = div_q64(sum - d, d)?;
        let variable_over_d = div_q64(variable, d)?;
        let curvature = if q < Q64_ONE {
            div_q64(
                mul_q64(q, h)?.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?,
                denominator,
            )?
        } else {
            0
        };
        let derivative_bracket = h
            .checked_add(variable_over_d)
            .and_then(|value| value.checked_add(curvature))
            .ok_or(ErrorCode::InvariantOverflow)?;
        let derivative_times_variable = q
            .checked_add(mul_q64(coefficient, derivative_bracket)?)
            .filter(|value| *value > 0)
            .ok_or(ErrorCode::DenominatorOverflow)?;
        let step = mul_div_ceil_u128(accelerator.magnitude_hint, variable, derivative_times_variable)
            .map_err(|_| ErrorCode::InvariantOverflow)?
            .max(1);
        Ok(if accelerator.positive {
            variable.checked_sub(step)
        } else {
            variable.checked_add(step)
        })
    })()
    .ok()
    .flatten()
}

fn variable_reserve_newton_probe(
    fixed: u128,
    variable: u128,
    d: u128,
    evaluation: ConcentratedResidualEvaluation,
    geometry: Option<ConcentratedC1Geometry>,
    context: Option<ConcentratedResidualContext>,
) -> Result<Option<u128>> {
    let accelerator = evaluation.accelerator();
    if context.map(|context| context.branch) == Some(ConcentratedHybridBranch::Inner) {
        #[cfg(test)]
        if !INNER_NEWTON_ACCELERATION_ENABLED.with(Cell::get) {
            return Ok(None);
        }
        return Ok(geometry.and_then(|geometry| inner_newton_probe(fixed, variable, d, accelerator, geometry)));
    }
    transition_newton_probe(fixed, variable, accelerator, context)
}

/// Guidance accelerators are optional by construction. Their arithmetic can
/// choose the next bounded probe, but can neither fail the quote nor update a
/// sign bracket until that exact coordinate has been independently certified.
fn bounded_guidance_variable_reserve_newton_probe(
    fixed: u128,
    variable: u128,
    d: u128,
    evaluation: ConcentratedGuidanceResidualEvaluation,
    geometry: Option<ConcentratedC1Geometry>,
    context: Option<ConcentratedResidualContext>,
) -> Option<u128> {
    let accelerator = evaluation.accelerator();
    if context.map(|context| context.branch) == Some(ConcentratedHybridBranch::Inner) {
        #[cfg(test)]
        if !INNER_NEWTON_ACCELERATION_ENABLED.with(Cell::get) {
            return None;
        }
        return geometry.and_then(|geometry| inner_newton_probe(fixed, variable, d, accelerator, geometry));
    }
    transition_newton_probe(fixed, variable, accelerator, context)
        .ok()
        .flatten()
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
    invariant_seed: Option<PreparedCurveInvariantSeed>,
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
        Some(PreparedCurveInvariantSeed::Canonical(ConcentratedInvariantSeed::Exact(invariant_d))) => {
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
        Some(PreparedCurveInvariantSeed::Guidance(invariant_d)) => {
            require!(invariant_d > 0, ErrorCode::BrokenInvariant);
            invariant_d
        }
        seed => {
            let hint = match seed {
                Some(PreparedCurveInvariantSeed::Canonical(ConcentratedInvariantSeed::Hint(value))) => Some(value),
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
                let hint_evidence = if let Some(hint) = hint {
                    canonical_d_hint_evidence(base_common, quote_common, hint, low, high, geometry, residual_context)?
                } else {
                    CanonicalDHintEvidence::default()
                };
                if let Some(hit) = hint_evidence.hit {
                    return Ok(ConcentratedPreparedCurve {
                        base_reserve_nad,
                        quote_reserve_nad,
                        base_common,
                        quote_common,
                        center_price_nad,
                        peak_depth_nad,
                        fade_scale_nad,
                        invariant_d: hit,
                        common_numeraire,
                        geometry,
                    });
                }
                let (mut low_magnitude, mut high_magnitude);
                if let Some((hinted_low, low_sign, magnitude)) = hint_evidence.low {
                    low = hinted_low;
                    require!(low_sign, ErrorCode::InvariantOverflow);
                    low_magnitude = magnitude;
                    high_magnitude = invariant_sum_endpoint_magnitude(
                        base_common,
                        quote_common,
                        high,
                        peak_depth_nad,
                        residual_context,
                    )?;
                } else if let Some((hinted_high, magnitude)) = hint_evidence.high {
                    high = hinted_high;
                    high_magnitude = magnitude;
                    let (low_sign, magnitude) =
                        hybrid_residual_with_context(base_common, quote_common, low, geometry, residual_context)?;
                    require!(low_sign, ErrorCode::InvariantOverflow);
                    low_magnitude = magnitude;
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
        Some(PreparedCurveInvariantSeed::Canonical(invariant_seed)),
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
        safeguarded_newton_probe =
            variable_reserve_newton_probe(fixed, structural_probe, d, evaluation, geometry, context)?
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
        safeguarded_newton_probe = variable_reserve_newton_probe(fixed, probe, d, evaluation, geometry, context)?
            .filter(|candidate| *candidate > low && *candidate < high);
        previous_probe_was_valid = Some(valid);
    }
    require!(high - low <= 1, ErrorCode::InvariantOverflow);
    Ok((low, high))
}

/// Test-only canonical reference that deliberately uses plain bisection.
/// Production acceleration must return the identical adjacent sign bracket.
#[cfg(test)]
pub(crate) fn solve_variable_reserve_bisection_reference(
    fixed: u128,
    d: u128,
    geometry: Option<ConcentratedC1Geometry>,
    mut low: u128,
    mut high: u128,
) -> Result<(u128, u128)> {
    require!(low < high, ErrorCode::InvalidArgument);
    require!(
        !hybrid_residual(fixed, low, d, geometry)?.0,
        ErrorCode::InvariantOverflow
    );
    require!(
        hybrid_residual(fixed, high, d, geometry)?.0,
        ErrorCode::InvariantOverflow
    );
    while high - low > 1 {
        let probe = low + (high - low) / 2;
        if hybrid_residual(fixed, probe, d, geometry)?.0 {
            high = probe;
        } else {
            low = probe;
        }
    }
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

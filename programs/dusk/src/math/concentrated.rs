//! Dusk Concentrated AMM, a two-asset hybrid invariant whose additional center
//! depth fades until a protocol-fixed shoulder, then continues on an exact
//! CPMM branch.
//!
//! In quote-value coordinates, define:
//! ```text
//! q       = 4*x*y / D^2
//! delta   = 1 - q
//! weight  = (imbalance_scale / (imbalance_scale + delta))^2
//! depth_eff = (peak_depth / 2) * q * weight
//! depth_eff*D*(x + y - D) + x*y - D^2/4 = 0   on the inner branch
//!
//! q_*   = 1 - imbalance_scale
//! t_*   = 1 + 2*imbalance_scale/(peak_depth*q_*)
//! rho_* = q_*/t_*^2
//! rho   = 4*x*y/(x+y)^2
//!
//! 4*x*y*NAD - D^2*(NAD-imbalance_scale) = 0   when rho < rho_*
//! ```
//! `x` and `y` are first expressed in the quote asset's NAD units at `center`.
//! At the balanced center, `1 + peak_depth` is the marginal-depth multiplier
//! relative to CPMM. `imbalance_scale` controls how far reserves can move from
//! balance before that extra depth fades. `rho` is a D-independent,
//! homogeneous branch selector; the equality point belongs to the
//! concentrated branch. The invariant value is continuous at the shoulder;
//! its marginal price has a deliberate one-sided kink into the CPMM tail.
//! Both parameters zero selects exact legacy CPMM.
//!
//! The implementation below was derived independently from the mathematical
//! equation. It uses a division-free residual so fixed-point rounding cannot
//! change the number or ordering of executable roots.

use anchor_lang::prelude::*;
use core::cmp::Ordering;
#[cfg(test)]
use std::cell::Cell;

use crate::{constants::NAD, errors::ErrorCode};

use super::gamm::{calculate_normalized_amount_in, calculate_normalized_amount_out};

#[allow(clippy::assign_op_pattern, clippy::manual_div_ceil)]
mod wide {
    use uint::construct_uint;

    construct_uint! {
        pub struct U256(4);
    }

    construct_uint! {
        pub struct U512(8);
    }
}

use wide::{U256, U512};

#[cfg(test)]
thread_local! {
    static RESIDUAL_EVALUATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_residual_evaluations() {
    RESIDUAL_EVALUATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn residual_evaluations() -> usize {
    RESIDUAL_EVALUATIONS.with(Cell::get)
}

/// Peak depth and imbalance scale are NAD-scaled. The caps make the on-chain
/// arithmetic domain explicit and keeps every residual evaluation comfortably
/// inside U512.
pub(crate) const CONCENTRATED_MAX_PEAK_DEPTH_NAD: u128 = 2_000 * NAD as u128;
pub(crate) const CONCENTRATED_MAX_IMBALANCE_SCALE_NAD: u128 = 199_000_000;
pub(crate) const CONCENTRATED_INVARIANT_MAX_ITERS: usize = 48;
pub(crate) const CONCENTRATED_RESERVE_MAX_ITERS: usize = 24;
pub(crate) const CONCENTRATED_SOLVER_SAFETY_PPM: u128 = 10;
const PPM_DENOMINATOR: u128 = 1_000_000;
// A 0.01 ppb D bracket is six orders tighter than the 10 ppm executable
// quote margin while avoiding needless wide-integer rounds on SBF.
const INVARIANT_PROOF_DENOMINATOR: u128 = 100_000_000_000;
// A retained-fee successor is already branch-bound to its fully certified
// trade endpoint. Its 10 ppb D bracket is persisted and may be restored
// directly. The canonical mark therefore evaluates the branch-aware gradient
// at D_high, the same conservative D parameter used by exact-in execution;
// configured parameter/reserve floors keep that convention within the
// protocol's independently tested 25 ppm true-root price budget.
const CONTINUOUS_SUCCESSOR_PROOF_DENOMINATOR: u128 = 100_000_000;
/// Coarse retained-successor brackets are enabled only at or above the
/// configured concentration-depth floor. Sub-floor ramp points can be almost
/// CPMM-flat while still selecting the inner branch, which makes their
/// marginal derivative more sensitive to D rounding; they retain the standard
/// proof width instead.
pub(crate) const CONCENTRATED_COARSE_SUCCESSOR_MIN_PEAK_DEPTH_NAD: u128 = 2 * NAD as u128;
const CONCENTRATED_INVARIANT_PROOF_PARTS: u128 = 1;
const CONCENTRATED_RESERVE_PROOF_PPM: u128 = 10;

/// Concentrated-mode common-coordinate reserves are restricted so every
/// degree-eight cleared-residual term fits in U512 under the parameter caps.
/// At NAD precision this still represents more than 18 billion whole tokens.
/// Exact CPMM mode bypasses this concentrated-mode arithmetic domain.
pub(crate) const MAX_COMMON_RESERVE: u128 = u64::MAX as u128;
/// Positive-concentration inner states need at least one quote-value unit on
/// each side so the one-atom invariant certificate remains within the
/// protocol's 25 ppm marginal-price error budget. Exact CPMM tails do not use
/// this floor because their price is the raw reserve ratio and has no implicit
/// invariant-root conditioning.
pub(crate) const MIN_INNER_COMMON_RESERVE: u128 = NAD as u128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcentratedSwapDirection {
    BaseToQuote,
    QuoteToBase,
}

/// The homogeneous branch selected solely by the common-coordinate reserve
/// ratio. The two tail variants make directional shoulder behavior explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcentratedHybridBranch {
    Inner,
    BaseScarceTail,
    QuoteScarceTail,
}

/// Canonical protocol shoulder for one invariant value in common coordinates.
///
/// `low_common` and `high_common` are rounded inward to executable integer
/// coordinates. The marginal prices are normalized low-side prices: the
/// quote-scarce shoulder itself has base->quote price equal to these values;
/// the base-scarce shoulder has their reciprocal orientation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedHybridShoulder {
    pub low_common: u128,
    pub high_common: u128,
    pub tail_product_common: u128,
    pub inner_low_marginal_nad: u128,
    pub tail_low_marginal_nad: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedEvaluation {
    pub invariant_d: u128,
    pub invariant_d_high: u128,
    pub balanced_equivalent_q: u128,
    pub marginal_price_nad: u128,
}

/// One certified Dusk Concentrated AMM start state.
///
/// A swap needs the same normalized reserves and invariant bracket for three
/// independent decisions: divergence fees, the conservative output solve, and
/// the starting marginal price. Keeping the certificate together prevents
/// those consumers from solving the identical invariant more than once.
///
/// Fields stay private so a certificate cannot be paired with different
/// reserves, center, or curve parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConcentratedPreparedCurve {
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    base_common: u128,
    quote_common: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    /// Protocol-fixed shoulder relation certified during the invariant solve.
    /// Reusing it makes marginal branch selection identical to execution,
    /// including the directional rule at exact equality.
    shoulder_relation: Ordering,
    invariant_low: u128,
    invariant_high: u128,
}

impl ConcentratedPreparedCurve {
    /// Matches the lower certified endpoint historically returned by
    /// `concentrated_invariant`. Accounting and divergence coordinates therefore retain
    /// byte-for-byte rounding behavior.
    pub(crate) const fn invariant_d(self) -> u128 {
        self.invariant_low
    }

    pub(crate) const fn invariant_bracket(self) -> (u128, u128) {
        (self.invariant_low, self.invariant_high)
    }

    pub(crate) fn balanced_equivalent_q(self) -> Result<u128> {
        concentrated_balanced_equivalent_q(self.invariant_low, self.center_price_nad)
    }

    /// Canonical branch-aware gradient evaluated at `invariant_high`, the same
    /// conservative D parameter used by executable exact-in quotes. D_high is
    /// a certified bracket endpoint, not asserted to be the real root; the
    /// supported domain proves this convention stays within 25 ppm of the
    /// independent true-root marginal reference.
    ///
    /// The shoulder relation was computed by the invariant certification. At
    /// exact equality, base input uses the CPMM side when base is abundant and
    /// the inner side when base is scarce, matching an infinitesimal
    /// base-to-quote trade.
    pub(crate) fn marginal_price_nad(self) -> Result<u128> {
        if self.peak_depth_nad == 0 {
            mul_div_u128(self.quote_reserve_nad, NAD as u128, self.base_reserve_nad)
        } else if self.shoulder_relation == Ordering::Less
            || (self.shoulder_relation == Ordering::Equal && self.base_common >= self.quote_common)
        {
            // Same-tail execution is exact CPMM in the raw normalized reserve
            // coordinates. Use that identical level set here: converting base
            // into common coordinates floors first and can otherwise move the
            // reported marginal by one common-coordinate atom.
            mul_div_u128(self.quote_reserve_nad, NAD as u128, self.base_reserve_nad)
        } else {
            concentrated_canonical_high_inner_marginal_price_from_common(
                self.base_common,
                self.quote_common,
                self.invariant_high,
                self.center_price_nad,
                self.peak_depth_nad,
                self.imbalance_scale_nad,
            )
        }
    }

    pub(crate) fn evaluation(self) -> Result<ConcentratedEvaluation> {
        Ok(ConcentratedEvaluation {
            invariant_d: self.invariant_low,
            invariant_d_high: self.invariant_high,
            balanced_equivalent_q: self.balanced_equivalent_q()?,
            marginal_price_nad: self.marginal_price_nad()?,
        })
    }

    /// Evaluates a same-curve successor from its already-certified executable
    /// level set and shoulder branch.
    pub(crate) fn continuous_successor_evaluation(self) -> Result<ConcentratedEvaluation> {
        Ok(ConcentratedEvaluation {
            invariant_d: self.invariant_low,
            invariant_d_high: self.invariant_high,
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
                    calculate_normalized_amount_out(self.base_reserve_nad, self.quote_reserve_nad, amount_in_nad)
                }
                ConcentratedSwapDirection::QuoteToBase => {
                    calculate_normalized_amount_out(self.quote_reserve_nad, self.base_reserve_nad, amount_in_nad)
                }
            };
        }
        if let Some(output) = exact_cpmm_tail_in_raw(
            self.base_reserve_nad,
            self.quote_reserve_nad,
            amount_in_nad,
            direction,
            self.center_price_nad,
            self.peak_depth_nad,
            self.imbalance_scale_nad,
        )? {
            return Ok(output);
        }

        match direction {
            ConcentratedSwapDirection::BaseToQuote => {
                let input_common = base_to_common(amount_in_nad, self.center_price_nad)?;
                if input_common == 0 {
                    return Ok(0);
                }
                quote_common_exact_in_with_d(
                    self.base_common,
                    self.quote_common,
                    input_common,
                    self.invariant_high,
                    self.peak_depth_nad,
                    self.imbalance_scale_nad,
                )
            }
            ConcentratedSwapDirection::QuoteToBase => {
                let output_common = quote_common_exact_in_with_d(
                    self.quote_common,
                    self.base_common,
                    amount_in_nad,
                    self.invariant_high,
                    self.peak_depth_nad,
                    self.imbalance_scale_nad,
                )?;
                common_to_base_floor(output_common, self.center_price_nad)
            }
        }
    }

    pub(crate) const fn base_reserve_nad(self) -> u128 {
        self.base_reserve_nad
    }

    pub(crate) const fn quote_reserve_nad(self) -> u128 {
        self.quote_reserve_nad
    }

    /// Canonical quote-value coordinates already used by the invariant. Fee
    /// logic consumes these accessors so it cannot reproduce center-price
    /// normalization with different floor ordering.
    pub(crate) const fn base_common_nad(self) -> u128 {
        self.base_common
    }

    pub(crate) const fn quote_common_nad(self) -> u128 {
        self.quote_common
    }

    pub(crate) const fn center_price_nad(self) -> u128 {
        self.center_price_nad
    }

    pub(crate) const fn peak_depth_nad(self) -> u128 {
        self.peak_depth_nad
    }

    pub(crate) const fn imbalance_scale_nad(self) -> u128 {
        self.imbalance_scale_nad
    }

    /// Certifies a successor reserve state using this state's already
    /// certified invariant as a first bisection probe.
    ///
    /// The hint is never trusted as a bound: the successor solve rebuilds the
    /// full `[2*sqrt(x*y), x+y]` sign bracket, clamps the hint inside it, and
    /// retains the same fail-closed residual proofs as a cold solve. Chaining
    /// this method lets a post-retention endpoint reuse the post-trade
    /// certificate in exactly the same way.
    pub(crate) fn prepare_successor(
        self,
        base_reserve_nad: u128,
        quote_reserve_nad: u128,
    ) -> Result<ConcentratedPreparedCurve> {
        prepare_curve_internal(
            base_reserve_nad,
            quote_reserve_nad,
            self.center_price_nad,
            self.peak_depth_nad,
            self.imbalance_scale_nad,
            Some(self.invariant_low),
            INVARIANT_PROOF_DENOMINATOR,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResidualSign {
    Negative,
    NonNegative,
}

fn checked_add_512(a: U512, b: U512) -> Result<U512> {
    let (value, overflow) = a.overflowing_add(b);
    require!(!overflow, ErrorCode::InvariantOverflow);
    Ok(value)
}

fn checked_mul_512(a: U512, b: U512) -> Result<U512> {
    let (value, overflow) = a.overflowing_mul(b);
    require!(!overflow, ErrorCode::InvariantOverflow);
    Ok(value)
}

/// Multiplies by a native scalar through the uint crate's linear eight-limb
/// `U512 * u64` kernel instead of its quadratic wide-by-wide kernel.
///
/// Callers must first prove the complete result fits U512. Every use in the
/// canonical-high derivative below has at least 25 spare bits at the protocol
/// maxima; `Mul`/`Add` still panic on an impossible violated proof rather than
/// returning a wrapped value.
fn mul_512_u128_proven(value: U512, scalar: u128) -> U512 {
    let low = scalar as u64;
    let high = (scalar >> 64) as u64;
    let low_product = value * low;
    if high == 0 {
        low_product
    } else {
        low_product + ((value * high) << 64)
    }
}

fn u256_to_u128(value: U256) -> Result<u128> {
    require!(value <= U256::from(u128::MAX), ErrorCode::InvariantOverflow);
    Ok(value.as_u128())
}

fn mul_div_u128(a: u128, b: u128, denominator: u128) -> Result<u128> {
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    let value = U256::from(a)
        .checked_mul(U256::from(b))
        .ok_or(ErrorCode::InvariantOverflow)?
        / U256::from(denominator);
    u256_to_u128(value)
}

fn validate_parameters(center_price_nad: u128, peak_depth_nad: u128, imbalance_scale_nad: u128) -> Result<()> {
    require!(center_price_nad > 0, ErrorCode::InvalidArgument);
    require!(
        peak_depth_nad <= CONCENTRATED_MAX_PEAK_DEPTH_NAD,
        ErrorCode::InvalidArgument
    );
    if peak_depth_nad == 0 || imbalance_scale_nad == 0 {
        require!(
            peak_depth_nad == 0 && imbalance_scale_nad == 0,
            ErrorCode::InvalidArgument
        );
    } else {
        require!(
            imbalance_scale_nad <= CONCENTRATED_MAX_IMBALANCE_SCALE_NAD,
            ErrorCode::InvalidArgument
        );
    }
    Ok(())
}

fn validate_positive_reserves(x: u128, y: u128) -> Result<()> {
    require!(x > 0 && y > 0, ErrorCode::InvalidArgument);
    Ok(())
}

fn validate_common_reserves(x: u128, y: u128) -> Result<()> {
    validate_positive_reserves(x, y)?;
    require!(
        x <= MAX_COMMON_RESERVE && y <= MAX_COMMON_RESERVE,
        ErrorCode::InvalidArgument
    );
    Ok(())
}

fn validate_inner_common_reserve_floor(x: u128, y: u128, shoulder_relation: Ordering) -> Result<()> {
    if shoulder_relation != Ordering::Less {
        require!(
            x >= MIN_INNER_COMMON_RESERVE && y >= MIN_INNER_COMMON_RESERVE,
            ErrorCode::InsufficientLiquidity
        );
    }
    Ok(())
}

/// Selects the protocol-fixed hybrid branch without solving for `D`.
///
/// At the shoulder `delta = imbalance_scale`, the inner invariant implies
/// ```text
/// q_s = 1 - s
/// (x+y)/D = 1 + 2*s/(peak_depth*q_s).
/// ```
/// Eliminating `D` yields the homogeneous cross-product below. Equality is
/// deliberately classified as `Inner`; directional marginal evaluation uses
/// the appropriate one-sided derivative at that equality point.
fn concentrated_hybrid_shoulder_relation(
    x: u128,
    y: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<Ordering> {
    validate_positive_reserves(x, y)?;
    if peak_depth_nad == 0 {
        return Ok(Ordering::Greater);
    }
    validate_common_reserves(x, y)?;
    require!(
        imbalance_scale_nad > 0 && imbalance_scale_nad < NAD as u128,
        ErrorCode::InvalidArgument
    );

    let one_minus_scale = (NAD as u128)
        .checked_sub(imbalance_scale_nad)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let a = checked_mul_512(U512::from(peak_depth_nad), U512::from(one_minus_scale))?;
    let twice_scale = checked_mul_512(
        U512::from(2_u8),
        checked_mul_512(U512::from(imbalance_scale_nad), U512::from(NAD))?,
    )?;
    let c = checked_add_512(a, twice_scale)?;
    let sum = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;

    // central iff 4xy/(x+y)^2 >= q_s/t_s^2.
    let left = checked_mul_512(
        checked_mul_512(
            checked_mul_512(U512::from(4_u8), checked_mul_512(U512::from(x), U512::from(y))?)?,
            U512::from(NAD),
        )?,
        checked_mul_512(c, c)?,
    )?;
    let right = checked_mul_512(
        checked_mul_512(U512::from(sum), U512::from(sum))?,
        checked_mul_512(U512::from(one_minus_scale), checked_mul_512(a, a)?)?,
    )?;
    Ok(left.cmp(&right))
}

/// Returns the reserve-ratio branch; exact shoulder equality is `Inner`.
pub(crate) fn concentrated_hybrid_branch_from_common(
    x: u128,
    y: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<ConcentratedHybridBranch> {
    if concentrated_hybrid_shoulder_relation(x, y, peak_depth_nad, imbalance_scale_nad)? != Ordering::Less {
        Ok(ConcentratedHybridBranch::Inner)
    } else if x < y {
        Ok(ConcentratedHybridBranch::BaseScarceTail)
    } else {
        Ok(ConcentratedHybridBranch::QuoteScarceTail)
    }
}

fn hybrid_residual_terms(
    x: u128,
    y: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<(U512, U512)> {
    match concentrated_hybrid_branch_from_common(x, y, peak_depth_nad, imbalance_scale_nad)? {
        ConcentratedHybridBranch::Inner => residual_terms(x, y, d, peak_depth_nad, imbalance_scale_nad),
        ConcentratedHybridBranch::BaseScarceTail | ConcentratedHybridBranch::QuoteScarceTail => {
            let positive = checked_mul_512(
                checked_mul_512(U512::from(4_u8), checked_mul_512(U512::from(x), U512::from(y))?)?,
                U512::from(NAD),
            )?;
            let negative = checked_mul_512(
                checked_mul_512(U512::from(d), U512::from(d))?,
                U512::from(
                    (NAD as u128)
                        .checked_sub(imbalance_scale_nad)
                        .ok_or(ErrorCode::InvariantOverflow)?,
                ),
            )?;
            Ok((positive, negative))
        }
    }
}

fn hybrid_residual_sign(
    x: u128,
    y: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<ResidualSign> {
    let (positive, negative) = hybrid_residual_terms(x, y, d, peak_depth_nad, imbalance_scale_nad)?;
    Ok(if positive >= negative {
        ResidualSign::NonNegative
    } else {
        ResidualSign::Negative
    })
}

fn base_to_common(base_amount_nad: u128, center_price_nad: u128) -> Result<u128> {
    mul_div_u128(base_amount_nad, center_price_nad, NAD as u128)
}

fn common_to_base_floor(common_amount_nad: u128, center_price_nad: u128) -> Result<u128> {
    mul_div_u128(common_amount_nad, NAD as u128, center_price_nad)
}

fn mul_div_u128_ceil(a: u128, b: u128, denominator: u128) -> Result<u128> {
    require!(denominator > 0, ErrorCode::DenominatorOverflow);
    let numerator = U256::from(a)
        .checked_mul(U256::from(b))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let denominator = U256::from(denominator);
    let value = if numerator.is_zero() {
        U256::zero()
    } else {
        (numerator - U256::one()) / denominator + U256::one()
    };
    u256_to_u128(value)
}

fn common_to_base_ceil(common_amount_nad: u128, center_price_nad: u128) -> Result<u128> {
    mul_div_u128_ceil(common_amount_nad, NAD as u128, center_price_nad)
}

fn base_to_common_ceil(base_amount_nad: u128, center_price_nad: u128) -> Result<u128> {
    mul_div_u128_ceil(base_amount_nad, center_price_nad, NAD as u128)
}

fn normalize_reserves_unbounded(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
) -> Result<(u128, u128)> {
    let base_common = base_to_common(base_reserve_nad, center_price_nad)?;
    validate_positive_reserves(base_common, quote_reserve_nad)?;
    Ok((base_common, quote_reserve_nad))
}

fn normalize_reserves(base_reserve_nad: u128, quote_reserve_nad: u128, center_price_nad: u128) -> Result<(u128, u128)> {
    let (base_common, quote_common) =
        normalize_reserves_unbounded(base_reserve_nad, quote_reserve_nad, center_price_nad)?;
    validate_common_reserves(base_common, quote_common)?;
    Ok((base_common, quote_common))
}

fn normalize_reserves_for_curve(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
) -> Result<(u128, u128)> {
    if peak_depth_nad == 0 {
        return normalize_reserves_unbounded(base_reserve_nad, quote_reserve_nad, center_price_nad);
    }
    normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)
}

/// Classifies raw NAD-normalized asset reserves against the protocol-fixed
/// hybrid shoulder at `center_price_nad`.
///
/// This is the canonical branch check for controller and risk code. Keeping
/// normalization here prevents those callers from reproducing center-price
/// rounding differently from executable quotes.
pub(crate) fn concentrated_hybrid_branch(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<ConcentratedHybridBranch> {
    let (base_common, quote_common) =
        normalize_reserves_for_curve(base_reserve_nad, quote_reserve_nad, center_price_nad, peak_depth_nad)?;
    concentrated_hybrid_branch_from_common(base_common, quote_common, peak_depth_nad, imbalance_scale_nad)
}

fn exact_cpmm_tail_in_raw(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_in_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<Option<u128>> {
    let (base_common, quote_common) = normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)?;
    let start = concentrated_hybrid_branch_from_common(base_common, quote_common, peak_depth_nad, imbalance_scale_nad)?;
    if start == ConcentratedHybridBranch::Inner {
        return Ok(None);
    }
    let output = match direction {
        ConcentratedSwapDirection::BaseToQuote => {
            calculate_normalized_amount_out(base_reserve_nad, quote_reserve_nad, amount_in_nad)?
        }
        ConcentratedSwapDirection::QuoteToBase => {
            calculate_normalized_amount_out(quote_reserve_nad, base_reserve_nad, amount_in_nad)?
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
    let end = concentrated_hybrid_branch_from_common(
        base_after_common,
        quote_after_common,
        peak_depth_nad,
        imbalance_scale_nad,
    )?;
    Ok(remains_on_same_tail(start, end).then_some(output))
}

fn exact_cpmm_tail_out_raw(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_out_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<Option<u128>> {
    let (base_common, quote_common) = normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)?;
    let start = concentrated_hybrid_branch_from_common(base_common, quote_common, peak_depth_nad, imbalance_scale_nad)?;
    if start == ConcentratedHybridBranch::Inner {
        return Ok(None);
    }
    let input = match direction {
        ConcentratedSwapDirection::BaseToQuote => {
            calculate_normalized_amount_in(base_reserve_nad, quote_reserve_nad, amount_out_nad)?
        }
        ConcentratedSwapDirection::QuoteToBase => {
            calculate_normalized_amount_in(quote_reserve_nad, base_reserve_nad, amount_out_nad)?
        }
    };
    let (base_after, quote_after) = match direction {
        ConcentratedSwapDirection::BaseToQuote => (
            base_reserve_nad
                .checked_add(input)
                .ok_or(ErrorCode::InvariantOverflow)?,
            quote_reserve_nad
                .checked_sub(amount_out_nad)
                .ok_or(ErrorCode::InsufficientLiquidity)?,
        ),
        ConcentratedSwapDirection::QuoteToBase => (
            base_reserve_nad
                .checked_sub(amount_out_nad)
                .ok_or(ErrorCode::InsufficientLiquidity)?,
            quote_reserve_nad
                .checked_add(input)
                .ok_or(ErrorCode::InvariantOverflow)?,
        ),
    };
    let (base_after_common, quote_after_common) = normalize_reserves(base_after, quote_after, center_price_nad)?;
    let end = concentrated_hybrid_branch_from_common(
        base_after_common,
        quote_after_common,
        peak_depth_nad,
        imbalance_scale_nad,
    )?;
    Ok(remains_on_same_tail(start, end).then_some(input))
}

/// Evaluates the exact rational invariant residual without division.
///
/// Let `N=NAD`, `P=peak_depth_nad`, `S=imbalance_scale_nad`,
/// `Q=4*x*y`, `E=D^2-Q`, `B=S*D^2+N*E`, and `H=x+y-D`.
/// Clearing the strictly positive fixed-point denominator gives:
/// ```text
/// R = 2*P*Q*S^2*D^3*H - N*E*B^2
/// ```
/// `residual_terms` returns the positive and negative magnitudes separately,
/// including for integer probes just below `2*sqrt(x*y)` where `E` is signed.
/// This avoids rounding an effective amplification before root finding.
fn residual_terms(x: u128, y: u128, d: u128, peak_depth_nad: u128, imbalance_scale_nad: u128) -> Result<(U512, U512)> {
    #[cfg(test)]
    RESIDUAL_EVALUATIONS.with(|count| count.set(count.get() + 1));

    validate_common_reserves(x, y)?;
    require!(d > 0, ErrorCode::InvalidArgument);

    let four_xy = checked_mul_512(U512::from(4_u8), checked_mul_512(U512::from(x), U512::from(y))?)?;
    let d_squared = checked_mul_512(U512::from(d), U512::from(d))?;
    if peak_depth_nad == 0 {
        return Ok((
            checked_mul_512(four_xy, U512::from(NAD))?,
            checked_mul_512(U512::from(NAD), d_squared)?,
        ));
    }

    let (e_nonnegative, e_magnitude) = if d_squared >= four_xy {
        (true, d_squared - four_xy)
    } else {
        (false, four_xy - d_squared)
    };
    let scale_d_squared = checked_mul_512(U512::from(imbalance_scale_nad), d_squared)?;
    let scaled_e = checked_mul_512(U512::from(NAD), e_magnitude)?;
    let (b_nonnegative, b_magnitude) = if e_nonnegative {
        (true, checked_add_512(scale_d_squared, scaled_e)?)
    } else if scale_d_squared >= scaled_e {
        (true, scale_d_squared - scaled_e)
    } else {
        (false, scaled_e - scale_d_squared)
    };
    // Only B^2 enters the residual; retain the sign above for the exact
    // reserve partial derivative used by marginal-price proofs.
    let _ = b_nonnegative;
    let b_squared = checked_mul_512(b_magnitude, b_magnitude)?;

    let sum = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
    let d_cubed = checked_mul_512(d_squared, U512::from(d))?;
    let scale_squared = checked_mul_512(U512::from(imbalance_scale_nad), U512::from(imbalance_scale_nad))?;
    let concentration = checked_mul_512(
        U512::from(2_u8),
        checked_mul_512(
            checked_mul_512(
                checked_mul_512(checked_mul_512(U512::from(peak_depth_nad), four_xy)?, scale_squared)?,
                d_cubed,
            )?,
            U512::from(sum.abs_diff(d)),
        )?,
    )?;

    let cpmm = checked_mul_512(checked_mul_512(U512::from(NAD), e_magnitude)?, b_squared)?;
    let mut positive = U512::zero();
    let mut negative = U512::zero();
    if sum >= d {
        positive = checked_add_512(positive, concentration)?;
    } else {
        negative = checked_add_512(negative, concentration)?;
    }
    if e_nonnegative {
        negative = checked_add_512(negative, cpmm)?;
    } else {
        positive = checked_add_512(positive, cpmm)?;
    }
    Ok((positive, negative))
}

/// Exact signed derivative of the cleared residual with respect to `D`.
/// Newton uses it only on the decreasing root branch; the global sign bracket
/// remains authoritative when the derivative is unusable.
fn invariant_residual_derivative(
    x: u128,
    y: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<(ResidualSign, U512)> {
    if peak_depth_nad == 0 {
        return Ok((
            ResidualSign::Negative,
            checked_mul_512(U512::from(2 * NAD as u128), U512::from(d))?,
        ));
    }

    let d_squared = checked_mul_512(U512::from(d), U512::from(d))?;
    let four_xy = checked_mul_512(U512::from(4_u8), checked_mul_512(U512::from(x), U512::from(y))?)?;
    let (e_nonnegative, e_magnitude) = if d_squared >= four_xy {
        (true, d_squared - four_xy)
    } else {
        (false, four_xy - d_squared)
    };
    let scale_d_squared = checked_mul_512(U512::from(imbalance_scale_nad), d_squared)?;
    let scaled_e = checked_mul_512(U512::from(NAD), e_magnitude)?;
    let (b_nonnegative, b_magnitude) = if e_nonnegative {
        (true, checked_add_512(scale_d_squared, scaled_e)?)
    } else if scale_d_squared >= scaled_e {
        (true, scale_d_squared - scaled_e)
    } else {
        (false, scaled_e - scale_d_squared)
    };

    // 2*P*S^2*Q*D^2*(3*(x+y)-4*D)
    let sum = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
    let three_sum = sum.checked_mul(3).ok_or(ErrorCode::InvariantOverflow)?;
    let four_d = d.checked_mul(4).ok_or(ErrorCode::InvariantOverflow)?;
    let scale_squared = checked_mul_512(U512::from(imbalance_scale_nad), U512::from(imbalance_scale_nad))?;
    let concentration = checked_mul_512(
        U512::from(2_u8),
        checked_mul_512(
            checked_mul_512(
                checked_mul_512(checked_mul_512(U512::from(peak_depth_nad), scale_squared)?, four_xy)?,
                d_squared,
            )?,
            U512::from(three_sum.abs_diff(four_d)),
        )?,
    )?;

    // -2*N*D*[B^2 + 2*(S+N)*E*B]
    let b_squared = checked_mul_512(b_magnitude, b_magnitude)?;
    let direct = checked_mul_512(checked_mul_512(U512::from(2 * NAD as u128), U512::from(d))?, b_squared)?;
    let scale_plus_one = imbalance_scale_nad
        .checked_add(NAD as u128)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let interaction = checked_mul_512(
        U512::from(4_u8),
        checked_mul_512(
            checked_mul_512(
                checked_mul_512(U512::from(NAD), U512::from(d))?,
                U512::from(scale_plus_one),
            )?,
            checked_mul_512(e_magnitude, b_magnitude)?,
        )?,
    )?;

    let mut positive = U512::zero();
    let mut negative = direct;
    if three_sum >= four_d {
        positive = checked_add_512(positive, concentration)?;
    } else {
        negative = checked_add_512(negative, concentration)?;
    }
    if e_nonnegative == b_nonnegative {
        negative = checked_add_512(negative, interaction)?;
    } else {
        positive = checked_add_512(positive, interaction)?;
    }
    Ok((
        if positive >= negative {
            ResidualSign::NonNegative
        } else {
            ResidualSign::Negative
        },
        residual_magnitude(positive, negative),
    ))
}

/// Evaluates the invariant residual and its D derivative from one shared set
/// of degree-eight intermediates. A warm successor Newton step needs both at
/// the same D; computing them together avoids rebuilding `D²`, `4xy`, `E`,
/// `B`, `B²`, and the concentration factors on SBF. The returned values are
/// algebraically identical to `residual_terms` plus
/// `invariant_residual_derivative`.
fn invariant_residual_and_derivative(
    x: u128,
    y: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<((U512, U512), (ResidualSign, U512))> {
    #[cfg(test)]
    RESIDUAL_EVALUATIONS.with(|count| count.set(count.get() + 1));

    validate_common_reserves(x, y)?;
    require!(d > 0, ErrorCode::InvalidArgument);
    let d_squared = checked_mul_512(U512::from(d), U512::from(d))?;
    let four_xy = checked_mul_512(U512::from(4_u8), checked_mul_512(U512::from(x), U512::from(y))?)?;
    if peak_depth_nad == 0 {
        let terms = (
            checked_mul_512(four_xy, U512::from(NAD))?,
            checked_mul_512(U512::from(NAD), d_squared)?,
        );
        let derivative = (
            ResidualSign::Negative,
            checked_mul_512(U512::from(2 * NAD as u128), U512::from(d))?,
        );
        return Ok((terms, derivative));
    }

    let (e_nonnegative, e_magnitude) = if d_squared >= four_xy {
        (true, d_squared - four_xy)
    } else {
        (false, four_xy - d_squared)
    };
    let scale_d_squared = checked_mul_512(U512::from(imbalance_scale_nad), d_squared)?;
    let scaled_e = checked_mul_512(U512::from(NAD), e_magnitude)?;
    let (b_nonnegative, b_magnitude) = if e_nonnegative {
        (true, checked_add_512(scale_d_squared, scaled_e)?)
    } else if scale_d_squared >= scaled_e {
        (true, scale_d_squared - scaled_e)
    } else {
        (false, scaled_e - scale_d_squared)
    };
    let b_squared = checked_mul_512(b_magnitude, b_magnitude)?;
    let sum = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
    let scale_squared = checked_mul_512(U512::from(imbalance_scale_nad), U512::from(imbalance_scale_nad))?;
    let shared_concentration = checked_mul_512(
        U512::from(2_u8),
        checked_mul_512(
            checked_mul_512(checked_mul_512(U512::from(peak_depth_nad), four_xy)?, scale_squared)?,
            d_squared,
        )?,
    )?;

    let residual_concentration = checked_mul_512(
        checked_mul_512(shared_concentration, U512::from(d))?,
        U512::from(sum.abs_diff(d)),
    )?;
    let residual_cpmm = checked_mul_512(checked_mul_512(U512::from(NAD), e_magnitude)?, b_squared)?;
    let mut residual_positive = U512::zero();
    let mut residual_negative = U512::zero();
    if sum >= d {
        residual_positive = checked_add_512(residual_positive, residual_concentration)?;
    } else {
        residual_negative = checked_add_512(residual_negative, residual_concentration)?;
    }
    if e_nonnegative {
        residual_negative = checked_add_512(residual_negative, residual_cpmm)?;
    } else {
        residual_positive = checked_add_512(residual_positive, residual_cpmm)?;
    }

    let three_sum = sum.checked_mul(3).ok_or(ErrorCode::InvariantOverflow)?;
    let four_d = d.checked_mul(4).ok_or(ErrorCode::InvariantOverflow)?;
    let derivative_concentration = checked_mul_512(shared_concentration, U512::from(three_sum.abs_diff(four_d)))?;
    let derivative_direct = checked_mul_512(checked_mul_512(U512::from(2 * NAD as u128), U512::from(d))?, b_squared)?;
    let scale_plus_one = imbalance_scale_nad
        .checked_add(NAD as u128)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let derivative_interaction = checked_mul_512(
        U512::from(4_u8),
        checked_mul_512(
            checked_mul_512(
                checked_mul_512(U512::from(NAD), U512::from(d))?,
                U512::from(scale_plus_one),
            )?,
            checked_mul_512(e_magnitude, b_magnitude)?,
        )?,
    )?;
    let mut derivative_positive = U512::zero();
    let mut derivative_negative = derivative_direct;
    if three_sum >= four_d {
        derivative_positive = checked_add_512(derivative_positive, derivative_concentration)?;
    } else {
        derivative_negative = checked_add_512(derivative_negative, derivative_concentration)?;
    }
    if e_nonnegative == b_nonnegative {
        derivative_negative = checked_add_512(derivative_negative, derivative_interaction)?;
    } else {
        derivative_positive = checked_add_512(derivative_positive, derivative_interaction)?;
    }
    Ok((
        (residual_positive, residual_negative),
        (
            if derivative_positive >= derivative_negative {
                ResidualSign::NonNegative
            } else {
                ResidualSign::Negative
            },
            residual_magnitude(derivative_positive, derivative_negative),
        ),
    ))
}

/// Exact signed reserve-partial cores at fixed D.
///
/// The full derivatives are `4*y*x_core` and `4*x*y_core`. Marginal-price
/// evaluation consumes their ratio, so returning the cores cancels the common
/// factor `4` and applies `x/y` only once. This is algebraically exact and
/// avoids four redundant U512 multiplications per certified D endpoint.
fn reserve_residual_derivatives(
    x: u128,
    y: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<((ResidualSign, U512), (ResidualSign, U512))> {
    if peak_depth_nad == 0 {
        let x_derivative = checked_mul_512(U512::from(4 * NAD as u128), U512::from(y))?;
        let y_derivative = checked_mul_512(U512::from(4 * NAD as u128), U512::from(x))?;
        return Ok((
            (ResidualSign::NonNegative, x_derivative),
            (ResidualSign::NonNegative, y_derivative),
        ));
    }

    let d_squared = checked_mul_512(U512::from(d), U512::from(d))?;
    let four_xy = checked_mul_512(U512::from(4_u8), checked_mul_512(U512::from(x), U512::from(y))?)?;
    let (e_nonnegative, e_magnitude) = if d_squared >= four_xy {
        (true, d_squared - four_xy)
    } else {
        (false, four_xy - d_squared)
    };
    let scale_d_squared = checked_mul_512(U512::from(imbalance_scale_nad), d_squared)?;
    let scaled_e = checked_mul_512(U512::from(NAD), e_magnitude)?;
    let (b_nonnegative, b_magnitude) = if e_nonnegative {
        (true, checked_add_512(scale_d_squared, scaled_e)?)
    } else if scale_d_squared >= scaled_e {
        (true, scale_d_squared - scaled_e)
    } else {
        (false, scaled_e - scale_d_squared)
    };

    // dR/dv = 4*fixed * [
    //   2*P*S^2*D^3*(fixed + 2*variable - D)
    //   + N*B^2 + 2*N^2*E*B
    // ].
    let d_cubed = checked_mul_512(d_squared, U512::from(d))?;
    let scale_squared = checked_mul_512(U512::from(imbalance_scale_nad), U512::from(imbalance_scale_nad))?;
    let concentration_factor = checked_mul_512(
        U512::from(2_u8),
        checked_mul_512(checked_mul_512(U512::from(peak_depth_nad), scale_squared)?, d_cubed)?,
    )?;
    let b_squared = checked_mul_512(b_magnitude, b_magnitude)?;
    let direct = checked_mul_512(U512::from(NAD), b_squared)?;
    let interaction = checked_mul_512(
        U512::from(2_u8),
        checked_mul_512(
            checked_mul_512(U512::from(NAD), U512::from(NAD))?,
            checked_mul_512(e_magnitude, b_magnitude)?,
        )?,
    )?;

    let x_coordinate = y
        .checked_add(x.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let y_coordinate = x
        .checked_add(y.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?)
        .ok_or(ErrorCode::InvariantOverflow)?;

    let evaluate_partial = |coordinate: u128| -> Result<(ResidualSign, U512)> {
        let concentration = checked_mul_512(concentration_factor, U512::from(coordinate.abs_diff(d)))?;
        let mut positive = direct;
        let mut negative = U512::zero();
        if coordinate >= d {
            positive = checked_add_512(positive, concentration)?;
        } else {
            negative = checked_add_512(negative, concentration)?;
        }
        if e_nonnegative == b_nonnegative {
            positive = checked_add_512(positive, interaction)?;
        } else {
            negative = checked_add_512(negative, interaction)?;
        }
        Ok((
            if positive >= negative {
                ResidualSign::NonNegative
            } else {
                ResidualSign::Negative
            },
            residual_magnitude(positive, negative),
        ))
    };

    Ok((evaluate_partial(x_coordinate)?, evaluate_partial(y_coordinate)?))
}

/// Exact positive reserve-partial cores for a certified inner `D_high`.
///
/// The signed derivative above is valid for an arbitrary D probe. Executable
/// marginals have a stronger certificate: `4*x*y <= D_high^2` and
/// `D_high <= x+y`. Therefore `E = D^2-4xy`, `H = x+y-D`, and
/// `B = S*D^2+N*E` are all non-negative. Their common interaction factors as
///
/// ```text
/// N*B^2 + 2*N^2*E*B = N*B*(S*D^2 + 3*N*E).
/// ```
///
/// The old concentration core has coefficient `2*P*S^2*D^3`. Since
/// `N == 1e9` is even, both complete cores share an exact factor two. These
/// returned cores divide that factor out; their ratio and every floored
/// marginal price are bit-identical to the signed U512 formulation.
fn canonical_high_reserve_residual_derivative_cores(
    x: u128,
    y: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<(U512, U512)> {
    validate_common_reserves(x, y)?;
    require!(d > 0 && peak_depth_nad > 0, ErrorCode::InvalidArgument);

    let sum = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
    require!(d <= sum, ErrorCode::InvariantOverflow);
    let y_u64 = y as u64;
    let scale_u64 = u64::try_from(imbalance_scale_nad).map_err(|_| ErrorCode::InvariantOverflow)?;

    // The certified bounds above prove every following core intermediate is
    // below 2^358. Plain U512 arithmetic avoids rebuilding overflow flags for
    // operations whose safety follows from that certificate.
    let d_squared = mul_512_u128_proven(U512::from(d), d);
    // x,y <= u64::MAX makes this shift exact with more than 380 spare bits.
    let four_xy = (U512::from(x) * y_u64) << 2;
    require!(d_squared >= four_xy, ErrorCode::InvariantOverflow);

    let h = sum - d;

    let e = d_squared - four_xy;
    let scale_d_squared = d_squared * scale_u64;
    let scaled_e = e * NAD;
    let b = scale_d_squared + scaled_e;
    let three_scaled_e = scaled_e + scaled_e + scaled_e;
    let b_plus_two_scaled_e = scale_d_squared + three_scaled_e;
    let interaction = (b * (NAD / 2)) * b_plus_two_scaled_e;

    // P*S^2 < 2^97 over the protocol parameter domain, so build that
    // coefficient in native u128 before its one required wide product.
    let concentration_coefficient = peak_depth_nad
        .checked_mul(imbalance_scale_nad)
        .and_then(|value| value.checked_mul(imbalance_scale_nad))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let d_cubed = mul_512_u128_proven(d_squared, d);
    let concentration = mul_512_u128_proven(d_cubed, concentration_coefficient);
    let x_coordinate = h.checked_add(x).ok_or(ErrorCode::InvariantOverflow)?;
    let y_coordinate = h.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;
    let x_core = mul_512_u128_proven(concentration, x_coordinate) + interaction;
    let y_core = mul_512_u128_proven(concentration, y_coordinate) + interaction;
    require!(!x_core.is_zero() && !y_core.is_zero(), ErrorCode::InvariantOverflow);
    Ok((x_core, y_core))
}

/// Exact signed partial derivative with respect to `variable`, holding
/// `fixed` and D constant. Reserve root-finding needs only one partial, while
/// marginal-price evaluation uses the shared two-partial routine above.
fn variable_residual_derivative(
    fixed: u128,
    variable: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<(ResidualSign, U512)> {
    if peak_depth_nad == 0 {
        let derivative = checked_mul_512(U512::from(4 * NAD as u128), U512::from(fixed))?;
        return Ok((ResidualSign::NonNegative, derivative));
    }

    let d_squared = checked_mul_512(U512::from(d), U512::from(d))?;
    let four_fixed_variable = checked_mul_512(
        U512::from(4_u8),
        checked_mul_512(U512::from(fixed), U512::from(variable))?,
    )?;
    let (e_nonnegative, e_magnitude) = if d_squared >= four_fixed_variable {
        (true, d_squared - four_fixed_variable)
    } else {
        (false, four_fixed_variable - d_squared)
    };
    let scale_d_squared = checked_mul_512(U512::from(imbalance_scale_nad), d_squared)?;
    let scaled_e = checked_mul_512(U512::from(NAD), e_magnitude)?;
    let (b_nonnegative, b_magnitude) = if e_nonnegative {
        (true, checked_add_512(scale_d_squared, scaled_e)?)
    } else if scale_d_squared >= scaled_e {
        (true, scale_d_squared - scaled_e)
    } else {
        (false, scaled_e - scale_d_squared)
    };

    let coordinate = fixed
        .checked_add(variable.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let d_cubed = checked_mul_512(d_squared, U512::from(d))?;
    let scale_squared = checked_mul_512(U512::from(imbalance_scale_nad), U512::from(imbalance_scale_nad))?;
    let concentration = checked_mul_512(
        U512::from(2_u8),
        checked_mul_512(
            checked_mul_512(checked_mul_512(U512::from(peak_depth_nad), scale_squared)?, d_cubed)?,
            U512::from(coordinate.abs_diff(d)),
        )?,
    )?;
    let b_squared = checked_mul_512(b_magnitude, b_magnitude)?;
    let direct = checked_mul_512(U512::from(NAD), b_squared)?;
    let interaction = checked_mul_512(
        U512::from(2_u8),
        checked_mul_512(
            checked_mul_512(U512::from(NAD), U512::from(NAD))?,
            checked_mul_512(e_magnitude, b_magnitude)?,
        )?,
    )?;

    let mut positive = direct;
    let mut negative = U512::zero();
    if coordinate >= d {
        positive = checked_add_512(positive, concentration)?;
    } else {
        negative = checked_add_512(negative, concentration)?;
    }
    if e_nonnegative == b_nonnegative {
        positive = checked_add_512(positive, interaction)?;
    } else {
        negative = checked_add_512(negative, interaction)?;
    }
    let multiplier = checked_mul_512(U512::from(4_u8), U512::from(fixed))?;
    positive = checked_mul_512(positive, multiplier)?;
    negative = checked_mul_512(negative, multiplier)?;

    Ok((
        if positive >= negative {
            ResidualSign::NonNegative
        } else {
            ResidualSign::Negative
        },
        residual_magnitude(positive, negative),
    ))
}

fn hybrid_variable_residual_derivative(
    fixed: u128,
    variable: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<(ResidualSign, U512)> {
    match concentrated_hybrid_branch_from_common(fixed, variable, peak_depth_nad, imbalance_scale_nad)? {
        ConcentratedHybridBranch::Inner => {
            variable_residual_derivative(fixed, variable, d, peak_depth_nad, imbalance_scale_nad)
        }
        ConcentratedHybridBranch::BaseScarceTail | ConcentratedHybridBranch::QuoteScarceTail => Ok((
            ResidualSign::NonNegative,
            checked_mul_512(U512::from(4 * NAD as u128), U512::from(fixed))?,
        )),
    }
}

fn residual_magnitude(positive: U512, negative: U512) -> U512 {
    if positive >= negative {
        positive - negative
    } else {
        negative - positive
    }
}

fn u512_to_u128_saturating(value: U512) -> u128 {
    if value > U512::from(u128::MAX) {
        u128::MAX
    } else {
        value.as_u128()
    }
}

fn sqrt_u256_to_u128(value: U256) -> Result<u128> {
    if value.is_zero() {
        return Ok(0);
    }

    // Start at a power-of-two upper bound and use the monotone integer
    // Babylonian method. It converges quadratically and normally takes fewer
    // than ten rounds for U256 inputs, versus 128 rounds for binary search.
    let mut root = U256::one() << value.bits().div_ceil(2);
    for _ in 0..128 {
        let next = root.checked_add(value / root).ok_or(ErrorCode::InvariantOverflow)? >> 1;
        if next >= root {
            return u256_to_u128(root);
        }
        root = next;
    }
    err!(ErrorCode::InvariantOverflow)
}

type CachedInvariantNewtonEvaluation = ((U512, U512), (ResidualSign, U512));

/// Tightens an already certified invariant sign bracket. Callers establish a
/// non-negative residual at `low` and a negative residual at `high` (or return
/// an exact singleton before entering). Safeguarded Newton steps accelerate
/// convergence, but only exact residual classifications are allowed to move a
/// bound.
fn refine_invariant_common_bracket(
    x: u128,
    y: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    mut low: u128,
    mut high: u128,
    mut current: u128,
    mut cached_current_evaluation: Option<CachedInvariantNewtonEvaluation>,
    proof_denominator: u128,
) -> Result<(u128, u128, u128)> {
    require!(low < high, ErrorCode::InvariantOverflow);
    require!(current >= low && current <= high, ErrorCode::InvariantOverflow);

    for _ in 0..CONCENTRATED_INVARIANT_MAX_ITERS {
        let proof_width = mul_div_u128_ceil(high, CONCENTRATED_INVARIANT_PROOF_PARTS, proof_denominator)?.max(1);
        if high - low <= proof_width {
            break;
        }
        let ((positive, negative), cached_derivative) = if let Some(evaluation) = cached_current_evaluation.take() {
            (evaluation.0, Some(evaluation.1))
        } else {
            let evaluation = invariant_residual_and_derivative(x, y, current, peak_depth_nad, imbalance_scale_nad)?;
            (evaluation.0, Some(evaluation.1))
        };
        let sign = if positive >= negative {
            ResidualSign::NonNegative
        } else {
            ResidualSign::Negative
        };
        if sign == ResidualSign::NonNegative {
            low = current;
        } else {
            high = current;
        }
        let proof_width = mul_div_u128_ceil(high, CONCENTRATED_INVARIANT_PROOF_PARTS, proof_denominator)?.max(1);
        if high - low <= proof_width {
            break;
        }
        let (derivative_sign, derivative) = if let Some(derivative) = cached_derivative {
            derivative
        } else {
            invariant_residual_derivative(x, y, current, peak_depth_nad, imbalance_scale_nad)?
        };
        let delta = if derivative_sign == ResidualSign::Negative && !derivative.is_zero() {
            u512_to_u128_saturating(residual_magnitude(positive, negative) / derivative).max(1)
        } else {
            0
        };
        let newton = match sign {
            ResidualSign::NonNegative => current.saturating_add(delta),
            ResidualSign::Negative => current.saturating_sub(delta),
        };
        current = if delta == 0 || newton <= low || newton >= high {
            low + (high - low) / 2
        } else {
            newton
        };
    }
    let proof_width = mul_div_u128_ceil(high, CONCENTRATED_INVARIANT_PROOF_PARTS, proof_denominator)?.max(1);
    require!(high - low <= proof_width, ErrorCode::InvariantOverflow);
    // Boundary signs are preserved inductively: a bound moves only after the
    // exact residual evaluation above classifies the replacement point.
    let midpoint = low + (high - low) / 2;
    Ok((low, high, midpoint))
}

fn invariant_common_bracket_with_hint(
    x: u128,
    y: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    initial_d_hint: Option<u128>,
    proof_denominator: u128,
) -> Result<(u128, u128, u128, Ordering)> {
    validate_positive_reserves(x, y)?;
    if peak_depth_nad == 0 {
        let product = U256::from(x)
            .checked_mul(U256::from(y))
            .ok_or(ErrorCode::InvariantOverflow)?;
        let d = sqrt_u256_to_u128(product)?
            .checked_mul(2)
            .ok_or(ErrorCode::InvariantOverflow)?;
        return Ok((d, d, d, Ordering::Greater));
    }
    validate_common_reserves(x, y)?;

    let shoulder_relation = concentrated_hybrid_shoulder_relation(x, y, peak_depth_nad, imbalance_scale_nad)?;
    validate_inner_common_reserve_floor(x, y, shoulder_relation)?;
    if shoulder_relation == Ordering::Less {
        // Exact CPMM tail: 4*x*y*NAD = D^2*(NAD-imbalance_scale).
        // Taking sqrt after integer division preserves the exact floor of the
        // rational square root. The cross-product determines whether the
        // upper endpoint is the same integer or the next one.
        let numerator = U256::from(4_u8)
            .checked_mul(U256::from(x))
            .and_then(|value| value.checked_mul(U256::from(y)))
            .and_then(|value| value.checked_mul(U256::from(NAD)))
            .ok_or(ErrorCode::InvariantOverflow)?;
        let denominator = U256::from(
            (NAD as u128)
                .checked_sub(imbalance_scale_nad)
                .ok_or(ErrorCode::InvariantOverflow)?,
        );
        let low = sqrt_u256_to_u128(numerator / denominator)?;
        let low_squared_scaled = U256::from(low)
            .checked_mul(U256::from(low))
            .and_then(|value| value.checked_mul(denominator))
            .ok_or(ErrorCode::InvariantOverflow)?;
        let high = if low_squared_scaled == numerator {
            low
        } else {
            low.checked_add(1).ok_or(ErrorCode::InvariantOverflow)?
        };
        return Ok((low, high, low, shoulder_relation));
    }

    let product = U256::from(x)
        .checked_mul(U256::from(y))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let geometric = sqrt_u256_to_u128(product)?;
    let mut low = geometric.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    let mut high = x.checked_add(y).ok_or(ErrorCode::InvariantOverflow)?;

    // These boundary signs follow directly from the cleared equation and do
    // not need an expensive U512 evaluation. At `low=2*floor(sqrt(x*y))`,
    // `E=D²-4xy<=0` and `H=x+y-D>=0`, so every residual term is
    // non-negative. At `high=x+y`, `H=0` and `E=(x-y)²>0`, so the residual is
    // negative. Equality is exactly the balanced singleton.
    if low == high {
        return Ok((low, high, low, shoulder_relation));
    }

    // A warm certificate is only a first probe. Its sign may narrow the
    // global bracket, but it is never trusted as a root or boundary.
    let mut current = low + (high - low) / 2;
    let mut cached_current_evaluation = None;
    if let Some(hint) = initial_d_hint.filter(|hint| *hint > low && *hint < high) {
        let evaluation = invariant_residual_and_derivative(x, y, hint, peak_depth_nad, imbalance_scale_nad)?;
        let terms = evaluation.0;
        let hint_sign = if terms.0 >= terms.1 {
            ResidualSign::NonNegative
        } else {
            ResidualSign::Negative
        };
        if hint_sign == ResidualSign::NonNegative {
            low = hint;
        } else {
            high = hint;
        }
        // The previous certified D is normally extremely close after a swap,
        // retained-fee credit, small center step, or ramp point. Start Newton
        // at that warm point while the rebuilt global bracket remains the
        // authoritative safety proof.
        current = hint;
        // The first safeguarded-Newton round is at the same warm point. Reuse
        // its exact residual and derivative instead of rebuilding the shared
        // degree-eight U512 intermediates.
        cached_current_evaluation = Some(evaluation);
    }

    let (low, high, midpoint) = refine_invariant_common_bracket(
        x,
        y,
        peak_depth_nad,
        imbalance_scale_nad,
        low,
        high,
        current,
        cached_current_evaluation,
        proof_denominator,
    )?;
    Ok((low, high, midpoint, shoulder_relation))
}

fn invariant_common_bracket(
    x: u128,
    y: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<(u128, u128, u128)> {
    let (low, high, midpoint, _) = invariant_common_bracket_with_hint(
        x,
        y,
        peak_depth_nad,
        imbalance_scale_nad,
        None,
        INVARIANT_PROOF_DENOMINATOR,
    )?;
    Ok((low, high, midpoint))
}

#[cfg(test)]
fn invariant_common(x: u128, y: u128, peak_depth_nad: u128, imbalance_scale_nad: u128) -> Result<u128> {
    Ok(invariant_common_bracket(x, y, peak_depth_nad, imbalance_scale_nad)?.0)
}

fn prepare_curve_internal(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    initial_d_hint: Option<u128>,
    proof_denominator: u128,
) -> Result<ConcentratedPreparedCurve> {
    validate_parameters(center_price_nad, peak_depth_nad, imbalance_scale_nad)?;
    let (base_common, quote_common) =
        normalize_reserves_for_curve(base_reserve_nad, quote_reserve_nad, center_price_nad, peak_depth_nad)?;
    let (invariant_low, invariant_high, _, shoulder_relation) = invariant_common_bracket_with_hint(
        base_common,
        quote_common,
        peak_depth_nad,
        imbalance_scale_nad,
        initial_d_hint,
        proof_denominator,
    )?;
    Ok(ConcentratedPreparedCurve {
        base_reserve_nad,
        quote_reserve_nad,
        base_common,
        quote_common,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
        shoulder_relation,
        invariant_low,
        invariant_high,
    })
}

/// Normalizes reserves and certifies their unique invariant root once.
pub(crate) fn concentrated_prepare_curve(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<ConcentratedPreparedCurve> {
    prepare_curve_internal(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
        None,
        INVARIANT_PROOF_DENOMINATOR,
    )
}

/// Certifies a successor point while using a prior certified D only as the
/// first Newton candidate. The complete global sign bracket is rebuilt, so an
/// inaccurate hint can affect performance but cannot affect safety.
pub(crate) fn concentrated_prepare_curve_with_hint(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    invariant_d_hint: u128,
) -> Result<ConcentratedPreparedCurve> {
    prepare_curve_internal(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
        Some(invariant_d_hint),
        INVARIANT_PROOF_DENOMINATOR,
    )
}

/// Certifies a one-coordinate retained-fee successor from an authentic
/// predecessor bracket without rebuilding the global invariant interval.
///
/// The hybrid invariant is degree-one homogeneous and strictly increasing in
/// either common-coordinate reserve. If `x' > x` while `y` is fixed, then for
/// `lambda = x'/x`:
///
/// ```text
/// D(x, y) <= D(x', y) < D(lambda*x, lambda*y) = lambda*D(x, y).
/// ```
///
/// Thus the predecessor lower endpoint remains a successor lower bound, while
/// `ceil(predecessor_high*x'/x)` is a strict upper bound. The same proof
/// applies when only `y` increases. Exact residual signs still authorize every
/// interior Newton/bisection update. Tail successors keep their cheaper closed
/// form, and a floor-normalized no-op reuses the predecessor bracket.
///
/// The predecessor bracket must come from the same opaque curve certificate;
/// structural checks below bind it to the supplied predecessor identity.
#[allow(clippy::too_many_arguments)]
pub(crate) fn concentrated_prepare_continuous_successor_from_bracket(
    predecessor_base_reserve_nad: u128,
    predecessor_quote_reserve_nad: u128,
    successor_base_reserve_nad: u128,
    successor_quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    predecessor_invariant_low: u128,
    predecessor_invariant_high: u128,
) -> Result<ConcentratedPreparedCurve> {
    validate_parameters(center_price_nad, peak_depth_nad, imbalance_scale_nad)?;
    let successor_proof_denominator = if peak_depth_nad < CONCENTRATED_COARSE_SUCCESSOR_MIN_PEAK_DEPTH_NAD {
        INVARIANT_PROOF_DENOMINATOR
    } else {
        CONTINUOUS_SUCCESSOR_PROOF_DENOMINATOR
    };
    if peak_depth_nad == 0 {
        return prepare_curve_internal(
            successor_base_reserve_nad,
            successor_quote_reserve_nad,
            center_price_nad,
            peak_depth_nad,
            imbalance_scale_nad,
            None,
            successor_proof_denominator,
        );
    }

    let predecessor = concentrated_restore_prepared_curve_from_bracket(
        predecessor_base_reserve_nad,
        predecessor_quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
        predecessor_invariant_low,
        predecessor_invariant_high,
    )?;
    let (successor_base_common, successor_quote_common) = normalize_reserves_for_curve(
        successor_base_reserve_nad,
        successor_quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
    )?;
    let predecessor_base_common = predecessor.base_common_nad();
    let predecessor_quote_common = predecessor.quote_common_nad();

    if successor_base_common == predecessor_base_common && successor_quote_common == predecessor_quote_common {
        return concentrated_restore_prepared_curve_from_bracket(
            successor_base_reserve_nad,
            successor_quote_reserve_nad,
            center_price_nad,
            peak_depth_nad,
            imbalance_scale_nad,
            predecessor_invariant_low,
            predecessor_invariant_high,
        );
    }

    let base_increased =
        successor_base_common > predecessor_base_common && successor_quote_common == predecessor_quote_common;
    let quote_increased =
        successor_quote_common > predecessor_quote_common && successor_base_common == predecessor_base_common;
    if !(base_increased ^ quote_increased) {
        return prepare_curve_internal(
            successor_base_reserve_nad,
            successor_quote_reserve_nad,
            center_price_nad,
            peak_depth_nad,
            imbalance_scale_nad,
            None,
            successor_proof_denominator,
        );
    }

    let successor_shoulder_relation = concentrated_hybrid_shoulder_relation(
        successor_base_common,
        successor_quote_common,
        peak_depth_nad,
        imbalance_scale_nad,
    )?;
    validate_inner_common_reserve_floor(
        successor_base_common,
        successor_quote_common,
        successor_shoulder_relation,
    )?;
    if successor_shoulder_relation == Ordering::Less {
        return prepare_curve_internal(
            successor_base_reserve_nad,
            successor_quote_reserve_nad,
            center_price_nad,
            peak_depth_nad,
            imbalance_scale_nad,
            None,
            successor_proof_denominator,
        );
    }

    let successor_sum = successor_base_common
        .checked_add(successor_quote_common)
        .ok_or(ErrorCode::InvariantOverflow)?;
    if successor_base_common == successor_quote_common {
        return Ok(ConcentratedPreparedCurve {
            base_reserve_nad: successor_base_reserve_nad,
            quote_reserve_nad: successor_quote_reserve_nad,
            base_common: successor_base_common,
            quote_common: successor_quote_common,
            center_price_nad,
            peak_depth_nad,
            imbalance_scale_nad,
            shoulder_relation: successor_shoulder_relation,
            invariant_low: successor_sum,
            invariant_high: successor_sum,
        });
    }

    let (old_coordinate, new_coordinate) = if base_increased {
        (predecessor_base_common, successor_base_common)
    } else {
        (predecessor_quote_common, successor_quote_common)
    };
    let scaled_upper = {
        let numerator = U256::from(predecessor_invariant_high)
            .checked_mul(U256::from(new_coordinate))
            .ok_or(ErrorCode::InvariantOverflow)?;
        let denominator = U256::from(old_coordinate);
        let ceil = if numerator.is_zero() {
            U256::zero()
        } else {
            (numerator - U256::one()) / denominator + U256::one()
        };
        if ceil >= U256::from(successor_sum) {
            successor_sum
        } else {
            u256_to_u128(ceil)?
        }
    };
    let low = predecessor_invariant_low;
    let high = scaled_upper;
    if low >= high {
        return prepare_curve_internal(
            successor_base_reserve_nad,
            successor_quote_reserve_nad,
            center_price_nad,
            peak_depth_nad,
            imbalance_scale_nad,
            None,
            successor_proof_denominator,
        );
    }

    let predecessor_sum = predecessor_base_common
        .checked_add(predecessor_quote_common)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let sum_hint = mul_div_u128(predecessor_invariant_low, successor_sum, predecessor_sum)?;
    let current = if high - low > 1 {
        sum_hint.clamp(low + 1, high - 1)
    } else {
        low
    };
    let (invariant_low, invariant_high, _) = refine_invariant_common_bracket(
        successor_base_common,
        successor_quote_common,
        peak_depth_nad,
        imbalance_scale_nad,
        low,
        high,
        current,
        None,
        successor_proof_denominator,
    )?;
    Ok(ConcentratedPreparedCurve {
        base_reserve_nad: successor_base_reserve_nad,
        quote_reserve_nad: successor_quote_reserve_nad,
        base_common: successor_base_common,
        quote_common: successor_quote_common,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
        shoulder_relation: successor_shoulder_relation,
        invariant_low,
        invariant_high,
    })
}

/// Restores a previously certified bracket for the exact same program-owned
/// curve state without repeating its residual solve.
///
/// The caller must first bind the bracket to identical normalized reserves,
/// center, peak depth, and imbalance scale. This function still validates the arithmetic domain
/// and the solver's public proof-width contract, but deliberately does not
/// re-run residual signs: doing so would defeat the cache's compute saving.
pub(crate) fn concentrated_restore_prepared_curve_from_bracket(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    invariant_low: u128,
    invariant_high: u128,
) -> Result<ConcentratedPreparedCurve> {
    validate_parameters(center_price_nad, peak_depth_nad, imbalance_scale_nad)?;
    let (base_common, quote_common) =
        normalize_reserves_for_curve(base_reserve_nad, quote_reserve_nad, center_price_nad, peak_depth_nad)?;
    let shoulder_relation =
        concentrated_hybrid_shoulder_relation(base_common, quote_common, peak_depth_nad, imbalance_scale_nad)?;
    validate_inner_common_reserve_floor(base_common, quote_common, shoulder_relation)?;
    require!(
        invariant_low > 0 && invariant_low <= invariant_high,
        ErrorCode::BrokenInvariant
    );
    require!(
        invariant_high
            <= base_common
                .checked_add(quote_common)
                .ok_or(ErrorCode::InvariantOverflow)?,
        ErrorCode::BrokenInvariant
    );
    let proof_width = mul_div_u128_ceil(
        invariant_high,
        CONCENTRATED_INVARIANT_PROOF_PARTS,
        CONTINUOUS_SUCCESSOR_PROOF_DENOMINATOR,
    )?
    .max(1);
    require!(
        invariant_high - invariant_low <= proof_width,
        ErrorCode::BrokenInvariant
    );
    Ok(ConcentratedPreparedCurve {
        base_reserve_nad,
        quote_reserve_nad,
        base_common,
        quote_common,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
        shoulder_relation,
        invariant_low,
        invariant_high,
    })
}

/// Returns invariant D for arbitrary reserves and candidate center/peak-depth/imbalance-scale.
/// This function is read-only and is intended for recenter/ramp impairment
/// checks as well as swap execution.
#[cfg(test)]
pub(crate) fn concentrated_invariant(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    validate_parameters(center_price_nad, peak_depth_nad, imbalance_scale_nad)?;
    let (x, y) = normalize_reserves_for_curve(base_reserve_nad, quote_reserve_nad, center_price_nad, peak_depth_nad)?;
    invariant_common(x, y, peak_depth_nad, imbalance_scale_nad)
}

/// Brackets the smallest variable reserve with a non-negative residual.
/// `low` remains negative and `high` remains non-negative. Exact derivative
/// steps accelerate ordinary quotes; the sign bracket remains authoritative.
fn solve_variable_reserve(
    fixed: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    mut low: u128,
    mut high: u128,
    max_iters: usize,
) -> Result<(u128, u128)> {
    require!(low < high, ErrorCode::InvalidArgument);
    let low_sign = hybrid_residual_sign(fixed, low, d, peak_depth_nad, imbalance_scale_nad)?;
    let high_terms = hybrid_residual_terms(fixed, high, d, peak_depth_nad, imbalance_scale_nad)?;
    let high_sign = if high_terms.0 >= high_terms.1 {
        ResidualSign::NonNegative
    } else {
        ResidualSign::Negative
    };
    require!(
        low_sign == ResidualSign::Negative && high_sign == ResidualSign::NonNegative,
        ErrorCode::InvariantOverflow
    );
    let mut current = high;
    let mut cached_current_terms = Some(high_terms);
    for _ in 0..max_iters {
        if high - low <= 1 {
            break;
        }
        let (positive, negative) = if let Some(terms) = cached_current_terms.take() {
            terms
        } else {
            hybrid_residual_terms(fixed, current, d, peak_depth_nad, imbalance_scale_nad)?
        };
        let sign = if positive >= negative {
            ResidualSign::NonNegative
        } else {
            ResidualSign::Negative
        };
        if sign == ResidualSign::NonNegative {
            high = current;
        } else {
            low = current;
        }
        if high - low <= 1 {
            break;
        }

        let (derivative_sign, derivative) =
            hybrid_variable_residual_derivative(fixed, current, d, peak_depth_nad, imbalance_scale_nad)?;
        let delta = if derivative_sign == ResidualSign::NonNegative && !derivative.is_zero() {
            u512_to_u128_saturating(residual_magnitude(positive, negative) / derivative).max(1)
        } else {
            0
        };
        let newton = match sign {
            ResidualSign::NonNegative => current.saturating_sub(delta),
            ResidualSign::Negative => current.saturating_add(delta),
        };
        current = if delta == 0 || newton <= low || newton >= high {
            low + (high - low) / 2
        } else {
            newton
        };
    }
    Ok((low, high))
}

/// Proves that the returned upper reserve endpoint is within ten ppm of the
/// quoted trade amount. The upper endpoint is conservative for both exact-in
/// and exact-out; failure to prove its accuracy rejects the quote.
fn prove_variable_upper_bound(
    fixed: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
    low: u128,
    mut high: u128,
    trade_amount: u128,
) -> Result<u128> {
    if trade_amount == 0 {
        return Ok(high);
    }
    let proof_width = mul_div_u128_ceil(trade_amount, CONCENTRATED_RESERVE_PROOF_PPM, PPM_DENOMINATOR)?.max(1);
    if high - low <= proof_width {
        return Ok(high);
    }

    // A safeguarded Newton solve can finish with a certified non-negative
    // endpoint that is slightly more than one proof width above the root,
    // especially when the opposite reserve is deep in a faded-depth tail. Tighten
    // that endpoint only after proving the lower candidate is also
    // non-negative; otherwise the negative candidate proves the requested
    // error bound. Four bounded probes cover the certified extreme-domain
    // fixtures while preserving fail-closed behavior beyond that budget.
    for _ in 0..4 {
        let near_high = high.saturating_sub(proof_width).max(low);
        if hybrid_residual_sign(fixed, near_high, d, peak_depth_nad, imbalance_scale_nad)? == ResidualSign::Negative {
            return Ok(high);
        }
        high = near_high;
    }
    err!(ErrorCode::InvariantOverflow)
}

fn remains_on_same_tail(start: ConcentratedHybridBranch, end: ConcentratedHybridBranch) -> bool {
    matches!(
        (start, end),
        (
            ConcentratedHybridBranch::BaseScarceTail,
            ConcentratedHybridBranch::BaseScarceTail
        ) | (
            ConcentratedHybridBranch::QuoteScarceTail,
            ConcentratedHybridBranch::QuoteScarceTail
        )
    )
}

fn inner_output_along_increasing_x_path(
    start_branch: ConcentratedHybridBranch,
    end_branch: ConcentratedHybridBranch,
    y_before: u128,
    y_after: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    let raw_output = y_before.checked_sub(y_after).ok_or(ErrorCode::OutputAmountOverflow)?;
    let inner_output = match (start_branch, end_branch) {
        (ConcentratedHybridBranch::Inner, ConcentratedHybridBranch::Inner) => raw_output,
        (ConcentratedHybridBranch::Inner, ConcentratedHybridBranch::QuoteScarceTail) => {
            let (low_common, _, _) = concentrated_hybrid_shoulder_coordinates(d, peak_depth_nad, imbalance_scale_nad)?;
            y_before.saturating_sub(low_common)
        }
        (ConcentratedHybridBranch::BaseScarceTail, ConcentratedHybridBranch::Inner) => {
            let (_, high_common, _) = concentrated_hybrid_shoulder_coordinates(d, peak_depth_nad, imbalance_scale_nad)?;
            high_common.saturating_sub(y_after)
        }
        (ConcentratedHybridBranch::BaseScarceTail, ConcentratedHybridBranch::QuoteScarceTail) => {
            let (low_common, high_common, _) =
                concentrated_hybrid_shoulder_coordinates(d, peak_depth_nad, imbalance_scale_nad)?;
            high_common.saturating_sub(low_common)
        }
        // Increasing x cannot move from the inner/right branches into the
        // base-scarce tail. Same-tail paths are handled by exact CPMM before
        // this helper; return zero defensively for that already-proved case.
        _ => 0,
    };
    Ok(inner_output.min(raw_output))
}

fn quote_common_exact_in_with_d(
    x: u128,
    y: u128,
    dx: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    if dx == 0 {
        return Ok(0);
    }
    validate_common_reserves(x, y)?;
    let x_after = x.checked_add(dx).ok_or(ErrorCode::InvariantOverflow)?;
    validate_common_reserves(x_after, y)?;
    let start_branch = concentrated_hybrid_branch_from_common(x, y, peak_depth_nad, imbalance_scale_nad)?;
    if start_branch != ConcentratedHybridBranch::Inner {
        let cpmm_output = calculate_normalized_amount_out(x, y, dx)?;
        let cpmm_y_after = y.checked_sub(cpmm_output).ok_or(ErrorCode::OutputAmountOverflow)?;
        let end_branch =
            concentrated_hybrid_branch_from_common(x_after, cpmm_y_after, peak_depth_nad, imbalance_scale_nad)?;
        if remains_on_same_tail(start_branch, end_branch) {
            return Ok(cpmm_output);
        }
    }
    let (low, high) = solve_variable_reserve(
        x_after,
        d,
        peak_depth_nad,
        imbalance_scale_nad,
        1,
        y,
        CONCENTRATED_RESERVE_MAX_ITERS,
    )?;
    let provisional_output = y.checked_sub(high).ok_or(ErrorCode::OutputAmountOverflow)?;
    let y_after = prove_variable_upper_bound(
        x_after,
        d,
        peak_depth_nad,
        imbalance_scale_nad,
        low,
        high,
        provisional_output,
    )?;
    let raw_output = y.checked_sub(y_after).ok_or(ErrorCode::OutputAmountOverflow)?;
    let end_branch = concentrated_hybrid_branch_from_common(x_after, y_after, peak_depth_nad, imbalance_scale_nad)?;
    // The CPMM branch is solved exactly, so applying the concentrated solver
    // margin to an earlier/later CPMM segment would create a quote cliff at
    // the shoulder. Haircut only the output traversed on the inner branch.
    let inner_output = inner_output_along_increasing_x_path(
        start_branch,
        end_branch,
        y,
        y_after,
        d,
        peak_depth_nad,
        imbalance_scale_nad,
    )?;
    let safety_haircut = mul_div_u128_ceil(inner_output, CONCENTRATED_SOLVER_SAFETY_PPM, PPM_DENOMINATOR)?;
    raw_output
        .checked_sub(safety_haircut)
        .ok_or_else(|| ErrorCode::OutputAmountOverflow.into())
}

fn quote_common_exact_in(x: u128, y: u128, dx: u128, peak_depth_nad: u128, imbalance_scale_nad: u128) -> Result<u128> {
    // The upper invariant endpoint is conservative for swaps: holding the
    // post-trade input reserve fixed, a larger D requires a larger output
    // reserve. Never price from the lower endpoint because a loose lower
    // bracket can overpay badly in the fade transition.
    let d = invariant_common_bracket(x, y, peak_depth_nad, imbalance_scale_nad)?.1;
    quote_common_exact_in_with_d(x, y, dx, d, peak_depth_nad, imbalance_scale_nad)
}

/// Returns raw input endpoints bracketing the inverse of an unhaircut output.
/// The lower endpoint has a negative residual at `y-dy`; therefore feeding it
/// through exact-in produces strictly less raw output than `dy`. The upper
/// endpoint has a non-negative residual and therefore produces at least `dy`.
fn quote_common_raw_exact_out_bracket(
    x: u128,
    y: u128,
    dy: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<(u128, u128)> {
    require!(dy > 0 && dy < y, ErrorCode::InsufficientLiquidity);
    let d = invariant_common_bracket(x, y, peak_depth_nad, imbalance_scale_nad)?.1;
    let y_after = y.checked_sub(dy).ok_or(ErrorCode::InsufficientLiquidity)?;

    // Grow a CPMM-derived sign bracket, then solve that bracket independently.
    let cpmm_input = mul_div_u128_ceil(dy, x, y_after)?.max(1);
    let mut multiplier = 1_u128;
    let mut high = x;
    let mut bracket_evaluations = 0_usize;
    let mut bracketed = false;
    for _ in 0..CONCENTRATED_RESERVE_MAX_ITERS {
        high = x
            .checked_add(cpmm_input.checked_mul(multiplier).ok_or(ErrorCode::InvariantOverflow)?)
            .ok_or(ErrorCode::InvariantOverflow)?
            .min(MAX_COMMON_RESERVE);
        bracket_evaluations += 1;
        if hybrid_residual_sign(y_after, high, d, peak_depth_nad, imbalance_scale_nad)? == ResidualSign::NonNegative {
            bracketed = true;
            break;
        }
        require!(high < MAX_COMMON_RESERVE, ErrorCode::InsufficientLiquidity);
        multiplier = multiplier.checked_mul(2).ok_or(ErrorCode::InvariantOverflow)?;
    }
    require!(bracketed, ErrorCode::InvariantOverflow);
    let _ = bracket_evaluations;
    let (low, high) = solve_variable_reserve(
        y_after,
        d,
        peak_depth_nad,
        imbalance_scale_nad,
        x,
        high,
        CONCENTRATED_RESERVE_MAX_ITERS,
    )?;
    let provisional_input = high.checked_sub(x).ok_or(ErrorCode::OutputAmountOverflow)?;
    let proven_high = prove_variable_upper_bound(
        y_after,
        d,
        peak_depth_nad,
        imbalance_scale_nad,
        low,
        high,
        provisional_input,
    )?;
    Ok((
        low.checked_sub(x).ok_or(ErrorCode::OutputAmountOverflow)?,
        proven_high.checked_sub(x).ok_or(ErrorCode::OutputAmountOverflow)?,
    ))
}

fn quote_common_exact_out(x: u128, y: u128, dy: u128, peak_depth_nad: u128, imbalance_scale_nad: u128) -> Result<u128> {
    require!(dy < y, ErrorCode::InsufficientLiquidity);
    let start_branch = concentrated_hybrid_branch_from_common(x, y, peak_depth_nad, imbalance_scale_nad)?;
    if start_branch != ConcentratedHybridBranch::Inner {
        let cpmm_input = calculate_normalized_amount_in(x, y, dy)?;
        let cpmm_x_after = x.checked_add(cpmm_input).ok_or(ErrorCode::InvariantOverflow)?;
        let cpmm_y_after = y.checked_sub(dy).ok_or(ErrorCode::InsufficientLiquidity)?;
        let end_branch =
            concentrated_hybrid_branch_from_common(cpmm_x_after, cpmm_y_after, peak_depth_nad, imbalance_scale_nad)?;
        if remains_on_same_tail(start_branch, end_branch) {
            return Ok(cpmm_input);
        }
    }

    // Exact-in subtracts at most ceil(raw_output*h) because its inner segment
    // can never exceed total raw output. For
    //   gross = ceil(requested*N/(N-h)),
    // floor(gross*(N-h)/N) >= requested. The upper raw inverse endpoint has a
    // non-negative residual at `y-gross`; replaying it through the same
    // certified D therefore returns at least gross before the haircut and at
    // least `dy` after it. No guessed input premium or replay loop is needed.
    let gross_output_denominator = PPM_DENOMINATOR
        .checked_sub(CONCENTRATED_SOLVER_SAFETY_PPM)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let gross_dy = mul_div_u128_ceil(dy, PPM_DENOMINATOR, gross_output_denominator)?;
    require!(gross_dy < y, ErrorCode::InsufficientLiquidity);
    Ok(quote_common_raw_exact_out_bracket(x, y, gross_dy, peak_depth_nad, imbalance_scale_nad)?.1)
}

/// Conservative exact-input quote over raw NAD-normalized asset reserves.
pub(crate) fn concentrated_quote_exact_in(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_in_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    validate_parameters(center_price_nad, peak_depth_nad, imbalance_scale_nad)?;
    if amount_in_nad == 0 {
        return Ok(0);
    }
    if peak_depth_nad == 0 {
        return match direction {
            ConcentratedSwapDirection::BaseToQuote => {
                calculate_normalized_amount_out(base_reserve_nad, quote_reserve_nad, amount_in_nad)
            }
            ConcentratedSwapDirection::QuoteToBase => {
                calculate_normalized_amount_out(quote_reserve_nad, base_reserve_nad, amount_in_nad)
            }
        };
    }
    if let Some(output) = exact_cpmm_tail_in_raw(
        base_reserve_nad,
        quote_reserve_nad,
        amount_in_nad,
        direction,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
    )? {
        return Ok(output);
    }

    let (base_common, quote_common) = normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)?;
    match direction {
        ConcentratedSwapDirection::BaseToQuote => {
            let input_common = base_to_common(amount_in_nad, center_price_nad)?;
            if input_common == 0 {
                return Ok(0);
            }
            quote_common_exact_in(
                base_common,
                quote_common,
                input_common,
                peak_depth_nad,
                imbalance_scale_nad,
            )
        }
        ConcentratedSwapDirection::QuoteToBase => {
            let output_common = quote_common_exact_in(
                quote_common,
                base_common,
                amount_in_nad,
                peak_depth_nad,
                imbalance_scale_nad,
            )?;
            common_to_base_floor(output_common, center_price_nad)
        }
    }
}

/// Returns a conservative input guaranteed to produce at least
/// `amount_out_nad` when replayed through exact-in.
pub(crate) fn concentrated_quote_exact_out(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_out_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    validate_parameters(center_price_nad, peak_depth_nad, imbalance_scale_nad)?;
    if amount_out_nad == 0 {
        return Ok(0);
    }
    let output_reserve = match direction {
        ConcentratedSwapDirection::BaseToQuote => quote_reserve_nad,
        ConcentratedSwapDirection::QuoteToBase => base_reserve_nad,
    };
    require!(amount_out_nad < output_reserve, ErrorCode::InsufficientLiquidity);

    if peak_depth_nad == 0 {
        return match direction {
            ConcentratedSwapDirection::BaseToQuote => {
                calculate_normalized_amount_in(base_reserve_nad, quote_reserve_nad, amount_out_nad)
            }
            ConcentratedSwapDirection::QuoteToBase => {
                calculate_normalized_amount_in(quote_reserve_nad, base_reserve_nad, amount_out_nad)
            }
        };
    }
    if let Some(input) = exact_cpmm_tail_out_raw(
        base_reserve_nad,
        quote_reserve_nad,
        amount_out_nad,
        direction,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
    )? {
        return Ok(input);
    }

    let (base_common, quote_common) = normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)?;
    match direction {
        ConcentratedSwapDirection::BaseToQuote => {
            let input_common = quote_common_exact_out(
                base_common,
                quote_common,
                amount_out_nad,
                peak_depth_nad,
                imbalance_scale_nad,
            )?;
            common_to_base_ceil(input_common, center_price_nad)
        }
        ConcentratedSwapDirection::QuoteToBase => {
            let output_common = base_to_common_ceil(amount_out_nad, center_price_nad)?;
            quote_common_exact_out(
                quote_common,
                base_common,
                output_common,
                peak_depth_nad,
                imbalance_scale_nad,
            )
        }
    }
}

/// Returns a proven lower bound on the input consumed by the smallest
/// executable exact-in quote that can cover `amount_out_nad`.
///
/// In a pure CPMM segment the closed-form inverse is exact. Otherwise this
/// returns the negative endpoint of the raw invariant inverse bracket,
/// rounded down when converting common value back to the base asset. Because
/// executable exact-in can only subtract an output haircut, this endpoint can
/// never overstate already-utilized collateral.
pub(crate) fn concentrated_quote_exact_out_input_lower_bound(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    amount_out_nad: u128,
    direction: ConcentratedSwapDirection,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    validate_parameters(center_price_nad, peak_depth_nad, imbalance_scale_nad)?;
    if amount_out_nad == 0 {
        return Ok(0);
    }
    let output_reserve = match direction {
        ConcentratedSwapDirection::BaseToQuote => quote_reserve_nad,
        ConcentratedSwapDirection::QuoteToBase => base_reserve_nad,
    };
    require!(amount_out_nad < output_reserve, ErrorCode::InsufficientLiquidity);

    if peak_depth_nad == 0 {
        return match direction {
            ConcentratedSwapDirection::BaseToQuote => {
                calculate_normalized_amount_in(base_reserve_nad, quote_reserve_nad, amount_out_nad)
            }
            ConcentratedSwapDirection::QuoteToBase => {
                calculate_normalized_amount_in(quote_reserve_nad, base_reserve_nad, amount_out_nad)
            }
        };
    }
    if let Some(input) = exact_cpmm_tail_out_raw(
        base_reserve_nad,
        quote_reserve_nad,
        amount_out_nad,
        direction,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
    )? {
        return Ok(input);
    }

    let (base_common, quote_common) = normalize_reserves(base_reserve_nad, quote_reserve_nad, center_price_nad)?;
    match direction {
        ConcentratedSwapDirection::BaseToQuote => {
            let lower_common = quote_common_raw_exact_out_bracket(
                base_common,
                quote_common,
                amount_out_nad,
                peak_depth_nad,
                imbalance_scale_nad,
            )?
            .0;
            common_to_base_floor(lower_common, center_price_nad)
        }
        ConcentratedSwapDirection::QuoteToBase => {
            // This is the smallest common-coordinate output whose floor
            // conversion can reach the requested raw base amount. A negative
            // inverse endpoint below it therefore remains a valid raw bound.
            let output_common = base_to_common_ceil(amount_out_nad, center_price_nad)?;
            Ok(quote_common_raw_exact_out_bracket(
                quote_common,
                base_common,
                output_common,
                peak_depth_nad,
                imbalance_scale_nad,
            )?
            .0)
        }
    }
}

/// Q = D / (2*sqrt(center)) in fixed-point form:
/// `Q^2 = D^2*NAD/(4*center_nad)`.
pub(crate) fn concentrated_balanced_equivalent_q(invariant_d: u128, center_price_nad: u128) -> Result<u128> {
    require!(center_price_nad > 0, ErrorCode::InvalidArgument);
    let numerator = U256::from(invariant_d)
        .checked_mul(U256::from(invariant_d))
        .and_then(|value| value.checked_mul(U256::from(NAD)))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let denominator = U256::from(center_price_nad)
        .checked_mul(U256::from(4_u8))
        .ok_or(ErrorCode::InvariantOverflow)?;
    sqrt_u256_to_u128(numerator / denominator)
}

fn concentrated_inner_marginal_price_from_common(
    x: u128,
    y: u128,
    d: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    // The cross-multiplied residual is smooth, so its exact partial
    // derivatives define the implicit local slope without finite-difference
    // probes or a rounded effective-depth coordinate.
    let ((x_sign, x_derivative), (y_sign, y_derivative)) =
        reserve_residual_derivatives(x, y, d, peak_depth_nad, imbalance_scale_nad)?;
    require!(
        x_sign == ResidualSign::NonNegative
            && y_sign == ResidualSign::NonNegative
            && !x_derivative.is_zero()
            && !y_derivative.is_zero(),
        ErrorCode::InvariantOverflow
    );

    let numerator = checked_mul_512(
        checked_mul_512(x_derivative, U512::from(center_price_nad))?,
        U512::from(y),
    )?;
    let denominator = checked_mul_512(y_derivative, U512::from(x))?;
    let scaled = numerator / denominator;
    require!(scaled <= U512::from(u128::MAX), ErrorCode::InvariantOverflow);
    Ok(scaled.as_u128())
}

/// Canonical branch-aware gradient at the conservative executable D_high
/// parameter. The dedicated positive kernel cancels only exact common factors;
/// the result is identical to evaluating the general signed gradient at the
/// same D_high endpoint.
fn concentrated_canonical_high_inner_marginal_price_from_common(
    x: u128,
    y: u128,
    d_high: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    let (x_core, y_core) =
        canonical_high_reserve_residual_derivative_cores(x, y, d_high, peak_depth_nad, imbalance_scale_nad)?;

    // The live center and both common reserves are u64. Scalar limb kernels
    // are exact here and materially cheaper on SBF than one generic U512
    // multiplication by their combined u128 product. Retain the checked wide
    // fallback for direct math callers with a center outside the state domain.
    let numerator = if let Ok(center_u64) = u64::try_from(center_price_nad) {
        (x_core * y as u64) * center_u64
    } else {
        checked_mul_512(x_core * y as u64, U512::from(center_price_nad))?
    };
    let denominator = y_core * x as u64;
    let scaled = numerator / denominator;
    require!(scaled <= U512::from(u128::MAX), ErrorCode::InvariantOverflow);
    Ok(scaled.as_u128())
}

/// Returns the base->quote marginal of the selected hybrid branch. The CPMM
/// tails use `center*y/x` exactly; the central branch keeps the original
/// implicit derivative. At exact shoulder equality this directional API uses
/// the derivative followed by an infinitesimal base input: CPMM on the
/// quote-scarce/outward shoulder, inner on the base-scarce/restoring shoulder.
pub(crate) fn concentrated_marginal_price_from_common(
    x: u128,
    y: u128,
    d: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    let relation = concentrated_hybrid_shoulder_relation(x, y, peak_depth_nad, imbalance_scale_nad)?;
    let use_cpmm = relation == Ordering::Less || (relation == Ordering::Equal && x >= y);
    if use_cpmm {
        mul_div_u128(y, center_price_nad, x)
    } else {
        concentrated_inner_marginal_price_from_common(x, y, d, center_price_nad, peak_depth_nad, imbalance_scale_nad)
    }
}

/// Builds the canonical `delta = imbalance_scale` shoulder for a supplied D.
/// Coordinates are integer approximations of the exact homogeneous boundary;
/// both one-sided marginal prices are evaluated from those same coordinates.
fn concentrated_hybrid_shoulder_coordinates(
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<(u128, u128, u128)> {
    require!(d > 0 && peak_depth_nad > 0, ErrorCode::InvalidArgument);
    require!(
        imbalance_scale_nad > 0 && imbalance_scale_nad < NAD as u128,
        ErrorCode::InvalidArgument
    );
    let one_minus_scale = (NAD as u128)
        .checked_sub(imbalance_scale_nad)
        .ok_or(ErrorCode::InvariantOverflow)?;
    let a = U256::from(peak_depth_nad)
        .checked_mul(U256::from(one_minus_scale))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let c = a
        .checked_add(
            U256::from(2_u8)
                .checked_mul(U256::from(imbalance_scale_nad))
                .and_then(|value| value.checked_mul(U256::from(NAD)))
                .ok_or(ErrorCode::InvariantOverflow)?,
        )
        .ok_or(ErrorCode::InvariantOverflow)?;
    let sum = u256_to_u128(U256::from(d).checked_mul(c).ok_or(ErrorCode::InvariantOverflow)? / a)?;
    let product = u256_to_u128(
        U256::from(d)
            .checked_mul(U256::from(d))
            .and_then(|value| value.checked_mul(U256::from(one_minus_scale)))
            .ok_or(ErrorCode::InvariantOverflow)?
            / U256::from(4 * NAD as u128),
    )?;
    let discriminant = U256::from(sum)
        .checked_mul(U256::from(sum))
        .and_then(|value| value.checked_sub(U256::from(product).checked_mul(U256::from(4_u8))?))
        .ok_or(ErrorCode::InvariantOverflow)?;
    let difference = sqrt_u256_to_u128(discriminant)?;
    let low_common = sum.checked_sub(difference).ok_or(ErrorCode::InvariantOverflow)? / 2;
    let high_common = sum.checked_sub(low_common).ok_or(ErrorCode::InvariantOverflow)?;
    validate_common_reserves(low_common, high_common)?;

    Ok((low_common, high_common, product))
}

pub(crate) fn concentrated_hybrid_shoulder_from_d(
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<ConcentratedHybridShoulder> {
    let (low_common, high_common, product) =
        concentrated_hybrid_shoulder_coordinates(d, peak_depth_nad, imbalance_scale_nad)?;

    let inner_low_marginal_nad = concentrated_inner_marginal_price_from_common(
        high_common,
        low_common,
        d,
        NAD as u128,
        peak_depth_nad,
        imbalance_scale_nad,
    )?;
    let tail_low_marginal_nad = mul_div_u128(low_common, NAD as u128, high_common)?;
    Ok(ConcentratedHybridShoulder {
        low_common,
        high_common,
        tail_product_common: product,
        inner_low_marginal_nad,
        tail_low_marginal_nad,
    })
}

/// Returns the deterministic canonical base-to-quote mark. Zero peak depth
/// uses raw CPMM reserves. Positive peak depth certifies the protocol-fixed
/// shoulder branch once and evaluates its gradient at the same conservative
/// `D_high` parameter used by exact-in execution. D_high is a certified upper
/// endpoint; supported-domain tests bound this convention against the
/// independent true-root marginal reference to 25 ppm.
pub(crate) fn concentrated_marginal_price_nad(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<u128> {
    validate_parameters(center_price_nad, peak_depth_nad, imbalance_scale_nad)?;
    require!(
        base_reserve_nad > 0 && quote_reserve_nad > 0,
        ErrorCode::InvalidArgument
    );
    if peak_depth_nad == 0 {
        return mul_div_u128(quote_reserve_nad, NAD as u128, base_reserve_nad);
    }
    Ok(concentrated_prepare_curve(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
    )?
    .evaluation()?
    .marginal_price_nad)
}

/// Evaluates all recenter/ramp gate values for arbitrary candidate parameters
/// without changing market state.
#[cfg(test)]
pub(crate) fn concentrated_evaluate(
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> Result<ConcentratedEvaluation> {
    concentrated_prepare_curve(
        base_reserve_nad,
        quote_reserve_nad,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
    )?
    .evaluation()
}

#[cfg(test)]
mod tests {
    include!("../tests/math/concentrated.rs");
}

// Independent arbitrary-precision oracle for the finite-C1 curve.
//
// This module intentionally does not call the production geometry,
// fixed-point, residual, branch, or CPMM helpers to obtain expected values.
// It evaluates the published dimensionless equations with a 512-bit binary
// fractional field backed by arbitrary-width integers, then performs its own
// monotone integer bisections. Production entrypoints appear only on the
// comparison side of the tests.

use super::super::{
    concentrated_hybrid_branch, concentrated_prepare_curve, concentrated_quote_exact_out,
    ConcentratedHybridBranch, ConcentratedSwapDirection,
};
use num_bigint::BigUint;
use num_traits::{One, ToPrimitive};

const REF_NAD: u128 = 1_000_000_000;
thread_local! {
    static REFERENCE_PRECISION_BITS: std::cell::Cell<usize> = const { std::cell::Cell::new(512) };
}

fn reference_precision_bits() -> usize {
    REFERENCE_PRECISION_BITS.with(std::cell::Cell::get)
}

fn with_reference_precision<T>(bits: usize, evaluate: impl FnOnce() -> T) -> T {
    REFERENCE_PRECISION_BITS.with(|precision| {
        let previous = precision.replace(bits);
        let result = evaluate();
        precision.set(previous);
        result
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReferenceBranch {
    Inner,
    BaseScarceTransition,
    QuoteScarceTransition,
    BaseScarceTail,
    QuoteScarceTail,
}

impl ReferenceBranch {
    fn is_tail(self) -> bool {
        matches!(self, Self::BaseScarceTail | Self::QuoteScarceTail)
    }

    fn same_tail(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::BaseScarceTail, Self::BaseScarceTail)
                | (Self::QuoteScarceTail, Self::QuoteScarceTail)
        )
    }
}

#[derive(Clone, Debug)]
struct ReferenceGeometry {
    peak: BigUint,
    scale: BigUint,
    q_start: BigUint,
    q_tail: BigUint,
    v_start: BigUint,
    v_tail: BigUint,
    negative_q_prime_start: BigUint,
    reserve_ratio_start: BigUint,
    reserve_ratio_tail: BigUint,
}

#[derive(Clone, Copy)]
struct ReferenceScale {
    numerator: u128,
    denominator: u128,
}

fn hp_one() -> BigUint {
    BigUint::one() << reference_precision_bits()
}

fn hp_from_ratio(numerator: u128, denominator: u128) -> BigUint {
    (BigUint::from(numerator) << reference_precision_bits()) / BigUint::from(denominator)
}

fn hp_mul(left: &BigUint, right: &BigUint) -> BigUint {
    (left * right) >> reference_precision_bits()
}

fn hp_div(numerator: &BigUint, denominator: &BigUint) -> BigUint {
    (numerator << reference_precision_bits()) / denominator
}

fn hp_sqrt(value: &BigUint) -> BigUint {
    (value << reference_precision_bits()).sqrt()
}

fn hp_average(left: &BigUint, right: &BigUint) -> BigUint {
    (left + right) >> 1
}

fn hp_abs_diff(left: &BigUint, right: &BigUint) -> BigUint {
    if left >= right {
        left - right
    } else {
        right - left
    }
}

fn hp_ratio_from_u128(numerator: u128, denominator: u128) -> BigUint {
    (BigUint::from(numerator) << reference_precision_bits()) / BigUint::from(denominator)
}

fn hp_times_u128_floor(value: &BigUint, integer: u128) -> u128 {
    ((value * BigUint::from(integer)) >> reference_precision_bits())
        .to_u128()
        .expect("reference value fits u128")
}

fn reference_geometry(peak_depth_nad: u128, fade_scale_nad: u128) -> ReferenceGeometry {
    let one = hp_one();
    let peak = hp_from_ratio(peak_depth_nad, REF_NAD);
    let scale = hp_from_ratio(fade_scale_nad, REF_NAD);
    let delta_start = &scale >> 2;
    let q_start = &one - &delta_start;
    let q_tail = &one - &scale;
    let weight_base = hp_div(&scale, &(&scale + &delta_start));
    let weight = hp_mul(&weight_base, &weight_base);
    let coefficient = hp_mul(&(&peak << 1), &hp_mul(&q_start, &weight));
    let h_start = hp_div(&delta_start, &coefficient);
    let sqrt_q_start = hp_sqrt(&q_start);
    let cosh_start = hp_div(&(&one + &h_start), &sqrt_q_start);
    let v_start = hp_sqrt(&(hp_mul(&cosh_start, &cosh_start) - &one));

    // Implicit differentiation of the independent inner scalar at delta=s/4.
    let coefficient_derivative = hp_mul(
        &coefficient,
        &(hp_div(&one, &q_start) + hp_div(&(&one << 1), &(&scale + &delta_start))),
    );
    let residual_q = hp_mul(&coefficient_derivative, &h_start)
        + hp_div(&hp_mul(&coefficient, &cosh_start), &(&sqrt_q_start << 1))
        + &one;
    let residual_v = hp_div(
        &hp_mul(&hp_mul(&coefficient, &sqrt_q_start), &v_start),
        &cosh_start,
    );
    let negative_q_prime_start = hp_div(&residual_v, &residual_q);
    let transition_drop = &q_start - &q_tail;
    let transition_length = hp_div(&(&transition_drop * BigUint::from(3_u8)), &negative_q_prime_start);
    let v_tail = &v_start + transition_length;
    let reserve_ratio_start = hp_mul(
        &(&cosh_start - &v_start),
        &(&cosh_start - &v_start),
    );
    let cosh_tail = hp_sqrt(&(&one + hp_mul(&v_tail, &v_tail)));
    let reserve_ratio_tail = hp_mul(&(&cosh_tail - &v_tail), &(&cosh_tail - &v_tail));

    ReferenceGeometry {
        peak,
        scale,
        q_start,
        q_tail,
        v_start,
        v_tail,
        negative_q_prime_start,
        reserve_ratio_start,
        reserve_ratio_tail,
    }
}

fn reference_scales(center_price_nad: u128) -> (ReferenceScale, ReferenceScale) {
    if center_price_nad >= REF_NAD {
        (
            ReferenceScale {
                numerator: center_price_nad,
                denominator: REF_NAD,
            },
            ReferenceScale {
                numerator: 1,
                denominator: 1,
            },
        )
    } else {
        (
            ReferenceScale {
                numerator: 1,
                denominator: 1,
            },
            ReferenceScale {
                numerator: REF_NAD,
                denominator: center_price_nad,
            },
        )
    }
}

fn reference_scale_floor(amount: u128, scale: ReferenceScale) -> u128 {
    ((BigUint::from(amount) * BigUint::from(scale.numerator)) / BigUint::from(scale.denominator))
        .to_u128()
        .expect("normalized reserve fits u128")
}

fn reference_scale_inverse_ceil(amount: u128, scale: ReferenceScale) -> u128 {
    let numerator = BigUint::from(amount) * BigUint::from(scale.denominator);
    let denominator = BigUint::from(scale.numerator);
    ((&numerator + &denominator - BigUint::one()) / denominator)
        .to_u128()
        .expect("raw reserve fits u128")
}

fn reference_normalize(base: u128, quote: u128, center: u128) -> (u128, u128) {
    let (base_scale, quote_scale) = reference_scales(center);
    (
        reference_scale_floor(base, base_scale),
        reference_scale_floor(quote, quote_scale),
    )
}

fn reference_branch(x: u128, y: u128, geometry: &ReferenceGeometry) -> ReferenceBranch {
    let ratio = hp_ratio_from_u128(x.min(y), x.max(y));
    if ratio <= geometry.reserve_ratio_tail {
        if x < y {
            ReferenceBranch::BaseScarceTail
        } else {
            ReferenceBranch::QuoteScarceTail
        }
    } else if ratio <= geometry.reserve_ratio_start {
        if x < y {
            ReferenceBranch::BaseScarceTransition
        } else {
            ReferenceBranch::QuoteScarceTransition
        }
    } else {
        ReferenceBranch::Inner
    }
}

fn reference_q(x: u128, y: u128, d: u128) -> BigUint {
    ((BigUint::from(x) * BigUint::from(y) * BigUint::from(4_u8)) << reference_precision_bits())
        / (BigUint::from(d) * BigUint::from(d))
}

fn reference_target_q(x: u128, y: u128, geometry: &ReferenceGeometry) -> (ReferenceBranch, BigUint) {
    let branch = reference_branch(x, y, geometry);
    if branch == ReferenceBranch::Inner {
        return (branch, hp_one());
    }
    if branch.is_tail() {
        return (branch, geometry.q_tail.clone());
    }

    let one = hp_one();
    let ratio = hp_ratio_from_u128(x.min(y), x.max(y));
    let sqrt_ratio = hp_sqrt(&ratio);
    let v = hp_div(&(&one - &ratio), &(&sqrt_ratio << 1));
    let z = hp_div(&(&v - &geometry.v_start), &(&geometry.v_tail - &geometry.v_start));
    let one_minus_z = &one - z.min(one.clone());
    let cubic = hp_mul(&hp_mul(&one_minus_z, &one_minus_z), &one_minus_z);
    (
        branch,
        &geometry.q_tail + hp_mul(&(&geometry.q_start - &geometry.q_tail), &cubic),
    )
}

fn reference_transition_q_and_slope(v: &BigUint, geometry: &ReferenceGeometry) -> (BigUint, BigUint) {
    if v <= &geometry.v_start {
        return (
            geometry.q_start.clone(),
            geometry.negative_q_prime_start.clone(),
        );
    }
    if v >= &geometry.v_tail {
        return (geometry.q_tail.clone(), BigUint::default());
    }
    let one = hp_one();
    let z = hp_div(
        &(v - &geometry.v_start),
        &(&geometry.v_tail - &geometry.v_start),
    );
    let one_minus_z = &one - z;
    let square = hp_mul(&one_minus_z, &one_minus_z);
    let cubic = hp_mul(&square, &one_minus_z);
    (
        &geometry.q_tail + hp_mul(&(&geometry.q_start - &geometry.q_tail), &cubic),
        hp_mul(&geometry.negative_q_prime_start, &square),
    )
}

fn reference_inner_q_at_v(v: &BigUint, geometry: &ReferenceGeometry) -> BigUint {
    let one = hp_one();
    let cosh = hp_sqrt(&(&one + hp_mul(v, v)));
    let is_valid = |q: &BigUint| {
        let sqrt_q = hp_sqrt(q);
        let scaled_sum = hp_mul(&sqrt_q, &cosh);
        if scaled_sum < one {
            return false;
        }
        let delta = &one - q;
        let weight_base = hp_div(&geometry.scale, &(&geometry.scale + &delta));
        let concentration = hp_mul(
            &hp_mul(
                &(&geometry.peak << 1),
                &hp_mul(q, &hp_mul(&weight_base, &weight_base)),
            ),
            &(scaled_sum - &one),
        );
        concentration + q >= one
    };
    let mut low = geometry.q_start.clone();
    let mut high = one.clone();
    while &high - &low > BigUint::one() {
        let midpoint = hp_average(&low, &high);
        if is_valid(&midpoint) {
            high = midpoint;
        } else {
            low = midpoint;
        }
    }
    high
}

fn reference_marginal_price_nad(
    base: u128,
    quote: u128,
    center: u128,
    geometry: &ReferenceGeometry,
) -> u128 {
    let (x, y) = reference_normalize(base, quote, center);
    let one = hp_one();
    let reserve_ratio = hp_ratio_from_u128(x.min(y), x.max(y));
    let sqrt_ratio = hp_sqrt(&reserve_ratio);
    let v = hp_div(&(&one - &reserve_ratio), &(&sqrt_ratio << 1));
    let cosh = hp_sqrt(&(&one + hp_mul(&v, &v)));
    let branch = reference_branch(x, y, geometry);
    let (q, negative_q_prime) = match branch {
        ReferenceBranch::Inner => {
            let q = reference_inner_q_at_v(&v, geometry);
            let sqrt_q = hp_sqrt(&q);
            let delta = &one - &q;
            let weight_base = hp_div(&geometry.scale, &(&geometry.scale + &delta));
            let coefficient = hp_mul(
                &(&geometry.peak << 1),
                &hp_mul(&q, &hp_mul(&weight_base, &weight_base)),
            );
            let h = hp_mul(&sqrt_q, &cosh) - &one;
            let coefficient_derivative = hp_mul(
                &coefficient,
                &(hp_div(&one, &q) + hp_div(&(&one << 1), &(&geometry.scale + &delta))),
            );
            let residual_q = hp_mul(&coefficient_derivative, &h)
                + hp_div(&hp_mul(&coefficient, &cosh), &(&sqrt_q << 1))
                + &one;
            let residual_v = hp_div(
                &hp_mul(&hp_mul(&coefficient, &sqrt_q), &v),
                &cosh,
            );
            let slope = hp_div(&residual_v, &residual_q);
            (q, slope)
        }
        ReferenceBranch::BaseScarceTransition | ReferenceBranch::QuoteScarceTransition => {
            reference_transition_q_and_slope(&v, geometry)
        }
        ReferenceBranch::BaseScarceTail | ReferenceBranch::QuoteScarceTail => {
            (geometry.q_tail.clone(), BigUint::default())
        }
    };
    let radial = hp_div(&hp_mul(&negative_q_prime, &cosh), &(&q << 1));
    assert!(radial < one);
    let low_per_high = hp_mul(
        &reserve_ratio,
        &hp_div(&(&one + &radial), &(&one - &radial)),
    );
    let common_quote_per_base = if x >= y {
        low_per_high
    } else {
        hp_div(&one, &low_per_high)
    };
    ((common_quote_per_base * BigUint::from(center)) >> reference_precision_bits())
        .to_u128()
        .expect("reference marginal price fits u128")
}

/// True means the continuous reference residual is on the executable side.
fn reference_valid(x: u128, y: u128, d: u128, geometry: &ReferenceGeometry) -> bool {
    let q = reference_q(x, y, d);
    let (branch, target_q) = reference_target_q(x, y, geometry);
    if branch != ReferenceBranch::Inner {
        return q >= target_q;
    }
    if x + y < d {
        return false;
    }
    let one = hp_one();
    let delta = if q >= one { BigUint::default() } else { &one - &q };
    let weight_base = hp_div(&geometry.scale, &(&geometry.scale + &delta));
    let weight = hp_mul(&weight_base, &weight_base);
    let h = hp_from_ratio(x + y - d, d);
    let concentration = hp_mul(
        &hp_mul(&(&geometry.peak << 1), &hp_mul(&q, &weight)),
        &h,
    );
    concentration + q >= one
}

fn reference_invariant(x: u128, y: u128, geometry: &ReferenceGeometry) -> u128 {
    let product_root = (BigUint::from(x) * BigUint::from(y)).sqrt();
    let mut low = (product_root << 1_usize)
        .to_u128()
        .expect("invariant lower bound fits u128");
    let mut high = x + y;
    if low == high {
        return low;
    }
    assert!(reference_valid(x, y, low, geometry));
    assert!(!reference_valid(x, y, high, geometry));
    while high - low > 1 {
        let midpoint = low + (high - low) / 2;
        if reference_valid(x, y, midpoint, geometry) {
            low = midpoint;
        } else {
            high = midpoint;
        }
    }
    high
}

fn reference_cpmm_exact_in(input_reserve: u128, output_reserve: u128, amount_in: u128) -> u128 {
    ((BigUint::from(output_reserve) * BigUint::from(amount_in)) / BigUint::from(input_reserve + amount_in))
        .to_u128()
        .expect("CPMM output fits u128")
}

fn reference_variable_reserve(fixed: u128, current: u128, d: u128, geometry: &ReferenceGeometry) -> u128 {
    if !reference_valid(fixed, current, d, geometry) {
        return current;
    }
    let structural = d.saturating_sub(fixed);
    let mut low = structural.saturating_sub(1).max(1);
    let mut high = current;
    assert!(!reference_valid(fixed, low, d, geometry));
    while high - low > 1 {
        let midpoint = low + (high - low) / 2;
        if reference_valid(fixed, midpoint, d, geometry) {
            high = midpoint;
        } else {
            low = midpoint;
        }
    }
    high
}

fn reference_quote_exact_in(
    base: u128,
    quote: u128,
    amount_in: u128,
    direction: ConcentratedSwapDirection,
    center: u128,
    geometry: &ReferenceGeometry,
) -> u128 {
    let (base_common, quote_common) = reference_normalize(base, quote, center);
    let start_branch = reference_branch(base_common, quote_common, geometry);
    let cpmm_output = match direction {
        ConcentratedSwapDirection::BaseToQuote => reference_cpmm_exact_in(base, quote, amount_in),
        ConcentratedSwapDirection::QuoteToBase => reference_cpmm_exact_in(quote, base, amount_in),
    };
    let (cpmm_base_after, cpmm_quote_after) = match direction {
        ConcentratedSwapDirection::BaseToQuote => (base + amount_in, quote - cpmm_output),
        ConcentratedSwapDirection::QuoteToBase => (base - cpmm_output, quote + amount_in),
    };
    let (cpmm_base_common, cpmm_quote_common) = reference_normalize(cpmm_base_after, cpmm_quote_after, center);
    if start_branch.is_tail()
        && start_branch.same_tail(reference_branch(cpmm_base_common, cpmm_quote_common, geometry))
    {
        return cpmm_output;
    }

    let d = reference_invariant(base_common, quote_common, geometry);
    let (base_scale, quote_scale) = reference_scales(center);
    let (input_after_raw, output_raw, output_common, input_scale, output_scale) = match direction {
        ConcentratedSwapDirection::BaseToQuote => (
            base + amount_in,
            quote,
            quote_common,
            base_scale,
            quote_scale,
        ),
        ConcentratedSwapDirection::QuoteToBase => (
            quote + amount_in,
            base,
            base_common,
            quote_scale,
            base_scale,
        ),
    };
    let input_after_common = reference_scale_floor(input_after_raw, input_scale);
    let output_after_common = reference_variable_reserve(input_after_common, output_common, d, geometry);
    let output_after_raw = reference_scale_inverse_ceil(output_after_common, output_scale);
    output_raw - output_after_raw
}

fn reference_raw_output_is_valid(
    base: u128,
    quote: u128,
    amount_in: u128,
    output: u128,
    direction: ConcentratedSwapDirection,
    center: u128,
    geometry: &ReferenceGeometry,
) -> bool {
    let (base_common, quote_common) = reference_normalize(base, quote, center);
    let d = reference_invariant(base_common, quote_common, geometry);
    let start_branch = reference_branch(base_common, quote_common, geometry);
    let (base_after, quote_after) = match direction {
        ConcentratedSwapDirection::BaseToQuote => {
            if output >= quote {
                return false;
            }
            (base + amount_in, quote - output)
        }
        ConcentratedSwapDirection::QuoteToBase => {
            if output >= base {
                return false;
            }
            (base - output, quote + amount_in)
        }
    };
    let (base_after_common, quote_after_common) = reference_normalize(base_after, quote_after, center);
    let end_branch = reference_branch(base_after_common, quote_after_common, geometry);
    if start_branch.same_tail(end_branch) {
        return BigUint::from(base_after) * BigUint::from(quote_after)
            >= BigUint::from(base) * BigUint::from(quote);
    }
    reference_valid(base_after_common, quote_after_common, d, geometry)
}

fn reference_quote_exact_out(
    base: u128,
    quote: u128,
    amount_out: u128,
    direction: ConcentratedSwapDirection,
    center: u128,
    geometry: &ReferenceGeometry,
) -> u128 {
    let mut low = 0_u128;
    let mut high = 1_u128;
    while reference_quote_exact_in(base, quote, high, direction, center, geometry) < amount_out {
        low = high;
        high = high.checked_mul(2).expect("reference exact-out bracket fits u128");
    }
    while high - low > 1 {
        let midpoint = low + (high - low) / 2;
        if reference_quote_exact_in(base, quote, midpoint, direction, center, geometry) >= amount_out {
            high = midpoint;
        } else {
            low = midpoint;
        }
    }
    high
}

// This is the independently reconstructed ideal two-region C0 equation. It
// establishes mathematical equivalence where the C1 rewrite left the equation
// unchanged; it is not a byte-for-byte oracle for the former U512 rounding.
fn reference_ideal_c0_rho_threshold(geometry: &ReferenceGeometry) -> BigUint {
    let one = hp_one();
    let t = &one
        + hp_div(
            &(&geometry.scale << 1),
            &hp_mul(&geometry.peak, &geometry.q_tail),
        );
    hp_div(&geometry.q_tail, &hp_mul(&t, &t))
}

fn reference_ideal_c0_inner(x: u128, y: u128, geometry: &ReferenceGeometry) -> bool {
    let sum = x + y;
    let rho = ((BigUint::from(x) * BigUint::from(y) * BigUint::from(4_u8))
        << reference_precision_bits())
        / (BigUint::from(sum) * BigUint::from(sum));
    rho >= reference_ideal_c0_rho_threshold(geometry)
}

fn reference_ideal_c0_valid(x: u128, y: u128, d: u128, geometry: &ReferenceGeometry) -> bool {
    if reference_ideal_c0_inner(x, y, geometry) {
        reference_valid_inner_only(x, y, d, geometry)
    } else {
        reference_q(x, y, d) >= geometry.q_tail
    }
}

fn reference_valid_inner_only(x: u128, y: u128, d: u128, geometry: &ReferenceGeometry) -> bool {
    if x + y < d {
        return false;
    }
    let one = hp_one();
    let q = reference_q(x, y, d);
    let delta = if q >= one { BigUint::default() } else { &one - &q };
    let weight_base = hp_div(&geometry.scale, &(&geometry.scale + &delta));
    let weight = hp_mul(&weight_base, &weight_base);
    let h = hp_from_ratio(x + y - d, d);
    hp_mul(
        &hp_mul(&(&geometry.peak << 1), &hp_mul(&q, &weight)),
        &h,
    ) + q
        >= one
}

fn reference_ideal_c0_invariant(x: u128, y: u128, geometry: &ReferenceGeometry) -> u128 {
    let mut low = ((BigUint::from(x) * BigUint::from(y)).sqrt() << 1_usize)
        .to_u128()
        .unwrap();
    let mut high = x + y;
    if low == high {
        return low;
    }
    while high - low > 1 {
        let midpoint = low + (high - low) / 2;
        if reference_ideal_c0_valid(x, y, midpoint, geometry) {
            low = midpoint;
        } else {
            high = midpoint;
        }
    }
    high
}

fn reference_ideal_c0_quote(
    base: u128,
    quote: u128,
    amount_in: u128,
    direction: ConcentratedSwapDirection,
    geometry: &ReferenceGeometry,
) -> u128 {
    let starts_inner = reference_ideal_c0_inner(base, quote, geometry);
    let cpmm = match direction {
        ConcentratedSwapDirection::BaseToQuote => reference_cpmm_exact_in(base, quote, amount_in),
        ConcentratedSwapDirection::QuoteToBase => reference_cpmm_exact_in(quote, base, amount_in),
    };
    let (cpmm_base_after, cpmm_quote_after) = match direction {
        ConcentratedSwapDirection::BaseToQuote => (base + amount_in, quote - cpmm),
        ConcentratedSwapDirection::QuoteToBase => (base - cpmm, quote + amount_in),
    };
    if !starts_inner && !reference_ideal_c0_inner(cpmm_base_after, cpmm_quote_after, geometry) {
        return cpmm;
    }
    let d = reference_ideal_c0_invariant(base, quote, geometry);
    let (fixed, current) = match direction {
        ConcentratedSwapDirection::BaseToQuote => (base + amount_in, quote),
        ConcentratedSwapDirection::QuoteToBase => (quote + amount_in, base),
    };
    let mut low = d.saturating_sub(fixed).saturating_sub(1).max(1);
    let mut high = current;
    while high - low > 1 {
        let midpoint = low + (high - low) / 2;
        if reference_ideal_c0_valid(fixed, midpoint, d, geometry) {
            high = midpoint;
        } else {
            low = midpoint;
        }
    }
    current - high
}

fn production_branch_to_reference(branch: ConcentratedHybridBranch) -> ReferenceBranch {
    match branch {
        ConcentratedHybridBranch::Inner => ReferenceBranch::Inner,
        ConcentratedHybridBranch::BaseScarceTransition => ReferenceBranch::BaseScarceTransition,
        ConcentratedHybridBranch::QuoteScarceTransition => ReferenceBranch::QuoteScarceTransition,
        ConcentratedHybridBranch::BaseScarceTail => ReferenceBranch::BaseScarceTail,
        ConcentratedHybridBranch::QuoteScarceTail => ReferenceBranch::QuoteScarceTail,
    }
}

fn representative_ratios(geometry: &ReferenceGeometry) -> [(ReferenceBranch, BigUint); 3] {
    [
        (
            ReferenceBranch::Inner,
            hp_average(&hp_one(), &geometry.reserve_ratio_start),
        ),
        (
            ReferenceBranch::BaseScarceTransition,
            hp_average(&geometry.reserve_ratio_start, &geometry.reserve_ratio_tail),
        ),
        (
            ReferenceBranch::BaseScarceTail,
            &geometry.reserve_ratio_tail >> 1,
        ),
    ]
}

fn raw_reserves_from_common(base_common: u128, quote_common: u128, center: u128) -> (u128, u128) {
    let (base_scale, quote_scale) = reference_scales(center);
    (
        reference_scale_inverse_ceil(base_common, base_scale),
        reference_scale_inverse_ceil(quote_common, quote_scale),
    )
}

#[test]
fn finite_c1_reference_matches_production_across_all_regions_and_directions() {
    for (peak, fade, high) in [
        (2 * REF_NAD, 100, 4_000_000_000_000_000),
        (2 * REF_NAD, REF_NAD / 1_000, 4_000_000_000_000_000),
        (200 * REF_NAD, REF_NAD / 10, 4_000_000_000_000_000),
        (
            2_000 * REF_NAD,
            199_000_000,
            (u64::MAX as u128 / 5) * 4,
        ),
    ] {
        let geometry = reference_geometry(peak, fade);
        for (expected_low_branch, ratio) in representative_ratios(&geometry) {
            let low = hp_times_u128_floor(&ratio, high).max(REF_NAD);
            for (base_common, quote_common, expected_branch) in [
                (low, high, expected_low_branch),
                (
                    high,
                    low,
                    match expected_low_branch {
                        ReferenceBranch::BaseScarceTransition => ReferenceBranch::QuoteScarceTransition,
                        ReferenceBranch::BaseScarceTail => ReferenceBranch::QuoteScarceTail,
                        other => other,
                    },
                ),
            ] {
                for center in [123_456_789, REF_NAD, 3 * REF_NAD / 2] {
                    let (base, quote) = raw_reserves_from_common(base_common, quote_common, center);
                    let (normalized_base, normalized_quote) = reference_normalize(base, quote, center);
                    assert_eq!(reference_branch(normalized_base, normalized_quote, &geometry), expected_branch);
                    assert_eq!(
                        production_branch_to_reference(
                            concentrated_hybrid_branch(base, quote, center, peak, fade).unwrap(),
                        ),
                        expected_branch,
                        "peak={peak} fade={fade} center={center} base={base} quote={quote}"
                    );
                    let reference_d = reference_invariant(normalized_base, normalized_quote, &geometry);
                    let production = concentrated_prepare_curve(base, quote, center, peak, fade).unwrap();
                    assert!(
                        reference_d.abs_diff(production.invariant_d()) <= 1,
                        "invariant mismatch: peak={peak} fade={fade} center={center} branch={expected_branch:?} reference={reference_d} production={}",
                        production.invariant_d()
                    );

                    for direction in [
                        ConcentratedSwapDirection::BaseToQuote,
                        ConcentratedSwapDirection::QuoteToBase,
                    ] {
                        let input_reserve = match direction {
                            ConcentratedSwapDirection::BaseToQuote => base,
                            ConcentratedSwapDirection::QuoteToBase => quote,
                        };
                        for amount_in in [
                            1_u128,
                            (input_reserve / 1_000_000).max(1),
                            (input_reserve / 10_000).max(1),
                            (input_reserve / 5).max(1),
                        ] {
                            let expected =
                                reference_quote_exact_in(base, quote, amount_in, direction, center, &geometry);
                            let actual = production.quote_exact_in(amount_in, direction).unwrap();
                            assert!(
                                expected.abs_diff(actual) <= 1,
                                "quote mismatch: peak={peak} fade={fade} center={center} branch={expected_branch:?} direction={direction:?} input={amount_in} reference={expected} production={actual}"
                            );
                            assert!(reference_raw_output_is_valid(
                                base, quote, amount_in, actual, direction, center, &geometry,
                            ));
                            assert!(
                                !reference_raw_output_is_valid(
                                    base,
                                    quote,
                                    amount_in,
                                    actual + 1,
                                    direction,
                                    center,
                                    &geometry,
                                ),
                                "production output was not maximal in the independent model: center={center} direction={direction:?} input={amount_in} output={actual}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn reference_256_and_512_bit_floors_are_identical_before_production_comparison() {
    let high_common = 4_000_000_000_000_000_u128;
    for (peak, fade) in [
        (2 * REF_NAD, REF_NAD / 1_000),
        (200 * REF_NAD, REF_NAD / 10),
        (2_000 * REF_NAD, 199_000_000),
    ] {
        let geometry_512 = with_reference_precision(512, || reference_geometry(peak, fade));
        for (_, ratio) in representative_ratios(&geometry_512) {
            let low_common = hp_times_u128_floor(&ratio, high_common).max(REF_NAD);
            for center in [123_456_789, REF_NAD, 3 * REF_NAD / 2] {
                for (base_common, quote_common) in [(low_common, high_common), (high_common, low_common)] {
                    let (base, quote) = raw_reserves_from_common(base_common, quote_common, center);
                    let snapshot = |bits| {
                        with_reference_precision(bits, || {
                            let geometry = reference_geometry(peak, fade);
                            let (x, y) = reference_normalize(base, quote, center);
                            let branch = reference_branch(x, y, &geometry);
                            let d = reference_invariant(x, y, &geometry);
                            let mut exact_in = [0_u128; 2];
                            let mut exact_out = [0_u128; 2];
                            for (index, direction) in [
                                ConcentratedSwapDirection::BaseToQuote,
                                ConcentratedSwapDirection::QuoteToBase,
                            ]
                            .into_iter()
                            .enumerate()
                            {
                                let input_reserve = if index == 0 { base } else { quote };
                                let output_reserve = if index == 0 { quote } else { base };
                                exact_in[index] = reference_quote_exact_in(
                                    base,
                                    quote,
                                    (input_reserve / 10_000).max(1),
                                    direction,
                                    center,
                                    &geometry,
                                );
                                exact_out[index] = reference_quote_exact_out(
                                    base,
                                    quote,
                                    (output_reserve / 1_000_000).max(1),
                                    direction,
                                    center,
                                    &geometry,
                                );
                            }
                            (branch, d, exact_in, exact_out)
                        })
                    };
                    assert_eq!(snapshot(256), snapshot(512));
                }
            }
        }
    }
}

#[test]
fn reference_roots_and_outputs_are_canonical_to_one_raw_atom() {
    let peak = 200 * REF_NAD;
    let fade = REF_NAD / 10;
    let geometry = reference_geometry(peak, fade);
    let high = 3_000_000_000_000_000_u128;
    for (_, ratio) in representative_ratios(&geometry) {
        let low = hp_times_u128_floor(&ratio, high).max(REF_NAD);
        for (base, quote) in [(low, high), (high, low)] {
            let d = reference_invariant(base, quote, &geometry);
            if d != base + quote {
                assert!(reference_valid(base, quote, d - 1, &geometry));
                assert!(!reference_valid(base, quote, d, &geometry));
            }
            for direction in [
                ConcentratedSwapDirection::BaseToQuote,
                ConcentratedSwapDirection::QuoteToBase,
            ] {
                let amount_in = match direction {
                    ConcentratedSwapDirection::BaseToQuote => base / 100_000,
                    ConcentratedSwapDirection::QuoteToBase => quote / 100_000,
                }
                .max(1);
                let expected = reference_quote_exact_in(base, quote, amount_in, direction, REF_NAD, &geometry);
                let actual = concentrated_prepare_curve(base, quote, REF_NAD, peak, fade)
                    .unwrap()
                    .quote_exact_in(amount_in, direction)
                    .unwrap();
                assert!(expected.abs_diff(actual) <= 1);
            }
        }
    }
}

#[test]
fn independent_geometry_proves_c1_value_and_slope_at_both_joins() {
    for (peak, fade) in [
        (2 * REF_NAD, REF_NAD / 1_000),
        (200 * REF_NAD, REF_NAD / 10),
        (2_000 * REF_NAD, 199_000_000),
    ] {
        let geometry = reference_geometry(peak, fade);
        let one = hp_one();
        let transition_width = &geometry.v_tail - &geometry.v_start;
        let epsilon = (&transition_width / BigUint::from(1_000_000_u32)).max(BigUint::one());
        let (start_target, start_slope) = reference_transition_q_and_slope(&geometry.v_start, &geometry);
        let (tail_target, tail_slope) = reference_transition_q_and_slope(&geometry.v_tail, &geometry);
        assert!(hp_abs_diff(&start_target, &geometry.q_start) <= BigUint::one());
        assert!(hp_abs_diff(&start_slope, &geometry.negative_q_prime_start) <= BigUint::one());
        assert!(hp_abs_diff(&tail_target, &geometry.q_tail) <= BigUint::one());
        assert_eq!(tail_slope, BigUint::default());

        let inner_q = reference_inner_q_at_v(&(&geometry.v_start - &epsilon), &geometry);
        let (start_right_q, _) =
            reference_transition_q_and_slope(&(&geometry.v_start + &epsilon), &geometry);
        let inner_one_sided_slope = hp_div(&(&inner_q - &geometry.q_start), &epsilon);
        let transition_one_sided_slope = hp_div(&(&geometry.q_start - start_right_q), &epsilon);
        let slope_scale = inner_one_sided_slope
            .clone()
            .max(transition_one_sided_slope.clone());
        assert!(
            hp_abs_diff(&inner_one_sided_slope, &transition_one_sided_slope)
                * BigUint::from(1_000_u16)
                <= slope_scale
        );

        let (tail_left_q, tail_left_slope) =
            reference_transition_q_and_slope(&(&geometry.v_tail - &epsilon), &geometry);
        let tail_secant = hp_div(&(tail_left_q - &geometry.q_tail), &epsilon);
        assert!(tail_left_slope < geometry.negative_q_prime_start);
        assert!(tail_secant < geometry.negative_q_prime_start);

        // The inner scalar is zero at the independently derived start join.
        let sqrt_q = hp_sqrt(&geometry.q_start);
        let cosh = hp_sqrt(&(&one + hp_mul(&geometry.v_start, &geometry.v_start)));
        let h = hp_mul(&sqrt_q, &cosh) - &one;
        let delta = &one - &geometry.q_start;
        let weight_base = hp_div(&geometry.scale, &(&geometry.scale + delta));
        let inner = hp_mul(
            &hp_mul(
                &(&geometry.peak << 1),
                &hp_mul(&geometry.q_start, &hp_mul(&weight_base, &weight_base)),
            ),
            &h,
        ) + &geometry.q_start;
        // This is 2^-448 relative tolerance: it absorbs only truncation in the
        // independent 512-fractional-bit square roots, not raw-token error.
        assert!(hp_abs_diff(&inner, &one) <= (BigUint::one() << 64_usize));
    }
}

#[test]
fn exact_out_reference_is_minimal_across_centers_regions_and_directions() {
    let peak = 200 * REF_NAD;
    let fade = REF_NAD / 10;
    let geometry = reference_geometry(peak, fade);
    let high_common = 2_000_000_000_000_000_u128;
    for (_, ratio) in representative_ratios(&geometry) {
        let low_common = hp_times_u128_floor(&ratio, high_common).max(REF_NAD);
        for center in [
            1,
            123_456_789,
            REF_NAD - 1,
            REF_NAD,
            REF_NAD + 1,
            3 * REF_NAD / 2,
            u64::MAX as u128,
        ] {
            for (base_common, quote_common) in [(low_common, high_common), (high_common, low_common)] {
                let (base, quote) = raw_reserves_from_common(base_common, quote_common, center);
                let production = concentrated_prepare_curve(base, quote, center, peak, fade).unwrap();
                for direction in [
                    ConcentratedSwapDirection::BaseToQuote,
                    ConcentratedSwapDirection::QuoteToBase,
                ] {
                    let output_reserve = match direction {
                        ConcentratedSwapDirection::BaseToQuote => quote,
                        ConcentratedSwapDirection::QuoteToBase => base,
                    };
                    let requested = (output_reserve / 1_000_000).max(1);
                    let expected =
                        reference_quote_exact_out(base, quote, requested, direction, center, &geometry);
                    assert!(
                        reference_quote_exact_in(base, quote, expected, direction, center, &geometry) >= requested
                    );
                    assert!(
                        expected == 0
                            || reference_quote_exact_in(
                                base,
                                quote,
                                expected - 1,
                                direction,
                                center,
                                &geometry,
                            ) < requested
                    );
                    let actual = concentrated_quote_exact_out(
                        base, quote, requested, direction, center, peak, fade,
                    )
                    .unwrap();
                    assert!(
                        expected.abs_diff(actual) <= 1,
                        "exact-out mismatch center={center} direction={direction:?} requested={requested} reference={expected} production={actual}"
                    );
                    assert!(production.quote_exact_in(actual, direction).unwrap() >= requested);
                    assert!(actual == 0 || production.quote_exact_in(actual - 1, direction).unwrap() < requested);
                }
            }
        }
    }
}

#[test]
fn join_adjacent_vectors_match_the_independent_branch_and_quote_model() {
    let peak = 200 * REF_NAD;
    let fade = REF_NAD / 10;
    let geometry = reference_geometry(peak, fade);
    let high = 8_000_000_000_000_000_u128;
    for boundary in [&geometry.reserve_ratio_start, &geometry.reserve_ratio_tail] {
        let boundary_low = hp_times_u128_floor(boundary, high);
        for low in (boundary_low - 3)..=(boundary_low + 3) {
            for (base, quote) in [(low, high), (high, low)] {
                let expected_branch = reference_branch(base, quote, &geometry);
                let actual_branch = production_branch_to_reference(
                    concentrated_hybrid_branch(base, quote, REF_NAD, peak, fade).unwrap(),
                );
                assert_eq!(actual_branch, expected_branch, "base={base} quote={quote}");
                let production = concentrated_prepare_curve(base, quote, REF_NAD, peak, fade).unwrap();
                let expected_d = reference_invariant(base, quote, &geometry);
                assert!(expected_d.abs_diff(production.invariant_d()) <= 1);
                let expected_price = reference_marginal_price_nad(base, quote, REF_NAD, &geometry);
                let actual_price = production.marginal_price_nad().unwrap();
                assert!(
                    expected_price.abs_diff(actual_price)
                        <= ((expected_price * 25).div_ceil(1_000_000)).max(2),
                    "join marginal mismatch branch={expected_branch:?} reference={expected_price} production={actual_price}"
                );
                for direction in [
                    ConcentratedSwapDirection::BaseToQuote,
                    ConcentratedSwapDirection::QuoteToBase,
                ] {
                    let amount_in = 10_000_000_u128;
                    let expected =
                        reference_quote_exact_in(base, quote, amount_in, direction, REF_NAD, &geometry);
                    let actual = production.quote_exact_in(amount_in, direction).unwrap();
                    assert!(
                        expected.abs_diff(actual) <= 1,
                        "join quote mismatch branch={expected_branch:?} direction={direction:?} reference={expected} production={actual}"
                    );
                }
            }
        }
    }
}

#[test]
fn exact_tail_is_independently_bit_identical_to_cpmm_in_both_directions() {
    let peak = 200 * REF_NAD;
    let fade = REF_NAD / 10;
    let geometry = reference_geometry(peak, fade);
    let high = 5_000_000_000_000_000_u128;
    let low = hp_times_u128_floor(&(&geometry.reserve_ratio_tail >> 2), high).max(REF_NAD);
    for (base, quote, direction) in [
        (high, low, ConcentratedSwapDirection::BaseToQuote),
        (low, high, ConcentratedSwapDirection::QuoteToBase),
    ] {
        let amount_in = high / 10_000;
        let cpmm = match direction {
            ConcentratedSwapDirection::BaseToQuote => reference_cpmm_exact_in(base, quote, amount_in),
            ConcentratedSwapDirection::QuoteToBase => reference_cpmm_exact_in(quote, base, amount_in),
        };
        let reference = reference_quote_exact_in(base, quote, amount_in, direction, REF_NAD, &geometry);
        let production = concentrated_prepare_curve(base, quote, REF_NAD, peak, fade)
            .unwrap()
            .quote_exact_in(amount_in, direction)
            .unwrap();
        assert_eq!(reference, cpmm);
        assert_eq!(production, cpmm);
    }
}

#[test]
fn ideal_c0_matches_unchanged_inner_and_tail_but_not_the_c1_transition() {
    let peak = 200 * REF_NAD;
    let fade = REF_NAD / 10;
    let geometry = reference_geometry(peak, fade);
    let high = 6_000_000_000_000_000_u128;

    let inner_low = hp_times_u128_floor(
        &hp_average(&hp_one(), &geometry.reserve_ratio_start),
        high,
    );
    for (base, quote, direction) in [
        (inner_low, high, ConcentratedSwapDirection::QuoteToBase),
        (high, inner_low, ConcentratedSwapDirection::BaseToQuote),
    ] {
        let amount_in = base.min(quote) / 1_000_000;
        assert!(reference_ideal_c0_inner(base, quote, &geometry));
        assert_eq!(reference_branch(base, quote, &geometry), ReferenceBranch::Inner);
        assert!(
            reference_invariant(base, quote, &geometry)
                .abs_diff(reference_ideal_c0_invariant(base, quote, &geometry))
                <= 1
        );
        assert!(
            reference_quote_exact_in(base, quote, amount_in, direction, REF_NAD, &geometry)
                .abs_diff(reference_ideal_c0_quote(base, quote, amount_in, direction, &geometry))
                <= 1
        );
    }

    let tail_low = hp_times_u128_floor(&(&geometry.reserve_ratio_tail >> 2), high).max(REF_NAD);
    for (base, quote, direction) in [
        (high, tail_low, ConcentratedSwapDirection::BaseToQuote),
        (tail_low, high, ConcentratedSwapDirection::QuoteToBase),
    ] {
        let amount_in = high / 100_000;
        assert!(!reference_ideal_c0_inner(base, quote, &geometry));
        assert!(reference_branch(base, quote, &geometry).is_tail());
        let cpmm = match direction {
            ConcentratedSwapDirection::BaseToQuote => reference_cpmm_exact_in(base, quote, amount_in),
            ConcentratedSwapDirection::QuoteToBase => reference_cpmm_exact_in(quote, base, amount_in),
        };
        assert_eq!(reference_quote_exact_in(base, quote, amount_in, direction, REF_NAD, &geometry), cpmm);
        assert_eq!(reference_ideal_c0_quote(base, quote, amount_in, direction, &geometry), cpmm);
    }

    // The finite-C1 band deliberately replaces the legacy one-sided shoulder.
    // We characterize this changed region by proving that both branches remain
    // executable and monotone, while requiring at least one quote to differ.
    let transition_ratio = hp_average(&geometry.reserve_ratio_start, &geometry.reserve_ratio_tail);
    let transition_low = hp_times_u128_floor(&transition_ratio, high);
    let mut distinct_quotes = 0_u8;
    for (base, quote, direction) in [
        (high, transition_low, ConcentratedSwapDirection::BaseToQuote),
        (transition_low, high, ConcentratedSwapDirection::QuoteToBase),
    ] {
        assert!(matches!(
            reference_branch(base, quote, &geometry),
            ReferenceBranch::BaseScarceTransition | ReferenceBranch::QuoteScarceTransition
        ));
        let input = high / 1_000;
        let c1 = reference_quote_exact_in(base, quote, input, direction, REF_NAD, &geometry);
        let ideal_c0 = reference_ideal_c0_quote(base, quote, input, direction, &geometry);
        assert!(c1 > 0 && ideal_c0 > 0);
        let c1_next = reference_quote_exact_in(base, quote, input + 1, direction, REF_NAD, &geometry);
        let ideal_c0_next = reference_ideal_c0_quote(base, quote, input + 1, direction, &geometry);
        assert!(c1_next >= c1 && ideal_c0_next >= ideal_c0);
        distinct_quotes += u8::from(c1 != ideal_c0);
    }
    assert!(distinct_quotes > 0, "the intentional finite-C1 transition change was not exercised");
}

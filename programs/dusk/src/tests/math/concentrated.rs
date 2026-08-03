use super::*;
use crate::state::{
    AmmCurveParameters, AmmRamp, MAX_AMM_IMBALANCE_SCALE_NAD, MAX_AMM_PEAK_DEPTH_NAD,
    MAX_AMM_RAMP_DURATION_SLOTS, MIN_AMM_IMBALANCE_SCALE_NAD, MIN_AMM_PEAK_DEPTH_NAD,
    MIN_AMM_RAMP_DURATION_SLOTS,
};
use proptest::prelude::*;

const PEAK_DEPTH_200: u128 = 200 * NAD as u128;
const IMBALANCE_SCALE_TENTH: u128 = NAD as u128 / 10;

fn assert_canonical_high_kernel_matches_signed_u512_reference(
    x: u128,
    y: u128,
    d_high: u128,
    center_price_nad: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) {
    let (x_core, y_core) = canonical_high_reserve_residual_derivative_cores(
        x,
        y,
        d_high,
        peak_depth_nad,
        imbalance_scale_nad,
    )
    .unwrap();
    let ((x_sign, reference_x_core), (y_sign, reference_y_core)) =
        reserve_residual_derivatives(x, y, d_high, peak_depth_nad, imbalance_scale_nad).unwrap();
    assert_eq!(x_sign, ResidualSign::NonNegative);
    assert_eq!(y_sign, ResidualSign::NonNegative);
    assert_eq!(checked_mul_512(x_core, U512::from(2_u8)).unwrap(), reference_x_core);
    assert_eq!(checked_mul_512(y_core, U512::from(2_u8)).unwrap(), reference_y_core);

    let reference_price = concentrated_inner_marginal_price_from_common(
        x,
        y,
        d_high,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
    )
    .unwrap();
    let canonical_price = concentrated_canonical_high_inner_marginal_price_from_common(
        x,
        y,
        d_high,
        center_price_nad,
        peak_depth_nad,
        imbalance_scale_nad,
    )
    .unwrap();
    assert_eq!(canonical_price, reference_price);
}

fn assert_exact_out_inverse_bracket(
    base_reserve: u128,
    quote_reserve: u128,
    requested: u128,
    direction: ConcentratedSwapDirection,
    center: u128,
    peak_depth: u128,
    imbalance_scale: u128,
) {
    let upper = concentrated_quote_exact_out(
        base_reserve,
        quote_reserve,
        requested,
        direction,
        center,
        peak_depth,
        imbalance_scale,
    )
    .unwrap();
    let lower = concentrated_quote_exact_out_input_lower_bound(
        base_reserve,
        quote_reserve,
        requested,
        direction,
        center,
        peak_depth,
        imbalance_scale,
    )
    .unwrap();
    let replay = concentrated_quote_exact_in(
        base_reserve,
        quote_reserve,
        upper,
        direction,
        center,
        peak_depth,
        imbalance_scale,
    )
    .unwrap();
    assert!(
        replay >= requested,
        "requested={requested} upper={upper} replay={replay}"
    );
    assert!(lower <= upper, "requested={requested} lower={lower} upper={upper}");

    // Independent executable inverse used only by tests. Production obtains
    // both bounds from one fixed-iteration invariant solve.
    let mut reference_low = 0_u128;
    let mut reference_high = upper;
    while reference_low < reference_high {
        let midpoint = reference_low + (reference_high - reference_low) / 2;
        if concentrated_quote_exact_in(
            base_reserve,
            quote_reserve,
            midpoint,
            direction,
            center,
            peak_depth,
            imbalance_scale,
        )
        .unwrap()
            >= requested
        {
            reference_high = midpoint;
        } else {
            reference_low = midpoint + 1;
        }
    }
    assert!(
        lower <= reference_low && reference_low <= upper,
        "requested={requested} lower={lower} reference={reference_low} upper={upper}"
    );
    if lower < reference_low {
        let lower_replay = concentrated_quote_exact_in(
            base_reserve,
            quote_reserve,
            lower,
            direction,
            center,
            peak_depth,
            imbalance_scale,
        )
        .unwrap();
        assert!(lower_replay < requested);
    }
}

fn analytical_residual(x: f64, y: f64, d: f64, peak_depth: f64, imbalance_scale: f64) -> f64 {
    let q = 4.0 * x * y / (d * d);
    let delta = 1.0 - q;
    let weight = (imbalance_scale / (imbalance_scale + delta)).powi(2);
    let depth_eff = 0.5 * peak_depth * q * weight;
    depth_eff * d * (x + y - d) + x * y - d * d / 4.0
}

fn analytical_is_inner(x: f64, y: f64, peak_depth: f64, imbalance_scale: f64) -> bool {
    if peak_depth == 0.0 {
        return true;
    }
    let shoulder_q = 1.0 - imbalance_scale;
    let shoulder_sum_over_d = 1.0 + 2.0 * imbalance_scale / (peak_depth * shoulder_q);
    4.0 * x * y / (x + y).powi(2) >= shoulder_q / shoulder_sum_over_d.powi(2)
}

fn analytical_hybrid_residual(x: f64, y: f64, d: f64, peak_depth: f64, imbalance_scale: f64) -> f64 {
    if analytical_is_inner(x, y, peak_depth, imbalance_scale) {
        analytical_residual(x, y, d, peak_depth, imbalance_scale)
    } else {
        x * y - d * d * (1.0 - imbalance_scale) / 4.0
    }
}

fn analytical_invariant(x: f64, y: f64, peak_depth: f64, imbalance_scale: f64) -> f64 {
    if peak_depth == 0.0 {
        return 2.0 * (x * y).sqrt();
    }
    if !analytical_is_inner(x, y, peak_depth, imbalance_scale) {
        return 2.0 * (x * y / (1.0 - imbalance_scale)).sqrt();
    }
    let mut low = 2.0 * (x * y).sqrt();
    let mut high = x + y;
    for _ in 0..160 {
        let midpoint = (low + high) / 2.0;
        if analytical_residual(x, y, midpoint, peak_depth, imbalance_scale) >= 0.0 {
            low = midpoint;
        } else {
            high = midpoint;
        }
    }
    (low + high) / 2.0
}

fn analytical_inner_marginal_from_d(
    x: f64,
    y: f64,
    d: f64,
    peak_depth: f64,
    imbalance_scale: f64,
) -> f64 {
    let e = d * d - 4.0 * x * y;
    let b = imbalance_scale * d * d + e;
    let concentration_factor = 2.0 * peak_depth * imbalance_scale * imbalance_scale * d.powi(3);
    let direct = b * b;
    let interaction = 2.0 * e * b;
    let x_core = concentration_factor * (y + 2.0 * x - d) + direct + interaction;
    let y_core = concentration_factor * (x + 2.0 * y - d) + direct + interaction;
    y * x_core / (x * y_core)
}

fn analytical_inner_marginal(x: u128, y: u128, peak_depth_nad: u128, imbalance_scale_nad: u128) -> f64 {
    let normalization = x.max(y) as f64;
    let normalized_x = x as f64 / normalization;
    let normalized_y = y as f64 / normalization;
    let peak_depth = peak_depth_nad as f64 / NAD as f64;
    let imbalance_scale = imbalance_scale_nad as f64 / NAD as f64;
    let d = analytical_invariant(normalized_x, normalized_y, peak_depth, imbalance_scale);
    analytical_inner_marginal_from_d(
        normalized_x,
        normalized_y,
        d,
        peak_depth,
        imbalance_scale,
    )
}

fn analytical_exact_in(x: f64, y: f64, dx: f64, peak_depth: f64, imbalance_scale: f64) -> f64 {
    let d = analytical_invariant(x, y, peak_depth, imbalance_scale);
    let fixed = x + dx;
    let mut low = f64::MIN_POSITIVE;
    let mut high = y;
    for _ in 0..160 {
        let midpoint = (low + high) / 2.0;
        if analytical_hybrid_residual(fixed, midpoint, d, peak_depth, imbalance_scale) >= 0.0 {
            high = midpoint;
        } else {
            low = midpoint;
        }
    }
    y - (low + high) / 2.0
}

fn signed_residual_value(
    x: u128,
    y: u128,
    d: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) -> (ResidualSign, U512) {
    let (positive, negative) = residual_terms(x, y, d, peak_depth_nad, imbalance_scale_nad).unwrap();
    (
        if positive >= negative {
            ResidualSign::NonNegative
        } else {
            ResidualSign::Negative
        },
        residual_magnitude(positive, negative),
    )
}

#[test]
fn direct_equation_and_cleared_residual_have_the_same_sign() {
    let fixtures = [
        (900_000_000_000_u128, 1_400_000_000_000_u128),
        (2_000_000_000_000, 250_000_000_000),
        (125_000_000_000, 4_000_000_000_000),
    ];
    for (x, y) in fixtures {
        let low = 2.0 * ((x as f64) * (y as f64)).sqrt();
        let high = (x + y) as f64;
        for fraction in [0.01_f64, 0.2, 0.5, 0.8, 0.99] {
            let d = (low + fraction * (high - low)) as u128;
            let analytical = analytical_residual(x as f64, y as f64, d as f64, 200.0, 0.1);
            let (integer_sign, _) = signed_residual_value(x, y, d, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH);
            let analytical_sign = if analytical >= 0.0 {
                ResidualSign::NonNegative
            } else {
                ResidualSign::Negative
            };
            assert_eq!(integer_sign, analytical_sign, "x={x}, y={y}, d={d}");
        }
    }
}

#[test]
fn shared_invariant_newton_evaluation_matches_independent_formulas() {
    for (x, y) in [
        (900_000_000_000_u128, 1_400_000_000_000_u128),
        (1_000_000_000_000, 1_001_000_000_000),
        (2_000_000_000_000, 250_000_000_000),
    ] {
        let prepared =
            concentrated_prepare_curve(x, y, NAD as u128, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let d = prepared.invariant_d();
        let shared = invariant_residual_and_derivative(x, y, d, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        assert_eq!(
            shared.0,
            residual_terms(x, y, d, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap()
        );
        assert_eq!(
            shared.1,
            invariant_residual_derivative(x, y, d, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap()
        );
    }
}

fn assert_certified_successor_matches_cold(
    predecessor_base: u128,
    predecessor_quote: u128,
    successor_base: u128,
    successor_quote: u128,
    center: u128,
    peak_depth: u128,
    imbalance_scale: u128,
) -> (usize, usize) {
    let predecessor =
        concentrated_prepare_curve(predecessor_base, predecessor_quote, center, peak_depth, imbalance_scale).unwrap();
    let (predecessor_low, predecessor_high) = predecessor.invariant_bracket();

    reset_residual_evaluations();
    let successor = concentrated_prepare_continuous_successor_from_bracket(
        predecessor_base,
        predecessor_quote,
        successor_base,
        successor_quote,
        center,
        peak_depth,
        imbalance_scale,
        predecessor_low,
        predecessor_high,
    )
    .unwrap();
    let successor_evaluations = residual_evaluations();

    reset_residual_evaluations();
    let cold = prepare_curve_internal(
        successor_base,
        successor_quote,
        center,
        peak_depth,
        imbalance_scale,
        None,
        CONTINUOUS_SUCCESSOR_PROOF_DENOMINATOR,
    )
    .unwrap();
    let cold_evaluations = residual_evaluations();

    let (successor_low, successor_high) = successor.invariant_bracket();
    let (cold_low, cold_high) = cold.invariant_bracket();
    assert!(successor_low <= cold_high && cold_low <= successor_high);
    assert_eq!(successor.base_reserve_nad(), successor_base);
    assert_eq!(successor.quote_reserve_nad(), successor_quote);

    let x = successor.base_common_nad();
    let y = successor.quote_common_nad();
    assert_eq!(
        hybrid_residual_sign(x, y, successor_low, peak_depth, imbalance_scale).unwrap(),
        ResidualSign::NonNegative
    );
    if successor_low < successor_high {
        assert_eq!(
            hybrid_residual_sign(x, y, successor_high, peak_depth, imbalance_scale).unwrap(),
            ResidualSign::Negative
        );
    }
    assert!(successor.continuous_successor_evaluation().is_ok());
    (successor_evaluations, cold_evaluations)
}

#[test]
fn theorem_successor_bracket_matches_cold_across_coordinates_and_branches() {
    let fixtures = [
        (1_000_000_000_000, 1_000_000_000_000, 1_100_000_000_000, 1_000_000_000_000),
        (1_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000, 1_100_000_000_000),
        (4_000_000_000_000, 100_000_000_000, 4_100_000_000_000, 100_000_000_000),
        (4_000_000_000_000, 100_000_000_000, 4_000_000_000_000, 800_000_000_000),
        (4_000_000_000_000, 100_000_000_000, 4_000_000_000_000, 4_000_000_000_000),
    ];
    let mut improved = false;
    for (predecessor_base, predecessor_quote, successor_base, successor_quote) in fixtures {
        let (successor_evaluations, cold_evaluations) = assert_certified_successor_matches_cold(
            predecessor_base,
            predecessor_quote,
            successor_base,
            successor_quote,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        );
        improved |= successor_evaluations < cold_evaluations;
    }
    assert!(improved, "the inherited theorem bracket never reduced residual work");
}

#[test]
fn theorem_successor_reuses_floor_normalized_noop_and_falls_back_for_two_coordinate_change() {
    let center = 1_u128;
    let predecessor_base = 1_000_000_000_000_000_000_u128;
    let predecessor_quote = 1_000_000_000_u128;
    let predecessor = concentrated_prepare_curve(
        predecessor_base,
        predecessor_quote,
        center,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .unwrap();
    let (low, high) = predecessor.invariant_bracket();
    let normalized_noop = concentrated_prepare_continuous_successor_from_bracket(
        predecessor_base,
        predecessor_quote,
        predecessor_base + 1,
        predecessor_quote,
        center,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
        low,
        high,
    )
    .unwrap();
    assert_eq!(normalized_noop.invariant_bracket(), (low, high));

    assert_certified_successor_matches_cold(
        predecessor_base,
        predecessor_quote,
        predecessor_base + NAD as u128,
        predecessor_quote + 1,
        center,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    );
}

fn assert_canonical_marginal_tracks_both_executable_directions(prepared: ConcentratedPreparedCurve) {
    let marginal = prepared.marginal_price_nad().unwrap();
    assert!(marginal > 0);

    let base_probe = (prepared.base_reserve_nad() / 1_000_000).max(1);
    let quote_out = prepared
        .quote_exact_in(base_probe, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();
    assert!(quote_out > 0);
    let forward_average = mul_div_u128(quote_out, NAD as u128, base_probe).unwrap();

    let quote_probe = (prepared.quote_reserve_nad() / 1_000_000).max(1);
    let base_out = prepared
        .quote_exact_in(quote_probe, ConcentratedSwapDirection::QuoteToBase)
        .unwrap();
    assert!(base_out > 0);
    let reverse_average = mul_div_u128(quote_probe, NAD as u128, base_out).unwrap();

    // The probe is one millionth of a reserve. The configured parameter and
    // reserve floors keep integer output flooring plus the deliberate
    // pool-favoring shoulder kink inside the 25 ppm protocol budget.
    let tolerance = mul_div_u128_ceil(marginal, 25, PPM_DENOMINATOR).unwrap().max(10);
    assert!(
        forward_average <= marginal.saturating_add(tolerance),
        "forward={forward_average}, marginal={marginal}, tolerance={tolerance}"
    );
    assert!(
        reverse_average.saturating_add(tolerance) >= marginal,
        "reverse={reverse_average}, marginal={marginal}, tolerance={tolerance}"
    );
}

#[test]
fn canonical_high_marginal_tracks_execution_across_centers_branches_and_parameter_extremes() {
    let parameter_sets = [
        (2 * NAD as u128, 100_u128),
        (PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH),
        (CONCENTRATED_MAX_PEAK_DEPTH_NAD, CONCENTRATED_MAX_IMBALANCE_SCALE_NAD),
    ];
    for center in [NAD as u128 / 1_000, NAD as u128, 1_000 * NAD as u128] {
        let base = 20_000_000_000_000_u128;
        let balanced_quote = mul_div_u128(base, center, NAD as u128).unwrap();
        for (quote_numerator, quote_denominator) in [(1_u128, 1_u128), (9, 10), (1, 20), (20, 1)] {
            let quote = mul_div_u128(balanced_quote, quote_numerator, quote_denominator).unwrap();
            for (peak_depth, imbalance_scale) in parameter_sets {
                let prepared =
                    concentrated_prepare_curve(base, quote, center, peak_depth, imbalance_scale).unwrap();
                assert_eq!(
                    prepared.shoulder_relation,
                    concentrated_hybrid_shoulder_relation(
                        prepared.base_common_nad(),
                        prepared.quote_common_nad(),
                        peak_depth,
                        imbalance_scale,
                    )
                    .unwrap()
                );
                assert_canonical_marginal_tracks_both_executable_directions(prepared);
            }
        }
    }
}

#[test]
fn canonical_high_marginal_is_exactly_reproducible_from_persisted_bracket() {
    for (base, quote, center) in [
        (1_000_000_000_000_u128, 1_000_000_000_000_u128, NAD as u128),
        (4_000_000_000_000, 100_000_000_000, NAD as u128),
        (500_000_000_000, 1_000_000_000_000, 2 * NAD as u128),
    ] {
        let prepared =
            concentrated_prepare_curve(base, quote, center, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let (low, high) = prepared.invariant_bracket();
        let restored = concentrated_restore_prepared_curve_from_bracket(
            base,
            quote,
            center,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
            low,
            high,
        )
        .unwrap();
        assert_eq!(restored.evaluation().unwrap(), prepared.evaluation().unwrap());
        assert_eq!(restored.shoulder_relation, prepared.shoulder_relation);

        // A cold solve may choose a neighboring certified upper endpoint, but
        // its executable marginal must remain inside the same proof-scale
        // neighborhood. Live state always restores the persisted endpoint.
        let warm = concentrated_prepare_curve_with_hint(
            base,
            quote,
            center,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
            low + (high - low) / 2,
        )
        .unwrap();
        let price = prepared.marginal_price_nad().unwrap();
        let warm_price = warm.marginal_price_nad().unwrap();
        let tolerance = mul_div_u128_ceil(price.max(warm_price), 25, PPM_DENOMINATOR)
            .unwrap()
            .max(5);
        assert!(price.abs_diff(warm_price) <= tolerance);
    }
}

#[test]
fn cached_shoulder_relation_preserves_directional_equality_rule() {
    // At exact equality the invariant stays on the inner branch. The
    // base-to-quote marginal uses the CPMM side when base is abundant and the
    // inner side when base is scarce, matching an infinitesimal base input.
    for (x, y) in [
        (1_100_000_000_000_u128, 1_000_000_000_000_u128),
        (1_000_000_000_000, 1_100_000_000_000),
    ] {
        let mut prepared =
            concentrated_prepare_curve(x, y, NAD as u128, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        // Equality is rare in integer reserve coordinates; inject the already
        // certified selector to exercise its deterministic one-sided dispatch.
        prepared.shoulder_relation = Ordering::Equal;
        let expected = if x >= y {
            mul_div_u128(y, NAD as u128, x).unwrap()
        } else {
            concentrated_inner_marginal_price_from_common(
                x,
                y,
                prepared.invariant_bracket().1,
                NAD as u128,
                PEAK_DEPTH_200,
                IMBALANCE_SCALE_TENTH,
            )
            .unwrap()
        };
        assert_eq!(prepared.marginal_price_nad().unwrap(), expected);
    }
}

fn assert_canonical_marginal_conditioned_at_inner_state(
    x: u128,
    y: u128,
    peak_depth_nad: u128,
    imbalance_scale_nad: u128,
) {
    assert!(x >= MIN_INNER_COMMON_RESERVE && y >= MIN_INNER_COMMON_RESERVE);
    assert_ne!(
        concentrated_hybrid_shoulder_relation(x, y, peak_depth_nad, imbalance_scale_nad).unwrap(),
        Ordering::Less
    );

    // Retained-surcharge successors use the coarser certified bracket. Build
    // that bracket through the real one-coordinate successor proof instead of
    // cold-solving with a tolerance that production never uses cold.
    let predecessor = concentrated_prepare_curve(
        x,
        y,
        NAD as u128,
        peak_depth_nad,
        imbalance_scale_nad,
    )
    .unwrap_or_else(|error| {
        panic!(
            "conditioning predecessor failed: x={x}, y={y}, peak={peak_depth_nad}, fade={imbalance_scale_nad}: {error:?}"
        )
    });
    let (successor_x, successor_y) = if x <= y && x < MAX_COMMON_RESERVE {
        (x + 1, y)
    } else if y < MAX_COMMON_RESERVE {
        (x, y + 1)
    } else {
        // The fully saturated balanced domain cannot admit another input atom;
        // its exact singleton bracket was already certified above.
        assert_eq!(x, y);
        return;
    };
    let (predecessor_low, predecessor_high) = predecessor.invariant_bracket();
    let prepared = concentrated_prepare_continuous_successor_from_bracket(
        x,
        y,
        successor_x,
        successor_y,
        NAD as u128,
        peak_depth_nad,
        imbalance_scale_nad,
        predecessor_low,
        predecessor_high,
    )
    .unwrap_or_else(|error| {
        panic!(
            "conditioning successor failed: x={x}, y={y}, successor_x={successor_x}, successor_y={successor_y}, peak={peak_depth_nad}, fade={imbalance_scale_nad}: {error:?}"
        )
    });
    let x = prepared.base_common_nad();
    let y = prepared.quote_common_nad();
    assert_ne!(
        concentrated_hybrid_shoulder_relation(x, y, peak_depth_nad, imbalance_scale_nad).unwrap(),
        Ordering::Less
    );
    let (d_low, d_high) = prepared.invariant_bracket();
    let p_low = concentrated_inner_marginal_price_from_common(
        x,
        y,
        d_low,
        NAD as u128,
        peak_depth_nad,
        imbalance_scale_nad,
    )
    .unwrap();
    let p_high = concentrated_inner_marginal_price_from_common(
        x,
        y,
        d_high,
        NAD as u128,
        peak_depth_nad,
        imbalance_scale_nad,
    )
    .unwrap();
    let endpoint_spread_ppm = p_low.abs_diff(p_high) as f64 * PPM_DENOMINATOR as f64
        / p_low.min(p_high).max(1) as f64;
    let reference = analytical_inner_marginal(x, y, peak_depth_nad, imbalance_scale_nad);
    let canonical = p_high as f64 / NAD as f64;
    let true_error_ppm = (canonical / reference - 1.0).abs() * PPM_DENOMINATOR as f64;

    assert!(
        endpoint_spread_ppm <= 25.0,
        "endpoint spread {endpoint_spread_ppm:.6} ppm: x={x}, y={y}, peak={peak_depth_nad}, fade={imbalance_scale_nad}, bracket={d_low}..={d_high}"
    );
    assert!(
        true_error_ppm <= 25.0,
        "true error {true_error_ppm:.6} ppm: x={x}, y={y}, peak={peak_depth_nad}, fade={imbalance_scale_nad}, bracket={d_low}..={d_high}"
    );
}

#[test]
fn runtime_parameter_domain_keeps_canonical_marginal_within_25_ppm() {
    // Include configured endpoint extrema/fade neighborhoods plus actual
    // first, midpoint, and penultimate states from both CPMM ramp directions.
    // This prevents the test model from inventing unreachable low-peak,
    // broad-fade pairs.
    let peaks = [
        MIN_AMM_PEAK_DEPTH_NAD as u128,
        PEAK_DEPTH_200,
        MAX_AMM_PEAK_DEPTH_NAD as u128,
    ];
    let fades = [
        MIN_AMM_IMBALANCE_SCALE_NAD as u128,
        MIN_AMM_IMBALANCE_SCALE_NAD as u128 + 1,
        1_000,
        NAD as u128 / 10,
        MAX_AMM_IMBALANCE_SCALE_NAD as u128,
    ];
    let reserve_scales = [
        MIN_INNER_COMMON_RESERVE,
        10 * MIN_INNER_COMMON_RESERVE,
        1_000_000_000_000,
        1_000_000_000_000_000,
        MAX_COMMON_RESERVE,
    ];

    let mut parameter_pairs = Vec::new();
    for peak_depth_nad in peaks {
        for imbalance_scale_nad in fades {
            parameter_pairs.push((peak_depth_nad, imbalance_scale_nad));
        }
    }
    let cpmm = AmmCurveParameters::cpmm();
    for target in [
        AmmCurveParameters {
            peak_depth_nad: MIN_AMM_PEAK_DEPTH_NAD,
            imbalance_scale_nad: MIN_AMM_IMBALANCE_SCALE_NAD,
        },
        AmmCurveParameters {
            peak_depth_nad: MIN_AMM_PEAK_DEPTH_NAD,
            imbalance_scale_nad: MAX_AMM_IMBALANCE_SCALE_NAD,
        },
        AmmCurveParameters {
            peak_depth_nad: MAX_AMM_PEAK_DEPTH_NAD,
            imbalance_scale_nad: MIN_AMM_IMBALANCE_SCALE_NAD,
        },
        AmmCurveParameters {
            peak_depth_nad: MAX_AMM_PEAK_DEPTH_NAD,
            imbalance_scale_nad: MAX_AMM_IMBALANCE_SCALE_NAD,
        },
    ] {
        for duration in [MIN_AMM_RAMP_DURATION_SLOTS, MAX_AMM_RAMP_DURATION_SLOTS] {
            for (start, end) in [(cpmm, target), (target, cpmm)] {
                let ramp = AmmRamp::start(start, end, 1_000, duration).unwrap();
                for slot in [ramp.start_slot + 1, ramp.start_slot + duration / 2, ramp.end_slot - 1] {
                    let parameters = ramp.parameters_at(start, slot);
                    assert!(parameters.peak_depth_nad > 0);
                    assert!(parameters.imbalance_scale_nad >= MIN_AMM_IMBALANCE_SCALE_NAD);
                    parameter_pairs.push((
                        parameters.peak_depth_nad as u128,
                        parameters.imbalance_scale_nad as u128,
                    ));
                }
            }
        }
    }
    parameter_pairs.sort_unstable();
    parameter_pairs.dedup();

    let mut samples = 0_usize;
    for (peak_depth_nad, imbalance_scale_nad) in parameter_pairs {
            for x in reserve_scales {
                let mut lower = 1_u128;
                let mut upper = x;
                while lower < upper {
                    let middle = lower + (upper - lower) / 2;
                    if concentrated_hybrid_shoulder_relation(
                        x,
                        middle,
                        peak_depth_nad,
                        imbalance_scale_nad,
                    )
                    .unwrap()
                        == Ordering::Less
                    {
                        lower = middle + 1;
                    } else {
                        upper = middle;
                    }
                }
                let first_supported_inner = lower.max(MIN_INNER_COMMON_RESERVE).min(x);
                for offset in [0_u128, 1, 100] {
                    let y = first_supported_inner.saturating_add(offset).min(x);
                    if concentrated_hybrid_shoulder_relation(
                        x,
                        y,
                        peak_depth_nad,
                        imbalance_scale_nad,
                    )
                    .unwrap()
                        == Ordering::Less
                    {
                        continue;
                    }
                    assert_canonical_marginal_conditioned_at_inner_state(
                        x,
                        y,
                        peak_depth_nad,
                        imbalance_scale_nad,
                    );
                    if x != y {
                        assert_canonical_marginal_conditioned_at_inner_state(
                            y,
                            x,
                            peak_depth_nad,
                            imbalance_scale_nad,
                        );
                    }
                    samples += 1;
                }
            }
    }
    assert!(samples >= 200);
}

#[test]
fn integer_invariant_matches_independent_analytical_reference() {
    let fixtures = [
        (1_000_000_000_000_u128, 1_000_000_000_000_u128),
        (900_000_000_000, 1_400_000_000_000),
        (2_000_000_000_000, 250_000_000_000),
        (125_000_000_000, 4_000_000_000_000),
    ];
    for (x, y) in fixtures {
        let (low, high, _) = invariant_common_bracket(x, y, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let reference = analytical_invariant(x as f64, y as f64, 200.0, 0.1);
        let tolerance = (reference * 2e-9).max(2.0);
        assert!((low as f64 - reference).abs() <= tolerance, "low: x={x}, y={y}");
        assert!((high as f64 - reference).abs() <= tolerance, "high: x={x}, y={y}");
    }
}

#[test]
fn exact_in_is_conservative_and_close_to_analytical_reference() {
    let fixtures = [
        (1_000_000_000_000_u128, 1_000_000_000_000_u128, 10_000_000_000_u128),
        (900_000_000_000, 1_400_000_000_000, 50_000_000_000),
        (2_000_000_000_000, 250_000_000_000, 25_000_000_000),
    ];
    for (x, y, dx) in fixtures {
        let actual = quote_common_exact_in(x, y, dx, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let reference = analytical_exact_in(x as f64, y as f64, dx as f64, 200.0, 0.1);
        assert!(
            actual as f64 <= reference + 2.0,
            "quote overpays: x={x}, y={y}, dx={dx}"
        );
        assert!(reference - actual as f64 <= (reference * 40e-6).max(4.0));
    }
}

#[test]
fn centered_extra_depth_improves_execution_without_changing_spot_price() {
    let reserve = 1_000_000_000_000_u128;
    let dx = reserve / 100;
    let concentrated = concentrated_quote_exact_in(
        reserve,
        reserve,
        dx,
        ConcentratedSwapDirection::BaseToQuote,
        NAD as u128,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .unwrap();
    let cpmm = calculate_normalized_amount_out(reserve, reserve, dx).unwrap();
    assert!(concentrated > cpmm);
    assert_eq!(
        concentrated_marginal_price_nad(reserve, reserve, NAD as u128, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH,).unwrap(),
        NAD as u128
    );
}

#[test]
fn zero_parameters_are_exact_cpmm_for_exact_in_and_exact_out() {
    let x = 1_234_567_890_123_u128;
    let y = 987_654_321_987_u128;
    let dx = 12_345_678_901_u128;
    let dy = 9_876_543_210_u128;
    assert_eq!(
        concentrated_quote_exact_in(x, y, dx, ConcentratedSwapDirection::BaseToQuote, NAD as u128, 0, 0,).unwrap(),
        calculate_normalized_amount_out(x, y, dx).unwrap()
    );
    assert_eq!(
        concentrated_quote_exact_out(x, y, dy, ConcentratedSwapDirection::BaseToQuote, NAD as u128, 0, 0,).unwrap(),
        calculate_normalized_amount_in(x, y, dy).unwrap()
    );
}

#[test]
fn cpmm_prepared_domain_exceeds_the_concentrated_common_reserve_bound() {
    let x = MAX_COMMON_RESERVE + 1;
    let y = MAX_COMMON_RESERVE + 17;
    let dx = 1_000_000_u128;
    let prepared = concentrated_prepare_curve(x, y, NAD as u128, 0, 0).unwrap();
    assert_eq!(
        concentrated_hybrid_branch(x, y, NAD as u128, 0, 0).unwrap(),
        ConcentratedHybridBranch::Inner
    );
    let output = prepared
        .quote_exact_in(dx, ConcentratedSwapDirection::BaseToQuote)
        .unwrap();

    assert_eq!(output, calculate_normalized_amount_out(x, y, dx).unwrap());
    assert!(prepared.evaluation().is_ok());

    let successor = prepared.prepare_successor(x + dx, y - output).unwrap();
    assert!(successor.continuous_successor_evaluation().is_ok());

    let (invariant_low, invariant_high) = prepared.invariant_bracket();
    let restored =
        concentrated_restore_prepared_curve_from_bracket(x, y, NAD as u128, 0, 0, invariant_low, invariant_high)
            .unwrap();
    assert_eq!(restored.evaluation().unwrap(), prepared.evaluation().unwrap());

    assert!(concentrated_prepare_curve(x, y, NAD as u128, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).is_err());
}

#[test]
fn exact_out_input_replays_to_at_least_the_requested_output() {
    let x = 1_000_000_000_000_u128;
    let y = 1_300_000_000_000_u128;
    for dy in [1_000_000_u128, 1_000_000_000, 25_000_000_000, 100_000_000_000] {
        let input = concentrated_quote_exact_out(
            x,
            y,
            dy,
            ConcentratedSwapDirection::BaseToQuote,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        let replay = concentrated_quote_exact_in(
            x,
            y,
            input,
            ConcentratedSwapDirection::BaseToQuote,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        assert!(replay >= dy, "dy={dy}, input={input}, replay={replay}");
    }
}

#[test]
fn eighty_percent_exact_out_replays_in_both_directions() {
    let reserve = 1_000_000_000_000_u128;
    let requested = reserve * 8 / 10;
    for direction in [
        ConcentratedSwapDirection::BaseToQuote,
        ConcentratedSwapDirection::QuoteToBase,
    ] {
        let input = concentrated_quote_exact_out(
            reserve,
            reserve,
            requested,
            direction,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        let replay = concentrated_quote_exact_in(
            reserve,
            reserve,
            input,
            direction,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        assert!(
            replay >= requested,
            "direction={direction:?}, input={input}, replay={replay}"
        );
        let tail_product = reserve * reserve * (NAD as u128 - IMBALANCE_SCALE_TENTH) / NAD as u128;
        let mathematical_input = mul_div_u128_ceil(tail_product, 1, reserve - requested).unwrap() - reserve;
        let max_conservative_input =
            mul_div_u128_ceil(mathematical_input, PPM_DENOMINATOR + 1_000, PPM_DENOMINATOR).unwrap();
        assert!(
            input <= max_conservative_input,
            "input={input}, mathematical={mathematical_input}"
        );
    }
}

#[test]
fn restorative_exact_out_crosses_from_cpmm_tail_into_inner_and_replays() {
    let low = 100_000_000_000_u128;
    let high = 4_000_000_000_000_u128;
    let requested = 3_200_000_000_000_u128;
    for (base, quote, direction) in [
        (low, high, ConcentratedSwapDirection::BaseToQuote),
        (high, low, ConcentratedSwapDirection::QuoteToBase),
    ] {
        let input = concentrated_quote_exact_out(
            base,
            quote,
            requested,
            direction,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        let replay = concentrated_quote_exact_in(
            base,
            quote,
            input,
            direction,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        assert!(
            replay >= requested,
            "direction={direction:?}, input={input}, replay={replay}"
        );
    }
}

#[test]
fn exact_out_upper_and_utilized_lower_bound_bracket_every_hybrid_region() {
    let reserve = 1_000_000_000_000_u128;
    for (peak_depth, imbalance_scale) in [
        (2 * NAD as u128, 10),
        (PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH),
        (CONCENTRATED_MAX_PEAK_DEPTH_NAD, CONCENTRATED_MAX_IMBALANCE_SCALE_NAD),
    ] {
        for direction in [
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ] {
            // Small output stays in the inner shoulder; medium and large
            // outputs cross the shoulder and traverse the exact CPMM tail.
            for requested in [reserve / 100, reserve * 45 / 100, reserve * 95 / 100] {
                assert_exact_out_inverse_bracket(
                    reserve,
                    reserve,
                    requested,
                    direction,
                    NAD as u128,
                    peak_depth,
                    imbalance_scale,
                );
            }
        }
    }

    // Exact CPMM tail in the outward direction and a restoring trade that
    // crosses from that tail back through the inner shoulder, mirrored across
    // both asset directions.
    let scarce = 100_000_000_000_u128;
    let abundant = 4_000_000_000_000_u128;
    for (base, quote, outward, restoring) in [
        (
            abundant,
            scarce,
            ConcentratedSwapDirection::BaseToQuote,
            ConcentratedSwapDirection::QuoteToBase,
        ),
        (
            scarce,
            abundant,
            ConcentratedSwapDirection::QuoteToBase,
            ConcentratedSwapDirection::BaseToQuote,
        ),
    ] {
        let outward_reserve = match outward {
            ConcentratedSwapDirection::BaseToQuote => quote,
            ConcentratedSwapDirection::QuoteToBase => base,
        };
        assert_exact_out_inverse_bracket(
            base,
            quote,
            outward_reserve / 10,
            outward,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        );
        let restoring_reserve = match restoring {
            ConcentratedSwapDirection::BaseToQuote => quote,
            ConcentratedSwapDirection::QuoteToBase => base,
        };
        assert_exact_out_inverse_bracket(
            base,
            quote,
            restoring_reserve * 8 / 10,
            restoring,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        );
    }

    // Unequal center exercises both raw/common conversion roundings.
    assert_exact_out_inverse_bracket(
        reserve / 2,
        reserve,
        reserve / 3,
        ConcentratedSwapDirection::BaseToQuote,
        2 * NAD as u128,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    );
    assert_exact_out_inverse_bracket(
        reserve / 2,
        reserve,
        reserve / 6,
        ConcentratedSwapDirection::QuoteToBase,
        2 * NAD as u128,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    );
}

#[test]
fn shoulder_is_continuous_and_has_pool_favoring_outward_kink() {
    let d = 2_000_000_000_000_u128;
    let shoulder = concentrated_hybrid_shoulder_from_d(d, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();

    assert!(shoulder.inner_low_marginal_nad > shoulder.tail_low_marginal_nad);
    assert_eq!(
        shoulder.tail_product_common,
        d * d / 4 * (NAD as u128 - IMBALANCE_SCALE_TENTH) / NAD as u128
    );

    // One atom on either side of the rounded shoulder must not create a jump
    // in the homogeneous invariant value.
    let inner_d = invariant_common(
        shoulder.high_common,
        shoulder.low_common.saturating_add(1),
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .unwrap();
    let outer_d = invariant_common(
        shoulder.high_common,
        shoulder.low_common.saturating_sub(1),
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .unwrap();
    assert!(inner_d.abs_diff(d) <= 16, "inner_d={inner_d}, d={d}");
    assert!(outer_d.abs_diff(d) <= 16, "outer_d={outer_d}, d={d}");
    assert!(inner_d.abs_diff(outer_d) <= 32);
}

#[test]
fn shoulder_kink_is_pool_favoring_across_operator_parameter_extremes() {
    let d = 2_000_000_000_000_000_u128;
    for peak_depth_nad in [2, 10, 200, 2_000].map(|value| value * NAD as u128) {
        for imbalance_scale_nad in [10_u128, NAD as u128 / 1_000, NAD as u128 / 10, 199_000_000] {
            let shoulder = concentrated_hybrid_shoulder_from_d(d, peak_depth_nad, imbalance_scale_nad).unwrap();
            assert!(
                shoulder.inner_low_marginal_nad >= shoulder.tail_low_marginal_nad,
                "peak={peak_depth_nad}, scale={imbalance_scale_nad}, inner={}, tail={}",
                shoulder.inner_low_marginal_nad,
                shoulder.tail_low_marginal_nad,
            );
        }
    }
}

#[test]
fn branches_are_symmetric_and_outer_quotes_are_exact_cpmm() {
    let center = 3 * NAD as u128;
    let quote_scarce = (4_000_000_000_000_u128, 100_000_000_000_u128);
    let base_scarce = (100_000_000_000_u128, 4_000_000_000_000_u128);
    let (quote_scarce_x, quote_scarce_y) = normalize_reserves(quote_scarce.0, quote_scarce.1, center).unwrap();
    let (base_scarce_x, base_scarce_y) = normalize_reserves(base_scarce.0, base_scarce.1, center).unwrap();
    assert_eq!(
        concentrated_hybrid_branch_from_common(quote_scarce_x, quote_scarce_y, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH,)
            .unwrap(),
        ConcentratedHybridBranch::QuoteScarceTail
    );
    assert_eq!(
        concentrated_hybrid_branch_from_common(base_scarce_x, base_scarce_y, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH,)
            .unwrap(),
        ConcentratedHybridBranch::BaseScarceTail
    );
    let symmetric_x = 4_000_000_000_000_u128;
    let symmetric_y = 100_000_000_000_u128;
    assert_eq!(
        invariant_common(symmetric_x, symmetric_y, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap(),
        invariant_common(symmetric_y, symmetric_x, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap()
    );

    let base_in = quote_scarce.0 / 100;
    let quote_in = base_scarce.1 / 100;
    assert_eq!(
        concentrated_quote_exact_in(
            quote_scarce.0,
            quote_scarce.1,
            base_in,
            ConcentratedSwapDirection::BaseToQuote,
            center,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap(),
        calculate_normalized_amount_out(quote_scarce.0, quote_scarce.1, base_in).unwrap()
    );
    assert_eq!(
        concentrated_quote_exact_in(
            base_scarce.0,
            base_scarce.1,
            quote_in,
            ConcentratedSwapDirection::QuoteToBase,
            center,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap(),
        calculate_normalized_amount_out(base_scarce.1, base_scarce.0, quote_in).unwrap()
    );

    let quote_out = quote_scarce.1 / 100;
    assert_eq!(
        concentrated_quote_exact_out(
            quote_scarce.0,
            quote_scarce.1,
            quote_out,
            ConcentratedSwapDirection::BaseToQuote,
            center,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap(),
        calculate_normalized_amount_in(quote_scarce.0, quote_scarce.1, quote_out).unwrap()
    );
    assert_eq!(
        concentrated_marginal_price_nad(
            quote_scarce.0,
            quote_scarce.1,
            center,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap(),
        quote_scarce.1 * NAD as u128 / quote_scarce.0
    );
}

#[test]
fn exact_in_crosses_shoulder_monotonically_atom_by_atom() {
    let reserve = 1_000_000_000_000_u128;
    let mut low = 0_u128;
    let mut high = reserve * 8;
    while high - low > 1 {
        let midpoint = low + (high - low) / 2;
        let output = quote_common_exact_in(reserve, reserve, midpoint, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let branch = concentrated_hybrid_branch_from_common(
            reserve + midpoint,
            reserve - output,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        if branch == ConcentratedHybridBranch::QuoteScarceTail {
            high = midpoint;
        } else {
            low = midpoint;
        }
    }
    let before = quote_common_exact_in(reserve, reserve, high - 1, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
    let at = quote_common_exact_in(reserve, reserve, high, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
    let after = quote_common_exact_in(reserve, reserve, high + 1, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
    assert!(before <= at && at <= after, "before={before}, at={at}, after={after}");
    assert!(at - before <= 2 && after - at <= 2);
}

#[test]
fn restorative_exact_in_crosses_from_cpmm_tail_without_a_haircut_cliff() {
    let x = 100_000_000_000_u128;
    let y = 4_000_000_000_000_u128;
    let mut low = 0_u128;
    let mut high = 1_000_000_000_000_u128;
    while high - low > 1 {
        let midpoint = low + (high - low) / 2;
        let output = quote_common_exact_in(x, y, midpoint, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let branch =
            concentrated_hybrid_branch_from_common(x + midpoint, y - output, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH)
                .unwrap();
        if branch == ConcentratedHybridBranch::BaseScarceTail {
            low = midpoint;
        } else {
            high = midpoint;
        }
    }
    let before = quote_common_exact_in(x, y, high - 1, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
    let at = quote_common_exact_in(x, y, high, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
    let after = quote_common_exact_in(x, y, high + 1, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
    assert!(before <= at && at <= after, "before={before}, at={at}, after={after}");
    assert!(at - before <= 3 && after - at <= 3);
}

#[test]
fn crossing_the_shoulder_cannot_create_a_round_trip_gain() {
    let reserve = 1_000_000_000_000_u128;
    let base_in = reserve * 3;
    let quote_out = concentrated_quote_exact_in(
        reserve,
        reserve,
        base_in,
        ConcentratedSwapDirection::BaseToQuote,
        NAD as u128,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .unwrap();
    let base_after = reserve + base_in;
    let quote_after = reserve - quote_out;
    let base_back = concentrated_quote_exact_in(
        base_after,
        quote_after,
        quote_out,
        ConcentratedSwapDirection::QuoteToBase,
        NAD as u128,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .unwrap();
    assert!(base_back <= base_in, "base_in={base_in}, base_back={base_back}");
}

#[test]
fn parameter_domain_is_explicit_and_fail_closed() {
    let reserve = 1_000_000_000_000_u128;
    assert!(concentrated_prepare_curve(reserve, reserve, NAD as u128, PEAK_DEPTH_200, 0).is_err());
    assert!(concentrated_prepare_curve(
        reserve,
        reserve,
        NAD as u128,
        CONCENTRATED_MAX_PEAK_DEPTH_NAD + 1,
        IMBALANCE_SCALE_TENTH,
    )
    .is_err());
    assert!(concentrated_prepare_curve(
        reserve,
        reserve,
        NAD as u128,
        PEAK_DEPTH_200,
        CONCENTRATED_MAX_IMBALANCE_SCALE_NAD + 1,
    )
    .is_err());
}

#[test]
fn inner_common_floor_rejects_dust_while_exact_cpmm_tail_stays_available() {
    let below = MIN_INNER_COMMON_RESERVE - 1;
    assert!(concentrated_prepare_curve(
        below,
        below,
        NAD as u128,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .is_err());

    let supported = concentrated_prepare_curve(
        MIN_INNER_COMMON_RESERVE,
        MIN_INNER_COMMON_RESERVE,
        NAD as u128,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .unwrap();
    assert_eq!(supported.marginal_price_nad().unwrap(), NAD as u128);

    let tail_base = 1_000_000_000_000_u128;
    let tail_quote = MIN_INNER_COMMON_RESERVE / 10;
    let tail = concentrated_prepare_curve(
        tail_base,
        tail_quote,
        NAD as u128,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .unwrap();
    assert_eq!(tail.shoulder_relation, Ordering::Less);
    assert_eq!(
        tail.marginal_price_nad().unwrap(),
        mul_div_u128(tail_quote, NAD as u128, tail_base).unwrap()
    );
}

#[test]
fn documented_maximum_domain_fits_the_wide_integer() {
    let x = MAX_COMMON_RESERVE;
    let y = MAX_COMMON_RESERVE - 1;
    let d = x + y;
    let (positive, negative) = residual_terms(
        x,
        y,
        d,
        CONCENTRATED_MAX_PEAK_DEPTH_NAD,
        CONCENTRATED_MAX_IMBALANCE_SCALE_NAD,
    )
    .unwrap();
    assert!(!positive.is_zero() || !negative.is_zero());
    let (_, derivative) = variable_residual_derivative(
        x,
        y,
        d,
        CONCENTRATED_MAX_PEAK_DEPTH_NAD,
        CONCENTRATED_MAX_IMBALANCE_SCALE_NAD,
    )
    .unwrap();
    assert!(!derivative.is_zero());
}

#[test]
fn canonical_high_kernel_is_bit_exact_at_maximum_domain_and_fails_closed_outside_it() {
    let x = MAX_COMMON_RESERVE;
    let y = MAX_COMMON_RESERVE - 1;
    let (_, d_high, _) = invariant_common_bracket(
        x,
        y,
        CONCENTRATED_MAX_PEAK_DEPTH_NAD,
        CONCENTRATED_MAX_IMBALANCE_SCALE_NAD,
    )
    .unwrap();
    assert_canonical_high_kernel_matches_signed_u512_reference(
        x,
        y,
        d_high,
        u64::MAX as u128,
        CONCENTRATED_MAX_PEAK_DEPTH_NAD,
        CONCENTRATED_MAX_IMBALANCE_SCALE_NAD,
    );

    let reserve = 10 * MIN_INNER_COMMON_RESERVE;
    assert!(canonical_high_reserve_residual_derivative_cores(
        reserve,
        reserve,
        2 * reserve - 1,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .is_err());
    assert!(canonical_high_reserve_residual_derivative_cores(
        reserve,
        reserve,
        2 * reserve + 1,
        PEAK_DEPTH_200,
        IMBALANCE_SCALE_TENTH,
    )
    .is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn canonical_high_kernel_is_bit_exact_for_randomized_certified_inner_states(
        reserve_seed in any::<u64>(),
        offset_ppm in 0_u128..101_u128,
        subtract_offset in any::<bool>(),
        peak_depth_nad in MIN_AMM_PEAK_DEPTH_NAD..=MAX_AMM_PEAK_DEPTH_NAD,
        imbalance_scale_nad in MIN_AMM_IMBALANCE_SCALE_NAD..=MAX_AMM_IMBALANCE_SCALE_NAD,
        center_selector in 0_u8..3_u8,
    ) {
        let domain_width = MAX_COMMON_RESERVE - MIN_INNER_COMMON_RESERVE + 1;
        let x = MIN_INNER_COMMON_RESERVE + reserve_seed as u128 % domain_width;
        let delta = x * offset_ppm / PPM_DENOMINATOR;
        let y = if subtract_offset {
            x.saturating_sub(delta).max(MIN_INNER_COMMON_RESERVE)
        } else if x <= MAX_COMMON_RESERVE - delta {
            x + delta
        } else {
            x.saturating_sub(delta).max(MIN_INNER_COMMON_RESERVE)
        };
        let peak_depth_nad = peak_depth_nad as u128;
        let imbalance_scale_nad = imbalance_scale_nad as u128;
        prop_assume!(
            concentrated_hybrid_shoulder_relation(x, y, peak_depth_nad, imbalance_scale_nad).unwrap()
                != Ordering::Less
        );
        let (_, d_high, _) = invariant_common_bracket(x, y, peak_depth_nad, imbalance_scale_nad).unwrap();
        let center_price_nad = match center_selector {
            0 => NAD as u128 / 1_000,
            1 => NAD as u128,
            _ => 1_000 * NAD as u128,
        };
        assert_canonical_high_kernel_matches_signed_u512_reference(
            x,
            y,
            d_high,
            center_price_nad,
            peak_depth_nad,
            imbalance_scale_nad,
        );
    }

    #[test]
    fn inherited_successor_bracket_matches_fresh_global_certificate(
        x in 1_000_000_000_u128..10_000_000_000_000_u128,
        y in 1_000_000_000_u128..10_000_000_000_000_u128,
        increment in 1_u128..1_000_000_000_000_u128,
        increase_base in any::<bool>(),
    ) {
        let (successor_x, successor_y) = if increase_base {
            (x + increment, y)
        } else {
            (x, y + increment)
        };
        let predecessor =
            concentrated_prepare_curve(x, y, NAD as u128, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let (predecessor_low, predecessor_high) = predecessor.invariant_bracket();
        let successor = concentrated_prepare_continuous_successor_from_bracket(
            x,
            y,
            successor_x,
            successor_y,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
            predecessor_low,
            predecessor_high,
        )
        .unwrap();
        let cold = prepare_curve_internal(
            successor_x,
            successor_y,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
            None,
            CONTINUOUS_SUCCESSOR_PROOF_DENOMINATOR,
        )
        .unwrap();
        let (successor_low, successor_high) = successor.invariant_bracket();
        let (cold_low, cold_high) = cold.invariant_bracket();
        prop_assert!(successor_low <= cold_high && cold_low <= successor_high);
        prop_assert_eq!(
            hybrid_residual_sign(
                successor_x,
                successor_y,
                successor_low,
                PEAK_DEPTH_200,
                IMBALANCE_SCALE_TENTH,
            )
            .unwrap(),
            ResidualSign::NonNegative
        );
        if successor_low < successor_high {
            prop_assert_eq!(
                hybrid_residual_sign(
                    successor_x,
                    successor_y,
                    successor_high,
                    PEAK_DEPTH_200,
                    IMBALANCE_SCALE_TENTH,
                )
                .unwrap(),
                ResidualSign::Negative
            );
        }
    }

    #[test]
    fn large_exact_out_replays_through_ninety_five_percent(
        reserve in 100_000_000_000_u128..2_000_000_000_000_u128,
        output_bps in 5_000_u128..9_501_u128,
        quote_to_base in any::<bool>(),
    ) {
        let requested = reserve * output_bps / 10_000;
        let direction = if quote_to_base {
            ConcentratedSwapDirection::QuoteToBase
        } else {
            ConcentratedSwapDirection::BaseToQuote
        };
        let input = concentrated_quote_exact_out(
            reserve,
            reserve,
            requested,
            direction,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        let replay = concentrated_quote_exact_in(
            reserve,
            reserve,
            input,
            direction,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        prop_assert!(replay >= requested, "requested={requested}, input={input}, replay={replay}");
        let tail_product = reserve * reserve * (NAD as u128 - IMBALANCE_SCALE_TENTH) / NAD as u128;
        let mathematical_input = mul_div_u128_ceil(tail_product, 1, reserve - requested).unwrap() - reserve;
        let max_conservative_input =
            mul_div_u128_ceil(mathematical_input, PPM_DENOMINATOR + 1_000, PPM_DENOMINATOR).unwrap();
        prop_assert!(input <= max_conservative_input, "input={input}, mathematical={mathematical_input}");
    }

    #[test]
    fn hybrid_invariant_is_homogeneous_on_inner_and_tail_branches(
        x in 10_000_000_000_u128..1_000_000_000_000_u128,
        y in 10_000_000_000_u128..1_000_000_000_000_u128,
        scale in 2_u128..20_u128,
    ) {
        let d = invariant_common(x, y, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let scaled_d = invariant_common(
            x * scale,
            y * scale,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        prop_assert!(scaled_d.abs_diff(d * scale) <= scale + 1);
    }

    #[test]
    fn randomized_cross_branch_round_trips_never_gain(
        reserve in 100_000_000_000_u128..1_000_000_000_000_u128,
        input_bps in 1_000_u128..50_001_u128,
        quote_to_base in any::<bool>(),
    ) {
        let input = reserve * input_bps / 10_000;
        let forward = if quote_to_base {
            ConcentratedSwapDirection::QuoteToBase
        } else {
            ConcentratedSwapDirection::BaseToQuote
        };
        let reverse = if quote_to_base {
            ConcentratedSwapDirection::BaseToQuote
        } else {
            ConcentratedSwapDirection::QuoteToBase
        };
        let output = concentrated_quote_exact_in(
            reserve,
            reserve,
            input,
            forward,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        let (base_after, quote_after) = if quote_to_base {
            (reserve - output, reserve + input)
        } else {
            (reserve + input, reserve - output)
        };
        let returned = concentrated_quote_exact_in(
            base_after,
            quote_after,
            output,
            reverse,
            NAD as u128,
            PEAK_DEPTH_200,
            IMBALANCE_SCALE_TENTH,
        )
        .unwrap();
        prop_assert!(returned <= input, "input={input}, output={output}, returned={returned}");
    }

    #[test]
    fn quotes_are_monotone_in_input(
        x in 10_000_000_000_u128..10_000_000_000_000_u128,
        y in 10_000_000_000_u128..10_000_000_000_000_u128,
        first_bps in 1_u128..500_u128,
        extra_bps in 1_u128..500_u128,
    ) {
        let first = x * first_bps / 10_000;
        let second = x * (first_bps + extra_bps) / 10_000;
        let first_out = quote_common_exact_in(x, y, first, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        let second_out = quote_common_exact_in(x, y, second, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        prop_assert!(second_out >= first_out);
        prop_assert!(second_out < y);
    }

    #[test]
    fn invariant_bracket_preserves_its_sign_certificate(
        x in 1_000_000_000_u128..10_000_000_000_000_u128,
        y in 1_000_000_000_u128..10_000_000_000_000_u128,
    ) {
        let (low, high, _) = invariant_common_bracket(x, y, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap();
        prop_assert_eq!(hybrid_residual_sign(x, y, low, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap(), ResidualSign::NonNegative);
        if low != high {
            prop_assert_eq!(hybrid_residual_sign(x, y, high, PEAK_DEPTH_200, IMBALANCE_SCALE_TENTH).unwrap(), ResidualSign::Negative);
        }
    }
}

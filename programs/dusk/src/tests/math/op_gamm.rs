use super::*;
fn point(x: u128, y: u128) -> ConcentratedCurvePoint {
    ConcentratedCurvePoint {
        base_reserve: x,
        quote_reserve: y,
    }
}
#[test]
fn exact_input_solver_certifies_integer_outputs_and_round_trips() {
    for (amp, core, fade) in [(1, 0, 0), (4, 1000, 4000), (32, 100, 0), (2000, 1, 0)] {
        for lambda in [1, 2500, 5000, 9999] {
            for reference in [6, 60, 600] {
                let curve = BoundedOracleCurve::new(
                    ConcentratedCurveParameters {
                        peak_amplification_nad: amp * NAD,
                        core_half_width_bps: core,
                        fade_width_bps: fade,
                    },
                    60 * NAD,
                    reference * NAD,
                    lambda,
                    2 * NAD,
                )
                .unwrap();
                for bps in [1, 100, 2500, 10000] {
                    for forward in [true, false] {
                        let start = ConcentratedCurvePoint {
                            base_reserve: 10_000_000_000,
                            quote_reserve: 600_000_000_000,
                        };
                        let input = (if forward {
                            start.base_reserve
                        } else {
                            start.quote_reserve
                        }) * bps
                            / 10_000;
                        let (output, end) = curve.quote_exact_in(start, input, 1, forward).unwrap_or_else(|e| {
                            panic!("amp={amp} lambda={lambda} O={reference} bps={bps} forward={forward}: {e}")
                        });
                        assert!(output > 0);
                        let certificate = curve.invariant_delta_bounds(start, end).unwrap();
                        assert!(
                            certificate.1 >= 0,
                            "independent enclosure rejected output: {certificate:?}"
                        );
                        let mut better = end;
                        let inventory = if forward {
                            start.quote_reserve
                        } else {
                            start.base_reserve
                        };
                        let increment = (output / 1_000_000).max(inventory / 100_000_000).max(100);
                        if forward {
                            better.quote_reserve -= increment;
                        } else {
                            better.base_reserve -= increment;
                        }
                        assert!(
                            curve.invariant_delta_bounds(start, better).unwrap().1 < 0,
                            "underfilled amp={amp} lambda={lambda} O={reference} bps={bps} dir={forward}: {output}"
                        );
                        let (returned, back) = curve.quote_exact_in(end, output, 1, !forward).unwrap();
                        assert!(returned<=input, "repeatable profit amp={amp} lambda={lambda} O={reference} bps={bps} dir={forward}: {returned}>{input} end={end:?} back={back:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn bounded_prices_cover_reference_examples_and_reciprocal_cap() {
    for (reference, expected) in [
        (120 * NAD, 109_544_511_501_u64),
        (900 * NAD, 200 * NAD),
        (NAD, 50 * NAD),
    ] {
        let c = BoundedOracleCurve::new(
            ConcentratedCurveParameters::cpmm(),
            100 * NAD,
            reference,
            5_000,
            2 * NAD,
        )
        .unwrap();
        let (lo, hi) = c.price_bounds_nad(point(10_000, 1_000_000)).unwrap();
        assert!(lo <= expected && hi >= expected, "{reference}: {lo}..{hi}");
        assert!(hi - lo < 100);
    }
}
#[test]
fn conditional_invariant_certifies_both_sides_of_exact_cpmm_root() {
    // At lambda=.5 and O=100, sqrt(100*x)+sqrt(y) is constant.
    let c = BoundedOracleCurve::new(
        ConcentratedCurveParameters::cpmm(),
        100 * NAD,
        100 * NAD,
        5_000,
        2 * NAD,
    )
    .unwrap();
    let start = point(1_000_000, 100_000_000);
    // sqrt(100*810000)=9000, sqrt(121000000)=11000: same sum 20000.
    let end = point(810_000, 121_000_000);
    let (lo, hi) = c.invariant_delta_bounds(start, end).unwrap();
    assert!(lo <= 0 && hi >= 0, "{lo} {hi}");
    assert!(c.invariant_delta_bounds(start, point(810_000, 121_000_100)).unwrap().0 > 0);
    assert!(c.invariant_delta_bounds(start, point(810_000, 120_999_900)).unwrap().1 < 0);
    let reverse = c.invariant_delta_bounds(end, start).unwrap();
    assert!(reverse.0 <= 0 && reverse.1 >= 0);
}
#[test]
fn concentrated_coordinate_and_reversibility_cross_all_bands() {
    let parameters = ConcentratedCurveParameters {
        peak_amplification_nad: 4 * NAD,
        core_half_width_bps: 1_000,
        fade_width_bps: 4_000,
    };
    let c = BoundedOracleCurve::new(parameters, 60 * NAD, 60 * NAD, 5_000, 2 * NAD).unwrap();
    let center = point(10_000_000, 600_000_000);
    let p = c.price_bounds_nad(center).unwrap();
    assert!(p.0 <= 60 * NAD && p.1 >= 60 * NAD, "{p:?}");
    for endpoint in [point(20_000_000, 300_000_000), point(5_000_000, 1_200_000_000)] {
        let forward = c.invariant_delta_bounds(center, endpoint).unwrap();
        let reverse = c.invariant_delta_bounds(endpoint, center).unwrap();
        assert!(forward.0 + reverse.0 <= 0 && forward.1 + reverse.1 >= 0);
    }
}
#[test]
fn bounds_enclose_independent_high_precision_integrals() {
    let references = [
        include_str!("../../../../../analysis/op-gamm-implementation-2026-10-30/invariant-reference.csv"),
        include_str!("../../../../../analysis/op-gamm-implementation-2026-10-30/local-invariant-reference.csv"),
    ];
    for row in references.iter().flat_map(|rows| rows.lines().skip(1)) {
        let mut v: Vec<i128> = row.split(',').map(|x| x.parse().unwrap()).collect();
        v[10] = v[10].div_euclid(256);
        let curve = BoundedOracleCurve::new(
            ConcentratedCurveParameters {
                peak_amplification_nad: v[0] as u64 * NAD,
                core_half_width_bps: v[1] as u16,
                fade_width_bps: v[2] as u16,
            },
            60 * NAD,
            v[4] as u64 * NAD,
            v[3] as u16,
            v[5] as u64,
        )
        .unwrap();
        let start = point(v[6] as u128, v[7] as u128);
        let end = point(v[8] as u128, v[9] as u128);
        let (lo, hi) = curve
            .invariant_delta_bounds(start, end)
            .unwrap_or_else(|e| panic!("{row}: {e}"));
        assert!(lo <= v[10] + 1 && hi >= v[10], "delta {row}: [{lo},{hi}]");
        let price = curve.price_bounds_nad(end).unwrap();
        assert!(
            price.0 as i128 <= v[11] + 1 && price.1 as i128 >= v[11],
            "price {row}: {price:?}"
        );
        let reverse = curve.invariant_delta_bounds(end, start).unwrap();
        assert!(lo + reverse.0 <= 0 && hi + reverse.1 >= 0, "reverse {row}");
    }
}

#[test]
fn majorants_cover_independent_interval_derivative_evaluations() {
    for amp in [1, 4] {
        let curve = BoundedOracleCurve::new(
            ConcentratedCurveParameters {
                peak_amplification_nad: amp * NAD,
                core_half_width_bps: if amp == 1 { 0 } else { 1000 },
                fade_width_bps: if amp == 1 { 0 } else { 4000 },
            },
            60 * NAD,
            90 * NAD,
            3700,
            2 * NAD,
        )
        .unwrap();
        for k in -30..=30 {
            let t = k * ONE / 100;
            if curve
                .breaks
                .iter()
                .any(|b| t >= b.lo - ONE / 1000 && t <= b.hi + ONE / 1000)
            {
                continue;
            }
            let cell = I {
                lo: t - ONE / 1000,
                hi: t + ONE / 1000,
            };
            for degree in [4, 8] {
                let bound = curve.derivative_bound(cell, t, degree).unwrap();
                for sample in [cell.lo, t, cell.hi] {
                    let derivative = curve.integrand(I::point(sample), t).unwrap().0[degree];
                    let mut scaled = I::point(derivative.abs_max().unwrap());
                    for _ in 0..degree {
                        scaled = scaled.mul(I::point(cell.hi - cell.lo)).unwrap();
                    }
                    assert!(
                        bound >= scaled.lo,
                        "amp={amp},t={k},degree={degree}: bound {bound}, {scaled:?}"
                    );
                }
            }
        }
    }
}

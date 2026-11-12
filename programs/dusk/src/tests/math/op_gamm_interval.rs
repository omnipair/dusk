use super::*;
#[test]
fn native_limb_division_matches_wide_division() {
    let mut seed = 0x8f17_c895_dad2_4c6a_3755_1f04_1122_0001_u128;
    for i in 0..100_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let d = ((seed >> 32) as u64 >> (i % 63)).max(1) as u128;
        let n = if i % 2 == 0 {
            seed
        } else {
            (seed % d) << 64 | (seed as u64 as u128)
        };
        assert_eq!(div_rem_native(n, d), (n / d, n % d), "n={n} d={d}");
    }
    for d in [1, 2, 3, 1 << 31, 1 << 32, (1 << 32) + 1, 1 << 63, u64::MAX as u128] {
        for n in [
            0,
            1,
            d - 1,
            d,
            d + 1,
            u64::MAX as u128,
            (d - 1) << 64 | u64::MAX as u128,
            u128::MAX,
        ] {
            assert_eq!(div_rem_native(n, d), (n / d, n % d));
        }
    }
}
#[test]
fn transcendental_intervals_enclose_decimal_reference_values() {
    // Q56 floors independently computed with 100-digit Decimal arithmetic.
    for (n, d, ln_floor) in [
        (1, 1, 0),
        (2, 1, LN2_LO),
        (1, 2, -LN2_LO - 1),
        (3, 1, 79_163_358_301_925_545),
        (10, 1, 165_918_741_868_749_488),
    ] {
        let v = Interval::ratio(n, d).unwrap().ln().unwrap();
        assert!(v.lo <= ln_floor && v.hi >= ln_floor, "{n}/{d}: {v:?} != {ln_floor}");
        let round = v.exp().unwrap();
        let exact = Interval::ratio(n, d).unwrap();
        assert!(round.lo <= exact.lo && round.hi >= exact.hi);
    }
}
#[test]
fn interval_arithmetic_keeps_negative_rounding_direction() {
    let a = Interval::point(-ONE - 1);
    let b = Interval::ratio(1, 3).unwrap();
    let product = a.mul(b).unwrap();
    assert!(product.lo <= (-ONE - 1) / 3 && product.hi >= (-ONE - 1) / 3);
    assert!(Interval::UNIT.div(Interval { lo: -1, hi: 1 }).is_err());
    assert!(Interval::ZERO.ln().is_err());
    assert!(Interval::point(i128::MAX).add(Interval::UNIT).is_err());
}
#[test]
fn transcendental_bounds_enclose_independent_high_precision_grid() {
    let rows =
        include_str!("../../../../../analysis/op-gamm-implementation-2026-10-30/transcendental-reference-q56.csv");
    for row in rows.lines().skip(1) {
        let v: Vec<i128> = row.split(',').map(|x| x.parse().unwrap()).collect();
        let log = Interval::point(v[0]).ln().unwrap();
        assert!(log.lo <= v[1] + 1 && log.hi >= v[1], "ln {row}: {log:?}");
        if v[2] >= 0 {
            let exp = Interval::point(v[0]).exp().unwrap();
            assert!(exp.lo <= v[2] + 1 && exp.hi >= v[2], "exp {row}: {exp:?}");
        }
    }
}
#[test]
fn signed_interval_fast_paths_match_all_endpoint_extrema() {
    // Each optimized sign case must agree with the independent four-corner
    // definition, including zero, sign crossings and fractional Q56 bits.
    let values = [-7 * ONE - 17, -ONE - 1, -1, 0, 1, ONE / 3, ONE + 7, 11 * ONE];
    for &a in &values {
        for &b in &values {
            if a > b {
                continue;
            }
            for &c in &values {
                for &d in &values {
                    if c > d {
                        continue;
                    }
                    let x = Interval { lo: a, hi: b };
                    let y = Interval { lo: c, hi: d };
                    let mut product = Interval {
                        lo: i128::MAX,
                        hi: i128::MIN,
                    };
                    let mut quotient = product;
                    let mut quotient_overflow = false;
                    for n in [a, b] {
                        for m in [c, d] {
                            product.lo = product.lo.min(rounded_ratio(n, m, ONE, false).unwrap());
                            product.hi = product.hi.max(rounded_ratio(n, m, ONE, true).unwrap());
                            if c > 0 || d < 0 {
                                match (rounded_ratio(n, ONE, m, false), rounded_ratio(n, ONE, m, true)) {
                                    (Ok(lo), Ok(hi)) => {
                                        quotient.lo = quotient.lo.min(lo);
                                        quotient.hi = quotient.hi.max(hi);
                                    }
                                    _ => quotient_overflow = true,
                                }
                            }
                        }
                    }
                    assert_eq!(x.mul(y).unwrap(), product, "mul {x:?} {y:?}");
                    if (c > 0 || d < 0) && !quotient_overflow {
                        assert_eq!(x.div(y).unwrap(), quotient, "div {x:?} {y:?}");
                    } else {
                        assert!(x.div(y).is_err());
                    }
                }
            }
        }
    }
}
#[test]
fn limb_multiplication_matches_independent_wide_integer_arithmetic() {
    let mut seed = 0x726f756e64696e675f63657274696679_u128;
    for k in 0..4096 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let a = seed >> (k % 65);
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let b = seed >> ((k / 65) % 65);
        let product = U256::from(a) * U256::from(b);
        for up in [false, true] {
            let divisor = b.max(1);
            let dividend = U256::from(a) << 56;
            let div = U256::from(divisor);
            let quotient = dividend / div;
            let expected = quotient + U256::from(u8::from(up && dividend % div != U256::zero()));
            let computed = unsigned_ratio(a, ONE as u128, divisor, up);
            if expected > U256::from(u128::MAX) {
                assert!(computed.is_err());
            } else {
                assert_eq!(computed.unwrap(), expected.as_u128());
            }

            let exact = (product >> 56) + U256::from(u8::from(up && product.low_u64() & ((1_u64 << 56) - 1) != 0));
            let actual = unsigned_ratio(a, b, ONE as u128, up);
            if exact > U256::from(u128::MAX) {
                assert!(actual.is_err());
            } else {
                assert_eq!(actual.unwrap(), exact.as_u128());
            }
        }
    }
}

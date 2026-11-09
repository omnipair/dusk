use super::*;

#[test]
fn upper_tangent_encloses_random_and_extreme_square_roots() {
    let mut seed = 0x58f32beaa1d2e8ff_u64;
    let mut random = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in 0..20_000 {
        let value = (((random() as u128) << 64) | random() as u128) >> (1 + i % 127);
        let width = (random() as u128) >> (i % 64);
        let hi = value.saturating_add(width).min(i128::MAX as u128);
        let result = Interval {
            lo: value as i128,
            hi: hi as i128,
        }
        .sqrt()
        .unwrap();
        let lo_root = U256::from(result.lo as u128);
        let hi_root = U256::from(result.hi as u128);
        let nlo = U256::from(value) << 56;
        let nhi = U256::from(hi) << 56;
        assert!(lo_root * lo_root <= nlo);
        assert!((lo_root + U256::one()) * (lo_root + U256::one()) > nlo);
        assert!(hi_root * hi_root >= nhi);
    }
    for value in [0, 1, ONE, i128::MAX] {
        let result = Interval::point(value).sqrt().unwrap();
        assert_eq!(result.lo, sqrt_q56(value, false).unwrap());
        assert_eq!(result.hi, sqrt_q56(value, true).unwrap());
    }
}

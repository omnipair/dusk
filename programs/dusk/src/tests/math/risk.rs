use super::*;

    #[test]
    fn pessimistic_max_debt_matches_v1_exact_values() {
        let reserve = 1_000_000_u128 * NAD as u128;

        let terms = pessimistic_max_debt_nad(reserve, reserve, reserve, 0).unwrap();
        assert_eq!(terms.liquidation_cf_bps, 8_500);
        assert_eq!(terms.max_cf_bps, 8_075);
        assert_eq!(terms.max_debt_nad, 403_750_u128 * NAD as u128);

        let terms = pessimistic_max_debt_nad(reserve / 2, reserve, reserve, 0).unwrap();
        assert_eq!(terms.max_cf_bps, 8_075);
        assert_eq!(terms.max_debt_nad, 269_166_666_666_666);

        let terms = pessimistic_max_debt_nad(
            reserve / 2,
            reserve,
            reserve,
            200_000_u128 * NAD as u128,
        )
        .unwrap();
        assert_eq!(terms.max_cf_bps, 6_514);
        assert_eq!(terms.max_debt_nad, 217_133_333_333_333);
    }

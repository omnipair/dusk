use super::*;
use proptest::prelude::*;

mod reference {
    use uint::construct_uint;
    construct_uint! { pub struct U256(4); }
}
use reference::U256;

fn reference_mul_div_rem(value: u128, numerator: u128, denominator: u128) -> Option<(u128, u128)> {
    let product = U256::from(value) * U256::from(numerator);
    let quotient = product / U256::from(denominator);
    let remainder = product % U256::from(denominator);
    (quotient <= U256::from(u128::MAX)).then(|| (quotient.as_u128(), remainder.as_u128()))
}

fn assert_matches_reference(value: u128, numerator: u128, denominator: u128) {
    assert_eq!(
        mul_div_rem_u128(value, numerator, denominator).ok(),
        reference_mul_div_rem(value, numerator, denominator),
        "value={value} numerator={numerator} denominator={denominator}"
    );
}

/// Operands drawn across every magnitude, so products land on the native
/// path, the single-division path and the bitwise loop.
fn wide() -> impl Strategy<Value = u128> {
    (0_u32..=127, any::<u128>()).prop_map(|(bits, raw)| raw >> bits)
}

#[test]
fn mul_div_matches_wide_reference_at_the_edges() {
    let edges = [1, 2, 3, 999, NAD as u128, u64::MAX as u128, 1_u128 << 64, u128::MAX / 3, u128::MAX - 1, u128::MAX];
    for value in edges {
        for numerator in edges {
            for denominator in edges {
                assert_matches_reference(value, numerator, denominator);
            }
        }
    }
}

proptest! {
    #[test]
    fn mul_div_matches_wide_reference(value in wide(), numerator in wide(), denominator in wide().prop_map(|d| d.max(1))) {
        assert_matches_reference(value, numerator, denominator);
    }

    // Large amounts scaled by NAD-sized prices or u64 share ratios.
    #[test]
    fn scaled_amounts_match_wide_reference(value in any::<u128>(), numerator in 1_u128..=u64::MAX as u128, denominator in 1_u128..=u64::MAX as u128) {
        assert_matches_reference(value, numerator, denominator);
    }
}

#[test]
fn widening_products_match_u128_at_the_edges() {
    let edges = [0, 1, 2, 0xffff_ffff, 1 << 32, (1 << 63) - 1, 1 << 63, u64::MAX - 1, u64::MAX];
    for lhs in edges {
        for rhs in edges {
            assert_eq!(widening_mul_u64(lhs, rhs), u128::from(lhs) * u128::from(rhs));
        }
    }
}

proptest! {
    #[test]
    fn widening_products_match_u128(lhs in any::<u64>(), rhs in any::<u64>()) {
        prop_assert_eq!(widening_mul_u64(lhs, rhs), u128::from(lhs) * u128::from(rhs));
    }

    // Narrow operands take the widening comparison; wider ones the u128
    // products or the continued-fraction loop.
    #[test]
    fn ratio_comparisons_match_wide_cross_products(
        left_numerator in wide(),
        left_denominator in wide().prop_map(|d| d.max(1)),
        right_numerator in wide(),
        right_denominator in wide().prop_map(|d| d.max(1)),
    ) {
        prop_assert_eq!(
            ratio_lte_full_width(left_numerator, left_denominator, right_numerator, right_denominator).unwrap(),
            U256::from(left_numerator) * U256::from(right_denominator)
                <= U256::from(right_numerator) * U256::from(left_denominator)
        );
    }
}

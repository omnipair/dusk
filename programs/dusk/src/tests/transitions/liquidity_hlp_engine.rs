use super::*;
use proptest::prelude::*;

#[test]
fn proportional_debt_checks_adjacent_shares_after_interest_accrual() {
    let index = 6_989_199_360;
    // All five old raw candidates (6,990..=6,994) round up to 1,001
    // shares and 6,996 debt atoms, two atoms above the proportional claim.
    // The preceding share count has debt 6,989 and claim 6,990.
    for quoted_debt in [None, Some(6_992)] {
        assert_eq!(
            canonical_debt_for_proportional_claim(6_992, 10_000, 20_000, index, quoted_debt).unwrap(),
            (1_000, 6_989),
        );
    }
}

#[test]
fn proportional_debt_matches_exhaustive_share_search() {
    for reserve in 0..=32_u64 {
        for hlp in 1..=8_u64 {
            for ordinary in 1..=8_u64 {
                let supply = hlp + ordinary;
                for index in [
                    NAD as u128,
                    1_500_000_000,
                    6_989_199_360,
                    7_000_000_000,
                    100_000_000_000,
                ] {
                    // Every larger share count is above the continuous root
                    // because index >= NAD. Enumerate independently of the
                    // production candidate selection and its rounding order.
                    let limit = reserve * hlp / ordinary + 2;
                    let exists = (0..=limit).any(|shares| {
                        let debt = shares as u128 * index / NAD as u128;
                        let claim = (reserve as u128 + debt) * hlp as u128 / supply as u128;
                        debt.abs_diff(claim) <= 1
                    });
                    let result = canonical_debt_for_proportional_claim(reserve, hlp, supply, index, None);
                    assert_eq!(
                        result.is_ok(),
                        exists,
                        "reserve={reserve}, h={hlp}, S={supply}, index={index}"
                    );
                    if let Ok((shares, debt)) = result {
                        assert_eq!(shares * index / NAD as u128, debt as u128);
                        assert!(debt.abs_diff((reserve + debt) * hlp / supply) <= 1);
                    }
                }
            }
        }
    }
}

#[test]
fn proportional_debt_preserves_a_valid_quote_and_rejects_an_unsatisfiable_gap() {
    // Both adjacent points satisfy the tolerance; preserve the quoted one.
    assert_eq!(
        canonical_debt_for_proportional_claim(10, 1, 2, 2 * NAD as u128, Some(11)).unwrap(),
        (6, 12)
    );
    // At a 10-atom quantum, debt 0 has error -2 and debt 10 has error +3.
    assert!(canonical_debt_for_proportional_claim(5, 1, 2, 10 * NAD as u128, None).is_err());
    assert_eq!(
        canonical_debt_for_proportional_claim(10, 0, 0, 0, None).unwrap(),
        (0, 0)
    );
    assert!(canonical_debt_for_proportional_claim(10, 1, 1, NAD as u128, None).is_err());
    assert!(canonical_debt_for_proportional_claim(10, 1, 2, 0, None).is_err());
}

#[test]
fn proportional_debt_skips_unrepresentable_candidates_at_reserve_capacity() {
    let reserve = u64::MAX - 1;
    // The continuous debt is just over one atom, but only zero debt fits
    // at this two-atom share quantum. Its claim error is exactly one.
    assert_eq!(
        canonical_debt_for_proportional_claim(reserve, 1, u64::MAX - 1, 2 * NAD as u128, Some(u64::MAX)).unwrap(),
        (0, 0),
    );
    // A larger continuous solution can exceed u64 while its capacity-clipped
    // predecessor still meets the tolerance (a nearly all-hLP supply).
    assert_eq!(
        canonical_debt_for_proportional_claim(2, u64::MAX - 1, u64::MAX, NAD as u128, None).unwrap(),
        ((u64::MAX - 2) as u128, u64::MAX - 2),
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn proportional_debt_matches_exact_feasible_interval(
        reserve in any::<u64>(),
        supply in 2..=u64::MAX,
        seed in any::<u64>(),
        index in prop_oneof![4 => NAD as u128..=10_000 * NAD as u128, 1 => NAD as u128..=u128::MAX],
        quoted in proptest::option::of(any::<u64>()),
    ) {
        let hlp = 1 + seed % (supply - 1);
        let ordinary = u128::from(supply - hlp);
        let product = u128::from(reserve) * u128::from(hlp);
        // |d - floor((R+d)h/S)| <= 1 iff -2S < d(S-h)-Rh <= S.
        // Solve that interval directly, independently of the fixed-point
        // neighborhood used by the implementation.
        let min_debt = (product + 1).saturating_sub(2 * u128::from(supply)).div_ceil(ordinary);
        let max_debt = ((product + u128::from(supply)) / ordinary).min(u128::from(u64::MAX - reserve));
        let feasible = if min_debt > max_debt {
            false
        } else {
            let first_shares = (min_debt * NAD as u128).div_ceil(index);
            first_shares * index / NAD as u128 <= max_debt
        };
        let result = canonical_debt_for_proportional_claim(reserve, hlp, supply, index, quoted);
        prop_assert_eq!(result.is_ok(), feasible);
        if let Ok((shares, debt)) = result {
            prop_assert_eq!(shares * index / NAD as u128, u128::from(debt));
            prop_assert!(u128::from(debt) >= min_debt && u128::from(debt) <= max_debt);
        }
    }
}

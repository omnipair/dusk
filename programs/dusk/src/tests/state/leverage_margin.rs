use super::*;

#[test]
fn incremental_equity_is_wallet_independent_and_retained_on_reduction() {
    let n = u128::from(NAD);
    let f = |amount| leverage_equity_potential(amount, 50_000).unwrap();
    assert_eq!(
        [f(15_000), f(30_000) - f(15_000), f(45_000) - f(30_000)],
        [2500 * n, 3000 * n, 3000 * n]
    );
    assert_eq!(f(45_000), 8500 * n);
    let mut later = LeverageMarginTerms::at_entry(50_000).unwrap();
    later.admission_equity_collateral_nad = f(30_000) - f(15_000);
    assert_eq!(later.initial_bps(15_000).unwrap(), 2000);
    assert_eq!(later.maintenance_bps(15_000).unwrap(), 1367);
    later.admission_equity_collateral_nad = 1000 * n;
    assert_eq!(later.admission_equity_collateral_nad, 1000 * n);
    assert_eq!(later.initial_bps(5_000).unwrap(), 2000);
    assert_eq!(later.maintenance_bps(5_000).unwrap(), 950);
}

#[test]
fn depth_changes_are_bound_to_both_states() {
    let n = u128::from(NAD);
    let mut sum = 0;
    for (before, after) in [(0, 15000), (15000, 30000), (30000, 45000)] {
        sum += leverage_equity_potential(after, 80000 - after).unwrap()
            - leverage_equity_potential(before, 80000 - before).unwrap();
    }
    assert_eq!(sum, leverage_equity_potential(45_000, 35_000).unwrap());
    assert!(sum >= 8650 * n);
}

#[test]
fn tiny_positions_do_not_get_full_margin_from_atom_rounding() {
    let terms = LeverageMarginTerms::at_entry(1_000_000).unwrap();
    assert_eq!(terms.maintenance_bps(1).unwrap(), 700);
    assert_eq!(terms.initial_bps(1).unwrap(), 1000);
    assert_eq!(terms.own_initial_equity_nad(1).unwrap(), u128::from(NAD) / 10);
    assert!(LeverageMarginTerms::at_entry(0).is_err());
    assert!(LeverageMarginTerms::default().initial_bps(1).is_err());
}

#[test]
fn crowding_tail_keeps_growing_and_cannot_be_capped_into_cheaper_admission() {
    let f = |amount| leverage_equity_potential(amount, 100_000).unwrap();
    assert!(f(200_000) - f(190_000) > f(100_000) - f(90_000));
    let mut terms = LeverageMarginTerms::at_entry(100_000).unwrap();
    terms.admission_equity_collateral_nad = f(930_000) - f(920_000);
    assert!(terms.initial_bps(10_000).is_err());
}

use super::*;

#[test]
fn utilization_is_ratio_of_borrowed_to_supplied() {
    assert_eq!(utilization_bps(600, 400).unwrap(), 6_000);
    assert_eq!(utilization_bps(1_000, 0).unwrap(), 10_000);
    assert_eq!(utilization_bps(0, 0).unwrap(), 0);
}

#[test]
fn full_repay_splits_exactly_principal_and_interest() {
    assert_eq!(realized_interest_split(110, 110, 100).unwrap(), (100, 10));
}

#[test]
fn partial_repay_splits_proportionally() {
    assert_eq!(realized_interest_split(55, 110, 100).unwrap(), (50, 5));
}

#[test]
fn no_accrued_interest_routes_nothing() {
    assert_eq!(realized_interest_split(100, 100, 100).unwrap(), (100, 0));
    assert_eq!(realized_interest_split(40, 100, 100).unwrap(), (40, 0));
}

#[test]
fn repay_above_debt_is_rejected() {
    assert_eq!(
        realized_interest_split(120, 110, 100).unwrap_err(),
        error!(ErrorCode::InsufficientDebt)
    );
}

#[test]
fn principal_rounds_down_so_interest_is_never_underfunded() {
    assert_eq!(realized_interest_split(1, 3, 2).unwrap(), (0, 1));
}

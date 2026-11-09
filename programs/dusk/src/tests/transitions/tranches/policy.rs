use super::*;

#[test]
fn operator_bounds_admit_defaults_and_reject_economic_extremes() {
    assert!(TranchePolicy::default().validate().is_ok());
    for share in [999, 9_001] {
        assert!(TranchePolicy {
            junior_income_share_bps: share,
            ..Default::default()
        }
        .validate()
        .is_err());
    }
}

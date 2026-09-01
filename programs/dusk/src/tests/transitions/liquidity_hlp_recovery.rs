use super::*;

#[test]
fn balanced_or_revenue_covered_hlp_has_no_recovery_quote() {
    let balanced = quote_hlp_recovery(1_000, 1_000, 0, 500, 1_000, 1, 1).unwrap();
    assert_eq!(balanced.discount_bps, 0);
    assert_eq!(balanced.bonus_output, 0);

    let revenue_covered = quote_hlp_recovery(1_100, 1_000, 100, 500, 1_000, 1, 1).unwrap();
    assert_eq!(revenue_covered.discount_bps, 0);
    assert_eq!(revenue_covered.bonus_output, 0);

    let dust = quote_hlp_recovery(1_002_499, 1_000_000, 0, 500, 1_000, 1, 1).unwrap();
    assert_eq!(dust.funding_gap, 2_499);
    assert_eq!(dust.discount_bps, 0);
}

#[test]
fn recovery_discount_reaches_max_at_seventeen_sixteenths() {
    let quote = quote_hlp_recovery(1_062_500, 1_000_000, 0, 1_000_000, 1_000_000, 1, 1).unwrap();
    assert_eq!(quote.funding_gap, 62_500);
    assert_eq!(quote.matched_input, 62_500);
    assert_eq!(quote.discount_bps, HLP_RECOVERY_MAX_DISCOUNT_BPS);
    assert_eq!(quote.bonus_output, 3_289);
    assert!(!quote.critical);
}

#[test]
fn recovery_is_input_and_equity_capped_and_marks_critical_stress() {
    let input_capped = quote_hlp_recovery(1_125_000, 1_000_000, 0, 10_000, 100, 2, 1).unwrap();
    assert_eq!(input_capped.matched_input, 10_000);
    assert_eq!(input_capped.bonus_output, 100);
    assert!(input_capped.critical);

    let no_claim = quote_hlp_recovery(1, 0, 0, 10_000, 100, 2, 1).unwrap();
    assert_eq!(no_claim.funding_gap, 1);
    assert_eq!(no_claim.bonus_output, 0);
    assert!(no_claim.critical);
}

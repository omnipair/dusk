use super::*;

#[test]
fn leverage_entry_escrow_keeps_bounty_separate_from_margin() {
    assert_eq!(escrow_margin_after_bounty(1_050, 50, 1_000).unwrap(), 1_000);
    assert!(escrow_margin_after_bounty(1_049, 50, 1_000).is_err());
    assert!(escrow_margin_after_bounty(50, 50, 0).is_err());
    assert!(escrow_margin_after_bounty(49, 50, 0).is_err());
}

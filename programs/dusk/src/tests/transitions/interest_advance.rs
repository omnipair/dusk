use super::*;

#[test]
fn share_floor_growth_does_not_release_or_cancel_an_interest_advance() {
    let mut advance = PublicInterestAdvance::default();
    advance.fund(100, 80, 16).unwrap();
    let before = advance;
    assert!(advance.clear(100, 101, 1).is_err());
    assert_eq!(advance, before);
    let cleared = advance.clear(100, 101, 0).unwrap();
    assert_eq!((cleared.repaid, cleared.canceled, cleared.revenue_returned), (0, 0, 0));
    assert_eq!(advance, before);
    assert!(advance.clear(101, 103, 0).is_err());
    assert_eq!(advance, before);
    let cleared = advance.clear(101, 0, 101).unwrap();
    assert_eq!((cleared.repaid, cleared.canceled), (80, 0));
    assert_eq!(advance.take_payment(101).unwrap(), (21, 16));
    assert_eq!(advance, PublicInterestAdvance::default());
    assert!(advance.clear(0, 1, 1).is_err());
    assert_eq!(advance, PublicInterestAdvance::default());
}

#[test]
fn frozen_split_survives_later_rate_changes_and_partial_repayment() {
    let mut a = PublicInterestAdvance::default();
    a.fund(100, 100, 20).unwrap();
    // A later 30% protocol rate changes only the next advance.
    a.fund(200, 100, 30).unwrap();
    let c = a.clear(200, 100, 100).unwrap();
    assert_eq!((c.repaid, c.canceled, c.revenue_returned), (100, 0, 0));
    assert_eq!(a.take_payment(100).unwrap(), (0, 25));
    let c = a.clear(100, 0, 100).unwrap();
    assert_eq!(c.repaid, 100);
    assert_eq!(a.take_payment(100).unwrap(), (0, 25));
    assert_eq!(a, PublicInterestAdvance::default());
}

#[test]
fn canceled_advance_returns_held_protocol_but_does_not_invent_lp_cash() {
    let mut a = PublicInterestAdvance::default();
    a.fund(100, 80, 16).unwrap();
    let c = a.clear(100, 0, 25).unwrap();
    assert_eq!((c.repaid, c.canceled, c.revenue_returned), (20, 60, 12));
    assert_eq!(a.take_payment(25).unwrap(), (5, 4));
    assert_eq!(a, PublicInterestAdvance::default());
}

#[test]
fn partial_clearances_conserve_escrow_and_advance_atoms() {
    for gross in 1..100_u64 {
        for protocol in 0..=gross {
            let mut a = PublicInterestAdvance::default();
            a.fund(101, gross, protocol).unwrap();
            let mut advanced = 0;
            let mut released = 0;
            for before in (1..=101).rev() {
                let paid = if before % 3 == 0 { 0 } else { 1 };
                let c = a.clear(before, before - 1, paid).unwrap();
                advanced += c.repaid + c.canceled;
                released += c.revenue_returned;
                let (fresh, frozen) = a.take_payment(paid).unwrap();
                assert!(fresh <= paid);
                released += frozen;
            }
            assert_eq!(advanced, gross);
            assert_eq!(released, protocol);
            assert_eq!(a, PublicInterestAdvance::default());
        }
    }
}

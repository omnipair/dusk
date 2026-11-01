use super::*;

fn setup() -> (TrancheCapital, JuniorExitConfig) {
    let mut capital = TrancheCapital::default();
    capital.enter(100_000, 500_000).unwrap();
    capital.pay_seed_fee(100).unwrap();
    (
        capital,
        JuniorExitConfig {
            minimum_notice_slots: 7_000,
            maximum_notice_slots: 10_000,
            exercise_window_slots: 2_000,
            request_fee_bps: 5,
        },
    )
}

#[test]
fn request_keeps_backing_and_limits_exercise_to_one_window() {
    let (mut capital, config) = setup();
    let mut ticket = JuniorExitTicket::default();
    let fee = ticket.request(&mut capital, &config, 20_000, 100).unwrap();
    assert_eq!(fee, 10);
    assert_eq!(capital.junior_reserve_shares, 100_000);
    assert_eq!(capital.queued_receipts, 19_990);
    assert_eq!(ticket.ready_at, 7_700);
    assert_eq!(ticket.expires_at, 9_700);
    assert!(ticket.convert(&mut capital, 7_699, 0, false).is_err());
    assert!(ticket.convert(&mut capital, 9_700, 0, false).is_err());
    assert_eq!(capital.queued_receipts, 19_990);
    assert_eq!(ticket.convert(&mut capital, 7_700, 0, false).unwrap(), 19_990);
    assert_eq!(capital.queued_receipts, 0);
}

#[test]
fn large_and_expired_requests_still_count_as_crowding() {
    let (mut capital, config) = setup();
    let mut whale = JuniorExitTicket::default();
    whale.request(&mut capital, &config, 80_000, 0).unwrap();
    let mut second = JuniorExitTicket::default();
    let now = whale.expires_at + 100;
    second.request(&mut capital, &config, 10_000, now).unwrap();
    assert_eq!(capital.queued_receipts, 89_955);
    assert_eq!(second.ready_at - now, 9_699);
}

#[test]
fn additions_cannot_shorten_notice_and_renewal_pays_on_all_units() {
    let (mut capital, config) = setup();
    let mut ticket = JuniorExitTicket::default();
    ticket.request(&mut capital, &config, 20_000, 0).unwrap();
    let previous_ready = ticket.ready_at;
    assert_eq!(ticket.request(&mut capital, &config, 10_000, 1).unwrap(), 5);
    assert!(ticket.ready_at >= previous_ready);
    let now = ticket.ready_at;
    assert_eq!(ticket.request(&mut capital, &config, 0, now).unwrap(), 14);
    assert!(ticket.ready_at >= now + config.minimum_notice_slots);
    assert!(ticket.convert(&mut capital, now, 0, false).is_err());
}

#[test]
fn cancellation_waits_for_notice_and_returns_junior() {
    let (mut capital, config) = setup();
    let mut ticket = JuniorExitTicket::default();
    ticket.request(&mut capital, &config, 20_000, 0).unwrap();
    assert!(ticket.cancel(&mut capital, 0).is_err());
    assert_eq!(ticket.cancel(&mut capital, ticket.ready_at).unwrap(), 19_990);
    assert_eq!(capital.junior_reserve_shares, 100_000);
    assert_eq!(capital.junior_receipt_supply, 100_000);
    assert_eq!(capital.permanent_receipts, 110);
    assert_eq!(capital.queued_receipts, 0);
}

#[test]
fn rejected_request_does_not_mutate_capital_or_ticket() {
    let (mut capital, config) = setup();
    let original = capital;
    let mut ticket = JuniorExitTicket::default();
    assert!(ticket.request(&mut capital, &config, 200_000, 0).is_err());
    assert_eq!(capital, original);
    assert_eq!(ticket, JuniorExitTicket::default());
    assert_eq!(ticket.request(&mut capital, &config, 1, 0).unwrap(), 0);
    assert_eq!(ticket.convert(&mut capital, ticket.ready_at, 0, false).unwrap(), 1);
    assert_eq!(capital.junior_reserve_shares, original.junior_reserve_shares - 1);
}

#[test]
fn notice_is_not_a_loss_free_option_and_expired_exits_stay_junior() {
    let (mut capital, config) = setup();
    let mut ticket = JuniorExitTicket::default();
    ticket.request(&mut capital, &config, 20_000, 0).unwrap();
    let mut total = 500_000;
    capital.absorb_loss(500_000, 50_000, &mut total).unwrap();
    assert!(ticket.convert(&mut capital, ticket.expires_at, 0, false).is_err());
    let now = ticket.expires_at;
    ticket.request(&mut capital, &config, 0, now).unwrap();
    assert_eq!(ticket.convert(&mut capital, ticket.ready_at, 0, false).unwrap(), 9_990);
}

#[test]
fn verified_restriction_preserves_only_unused_exercise_time() {
    let (mut capital, config) = setup();
    let mut ticket = JuniorExitTicket::default();
    ticket.request(&mut capital, &config, 20_000, 100).unwrap();
    let ready = ticket.ready_at;
    assert!(ticket.convert(&mut capital, ready - 1, 0, false).is_err());
    assert!(ticket.convert(&mut capital, ready + 100, 100, true).is_err());
    assert!(ticket.convert(&mut capital, ready + 100, 101, false).is_err());
    assert!(ticket.convert(&mut capital, ready + 5_000, 3_000, false).is_err());
    assert_eq!(
        ticket.convert(&mut capital, ready + 4_999, 3_000, false).unwrap(),
        19_990
    );
}

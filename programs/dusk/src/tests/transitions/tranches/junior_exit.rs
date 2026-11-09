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
        },
    )
}

#[test]
fn request_keeps_backing_and_limits_exercise_to_one_window() {
    let (mut capital, config) = setup();
    let mut ticket = JuniorExitTicket::default();
    let fee = ticket.request(&mut capital, &config, 20_000, 100).unwrap();
    assert_eq!(fee, 100);
    assert_eq!(capital.junior_reserve_shares, 100_000);
    assert_eq!(capital.queued_receipts, 19_900);
    assert_eq!(ticket.ready_at, 7_697);
    assert_eq!(ticket.expires_at, 9_697);
    assert!(ticket.convert(&mut capital, 7_696, 0, false).is_err());
    assert!(ticket.convert(&mut capital, 9_697, 0, false).is_err());
    assert_eq!(capital.queued_receipts, 19_900);
    assert_eq!(ticket.convert(&mut capital, 7_697, 0, false).unwrap(), 19_900);
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
    assert_eq!(capital.queued_receipts, 89_550);
    assert_eq!(second.ready_at - now, 9_687);
}

#[test]
fn existing_requests_must_be_cancelled_before_replacement() {
    let (mut capital, config) = setup();
    let mut ticket = JuniorExitTicket::default();
    ticket.request(&mut capital, &config, 20_000, 0).unwrap();
    let original_ticket = ticket;
    let original_capital = capital;
    for now in [1, ticket.ready_at, ticket.expires_at] {
        for amount in [0, 10_000] {
            assert!(ticket.request(&mut capital, &config, amount, now).is_err());
            assert_eq!(ticket, original_ticket);
            assert_eq!(capital, original_capital);
        }
    }
}

#[test]
fn cancellation_during_notice_returns_junior_without_refunding_fees() {
    let (mut capital, config) = setup();
    let mut ticket = JuniorExitTicket::default();
    ticket.request(&mut capital, &config, 20_000, 0).unwrap();
    assert_eq!(ticket.cancel(&mut capital).unwrap(), 19_900);
    assert_eq!(capital.junior_reserve_shares, 100_000);
    assert_eq!(capital.junior_receipt_supply, 100_000);
    assert_eq!(capital.permanent_receipts, 200);
    assert_eq!(capital.queued_receipts, 0);
    assert_eq!(ticket, JuniorExitTicket::default());
    assert!(ticket.cancel(&mut capital).is_err());
}

#[test]
fn larger_and_smaller_replacements_restart_the_full_notice() {
    for amount in [10_000, 30_000] {
        let (mut capital, config) = setup();
        let mut ticket = JuniorExitTicket::default();
        ticket.request(&mut capital, &config, 20_000, 0).unwrap();
        let previous_ready = ticket.ready_at;
        let now = previous_ready - 1;
        ticket.cancel(&mut capital).unwrap();
        let fee = ticket.request(&mut capital, &config, amount, now).unwrap();
        assert_eq!(fee, amount / 200);
        assert_eq!(ticket.receipts, amount - fee);
        assert_eq!(capital.queued_receipts, amount - fee);
        assert!(ticket.ready_at >= now + config.minimum_notice_slots);
        assert_eq!(ticket.expires_at, ticket.ready_at + config.exercise_window_slots);
        assert!(ticket.convert(&mut capital, previous_ready, 0, false).is_err());
        assert_eq!(capital.junior_reserve_shares, 100_000);
        assert_eq!(ticket.convert(&mut capital, ticket.ready_at, 0, false).unwrap(), amount - fee);
    }
}

#[test]
fn cancelling_preserves_losses_and_other_holders_queued_claims() {
    let (mut capital, config) = setup();
    let mut ticket = JuniorExitTicket::default();
    let mut other = JuniorExitTicket::default();
    ticket.request(&mut capital, &config, 20_000, 0).unwrap();
    other.request(&mut capital, &config, 10_000, 0).unwrap();
    let mut total = 500_000;
    capital.absorb_loss(500_000, 50_000, &mut total).unwrap();
    let before_cancel = capital;
    let returned = ticket.cancel(&mut capital).unwrap();
    assert_eq!(returned, 19_900);
    assert_eq!(capital.queued_receipts, other.receipts);
    assert_eq!(capital.junior_reserve_shares, before_cancel.junior_reserve_shares);
    assert_eq!(capital.junior_receipt_supply, before_cancel.junior_receipt_supply);
    ticket.request(&mut capital, &config, returned, 1).unwrap();
    assert_eq!(ticket.convert(&mut capital, ticket.ready_at, 0, false).unwrap(), 9_900);
}

#[test]
fn rejected_request_does_not_mutate_capital_or_ticket() {
    let (mut capital, config) = setup();
    let original = capital;
    let mut ticket = JuniorExitTicket::default();
    assert!(ticket.request(&mut capital, &config, 0, 0).is_err());
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
    let receipts = ticket.cancel(&mut capital).unwrap();
    ticket.request(&mut capital, &config, receipts, now).unwrap();
    assert_eq!(ticket.convert(&mut capital, ticket.ready_at, 0, false).unwrap(), 9_900);
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
        19_900
    );
}

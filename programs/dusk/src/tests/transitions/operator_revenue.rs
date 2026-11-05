use super::*;

fn pay(book: &mut InterestRevenueBook, remaining: u64, paid: u64, markup: u16, share: u16) -> (u64, u64) {
    book.synchronize(remaining, paid, markup, share).unwrap();
    let (fee, protocol) = book.quote_cash(paid, paid).unwrap();
    assert_eq!(book.collect(fee).unwrap(), protocol);
    (fee, protocol)
}

#[test]
fn base_interest_operator_share_and_added_protocol_markup_conserve_cash() {
    let mut book = InterestRevenueBook::default();
    book.synchronize(120, 0, 2_000, 3_000).unwrap();
    assert_eq!(book.unpaid_fees().unwrap(), 50);
    let (fee, protocol) = pay(&mut book, 0, 120, 2_000, 3_000);
    assert_eq!((120 - fee, book.operator_claimable, protocol), (70, 30, 20));
}

#[test]
fn recipient_handoff_preserves_collected_cash_and_transfers_future_collections() {
    let mut book = InterestRevenueBook::default();
    book.synchronize(120, 0, 2_000, 3_000).unwrap();
    assert_eq!(pay(&mut book, 60, 60, 2_000, 3_000), (25, 10));
    let alice = book.take_operator_cash().unwrap();
    assert_eq!(alice, 15);
    assert_eq!(book.operator_claimable, 0);
    assert_eq!(pay(&mut book, 0, 60, 2_000, 3_000), (25, 10));
    assert_eq!(book.operator_claimable, 15);
    assert_eq!(alice + book.operator_claimable, 30);
}

#[test]
fn rate_changes_do_not_reprice_already_issued_interest() {
    let mut book = InterestRevenueBook::default();
    book.synchronize(120, 0, 2_000, 3_000).unwrap();
    book.synchronize(260, 0, 4_000, 3_000).unwrap();
    let (fee, protocol) = pay(&mut book, 0, 260, 4_000, 3_000);
    assert_eq!((260 - fee, book.operator_claimable, protocol), (140, 60, 60));

    let mut book = InterestRevenueBook::default();
    book.synchronize(120, 0, 2_000, 3_000).unwrap();
    book.synchronize(240, 0, 2_000, 5_000).unwrap();
    let (fee, protocol) = pay(&mut book, 0, 240, 2_000, 5_000);
    assert_eq!((240 - fee, book.operator_claimable, protocol), (120, 80, 40));
}

#[test]
fn default_creates_no_cash_and_preserves_collected_operator_entitlement() {
    let mut book = InterestRevenueBook::default();
    book.synchronize(120, 0, 2_000, 3_000).unwrap();
    assert_eq!(pay(&mut book, 60, 60, 2_000, 3_000), (25, 10));
    book.synchronize(0, 0, 2_000, 3_000).unwrap();
    assert_eq!(book.pending_gross, 0);
    assert_eq!(book.collect(0).unwrap(), 0);
    assert_eq!(book.operator_claimable, 15);
    book.synchronize(120, 0, 2_000, 3_000).unwrap();
    assert_eq!(pay(&mut book, 0, 120, 2_000, 3_000), (50, 20));
    assert_eq!(book.operator_claimable, 45);
}

#[test]
fn transfer_fee_cash_is_measured_and_never_creates_unbacked_revenue() {
    let mut book = InterestRevenueBook::default();
    book.synchronize(1_200, 0, 2_000, 3_000).unwrap();
    book.synchronize(0, 1_200, 2_000, 3_000).unwrap();
    let (fee, protocol) = book.quote_cash(1_188, 1_200).unwrap();
    assert_eq!((fee, protocol), (495, 198));
    assert_eq!(book.collect(fee).unwrap(), 198);
    assert_eq!(book.operator_claimable, 297);
    assert_eq!(1_188 - fee, 693);
    assert_eq!(fee, protocol + book.operator_claimable);
}

proptest::proptest! {
    #[test]
    fn all_collected_fee_claims_are_cash_backed(
        steps in proptest::collection::vec((0_u8..3, 1_u16..10_000, 0_u16..10_001, 0_u16..5_001), 1..80)
    ) {
        let mut book=InterestRevenueBook::default();
        let mut collected=0_u64;
        let mut protocol_claimed=0_u64;
        let mut archived_cash=0_u64;
        for (kind,amount,markup,share) in steps {
            if kind==0 || book.gross_unpaid==0 {
                let gross=book.gross_unpaid+amount as u64;
                book.synchronize(gross,0,markup,share).unwrap();
            } else {
                let removed=(amount as u64).min(book.gross_unpaid);
                let paid=if kind==1 {removed} else {0};
                let remaining=book.gross_unpaid-removed;
                let (fee,protocol)=pay(&mut book,remaining,paid,markup,share);
                collected+=fee;protocol_claimed+=protocol;
            }
            if amount%7==0 {archived_cash+=book.take_operator_cash().unwrap();}
            proptest::prop_assert!(book.unpaid_fees().unwrap()<=book.gross_unpaid);
            proptest::prop_assert_eq!(protocol_claimed+archived_cash+book.operator_claimable,collected);
        }
    }
}

proptest::proptest! {
    #[test]
    fn mixed_advances_repayments_defaults_and_fee_changes_preserve_lp_budgets(
        steps in proptest::collection::vec((0_u8..4, 1_u16..10_000, 0_u16..10_001, 0_u16..5_001), 1..100)
    ) {
        let mut fees=InterestRevenueBook::default();
        let mut advance=crate::state::PublicInterestAdvance::default();
        for (kind,amount,markup,share) in steps {
            if kind==0 || fees.gross_unpaid==0 {
                fees.synchronize(fees.gross_unpaid+amount as u64,0,markup,share).unwrap();
            } else if kind==1 {
                let available=fees.gross_unpaid-advance.outstanding_gross;
                let gross=(amount as u64).min(available);
                let unpaid=fees.unpaid_fees().unwrap()-advance.revenue_escrow;
                let escrow=if available==0 {0} else {unpaid*gross/available};
                advance.fund(fees.gross_unpaid as u128,gross,escrow).unwrap();
            } else {
                let old=fees.gross_unpaid;
                let removed=(amount as u64).min(old);
                let paid=if kind==2 {removed} else {0};
                let remaining=old-removed;
                advance.clear(old as u128,remaining as u128,paid).unwrap();
                let fee_paid=fees.synchronize(remaining,paid,markup,share).unwrap();
                let transferred=paid-advance.repayment_credit_pending+advance.revenue_release_pending;
                proptest::prop_assert!(transferred>=fee_paid,"cash budget {} below fees {}",transferred,fee_paid);
                let (fee_cash,_)=fees.quote_cash(transferred,transferred).unwrap();
                fees.collect(fee_cash).unwrap();
                advance.take_payment(paid).unwrap();
            }
            proptest::prop_assert!(fees.unpaid_fees().unwrap()+advance.outstanding_gross-advance.revenue_escrow<=fees.gross_unpaid);
        }
    }
}

#[test]
fn rejected_fee_updates_leave_the_entire_book_unchanged() {
    let mut book = InterestRevenueBook::default();
    book.synchronize(120, 0, 2_000, 3_000).unwrap();
    let before = book;
    assert!(book.synchronize(60, 61, 2_000, 3_000).is_err());
    assert_eq!(book, before);
    assert!(book.synchronize(240, 0, 10_001, 3_000).is_err());
    assert_eq!(book, before);
    assert!(book.collect(1).is_err());
    assert_eq!(book, before);
}

#[test]
fn a_payment_consumed_by_external_fees_clears_its_budget_without_earnings() {
    let mut book = InterestRevenueBook::default();
    book.synchronize(120, 0, 2_000, 3_000).unwrap();
    book.synchronize(0, 120, 2_000, 3_000).unwrap();
    assert_eq!(book.quote_cash(0, 120).unwrap(), (0, 0));
    assert_eq!(book.collect(0).unwrap(), 0);
    assert_eq!(book.pending_gross, 0);
    assert_eq!(book.pending_protocol_gross, 0);
    assert_eq!(book.operator_claimable, 0);
    book.synchronize(120, 0, 2_000, 3_000).unwrap();
    assert_eq!(pay(&mut book, 0, 120, 2_000, 3_000), (50, 20));
    assert_eq!(book.operator_claimable, 30);
}

use super::*;

#[test]
fn old_income_survives_transfer_and_new_holder_cannot_take_it() {
    let mut book = IncomeReceivableBook::default();
    let mut alice = IncomeReceivableClaim::default();
    let mut bob = IncomeReceivableClaim::default();
    book.issue([(100, 100), (0, 0), (0, 0), (0, 0)]).unwrap();
    alice.checkpoint(100, &book, 0).unwrap();
    bob.checkpoint(0, &book, 0).unwrap();
    book.settle(100, 80).unwrap();
    alice.checkpoint(0, &book, 0).unwrap();
    bob.checkpoint(100, &book, 0).unwrap();
    assert_eq!(alice.claimable, 79); // remaining fraction is retained, never paid twice
    assert_eq!(bob.claimable, 0);
    assert!(alice.paid_fraction_q128 > 0);
}

#[test]
fn delayed_checkpoint_excludes_cash_collected_before_units_were_issued() {
    let mut book = IncomeReceivableBook::default();
    let mut original = IncomeReceivableClaim::default();
    book.issue([(100, 100), (0, 0), (0, 0), (0, 0)]).unwrap();
    book.settle(50, 50).unwrap();
    // The same holders earn more receivables at their current unit value.
    book.issue([(50, 100), (0, 0), (0, 0), (0, 0)]).unwrap();
    book.settle(100, 100).unwrap();
    original.checkpoint(100, &book, 0).unwrap();
    assert_eq!(original.claimable, 150);
}

#[test]
fn writeoff_does_not_create_cash_and_rollover_preserves_partial_claims() {
    let mut book = IncomeReceivableBook::default();
    let mut alice = IncomeReceivableClaim::default();
    book.issue([(80, 80), (20, 20), (0, 0), (0, 0)]).unwrap();
    book.settle(50, 0).unwrap();
    let archived = book.roll().unwrap();
    alice.advance_epoch(20, &archived, 1).unwrap();
    assert_eq!(alice.units_q64, 10 * Q64);
    assert_eq!(alice.claimable, 0);
    book.settle(50, 50).unwrap();
    alice.checkpoint(0, &book, 1).unwrap();
    assert_eq!(alice.claimable, 10);
}

#[test]
fn alternating_repayment_and_issuance_can_roll_without_reviving_drained_units() {
    let mut book = IncomeReceivableBook::default();
    let mut holder = IncomeReceivableClaim::default();
    for _ in 0..256 {
        book.issue([(100, 1), (0, 0), (0, 0), (0, 0)]).unwrap();
        book.settle(99, 99).unwrap();
        let old = book.roll().unwrap();
        holder.advance_epoch(1, &old, 0).unwrap();
    }
    let old_units = book.units;
    assert!(old_units > 0);
    book.settle(book.unpaid, book.unpaid).unwrap();
    let before = book.clone();
    assert!(book.issue([(1, 1), (0, 0), (0, 0), (0, 0)]).is_err());
    assert_eq!(book, before);
    let old = book.roll().unwrap();
    holder.advance_epoch(1, &old, 0).unwrap();
    assert_eq!(holder.units_q64, 0);
    assert!(holder.claimable <= 25_600);
    assert!(holder.claimable >= 25_599);
}

#[test]
fn repeated_checkpoints_and_four_lane_claims_never_overpay_custody() {
    let mut book = IncomeReceivableBook::default();
    let mut holders = [IncomeReceivableClaim::default(); 4];
    let balances = [7, 13, 19, 23];
    let mut cash = 0;
    for i in 1..=200 {
        book.issue([(7, 7), (13, 13), (19, 19), (23, 23)]).unwrap();
        let gross = book.unpaid / 3;
        let net = gross * 4 / 5;
        book.settle(gross, net).unwrap();
        cash += net;
        for lane in 0..4 {
            if i % (lane + 1) == 0 { holders[lane].checkpoint(balances[lane], &book, lane).unwrap(); }
        }
        if i % 7 == 0 {
            let old = book.roll().unwrap();
            for lane in 0..4 { holders[lane].advance_epoch(balances[lane], &old, lane).unwrap(); }
        }
    }
    for lane in 0..4 { holders[lane].checkpoint(balances[lane], &book, lane).unwrap(); }
    assert!(holders.iter().map(|h| h.claimable).sum::<u64>() <= cash);
}


#[test]
fn targeted_receivable_purchase_preserves_other_owners_and_past_cash() {
    let mut book = IncomeReceivableBook::default();
    book.issue([(800, 1), (100, 1), (100, 1), (0, 0)]).unwrap();
    let mut senior = IncomeReceivableClaim::default();
    let mut junior = IncomeReceivableClaim::default();
    let mut vault = IncomeReceivableClaim::default();
    senior.checkpoint(1, &book, 0).unwrap();
    junior.checkpoint(1, &book, 1).unwrap();
    vault.checkpoint(1, &book, 2).unwrap();
    book.settle(200, 180).unwrap();
    vault.checkpoint(1, &book, 2).unwrap();
    let already_paid = vault.claimable;
    let purchased = vault.redeem_unpaid_units(&mut book, 1, 2).unwrap();
    assert_eq!(purchased, 40);
    assert_eq!(vault.claimable, already_paid);
    assert_eq!(vault.unpaid_value(&book).unwrap(), 40);
    assert_eq!(senior.unpaid_value(&book).unwrap(), 640);
    assert_eq!(junior.unpaid_value(&book).unwrap(), 80);
    book.settle(book.unpaid, book.unpaid).unwrap();
    senior.checkpoint(1, &book, 0).unwrap();
    junior.checkpoint(1, &book, 1).unwrap();
    vault.checkpoint(1, &book, 2).unwrap();
    assert!(senior.claimable.abs_diff(784) <= 1);
    assert!(junior.claimable.abs_diff(98) <= 1);
    assert!(vault.claimable.abs_diff(58) <= 1);
    assert!(senior.claimable + junior.claimable + vault.claimable + purchased <= 980);
}

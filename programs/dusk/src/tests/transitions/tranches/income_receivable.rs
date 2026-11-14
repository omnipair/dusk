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
    assert!(book.units_q64 > 0);
    book.settle(book.unpaid, book.unpaid).unwrap();
    // A loss-free drained epoch advances in place instead of reviving its
    // units; the holder needs no archive to collect it.
    let epoch = book.epoch;
    assert!(!book.may_roll());
    book.issue([(1, 1), (0, 0), (0, 0), (0, 0)]).unwrap();
    assert_eq!(book.epoch, epoch + 1);
    assert_eq!((book.units_q64, book.unpaid), (Q64, 1));
    holder.checkpoint(0, &book, 0).unwrap();
    assert_eq!(holder.units_q64, 0);
    assert!(holder.claimable <= 25_600);
    assert!(holder.claimable >= 25_599);
}

#[test]
fn impaired_drained_epoch_still_needs_an_archived_roll() {
    let mut book = IncomeReceivableBook::default();
    let mut holder = IncomeReceivableClaim::default();
    book.issue([(100, 1), (0, 0), (0, 0), (0, 0)]).unwrap();
    holder.checkpoint(1, &book, 0).unwrap();
    // A custody shortfall, which the market records as an impairment.
    book.settle(100, 60).unwrap();
    book.impaired = true;
    let before = book.clone();
    assert!(!book.can_advance_in_place());
    assert!(book.issue([(1, 1), (0, 0), (0, 0), (0, 0)]).is_err());
    assert_eq!(book, before);
    assert!(book.may_roll());
    let old = book.roll().unwrap();
    holder.advance_epoch(1, &old, 0).unwrap();
    assert!(holder.claimable.abs_diff(60) <= 1);
    book.issue([(10, 1), (0, 0), (0, 0), (0, 0)]).unwrap();
    holder.checkpoint(1, &book, 0).unwrap();
}

/// Mirrors an hLP funding book: every accrual is followed by a payment of
/// everything due. Holders that never checkpoint, checkpoint often, change
/// balance, or stop exposure all collect what the epochs paid them, and the
/// total never exceeds the cash credited.
#[test]
fn drained_epochs_advance_in_place_and_holders_catch_up_exactly() {
    let mut book = IncomeReceivableBook::default();
    let mut often = IncomeReceivableClaim::default();
    let mut idle = IncomeReceivableClaim::default();
    let mut junior = IncomeReceivableClaim::default();
    let mut ended = IncomeReceivableClaim::default();
    let (mut often_balance, idle_balance, junior_balance, ended_balance) = (60_u64, 40_u64, 10_u64, 5_u64);
    let mut end = IncomeExposureEnd::default();
    let (mut senior_cash, mut junior_cash) = (0_u128, 0_u128);
    let (mut idle_expected, mut junior_expected, mut ended_expected) = (0_u128, 0_u128, 0_u128);
    for cycle in 0..120_u64 {
        let senior = 1_000 + cycle * 37 % 501;
        let junior_gross = 100 + cycle * 13 % 97;
        let senior_supply = often_balance + idle_balance;
        let junior_supply = junior_balance + if end.ended { 0 } else { ended_balance };
        book.issue([(senior, senior_supply), (junior_gross, junior_supply), (0, 0), (0, 0)]).unwrap();
        if cycle % 3 == 0 {
            // A partial repayment first, then the rest: still loss-free.
            let part = book.unpaid / 3;
            book.settle(part, part).unwrap();
            book.issue([(senior / 2, senior_supply), (0, junior_supply), (0, 0), (0, 0)]).unwrap();
            senior_cash += u128::from(senior / 2);
            idle_expected += u128::from(senior / 2) * u128::from(idle_balance) / u128::from(senior_supply);
        }
        let unpaid = book.unpaid;
        book.settle(unpaid, unpaid).unwrap();
        senior_cash += u128::from(senior);
        junior_cash += u128::from(junior_gross);
        idle_expected += u128::from(senior) * u128::from(idle_balance) / u128::from(senior_supply);
        junior_expected += u128::from(junior_gross) * u128::from(junior_balance) / u128::from(junior_supply);
        if !end.ended {
            ended_expected += u128::from(junior_gross) * u128::from(ended_balance) / u128::from(junior_supply);
        }
        if cycle % 5 == 0 {
            often.checkpoint(often_balance, &book, 0).unwrap();
        }
        if cycle == 60 {
            // Balance change: checkpoint at the old balance first.
            often.checkpoint(often_balance, &book, 0).unwrap();
            often_balance = 30;
        }
        if cycle == 80 {
            end = IncomeExposureEnd { ended: true, epoch: book.epoch, growth: book.growth[1] };
        }
    }
    assert!(book.epoch >= 119);
    assert_eq!(book.run_start_epoch(), 0);
    // The last epoch is drained but not yet advanced.
    book.issue([(1, often_balance + idle_balance), (0, 1), (0, 0), (0, 0)]).unwrap();
    often.checkpoint(often_balance, &book, 0).unwrap();
    idle.checkpoint(idle_balance, &book, 0).unwrap();
    junior.checkpoint(junior_balance, &book, 1).unwrap();
    ended.checkpoint_exposure(ended_balance, &book, 1, end).unwrap();
    for claim in [&often, &idle, &junior, &ended] {
        assert_eq!(claim.epoch, book.epoch);
    }
    // Only the final one-atom issuance remains as units.
    assert!(often.units_q64 + idle.units_q64 <= Q64);
    let senior_paid = u128::from(often.claimable) + u128::from(idle.claimable);
    let junior_paid = u128::from(junior.claimable) + u128::from(ended.claimable);
    // Never more than the cash credited. What stays behind is rounding dust
    // of at most an atom per issuance: issuance rounds units down, and an
    // epoch's paid index above the drained unit value is not distributed.
    assert!(senior_paid <= senior_cash && senior_paid + 160 >= senior_cash, "{senior_paid} vs {senior_cash}");
    assert!(junior_paid <= junior_cash && junior_paid + 160 >= junior_cash, "{junior_paid} vs {junior_cash}");
    // Per-holder shares match pro-rata exposure up to per-cycle flooring.
    assert!(u128::from(idle.claimable).abs_diff(idle_expected) <= 200, "{} vs {idle_expected}", idle.claimable);
    assert!(u128::from(junior.claimable).abs_diff(junior_expected) <= 200);
    assert!(u128::from(ended.claimable).abs_diff(ended_expected) <= 200);
}

#[test]
fn drained_run_then_archived_roll_keeps_lagging_holders_whole() {
    let mut book = IncomeReceivableBook::default();
    let mut lagging = IncomeReceivableClaim::default();
    let mut current = IncomeReceivableClaim::default();
    lagging.checkpoint(1, &book, 0).unwrap();
    current.checkpoint(1, &book, 0).unwrap();
    let mut cash = 0_u64;
    for _ in 0..10 {
        book.issue([(1_000, 2), (0, 0), (0, 0), (0, 0)]).unwrap();
        book.settle(1_000, 1_000).unwrap();
        cash += 1_000;
    }
    // An impaired epoch ends the run and is archived.
    book.issue([(1_000, 2), (0, 0), (0, 0), (0, 0)]).unwrap();
    book.settle(400, 400).unwrap();
    book.settle(100, 0).unwrap();
    book.impaired = true;
    cash += 400;
    assert!(book.may_roll());
    let archived = book.roll().unwrap();
    assert_eq!(book.run_start_epoch(), book.epoch);
    current.advance_epoch(1, &archived, 0).unwrap();
    // The lagging holder cannot skip the archive.
    assert!(lagging.clone().checkpoint(1, &book, 0).is_err());
    lagging.advance_epoch(1, &archived, 0).unwrap();
    assert_eq!(lagging, current);
    let carried = book.unpaid;
    book.settle(carried, carried).unwrap();
    cash += carried;
    lagging.checkpoint(1, &book, 0).unwrap();
    current.checkpoint(1, &book, 0).unwrap();
    let paid = lagging.claimable + current.claimable;
    assert!(paid <= cash && paid + 2 >= cash, "{paid} vs {cash}");
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

#[test]
fn rolls_need_half_the_units_repaid_or_a_book_near_its_limits() {
    let book = |units: u64, unpaid: u64| IncomeReceivableBook {
        units_q64: (units as u128) << 64,
        unpaid,
        ..IncomeReceivableBook::default()
    };
    // Nothing settled, or one atom repaid: no roll.
    assert!(!book(0, 0).may_roll());
    assert!(!book(1_000, 1_000).may_roll());
    assert!(!book(1_000, 999).may_roll());
    assert!(!book(1_000, 501).may_roll());
    // Half repaid, or drained below the in-place drained unit value.
    assert!(book(1_000, 500).may_roll());
    assert!(book(1_000, 0).may_roll());
    // A loss-free drained epoch advances in place; rolling it would only add
    // an archive every holder must catch up through.
    let mut drained = book(1_000, 0);
    drained.paid_per_unit_q64 = Q64;
    assert!(!drained.may_roll());
    let mut exhausted_run = drained.clone();
    exhausted_run.growth[0].units_q64 = u128::MAX / 2;
    assert!(exhausted_run.may_roll());
    drained.impaired = true;
    assert!(drained.may_roll());
    // Near a fixed-width limit, a small repayment is enough to recover.
    assert!(book(u64::MAX / 2 + 2, u64::MAX / 2 + 1).may_roll());
    let mut index = book(1_000, 999);
    index.paid_per_unit_q64 = u128::MAX / 2;
    assert!(index.may_roll());
    let mut lane = book(1_000, 999);
    lane.growth[1].units_q64 = u128::MAX / 2;
    assert!(lane.may_roll());
}

/// Partial repayments between small accruals no longer drift a unit's value:
/// fractional issuance keeps it at face value, so an in-place advance pays a
/// holder what an archived roll pays, less at most rounding.
#[test]
fn in_place_advance_pays_what_an_archived_roll_pays_after_partial_repayments() {
    for (accrual, repay) in [(3_u64, 1_u64), (7, 3), (13, 5), (1_000, 333)] {
        let run = |advance: bool| -> (u128, u128) {
            let mut book = IncomeReceivableBook::default();
            let mut holder = IncomeReceivableClaim::default();
            holder.checkpoint(1, &book, 0).unwrap();
            let mut cash = 0_u128;
            for _ in 0..2_000 {
                book.issue([(accrual, 1), (0, 0), (0, 0), (0, 0)]).unwrap();
                let part = repay.min(book.unpaid);
                book.settle(part, part).unwrap();
                cash += u128::from(part);
            }
            let rest = book.unpaid;
            book.settle(rest, rest).unwrap();
            cash += u128::from(rest);
            if advance {
                assert!(book.can_advance_in_place());
                book.issue([(1, 1), (0, 0), (0, 0), (0, 0)]).unwrap();
                holder.checkpoint(1, &book, 0).unwrap();
            } else {
                let old = book.roll().unwrap();
                holder.advance_epoch(1, &old, 0).unwrap();
            }
            (u128::from(holder.claimable), cash)
        };
        let (in_place, cash) = run(true);
        let (rolled, _) = run(false);
        assert!(in_place <= cash && rolled <= cash);
        assert!(in_place + 2 >= rolled, "accrual={accrual} repay={repay}: in place {in_place} vs rolled {rolled} of {cash}");
    }
}

/// One epoch of small accruals and 1-atom repayments (a cheap griefing
/// pattern) does not cancel a lagging holder's income from the run.
#[test]
fn partially_repaid_epoch_does_not_cancel_a_runs_income() {
    for (small, cycles) in [(1_268_u64, 1_000_u32), (100, 1_000), (13, 2_000)] {
        let mut book = IncomeReceivableBook::default();
        let mut lagging = IncomeReceivableClaim::default();
        let mut eager = IncomeReceivableClaim::default();
        lagging.checkpoint(1, &book, 0).unwrap();
        eager.checkpoint(1, &book, 0).unwrap();
        let mut cash = 0_u128;
        for _ in 0..5 {
            book.issue([(1_000_000, 2), (0, 0), (0, 0), (0, 0)]).unwrap();
            let unpaid = book.unpaid;
            book.settle(unpaid, unpaid).unwrap();
            cash += u128::from(unpaid);
            eager.checkpoint(1, &book, 0).unwrap();
        }
        for _ in 0..cycles {
            book.issue([(small, 2), (0, 0), (0, 0), (0, 0)]).unwrap();
            book.settle(1, 1).unwrap();
            cash += 1;
        }
        book.issue([(1_000_000, 2), (0, 0), (0, 0), (0, 0)]).unwrap();
        let unpaid = book.unpaid;
        book.settle(unpaid, unpaid).unwrap();
        cash += u128::from(unpaid);
        eager.checkpoint(1, &book, 0).unwrap();
        book.issue([(1, 2), (0, 0), (0, 0), (0, 0)]).unwrap();
        lagging.checkpoint(1, &book, 0).unwrap();
        eager.checkpoint(1, &book, 0).unwrap();
        let fair = cash / 2;
        assert!(u128::from(lagging.claimable) + u128::from(eager.claimable) <= cash);
        assert!(u128::from(lagging.claimable) + 8 >= fair, "small={small}: lagging {} of fair {fair}", lagging.claimable);
    }
}


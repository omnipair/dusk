use super::*;

#[test]
fn zero_shares_skip_debt_arithmetic() {
    Debt::reset_shares_to_debt_call_count();

    assert_eq!(Debt::shares_to_debt(0, u128::MAX).unwrap(), 0);
    assert_eq!(Debt::shares_to_debt_call_count(), 0);
}

#[test]
fn raw_principal_accumulators_accept_u64_max_and_reject_the_next_atom() {
    let mut margin = Debt::default();
    margin.add_margin_principal(MarketAsset::Base, u64::MAX).unwrap();
    assert_eq!(margin.fixed_base_principal, u64::MAX);
    assert!(margin.add_margin_principal(MarketAsset::Base, 1).is_err());
    assert_eq!(margin.fixed_base_principal, u64::MAX);

    let mut isolated = Debt {
        base_borrow_index_nad: NAD as u128,
        ..Debt::default()
    };
    isolated.add_isolated_debt(MarketAsset::Base, u64::MAX).unwrap();
    let shares_at_limit = isolated.isolated_base_shares;
    assert_eq!(isolated.isolated_base_principal, u64::MAX);
    assert!(isolated.add_isolated_debt(MarketAsset::Base, 1).is_err());
    assert_eq!(isolated.isolated_base_principal, u64::MAX);
    assert_eq!(isolated.isolated_base_shares, shares_at_limit);
}

#[test]
fn isolated_position_principal_above_raw_token_domain_fails_before_mutation() {
    let mut debt = Debt {
        base_borrow_index_nad: NAD as u128,
        isolated_base_shares: 1,
        isolated_base_principal: 1,
        ..Debt::default()
    };
    let mut position_shares = 1;
    let mut position_principal = u64::MAX as u128 + 1;

    assert!(debt
        .clear_isolated_debt(MarketAsset::Base, &mut position_shares, &mut position_principal, 1,)
        .is_err());
    assert_eq!(debt.isolated_base_shares, 1);
    assert_eq!(debt.isolated_base_principal, 1);
    assert_eq!(position_shares, 1);
    assert_eq!(position_principal, u64::MAX as u128 + 1);
}

#[test]
fn add_margin_principal_accumulates_per_side() {
    let mut debt = Debt::default();
    debt.add_margin_principal(MarketAsset::Base, 1_000).unwrap();
    debt.add_margin_principal(MarketAsset::Base, 500).unwrap();
    debt.add_margin_principal(MarketAsset::Quote, 200).unwrap();
    assert_eq!(debt.fixed_base_principal, 1_500);
    assert_eq!(debt.fixed_quote_principal, 200);
}

#[test]
fn realize_margin_repay_is_all_principal_without_interest() {
    let mut debt = Debt {
        fixed_base_shares: 1_000,
        base_borrow_index_nad: NAD as u128,
        fixed_base_principal: 1_000,
        ..Debt::default()
    };
    let interest = debt.realize_margin_repay(MarketAsset::Base, 400).unwrap();
    assert_eq!(interest, 0);
    assert_eq!(debt.fixed_base_principal, 600);
}

#[test]
fn realize_margin_repay_splits_accrued_interest() {
    // Index 1.1: 1_000 of principal now owes 1_100 of debt.
    let mut debt = Debt {
        fixed_base_shares: 1_000,
        base_borrow_index_nad: (NAD as u128) * 11 / 10,
        fixed_base_principal: 1_000,
        ..Debt::default()
    };
    // Repay 550 of 1_100: 500 principal + 50 interest.
    let interest = debt.realize_margin_repay(MarketAsset::Base, 550).unwrap();
    assert_eq!(interest, 50);
    assert_eq!(debt.fixed_base_principal, 500);
}

#[test]
fn realize_margin_repay_full_clears_principal_and_returns_all_interest() {
    let mut debt = Debt {
        fixed_quote_shares: 1_000,
        quote_borrow_index_nad: (NAD as u128) * 11 / 10,
        fixed_quote_principal: 1_000,
        ..Debt::default()
    };
    let interest = debt.realize_margin_repay(MarketAsset::Quote, 1_100).unwrap();
    assert_eq!(interest, 100);
    assert_eq!(debt.fixed_quote_principal, 0);
}

#[test]
fn liquidation_writeoff_reduces_principal_without_realizing_interest_as_cash() {
    let mut debt = Debt {
        fixed_base_shares: 1_000,
        base_borrow_index_nad: (NAD as u128) * 11 / 10,
        fixed_base_principal: 1_000,
        ..Debt::default()
    };

    let interest = debt.realize_margin_liquidation(MarketAsset::Base, 550, 1_100).unwrap();

    assert_eq!(interest, 50);
    assert_eq!(debt.fixed_base_principal, 0);
}

#[test]
fn isolated_debt_uses_separate_shares_and_principal() {
    let mut debt = Debt {
        base_borrow_index_nad: NAD as u128,
        quote_borrow_index_nad: NAD as u128,
        ..Debt::default()
    };

    let shares = debt.add_isolated_debt(MarketAsset::Base, 1_000).unwrap();

    assert_eq!(shares, 1_000);
    assert_eq!(debt.isolated_base_shares, 1_000);
    assert_eq!(debt.isolated_base_principal, 1_000);
    assert_eq!(debt.fixed_base_shares, 0);
    assert_eq!(debt.isolated_debt(MarketAsset::Base).unwrap(), 1_000);
}

#[test]
fn isolated_repay_splits_interest_without_touching_margin_principal() {
    let mut debt = Debt {
        base_borrow_index_nad: (NAD as u128) * 11 / 10,
        isolated_base_shares: 1_000,
        isolated_base_principal: 1_000,
        fixed_base_principal: 777,
        ..Debt::default()
    };
    let mut position_shares = 1_000;
    let mut position_principal = 1_000;

    let clearance = debt
        .clear_isolated_debt(MarketAsset::Base, &mut position_shares, &mut position_principal, 550)
        .unwrap();

    assert_eq!(clearance.cash_repaid, 550);
    assert_eq!(clearance.principal_paid, 500);
    assert_eq!(clearance.interest_paid, 50);
    assert_eq!(debt.isolated_base_principal, 500);
    assert_eq!(position_principal, 500);
    assert_eq!(debt.fixed_base_principal, 777);
}

#[test]
fn isolated_repay_never_reduces_more_aggregate_debt_than_the_maximum() {
    let mut debt = Debt {
        base_borrow_index_nad: (NAD as u128) * 3 / 2,
        isolated_base_shares: 100,
        isolated_base_principal: 100,
        ..Debt::default()
    };
    let mut position_shares = 100;
    let mut position_principal = 100;

    let clearance = debt
        .clear_isolated_debt(MarketAsset::Base, &mut position_shares, &mut position_principal, 2)
        .unwrap();

    assert_eq!(clearance.shares_burned, 1);
    assert_eq!(clearance.cash_repaid, 2);
    assert_eq!(clearance.debt_reduced, 2);
    assert_eq!(clearance.aggregate_debt_reduced, 2);
    assert_eq!(clearance.remaining_debt, 148);
    assert_eq!(clearance.principal_paid, 1);
    assert_eq!(clearance.interest_paid, 1);
    assert_eq!(clearance.live_debit_for_cash_repay().unwrap(), 1);
    assert_eq!(position_shares, 99);
    assert_eq!(position_principal, 99);
    assert_eq!(debt.isolated_base_shares, 99);
    assert_eq!(debt.isolated_base_principal, 99);
}

#[test]
fn isolated_principal_remains_the_sum_across_sequential_close_and_liquidation() {
    let index = (NAD as u128) * 103 / 100;
    let mut debt = Debt {
        base_borrow_index_nad: index,
        ..Debt::default()
    };
    let mut first_shares = debt.add_isolated_debt(MarketAsset::Base, 34).unwrap();
    let mut second_shares = debt.add_isolated_debt(MarketAsset::Base, 67).unwrap();
    let mut first_principal = 34u128;
    let mut second_principal = 67u128;

    let first_close = debt
        .clear_isolated_debt(MarketAsset::Base, &mut first_shares, &mut first_principal, u64::MAX)
        .unwrap();

    // The aggregate floor phase charges 36 even though the first position's
    // displayed debt is 35. Cash classification and principal ownership are
    // deliberately distinct.
    assert_eq!(first_close.cash_repaid, 36);
    assert_eq!(first_close.debt_reduced, 35);
    assert_eq!(first_shares, 0);
    assert_eq!(first_principal, 0);
    assert_eq!(debt.isolated_base_shares, second_shares);
    assert_eq!(u128::from(debt.isolated_base_principal), second_principal);

    // A later liquidation/writeoff of the remaining position must not fail
    // because an earlier close rounded aggregate principal down by an extra
    // atom.
    let writeoff = debt
        .writeoff_isolated_position(MarketAsset::Base, &mut second_shares, &mut second_principal)
        .unwrap();
    assert_eq!(writeoff.principal_written_off, 67);
    assert_eq!(second_shares, 0);
    assert_eq!(second_principal, 0);
    assert_eq!(debt.isolated_base_shares, 0);
    assert_eq!(debt.isolated_base_principal, 0);
}

#[test]
fn one_atom_cannot_erase_a_two_atom_aggregate_debt_delta() {
    let mut debt = Debt {
        base_borrow_index_nad: (NAD as u128) * 3 / 2,
        isolated_base_shares: 2,
        isolated_base_principal: 2,
        ..Debt::default()
    };
    let mut position_shares = 1;
    let mut position_principal = 1;

    let error = debt
        .clear_isolated_debt(MarketAsset::Base, &mut position_shares, &mut position_principal, 1)
        .unwrap_err();

    assert_eq!(error, error!(ErrorCode::DebtShareDivisionOverflow));
    assert_eq!(position_shares, 1);
    assert_eq!(position_principal, 1);
    assert_eq!(debt.isolated_base_shares, 2);
    assert_eq!(debt.isolated_base_principal, 2);
}

#[test]
fn full_close_charges_the_canonical_aggregate_delta() {
    let repayment = Debt::repayment_for_max(1, 2, (NAD as u128) * 3 / 2, 2).unwrap();

    assert_eq!(repayment.shares_to_burn, 1);
    assert_eq!(repayment.cash_repaid, 2);
    assert_eq!(repayment.position_debt_reduced, 1);
    assert_eq!(repayment.remaining_position_debt, 0);
}

#[test]
fn split_repayments_telescope_to_the_original_aggregate_debt() {
    let index = (NAD as u128) * 3 / 2;
    let mut aggregate_shares = 100u128;
    let debt_before = Debt::shares_to_debt(aggregate_shares, index).unwrap() as u64;
    let mut total_cash_repaid = 0u64;

    while aggregate_shares > 0 {
        let repayment = Debt::repayment_for_max(aggregate_shares, aggregate_shares, index, 2).unwrap();
        assert!(repayment.cash_repaid <= 2);
        aggregate_shares -= repayment.shares_to_burn;
        total_cash_repaid += repayment.cash_repaid;
    }

    assert_eq!(total_cash_repaid, debt_before);
}

#[test]
fn repayment_quote_is_max_safe_and_adjacent_share_is_unsafe() {
    for index_bps in [10_000u128, 10_001, 15_000, 20_000, 75_000] {
        let index = (NAD as u128) * index_bps / 10_000;
        for aggregate_shares in 1u128..64 {
            for position_shares in 1u128..=aggregate_shares {
                for max_repay in 1u64..128 {
                    let Ok(repayment) = Debt::repayment_for_max(position_shares, aggregate_shares, index, max_repay)
                    else {
                        continue;
                    };
                    assert!(repayment.cash_repaid <= max_repay);
                    if repayment.shares_to_burn < position_shares {
                        let adjacent_delta = Debt::aggregate_debt_reduction_for_shares(
                            aggregate_shares,
                            repayment.shares_to_burn + 1,
                            index,
                        )
                        .unwrap();
                        assert!(adjacent_delta > max_repay);
                    }
                }
            }
        }
    }
}

#[test]
fn isolated_writeoff_reports_aggregate_debt_delta_across_positions() {
    let mut debt = Debt {
        base_borrow_index_nad: (NAD as u128) * 3 / 2,
        isolated_base_shares: 2,
        isolated_base_principal: 2,
        ..Debt::default()
    };
    let mut position_shares = 1;
    let mut position_principal = 1;

    let writeoff = debt
        .writeoff_isolated_position(MarketAsset::Base, &mut position_shares, &mut position_principal)
        .unwrap();

    assert_eq!(writeoff.debt_written_off, 1);
    assert_eq!(writeoff.aggregate_debt_written_off, 2);
    assert_eq!(debt.isolated_base_shares, 1);
}

#[test]
fn separate_interest_buckets_preserve_flooring_at_the_one_atom_boundary() {
    let old_index = (NAD as u128) * 11 / 10;
    let next_index = (NAD as u128) * 3 / 2;
    let cash_backed_before = Debt::shares_to_debt(1, old_index).unwrap() + Debt::shares_to_debt(1, old_index).unwrap();
    let separately_rounded_after =
        Debt::shares_to_debt(1, next_index).unwrap() + Debt::shares_to_debt(1, next_index).unwrap();
    let aggregate_after = Debt::shares_to_debt(2, next_index).unwrap();

    // Combining the fixed and isolated buckets would create one atom of
    // debt which does not exist under the protocol's established
    // per-bucket flooring.
    assert_eq!(aggregate_after, separately_rounded_after + 1);

    assert_eq!(cash_backed_before, 2);
    assert_eq!(separately_rounded_after - cash_backed_before, 0);
}

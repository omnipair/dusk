use super::*;
use crate::constants::YIELD_GROWTH_SCALE_Q64;

fn empty_yield_account() -> YieldAccount {
    YieldAccount {
        escrow_balance: 0,
        owner: Pubkey::default(),
        market: Pubkey::default(),
        lp_mint: Pubkey::default(),
        asset_mint: Pubkey::default(),
        token_kind: 0,
        recipient: Pubkey::default(),
        swap_fee_checkpoint_q64: 0,
        interest_checkpoint_q64: 0,
        accrued_swap_fee_amount: 0,
        accrued_interest_amount: 0,
        swap_fee_remainder_q64: 0,
        interest_remainder_q64: 0,
        bump: 0,
        harvest_authority: None,
    }
}

#[test]
fn split_checkpoint_interest_growth_telescopes_to_single_checkpoint() {
    let mut single_exit = empty_yield_account();
    single_exit.accrue(1, 0, YIELD_GROWTH_SCALE_Q64).unwrap();

    let mut split_exit = empty_yield_account();
    split_exit.accrue(1, 0, YIELD_GROWTH_SCALE_Q64 / 2).unwrap();
    split_exit.accrue(1, 0, YIELD_GROWTH_SCALE_Q64).unwrap();

    assert_eq!(split_exit.accrued_interest_amount, single_exit.accrued_interest_amount);
    assert_eq!(split_exit.interest_remainder_q64, single_exit.interest_remainder_q64);
    assert_eq!(split_exit.accrued_interest_amount, 1);
    assert_eq!(split_exit.interest_remainder_q64, 0);
}

#[test]
fn cash_limited_claim_keeps_the_unpaid_interest_claimable() {
    let mut account = empty_yield_account();
    account.accrue(10, 3 * YIELD_GROWTH_SCALE_Q64, 5 * YIELD_GROWTH_SCALE_Q64).unwrap();
    assert_eq!(account.accrued_swap_fee_amount, 30);
    assert_eq!(account.accrued_interest_amount, 50);
    account.clear_claimed(20).unwrap();
    assert_eq!(account.accrued_swap_fee_amount, 0);
    assert_eq!(account.accrued_interest_amount, 30);
    assert!(account.clear_claimed(31).is_err());
}

#[test]
fn junior_escrow_keeps_earning_interest() {
    let mut account = empty_yield_account();
    account.token_kind = YieldTokenKind::Junior.code();
    account.escrow_balance = 4;
    account.accrue(6, 0, 2 * YIELD_GROWTH_SCALE_Q64).unwrap();
    assert_eq!(account.accrued_interest_amount, 20);
}

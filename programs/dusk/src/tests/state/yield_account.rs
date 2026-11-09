use super::*;
use crate::constants::YIELD_GROWTH_SCALE_Q64;

fn empty_yield_account() -> YieldAccount {
    YieldAccount {
        income_initialized: false,
        income: Default::default(),
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
fn split_exit_interest_growth_telescopes_to_single_exit() {
    let mut single_exit = empty_yield_account();
    single_exit
        .credit_interest_growth(1, YIELD_GROWTH_SCALE_Q64, 0)
        .unwrap();

    let mut split_exit = empty_yield_account();
    split_exit
        .credit_interest_growth(1, YIELD_GROWTH_SCALE_Q64 / 2, 0)
        .unwrap();
    split_exit
        .credit_interest_growth(1, YIELD_GROWTH_SCALE_Q64, YIELD_GROWTH_SCALE_Q64 / 2)
        .unwrap();

    assert_eq!(split_exit.accrued_interest_amount, single_exit.accrued_interest_amount);
    assert_eq!(split_exit.interest_remainder_q64, single_exit.interest_remainder_q64);
    assert_eq!(split_exit.accrued_interest_amount, 1);
    assert_eq!(split_exit.interest_remainder_q64, 0);
}

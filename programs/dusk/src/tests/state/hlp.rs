use super::*;
use crate::{
    constants::{NAD, YIELD_GROWTH_SCALE_Q64},
    state::YieldTokenKind,
};

#[test]
fn hlp_raw_principal_accepts_u64_max_and_rejects_the_next_atom() {
    let mut vault = HlpVault::default();
    vault.add_debt_principal(u64::MAX).unwrap();
    assert_eq!(vault.debt_principal, u64::MAX);
    assert!(vault.add_debt_principal(1).is_err());
    assert_eq!(vault.debt_principal, u64::MAX);
}

#[test]
fn hlp_vault_checkpoints_owned_ylp_revenue_into_hlp_indexes() {
    let mut vault = HlpVault {
        ylp_shares: 50,
        hlp_supply: 25,
        ..HlpVault::default()
    };
    let mut base_side = MarketSide::default();
    let quote_side = MarketSide::default();
    base_side.fees.swap_fee_growth_index_q64 = 2 * YIELD_GROWTH_SCALE_Q64;
    base_side.fees.interest_growth_index_q64 = 3 * YIELD_GROWTH_SCALE_Q64;

    vault.checkpoint_yield_from_ylp(&base_side, &quote_side).unwrap();

    assert_eq!(vault.base_swap_fee_growth_index_q64, 4 * YIELD_GROWTH_SCALE_Q64);
    assert_eq!(vault.base_interest_growth_index_q64, 6 * YIELD_GROWTH_SCALE_Q64);
    assert_eq!(
        vault.base_swap_fee_checkpoint_q64,
        base_side.fees.swap_fee_growth_index_q64
    );
    assert_eq!(
        vault.base_interest_checkpoint_q64,
        base_side.fees.interest_growth_index_q64
    );
}

#[test]
fn split_hlp_checkpoints_preserve_all_four_aggregate_remainders() {
    let mut split = HlpVault {
        ylp_shares: 1,
        hlp_supply: 1,
        ..HlpVault::default()
    };
    let mut base_side = MarketSide::default();
    let mut quote_side = MarketSide::default();
    base_side.fees.swap_fee_growth_index_q64 = YIELD_GROWTH_SCALE_Q64 / 2;
    base_side.fees.interest_growth_index_q64 = YIELD_GROWTH_SCALE_Q64 / 2;
    quote_side.fees.swap_fee_growth_index_q64 = YIELD_GROWTH_SCALE_Q64 / 2;
    quote_side.fees.interest_growth_index_q64 = YIELD_GROWTH_SCALE_Q64 / 2;
    split.checkpoint_yield_from_ylp(&base_side, &quote_side).unwrap();
    assert_eq!(split.base_swap_fee_remainder_q64, 1_u64 << 63);
    assert_eq!(split.base_interest_remainder_q64, 1_u64 << 63);
    assert_eq!(split.quote_swap_fee_remainder_q64, 1_u64 << 63);
    assert_eq!(split.quote_interest_remainder_q64, 1_u64 << 63);

    base_side.fees.swap_fee_growth_index_q64 = YIELD_GROWTH_SCALE_Q64;
    base_side.fees.interest_growth_index_q64 = YIELD_GROWTH_SCALE_Q64;
    quote_side.fees.swap_fee_growth_index_q64 = YIELD_GROWTH_SCALE_Q64;
    quote_side.fees.interest_growth_index_q64 = YIELD_GROWTH_SCALE_Q64;
    split.checkpoint_yield_from_ylp(&base_side, &quote_side).unwrap();

    let mut combined = HlpVault {
        ylp_shares: 1,
        hlp_supply: 1,
        ..HlpVault::default()
    };
    combined.checkpoint_yield_from_ylp(&base_side, &quote_side).unwrap();
    assert_eq!(
        split.base_swap_fee_growth_index_q64,
        combined.base_swap_fee_growth_index_q64
    );
    assert_eq!(
        split.base_interest_growth_index_q64,
        combined.base_interest_growth_index_q64
    );
    assert_eq!(
        split.quote_swap_fee_growth_index_q64,
        combined.quote_swap_fee_growth_index_q64
    );
    assert_eq!(
        split.quote_interest_growth_index_q64,
        combined.quote_interest_growth_index_q64
    );
    assert_eq!(split.base_swap_fee_remainder_q64, 0);
    assert_eq!(split.base_interest_remainder_q64, 0);
    assert_eq!(split.quote_swap_fee_remainder_q64, 0);
    assert_eq!(split.quote_interest_remainder_q64, 0);
}

#[test]
fn nested_hlp_distribution_does_not_reallocate_fractional_entitlement() {
    let mut unallocated = 0;
    let mut growth_index = 0;
    let mut distributor_carry = 0;

    credit_hlp_growth(3, &mut unallocated, &mut growth_index, &mut distributor_carry, 2).unwrap();
    assert_eq!(growth_index, 2 * YIELD_GROWTH_SCALE_Q64 / 3);
    assert_eq!(unallocated, 0);
    assert_eq!(distributor_carry, 2);

    credit_hlp_growth(3, &mut unallocated, &mut growth_index, &mut distributor_carry, 1).unwrap();
    assert_eq!(growth_index, YIELD_GROWTH_SCALE_Q64);
    assert_eq!(unallocated, 0);
    assert_eq!(distributor_carry, 0);

    let claims = (0..3)
        .map(|_| accrue_fee_liability_with_remainder(1, growth_index, 0, 0).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(claims.iter().map(|(amount, _)| *amount).sum::<u64>(), 3);
    assert!(claims.iter().all(|(_, remainder)| *remainder == 0));
}

#[test]
fn hlp_debt_clearance_tracks_realized_interest_separately() {
    let mut vault = HlpVault::default();
    vault.add_debt_shares(1_000).unwrap();
    vault.add_debt_principal(1_000).unwrap();

    let clearance = vault.clear_debt_repay(500, (NAD as u128) * 11 / 10).unwrap();

    assert_eq!(clearance.cash_repaid, 550);
    assert_eq!(clearance.interest_paid, 50);
    assert_eq!(clearance.principal_paid, 500);
    assert_eq!(clearance.debt_reduced, 550);
    assert_eq!(vault.debt_principal, 500);
}

#[test]
fn hlp_debt_clearance_reports_actual_debt_reduced_after_rounded_share_burn() {
    let mut vault = HlpVault::default();
    vault.add_debt_shares(100).unwrap();
    vault.add_debt_principal(100).unwrap();

    let clearance = vault.clear_debt_repay(2, (NAD as u128) * 3 / 2).unwrap();

    assert_eq!(clearance.shares_burned, 2);
    assert_eq!(clearance.cash_repaid, 3);
    assert_eq!(clearance.debt_reduced, 3);
    assert_eq!(clearance.remaining_debt, 147);
    assert_eq!(clearance.principal_paid, 2);
    assert_eq!(clearance.interest_paid, 1);
    assert_eq!(vault.debt_shares, 98);
    assert_eq!(vault.debt_principal, 98);
}

#[test]
fn debt_borrowed_and_repaid_at_the_same_index_creates_no_new_interest() {
    let borrow_index = (NAD as u128) * 11 / 10;
    let mut vault = HlpVault::default();
    // Existing debt has accrued ten atoms of interest.
    vault.add_debt_shares(100).unwrap();
    vault.add_debt_principal(100).unwrap();
    // Fifty new shares borrow 55 atoms at the current 1.1 index. Their
    // principal and debt are identical, so they contribute no interest.
    vault.add_debt_shares(50).unwrap();
    vault.add_debt_principal(55).unwrap();

    let clearance = vault.clear_debt_repay(150, borrow_index).unwrap();
    assert_eq!(clearance.cash_repaid, 165);
    assert_eq!(clearance.principal_paid, 155);
    assert_eq!(clearance.interest_paid, 10);
}

#[test]
fn final_hlp_exit_drains_both_asset_remainders_to_final_holder() {
    let owner = Pubkey::new_unique();
    let market_key = Pubkey::new_unique();
    let hlp_mint = Pubkey::new_unique();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let mut market = Market::default();
    market.base_hlp_vault.unallocated_base_swap_fee_amount = 2;
    market.base_hlp_vault.unallocated_base_interest_amount = 3;
    market.base_hlp_vault.unallocated_quote_swap_fee_amount = 5;
    market.base_hlp_vault.unallocated_quote_interest_amount = 7;
    market.base_hlp_vault.base_swap_fee_remainder_q64 = 1_u64 << 63;
    market.base_hlp_vault.base_interest_remainder_q64 = 1_u64 << 63;
    market.base_hlp_vault.quote_swap_fee_remainder_q64 = 1_u64 << 63;
    market.base_hlp_vault.quote_interest_remainder_q64 = 1_u64 << 63;
    // A distributor carry is always <2^64, hence strictly below one raw
    // atom. Together with the yLP and holder half-atoms it telescopes to
    // exactly one atom on final close.
    market.base_hlp_vault.base_swap_fee_growth_remainder_scaled = u64::MAX / 2;
    market.base_hlp_vault.base_interest_growth_remainder_scaled = u64::MAX / 2;
    market.base_hlp_vault.quote_swap_fee_growth_remainder_scaled = u64::MAX / 2;
    market.base_hlp_vault.quote_interest_growth_remainder_scaled = u64::MAX / 2;
    let mut base_account = YieldAccount {
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
    };
    let mut quote_account = YieldAccount {
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
    };
    base_account.initialize(owner, market_key, hlp_mint, base_mint, YieldTokenKind::Hlp, owner, 1);
    quote_account.initialize(owner, market_key, hlp_mint, quote_mint, YieldTokenKind::Hlp, owner, 2);
    base_account.swap_fee_remainder_q64 = 1;
    base_account.interest_remainder_q64 = 1;
    quote_account.swap_fee_remainder_q64 = 1;
    quote_account.interest_remainder_q64 = 1;

    market
        .drain_hlp_unallocated_yield(MarketAsset::Base, &mut base_account, &mut quote_account)
        .unwrap();

    assert_eq!(
        (
            base_account.accrued_swap_fee_amount,
            base_account.accrued_interest_amount
        ),
        (3, 4)
    );
    assert_eq!(
        (
            quote_account.accrued_swap_fee_amount,
            quote_account.accrued_interest_amount
        ),
        (6, 8)
    );
    assert_eq!(base_account.swap_fee_remainder_q64, 0);
    assert_eq!(base_account.interest_remainder_q64, 0);
    assert_eq!(quote_account.swap_fee_remainder_q64, 0);
    assert_eq!(quote_account.interest_remainder_q64, 0);
    assert_eq!(market.base_hlp_vault.unallocated_base_swap_fee_amount, 0);
    assert_eq!(market.base_hlp_vault.unallocated_base_interest_amount, 0);
    assert_eq!(market.base_hlp_vault.unallocated_quote_swap_fee_amount, 0);
    assert_eq!(market.base_hlp_vault.unallocated_quote_interest_amount, 0);
    assert_eq!(market.base_hlp_vault.base_swap_fee_remainder_q64, 0);
    assert_eq!(market.base_hlp_vault.base_interest_remainder_q64, 0);
    assert_eq!(market.base_hlp_vault.quote_swap_fee_remainder_q64, 0);
    assert_eq!(market.base_hlp_vault.quote_interest_remainder_q64, 0);
    assert_eq!(market.base_hlp_vault.base_swap_fee_growth_remainder_scaled, 0);
    assert_eq!(market.base_hlp_vault.base_interest_growth_remainder_scaled, 0);
    assert_eq!(market.base_hlp_vault.quote_swap_fee_growth_remainder_scaled, 0);
    assert_eq!(market.base_hlp_vault.quote_interest_growth_remainder_scaled, 0);
}

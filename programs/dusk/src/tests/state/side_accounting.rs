use super::*;
use crate::{accrue_fee_liability, accrue_fee_liability_with_remainder, constants::YIELD_GROWTH_SCALE_Q64};

// MarketSide accounting invariants exercised through its public domain API.

#[test]
fn swap_protocol_fee_splits_between_auction_lanes_at_accrual() {
    let mut side = MarketSide::default();
    let receipt = side
        .record_swap_fee_credit(
            10_000,
            2_000,
            ProtocolAuctionSplit {
                fee_auction_bps: 7_500,
                buyback_auction_bps: 2_500,
            },
        )
        .unwrap();

    assert_eq!(receipt.protocol_fee_liability, 1_500);
    assert_eq!(receipt.buyback_fee_liability, 500);
    assert_eq!(receipt.unallocated_swap_fee_liability, 8_000);
    assert_eq!(receipt.swap_fee_custody_balance, 10_000);
    side.fees.assert_backed().unwrap();
}

#[test]
fn distributed_dynamic_surcharge_belongs_entirely_to_ylps() {
    let mut side = MarketSide::default();
    let receipt = side
        .record_claimable_swap_fees(
            10_000,
            3_000,
            2_000,
            ProtocolAuctionSplit {
                fee_auction_bps: 7_500,
                buyback_auction_bps: 2_500,
            },
            0,
        )
        .unwrap();

    assert_eq!(receipt.protocol_fee_liability, 1_500);
    assert_eq!(receipt.buyback_fee_liability, 500);
    assert_eq!(receipt.unallocated_swap_fee_liability, 11_000);
    assert_eq!(receipt.swap_fee_custody_balance, 13_000);
    side.fees.assert_backed().unwrap();
}

#[test]
fn surcharge_only_credit_has_no_protocol_leakage() {
    let mut side = MarketSide::default();
    let receipt = side
        .record_claimable_swap_fees(
            0,
            3_000,
            2_000,
            ProtocolAuctionSplit {
                fee_auction_bps: 7_500,
                buyback_auction_bps: 2_500,
            },
            0,
        )
        .unwrap();

    assert_eq!(receipt.protocol_fee_liability, 0);
    assert_eq!(receipt.buyback_fee_liability, 0);
    assert_eq!(receipt.unallocated_swap_fee_liability, 3_000);
    assert_eq!(receipt.swap_fee_custody_balance, 3_000);
    side.fees.assert_backed().unwrap();
}

#[test]
fn distributed_surcharge_uses_the_current_swap_eligible_supply() {
    let mut side = MarketSide::default();
    let receipt = side
        .record_claimable_swap_fees(
            1_000,
            500,
            2_000,
            ProtocolAuctionSplit {
                fee_auction_bps: 10_000,
                buyback_auction_bps: 0,
            },
            1_000,
        )
        .unwrap();

    // Base LP fee is 800; all 500 surcharge units also go to yLPs.
    assert_eq!(
        receipt.swap_fee_growth_index_q64,
        1_300u128 * YIELD_GROWTH_SCALE_Q64 / 1_000
    );
    assert_eq!(receipt.swap_fee_liability, 1_300);
    assert_eq!(receipt.unallocated_swap_fee_liability, 0);
    assert_eq!(receipt.protocol_fee_liability, 200);
    assert_eq!(receipt.swap_fee_custody_balance, 1_500);
    side.fees.assert_backed().unwrap();
}

#[test]
fn interest_protocol_fee_splits_between_auction_lanes_at_accrual() {
    let mut side = MarketSide::default();
    let receipt = side
        .record_interest_credit(
            10_000,
            1_000,
            ProtocolAuctionSplit {
                fee_auction_bps: 4_000,
                buyback_auction_bps: 6_000,
            },
            250,
        )
        .unwrap();

    assert_eq!(receipt.referral_interest_liability, 250);
    assert_eq!(receipt.protocol_fee_liability, 300);
    assert_eq!(receipt.buyback_fee_liability, 450);
    assert_eq!(receipt.unallocated_interest_liability, 9_000);
    assert_eq!(receipt.interest_vault_balance, 10_000);
    side.fees.assert_backed().unwrap();
}

#[test]
fn direct_lp_token_burn_is_an_irreversible_yield_donation() {
    let mut side = MarketSide::default();
    side.shares.ylp_supply = 100;
    let split = ProtocolAuctionSplit::default();

    let first = side.record_swap_fee_credit(100, 0, split).unwrap();
    // Simulate an owner-authorized Token-2022 Burn of 40 LP tokens. The
    // token program does not invoke transfer hooks, so Dusk state cannot
    // and must not reduce its internal claim denominator.
    let externally_remaining_balance = 60;
    assert_eq!(side.shares.ylp_supply, 100);
    assert_eq!(
        accrue_fee_liability(externally_remaining_balance, first.swap_fee_growth_index_q64, 0,).unwrap(),
        60
    );

    let second = side.record_swap_fee_credit(100, 0, split).unwrap();
    let second_period_claim = accrue_fee_liability(
        externally_remaining_balance,
        second.swap_fee_growth_index_q64,
        first.swap_fee_growth_index_q64,
    )
    .unwrap();

    assert_eq!(second_period_claim, 60);
    assert_eq!(side.shares.ylp_supply, 100);
    assert_eq!(second.swap_fee_liability, 200);
    // The burned 40% neither increases the remaining holder's 60% rate
    // nor creates a token balance that could authorize withdrawal.
    assert_eq!(60 + second_period_claim, 120);
}

#[test]
fn fractional_growth_is_backed_once_across_interleaved_transfer_checkpoints() {
    let mut side = MarketSide::default();
    side.fees.swap_fee_custody_balance = 3;
    side.fees.unallocated_swap_fee_liability = 2;
    side.carry_forward_swap_fees_with_supply(3).unwrap();

    let first_index = side.fees.swap_fee_growth_index_q64;
    assert_eq!(first_index, 2 * YIELD_GROWTH_SCALE_Q64 / 3);
    assert_eq!(side.fees.swap_fee_liability, 2);
    assert_eq!(side.fees.unallocated_swap_fee_liability, 0);
    assert_eq!(side.fees.swap_fee_growth_remainder_scaled, 2);

    // Checkpoint both sides of a one-share transfer before balances move.
    let (source_first, source_remainder) = accrue_fee_liability_with_remainder(1, first_index, 0, 0).unwrap();
    let (destination_first, destination_remainder) = accrue_fee_liability_with_remainder(1, first_index, 0, 0).unwrap();
    assert_eq!((source_first, destination_first), (0, 0));

    // The source now holds zero shares, the destination two, and the
    // untouched third holder one. One more atom completes the exact
    // distributor carry left by the first round.
    side.fees.unallocated_swap_fee_liability = 1;
    side.carry_forward_swap_fees_with_supply(3).unwrap();
    let final_index = side.fees.swap_fee_growth_index_q64;
    let (source_second, source_final_remainder) =
        accrue_fee_liability_with_remainder(0, final_index, first_index, source_remainder).unwrap();
    let (destination_second, destination_final_remainder) =
        accrue_fee_liability_with_remainder(2, final_index, first_index, destination_remainder).unwrap();
    let (third_claim, third_remainder) = accrue_fee_liability_with_remainder(1, final_index, 0, 0).unwrap();

    assert_eq!(final_index, YIELD_GROWTH_SCALE_Q64);
    assert_eq!(side.fees.swap_fee_growth_remainder_scaled, 0);
    assert_eq!(side.fees.swap_fee_liability, 3);
    assert_eq!(source_second + destination_second + third_claim, 2);
    assert_eq!(source_final_remainder as u128, 2 * YIELD_GROWTH_SCALE_Q64 / 3);
    assert_eq!(
        destination_final_remainder as u128 + third_remainder as u128,
        YIELD_GROWTH_SCALE_Q64 / 3 + 1
    );
    assert_eq!(
        ((source_second + destination_second + third_claim) as u128) * YIELD_GROWTH_SCALE_Q64
            + source_final_remainder as u128
            + destination_final_remainder as u128
            + third_remainder as u128,
        3 * YIELD_GROWTH_SCALE_Q64
    );
    side.fees.assert_backed().unwrap();
}

#[test]
fn distributor_carry_remains_exact_when_supply_changes() {
    let (first_delta, first_carry) = distribute_growth_q64(2, 3, 0).unwrap();
    assert_eq!(2 * YIELD_GROWTH_SCALE_Q64, first_delta * 3 + first_carry as u128);
    assert!(first_carry < 3);

    // A supply reduction may make a previously sub-index carry
    // representable without any new revenue.
    let (shrink_delta, shrink_carry) = distribute_growth_q64(0, 2, first_carry).unwrap();
    assert_eq!(first_carry as u128, shrink_delta * 2 + shrink_carry as u128);
    assert!(shrink_carry < 2);

    let (second_delta, second_carry) = distribute_growth_q64(1, 2, shrink_carry).unwrap();
    assert_eq!(
        YIELD_GROWTH_SCALE_Q64 + shrink_carry as u128,
        second_delta * 2 + second_carry as u128
    );
    assert!(second_carry < 2);
}

#[test]
fn q64_carry_limits_cohort_drift_below_one_atom_for_large_supply() {
    let old_supply = 5_000_000_007_u64;
    let old_balances = [2_000_000_003_u64, 3_000_000_004_u64];
    let mut side = MarketSide::default();
    side.fees.swap_fee_custody_balance = 5;
    side.fees.unallocated_swap_fee_liability = 3;
    side.carry_forward_swap_fees_with_supply(old_supply).unwrap();
    let old_index = side.fees.swap_fee_growth_index_q64;
    let old_carry = side.fees.swap_fee_growth_remainder_scaled;

    let mut claims = 0_u64;
    let mut remainders = [0_u64; 3];
    for (index, balance) in old_balances.into_iter().enumerate() {
        let (amount, remainder) = accrue_fee_liability_with_remainder(balance, old_index, 0, 0).unwrap();
        claims = claims.checked_add(amount).unwrap();
        remainders[index] = remainder;
    }

    // A new cohort enters after the old holders checkpoint. Reusing the
    // aggregate carry can move only `old_carry / 2^64`, which is strictly
    // less than one raw atom regardless of the (u64) share supply.
    assert!((old_carry as u128) < YIELD_GROWTH_SCALE_Q64);
    let entrant_balance = 2_000_000_011_u64;
    let new_supply = old_supply.checked_add(entrant_balance).unwrap();
    side.fees.unallocated_swap_fee_liability = 2;
    side.carry_forward_swap_fees_with_supply(new_supply).unwrap();
    let final_index = side.fees.swap_fee_growth_index_q64;

    for (index, balance) in old_balances.into_iter().enumerate() {
        let (amount, remainder) =
            accrue_fee_liability_with_remainder(balance, final_index, old_index, remainders[index]).unwrap();
        claims = claims.checked_add(amount).unwrap();
        remainders[index] = remainder;
    }
    let (entrant_claim, entrant_remainder) =
        accrue_fee_liability_with_remainder(entrant_balance, final_index, old_index, 0).unwrap();
    claims = claims.checked_add(entrant_claim).unwrap();
    remainders[2] = entrant_remainder;

    let represented = (claims as u128)
        .checked_mul(YIELD_GROWTH_SCALE_Q64)
        .and_then(|value| value.checked_add(remainders.into_iter().map(u128::from).sum::<u128>()))
        .and_then(|value| value.checked_add(side.fees.swap_fee_growth_remainder_scaled as u128))
        .unwrap();
    assert_eq!(represented, 5 * YIELD_GROWTH_SCALE_Q64);
    assert!(claims <= side.fees.swap_fee_liability);
    assert_eq!(side.fees.swap_fee_liability, 5);
    side.fees.assert_backed().unwrap();
}

#[test]
fn invalid_auction_split_is_rejected_before_liabilities_move() {
    let mut side = MarketSide::default();
    let err = side
        .record_swap_fee_credit(
            10_000,
            1_000,
            ProtocolAuctionSplit {
                fee_auction_bps: 7_000,
                buyback_auction_bps: 4_000,
            },
        )
        .unwrap_err();

    assert_eq!(err, error!(ErrorCode::InvalidDistribution));
    assert_eq!(side.fees.swap_fee_custody_balance, 0);
    assert_eq!(side.fees.protocol_fee_liability().unwrap(), 0);
    assert_eq!(side.fees.buyback_fee_liability().unwrap(), 0);
}

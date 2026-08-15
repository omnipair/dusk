use super::*;
use crate::{
    constants::{MIN_LIQUIDITY, YIELD_GROWTH_SCALE_Q64},
    state::{accrue_fee_liability_with_remainder, distribute_growth_q64},
};
use spl_token_2022::extension::transfer_fee::TransferFee;

#[test]
fn token_2022_hlp_interest_books_only_actual_vault_credit() {
    let transfer_fee = TransferFee {
        epoch: 0_u64.into(),
        maximum_fee: u64::MAX.into(),
        transfer_fee_basis_points: 300_u16.into(),
    };
    let gross_interest_paid = 10_000;
    let actual_interest_credit = transfer_fee.calculate_post_fee_amount(gross_interest_paid).unwrap();
    let mut market = Market::default();
    market.base_side.shares.ylp_supply = 4_000;
    market.quote_side.shares.ylp_supply = 4_000;
    market.base_hlp_vault.ylp_shares = 1_000;
    market.base_hlp_vault.hlp_supply = 1_000;
    market.quote_hlp_vault.ylp_shares = 1_000;
    market.quote_hlp_vault.hlp_supply = 1_000;
    let eligibility = HlpYieldEligibility {
        ylp_supply: 4_000,
        base_hlp_ylp_shares: 1_000,
        quote_hlp_ylp_shares: 1_000,
    };

    record_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        actual_interest_credit,
        0,
        ProtocolAuctionSplit::default(),
        eligibility,
    )
    .unwrap();

    assert!(actual_interest_credit < gross_interest_paid);
    assert_eq!(actual_interest_credit, 9_700);
    assert_eq!(market.quote_side.fees.interest_vault_balance, actual_interest_credit);
    assert_eq!(market.quote_side.fees.interest_liability, actual_interest_credit);
    assert_eq!(market.quote_side.fees.unallocated_interest_liability, 0);
    let (expected_growth, expected_remainder) = distribute_growth_q64(actual_interest_credit, 2_000, 0).unwrap();
    assert_eq!(market.quote_side.fees.interest_growth_index_q64, expected_growth);
    assert_eq!(
        market.quote_side.fees.hlp_funding_interest_growth_remainder_scaled,
        expected_remainder
    );
    assert_eq!(market.base_hlp_vault.quote_interest_growth_index_q64, 0);
    assert_eq!(market.quote_hlp_vault.quote_interest_growth_index_q64, 0);
    market.quote_side.fees.assert_backed().unwrap();
}

#[test]
fn inline_funding_preserves_prior_public_yield_but_excludes_both_hlps() {
    let mut market = Market::default();
    market.base_side.shares.ylp_supply = 10_000;
    market.quote_side.shares.ylp_supply = 10_000;
    market.base_hlp_vault.ylp_shares = 4_000;
    market.base_hlp_vault.hlp_supply = 4_000;
    market.quote_hlp_vault.ylp_shares = 2_000;
    market.quote_hlp_vault.hlp_supply = 2_000;
    market.base_hlp_vault.quote_interest_remainder_q64 = 17;
    market.quote_hlp_vault.quote_interest_remainder_q64 = 29;
    let eligibility = HlpYieldEligibility {
        ylp_supply: 10_000,
        base_hlp_ylp_shares: 4_000,
        quote_hlp_ylp_shares: 2_000,
    };
    market
        .quote_side
        .record_interest_credit_with_supply(1_000, 0, ProtocolAuctionSplit::default(), 0, 10_000)
        .unwrap();
    let public_growth = market.quote_side.fees.interest_growth_index_q64;
    let public_carry = market.quote_side.fees.interest_growth_remainder_scaled;
    let (base_public_whole, base_public_remainder) =
        accrue_fee_liability_with_remainder(4_000, public_growth, 0, 17).unwrap();
    let (base_nested_growth, base_nested_remainder) =
        distribute_growth_q64(base_public_whole, 4_000, 0).unwrap();
    let (quote_public_whole, quote_public_remainder) =
        accrue_fee_liability_with_remainder(2_000, public_growth, 0, 29).unwrap();
    let (quote_nested_growth, quote_nested_remainder) =
        distribute_growth_q64(quote_public_whole, 2_000, 0).unwrap();
    market.checkpoint_hlp_yield_from_ylp(MarketAsset::Base).unwrap();
    market.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote).unwrap();

    // Model predictive yLP ownership changes while conserving ordinary+MIN
    // supply. The mutation boundary already checkpointed prior public yield;
    // the new hLP-funded interval credits neither old nor current balances.
    market.base_side.shares.ylp_supply = 9_000;
    market.quote_side.shares.ylp_supply = 9_000;
    market.base_hlp_vault.ylp_shares = 1_000;
    market.quote_hlp_vault.ylp_shares = 4_000;
    record_inline_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        400,
        0,
        ProtocolAuctionSplit::default(),
        eligibility,
    )
    .unwrap();

    assert_eq!(market.quote_side.fees.interest_liability, 1_400);
    assert_eq!(market.quote_side.fees.unallocated_interest_liability, 0);
    assert_eq!(market.quote_side.fees.interest_growth_remainder_scaled, public_carry);
    assert_eq!(market.base_hlp_vault.quote_interest_growth_index_q64, base_nested_growth);
    assert_eq!(
        market.base_hlp_vault.quote_interest_growth_remainder_scaled,
        base_nested_remainder
    );
    assert_eq!(market.base_hlp_vault.quote_interest_remainder_q64, base_public_remainder);
    assert_eq!(market.quote_hlp_vault.quote_interest_growth_index_q64, quote_nested_growth);
    assert_eq!(
        market.quote_hlp_vault.quote_interest_growth_remainder_scaled,
        quote_nested_remainder
    );
    assert_eq!(market.quote_hlp_vault.quote_interest_remainder_q64, quote_public_remainder);
    let global_growth = market.quote_side.fees.interest_growth_index_q64;
    assert_eq!(market.base_hlp_vault.quote_interest_checkpoint_q64, global_growth);
    assert_eq!(market.quote_hlp_vault.quote_interest_checkpoint_q64, global_growth);
    market.quote_side.fees.assert_backed().unwrap();
}

#[test]
fn public_interest_does_not_consume_hlp_funding_carry() {
    let mut market = Market::default();
    market.base_side.shares.ylp_supply = 10_000;
    market.quote_side.shares.ylp_supply = 10_000;
    market.base_hlp_vault.ylp_shares = 4_000;
    market.base_hlp_vault.hlp_supply = 4_000;
    market.quote_hlp_vault.ylp_shares = 2_000;
    market.quote_hlp_vault.hlp_supply = 2_000;
    let eligibility = HlpYieldEligibility {
        ylp_supply: 10_000,
        base_hlp_ylp_shares: 4_000,
        quote_hlp_ylp_shares: 2_000,
    };
    record_inline_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        401,
        0,
        ProtocolAuctionSplit::default(),
        eligibility,
    )
    .unwrap();
    let funding_carry = market.quote_side.fees.hlp_funding_interest_growth_remainder_scaled;
    assert!(funding_carry > 0);
    let index_after_funding = market.quote_side.fees.interest_growth_index_q64;
    assert_eq!(market.base_hlp_vault.quote_interest_growth_index_q64, 0);
    assert_eq!(market.quote_hlp_vault.quote_interest_growth_index_q64, 0);

    market
        .quote_side
        .record_interest_credit_with_supply(137, 0, ProtocolAuctionSplit::default(), 0, 10_000)
        .unwrap();
    let (public_growth, public_carry) = distribute_growth_q64(137, 10_000, 0).unwrap();
    assert_eq!(
        market.quote_side.fees.interest_growth_index_q64 - index_after_funding,
        public_growth
    );
    assert_eq!(
        market.quote_side.fees.hlp_funding_interest_growth_remainder_scaled,
        funding_carry
    );
    assert_eq!(market.quote_side.fees.interest_growth_remainder_scaled, public_carry);

    market.checkpoint_hlp_yield_from_ylp(MarketAsset::Base).unwrap();
    market.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote).unwrap();
    let (base_public_whole, base_outer_remainder) =
        accrue_fee_liability_with_remainder(4_000, public_growth, 0, 0).unwrap();
    let (base_nested_growth, base_nested_remainder) =
        distribute_growth_q64(base_public_whole, 4_000, 0).unwrap();
    let (quote_public_whole, quote_outer_remainder) =
        accrue_fee_liability_with_remainder(2_000, public_growth, 0, 0).unwrap();
    let (quote_nested_growth, quote_nested_remainder) =
        distribute_growth_q64(quote_public_whole, 2_000, 0).unwrap();
    assert_eq!(market.base_hlp_vault.quote_interest_growth_index_q64, base_nested_growth);
    assert_eq!(
        market.base_hlp_vault.quote_interest_growth_remainder_scaled,
        base_nested_remainder
    );
    assert_eq!(market.base_hlp_vault.quote_interest_remainder_q64, base_outer_remainder);
    assert_eq!(market.quote_hlp_vault.quote_interest_growth_index_q64, quote_nested_growth);
    assert_eq!(
        market.quote_hlp_vault.quote_interest_growth_remainder_scaled,
        quote_nested_remainder
    );
    assert_eq!(market.quote_hlp_vault.quote_interest_remainder_q64, quote_outer_remainder);
}

#[test]
fn min_liquidity_sinks_zero_holder_funding_and_later_deposit_starts_clean() {
    let mut market = Market::default();
    market.base_side.shares.ylp_supply = 301_000;
    market.quote_side.shares.ylp_supply = 301_000;
    market.base_hlp_vault.ylp_shares = 200_000;
    market.base_hlp_vault.hlp_supply = 200_000;
    market.quote_hlp_vault.ylp_shares = 100_000;
    market.quote_hlp_vault.hlp_supply = 100_000;
    let eligibility = HlpYieldEligibility {
        ylp_supply: 301_000,
        base_hlp_ylp_shares: 200_000,
        quote_hlp_ylp_shares: 100_000,
    };

    record_inline_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        97_000,
        1_000,
        ProtocolAuctionSplit::default(),
        eligibility,
    )
    .unwrap();

    let lp_interest = 87_300;
    let (expected_growth, expected_burned_remainder) =
        distribute_growth_q64(lp_interest, MIN_LIQUIDITY, 0).unwrap();
    assert_eq!(expected_burned_remainder, 800);
    assert_eq!(market.quote_side.fees.interest_growth_index_q64, expected_growth);
    assert_eq!(market.quote_side.fees.interest_protocol_fee_liability, 9_700);
    assert_eq!(market.quote_side.fees.interest_liability, lp_interest);
    assert_eq!(market.quote_side.fees.unallocated_interest_liability, 0);
    assert_eq!(
        market.quote_side.fees.hlp_funding_interest_growth_remainder_scaled,
        0
    );

    // A depositor arriving after publication initializes at this checkpoint.
    // Increasing supply cannot make the already-backed sink claimable.
    market.base_side.shares.ylp_supply += 50_000;
    market.quote_side.shares.ylp_supply += 50_000;
    let post_funding_checkpoint = market.quote_side.fees.interest_growth_index_q64;
    market.quote_side.carry_forward_interest().unwrap();
    assert_eq!(market.quote_side.fees.interest_growth_index_q64, post_funding_checkpoint);
    assert_eq!(
        accrue_fee_liability_with_remainder(50_000, post_funding_checkpoint, post_funding_checkpoint, 0).unwrap(),
        (0, 0)
    );
    market.quote_side.fees.assert_backed().unwrap();
}

#[test]
fn last_ordinary_exit_clears_funding_carry_before_next_cohort() {
    let mut market = Market::default();
    market.base_side.shares.ylp_supply = 4_000;
    market.quote_side.shares.ylp_supply = 4_000;
    market.base_side.reserves.live_reserve = 4_000;
    market.base_side.reserves.cash_reserve = 4_000;
    market.quote_side.reserves.live_reserve = 4_000;
    market.quote_side.reserves.cash_reserve = 4_000;
    market.base_hlp_vault.ylp_shares = 1_000;
    market.base_hlp_vault.hlp_supply = 1_000;
    market.quote_hlp_vault.ylp_shares = 1_000;
    market.quote_hlp_vault.hlp_supply = 1_000;
    let first_eligibility = HlpYieldEligibility {
        ylp_supply: 4_000,
        base_hlp_ylp_shares: 1_000,
        quote_hlp_ylp_shares: 1_000,
    };
    record_inline_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        401,
        0,
        ProtocolAuctionSplit::default(),
        first_eligibility,
    )
    .unwrap();
    assert!(market.quote_side.fees.hlp_funding_interest_growth_remainder_scaled > 0);
    market.base_side.fees.hlp_funding_interest_growth_remainder_scaled = 17;

    // The 1_000 ordinary shares exit, leaving only MIN_LIQUIDITY outside the
    // two hLP vaults. Both source carries must end with that departing cohort.
    market.remove_liquidity(1_000).unwrap();
    assert_eq!(market.base_side.shares.ylp_supply, 3_000);
    assert_eq!(
        market.base_side.shares.ylp_supply
            - market.base_hlp_vault.ylp_shares
            - market.quote_hlp_vault.ylp_shares,
        MIN_LIQUIDITY
    );
    assert_eq!(market.base_side.fees.hlp_funding_interest_growth_remainder_scaled, 0);
    assert_eq!(market.quote_side.fees.hlp_funding_interest_growth_remainder_scaled, 0);

    let deposit = market.add_liquidity(1_000, 1_000).unwrap();
    let next_holder_checkpoint = market.quote_side.fees.interest_growth_index_q64;
    let second_eligibility = HlpYieldEligibility {
        ylp_supply: market.base_side.shares.ylp_supply,
        base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
        quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
    };
    let (expected_next_growth, expected_next_carry) = distribute_growth_q64(137, 2_000, 0).unwrap();
    record_inline_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        137,
        0,
        ProtocolAuctionSplit::default(),
        second_eligibility,
    )
    .unwrap();

    assert_eq!(
        market.quote_side.fees.interest_growth_index_q64 - next_holder_checkpoint,
        expected_next_growth
    );
    assert_eq!(
        market.quote_side.fees.hlp_funding_interest_growth_remainder_scaled,
        expected_next_carry
    );
    assert_eq!(
        accrue_fee_liability_with_remainder(
            deposit.ylp_amount,
            market.quote_side.fees.interest_growth_index_q64,
            next_holder_checkpoint,
            0,
        )
        .unwrap(),
        accrue_fee_liability_with_remainder(deposit.ylp_amount, expected_next_growth, 0, 0).unwrap()
    );
}

#[test]
fn partial_direct_burn_checkpoints_old_supply_before_reconciling() {
    let mut market = Market::default();
    market.base_side.shares.ylp_supply = 10;
    market.quote_side.shares.ylp_supply = 10;
    market.base_side.fees.unallocated_swap_fee_liability = 10;
    market.base_hlp_vault.ylp_shares = 10;
    market.base_hlp_vault.hlp_supply = 10;

    reconcile_live_hlp_supply(&mut market, MarketAsset::Base, 5).unwrap();

    assert_eq!(market.base_hlp_vault.hlp_supply, 5);
    assert_eq!(
        market.base_hlp_vault.base_swap_fee_growth_index_q64,
        YIELD_GROWTH_SCALE_Q64
    );
    assert_eq!(market.base_hlp_vault.unallocated_base_swap_fee_amount, 0);
}

#[test]
fn direct_burn_reconciliation_rejects_external_minting_and_all_burn_zombie() {
    let mut market = Market::default();
    market.quote_hlp_vault.hlp_supply = 1;

    for invalid_live_supply in [0, 2] {
        let error = reconcile_live_hlp_supply(&mut market, MarketAsset::Quote, invalid_live_supply).unwrap_err();
        match error {
            anchor_lang::error::Error::AnchorError(error) => {
                assert_eq!(error.error_name, "InvalidHlpMintSupply")
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(market.quote_hlp_vault.hlp_supply, 1);
    }
}

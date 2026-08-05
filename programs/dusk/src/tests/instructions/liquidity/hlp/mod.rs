use super::*;
use crate::{constants::YIELD_GROWTH_SCALE_Q64, state::accrue_fee_liability_with_remainder};
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
    let mut borrowed_side = MarketSide::default();
    let eligible_ylp_supply = borrowed_side.shares.ylp_supply;

    record_hlp_interest_credit(
        &mut borrowed_side,
        actual_interest_credit,
        0,
        0,
        ProtocolAuctionSplit::default(),
        eligible_ylp_supply,
    )
    .unwrap();

    assert!(actual_interest_credit < gross_interest_paid);
    assert_eq!(actual_interest_credit, 9_700);
    assert_eq!(borrowed_side.fees.interest_vault_balance, actual_interest_credit);
    assert_eq!(
        borrowed_side.fees.unallocated_interest_liability,
        actual_interest_credit
    );
    borrowed_side.fees.assert_backed().unwrap();
}

#[test]
fn inline_interest_uses_operation_start_ownership_for_spot_and_leverage() {
    let mut market = Market::default();
    market.base_side.shares.ylp_supply = 100;
    market.quote_side.shares.ylp_supply = 100;
    market.base_hlp_vault.ylp_shares = 50;
    market.base_hlp_vault.hlp_supply = 50;
    market.quote_hlp_vault.ylp_shares = 25;
    market.quote_hlp_vault.hlp_supply = 25;
    let eligibility = HlpYieldEligibility {
        ylp_supply: 100,
        base_hlp_ylp_shares: 50,
        quote_hlp_ylp_shares: 25,
    };

    // Model a same-operation burn from base-hLP and mint into quote-hLP.
    // Neither mutation may alter who earned the already-accrued interest.
    market.base_side.shares.ylp_supply = 75;
    market.quote_side.shares.ylp_supply = 75;
    market.base_hlp_vault.ylp_shares = 0;
    market.quote_hlp_vault.ylp_shares = 50;
    record_inline_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        10,
        0,
        0,
        ProtocolAuctionSplit::default(),
        eligibility,
    )
    .unwrap();

    assert_eq!(market.quote_side.fees.interest_liability, 10);
    assert_eq!(market.quote_side.fees.unallocated_interest_liability, 0);
    let global_growth = market.quote_side.fees.interest_growth_index_q64;
    let (base_hlp_whole, base_hlp_remainder) = accrue_fee_liability_with_remainder(50, global_growth, 0, 0).unwrap();
    assert_eq!(base_hlp_whole, 4);
    assert_eq!(market.base_hlp_vault.quote_interest_remainder_q64, base_hlp_remainder);
    assert_eq!(
        market.base_hlp_vault.quote_interest_growth_index_q64,
        crate::state::distribute_growth_q64(base_hlp_whole, 50, 0).unwrap().0
    );
    // Quote-hLP receives only its 25 start shares, not its 50 post-mint
    // shares: two whole atoms plus one-half atom at the yLP layer.
    let (quote_hlp_whole, quote_hlp_remainder) = accrue_fee_liability_with_remainder(25, global_growth, 0, 0).unwrap();
    assert_eq!(quote_hlp_whole, 2);
    assert_eq!(market.quote_hlp_vault.quote_interest_remainder_q64, quote_hlp_remainder);
    let (ordinary_amount, ordinary_remainder) =
        accrue_fee_liability_with_remainder(25, market.quote_side.fees.interest_growth_index_q64, 0, 0).unwrap();
    assert_eq!(ordinary_amount, 2);
    assert_eq!(ordinary_remainder, quote_hlp_remainder);
    market.quote_side.fees.assert_backed().unwrap();
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

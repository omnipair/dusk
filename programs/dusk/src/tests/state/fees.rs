use super::*;
use crate::constants::YIELD_GROWTH_SCALE_Q64;

#[test]
fn fee_accrual_uses_growth_delta() {
    let fees = accrue_fee_liability(
        1_000_000,
        3 * YIELD_GROWTH_SCALE_Q64,
        YIELD_GROWTH_SCALE_Q64,
    )
    .unwrap();
    assert_eq!(fees, 2_000_000);
}

#[test]
fn fee_accrual_carries_fraction_across_split_checkpoints() {
    let (first_amount, first_remainder) =
        accrue_fee_liability_with_remainder(1, YIELD_GROWTH_SCALE_Q64 / 2, 0, 0).unwrap();
    let (second_amount, second_remainder) = accrue_fee_liability_with_remainder(
        1,
        YIELD_GROWTH_SCALE_Q64,
        YIELD_GROWTH_SCALE_Q64 / 2,
        first_remainder,
    )
    .unwrap();

    assert_eq!(first_amount, 0);
    assert_eq!(first_remainder, 1_u64 << 63);
    assert_eq!(second_amount, 1);
    assert_eq!(second_remainder, 0);
}

#[test]
fn fragmented_balances_preserve_double_floor_dust_for_later_growth() {
    let first_index = 9 * YIELD_GROWTH_SCALE_Q64 / 10;
    let second_index = YIELD_GROWTH_SCALE_Q64;
    let (left_first, left_remainder) =
        accrue_fee_liability_with_remainder(5, first_index, 0, 0).unwrap();
    let (right_first, right_remainder) =
        accrue_fee_liability_with_remainder(5, first_index, 0, 0).unwrap();
    assert_eq!(left_first + right_first, 8);
    assert_eq!(
        left_remainder as u128 + right_remainder as u128,
        YIELD_GROWTH_SCALE_Q64 - 4
    );

    let (left_second, left_final_remainder) =
        accrue_fee_liability_with_remainder(5, second_index, first_index, left_remainder).unwrap();
    let (right_second, right_final_remainder) =
        accrue_fee_liability_with_remainder(5, second_index, first_index, right_remainder).unwrap();

    assert_eq!(left_second + right_second, 2);
    assert_eq!(left_final_remainder, 0);
    assert_eq!(right_final_remainder, 0);
    assert_eq!(left_first + right_first + left_second + right_second, 10);
}

#[test]
fn total_liability_includes_manager_fee_buckets() {
    let mut fees = Fees {
        swap_fee_custody_balance: 700,
        interest_vault_balance: 300,
        manager_swap_fee_liability: 400,
        manager_interest_fee_liability: 100,
        referral_interest_liability: 50,
        swap_protocol_fee_liability: 250,
        swap_buyback_fee_liability: 50,
        ..Fees::default()
    };

    assert_eq!(fees.total_liability().unwrap(), 850);
    fees.manager_swap_fee_liability = 0;
    fees.manager_interest_fee_liability = 0;
    assert_eq!(fees.total_liability().unwrap(), 350);
}

#[test]
fn auction_liabilities_settle_by_lane() {
    let mut fees = Fees {
        swap_fee_custody_balance: 700,
        interest_vault_balance: 300,
        swap_protocol_fee_liability: 500,
        swap_buyback_fee_liability: 200,
        interest_protocol_fee_liability: 200,
        interest_buyback_fee_liability: 100,
        ..Fees::default()
    };

    fees.settle_protocol_auction_liability(ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap, 125, 10)
        .unwrap();
    fees.settle_protocol_auction_liability(
        ProtocolAuctionLane::Buyback,
        ProtocolRevenueSource::Interest,
        50,
        20,
    )
        .unwrap();

    assert_eq!(fees.swap_protocol_fee_liability, 375);
    assert_eq!(fees.swap_buyback_fee_liability, 200);
    assert_eq!(fees.interest_protocol_fee_liability, 200);
    assert_eq!(fees.interest_buyback_fee_liability, 50);
    assert_eq!(fees.swap_fee_custody_balance, 575);
    assert_eq!(fees.interest_vault_balance, 250);
    fees.assert_backed().unwrap();
}

#[test]
fn protocol_auction_epoch_isolated_by_market_lane_and_source() {
    let mut first_market = Fees {
        swap_fee_custody_balance: 200,
        interest_vault_balance: 100,
        swap_protocol_fee_liability: 100,
        interest_protocol_fee_liability: 50,
        swap_buyback_fee_liability: 100,
        interest_buyback_fee_liability: 50,
        ..Fees::default()
    };
    let second_market = first_market;

    let first_epoch = first_market.protocol_auction_epoch(
        ProtocolAuctionLane::Fee,
        ProtocolRevenueSource::Swap,
        10,
    );
    first_market
        .settle_protocol_auction_liability(
            ProtocolAuctionLane::Fee,
            ProtocolRevenueSource::Swap,
            1,
            first_epoch.start_slot,
        )
        .unwrap();

    assert_eq!(first_market.fee_swap_auction_epoch.start_slot, 10);
    assert_eq!(first_market.fee_swap_auction_epoch.tracked_inventory, 99);
    assert_eq!(first_market.fee_interest_auction_epoch, ProtocolAuctionEpoch::default());
    assert_eq!(first_market.buyback_swap_auction_epoch, ProtocolAuctionEpoch::default());
    assert_eq!(second_market.fee_swap_auction_epoch, ProtocolAuctionEpoch::default());
}

#[test]
fn partial_fill_preserves_epoch_but_new_inventory_starts_a_new_one() {
    let mut fees = Fees {
        swap_fee_custody_balance: 100,
        swap_protocol_fee_liability: 100,
        ..Fees::default()
    };
    let initial = fees.protocol_auction_epoch(ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap, 10);
    fees.settle_protocol_auction_liability(
        ProtocolAuctionLane::Fee,
        ProtocolRevenueSource::Swap,
        99,
        initial.start_slot,
    )
    .unwrap();

    let old_inventory = fees.protocol_auction_epoch(ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap, 1_000);
    assert_eq!(old_inventory.start_slot, 10);
    assert_eq!(old_inventory.tracked_inventory, 1);

    fees.swap_protocol_fee_liability = 101;
    fees.swap_fee_custody_balance = 101;
    let replenished = fees.protocol_auction_epoch(ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap, 1_000);
    assert_eq!(replenished.start_slot, 1_000);
    assert_eq!(replenished.tracked_inventory, 101);
}

#[test]
fn exhausting_inventory_clears_only_its_epoch() {
    let mut fees = Fees {
        swap_fee_custody_balance: 2,
        swap_protocol_fee_liability: 1,
        swap_buyback_fee_liability: 1,
        fee_swap_auction_epoch: ProtocolAuctionEpoch {
            start_slot: 10,
            tracked_inventory: 1,
        },
        buyback_swap_auction_epoch: ProtocolAuctionEpoch {
            start_slot: 20,
            tracked_inventory: 1,
        },
        ..Fees::default()
    };

    fees.settle_protocol_auction_liability(ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap, 1, 10)
        .unwrap();

    assert_eq!(fees.fee_swap_auction_epoch, ProtocolAuctionEpoch::default());
    assert_eq!(
        fees.buyback_swap_auction_epoch,
        ProtocolAuctionEpoch {
            start_slot: 20,
            tracked_inventory: 1,
        }
    );
}

#[test]
fn governance_route_is_per_lane_and_resets_only_that_lanes_epochs() {
    let fee_route = Pubkey::new_unique();
    let buyback_route = Pubkey::new_unique();
    let mut fees = Fees {
        fee_swap_auction_epoch: ProtocolAuctionEpoch {
            start_slot: 10,
            tracked_inventory: 1,
        },
        buyback_interest_auction_epoch: ProtocolAuctionEpoch {
            start_slot: 20,
            tracked_inventory: 2,
        },
        ..Fees::default()
    };

    fees.set_protocol_auction_reference_market(ProtocolAuctionLane::Fee, fee_route);
    fees.set_protocol_auction_reference_market(ProtocolAuctionLane::Buyback, buyback_route);

    assert_eq!(
        fees.protocol_auction_reference_market(ProtocolAuctionLane::Fee),
        fee_route
    );
    assert_eq!(
        fees.protocol_auction_reference_market(ProtocolAuctionLane::Buyback),
        buyback_route
    );
    assert_eq!(fees.fee_swap_auction_epoch, ProtocolAuctionEpoch::default());
    assert_eq!(fees.buyback_interest_auction_epoch, ProtocolAuctionEpoch::default());
}

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
fn total_liability_includes_lp_protocol_and_referral_buckets() {
    let fees = Fees {
        swap_fee_custody_balance: 700,
        interest_vault_balance: 300,
        referral_interest_liability: 50,
        swap_protocol_fee_liability: 250,
        swap_buyback_fee_liability: 50,
        ..Fees::default()
    };

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

    fees.settle_protocol_auction_liability(ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap, 125)
        .unwrap();
    fees.settle_protocol_auction_liability(
        ProtocolAuctionLane::Buyback,
        ProtocolRevenueSource::Interest,
        50,
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
fn auction_curves_are_isolated_and_inventory_changes_do_not_restart_the_clock() {
    let curve = ProtocolAuctionState {
        accepted_mint: Pubkey::new_unique(),
        virtual_sold_reserve: 1_000,
        virtual_payment_reserve: 2_000,
        sold_rate_per_day: 100,
        last_update_slot: 0, // Slot zero is a valid configured start.
        stream_remainder: 1,
    };
    for lane in [ProtocolAuctionLane::Fee, ProtocolAuctionLane::Buyback] {
        for source in [ProtocolRevenueSource::Swap, ProtocolRevenueSource::Interest] {
            let mut fees = Fees {
                swap_fee_custody_balance: 1,
                interest_vault_balance: 1,
                swap_protocol_fee_liability: 1,
                interest_protocol_fee_liability: 1,
                ..Fees::default()
            };
            *fees.protocol_auction_mut(lane, source) = curve;
            let other_market = Fees::default();
            for other_lane in [ProtocolAuctionLane::Fee, ProtocolAuctionLane::Buyback] {
                for other_source in [ProtocolRevenueSource::Swap, ProtocolRevenueSource::Interest] {
                    let expected = if lane == other_lane && source == other_source { curve } else { ProtocolAuctionState::default() };
                    assert_eq!(fees.protocol_auction(other_lane, other_source), expected);
                    assert_eq!(other_market.protocol_auction(other_lane, other_source), ProtocolAuctionState::default());
                }
            }
            // Emptying custody and subsequent new fees preserve virtual state.
            fees.settle_protocol_auction_liability(ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap, 1).unwrap();
            fees.settle_protocol_auction_liability(ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest, 1).unwrap();
            fees.interest_vault_balance = 100;
            fees.credit_protocol_interest_revenue(100, ProtocolAuctionSplit::default(), 0).unwrap();
            assert_eq!(fees.protocol_auction(lane, source), curve);
            fees.assert_backed().unwrap();
        }
    }
}

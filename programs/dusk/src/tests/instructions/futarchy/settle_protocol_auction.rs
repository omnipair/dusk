use super::*;
use crate::state::ProtocolAuctionState;
use proptest::prelude::*;

fn curve(sold: u128, payment: u128, rate: u64) -> ProtocolAuctionState {
    ProtocolAuctionState {
        accepted_mint: Pubkey::new_unique(),
        virtual_sold_reserve: sold,
        virtual_payment_reserve: payment,
        sold_rate_per_day: rate,
        last_update_slot: 10,
        stream_remainder: 0,
    }
}

#[test]
fn first_purchase_decays_from_configuration_and_has_no_ema_floor() {
    let initial = curve(1_000, 1_000, 1_000);
    let day = MS_PER_DAY / TARGET_MS_PER_SLOT;
    let immediate = quote_protocol_auction_settlement(initial, 100, 10, 0).unwrap();
    let first = quote_protocol_auction_settlement(initial, 100, 10 + day, 0).unwrap();
    assert_eq!(immediate.payment_amount, 112);
    assert_eq!(first.payment_amount, 27);
    assert_eq!(first.next_state.virtual_sold_reserve, 1_900);
    assert_eq!(first.next_state.virtual_payment_reserve, 527);
    assert_eq!(first.next_state.last_update_slot, 10 + day);
    // Buying raises the next purchase's price at the same slot.
    let next = quote_protocol_auction_settlement(first.next_state, 100, 10 + day, 0).unwrap();
    assert_eq!(next.payment_amount, 30);
    // A quote is read-only; failed transactions cannot commit its new clock.
    assert_eq!(initial.last_update_slot, 10);
}

#[test]
fn fragmented_streaming_carries_fractional_atoms_across_purchases() {
    let day = MS_PER_DAY / TARGET_MS_PER_SLOT;
    let initial = curve(10_000, 10_000, 1);
    let first = quote_protocol_auction_settlement(initial, 1, 10 + day - 1, 0).unwrap();
    assert_eq!(first.next_state.virtual_sold_reserve, 9_999);
    assert_eq!(first.next_state.stream_remainder, day - 1);
    let second = quote_protocol_auction_settlement(first.next_state, 1, 10 + day, 0).unwrap();
    assert_eq!(second.next_state.virtual_sold_reserve, 9_999);
    assert_eq!(second.next_state.stream_remainder, 0);
}

#[test]
fn payment_rounds_up_in_raw_mint_units_and_splits_without_loss() {
    for (sold_reserve, payment_reserve, sold, payment) in [
        (1_000_000_000, 1_000_000, 1, 1), // 9-decimal sold, 6-decimal payment.
        (1_000_000, 1_000_000_000, 1, 1_001), // Reverse decimal direction.
        (u64::MAX as u128, 1, 1, 1), // A positive sale can never be free.
        (200, 101, 100, 101),
    ] {
        let quote = quote_protocol_auction_settlement(curve(sold_reserve, payment_reserve, 0), sold, 10, 3_333).unwrap();
        assert_eq!(quote.payment_amount, payment);
        assert_eq!(quote.treasury_amount + quote.staking_vault_amount, payment);
    }
    let quote = quote_protocol_auction_settlement(curve(200, 101, 0), 100, 10, 3_333).unwrap();
    assert_eq!(quote.treasury_amount, 68);
    assert_eq!(quote.staking_vault_amount, 33);
}

#[test]
fn zero_streaming_still_sells_and_large_idle_intervals_remain_nonzero() {
    let static_curve = curve(1_000, 1_000, 0);
    assert_eq!(
        quote_protocol_auction_settlement(static_curve, 100, u64::MAX, 0).unwrap().payment_amount,
        quote_protocol_auction_settlement(static_curve, 100, 10, 0).unwrap().payment_amount,
    );
    let quote = quote_protocol_auction_settlement(curve(u64::MAX as u128, u64::MAX as u128, u64::MAX), 1, u64::MAX, 0).unwrap();
    assert_eq!(quote.payment_amount, 1);
    assert!(quote.next_state.virtual_sold_reserve > u64::MAX as u128);
    assert!(quote.next_state.virtual_payment_reserve > 0);
}

#[test]
fn invalid_clock_unconfigured_curve_and_virtual_exhaustion_are_rejected() {
    let initial = curve(100, 100, 0);
    for amount in [100, 101] {
        assert_eq!(quote_protocol_auction_settlement(initial, amount, 10, 0).unwrap_err(), error!(ErrorCode::InsufficientAmount));
    }
    assert_eq!(quote_protocol_auction_settlement(initial, 0, 10, 0).unwrap_err(), error!(ErrorCode::AmountZero));
    assert_eq!(quote_protocol_auction_settlement(initial, 1, 9, 0).unwrap_err(), error!(ErrorCode::InvalidAuctionClock));
    assert_eq!(quote_protocol_auction_settlement(ProtocolAuctionState::default(), 1, 10, 0).unwrap_err(), error!(ErrorCode::InvalidAuctionConfig));
    assert_eq!(quote_protocol_auction_settlement(initial, 1, 10, BPS_DENOMINATOR + 1).unwrap_err(), error!(ErrorCode::InvalidDistribution));
}

proptest! {
    #[test]
    fn continuous_curve_preserves_product_and_conserves_streamed_atoms(
        sold in 2_u64..1_000_000_000,
        payment in 1_u64..1_000_000_000,
        rate in 0_u64..1_000_000_000,
        elapsed in 0_u64..10_000_000,
        buy_bps in 1_u64..10_000,
        share in 0_u16..=10_000,
    ) {
        let initial = curve(sold as u128, payment as u128, rate);
        let day = (MS_PER_DAY / TARGET_MS_PER_SLOT) as u128;
        let streamed = rate as u128 * elapsed as u128;
        let available_virtual = sold as u128 + streamed / day;
        let amount = ((available_virtual * buy_bps as u128 / 10_000).max(1)) as u64;
        let quote = quote_protocol_auction_settlement(initial, amount, 10 + elapsed, share).unwrap();
        prop_assert_eq!(quote.next_state.virtual_sold_reserve + amount as u128, available_virtual);
        prop_assert_eq!(quote.next_state.stream_remainder as u128, streamed % day);
        prop_assert!(quote.payment_amount > 0);
        prop_assert_eq!(quote.treasury_amount + quote.staking_vault_amount, quote.payment_amount);
        let decayed_payment = quote.next_state.virtual_payment_reserve - quote.payment_amount as u128;
        prop_assert!(decayed_payment * available_virtual >= sold as u128 * payment as u128);
        prop_assert!(quote.next_state.virtual_payment_reserve * quote.next_state.virtual_sold_reserve >= decayed_payment * available_virtual);
        // One fewer payment atom would violate the purchase invariant.
        prop_assert!((decayed_payment + quote.payment_amount as u128 - 1) * quote.next_state.virtual_sold_reserve < decayed_payment * available_virtual);
    }
}

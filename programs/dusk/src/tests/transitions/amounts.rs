use crate::{constants::NAD, math::normalize_to_nad, state::MarketSide};
use proptest::prelude::*;

fn market_with_decimals(base: u8, quote: u8) -> Market {
    Market {
        base_side: MarketSide { asset_decimals: base, ..MarketSide::default() },
        quote_side: MarketSide { asset_decimals: quote, ..MarketSide::default() },
        ..Market::default()
    }
}

#[test]
fn common_precision_keeps_sub_nad_atoms_and_price_ratios() {
    for (base, quote) in [(10, 9), (12, 6), (6, 12), (18, 9), (18, 18), (255, 255)] {
        let market = market_with_decimals(base, quote);
        for decimals in [base, quote] {
            for raw in [0, 1, 9, 999, 1_001, u64::MAX] {
                let scaled = market.normalize_amount(u128::from(raw), decimals).unwrap();
                assert_eq!(scaled == 0, raw == 0);
                assert_eq!(market.denormalize_amount_floor(scaled, decimals).unwrap(), raw);
                assert_eq!(market.denormalize_amount_ceil(scaled, decimals).unwrap(), raw);
            }
        }
    }
    let market = market_with_decimals(12, 6);
    let base = market.normalize_amount(1_000_000_000_000, 12).unwrap();
    let quote = market.normalize_amount(2_000_000, 6).unwrap();
    assert_eq!(quote * u128::from(NAD) / base, 2 * u128::from(NAD));
    assert_eq!(market.denormalize_amount_floor(999_999, 6).unwrap(), 0);
    assert_eq!(market.denormalize_amount_ceil(999_999, 6).unwrap(), 1);
}

#[test]
fn existing_market_precision_is_unchanged() {
    for base in 0..=9 {
        for quote in 0..=9 {
            let market = market_with_decimals(base, quote);
            for decimals in [base, quote] {
                for amount in [0, 1, 1_001, u128::from(u64::MAX)] {
                    assert_eq!(market.normalize_amount(amount, decimals).unwrap(), normalize_to_nad(amount, decimals).unwrap());
                }
            }
        }
    }
}

#[test]
fn scale_boundaries_return_errors_without_losing_nonzero_quantities() {
    assert!(rescale_amount(u128::MAX, 9, 10, false).is_err());
    assert!(rescale_amount(1, 0, 255, false).is_err());
    assert_eq!(rescale_amount(0, 0, 255, false).unwrap(), 0);
    assert_eq!(rescale_amount(u128::MAX, 255, 0, false).unwrap(), 0);
    assert_eq!(rescale_amount(u128::MAX, 255, 0, true).unwrap(), 1);
    assert_eq!(rescale_amount(u128::MAX, 10, 9, true).unwrap(), u128::MAX / 10 + 1);
}

proptest! {
    #[test]
    fn normalization_roundtrips_every_raw_amount(raw in any::<u64>(), base in 0_u8..=18, quote in 0_u8..=18) {
        let market = market_with_decimals(base, quote);
        for decimals in [base, quote] {
            let scaled = market.normalize_amount(u128::from(raw), decimals).unwrap();
            prop_assert_eq!(market.denormalize_amount_floor(scaled, decimals).unwrap(), raw);
            prop_assert_eq!(market.denormalize_amount_ceil(scaled, decimals).unwrap(), raw);
        }
    }
}

#[test]
fn high_decimal_auction_payment_rounds_up_and_cannot_be_free() {
    use crate::transitions::revenue::quote_protocol_auction_settlement;
    for (sold, accepted, price, expected) in [(12, 12, NAD, 1), (12, 6, NAD, 1), (6, 12, NAD, 1_000_000), (18, 18, 1, 1), (255, 255, NAD, 1)] {
        let quote = quote_protocol_auction_settlement(1, sold, accepted, price, 10_000, 10_000, 0, 100, 0).unwrap();
        assert_eq!(quote.payment_amount, expected);
    }
}

#[test]
fn auction_rejects_a_discounted_price_that_rounds_to_zero() {
    use crate::transitions::revenue::quote_protocol_auction_settlement;
    // A positive reference does not guarantee a positive discounted price.
    // At expiry, 1 NAD atom * 50% rounds to zero and must not sell custody free.
    for decimals in [6, 12, 18] {
        let result = quote_protocol_auction_settlement(1_000, decimals, decimals, 1, 10_000, 5_000, 100, 100, 0);
        assert!(matches!(result, Err(anchor_lang::error::Error::AnchorError(error)) if error.error_name == "InvalidSettlementPrice"));
    }
}

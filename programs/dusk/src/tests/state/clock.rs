use super::*;
use crate::{constants::*, state::*};

fn at(slot: u64, seconds: i64) -> MarketClock {
    MarketClock::from_timestamp(slot, seconds).unwrap()
}

#[test]
fn clock_preserves_seconds_and_rejects_invalid_conversion() {
    assert_eq!(at(25, 10).timestamp_ms, 10_000);
    assert!(MarketClock::from_timestamp(1, -1).is_err());
    assert!(MarketClock::from_timestamp(1, i64::MAX).is_err());
    assert_eq!(at(500, 10).elapsed_ms(25, 10_000).unwrap(), 0);
    assert_eq!(at(25, 11).elapsed_ms(25, 10_000).unwrap(), 0);
    assert!(at(24, 11).elapsed_ms(25, 10_000).is_err());
    assert!(at(26, 9).elapsed_ms(25, 10_000).is_err());
}

#[test]
fn amm_half_lives_follow_elapsed_time_at_any_slot_cadence() {
    let config = AmmConfig { center_ema_half_life_ms: 60_000, volatility_half_life_ms: 60_000, ..AmmConfig::default() };
    let mut initial = AmmState::initialize(&config, NAD, NAD as u128, at(100, 10)).unwrap();
    initial.last_trade_price_nad = 2 * NAD;
    initial.volatility_accumulator_nad = NAD;
    for slot_delta in [1, 10, 20, 10_000] {
        let mut state = initial;
        state.observe_clock_from_validated_config(&config, at(100 + slot_delta, 70)).unwrap();
        assert!(state.price_ema_nad.abs_diff(3 * NAD / 2) <= 1_000);
        assert!(state.volatility_accumulator_nad.abs_diff(NAD / 2) <= 1_000);
    }
    let mut repeated = initial;
    repeated.observe_clock_from_validated_config(&config, at(200, 10)).unwrap();
    assert_eq!(repeated.price_ema_nad, initial.price_ema_nad);
    assert_eq!(repeated.volatility_accumulator_nad, NAD);
    repeated.observe_clock_from_validated_config(&config, at(201, 70)).unwrap();
    assert!(repeated.price_ema_nad.abs_diff(3 * NAD / 2) <= 1_000);
    let snapshot = repeated;
    assert!(repeated.observe_clock_from_validated_config(&config, at(202, 69)).is_err());
    assert_eq!(repeated, snapshot);
}

#[test]
fn risk_ema_and_depth_follow_time_and_keep_same_slot_ordering() {
    let config = MarketConfig { ema_half_life_ms: 60_000, directional_ema_half_life_ms: 60_000, curve_depth_ema_half_life_ms: 60_000, ..MarketConfig::default() };
    let initial = Risk {
        base_price_ema_nad: NAD,
        quote_price_ema_nad: NAD,
        directional_base_price_ema_nad: NAD,
        directional_quote_price_ema_nad: NAD,
        cached_spot_base_price_nad: 2 * NAD,
        cached_spot_quote_price_nad: 2 * NAD,
        curve_depth_ema_nad: NAD as u128,
        observed_curve_depth_nad: 2 * NAD as u128,
        last_snapshot_slot: 100,
        last_snapshot_timestamp_ms: 10_000,
    };
    for next_slot in [110, 120] {
        let result = initial.refreshed(2 * NAD, 2 * NAD, 2 * NAD as u128, &config, at(next_slot, 70)).unwrap();
        assert!(result.base_price_ema_nad.abs_diff(3 * NAD / 2) <= 1_000);
        assert!(result.directional_base_price_ema_nad.abs_diff(3 * NAD / 2) <= 1_000);
        assert!(result.curve_depth_ema_nad.abs_diff(3 * NAD as u128 / 2) <= 1_000);
    }
    for clock in [at(101, 10), at(100, 14)] {
        let result = initial.refreshed(2 * NAD, 2 * NAD, 2 * NAD as u128, &config, clock).unwrap();
        assert_eq!(result.base_price_ema_nad, NAD);
        assert_eq!(result.last_snapshot_timestamp_ms, 10_000);
    }
}

#[test]
fn daily_refill_uses_twenty_four_hours_instead_of_slot_count() {
    let initial = DailyBorrowBucket { borrowed_bucket: 100_003, last_decay_slot: 1, last_decay_timestamp_ms: 10_000, decay_remainder_ms: 0 };
    let mut repeated = initial;
    repeated.decay_to_clock(100_003, at(1_000_000, 10)).unwrap();
    assert_eq!(repeated.borrowed_bucket, initial.borrowed_bucket);
    for slot in [2, 216_001, 432_001] {
        let mut half = initial;
        half.decay_to_clock(100_003, at(slot, 43_210)).unwrap();
        assert_eq!(half.borrowed_bucket, 50_002);
        half.decay_to_clock(100_003, at(slot + 1, 86_410)).unwrap();
        assert_eq!(half.borrowed_bucket, 0);
        assert_eq!(half.decay_remainder_ms, 0);
    }
    let mut same_slot = initial;
    same_slot.decay_to_clock(100_003, at(1, 86_410)).unwrap();
    assert_eq!(same_slot.borrowed_bucket, initial.borrowed_bucket);
    assert_eq!(same_slot.last_decay_timestamp_ms, 10_000);
    assert!(same_slot.decay_to_clock(100_003, at(2, 9)).is_err());
}

#[test]
fn insurance_window_can_start_at_zero_and_resets_only_after_a_day() {
    let mut insurance = Insurance { base_available: 1_000, ..Insurance::default() };
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, at(0, 0)).unwrap(), 500);
    insurance.base_draw_window.drawn = 500;
    insurance.base_available = 500;
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, at(1_000_000, 0)).unwrap(), 0);
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, at(1_000_001, 86_399)).unwrap(), 0);
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, at(1_000_002, 86_400)).unwrap(), 250);
    assert_eq!(insurance.base_draw_window.start_timestamp_ms, MS_PER_DAY);
    assert!(insurance.draw_capacity(MarketAsset::Base, at(1_000_003, 86_399)).is_err());
}

#[test]
fn risk_reciprocal_preserves_wide_integer_floor_at_price_extremes() {
    for price in [1, 3, NAD / 2, NAD, 2 * NAD, NAD * NAD, u64::MAX] {
        let mut market = Market::default();
        let expected = u128::from(NAD).pow(2) / u128::from(price);
        let result = market.observe_risk_from_concentrated_curve(price, u128::from(NAD), at(1, 1));
        if expected == 0 {
            assert!(result.is_err());
        } else {
            result.unwrap();
            assert_eq!(u128::from(market.risk.cached_spot_quote_price_nad), expected);
        }
    }
}

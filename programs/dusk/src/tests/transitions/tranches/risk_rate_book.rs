use super::*;
use crate::constants::TARGET_MS_PER_SLOT;

#[test]
fn governance_change_settles_the_old_model_and_does_not_reset_apr() {
    let old = RiskRateConfig::default();
    let mut book = RiskRateBook::new(old, 100).unwrap();
    let next = RiskRateConfig {
        initial_apr_nad: 100_000_000,
        half_life_ms: old.half_life_ms / 2,
        ..old
    };
    let now = 100 + old.half_life_ms / TARGET_MS_PER_SLOT;
    let accruals = book.replace_config(next, [Some(0); 2], now).unwrap();
    assert_eq!(book.sides[0].apr_e18, 8 * RATE_SCALE / 100);
    assert_eq!(book.sides[1].apr_e18, 8 * RATE_SCALE / 100);
    assert_eq!(accruals[0], accruals[1]);
    book.accrue([Some(0); 2], now + next.half_life_ms / TARGET_MS_PER_SLOT)
        .unwrap();
    assert_eq!(book.sides[0].apr_e18, 16 * RATE_SCALE / 100);
}

#[test]
fn second_asset_index_overflow_cannot_partially_advance_the_first() {
    let config = RiskRateConfig::default();
    let mut book = RiskRateBook::new(config, 100).unwrap();
    book.sides[1].borrow_index_nad = u128::MAX;
    let original = book;
    assert!(book
        .accrue([Some(0); 2], 100 + config.half_life_ms / TARGET_MS_PER_SLOT)
        .is_err());
    assert_eq!(book, original);
}

#[test]
fn invalid_updates_and_backward_time_leave_state_unchanged() {
    let config = RiskRateConfig::default();
    let mut book = RiskRateBook::new(config, 100).unwrap();
    let original = book;
    assert!(book
        .replace_config(
            RiskRateConfig {
                half_life_ms: 0,
                ..config
            },
            [Some(0); 2],
            1_000
        )
        .is_err());
    assert_eq!(book, original);
    assert!(book.accrue([Some(0); 2], 99).is_err());
    assert_eq!(book, original);
}

#[test]
fn asymmetric_signals_are_applied_to_the_matching_asset() {
    let config = RiskRateConfig::default();
    let mut book = RiskRateBook::new(config, 100).unwrap();
    book.accrue(
        [Some(0), Some(NAD as u128)],
        100 + config.half_life_ms / TARGET_MS_PER_SLOT,
    )
    .unwrap();
    assert_eq!(book.sides[0].apr_e18, 8 * RATE_SCALE / 100);
    assert_eq!(book.sides[1].apr_e18, 2 * RATE_SCALE / 100);
    assert!(book.sides[0].borrow_index_nad > book.sides[1].borrow_index_nad);
}

#[test]
fn debt_index_keeps_sub_atom_interest_and_wide_products() {
    let mut side = RiskRateSide {
        apr_e18: 0,
        borrow_index_nad: u128::MAX / 2,
        borrow_index_remainder: 1,
    };
    let denominator = NAD as u128 * MS_PER_YEAR as u128;
    side.apply(RateAccrual {
        end_apr_e18: 100,
        integrated_apr_nad_ms: denominator / 4,
    })
    .unwrap();
    assert_eq!(side.borrow_index_nad, (u128::MAX / 2) + (u128::MAX / 2) / 4);
    let mut tiny = RiskRateSide {
        apr_e18: 0,
        borrow_index_nad: 1,
        borrow_index_remainder: denominator - 1,
    };
    tiny.apply(RateAccrual {
        end_apr_e18: 100,
        integrated_apr_nad_ms: 1,
    })
    .unwrap();
    assert_eq!(tiny.borrow_index_nad, 2);
    assert_eq!(tiny.borrow_index_remainder, 0);
}

#[test]
fn idle_market_holds_rate_without_compounding_an_empty_debt_index() {
    let config = RiskRateConfig::default();
    let mut book = RiskRateBook::new(config, 100).unwrap();
    book.sides[0].borrow_index_remainder = 99;
    let original = book.sides[0];
    book.accrue([None, None], 100 + config.half_life_ms / TARGET_MS_PER_SLOT)
        .unwrap();
    assert_eq!(book.sides[0].apr_e18, original.apr_e18);
    assert_eq!(book.sides[0].borrow_index_nad, original.borrow_index_nad);
    assert_eq!(book.sides[0].borrow_index_remainder, 0);
}

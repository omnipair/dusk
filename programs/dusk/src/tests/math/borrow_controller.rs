use super::*;
use crate::state::RiskRateOverrides;
use proptest::prelude::*;

fn rate(apr: f64) -> u128 {
    (apr * RATE_SCALE as f64).round() as u128
}
fn input(signal_nad: Option<u128>, utilization_bps: u64) -> RiskRateInput {
    RiskRateInput {
        signal_nad,
        utilization_bps,
    }
}
fn assert_close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual {actual}, expected {expected}, tolerance {tolerance}"
    );
}

#[test]
fn market_defaults_and_explicit_zero_overrides() {
    let defaults = RiskRateOverrides::default().resolve().unwrap();
    assert_eq!(defaults, RiskRateConfig::default());
    assert_eq!(defaults.half_life_ms, 14 * crate::constants::MS_PER_DAY);
    assert_eq!(defaults.initial_apr_nad, 40_000_000);
    assert_eq!(MINIMUM_BORROW_APR_NAD, 10_000_000);
    assert_eq!(defaults.lower_signal_nad, 50_000_000);
    assert_eq!(defaults.upper_signal_nad, 62_500_000);
    assert_eq!(defaults.lower_utilization_bps, 7_500);
    assert_eq!(defaults.upper_utilization_bps, 8_000);
    assert!(RiskRateOverrides {
        initial_apr_nad: Some(0),
        ..Default::default()
    }
    .resolve()
    .is_err());
    assert!(RiskRateOverrides {
        half_life_ms: Some(0),
        ..Default::default()
    }
    .resolve()
    .is_err());
    assert!(RiskRateOverrides {
        lower_signal_nad: Some(0),
        ..Default::default()
    }
    .resolve()
    .is_err());
}

#[test]
fn rates_double_hold_and_halve_with_inclusive_band() {
    let config = RiskRateConfig::default();
    let start = rate(0.04);
    let up = accrue_risk_rate(&config, start, input(Some(0), 0), config.half_life_ms).unwrap();
    assert_eq!(up.end_apr_e18, rate(0.08));
    assert_close(
        up.integrated_apr_nad_ms as f64,
        0.04 * NAD as f64 * config.half_life_ms as f64 / 2_f64.ln(),
        1.0,
    );
    for signal in [
        None,
        Some(config.lower_signal_nad as u128),
        Some(config.upper_signal_nad as u128),
    ] {
        let flat = accrue_risk_rate(&config, start, input(signal, 7_750), config.half_life_ms).unwrap();
        assert_eq!(flat.end_apr_e18, start);
        assert_eq!(flat.integrated_apr_nad_ms, 40_000_000 * config.half_life_ms as u128);
    }
    let down = accrue_risk_rate(&config, start, input(Some(NAD as u128), 0), config.half_life_ms).unwrap();
    assert_eq!(down.end_apr_e18, rate(0.02));
}

#[test]
fn fixed_floor_integrates_tail_and_upward_rate_has_no_economic_cap() {
    let config = RiskRateConfig::default();
    let elapsed = 3 * config.half_life_ms;
    let up = accrue_risk_rate(&config, rate(0.04), input(Some(0), 0), elapsed).unwrap();
    let down = accrue_risk_rate(&config, rate(0.04), input(Some(NAD as u128), 0), elapsed).unwrap();
    let scale = NAD as f64 * config.half_life_ms as f64;
    assert_eq!(up.end_apr_e18, rate(0.32));
    assert_eq!(down.end_apr_e18, rate(0.01));
    assert_close(up.integrated_apr_nad_ms as f64 / scale, 0.28 / 2_f64.ln(), 1e-15);
    assert_close(down.integrated_apr_nad_ms as f64 / scale, 0.03 / 2_f64.ln() + 0.01, 1e-15);
}

#[test]
fn initial_rate_and_half_life_cannot_bypass_fixed_policy() {
    for apr in [0, MINIMUM_BORROW_APR_NAD - 1] {
        assert!(RiskRateOverrides { initial_apr_nad: Some(apr), ..Default::default() }.resolve().is_err());
    }
    for half in [0, crate::constants::MS_PER_DAY / 2 - 1, 30 * crate::constants::MS_PER_DAY + 1] {
        assert!(RiskRateOverrides { half_life_ms: Some(half), ..Default::default() }.resolve().is_err());
    }
    for half in [crate::constants::MS_PER_DAY / 2, 30 * crate::constants::MS_PER_DAY] {
        assert!(RiskRateOverrides { half_life_ms: Some(half), ..Default::default() }.resolve().is_ok());
    }
}

#[test]
fn fourteen_and_thirty_day_half_lives_do_not_truncate_to_zero() {
    for days in [1, 3, 7, 14, 30] {
        let config = RiskRateConfig {
            half_life_ms: days * crate::constants::MS_PER_DAY,
            ..Default::default()
        };
        let result = accrue_risk_rate(&config, rate(0.04), input(Some(0), 0), config.half_life_ms).unwrap();
        assert_eq!(result.end_apr_e18, rate(0.08));
    }
}

#[test]
fn senior_withdrawal_changes_liquidity_signal_without_inventing_coverage() {
    assert_eq!(coverage_signal_nad(20, 80, 8_000).unwrap(), Some(50_000_000));
    // Debt 80, cash 10: floor utilization to 8,888 bps in the caller.
    assert_eq!(coverage_signal_nad(20, 80, 8_888).unwrap(), Some(27_800_000));
    assert_eq!(coverage_signal_nad(0, 80, 8_000).unwrap(), Some(0));
    assert_eq!(coverage_signal_nad(20, 0, 0).unwrap(), None);
    assert_eq!(coverage_signal_nad(u128::MAX, 1, 10_000).unwrap(), Some(0));
    assert!(coverage_signal_nad(20, 80, 10_001).is_err());
}

#[test]
fn frequent_accrual_cannot_freeze_a_slow_rate() {
    let config = RiskRateConfig {
        initial_apr_nad: MINIMUM_BORROW_APR_NAD,
        half_life_ms: 30 * crate::constants::MS_PER_DAY,
        ..Default::default()
    };
    let initial = MINIMUM_BORROW_APR_NAD as u128 * RATE_TO_NAD;
    let mut current = initial;
    for _ in 0..10_000 {
        current = accrue_risk_rate(&config, current, input(Some(0), 0), 200).unwrap().end_apr_e18;
    }
    let once = accrue_risk_rate(&config, initial, input(Some(0), 0), 2_000_000).unwrap();
    assert!(current > initial);
    assert!(once.end_apr_e18.abs_diff(current) <= 10_001);
}

#[test]
fn zero_time_and_numeric_overflow_do_not_invent_elapsed_interest() {
    let config = RiskRateConfig::default();
    let same = accrue_risk_rate(&config, 1, input(Some(0), 0), 0).unwrap();
    assert_eq!(
        same,
        RateAccrual {
            end_apr_e18: 1,
            integrated_apr_nad_ms: 0
        }
    );
    let clamped = accrue_risk_rate(&config, u128::MAX, input(Some(0), 0), config.half_life_ms).unwrap();
    assert_eq!(clamped.end_apr_e18, u64::MAX as u128 * RATE_TO_NAD);
    let stale = accrue_risk_rate(&config, rate(0.04), input(Some(0), 0), u64::MAX).unwrap();
    assert_eq!(stale.end_apr_e18, u64::MAX as u128 * RATE_TO_NAD);
    assert!(stale.integrated_apr_nad_ms > 0);
}

#[test]
fn valid_uncapped_market_remains_accruable_after_seventy_three_half_lives() {
    let config = RiskRateConfig {
        half_life_ms: crate::constants::MS_PER_DAY / 2,
        ..RiskRateConfig::default()
    };
    config.validate().unwrap();
    let elapsed = 73 * config.half_life_ms;
    let accrued = accrue_risk_rate(&config, rate(0.04), input(Some(0), 0), elapsed).unwrap();
    assert_eq!(accrued.end_apr_e18, u64::MAX as u128 * RATE_TO_NAD);
    assert!(accrued.integrated_apr_nad_ms > 0);
}

#[test]
fn every_market_has_the_same_nonzero_downward_floor() {
    let config = RiskRateConfig::default();
    let accrued = accrue_risk_rate(
        &config,
        rate(0.04),
        input(Some(u64::MAX as u128), 0),
        u64::MAX,
    )
    .unwrap();
    assert_eq!(accrued.end_apr_e18, rate(0.01));
    assert!(accrued.integrated_apr_nad_ms > 0);
}

proptest! {
    #[test]
    fn bounded_controller_matches_analytic_integral(
        half in crate::constants::MS_PER_DAY / 2..=30 * crate::constants::MS_PER_DAY,
        elapsed in 1_u64..=120 * crate::constants::MS_PER_DAY,
        start_bps in 200_u64..=20_000,
        rising in any::<bool>(),
    ) {
        let config = RiskRateConfig { half_life_ms: half, ..Default::default() };
        let start = start_bps as f64 / 10_000.0;
        let bound = if rising { u64::MAX as f64 / NAD as f64 } else { 0.01 };
        let k = if rising { 1.0 } else { -1.0 } * 2_f64.ln() / half as f64;
        let crossing = (bound / start).ln() / k;
        let time = (elapsed as f64).min(crossing.max(0.0));
        let end = if elapsed as f64 >= crossing.max(0.0) { bound } else { start * (k * time).exp() };
        let area = start * (k * time).exp_m1() / k + bound * (elapsed as f64 - time);
        let actual = accrue_risk_rate(
            &config,
            start_bps as u128 * RATE_SCALE / 10_000,
            input(Some(if rising { 0 } else { NAD as u128 }), 0),
            elapsed,
        )
        .unwrap();
        assert_close(actual.end_apr_e18 as f64 / RATE_SCALE as f64, end, 1e-7 + end * 1e-12);
        assert_close(actual.integrated_apr_nad_ms as f64 / NAD as f64, area, 1e-4 + area * 1e-12);
    }
}

#[test]
fn six_changes_separate_coverage_and_exit_liquidity() {
    // (junior, debt, cash, expected rate direction relative to the default band)
    // Initial pool: junior 200, senior 800, debt 800, cash 200; signal 5%.
    let cases = [
        (300, 800, 200, 0),  // senior -> junior, but utilization is at its upper bound
        (100, 800, 200, 1),  // junior -> senior, no cash moves
        (200, 900, 100, 1),  // additional borrowing
        (200, 700, 300, -1), // repayment
        (200, 800, 300, -1), // senior deposit
        (200, 800, 100, 1),  // senior withdrawal
    ];
    let config = RiskRateConfig::default();
    for (junior, debt, cash, direction) in cases {
        let utilization = crate::math::utilization_bps(debt, cash).unwrap();
        let signal = coverage_signal_nad(junior, debt, utilization).unwrap();
        let result = accrue_risk_rate(
            &config,
            rate(0.04),
            input(signal, utilization),
            config.half_life_ms,
        )
        .unwrap();
        assert_eq!(
            result.end_apr_e18.cmp(&rate(0.04)),
            match direction {
                1 => std::cmp::Ordering::Greater,
                -1 => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Equal,
            }
        );
    }
}

#[test]
fn independent_bands_resolve_conflicting_risk_and_demand_signals() {
    let config = RiskRateConfig::default();
    let start = rate(0.04);
    let elapsed = config.half_life_ms;
    let cases = [
        (Some(0), 1_000, rate(0.08)),              // low coverage, low utilization
        (Some(NAD as u128), 9_000, rate(0.08)),    // high coverage, high utilization
        (Some(NAD as u128), 1_000, rate(0.02)),    // high coverage, low utilization
        (Some(55_000_000), 7_750, start),          // both values inside their bands
        (Some(NAD as u128), 8_000, start),         // upper utilization bound is inclusive
        (Some(NAD as u128), 7_500, start),         // lower utilization bound is inclusive
    ];
    for (signal, utilization, expected) in cases {
        assert_eq!(
            accrue_risk_rate(&config, start, input(signal, utilization), elapsed)
                .unwrap()
                .end_apr_e18,
            expected
        );
    }
}

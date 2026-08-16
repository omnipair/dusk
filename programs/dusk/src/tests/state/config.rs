use super::*;
use crate::constants::MAX_PARAMETER_FEE_BPS;

fn valid_config() -> MarketConfig {
    MarketConfig {
        swap_fee_bps: 30,
        divergence_fee_share_cap_bps: 2_000,
        volatility_fee_share_cap_bps: 2_000,
        target_hlp_leverage_bps: 20_000,
        settlement_divergence_bps: 500,
        ema_half_life_ms: 60_000,
        directional_ema_half_life_ms: 60_000,
        q_ema_half_life_ms: 60_000,
        max_daily_borrow_bps: 2_000,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 11_000,
        amm: Default::default(),
        irm: IrmConfig::default(),
        start_time: 0,
    }
}

#[test]
fn fee_caps_accept_the_5000_bps_edge_and_reject_any_excess() {
    let mut config = valid_config();
    config.swap_fee_bps = 1_000;
    config.divergence_fee_share_cap_bps = 2_000;
    config.volatility_fee_share_cap_bps = 2_000;
    config.validate().unwrap();

    config.volatility_fee_share_cap_bps += 1;
    assert_eq!(
        config.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidSwapFeeBps)
    );

    let mut individually_too_large = valid_config();
    individually_too_large.swap_fee_bps = MAX_PARAMETER_FEE_BPS + 1;
    individually_too_large.divergence_fee_share_cap_bps = 0;
    individually_too_large.volatility_fee_share_cap_bps = 0;
    assert_eq!(
        individually_too_large.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidSwapFeeBps)
    );
}

#[test]
fn fee_profile_round_trips_and_invalid_apply_is_atomic() {
    let mut config = valid_config();
    let original = config;
    let mut profile = config.fee_profile();
    profile.base_fee_bps = 100;
    profile.divergence_fee_share_cap_bps = 1_500;
    profile.volatility_fee_share_cap_bps = 1_000;
    profile.divergence_fee_coefficient_nad = NAD;
    profile.volatility_fee_coefficient_nad = NAD / 2;
    profile.volatility_shock_cap_nad = NAD / 10;
    profile.volatility_accumulator_cap_nad = NAD;
    config.apply_fee_profile(profile).unwrap();
    assert_eq!(config.fee_profile(), profile);

    let before_invalid = config;
    profile.base_fee_bps = 5_000;
    assert!(config.apply_fee_profile(profile).is_err());
    assert_eq!(config, before_invalid);
    assert_ne!(config, original);
}

#[test]
fn standalone_fee_profile_validation_covers_signal_and_coefficient_bounds() {
    let mut profile = valid_config().fee_profile();
    profile.divergence_fee_coefficient_nad = MAX_AMM_FEE_COEFFICIENT_NAD + 1;
    assert!(profile.validate().is_err());

    profile = valid_config().fee_profile();
    profile.volatility_fee_coefficient_nad = 1;
    assert!(profile.validate().is_err());

    profile.volatility_shock_cap_nad = 1;
    profile.volatility_accumulator_cap_nad = MAX_AMM_VOLATILITY_NAD;
    assert!(profile.validate().is_ok());

    profile.volatility_half_life_ms = MAX_HALF_LIFE_MS + 1;
    assert!(profile.validate().is_err());
}

#[test]
fn irm_defaults_and_inclusive_bounds_validate() {
    assert_eq!(IrmConfig::default().target_utilization_bps, 7_000);
    assert_eq!(IrmConfig::default().curve_steepness_nad, 4 * NAD);
    assert_eq!(IrmConfig::default().adjustment_speed_per_year, 20);

    for target in [MIN_IRM_TARGET_UTILIZATION_BPS, MAX_IRM_TARGET_UTILIZATION_BPS] {
        let config = IrmConfig {
            target_utilization_bps: target,
            curve_steepness_nad: MIN_IRM_CURVE_STEEPNESS_NAD,
            adjustment_speed_per_year: MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR,
        };
        config.validate().unwrap();
    }

    let mut invalid = IrmConfig::default();
    invalid.target_utilization_bps = MIN_IRM_TARGET_UTILIZATION_BPS - 1;
    assert!(invalid.validate().is_err());
    invalid = IrmConfig::default();
    invalid.curve_steepness_nad = MAX_IRM_CURVE_STEEPNESS_NAD + 1;
    assert!(invalid.validate().is_err());
    invalid = IrmConfig::default();
    invalid.adjustment_speed_per_year = MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR + 1;
    assert!(invalid.validate().is_err());
}

#[test]
fn market_config_rejects_contribution_cap_below_health_floor() {
    let mut config = valid_config();
    config.global_health_contribution_cap_bps = 10_000;
    config.borrow_market_health_floor_bps = 11_000;

    let err = config.validate().unwrap_err();

    assert_eq!(err, anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig));
}

#[test]
fn market_config_rejects_inert_ema_half_lives() {
    let mut config = valid_config();
    config.ema_half_life_ms = 0;
    assert_eq!(
        config.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
    );

    let mut config = valid_config();
    config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS - 1;
    assert_eq!(
        config.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
    );

    let mut config = valid_config();
    config.q_ema_half_life_ms = MAX_HALF_LIFE_MS + 1;
    assert_eq!(
        config.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
    );
}

#[test]
fn market_config_rejects_invalid_hlp_leverage() {
    let mut config = valid_config();
    config.target_hlp_leverage_bps = 19_999;

    let err = config.validate().unwrap_err();

    assert_eq!(err, anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig));
}

#[test]
fn market_config_caps_daily_borrow_at_3000_bps() {
    let mut config = valid_config();
    config.max_daily_borrow_bps = MAX_DAILY_BORROW_BPS;
    config.validate().unwrap();

    config.max_daily_borrow_bps += 1;
    assert_eq!(
        config.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
    );
}

#[test]
fn market_config_validates_embedded_amm_config() {
    let mut config = valid_config();
    config.amm.range_width_nad = NAD;
    config.amm.concentrated_liquidity_share_nad = NAD / 2;

    assert_eq!(
        config.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
    );
}

#[test]
fn launch_fee_schedule_is_bounded_and_reaches_the_normal_fee_exactly() {
    let mut config = valid_config();
    config.start_time = 1_000;
    config.amm.launch_fee_start_bps = 1_000;
    config.amm.launch_fee_duration_seconds = 100;
    config.amm.launch_fee_decay_mode = LAUNCH_FEE_DECAY_LINEAR;
    config.validate().unwrap();

    assert_eq!(config.effective_base_fee_bps_at(999).unwrap(), 1_000);
    assert_eq!(config.effective_base_fee_bps_at(1_000).unwrap(), 1_000);
    assert_eq!(config.effective_base_fee_bps_at(1_050).unwrap(), 515);
    assert_eq!(config.effective_base_fee_bps_at(1_100).unwrap(), 30);

    config.amm.launch_fee_decay_mode = LAUNCH_FEE_DECAY_EXPONENTIAL;
    assert_eq!(config.effective_base_fee_bps_at(1_050).unwrap(), 34);
    assert_eq!(config.effective_base_fee_bps_at(1_100).unwrap(), 30);
}

#[test]
fn launch_fee_configuration_rejects_partial_or_over_budget_schedules() {
    let mut config = valid_config();
    config.amm.launch_fee_start_bps = 1_000;
    assert!(config.validate().is_err());

    config.amm.launch_fee_duration_seconds = 100;
    config.amm.launch_fee_decay_mode = LAUNCH_FEE_DECAY_LINEAR;
    config.amm.launch_fee_start_bps = 1_001;
    assert_eq!(
        config.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidSwapFeeBps)
    );
}

#[test]
fn launch_buy_size_limiter_composes_with_time_scheduler_and_only_charges_buys() {
    let mut config = valid_config();
    config.start_time = 1_000;
    config.divergence_fee_share_cap_bps = 1_500;
    config.volatility_fee_share_cap_bps = 1_500;
    config.amm.launch_fee_start_bps = 500;
    config.amm.launch_fee_duration_seconds = 50;
    config.amm.launch_fee_decay_mode = LAUNCH_FEE_DECAY_LINEAR;
    config.amm.launch_rate_limit_asset = LAUNCH_RATE_LIMIT_ASSET_BASE;
    config.amm.launch_rate_limit_reference_nad = 100 * NAD;
    config.amm.launch_rate_limit_increment_bps = 100;
    config.amm.launch_rate_limit_max_fee_bps = 1_000;
    config.amm.launch_rate_limit_duration_seconds = 100;
    config.validate().unwrap();

    // Quote input buys the configured launch asset (Base). The first
    // reference amount pays only the scheduled fee; 250 NAD spans three
    // reference units and therefore adds two increments.
    assert_eq!(
        config
            .effective_base_fee_bps_for_swap_at(MarketAsset::Quote, 100 * NAD as u128, 1_000)
            .unwrap(),
        500
    );
    assert_eq!(
        config
            .effective_base_fee_bps_for_swap_at(MarketAsset::Quote, 250 * NAD as u128, 1_000)
            .unwrap(),
        700
    );

    // Selling Base is not a buy of the protected launch asset.
    assert_eq!(
        config
            .effective_base_fee_bps_for_swap_at(MarketAsset::Base, 250 * NAD as u128, 1_000)
            .unwrap(),
        500
    );

    // The time premium has ended, while the size limiter remains active.
    assert_eq!(
        config
            .effective_base_fee_bps_for_swap_at(MarketAsset::Quote, 250 * NAD as u128, 1_050)
            .unwrap(),
        230
    );
    assert_eq!(
        config
            .effective_base_fee_bps_for_swap_at(MarketAsset::Quote, 250 * NAD as u128, 1_100)
            .unwrap(),
        30
    );
}

#[test]
fn launch_buy_size_limiter_rejects_partial_or_over_budget_configuration() {
    let mut config = valid_config();
    config.amm.launch_rate_limit_asset = LAUNCH_RATE_LIMIT_ASSET_BASE;
    assert!(config.validate().is_err());

    config.amm.launch_rate_limit_reference_nad = NAD;
    config.amm.launch_rate_limit_increment_bps = 100;
    config.amm.launch_rate_limit_max_fee_bps = 1_000;
    config.amm.launch_rate_limit_duration_seconds = 100;
    config.validate().unwrap();

    config.amm.launch_rate_limit_max_fee_bps = 1_001;
    assert_eq!(
        config.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidSwapFeeBps)
    );
}

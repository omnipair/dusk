use super::*;
use crate::{
    constants::{
        BPS_DENOMINATOR, INTEREST_INITIAL_RATE_AT_TARGET_NAD, MAX_HALF_LIFE_MS, MIN_HALF_LIFE_MS, MS_PER_YEAR, NAD,
        TARGET_MS_PER_SLOT,
    },
    math::{decay_volatility_nad, ema_u64},
    state::{AmmConfig, AmmCurveParameters, AmmState, MarketSide, ReserveShares, Reserves, Risk},
};

/// Independent state-only reference for rollback and ramp unit tests. The
/// production handler intentionally keeps its one execution path inline.
fn apply_config_update(market: &mut Market, config: MarketConfig, current_slot: u64) -> Result<()> {
    config.validate()?;
    let previous_config = market.config;
    let previous_base_side = market.base_side;
    let previous_quote_side = market.quote_side;
    let previous_amm = market.amm;
    let previous_debt = market.debt;
    let previous_risk = market.risk;
    let previous_last_marginal_observation_nad = market.last_marginal_observation_nad;
    let previous_curve_revision = market.curve_revision;
    let previous_risk_revision = market.risk_revision;
    let previous_last_update_slot = market.last_update_slot;

    let result = (|| {
        let curve_changed = previous_config.amm.curve_parameters() != config.amm.curve_parameters();
        market.accrue_interest_to_slot(current_slot)?;
        if market.amm.initialized {
            market
                .amm
                .observe_clock_from_validated_config(&previous_config.amm, current_slot)?;
        }
        market.refresh_risk_at_slot(current_slot)?;
        if curve_changed && market.amm.initialized {
            // This schedules the desired path only. Swap/risk integration must
            // value each candidate and commit it through the protected-profit
            // gate before it becomes effective.
            let applied = market
                .amm
                .effective_curve_parameters(&previous_config.amm, current_slot);
            market.amm.start_applied_ramp(applied, &config.amm, current_slot)?;
        }
        market.config = config;
        if market.amm.initialized && !curve_changed && previous_config.amm != config.amm {
            market.amm.invalidate_deferred_controller_target();
        }
        // Adjustment controls and a newly scheduled curve ramp can change the
        // protected-liquidity requirement even before executable reserves
        // move. Refresh it atomically with the timelocked config so the first
        // following swap cannot use stale fee-retention routing.
        market.finalize_amm_transition(current_slot)?;
        market.refresh_risk_at_slot(current_slot)?;
        market.assert_market_health()
    })();
    if result.is_err() {
        market.config = previous_config;
        market.base_side = previous_base_side;
        market.quote_side = previous_quote_side;
        market.amm = previous_amm;
        market.debt = previous_debt;
        market.risk = previous_risk;
        market.last_marginal_observation_nad = previous_last_marginal_observation_nad;
        market.curve_revision = previous_curve_revision;
        market.risk_revision = previous_risk_revision;
        market.last_update_slot = previous_last_update_slot;
    }
    result
}

fn valid_market_config() -> MarketConfig {
    MarketConfig {
        target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
        settlement_divergence_bps: 500,
        ema_half_life_ms: MIN_HALF_LIFE_MS,
        directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
        q_ema_half_life_ms: MIN_HALF_LIFE_MS,
        max_daily_borrow_bps: 2_000,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 11_000,
        amm: AmmConfig::default(),
        ..MarketConfig::default()
    }
}

fn concentrated_amm_config() -> AmmConfig {
    AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 100,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        adjustment_threshold_nad: NAD / 50,
        adjustment_step_nad: NAD / 100,
        min_adjustment_interval_slots: 10,
        ramp_duration_slots: crate::state::MIN_AMM_RAMP_DURATION_SLOTS,
        ..AmmConfig::default()
    }
}

fn initialized_market() -> Market {
    let config = valid_market_config();
    let mut market = Market {
        config,
        base_side: MarketSide {
            asset_decimals: 0,
            reserves: Reserves {
                live_reserve: 1_000_000,
                cash_reserve: 1_000_000,
            },
            shares: ReserveShares {
                ylp_supply: 1_000_000,
                ..ReserveShares::default()
            },
            ..MarketSide::default()
        },
        quote_side: MarketSide {
            asset_decimals: 0,
            reserves: Reserves {
                live_reserve: 1_000_000,
                cash_reserve: 1_000_000,
            },
            shares: ReserveShares {
                ylp_supply: 1_000_000,
                ..ReserveShares::default()
            },
            ..MarketSide::default()
        },
        ..Market::default()
    };
    market.amm = AmmState::initialize(&config.amm, NAD, NAD as u128, 0).unwrap();
    market
}

#[test]
fn applied_config_starts_desired_ramp_without_changing_effective_curve() {
    let mut market = initialized_market();
    let mut target = market.config;
    target.amm = concentrated_amm_config();
    let applied_slot = 100;

    apply_config_update(&mut market, target, applied_slot).unwrap();

    assert!(market.amm.ramp.active);
    assert_eq!(
        market.amm.effective_curve_parameters(&market.config.amm, applied_slot),
        AmmCurveParameters::cpmm()
    );
    assert_eq!(
        market
            .amm
            .desired_curve_parameters(&market.config.amm, market.amm.ramp.end_slot),
        target.amm.curve_parameters()
    );
}

#[test]
fn failed_overlapping_curve_update_restores_config_and_ramp() {
    let mut market = initialized_market();
    let mut first = market.config;
    first.amm = concentrated_amm_config();
    apply_config_update(&mut market, first, 100).unwrap();
    let saved_state = market.try_to_vec().unwrap();

    let mut overlapping = first;
    overlapping.amm.peak_depth_nad = 400 * NAD;
    assert!(apply_config_update(&mut market, overlapping, 101).is_err());
    assert_eq!(market.try_to_vec().unwrap(), saved_state);
}

#[test]
fn adjustment_config_update_defers_retention_to_the_lazy_controller() {
    let mut config = valid_market_config();
    config.amm = concentrated_amm_config();
    let mut market = initialized_market();
    market.config = config;
    market.amm = AmmState::initialize(&config.amm, NAD, NAD as u128, 100).unwrap();
    // Start off-center: at a perfectly balanced reserve composition the
    // first symmetric center step improves CONCENTRATED Q in either direction and
    // correctly needs no protection. An imbalanced state exercises the
    // impairing next-step target this regression is about.
    market.base_side.reserves.live_reserve = 1_200_000;
    market.base_side.reserves.cash_reserve = 1_200_000;
    market.checkpoint_amm_neutral_inventory(100).unwrap();
    assert!(market.amm.retention_target_stale);
    assert!(market.amm.retain_dynamic_surcharge);
    market.amm.deferred_controller_target = crate::state::DeferredControllerTarget {
        kind: crate::state::DeferredControllerTarget::RECENTER,
        center_price_nad: market.amm.center_price_nad + 1,
        parameters: market.amm.applied_curve_parameters,
        saturated: true,
        ..crate::state::DeferredControllerTarget::default()
    };

    let mut disabled = config;
    disabled.amm.adjustment_threshold_nad = 0;
    disabled.amm.adjustment_step_nad = 0;
    disabled.amm.min_adjustment_interval_slots = 0;
    apply_config_update(&mut market, disabled, 200).unwrap();

    assert_eq!(market.amm.retention_required_nad, 0);
    assert!(!market.amm.retain_dynamic_surcharge);
    assert_eq!(
        market.amm.deferred_controller_target,
        crate::state::DeferredControllerTarget::default()
    );
}

#[test]
fn half_life_update_closes_elapsed_interval_under_previous_config() {
    for (old_half_life, new_half_life) in [
        (MIN_HALF_LIFE_MS, MAX_HALF_LIFE_MS),
        (MAX_HALF_LIFE_MS, MIN_HALF_LIFE_MS),
    ] {
        let mut market = initialized_market();
        market.config.ema_half_life_ms = old_half_life;
        market.config.directional_ema_half_life_ms = old_half_life;
        market.config.q_ema_half_life_ms = old_half_life;
        market.config.amm.center_ema_half_life_ms = old_half_life;
        market.config.amm.volatility_half_life_ms = old_half_life;
        market.amm = AmmState::initialize(&market.config.amm, NAD, NAD as u128, 0).unwrap();
        let evaluation = market.evaluate_current_curve(0).unwrap();
        let current_q = evaluation.balanced_equivalent_q;
        market.amm.price_ema_nad = 2 * NAD;
        market.amm.last_trade_price_nad = NAD;
        market.amm.volatility_accumulator_nad = NAD;
        market.risk = Risk {
            base_price_ema_nad: 2 * NAD,
            quote_price_ema_nad: 2 * NAD,
            directional_base_price_ema_nad: 2 * NAD,
            directional_quote_price_ema_nad: 2 * NAD,
            cached_spot_base_price_nad: NAD,
            cached_spot_quote_price_nad: NAD,
            cached_q_nad: current_q,
            q_ema_nad: current_q,
            last_snapshot_slot: 0,
        };

        let elapsed_slots = old_half_life / TARGET_MS_PER_SLOT;
        let expected_price_ema = ema_u64(2 * NAD, NAD, 0, elapsed_slots, old_half_life);
        let expected_volatility = decay_volatility_nad(NAD, 0, elapsed_slots, old_half_life).unwrap();

        let mut target = market.config;
        target.ema_half_life_ms = new_half_life;
        target.directional_ema_half_life_ms = new_half_life;
        target.q_ema_half_life_ms = new_half_life;
        target.amm.center_ema_half_life_ms = new_half_life;
        target.amm.volatility_half_life_ms = new_half_life;
        apply_config_update(&mut market, target, elapsed_slots).unwrap();

        assert_eq!(market.amm.price_ema_nad, expected_price_ema);
        assert_eq!(market.amm.volatility_accumulator_nad, expected_volatility);
        assert_eq!(market.amm.last_observation_slot, elapsed_slots);
        assert_eq!(market.risk.base_price_ema_nad, expected_price_ema);
        assert_eq!(market.risk.quote_price_ema_nad, expected_price_ema);
        assert_eq!(market.risk.last_snapshot_slot, elapsed_slots);
    }
}

#[test]
fn config_execution_accrues_existing_debt_before_health_validation() {
    let mut market = initialized_market();
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.debt.base_rate_at_target_nad = INTEREST_INITIAL_RATE_AT_TARGET_NAD;
    market.debt.quote_rate_at_target_nad = INTEREST_INITIAL_RATE_AT_TARGET_NAD;
    market.debt.isolated_base_shares = 100_000;
    market.debt.isolated_base_principal = 100_000;
    market.base_side.reserves.live_reserve += 100_000;
    let index_before = market.debt.base_borrow_index_nad;
    let live_before = market.base_side.reserves.live_reserve;
    let execution_slot = MS_PER_YEAR / TARGET_MS_PER_SLOT;
    let config = market.config;

    apply_config_update(&mut market, config, execution_slot).unwrap();

    assert!(market.debt.base_borrow_index_nad > index_before);
    assert!(market.base_side.reserves.live_reserve > live_before);
    assert_eq!(market.debt.base_last_accrual_slot, execution_slot);
    market.assert_market_invariants().unwrap();
}

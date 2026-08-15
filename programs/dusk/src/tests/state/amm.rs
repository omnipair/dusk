use super::*;
use crate::constants::{MAX_HALF_LIFE_MS, MIN_HALF_LIFE_MS, NAD};
use crate::state::{Market, MarketConfig};

fn cpmm_config() -> AmmConfig {
    AmmConfig {
        peak_depth_nad: 0,
        fade_scale_nad: 0,
        range_width_nad: 0,
        concentrated_liquidity_share_nad: 0,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        adjustment_threshold_nad: 0,
        adjustment_step_nad: 0,
        min_adjustment_interval_slots: 0,
        volatility_shock_cap_nad: NAD / 10,
        volatility_cap_nad: NAD,
        divergence_fee_coefficient_nad: NAD,
        volatility_fee_coefficient_nad: NAD,
        reserved: [0; AMM_CONFIG_RESERVED_BYTES],
    }
}

fn concentrated_config() -> AmmConfig {
    AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 100,
        adjustment_threshold_nad: NAD / 50,
        adjustment_step_nad: NAD / 100,
        min_adjustment_interval_slots: 10,
        ..cpmm_config()
    }
}

fn geometry_cache_for(parameters: ConcentrationParameters) -> Option<ConcentratedGeometryCache> {
    (!parameters.is_cpmm())
        .then(|| {
            ConcentratedGeometryCache::derive(parameters.peak_depth_nad as u128, parameters.fade_scale_nad as u128)
        })
        .transpose()
        .unwrap()
}

#[test]
fn validates_cpmm_and_concentrated_endpoints() {
    assert!(AmmConfig::default().validate().is_ok());
    assert!(cpmm_config().validate().is_ok());
    assert!(concentrated_config().validate().is_ok());
    let mut cpmm_with_moving_fee_anchor = cpmm_config();
    cpmm_with_moving_fee_anchor.adjustment_threshold_nad = NAD / 50;
    cpmm_with_moving_fee_anchor.adjustment_step_nad = NAD / 100;
    cpmm_with_moving_fee_anchor.min_adjustment_interval_slots = 10;
    assert!(cpmm_with_moving_fee_anchor.validate().is_ok());

    let mut invalid = cpmm_config();
    invalid.fade_scale_nad = MIN_AMM_FADE_SCALE_NAD;
    assert!(invalid.validate().is_err());

    let mut invalid = concentrated_config();
    invalid.peak_depth_nad = MIN_AMM_PEAK_DEPTH_NAD - 1;
    assert!(invalid.validate().is_err());

    let mut invalid = concentrated_config();
    invalid.fade_scale_nad = MIN_AMM_FADE_SCALE_NAD - 1;
    assert!(invalid.validate().is_err());

    let mut invalid = concentrated_config();
    invalid.adjustment_threshold_nad = invalid.adjustment_step_nad - 1;
    assert!(invalid.validate().is_err());
}

#[test]
fn explicit_curve_parameters_round_trip_in_public_config() {
    let mut config = cpmm_config();
    let parameters = ExplicitCurveParameters {
        range_width_nad: 2 * NAD,
        concentrated_liquidity_share_nad: NAD / 2,
    };
    config.set_explicit_curve_parameters(parameters).unwrap();
    assert_eq!(config.explicit_curve_parameters().unwrap(), Some(parameters));
    assert!(!config.is_cpmm());
    config.validate().unwrap();

    let mut malformed = config;
    malformed.reserved[0] = 1;
    assert!(malformed.validate().is_err());
}

#[test]
fn validates_signal_and_fee_bounds() {
    let mut at_coefficient_bound = concentrated_config();
    at_coefficient_bound.divergence_fee_coefficient_nad = MAX_AMM_FEE_COEFFICIENT_NAD;
    at_coefficient_bound.volatility_fee_coefficient_nad = MAX_AMM_FEE_COEFFICIENT_NAD;
    assert!(at_coefficient_bound.validate().is_ok());

    let mut above_divergence_bound = at_coefficient_bound;
    above_divergence_bound.divergence_fee_coefficient_nad = MAX_AMM_FEE_COEFFICIENT_NAD + 1;
    assert!(above_divergence_bound.validate().is_err());

    let mut above_volatility_bound = at_coefficient_bound;
    above_volatility_bound.volatility_fee_coefficient_nad = MAX_AMM_FEE_COEFFICIENT_NAD + 1;
    assert!(above_volatility_bound.validate().is_err());

    let mut config = concentrated_config();
    config.center_ema_half_life_ms = MIN_HALF_LIFE_MS - 1;
    assert!(config.validate().is_err());

    config = concentrated_config();
    config.volatility_half_life_ms = MAX_HALF_LIFE_MS + 1;
    assert!(config.validate().is_err());

    config = concentrated_config();
    config.volatility_shock_cap_nad = config.volatility_cap_nad + 1;
    assert!(config.validate().is_err());

    config = concentrated_config();
    config.reserved[0] = 1;
    assert!(config.validate().is_err());
}

#[test]
fn initialization_anchors_center_and_protected_floor() {
    let initial_q = 2 * NAD as u128;
    let state = AmmState::initialize(&concentrated_config(), 150 * NAD, initial_q, 42).unwrap();
    assert!(state.initialized);
    assert_eq!(state.applied_curve_parameters, concentrated_config().curve_parameters());
    assert_eq!(state.center_price_nad, 150 * NAD);
    assert_eq!(state.price_ema_nad, 150 * NAD);
    assert_eq!(state.last_observation_slot, 42);
    assert_eq!(state.q_per_share_nad, initial_q);
    assert_eq!(state.protected_floor_per_share_nad, initial_q);
    assert_eq!(state.spendable_protected_profit_nad(), 0);
    assert!(!state.retain_dynamic_surcharge);
    assert_eq!(state.invariant_d_nad, 0);
    assert_eq!(state.curve_math_revision, CONCENTRATED_MATH_REVISION);
    assert!(state.concentrated_geometry_cache.matches(
        state.applied_curve_parameters.peak_depth_nad as u128,
        state.applied_curve_parameters.fade_scale_nad as u128,
    ));
    assert!(!state.retention_target_stale);
    assert_eq!(
        AMM_CONCENTRATED_GEOMETRY_CACHE_BYTES
            + AMM_RETENTION_TARGET_STALE_BYTES
            + AMM_DEFERRED_CONTROLLER_TARGET_BYTES
            + AMM_STATE_RESERVED_BYTES,
        212
    );
    assert_eq!(state._reserved, [0; AMM_STATE_RESERVED_BYTES]);
}

#[test]
fn concentrated_ready_amm_serialized_layout_is_locked() {
    assert_eq!(<AmmConfig as anchor_lang::Space>::INIT_SPACE, 129);
    assert_eq!(AMM_RETENTION_TARGET_STALE_BYTES, 1);
    assert_eq!(AMM_DEFERRED_CONTROLLER_TARGET_BYTES, 82);
    assert_eq!(AMM_STATE_RESERVED_BYTES, 0);
    assert_eq!(<AmmState as anchor_lang::Space>::INIT_SPACE, 433);
    assert_eq!(<MarketConfig as anchor_lang::Space>::INIT_SPACE, 195);
    // The initial Dusk layout starts without manager/operator or pending
    // whole-config state, and includes direct-yLP governance locks plus five
    // independent family revisions.
    // The leaky daily buckets carry one additional u64 remainder per side
    // (+16 bytes total). Four source-scoped hLP backing counters add 32 bytes,
    // and the two hLP-funding carry fields add another 16 bytes.
    // Dev markets are recreated, so this is canonical.
    assert_eq!(<Market as anchor_lang::Space>::INIT_SPACE, 2_849);
    assert_eq!(8 + <Market as anchor_lang::Space>::INIT_SPACE, 2_857);
}

#[test]
fn formula_revision_refreshes_the_authoritative_geometry() {
    let config = concentrated_config();
    let parameters = config.curve_parameters();
    let mut state = AmmState::initialize(&config, NAD, NAD as u128, 10).unwrap();
    state.curve_math_revision = CONCENTRATED_MATH_REVISION.wrapping_sub(1);

    state.commit_invariant(123).unwrap();

    assert_eq!(state.curve_math_revision, CONCENTRATED_MATH_REVISION);
    assert_eq!(state.invariant_d_nad, 123);
    assert!(state
        .concentrated_geometry_cache
        .matches(parameters.peak_depth_nad as u128, parameters.fade_scale_nad as u128,));
}

#[test]
fn same_slot_trades_accumulate_volatility_without_moving_ema() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, 100 * NAD, NAD as u128, 10).unwrap();

    state.checkpoint_trade(&config, 100 * NAD, 110 * NAD, 10).unwrap();
    assert_eq!(state.price_ema_nad, 100 * NAD);
    assert_eq!(state.volatility_accumulator_nad, NAD / 10);

    state.checkpoint_trade(&config, 110 * NAD, 121 * NAD, 10).unwrap();
    assert_eq!(state.price_ema_nad, 100 * NAD);
    assert_eq!(state.volatility_accumulator_nad, NAD / 5);
}

#[test]
fn same_slot_internal_rebase_is_not_counted_as_external_flow() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, 100 * NAD, NAD as u128, 10).unwrap();

    state.checkpoint_trade(&config, 100 * NAD, 110 * NAD, 10).unwrap();
    // hLP settlement, recentering, or a funded ramp may make the next swap
    // start far from the prior trade endpoint. Only this swap's 200 -> 220
    // executable path is a volatility shock.
    state.checkpoint_trade(&config, 200 * NAD, 220 * NAD, 10).unwrap();

    assert_eq!(state.volatility_accumulator_nad, NAD / 5);
    assert_eq!(state.last_trade_price_nad, 220 * NAD);
    assert_eq!(state.price_ema_nad, 100 * NAD);
}

#[test]
fn next_slot_ema_observes_prior_slot_final_trade_and_volatility_decays() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, 100 * NAD, NAD as u128, 10).unwrap();
    state.checkpoint_trade(&config, 100 * NAD, 120 * NAD, 10).unwrap();

    let one_half_life_later = 10 + MIN_HALF_LIFE_MS / 400;
    state
        .checkpoint_trade(&config, 120 * NAD, 120 * NAD, one_half_life_later)
        .unwrap();
    assert!(state.price_ema_nad.abs_diff(110 * NAD) <= 200);
    assert!(state.volatility_accumulator_nad.abs_diff(NAD / 20) <= 2);
}

#[test]
fn clock_observation_advances_ema_and_decay_without_trade_shock() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, 100 * NAD, NAD as u128, 10).unwrap();
    state.checkpoint_trade(&config, 100 * NAD, 120 * NAD, 10).unwrap();
    let last_trade = state.last_trade_price_nad;
    let one_half_life_later = 10 + MIN_HALF_LIFE_MS / 400;

    state.observe_clock(&config, one_half_life_later).unwrap();

    assert!(state.price_ema_nad.abs_diff(110 * NAD) <= 200);
    assert!(state.volatility_accumulator_nad.abs_diff(NAD / 20) <= 2);
    assert_eq!(state.last_trade_price_nad, last_trade);
    assert_eq!(state.last_observation_slot, one_half_life_later);

    let snapshot = state;
    state.observe_clock(&config, one_half_life_later).unwrap();
    assert_eq!(state, snapshot);
    assert!(state.observe_clock(&config, one_half_life_later - 1).is_err());
    assert_eq!(state, snapshot);
}

#[test]
fn next_slot_ema_uses_last_trade_signal_but_flow_rebases_to_current_curve() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, 100 * NAD, NAD as u128, 10).unwrap();
    state.checkpoint_trade(&config, 100 * NAD, 120 * NAD, 10).unwrap();

    let one_half_life_later = 10 + MIN_HALF_LIFE_MS / 400;
    // The internal curve has moved to 50 before the next external trade.
    state
        .checkpoint_trade(&config, 50 * NAD, 55 * NAD, one_half_life_later)
        .unwrap();

    // EMA still observes the prior successful trade signal (120), while the
    // new volatility shock is only 50 -> 55. The prior shock was capped at
    // 10%, decays to 5%, then this 10% path brings the accumulator to 15%.
    assert!(state.price_ema_nad.abs_diff(110 * NAD) <= 200);
    assert!(state.volatility_accumulator_nad.abs_diff(3 * NAD / 20) <= 2);
    assert_eq!(state.last_trade_price_nad, 55 * NAD);
}

#[test]
fn recenter_ramp_and_hlp_checkpoints_do_not_create_flow_volatility() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, 100 * NAD, 1_000, 10).unwrap();
    state.checkpoint_trade(&config, 100 * NAD, 110 * NAD, 10).unwrap();
    let flow_volatility = state.volatility_accumulator_nad;
    let trade_signal = state.last_trade_price_nad;

    // Recenter changes internal curve geometry but is not a trader execution.
    state.commit_recenter(&config, 101 * NAD, 123, 1_000, 0, 20).unwrap();
    assert_eq!(state.volatility_accumulator_nad, flow_volatility);
    assert_eq!(state.last_trade_price_nad, trade_signal);

    // A neutral inventory checkpoint represents hLP/internal depth changes.
    state.checkpoint_neutral_liquidity(1_100);
    assert_eq!(state.volatility_accumulator_nad, flow_volatility);
    assert_eq!(state.last_trade_price_nad, trade_signal);

    // A funded ramp point likewise changes curve geometry without fabricating
    // an external price shock.
    let mut target = config;
    target.peak_depth_nad *= 2;
    state
        .start_concentration_ramp(config.curve_parameters(), &target, 20)
        .unwrap();
    let candidate = ConcentrationParameters {
        peak_depth_nad: config.peak_depth_nad + 1,
        fade_scale_nad: config.fade_scale_nad + 1,
    };
    state
        .commit_applied_curve_parameters(candidate, geometry_cache_for(candidate), 21)
        .unwrap();
    assert_eq!(state.volatility_accumulator_nad, flow_volatility);
    assert_eq!(state.last_trade_price_nad, trade_signal);

    // The next trade rebases to its actual post-internal-move start. EMA still
    // receives the prior successful trade signal.
    let decayed = state.decayed_volatility(&target, 21).unwrap();
    let expected = crate::math::volatility_after_success_nad(
        decayed,
        50 * NAD,
        55 * NAD,
        target.volatility_shock_cap_nad,
        target.volatility_cap_nad,
    )
    .unwrap();
    state.checkpoint_trade(&target, 50 * NAD, 55 * NAD, 21).unwrap();
    assert_eq!(state.volatility_accumulator_nad, expected);
    assert_eq!(state.last_trade_price_nad, 55 * NAD);
    assert!(state.price_ema_nad > 100 * NAD);
}

#[test]
fn applied_ramp_starts_immediately_and_interpolates_deterministically() {
    let start = ConcentrationParameters::cpmm();
    let target = concentrated_config().curve_parameters();
    let duration = MIN_CONCENTRATION_RAMP_DURATION_SLOTS * 2;
    let ramp = ConcentrationRamp::start(start, target, 500, duration).unwrap();

    assert_eq!(ramp.parameters_at(start, 499), start);
    assert_eq!(ramp.parameters_at(start, 500), start);
    assert_eq!(
        ramp.parameters_at(start, 500 + duration / 2),
        ConcentrationParameters {
            peak_depth_nad: target.peak_depth_nad / 2,
            fade_scale_nad: target.fade_scale_nad / 2,
        }
    );
    assert_eq!(ramp.parameters_at(start, ramp.end_slot), target);
}

#[test]
fn protocol_sequenced_cpmm_ramps_keep_every_positive_peak_conditioned() {
    let cpmm = ConcentrationParameters::cpmm();
    let concentrated = ConcentrationParameters {
        peak_depth_nad: MIN_AMM_PEAK_DEPTH_NAD,
        fade_scale_nad: MIN_AMM_FADE_SCALE_NAD,
    };
    let duration = MIN_CONCENTRATION_RAMP_DURATION_SLOTS;

    let entering = ConcentrationRamp::start(cpmm, concentrated, 100, duration).unwrap();
    let first = entering.parameters_at(cpmm, 101);
    assert_eq!(first.peak_depth_nad, concentrated.peak_depth_nad / duration);
    assert_eq!(first.fade_scale_nad, MIN_AMM_FADE_SCALE_NAD);
    assert!(first.peak_depth_nad < MIN_AMM_PEAK_DEPTH_NAD);
    first.validate_runtime().unwrap();
    assert_eq!(entering.parameters_at(cpmm, entering.end_slot), concentrated);

    let exiting = ConcentrationRamp::start(concentrated, cpmm, 1_000_000, duration).unwrap();
    let penultimate = exiting.parameters_at(concentrated, exiting.end_slot - 1);
    assert_eq!(penultimate.peak_depth_nad, concentrated.peak_depth_nad / duration);
    assert_eq!(penultimate.fade_scale_nad, MIN_AMM_FADE_SCALE_NAD);
    penultimate.validate_runtime().unwrap();
    assert_eq!(exiting.parameters_at(concentrated, exiting.end_slot), cpmm);
}

#[test]
fn concentrated_to_concentrated_ramp_interpolates_both_safe_coordinates() {
    let start = ConcentrationParameters {
        peak_depth_nad: 10 * NAD,
        fade_scale_nad: MIN_AMM_FADE_SCALE_NAD,
    };
    let target = ConcentrationParameters {
        peak_depth_nad: 100 * NAD,
        fade_scale_nad: 10_000,
    };
    let duration = MIN_CONCENTRATION_RAMP_DURATION_SLOTS;
    let ramp = ConcentrationRamp::start(start, target, 500, duration).unwrap();
    for slot in [501, 500 + duration / 2, ramp.end_slot - 1] {
        let candidate = ramp.parameters_at(start, slot);
        assert!(candidate.peak_depth_nad > 0);
        assert!(candidate.fade_scale_nad >= MIN_AMM_FADE_SCALE_NAD);
        candidate.validate_runtime().unwrap();
    }
}

#[test]
fn state_rejects_overlapping_ramp_and_clears_finished_history() {
    let old = cpmm_config();
    let target = concentrated_config();
    let mut state = AmmState::initialize(&old, NAD, NAD as u128, 100).unwrap();
    state.start_concentration_ramp(old.curve_parameters(), &target, 100).unwrap();
    assert!(state.start_concentration_ramp(old.curve_parameters(), &target, 101).is_err());
    assert!(!state.settle_concentration_ramp(state.concentration_ramp.end_slot - 1));
    assert!(!state.settle_concentration_ramp(state.concentration_ramp.end_slot));
    assert_eq!(
        state.effective_curve_parameters(&target, state.concentration_ramp.end_slot),
        old.curve_parameters()
    );
    assert_eq!(
        state.desired_curve_parameters(&target, state.concentration_ramp.end_slot),
        target.curve_parameters()
    );
    let candidate = target.curve_parameters();
    state
        .commit_applied_curve_parameters(candidate, geometry_cache_for(candidate), state.concentration_ramp.end_slot)
        .unwrap();
    assert!(state.settle_concentration_ramp(state.concentration_ramp.end_slot));
    assert!(!state.concentration_ramp.active);
}

#[test]
fn cpmm_transition_accepts_sub_minimum_runtime_points_without_applying_time_alone() {
    let old = cpmm_config();
    let target = concentrated_config();
    let mut state = AmmState::initialize(&old, NAD, NAD as u128, 100).unwrap();
    state.start_concentration_ramp(old.curve_parameters(), &target, 100).unwrap();

    let first_candidate = state.desired_curve_parameters(&target, 101);
    assert!(first_candidate.peak_depth_nad < MIN_AMM_PEAK_DEPTH_NAD);
    assert_eq!(
        first_candidate.fade_scale_nad,
        (target.fade_scale_nad / MIN_CONCENTRATION_RAMP_DURATION_SLOTS)
            .max(MIN_AMM_FADE_SCALE_NAD)
    );
    assert_eq!(
        state.effective_curve_parameters(&target, 101),
        ConcentrationParameters::cpmm()
    );
    state
        .commit_applied_curve_parameters(first_candidate, geometry_cache_for(first_candidate), 101)
        .unwrap();
    assert_eq!(state.effective_curve_parameters(&target, 101), first_candidate);
}

#[test]
fn cpmm_ramp_never_exposes_half_enabled_concentration_after_integer_rounding() {
    let start = ConcentrationParameters::cpmm();
    let target = ConcentrationParameters {
        peak_depth_nad: MIN_AMM_PEAK_DEPTH_NAD,
        fade_scale_nad: MIN_AMM_FADE_SCALE_NAD,
    };
    let ramp = ConcentrationRamp::start(start, target, 100, MIN_CONCENTRATION_RAMP_DURATION_SLOTS).unwrap();

    for slot in 101..110 {
        let candidate = ramp.parameters_at(start, slot);
        assert!(
            candidate == ConcentrationParameters::cpmm()
                || (candidate.peak_depth_nad > 0 && candidate.fade_scale_nad >= MIN_AMM_FADE_SCALE_NAD)
        );
        candidate.validate_runtime().unwrap();
    }
}

#[test]
fn active_protocol_sequenced_ramp_preserves_retention_routing_state() {
    let old = cpmm_config();
    let target = concentrated_config();
    let mut state = AmmState::initialize(&old, NAD, NAD as u128, 100).unwrap();
    state.retain_dynamic_surcharge = true;
    state.start_concentration_ramp(old.curve_parameters(), &target, 100).unwrap();

    let candidate = state.desired_curve_parameters(&target, 101);
    state
        .commit_applied_curve_parameters(candidate, geometry_cache_for(candidate), 101)
        .unwrap();
    assert!(state.concentration_ramp.active);
    assert!(state.retain_dynamic_surcharge);
    assert!(state.applied_curve_parameters.fade_scale_nad >= MIN_AMM_FADE_SCALE_NAD);
}

#[test]
fn expired_underfunded_intermediate_can_be_redirected_by_new_ramp() {
    let old = cpmm_config();
    let first_target = concentrated_config();
    let mut state = AmmState::initialize(&old, NAD, NAD as u128, 100).unwrap();
    state
        .start_concentration_ramp(old.curve_parameters(), &first_target, 100)
        .unwrap();

    let intermediate = state.desired_curve_parameters(&first_target, 101);
    assert!(intermediate.peak_depth_nad < MIN_AMM_PEAK_DEPTH_NAD);
    state
        .commit_applied_curve_parameters(intermediate, geometry_cache_for(intermediate), 101)
        .unwrap();

    let expired_slot = state.concentration_ramp.end_slot;
    state.deferred_controller_target = DeferredControllerTarget {
        kind: DeferredControllerTarget::RAMP,
        parameters: state.concentration_ramp.target,
        saturated: true,
        ..DeferredControllerTarget::default()
    };
    state.retention_target_saturated = true;
    let mut replacement = concentrated_config();
    replacement.peak_depth_nad *= 2;
    state
        .start_concentration_ramp(intermediate, &replacement, expired_slot)
        .unwrap();

    assert_eq!(state.concentration_ramp.start, intermediate);
    assert_eq!(state.concentration_ramp.target, replacement.curve_parameters());
    assert_eq!(state.deferred_controller_target, DeferredControllerTarget::default());
    assert!(!state.retention_target_saturated);
    assert!(state.retention_target_stale);
}

#[test]
fn retained_surcharge_is_only_transition_that_creates_buffer() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, NAD, 1_000, 0).unwrap();

    state.checkpoint_retained_surcharge(1_100).unwrap();
    assert_eq!(state.spendable_protected_profit_nad(), 100);

    state.checkpoint_neutral_liquidity(1_300);
    assert_eq!(state.protected_floor_per_share_nad, 1_200);
    assert_eq!(state.spendable_protected_profit_nad(), 100);

    state.checkpoint_recenter_or_loss(1_250);
    assert_eq!(state.protected_floor_per_share_nad, 1_200);
    assert_eq!(state.spendable_protected_profit_nad(), 50);

    state.checkpoint_recenter_or_loss(1_350);
    assert_eq!(state.protected_floor_per_share_nad, 1_300);
    assert_eq!(state.spendable_protected_profit_nad(), 50);
}

#[test]
fn retention_target_uses_fixed_coverage_guard_cap_and_hysteresis() {
    let q = 10_000_000;
    let target = retention_target(q, 30_000).unwrap();
    assert_eq!(target.hard_cap_nad, 100_000);
    assert_eq!(target.required_nad, 38_500);
    assert_eq!(target.stop_nad, 42_350);
    assert!(!target.saturated);

    let saturated = retention_target(q, 120_000).unwrap();
    assert_eq!(saturated.required_nad, 100_000);
    assert_eq!(saturated.stop_nad, 100_000);
    assert!(saturated.saturated);
}

#[test]
fn retention_stays_armed_while_target_is_stale_and_stops_after_refresh() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, NAD, 10_000_000, 0).unwrap();
    let target = state.refresh_retention_target(10_000_000, 30_000).unwrap();
    assert!(state.retain_dynamic_surcharge);

    state
        .checkpoint_retained_surcharge(10_000_000 + target.required_nad)
        .unwrap();
    assert!(state.retain_dynamic_surcharge);
    state
        .checkpoint_retained_surcharge(10_000_000 + target.hard_cap_nad)
        .unwrap();
    assert!(state.retention_target_stale);
    assert!(state.retain_dynamic_surcharge);
    state.checkpoint_neutral_liquidity(state.q_per_share_nad);
    assert!(state.retain_dynamic_surcharge);

    state.refresh_retention_target(state.q_per_share_nad, 30_000).unwrap();
    assert!(!state.retention_target_stale);
    assert!(!state.retain_dynamic_surcharge);

    state.checkpoint_recenter_or_loss(10_000_000);
    assert!(state.retain_dynamic_surcharge);
}

#[test]
fn recenter_requires_covered_buffer_and_consumes_it() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, 100 * NAD, 10_000, 0).unwrap();
    state.refresh_retention_target(10_000, 10).unwrap();
    state.checkpoint_retained_surcharge(10_100).unwrap();

    let before_underfunded = state;
    assert!(state.commit_recenter(&config, 110 * NAD, 900, 10_050, 101, 10).is_err());
    assert_eq!(state, before_underfunded);

    state.commit_recenter(&config, 110 * NAD, 900, 10_050, 50, 10).unwrap();
    assert_eq!(state.center_price_nad, 110 * NAD);
    assert_eq!(state.invariant_d_nad, 900);
    assert_eq!(state.spendable_protected_profit_nad(), 50);
}

#[test]
fn recenter_rejects_a_malformed_invariant_without_partial_state_mutation() {
    let config = concentrated_config();
    let mut state = AmmState::initialize(&config, 100 * NAD, 10_000, 0).unwrap();
    state.refresh_retention_target(10_000, 10).unwrap();
    state.checkpoint_retained_surcharge(10_100).unwrap();
    state.commit_invariant(1_001).unwrap();
    let before = state;

    assert!(state.commit_recenter(&config, 110 * NAD, 0, 10_050, 50, 10).is_err());
    assert_eq!(state, before);
}

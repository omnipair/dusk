use super::*;
use crate::{
    constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS, MIN_LIQUIDITY},
    math::{ExplicitCurveParameters, MIN_INNER_COMMON_RESERVE},
    state::{
        AmmConfig, Debt, MarketAsset, MarketConfig, MarketSide, ReserveShares, Reserves, MAX_AMM_ADJUSTMENT_NAD,
        MIN_AMM_ADJUSTMENT_NAD, MIN_AMM_FADE_SCALE_NAD, MIN_AMM_PEAK_DEPTH_NAD, MIN_CONCENTRATION_RAMP_DURATION_SLOTS,
    },
};

/// Mirrors the identity-checked neutral endpoint application in the spot
/// instruction without restoring a one-use production wrapper.
fn checkpoint_trade_endpoint_like_spot(
    market: &mut Market,
    checkpoint: CurveCheckpoint,
    current_slot: u64,
) -> Result<()> {
    market.ensure_amm_initialized(current_slot)?;
    require!(market.amm.initialized, ErrorCode::BrokenInvariant);
    let evaluation = checkpoint.validated_evaluation(market, current_slot)?;
    let q_per_share_nad = market.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
    market.amm.commit_invariant(evaluation.invariant_d)?;
    market.amm.checkpoint_neutral_liquidity(q_per_share_nad);
    Ok(())
}

fn center_step_toward(center: u64, target: u64, step_nad: u64) -> Result<u64> {
    if target > center {
        let stepped = ceil_div(
            (center as u128)
                .checked_mul(NAD.checked_add(step_nad).ok_or(ErrorCode::MarketMathOverflow)? as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            NAD as u128,
        )
        .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(u64::try_from(stepped)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
            .min(target))
    } else if target < center {
        let down = (center as u128)
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div((NAD + step_nad) as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?
            .max(1);
        Ok(u64::try_from(down)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
            .max(target))
    } else {
        Ok(center)
    }
}

fn concentrated_config() -> AmmConfig {
    AmmConfig {
        range_width_nad: 4 * NAD,
        concentrated_liquidity_share_nad: NAD / 2,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 1_000,
        min_adjustment_interval_slots: 1,
        ..AmmConfig::default()
    }
}

fn market_with_liquidity(config: AmmConfig) -> Market {
    let reserve = 1_000_000 * NAD;
    Market {
        version: MARKET_LAYOUT_VERSION,
        base_side: MarketSide {
            asset_decimals: 9,
            reserves: Reserves {
                live_reserve: reserve,
                cash_reserve: reserve,
                ..Reserves::default()
            },
            shares: ReserveShares {
                ylp_supply: reserve,
                ..ReserveShares::default()
            },
            ..MarketSide::default()
        },
        quote_side: MarketSide {
            asset_decimals: 9,
            reserves: Reserves {
                live_reserve: reserve,
                cash_reserve: reserve,
                ..Reserves::default()
            },
            shares: ReserveShares {
                ylp_supply: reserve,
                ..ReserveShares::default()
            },
            ..MarketSide::default()
        },
        config: MarketConfig {
            divergence_fee_share_cap_bps: 2_000,
            volatility_fee_share_cap_bps: 2_000,
            amm: config,
            ..MarketConfig::default()
        },
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            ..Debt::default()
        },
        ..Market::default()
    }
}

#[test]
fn first_liquidity_initializes_center_without_an_oracle() {
    let mut market = market_with_liquidity(concentrated_config());
    assert!(market.ensure_amm_initialized(10).unwrap());
    assert_eq!(market.amm.center_price_nad, NAD);
    assert_eq!(market.amm.price_ema_nad, NAD);
    assert_eq!(
        market.amm.applied_curve_parameters,
        market.config.amm.curve_parameters()
    );
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
}

#[test]
fn explicit_curve_initializes_and_quotes_the_integrated_ordinary_tranche() {
    let mut config = AmmConfig::default();
    config
        .set_explicit_curve_parameters(ExplicitCurveParameters {
            range_width_nad: 2 * NAD,
            concentrated_liquidity_share_nad: NAD / 2,
        })
        .unwrap();
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    assert_eq!(
        market.amm.explicit_curve_cache.parameters(),
        market.config.amm.explicit_curve_parameters().unwrap().unwrap()
    );

    let quote = market
        .quote_integrated_explicit_exact_in_nad(10_000 * NAD as u128, 100 * NAD as u128, MarketAsset::Base)
        .unwrap()
        .unwrap();
    assert_eq!(quote.amount_in_after_fee, 9_900 * NAD as u128);
    assert_eq!(quote.executable.curve.boundary_crossings, 0);
    assert!(quote.executable.amount_out > 0);

    let pre_state = market.dynamic_fee_pre_state(10).unwrap();
    let preliminary = market
        .preliminary_swap_inputs_for_state(10_000 * NAD, 10, pre_state)
        .unwrap();
    let fee_quote = market
        .quote_explicit_integrated_with_fee(MarketAsset::Base, 10_000 * NAD, preliminary)
        .unwrap()
        .unwrap();
    assert_eq!(
        fee_quote.fee.total_fee_debit + fee_quote.fee.amount_in_for_quote,
        fee_quote.fee.reserve_credit
    );
    assert_eq!(fee_quote.amount_out as u128, fee_quote.integrated.executable.amount_out);
}

#[test]
fn explicit_center_move_reconstructs_unchanged_reserves_in_closed_form() {
    let mut config = AmmConfig {
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 100,
        min_adjustment_interval_slots: 1,
        ..AmmConfig::default()
    };
    config
        .set_explicit_curve_parameters(ExplicitCurveParameters {
            range_width_nad: 2 * NAD,
            concentrated_liquidity_share_nad: NAD / 2,
        })
        .unwrap();
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    let ordinary_before = market.integrated_curve_state_nad().unwrap();
    let cache_before = market.amm.explicit_curve_cache;
    market.amm.price_ema_nad = 2 * NAD;
    market.amm.protected_floor_per_share_nad = 0;

    assert!(market.advance_one_amm_controller_target(11).unwrap());
    assert_eq!(market.integrated_curve_state_nad().unwrap(), ordinary_before);
    assert_ne!(market.amm.explicit_curve_cache, cache_before);
    assert_eq!(market.amm.center_price_nad, NAD + NAD / 100);
    assert_eq!(market.amm.last_adjustment_slot, 11);
    assert!(market.current_explicit_spot_price_nad().unwrap().unwrap() > 0);
}

#[test]
fn explicit_center_move_deploys_locked_bucket_only_when_it_preserves_ylp_principal() {
    let mut config = AmmConfig {
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 100,
        min_adjustment_interval_slots: 1,
        ..AmmConfig::default()
    };
    config
        .set_explicit_curve_parameters(ExplicitCurveParameters {
            range_width_nad: 2 * NAD,
            concentrated_liquidity_share_nad: NAD / 2,
        })
        .unwrap();
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    market.amm.price_ema_nad = 2 * NAD;
    let q_before = market.amm.q_per_share_nad;
    let base_live_before = market.base_side.reserves.live_reserve;
    let quote_live_before = market.quote_side.reserves.live_reserve;
    let protected = 100_000 * NAD;

    market
        .credit_protected_recenter_reserve(MarketAsset::Base, protected)
        .unwrap();
    market
        .credit_protected_recenter_reserve(MarketAsset::Quote, protected)
        .unwrap();
    assert_eq!(market.base_side.reserves.live_reserve, base_live_before);
    assert_eq!(market.quote_side.reserves.live_reserve, quote_live_before);

    assert!(market.advance_one_amm_controller_target(11).unwrap());
    assert_eq!(market.base_side.reserves.protected_recenter_reserve, 0);
    assert_eq!(market.quote_side.reserves.protected_recenter_reserve, 0);
    assert_eq!(market.base_side.reserves.live_reserve, base_live_before + protected);
    assert_eq!(market.quote_side.reserves.live_reserve, quote_live_before + protected);
    assert!(market.amm.q_per_share_nad >= q_before);
    assert_eq!(market.amm.center_price_nad, NAD + NAD / 100);
    market.assert_market_invariants().unwrap();
}

#[test]
fn explicit_proportional_liquidity_scales_and_restores_both_tranches() {
    let mut config = AmmConfig::default();
    config
        .set_explicit_curve_parameters(ExplicitCurveParameters {
            range_width_nad: 2 * NAD,
            concentrated_liquidity_share_nad: NAD / 2,
        })
        .unwrap();
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    let cache_before = market.amm.explicit_curve_cache;
    let spot_before = market.current_explicit_spot_price_nad().unwrap().unwrap();
    let credit = 100_000 * NAD;
    let added = market.add_liquidity(credit, credit).unwrap();
    market.finalize_amm_transition_and_observe_risk(11).unwrap();
    assert!(market.amm.explicit_curve_cache.tail_liquidity > cache_before.tail_liquidity);
    assert!(
        market.amm.explicit_curve_cache.concentrated_liquidity > cache_before.concentrated_liquidity
    );

    market.remove_liquidity(added.ylp_amount).unwrap();
    market.finalize_amm_transition_and_observe_risk(12).unwrap();
    assert_eq!(market.amm.explicit_curve_cache.parameters(), cache_before.parameters());
    assert!(market
        .current_explicit_spot_price_nad()
        .unwrap()
        .unwrap()
        .abs_diff(spot_before)
        <= 2);
    market.assert_market_invariants().unwrap();
}

#[cfg(any())]
mod legacy_implicit_curve_tests {
use super::*;

#[test]
fn combined_transition_observation_matches_the_split_reference() {
    let mut combined = market_with_liquidity(concentrated_config());
    let mut reference = combined.clone();

    combined.finalize_amm_transition_and_observe_risk(10).unwrap();
    reference.finalize_amm_transition(10).unwrap();
    reference.observe_current_risk(10).unwrap();
    assert_eq!(combined.amm, reference.amm);
    assert_eq!(combined.risk, reference.risk);
    assert_eq!(combined.curve_revision, reference.curve_revision);
    assert_eq!(combined.risk_revision, reference.risk_revision);

    combined.base_side.reserves.live_reserve += NAD;
    combined.base_side.reserves.cash_reserve += NAD;
    reference.base_side.reserves.live_reserve += NAD;
    reference.base_side.reserves.cash_reserve += NAD;
    combined.finalize_amm_transition_and_observe_risk(11).unwrap();
    reference.finalize_amm_transition(11).unwrap();
    reference.observe_current_risk(11).unwrap();
    assert_eq!(combined.amm, reference.amm);
    assert_eq!(combined.risk, reference.risk);
    assert_eq!(combined.curve_revision, reference.curve_revision);
    assert_eq!(combined.risk_revision, reference.risk_revision);
}

#[test]
fn concentrated_initialization_rejects_an_inner_state_below_the_common_reserve_floor() {
    let mut market = market_with_liquidity(concentrated_config());
    let unsupported = u64::try_from(MIN_INNER_COMMON_RESERVE - 1).unwrap();
    for side in [&mut market.base_side, &mut market.quote_side] {
        side.reserves.live_reserve = unsupported;
        side.reserves.cash_reserve = unsupported;
        side.shares.ylp_supply = unsupported;
    }

    assert!(market.ensure_amm_initialized(10).is_err());
    assert!(!market.amm.initialized);
}

#[test]
fn partial_public_exit_cannot_leave_an_unsupported_concentrated_inner_state() {
    let mut market = market_with_liquidity(concentrated_config());
    let supported = u64::try_from(2 * MIN_INNER_COMMON_RESERVE).unwrap();
    for side in [&mut market.base_side, &mut market.quote_side] {
        side.reserves.live_reserve = supported;
        side.reserves.cash_reserve = supported;
        side.shares.ylp_supply = supported;
    }
    market.ensure_amm_initialized(10).unwrap();

    let unsupported_remainder = u64::try_from(MIN_INNER_COMMON_RESERVE - 1).unwrap();
    let receipt = market.remove_liquidity(supported - unsupported_remainder).unwrap();
    assert_eq!(receipt.ylp_supply, unsupported_remainder);
    assert_eq!(market.base_side.reserves.live_reserve, unsupported_remainder);
    assert_eq!(market.quote_side.reserves.live_reserve, unsupported_remainder);
    assert!(market.finalize_amm_transition(11).is_err());
}

#[test]
fn full_public_exit_parks_dust_and_a_later_supported_deposit_rebuilds_the_curve() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let public_supply = market.base_side.shares.ylp_supply - MIN_LIQUIDITY;

    let receipt = market.remove_liquidity(public_supply).unwrap();
    assert_eq!(receipt.ylp_supply, MIN_LIQUIDITY);
    market.finalize_amm_transition_and_observe_risk(11).unwrap();
    assert_eq!(market.amm.invariant_d_nad, 0);
    assert_eq!(market.amm.q_per_share_nad, 0);
    assert_eq!(market.risk, Default::default());

    let deposit = u64::try_from(2 * MIN_INNER_COMMON_RESERVE).unwrap();
    market.add_liquidity(deposit, deposit).unwrap();
    market.finalize_amm_transition(12).unwrap();
    assert!(market.amm.invariant_d_nad > 0);
    assert!(market.amm.q_per_share_nad > 0);
}

#[test]
fn full_public_exit_parking_is_blocked_by_debt_or_active_hlp_state() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let public_supply = market.base_side.shares.ylp_supply - MIN_LIQUIDITY;
    let receipt = market.remove_liquidity(public_supply).unwrap();
    assert_eq!(receipt.ylp_supply, MIN_LIQUIDITY);

    let mut with_debt = market.clone();
    with_debt.debt.fixed_base_shares = 1;
    assert!(with_debt.finalize_amm_transition_and_observe_risk(11).is_err());

    let mut with_hlp = market;
    with_hlp.base_hlp_vault.hlp_supply = 1;
    assert!(with_hlp.finalize_amm_transition_and_observe_risk(11).is_err());
}

#[test]
fn neutral_interest_accrual_does_not_create_protected_profit() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let q_before = market.amm.q_per_share_nad;

    market.debt.fixed_base_shares = 100 * NAD as u128;
    market.debt.fixed_base_principal = 100 * NAD;
    market.debt.base_borrow_index_nad = NAD as u128 + NAD as u128 / 10;
    market.base_side.reserves.live_reserve += 10 * NAD;
    market.checkpoint_amm_neutral_inventory(11).unwrap();

    assert_eq!(market.amm.q_per_share_nad, q_before);
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
}

#[test]
fn retained_surcharge_is_the_only_path_that_creates_budget() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    market.checkpoint_amm_neutral_inventory(10).unwrap();
    market.base_side.reserves.live_reserve += 1_000 * NAD;
    market.base_side.reserves.cash_reserve += 1_000 * NAD;
    market.checkpoint_amm_retained_surcharge(10).unwrap();
    assert!(market.amm.spendable_protected_profit_nad() > 0);
}

#[test]
fn stale_retention_is_released_only_by_a_controller_decision_inside_a_user_path() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let q = market.amm.q_per_share_nad;
    let target = market.amm.refresh_retention_target(q, q / 10_000).unwrap();
    market
        .amm
        .checkpoint_retained_surcharge(q + target.hard_cap_nad)
        .unwrap();
    assert!(market.amm.retention_target_stale);
    assert!(market.amm.retain_dynamic_surcharge);

    market.advance_amm_clock(11).unwrap();
    assert!(market.amm.retain_dynamic_surcharge);

    market.prepare_amm_for_swap(11).unwrap();
    assert!(market.amm.retention_target_stale);
    assert!(market.amm.retain_dynamic_surcharge);

    // A real user operation may clear an obsolete frozen target. Clock/prep
    // alone cannot do so: the controller decision validates that the EMA has
    // returned inside the adjustment threshold before releasing retention.
    market.amm.deferred_controller_target = DeferredControllerTarget {
        kind: DeferredControllerTarget::RECENTER,
        center_price_nad: market.amm.center_price_nad + 1,
        parameters: market.amm.applied_curve_parameters,
        created_slot: 10,
        ..DeferredControllerTarget::default()
    };
    assert!(!market.advance_one_amm_controller_target(11).unwrap());
    assert!(!market.amm.retention_target_stale);
    assert!(!market.amm.retain_dynamic_surcharge);
    market
        .finalize_amm_trade_after_inventory_checkpoint(NAD, NAD, 11)
        .unwrap();
    assert!(market.amm.retention_target_stale);
    assert!(market.amm.retain_dynamic_surcharge);
}

#[test]
fn pending_parameter_ramp_arms_retention_before_lazy_admission() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();

    // Move the inventory away from center through the actual curve, then
    // checkpoint the invariant-preserving trade as neutral.
    let trade = market
        .quote_curve_exact_in(MarketAsset::Base, 400_000 * NAD, 10)
        .unwrap();
    market.base_side.credit_reserve(trade.amount_in, true).unwrap();
    market.quote_side.debit_reserve(trade.amount_out, true).unwrap();
    market.checkpoint_amm_neutral_inventory(10).unwrap();

    let old = market.amm.applied_curve_parameters;
    let mut target = market.config.amm;
    target.peak_depth_nad = MIN_AMM_PEAK_DEPTH_NAD;
    target.fade_scale_nad = MIN_AMM_FADE_SCALE_NAD;
    market.amm.start_concentration_ramp(old, &target, 10).unwrap();
    market.config.amm = target;
    let target_slot = market.amm.concentration_ramp.end_slot;
    let candidate_q = market
        .curve_q_per_share_nad(
            market
                .evaluate_curve_candidate(market.amm.center_price_nad, target.curve_parameters())
                .unwrap()
                .balanced_equivalent_q,
        )
        .unwrap();
    assert!(candidate_q < market.amm.q_per_share_nad);

    market.checkpoint_amm_neutral_inventory(target_slot).unwrap();

    assert!(market.amm.retention_target_stale);
    assert!(market.amm.retain_dynamic_surcharge);
    assert_eq!(market.amm.applied_curve_parameters, old);
}

#[test]
fn cpmm_can_move_the_internal_fee_anchor_without_spending_protection() {
    let mut config = AmmConfig {
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 1_000,
        min_adjustment_interval_slots: 1,
        ..AmmConfig::default()
    };
    config.divergence_fee_coefficient_nad = NAD;
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    let q_before = market.amm.q_per_share_nad;
    market.amm.price_ema_nad = NAD + NAD / 10;

    assert!(market.advance_one_amm_controller_target(11).unwrap());

    assert!(market.amm.center_price_nad > NAD);
    assert_eq!(market.amm.q_per_share_nad, q_before);
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
}

#[test]
fn cpmm_next_user_operation_observes_silence_and_moves_fee_anchor() {
    let mut config = AmmConfig {
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 100,
        min_adjustment_interval_slots: 1,
        volatility_shock_cap_nad: NAD,
        volatility_cap_nad: NAD,
        ..AmmConfig::default()
    };
    config.divergence_fee_coefficient_nad = NAD;
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    market.amm.checkpoint_trade(&config, NAD, 120 * NAD / 100, 10).unwrap();
    let one_half_life_later = 10 + MIN_HALF_LIFE_MS / 400;
    let q_before = market.amm.q_per_share_nad;

    market.prepare_amm_for_swap(one_half_life_later).unwrap();
    let moved = market.advance_one_amm_controller_target(one_half_life_later).unwrap();

    assert!(moved);
    assert!(market.amm.price_ema_nad.abs_diff(110 * NAD / 100) <= 2);
    assert!(market.amm.center_price_nad > NAD);
    assert_eq!(market.amm.q_per_share_nad, q_before);
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
    assert_eq!(market.amm.last_observation_slot, one_half_life_later);

    let after_first_operation = market.amm;
    assert!(!market.advance_one_amm_controller_target(one_half_life_later).unwrap());
    assert_eq!(market.amm, after_first_operation);
}

#[test]
fn same_hybrid_tail_center_move_is_zero_impairment() {
    let mut config = concentrated_config();
    config.adjustment_step_nad = NAD / 1_000;
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    let trade = market
        .quote_curve_exact_in(MarketAsset::Base, 10_000_000 * NAD, 10)
        .unwrap();
    market.base_side.credit_reserve(trade.amount_in, true).unwrap();
    market.quote_side.debit_reserve(trade.amount_out, true).unwrap();
    market.checkpoint_amm_neutral_inventory(10).unwrap();
    let q_before = market.amm.q_per_share_nad;
    let center_before = market.amm.center_price_nad;
    market.amm.price_ema_nad = center_before - center_before / 10;
    let candidate_center = center_step_toward(
        center_before,
        market.amm.price_ema_nad,
        market.config.amm.adjustment_step_nad,
    )
    .unwrap();
    assert!(market
        .recenter_stays_on_same_cpmm_tail(candidate_center, market.amm.applied_curve_parameters)
        .unwrap());

    assert!(market.advance_one_amm_controller_target(11).unwrap());

    assert_eq!(market.amm.center_price_nad, candidate_center);
    assert_eq!(market.amm.q_per_share_nad, q_before);
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
}

#[test]
fn funded_concentrated_recenter_commits_the_exact_canonical_invariant() {
    let mut config = concentrated_config();
    config.adjustment_threshold_nad = MIN_AMM_ADJUSTMENT_NAD;
    config.adjustment_step_nad = MIN_AMM_ADJUSTMENT_NAD;
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    let trade = market
        .quote_curve_exact_in(MarketAsset::Base, 100_000 * NAD, 10)
        .unwrap();
    market.base_side.credit_reserve(trade.amount_in, true).unwrap();
    market.quote_side.debit_reserve(trade.amount_out, true).unwrap();
    market.checkpoint_amm_neutral_inventory(10).unwrap();
    market.amm.price_ema_nad = trade.end_price_nad;

    let candidate_center = center_step_toward(
        market.amm.center_price_nad,
        market.amm.price_ema_nad,
        market.config.amm.adjustment_step_nad,
    )
    .unwrap();
    let expected = market
        .evaluate_amm_liquidity_candidate(candidate_center, market.amm.applied_curve_parameters)
        .unwrap();
    let candidate_q = market.curve_q_per_share_nad(expected.balanced_equivalent_q).unwrap();
    let impairment = market.amm.q_per_share_nad.saturating_sub(candidate_q);
    assert!(impairment > 0);
    let covered = covered_impairment_nad(market.amm.q_per_share_nad, candidate_q).unwrap();
    let target = market
        .amm
        .refresh_retention_target(market.amm.q_per_share_nad, impairment)
        .unwrap();
    assert!(!target.saturated);
    assert_eq!(target.required_nad, covered);
    market.amm.protected_floor_per_share_nad = market.amm.q_per_share_nad.saturating_sub(target.required_nad);

    assert!(market.advance_one_amm_controller_target(11).unwrap());

    assert_eq!(market.amm.center_price_nad, candidate_center);
    assert_eq!(market.amm.invariant_d_nad, expected.invariant_d);
    let mut independently_solved = market.clone();
    independently_solved.amm.clear_invariant();
    let fresh = independently_solved.evaluate_current_curve(11).unwrap();
    assert_eq!(market.amm.invariant_d_nad, fresh.invariant_d);
}

#[test]
fn underfunded_concentrated_recenter_preserves_the_canonical_invariant() {
    let mut config = concentrated_config();
    config.adjustment_threshold_nad = MAX_AMM_ADJUSTMENT_NAD;
    config.adjustment_step_nad = MAX_AMM_ADJUSTMENT_NAD;
    let mut market = market_with_liquidity(config);
    // Increase q/share precision so the single full-step candidate has a
    // measurable positive impairment.
    market.base_side.shares.ylp_supply = NAD;
    market.quote_side.shares.ylp_supply = NAD;
    market.ensure_amm_initialized(10).unwrap();
    let trade = market
        .quote_curve_exact_in(MarketAsset::Base, 100_000 * NAD, 10)
        .unwrap();
    market.base_side.credit_reserve(trade.amount_in, true).unwrap();
    market.quote_side.debit_reserve(trade.amount_out, true).unwrap();
    market.checkpoint_amm_neutral_inventory(10).unwrap();
    market.amm.price_ema_nad = NAD / 2;
    let candidate_center = center_step_toward(
        market.amm.center_price_nad,
        market.amm.price_ema_nad,
        market.config.amm.adjustment_step_nad,
    )
    .unwrap();
    let candidate = market
        .evaluate_amm_liquidity_candidate(candidate_center, market.amm.applied_curve_parameters)
        .unwrap();
    let candidate_q = market.curve_q_per_share_nad(candidate.balanced_equivalent_q).unwrap();
    assert!(candidate_q < market.amm.q_per_share_nad);
    assert!(covered_impairment_nad(market.amm.q_per_share_nad, candidate_q).unwrap() > 0);
    let center_before = market.amm.center_price_nad;
    let q_before = market.amm.q_per_share_nad;
    let last_adjustment_before = market.amm.last_adjustment_slot;
    let invariant_before = market.amm.invariant_d_nad;
    assert!(
        symmetric_distance_nad(market.amm.center_price_nad, market.amm.price_ema_nad).unwrap()
            >= market.config.amm.adjustment_threshold_nad as u128
    );

    // Fund exactly the fixed guard. The full candidate additionally needs its
    // positive impairment covered, so it is frozen without a partial move.
    let guard = mul_bps_ceil(market.amm.q_per_share_nad, PROTECTED_LIQUIDITY_GUARD_BPS).unwrap();
    market.amm.protected_floor_per_share_nad = market.amm.q_per_share_nad - guard;
    reset_amm_liquidity_candidate_solves();

    assert!(!market.advance_one_amm_controller_target(11).unwrap());

    assert_eq!(amm_liquidity_candidate_solves(), 1);
    assert_eq!(
        market.amm.deferred_controller_target.kind,
        DeferredControllerTarget::RECENTER
    );
    assert_eq!(market.amm.center_price_nad, center_before);
    assert_eq!(market.amm.q_per_share_nad, q_before);
    assert_eq!(market.amm.last_adjustment_slot, last_adjustment_before);
    assert_eq!(market.amm.invariant_d_nad, invariant_before);

    // Once a reserve-specific target is proven above the hard impairment cap,
    // accumulating the capped budget cannot make it executable. Do not pay
    // for the same invariant solve on every later operation.
    market.amm.deferred_controller_target.saturated = true;
    let required = market.amm.deferred_controller_target.required_nad;
    market.amm.protected_floor_per_share_nad = market.amm.q_per_share_nad.saturating_sub(required);
    reset_amm_liquidity_candidate_solves();
    assert!(!market.advance_one_amm_controller_target(12).unwrap());
    assert_eq!(amm_liquidity_candidate_solves(), 0);
    assert_eq!(market.amm.center_price_nad, center_before);
    assert_eq!(market.amm.invariant_d_nad, invariant_before);

    // Saturation is a property of the governance-requested move, not a cache
    // freshness hint. Even a material inventory change cannot reactivate it;
    // a config update (or EMA reversal) must clear the frozen request.
    market.base_side.reserves.live_reserve += 10_000;
    market.base_side.reserves.cash_reserve += 10_000;
    reset_amm_liquidity_candidate_solves();
    assert!(!market.advance_one_amm_controller_target(13).unwrap());
    assert_eq!(amm_liquidity_candidate_solves(), 0);
    assert_eq!(market.amm.center_price_nad, center_before);
}

#[test]
fn admitted_parameter_ramp_commits_a_canonical_invariant_matching_a_fresh_solve() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let applied = market.amm.applied_curve_parameters;
    let mut target = market.config.amm;
    target.peak_depth_nad *= 2;
    market.amm.start_concentration_ramp(applied, &target, 10).unwrap();
    market.config.amm = target;

    let desired = market.amm.desired_curve_parameters(&market.config.amm, 11);
    let expected = market
        .evaluate_amm_liquidity_candidate(market.amm.center_price_nad, desired)
        .unwrap();
    let candidate_q = market.curve_q_per_share_nad(expected.balanced_equivalent_q).unwrap();
    let impairment = market.amm.q_per_share_nad.saturating_sub(candidate_q);
    let covered = covered_impairment_nad(market.amm.q_per_share_nad, candidate_q).unwrap();
    let target = market
        .amm
        .refresh_retention_target(market.amm.q_per_share_nad, impairment)
        .unwrap();
    assert!(!target.saturated);
    assert_eq!(target.required_nad, covered);
    market.amm.protected_floor_per_share_nad = market.amm.q_per_share_nad.saturating_sub(target.required_nad);

    assert!(market.advance_one_amm_controller_target(11).unwrap());

    assert_eq!(market.amm.applied_curve_parameters, desired);
    assert_eq!(market.amm.invariant_d_nad, expected.invariant_d);
    let mut independently_solved = market.clone();
    independently_solved.amm.clear_invariant();
    let fresh = independently_solved.evaluate_current_curve(11).unwrap();
    assert_eq!(market.amm.invariant_d_nad, fresh.invariant_d);
}

#[test]
fn final_ramp_admission_and_center_recenter_never_share_one_operation() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let center_before = market.amm.center_price_nad;
    market.amm.price_ema_nad = center_before + center_before / 10;
    market.amm.last_trade_price_nad = market.amm.price_ema_nad;
    market.amm.protected_floor_per_share_nad = 0;

    let applied = market.amm.applied_curve_parameters;
    let mut target = market.config.amm;
    target.peak_depth_nad *= 2;
    market.amm.start_concentration_ramp(applied, &target, 10).unwrap();
    market.config.amm = target;
    let final_slot = market.amm.concentration_ramp.end_slot;

    reset_amm_liquidity_candidate_solves();
    market.prepare_amm_for_swap(final_slot).unwrap();
    let ramp_moved = market.advance_one_amm_controller_target(final_slot).unwrap();

    assert!(ramp_moved);
    assert_eq!(market.amm.applied_curve_parameters, target.curve_parameters());
    assert!(!market.amm.concentration_ramp.active);
    assert_eq!(market.amm.center_price_nad, center_before);
    assert!(amm_liquidity_candidate_solves() <= 1);

    reset_amm_liquidity_candidate_solves();
    assert!(!market.advance_one_amm_controller_target(final_slot).unwrap());
    assert_eq!(market.amm.center_price_nad, center_before);
    assert_eq!(amm_liquidity_candidate_solves(), 0);

    reset_amm_liquidity_candidate_solves();
    market.prepare_amm_for_swap(final_slot + 1).unwrap();
    let center_moved = market.advance_one_amm_controller_target(final_slot + 1).unwrap();
    assert!(center_moved);
    assert!(market.amm.center_price_nad > center_before);
    assert!(amm_liquidity_candidate_solves() <= 1);
}

#[test]
fn trade_finalization_commits_quoted_flow_not_internal_post_trade_price() {
    let mut config = concentrated_config();
    config.volatility_shock_cap_nad = NAD / 2;
    config.volatility_cap_nad = NAD;
    config.volatility_fee_coefficient_nad = NAD;
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();

    let quote = market.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
    assert!(quote.post_success_volatility_nad > 0);
    market.base_side.reserves.live_reserve += quote.fee.reserve_input_credit;
    market.base_side.reserves.cash_reserve += quote.fee.reserve_input_credit;
    market.quote_side.reserves.live_reserve -= quote.amount_out;
    market.quote_side.reserves.cash_reserve -= quote.amount_out;

    // Model an hLP/internal inventory correction after trader execution. It
    // deliberately changes the final marginal price away from the trader's
    // frozen endpoint.
    market.quote_side.reserves.live_reserve += 100_000 * NAD;
    market.quote_side.reserves.cash_reserve += 100_000 * NAD;
    let internal_final_price = market.curve_marginal_price_nad(10).unwrap();
    assert_ne!(internal_final_price, quote.end_price_nad);

    market
        .finalize_amm_trade(quote.start_price_nad, quote.end_price_nad, 10)
        .unwrap();

    assert_eq!(market.amm.volatility_accumulator_nad, quote.post_success_volatility_nad);
    assert_eq!(market.amm.last_trade_price_nad, quote.end_price_nad);
}

#[test]
fn checkpointed_trade_finalization_matches_full_finalization() {
    let post_trade = || {
        let mut market = market_with_liquidity(concentrated_config());
        market.ensure_amm_initialized(10).unwrap();
        let quote = market.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
        market.base_side.reserves.live_reserve += quote.fee.reserve_input_credit;
        market.base_side.reserves.cash_reserve += quote.fee.reserve_input_credit;
        market.quote_side.reserves.live_reserve -= quote.amount_out;
        market.quote_side.reserves.cash_reserve -= quote.amount_out;
        (market, quote)
    };
    let (mut checkpointed, quote) = post_trade();
    let (mut full, full_quote) = post_trade();
    assert_eq!(quote, full_quote);

    checkpointed.checkpoint_amm_neutral_inventory_raw(10).unwrap();
    checkpointed
        .finalize_amm_trade_after_inventory_checkpoint(quote.start_price_nad, quote.end_price_nad, 10)
        .unwrap();
    full.finalize_amm_trade(quote.start_price_nad, quote.end_price_nad, 10)
        .unwrap();

    assert_eq!(checkpointed.amm, full.amm);
}

#[test]
fn trade_finalization_advances_curve_revision_once_and_leaves_unmaterialized_risk_stale() {
    let mut market = market_with_liquidity(concentrated_config());
    market.config.ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.q_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.ensure_amm_initialized(10).unwrap();
    market.refresh_risk().unwrap();
    assert_eq!(market.risk_revision, market.curve_revision);
    let revision_before = market.curve_revision;

    let quote = market.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
    assert_eq!(quote.fee.retained_surcharge, 0);
    market
        .base_side
        .credit_reserve(quote.fee.amount_in_for_quote, true)
        .unwrap();
    market.quote_side.debit_reserve(quote.amount_out, true).unwrap();
    checkpoint_trade_endpoint_like_spot(&mut market, quote.trade_endpoint().unwrap(), 10).unwrap();
    let final_evaluation = quote
        .reserve_endpoint()
        .unwrap()
        .validated_evaluation(&market, 10)
        .unwrap();
    market
        .finalize_amm_trade_after_inventory_checkpoint(quote.start_price_nad, quote.end_price_nad, 10)
        .unwrap();
    market.observe_risk_from_curve_evaluation(final_evaluation, 10).unwrap();

    assert_eq!(market.curve_revision, revision_before + 1);
    assert_eq!(market.risk_revision, revision_before);
    assert_eq!(market.risk.cached_spot_base_price_nad, quote.reserve_end_price_nad);
    assert_eq!(market.last_marginal_observation_nad, quote.reserve_end_price_nad);

    let pre_refresh_ema = market.risk.base_price_ema_nad;
    market.observe_current_risk(20).unwrap();
    assert_ne!(market.risk.base_price_ema_nad, pre_refresh_ema);
    assert_eq!(market.risk_revision, market.curve_revision);
}

#[test]
fn transition_and_exact_observation_keep_curve_and_risk_revisions_separate() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    market.refresh_risk().unwrap();
    let revision_before = market.curve_revision;

    market.base_side.reserves.live_reserve += NAD;
    market.base_side.reserves.cash_reserve += NAD;
    market.finalize_amm_transition(11).unwrap();
    assert_eq!(market.curve_revision, revision_before + 1);
    assert_eq!(market.risk_revision, revision_before);

    market.observe_current_risk(11).unwrap();
    assert_eq!(market.risk_revision, market.curve_revision);
    let observed_revision = market.curve_revision;

    market.quote_side.reserves.live_reserve += NAD;
    market.quote_side.reserves.cash_reserve += NAD;
    market.finalize_amm_transition_and_observe_risk(12).unwrap();
    assert_eq!(market.curve_revision, observed_revision + 1);
    assert_eq!(market.risk_revision, market.curve_revision);
}

#[test]
fn trade_endpoint_checkpoint_matches_a_fresh_curve_solve() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let quote = market.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
    assert_eq!(quote.fee.retained_surcharge, 0);

    let mut checkpointed = market.clone();
    checkpointed
        .base_side
        .credit_reserve(quote.fee.amount_in_for_quote, true)
        .unwrap();
    checkpointed.quote_side.debit_reserve(quote.amount_out, true).unwrap();
    let mut fresh = checkpointed.clone();

    checkpoint_trade_endpoint_like_spot(&mut checkpointed, quote.trade_endpoint().unwrap(), 10).unwrap();
    fresh.checkpoint_amm_neutral_inventory_raw(10).unwrap();
    assert_eq!(checkpointed.amm, fresh.amm);

    assert!(checkpointed
        .try_observe_risk_from_curve_checkpoint(quote.reserve_endpoint().unwrap(), 10,)
        .unwrap());
    fresh.observe_current_risk(10).unwrap();
    assert_eq!(checkpointed.risk, fresh.risk);
}

#[test]
fn authoritative_start_checkpoint_reuse_is_exactly_differential() {
    let mut config = concentrated_config();
    config.divergence_fee_coefficient_nad = 10 * NAD;

    for retain_dynamic_surcharge in [false, true] {
        for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = market_with_liquidity(config);
            market.ensure_amm_initialized(10).unwrap();
            market.amm.retain_dynamic_surcharge = retain_dynamic_surcharge;
            if retain_dynamic_surcharge {
                market.defer_amm_retention_target().unwrap();
            }
            let reserve_credit = 50_000 * NAD;
            let current_slot = 10;
            let reserves = market.curve_reserves_nad().unwrap();
            let pre_state = market.dynamic_fee_pre_state(current_slot).unwrap();
            let preliminary = market
                .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
                .unwrap();

            let ordinary = market
                .quote_amm_swap_for_reserves_nad(
                    asset_in,
                    reserve_credit,
                    current_slot,
                    reserves,
                    pre_state,
                    preliminary,
                )
                .unwrap();
            let mut start_checkpoint = None;
            let reused = market
                .quote_amm_swap_for_reserves_nad_with_start(
                    asset_in,
                    reserve_credit,
                    current_slot,
                    reserves,
                    pre_state,
                    preliminary,
                    Some(&mut start_checkpoint),
                )
                .unwrap();

            assert_eq!(reused, ordinary);
            assert_eq!(
                start_checkpoint.unwrap().evaluation().marginal_price_nad,
                reused.start_price_nad as u128
            );
        }
    }
}

#[test]
fn authoritative_start_checkpoint_reuse_preserves_exact_error_order() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let reserve_credit = 1;
    let current_slot = 10;
    let reserves = market.curve_reserves_nad().unwrap();
    let pre_state = market.dynamic_fee_pre_state(current_slot).unwrap();
    let preliminary = market
        .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
        .unwrap();

    let ordinary_error = market
        .quote_amm_swap_for_reserves_nad(
            MarketAsset::Base,
            reserve_credit,
            current_slot,
            reserves,
            pre_state,
            preliminary,
        )
        .unwrap_err();
    let mut start_checkpoint = None;
    let reused_error = market
        .quote_amm_swap_for_reserves_nad_with_start(
            MarketAsset::Base,
            reserve_credit,
            current_slot,
            reserves,
            pre_state,
            preliminary,
            Some(&mut start_checkpoint),
        )
        .unwrap_err();

    assert_eq!(reused_error, ordinary_error);
    assert_eq!(reused_error, error!(ErrorCode::InsufficientOutputAmount));
    assert!(start_checkpoint.is_some());
}

#[test]
fn retained_endpoint_checkpoint_matches_two_fresh_curve_solves() {
    let mut config = concentrated_config();
    config.divergence_fee_coefficient_nad = 10 * NAD;
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    let mut distributed = market.clone();
    distributed.amm.retain_dynamic_surcharge = false;
    crate::math::reset_residual_evaluations();
    distributed.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
    let distributed_evaluations = crate::math::residual_evaluations();
    market.amm.retain_dynamic_surcharge = true;
    market.defer_amm_retention_target().unwrap();
    crate::math::reset_residual_evaluations();
    let quote = market.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
    let retained_evaluations = crate::math::residual_evaluations();
    assert!(
        retained_evaluations <= distributed_evaluations + 6,
        "distributed={distributed_evaluations}, retained={retained_evaluations}"
    );
    assert!(quote.fee.retained_surcharge > 0);

    let mut checkpointed = market.clone();
    checkpointed
        .base_side
        .credit_reserve(quote.fee.amount_in_for_quote, true)
        .unwrap();
    checkpointed.quote_side.debit_reserve(quote.amount_out, true).unwrap();
    let mut fresh = checkpointed.clone();

    checkpoint_trade_endpoint_like_spot(&mut checkpointed, quote.trade_endpoint().unwrap(), 10).unwrap();
    fresh.checkpoint_amm_neutral_inventory_raw(10).unwrap();
    checkpointed
        .base_side
        .credit_reserve(quote.fee.retained_surcharge, true)
        .unwrap();
    fresh
        .base_side
        .credit_reserve(quote.fee.retained_surcharge, true)
        .unwrap();
    let evaluation = quote
        .reserve_endpoint()
        .unwrap()
        .validated_evaluation(&checkpointed, 10)
        .unwrap();
    let q_per_share_nad = checkpointed
        .curve_q_per_share_nad(evaluation.balanced_equivalent_q)
        .unwrap();
    checkpointed.amm.commit_invariant(evaluation.invariant_d).unwrap();
    checkpointed.amm.checkpoint_retained_surcharge(q_per_share_nad).unwrap();
    fresh.checkpoint_amm_retained_surcharge_raw(10).unwrap();

    assert_eq!(checkpointed.amm, fresh.amm);
    assert!(checkpointed
        .try_observe_risk_from_curve_checkpoint(quote.reserve_endpoint().unwrap(), 10,)
        .unwrap());
    fresh.observe_current_risk(10).unwrap();
    assert_eq!(checkpointed.risk, fresh.risk);
}

#[test]
fn partially_enabled_runtime_curve_is_rejected() {
    assert!(ConcentrationParameters {
        peak_depth_nad: 1,
        fade_scale_nad: 0,
    }
    .validate_runtime()
    .is_err());
    assert!(ConcentrationParameters {
        peak_depth_nad: 0,
        fade_scale_nad: MIN_AMM_FADE_SCALE_NAD,
    }
    .validate_runtime()
    .is_err());
}

#[test]
fn risk_shape_reconstruction_tracks_the_latest_exact_scalar_snapshot() {
    let mut market = market_with_liquidity(concentrated_config());
    market.config.ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.q_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.ensure_amm_initialized(10).unwrap();
    market.last_update_slot = 10;
    market.refresh_risk().unwrap();
    assert_eq!(market.risk_revision, market.curve_revision);

    let trade = market
        .quote_curve_exact_in(MarketAsset::Base, 100_000 * NAD, 11)
        .unwrap();
    market.base_side.credit_reserve(trade.amount_in, true).unwrap();
    market.quote_side.debit_reserve(trade.amount_out, true).unwrap();
    market.finalize_amm_transition_and_observe_risk(11).unwrap();

    // The first observation stores the new spot. One later slot lets that
    // observation enter the symmetric/directional EMAs and change the cached
    // pessimistic shapes.
    let evaluation = market.evaluate_current_curve(12).unwrap();
    let base_price_nad = u64::try_from(evaluation.marginal_price_nad).unwrap();
    let quote_price_nad = u64::try_from(
        (NAD as u128)
            .checked_mul(NAD as u128)
            .unwrap()
            .checked_div(base_price_nad as u128)
            .unwrap(),
    )
    .unwrap();
    let expected_risk = market
        .risk
        .refreshed(
            base_price_nad,
            quote_price_nad,
            evaluation.balanced_equivalent_q,
            &market.config,
            12,
        )
        .unwrap();
    market.finalize_amm_transition_and_observe_risk(12).unwrap();

    assert_eq!(market.risk, expected_risk);
    let reconstructed_before_refresh = market
        .pessimistic_virtual_reserves_nad(MarketAsset::Base, &market.risk, true)
        .unwrap();

    market.refresh_risk().unwrap();
    assert_eq!(market.risk_revision, market.curve_revision);
    let reconstructed_after_refresh = market
        .pessimistic_virtual_reserves_nad(MarketAsset::Base, &market.risk, true)
        .unwrap();
    assert_eq!(reconstructed_before_refresh, reconstructed_after_refresh);
}

}

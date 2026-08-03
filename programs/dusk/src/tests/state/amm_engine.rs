use super::*;
use crate::{
    constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS, MIN_LIQUIDITY},
    math::MIN_INNER_COMMON_RESERVE,
    state::{
        AmmConfig, Debt, MarketAsset, MarketConfig, MarketSide, Reserves, MIN_AMM_IMBALANCE_SCALE_NAD,
        MIN_AMM_PEAK_DEPTH_NAD,
    },
};

fn concentrated_config() -> AmmConfig {
    AmmConfig {
        peak_depth_nad: 200 * NAD,
        imbalance_scale_nad: NAD / 10,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 1_000,
        min_adjustment_interval_slots: 1,
        ramp_duration_slots: super::super::MIN_AMM_RAMP_DURATION_SLOTS,
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
            shares: super::super::ReserveShares {
                ylp_supply: reserve,
                ..super::super::ReserveShares::default()
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
            shares: super::super::ReserveShares {
                ylp_supply: reserve,
                ..super::super::ReserveShares::default()
            },
            ..MarketSide::default()
        },
        config: MarketConfig {
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
    market.park_amm_after_full_public_liquidity_exit(11).unwrap();
    assert_eq!(market.amm.invariant_d_nad, 0);
    assert_eq!(market.amm.invariant_d_high_nad, 0);
    assert_eq!(market.amm.q_per_share_nad, 0);
    assert_eq!(market.risk, Default::default());

    let deposit = u64::try_from(2 * MIN_INNER_COMMON_RESERVE).unwrap();
    market.add_liquidity(deposit, deposit).unwrap();
    market.finalize_amm_transition(12).unwrap();
    assert!(market.amm.invariant_d_nad > 0);
    assert!(market.amm.invariant_d_high_nad >= market.amm.invariant_d_nad);
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
    assert!(with_debt.park_amm_after_full_public_liquidity_exit(11).is_err());

    let mut with_hlp = market;
    with_hlp.base_hlp_vault.hlp_supply = 1;
    assert!(with_hlp.park_amm_after_full_public_liquidity_exit(11).is_err());
}

#[test]
fn neutral_interest_accrual_does_not_create_protected_profit() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let q_before = market.amm.q_per_share_nad;

    market.debt.fixed_base_shares = 100 * NAD as u128;
    market.debt.fixed_base_principal = 100 * NAD as u128;
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
fn stale_retention_probe_is_released_only_for_an_actual_swap_path() {
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
    assert!(!market.amm.retain_dynamic_surcharge);
    market
        .finalize_amm_trade_after_inventory_checkpoint(NAD, NAD, 11)
        .unwrap();
    assert!(market.amm.retention_target_stale);
    assert!(market.amm.retain_dynamic_surcharge);
}

#[test]
fn pending_parameter_ramp_arms_retention_before_maintenance_admission() {
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
    target.imbalance_scale_nad = MIN_AMM_IMBALANCE_SCALE_NAD;
    market.amm.start_applied_ramp(old, &target, 10).unwrap();
    market.config.amm = target;
    let target_slot = market.amm.ramp.end_slot;
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

    market.maybe_recenter_amm(11).unwrap();

    assert!(market.amm.center_price_nad > NAD);
    assert_eq!(market.amm.q_per_share_nad, q_before);
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
}

#[test]
fn cpmm_public_maintenance_path_observes_silence_and_moves_fee_anchor() {
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

    let moved = market.crank_concentrated_amm_with_hlp(one_half_life_later).unwrap();

    assert!(moved);
    assert!(market.amm.price_ema_nad.abs_diff(110 * NAD / 100) <= 2);
    assert!(market.amm.center_price_nad > NAD);
    assert_eq!(market.amm.q_per_share_nad, q_before);
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
    assert_eq!(market.amm.last_observation_slot, one_half_life_later);

    let after_first_crank = market.amm;
    assert!(!market.crank_concentrated_amm_with_hlp(one_half_life_later).unwrap());
    assert_eq!(market.amm, after_first_crank);
}

#[test]
fn same_hybrid_tail_center_move_is_zero_impairment() {
    let mut config = concentrated_config();
    config.adjustment_step_nad = NAD / 1_000;
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    let trade = market
        .quote_curve_exact_in(MarketAsset::Base, 1_000_000 * NAD, 10)
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

    market.maybe_recenter_amm(11).unwrap();

    assert_eq!(market.amm.center_price_nad, candidate_center);
    assert_eq!(market.amm.q_per_share_nad, q_before);
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
}

#[test]
fn funded_concentrated_recenter_commits_the_exact_candidate_bracket() {
    let mut config = concentrated_config();
    config.adjustment_threshold_nad = super::super::MIN_AMM_ADJUSTMENT_NAD;
    config.adjustment_step_nad = super::super::MIN_AMM_ADJUSTMENT_NAD;
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

    market.maybe_recenter_amm(11).unwrap();

    assert_eq!(market.amm.center_price_nad, candidate_center);
    assert_eq!(market.amm.invariant_d_nad, expected.invariant_d);
    assert_eq!(market.amm.invariant_d_high_nad, expected.invariant_d_high);
    let mut independently_solved = market.clone();
    independently_solved.amm.clear_invariant_bracket();
    let fresh = independently_solved.evaluate_current_curve(11).unwrap();
    assert_eq!(market.amm.invariant_d_nad, fresh.invariant_d);
    assert_eq!(market.amm.invariant_d_high_nad, fresh.invariant_d_high);
}

#[test]
fn underfunded_concentrated_recenter_preserves_both_bracket_endpoints() {
    let mut config = concentrated_config();
    config.adjustment_threshold_nad = super::super::MAX_AMM_ADJUSTMENT_NAD;
    config.adjustment_step_nad = super::super::MAX_AMM_ADJUSTMENT_NAD;
    let mut market = market_with_liquidity(config);
    // Increase q/share precision so every one of the nine deterministic
    // halving candidates has a measurable positive impairment.
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
    for halving in 0..=MAX_FUNDED_STEP_HALVINGS {
        let step = market.config.amm.adjustment_step_nad >> halving;
        let candidate_center = center_step_toward(market.amm.center_price_nad, market.amm.price_ema_nad, step).unwrap();
        let candidate = market
            .evaluate_amm_liquidity_candidate(candidate_center, market.amm.applied_curve_parameters)
            .unwrap();
        let candidate_q = market.curve_q_per_share_nad(candidate.balanced_equivalent_q).unwrap();
        assert!(
            candidate_q < market.amm.q_per_share_nad,
            "halving {halving}: current_q={}, candidate_q={candidate_q}, center={}, candidate_center={candidate_center}",
            market.amm.q_per_share_nad,
            market.amm.center_price_nad,
        );
        assert!(covered_impairment_nad(market.amm.q_per_share_nad, candidate_q).unwrap() > 0);
    }
    let center_before = market.amm.center_price_nad;
    let q_before = market.amm.q_per_share_nad;
    let last_adjustment_before = market.amm.last_adjustment_slot;
    let bracket_before = (market.amm.invariant_d_nad, market.amm.invariant_d_high_nad);
    assert!(
        symmetric_distance_nad(market.amm.center_price_nad, market.amm.price_ema_nad).unwrap()
            >= market.config.amm.adjustment_threshold_nad as u128
    );

    // Fund exactly the fixed guard. Every candidate additionally needs its
    // positive impairment covered, forcing the complete nine-candidate search
    // without admitting any point.
    let guard = mul_bps_ceil(market.amm.q_per_share_nad, PROTECTED_LIQUIDITY_GUARD_BPS).unwrap();
    market.amm.protected_floor_per_share_nad = market.amm.q_per_share_nad - guard;
    reset_amm_liquidity_candidate_solves();

    market.maybe_recenter_amm(11).unwrap();

    assert_eq!(amm_liquidity_candidate_solves(), MAX_FUNDED_CANDIDATE_SOLVES);
    assert_eq!(market.amm.center_price_nad, center_before);
    assert_eq!(market.amm.q_per_share_nad, q_before);
    assert_eq!(market.amm.last_adjustment_slot, last_adjustment_before);
    assert_eq!(
        (market.amm.invariant_d_nad, market.amm.invariant_d_high_nad),
        bracket_before
    );
}

#[test]
fn admitted_parameter_ramp_commits_a_bracket_matching_a_fresh_solve() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let applied = market.amm.applied_curve_parameters;
    let mut target = market.config.amm;
    target.peak_depth_nad *= 2;
    market.amm.start_applied_ramp(applied, &target, 10).unwrap();
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

    market.advance_funded_amm_ramp(11).unwrap();

    assert_eq!(market.amm.applied_curve_parameters, desired);
    assert_eq!(market.amm.invariant_d_nad, expected.invariant_d);
    assert_eq!(market.amm.invariant_d_high_nad, expected.invariant_d_high);
    let mut independently_solved = market.clone();
    independently_solved.amm.clear_invariant_bracket();
    let fresh = independently_solved.evaluate_current_curve(11).unwrap();
    assert_eq!(market.amm.invariant_d_nad, fresh.invariant_d);
    assert_eq!(market.amm.invariant_d_high_nad, fresh.invariant_d_high);
}

#[test]
fn final_ramp_admission_and_center_recenter_never_share_one_crank() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let center_before = market.amm.center_price_nad;
    market.amm.price_ema_nad = center_before + center_before / 10;
    market.amm.last_trade_price_nad = market.amm.price_ema_nad;
    market.amm.protected_floor_per_share_nad = 0;

    let applied = market.amm.applied_curve_parameters;
    let mut target = market.config.amm;
    target.peak_depth_nad *= 2;
    market.amm.start_applied_ramp(applied, &target, 10).unwrap();
    market.config.amm = target;
    let final_slot = market.amm.ramp.end_slot;

    reset_amm_liquidity_candidate_solves();
    let ramp_moved = market.crank_concentrated_amm_with_hlp(final_slot).unwrap();

    assert!(ramp_moved);
    assert_eq!(market.amm.applied_curve_parameters, target.curve_parameters());
    assert!(!market.amm.ramp.active);
    assert_eq!(market.amm.center_price_nad, center_before);
    assert!(amm_liquidity_candidate_solves() <= MAX_FUNDED_CANDIDATE_SOLVES);

    reset_amm_liquidity_candidate_solves();
    let center_moved = market.crank_concentrated_amm_with_hlp(final_slot + 1).unwrap();
    assert!(center_moved);
    assert!(market.amm.center_price_nad > center_before);
    assert!(amm_liquidity_candidate_solves() <= MAX_FUNDED_CANDIDATE_SOLVES);
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
fn certified_trade_endpoint_checkpoint_matches_a_fresh_curve_solve() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let quote = market.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
    assert_eq!(quote.fee.retained_surcharge, 0);

    let mut certified = market.clone();
    certified
        .base_side
        .credit_reserve(quote.fee.amount_in_for_quote, true)
        .unwrap();
    certified.quote_side.debit_reserve(quote.amount_out, true).unwrap();
    let mut fresh = certified.clone();

    certified
        .checkpoint_amm_neutral_inventory_from_certificate(quote.trade_endpoint_certificate().unwrap(), 10)
        .unwrap();
    fresh.checkpoint_amm_neutral_inventory_raw(10).unwrap();
    assert_eq!(certified.amm, fresh.amm);

    assert!(certified
        .try_observe_risk_from_curve_certificate(quote.reserve_endpoint_certificate().unwrap(), 10,)
        .unwrap());
    fresh.observe_current_risk(10).unwrap();
    assert_eq!(certified.risk, fresh.risk);
}

#[test]
fn certified_retained_endpoint_checkpoint_matches_two_fresh_curve_solves() {
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
    crate::math::reset_residual_evaluations();
    let quote = market.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
    let retained_evaluations = crate::math::residual_evaluations();
    assert!(retained_evaluations <= distributed_evaluations + 6);
    assert!(quote.fee.retained_surcharge > 0);

    let mut certified = market.clone();
    certified
        .base_side
        .credit_reserve(quote.fee.amount_in_for_quote, true)
        .unwrap();
    certified.quote_side.debit_reserve(quote.amount_out, true).unwrap();
    let mut fresh = certified.clone();

    certified
        .checkpoint_amm_neutral_inventory_from_certificate(quote.trade_endpoint_certificate().unwrap(), 10)
        .unwrap();
    fresh.checkpoint_amm_neutral_inventory_raw(10).unwrap();
    certified
        .base_side
        .credit_reserve(quote.fee.retained_surcharge, true)
        .unwrap();
    fresh
        .base_side
        .credit_reserve(quote.fee.retained_surcharge, true)
        .unwrap();
    certified
        .checkpoint_amm_retained_surcharge_from_certificate(quote.reserve_endpoint_certificate().unwrap(), 10)
        .unwrap();
    fresh.checkpoint_amm_retained_surcharge_raw(10).unwrap();

    assert_eq!(certified.amm, fresh.amm);
    assert!(certified
        .try_observe_risk_from_curve_certificate(quote.reserve_endpoint_certificate().unwrap(), 10,)
        .unwrap());
    fresh.observe_current_risk(10).unwrap();
    assert_eq!(certified.risk, fresh.risk);
}

#[test]
fn halved_runtime_candidate_never_exposes_half_enabled_curve() {
    let candidate = interpolate_parameters(
        AmmCurveParameters::cpmm(),
        AmmCurveParameters {
            peak_depth_nad: 1,
            imbalance_scale_nad: 1,
        },
        1,
        2,
    );
    assert_eq!(candidate, AmmCurveParameters::cpmm());
    candidate.validate_runtime().unwrap();
}

#[test]
fn exact_risk_observation_preserves_an_identical_shape_cache() {
    let mut market = market_with_liquidity(concentrated_config());
    market.config.ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.q_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.ensure_amm_initialized(10).unwrap();
    market.last_update_slot = 10;
    market.refresh_risk().unwrap();
    let cached_shapes = market.amm.risk_curve_cache;
    assert!(cached_shapes.is_initialized());

    let evaluation = market.evaluate_current_curve(10).unwrap();
    market.observe_risk_from_curve_evaluation(evaluation, 10).unwrap();

    assert_eq!(market.amm.risk_curve_cache, cached_shapes);
}

#[test]
fn exact_risk_observation_invalidates_stale_shapes_and_lazy_reconstruction_matches_rebuild() {
    let mut market = market_with_liquidity(concentrated_config());
    market.config.ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.q_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.ensure_amm_initialized(10).unwrap();
    market.last_update_slot = 10;
    market.refresh_risk().unwrap();
    assert!(market.amm.risk_curve_cache.is_initialized());

    let trade = market
        .quote_curve_exact_in(MarketAsset::Base, 100_000 * NAD, 11)
        .unwrap();
    market.base_side.credit_reserve(trade.amount_in, true).unwrap();
    market.quote_side.debit_reserve(trade.amount_out, true).unwrap();
    market.checkpoint_amm_neutral_inventory_and_observe_risk(11).unwrap();

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
    market.checkpoint_amm_neutral_inventory_and_observe_risk(12).unwrap();

    assert_eq!(market.risk, expected_risk);
    assert!(!market.amm.risk_curve_cache.is_initialized());
    let lazy_reserves = market
        .pessimistic_virtual_reserves_nad(MarketAsset::Base, &market.risk, true)
        .unwrap();

    market.refresh_risk().unwrap();
    assert!(market.amm.risk_curve_cache.is_initialized());
    let cached_reserves = market
        .pessimistic_virtual_reserves_nad(MarketAsset::Base, &market.risk, true)
        .unwrap();
    assert_eq!(lazy_reserves, cached_reserves);
}

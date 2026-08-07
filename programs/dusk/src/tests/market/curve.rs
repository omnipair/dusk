use super::*;

#[test]
fn unrealized_interest_aggregates_multiple_u64_max_principals_in_u128() {
    let mut market = market_with_reserves(u64::MAX, u64::MAX);
    market.debt.base_borrow_index_nad = 2 * NAD as u128;
    market.debt.fixed_base_shares = u64::MAX as u128;
    market.debt.isolated_base_shares = u64::MAX as u128;
    market.debt.fixed_base_principal = u64::MAX;
    market.debt.isolated_base_principal = u64::MAX;

    assert_eq!(
        market.unrealized_interest(MarketAsset::Base).unwrap(),
        2 * u64::MAX as u128
    );
}
use crate::{
    constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_LAYOUT_VERSION},
    math::{reset_residual_evaluations, residual_evaluations},
    state::{AmmConfig, AmmState, Debt, MarketConfig, MarketSide, Reserves},
};

fn market_with_reserves(base: u64, quote: u64) -> Market {
    let mut market = Market {
        version: MARKET_LAYOUT_VERSION,
        base_side: MarketSide {
            asset_decimals: 9,
            reserves: Reserves {
                live_reserve: base,
                cash_reserve: base,
                ..Reserves::default()
            },
            ..MarketSide::default()
        },
        quote_side: MarketSide {
            asset_decimals: 9,
            reserves: Reserves {
                live_reserve: quote,
                cash_reserve: quote,
                ..Reserves::default()
            },
            ..MarketSide::default()
        },
        config: MarketConfig {
            amm: AmmConfig::default(),
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
    };
    market.base_side.shares.ylp_supply = base.min(quote);
    market.quote_side.shares.ylp_supply = base.min(quote);
    market
}

#[test]
fn executable_curve_reserve_excludes_fixed_and_isolated_unrealized_interest() {
    let mut market = market_with_reserves(1_100 * NAD, 2_000 * NAD);
    market.debt.base_borrow_index_nad = 2 * NAD as u128;
    market.debt.fixed_base_shares = 100 * NAD as u128;
    market.debt.fixed_base_principal = 150 * NAD;
    market.debt.isolated_base_shares = 25 * NAD as u128;
    market.debt.isolated_base_principal = 40 * NAD;

    // Fixed debt=200, interest=50. Isolated debt=50, interest=10.
    assert_eq!(market.unrealized_interest(MarketAsset::Base).unwrap(), 60 * NAD as u128);
    assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), 1_040 * NAD);
}

#[test]
fn principal_above_rounded_debt_is_clamped_not_subtracted() {
    let mut market = market_with_reserves(1_000 * NAD, 1_000 * NAD);
    market.debt.fixed_base_shares = 10;
    market.debt.fixed_base_principal = 11;
    assert_eq!(market.unrealized_interest(MarketAsset::Base).unwrap(), 0);
    assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), 1_000 * NAD);
}

#[test]
fn uninitialized_cpmm_quote_matches_raw_constant_product() {
    let market = market_with_reserves(1_000 * NAD, 2_000 * NAD);
    let input = 10 * NAD;
    let quote = market.quote_curve_exact_in(MarketAsset::Base, input, 0).unwrap();
    let expected = crate::math::calculate_raw_amount_out(1_000 * NAD, 2_000 * NAD, input).unwrap();
    assert_eq!(quote.amount_out, expected);
    assert_eq!(quote.start_price_nad, 2 * NAD);
}

#[test]
fn cpmm_keeps_legacy_raw_quotes_when_normalized_reserves_exceed_u64() {
    let reserve = 1_000_000_000_000_u64;
    let input = 100_000_000_000_u64;
    let mut market = market_with_reserves(reserve, reserve);
    market.base_side.asset_decimals = 0;
    market.quote_side.asset_decimals = 0;
    market.ensure_amm_initialized(0).unwrap();

    let reserves = market.curve_reserves_nad().unwrap();
    assert!(reserves.base > u64::MAX as u128);
    assert!(reserves.quote > u64::MAX as u128);

    let quote = market.quote_curve_exact_in(MarketAsset::Base, input, 0).unwrap();
    let expected = u64::try_from((input as u128) * (reserve as u128) / ((reserve as u128) + (input as u128))).unwrap();
    assert_eq!(quote.amount_out, expected);

    market.base_side.credit_reserve(quote.amount_in, true).unwrap();
    market.quote_side.debit_reserve(quote.amount_out, true).unwrap();
    let cached = quote.endpoint.validated_evaluation(&market, 0).unwrap();
    assert_eq!(cached, market.evaluate_current_curve(0).unwrap());
}

#[test]
fn initialized_concentrated_curve_improves_near_center_depth() {
    let mut concentrated = market_with_reserves(1_000 * NAD, 1_000 * NAD);
    concentrated.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 1_000,
        min_adjustment_interval_slots: 1,
        ..AmmConfig::default()
    };
    concentrated.amm = AmmState::initialize(&concentrated.config.amm, NAD, NAD as u128, 0).unwrap();
    let cpmm = market_with_reserves(1_000 * NAD, 1_000 * NAD);
    let input = 10 * NAD;

    let concentrated_out = concentrated
        .quote_curve_exact_in(MarketAsset::Base, input, 0)
        .unwrap()
        .amount_out;
    let cpmm_out = cpmm
        .quote_curve_exact_in(MarketAsset::Base, input, 0)
        .unwrap()
        .amount_out;
    assert!(concentrated_out > cpmm_out);
}

#[test]
fn small_concentrated_pool_uses_the_same_canonical_marginal_as_execution() {
    let mut market = market_with_reserves(1_000_000, 2_000_000);
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    market.base_side.shares.ylp_supply = 1_414_213;
    market.quote_side.shares.ylp_supply = 1_414_213;
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        ..AmmConfig::default()
    };

    market.ensure_amm_initialized(1).unwrap();
    assert_eq!(market.amm.center_price_nad, 2 * NAD);
    market.refresh_risk().unwrap();
    let marginal = market.curve_marginal_price_nad(1).unwrap() as u128;
    let probe = 1_000_u64;
    let quote = market.quote_curve_exact_in(MarketAsset::Base, probe, 1).unwrap();
    let executable_average = (quote.amount_out as u128) * NAD as u128 / probe as u128;
    // One raw quote atom is a visible price quantum in a deliberately tiny
    // pool. Account for that unavoidable output floor in addition to 0.1%
    // finite-trade impact.
    let output_floor_quantum = NAD as u128 / probe as u128;
    let tolerance = marginal / 1_000 + output_floor_quantum + 10;
    assert!(
        marginal.abs_diff(executable_average) <= tolerance,
        "marginal={marginal}, executable_average={executable_average}, output={}, tolerance={tolerance}",
        quote.amount_out,
    );
}

#[test]
fn concentrated_endpoint_checkpoint_uses_the_raw_rounded_output_coordinate() {
    let mut market = market_with_reserves(1_000 * NAD, 2_000 * 1_000_000);
    market.quote_side.asset_decimals = 6;
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        ..AmmConfig::default()
    };
    market.ensure_amm_initialized(10).unwrap();
    assert_eq!(market.amm.center_price_nad, 2 * NAD);

    let amount_in = 7_123_456_789;
    let reserves = market.curve_reserves_nad().unwrap();
    let prepared = market
        .prepare_curve_for_reserves_nad(reserves, market.amm.center_price_nad, 10)
        .unwrap();
    let solved_output_nad = prepared
        .quote_exact_in(
            normalize_to_nad(amount_in as u128, market.base_side.asset_decimals).unwrap(),
            direction(MarketAsset::Base),
        )
        .unwrap();
    let quote = market.quote_curve_exact_in(MarketAsset::Base, amount_in, 10).unwrap();
    let executable_output_nad = normalize_to_nad(quote.amount_out as u128, market.quote_side.asset_decimals).unwrap();

    // Six-decimal output execution discards sub-micro-token solver dust.
    assert!(solved_output_nad > executable_output_nad);

    market.base_side.credit_reserve(quote.amount_in, true).unwrap();
    market.quote_side.debit_reserve(quote.amount_out, true).unwrap();
    let checkpoint = quote.endpoint;
    let cached = checkpoint.validated_evaluation(&market, 10).unwrap();
    let fresh = market.evaluate_current_curve(10).unwrap();

    assert_eq!(cached, fresh);
    assert_eq!(quote.end_price_nad, fresh.marginal_price_nad as u64);
}

#[test]
fn endpoint_checkpoint_rejects_any_reserve_center_or_parameter_mismatch() {
    let mut market = market_with_reserves(1_000 * NAD, 1_000 * NAD);
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        ..AmmConfig::default()
    };
    market.ensure_amm_initialized(10).unwrap();
    let quote = market.quote_curve_exact_in(MarketAsset::Base, 10 * NAD, 10).unwrap();
    market.base_side.credit_reserve(quote.amount_in, true).unwrap();
    market.quote_side.debit_reserve(quote.amount_out, true).unwrap();
    let checkpoint = quote.endpoint;
    assert!(checkpoint.evaluation_if_matches(&market, 10).unwrap().is_some());

    market.base_side.reserves.live_reserve += 1;
    market.base_side.reserves.cash_reserve += 1;
    assert!(checkpoint.evaluation_if_matches(&market, 10).unwrap().is_none());
    market.base_side.reserves.live_reserve -= 1;
    market.base_side.reserves.cash_reserve -= 1;

    market.amm.center_price_nad += 1;
    assert!(checkpoint.evaluation_if_matches(&market, 10).unwrap().is_none());
    market.amm.center_price_nad -= 1;

    market.amm.applied_curve_parameters.peak_depth_nad += 1;
    assert!(checkpoint.evaluation_if_matches(&market, 10).unwrap().is_none());
}

#[test]
fn prepared_curve_verifies_the_persisted_invariant_as_a_hint() {
    let mut market = market_with_reserves(1_000 * NAD, 2_000 * NAD);
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        ..AmmConfig::default()
    };
    market.ensure_amm_initialized(10).unwrap();
    let reserves = market.curve_reserves_nad().unwrap();

    reset_residual_evaluations();
    let restored = market
        .prepare_curve_for_reserves_nad(reserves, market.amm.center_price_nad, 10)
        .unwrap();
    assert_eq!(restored.invariant_d(), market.amm.invariant_d_nad);

    let overlay = CurveReservesNad {
        // A changed reserve state may still reuse the old D only as a starting
        // point; the canonical bracket remains authoritative.
        base: reserves.base + NAD as u128,
        ..reserves
    };
    reset_residual_evaluations();
    market
        .prepare_curve_for_reserves_nad(overlay, market.amm.center_price_nad, 10)
        .unwrap();
    assert!(residual_evaluations() > 0);
}

#[test]
fn malformed_persisted_invariant_cannot_change_the_canonical_result() {
    let mut market = market_with_reserves(1_000 * NAD, 2_000 * NAD);
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        ..AmmConfig::default()
    };
    market.ensure_amm_initialized(10).unwrap();
    let mut reference = market.clone();
    reference.amm.invariant_d_nad = 0;
    market.amm.invariant_d_nad = u128::MAX;
    let reserves = market.curve_reserves_nad().unwrap();

    let recovered = market
        .prepare_curve_for_reserves_nad(reserves, market.amm.center_price_nad, 10)
        .unwrap();
    let canonical = reference
        .prepare_curve_for_reserves_nad(reserves, reference.amm.center_price_nad, 10)
        .unwrap();
    assert_eq!(recovered.invariant_d(), canonical.invariant_d());
}

#[test]
fn failed_risk_observation_is_atomic() {
    let mut market = market_with_reserves(1_000 * NAD, 2_000 * NAD);
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        ..AmmConfig::default()
    };
    market.ensure_amm_initialized(10).unwrap();
    market.observe_current_risk(10).unwrap();

    let risk_before = market.risk;
    let invariant_before = market.amm.invariant_d_nad;
    let risk_revision_before = market.risk_revision;
    let last_update_before = market.last_update_slot;
    let invalid = ConcentratedEvaluation {
        invariant_d: risk_before.cached_q_nad,
        balanced_equivalent_q: risk_before.cached_q_nad,
        marginal_price_nad: 0,
    };

    assert!(market.observe_exact_risk_from_curve_evaluation(invalid, 11).is_err());
    assert_eq!(market.risk, risk_before);
    assert_eq!(market.amm.invariant_d_nad, invariant_before);
    assert_eq!(market.risk_revision, risk_revision_before);
    assert_eq!(market.last_update_slot, last_update_before);
}

#[test]
fn curve_revision_keeps_risk_stale_until_exact_refresh() {
    let mut market = market_with_reserves(1_000 * NAD, 2_000 * NAD);
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        ..AmmConfig::default()
    };
    market.ensure_amm_initialized(10).unwrap();
    market.observe_current_risk(10).unwrap();
    assert_eq!(market.risk_revision, market.curve_revision);

    market.base_side.reserves.live_reserve += NAD;
    market.base_side.reserves.cash_reserve += NAD;
    market.finalize_amm_transition(11).unwrap();
    assert_ne!(market.risk_revision, market.curve_revision);

    market.refresh_risk().unwrap();
    assert_eq!(market.risk_revision, market.curve_revision);
}

use super::*;
use crate::{
    constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS},
    math::ExplicitCurveParameters,
    state::{AmmConfig, Debt, MarketAsset, MarketConfig, MarketSide, ReserveShares, Reserves},
};

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
        market.amm.explicit_curve_cache.parameters(),
        market.config.amm.explicit_curve_parameters().unwrap()
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
        market.config.amm.explicit_curve_parameters().unwrap()
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
        .preliminary_swap_inputs_for_state(MarketAsset::Base, 10_000 * NAD, 10, pre_state)
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

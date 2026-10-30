use super::*;
use crate::{
    constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS},
    state::{AmmConfig, Debt, MarketAsset, MarketConfig, MarketSide, ReserveShares, Reserves},
    transitions::amm::ConcentratedCurveParameters,
};

fn concentrated_config() -> AmmConfig {
    AmmConfig {
        peak_amplification_nad: 4 * NAD,
        core_half_width_bps: 100,
        fade_width_bps: 400,
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
fn distressed_hlp_does_not_block_spot_swap() {
    use crate::transitions::amm::SwapRequest;

    let mut market = market_with_liquidity(AmmConfig::default());
    market.deposit_single_sided(MarketAsset::Base, 20_000 * NAD, 1).unwrap();
    market.debt.quote_borrow_index_nad = 3 * NAD as u128;
    let prices = crate::transitions::liquidity::current_hlp_curve_prices(&market).unwrap();
    let (base_nav, _) = crate::transitions::liquidity::current_hlp_signed_navs_with_prices(&market, prices).unwrap();
    assert!(base_nav < 0);

    let before_shares = market.base_hlp_vault.ylp_shares;
    let before_debt_shares = market.base_hlp_vault.debt_shares;
    let before_principal = market.base_hlp_vault.debt_principal;
    let mut prepared = SwapRequest {
        current_slot: 2,
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Quote,
        reserve_credit: 1_000 * NAD,
        protocol_fee_bps: 0,
    }
    .prepare(&mut market)
    .unwrap();
    assert!(prepared.quote.amount_out > 0);
    assert_eq!(market.amm.frozen_hlp_mask, 1);
    assert!(market.assert_hlp_entry_available(MarketAsset::Base).is_err());
    assert!(market.deposit_single_sided(MarketAsset::Base, NAD, 1).is_err());
    let credit = crate::transitions::LeverageSwapFeeCredit::from_total_actual_credit(
        &prepared.leverage_quote(),
        prepared.quote.fee.claimable_fee_debit,
    )
    .unwrap();
    prepared
        .apply(
            &mut market,
            crate::transitions::liquidity::SwapCashPolicy::Spot,
            credit,
            2,
            0,
            crate::state::ProtocolAuctionSplit::default(),
            None,
        )
        .unwrap();
    assert_eq!(market.base_hlp_vault.ylp_shares, before_shares);
    assert_eq!(market.base_hlp_vault.debt_shares, before_debt_shares);
    assert_eq!(market.base_hlp_vault.debt_principal, before_principal);
    market.assert_market_invariants().unwrap();
}

#[test]
fn frozen_hlp_keeps_opposite_hedge_and_cannot_reset_the_swap_price() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageSwapFeeCredit};

    for config in [AmmConfig::default(), concentrated_config()] {
        let mut market = market_with_liquidity(config);
        market.deposit_single_sided(MarketAsset::Base, 20_000 * NAD, 1).unwrap();
        market.deposit_single_sided(MarketAsset::Quote, 20_000 * NAD, 1).unwrap();
        market.debt.quote_borrow_index_nad = 3 * NAD as u128;
        market.prepare_amm_for_swap(2).unwrap();
        let initial_price = market.current_concentrated_spot_price_nad().unwrap().unwrap();
        let frozen_shares = market.base_hlp_vault.ylp_shares;
        let frozen_debt_shares = market.base_hlp_vault.debt_shares;
        let frozen_principal = market.base_hlp_vault.debt_principal;
        market.freeze_negative_hlp_vaults().unwrap();
        assert_eq!(market.amm.frozen_hlp_mask, 1);
        assert_eq!(market.current_concentrated_spot_price_nad().unwrap().unwrap(), initial_price);

        let mut forward = SwapRequest {
            current_slot: 2,
            current_unix_timestamp: 0,
            asset_in: MarketAsset::Quote,
            reserve_credit: 1_000 * NAD,
            protocol_fee_bps: 0,
        }
        .prepare(&mut market)
        .unwrap();
        let base_out = forward.quote.amount_out;
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(
            &forward.leverage_quote(),
            forward.quote.fee.claimable_fee_debit,
        )
        .unwrap();
        forward
            .apply(&mut market, SwapCashPolicy::Spot, credit, 2, 0, ProtocolAuctionSplit::default(), None)
            .unwrap();
        assert_eq!(market.base_hlp_vault.ylp_shares, frozen_shares);
        assert_eq!(market.base_hlp_vault.debt_shares, frozen_debt_shares);
        assert_eq!(market.base_hlp_vault.debt_principal, frozen_principal);
        let healthy_claim = (u128::from(market.base_side.reserves.live_reserve)
            * u128::from(market.quote_hlp_vault.ylp_shares)
            / u128::from(market.base_side.shares.ylp_supply)) as u64;
        let healthy_debt = Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad)
            .unwrap();
        assert!(healthy_debt.abs_diff(u128::from(healthy_claim)) <= 1);

        // Funding interest keeps accruing on the frozen liability. It must
        // neither change executable price nor turn that debt into swap cash.
        let price_before_interest = market.current_concentrated_spot_price_nad().unwrap().unwrap();
        market.debt.quote_borrow_index_nad = 31 * NAD as u128 / 10;
        assert_eq!(market.current_concentrated_spot_price_nad().unwrap().unwrap(), price_before_interest);

        let mut reverse = SwapRequest {
            current_slot: 3,
            current_unix_timestamp: 0,
            asset_in: MarketAsset::Base,
            reserve_credit: base_out,
            protocol_fee_bps: 0,
        }
        .prepare(&mut market)
        .unwrap();
        let quote_out = reverse.quote.amount_out;
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(
            &reverse.leverage_quote(),
            reverse.quote.fee.claimable_fee_debit,
        )
        .unwrap();
        reverse
            .apply(&mut market, SwapCashPolicy::Spot, credit, 3, 0, ProtocolAuctionSplit::default(), None)
            .unwrap();
        assert!(quote_out <= 1_000 * NAD, "round-trip profit: {quote_out}");
        assert_eq!(market.amm.frozen_hlp_mask, 1);
        market.assert_market_invariants().unwrap();
        let mut liquidity = market.clone();
        let added = liquidity.deposit_single_sided(MarketAsset::Quote, 1_000 * NAD, 1).unwrap();
        let exited = liquidity.withdraw_single_sided(MarketAsset::Quote, added.hlp_amount).unwrap();
        assert!(exited.target_amount_out > 0);
        assert_eq!(liquidity.amm.frozen_hlp_mask, 1);
        let old_supply = liquidity.base_side.shares.ylp_supply;
        let ordinary_entry = liquidity.add_liquidity(1_000 * NAD, 1_000 * NAD).unwrap();
        liquidity.remove_liquidity(ordinary_entry.ylp_supply - old_supply).unwrap();
        liquidity.assert_market_invariants().unwrap();
    }
}

#[test]
fn frozen_hlp_terminal_close_restores_ordinary_swaps() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageCollateralFee, LeverageSwapFeeCredit};

    let mut market = market_with_liquidity(AmmConfig::default());
    market.deposit_single_sided(MarketAsset::Base, 20_000 * NAD, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 20_000 * NAD, 1).unwrap();
    market.debt.quote_borrow_index_nad = 3 * NAD as u128;
    market.risk.base_price_ema_nad = NAD;
    market.risk.quote_price_ema_nad = NAD;
    market.prepare_amm_for_swap(2).unwrap();
    market.freeze_negative_hlp_vaults().unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 1);
    let plan = market.prepare_terminal_hlp_waterfall(MarketAsset::Base, u64::MAX,
        LeverageCollateralFee::default(), 2, 0, 0).unwrap();
    let receipt = plan.consume(&mut market, 0, 0, u64::MAX).unwrap();
    assert!(receipt.debt_closed > 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 0);
    assert_eq!(market.amm.frozen_hlp_mask, 0);
    let closed_receipts = market.base_hlp_vault.hlp_supply;
    assert!(closed_receipts > 0);
    let exit = market.withdraw_single_sided(MarketAsset::Base, closed_receipts).unwrap();
    assert_eq!(exit.target_amount_out, 0);
    assert_eq!(exit.hlp_supply, 0);
    market.assert_market_invariants().unwrap();
    let mut prepared = SwapRequest { current_slot: 3, current_unix_timestamp: 0,
        asset_in: MarketAsset::Quote, reserve_credit: 1_000 * NAD, protocol_fee_bps: 0 }
        .prepare(&mut market).unwrap();
    let credit = LeverageSwapFeeCredit::from_total_actual_credit(&prepared.leverage_quote(),
        prepared.quote.fee.claimable_fee_debit).unwrap();
    prepared.apply(&mut market, SwapCashPolicy::Spot, credit, 3, 0,
        ProtocolAuctionSplit::default(), None).unwrap();
    market.assert_market_invariants().unwrap();
}

#[test]
fn spot_only_hlp_deficit_cannot_write_off_the_vault() {
    use crate::transitions::LeverageCollateralFee;

    let mut market = market_with_liquidity(AmmConfig::default());
    market.deposit_single_sided(MarketAsset::Base, 20_000 * NAD, 1).unwrap();
    market.debt.quote_borrow_index_nad = 3 * NAD as u128;
    market.risk.base_price_ema_nad = 3 * NAD;
    market.risk.quote_price_ema_nad = NAD / 3;
    market.prepare_amm_for_swap(2).unwrap();
    market.freeze_negative_hlp_vaults().unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 1);
    let fee = LeverageCollateralFee::default();
    assert!(market.prepare_terminal_hlp_waterfall(MarketAsset::Base, u64::MAX, fee, 2, 0, 0).is_err());
    assert!(market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).is_err());
}

#[test]
fn closing_one_of_two_frozen_hlp_vaults_keeps_the_other_isolated() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageCollateralFee, LeverageSwapFeeCredit};

    let mut market = market_with_liquidity(AmmConfig::default());
    market.deposit_single_sided(MarketAsset::Base, 20_000 * NAD, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 20_000 * NAD, 1).unwrap();
    market.debt.quote_borrow_index_nad = 3 * NAD as u128;
    market.debt.base_borrow_index_nad = 3 * NAD as u128;
    market.risk.base_price_ema_nad = NAD;
    market.risk.quote_price_ema_nad = NAD;
    market.prepare_amm_for_swap(2).unwrap();
    market.freeze_negative_hlp_vaults().unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 3);
    let quote_shares = market.quote_hlp_vault.ylp_shares;
    let quote_debt = market.quote_hlp_vault.debt_shares;
    let plan = market.prepare_terminal_hlp_waterfall(MarketAsset::Base, u64::MAX,
        LeverageCollateralFee::default(), 2, 0, 0).unwrap();
    plan.consume(&mut market, 0, 0, u64::MAX).unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 2);
    assert_eq!(market.quote_hlp_vault.ylp_shares, quote_shares);
    assert_eq!(market.quote_hlp_vault.debt_shares, quote_debt);
    let mut prepared = SwapRequest { current_slot: 3, current_unix_timestamp: 0,
        asset_in: MarketAsset::Quote, reserve_credit: 1_000 * NAD, protocol_fee_bps: 0 }
        .prepare(&mut market).unwrap();
    assert!(prepared.quote.amount_out > 0);
    let credit = LeverageSwapFeeCredit::from_total_actual_credit(&prepared.leverage_quote(),
        prepared.quote.fee.claimable_fee_debit).unwrap();
    prepared.apply(&mut market, SwapCashPolicy::Spot, credit, 3, 0,
        ProtocolAuctionSplit::default(), None).unwrap();
    assert_eq!(market.quote_hlp_vault.ylp_shares, quote_shares);
    assert_eq!(market.quote_hlp_vault.debt_shares, quote_debt);
    market.assert_market_invariants().unwrap();
}

#[test]
fn first_liquidity_initializes_center_without_an_oracle() {
    let mut market = market_with_liquidity(concentrated_config());
    assert!(market.ensure_amm_initialized(10).unwrap());
    assert_eq!(market.amm.center_price_nad, NAD);
    assert_eq!(market.amm.price_ema_nad, NAD);
    assert_eq!(
        market.amm.concentrated_curve_cache.parameters(),
        market.config.amm.concentrated_curve_parameters().unwrap()
    );
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
}

#[test]
fn final_user_withdrawal_allows_the_amm_to_reinitialize() {
    let mut market = market_with_liquidity(concentrated_config());
    market.ensure_amm_initialized(10).unwrap();
    let user_shares = market.base_side.shares.ylp_supply - MIN_LIQUIDITY;
    market.remove_liquidity(user_shares).unwrap();
    market.finalize_amm_transition_and_observe_risk(11).unwrap();
    assert!(!market.amm.initialized);

    market.add_liquidity(100_000 * NAD, 100_000 * NAD).unwrap();
    market.finalize_amm_transition_and_observe_risk(12).unwrap();
    assert!(market.amm.initialized);
    assert!(market.current_concentrated_spot_price_nad().unwrap().is_some());
}

#[test]
fn concentrated_curve_initializes_and_quotes_the_integrated_ordinary_tranche() {
    let mut config = AmmConfig::default();
    config
        .set_concentrated_curve_parameters(ConcentratedCurveParameters {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
        })
        .unwrap();
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    assert_eq!(
        market.amm.concentrated_curve_cache.parameters(),
        market.config.amm.concentrated_curve_parameters().unwrap()
    );

    let quote = market
        .quote_integrated_concentrated_exact_in_nad(10_000 * NAD as u128, 100 * NAD as u128, MarketAsset::Base)
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
        .quote_concentrated_integrated_with_fee(MarketAsset::Base, 10_000 * NAD, preliminary, 0)
        .unwrap()
        .unwrap();
    assert_eq!(
        fee_quote.fee.total_fee_debit + fee_quote.fee.amount_in_for_quote,
        fee_quote.fee.reserve_credit
    );
    assert_eq!(fee_quote.amount_out as u128, fee_quote.integrated.executable.amount_out);
}

#[test]
fn quote_only_fee_is_withheld_from_base_sell_output() {
    let mut config = concentrated_config();
    config.swap_fee_collect_mode = crate::state::SWAP_FEE_COLLECT_QUOTE_ONLY;
    let mut market = market_with_liquidity(config);
    market.config.swap_fee_bps = 100;
    market.ensure_amm_initialized(10).unwrap();

    let reserve_credit = 10_000 * NAD;
    let pre_state = market.dynamic_fee_pre_state(10).unwrap();
    let preliminary = market
        .preliminary_swap_inputs_for_state(MarketAsset::Base, reserve_credit, 10, pre_state)
        .unwrap();
    let quote = market
        .quote_concentrated_integrated_with_fee(MarketAsset::Base, reserve_credit, preliminary, 0)
        .unwrap()
        .unwrap();

    assert_eq!(quote.fee.fee_asset, MarketAsset::Quote.code());
    assert_eq!(quote.fee.amount_in_for_quote, reserve_credit);
    assert_eq!(quote.fee.reserve_input_credit, reserve_credit);
    assert_eq!(quote.fee.retained_surcharge, 0);
    assert_eq!(
        quote.amount_out + quote.fee.claimable_fee_debit,
        quote.gross_amount_out
    );
    assert_eq!(
        quote.gross_amount_out as u128,
        quote.integrated.executable.amount_out
    );
}

#[test]
fn concentrated_center_move_reconstructs_unchanged_reserves_in_closed_form() {
    let mut config = AmmConfig {
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 100,
        min_adjustment_interval_slots: 1,
        ..AmmConfig::default()
    };
    config
        .set_concentrated_curve_parameters(ConcentratedCurveParameters {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
        })
        .unwrap();
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    let ordinary_before = market.integrated_curve_state_nad().unwrap();
    let cache_before = market.amm.concentrated_curve_cache;
    market.amm.price_ema_nad = 2 * NAD;
    market.amm.protected_floor_per_share_nad = 0;

    assert!(market.advance_one_amm_controller_target(11).unwrap());
    assert_eq!(market.integrated_curve_state_nad().unwrap(), ordinary_before);
    assert_ne!(market.amm.concentrated_curve_cache, cache_before);
    assert_eq!(market.amm.center_price_nad, NAD + NAD / 100);
    assert_eq!(market.amm.last_adjustment_slot, 11);
    assert!(market.current_concentrated_spot_price_nad().unwrap().unwrap() > 0);
}

#[test]
fn concentrated_center_move_deploys_locked_bucket_only_when_it_preserves_ylp_principal() {
    let mut config = AmmConfig {
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 100,
        min_adjustment_interval_slots: 1,
        ..AmmConfig::default()
    };
    config
        .set_concentrated_curve_parameters(ConcentratedCurveParameters {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
        })
        .unwrap();
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    market.amm.price_ema_nad = 2 * NAD;
    let depth_before = market.amm.curve_depth_per_share_nad;
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
    assert!(market.amm.curve_depth_per_share_nad >= depth_before);
    assert_eq!(market.amm.center_price_nad, NAD + NAD / 100);
    market.assert_market_invariants().unwrap();
}

#[test]
fn concentrated_proportional_liquidity_scales_and_restores_both_tranches() {
    let mut config = AmmConfig::default();
    config
        .set_concentrated_curve_parameters(ConcentratedCurveParameters {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
        })
        .unwrap();
    let mut market = market_with_liquidity(config);
    market.ensure_amm_initialized(10).unwrap();
    let cache_before = market.amm.concentrated_curve_cache;
    let spot_before = market.current_concentrated_spot_price_nad().unwrap().unwrap();
    let credit = 100_000 * NAD;
    let added = market.add_liquidity(credit, credit).unwrap();
    market.finalize_amm_transition_and_observe_risk(11).unwrap();
    assert!(market.amm.concentrated_curve_cache.tail_liquidity > cache_before.tail_liquidity);
    assert!(
        market.amm.concentrated_curve_cache.concentrated_liquidity > cache_before.concentrated_liquidity
    );

    market.remove_liquidity(added.ylp_amount).unwrap();
    market.finalize_amm_transition_and_observe_risk(12).unwrap();
    assert_eq!(market.amm.concentrated_curve_cache.parameters(), cache_before.parameters());
    assert!(market
        .current_concentrated_spot_price_nad()
        .unwrap()
        .unwrap()
        .abs_diff(spot_before)
        <= 2);
    market.assert_market_invariants().unwrap();
}

#[test]
fn lp_fee_compounding_percentage_excludes_protocol_revenue() {
    let disabled = split_compounded_swap_fee(1_000, 500, 2_500, 0).unwrap();
    assert_eq!(disabled.claimable_base_fee, 1_000);
    assert_eq!(disabled.claimable_dynamic_surcharge, 500);
    assert_eq!(disabled.compounded_base_fee, 0);
    assert_eq!(disabled.compounded_dynamic_surcharge, 0);

    let forty_percent = split_compounded_swap_fee(1_000, 500, 2_500, 4_000).unwrap();
    assert_eq!(forty_percent.claimable_base_fee, 700);
    assert_eq!(forty_percent.claimable_dynamic_surcharge, 300);
    assert_eq!(forty_percent.compounded_base_fee, 300);
    assert_eq!(forty_percent.compounded_dynamic_surcharge, 200);

    let complete = split_compounded_swap_fee(1_000, 500, 2_500, BPS_DENOMINATOR).unwrap();
    assert_eq!(complete.claimable_base_fee, 250);
    assert_eq!(complete.claimable_dynamic_surcharge, 0);
    assert_eq!(complete.compounded_base_fee, 750);
    assert_eq!(complete.compounded_dynamic_surcharge, 500);
}

fn bind_test_oracle(market: &mut Market, lambda: u16, reference: u64) {
    market.oracle.policy = OraclePolicy {lambda_bps:lambda,adjustment_cap_nad:2*NAD,..OraclePolicy::default()};
    market.oracle.runtime = RuntimePricing::bind(market.oracle.policy, OracleObservation {
        price_nad:reference, ..OracleObservation::default()
    });
    market.checkpoint_oracle_risk_depth().unwrap();
}
fn oracle_test_trade(market: &mut Market, asset: MarketAsset, input: u64) -> Result<u64> {
    use crate::transitions::{liquidity::SwapCashPolicy, LeverageSwapFeeCredit};
    let before = market.ordinary_curve_point_nad()?;
    let curve = market.bounded_curve()?;
    let mut prepared = SwapRequest {current_slot:2,current_unix_timestamp:0,asset_in:asset,
        reserve_credit:input,protocol_fee_bps:0}.prepare(market)?;
    let output=prepared.quote.amount_out;
    let credit=LeverageSwapFeeCredit::from_total_actual_credit(&prepared.leverage_quote(), prepared.quote.fee.claimable_fee_debit)?;
    prepared.apply(market,SwapCashPolicy::Spot,credit,2,0,ProtocolAuctionSplit::default(),None)?;
    market.assert_market_invariants()?;
    if let Some(curve) = curve {
        let after = market.ordinary_curve_point_nad()?;
        assert!(curve.invariant_delta_bounds(before,after)?.1>=0,"materialized invariant decreased");
    }
    Ok(output)
}

#[test]
fn bounded_oracle_swaps_materialize_hlp_and_reject_cycle_profit() {
    for config in [AmmConfig::default(),concentrated_config()] {
        for lambda in [0,2500,5000,9999] {
            for owners in [0,1,2,3] {
                let mut market=market_with_liquidity(config);
                market.config.amm.adjustment_step_nad=0;
                market.config.amm.adjustment_threshold_nad=0;
                market.config.amm.min_adjustment_interval_slots=0;
                market.prepare_amm_for_swap(1).unwrap();
                if owners&1!=0 {market.deposit_single_sided(MarketAsset::Base,20_000*NAD,1).unwrap();}
                if owners&2!=0 {market.deposit_single_sided(MarketAsset::Quote,20_000*NAD,1).unwrap();}
                market.finalize_amm_transition_and_observe_risk(1).unwrap();
                bind_test_oracle(&mut market,lambda,3*NAD/2);
                for cycle in 0..8 {
                    for asset in [MarketAsset::Base,MarketAsset::Quote] {
                        let initial=1000*NAD;
                        let out=oracle_test_trade(&mut market,asset,initial).unwrap_or_else(|e|panic!("lambda={lambda} owners={owners} cycle={cycle} direction={asset:?}: {e}"));
                        let returned=oracle_test_trade(&mut market,asset.opposite(),out).unwrap_or_else(|e|panic!("reverse lambda={lambda} owners={owners} cycle={cycle} direction={asset:?}: {e}"));
                        assert!(returned<=initial,"profitable cycle lambda={lambda} owners={owners}: {returned}>{initial}");
                    }
                }
            }
        }
    }
}

#[test]
fn bounded_oracle_carry_is_not_recharged_by_repeated_materialization() {
    use crate::transitions::liquidity::prepare_concentrated_hlp_transition;
    for lambda in [2500,5000,9999] {
        for asset in [MarketAsset::Base,MarketAsset::Quote] {
            let mut market=market_with_liquidity(AmmConfig::default());
            market.prepare_amm_for_swap(1).unwrap();
            market.deposit_single_sided(MarketAsset::Base,20_000*NAD,1).unwrap();
            market.deposit_single_sided(MarketAsset::Quote,20_000*NAD,1).unwrap();
            market.debt.base_borrow_index_nad=101*NAD as u128/100;
            market.debt.quote_borrow_index_nad=101*NAD as u128/100;
            market.oracle.policy=OraclePolicy{lambda_bps:lambda,adjustment_cap_nad:2*NAD,..OraclePolicy::default()};
            market.oracle.runtime=RuntimePricing::bind(market.oracle.policy,OracleObservation{price_nad:3*NAD/2,..OracleObservation::default()});
            let mut trade=|side:MarketAsset,input:u64|->Result<u64>{
                let preliminary=market.preliminary_swap_inputs_for_state(side,input,2,DynamicFeePreState {center_price_nad:market.amm.center_price_nad,volatility_accumulator_nad:market.amm.volatility_accumulator_nad,volatility_last_update_slot:market.amm.last_observation_slot})?;
                let (start,quote)=market.prepare_current_hlp_quote(side,input,preliminary,0,true)?;
                let plan=prepare_concentrated_hlp_transition(&market,*start,quote,side)?;
                let previous_point=market.ordinary_curve_point_nad()?;
                market.side_mut(side).credit_reserve(quote.fee.amount_in_for_quote,true)?;
                market.side_mut(side.opposite()).debit_reserve(quote.gross_amount_out,true)?;
                plan.consume(&mut market)?;
                market.checkpoint_amm_neutral_inventory_raw(2,quote.post_fee_curve_cache)?;
                market.assert_market_invariants()?;
                let after=market.ordinary_curve_point_nad()?;
                assert!(market.bounded_curve()?.unwrap().invariant_delta_bounds(previous_point,after)?.1>=0);
                Ok(quote.amount_out)
            };
            for cycle in 0..32 {
                let input=1000*NAD;
                let out=trade(asset,input).unwrap_or_else(|e|panic!("carry lambda={lambda} cycle={cycle} asset={asset:?}: {e}"));
                let returned=trade(asset.opposite(),out).unwrap_or_else(|e|panic!("reverse carry lambda={lambda} cycle={cycle} asset={asset:?}: {e}"));
                assert!(returned<=input,"carry round-trip extraction: {returned}>{input}");
            }
        }
    }
}

#[test]
fn bounded_oracle_fee_compounding_and_frozen_cohorts_preserve_cycles() {
    for lambda in [2500,5000,9999] {
        for compound in [0,10_000] {
            for frozen in [0,1,2,3] {
                let mut market=market_with_liquidity(concentrated_config());
                market.config.amm.adjustment_step_nad=0;
                market.config.amm.adjustment_threshold_nad=0;
                market.config.amm.min_adjustment_interval_slots=0;
                market.config.swap_fee_bps=100;
                market.config.amm.compounding_fee_bps=compound;
                market.prepare_amm_for_swap(1).unwrap();
                market.deposit_single_sided(MarketAsset::Base,20_000*NAD,1).unwrap();
                market.deposit_single_sided(MarketAsset::Quote,20_000*NAD,1).unwrap();
                if frozen&1!=0 {market.debt.quote_borrow_index_nad=3*NAD as u128;}
                if frozen&2!=0 {market.debt.base_borrow_index_nad=3*NAD as u128;}
                bind_test_oracle(&mut market,lambda,3*NAD/2);
                let mut previous_depth=market.oracle.recognized_tail_liquidity;
                for cycle in 0..8 {
                    for asset in [MarketAsset::Base,MarketAsset::Quote] {
                        let input=1000*NAD;
                        let out=oracle_test_trade(&mut market,asset,input).unwrap_or_else(|e|panic!("lambda={lambda} compound={compound} frozen={frozen} cycle={cycle} asset={asset:?}: {e}"));
                        let returned=oracle_test_trade(&mut market,asset.opposite(),out).unwrap();
                        assert!(returned<=input,"fee/frozen round-trip profit");
                        assert!(market.oracle.recognized_tail_liquidity<=previous_depth);
                        previous_depth=market.oracle.recognized_tail_liquidity;
                    }
                }
            }
        }
    }
}

#[test]
fn bounded_oracle_exit_loans_cannot_outpay_own_pool_acquisition() {
    use crate::transitions::LeverageCollateralFee;
    for config in [AmmConfig::default(),concentrated_config()] {
        for lambda in [0,2500,5000,9999] {
            for reference in [NAD/10,NAD,10*NAD] {
                for with_hlp in [false,true] {
                    for asset in [MarketAsset::Base,MarketAsset::Quote] {
                        for input_tokens in [100,10_000,200_000] {
                            let mut market=market_with_liquidity(config);
                            market.config.amm.adjustment_step_nad=0;
                            market.config.amm.adjustment_threshold_nad=0;
                            market.config.amm.min_adjustment_interval_slots=0;
                            market.prepare_amm_for_swap(1).unwrap();
                            if with_hlp {
                                market.deposit_single_sided(MarketAsset::Base,20_000*NAD,1).unwrap();
                                market.deposit_single_sided(MarketAsset::Quote,20_000*NAD,1).unwrap();
                            }
                            bind_test_oracle(&mut market,lambda,reference);
                            let input=input_tokens*NAD;
                            let collateral=oracle_test_trade(&mut market,asset,input).unwrap();
                            // Grant the attacker fully settled favorable filters. The
                            // exit bound must not depend on waiting out an EMA.
                            let price=market.current_concentrated_spot_price_nad().unwrap().unwrap();
                            market.risk.base_price_ema_nad=price;
                            market.risk.directional_base_price_ema_nad=price;
                            market.risk.quote_price_ema_nad=NAD*NAD/price;
                            market.risk.directional_quote_price_ema_nad=NAD*NAD/price;
                            market.risk.curve_depth_ema_nad=market.risk.observed_curve_depth_nad;
                            let capacity=market.borrow_capacity_quote(asset.opposite(),collateral,
                                LeverageCollateralFee::default(),None,2).unwrap();
                            assert!(capacity.max_debt<=input,
                                "exit loan: lambda={lambda} reference={reference} hlp={with_hlp} direction={asset:?} input={input} debt={}",capacity.max_debt);
                            // Splitting the purchase must consume shared debt capacity.
                            let mut total=0_u64;
                            for _ in 0..8 {
                                let amount=collateral/8;
                                let capacity=market.borrow_capacity_quote(asset.opposite(),amount,
                                    LeverageCollateralFee::default(),None,2).unwrap();
                                if capacity.max_debt==0 {break;}
                                let mut position=BorrowPosition::default();
                                match asset {MarketAsset::Base=>position.quote_collateral=amount,
                                    MarketAsset::Quote=>position.base_collateral=amount}
                                market.borrow(&mut position,asset,capacity.max_debt,0,2).unwrap();
                                total+=capacity.max_debt;
                            }
                            assert!(total<=input,"split exit loan: {total}>{input}");
                            market.assert_market_invariants().unwrap();
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn oracle_only_repricing_records_volatility_once_without_rehedging() {
    let mut market=market_with_liquidity(AmmConfig::default());
    market.config.amm.volatility_shock_cap_nad=NAD;
    market.config.amm.volatility_cap_nad=10*NAD;
    market.prepare_amm_for_swap(1).unwrap();
    market.deposit_single_sided(MarketAsset::Base,20_000*NAD,1).unwrap();
    market.base_side.asset_mint=Pubkey::new_unique();
    market.quote_side.asset_mint=Pubkey::new_unique();
    let provider=Pubkey::new_unique();
    let key=Pubkey::new_unique();
    market.oracle.policy=OraclePolicy{format:crate::state::NORMALIZED_ORACLE_V1,provider,base_account:key,
        base_feed_id:[7;32],lambda_bps:5000,adjustment_cap_nad:2*NAD,max_age_seconds:1000,max_age_slots:1000,
        max_confidence_bps:100,..Default::default()};
    let before=(market.base_side.try_to_vec().unwrap(),market.quote_side.try_to_vec().unwrap(),
        market.base_hlp_vault.try_to_vec().unwrap(),market.debt.try_to_vec().unwrap());
    let mut feed=crate::state::NormalizedOracleV1{magic:crate::state::NORMALIZED_ORACLE_MAGIC,feed_id:[7;32],
        base_mint:market.base_side.asset_mint,quote_mint:market.quote_side.asset_mint,price_nad:NAD,
        confidence_nad:0,publish_time:100,publish_slot:2,sequence:1,status:1};
    let mut lamports=1;
    for (seq,reference) in [(1,NAD),(2,4*NAD)] {
        feed.sequence=seq;feed.price_nad=reference;
        let mut data=feed.try_to_vec().unwrap();
        let account=AccountInfo::new(&key,false,false,&mut lamports,&mut data,&provider,false,0);
        market.bind_oracle(&[account],&Clock{slot:2,unix_timestamp:100,..Clock::default()}).unwrap();
    }
    let volatility=market.amm.volatility_accumulator_nad;
    assert!(volatility>0);
    let mut data=feed.try_to_vec().unwrap();
    let account=AccountInfo::new(&key,false,false,&mut lamports,&mut data,&provider,false,0);
    market.bind_oracle(&[account],&Clock{slot:2,unix_timestamp:100,..Clock::default()}).unwrap();
    assert_eq!(market.amm.volatility_accumulator_nad,volatility);
    assert_eq!((market.base_side.try_to_vec().unwrap(),market.quote_side.try_to_vec().unwrap(),
        market.base_hlp_vault.try_to_vec().unwrap(),market.debt.try_to_vec().unwrap()),before);
    assert_eq!(market.amm.last_trade_price_nad,market.current_concentrated_spot_price_nad().unwrap().unwrap());
}

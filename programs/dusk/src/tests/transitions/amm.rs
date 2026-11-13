use super::*;
use crate::{
    constants::{DEFAULT_BORROW_APR_NAD, MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS},
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
        adjustment_step_nad: NAD / 1_000,
        ..AmmConfig::default()
    }
}

fn market_with_liquidity(config: AmmConfig) -> Market {
    let reserve = 1_000_000 * NAD;
    Market {
        income: Default::default(),
        tranches: Default::default(),
        junior_exit_config: Default::default(),
        junior_mint: Pubkey::default(),
        version: MARKET_LAYOUT_VERSION,
        base_side: Box::new(MarketSide {
            asset_decimals: 9,
            reserves: Reserves {
                live_reserve: u128::from(reserve),
                cash_reserve: reserve,
                ..Reserves::default()
            },
            shares: ReserveShares {
                srlp_supply: reserve,
            },
            ..MarketSide::default()
        }),
        quote_side: Box::new(MarketSide {
            asset_decimals: 9,
            reserves: Reserves {
                live_reserve: u128::from(reserve),
                cash_reserve: reserve,
                ..Reserves::default()
            },
            shares: ReserveShares {
                srlp_supply: reserve,
            },
            ..MarketSide::default()
        }),
        config: MarketConfig {
            divergence_fee_share_cap_bps: 2_000,
            volatility_fee_share_cap_bps: 2_000,
            amm: config,
            ..MarketConfig::default()
        },
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_borrow_apr_nad: DEFAULT_BORROW_APR_NAD,
            quote_borrow_apr_nad: DEFAULT_BORROW_APR_NAD,
            ..Debt::default()
        },
        ..Market::current_layout()
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

    let before_shares = market.base_hlp_vault.srlp_shares;
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
    assert_eq!(market.base_hlp_vault.srlp_shares, before_shares);
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
        let frozen_shares = market.base_hlp_vault.srlp_shares;
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
        assert_eq!(market.base_hlp_vault.srlp_shares, frozen_shares);
        assert_eq!(market.base_hlp_vault.debt_shares, frozen_debt_shares);
        assert_eq!(market.base_hlp_vault.debt_principal, frozen_principal);
        let healthy_claim = (u128::from(market.base_side.reserves.live_reserve)
            * u128::from(market.quote_hlp_vault.srlp_shares)
            / u128::from(market.base_side.shares.srlp_supply)) as u64;
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
        let old_supply = liquidity.base_side.shares.srlp_supply;
        let ordinary_entry = liquidity.add_liquidity(1_000 * NAD, 1_000 * NAD).unwrap();
        liquidity.remove_liquidity(ordinary_entry.srlp_supply - old_supply).unwrap();
        liquidity.assert_market_invariants().unwrap();
    }
}

#[test]
fn frozen_hlp_terminal_close_restores_ordinary_swaps() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageSwapFeeCredit};

    let mut market = market_with_liquidity(AmmConfig::default());
    market.deposit_single_sided(MarketAsset::Base, 20_000 * NAD, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 20_000 * NAD, 1).unwrap();
    market.debt.quote_borrow_index_nad = 3 * NAD as u128;
    market.risk.base_price_ema_nad = NAD;
    market.risk.quote_price_ema_nad = NAD;
    market.prepare_amm_for_swap(2).unwrap();
    market.freeze_negative_hlp_vaults().unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 1);
    let plan = market.prepare_terminal_hlp_waterfall(MarketAsset::Base, 2, 0, 0).unwrap();
    let receipt = plan.consume(&mut market, u64::MAX).unwrap();
    assert!(receipt.debt_closed > 0);
    assert_eq!(market.base_hlp_vault.srlp_shares, 0);
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
    assert!(market.prepare_terminal_hlp_waterfall(MarketAsset::Base, 2, 0, 0).is_err());
    assert!(market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).is_err());
}

#[test]
fn closing_one_of_two_frozen_hlp_vaults_keeps_the_other_isolated() {
    use crate::transitions::{amm::SwapRequest, liquidity::SwapCashPolicy, LeverageSwapFeeCredit};

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
    let quote_shares = market.quote_hlp_vault.srlp_shares;
    let quote_debt = market.quote_hlp_vault.debt_shares;
    let plan = market.prepare_terminal_hlp_waterfall(MarketAsset::Base, 2, 0, 0).unwrap();
    plan.consume(&mut market, u64::MAX).unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 2);
    assert_eq!(market.quote_hlp_vault.srlp_shares, quote_shares);
    assert_eq!(market.quote_hlp_vault.debt_shares, quote_debt);
    let mut prepared = SwapRequest { current_slot: 3, current_unix_timestamp: 0,
        asset_in: MarketAsset::Quote, reserve_credit: 1_000 * NAD, protocol_fee_bps: 0 }
        .prepare(&mut market).unwrap();
    assert!(prepared.quote.amount_out > 0);
    let credit = LeverageSwapFeeCredit::from_total_actual_credit(&prepared.leverage_quote(),
        prepared.quote.fee.claimable_fee_debit).unwrap();
    prepared.apply(&mut market, SwapCashPolicy::Spot, credit, 3, 0,
        ProtocolAuctionSplit::default(), None).unwrap();
    assert_eq!(market.quote_hlp_vault.srlp_shares, quote_shares);
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
    let user_shares = market.base_side.shares.srlp_supply - MIN_LIQUIDITY;
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
    // Payouts are whole token atoms: the floor of the curve's internal output.
    assert_eq!(
        fee_quote.amount_out,
        market
            .denormalize_amount_floor(fee_quote.integrated.executable.amount_out, market.quote_side.asset_decimals)
            .unwrap()
    );
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
        quote.gross_amount_out,
        market
            .denormalize_amount_floor(quote.integrated.executable.amount_out, market.quote_side.asset_decimals)
            .unwrap()
    );
}

#[test]
fn concentrated_center_move_reconstructs_unchanged_reserves_in_closed_form() {
    let mut config = AmmConfig {
        adjustment_step_nad: NAD / 100,
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
    assert_eq!(market.amm.center_price_nad, NAD + NAD / 5);
    assert_eq!(market.amm.last_adjustment_slot, 11);
    assert!(market.current_concentrated_spot_price_nad().unwrap().unwrap() > 0);
}

#[test]
fn concentrated_center_move_deploys_locked_bucket_only_when_it_preserves_srlp_principal() {
    let mut config = AmmConfig {
        adjustment_step_nad: NAD / 100,
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
    assert_eq!(market.base_side.reserves.live_reserve, base_live_before + u128::from(protected));
    assert_eq!(market.quote_side.reserves.live_reserve, quote_live_before + u128::from(protected));
    assert!(market.amm.curve_depth_per_share_nad >= depth_before);
    assert_eq!(market.amm.center_price_nad, NAD + NAD / 5);
    market.assert_market_invariants().unwrap();
}

#[test]
fn funded_recenter_with_a_frozen_vault_caches_the_quoted_point() {
    let mut config = AmmConfig {
        adjustment_step_nad: NAD / 100,
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
    market.deposit_single_sided(MarketAsset::Base, 20_000 * NAD, 1).unwrap();
    market.debt.quote_borrow_index_nad = 3 * NAD as u128;
    market.freeze_negative_hlp_vaults().unwrap();
    assert_eq!(market.amm.frozen_hlp_mask, 1);
    // The frozen vault's indexed debt keeps growing after the freeze.
    market.debt.quote_borrow_index_nad = 4 * NAD as u128;
    let unadjusted = market
        .hlp_curve_basis_from_reserves_nad(market.curve_reserves_nad().unwrap())
        .unwrap()
        .ordinary;
    assert_ne!(market.ordinary_curve_point_nad().unwrap(), unadjusted);

    market.amm.price_ema_nad = 2 * NAD;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        market
            .credit_protected_recenter_reserve(asset, 100_000 * NAD)
            .unwrap();
    }
    assert!(market.advance_one_amm_controller_target(11).unwrap());
    assert_eq!(market.base_side.reserves.protected_recenter_reserve, 0);
    // Quotes evaluate the frozen-adjusted point, so the admitted cache must
    // pass through it.
    let ordinary = market.ordinary_curve_point_nad().unwrap();
    assert_eq!(
        market.amm.concentrated_curve_cache,
        crate::transitions::amm::prepare_concentrated_cache_at_point(
            ordinary.base_reserve,
            ordinary.quote_reserve,
            market.current_curve_center_price_nad().unwrap(),
            market.config.amm.concentrated_curve_parameters().unwrap(),
        )
        .unwrap()
    );
}

fn retained_surcharge_market(base_bucket: u64, quote_bucket: u64) -> Market {
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
    market
        .credit_protected_recenter_reserve(MarketAsset::Base, base_bucket)
        .unwrap();
    market
        .credit_protected_recenter_reserve(MarketAsset::Quote, quote_bucket)
        .unwrap();
    market
}

fn deploy_retained_surcharge(market: &mut Market, slot: u64) {
    market.release_protected_recenter_reserves().unwrap();
    market.finalize_amm_transition_and_observe_risk(slot).unwrap();
}

#[test]
fn srlp_entry_buys_its_slice_of_retained_surcharge() {
    let reserve = 1_000_000 * NAD;
    for (base_bucket, quote_bucket) in [(10_000 * NAD, 10_000 * NAD), (25_000 * NAD, 0), (0, 40_000 * NAD)] {
        for k in [1_u64, 10, 100] {
            let mut market = retained_surcharge_market(base_bucket, quote_bucket);
            let price = u128::from(market.current_concentrated_spot_price_nad().unwrap().unwrap());
            let value = |base: u64, quote: u64| u128::from(base) * price / NAD as u128 + u128::from(quote);
            let reserves = |market: &Market| {
                value(
                    market.curve_reserve(MarketAsset::Base).unwrap(),
                    market.curve_reserve(MarketAsset::Quote).unwrap(),
                )
            };
            let mut alone = market.clone();
            deploy_retained_surcharge(&mut alone, 11);
            let held = reserves(&alone);

            // An entry and an immediate exit around a bucket cannot gain.
            let mut round_trip = market.clone();
            let entry = round_trip.add_liquidity(k * reserve, k * reserve).unwrap();
            let exit = round_trip.remove_liquidity(entry.srlp_amount).unwrap();
            assert!(
                value(exit.base_amount_out, exit.quote_amount_out)
                    <= value(entry.base_reserve_credit, entry.quote_reserve_credit),
                "k={k}"
            );

            // An entry ahead of the deployment recovers what it paid and no
            // more; the holders of record keep the whole bucket.
            let entry = market.add_liquidity(k * reserve, k * reserve).unwrap();
            market.finalize_amm_transition_and_observe_risk(10).unwrap();
            deploy_retained_surcharge(&mut market, 11);
            let exit = market.remove_liquidity(entry.srlp_amount).unwrap();
            let paid = value(entry.base_reserve_credit, entry.quote_reserve_credit);
            let received = value(exit.base_amount_out, exit.quote_amount_out);
            assert!(received <= paid, "k={k} paid={paid} received={received}");
            assert!(paid - received <= paid / 1_000_000_000 + 4, "k={k} paid={paid} received={received}");
            assert!(reserves(&market) >= held, "k={k}");
            market.assert_market_invariants().unwrap();
        }
    }
}

#[test]
fn srlp_entry_ahead_of_a_funded_controller_recenter_gains_nothing() {
    let reserve = 1_000_000 * NAD;
    let mut config = AmmConfig {
        adjustment_step_nad: NAD / 100,
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
    let protected = 200_000 * NAD;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        market.credit_protected_recenter_reserve(asset, protected).unwrap();
    }
    let price = u128::from(market.current_concentrated_spot_price_nad().unwrap().unwrap());
    let value = |base: u64, quote: u64| u128::from(base) * price / NAD as u128 + u128::from(quote);
    let reserves = |market: &Market| {
        value(
            market.curve_reserve(MarketAsset::Base).unwrap(),
            market.curve_reserve(MarketAsset::Quote).unwrap(),
        )
    };
    let mut alone = market.clone();
    assert!(alone.advance_one_amm_controller_target(11).unwrap());
    let held = reserves(&alone);

    let entry = market.add_liquidity(reserve, reserve).unwrap();
    market.finalize_amm_transition_and_observe_risk(10).unwrap();
    // The swap controller path deploys the bucket in a later slot.
    assert!(market.advance_one_amm_controller_target(11).unwrap());
    assert_eq!(market.base_side.reserves.protected_recenter_reserve, 0);
    let exit = market.remove_liquidity(entry.srlp_amount).unwrap();
    let paid = value(entry.base_reserve_credit, entry.quote_reserve_credit);
    let received = value(exit.base_amount_out, exit.quote_amount_out);
    assert!(received <= paid, "paid={paid} received={received}");
    assert!(paid - received <= paid / 1_000_000_000 + 4, "paid={paid} received={received}");
    assert!(reserves(&market) >= held);
}

#[test]
fn hlp_entry_pays_for_retained_surcharge_its_srlp_will_claim() {
    let deposit = 200_000 * NAD;
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        for (base_bucket, quote_bucket) in [(20_000 * NAD, 20_000 * NAD), (0, 40_000 * NAD)] {
            let market = retained_surcharge_market(base_bucket, quote_bucket);
            let (reserve_value, bucket_value) = market.protected_recenter_value(target, false).unwrap().unwrap();
            let capture = mul_div_u128(u128::from(deposit), bucket_value, reserve_value).unwrap();

            // The same entry after the bucket reached its holders.
            let mut deployed_first = market.clone();
            deploy_retained_surcharge(&mut deployed_first, 11);
            let entry = deployed_first.deposit_single_sided(target, deposit, 1).unwrap();
            let control = deployed_first
                .withdraw_single_sided(target, entry.hlp_amount)
                .unwrap()
                .target_amount_out;

            let mut market = market;
            let entry = market.deposit_single_sided(target, deposit, 1).unwrap();
            assert!(
                market.side(target).reserves.protected_recenter_reserve
                    > match target {
                        MarketAsset::Base => base_bucket,
                        MarketAsset::Quote => quote_bucket,
                    }
            );
            deploy_retained_surcharge(&mut market, 11);
            let received = market
                .withdraw_single_sided(target, entry.hlp_amount)
                .unwrap()
                .target_amount_out;
            // Without the entry payment the vault's new srLP would capture
            // about `capture` of the bucket. The payment is made in the
            // target asset, so a one-sided bucket costs the entry slightly
            // more than its slice once deployed; it never gains.
            assert!(u128::from(received) <= u128::from(control), "{target:?} {received} > {control}");
            assert!(
                u128::from(received) + capture / 50 >= u128::from(control),
                "{target:?} {received} vs {control}"
            );
            market.assert_market_invariants().unwrap();
        }
    }
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

    market.remove_liquidity(added.srlp_amount).unwrap();
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
    let curve = market.bounded_curve()?;
    let mut prepared = SwapRequest {current_slot:2,current_unix_timestamp:0,asset_in:asset,
        reserve_credit:input,protocol_fee_bps:0}.prepare(market)?;
    // Quote preparation accrues interest before computing the executable
    // curve. Compare the swap against that post-accrual boundary; the rate
    // advance is a separate state transition from the swap itself.
    let before = market.ordinary_curve_point_nad()?;
    let output=prepared.quote.amount_out;
    let credit=LeverageSwapFeeCredit::from_total_actual_credit(&prepared.leverage_quote(), prepared.quote.fee.claimable_fee_debit)?;
    prepared.apply(market,SwapCashPolicy::Spot,credit,2,0,ProtocolAuctionSplit::default(),None)?;
    market.assert_market_invariants()?;
    if let Some(curve) = curve {
        let after = market.ordinary_curve_point_nad()?;
        let delta = curve.invariant_delta_bounds(before,after)?;
        assert!(delta.1>=0,"materialized invariant decreased: asset={asset:?} lambda={} compounding={} frozen={} before={before:?} after={after:?} delta={delta:?} after_state={:?}",
            market.oracle.policy.lambda_bps, market.config.amm.compounding_fee_bps,
            market.amm.frozen_hlp_mask, market.integrated_curve_state_nad()?);
    }
    Ok(output)
}

#[test]
fn bounded_oracle_swaps_materialize_hlp_and_reject_cycle_profit() {
    for config in [AmmConfig::default(),concentrated_config()] {
        for lambda in [0,2500,5000,9999] {
            for owners in [0,1,2,3] {
                let mut market=market_with_liquidity(config);

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
                let plan=prepare_concentrated_hlp_transition(&market,&start,&quote,side)?;
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
